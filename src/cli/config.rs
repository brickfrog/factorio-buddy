//! Configuration management

use anyhow::Result;
use clap::{Args, Subcommand};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Args, Debug)]
pub struct ConfigCommand {
    #[command(subcommand)]
    pub command: ConfigSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum ConfigSubcommand {
    /// Set connection settings
    Set {
        /// RCON host
        #[arg(long)]
        host: Option<String>,

        /// RCON port
        #[arg(long)]
        port: Option<u16>,

        /// RCON password
        #[arg(long)]
        password: Option<String>,
    },

    /// Show current configuration
    Show,

    /// Clear saved configuration
    Clear,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
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

pub async fn execute(cmd: ConfigCommand) -> Result<()> {
    match cmd.command {
        ConfigSubcommand::Set { host, port, password } => {
            let mut config = Config::load().unwrap_or_default();
            if let Some(h) = host {
                config.host = Some(h);
            }
            if let Some(p) = port {
                config.port = Some(p);
            }
            if let Some(pw) = password {
                config.password = Some(pw);
            }
            config.save()?;
            println!("Configuration saved to {}", Config::path().display());
        }
        ConfigSubcommand::Show => {
            let config = Config::load()?;
            println!("Config file: {}", Config::path().display());
            println!("Host: {}", config.host.as_deref().unwrap_or("(not set)"));
            println!("Port: {}", config.port.map(|p| p.to_string()).unwrap_or_else(|| "(not set)".to_string()));
            println!("Password: {}", if config.password.is_some() { "(set)" } else { "(not set)" });
        }
        ConfigSubcommand::Clear => {
            Config::clear()?;
            println!("Configuration cleared");
        }
    }
    Ok(())
}
