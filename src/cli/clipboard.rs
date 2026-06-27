//! Copy/paste commands using the blueprint system

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

use super::parsing::{parse_area, parse_position};
use crate::cli::ResolvedConnectionArgs;
use crate::world::{Blueprint, BlueprintEntity, Direction, Position};

/// Default clipboard file location
fn default_clipboard_path() -> PathBuf {
    // Use home directory or current directory
    std::env::var("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".factorioctl-clipboard.json")
}

/// Clipboard commands for copy/paste operations
#[derive(Parser, Debug)]
pub struct ClipboardCommand {
    #[command(subcommand)]
    pub subcommand: ClipboardSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum ClipboardSubcommand {
    /// Show current clipboard contents
    Show,

    /// Clear the clipboard
    Clear,
}

/// Copy command
#[derive(Parser, Debug)]
pub struct CopyCommand {
    /// Area to copy (x1,y1,x2,y2)
    #[arg(long, allow_hyphen_values = true)]
    pub area: String,

    /// Origin point for the copy (x,y) - positions will be relative to this
    /// If not specified, uses the center of the area
    #[arg(long, allow_hyphen_values = true)]
    pub origin: Option<String>,

    /// Save to a specific file instead of clipboard
    #[arg(long)]
    pub to: Option<PathBuf>,

    /// Name for the copied blueprint
    #[arg(long, default_value = "copied")]
    pub name: String,
}

/// Paste command
#[derive(Parser, Debug)]
pub struct PasteCommand {
    /// Position to paste at (x,y) - this becomes the new origin
    #[arg(long, allow_hyphen_values = true)]
    pub at: String,

    /// Load from a specific file instead of clipboard
    #[arg(long)]
    pub from: Option<PathBuf>,

    /// Rotate the blueprint (0, 90, 180, 270 degrees clockwise)
    #[arg(long, default_value = "0")]
    pub rotate: i32,

    /// Flip horizontally (mirror on Y axis)
    #[arg(long)]
    pub flip_h: bool,

    /// Flip vertically (mirror on X axis)
    #[arg(long)]
    pub flip_v: bool,

    /// Place as ghosts instead of real entities
    #[arg(long)]
    pub ghosts: bool,

    /// Dry run - show what would be placed
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn execute_clipboard(
    cmd: ClipboardCommand,
    _conn: &ResolvedConnectionArgs,
) -> Result<()> {
    match cmd.subcommand {
        ClipboardSubcommand::Show => {
            let path = default_clipboard_path();
            if path.exists() {
                let content = std::fs::read_to_string(&path)?;
                let blueprint: Blueprint = serde_json::from_str(&content)?;
                println!("Clipboard: {}", blueprint.name);
                println!("Origin: ({}, {})", blueprint.origin[0], blueprint.origin[1]);
                println!("Entities: {}", blueprint.entities.len());
                for e in &blueprint.entities {
                    println!(
                        "  {} at ({}, {}) facing {}",
                        e.name, e.pos[0], e.pos[1], e.dir
                    );
                }
            } else {
                println!("Clipboard is empty");
            }
        }
        ClipboardSubcommand::Clear => {
            let path = default_clipboard_path();
            if path.exists() {
                std::fs::remove_file(&path)?;
                println!("Clipboard cleared");
            } else {
                println!("Clipboard was already empty");
            }
        }
    }
    Ok(())
}

pub async fn execute_copy(cmd: CopyCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    let area = parse_area(&cmd.area)?;
    let origin = if let Some(o) = &cmd.origin {
        parse_position(o)?
    } else {
        // Default to area center
        Position {
            x: (area.left_top.x + area.right_bottom.x) / 2.0,
            y: (area.left_top.y + area.right_bottom.y) / 2.0,
        }
    };

    // Get entities in area
    let entities = client.find_entities(area.clone(), None, None).await?;
    let entities: Vec<_> = entities
        .into_iter()
        .filter(|e| {
            e.force.as_deref() == Some("player") && e.unit_number.is_some() && e.name != "character"
        })
        .collect();

    if entities.is_empty() {
        println!("No entities found in area");
        client.close().await?;
        return Ok(());
    }

    // Convert to blueprint
    let bp_entities: Vec<BlueprintEntity> = entities
        .iter()
        .map(|e| BlueprintEntity {
            name: e.name.clone(),
            pos: [e.position.x - origin.x, e.position.y - origin.y],
            dir: direction_short_name(e.direction),
        })
        .collect();

    let blueprint = Blueprint {
        name: cmd.name,
        origin: [origin.x, origin.y],
        entities: bp_entities,
    };

    // Save to file
    let path = cmd.to.unwrap_or_else(default_clipboard_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(&blueprint)?;
    std::fs::write(&path, &json)?;

    println!(
        "Copied {} entities to {}",
        blueprint.entities.len(),
        path.display()
    );
    println!("Origin: ({}, {})", origin.x, origin.y);
    for e in &blueprint.entities {
        println!(
            "  {} at ({}, {}) facing {}",
            e.name, e.pos[0], e.pos[1], e.dir
        );
    }

    client.close().await?;
    Ok(())
}

pub async fn execute_paste(cmd: PasteCommand, conn: &ResolvedConnectionArgs) -> Result<()> {
    let mut client = conn.connect_client().await?;

    // Load blueprint
    let path = cmd.from.unwrap_or_else(default_clipboard_path);
    if !path.exists() {
        anyhow::bail!("Clipboard is empty (no file at {})", path.display());
    }
    let content = std::fs::read_to_string(&path)?;
    let blueprint: Blueprint = serde_json::from_str(&content)?;

    // Parse target position
    let target = parse_position(&cmd.at)?;

    // Normalize rotation to 0, 90, 180, or 270
    let rotation = ((cmd.rotate % 360) + 360) % 360;
    if rotation != 0 && rotation != 90 && rotation != 180 && rotation != 270 {
        anyhow::bail!("Rotation must be 0, 90, 180, or 270 degrees");
    }

    let transform_desc = if rotation != 0 || cmd.flip_h || cmd.flip_v {
        let mut parts = Vec::new();
        if rotation != 0 {
            parts.push(format!("rotated {}°", rotation));
        }
        if cmd.flip_h {
            parts.push("flipped horizontally".to_string());
        }
        if cmd.flip_v {
            parts.push("flipped vertically".to_string());
        }
        format!(" ({})", parts.join(", "))
    } else {
        String::new()
    };

    println!(
        "Pasting {} entities at ({}, {}){}",
        blueprint.entities.len(),
        target.x,
        target.y,
        transform_desc
    );

    if cmd.dry_run {
        println!("\nDry run - would place:");
        for e in &blueprint.entities {
            let (tx, ty) = transform_position(e.pos[0], e.pos[1], rotation, cmd.flip_h, cmd.flip_v);
            let abs_x = target.x + tx;
            let abs_y = target.y + ty;
            let new_dir = transform_direction(&e.dir, rotation, cmd.flip_h, cmd.flip_v);
            let entity_type = if cmd.ghosts { "ghost" } else { "entity" };
            println!(
                "  {} {} at ({:.1}, {:.1}) facing {}",
                entity_type, e.name, abs_x, abs_y, new_dir
            );
        }
        client.close().await?;
        return Ok(());
    }

    // Place entities
    let mut placed = 0;
    let mut errors = Vec::new();

    for e in &blueprint.entities {
        let (tx, ty) = transform_position(e.pos[0], e.pos[1], rotation, cmd.flip_h, cmd.flip_v);
        let abs_pos = Position {
            x: target.x + tx,
            y: target.y + ty,
        };
        let new_dir_str = transform_direction(&e.dir, rotation, cmd.flip_h, cmd.flip_v);
        let direction = parse_direction_str(&new_dir_str);

        let result = if cmd.ghosts {
            client.place_ghost(&e.name, abs_pos, direction).await
        } else {
            client.place_entity(&e.name, abs_pos, direction).await
        };

        match result {
            Ok(entity) => {
                placed += 1;
                let entity_type = if cmd.ghosts { "ghost" } else { "" };
                println!(
                    "  Placed {} {} #{} at ({:.1}, {:.1})",
                    entity_type,
                    entity.name,
                    entity.unit_number.unwrap_or(0),
                    entity.position.x,
                    entity.position.y
                );
            }
            Err(err) => {
                errors.push(format!(
                    "Failed to place {} at ({:.1}, {:.1}): {}",
                    e.name, abs_pos.x, abs_pos.y, err
                ));
            }
        }
    }

    println!("\nPlaced {}/{} entities", placed, blueprint.entities.len());
    if !errors.is_empty() {
        println!("Errors:");
        for err in &errors {
            println!("  {}", err);
        }
    }

    client.close().await?;
    Ok(())
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

/// Transform a position based on rotation and flip
/// Rotation is clockwise in degrees (0, 90, 180, 270)
fn transform_position(x: f64, y: f64, rotation: i32, flip_h: bool, flip_v: bool) -> (f64, f64) {
    // Apply rotation first (clockwise)
    let (rx, ry) = match rotation {
        90 => (-y, x),   // 90° clockwise: (x,y) -> (-y, x)
        180 => (-x, -y), // 180°: (x,y) -> (-x, -y)
        270 => (y, -x),  // 270° clockwise: (x,y) -> (y, -x)
        _ => (x, y),     // 0° or invalid
    };

    // Apply flips
    let fx = if flip_h { -rx } else { rx };
    let fy = if flip_v { -ry } else { ry };

    (fx, fy)
}

/// Transform a direction string based on rotation and flip
fn transform_direction(dir: &str, rotation: i32, flip_h: bool, flip_v: bool) -> String {
    // Direction order: n=0, ne=1, e=2, se=3, s=4, sw=5, w=6, nw=7
    let dir_index = match dir.to_lowercase().as_str() {
        "n" | "north" => 0,
        "ne" | "northeast" => 1,
        "e" | "east" => 2,
        "se" | "southeast" => 3,
        "s" | "south" => 4,
        "sw" | "southwest" => 5,
        "w" | "west" => 6,
        "nw" | "northwest" => 7,
        _ => 0,
    };

    // Rotation adds to direction (clockwise)
    let rotation_steps = rotation / 45;
    let mut new_index = (dir_index + rotation_steps) % 8;

    // Horizontal flip mirrors east-west (reflects across Y axis)
    if flip_h {
        new_index = match new_index {
            1 => 7, // ne -> nw
            2 => 6, // e -> w
            3 => 5, // se -> sw
            5 => 3, // sw -> se
            6 => 2, // w -> e
            7 => 1, // nw -> ne
            x => x, // n, s stay same
        };
    }

    // Vertical flip mirrors north-south (reflects across X axis)
    if flip_v {
        new_index = match new_index {
            0 => 4, // n -> s
            1 => 3, // ne -> se
            3 => 1, // se -> ne
            4 => 0, // s -> n
            5 => 7, // sw -> nw
            7 => 5, // nw -> sw
            x => x, // e, w stay same
        };
    }

    match new_index {
        0 => "n",
        1 => "ne",
        2 => "e",
        3 => "se",
        4 => "s",
        5 => "sw",
        6 => "w",
        7 => "nw",
        _ => "n",
    }
    .to_string()
}

/// Parse direction string to Direction enum
fn parse_direction_str(s: &str) -> Direction {
    Direction::from_name(s).unwrap_or(Direction::North)
}
