//! BFS-based belt reachability analysis

use super::{BeltGraph, BeltReachResult};
use crate::world::TilePos;
use std::collections::{HashSet, VecDeque};

/// Analyze belt connectivity from a starting position using BFS
pub fn analyze_belt_reach(graph: &BeltGraph, start: TilePos) -> Option<BeltReachResult> {
    // Check if start position has a belt
    if !graph.contains(&start) {
        return None;
    }

    // BFS upstream (find all belts feeding into this one)
    let upstream = bfs_traverse(graph, start, TraverseDirection::Upstream);

    // BFS downstream (find all belts this one feeds into)
    let downstream = bfs_traverse(graph, start, TraverseDirection::Downstream);

    // Find endpoints
    let upstream_endpoints: Vec<TilePos> = upstream
        .iter()
        .filter(|pos| graph.upstream_of(pos).is_empty())
        .copied()
        .collect();

    let downstream_endpoints: Vec<TilePos> = downstream
        .iter()
        .filter(|pos| graph.downstream_of(pos).is_empty())
        .copied()
        .collect();

    // Total belts = upstream + downstream + origin (avoid double counting)
    let total_belts = (upstream.len() + downstream.len() + 1) as u32;

    Some(BeltReachResult {
        analysis_scope: graph.analysis_scope().clone(),
        origin: start,
        upstream,
        downstream,
        upstream_endpoints,
        downstream_endpoints,
        total_belts,
    })
}

#[derive(Clone, Copy)]
enum TraverseDirection {
    Upstream,
    Downstream,
}

fn bfs_traverse(graph: &BeltGraph, start: TilePos, direction: TraverseDirection) -> Vec<TilePos> {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    let mut result = Vec::new();

    // Start with neighbors, not the start position itself
    let initial_neighbors = match direction {
        TraverseDirection::Upstream => graph.upstream_of(&start),
        TraverseDirection::Downstream => graph.downstream_of(&start),
    };

    for &neighbor in initial_neighbors {
        queue.push_back(neighbor);
        visited.insert(neighbor);
    }

    while let Some(pos) = queue.pop_front() {
        result.push(pos);

        let neighbors = match direction {
            TraverseDirection::Upstream => graph.upstream_of(&pos),
            TraverseDirection::Downstream => graph.downstream_of(&pos),
        };

        for &neighbor in neighbors {
            if !visited.contains(&neighbor) {
                visited.insert(neighbor);
                queue.push_back(neighbor);
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::{Direction, Entity, Position};

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
            belt_to_ground_type: None,
            underground_belt_neighbour: None,
            belt_input_neighbours: Vec::new(),
            belt_output_neighbours: Vec::new(),
            belt_neighbours_observed: false,
        }
    }

    #[test]
    fn test_linear_reach() {
        // Belt line: (0,0) -> (1,0) -> (2,0) -> (3,0)
        let entities = vec![
            make_belt(0, 0, Direction::East),
            make_belt(1, 0, Direction::East),
            make_belt(2, 0, Direction::East),
            make_belt(3, 0, Direction::East),
        ];

        let graph = BeltGraph::from_entities(&entities);

        // Analyze from middle belt (1,0)
        let result = analyze_belt_reach(&graph, TilePos::new(1, 0)).unwrap();

        assert_eq!(result.origin, TilePos::new(1, 0));
        assert_eq!(result.upstream.len(), 1); // (0,0)
        assert_eq!(result.downstream.len(), 2); // (2,0), (3,0)
        assert_eq!(result.upstream_endpoints, vec![TilePos::new(0, 0)]);
        assert_eq!(result.downstream_endpoints, vec![TilePos::new(3, 0)]);
        assert_eq!(result.total_belts, 4);
    }

    #[test]
    fn test_merge_reach() {
        // Two belts merging:
        //   (0,0) -> (1,0)
        //   (1,-1) |
        //          v
        //   (1,1) -> (1,0)
        let entities = vec![
            make_belt(0, 0, Direction::East),  // feeds into (1,0) from behind
            make_belt(1, 1, Direction::North), // side-loads into (1,0)
            make_belt(1, 0, Direction::East),  // main belt
        ];

        let graph = BeltGraph::from_entities(&entities);

        let result = analyze_belt_reach(&graph, TilePos::new(1, 0)).unwrap();

        assert_eq!(result.upstream.len(), 2);
        assert!(result.upstream.contains(&TilePos::new(0, 0)));
        assert!(result.upstream.contains(&TilePos::new(1, 1)));
        assert_eq!(result.upstream_endpoints.len(), 2);
    }

    #[test]
    fn test_no_belt_at_position() {
        let entities = vec![make_belt(0, 0, Direction::East)];
        let graph = BeltGraph::from_entities(&entities);

        // Try to analyze from position with no belt
        let result = analyze_belt_reach(&graph, TilePos::new(5, 5));
        assert!(result.is_none());
    }
}
