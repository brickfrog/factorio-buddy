//! Character control commands

use anyhow::Result;
use clap::{Args, Subcommand};

use super::ResolvedConnectionArgs;
use crate::client::FactorioClient;
use crate::output::Output;
use crate::world::Position;

#[derive(Args, Debug)]
pub struct CharacterCommand {
    #[command(subcommand)]
    pub command: CharacterSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum CharacterSubcommand {
    /// Initialize/create character at spawn
    Init,

    /// Teleport character to position
    Teleport {
        /// Target position (x,y)
        #[arg(allow_hyphen_values = true)]
        position: String,
    },

    /// Walk character to position (pathfinding)
    Walk {
        /// Target position
        #[arg(long, allow_hyphen_values = true)]
        to: String,
    },

    /// Get character status
    Status,

    /// Get character inventory
    Inventory,
}

pub async fn execute(cmd: CharacterCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = FactorioClient::connect(&conn.host, conn.port, &conn.password).await?;

    match cmd.command {
        CharacterSubcommand::Init => {
            let character = client.init_character().await?;
            Output::new(conn.output).print(&character)?;
        }
        CharacterSubcommand::Teleport { position } => {
            let pos = parse_position(&position)?;
            client.teleport_character(pos).await?;
            println!("Teleported to ({}, {})", pos.x, pos.y);
        }
        CharacterSubcommand::Walk { to } => {
            let pos = parse_position(&to)?;
            client.walk_character(pos).await?;
            println!("Walking to ({}, {})", pos.x, pos.y);
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

fn parse_position(s: &str) -> Result<Position> {
    let parts: Vec<f64> = s
        .split(',')
        .map(|p| p.trim().parse())
        .collect::<Result<_, _>>()?;
    if parts.len() != 2 {
        anyhow::bail!("Position must be x,y");
    }
    Ok(Position {
        x: parts[0],
        y: parts[1],
    })
}
