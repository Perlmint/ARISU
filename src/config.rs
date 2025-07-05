use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub auth_id: String,
    pub auth_password: String,
    pub certificate: PathBuf,
    pub key: PathBuf,
}

impl Config {
    pub fn load() -> anyhow::Result<Option<Self>> {
        let config_path = Self::config_path()?;
        if !config_path.exists() {
            return Ok(None);
        }

        let content = std::fs::read_to_string(&config_path)
            .with_context(|| format!("Failed to read config file: {}", config_path.display()))?;
        let config: Config = toml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", config_path.display()))?;
        Ok(Some(config))
    }

    pub fn save(&self) -> anyhow::Result<()> {
        let config_path = Self::config_path()?;
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create config directory: {}", parent.display())
            })?;
        }

        let content = toml::to_string_pretty(self).context("Failed to serialize config to TOML")?;
        std::fs::write(&config_path, content)
            .with_context(|| format!("Failed to write config file: {}", config_path.display()))?;
        Ok(())
    }

    fn config_path() -> anyhow::Result<PathBuf> {
        let config_dir = dirs::config_dir().context("Could not find config directory")?;
        Ok(config_dir.join("ARISU").join("config.toml"))
    }
}
