use crate::audio::{
    AudioChunk, AudioResampler, DecodedWord, FRAME_SAMPLES, SAMPLE_RATE, Segmenter, Transcript,
};
use color_eyre::eyre::{ContextCompat, bail, eyre};
use cpal::{
    SampleFormat,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use hf_hub::{
    HFClient,
    progress::{DownloadEvent, ProgressEvent, ProgressHandler},
};
use indicatif::{ProgressBar, ProgressStyle};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self as sync_mpsc, RecvTimeoutError, SyncSender},
    },
    time::{Duration, Instant},
};
use tokio::{
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    task,
};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
    WhisperVadContext, WhisperVadContextParams,
};

pub struct IndicatifProgress {
    pub pb: ProgressBar,
}

impl ProgressHandler for IndicatifProgress {
    fn on_progress(&self, event: &ProgressEvent) {
        let ProgressEvent::Download(event) = event else {
            return;
        };

        match event {
            DownloadEvent::Start { total_bytes, .. } => {
                self.pb.set_length(*total_bytes);
            }

            DownloadEvent::AggregateProgress {
                bytes_completed,
                total_bytes,
                ..
            } => {
                self.pb.set_length(*total_bytes);
                self.pb.set_position(*bytes_completed);
            }

            DownloadEvent::Complete => {
                self.pb.finish_and_clear();
            }

            _ => {}
        }
    }
}

pub struct Whisper<'a, 'b> {
    ctx: WhisperContext,
    params: FullParams<'a, 'b>,
    vad: WhisperVadContext,
    prompt: String,
}

impl<'a, 'b> Whisper<'a, 'b>
where
    'a: 'static,
    'b: 'static,
{
    pub async fn new(
        repo: String,
        model: String,
        cache: Option<PathBuf>,
    ) -> color_eyre::Result<Self> {
        whisper_rs::install_logging_hooks();

        let mut iter = repo.split('/');
        let user = iter.next().context("Expected user before '/'")?;
        let repo_name = iter.next().context("Expected repo after '/'")?;

        tracing::info!("Fetching model {} from {}...", model, repo);

        let client = HFClient::new()?;
        let repo = client.model(user, repo_name);

        let pb = ProgressBar::new(0);

        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] \
                 [{bar:40.cyan/blue}] \
                 {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
            )?
            .progress_chars("#>-"),
        );

        pb.set_message(format!("Downloading {}", model));

        let progress = IndicatifProgress { pb: pb.clone() };

        let model_path = repo
            .download_file()
            .maybe_local_dir(cache.clone())
            .filename(&model)
            .progress(progress)
            .send()
            .await?;

        if !pb.is_finished() {
            pb.finish_and_clear();
        }

        tracing::info!("Loading model...");
        let ctx = task::spawn_blocking(move || {
            WhisperContext::new_with_params(model_path, WhisperContextParameters::default())
        })
        .await??;

        tracing::info!("Loading Silero voice activity detector...");

        let pb = ProgressBar::new(0);

        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] \
                 [{bar:40.cyan/blue}] \
                 {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
            )?
            .progress_chars("#>-"),
        );

        pb.set_message(format!("Downloading {}", model));

        let progress = IndicatifProgress { pb: pb.clone() };

        let vad_path = client
            .model("ggml-org", "whisper-vad")
            .download_file()
            .maybe_local_dir(cache)
            .filename("ggml-silero-v6.2.0.bin")
            .progress(progress)
            .send()
            .await?;

        if !pb.is_finished() {
            pb.finish_and_clear();
        }

        let vad = task::spawn_blocking(move || {
            let mut params = WhisperVadContextParams::default();
            params.set_n_threads(1);
            WhisperVadContext::new(&vad_path.to_string_lossy(), params)
        })
        .await??;

        let mut params = FullParams::new(SamplingStrategy::BeamSearch {
            beam_size: 5,
            patience: -1.0,
        });

        params.set_n_threads(
            std::thread::available_parallelism()
                .map(|n| n.get().min(8) as i32)
                .unwrap_or(4),
        );

        params.set_language(Some("en"));
        params.set_translate(false);

        params.set_no_context(true);
        params.set_single_segment(false);
        // Word-level segments let us commit whole words by audio time instead
        // of deleting text that happens to match the previous chunk's suffix.
        params.set_no_timestamps(false);
        params.set_token_timestamps(true);
        params.set_max_len(1);
        params.set_split_on_word(true);
        params.set_suppress_blank(true);
        params.set_suppress_nst(true);
        params.set_temperature(0.0);
        params.set_temperature_inc(0.2);

        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        Ok(Self {
            ctx,
            params,
            vad,
            prompt: String::new(),
        })
    }

    pub fn transcribe_mic(
        self,
        playback_active: Arc<AtomicBool>,
        agent_running: Arc<AtomicBool>,
    ) -> color_eyre::Result<UnboundedReceiver<SpeechSegment>> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .context("no default input device")?;

        let supported = device.default_input_config()?;
        tracing::info!(?supported, "Using native microphone format");

        let (segment_tx, segment_rx) = mpsc::unbounded_channel();

        task::spawn_blocking(move || {
            if let Err(e) = run_transcription_worker(
                device,
                supported,
                self,
                segment_tx,
                playback_active,
                agent_running,
            ) {
                tracing::error!("Whisper transcription error: {}", e);
            }
        });

        Ok(segment_rx)
    }
}

