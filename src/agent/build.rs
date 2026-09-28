use crate::{
    agent::{
        consts::preamble,
        tools::{Cmd, Image, Input, NewSession, Say},
        vision::VisionModel,
    },
    config::Config,
    mcp::{Connections, McpConfig},
};
use rig::{
    Agent, AgentBuilder, client::CompletionClient, memory::InMemoryConversationMemory,
    providers::ollama, tool::server::ToolServer,
};
use std::sync::{Arc, Mutex};

pub async fn build(
    config: &Config,
    sam: crate::sam::Speech,
    session_id: Arc<Mutex<String>>,
    mcp_config: McpConfig,
    gui: bool,
) -> color_eyre::Result<(Agent, Connections)> {
    tracing::info!("Connecting to {}...", config.ollama_base);

    let client = ollama::Client::builder()
        .base_url(&config.ollama_base)
        .api_key("")
        .build()?;

    tracing::info!("Using model {}...", config.ollama_model);

    let instructions = preamble(gui);
    let tool_server = ToolServer::new()
        .tool(Say::new(sam, None))
        .tool(Cmd)
        .tool(Image)
        .tool(NewSession::new(session_id))
        .tool(Input::new()?);
    #[cfg(any(feature = "wayland", feature = "x11"))]
    let tool_server = tool_server.tool(crate::agent::tools::Screencap);
    let tool_server = tool_server.run();
    let connections = Connections::connect(mcp_config, tool_server.clone()).await?;

    Ok((
        AgentBuilder::new(VisionModel(client.completion_model(&config.ollama_model)))
            .preamble(&instructions)
            .tool_server_handle(tool_server)
            .memory(InMemoryConversationMemory::new())
            .default_max_turns(25)
            .build(),
        connections,
    ))
}
