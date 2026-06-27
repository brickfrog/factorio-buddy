//! Character control commands

use anyhow::Result;
use clap::{Args, Subcommand};

use super::parsing::{parse_position, parse_tile};
use super::ResolvedConnectionArgs;
use crate::output::Output;

#[derive(Args, Debug)]
pub struct CharacterCommand {
    #[command(subcommand)]
    pub command: CharacterSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum CharacterSubcommand {
    /// Initialize/create character
    Init {
        /// Spawn X coordinate
        #[arg(long, allow_hyphen_values = true)]
        x: Option<f64>,

        /// Spawn Y coordinate
        #[arg(long, allow_hyphen_values = true)]
        y: Option<f64>,
    },

    /// Teleport character to position (debug command, accepts floats)
    Teleport {
        /// Target position (x,y as floats)
        #[arg(allow_hyphen_values = true)]
        position: String,
    },

    /// Walk character to position
    Walk {
        /// Target tile position (x,y as integers)
        #[arg(long, allow_hyphen_values = true)]
        to: String,
    },

    /// Get character status
    Status,

    /// Get character inventory
    Inventory,
}

pub async fn execute(cmd: CharacterCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    match cmd.command {
        CharacterSubcommand::Init { x, y } => {
            if !conn.agent_id.is_legacy() && (x.is_none() || y.is_none()) {
                anyhow::bail!("named agent requires --x/--y");
            }
            let character = client
                .init_character(x.unwrap_or(0.0), y.unwrap_or(0.0))
                .await?;
            Output::new(conn.output).print(&character)?;
        }
        CharacterSubcommand::Teleport { position } => {
            let pos = parse_position(&position)?;
            client.teleport_character(pos).await?;
            println!("Teleported to ({}, {})", pos.x, pos.y);
        }
        CharacterSubcommand::Walk { to } => {
            let tile = parse_tile(&to)?;
            let pos = tile.to_world_1x1();
            client.walk_character(pos).await?;
            println!("Walking to tile ({}, {})", tile.x, tile.y);
        }
        CharacterSubcommand::Status => {
            let status = client.character_status().await?;
            Output::new(conn.output).print(&status)?;
        }
        CharacterSubcommand::Inventory => {
            let inventory = client.character_inventory().await?;
            Output::new(conn.output).print(&inventory)?;
        }
    }

    client.close().await?;
    Ok(())
}
