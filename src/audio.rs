//! Streaming audio boundaries, independent of microphone and model inference.
use audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};
use std::{ops::Range, time::Instant};

pub const SAMPLE_RATE: usize = 16_000;
pub const FRAME_SAMPLES: usize = 512; // Silero's 32 ms frame at 16 kHz.
const MAX_FRAMES: usize = 384; // 12.288 seconds of acoustic context.
const PRE_ROLL_FRAMES: usize = 32;
const OVERLAP_FRAMES: usize = 128;
const DEFER_FRAMES: usize = 64;
const SILENCE_FRAMES: usize = 32;
const PADDING: usize = SAMPLE_RATE / 4;

pub struct AudioChunk {
    pub samples: Vec<f32>,
    /// Nominal interval to emit, adjusted by overlap alignment when timestamps
    /// drift. A final chunk has no upper timestamp limit: native word timestamps
    /// can overshoot the actual recording and must not clip its last word.
    pub commit: Range<usize>,
    pub final_chunk: bool,
    pub queued_at: Instant,
}

impl AudioChunk {
    pub fn owns_word(&self, start_cs: i64, end_cs: i64) -> bool {
        let midpoint = (start_cs.max(0) + end_cs.max(0)) as usize * SAMPLE_RATE / 200;
        midpoint >= self.commit.start && (self.final_chunk || midpoint < self.commit.end)
    }
}

pub struct DecodedWord {
    pub text: String,
    pub start_cs: i64,
    pub end_cs: i64,
}

impl DecodedWord {
    fn midpoint(&self) -> usize {
        (self.start_cs.max(0) + self.end_cs.max(0)) as usize * SAMPLE_RATE / 200
    }
}

/// Word timestamps can shift on re-decoding. Anchor the seam to already emitted
/// words within the actual overlapping audio, rather than trusting time alone.
#[derive(Default)]
pub struct Transcript {
    previous: Vec<String>,
}

impl Transcript {
    pub fn commit(&mut self, chunk: &AudioChunk, words: &[DecodedWord]) -> String {
        let normalize = |text: &str| {
            text.trim_matches(|c: char| !c.is_alphanumeric())
                .to_lowercase()
        };
        let normalized: Vec<_> = words.iter().map(|w| normalize(w.text.trim())).collect();
        let mut start = words
            .iter()
            .position(|w| chunk.owns_word(w.start_cs, w.end_cs))
            .unwrap_or(words.len());
        if chunk.commit.start > PADDING {
            // Only reconcile continuation windows. At a real pause, identical
            // phrases are new speech and must remain in the transcript.
            for count in (2..=self.previous.len().min(words.len()).min(8)).rev() {
                let matched = (count..=words.len())
                    .filter(|&end| {
                        normalized[end - count..end] == self.previous[self.previous.len() - count..]
                            && words[end - 1].midpoint().abs_diff(chunk.commit.start)
                                <= 2 * SAMPLE_RATE
                    })
                    .min_by_key(|&end| words[end - 1].midpoint().abs_diff(chunk.commit.start));
                if let Some(end) = matched {
                    start = end;
                    break;
                }
            }
        }
        let text: String = words[start..]
            .iter()
            .take_while(|w| chunk.final_chunk || w.midpoint() < chunk.commit.end)
            .map(|w| w.text.as_str())
            .collect();
        self.previous.extend(text.split_whitespace().map(normalize));
        let excess = self.previous.len().saturating_sub(16);
        self.previous.drain(..excess);
        text.trim().to_owned()
    }
}

#[derive(Default)]
pub struct Segmenter {
    samples: Vec<f32>,
    speaking: bool,
    silence: usize,
    new_speech: usize,
    // Nonzero after a forced split. The retained audio contains both previously
    // committed context and an uncommitted tail that MUST survive a following pause.
    committed: usize,
}

