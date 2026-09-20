use rig::{tool::rmcp::McpClientHandler, tool::server::ToolServerHandle};
use rmcp::transport::{StreamableHttpClientTransport, TokioChildProcess};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tokio::{fs, process::Command};

#[derive(Debug, Default, Deserialize)]
pub struct McpConfig {
    #[serde(default)]
    pub servers: BTreeMap<String, ServerConfig>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "transport", rename_all = "lowercase")]
pub enum ServerConfig {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: BTreeMap<String, String>,
        cwd: Option<PathBuf>,
    },
    Http {
        url: String,
    },
}

impl McpConfig {
    pub async fn load(path: &Path) -> color_eyre::Result<Self> {
        let content = fs::read(path).await?;
        Ok(toml::from_slice(&content)?)
    }
}

pub struct Connections {
    _services: Vec<rmcp::service::RunningService<rmcp::RoleClient, McpClientHandler>>,
}

impl Connections {
    pub async fn connect(config: McpConfig, handle: ToolServerHandle) -> color_eyre::Result<Self> {
        let client_info = rmcp::model::ClientInfo::new(
            rmcp::model::ClientCapabilities::default(),
            rmcp::model::Implementation::new("vobos", env!("CARGO_PKG_VERSION")),
        );
        let mut services = Vec::with_capacity(config.servers.len());

        for (name, server) in config.servers {
            let handler = McpClientHandler::new(client_info.clone(), handle.clone());
            let service = match server {
                ServerConfig::Stdio {
                    command,
                    args,
                    env,
                    cwd,
                } => {
                    tracing::info!(server = %name, command = %command, "Connecting to local MCP server");
                    let mut process = Command::new(command);
                    process.args(args).envs(env);
                    if let Some(cwd) = cwd {
                        process.current_dir(cwd);
                    }
                    handler.connect(TokioChildProcess::new(process)?).await?
                }
                ServerConfig::Http { url } => {
                    tracing::info!(server = %name, url = %url, "Connecting to HTTP MCP server");
                    handler
                        .connect(StreamableHttpClientTransport::from_uri(url))
                        .await?
                }
            };
            services.push(service);
        }

        Ok(Self {
            _services: services,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{McpConfig, ServerConfig};

    #[test]
    fn parses_stdio_and_http_servers() {
        let config: McpConfig = toml::from_str(
            r#"
                [servers.filesystem]
                transport = "stdio"
                command = "npx"
                args = ["-y", "server-filesystem"]

                [servers.remote]
                transport = "http"
                url = "https://example.test/mcp"
            "#,
        )
        .expect("valid MCP config");

        assert!(matches!(
            config.servers["filesystem"],
            ServerConfig::Stdio { .. }
        ));
        assert!(matches!(
            config.servers["remote"],
            ServerConfig::Http { .. }
        ));
    }
}
