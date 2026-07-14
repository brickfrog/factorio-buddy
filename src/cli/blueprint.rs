//! Blueprint commands for declarative entity placement

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use super::parsing::{parse_area, parse_position};
use crate::cli::ResolvedConnectionArgs;
use crate::client::FactorioClient;
use crate::output::{Output, OutputFormat, Outputable};
use crate::world::{
    ApplyResult, Area, Blueprint, BlueprintDiff, BlueprintEntity, DiffAdd, DiffRemove, DiffRotate,
    Direction, Position,
};

/// Blueprint commands for declarative placement
#[derive(Parser, Debug)]
pub struct BlueprintCommand {
    #[command(subcommand)]
    pub subcommand: BlueprintSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum BlueprintSubcommand {
    /// Apply a blueprint, placing/removing/rotating entities as needed
    Apply {
        /// Blueprint file path (JSON)
        file: PathBuf,

        /// Override origin X coordinate
        #[arg(long, allow_hyphen_values = true)]
        origin_x: Option<f64>,

        /// Override origin Y coordinate
        #[arg(long, allow_hyphen_values = true)]
        origin_y: Option<f64>,

        /// Dry run - show what would change without applying
        #[arg(long)]
        dry_run: bool,
    },

    /// Show diff between blueprint and current world state
    Diff {
        /// Blueprint file path (JSON)
        file: PathBuf,

        /// Override origin X coordinate
        #[arg(long, allow_hyphen_values = true)]
        origin_x: Option<f64>,

        /// Override origin Y coordinate
        #[arg(long, allow_hyphen_values = true)]
        origin_y: Option<f64>,
    },

    /// Export current entities as a blueprint (JSON format)
    Export {
        /// Area to export (x1,y1,x2,y2)
        #[arg(long, allow_hyphen_values = true)]
        area: String,

        /// Blueprint origin point (x,y) - positions will be relative to this
        #[arg(long, allow_hyphen_values = true)]
        origin: Option<String>,

        /// Blueprint name
        #[arg(long, default_value = "exported")]
        name: String,
    },

    // --- Native Factorio Blueprint Commands ---
    /// Export area to native Factorio blueprint string
    String {
        /// Area to export (x1,y1,x2,y2)
        #[arg(long, allow_hyphen_values = true)]
        area: String,
    },

    /// Save a blueprint with a name (stored in game save)
    Save {
        /// Blueprint name
        name: String,

        /// Area to save (x1,y1,x2,y2)
        #[arg(long, allow_hyphen_values = true)]
        area: String,
    },

    /// List saved blueprints
    List,

    /// Get a saved blueprint string by name
    Get {
        /// Blueprint name
        name: String,
    },

    /// Place a saved blueprint at a position
    Place {
        /// Blueprint name
        name: String,

        /// Position to place at (x,y)
        #[arg(long, allow_hyphen_values = true)]
        at: String,

        /// Direction to rotate (north, east, south, west)
        #[arg(long, default_value = "north")]
        direction: String,
    },

    /// Import and place from a blueprint string
    Import {
        /// Blueprint string
        string: String,

        /// Position to place at (x,y)
        #[arg(long, allow_hyphen_values = true)]
        at: String,

        /// Direction to rotate (north, east, south, west)
        #[arg(long, default_value = "north")]
        direction: String,
    },

    /// Delete a saved blueprint
    Delete {
        /// Blueprint name
        name: String,
    },
}

pub async fn execute(cmd: BlueprintCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    match cmd.subcommand {
        BlueprintSubcommand::Apply {
            file,
            origin_x,
            origin_y,
            dry_run,
        } => {
            let blueprint = Blueprint::from_file(&file)?;
            let origin = resolve_origin(&blueprint, origin_x, origin_y);

            let diff = compute_diff(&mut client, &blueprint, &origin).await?;

            if dry_run {
                println!("Dry run - would make {} changes:", diff.total_changes());
                print_diff(&diff);
            } else if diff.is_empty() {
                println!("No changes needed - world matches blueprint");
            } else {
                println!("Applying {} changes...", diff.total_changes());
                print_diff(&diff);
                let result = apply_diff(&mut client, &diff).await?;
                Output::new(conn.output).print(&result)?;
            }
        }

        BlueprintSubcommand::Diff {
            file,
            origin_x,
            origin_y,
        } => {
            let blueprint = Blueprint::from_file(&file)?;
            let origin = resolve_origin(&blueprint, origin_x, origin_y);

            let diff = compute_diff(&mut client, &blueprint, &origin).await?;

            if diff.is_empty() {
                println!("No differences - world matches blueprint");
            } else {
                print_diff(&diff);
            }

            if conn.output == OutputFormat::Json {
                println!("{}", serde_json::to_string_pretty(&diff)?);
            }
        }

        BlueprintSubcommand::Export { area, origin, name } => {
            let area = parse_area(&area)?;
            let origin_pos = if let Some(o) = origin {
                parse_position(&o)?
            } else {
                // Default origin to area center
                Position {
                    x: (area.left_top.x + area.right_bottom.x) / 2.0,
                    y: (area.left_top.y + area.right_bottom.y) / 2.0,
                }
            };

            let blueprint = export_blueprint(&mut client, &area, &origin_pos, &name).await?;
            println!("{}", serde_json::to_string_pretty(&blueprint)?);
        }

        // --- Native Factorio Blueprint Commands ---
        BlueprintSubcommand::String { area } => {
            let area = parse_area(&area)?;
            let result = client.create_native_blueprint(&conn.agent_id, area).await?;
            Output::new(conn.output).print(&result)?;
        }

        BlueprintSubcommand::Save { name, area } => {
            let area = parse_area(&area)?;
            let result = client.save_blueprint(&conn.agent_id, &name, area).await?;
            Output::new(conn.output).print(&result)?;
        }

        BlueprintSubcommand::List => {
            let blueprints = client.list_blueprints().await?;
            Output::new(conn.output).print(&blueprints)?;
        }

        BlueprintSubcommand::Get { name } => {
            let result = client.get_blueprint(&name).await?;
            if let Some(error) = &result.error {
                anyhow::bail!("{}", error);
            }
            if let Some(bp_string) = &result.blueprint_string {
                if conn.output == OutputFormat::Json {
                    println!("{}", serde_json::to_string_pretty(&result)?);
                } else {
                    println!("{}", bp_string);
                }
            }
        }

        BlueprintSubcommand::Place {
            name,
            at,
            direction,
        } => {
            let position = parse_position(&at)?;
            let dir = Direction::from_name(&direction).unwrap_or(Direction::North);
            let result = client
                .place_blueprint(&conn.agent_id, &name, position, dir.to_factorio())
                .await?;
            Output::new(conn.output).print(&result)?;
        }

        BlueprintSubcommand::Import {
            string,
            at,
            direction,
        } => {
            let position = parse_position(&at)?;
            let dir = Direction::from_name(&direction).unwrap_or(Direction::North);
            let result = client
                .import_blueprint(&conn.agent_id, &string, position, dir.to_factorio())
                .await?;
            Output::new(conn.output).print(&result)?;
        }

        BlueprintSubcommand::Delete { name } => {
            let success = client.delete_blueprint(&name).await?;
            if success {
                println!("Deleted blueprint '{}'", name);
            } else {
                anyhow::bail!("Blueprint '{}' not found", name);
            }
        }
    }

    client.close().await?;
    Ok(())
}

fn resolve_origin(blueprint: &Blueprint, origin_x: Option<f64>, origin_y: Option<f64>) -> [f64; 2] {
    [
        origin_x.unwrap_or(blueprint.origin[0]),
        origin_y.unwrap_or(blueprint.origin[1]),
    ]
}

/// Compute the diff between blueprint and world state
async fn compute_diff(
    client: &mut FactorioClient,
    blueprint: &Blueprint,
    origin: &[f64; 2],
) -> Result<BlueprintDiff> {
    // Get bounding box and query world entities
    let (min, max) = blueprint.bounding_box();
    let adjusted_min = Position {
        x: min.x + origin[0] - blueprint.origin[0],
        y: min.y + origin[1] - blueprint.origin[1],
    };
    let adjusted_max = Position {
        x: max.x + origin[0] - blueprint.origin[0],
        y: max.y + origin[1] - blueprint.origin[1],
    };

    let area = Area {
        left_top: adjusted_min,
        right_bottom: adjusted_max,
    };

    // Get current entities (player-built only, excluding character)
    let world_entities = client.find_entities(area, None, None).await?;
    let world_entities: Vec<_> = world_entities
        .into_iter()
        .filter(|e| {
            e.force.as_deref() == Some("player") && e.unit_number.is_some() && e.name != "character"
        })
        .collect();

    let mut diff = BlueprintDiff {
        add: Vec::new(),
        remove: Vec::new(),
        rotate: Vec::new(),
    };

    // Track which world entities are matched
    let mut matched_world_entities: Vec<bool> = vec![false; world_entities.len()];

    // For each blueprint entity, find matching world entity
    for bp_entity in &blueprint.entities {
        let abs_pos = bp_entity.absolute_position(origin);
        let bp_dir = bp_entity.direction();

        // Find world entity at this position with same name
        let mut found = false;
        for (i, world_entity) in world_entities.iter().enumerate() {
            if matched_world_entities[i] {
                continue;
            }

            // Check if position matches (within tolerance for entity size)
            let dx = (world_entity.position.x - abs_pos.x).abs();
            let dy = (world_entity.position.y - abs_pos.y).abs();

            if dx < 0.6 && dy < 0.6 && world_entity.name == bp_entity.name {
                matched_world_entities[i] = true;
                found = true;

                // Check if direction matches
                let world_dir = world_entity.direction;
                if world_dir != bp_dir.to_factorio() {
                    diff.rotate.push(DiffRotate {
                        unit_number: world_entity.unit_number.unwrap(),
                        name: world_entity.name.clone(),
                        position: world_entity.position,
                        from_direction: world_dir,
                        to_direction: bp_dir,
                    });
                }
                break;
            }
        }

        if !found {
            // Entity needs to be added
            diff.add.push(DiffAdd {
                name: bp_entity.name.clone(),
                position: abs_pos,
                direction: bp_dir,
            });
        }
    }

    // Any unmatched world entities need to be removed
    for (i, world_entity) in world_entities.iter().enumerate() {
        if !matched_world_entities[i] {
            diff.remove.push(DiffRemove {
                unit_number: world_entity.unit_number.unwrap(),
                name: world_entity.name.clone(),
                position: world_entity.position,
            });
        }
    }

    Ok(diff)
}

fn print_diff(diff: &BlueprintDiff) {
    if !diff.remove.is_empty() {
        println!("\nRemove ({}):", diff.remove.len());
        for r in &diff.remove {
            println!(
                "  - #{} {} at ({:.1}, {:.1})",
                r.unit_number, r.name, r.position.x, r.position.y
            );
        }
    }

    if !diff.rotate.is_empty() {
        println!("\nRotate ({}):", diff.rotate.len());
        for r in &diff.rotate {
            println!(
                "  ~ #{} {} at ({:.1}, {:.1}): {} -> {}",
                r.unit_number,
                r.name,
                r.position.x,
                r.position.y,
                direction_name(r.from_direction),
                direction_name(r.to_direction.to_factorio())
            );
        }
    }

    if !diff.add.is_empty() {
        println!("\nAdd ({}):", diff.add.len());
        for a in &diff.add {
            println!(
                "  + {} at ({:.1}, {:.1}) facing {}",
                a.name,
                a.position.x,
                a.position.y,
                direction_name(a.direction.to_factorio())
            );
        }
    }
}

fn direction_name(dir: u8) -> &'static str {
    match dir {
        0 => "north",
        2 => "northeast",
        4 => "east",
        6 => "southeast",
        8 => "south",
        10 => "southwest",
        12 => "west",
        14 => "northwest",
        _ => "unknown",
    }
}

