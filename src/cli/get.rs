//! Query commands for game state

use anyhow::Result;
use clap::{Args, Subcommand};

use super::ResolvedConnectionArgs;
use crate::client::FactorioClient;
use crate::output::Output;
use crate::world::{Area, Position};

#[derive(Args, Debug)]
pub struct GetCommand {
    #[command(subcommand)]
    pub command: GetSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum GetSubcommand {
    /// Get current game tick
    Tick,

    /// List surfaces
    Surfaces,

    /// Query entities in an area
    Entities {
        /// Area to search (x1,y1,x2,y2)
        #[arg(long, allow_hyphen_values = true)]
        area: String,

        /// Filter by entity type
        #[arg(long, name = "type")]
        entity_type: Option<String>,

        /// Filter by entity name
        #[arg(long)]
        name: Option<String>,
    },

    /// Get a specific entity by unit number
    Entity {
        /// Entity unit number
        unit_number: u32,
    },

    /// Get an entity's inventories
    EntityInventory {
        /// Entity unit number
        unit_number: u32,
    },

    /// Query resources in an area
    Resources {
        /// Area to search (x1,y1,x2,y2)
        #[arg(long, allow_hyphen_values = true)]
        area: Option<String>,

        /// Filter by resource type
        #[arg(long, name = "type")]
        resource_type: Option<String>,

        /// Find nearest resource from position
        #[arg(long)]
        nearest: Option<String>,

        /// Origin position for nearest search (x,y)
        #[arg(long, allow_hyphen_values = true)]
        from: Option<String>,
    },

    /// Query tiles in an area
    Tiles {
        /// Area to search (x1,y1,x2,y2)
        #[arg(long, allow_hyphen_values = true)]
        area: String,
    },

    /// Get a specific tile
    Tile {
        /// Position (x,y)
        #[arg(allow_hyphen_values = true)]
        position: String,
    },
}

pub async fn execute(cmd: GetCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = FactorioClient::connect(&conn.host, conn.port, &conn.password).await?;

    match cmd.command {
        GetSubcommand::Tick => {
            let tick = client.get_tick().await?;
            Output::new(conn.output).print(&tick)?;
        }
        GetSubcommand::Surfaces => {
            let surfaces = client.get_surfaces().await?;
            Output::new(conn.output).print(&surfaces)?;
        }
        GetSubcommand::Entities {
            area,
            entity_type,
            name,
        } => {
            let area = parse_area(&area)?;
            let entities = client
                .find_entities(area, entity_type.as_deref(), name.as_deref())
                .await?;
            Output::new(conn.output).print(&entities)?;
        }
        GetSubcommand::Entity { unit_number } => {
            let entity = client.get_entity(unit_number).await?;
            Output::new(conn.output).print(&entity)?;
        }
        GetSubcommand::EntityInventory { unit_number } => {
            let inv = client.get_entity_inventory(unit_number).await?;
            println!("{}", serde_json::to_string_pretty(&inv)?);
        }
        GetSubcommand::Resources {
            area,
            resource_type,
            nearest,
            from,
        } => {
            if let (Some(resource_name), Some(from_pos)) = (nearest, from) {
                let pos = parse_position(&from_pos)?;
                let resource = client.find_nearest_resource(&resource_name, pos).await?;
                Output::new(conn.output).print(&resource)?;
            } else if let Some(area_str) = area {
                let area = parse_area(&area_str)?;
                let resources = client
                    .find_resources(area, resource_type.as_deref())
                    .await?;
                Output::new(conn.output).print(&resources)?;
            } else {
                anyhow::bail!("Either --area or --nearest with --from must be specified");
            }
        }
        GetSubcommand::Tiles { area } => {
            let area = parse_area(&area)?;
            let tiles = client.get_tiles(area).await?;
            Output::new(conn.output).print(&tiles)?;
        }
        GetSubcommand::Tile { position } => {
            let pos = parse_position(&position)?;
            let tile = client.get_tile(pos).await?;
            Output::new(conn.output).print(&tile)?;
        }
    }

    client.close().await?;
    Ok(())
}

fn parse_area(s: &str) -> Result<Area> {
    let parts: Vec<f64> = s
        .split(',')
        .map(|p| p.trim().parse())
        .collect::<Result<_, _>>()?;
    if parts.len() != 4 {
        anyhow::bail!("Area must be x1,y1,x2,y2");
    }
    Ok(Area {
        left_top: Position {
            x: parts[0],
            y: parts[1],
        },
        right_bottom: Position {
            x: parts[2],
            y: parts[3],
        },
    })
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
