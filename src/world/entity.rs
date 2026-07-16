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

    /// Authoritative pickup point exposed by Factorio for inserters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pickup_position: Option<Position>,

    /// Authoritative drop point exposed by Factorio for inserters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drop_position: Option<Position>,
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
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct CraftQueueItem {
    /// Recipe name
    pub recipe: String,

    /// Count being crafted
    pub count: u32,
}

/// One live observation of the standalone character's complete crafting queue.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct CraftingQueueSnapshot {
    pub queue_size: u32,

    #[serde(default)]
    pub current_recipe: Option<String>,

    #[serde(default, deserialize_with = "super::deserialize_lua_empty_vec")]
    pub queue: Vec<CraftQueueItem>,
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

    /// Stable save-persisted transaction identity for accepted MCP/CLI crafts.
    #[serde(default)]
    pub operation_id: Option<String>,

    /// Stable machine-facing rejection category, when present.
    #[serde(default)]
    pub error_kind: Option<String>,

    /// Recipe requested by the caller
    #[serde(default)]
    pub recipe: Option<String>,
}

/// Requested deterministic output proof captured before craft admission.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct CraftProductExpectation {
    pub name: String,
    pub before_count: u32,
    pub expected_increase: u32,
}

/// Complete deterministic production and consumption accounting for a craft
/// queue admitted from an initially empty queue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CraftItemFlow {
    pub name: String,
    pub produced: u32,
    pub consumed: u32,
    /// Force/surface production-statistics baseline captured immediately after
    /// the exact queue was admitted, before Factorio can advance a game tick.
    #[serde(default)]
    pub production_before: f64,
    /// Matching native-consumption baseline for the admitted queue.
    #[serde(default)]
    pub consumption_before: f64,
}

/// Craft truth persisted in the Factorio save so a later MCP process can
/// finish the exact transaction after a turn boundary or restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CraftAdmissionRecord {
    pub operation_id: String,
    pub admitted_at_tick: u64,
    /// Exact standalone character context that owns this transaction.
    #[serde(default)]
    pub character_unit_number: Option<u32>,
    #[serde(default)]
    pub force_name: Option<String>,
    #[serde(default)]
    pub surface_name: Option<String>,
    /// Recomputed by the mod for active admissions and terminal receipts.
    #[serde(default)]
    pub identity_valid: bool,
    /// True when this record is the replayable terminal acknowledgement rather
    /// than a currently active craft admission.
    #[serde(default)]
    pub completion_receipt: bool,
    #[serde(default)]
    pub terminal_status: Option<String>,
    #[serde(default)]
    pub identity_error: Option<serde_json::Value>,
    pub result: CraftResult,
    #[serde(default, deserialize_with = "super::deserialize_lua_empty_vec")]
    pub products: Vec<CraftProductExpectation>,
    #[serde(default)]
    pub product_proof_complete: bool,
    #[serde(default, deserialize_with = "super::deserialize_lua_empty_vec")]
    pub flows: Vec<CraftItemFlow>,
    #[serde(default)]
    pub flow_accounting_complete: bool,
}

/// The observed lifecycle state of a character-crafting request.
///
/// `Accepted` and `Queued` describe admission, not item production. Only
/// `Completed` proves that a later queue observation reached zero.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CraftingStatus {
    Rejected,
    Accepted,
    Queued,
    Pending,
    Completed,
    TimedOut,
}

