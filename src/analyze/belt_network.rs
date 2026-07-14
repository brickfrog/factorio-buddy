//! Connected component analysis for belt networks

use super::{BeltGraph, BeltNetwork, BeltNetworkResult};
use crate::world::TilePos;
use std::collections::{HashSet, VecDeque};

/// Find all connected belt networks in the graph
pub fn find_belt_networks(graph: &BeltGraph) -> BeltNetworkResult {
    let mut visited = HashSet::new();
    let mut networks = Vec::new();
    let mut network_id = 0u32;

    for &start_pos in graph.all_positions() {
        if visited.contains(&start_pos) {
            continue;
        }

        // BFS to find all connected belts (bidirectional traversal)
        let mut network_belts = Vec::new();
        let mut queue = VecDeque::new();
        queue.push_back(start_pos);
        visited.insert(start_pos);

        while let Some(pos) = queue.pop_front() {
            network_belts.push(pos);

            // Traverse both upstream and downstream connections
            for &neighbor in graph.upstream_of(&pos) {
                if !visited.contains(&neighbor) {
                    visited.insert(neighbor);
                    queue.push_back(neighbor);
                }
            }
            for &neighbor in graph.downstream_of(&pos) {
                if !visited.contains(&neighbor) {
                    visited.insert(neighbor);
                    queue.push_back(neighbor);
                }
            }
        }

        // Identify inputs (no upstream) and outputs (no downstream)
        let inputs: Vec<TilePos> = network_belts
            .iter()
            .filter(|pos| graph.upstream_of(pos).is_empty())
            .copied()
            .collect();

        let outputs: Vec<TilePos> = network_belts
            .iter()
            .filter(|pos| graph.downstream_of(pos).is_empty())
            .copied()
            .collect();

        let belt_count = network_belts.len() as u32;

        networks.push(BeltNetwork {
            id: network_id,
            belts: network_belts,
            inputs,
            outputs,
            belt_count,
        });

        network_id += 1;
    }

    let total_belts = networks.iter().map(|n| n.belt_count).sum();

    BeltNetworkResult {
        analysis_scope: graph.analysis_scope().clone(),
        total_networks: networks.len() as u32,
        total_belts,
        networks,
    }
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
        }
    }

    #[test]
    fn test_single_network() {
        let entities = vec![
            make_belt(0, 0, Direction::East),
            make_belt(1, 0, Direction::East),
            make_belt(2, 0, Direction::East),
        ];

        let graph = BeltGraph::from_entities(&entities);
        let result = find_belt_networks(&graph);

        assert_eq!(result.total_networks, 1);
        assert_eq!(result.total_belts, 3);
        assert_eq!(result.networks[0].belt_count, 3);
        assert_eq!(result.networks[0].inputs.len(), 1);
        assert_eq!(result.networks[0].outputs.len(), 1);
    }

    #[test]
    fn test_separate_networks() {
        // Two disconnected belt lines
        let entities = vec![
            make_belt(0, 0, Direction::East),
            make_belt(1, 0, Direction::East),
            // Gap at (2, 0)
            make_belt(10, 0, Direction::East),
            make_belt(11, 0, Direction::East),
        ];

        let graph = BeltGraph::from_entities(&entities);
        let result = find_belt_networks(&graph);

        assert_eq!(result.total_networks, 2);
        assert_eq!(result.total_belts, 4);
    }

    #[test]
    fn test_empty_graph() {
        let graph = BeltGraph::from_entities(&[]);
        let result = find_belt_networks(&graph);

        assert_eq!(result.total_networks, 0);
        assert_eq!(result.total_belts, 0);
    }

    #[test]
    fn network_totals_disclose_unsupported_transport_scope() {
        let mut splitter = make_belt(2, 0, Direction::East);
        splitter.name = "splitter".to_string();
        splitter.entity_type = Some("splitter".to_string());
        let entities = vec![
            make_belt(0, 0, Direction::East),
            make_belt(1, 0, Direction::East),
            splitter,
        ];
        let graph = BeltGraph::from_entities(&entities);
        let result = find_belt_networks(&graph);

        assert_eq!(result.total_networks, 1);
        assert_eq!(result.total_belts, 2);
        assert!(!result.analysis_scope.connectivity_model_complete);
        assert_eq!(result.analysis_scope.modeled_surface_belts, 2);
        assert_eq!(result.analysis_scope.unsupported_transports.len(), 1);
    }
}
