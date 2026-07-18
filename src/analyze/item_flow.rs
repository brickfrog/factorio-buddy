//! Item-flow diagnostics across belts and inserters.

use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};

use super::{
    analyze_inserters, build_entity_occupancy_lookup, BeltAnalysisScope, BeltGraph,
    BeltReachResult, GapType, InserterAnalysis, UnsupportedTransport,
};
use crate::world::{BeltLaneSummary, Direction, Entity, InventoryItem, TilePos};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemFlowEndpoint {
    pub unit_number: Option<u32>,
    pub name: String,
    pub position: TilePos,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub belt_tile: Option<TilePos>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemFlowRepair {
    pub tool: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit_number: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entity_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemFlowBreak {
    pub from: TilePos,
    pub to: TilePos,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blocker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair: Option<ItemFlowRepair>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemFlowBeltItems {
    pub position: TilePos,
    pub unit_number: u32,
    pub total_items: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<InventoryItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemFlowReport {
    pub status: String,
    pub connected: bool,
    /// True only when connectivity was proven without unsupported transport semantics.
    pub connectivity_certified: bool,
    /// Exact-model coverage for all transport entities in the analyzed input.
    pub analysis_scope: BeltAnalysisScope,
    pub source: ItemFlowEndpoint,
    pub target: ItemFlowEndpoint,
    pub source_belt_tile: Option<TilePos>,
    pub target_belt_tile: Option<TilePos>,
    pub reachable_belts: Vec<TilePos>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items_on_path: Vec<ItemFlowBeltItems>,
    pub target_receives_item: bool,
    pub first_break: Option<ItemFlowBreak>,
    pub repair: Option<ItemFlowRepair>,
    pub guidance: String,
}

pub fn analyze_item_flow(
    entities: &[Entity],
    belt_contents: &[BeltLaneSummary],
    source_ref: EntityLookup,
    target_ref: EntityLookup,
) -> ItemFlowReport {
    let graph = BeltGraph::from_entities(entities);
    let entity_at = build_entity_occupancy_lookup(entities);
    let inserters = analyze_inserters(entities);
    let source = resolve_endpoint(
        entities,
        &entity_at,
        &graph,
        &inserters,
        source_ref,
        EndpointRole::Source,
    );
    let target = resolve_endpoint(
        entities,
        &entity_at,
        &graph,
        &inserters,
        target_ref,
        EndpointRole::Target,
    );

    let mut report = ItemFlowReport {
        status: "blocked".to_string(),
        connected: false,
        connectivity_certified: false,
        analysis_scope: graph.analysis_scope().clone(),
        source: source.endpoint,
        target: target.endpoint,
        source_belt_tile: source.belt_tile,
        target_belt_tile: target.belt_tile,
        reachable_belts: Vec::new(),
        items_on_path: Vec::new(),
        target_receives_item: false,
        first_break: None,
        repair: None,
        guidance: "Fix repair first, then rerun analyze_item_flow and verify_production."
            .to_string(),
    };

    if let Some(unsupported) = source.unsupported_transport {
        mark_unsupported_endpoint(&mut report, unsupported, "source");
        return report;
    }
    if let Some(unsupported) = target.unsupported_transport {
        mark_unsupported_endpoint(&mut report, unsupported, "target");
        return report;
    }

    let Some(source_belt) = source.belt_tile else {
        let repair = ItemFlowRepair {
            tool: "route_belt".to_string(),
            reason: "no_source_belt".to_string(),
            args: None,
            unit_number: report.source.unit_number,
            entity_name: Some("transport-belt".to_string()),
            x: None,
            y: None,
            direction: None,
            description: "No belt or inserter dropoff was found at the source. Route a belt from the source output first.".to_string(),
        };
        report.first_break = Some(ItemFlowBreak {
            from: report.source.position,
            to: report.source.position,
            reason: "no_source_belt".to_string(),
            blocker: None,
            repair: Some(repair.clone()),
        });
        report.repair = Some(repair);
        return report;
    };
    let Some(target_belt) = target.belt_tile else {
        let repair = ItemFlowRepair {
            tool: "route_belt".to_string(),
            reason: "no_target_belt_or_inserter_pickup".to_string(),
            args: None,
            unit_number: report.target.unit_number,
            entity_name: Some("transport-belt".to_string()),
            x: None,
            y: None,
            direction: None,
            description: "No belt feeding the target was found. Add an inserter that picks from a belt and drops into the target, or route the belt to an existing input inserter.".to_string(),
        };
        report.first_break = Some(ItemFlowBreak {
            from: source_belt,
            to: report.target.position,
            reason: "no_target_belt_or_inserter_pickup".to_string(),
            blocker: None,
            repair: Some(repair.clone()),
        });
        report.repair = Some(repair);
        return report;
    };

    let reach = downstream_reach(&graph, source_belt);
    report.reachable_belts = reach.downstream.clone();
    report.reachable_belts.insert(0, source_belt);
    report.items_on_path = belt_items_on_path(belt_contents, &report.reachable_belts);
    report.target_receives_item = belt_has_items(belt_contents, target_belt);

    if source_belt == target_belt || reach.downstream.contains(&target_belt) {
        report.status = "connected".to_string();
        report.connected = true;
        report.connectivity_certified = true;
        report.guidance = "Source belt reaches target belt. If production is still blocked, inspect inserter power/filtering and target inventory.".to_string();
        return report;
    }

    let first_break =
        first_reachable_break(&graph, &entity_at, &report.reachable_belts, target_belt);
    if first_break
        .as_ref()
        .is_some_and(|breakage| breakage.reason == "unsupported_transport")
    {
        report.status = "unsupported_transport".to_string();
        report.guidance = "Static analysis stopped at an unsupported splitter, underground belt, loader, or linked belt. Inspect live endpoint/pairing state; no connectivity claim or automatic repair is safe from this model.".to_string();
    }
    report.repair = first_break.as_ref().and_then(|b| b.repair.clone());
    report.first_break = first_break;
    report
}

#[derive(Debug, Clone, Copy)]
pub enum EntityLookup {
    Unit(u32),
    Tile(TilePos),
}

enum EndpointRole {
    Source,
    Target,
}

struct ResolvedEndpoint {
    endpoint: ItemFlowEndpoint,
    belt_tile: Option<TilePos>,
    unsupported_transport: Option<UnsupportedTransport>,
}

fn resolve_endpoint(
    entities: &[Entity],
    entity_at: &std::collections::HashMap<TilePos, &Entity>,
    graph: &BeltGraph,
    inserters: &[InserterAnalysis],
    lookup: EntityLookup,
    role: EndpointRole,
) -> ResolvedEndpoint {
    let entity = match lookup {
        EntityLookup::Unit(unit) => entities.iter().find(|e| e.unit_number == Some(unit)),
        EntityLookup::Tile(tile) => entity_at.get(&tile).copied(),
    };
    let fallback_tile = match lookup {
        EntityLookup::Unit(_) => entity.map(|e| e.position.to_tile()).unwrap_or_default(),
        EntityLookup::Tile(tile) => tile,
    };
    let endpoint = entity
        .map(|e| ItemFlowEndpoint {
            unit_number: e.unit_number,
            name: e.name.clone(),
            position: e.position.to_tile(),
            belt_tile: None,
        })
        .unwrap_or_else(|| ItemFlowEndpoint {
            unit_number: None,
            name: "tile".to_string(),
            position: fallback_tile,
            belt_tile: None,
        });

    let direct_belt = graph
        .contains(&endpoint.position)
        .then_some(endpoint.position);
    let belt_tile = match role {
        EndpointRole::Source => {
            direct_belt.or_else(|| source_belt_from_inserter(&endpoint, inserters))
        }
        EndpointRole::Target => {
            direct_belt.or_else(|| target_belt_from_inserter(&endpoint, inserters))
        }
    };

    ResolvedEndpoint {
        endpoint: ItemFlowEndpoint {
            belt_tile,
            ..endpoint
        },
        belt_tile,
        unsupported_transport: graph.unsupported_at(&endpoint.position).cloned(),
    }
}

fn mark_unsupported_endpoint(
    report: &mut ItemFlowReport,
    unsupported: UnsupportedTransport,
    endpoint_role: &str,
) {
    report.status = "unsupported_transport".to_string();
    report.guidance = format!(
        "Cannot certify {endpoint_role} connectivity through {}: {:?}. Inspect the live transport topology instead of treating omitted entities as connected.",
        unsupported.name, unsupported.reason
    );
    report.first_break = Some(ItemFlowBreak {
        from: unsupported.position,
        to: unsupported.position,
        reason: "unsupported_transport".to_string(),
        blocker: Some(unsupported.name),
        repair: None,
    });
    report.repair = None;
}

fn source_belt_from_inserter(
    endpoint: &ItemFlowEndpoint,
    inserters: &[InserterAnalysis],
) -> Option<TilePos> {
    inserters
        .iter()
        .find(|i| {
            i.pickup_target.as_ref().is_some_and(|target| {
                same_endpoint(target.unit_number, target.position.to_tile(), endpoint)
            }) && i
                .dropoff_target
                .as_ref()
                .is_some_and(|target| target.name.contains("belt"))
        })
        .map(|i| i.dropoff_position.to_tile())
}

fn target_belt_from_inserter(
    endpoint: &ItemFlowEndpoint,
    inserters: &[InserterAnalysis],
) -> Option<TilePos> {
    inserters
        .iter()
        .find(|i| {
            i.dropoff_target.as_ref().is_some_and(|target| {
                same_endpoint(target.unit_number, target.position.to_tile(), endpoint)
            }) && i
                .pickup_target
                .as_ref()
                .is_some_and(|target| target.name.contains("belt"))
        })
        .map(|i| i.pickup_position.to_tile())
}

fn same_endpoint(unit_number: Option<u32>, position: TilePos, endpoint: &ItemFlowEndpoint) -> bool {
    if endpoint.unit_number.is_some() && unit_number == endpoint.unit_number {
        return true;
    }
    position == endpoint.position
}

fn downstream_reach(graph: &BeltGraph, source_belt: TilePos) -> BeltReachResult {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    let mut downstream = Vec::new();

    for &neighbor in graph.downstream_of(&source_belt) {
        visited.insert(neighbor);
        queue.push_back(neighbor);
    }
    while let Some(pos) = queue.pop_front() {
        downstream.push(pos);
        for &neighbor in graph.downstream_of(&pos) {
            if visited.insert(neighbor) {
                queue.push_back(neighbor);
            }
        }
    }

    BeltReachResult {
        analysis_scope: graph.analysis_scope().clone(),
        origin: source_belt,
        upstream: Vec::new(),
        upstream_endpoints: Vec::new(),
        downstream_endpoints: downstream
            .iter()
            .copied()
            .filter(|pos| graph.downstream_of(pos).is_empty())
            .collect(),
        total_belts: downstream.len() as u32 + 1,
        downstream,
    }
}

fn belt_items_on_path(
    belt_contents: &[BeltLaneSummary],
    path: &[TilePos],
) -> Vec<ItemFlowBeltItems> {
    path.iter()
        .filter_map(|pos| {
            let belt = belt_contents.iter().find(|belt| belt.position == *pos)?;
            Some(ItemFlowBeltItems {
                position: *pos,
                unit_number: belt.unit_number,
                total_items: belt.left_lane.item_count + belt.right_lane.item_count,
                items: aggregate_belt_items(belt),
            })
        })
        .collect()
}

fn belt_has_items(belt_contents: &[BeltLaneSummary], pos: TilePos) -> bool {
    belt_contents.iter().any(|belt| {
        belt.position == pos && belt.left_lane.item_count + belt.right_lane.item_count > 0
    })
}

fn aggregate_belt_items(belt: &BeltLaneSummary) -> Vec<InventoryItem> {
    let mut items: Vec<InventoryItem> = Vec::new();
    for item in belt
        .left_lane
        .items
        .iter()
        .chain(belt.right_lane.items.iter())
    {
        if let Some(existing) = items.iter_mut().find(|existing| existing.name == item.name) {
            existing.count += item.count;
        } else {
            items.push(item.clone());
        }
    }
    items.sort_by(|a, b| a.name.cmp(&b.name));
    items
}

fn first_reachable_break(
    graph: &BeltGraph,
    entity_at: &std::collections::HashMap<TilePos, &Entity>,
    reachable: &[TilePos],
    target: TilePos,
) -> Option<ItemFlowBreak> {
    reachable
        .iter()
        .filter_map(|from| {
            let node = graph.get(from)?;
            if !graph.downstream_of(from).is_empty() {
                return None;
            }
            let to = node.output_tile();
            if let Some(unsupported) = graph.unsupported_at(&to) {
                return Some((
                    (to.x - target.x).abs() + (to.y - target.y).abs(),
                    ItemFlowBreak {
                        from: *from,
                        to,
                        reason: "unsupported_transport".to_string(),
                        blocker: Some(unsupported.name.clone()),
                        repair: None,
                    },
                ));
            }
            let wrong_direction_belt = graph.get(&to);
            let blocker = entity_at.get(&to).copied();
            let reason = if wrong_direction_belt.is_some() {
                "wrong_direction"
            } else if blocker.is_some() {
                "blocked"
            } else {
                "missing_belt"
            };
            let repair = match reason {
                "wrong_direction" => wrong_direction_belt.and_then(|belt| {
                    belt.unit_number.map(|unit| ItemFlowRepair {
                        tool: "rotate_entity".to_string(),
                        reason: reason.to_string(),
                        args: None,
                        unit_number: Some(unit),
                        entity_name: None,
                        x: None,
                        y: None,
                        direction: Some(node.direction.to_name().to_string()),
                        description: format!(
                            "Rotate belt unit {unit} at ({}, {}) to face {}.",
                            to.x,
                            to.y,
                            node.direction.to_name()
                        ),
                    })
                }),
                "blocked" => Some(ItemFlowRepair {
                    tool: "route_belt".to_string(),
                    reason: reason.to_string(),
                    args: Some(serde_json::json!({
                        "from_x": from.x,
                        "from_y": from.y,
                        "to_x": target.x,
                        "to_y": target.y,
                        "belt_type": node.belt_type,
                        "extend_existing": true,
                    })),
                    unit_number: None,
                    entity_name: Some("transport-belt".to_string()),
                    x: Some(to.x),
                    y: Some(to.y),
                    direction: Some(node.direction.to_name().to_string()),
                    description:
                        "Route around the blocking entity or remove it before extending the belt."
                            .to_string(),
                }),
                _ if graph.can_receive_from(
                    &to.offset_in_direction(node.direction),
                    &to,
                ) => Some(ItemFlowRepair {
                    tool: "route_belt".to_string(),
                    reason: "verified_one_tile_gap".to_string(),
                    args: Some(serde_json::json!({
                        "from_x": from.x,
                        "from_y": from.y,
                        "to_x": target.x,
                        "to_y": target.y,
                        "belt_type": node.belt_type,
                        "extend_existing": true,
                    })),
                    unit_number: None,
                    entity_name: Some(node.belt_type.clone()),
                    x: Some(to.x),
                    y: Some(to.y),
                    direction: Some(node.direction.to_name().to_string()),
                    description: format!(
                        "Repair the complete route through the verified one-tile gap at ({}, {}) facing {}.",
                        to.x,
                        to.y,
                        node.direction.to_name()
                    ),
                }),
                _ => Some(ItemFlowRepair {
                    tool: "route_belt".to_string(),
                    reason: "incomplete_route".to_string(),
                    args: Some(serde_json::json!({
                        "from_x": from.x,
                        "from_y": from.y,
                        "to_x": target.x,
                        "to_y": target.y,
                        "belt_type": node.belt_type,
                        "extend_existing": true,
                    })),
                    unit_number: None,
                    entity_name: Some(node.belt_type.clone()),
                    x: Some(target.x),
                    y: Some(target.y),
                    direction: None,
                    description: format!(
                        "Route one complete belt from ({}, {}) to ({}, {}); do not extend it one tile per turn.",
                        from.x, from.y, target.x, target.y
                    ),
                }),
            };
            let distance = (to.x - target.x).abs() + (to.y - target.y).abs();
            Some((
                distance,
                ItemFlowBreak {
                    from: *from,
                    to,
                    reason: reason.to_string(),
                    blocker: wrong_direction_belt
                        .map(|belt| belt.belt_type.clone())
                        .or_else(|| blocker.map(|entity| entity.name.clone())),
                    repair,
                },
            ))
        })
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, breakage)| breakage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::UnsupportedTransportReason;
    use crate::world::{Entity, LaneContents, Position};

    fn belt(x: i32, y: i32, direction: Direction) -> Entity {
        Entity {
            unit_number: Some((1000 + x * 10 + y) as u32),
            name: "transport-belt".to_string(),
            entity_type: Some("transport-belt".to_string()),
            position: Position::new(x as f64 + 0.5, y as f64 + 0.5),
            direction: direction.to_factorio(),
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

    fn unsupported_transport(
        x: i32,
        y: i32,
        direction: Direction,
        name: &str,
        entity_type: &str,
    ) -> Entity {
        typed_entity(x, y, direction, name, entity_type)
    }

    fn typed_entity(x: i32, y: i32, direction: Direction, name: &str, entity_type: &str) -> Entity {
        Entity {
            unit_number: Some((2000 + x * 10 + y) as u32),
            name: name.to_string(),
            entity_type: Some(entity_type.to_string()),
            position: Position::new(x as f64 + 0.5, y as f64 + 0.5),
            direction: direction.to_factorio(),
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

    fn live_belt(x: i32, outputs: &[(i32, i32)]) -> Entity {
        let mut entity = belt(x, 0, Direction::East);
        entity.belt_neighbours_observed = true;
        entity.belt_output_neighbours = outputs
            .iter()
            .map(|(x, y)| Position::new(*x as f64 + 0.5, *y as f64 + 0.5))
            .collect();
        entity
    }

    fn underground(x: i32, mode: &str, neighbour_x: i32, outputs: &[(i32, i32)]) -> Entity {
        let mut entity = live_belt(x, outputs);
        entity.name = "underground-belt".to_string();
        entity.entity_type = Some("underground-belt".to_string());
        entity.belt_to_ground_type = Some(mode.to_string());
        entity.underground_belt_neighbour = Some(Position::new(neighbour_x as f64 + 0.5, 0.5));
        entity
    }

    #[test]
    fn reports_connected_belt_flow() {
        let entities = vec![
            belt(0, 0, Direction::East),
            belt(1, 0, Direction::East),
            belt(2, 0, Direction::East),
        ];

        let report = analyze_item_flow(
            &entities,
            &[],
            EntityLookup::Tile(TilePos::new(0, 0)),
            EntityLookup::Tile(TilePos::new(2, 0)),
        );

        assert!(report.connected);
        assert!(report.connectivity_certified);
        assert_eq!(report.status, "connected");
        assert!(report.repair.is_none());
    }

    #[test]
    fn underground_network_fails_closed_with_explicit_scope_evidence() {
        let entities = vec![
            belt(0, 0, Direction::East),
            unsupported_transport(
                1,
                0,
                Direction::East,
                "underground-belt",
                "underground-belt",
            ),
            belt(4, 0, Direction::East),
        ];

        let report = analyze_item_flow(
            &entities,
            &[],
            EntityLookup::Tile(TilePos::new(0, 0)),
            EntityLookup::Tile(TilePos::new(4, 0)),
        );

        assert!(!report.connected);
        assert!(!report.connectivity_certified);
        assert_eq!(report.status, "unsupported_transport");
        assert_eq!(
            report.first_break.as_ref().map(|b| b.reason.as_str()),
            Some("unsupported_transport")
        );
        assert!(report.repair.is_none());
        assert!(!report.analysis_scope.connectivity_model_complete);
        assert_eq!(report.analysis_scope.unsupported_transports.len(), 1);
    }

    #[test]
    fn reports_connected_flow_through_authoritative_underground_pair() {
        let entities = vec![
            live_belt(0, &[(1, 0)]),
            underground(1, "input", 4, &[]),
            underground(4, "output", 1, &[(5, 0)]),
            live_belt(5, &[]),
        ];

        let report = analyze_item_flow(
            &entities,
            &[],
            EntityLookup::Tile(TilePos::new(0, 0)),
            EntityLookup::Tile(TilePos::new(5, 0)),
        );

        assert_eq!(report.status, "connected");
        assert!(report.connected);
        assert!(report.connectivity_certified);
        assert_eq!(report.analysis_scope.modeled_underground_belts, 2);
        assert!(report.analysis_scope.unsupported_transports.is_empty());
        assert_eq!(
            report.reachable_belts,
            vec![
                TilePos::new(0, 0),
                TilePos::new(1, 0),
                TilePos::new(4, 0),
                TilePos::new(5, 0),
            ]
        );
    }

    #[test]
    fn splitter_endpoint_fails_closed_instead_of_becoming_a_belt() {
        let splitter = unsupported_transport(1, 0, Direction::North, "splitter", "splitter");
        let entities = vec![belt(0, 0, Direction::East), splitter];

        let report = analyze_item_flow(
            &entities,
            &[],
            EntityLookup::Tile(TilePos::new(0, 0)),
            EntityLookup::Tile(TilePos::new(1, 0)),
        );

        assert!(!report.connected);
        assert!(!report.connectivity_certified);
        assert_eq!(report.status, "unsupported_transport");
        assert_eq!(
            report.analysis_scope.unsupported_transports[0].reason,
            UnsupportedTransportReason::SplitterSemanticsNotModeled
        );
    }

    #[test]
    fn reports_missing_belt_repair() {
        let entities = vec![belt(0, 0, Direction::East), belt(2, 0, Direction::East)];

        let report = analyze_item_flow(
            &entities,
            &[],
            EntityLookup::Tile(TilePos::new(0, 0)),
            EntityLookup::Tile(TilePos::new(2, 0)),
        );

        assert!(!report.connected);
        let repair = report.repair.expect("missing belt should have repair");
        assert_eq!(repair.tool, "route_belt");
        assert_eq!(repair.entity_name.as_deref(), Some("transport-belt"));
        assert_eq!(repair.x, Some(1));
        assert_eq!(repair.y, Some(0));
        assert_eq!(repair.direction.as_deref(), Some("east"));
        assert_eq!(repair.reason, "verified_one_tile_gap");
        assert_eq!(repair.args.as_ref().unwrap()["from_x"], 0);
        assert_eq!(repair.args.as_ref().unwrap()["to_x"], 2);
        assert_eq!(repair.args.as_ref().unwrap()["extend_existing"], true);
    }

    #[test]
    fn incomplete_distance_returns_one_complete_route_not_single_belt() {
        let entities = vec![belt(0, 0, Direction::East), belt(4, 0, Direction::East)];

        let report = analyze_item_flow(
            &entities,
            &[],
            EntityLookup::Tile(TilePos::new(0, 0)),
            EntityLookup::Tile(TilePos::new(4, 0)),
        );

        let repair = report.repair.expect("incomplete route should have repair");
        assert_eq!(repair.tool, "route_belt");
        assert_eq!(repair.reason, "incomplete_route");
        assert_eq!(repair.args.as_ref().unwrap()["from_x"], 0);
        assert_eq!(repair.args.as_ref().unwrap()["to_x"], 4);
        assert_eq!(repair.args.as_ref().unwrap()["extend_existing"], true);
    }

    #[test]
    fn reports_wrong_direction_repair() {
        let entities = vec![belt(0, 0, Direction::East), belt(1, 0, Direction::West)];

        let report = analyze_item_flow(
            &entities,
            &[],
            EntityLookup::Tile(TilePos::new(0, 0)),
            EntityLookup::Tile(TilePos::new(1, 0)),
        );

        assert!(!report.connected);
        let repair = report.repair.expect("wrong direction should have repair");
        assert_eq!(repair.tool, "rotate_entity");
        assert_eq!(repair.direction.as_deref(), Some("east"));
    }

    #[test]
    fn edge_of_three_by_three_entity_is_a_blocked_break() {
        let entities = vec![
            belt(0, 0, Direction::East),
            typed_entity(
                2,
                0,
                Direction::North,
                "assembling-machine-1",
                "assembling-machine",
            ),
            belt(5, 0, Direction::East),
        ];

        let report = analyze_item_flow(
            &entities,
            &[],
            EntityLookup::Tile(TilePos::new(0, 0)),
            EntityLookup::Tile(TilePos::new(5, 0)),
        );

        assert!(!report.connected);
        assert!(!report.connectivity_certified);
        let breakage = report.first_break.expect("3x3 edge must block the route");
        assert_eq!(breakage.to, TilePos::new(1, 0));
        assert_eq!(breakage.reason, "blocked");
        assert_eq!(breakage.blocker.as_deref(), Some("assembling-machine-1"));
        assert_eq!(
            breakage
                .repair
                .as_ref()
                .map(|repair| repair.reason.as_str()),
            Some("blocked")
        );
    }

    #[test]
    fn shared_resource_and_belt_tile_classifies_wrong_direction_deterministically() {
        let resource = typed_entity(1, 0, Direction::North, "iron-ore", "resource");
        let target_belt = belt(1, 0, Direction::West);

        for entities in [
            vec![
                belt(0, 0, Direction::East),
                resource.clone(),
                target_belt.clone(),
            ],
            vec![
                belt(0, 0, Direction::East),
                target_belt.clone(),
                resource.clone(),
            ],
        ] {
            let report = analyze_item_flow(
                &entities,
                &[],
                EntityLookup::Tile(TilePos::new(0, 0)),
                EntityLookup::Tile(TilePos::new(1, 0)),
            );

            assert!(!report.connected);
            assert_eq!(report.target.name, "transport-belt");
            let breakage = report
                .first_break
                .expect("wrong-facing belt must remain visible above ore");
            assert_eq!(breakage.reason, "wrong_direction");
            assert_eq!(breakage.blocker.as_deref(), Some("transport-belt"));
            assert_eq!(
                breakage.repair.as_ref().map(|repair| repair.tool.as_str()),
                Some("rotate_entity")
            );
        }
    }

    #[test]
    fn reports_current_items_on_reachable_belts() {
        let entities = vec![belt(0, 0, Direction::East), belt(1, 0, Direction::East)];
        let belt_contents = vec![BeltLaneSummary {
            position: TilePos::new(1, 0),
            unit_number: 1010,
            direction: Direction::East.to_factorio(),
            belt_type: "transport-belt".to_string(),
            left_lane: LaneContents {
                lane: 1,
                items: vec![InventoryItem {
                    name: "iron-ore".to_string(),
                    count: 2,
                }],
                item_count: 2,
            },
            right_lane: LaneContents {
                lane: 2,
                items: vec![InventoryItem {
                    name: "iron-ore".to_string(),
                    count: 1,
                }],
                item_count: 1,
            },
        }];

        let report = analyze_item_flow(
            &entities,
            &belt_contents,
            EntityLookup::Tile(TilePos::new(0, 0)),
            EntityLookup::Tile(TilePos::new(1, 0)),
        );

        assert!(report.connected);
        assert!(report.target_receives_item);
        assert_eq!(report.items_on_path.len(), 1);
        assert_eq!(report.items_on_path[0].total_items, 3);
        assert_eq!(report.items_on_path[0].items[0].name, "iron-ore");
        assert_eq!(report.items_on_path[0].items[0].count, 3);
    }
}
