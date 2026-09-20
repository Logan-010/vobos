use crate::{
    agent::{
        consts::PREAMBLE,
        tools::{Cmd, NewSession, Say},
    },
    config::Config,
};
use rig::{Agent, client::AgentClientExt, memory::InMemoryConversationMemory, providers::ollama};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub async fn build(
    config: &Config,
    sam: mpsc::Sender<String>,
    session_id: Arc<Mutex<String>>,
) -> color_eyre::Result<Agent> {
    tracing::info!("Connecting to {}...", config.ollama_base);

    let client = ollama::Client::builder()
        .base_url(&config.ollama_base)
        .api_key("")
        .build()?;

    tracing::info!("Using model {}...", config.ollama_model);

    Ok(client
        .agent(&config.ollama_model)
        .preamble(PREAMBLE)
        .tool(Say::new(sam))
        .tool(Cmd)
        .tool(NewSession::new(session_id))
        .memory(InMemoryConversationMemory::new())
        .default_max_turns(25)
        .build())
}
