//! Inventory types

use serde::{Deserialize, Serialize};

use super::entity::InventoryItem;

/// Inventory contents
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Inventory {
    /// Items in the inventory
    #[serde(default, deserialize_with = "super::deserialize_lua_empty_vec")]
    pub items: Vec<InventoryItem>,

    /// Number of free slots
    #[serde(default)]
    pub free_slots: u32,
}

impl Inventory {
    /// Get count of a specific item
    pub fn get_count(&self, item_name: &str) -> u32 {
        self.items
            .iter()
            .find(|i| i.name == item_name)
            .map(|i| i.count)
            .unwrap_or(0)
    }

    /// Check if inventory has at least N of an item
    pub fn has(&self, item_name: &str, count: u32) -> bool {
        self.get_count(item_name) >= count
    }

    /// Total number of items
    pub fn total_items(&self) -> u32 {
        self.items.iter().map(|i| i.count).sum()
    }
}