fn run_transcription_worker<'a, 'b>(
    device: cpal::Device,
    supported: cpal::SupportedStreamConfig,
    whisper: Whisper<'a, 'b>,
    segment_tx: UnboundedSender<SpeechSegment>,
    playback_active: Arc<AtomicBool>,
    agent_running: Arc<AtomicBool>,
) -> color_eyre::Result<()>
where
    'a: 'static,
    'b: 'static,
{
    let Whisper {
        ctx,
        params,
        vad,
        prompt,
    } = whisper;
    let mut state = ctx.create_state()?;
    let (audio_tx, audio_rx) = sync_mpsc::sync_channel(256);
    let failed = Arc::new(AtomicBool::new(false));
    let sample_rate = supported.sample_rate() as usize;
    let config = supported.config();
    let stream = match supported.sample_format() {
        SampleFormat::F32 => input_stream::<f32>(&device, config, audio_tx, Arc::clone(&failed)),
        SampleFormat::F64 => input_stream::<f64>(&device, config, audio_tx, Arc::clone(&failed)),
        SampleFormat::I16 => input_stream::<i16>(&device, config, audio_tx, Arc::clone(&failed)),
        SampleFormat::I32 => input_stream::<i32>(&device, config, audio_tx, Arc::clone(&failed)),
        SampleFormat::U16 => input_stream::<u16>(&device, config, audio_tx, Arc::clone(&failed)),
        format => bail!("Unsupported microphone format: {format:?}"),
    }?;
    stream.play()?;

    let stopping = AtomicBool::new(false);
    // A bounded queue absorbs brief inference spikes, not an entire lecture.
    let (chunk_tx, chunk_rx) = sync_mpsc::sync_channel::<AudioChunk>(8);
    let paused =
        || playback_active.load(Ordering::Acquire) || agent_running.load(Ordering::Acquire);
    std::thread::scope(|scope| {
        let stop = &stopping;
        let paused = &paused;
        let collector = scope.spawn(move || -> color_eyre::Result<()> {
            let mut audio = AudioProcessor::new(vad, sample_rate)?;
            let mut was_paused = false;
            let mut last_audio = Instant::now();
            let mut received_audio = false;
            while !stop.load(Ordering::Relaxed) {
                if failed.load(Ordering::Relaxed) {
                    bail!("Microphone capture failed or overflowed; audio would be missing");
                }
                let input = match audio_rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(input) => input,
                    Err(RecvTimeoutError::Timeout) => {
                        if last_audio.elapsed() > Duration::from_secs(5) {
                            bail!("No microphone audio received for five seconds; check the input device");
                        }
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => bail!("Microphone disconnected"),
                };
                last_audio = Instant::now();
                if !received_audio {
                    tracing::info!("Microphone audio is arriving");
                    received_audio = true;
                }
                // Discard assistant audio and any partial utterance at the pause.
                if paused() {
                    if !was_paused {
                        audio.reset(sample_rate)?;
                    }
                    was_paused = true;
                    continue;
                }
                was_paused = false;
                for chunk in audio.push(&input)? {
                    chunk_tx.try_send(chunk).map_err(|_| eyre!(
                        "Transcription cannot keep up: audio queue full. Use a smaller model."
                    ))?;
                }
            }
            Ok(())
        });

        let result = (|| -> color_eyre::Result<()> {
            // Acoustic overlap supplies recent context. Feeding already decoded
            // overlapping speech back as a text prompt can bias it into repetition.
            let tokens = ctx.tokenize(&prompt.replace('\0', ""), 1024)?;
            let tokens = &tokens[tokens.len().saturating_sub(224)..];
            let mut transcript = Transcript::default();
            while !segment_tx.is_closed() {
                if collector.is_finished() {
                    break;
                }
                let chunk = match chunk_rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(chunk) => chunk,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                };
                let duration = chunk.samples.len() as f64 / SAMPLE_RATE as f64;
                if paused() {
                    transcript = Transcript::default();
                    continue;
                }
                let new_duration = chunk.commit.len() as f64 / SAMPLE_RATE as f64;
                let queued = chunk.queued_at.elapsed().as_secs_f64();
                let started = Instant::now();
                let words = decode_chunk(&mut state, params.clone(), tokens, &chunk)?;
                let text = transcript.commit(&chunk, &words);
                let elapsed = started.elapsed().as_secs_f64();
                tracing::debug!(
                    audio_seconds = duration,
                    inference_seconds = elapsed,
                    queued_seconds = queued,
                    "Decoded audio"
                );
                if elapsed > new_duration || queued > new_duration {
                    tracing::warn!(
                        inference_seconds = elapsed,
                        queued_seconds = queued,
                        "Falling behind real time; consider a smaller model"
                    );
                }
                if paused() {
                    transcript = Transcript::default();
                    continue;
                }
                if !text.is_empty() && segment_tx.send(SpeechSegment { text }).is_err() {
                    break;
                }
            }
            Ok(())
        })();
        stopping.store(true, Ordering::Relaxed);
        let captured = collector
            .join()
            .map_err(|_| eyre!("Audio capture worker panicked"))?;
        result.and(captured)
    })
}

