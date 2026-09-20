use super::consts::PREAMBLE;
use super::tools::Say;
use rig::{Agent, client::AgentClientExt, providers::ollama};
use tokio::sync::mpsc;

pub async fn build(
    ollama_url: String,
    ollama_model: String,
    sam: mpsc::Sender<String>,
) -> color_eyre::Result<Agent> {
    let client = ollama::Client::builder()
        .base_url(ollama_url)
        .api_key("")
        .build()?;

    Ok(client
        .agent(ollama_model)
        .preamble(PREAMBLE)
        .tool(Say::new(sam))
        .build())
}
