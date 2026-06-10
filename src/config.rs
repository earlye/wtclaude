use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;

#[derive(Deserialize)]
pub struct Config {
    #[serde(rename = "default-mode")]
    pub default_mode: String,
    pub modes: HashMap<String, Mode>,
}

#[derive(Deserialize)]
pub struct Mode {
    #[serde(rename = "claude-flags", default)]
    pub claude_flags: Vec<String>,
}

const DEFAULT_CONFIG: &str = include_str!("default_config.yml");

pub fn load() -> Result<Config> {
    let exe = std::env::current_exe().context("resolving executable path")?;
    let path = exe
        .parent()
        .context("executable has no parent directory")?
        .join("wtclaude.yml");

    let content = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DEFAULT_CONFIG.to_string(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    serde_yaml::from_str(&content).context("parsing config")
}
