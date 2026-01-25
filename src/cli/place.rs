//! Entity placement commands

use anyhow::Result;
use clap::Args;

use super::ResolvedConnectionArgs;
use crate::client::FactorioClient;
use crate::output::Output;
use crate::world::{Direction, Position};

#[derive(Args, Debug)]
pub struct PlaceCommand {
    /// Entity name to place
    pub entity_name: String,

    /// Position to place at (x,y)
    #[arg(long, allow_hyphen_values = true)]
    pub at: String,

    /// Direction (n, e, s, w, or 0-7)
    #[arg(long, default_value = "n")]
    pub direction: String,
}

pub async fn execute(cmd: PlaceCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = FactorioClient::connect(&conn.host, conn.port, &conn.password).await?;

    let pos = parse_position(&cmd.at)?;
    let dir = parse_direction(&cmd.direction)?;

    let entity = client.place_entity(&cmd.entity_name, pos, dir).await?;
    Output::new(conn.output).print(&entity)?;

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

fn parse_direction(s: &str) -> Result<Direction> {
    match s.to_lowercase().as_str() {
        "n" | "north" | "0" => Ok(Direction::North),
        "ne" | "northeast" | "1" => Ok(Direction::NorthEast),
        "e" | "east" | "2" => Ok(Direction::East),
        "se" | "southeast" | "3" => Ok(Direction::SouthEast),
        "s" | "south" | "4" => Ok(Direction::South),
        "sw" | "southwest" | "5" => Ok(Direction::SouthWest),
        "w" | "west" | "6" => Ok(Direction::West),
        "nw" | "northwest" | "7" => Ok(Direction::NorthWest),
        _ => anyhow::bail!("Invalid direction: {}", s),
    }
}
