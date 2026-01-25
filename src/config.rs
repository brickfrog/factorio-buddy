//! Configuration types for factorioctl
//!
//! These types are shared between the CLI and MCP server.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Main configuration struct loaded from .factorioctl.json
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub broadcast: Option<BroadcastConfig>,
}

impl Config {
    /// Get the config file path
    pub fn path() -> PathBuf {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(".factorioctl.json")
    }

    /// Load config from file
    pub fn load() -> Result<Self> {
        let path = Self::path();
        if path.exists() {
            let content = std::fs::read_to_string(&path)?;
            Ok(serde_json::from_str(&content)?)
        } else {
            Ok(Self::default())
        }
    }

    /// Save config to file
    pub fn save(&self) -> Result<()> {
        let path = Self::path();
        let content = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        Ok(())
    }

    /// Clear the config file
    pub fn clear() -> Result<()> {
        let path = Self::path();
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }
}

/// Configuration for thought broadcasting (in-game display and TTS)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BroadcastConfig {
    /// Display messages as flying text near the character
    #[serde(default)]
    pub flying_text: bool,

    /// Display messages in the game console (game.print)
    #[serde(default = "default_true")]
    pub console: bool,

    /// TTS configuration
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts: Option<TtsConfig>,
}

impl Default for BroadcastConfig {
    fn default() -> Self {
        Self {
            flying_text: false,
            console: true,
            tts: None,
        }
    }
}

/// Text-to-speech configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TtsConfig {
    /// Enable TTS output
    #[serde(default)]
    pub enabled: bool,

    /// TTS backend: "say" (macOS) or "openai"
    #[serde(default = "default_backend")]
    pub backend: String,

    /// Voice name (backend-specific)
    /// macOS say: "Alex", "Samantha", "Daniel", "Zarvox", "Fred", etc.
    /// OpenAI: "alloy", "echo", "fable", "onyx", "nova", "shimmer"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,

    /// OpenAI API key (can also use OPENAI_API_KEY env var)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub openai_api_key: Option<String>,

    /// Speaking rate multiplier (default 1.0)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate: Option<f32>,
}

impl Default for TtsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            backend: "say".to_string(),
            voice: None,
            openai_api_key: None,
            rate: None,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_backend() -> String {
    "say".to_string()
}
