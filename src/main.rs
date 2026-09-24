#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod audio;
mod cli;
mod config;
mod gui;
mod mcp;
mod sam;
mod whisper;

use clap::Parser;
use cli::Cli;
use color_eyre::eyre::ContextCompat;
use config::Config;
use directories::ProjectDirs;
use rig::completion::Prompt;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use tokio::fs;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};
use whisper::Whisper;

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();

    if let Err(e) = tracing_subscriber::registry()
        .with(EnvFilter::new(&cli.logging))
        .with(tracing_subscriber::fmt::layer())
        .try_init()
    {
        eprintln!("Failed to initialize logger: {}", e);
    }

    if !cli.headless {
        return gui::run(cli).map_err(|error| color_eyre::eyre::eyre!(error.to_string()));
    }

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run_agent(cli))
}

async fn load_config(cli: &Cli) -> color_eyre::Result<(Config, mcp::McpConfig)> {
    let config = match cli.config.as_ref() {
        Some(p) => Config::load(p, cli).await?,
        None => {
            let from = ProjectDirs::from("com", "seedse", "vobos")
                .context("Expected project directories")?
                .config_dir()
                .join("config.toml");

            if fs::try_exists(&from).await? {
                Config::load(&from, cli).await?
            } else {
                Config::from_cli(cli)
            }
        }
    };

    let mcp_path = cli.mcp_config.clone().unwrap_or_else(|| {
        ProjectDirs::from("com", "seedse", "vobos")
            .expect("Expected project directories")
            .config_dir()
            .join("mcp.toml")
    });
    let mcp_config = if fs::try_exists(&mcp_path).await? {
        mcp::McpConfig::load(&mcp_path).await?
    } else {
        mcp::McpConfig::default()
    };

    Ok((config, mcp_config))
}

async fn run_agent(cli: Cli) -> color_eyre::Result<()> {
    let (config, mcp_config) = load_config(&cli).await?;

    tracing::info!("Initializing TTS...");
    let (sam, playback_active) = sam::init_task()?;
    let session_id = Arc::new(Mutex::new("default".to_owned()));

    tracing::info!("Initializing LLM...");
    let (agent, _mcp_connections) =
        agent::build(&config, sam, Arc::clone(&session_id), mcp_config, None).await?;

    tracing::info!("Initializing STT...");
    let whisper = Whisper::new(
        config.repo.clone(),
        config.model.clone(),
        config.cache.clone(),
    )
    .await?;
    let agent_running = Arc::new(AtomicBool::new(false));

    let mut rx = whisper.transcribe_mic(playback_active, Arc::clone(&agent_running))?;

    tracing::info!("Starting vOBOS");
    while let Some(segment) = rx.recv().await {
        if segment.text == "[BLANK_AUDIO]" {
            continue;
        }

        tracing::info!("Got speech segment: {:?}", segment);

        tracing::info!("Prompting LLM...");
        agent_running.store(true, Ordering::Release);
        let conversation_id = session_id
            .lock()
            .map_err(|_| color_eyre::eyre::eyre!("Session state is unavailable"))?
            .clone();
        let result = agent
            .prompt(segment.text)
            .conversation(conversation_id)
            .await;
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