impl Segmenter {
    pub fn push(&mut self, frame: &[f32], probability: f32) -> Option<AudioChunk> {
        debug_assert_eq!(frame.len(), FRAME_SAMPLES);
        self.samples.extend_from_slice(frame);
        let speech = probability >= if self.speaking { 0.2 } else { 0.35 };
        if speech {
            self.speaking = true;
            self.silence = 0;
            self.new_speech += 1;
        } else if self.speaking {
            self.silence += 1;
        } else {
            let excess = self
                .samples
                .len()
                .saturating_sub(PRE_ROLL_FRAMES * FRAME_SAMPLES);
            self.samples.drain(..excess);
            return None;
        }

        let ended = self.silence >= SILENCE_FRAMES;
        if !ended && self.samples.len() < MAX_FRAMES * FRAME_SAMPLES {
            return None;
        }

        let samples = std::mem::take(&mut self.samples);
        let start = self.committed;
        let has_speech = self.new_speech >= 2 || self.committed != 0;
        self.new_speech = 0;
        let end = if ended {
            // A natural endpoint already includes a second of trailing audio.
            self.speaking = false;
            self.silence = 0;
            self.committed = 0;
            // Keep pre-roll even across a natural endpoint, in case VAD missed
            // a quiet syllable immediately before the next detected utterance.
            self.samples.extend_from_slice(
                &samples[samples
                    .len()
                    .saturating_sub(PRE_ROLL_FRAMES * FRAME_SAMPLES)..],
            );
            samples.len() + PADDING
        } else {
            self.samples
                .extend_from_slice(&samples[samples.len() - OVERLAP_FRAMES * FRAME_SAMPLES..]);
            self.committed = (OVERLAP_FRAMES - DEFER_FRAMES) * FRAME_SAMPLES;
            samples.len() - DEFER_FRAMES * FRAME_SAMPLES
        };
        if !has_speech {
            return None;
        }

        // Padding gives edge phonemes breathing room, but is not counted as
        // new audio. Commit times are shifted to match the padded input.
        let mut padded = vec![0.0; PADDING];
        padded.extend_from_slice(&samples);
        padded.resize((padded.len() + PADDING).max(SAMPLE_RATE), 0.0);
        Some(AudioChunk {
            samples: padded,
            commit: start + PADDING..end + PADDING,
            final_chunk: ended,
            queued_at: Instant::now(),
        })
    }
}

/// Fixed-rate, band-limited conversion. State and unconsumed input are retained
/// across callbacks; creating a new resampler per callback would clip audio.
pub struct AudioResampler {
    inner: Option<Fft<f32>>,
    pending: Vec<f32>,
    scratch: Vec<f32>,
}

impl AudioResampler {
    pub fn new(input_rate: usize) -> color_eyre::Result<Self> {
        let inner = if input_rate == SAMPLE_RATE {
            None
        } else {
            Some(Fft::new(input_rate, SAMPLE_RATE, 1024, 1, FixedSync::Both)?)
        };
        let scratch = vec![0.0; inner.as_ref().map_or(0, |r| r.output_frames_max())];
        Ok(Self {
            inner,
            pending: Vec::new(),
            scratch,
        })
    }

