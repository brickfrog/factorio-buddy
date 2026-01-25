//! Entity removal commands

use anyhow::Result;
use clap::Args;

use super::ResolvedConnectionArgs;
use crate::client::FactorioClient;
use crate::world::Position;

#[derive(Args, Debug)]
pub struct RemoveCommand {
    /// Remove entity at position
    #[arg(long, allow_hyphen_values = true)]
    pub at: Option<String>,

    /// Remove entity by unit number
    #[arg(long)]
    pub unit_number: Option<u32>,
}

pub async fn execute(cmd: RemoveCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = FactorioClient::connect(&conn.host, conn.port, &conn.password).await?;

    if let Some(pos_str) = cmd.at {
        let pos = parse_position(&pos_str)?;
        client.remove_entity_at(pos).await?;
        println!("Removed entity at ({}, {})", pos.x, pos.y);
    } else if let Some(unit_number) = cmd.unit_number {
        client.remove_entity(unit_number).await?;
        println!("Removed entity #{}", unit_number);
    } else {
        anyhow::bail!("Either --at or --unit-number must be specified");
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
