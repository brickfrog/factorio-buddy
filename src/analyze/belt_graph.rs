//! Belt graph data structure for connectivity analysis

use super::{BeltAnalysisScope, UnsupportedTransport, UnsupportedTransportReason};
use crate::world::{entity_occupied_tiles, Direction, Entity, TilePos};
use std::collections::HashMap;

/// A belt entity with connectivity information
#[derive(Debug, Clone)]
pub struct BeltNode {
    pub unit_number: Option<u32>,
    pub position: TilePos,
    pub direction: Direction,
    pub belt_type: String,
}

impl BeltNode {
    /// Get the tile this belt outputs to (downstream)
    pub fn output_tile(&self) -> TilePos {
        self.position.offset_in_direction(self.direction)
    }

    /// Get the tile this belt primarily receives from (upstream, opposite direction)
    pub fn primary_input_tile(&self) -> TilePos {
        self.position.offset_in_direction(self.direction.opposite())
    }

    /// Get side-loading input tiles (perpendicular to belt direction)
    pub fn side_input_tiles(&self) -> [TilePos; 2] {
        [
            self.position
                .offset_in_direction(self.direction.rotate_ccw()),
            self.position
                .offset_in_direction(self.direction.rotate_cw()),
        ]
    }
}

/// Belt graph for connectivity analysis
pub struct BeltGraph {
    /// All belts indexed by position
    nodes: HashMap<TilePos, BeltNode>,
    /// Forward edges: position -> downstream positions that receive from this belt
    downstream: HashMap<TilePos, Vec<TilePos>>,
    /// Reverse edges: position -> upstream positions that feed this belt
    upstream: HashMap<TilePos, Vec<TilePos>>,
    /// Explicit record of exact and unsupported transport entities.
    analysis_scope: BeltAnalysisScope,
}

impl BeltGraph {
    /// Build belt graph from a list of entities
    pub fn from_entities(entities: &[Entity]) -> Self {
        let mut nodes = HashMap::new();
        let mut unsupported_transports = Vec::new();

        // First pass: collect all belt nodes
        for entity in entities {
            if !is_surface_transport_belt(entity) {
                if let Some(unsupported) = unsupported_transport(entity) {
                    unsupported_transports.push(unsupported);
                }
                continue;
            }

            let position = entity.position.to_tile();
            let direction = Direction::from_factorio(entity.direction);

            nodes.insert(
                position,
                BeltNode {
                    unit_number: entity.unit_number,
                    position,
                    direction,
                    belt_type: entity.name.clone(),
                },
            );
        }

        // Second pass: build edges
        let mut downstream: HashMap<TilePos, Vec<TilePos>> = HashMap::new();
        let mut upstream: HashMap<TilePos, Vec<TilePos>> = HashMap::new();

        for (pos, node) in &nodes {
            let output = node.output_tile();

            // Check if there's a belt at the output position
            if let Some(target) = nodes.get(&output) {
                // The target belt must be able to receive from this direction
                // A belt can receive from: behind (primary) or sides (side-loading)
                let can_receive = {
                    let target_input = target.primary_input_tile();
                    let [side_left, side_right] = target.side_input_tiles();
                    *pos == target_input || *pos == side_left || *pos == side_right
                };

                if can_receive {
                    downstream.entry(*pos).or_default().push(output);
                    upstream.entry(output).or_default().push(*pos);
                }
            }
        }

        unsupported_transports.sort_by(|left, right| {
            (
                left.position.x,
                left.position.y,
                &left.name,
                left.unit_number,
            )
                .cmp(&(
                    right.position.x,
                    right.position.y,
                    &right.name,
                    right.unit_number,
                ))
        });
        let analysis_scope = BeltAnalysisScope {
            connectivity_model_complete: unsupported_transports.is_empty(),
            modeled_surface_belts: nodes.len() as u32,
            unsupported_transports,
        };

        Self {
            nodes,
            downstream,
            upstream,
            analysis_scope,
        }
    }

    /// Get belt at position
    pub fn get(&self, pos: &TilePos) -> Option<&BeltNode> {
        self.nodes.get(pos)
    }

    /// Check if position contains a belt
    pub fn contains(&self, pos: &TilePos) -> bool {
        self.nodes.contains_key(pos)
    }