fn decode_chunk<'a, 'b>(
    state: &mut WhisperState,
    mut params: FullParams<'a, 'b>,
    prompt: &'b [i32],
    chunk: &AudioChunk,
) -> color_eyre::Result<Vec<DecodedWord>> {
    params.set_tokens(prompt);
    state.full(params, &chunk.samples)?;
    // Silero gates silence. Do not throw away a whole spoken passage based on
    // Whisper's no-speech score, which can be high even for intelligible speech.
    let words = state
        .as_iter()
        .map(|word| DecodedWord {
            text: word.to_string(),
            start_cs: word.start_timestamp(),
            end_cs: word.end_timestamp(),
        })
        .filter(|word| !word.text.trim().is_empty())
        .collect();
    Ok(words)
}

/// Shared by the live microphone worker and recorded-audio regression tests.
struct AudioProcessor {
    vad: WhisperVadContext,
    resampler: AudioResampler,
    pending: Vec<f32>,
    history: Vec<f32>,
    segmenter: Segmenter,
}

impl AudioProcessor {
    fn reset(&mut self, sample_rate: usize) -> color_eyre::Result<()> {
        self.resampler = AudioResampler::new(sample_rate)?;
        self.pending.clear();
        self.history.clear();
        self.segmenter = Segmenter::default();
        Ok(())
    }

