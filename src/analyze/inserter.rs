//! Inserter pickup/dropoff analysis

use super::{EntityRef, InserterAnalysis};
use crate::world::{entity_size, Direction, Entity, TilePos};
use std::collections::HashMap;

/// Analyze all inserters in the entity list
pub fn analyze_inserters(entities: &[Entity]) -> Vec<InserterAnalysis> {
    let entity_at = build_entity_lookup(entities);

    entities
        .iter()
        .filter(|e| e.name.contains("inserter"))
        .filter_map(|inserter| analyze_single_inserter(inserter, &entity_at))
        .collect()
}

fn build_entity_lookup(entities: &[Entity]) -> HashMap<TilePos, &Entity> {
    let mut entity_at: HashMap<TilePos, &Entity> = HashMap::new();
    for entity in entities {
        for tile in occupied_tiles(entity) {
            match entity_at.get(&tile) {
                Some(existing) if entity_priority(existing) >= entity_priority(entity) => {}
                _ => {
                    entity_at.insert(tile, entity);
                }
            }
        }
    }
    entity_at
}

fn occupied_tiles(entity: &Entity) -> Vec<TilePos> {
    let (left, top, right, bottom) = match entity.bounding_box {
        Some(bounds) => (
            bounds.left_top.x.floor() as i32,
            bounds.left_top.y.floor() as i32,
            bounds.right_bottom.x.ceil() as i32,
            bounds.right_bottom.y.ceil() as i32,
        ),
        None => {
            let (width, height) = entity_size(&entity.name);
            let half_width = width as f64 / 2.0;
            let half_height = height as f64 / 2.0;
            (
                (entity.position.x - half_width).floor() as i32,
                (entity.position.y - half_height).floor() as i32,
                (entity.position.x + half_width).ceil() as i32,
                (entity.position.y + half_height).ceil() as i32,
            )
        }
    };
    (left..right)
        .flat_map(|x| (top..bottom).map(move |y| TilePos::new(x, y)))
        .collect()
}

fn entity_priority(entity: &Entity) -> u8 {
    match entity.entity_type.as_deref() {
        Some("resource") => 0,
        Some("tree") | Some("simple-entity") => 1,
        _ => 2,
    }
}

/// Analyze a single inserter
fn analyze_single_inserter(
    inserter: &Entity,
    entity_at: &HashMap<TilePos, &Entity>,
) -> Option<InserterAnalysis> {
    let unit_number = inserter.unit_number?;
    let position = inserter.position.to_tile();
    let direction = Direction::from_factorio(inserter.direction);

    // Standard inserters FACE a direction (where they PICK from)
    // and DROP items to the OPPOSITE direction
    // direction = pickup direction, opposite = dropoff direction
    let pickup_position = position.offset_in_direction(direction);
    let dropoff_position = position.offset_in_direction(direction.opposite());

    // Check for long inserter (picks up 2 tiles away in facing direction)
    let is_long = inserter.name.contains("long");
    let pickup_position = if is_long {
        pickup_position.offset_in_direction(direction) // One more tile in same direction
    } else {
        pickup_position
    };

    let pickup_target = entity_at.get(&pickup_position).map(|e| EntityRef {
        unit_number: e.unit_number,
        name: e.name.clone(),
        entity_type: e.entity_type.clone().unwrap_or_default(),
        position: pickup_position,
    });

    let dropoff_target = entity_at.get(&dropoff_position).map(|e| EntityRef {
        unit_number: e.unit_number,
        name: e.name.clone(),
        entity_type: e.entity_type.clone().unwrap_or_default(),
        position: dropoff_position,
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
        .filter(|i| i.pickup_position == target || i.dropoff_position == target)
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
        }
    }

    #[test]
    fn test_inserter_positions() {
        let entities = vec![make_inserter(1, 0, Direction::East, "inserter")];

        let results = analyze_inserters(&entities);
        assert_eq!(results.len(), 1);

        let analysis = &results[0];
        assert_eq!(analysis.position, TilePos::new(1, 0));
        assert_eq!(analysis.pickup_position, TilePos::new(2, 0)); // In front (east) - where inserter faces/picks
        assert_eq!(analysis.dropoff_position, TilePos::new(0, 0)); // Behind (west) - opposite of facing
    }

    #[test]
    fn test_long_inserter() {
        let entities = vec![make_inserter(2, 0, Direction::East, "long-handed-inserter")];

        let results = analyze_inserters(&entities);
        assert_eq!(results.len(), 1);

        let analysis = &results[0];
        assert_eq!(analysis.pickup_position, TilePos::new(4, 0)); // 2 tiles in front (where inserter faces/picks)
        assert_eq!(analysis.dropoff_position, TilePos::new(1, 0)); // 1 tile behind (opposite of facing)
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
        let mut assembler = make_typed_entity(
            3,
            0,
            "assembling-machine-1",
            "assembling-machine",
        );
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
            result[0].dropoff_target.as_ref().map(|target| target.name.as_str()),
            Some("assembling-machine-1")
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
    fn test_find_inserters_at_position() {
        let entities = vec![
            make_inserter(1, 0, Direction::East, "inserter"), // drops at (2,0)
            make_inserter(3, 0, Direction::West, "inserter"), // drops at (2,0)
        ];

        let at_2_0 = find_inserters_at_position(&entities, TilePos::new(2, 0));
        assert_eq!(at_2_0.len(), 2); // Both inserters interact with (2,0)
    }
}
