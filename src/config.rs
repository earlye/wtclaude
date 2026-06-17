use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Deserialize)]
pub struct Config {
    #[serde(rename = "default-mode")]
    pub default_mode: String,
    pub modes: HashMap<String, Mode>,
}

#[derive(Deserialize, Default)]
pub struct UserConfig {
    #[serde(default)]
    pub allowlist: Vec<String>,
}

#[derive(Deserialize)]
pub struct Mode {
    #[serde(rename = "claude-flags", default)]
    pub claude_flags: Vec<String>,
}

const DEFAULT_CONFIG: &str = include_str!("default_config.yml");

pub fn load_user() -> Result<UserConfig> {
    let home = std::env::var("HOME").context("HOME not set")?;
    let path = PathBuf::from(&home).join(".config/wtclaude/wtclaude.yml");
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_yaml::from_str(&s).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(UserConfig::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

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
