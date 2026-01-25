//! Result types for high-level operations

use serde::{Deserialize, Serialize};

use super::entity::InventoryItem;
use super::Position;
use super::Entity;

/// Result of a gather operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatherResult {
    pub success: bool,
    pub resource_name: String,
    pub gathered: u32,
    pub distance_walked: f64,
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
    pub entities: Vec<Entity>,
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
