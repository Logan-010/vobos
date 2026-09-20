use crate::{
    agent::{
        consts::PREAMBLE,
        tools::{Cmd, NewSession, Say},
    },
    config::Config,
    mcp::{Connections, McpConfig},
};
use rig::{
    Agent, client::AgentClientExt, memory::InMemoryConversationMemory, providers::ollama,
    tool::server::ToolServer,
};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub async fn build(
    config: &Config,
    sam: mpsc::Sender<String>,
    session_id: Arc<Mutex<String>>,
    mcp_config: McpConfig,
) -> color_eyre::Result<(Agent, Connections)> {
    tracing::info!("Connecting to {}...", config.ollama_base);

    let client = ollama::Client::builder()
        .base_url(&config.ollama_base)
        .api_key("")
        .build()?;

    tracing::info!("Using model {}...", config.ollama_model);

    let tool_server = ToolServer::new()
        .tool(Say::new(sam))
        .tool(Cmd)
        .tool(NewSession::new(session_id))
        .run();
    let connections = Connections::connect(mcp_config, tool_server.clone()).await?;

    Ok((
        client
            .agent(&config.ollama_model)
            .preamble(PREAMBLE)
            .tool_server_handle(tool_server)
            .memory(InMemoryConversationMemory::new())
            .default_max_turns(25)
            .build(),
        connections,
    ))
}
