use clap::Parser;
use std::path::PathBuf;

#[derive(Clone, Parser)]
#[command(name = env!("CARGO_PKG_NAME"))]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = env!("CARGO_PKG_DESCRIPTION"))]
#[command(author = env!("CARGO_PKG_AUTHORS"))]
pub struct Cli {
    /// Run the voice agent without opening the GUI
    #[arg(long)]
    pub headless: bool,

    /// Set log level
    #[arg(long, short = 'L', env = "RUST_LOG", default_value_t = String::from(concat!(env!("CARGO_PKG_NAME"), "=info,warn")))]
    pub logging: String,

    /// Custom config path
    #[arg(long, short = 'c', env = "VOBOS_CONFIG")]
    pub config: Option<PathBuf>,

    /// Custom MCP config path
    #[arg(long, env = "VOBOS_MCP_CONFIG")]
    pub mcp_config: Option<PathBuf>,

    /// Huggingface repo
    #[arg(long, short = 'r', env = "VOBOS_STT_REPO")]
    pub repo: Option<String>,

    /// Model name
    #[arg(long, short = 'm', env = "VOBOS_STT_MODEL")]
    pub model: Option<String>,

    /// Model cache path
    #[arg(long, short = 'C', env = "VOBOS_STT_CACHE")]
    pub cache: Option<PathBuf>,

    /// Ollama base url
    #[arg(long, short = 'o', env = "OLLAMA_URL")]
    pub ollama_base: Option<String>,

    /// Ollama model
    #[arg(long, short = 'M', env = "VOBOS_LLM_MODEL")]
    pub ollama_model: Option<String>,
}
