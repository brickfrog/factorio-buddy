//! Route command for pathfinding belt routes

use anyhow::Result;
use clap::{Args, Subcommand};

use super::parsing::parse_tile;
use super::ResolvedConnectionArgs;
use crate::world::{find_belt_route, Area, GridPos, Position, TilePos};

#[derive(Args, Debug)]
pub struct RouteCommand {
    #[command(subcommand)]
    pub subcommand: RouteSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum RouteSubcommand {
    /// Route transport belts from point A to point B
    Belt {
        /// Starting tile position (x,y as integers)
        #[arg(long, allow_hyphen_values = true)]
        from: String,

        /// Ending tile position (x,y as integers)
        #[arg(long, allow_hyphen_values = true)]
        to: String,

        /// Belt type to use
        #[arg(long, default_value = "transport-belt")]
        belt_type: String,

        /// Search radius for obstacle detection (tiles from path bounds)
        #[arg(long, default_value = "10")]
        search_radius: u32,

        /// Dry run - show route without placing
        #[arg(long)]
        dry_run: bool,

        /// Output route as JSON (for piping to other commands)
        #[arg(long)]
        plan_only: bool,
    },
}

pub async fn execute(cmd: RouteCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    match cmd.subcommand {
        RouteSubcommand::Belt {
            from,
            to,
            belt_type,
            search_radius,
            dry_run,
            plan_only,
        } => {
            let start_tile = parse_tile(&from)?;
            let end_tile = parse_tile(&to)?;

            // Calculate search area (bounding box of path + radius padding)
            let area = calculate_search_area(&start_tile, &end_tile, search_radius);

            println!(
                "Finding route from tile ({}, {}) to ({}, {})",
                start_tile.x, start_tile.y, end_tile.x, end_tile.y
            );
            println!(
                "Search area: ({}, {}) to ({}, {})",
                area.left_top.x as i32,
                area.left_top.y as i32,
                area.right_bottom.x as i32,
                area.right_bottom.y as i32
            );

            // Build collision map
            let collision_map = client.build_collision_map(area).await?;
            println!(
                "Collision map: {} blocked tiles",
                collision_map.blocked_count()
            );

            // Find path using GridPos (integer coordinates)
            let start_grid = GridPos::new(start_tile.x, start_tile.y);
            let end_grid = GridPos::new(end_tile.x, end_tile.y);
            let result = find_belt_route(start_grid, end_grid, &collision_map);

            if plan_only {
                // Output as JSON
                println!("{}", serde_json::to_string_pretty(&result)?);
                client.close().await?;
                return Ok(());
            }

            if !result.success {
                println!(
                    "Route failed: {}",
                    result.error.as_deref().unwrap_or("unknown")
                );
                client.close().await?;
                return Ok(());
            }

            println!(
                "Found route: {} belts, {} turns",
                result.belt_count, result.turn_count
            );

            if dry_run {
                println!("\nDry run - would place:");
                for belt in &result.belts {
                    println!(
                        "  {} at ({}, {}) facing {:?}",
                        belt_type, belt.position.x, belt.position.y, belt.direction
                    );
                }
                client.close().await?;
                return Ok(());
            }

            // Place the belts
            println!("\nPlacing belts...");
            let mut placed = 0;
            let mut errors = Vec::new();

            for belt in &result.belts {
                match client
                    .place_entity(&belt_type, belt.position, belt.direction)
                    .await
                {
                    Ok(entity) => {
                        placed += 1;
                        println!(
                            "  Placed {} #{} at ({}, {})",
                            belt_type,
                            entity.unit_number.unwrap_or(0),
                            entity.position.x,
                            entity.position.y
                        );
                    }
                    Err(e) => {
                        errors.push(format!(
                            "Failed at ({}, {}): {}",
                            belt.position.x, belt.position.y, e
                        ));
                    }
                }
            }

            println!("\nPlaced {}/{} belts", placed, result.belt_count);
            if !errors.is_empty() {
                println!("Errors:");
                for err in &errors {
                    println!("  {}", err);
                }
            }
        }
    }

    client.close().await?;
    Ok(())
}

fn calculate_search_area(start: &TilePos, end: &TilePos, padding: u32) -> Area {
    let padding = padding as i32;
    Area {
        left_top: Position {
            x: (start.x.min(end.x) - padding) as f64,
            y: (start.y.min(end.y) - padding) as f64,
        },
        right_bottom: Position {
            x: (start.x.max(end.x) + padding + 1) as f64,
            y: (start.y.max(end.y) + padding + 1) as f64,
        },
    }
}
