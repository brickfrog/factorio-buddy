//! Item-flow diagnostics across belts and inserters.

use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};

use super::{analyze_inserters, BeltGraph, BeltReachResult, GapType, InserterAnalysis};
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
    let inserters = analyze_inserters(entities);
    let source = resolve_endpoint(
        entities,
        &graph,
        &inserters,
        source_ref,
        EndpointRole::Source,
    );
    let target = resolve_endpoint(
        entities,
        &graph,
        &inserters,
        target_ref,
        EndpointRole::Target,
    );

    let mut report = ItemFlowReport {
        status: "blocked".to_string(),
        connected: false,
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

    let Some(source_belt) = source.belt_tile else {
        let repair = ItemFlowRepair {
            tool: "route_belt".to_string(),
            reason: "no_source_belt".to_string(),
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
        report.guidance = "Source belt reaches target belt. If production is still blocked, inspect inserter power/filtering and target inventory.".to_string();
        return report;
    }

    let first_break = first_reachable_break(&graph, entities, &report.reachable_belts, target_belt);
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
}

fn resolve_endpoint(
    entities: &[Entity],
    graph: &BeltGraph,
    inserters: &[InserterAnalysis],
    lookup: EntityLookup,
    role: EndpointRole,
) -> ResolvedEndpoint {
    let entity = match lookup {
        EntityLookup::Unit(unit) => entities.iter().find(|e| e.unit_number == Some(unit)),
        EntityLookup::Tile(tile) => entities.iter().find(|e| e.position.to_tile() == tile),
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
    }
}

fn source_belt_from_inserter(
    endpoint: &ItemFlowEndpoint,
    inserters: &[InserterAnalysis],
) -> Option<TilePos> {
    inserters
        .iter()
        .find(|i| {
            i.pickup_target
                .as_ref()
                .is_some_and(|target| same_endpoint(target.unit_number, target.position, endpoint))
                && i.dropoff_target
                    .as_ref()
                    .is_some_and(|target| target.name.contains("belt"))
        })
        .map(|i| i.dropoff_position)
}

fn target_belt_from_inserter(
    endpoint: &ItemFlowEndpoint,
    inserters: &[InserterAnalysis],
) -> Option<TilePos> {
    inserters
        .iter()
        .find(|i| {
            i.dropoff_target
                .as_ref()
                .is_some_and(|target| same_endpoint(target.unit_number, target.position, endpoint))
                && i.pickup_target
                    .as_ref()
                    .is_some_and(|target| target.name.contains("belt"))
        })
        .map(|i| i.pickup_position)
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
    entities: &[Entity],
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
            let blocker = entities.iter().find(|e| e.position.to_tile() == to);
            let reason = match blocker {
                Some(entity) if entity.name.contains("belt") => "wrong_direction",
                Some(_) => "blocked",
                None => "missing_belt",
            };
            let repair = match reason {
                "wrong_direction" => blocker.and_then(|entity| {
                    entity.unit_number.map(|unit| ItemFlowRepair {
                        tool: "rotate_entity".to_string(),
                        reason: reason.to_string(),
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
                    unit_number: None,
                    entity_name: Some("transport-belt".to_string()),
                    x: Some(to.x),
                    y: Some(to.y),
                    direction: Some(node.direction.to_name().to_string()),
                    description:
                        "Route around the blocking entity or remove it before extending the belt."
                            .to_string(),
                }),
                _ => Some(ItemFlowRepair {
                    tool: "place_entity".to_string(),
                    reason: reason.to_string(),
                    unit_number: None,
                    entity_name: Some("transport-belt".to_string()),
                    x: Some(to.x),
                    y: Some(to.y),
                    direction: Some(node.direction.to_name().to_string()),
                    description: format!(
                        "Place transport-belt at ({}, {}) facing {}.",
                        to.x,
                        to.y,
                        node.direction.to_name()
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
                    blocker: blocker.map(|e| e.name.clone()),
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
        }
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
        assert_eq!(report.status, "connected");
        assert!(report.repair.is_none());
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
        assert_eq!(repair.tool, "place_entity");
        assert_eq!(repair.entity_name.as_deref(), Some("transport-belt"));
        assert_eq!(repair.x, Some(1));
        assert_eq!(repair.y, Some(0));
        assert_eq!(repair.direction.as_deref(), Some("east"));
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
