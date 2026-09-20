use color_eyre::eyre::ContextCompat;
use cpal::{
    SampleFormat,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use hf_hub::{
    HFClient,
    progress::{DownloadEvent, ProgressEvent, ProgressHandler},
};
use indicatif::{ProgressBar, ProgressStyle};
use std::{path::PathBuf, sync::{Arc, atomic::{AtomicBool, Ordering}}};
use tokio::{
    sync::mpsc::{self, UnboundedReceiver, UnboundedSender},
    task,
};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

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
}

impl<'a, 'b> Whisper<'a, 'b>
where
    'a: 'static,
    'b: 'static,
{
    pub async fn new(
        repo: String,
        file: String,
        cache: Option<PathBuf>,
    ) -> color_eyre::Result<Self> {
        let mut iter = repo.split('/');
        let user = iter.next().context("Expected user before '/'")?;
        let repo_name = iter.next().context("Expected repo after '/'")?;

        let client = HFClient::new()?;
        let repo = client.model(user, repo_name);

        tracing::info!("Downloading model...");

        let pb = ProgressBar::new(0);

        pb.set_style(
            ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] \
                 [{bar:40.cyan/blue}] \
                 {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
            )?
            .progress_chars("#>-"),
        );

        pb.set_message(format!("Downloading {file}"));

        let progress = IndicatifProgress { pb: pb.clone() };

        let model_path = repo
            .download_file()
            .maybe_local_dir(cache)
            .filename(file)
            .progress(progress)
            .send()
            .await?;

        if !pb.is_finished() {
            pb.finish_and_clear();
        }

        tracing::info!("Opening model...");
        let ctx = task::spawn_blocking(move || {
            WhisperContext::new_with_params(model_path, WhisperContextParameters::default())
        })
        .await??;

        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });

        params.set_n_threads(
            std::thread::available_parallelism()
                .map(|n| n.get() as i32)
                .unwrap_or(4),
        );

        params.set_language(Some("en"));
        params.set_translate(false);

        params.set_no_context(true);
        params.set_single_segment(true);
        params.set_no_timestamps(true);

        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        Ok(Self { ctx, params })
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

        let supported = device
            .supported_input_configs()?
            .filter(|c| c.sample_format() == SampleFormat::F32)
            .find_map(|c| {
                c.try_with_sample_rate(16_000)
                    .or_else(|| c.try_with_standard_sample_rate())
            })
            .context("default microphone has no usable f32 input format")?;

        let sample_rate = supported.sample_rate() as f64;
        let channels = supported.channels() as usize;
        let config = supported.config();

        let (segment_tx, segment_rx) = mpsc::unbounded_channel();

        let Whisper { ctx, mut params } = self;

        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);

        task::spawn_blocking(move || {
            if let Err(e) = run_transcription_worker(
                device,
                config,
                channels,
                sample_rate,
                ctx,
                params,
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
    config: cpal::StreamConfig,
    channels: usize,
    sample_rate: f64,
    ctx: WhisperContext,
    params: FullParams<'a, 'b>,
    segment_tx: UnboundedSender<SpeechSegment>,
    playback_active: Arc<AtomicBool>,
    agent_running: Arc<AtomicBool>,
) -> color_eyre::Result<()>
where
    'a: 'static,
    'b: 'static,
{
    let (audio_tx, audio_rx) = std::sync::mpsc::sync_channel::<Vec<f32>>(32);

    let stream = device.build_input_stream(
        config,
        move |input: &[f32], _| {
            let mono = input
                .chunks_exact(channels)
                .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                .collect::<Vec<_>>();

            let _ = audio_tx.try_send(mono);
        },
        |_| {},
        None,
    )?;

    stream.play()?;

    let mut resampler = LinearResampler::new(sample_rate, 16_000.0);

    const SAMPLE_RATE: usize = 16_000;
    const FRAME_MS: usize = 20;
    const FRAME_SAMPLES: usize = SAMPLE_RATE * FRAME_MS / 1000;
    const START_THRESHOLD: f32 = 0.012;
    const STOP_THRESHOLD: f32 = 0.008;
    const MIN_SPEECH_MS: usize = 250;
    const SILENCE_END_MS: usize = 500;

    let mut pending = Vec::<f32>::new();
    let mut utterance = Vec::<f32>::new();
    let mut speaking = false;
    let mut speech_ms = 0usize;
    let mut silence_ms = 0usize;

    while let Ok(input) = audio_rx.recv() {
        resampler.push(&input, &mut pending);

        // Do not transcribe the assistant's own speech from the microphone.
        if playback_active.load(Ordering::Acquire) || agent_running.load(Ordering::Acquire) {
            pending.clear();
            utterance.clear();
            speaking = false;
            speech_ms = 0;
            silence_ms = 0;
            continue;
        }

        while pending.len() >= FRAME_SAMPLES {
            let frame: Vec<f32> = pending.drain(..FRAME_SAMPLES).collect();
            let rms = (frame.iter().map(|x| x * x).sum::<f32>() / frame.len() as f32).sqrt();

            if speaking {
                utterance.extend_from_slice(&frame);

                if rms < STOP_THRESHOLD {
                    silence_ms += FRAME_MS;
                } else {
                    silence_ms = 0;
                }

                if silence_ms >= SILENCE_END_MS {
                    speaking = false;
                    silence_ms = 0;

                    if speech_ms >= MIN_SPEECH_MS {
                        let mut state = ctx.create_state()?;
                        state
                            .full(params.clone(), &utterance)
                            .expect("whisper inference failed");

                        let text = state.as_iter().map(|s| s.to_string()).collect::<String>();
                        let text = text.trim();

                        if !text.is_empty() {
                            let _ = segment_tx.send(SpeechSegment {
                                start_ms: 0,
                                end_ms: 0,
                                text: text.to_owned(),
                            });
                        }
                    }

                    utterance.clear();
                    speech_ms = 0;
                }
            } else if rms >= START_THRESHOLD {
                speaking = true;
                speech_ms = FRAME_MS;
                utterance.clear();
                utterance.extend_from_slice(&frame);
            }

            if speaking && rms >= STOP_THRESHOLD {
                speech_ms += FRAME_MS;
            }
        }
    }

    drop(stream);
    Ok(())
}

#[derive(Debug)]
pub struct SpeechSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

struct LinearResampler {
    ratio: f64,
    pos: f64,
    previous: Option<f32>,
}

impl LinearResampler {
    fn new(input_rate: f64, output_rate: f64) -> Self {
        Self {
            ratio: input_rate / output_rate,
            pos: 0.0,
            previous: None,
        }
    }

    fn push(&mut self, input: &[f32], output: &mut Vec<f32>) {
        if input.is_empty() {
            return;
        }

        let mut samples = Vec::with_capacity(input.len() + 1);

        if let Some(previous) = self.previous {
            samples.push(previous);
        }

        samples.extend_from_slice(input);

        while self.pos + 1.0 < samples.len() as f64 {
            let i = self.pos.floor() as usize;
            let frac = (self.pos - i as f64) as f32;

            output.push(samples[i] * (1.0 - frac) + samples[i + 1] * frac);

            self.pos += self.ratio;
        }

        self.pos -= (samples.len() - 1) as f64;
        self.previous = samples.last().copied();
    }
}