    /// Get downstream neighbors (where items flow to)
    pub fn downstream_of(&self, pos: &TilePos) -> &[TilePos] {
        self.downstream
            .get(pos)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Get upstream neighbors (where items come from)
    pub fn upstream_of(&self, pos: &TilePos) -> &[TilePos] {
        self.upstream.get(pos).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Whether a belt at `target` can receive an item from `source`.
    pub fn can_receive_from(&self, target: &TilePos, source: &TilePos) -> bool {
        self.nodes.get(target).is_some_and(|target| {
            let [side_left, side_right] = target.side_input_tiles();
            *source == target.primary_input_tile() || *source == side_left || *source == side_right
        })
    }

    /// Get all belt positions in the graph
    pub fn all_positions(&self) -> impl Iterator<Item = &TilePos> {
        self.nodes.keys()
    }

    /// Get all belt nodes
    pub fn iter(&self) -> impl Iterator<Item = (&TilePos, &BeltNode)> {
        self.nodes.iter()
    }

    /// Number of belts in the graph
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Check if graph is empty
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Completeness evidence for analyses derived from this graph.
    pub fn analysis_scope(&self) -> &BeltAnalysisScope {
        &self.analysis_scope
    }

    /// Unsupported transport occupying a given tile, if any.
    pub fn unsupported_at(&self, pos: &TilePos) -> Option<&UnsupportedTransport> {
        self.analysis_scope
            .unsupported_transports
            .iter()
            .find(|transport| transport.occupied_tiles.contains(pos))
    }
}

/// Whether an entity can be modeled exactly by the one-tile belt graph.
///
/// Splitters and underground belts require additional runtime state (lanes,
/// input/output mode, and underground pairing) which `Entity` does not carry.
/// Excluding them prevents the static analyzer from claiming connectivity it
/// cannot prove.
pub fn is_surface_transport_belt(entity: &Entity) -> bool {
    entity.entity_type.as_deref() == Some("transport-belt")
        && matches!(
            entity.name.as_str(),
            "transport-belt"
                | "fast-transport-belt"
                | "express-transport-belt"
                | "turbo-transport-belt"
        )
}

/// Whether an entity participates in item transport but is either modeled or
/// deliberately reported as unsupported by the static belt graph.
pub fn is_transport_entity(entity: &Entity) -> bool {
    is_surface_transport_belt(entity) || unsupported_transport(entity).is_some()
}

fn unsupported_transport(entity: &Entity) -> Option<UnsupportedTransport> {
    let entity_type = entity.entity_type.as_deref().unwrap_or("unknown");
    let reason = match entity_type {
        "splitter" => UnsupportedTransportReason::SplitterSemanticsNotModeled,
        "underground-belt" => UnsupportedTransportReason::UndergroundPairingNotModeled,
        "loader" | "loader-1x1" => UnsupportedTransportReason::LoaderSemanticsNotModeled,
        "linked-belt" => UnsupportedTransportReason::LinkedBeltPairingNotModeled,
        "transport-belt" => UnsupportedTransportReason::UnsupportedTransportPrototype,
        _ if transport_like_name(&entity.name) => {
            UnsupportedTransportReason::UnsupportedTransportPrototype
        }
        _ => return None,
    };

    Some(UnsupportedTransport {
        unit_number: entity.unit_number,
        name: entity.name.clone(),
        entity_type: entity_type.to_string(),
        position: entity.position.to_tile(),
        occupied_tiles: entity_occupied_tiles(entity),
        reason,
    })
}

fn transport_like_name(name: &str) -> bool {
    name.ends_with("transport-belt")
        || name.ends_with("underground-belt")
        || name.ends_with("splitter")
        || name.ends_with("loader")
        || name.ends_with("linked-belt")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::world::Position;

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

    #[test]
    fn test_straight_line() {
        // Three belts in a row going east: (0,0) -> (1,0) -> (2,0)
        let entities = vec![
            make_belt(0, 0, Direction::East),
            make_belt(1, 0, Direction::East),
            make_belt(2, 0, Direction::East),
        ];

        let graph = BeltGraph::from_entities(&entities);

        assert_eq!(graph.len(), 3);

        // Check downstream connections
        let p0 = TilePos::new(0, 0);
        let p1 = TilePos::new(1, 0);
        let p2 = TilePos::new(2, 0);

        assert_eq!(graph.downstream_of(&p0), &[p1]);
        assert_eq!(graph.downstream_of(&p1), &[p2]);
        assert!(graph.downstream_of(&p2).is_empty());

        // Check upstream connections
        assert!(graph.upstream_of(&p0).is_empty());
        assert_eq!(graph.upstream_of(&p1), &[p0]);
        assert_eq!(graph.upstream_of(&p2), &[p1]);
    }

    #[test]
    fn test_side_loading() {
        // Belt going east at (1,0), with side-loader from south at (1,1)
        let entities = vec![
            make_belt(1, 0, Direction::East),
            make_belt(1, 1, Direction::North), // Side-loading from south
        ];

        let graph = BeltGraph::from_entities(&entities);

        let main = TilePos::new(1, 0);
        let side = TilePos::new(1, 1);

        // Side belt should connect to main belt
        assert_eq!(graph.downstream_of(&side), &[main]);
        assert_eq!(graph.upstream_of(&main), &[side]);
    }

    #[test]
    fn unsupported_belt_kinds_are_not_certified_as_surface_connections() {
        let mut underground = make_belt(1, 0, Direction::East);
        underground.name = "underground-belt".to_string();
        underground.entity_type = Some("underground-belt".to_string());
        let mut splitter = make_belt(2, 0, Direction::East);
        splitter.name = "splitter".to_string();
        splitter.entity_type = Some("splitter".to_string());

        let graph =
            BeltGraph::from_entities(&[make_belt(0, 0, Direction::East), underground, splitter]);

        assert_eq!(graph.len(), 1);
        assert!(graph.downstream_of(&TilePos::new(0, 0)).is_empty());
        assert!(!graph.analysis_scope().connectivity_model_complete);
        assert_eq!(graph.analysis_scope().modeled_surface_belts, 1);
        assert_eq!(graph.analysis_scope().unsupported_transports.len(), 2);
        assert_eq!(
            graph.analysis_scope().unsupported_transports[0].reason,
            UnsupportedTransportReason::UndergroundPairingNotModeled
        );
        assert_eq!(
            graph.analysis_scope().unsupported_transports[1].reason,
            UnsupportedTransportReason::SplitterSemanticsNotModeled
        );
    }

    #[test]
    fn all_surface_transport_belt_tiers_are_modeled() {
        let entities = [
            "transport-belt",
            "fast-transport-belt",
            "express-transport-belt",
            "turbo-transport-belt",
        ]
        .into_iter()
        .enumerate()
        .map(|(x, name)| {
            let mut belt = make_belt(x as i32, 0, Direction::East);
            belt.name = name.to_string();
            belt
        })
        .collect::<Vec<_>>();
        let graph = BeltGraph::from_entities(&entities);

        assert_eq!(graph.len(), 4);
        assert!(graph.analysis_scope().connectivity_model_complete);
        assert!(graph.analysis_scope().unsupported_transports.is_empty());
        assert_eq!(graph.analysis_scope().modeled_surface_belts, 4);
        assert_eq!(
            graph.downstream_of(&TilePos::new(0, 0)),
            &[TilePos::new(1, 0)]
        );
        assert_eq!(
            graph.downstream_of(&TilePos::new(1, 0)),
            &[TilePos::new(2, 0)]
        );
        assert_eq!(
            graph.downstream_of(&TilePos::new(2, 0)),
            &[TilePos::new(3, 0)]
        );
    }

    #[test]
    fn unknown_transport_belt_prototype_is_reported_not_silently_ignored() {
        let mut belt = make_belt(0, 0, Direction::East);
        belt.name = "modded-ultra-transport-belt".to_string();
        let graph = BeltGraph::from_entities(&[belt]);

        assert!(graph.is_empty());
        assert!(!graph.analysis_scope().connectivity_model_complete);
        assert_eq!(graph.analysis_scope().unsupported_transports.len(), 1);
        assert_eq!(
            graph.analysis_scope().unsupported_transports[0].reason,
            UnsupportedTransportReason::UnsupportedTransportPrototype
        );
    }
}
