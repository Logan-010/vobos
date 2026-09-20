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

pub fn init_task() -> color_eyre::Result<(mpsc::Sender<String>, Arc<AtomicBool>)> {
    let (tx, mut rx) = mpsc::channel::<String>(CHANNEL_CAPACITY);
    let playback_active = Arc::new(AtomicBool::new(false));
    let worker_playback_active = Arc::clone(&playback_active);

    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<color_eyre::Result<()>>(1);

    task::spawn_blocking(move || {
        if let Err(err) = run_audio_worker(&mut rx, ready_tx, worker_playback_active) {
            tracing::error!("SAM audio worker stopped: {err}");
        }
    });

    ready_rx.recv()??;

    Ok((tx, playback_active))
}

fn run_audio_worker(
    rx: &mut mpsc::Receiver<String>,
    ready: std::sync::mpsc::SyncSender<color_eyre::Result<()>>,
    playback_active: Arc<AtomicBool>,
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
    let samples = Arc::new(Mutex::new(VecDeque::<f32>::new()));
    let output_samples = Arc::clone(&samples);
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
                let sample = match queue.pop_front() {
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

    while let Some(text) = rx.blocking_recv() {
        match synthesize(&text, sample_rate) {
            Ok(rendered) => {
                playback_active.store(true, Ordering::Release);
                samples.lock().unwrap().extend(rendered);
            }

            Err(err) => {
                eprintln!("Could not synthesize {text:?}: {err}");
            }
        }
    }

    drop(stream);

    Ok(())
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
