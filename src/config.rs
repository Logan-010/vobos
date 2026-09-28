use crate::cli::Cli;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::fs;

const DEFAULT_REPO: &str = "ggerganov/whisper.cpp";
const DEFAULT_MODEL: &str = "ggml-base.en.bin";
const DEFAULT_OLLAMA_BASE: &str = "http://localhost:11434";
const DEFAULT_OLLAMA_MODEL: &str = "qwen3.8:latest";

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub repo: String,
    pub model: String,
    pub cache: Option<PathBuf>,
    pub ollama_base: String,
    pub ollama_model: String,
}

impl Config {
    fn defaults() -> Self {
        Self {
            repo: DEFAULT_REPO.to_owned(),
            model: DEFAULT_MODEL.to_owned(),
            cache: None,
            ollama_base: DEFAULT_OLLAMA_BASE.to_owned(),
            ollama_model: DEFAULT_OLLAMA_MODEL.to_owned(),
        }
    }

    pub fn from_cli(cli: &Cli) -> Self {
        let defaults = Self::defaults();
        Self {
            repo: cli.repo.clone().unwrap_or(defaults.repo),
            model: cli.model.clone().unwrap_or(defaults.model),
            cache: cli.cache.clone(),
            ollama_base: cli.ollama_base.clone().unwrap_or(defaults.ollama_base),
            ollama_model: cli.ollama_model.clone().unwrap_or(defaults.ollama_model),
        }
    }

    pub async fn load(from: &Path, with: &Cli) -> color_eyre::Result<Self> {
        let content = fs::read(from).await?;
        let file: PartialConfig = toml::from_slice(&content)?;
        let defaults = Self::defaults();

        Ok(Self {
            repo: with.repo.clone().or(file.repo).unwrap_or(defaults.repo),
            model: with.model.clone().or(file.model).unwrap_or(defaults.model),
            cache: with.cache.clone().or(file.cache),
            ollama_base: with
                .ollama_base
                .clone()
                .or(file.ollama_base)
                .unwrap_or(defaults.ollama_base),
            ollama_model: with
                .ollama_model
                .clone()
                .or(file.ollama_model)
                .unwrap_or(defaults.ollama_model),
        })
    }
}

#[derive(Default, Deserialize)]
struct PartialConfig {
    repo: Option<String>,
    model: Option<String>,
    cache: Option<PathBuf>,
    ollama_base: Option<String>,
    ollama_model: Option<String>,
}
