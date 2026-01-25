//! Output formatting for CLI results

mod human;
mod json;

use anyhow::Result;
use serde::Serialize;

/// Output format for commands
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum OutputFormat {
    /// Human-readable output
    #[default]
    Human,
    /// JSON output for machine consumption
    Json,
}

/// Trait for types that can be output
pub trait Outputable: Serialize {
    /// Format for human-readable output
    fn format_human(&self) -> String;
}

/// Output formatter
pub struct Output {
    format: OutputFormat,
}

impl Output {
    pub fn new(format: OutputFormat) -> Self {
        Self { format }
    }

    /// Print a value according to the output format
    pub fn print<T: Outputable>(&self, value: &T) -> Result<()> {
        match self.format {
            OutputFormat::Human => {
                println!("{}", value.format_human());
            }
            OutputFormat::Json => {
                println!("{}", serde_json::to_string_pretty(value)?);
            }
        }
        Ok(())
    }
}

// Implement Outputable for common types

impl Outputable for crate::world::Tick {
    fn format_human(&self) -> String {
        format!("Tick: {} ({:.1}s)", self.tick, self.to_seconds())
    }
}

impl Outputable for Vec<crate::world::Surface> {
    fn format_human(&self) -> String {
        if self.is_empty() {
            return "No surfaces".to_string();
        }
        self.iter()
            .map(|s| {
                format!(
                    "{} (index: {}, daytime: {:.2})",
                    s.name,
                    s.index,
                    s.daytime.unwrap_or(0.5)
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

impl Outputable for Vec<crate::world::Entity> {
    fn format_human(&self) -> String {
        if self.is_empty() {
            return "No entities found".to_string();
        }
        let mut lines = vec![format!("Found {} entities:", self.len())];
        for e in self {
            lines.push(format!(
                "  #{} {} at ({:.1}, {:.1})",
                e.unit_number.unwrap_or(0),
                e.name,
                e.position.x,
                e.position.y
            ));
        }
        lines.join("\n")
    }
}

impl Outputable for crate::world::Entity {
    fn format_human(&self) -> String {
        format!(
            "Entity #{}: {} at ({:.1}, {:.1})\n  Type: {}\n  Direction: {}\n  Health: {}\n  Force: {}",
            self.unit_number.unwrap_or(0),
            self.name,
            self.position.x,
            self.position.y,
            self.entity_type.as_deref().unwrap_or("unknown"),
            self.direction,
            self.health.map(|h| format!("{:.0}", h)).unwrap_or_else(|| "N/A".to_string()),
            self.force.as_deref().unwrap_or("none")
        )
    }
}

impl Outputable for Vec<crate::world::ResourcePatch> {
    fn format_human(&self) -> String {
        if self.is_empty() {
            return "No resources found".to_string();
        }
        let mut lines = vec![format!("Found {} resource patches:", self.len())];
        for r in self {
            lines.push(format!(
                "  {} at ({:.1}, {:.1}): {} total ({} tiles)",
                r.name, r.center.x, r.center.y, r.total_amount, r.tile_count
            ));
        }
        lines.join("\n")
    }
}

impl Outputable for crate::world::ResourcePatch {
    fn format_human(&self) -> String {
        format!(
            "Resource Patch: {}\n  Center: ({:.1}, {:.1})\n  Total: {}\n  Tiles: {}\n  Bounds: ({:.0},{:.0}) to ({:.0},{:.0})",
            self.name,
            self.center.x,
            self.center.y,
            self.total_amount,
            self.tile_count,
            self.bounding_box.left_top.x,
            self.bounding_box.left_top.y,
            self.bounding_box.right_bottom.x,
            self.bounding_box.right_bottom.y
        )
    }
}

impl Outputable for Vec<crate::world::Tile> {
    fn format_human(&self) -> String {
        if self.is_empty() {
            return "No tiles".to_string();
        }
        format!("Found {} tiles", self.len())
    }
}

impl Outputable for crate::world::Tile {
    fn format_human(&self) -> String {
        format!(
            "Tile: {} at ({}, {})\n  Walkable: {}",
            self.name,
            self.position.x as i32,
            self.position.y as i32,
            !self.collides_with_player
        )
    }
}

impl Outputable for crate::world::CharacterStatus {
    fn format_human(&self) -> String {
        if !self.valid {
            return "Character not initialized".to_string();
        }
        format!(
            "Character #{}\n  Position: ({:.1}, {:.1})\n  Health: {:.0}\n  Crafting queue: {}\n  Walking: {}",
            self.unit_number.unwrap_or(0),
            self.position.as_ref().map(|p| p.x).unwrap_or(0.0),
            self.position.as_ref().map(|p| p.y).unwrap_or(0.0),
            self.health.unwrap_or(0.0),
            self.crafting_queue_size.unwrap_or(0),
            self.walking.unwrap_or(false)
        )
    }
}

impl Outputable for crate::world::Inventory {
    fn format_human(&self) -> String {
        if self.items.is_empty() {
            return format!("Inventory: empty ({} free slots)", self.free_slots);
        }
        let mut lines = vec![format!(
            "Inventory ({} items, {} free slots):",
            self.items.len(),
            self.free_slots
        )];
        for item in &self.items {
            lines.push(format!("  {} x{}", item.name, item.count));
        }
        lines.join("\n")
    }
}

impl Outputable for crate::world::MineResult {
    fn format_human(&self) -> String {
        if !self.success {
            return format!(
                "Mining failed: {}",
                self.error.as_deref().unwrap_or("unknown error")
            );
        }
        format!("Mined {} entities", self.mined_count)
    }
}

impl Outputable for crate::world::CraftResult {
    fn format_human(&self) -> String {
        if !self.success {
            return format!(
                "Crafting failed: {}",
                self.error.as_deref().unwrap_or("unknown error")
            );
        }
        format!(
            "Queued {} items for crafting (queue size: {})",
            self.queued, self.queue_size
        )
    }
}

impl Outputable for crate::world::GatherResult {
    fn format_human(&self) -> String {
        if !self.success {
            return format!(
                "Gathering failed: {}",
                self.error.as_deref().unwrap_or("unknown error")
            );
        }
        let mut lines = vec![format!(
            "Gathered {} {} (walked {:.1} tiles)",
            self.gathered, self.resource_name, self.distance_walked
        )];
        if !self.inventory.is_empty() {
            lines.push("Inventory:".to_string());
            for item in &self.inventory {
                lines.push(format!("  {} x{}", item.name, item.count));
            }
        }
        lines.join("\n")
    }
}

impl Outputable for crate::world::WalkResult {
    fn format_human(&self) -> String {
        if self.arrived {
            format!(
                "Arrived at ({:.1}, {:.1}) after walking {:.1} tiles",
                self.final_position.x, self.final_position.y, self.distance_walked
            )
        } else {
            format!(
                "Stopped at ({:.1}, {:.1}) after walking {:.1} tiles: {}",
                self.final_position.x,
                self.final_position.y,
                self.distance_walked,
                self.reason.as_deref().unwrap_or("unknown reason")
            )
        }
    }
}

impl Outputable for crate::world::BuildResult {
    fn format_human(&self) -> String {
        let mut lines = vec![format!("Placed {}/{} entities", self.placed, self.total)];
        for entity in &self.entities {
            lines.push(format!(
                "  #{} {} at ({:.1}, {:.1})",
                entity.unit_number.unwrap_or(0),
                entity.name,
                entity.position.x,
                entity.position.y
            ));
        }
        if !self.errors.is_empty() {
            lines.push("Errors:".to_string());
            for err in &self.errors {
                lines.push(format!("  - {}", err));
            }
        }
        lines.join("\n")
    }
}
