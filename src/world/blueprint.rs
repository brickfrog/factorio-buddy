//! Blueprint data structures for declarative entity placement

use serde::{Deserialize, Serialize};

use super::{Direction, Position};

// ============================================================================
// Native Factorio Blueprint Types
// ============================================================================

/// Info about a stored blueprint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredBlueprint {
    pub name: String,
    pub entity_count: u32,
}

/// Result of exporting to native blueprint string
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeBlueprintExport {
    pub blueprint_string: String,
    pub entity_count: u32,
}

/// Result of placing a blueprint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueprintPlaceResult {
    pub success: bool,
    pub ghosts_created: u32,
    #[serde(default)]
    pub error: Option<String>,
}

/// Result of saving a blueprint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueprintSaveResult {
    pub success: bool,
    #[serde(default)]
    pub entity_count: u32,
    #[serde(default)]
    pub error: Option<String>,
}

/// A retrieved blueprint string
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueprintGetResult {
    #[serde(default)]
    pub blueprint_string: Option<String>,
    #[serde(default)]
    pub entity_count: u32,
    #[serde(default)]
    pub error: Option<String>,
}

// ============================================================================
// Custom JSON Blueprint Types (existing)
// ============================================================================

/// A blueprint describing desired entity placement
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blueprint {
    /// Blueprint name
    #[serde(default)]
    pub name: String,

    /// Origin point - all entity positions are relative to this
    #[serde(default)]
    pub origin: [f64; 2],

    /// Entities in the blueprint
    pub entities: Vec<BlueprintEntity>,
}

/// An entity in a blueprint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueprintEntity {
    /// Entity prototype name (e.g., "burner-mining-drill")
    pub name: String,

    /// Position relative to blueprint origin [x, y]
    pub pos: [f64; 2],

    /// Direction (n, e, s, w, or ne, se, sw, nw)
    #[serde(default = "default_direction")]
    pub dir: String,
}

fn default_direction() -> String {
    "n".to_string()
}

impl BlueprintEntity {
    /// Get absolute position given blueprint origin
    pub fn absolute_position(&self, origin: &[f64; 2]) -> Position {
        Position {
            x: origin[0] + self.pos[0],
            y: origin[1] + self.pos[1],
        }
    }

    /// Parse direction string to Direction enum
    pub fn direction(&self) -> Direction {
        match self.dir.to_lowercase().as_str() {
            "n" | "north" => Direction::North,
            "ne" | "northeast" => Direction::NorthEast,
            "e" | "east" => Direction::East,
            "se" | "southeast" => Direction::SouthEast,
            "s" | "south" => Direction::South,
            "sw" | "southwest" => Direction::SouthWest,
            "w" | "west" => Direction::West,
            "nw" | "northwest" => Direction::NorthWest,
            _ => Direction::North,
        }
    }
}

/// Result of diffing a blueprint against world state
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlueprintDiff {
    /// Entities to add
    pub add: Vec<DiffAdd>,

    /// Entities to remove
    pub remove: Vec<DiffRemove>,

    /// Entities to rotate
    pub rotate: Vec<DiffRotate>,
}

impl BlueprintDiff {
    pub fn is_empty(&self) -> bool {
        self.add.is_empty() && self.remove.is_empty() && self.rotate.is_empty()
    }

    pub fn total_changes(&self) -> usize {
        self.add.len() + self.remove.len() + self.rotate.len()
    }
}

/// An entity to add
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffAdd {
    pub name: String,
    pub position: Position,
    pub direction: Direction,
}

/// An entity to remove
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffRemove {
    pub unit_number: u32,
    pub name: String,
    pub position: Position,
}

/// An entity to rotate
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiffRotate {
    pub unit_number: u32,
    pub name: String,
    pub position: Position,
    pub from_direction: u8,
    pub to_direction: Direction,
}

/// Result of applying a blueprint
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyResult {
    /// Number of entities added
    pub added: usize,

    /// Number of entities removed
    pub removed: usize,

    /// Number of entities rotated
    pub rotated: usize,

    /// Errors encountered
    pub errors: Vec<String>,
}

impl Blueprint {
    /// Load blueprint from JSON string
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// Load blueprint from file
    pub fn from_file(path: &std::path::Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)?;
        Ok(Self::from_json(&content)?)
    }

    /// Get the bounding box of the blueprint (in absolute coordinates)
    pub fn bounding_box(&self) -> (Position, Position) {
        if self.entities.is_empty() {
            let origin = Position {
                x: self.origin[0],
                y: self.origin[1],
            };
            return (origin, origin);
        }

        let mut min_x = f64::MAX;
        let mut min_y = f64::MAX;
        let mut max_x = f64::MIN;
        let mut max_y = f64::MIN;

        for entity in &self.entities {
            let pos = entity.absolute_position(&self.origin);
            // Add some padding for entity size (most entities are 1-3 tiles)
            min_x = min_x.min(pos.x - 2.0);
            min_y = min_y.min(pos.y - 2.0);
            max_x = max_x.max(pos.x + 2.0);
            max_y = max_y.max(pos.y + 2.0);
        }

        (
            Position { x: min_x, y: min_y },
            Position { x: max_x, y: max_y },
        )
    }
}