    fn new(vad: WhisperVadContext, sample_rate: usize) -> color_eyre::Result<Self> {
        Ok(Self {
            vad,
            resampler: AudioResampler::new(sample_rate)?,
            pending: Vec::new(),
            history: Vec::new(),
            segmenter: Segmenter::default(),
        })
    }

    fn push(&mut self, input: &[f32]) -> color_eyre::Result<Vec<AudioChunk>> {
        const BATCH: usize = 16 * FRAME_SAMPLES;
        let mut chunks = Vec::new();
        self.resampler.push(input, &mut self.pending)?;
        while self.pending.len() >= BATCH {
            let batch: Vec<_> = self.pending.drain(..BATCH).collect();
            // The native VAD resets recurrent state each call. Replay two seconds
            // of context, but apply decisions only to the new frames.
            let old_frames = self.history.len() / FRAME_SAMPLES;
            self.history.extend_from_slice(&batch);
            self.vad.detect_speech(&self.history)?;
            if self.vad.probabilities().len() != self.history.len() / FRAME_SAMPLES {
                bail!("Voice detector returned an unexpected number of audio frames");
            }
            tracing::trace!(
                speech_probability = self.vad.probabilities().last().copied().unwrap_or(0.0),
                "Voice activity"
            );
            for (frame, probability) in batch
                .as_chunks::<FRAME_SAMPLES>()
                .0
                .iter()
                .zip(self.vad.probabilities().iter().skip(old_frames))
            {
                if let Some(chunk) = self.segmenter.push(frame, *probability) {
                    chunks.push(chunk);
                }
            }
            let excess = self.history.len().saturating_sub(64 * FRAME_SAMPLES);
            self.history.drain(..excess);
        }
        Ok(chunks)
    }
}

fn input_stream<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    tx: SyncSender<Vec<f32>>,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, cpal::Error>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let channels = config.channels as usize;
    let stream_failed = Arc::clone(&failed);
    device.build_input_stream(
        config,
        move |input: &[T], _| {
            let mono = input
                .chunks_exact(channels)
                .map(|frame| {
                    frame
                        .iter()
                        .map(|&sample| cpal::Sample::to_sample::<f32>(sample))
                        .sum::<f32>()
                        / channels as f32
                })
                .collect();
            if tx.try_send(mono).is_err() {
                failed.store(true, Ordering::Relaxed);
            }
        },
        move |error| {
            tracing::error!(%error, "Microphone stream error");
            stream_failed.store(true, Ordering::Relaxed);
        },
        None,
    )
}

