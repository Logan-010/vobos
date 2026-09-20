mod agent;
mod cli;
mod sam;
mod whisper;

use clap::Parser;
use cli::Cli;
use rig::completion::Prompt;
use sam::Sam;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};
use whisper::Whisper;

#[tokio::main]
async fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();

    if let Err(e) = tracing_subscriber::registry()
        .with(EnvFilter::new(&cli.logging))
        .with(tracing_subscriber::fmt::layer())
        .try_init()
    {
        eprintln!("Failed to initialize logger: {}", e);
    }

    tracing::info!("Initializing TTS...");
    let (sam, playback_active) = Sam::new()?;

    tracing::info!("Initializing LLM...");
    let agent = agent::build(cli.ollama_base, cli.ollama_model, sam).await?;

    tracing::info!("Initializing STT...");
    let whisper = Whisper::new(cli.repo, cli.model, cli.cache).await?;
    let agent_running = Arc::new(AtomicBool::new(false));

    let mut rx = whisper.transcribe_mic(playback_active, Arc::clone(&agent_running))?;

    while let Some(segment) = rx.recv().await {
        if segment.text == "[BLANK_AUDIO]" {
            continue;
        }

        tracing::info!("Got speech segment: {:?}", segment);

        tracing::info!("Prompting LLM...");
        agent_running.store(true, Ordering::Release);
        let result = agent.prompt(segment.text).await;
        agent_running.store(false, Ordering::Release);

        let res = match result {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("Agent error: {}", e);
                continue;
            }
        };

        tracing::info!("LLM result: {}", res);
    }

    Ok(())
}
