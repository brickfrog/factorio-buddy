//! Belt infrastructure commands

use anyhow::Result;
use clap::{Args, Subcommand};

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

        /// Dry run - show planned route without placing
        #[arg(long)]
        dry_run: bool,
    },
}

pub async fn execute(cmd: BeltCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = FactorioClient::connect(&conn.host, conn.port, &conn.password).await?;

    match cmd.command {
        BeltSubcommand::Line {
            from,
            to,
            belt,
            search_radius,
            dry_run,
        } => {
            let from_pos = parse_position(&from)?;
            let to_pos = parse_position(&to)?;

            run_belt_line_astar(&mut client, from_pos, to_pos, &belt, search_radius, dry_run)
                .await?;
        }

        BeltSubcommand::Route {
            waypoints,
            belt,
            search_radius,
            dry_run,
        } => {
            let points: Vec<Position> = waypoints
                .split(';')
                .map(|s| parse_position(s.trim()))
                .collect::<Result<Vec<_>>>()?;

            if points.len() < 2 {
                anyhow::bail!("Route needs at least 2 waypoints");
            }

            for i in 0..points.len() - 1 {
                run_belt_line_astar(
                    &mut client,
                    points[i],
                    points[i + 1],
                    &belt,
                    search_radius,
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
    println!("Collision map: {} blocked tiles", collision_map.blocked_count());

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

    if dry_run {
        println!("\nDry run - would place:");
        for belt in &result.belts {
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

    for belt in &result.belts {
        // Check if we need to walk closer
        let char_pos = client.get_character_position().await?;
        let dist = char_pos.distance(&belt.position);

        if dist > 8.0 {
            // Walk to the target area
            let walk_result = client.walk_to(belt.position, false).await?;
            if !walk_result.arrived && walk_result.final_position.distance(&belt.position) > 10.0 {
                println!(
                    "  Couldn't reach ({:.0},{:.0})",
                    belt.position.x, belt.position.y
                );
                failed += 1;
                continue;
            }
        }

        // Place belt with correct direction from A*
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
