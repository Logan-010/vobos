use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = env!("CARGO_PKG_NAME"))]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(about = env!("CARGO_PKG_DESCRIPTION"))]
#[command(author = env!("CARGO_PKG_AUTHORS"))]
pub struct Cli {
    /// Set log level
    #[arg(long, short = 'L', env = "RUST_LOG", default_value_t = String::from(concat!(env!("CARGO_PKG_NAME"), "=info,warn")))]
    pub logging: String,

    /// Huggingface repo
    #[arg(long, short = 'r', env = "VOBOS_STT_REPO", default_value_t = String::from("ggerganov/whisper.cpp"))]
    pub repo: String,

    /// Model name
    #[arg(long, short = 'm', env = "VOBOS_STT_MODEL", default_value_t = String::from("ggml-base.en.bin"))]
    pub model: String,

    /// Model cache path
    #[arg(long, short = 'c', env = "VOBOS_STT_CACHE")]
    pub cache: Option<PathBuf>,

    /// Ollama base url
    #[arg(long, short = 'o', env = "OLLAMA_URL", default_value_t = String::from("http://localhost:11434"))]
    pub ollama_base: String,

    /// Ollama model
    #[arg(long,short =  'M', env = "VOBOS_LLM_MODEL", default_value_t = String::from("qwen3.6:latest"))]
    pub ollama_model: String,
}
