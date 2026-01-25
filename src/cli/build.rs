//! Build command - high-level construction operations

use anyhow::Result;
use clap::{Args, Subcommand};

use super::ConnectionArgs;
use crate::client::FactorioClient;
use crate::output::{Output, OutputFormat};

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

        /// Search near this position (x,y)
        #[arg(long, allow_hyphen_values = true)]
        near: Option<String>,

        /// Drill type (burner-mining-drill or electric-mining-drill)
        #[arg(long, default_value = "burner-mining-drill")]
        drill_type: String,

        /// Direction drills should face (for output)
        #[arg(long, default_value = "south")]
        direction: String,
    },

    /// Place a line of furnaces for smelting
    SmelterLine {
        /// Number of furnaces to place
        #[arg(long, default_value = "1")]
        count: u32,

        /// Starting position (x,y)
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

pub async fn execute(cmd: BuildCommand, conn: &ConnectionArgs) -> Result<()> {
    let mut client = FactorioClient::connect(&conn.host, conn.port, &conn.password).await?;

    match cmd.command {
        BuildSubcommand::DrillArray {
            count,
            resource,
            near,
            drill_type,
            direction,
        } => {
            let near_pos = if let Some(pos_str) = near {
                let parts: Vec<f64> = pos_str
                    .split(',')
                    .map(|p| p.trim().parse())
                    .collect::<Result<_, _>>()?;
                if parts.len() != 2 {
                    anyhow::bail!("Position must be x,y");
                }
                Some((parts[0], parts[1]))
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
                    println!("Failed to place {}: {}", count - result.placed,
                        result.errors.join(", "));
                }
                for entity in &result.entities {
                    println!("  #{} at ({:.1}, {:.1})", entity.unit_number.unwrap_or(0),
                        entity.position.x, entity.position.y);
                }
            }
        }

        BuildSubcommand::SmelterLine {
            count,
            at,
            furnace_type,
            direction,
            spacing,
        } => {
            let parts: Vec<f64> = at
                .split(',')
                .map(|p| p.trim().parse())
                .collect::<Result<_, _>>()?;
            if parts.len() != 2 {
                anyhow::bail!("Position must be x,y");
            }

            let result = client
                .build_smelter_line(count, (parts[0], parts[1]), &furnace_type, &direction, spacing)
                .await?;

            if conn.output == OutputFormat::Json {
                Output::new(conn.output).print(&result)?;
            } else {
                println!("Placed {} of {} {}", result.placed, count, furnace_type);
                if result.placed < count {
                    println!("Failed to place {}: {}", count - result.placed,
                        result.errors.join(", "));
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
