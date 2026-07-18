//! Inserter pickup/dropoff analysis

use super::{build_entity_occupancy_lookup, InserterAnalysis, InserterTargetRef};
use crate::world::{Direction, Entity, Position, TilePos};
use std::collections::HashMap;

/// Analyze all inserters in the entity list
pub fn analyze_inserters(entities: &[Entity]) -> Vec<InserterAnalysis> {
    let entity_at = build_entity_occupancy_lookup(entities);

    entities
        .iter()
        .filter(|e| e.name.contains("inserter"))
        .filter_map(|inserter| analyze_single_inserter(inserter, &entity_at))
        .collect()
}

/// Analyze a single inserter
fn analyze_single_inserter(
    inserter: &Entity,
    entity_at: &HashMap<TilePos, &Entity>,
) -> Option<InserterAnalysis> {
    let unit_number = inserter.unit_number?;
    let position = inserter.position;
    let direction = Direction::from_factorio(inserter.direction);

    // Standard inserters FACE a direction (where they PICK from)
    // and DROP items to the OPPOSITE direction
    // direction = pickup direction, opposite = dropoff direction
    let fallback_pickup_tile = position.to_tile().offset_in_direction(direction);
    let fallback_dropoff_tile = position.to_tile().offset_in_direction(direction.opposite());

    // Check for long inserter (picks up 2 tiles away in facing direction)
    let is_long = inserter.name.contains("long");
    let fallback_pickup_tile = if is_long {
        fallback_pickup_tile.offset_in_direction(direction) // One more tile in same direction
    } else {
        fallback_pickup_tile
    };

    // Live entity summaries carry Factorio's exact interaction points. The
    // tile-derived fallback keeps offline analyses usable for older fixtures,
    // but normal MCP traffic must use the authoritative values.
    let pickup_position = inserter
        .pickup_position
        .unwrap_or_else(|| fallback_pickup_tile.to_world_1x1());
    let dropoff_position = inserter
        .drop_position
        .unwrap_or_else(|| fallback_dropoff_tile.to_world_1x1());
    let pickup_tile = pickup_position.to_tile();
    let dropoff_tile = dropoff_position.to_tile();

    let pickup_target = entity_at.get(&pickup_tile).map(|e| InserterTargetRef {
        unit_number: e.unit_number,
        name: e.name.clone(),
        entity_type: e.entity_type.clone().unwrap_or_default(),
        position: e.position,
    });

    let dropoff_target = entity_at.get(&dropoff_tile).map(|e| InserterTargetRef {
        unit_number: e.unit_number,
        name: e.name.clone(),
        entity_type: e.entity_type.clone().unwrap_or_default(),
        position: e.position,
    });

    Some(InserterAnalysis {
        unit_number,
        position,
        direction,
        inserter_type: inserter.name.clone(),
        pickup_position,
        dropoff_position,
        pickup_target,
        dropoff_target,
    })
}

