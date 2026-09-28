use color_eyre::eyre::eyre;
use cpal::{
    SampleFormat, StreamConfig,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use std::{
    collections::VecDeque,
    error::Error,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use rustsam::{parser, reciter, renderer};
use tokio::{sync::mpsc, task};

const SAM_SAMPLE_RATE: u32 = 22_050;
const CHANNEL_CAPACITY: usize = 16;

#[derive(Default)]
struct AudioState {
    generation: u64,
    samples: VecDeque<f32>,
}

pub(crate) struct Utterance {
    pub text: String,
    generation: u64,
}

/// A generation-bound speech sender. Cancelling invalidates all old senders,
/// queued utterances, and synthesis results as well as clearing audible samples.
#[derive(Clone)]
pub struct Speech {
    tx: mpsc::Sender<Utterance>,
    state: Arc<Mutex<AudioState>>,
    playback_active: Arc<AtomicBool>,
    generation: u64,
}

impl Speech {
    pub fn current(&self) -> Self {
        Self {
            generation: self.state.lock().unwrap().generation,
            ..self.clone()
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.generation != self.state.lock().unwrap().generation
    }

    pub fn cancel(&self) {
        let mut state = self.state.lock().unwrap();
        state.generation += 1;
        state.samples.clear();
        self.playback_active.store(false, Ordering::Release);
    }

    pub async fn send(&self, text: String) -> color_eyre::Result<()> {
        if self.is_cancelled() {
            return Err(eyre!("Speech cancelled"));
        }
        let permit = self
            .tx
            .reserve()
            .await
            .map_err(|_| eyre!("Speech worker stopped"))?;
        let state = self.state.lock().unwrap();
        if self.generation != state.generation {
            return Err(eyre!("Speech cancelled"));
        }
        permit.send(Utterance {
            text,
            generation: self.generation,
        });
        Ok(())
    }
}

pub(crate) fn speech_channel() -> (Speech, mpsc::Receiver<Utterance>) {
    let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
    (
        Speech {
            tx,
            state: Arc::new(Mutex::new(AudioState::default())),
            playback_active: Arc::new(AtomicBool::new(false)),
            generation: 0,
        },
        rx,
    )
}

pub fn init_task() -> color_eyre::Result<(Speech, Arc<AtomicBool>)> {
    let (speech, mut rx) = speech_channel();
    let playback_active = Arc::clone(&speech.playback_active);
    let worker_playback_active = Arc::clone(&playback_active);
    let state = Arc::clone(&speech.state);

    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<color_eyre::Result<()>>(1);

    task::spawn_blocking(move || {
        if let Err(err) = run_audio_worker(&mut rx, ready_tx, worker_playback_active, state) {
            tracing::error!("SAM audio worker stopped: {err}");
        }
    });

    ready_rx.recv()??;

    Ok((speech, playback_active))
}

fn run_audio_worker(
    rx: &mut mpsc::Receiver<Utterance>,
    ready: std::sync::mpsc::SyncSender<color_eyre::Result<()>>,
    playback_active: Arc<AtomicBool>,
    state: Arc<Mutex<AudioState>>,
) -> color_eyre::Result<()> {
    let host = cpal::default_host();

    let device = match host.default_output_device() {
        Some(device) => device,
        None => {
            let msg = "No output audio device available";
            let _ = ready.send(Err(eyre!(msg)));
            return Err(eyre!(msg));
        }
    };

    let supported_config = match device
        .supported_output_configs()?
        .filter(|config| config.sample_format() == SampleFormat::F32)
        .find_map(|config| config.try_with_standard_sample_rate())
    {
        Some(config) => config,
        None => {
            let msg = "Output device has no supported F32 standard-rate config";
            let _ = ready.send(Err(eyre!(msg)));
            return Err(eyre!(msg));
        }
    };

    let config: StreamConfig = supported_config.into();

    let sample_rate = config.sample_rate;
    let channels = config.channels as usize;
    let output_samples = Arc::clone(&state);
    let output_playback_active = Arc::clone(&playback_active);

    let stream = match device.build_output_stream(
        config,
        move |output: &mut [f32], _| {
            let mut queue = match output_samples.try_lock() {
                Ok(queue) => queue,
                Err(_) => {
                    output.fill(0.0);
                    return;
                }
            };

            for frame in output.chunks_mut(channels) {
                let sample = match queue.samples.pop_front() {
                    Some(sample) => sample,
                    None => {
                        output_playback_active.store(false, Ordering::Release);
                        0.0
                    }
                };

                for channel in frame {
                    *channel = sample;
                }
            }
        },
        |err| {
            eprintln!("CPAL stream error: {err}");
        },
        None,
    ) {
        Ok(stream) => stream,
        Err(err) => {
            let msg = format!("Could not create CPAL output stream: {err}");
            let _ = ready.send(Err(eyre!(msg.clone())));
            return Err(eyre!(msg));
        }
    };

    if let Err(err) = stream.play() {
        let msg = format!("Could not start CPAL output stream: {err}");
        let _ = ready.send(Err(eyre!(msg.clone())));
        return Err(eyre!(msg));
    }

    let _ = ready.send(Ok(()));

    while let Some(utterance) = rx.blocking_recv() {
        if utterance.generation != state.lock().unwrap().generation {
            continue;
        }
        match synthesize(&utterance.text, sample_rate) {
            Ok(rendered) => {
                queue_rendered(&state, &playback_active, utterance.generation, rendered);
            }

            Err(err) => {
                tracing::warn!("Could not synthesize speech: {err}");
            }
        }
    }

    drop(stream);

    Ok(())
}

fn queue_rendered(
    state: &Mutex<AudioState>,
    active: &AtomicBool,
    generation: u64,
    rendered: Vec<f32>,
) {
    let mut state = state.lock().unwrap();
    if state.generation == generation && !rendered.is_empty() {
        state.samples.extend(rendered);
        active.store(true, Ordering::Release);
    }
}

fn synthesize(
    text: &str,
    output_sample_rate: u32,
) -> Result<Vec<f32>, Box<dyn Error + Send + Sync>> {
    let phrase =
        reciter::text_to_phonemes(text).map_err(|err| format!("Could not recite text: {err:?}"))?;

    let phonemes = parser::parse_phonemes(&phrase)
        .map_err(|err| format!("Could not parse phonemes: {err:?}"))?;

    let output = renderer::render(&phonemes, 64, 128, 128, 64, false);

    let mono: Vec<f32> = output
        .into_iter()
        .map(|sample| (sample as f32 - 128.0) / 128.0)
        .collect();

    if output_sample_rate == SAM_SAMPLE_RATE {
        return Ok(mono);
    }

    Ok(resample_linear(&mono, SAM_SAMPLE_RATE, output_sample_rate))
}

fn resample_linear(input: &[f32], input_rate: u32, output_rate: u32) -> Vec<f32> {
    if input.is_empty() {
        return Vec::new();
    }

    if input_rate == output_rate {
        return input.to_vec();
    }

    let ratio = output_rate as f64 / input_rate as f64;
    let output_len = (input.len() as f64 * ratio).ceil() as usize;

    let mut output = Vec::with_capacity(output_len);

    for output_index in 0..output_len {
        let source_position = output_index as f64 / ratio;

        let index = source_position.floor() as usize;
        let fraction = (source_position - index as f64) as f32;

        let a = input[index.min(input.len() - 1)];
        let b = input[(index + 1).min(input.len() - 1)];

        output.push(a + (b - a) * fraction);
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_clears_audio_and_rejects_queued_and_in_flight_speech() {
        let (speech, mut receiver) = speech_channel();
        let old_turn = speech.current();
        old_turn
            .send("Queued before cancellation".into())
            .await
            .unwrap();
        let queued = receiver.recv().await.unwrap();
        queue_rendered(
            &speech.state,
            &speech.playback_active,
            queued.generation,
            vec![0.5; 10],
        );
        assert!(speech.playback_active.load(Ordering::Acquire));

        speech.cancel();
        assert!(speech.state.lock().unwrap().samples.is_empty());
        assert!(!speech.playback_active.load(Ordering::Acquire));
        assert!(old_turn.send("Late tool speech".into()).await.is_err());
        // Synthesis that was already running must not restore cancelled audio.
        queue_rendered(
            &speech.state,
            &speech.playback_active,
            queued.generation,
            vec![0.5; 10],
        );
        assert!(speech.state.lock().unwrap().samples.is_empty());
        assert!(!speech.playback_active.load(Ordering::Acquire));

        let next_turn = speech.current();
        next_turn.send("New speech".into()).await.unwrap();
        let next = receiver.recv().await.unwrap();
        assert_ne!(next.generation, queued.generation);
        queue_rendered(
            &speech.state,
            &speech.playback_active,
            next.generation,
            vec![0.25],
        );
        assert_eq!(speech.state.lock().unwrap().samples.len(), 1);
        assert!(speech.playback_active.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn cancellation_invalidates_a_sender_waiting_for_queue_capacity() {
        let (speech, mut receiver) = speech_channel();
        for _ in 0..CHANNEL_CAPACITY {
            speech.send("Queued".into()).await.unwrap();
        }
        let pending = speech.send("Must never play".into());
        tokio::pin!(pending);
        assert!(iced::futures::poll!(&mut pending).is_pending());
        speech.cancel();
        receiver.recv().await.unwrap();
        assert!(pending.await.is_err());
        while let Ok(queued) = receiver.try_recv() {
            assert_ne!(queued.text, "Must never play");
            assert_ne!(queued.generation, speech.state.lock().unwrap().generation);
        }
    }
}