#[derive(Debug)]
pub struct SpeechSegment {
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires STT_TEST_WAV pointing to whisper.cpp samples/jfk.wav and downloads models"]
    async fn recorded_speech_keeps_onsets_endings_and_forced_boundaries() -> color_eyre::Result<()>
    {
        let path = std::env::var("STT_TEST_WAV")?;
        let mut reader = hound::WavReader::open(path)?;
        assert_eq!(reader.spec().sample_rate, SAMPLE_RATE as u32);
        assert_eq!(reader.spec().channels, 1);
        let source = reader
            .samples::<i16>()
            .map(|sample| sample.map(|v| v as f32 / 32768.0))
            .collect::<Result<Vec<_>, _>>()?;
        let Whisper {
            ctx, params, vad, ..
        } = Whisper::new(
            "ggerganov/whisper.cpp".into(),
            "ggml-small.en.bin".into(),
            None,
        )
        .await?;
        let mut state = ctx.create_state()?;

        // Exercise the exact live resampling/VAD/segmentation path at 48 kHz.
        let mut audio = AudioProcessor::new(vad, 48_000)?;
        let input: Vec<_> = std::iter::repeat_n(0.0, SAMPLE_RATE)
            .chain(source.iter().copied())
            .chain(std::iter::repeat_n(0.0, 3 * SAMPLE_RATE))
            .flat_map(|v| std::iter::repeat_n(v, 3))
            .collect();
        let mut transcript = String::new();
        let mut committed = Transcript::default();
        for block in input.chunks(1024) {
            for chunk in audio.push(block)? {
                let words = decode_chunk(&mut state, params.clone(), &[], &chunk)?;
                let text = committed.commit(&chunk, &words);
                println!("Natural chunk: {text}");
                transcript.push(' ');
                transcript.push_str(&text);
            }
        }
        assert_jfk_transcript(&transcript);

        // Deliberately cut inside the sentence and defer two seconds. This also
        // exercises a forced boundary if VAD finds only short natural utterances.
        let padding = SAMPLE_RATE / 4;
        let make_chunk = |from: usize, to: usize, commit: std::ops::Range<usize>| {
            let mut samples = vec![0.0; padding];
            samples.extend_from_slice(&source[from..to]);
            samples.resize(samples.len() + padding, 0.0);
            AudioChunk {
                samples,
                commit: commit.start + padding..commit.end + padding,
                final_chunk: to == source.len(),
                queued_at: Instant::now(),
            }
        };
        for split_seconds in [5, 6, 7, 8, 9] {
            let split = split_seconds * SAMPLE_RATE;
            let from = split - 4 * SAMPLE_RATE;
            let first = make_chunk(0, split, 0..split - 2 * SAMPLE_RATE);
            let last = make_chunk(
                from,
                source.len(),
                2 * SAMPLE_RATE..source.len() - from + padding,
            );
            let mut transcript = String::new();
            let mut committed = Transcript::default();
            for chunk in [first, last] {
                let words = decode_chunk(&mut state, params.clone(), &[], &chunk)?;
                let text = committed.commit(&chunk, &words);
                println!("Forced chunk ({split_seconds}s): {text}");
                transcript.push(' ');
                transcript.push_str(&text);
            }
            assert_jfk_transcript(&transcript);
        }
        Ok(())
    }

    fn assert_jfk_transcript(transcript: &str) {
        let words: Vec<_> = transcript
            .split_whitespace()
            .map(|word| {
                word.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase()
            })
            .collect();
        let expected: Vec<_> = "and so my fellow americans ask not what your country can do for you ask what you can do for your country".split_whitespace().collect();
        assert!(
            words.starts_with(&[
                "and".into(),
                "so".into(),
                "my".into(),
                "fellow".into(),
                "americans".into()
            ]),
            "Clipped onset: {transcript}"
        );
        assert!(
            words.ends_with(&["for".into(), "your".into(), "country".into()]),
            "Clipped ending: {transcript}"
        );
        // Word edit distance catches dropped or repeated words at the seam.
        let mut distances: Vec<usize> = (0..=expected.len()).collect();
        for (i, actual) in words.iter().enumerate() {
            let mut previous = distances[0];
            distances[0] = i + 1;
            for (j, wanted) in expected.iter().enumerate() {
                let above = distances[j + 1];
                distances[j + 1] = (previous + usize::from(actual != wanted))
                    .min(above + 1)
                    .min(distances[j] + 1);
                previous = above;
            }
        }
        assert!(
            distances[expected.len()] <= 2,
            "Unexpected omissions/repetitions: {transcript}"
        );
    }

    #[tokio::test]
    #[ignore = "downloads the Silero model; run explicitly for native VAD integration"]
    async fn silero_frame_alignment_and_silence() -> color_eyre::Result<()> {
        let path = HFClient::new()?
            .model("ggml-org", "whisper-vad")
            .download_file()
            .filename("ggml-silero-v6.2.0.bin")
            .send()
            .await?;
        let mut vad =
            WhisperVadContext::new(&path.to_string_lossy(), WhisperVadContextParams::default())?;
        // First batch, warm-up batch, then a full history + new audio batch.
        for frames in [16, 32, 48] {
            vad.detect_speech(&vec![0.0; frames * FRAME_SAMPLES])?;
            assert_eq!(vad.probabilities().len(), frames);
            assert!(
                vad.probabilities()
                    .iter()
                    .all(|&p| p.is_finite() && p < 0.5)
            );
        }
        Ok(())
    }
}
