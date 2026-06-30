//! Power infrastructure commands

use anyhow::Result;
use clap::{Args, Subcommand};

use super::parsing::{parse_area, parse_position, parse_tile};
use super::ResolvedConnectionArgs;
use crate::client::lua::LuaCommand;
use crate::world::Position;

#[derive(Args, Debug)]
pub struct PowerCommand {
    #[command(subcommand)]
    pub command: PowerSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum PowerSubcommand {
    /// Run a power line from one position to another
    Line {
        /// Starting tile position (x,y as integers)
        #[arg(long, allow_hyphen_values = true)]
        from: String,

        /// Ending tile position (x,y as integers)
        #[arg(long, allow_hyphen_values = true)]
        to: String,

        /// Pole type (small-electric-pole, medium-electric-pole, big-electric-pole)
        #[arg(long, default_value = "small-electric-pole")]
        pole: String,
    },

    /// Show power network status at a position
    Status {
        /// Center X coordinate
        #[arg(long, allow_hyphen_values = true)]
        x: i32,

        /// Center Y coordinate
        #[arg(long, allow_hyphen_values = true)]
        y: i32,

        /// Search radius
        #[arg(long, default_value = "50")]
        radius: u32,
    },

    /// Find power issues (unpowered/low-power entities)
    Issues {
        /// Center X coordinate
        #[arg(long, allow_hyphen_values = true)]
        x: i32,

        /// Center Y coordinate
        #[arg(long, allow_hyphen_values = true)]
        y: i32,

        /// Search radius
        #[arg(long, default_value = "50")]
        radius: u32,
    },

    /// Show all power networks in an area
    Networks {
        /// Center X coordinate
        #[arg(long, allow_hyphen_values = true)]
        x: i32,

        /// Center Y coordinate
        #[arg(long, allow_hyphen_values = true)]
        y: i32,

        /// Search radius
        #[arg(long, default_value = "50")]
        radius: u32,
    },

    /// Diagnose steam power fluid and grid connectivity in an area
    Steam {
        /// Center X coordinate
        #[arg(long, allow_hyphen_values = true)]
        x: i32,

        /// Center Y coordinate
        #[arg(long, allow_hyphen_values = true)]
        y: i32,

        /// Search radius
        #[arg(long, default_value = "50")]
        radius: u32,
    },

    /// Plan checked starter steam power without placing anything
    PlanSteam {
        /// Water bounding box as x1,y1,x2,y2
        #[arg(long, allow_hyphen_values = true)]
        water_area: String,

        /// Target position to power as x,y
        #[arg(long, allow_hyphen_values = true)]
        target: String,
    },

    /// Plan dry-run repairs for an existing steam power plant
    RepairSteam {
        /// Center X coordinate of the steam plant area
        #[arg(long, allow_hyphen_values = true)]
        x: i32,

        /// Center Y coordinate of the steam plant area
        #[arg(long, allow_hyphen_values = true)]
        y: i32,

        /// Search radius
        #[arg(long, default_value = "50")]
        radius: u32,

        /// Target position that should receive power as x,y
        #[arg(long, allow_hyphen_values = true)]
        target: String,
    },

    /// Plan dry-run pole placement to extend an existing grid to a target
    ExtendTo {
        /// Center X coordinate for searching existing poles
        #[arg(long, allow_hyphen_values = true)]
        x: i32,

        /// Center Y coordinate for searching existing poles
        #[arg(long, allow_hyphen_values = true)]
        y: i32,

        /// Search radius
        #[arg(long, default_value = "50")]
        radius: u32,

        /// Target position that should receive power as x,y
        #[arg(long, allow_hyphen_values = true)]
        target: String,
    },
}

pub async fn execute(cmd: PowerCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    match cmd.command {
        PowerSubcommand::Line { from, to, pole } => {
            let from_tile = parse_tile(&from)?;
            let to_tile = parse_tile(&to)?;
            // Poles are 1x1, use tile center
            let from_pos = from_tile.to_world_1x1();
            let to_pos = to_tile.to_world_1x1();

            // Get pole wire reach distance
            let spacing = get_pole_spacing(&pole);

            // Calculate total distance
            let dx = to_pos.x - from_pos.x;
            let dy = to_pos.y - from_pos.y;
            let total_dist = (dx * dx + dy * dy).sqrt();
            let num_poles = (total_dist / spacing).ceil() as i32 + 1;

            println!(
                "Running power line from ({:.0},{:.0}) to ({:.0},{:.0})",
                from_pos.x, from_pos.y, to_pos.x, to_pos.y
            );
            println!(
                "Using {} with {:.1} tile spacing, ~{} poles needed",
                pole, spacing, num_poles
            );

            let mut placed = 0;
            let mut failed = 0;

            for i in 0..num_poles {
                let t = i as f64 / (num_poles - 1).max(1) as f64;
                let x = from_pos.x + dx * t;
                let y = from_pos.y + dy * t;
                let target = Position { x, y };

                // Walk to position
                let walk_result = client.walk_to(target, false).await?;
                if !walk_result.arrived && walk_result.final_position.distance(&target) > 10.0 {
                    println!(
                        "  Couldn't reach ({:.0},{:.0}) - blocked",
                        target.x, target.y
                    );
                    failed += 1;
                    continue;
                }

                // Try to place pole
                match client
                    .place_entity(&pole, target, crate::world::Direction::North)
                    .await
                {
                    Ok(entity) => {
                        println!(
                            "  Placed {} at ({:.0},{:.0})",
                            pole, entity.position.x, entity.position.y
                        );
                        placed += 1;
                    }
                    Err(e) => {
                        // Try nearby positions
                        let mut placed_nearby = false;
                        for offset in &[(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
                            let alt_pos = Position {
                                x: target.x + offset.0,
                                y: target.y + offset.1,
                            };
                            if let Ok(entity) = client
                                .place_entity(&pole, alt_pos, crate::world::Direction::North)
                                .await
                            {
                                println!(
                                    "  Placed {} at ({:.0},{:.0})",
                                    pole, entity.position.x, entity.position.y
                                );
                                placed += 1;
                                placed_nearby = true;
                                break;
                            }
                        }
                        if !placed_nearby {
                            println!("  Failed at ({:.0},{:.0}): {}", target.x, target.y, e);
                            failed += 1;
                        }
                    }
                }
            }

            println!("\nPlaced {} poles, {} failed", placed, failed);
        }

        PowerSubcommand::Status { x, y, radius } => {
            let lua = LuaCommand::get_power_status(x, y, radius);
            let response = client.execute_lua(&lua).await?;
            println!("{}", response);
        }

        PowerSubcommand::Issues { x, y, radius } => {
            let lua = LuaCommand::find_power_issues(x, y, radius);
            let response = client.execute_lua(&lua).await?;
            println!("{}", response);
        }

        PowerSubcommand::Networks { x, y, radius } => {
            let lua = LuaCommand::get_power_networks(x, y, radius);
            let response = client.execute_lua(&lua).await?;
            println!("{}", response);
        }

        PowerSubcommand::Steam { x, y, radius } => {
            let lua = LuaCommand::diagnose_steam_power(x, y, radius);
            let response = client.execute_lua(&lua).await?;
            println!("{}", response);
        }

        PowerSubcommand::PlanSteam { water_area, target } => {
            let water_area = parse_area(&water_area)?;
            let target = parse_position(&target)?;
            let plan = client.plan_steam_power(water_area, target).await?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }

        PowerSubcommand::RepairSteam {
            x,
            y,
            radius,
            target,
        } => {
            let target = parse_position(&target)?;
            let plan = client.repair_steam_power(x, y, radius, target).await?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }

        PowerSubcommand::ExtendTo {
            x,
            y,
            radius,
            target,
        } => {
            let target = parse_position(&target)?;
            let plan = client.extend_power_to(x, y, radius, target).await?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }
    }

    client.close().await?;
    Ok(())
}

fn get_pole_spacing(pole_type: &str) -> f64 {
    match pole_type {
        "small-electric-pole" => 7.0, // Wire reach is 7.5, use 7 for margin
        "medium-electric-pole" => 9.0,
        "big-electric-pole" => 28.0, // Wire reach is 30, use 28 for margin
        "substation" => 16.0,
        _ => 7.0,
    }
}
