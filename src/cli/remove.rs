//! Entity removal commands

use anyhow::Result;
use clap::Args;

use super::parsing::parse_position;
use super::ResolvedConnectionArgs;

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
    let mut client = conn.connect_client().await?;

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
