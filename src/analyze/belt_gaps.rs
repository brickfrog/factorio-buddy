//! Belt gap detection for finding breaks in belt networks

use super::{build_entity_occupancy_lookup, BeltGap, BeltGapResult, BeltGraph, GapType};
use crate::world::{Direction, Entity, TilePos};

fn modeled_inserter_pickup_tile(entity: &Entity) -> Option<TilePos> {
    if entity.entity_type.as_deref() != Some("inserter") {
        return None;
    }

    let pickup_distance = match entity.name.as_str() {
        "long-handed-inserter" => 2,
        "inserter" | "burner-inserter" | "fast-inserter" | "bulk-inserter" | "stack-inserter" => 1,
        _ => return None,
    };

    if !matches!(entity.direction, 0 | 4 | 8 | 12) {
        return None;
    }

    let direction = Direction::from_factorio(entity.direction);
    Some(
        entity
            .position
            .to_tile()
            .offset_in_direction_by(direction, pickup_distance),
    )
}

fn is_intended_inserter_terminal(entity: &Entity, belt_tile: TilePos) -> bool {
    modeled_inserter_pickup_tile(entity) == Some(belt_tile)
}

/// Analyze belt network for gaps (missing, misaligned, or blocked connections)
pub fn find_belt_gaps(graph: &BeltGraph, all_entities: &[Entity]) -> BeltGapResult {
    let entity_at = build_entity_occupancy_lookup(all_entities);

    let mut gaps = Vec::new();

    for (pos, node) in graph.iter() {
        let output_pos = node.output_tile();

        // Check if this belt has no downstream connection
        if graph.downstream_of(pos).is_empty() {
            // There's a potential gap - what's at the output position?
            if let Some(target_belt) = graph.get(&output_pos) {
                // There IS a belt there, but no connection - must be misaligned
                gaps.push(BeltGap {
                    from: *pos,
                    to: output_pos,
                    from_direction: node.direction,
                    gap_type: GapType::Misaligned,
                    blocker: Some(format!(
                        "{} facing {:?}",
                        target_belt.belt_type, target_belt.direction
                    )),
                });
            } else if graph.unsupported_at(&output_pos).is_some() {
                // Unsupported transport is neither a proven gap nor a proven
                // connection. analysis_scope carries the fail-closed evidence.
            } else if let Some(blocker) = entity_at.get(&output_pos) {
                if is_intended_inserter_terminal(blocker, *pos) {
                    continue;
                }
                // Non-belt entity blocking the path
                gaps.push(BeltGap {
                    from: *pos,
                    to: output_pos,
                    from_direction: node.direction,
                    gap_type: GapType::Blocked,
                    blocker: Some(blocker.name.clone()),
                });
            } else if graph
                .can_receive_from(&output_pos.offset_in_direction(node.direction), &output_pos)
            {
                // Only call an empty tile a gap when a compatible belt resumes
                // immediately after it. An empty terminal is a legitimate belt
                // endpoint, not evidence of a broken line.
                gaps.push(BeltGap {
                    from: *pos,
                    to: output_pos,
                    from_direction: node.direction,
                    gap_type: GapType::Missing,
                    blocker: None,
                });
            }
        }
    }

    let certified_gap_free = gaps.is_empty() && graph.analysis_scope().connectivity_model_complete;
    BeltGapResult {
        analysis_scope: graph.analysis_scope().clone(),
        certified_gap_free,
        gap_count: gaps.len() as u32,
        gaps,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{Direction, Position};

    fn make_belt(x: i32, y: i32, dir: Direction) -> Entity {
        Entity {
            unit_number: Some((x * 100 + y) as u32),
            name: "transport-belt".to_string(),
            entity_type: Some("transport-belt".to_string()),
            position: Position::new(x as f64 + 0.5, y as f64 + 0.5),
            direction: dir.to_factorio(),
            health: Some(100.0),
            force: Some("player".to_string()),
            bounding_box: None,
            pickup_position: None,
            drop_position: None,
        }
    }

    fn make_entity(x: i32, y: i32, name: &str) -> Entity {
        Entity {
            unit_number: Some((x * 1000 + y) as u32),
            name: name.to_string(),
            entity_type: Some(name.to_string()),
            position: Position::new(x as f64 + 0.5, y as f64 + 0.5),
            direction: 0,
            health: Some(100.0),
            force: Some("player".to_string()),
            bounding_box: None,
            pickup_position: None,
            drop_position: None,
        }
    }

    #[test]
    fn test_no_gaps() {
        let entities = vec![
            make_belt(0, 0, Direction::East),
            make_belt(1, 0, Direction::East),
            make_belt(2, 0, Direction::East),
        ];

        let graph = BeltGraph::from_entities(&entities);
        let result = find_belt_gaps(&graph, &entities);

        assert_eq!(result.gap_count, 0);
        assert!(result.certified_gap_free);
    }

    #[test]
    fn legitimate_terminal_endpoint_is_not_a_gap() {
        let entities = vec![make_belt(0, 0, Direction::East)];
        let graph = BeltGraph::from_entities(&entities);
        let result = find_belt_gaps(&graph, &entities);

        assert!(result.gaps.is_empty());
        assert!(result.certified_gap_free);
    }

    #[test]
    fn unsupported_transport_prevents_gap_free_certification() {
        let mut underground = make_belt(1, 0, Direction::East);
        underground.name = "underground-belt".to_string();
        underground.entity_type = Some("underground-belt".to_string());
        let entities = vec![make_belt(0, 0, Direction::East), underground];
        let graph = BeltGraph::from_entities(&entities);
        let result = find_belt_gaps(&graph, &entities);

        assert!(result.gaps.is_empty());
        assert!(!result.certified_gap_free);
        assert!(!result.analysis_scope.connectivity_model_complete);
        assert_eq!(result.analysis_scope.unsupported_transports.len(), 1);
    }

    #[test]
    fn test_missing_gap() {
        let entities = vec![
            make_belt(0, 0, Direction::East),
            // Gap at (1, 0)
            make_belt(2, 0, Direction::East),
        ];

        let graph = BeltGraph::from_entities(&entities);
        let result = find_belt_gaps(&graph, &entities);

        assert_eq!(result.gap_count, 1);

        let gap_0 = result
            .gaps
            .iter()
            .find(|g| g.from == TilePos::new(0, 0))
            .unwrap();
        assert_eq!(gap_0.gap_type, GapType::Missing);
        assert_eq!(gap_0.to, TilePos::new(1, 0));
    }

    #[test]
    fn test_misaligned_gap() {
        let entities = vec![
            make_belt(0, 0, Direction::East),
            make_belt(1, 0, Direction::West), // Facing wrong way!
        ];

        let graph = BeltGraph::from_entities(&entities);
        let result = find_belt_gaps(&graph, &entities);

        // Belt at (0,0) outputs to (1,0) but belt there faces wrong way
        let gap = result
            .gaps
            .iter()
            .find(|g| g.from == TilePos::new(0, 0))
            .unwrap();
        assert_eq!(gap.gap_type, GapType::Misaligned);
    }

    #[test]
    fn test_blocked_gap() {
        let entities = vec![
            make_belt(0, 0, Direction::East),
            make_entity(1, 0, "stone-furnace"),
        ];

        let graph = BeltGraph::from_entities(&entities);
        let result = find_belt_gaps(&graph, &entities);

        let gap = result
            .gaps
            .iter()
            .find(|g| g.from == TilePos::new(0, 0))
            .unwrap();
        assert_eq!(gap.gap_type, GapType::Blocked);
        assert_eq!(gap.blocker, Some("stone-furnace".to_string()));
    }

    #[test]
    fn inserter_picking_from_terminal_belt_is_not_a_blocked_gap() {
        let mut inserter = make_entity(1, 0, "inserter");
        inserter.entity_type = Some("inserter".to_string());
        inserter.direction = Direction::West.to_factorio();
        let entities = vec![make_belt(0, 0, Direction::East), inserter];

        let graph = BeltGraph::from_entities(&entities);
        let result = find_belt_gaps(&graph, &entities);

        assert!(result.gaps.is_empty());
        assert!(result.certified_gap_free);
    }

    #[test]
    fn wrong_facing_inserter_still_blocks_the_terminal_belt() {
        let mut inserter = make_entity(1, 0, "inserter");
        inserter.entity_type = Some("inserter".to_string());
        inserter.direction = Direction::East.to_factorio();
        let entities = vec![make_belt(0, 0, Direction::East), inserter];

        let graph = BeltGraph::from_entities(&entities);
        let result = find_belt_gaps(&graph, &entities);

        assert_eq!(result.gap_count, 1);
        assert_eq!(result.gaps[0].gap_type, GapType::Blocked);
        assert_eq!(result.gaps[0].blocker.as_deref(), Some("inserter"));
    }

    #[test]
    fn unknown_modded_inserter_still_blocks_the_terminal_belt() {
        let mut inserter = make_entity(1, 0, "super-inserter");
        inserter.entity_type = Some("inserter".to_string());
        inserter.direction = Direction::West.to_factorio();
        let entities = vec![make_belt(0, 0, Direction::East), inserter];

        let graph = BeltGraph::from_entities(&entities);
        let result = find_belt_gaps(&graph, &entities);

        assert_eq!(result.gap_count, 1);
        assert_eq!(result.gaps[0].gap_type, GapType::Blocked);
        assert_eq!(result.gaps[0].blocker.as_deref(), Some("super-inserter"));
    }

    #[test]
    fn malformed_inserter_direction_still_blocks_the_terminal_belt() {
        let mut inserter = make_entity(1, 0, "inserter");
        inserter.entity_type = Some("inserter".to_string());
        inserter.direction = 1;
        let entities = vec![make_belt(0, 0, Direction::East), inserter];

        let graph = BeltGraph::from_entities(&entities);
        let result = find_belt_gaps(&graph, &entities);

        assert_eq!(result.gap_count, 1);
        assert_eq!(result.gaps[0].gap_type, GapType::Blocked);
        assert_eq!(result.gaps[0].blocker.as_deref(), Some("inserter"));
    }

    #[test]
    fn terminal_belt_into_edge_of_three_by_three_entity_is_blocked() {
        for blocker_name in ["assembling-machine-1", "electric-mining-drill"] {
            // The 3x3 entity is centered on tile (2, 0), so its west edge
            // occupies (1, 0), directly in front of the east-facing belt.
            let entities = vec![
                make_belt(0, 0, Direction::East),
                make_entity(2, 0, blocker_name),
            ];

            let graph = BeltGraph::from_entities(&entities);
            let result = find_belt_gaps(&graph, &entities);

            assert_eq!(result.gap_count, 1, "blocker={blocker_name}");
            assert!(!result.certified_gap_free, "blocker={blocker_name}");
            let gap = &result.gaps[0];
            assert_eq!(gap.from, TilePos::new(0, 0));
            assert_eq!(gap.to, TilePos::new(1, 0));
            assert_eq!(gap.gap_type, GapType::Blocked);
            assert_eq!(gap.blocker.as_deref(), Some(blocker_name));
        }
    }
}