/// Structured evidence from admitting, observing, or waiting for crafting.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct CraftingStatusEvidence {
    pub status: CraftingStatus,

    /// Recipe associated with the admission result, when known.
    #[serde(default)]
    pub recipe: Option<String>,

    /// Number of crafts accepted by `begin_crafting`, when known.
    #[serde(default)]
    pub accepted_count: u32,

    /// Recipe executing at the most recent live queue observation. This can be
    /// an automatically queued intermediate rather than `recipe`.
    #[serde(default)]
    pub current_recipe: Option<String>,

    /// Exact remaining queue at the most recent live observation.
    #[serde(default, deserialize_with = "super::deserialize_lua_empty_vec")]
    pub remaining_queue: Vec<CraftQueueItem>,

    /// Queue size at admission or at the first polling observation.
    pub initial_queue_size: u32,

    /// Queue size in the most recent observation.
    pub remaining_queue_size: u32,

    /// Number of queue observations made while producing this evidence.
    #[serde(default)]
    pub polls: u32,

    /// Wall-clock time spent observing the queue.
    #[serde(default)]
    pub elapsed_ms: u64,
}

impl CraftResult {
    /// Classify what the immediate `begin_crafting` response actually proves.
    pub fn status(&self) -> CraftingStatus {
        if !self.success {
            CraftingStatus::Rejected
        } else if self.queue_size > 0 {
            CraftingStatus::Queued
        } else {
            CraftingStatus::Accepted
        }
    }

    /// Return admission evidence without claiming that any item was produced.
    pub fn status_evidence(&self) -> CraftingStatusEvidence {
        CraftingStatusEvidence {
            status: self.status(),
            recipe: self.recipe.clone(),
            accepted_count: self.queued,
            current_recipe: self.queue.first().map(|item| item.recipe.clone()),
            remaining_queue: self.queue.clone(),
            initial_queue_size: self.queue_size,
            remaining_queue_size: self.queue_size,
            polls: 0,
            elapsed_ms: 0,
        }
    }
}

impl CraftingStatusEvidence {
    /// Attach the original admission context to later queue observations.
    pub fn with_craft_result(mut self, result: &CraftResult) -> Self {
        self.recipe.clone_from(&result.recipe);
        self.accepted_count = result.queued;
        self.initial_queue_size = result.queue_size;
        self
    }

    pub fn is_completed(&self) -> bool {
        self.status == CraftingStatus::Completed && self.remaining_queue_size == 0
    }
}

/// An item in an inventory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryItem {
    /// Item name
    pub name: String,

    /// Item count
    pub count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn craft_result(success: bool, queued: u32, queue_size: u32) -> CraftResult {
        CraftResult {
            success,
            queued,
            queue_size,
            queue: Vec::new(),
            error: (!success).then(|| "not craftable".to_string()),
            operation_id: success.then(|| "craft-1-1".to_string()),
            error_kind: (!success).then(|| "craft_not_started".to_string()),
            recipe: Some("lab".to_string()),
        }
    }

    #[test]
    fn craft_admission_never_claims_completion() {
        assert_eq!(craft_result(false, 0, 0).status(), CraftingStatus::Rejected);
        assert_eq!(craft_result(true, 0, 0).status(), CraftingStatus::Accepted);
        assert_eq!(craft_result(true, 1, 0).status(), CraftingStatus::Accepted);
        assert_eq!(craft_result(true, 1, 1).status(), CraftingStatus::Queued);
    }

    #[test]
    fn polling_evidence_keeps_admission_context() {
        let admitted = craft_result(true, 1, 3);
        let completed = CraftingStatusEvidence {
            status: CraftingStatus::Completed,
            recipe: None,
            accepted_count: 0,
            current_recipe: None,
            remaining_queue: Vec::new(),
            initial_queue_size: 2,
            remaining_queue_size: 0,
            polls: 3,
            elapsed_ms: 500,
        }
        .with_craft_result(&admitted);

        assert!(completed.is_completed());
        assert_eq!(completed.recipe.as_deref(), Some("lab"));
        assert_eq!(completed.accepted_count, 1);
        assert_eq!(completed.initial_queue_size, 3);
    }

    #[test]
    fn crafting_status_serializes_as_stable_snake_case() {
        assert_eq!(
            serde_json::to_string(&CraftingStatus::TimedOut).unwrap(),
            r#""timed_out""#
        );
    }
}