/// Apply the diff to the world
async fn apply_diff(client: &mut FactorioClient, diff: &BlueprintDiff) -> Result<ApplyResult> {
    let mut result = ApplyResult {
        added: 0,
        removed: 0,
        rotated: 0,
        errors: Vec::new(),
    };

    // Remove entities first (to free up space)
    for r in &diff.remove {
        match client.remove_entity(r.unit_number).await {
            Ok(_) => result.removed += 1,
            Err(e) => result
                .errors
                .push(format!("Failed to remove #{}: {}", r.unit_number, e)),
        }
    }

    // Rotate existing entities
    for r in &diff.rotate {
        match client
            .rotate_entity(r.unit_number, r.to_direction.to_factorio())
            .await
        {
            Ok(_) => result.rotated += 1,
            Err(e) => result
                .errors
                .push(format!("Failed to rotate #{}: {}", r.unit_number, e)),
        }
    }

    // Add new entities
    for a in &diff.add {
        match client.place_entity(&a.name, a.position, a.direction).await {
            Ok(_) => result.added += 1,
            Err(e) => result.errors.push(format!(
                "Failed to place {} at ({:.1}, {:.1}): {}",
                a.name, a.position.x, a.position.y, e
            )),
        }
    }

    Ok(result)
}

/// Export current entities as a blueprint
async fn export_blueprint(
    client: &mut FactorioClient,
    area: &Area,
    origin: &Position,
    name: &str,
) -> Result<Blueprint> {
    let entities = client.find_entities(*area, None, None).await?;

    // Filter to player-built entities only (excluding character)
    let entities: Vec<_> = entities
        .into_iter()
        .filter(|e| {
            e.force.as_deref() == Some("player") && e.unit_number.is_some() && e.name != "character"
        })
        .collect();

    let bp_entities: Vec<BlueprintEntity> = entities
        .iter()
        .map(|e| BlueprintEntity {
            name: e.name.clone(),
            pos: [e.position.x - origin.x, e.position.y - origin.y],
            dir: direction_short_name(e.direction),
        })
        .collect();

    Ok(Blueprint {
        name: name.to_string(),
        origin: [origin.x, origin.y],
        entities: bp_entities,
    })
}

fn direction_short_name(dir: u8) -> String {
    match dir {
        0 => "n",
        2 => "ne",
        4 => "e",
        6 => "se",
        8 => "s",
        10 => "sw",
        12 => "w",
        14 => "nw",
        _ => "n",
    }
    .to_string()
}

// Implement Outputable for ApplyResult
impl Outputable for ApplyResult {
    fn format_human(&self) -> String {
        let mut lines = vec![format!(
            "Applied: {} added, {} removed, {} rotated",
            self.added, self.removed, self.rotated
        )];
        if !self.errors.is_empty() {
            lines.push(format!("Errors ({}):", self.errors.len()));
            for err in &self.errors {
                lines.push(format!("  - {}", err));
            }
        }
        lines.join("\n")
    }
}
