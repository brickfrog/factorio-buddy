//! Build command - high-level construction operations

use anyhow::Result;
use clap::{Args, Subcommand};

use super::parsing::{parse_position, parse_tile};
use super::ResolvedConnectionArgs;
use crate::output::{Output, OutputFormat};
use crate::world::{entity_size, Direction};

#[derive(Args, Debug)]
pub struct BuildCommand {
    #[command(subcommand)]
    pub command: BuildSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum BuildSubcommand {
    /// Place multiple drills on a resource patch
    DrillArray {
        /// Number of drills to place
        #[arg(long, default_value = "1")]
        count: u32,

        /// Resource type to mine (iron-ore, copper-ore, coal, stone)
        #[arg(long)]
        resource: String,

        /// Search near this tile position (x,y as integers)
        #[arg(long, allow_hyphen_values = true)]
        near: Option<String>,

        /// Drill type (burner-mining-drill or electric-mining-drill)
        #[arg(long, default_value = "burner-mining-drill")]
        drill_type: String,

        /// Direction drills should face (for output)
        #[arg(long, default_value = "south")]
        direction: String,
    },

    /// Plan a patch-edge miner and output belt without placing anything
    EdgeMiner {
        /// Resource type to mine (iron-ore, copper-ore, coal, stone)
        #[arg(long)]
        resource: String,

        /// Target resource area center as x,y
        #[arg(long, allow_hyphen_values = true)]
        near: String,

        /// Search radius
        #[arg(long, default_value = "25")]
        radius: u32,

        /// Drill type (burner-mining-drill or electric-mining-drill)
        #[arg(long, default_value = "burner-mining-drill")]
        drill_type: String,

        /// Maximum candidate placements to return
        #[arg(long, default_value = "10")]
        limit: u32,
    },

    /// Plan a direct drill-output belt/inserter/furnace smelter without placing anything
    DirectSmelter {
        /// Existing drill unit number. If omitted, provide --output and --output-direction.
        #[arg(long)]
        drill_unit_number: Option<u32>,

        /// Drill output belt tile as x,y from build_edge_miner/get_machine_belt_positions
        #[arg(long, allow_hyphen_values = true)]
        output: Option<String>,

        /// Direction the output belt should face
        #[arg(long, default_value = "south")]
        output_direction: String,

        /// Furnace type
        #[arg(long, default_value = "stone-furnace")]
        furnace_type: String,

        /// Inserter type
        #[arg(long, default_value = "burner-inserter")]
        inserter_type: String,

        /// Belt type
        #[arg(long, default_value = "transport-belt")]
        belt_type: String,

        /// Search radius for furnace placement around the output tile
        #[arg(long, default_value = "6")]
        radius: u32,
    },

    /// Place a line of furnaces for smelting
    SmelterLine {
        /// Number of furnaces to place
        #[arg(long, default_value = "1")]
        count: u32,

        /// Starting tile position (x,y as integers)
        #[arg(long, allow_hyphen_values = true)]
        at: String,

        /// Furnace type (stone-furnace or steel-furnace)
        #[arg(long, default_value = "stone-furnace")]
        furnace_type: String,

        /// Direction of the line (east or south)
        #[arg(long, default_value = "east")]
        direction: String,

        /// Spacing between furnaces
        #[arg(long, default_value = "2")]
        spacing: u32,
    },

    /// Place entities from a JSON plan
    FromPlan {
        /// JSON array of entities to place: [{"name":"stone-furnace","position":[x,y],"direction":"north"},...]
        plan: String,
    },
}

pub async fn execute(cmd: BuildCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    match cmd.command {
        BuildSubcommand::DrillArray {
            count,
            resource,
            near,
            drill_type,
            direction,
        } => {
            let near_pos = if let Some(pos_str) = near {
                let tile = parse_tile(&pos_str)?;
                // Drills are 2x2, convert to world position
                let (w, h) = entity_size(&drill_type);
                let world_pos = tile.to_world(w, h);
                Some((world_pos.x, world_pos.y))
            } else {
                None
            };

            let result = client
                .build_drill_array(count, &resource, near_pos, &drill_type, &direction)
                .await?;

            if conn.output == OutputFormat::Json {
                Output::new(conn.output).print(&result)?;
            } else {
                println!(
                    "Placed {} of {} {} on {}",
                    result.placed, count, drill_type, resource
                );
                if result.placed < count {
                    println!(
                        "Failed to place {}: {}",
                        count - result.placed,
                        result.errors.join(", ")
                    );
                }
                for entity in &result.entities {
                    println!(
                        "  #{} at ({:.1}, {:.1})",
                        entity.unit_number.unwrap_or(0),
                        entity.position.x,
                        entity.position.y
                    );
                }
            }
        }

        BuildSubcommand::EdgeMiner {
            resource,
            near,
            radius,
            drill_type,
            limit,
        } => {
            let center = parse_position(&near)?;
            let result = client
                .build_edge_miner(&resource, center, radius, &drill_type, limit)
                .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }

        BuildSubcommand::DirectSmelter {
            drill_unit_number,
            output,
            output_direction,
            furnace_type,
            inserter_type,
            belt_type,
            radius,
        } => {
            let output = match output {
                Some(value) => {
                    let position = parse_position(&value)?;
                    let direction = Direction::parse(&output_direction).ok_or_else(|| {
                        anyhow::anyhow!("invalid output direction: {}", output_direction)
                    })?;
                    Some((position, direction))
                }
                None => None,
            };
            let result = client
                .build_direct_smelter(
                    drill_unit_number,
                    output,
                    &furnace_type,
                    &inserter_type,
                    &belt_type,
                    radius,
                )
                .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }

        BuildSubcommand::SmelterLine {
            count,
            at,
            furnace_type,
            direction,
            spacing,
        } => {
            let tile = parse_tile(&at)?;
            // Furnaces are 2x2, convert to world position
            let (w, h) = entity_size(&furnace_type);
            let world_pos = tile.to_world(w, h);

            let result = client
                .build_smelter_line(
                    count,
                    (world_pos.x, world_pos.y),
                    &furnace_type,
                    &direction,
                    spacing,
                )
                .await?;

            if conn.output == OutputFormat::Json {
                Output::new(conn.output).print(&result)?;
            } else {
                println!("Placed {} of {} {}", result.placed, count, furnace_type);
                if result.placed < count {
                    println!(
                        "Failed to place {}: {}",
                        count - result.placed,
                        result.errors.join(", ")
                    );
                }
            }
        }

        BuildSubcommand::FromPlan { plan } => {
            let result = client.build_from_plan(&plan).await?;

            if conn.output == OutputFormat::Json {
                Output::new(conn.output).print(&result)?;
            } else {
                println!("Placed {} of {} entities", result.placed, result.total);
                if !result.errors.is_empty() {
                    println!("Errors:");
                    for err in &result.errors {
                        println!("  - {}", err);
                    }
                }
            }
        }
    }

    client.close().await?;
    Ok(())
}
