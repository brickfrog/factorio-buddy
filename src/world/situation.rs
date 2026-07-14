use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::{Entity, InventoryItem, Position, ResourcePatch};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SituationInventoryItem {
    pub name: String,
    pub count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SituationResourcePatch {
    pub name: String,
    pub center_x: f64,
    pub center_y: f64,
    pub total_amount: u64,
    pub tile_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SituationReport {
    pub position: Position,
    pub health: Option<f64>,
    pub walking: Option<bool>,
    pub tick: u64,
    pub inventory: Vec<SituationInventoryItem>,
    pub nearby_entities: BTreeMap<String, u32>,
    pub nearby_resources: Vec<SituationResourcePatch>,
    pub radius: u32,
}

#[allow(clippy::too_many_arguments)]
pub fn build_situation_report(
    position: Position,
    health: Option<f64>,
    walking: Option<bool>,
    tick: u64,
    inventory_items: Vec<InventoryItem>,
    nearby_entities: Vec<Entity>,
    nearby_resources: Vec<ResourcePatch>,
    radius: u32,
) -> SituationReport {
    let mut inventory: Vec<SituationInventoryItem> = inventory_items
        .into_iter()
        .map(|item| SituationInventoryItem {
            name: item.name,
            count: item.count,
        })
        .collect();
    inventory.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));

    let mut entity_counts = BTreeMap::new();
    for entity in nearby_entities {
        *entity_counts.entry(entity.name).or_insert(0) += 1;
    }

    let nearby_resources = nearby_resources
        .into_iter()
        .map(|resource| SituationResourcePatch {
            name: resource.name,
            center_x: resource.center.x,
            center_y: resource.center.y,
            total_amount: resource.total_amount,
            tile_count: resource.tile_count,
        })
        .collect();

    SituationReport {
        position,
        health,
        walking,
        tick,
        inventory,
        nearby_entities: entity_counts,
        nearby_resources,
        radius,
    }
}