/// Find inserters that interact with a specific position
pub fn find_inserters_at_position(entities: &[Entity], target: TilePos) -> Vec<InserterAnalysis> {
    analyze_inserters(entities)
        .into_iter()
        .filter(|i| i.pickup_position.to_tile() == target || i.dropoff_position.to_tile() == target)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::Position;

    fn make_inserter(x: i32, y: i32, dir: Direction, name: &str) -> Entity {
        Entity {
            unit_number: Some((x * 100 + y) as u32),
            name: name.to_string(),
            entity_type: Some("inserter".to_string()),
            position: Position::new(x as f64 + 0.5, y as f64 + 0.5),
            direction: dir.to_factorio(),
            health: Some(100.0),
            force: Some("player".to_string()),
            bounding_box: None,
            pickup_position: None,
            drop_position: None,
            belt_to_ground_type: None,
            underground_belt_neighbour: None,
            belt_input_neighbours: Vec::new(),
            belt_output_neighbours: Vec::new(),
            belt_neighbours_observed: false,
        }
    }

    fn make_entity(x: i32, y: i32, name: &str) -> Entity {
        make_typed_entity(x, y, name, name)
    }

    fn make_typed_entity(x: i32, y: i32, name: &str, entity_type: &str) -> Entity {
        Entity {
            unit_number: Some((x * 1000 + y) as u32),
            name: name.to_string(),
            entity_type: Some(entity_type.to_string()),
            position: Position::new(x as f64 + 0.5, y as f64 + 0.5),
            direction: 0,
            health: Some(100.0),
            force: Some("player".to_string()),
            bounding_box: None,
            pickup_position: None,
            drop_position: None,
            belt_to_ground_type: None,
            underground_belt_neighbour: None,
            belt_input_neighbours: Vec::new(),
            belt_output_neighbours: Vec::new(),
            belt_neighbours_observed: false,
        }
    }

    #[test]
    fn test_inserter_positions() {
        let entities = vec![make_inserter(1, 0, Direction::East, "inserter")];

        let results = analyze_inserters(&entities);
        assert_eq!(results.len(), 1);

        let analysis = &results[0];
        assert_eq!(analysis.position, Position::new(1.5, 0.5));
        assert_eq!(analysis.pickup_position, Position::new(2.5, 0.5)); // In front (east) - where inserter faces/picks
        assert_eq!(analysis.dropoff_position, Position::new(0.5, 0.5)); // Behind (west) - opposite of facing
    }

    #[test]
    fn test_long_inserter() {
        let entities = vec![make_inserter(2, 0, Direction::East, "long-handed-inserter")];

        let results = analyze_inserters(&entities);
        assert_eq!(results.len(), 1);

        let analysis = &results[0];
        assert_eq!(analysis.pickup_position, Position::new(4.5, 0.5)); // 2 tiles in front (where inserter faces/picks)
        assert_eq!(analysis.dropoff_position, Position::new(1.5, 0.5)); // 1 tile behind (opposite of facing)
    }

    #[test]
    fn test_inserter_with_targets() {
        let entities = vec![
            make_entity(0, 0, "iron-chest"),
            make_inserter(1, 0, Direction::East, "inserter"),
            make_entity(2, 0, "transport-belt"),
        ];

        let results = analyze_inserters(&entities);
        assert_eq!(results.len(), 1);

        let analysis = &results[0];
        assert!(analysis.pickup_target.is_some());
        assert_eq!(
            analysis.pickup_target.as_ref().unwrap().name,
            "transport-belt"
        ); // East-facing picks from east
        assert!(analysis.dropoff_target.is_some());
        assert_eq!(analysis.dropoff_target.as_ref().unwrap().name, "iron-chest");
        // Drops to west (opposite)
    }

    #[test]
    fn inserter_resolves_edge_tile_of_multitile_machine() {
        let mut assembler = make_typed_entity(3, 0, "assembling-machine-1", "assembling-machine");
        assembler.position = Position::new(3.5, 0.5);
        assembler.bounding_box = Some(crate::world::Area::new(2.0, -1.0, 5.0, 2.0));
        let entities = vec![
            make_entity(0, 0, "transport-belt"),
            make_inserter(1, 0, Direction::West, "inserter"),
            assembler,
        ];

        let result = analyze_inserters(&entities);
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0]
                .dropoff_target
                .as_ref()
                .map(|target| target.name.as_str()),
            Some("assembling-machine-1")
        );
        assert_eq!(
            result[0].dropoff_target.as_ref().unwrap().position,
            Position::new(3.5, 0.5)
        );
    }

    #[test]
    fn target_position_is_the_entity_center_for_every_occupied_tile() {
        let mut assembler = make_typed_entity(10, 10, "assembling-machine-1", "assembling-machine");
        assembler.position = Position::new(10.5, 10.5);
        assembler.bounding_box = Some(crate::world::Area::new(9.0, 9.0, 12.0, 12.0));
        let assembler_unit = assembler.unit_number;

        let mut west_inserter = make_inserter(8, 9, Direction::East, "inserter");
        west_inserter.pickup_position = Some(Position::new(7.5, 9.5));
        west_inserter.drop_position = Some(Position::new(9.5, 9.5));
        let mut east_inserter = make_inserter(12, 11, Direction::West, "inserter");
        east_inserter.pickup_position = Some(Position::new(13.5, 11.5));
        east_inserter.drop_position = Some(Position::new(11.5, 11.5));

        let result = analyze_inserters(&[west_inserter, east_inserter, assembler]);
        assert_eq!(result.len(), 2);
        for analysis in result {
            let target = analysis.dropoff_target.expect("assembler target");
            assert_eq!(target.unit_number, assembler_unit);
            assert_eq!(target.position, Position::new(10.5, 10.5));
        }
    }

    #[test]
    fn inserter_resolves_edge_of_three_by_three_drill_without_bounding_box() {
        let entities = vec![
            make_entity(0, 0, "transport-belt"),
            make_inserter(1, 0, Direction::East, "inserter"),
            make_typed_entity(3, 0, "electric-mining-drill", "mining-drill"),
        ];

        let result = analyze_inserters(&entities);
        assert_eq!(result.len(), 1);
        assert_eq!(
            result[0]
                .pickup_target
                .as_ref()
                .map(|target| target.name.as_str()),
            Some("electric-mining-drill")
        );
        assert_eq!(
            result[0]
                .dropoff_target
                .as_ref()
                .map(|target| target.name.as_str()),
            Some("transport-belt")
        );
    }

    #[test]
    fn test_inserter_targets_prefer_belt_over_resource_on_same_tile() {
        let entities = vec![
            make_inserter(59, -23, Direction::South, "inserter"),
            make_typed_entity(59, -22, "iron-ore", "resource"),
            make_typed_entity(59, -22, "transport-belt", "transport-belt"),
            make_typed_entity(59, -24, "stone-furnace", "furnace"),
        ];

        let results = analyze_inserters(&entities);
        assert_eq!(results.len(), 1);

        let analysis = &results[0];
        assert_eq!(
            analysis.pickup_target.as_ref().unwrap().name,
            "transport-belt"
        );
        assert_eq!(
            analysis.dropoff_target.as_ref().unwrap().name,
            "stone-furnace"
        );
    }

    #[test]
    fn uses_authoritative_geometry_and_prefers_building_over_loose_item() {
        let mut inserter = make_inserter(-40, 59, Direction::East, "long-handed-inserter");
        inserter.position = Position::new(-39.5, 59.5);
        inserter.pickup_position = Some(Position::new(-37.5, 59.5));
        inserter.drop_position = Some(Position::new(-41.699, 59.5));

        let entities = vec![
            inserter,
            make_typed_entity(-41, 59, "transport-belt", "transport-belt"),
            make_typed_entity(-42, 59, "iron-chest", "container"),
            make_typed_entity(-42, 59, "item-on-ground", "item-entity"),
        ];

        let results = analyze_inserters(&entities);
        assert_eq!(results.len(), 1);
        let analysis = &results[0];
        assert_eq!(analysis.position, Position::new(-39.5, 59.5));
        assert_eq!(analysis.pickup_position, Position::new(-37.5, 59.5));
        assert_eq!(analysis.dropoff_position, Position::new(-41.699, 59.5));
        assert_eq!(analysis.dropoff_position.to_tile(), TilePos::new(-42, 59));
        assert_eq!(
            analysis
                .dropoff_target
                .as_ref()
                .map(|target| target.name.as_str()),
            Some("iron-chest")
        );
    }

    #[test]
    fn test_find_inserters_at_position() {
        let entities = vec![
            make_inserter(1, 0, Direction::East, "inserter"), // drops at (2,0)
            make_inserter(3, 0, Direction::West, "inserter"), // drops at (2,0)
        ];

        let at_2_0 = find_inserters_at_position(&entities, TilePos::new(2, 0));
        assert_eq!(at_2_0.len(), 2); // Both inserters interact with (2,0)
    }
}
