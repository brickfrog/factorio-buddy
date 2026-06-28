//! Result types for high-level operations

use serde::{Deserialize, Serialize};

use super::entity::InventoryItem;
use super::Entity;
use super::Position;
use super::TilePos;

/// Result of a gather operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatherResult {
    pub success: bool,
    pub resource_name: String,
    pub gathered: u32,
    pub distance_walked: f64,
    #[serde(default, deserialize_with = "super::deserialize_lua_empty_vec")]
    pub inventory: Vec<InventoryItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Result of a walk-to operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalkResult {
    pub arrived: bool,
    pub final_position: Position,
    pub distance_walked: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Result of a build operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildResult {
    pub placed: u32,
    pub total: u32,
    #[serde(default, deserialize_with = "super::deserialize_lua_empty_vec")]
    pub entities: Vec<Entity>,
    #[serde(default, deserialize_with = "super::deserialize_lua_empty_vec")]
    pub errors: Vec<String>,
}

/// Entity placement specification for bulk placement
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlacementSpec {
    pub name: String,
    pub position: (f64, f64),
    #[serde(default)]
    pub direction: Option<String>,
}

/// Summary of items on a single belt
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeltItemSummary {
    pub position: Position,
    pub unit_number: u32,
    #[serde(default, deserialize_with = "super::deserialize_lua_empty_vec")]
    pub items: Vec<InventoryItem>,
}

/// Result of querying belt contents
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeltContentsResult {
    pub belt_count: u32,
    pub total_items: u32,
    #[serde(default, deserialize_with = "super::deserialize_lua_empty_vec")]
    pub item_summary: Vec<InventoryItem>,
    #[serde(default, deserialize_with = "super::deserialize_lua_empty_vec")]
    pub belts: Vec<BeltItemSummary>,
}

/// Items on a single lane of a transport belt
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaneContents {
    /// Lane number: 1=left (line 1), 2=right (line 2)
    pub lane: u8,
    /// Items on this lane
    #[serde(default, deserialize_with = "super::deserialize_lua_empty_vec")]
    pub items: Vec<InventoryItem>,
    /// Total count of items on this lane
    pub item_count: u32,
}

/// Summary of a single belt with lane separation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeltLaneSummary {
    /// Belt position (tile coordinates)
    pub position: TilePos,
    /// Belt entity unit number
    pub unit_number: u32,
    /// Belt direction (Factorio direction value)
    pub direction: u8,
    /// Belt entity name (e.g., "transport-belt")
    pub belt_type: String,
    /// Contents of left lane (line 1)
    pub left_lane: LaneContents,
    /// Contents of right lane (line 2)
    pub right_lane: LaneContents,
}

/// Result of querying belt contents with lane separation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeltLaneContentsResult {
    /// Number of belts with items
    pub belt_count: u32,
    /// Total items across all belts
    pub total_items: u32,
    /// Summary of items by name (combined from both lanes)
    #[serde(default, deserialize_with = "super::deserialize_lua_empty_vec")]
    pub item_summary: Vec<InventoryItem>,
    /// Per-belt lane information
    #[serde(default, deserialize_with = "super::deserialize_lua_empty_vec")]
    pub belts: Vec<BeltLaneSummary>,
}