    pub fn push(&mut self, input: &[f32], output: &mut Vec<f32>) -> color_eyre::Result<()> {
        let Some(resampler) = &mut self.inner else {
            output.extend_from_slice(input);
            return Ok(());
        };
        self.pending.extend_from_slice(input);
        let mut consumed = 0;
        while self.pending.len() - consumed >= resampler.input_frames_next() {
            let needed = resampler.input_frames_next();
            let input =
                InterleavedSlice::new(&self.pending[consumed..consumed + needed], 1, needed)?;
            let capacity = self.scratch.len();
            let mut out = InterleavedSlice::new_mut(&mut self.scratch, 1, capacity)?;
            let (read, written) = resampler.process_into_buffer(&input, &mut out, None)?;
            output.extend_from_slice(&self.scratch[..written]);
            consumed += read;
        }
        self.pending.drain(..consumed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_drift_at_overlap_does_not_duplicate_or_drop_words() {
        let chunk = AudioChunk {
            samples: vec![],
            commit: 2 * SAMPLE_RATE + PADDING..10 * SAMPLE_RATE,
            final_chunk: true,
            queued_at: Instant::now(),
        };
        for shift in [-80, 0, 80] {
            let mut transcript = Transcript {
                previous: vec!["ask".into(), "not".into(), "what".into()],
            };
            let words: Vec<_> = [" ask", " not", " what", " your", " country"]
                .iter()
                .enumerate()
                .map(|(i, text)| DecodedWord {
                    text: text.to_string(),
                    start_cs: 100 + 50 * i as i64 + shift,
                    end_cs: 150 + 50 * i as i64 + shift,
                })
                .collect();
            assert_eq!(transcript.commit(&chunk, &words), "your country");
        }
    }

    #[test]
    fn repeated_phrase_after_a_natural_pause_is_not_removed() {
        let mut transcript = Transcript {
            previous: vec!["ask".into(), "not".into()],
        };
        let chunk = AudioChunk {
            samples: vec![],
            commit: PADDING..10 * SAMPLE_RATE,
            final_chunk: true,
            queued_at: Instant::now(),
        };
        let words = [
            DecodedWord {
                text: " Ask".into(),
                start_cs: 50,
                end_cs: 100,
            },
            DecodedWord {
                text: " not!".into(),
                start_cs: 100,
                end_cs: 150,
            },
        ];
        assert_eq!(transcript.commit(&chunk, &words), "Ask not!");
    }

    #[test]
    fn silence_does_not_emit_or_grow_without_bound() {
        let mut segmenter = Segmenter::default();
        for _ in 0..10_000 {
            assert!(segmenter.push(&[0.0; FRAME_SAMPLES], 0.01).is_none());
        }
        assert_eq!(segmenter.samples.len(), PRE_ROLL_FRAMES * FRAME_SAMPLES);
    }

    #[test]
    fn quiet_onset_and_ending_survive_vad_delay() {
        let mut segmenter = Segmenter::default();
        // A 640ms quiet onset before VAD recognises the voice must be retained.
        for _ in 0..20 {
            segmenter.push(&[0.001; FRAME_SAMPLES], 0.1);
        }
        for _ in 0..20 {
            segmenter.push(&[0.0001; FRAME_SAMPLES], 0.8);
        }
        let mut result = None;
        for _ in 0..SILENCE_FRAMES {
            result = segmenter.push(&[0.002; FRAME_SAMPLES], 0.1);
        }
        let chunk = result.unwrap();
        assert_eq!(
            &chunk.samples[PADDING..PADDING + 20 * FRAME_SAMPLES],
            &[0.001; 20 * FRAME_SAMPLES]
        );
        assert_eq!(
            &chunk.samples[PADDING + 40 * FRAME_SAMPLES..chunk.samples.len() - PADDING],
            &[0.002; SILENCE_FRAMES * FRAME_SAMPLES]
        );
    }

    #[test]
    fn continuous_speech_has_context_on_both_sides_of_each_commit_boundary() {
        let mut segmenter = Segmenter::default();
        let mut chunks = Vec::new();
        for i in 0..MAX_FRAMES * 4 {
            if let Some(chunk) = segmenter.push(&[i as f32; FRAME_SAMPLES], 0.9) {
                assert!(chunk.samples.len() <= MAX_FRAMES * FRAME_SAMPLES + 2 * PADDING);
                chunks.push(chunk);
            }
        }
        for pair in chunks.windows(2) {
            let previous = &pair[0];
            let next = &pair[1];
            // Last committed sample and first newly committed sample are contiguous.
            assert_eq!(
                previous.samples[previous.commit.end - 1] + 1.0,
                next.samples[next.commit.start]
            );
            // The uncommitted tail is present, byte-for-byte, in the next window.
            assert_eq!(
                &previous.samples[previous.commit.end..previous.samples.len() - PADDING],
                &next.samples[next.commit.start..next.commit.start + DEFER_FRAMES * FRAME_SAMPLES]
            );
            assert!(next.commit.start >= PADDING + DEFER_FRAMES * FRAME_SAMPLES);
        }
    }

    #[test]
    fn pause_after_forced_cut_flushes_deferred_tail_without_new_speech() {
        let mut segmenter = Segmenter::default();
        let mut first = None;
        for _ in 0..MAX_FRAMES {
            first = segmenter.push(&[0.1; FRAME_SAMPLES], 0.9);
        }
        assert!(first.is_some());
        let mut last = None;
        for _ in 0..SILENCE_FRAMES {
            last = segmenter.push(&[0.0; FRAME_SAMPLES], 0.01);
        }
        let last = last.expect("the deferred ending must not be thrown away as overlap");
        assert_eq!(
            &last.samples[last.commit.start..last.commit.start + DEFER_FRAMES * FRAME_SAMPLES],
            &[0.1; DEFER_FRAMES * FRAME_SAMPLES]
        );
        assert!(!segmenter.speaking);
    }

    #[test]
    fn whole_boundary_words_have_exactly_one_owner() {
        let first = AudioChunk {
            samples: vec![],
            commit: 0..10 * SAMPLE_RATE,
            final_chunk: false,
            queued_at: Instant::now(),
        };
        let next = AudioChunk {
            samples: vec![],
            commit: 10 * SAMPLE_RATE..20 * SAMPLE_RATE,
            final_chunk: false,
            queued_at: Instant::now(),
        };
        for (start, end) in [(950, 990), (980, 1020), (1010, 1040)] {
            assert_ne!(first.owns_word(start, end), next.owns_word(start, end));
        }
    }

    #[test]
    fn final_word_is_kept_even_when_whisper_timestamps_overshoot_audio() {
        let chunk = AudioChunk {
            samples: vec![],
            commit: 2 * SAMPLE_RATE..10 * SAMPLE_RATE,
            final_chunk: true,
            queued_at: Instant::now(),
        };
        assert!(chunk.owns_word(1050, 1150));
        assert!(!chunk.owns_word(0, 100));
    }

    #[test]
    fn resampling_is_independent_of_callback_boundaries() -> color_eyre::Result<()> {
        for rate in [8_000, 16_000, 44_100, 48_000] {
            let input: Vec<_> = (0..rate).map(|i| (i as f32 * 0.01).sin()).collect();
            let mut whole = Vec::new();
            AudioResampler::new(rate)?.push(&input, &mut whole)?;
            let mut streamed = Vec::new();
            let mut resampler = AudioResampler::new(rate)?;
            for block in input.chunks(137) {
                resampler.push(block, &mut streamed)?;
            }
            assert_eq!(whole, streamed);
            assert!(whole.len().abs_diff(SAMPLE_RATE) < 2048);
        }
        Ok(())
    }

    #[test]
    fn downsampling_rejects_out_of_band_noise() -> color_eyre::Result<()> {
        let rms = |frequency: f32| -> color_eyre::Result<f32> {
            let input: Vec<_> = (0..48_000)
                .map(|i| (std::f32::consts::TAU * frequency * i as f32 / 48_000.0).sin())
                .collect();
            let mut output = Vec::new();
            AudioResampler::new(48_000)?.push(&input, &mut output)?;
            let settled = &output[2000..];
            Ok((settled.iter().map(|x| x * x).sum::<f32>() / settled.len() as f32).sqrt())
        };
        assert!(rms(1000.0)? > 0.65);
        assert!(rms(12_000.0)? < 0.01);
        Ok(())
    }
}
