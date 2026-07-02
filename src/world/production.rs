use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::Position;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityProduction {
    pub name: String,
    pub unit_number: Option<u32>,
    pub position: Position,
    pub status: String,
    pub products_finished: Option<u64>,
    pub working: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductionReport {
    pub entities: Vec<EntityProduction>,
    pub status_counts: BTreeMap<String, u32>,
    pub working_count: u32,
    pub total: u32,
}

pub fn build_production_report(entities: Vec<EntityProduction>) -> ProductionReport {
    let mut status_counts = BTreeMap::new();
    let mut working_count = 0;

    for entity in &entities {
        *status_counts.entry(entity.status.clone()).or_insert(0) += 1;
        if entity.working {
            working_count += 1;
        }
    }

    ProductionReport {
        total: entities.len() as u32,
        entities,
        status_counts,
        working_count,
    }
}
