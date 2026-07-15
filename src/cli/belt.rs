//! Belt infrastructure commands

use anyhow::Result;
use clap::{Args, Subcommand};

use super::parsing::parse_tile;
use super::ResolvedConnectionArgs;
use crate::client::FactorioClient;
use crate::world::{find_belt_route, Area, GridPos, Position};

#[derive(Args, Debug)]
pub struct BeltCommand {
    #[command(subcommand)]
    pub command: BeltSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum BeltSubcommand {
    /// Run a belt line from one position to another using A* pathfinding
    Line {
        /// Starting position (x,y)
        #[arg(long, allow_hyphen_values = true)]
        from: String,

        /// Ending position (x,y)
        #[arg(long, allow_hyphen_values = true)]
        to: String,

        /// Belt type (transport-belt, fast-transport-belt, express-transport-belt)
        #[arg(long, default_value = "transport-belt")]
        belt: String,

        /// Search radius for obstacle detection (tiles from path bounds)
        #[arg(long, default_value = "10")]
        search_radius: u32,

        /// Leave N tiles gap at the end for inserters (0 = no gap)
        #[arg(long, default_value = "0")]
        inserter_gap: u32,

        /// Dry run - show planned route without placing
        #[arg(long)]
        dry_run: bool,
    },

    /// Run a belt line with turns (specify waypoints)
    Route {
        /// Waypoints as x1,y1;x2,y2;x3,y3
        #[arg(long, allow_hyphen_values = true)]
        waypoints: String,

        /// Belt type
        #[arg(long, default_value = "transport-belt")]
        belt: String,

        /// Search radius for obstacle detection
        #[arg(long, default_value = "10")]
        search_radius: u32,

        /// Leave N tiles gap at the end for inserters (0 = no gap)
        #[arg(long, default_value = "0")]
        inserter_gap: u32,

        /// Dry run - show planned route without placing
        #[arg(long)]
        dry_run: bool,
    },
}

pub async fn execute(cmd: BeltCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    match cmd.command {
        BeltSubcommand::Line {
            from,
            to,
            belt,
            search_radius,
            inserter_gap,
            dry_run,
        } => {
            let from_tile = parse_tile(&from)?;
            let to_tile = parse_tile(&to)?;
            // Belts are 1x1, use tile center
            let from_pos = from_tile.to_world_1x1();
            let to_pos = to_tile.to_world_1x1();

            run_belt_line_astar(
                &mut client,
                from_pos,
                to_pos,
                &belt,
                search_radius,
                inserter_gap,
                dry_run,
            )
            .await?;
        }

        BeltSubcommand::Route {
            waypoints,
            belt,
            search_radius,
            inserter_gap,
            dry_run,
        } => {
            let points: Vec<Position> = waypoints
                .split(';')
                .map(|s| parse_tile(s.trim()).map(|t| t.to_world_1x1()))
                .collect::<Result<Vec<_>>>()?;

            if points.len() < 2 {
                anyhow::bail!("Route needs at least 2 waypoints");
            }

            for i in 0..points.len() - 1 {
                // Only apply inserter_gap on the last segment
                let gap = if i == points.len() - 2 {
                    inserter_gap
                } else {
                    0
                };
                run_belt_line_astar(
                    &mut client,
                    points[i],
                    points[i + 1],
                    &belt,
                    search_radius,
                    gap,
                    dry_run,
                )
                .await?;
            }
        }
    }

    client.close().await?;
    Ok(())
}

/// Run a belt line using A* pathfinding to avoid obstacles
async fn run_belt_line_astar(
    client: &mut FactorioClient,
    from: Position,
    to: Position,
    belt_type: &str,
    search_radius: u32,
    inserter_gap: u32,
    dry_run: bool,
) -> Result<()> {
    // Calculate search area (bounding box of path + radius padding)
    let padding = search_radius as f64;
    let area = Area {
        left_top: Position {
            x: from.x.min(to.x) - padding,
            y: from.y.min(to.y) - padding,
        },
        right_bottom: Position {
            x: from.x.max(to.x) + padding,
            y: from.y.max(to.y) + padding,
        },
    };

    println!(
        "Planning belt route from ({:.0},{:.0}) to ({:.0},{:.0})",
        from.x, from.y, to.x, to.y
    );
    println!(
        "Search area: ({:.0},{:.0}) to ({:.0},{:.0})",
        area.left_top.x, area.left_top.y, area.right_bottom.x, area.right_bottom.y
    );

    // Build collision map
    let collision_map = client.build_collision_map(area).await?;
    println!(
        "Collision map: {} blocked tiles",
        collision_map.blocked_count()
    );

    // Find path using A*
    let start_grid = GridPos::from_position(&from);
    let end_grid = GridPos::from_position(&to);
    let result = find_belt_route(start_grid, end_grid, &collision_map);

    if !result.success {
        println!(
            "Route failed: {}",
            result.error.as_deref().unwrap_or("unknown")
        );
        return Ok(());
    }

    println!(
        "Found route: {} belts, {} turns",
        result.belt_count, result.turn_count
    );

    // Apply inserter gap by removing the last N belts from the plan
    let belts_to_place = if inserter_gap > 0 && result.belts.len() > inserter_gap as usize {
        let end = result.belts.len() - inserter_gap as usize;
        println!("Leaving {} tile gap at end for inserter", inserter_gap);
        &result.belts[..end]
    } else {
        &result.belts[..]
    };

    if dry_run {
        println!("\nDry run - would place:");
        for belt in belts_to_place {
            println!(
                "  {} at ({:.0},{:.0}) facing {:?}",
                belt_type, belt.position.x, belt.position.y, belt.direction
            );
        }
        return Ok(());
    }

    // Place the belts
    println!("\nPlacing belts...");
    let mut placed = 0;
    let mut failed = 0;

    for belt in belts_to_place {
        // Placement owns its range-aware approach. Exact navigation must not
        // walk onto the tile that is about to receive the belt.
        match client
            .place_entity(belt_type, belt.position, belt.direction)
            .await
        {
            Ok(_) => {
                placed += 1;
            }
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("Cannot place") || err_str.contains("not in inventory") {
                    println!(
                        "  Failed at ({:.0},{:.0}): {}",
                        belt.position.x, belt.position.y, e
                    );
                    failed += 1;
                }
            }
        }

        // Print progress every 10 belts
        if placed % 10 == 0 && placed > 0 {
            println!("  Placed {} belts...", placed);
        }
    }

    println!("Placed {} belts, {} failed", placed, failed);
    Ok(())
}
