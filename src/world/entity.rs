//! Entity types and operations

use serde::{Deserialize, Serialize};

use super::{Area, Direction, Position};

/// A Factorio entity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    /// Unique identifier for this entity
    #[serde(default)]
    pub unit_number: Option<u32>,

    /// Entity prototype name (e.g., "iron-chest", "burner-mining-drill")
    pub name: String,

    /// Entity type (e.g., "container", "mining-drill")
    #[serde(rename = "type")]
    pub entity_type: Option<String>,

    /// Position in the world
    pub position: Position,

    /// Direction the entity is facing
    #[serde(default)]
    pub direction: u8,

    /// Current health
    #[serde(default)]
    pub health: Option<f64>,

    /// Force/team this entity belongs to
    #[serde(default)]
    pub force: Option<String>,

    /// Collision bounding box in world coordinates
    #[serde(default)]
    pub bounding_box: Option<Area>,
}

impl Entity {
    /// Get the direction as an enum
    pub fn direction_enum(&self) -> Direction {
        Direction::from_factorio(self.direction)
    }
}

/// Character status information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterStatus {
    /// Whether the character entity is valid
    pub valid: bool,

    /// Unit number (if valid)
    #[serde(default)]
    pub unit_number: Option<u32>,

    /// Current position
    #[serde(default)]
    pub position: Option<Position>,

    /// Current health
    #[serde(default)]
    pub health: Option<f64>,

    /// Number of items in crafting queue
    #[serde(default)]
    pub crafting_queue_size: Option<u32>,

    /// Whether the character is currently walking
    #[serde(default)]
    pub walking: Option<bool>,

    /// Whether the character is currently mining
    #[serde(default)]
    pub mining: Option<bool>,
}

/// Result of a mining operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MineResult {
    /// Whether mining was successful
    pub success: bool,

    /// Number of entities mined
    #[serde(default)]
    pub mined_count: u32,

    /// Error message if failed
    #[serde(default)]
    pub error: Option<String>,

    /// Current inventory after mining
    #[serde(default, deserialize_with = "super::deserialize_lua_empty_vec")]
    pub inventory: Vec<InventoryItem>,
}

/// An item in the crafting queue
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CraftQueueItem {
    /// Recipe name
    pub recipe: String,

    /// Count being crafted
    pub count: u32,
}

/// Result of a crafting operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CraftResult {
    /// Whether crafting started successfully
    pub success: bool,

    /// Number of items queued for crafting
    #[serde(default)]
    pub queued: u32,

    /// Current crafting queue size
    #[serde(default)]
    pub queue_size: u32,

    /// Full crafting queue (includes auto-queued intermediates)
    #[serde(default, deserialize_with = "super::deserialize_lua_empty_vec")]
    pub queue: Vec<CraftQueueItem>,

    /// Error message if failed
    #[serde(default)]
    pub error: Option<String>,

    /// Recipe requested by the caller
    #[serde(default)]
    pub recipe: Option<String>,
}

/// An item in an inventory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryItem {
    /// Item name
    pub name: String,

    /// Item count
    pub count: u32,
}
