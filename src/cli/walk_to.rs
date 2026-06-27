//! Walk-to command - smooth navigation to a position

use anyhow::Result;
use clap::Args;

use super::ResolvedConnectionArgs;
use crate::world::TilePos;

#[derive(Args, Debug)]
pub struct WalkToCommand {
    /// Target tile position (x,y as integers)
    #[arg(allow_hyphen_values = true)]
    pub position: String,

    /// Run instead of walk (faster)
    #[arg(long, short)]
    pub run: bool,

    /// Use A* pathfinding to avoid obstacles
    #[arg(long, short = 'p')]
    pub pathfind: bool,

    /// Search radius for pathfinding obstacle detection (tiles)
    #[arg(long, default_value = "20")]
    pub search_radius: u32,
}

pub async fn execute(cmd: WalkToCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let parts: Vec<&str> = cmd.position.split(',').collect();
    if parts.len() != 2 {
        anyhow::bail!("Position must be x,y (integers)");
    }

    let x: i32 = parts[0].trim().parse().map_err(|_| {
        anyhow::anyhow!("X coordinate must be an integer, got '{}'", parts[0].trim())
    })?;
    let y: i32 = parts[1].trim().parse().map_err(|_| {
        anyhow::anyhow!("Y coordinate must be an integer, got '{}'", parts[1].trim())
    })?;

    let tile = TilePos::new(x, y);
    // For walking, target the center of the tile (same as 1x1 entity)
    let target = tile.to_world_1x1();

    let mut client = conn.connect_client().await?;

    let start = client.get_character_position().await?;

    if cmd.pathfind {
        println!(
            "Planning path from ({:.0}, {:.0}) to ({:.0}, {:.0}) with A*...",
            start.x, start.y, target.x, target.y
        );
    } else {
        println!(
            "Walking from ({:.0}, {:.0}) to ({:.0}, {:.0})...",
            start.x, start.y, target.x, target.y
        );
    }

    let result = if cmd.pathfind {
        client.walk_to_pathfind(target, cmd.search_radius).await?
    } else {
        client.walk_to(target, cmd.run).await?
    };

    if result.arrived {
        println!(
            "Arrived at ({:.0}, {:.0})",
            result.final_position.x, result.final_position.y
        );
    } else {
        println!(
            "Stopped at ({:.0}, {:.0}) - {}",
            result.final_position.x,
            result.final_position.y,
            result.reason.unwrap_or_else(|| "unknown".to_string())
        );
    }
    println!("Distance walked: {:.1} tiles", result.distance_walked);

    client.close().await?;
    Ok(())
}
