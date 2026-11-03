use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub adb_path: String,
    pub mcp_bind: String,
    pub mcp_port: u16,
    pub max_fps: u32,
    pub max_size: u32,
    pub video_bit_rate: u32,
    pub stay_awake: bool,
    pub manual_input: bool,
    /// If set, /mcp requires this bearer token.
    pub auth_token: Option<String>,
    /// How long screenshot() waits for a fresh frame after an action.
    pub settle_ms: u32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            adb_path: std::env::var("ADB").unwrap_or_else(|_| "adb".to_string()),
            mcp_bind: "127.0.0.1".to_string(),
            mcp_port: 18765,
            max_fps: 0,
            max_size: 0,
            video_bit_rate: 8_000_000,
            stay_awake: false,
            manual_input: true,
            auth_token: None,
            settle_ms: 120,
        }
    }
}

impl Config {
    pub fn path() -> Option<PathBuf> {
        dirs::config_dir().map(|dir| dir.join("scrcpy-agent").join("config.toml"))
    }

    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(cfg) => cfg,
                Err(err) => {
                    log::warn!("invalid config at {}: {err}", path.display());
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) -> Result<()> {
        let Some(path) = Self::path() else {
            return Ok(());
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self)?;
        fs::write(&path, text).with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }
}
