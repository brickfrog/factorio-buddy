//! Entity placement commands

use anyhow::Result;
use clap::Args;

use super::parsing::{parse_direction, parse_tile};
use super::ResolvedConnectionArgs;
use crate::output::Output;
use crate::world::entity_size;

#[derive(Args, Debug)]
pub struct PlaceCommand {
    /// Entity name to place
    pub entity_name: String,

    /// Tile position to place at (x,y as integers)
    #[arg(long, allow_hyphen_values = true)]
    pub at: String,

    /// Direction (n, e, s, w, or 0-7)
    #[arg(long, default_value = "n")]
    pub direction: String,
}

pub async fn execute(cmd: PlaceCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    let tile = parse_tile(&cmd.at)?;
    let dir = parse_direction(&cmd.direction)?;

    // Get entity size and compute world position (center of entity)
    let (width, height) = entity_size(&cmd.entity_name);
    let world_pos = tile.to_world(width, height);

    // Check proximity before placing
    client
        .ensure_proximity_to_position(world_pos, crate::client::PROXIMITY_RANGE_PLACE)
        .await?;

    let entity = client
        .place_entity(&cmd.entity_name, world_pos, dir)
        .await?;
    Output::new(conn.output).print(&entity)?;

    client.close().await?;
    Ok(())
}
