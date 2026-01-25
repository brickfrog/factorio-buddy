//! Configuration management CLI commands

use anyhow::Result;
use clap::{Args, Subcommand};

// Re-export config types from the shared config module
pub use crate::config::{BroadcastConfig, Config, TtsConfig};

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
