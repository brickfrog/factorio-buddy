//! Entity reach analysis - find what entities can interact with a position

use crate::world::{Entity, TilePos};
use super::{analyze_inserters, EntityRef, EntityReachResult, InserterAnalysis};

/// Analyze what entities can interact with a given position
pub fn analyze_entity_reach(
    entities: &[Entity],
    origin: TilePos,
    radius: u32,
) -> EntityReachResult {
    let radius_sq = (radius * radius) as i32;

    // Find belts within range
    let belts: Vec<EntityRef> = entities
        .iter()
        .filter(|e| e.name.contains("belt"))
        .filter(|e| {
            let pos = e.position.to_tile();
            let dx = pos.x - origin.x;
            let dy = pos.y - origin.y;
            dx * dx + dy * dy <= radius_sq
        })
        .map(|e| EntityRef {
            unit_number: e.unit_number,
            name: e.name.clone(),
            entity_type: e.entity_type.clone().unwrap_or_default(),
            position: e.position.to_tile(),
        })
        .collect();

    // Find inserters that can reach this position
    let all_inserters = analyze_inserters(entities);
    let inserters: Vec<InserterAnalysis> = all_inserters
        .into_iter()
        .filter(|i| i.pickup_position == origin || i.dropoff_position == origin)
        .collect();

    // Find other interacting entities (assemblers, chests, etc.) within range
    let interacting_entities: Vec<EntityRef> = entities
        .iter()
        .filter(|e| !e.name.contains("belt") && !e.name.contains("inserter"))
        .filter(|e| {
            let pos = e.position.to_tile();
            let dx = pos.x - origin.x;
            let dy = pos.y - origin.y;
            dx * dx + dy * dy <= radius_sq
        })
        .map(|e| EntityRef {
            unit_number: e.unit_number,
            name: e.name.clone(),
            entity_type: e.entity_type.clone().unwrap_or_default(),
            position: e.position.to_tile(),
        })
        .collect();

    EntityReachResult {
        origin,
        radius,
        belts,
        inserters,
        interacting_entities,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{Direction, Position};

    fn make_entity(x: i32, y: i32, name: &str, dir: Direction) -> Entity {
        Entity {
            unit_number: Some((x * 100 + y) as u32),
            name: name.to_string(),
            entity_type: Some(name.to_string()),
            position: Position::new(x as f64 + 0.5, y as f64 + 0.5),
            direction: dir.to_factorio(),
            health: Some(100.0),
            force: Some("player".to_string()),
            bounding_box: None,
        }
    }

    #[test]
    fn test_belt_in_range() {
        let entities = vec![
            make_entity(0, 0, "transport-belt", Direction::East),
            make_entity(1, 0, "transport-belt", Direction::East),
            make_entity(10, 0, "transport-belt", Direction::East), // Far away
        ];

        let result = analyze_entity_reach(&entities, TilePos::new(0, 0), 2);

        assert_eq!(result.belts.len(), 2); // Only nearby belts
        assert!(result.belts.iter().any(|b| b.position == TilePos::new(0, 0)));
        assert!(result.belts.iter().any(|b| b.position == TilePos::new(1, 0)));
    }

    #[test]
    fn test_inserter_interaction() {
        let entities = vec![
            make_entity(0, 0, "iron-chest", Direction::North),
            make_entity(1, 0, "inserter", Direction::East), // Picks from (2,0), drops to chest at (0,0)
        ];

        let result = analyze_entity_reach(&entities, TilePos::new(0, 0), 3);

        // Should find the inserter that drops to our target position
        assert_eq!(result.inserters.len(), 1);
        assert_eq!(result.inserters[0].dropoff_position, TilePos::new(0, 0));
    }

    #[test]
    fn test_nearby_entities() {
        let entities = vec![
            make_entity(0, 0, "stone-furnace", Direction::North),
            make_entity(1, 0, "iron-chest", Direction::North),
            make_entity(100, 100, "steel-chest", Direction::North), // Far away
        ];

        let result = analyze_entity_reach(&entities, TilePos::new(0, 0), 2);

        assert_eq!(result.interacting_entities.len(), 2);
    }
}
