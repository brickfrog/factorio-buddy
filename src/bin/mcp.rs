//! MCP (Model Context Protocol) server for factorioctl
//!
//! Exposes Factorio control as MCP tools for LLM agents.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    schemars::{self, JsonSchema},
    tool, tool_router, ServerHandler, ServiceExt,
};
use serde::{de, Deserialize, Deserializer, Serialize};

use factorioctl::analyze::{
    analyze_belt_reach, analyze_inserters, analyze_item_flow, detect_sushi_belts, find_belt_gaps,
    find_belt_networks, trace_belt_sources, BeltGraph, EntityLookup,
};
use factorioctl::client::{AgentId, FactorioClient};
use factorioctl::issue_report::{BeadsIssueReporter, IssueReportRequest, TrustedIssueContext};
use factorioctl::memory::{AgentMemory, BeltRouting, ProtectedResource, Zone, ZoneType};
use factorioctl::world::{
    build_production_report, build_situation_report, entity_occupied_tiles, entity_size,
    find_belt_route_with_options, Area, BeltKind, BeltPlacement, BeltRouteTopology, Direction,
    Entity, EntityProduction, GridPos, Position, RoutingOptions, TilePos, UndergroundConfig,
};

fn production_verification_json(
    entities: Vec<EntityProduction>,
) -> (serde_json::Value, bool, bool) {
    let producer_count = entities
        .iter()
        .filter(|entity| is_production_entity_name(&entity.name))
        .count();
    let has_working_producer = entities
        .iter()
        .any(|entity| is_production_entity_name(&entity.name) && entity.working);
    let report = build_production_report(entities);
    (
        serde_json::json!({
            "success": has_working_producer,
            "proof": if has_working_producer { "currently_working" } else { "no_active_producer" },
            "producer_count": producer_count,
            "report": report,
        }),
        true,
        has_working_producer,
    )
}

fn production_observation_json(
    before: Vec<EntityProduction>,
    after: Vec<EntityProduction>,
    observation_ticks: u32,
) -> serde_json::Value {
    let before_finished: HashMap<u32, u64> = before
        .iter()
        .filter_map(|entity| Some((entity.unit_number?, entity.products_finished?)))
        .collect();
    let mut progressed_units = Vec::new();
    let mut working_units = Vec::new();
    let producers: Vec<EntityProduction> = after
        .into_iter()
        .filter(|entity| is_production_entity_name(&entity.name))
        .collect();

    for entity in &producers {
        if entity.working {
            if let Some(unit_number) = entity.unit_number {
                working_units.push(unit_number);
            }
        }
        if let (Some(unit_number), Some(after_finished)) =
            (entity.unit_number, entity.products_finished)
        {
            if before_finished
                .get(&unit_number)
                .is_some_and(|before_finished| after_finished > *before_finished)
            {
                progressed_units.push(unit_number);
            }
        }
    }

    let report = build_production_report(producers);
    let success = !progressed_units.is_empty() || report.working_count > 0;
    let report_json = serde_json::to_value(&report).unwrap_or(serde_json::Value::Null);
    serde_json::json!({
        "success": success,
        "proof": if !progressed_units.is_empty() {
            "products_finished_increased"
        } else if report.working_count > 0 {
            "currently_working"
        } else if report.total == 0 {
            "no_producers"
        } else {
            "no_active_production"
        },
        "observation_ticks": observation_ticks,
        "progressed_units": progressed_units,
        "working_units": working_units,
        "producer_count": report.total,
        "working_count": report.working_count,
        "total": report.total,
        "status_counts": report.status_counts,
        "entities": report.entities,
        "report": report_json,
    })
}

fn production_unit_verified(verification: &serde_json::Value, unit_number: Option<u32>) -> bool {
    let Some(unit_number) = unit_number else {
        return false;
    };
    verification
        .get("progressed_units")
        .and_then(|units| units.as_array())
        .is_some_and(|units| {
            units
                .iter()
                .any(|unit| unit.as_u64() == Some(unit_number as u64))
        })
        || verification
            .get("working_units")
            .and_then(|units| units.as_array())
            .is_some_and(|units| {
                units
                    .iter()
                    .any(|unit| unit.as_u64() == Some(unit_number as u64))
            })
}

fn production_unit_observed(verification: &serde_json::Value, unit_number: Option<u32>) -> bool {
    let Some(unit_number) = unit_number else {
        return false;
    };
    verification
        .get("entities")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|entities| {
            entities.iter().any(|entity| {
                entity
                    .get("unit_number")
                    .and_then(serde_json::Value::as_u64)
                    == Some(unit_number as u64)
            })
        })
}

fn is_production_entity_name(name: &str) -> bool {
    name.starts_with("assembling-machine")
        || name.contains("furnace")
        || name.contains("mining-drill")
        || matches!(
            name,
            "lab"
                | "chemical-plant"
                | "oil-refinery"
                | "centrifuge"
                | "rocket-silo"
                | "boiler"
                | "steam-engine"
                | "steam-turbine"
        )
}

fn route_belt_failure_json(
    params: &RouteBeltParams,
    error_kind: &str,
    error: impl Into<String>,
) -> serde_json::Value {
    serde_json::json!({
        "success": false,
        "dry_run": params.dry_run,
        "error_kind": error_kind,
        "error": error.into(),
        "from": { "x": params.from_x, "y": params.from_y },
        "to": { "x": params.to_x, "y": params.to_y },
        "belt_type": params.belt_type,
        "search_radius": params.search_radius,
        "materials_sufficient": false,
    })
}

fn route_material_shortfall(
    surface_name: &str,
    surface_needed: u32,
    surface_available: u32,
    underground_name: Option<&str>,
    underground_needed: u32,
    underground_available: u32,
) -> Option<String> {
    let mut missing = Vec::new();
    if surface_available < surface_needed {
        missing.push(format!(
            "need {surface_needed} {surface_name}, have {surface_available}"
        ));
    }
    if underground_available < underground_needed {
        missing.push(format!(
            "need {underground_needed} {}, have {underground_available}",
            underground_name.unwrap_or("underground-belt")
        ));
    }
    if missing.is_empty() {
        None
    } else {
        Some(format!(
            "Insufficient materials for complete route: {}. No belts were placed.",
            missing.join("; ")
        ))
    }
}

fn compound_route_preflight(
    routes: &[(&str, &serde_json::Value)],
    available_items: &BTreeMap<String, u32>,
    additional_items: &BTreeMap<String, u32>,
    surface_belt_name: &str,
    allowed_shared_tiles: &HashSet<GridPos>,
    reserved_entity_tiles: &[(&str, GridPos)],
) -> serde_json::Value {
    let underground_name = UndergroundConfig::from_belt_type(surface_belt_name)
        .map(|config| config.entity_name)
        .unwrap_or_else(|| "underground-belt".to_string());
    let mut errors = Vec::new();
    let mut occupied: HashMap<GridPos, (String, BeltPlacement)> = HashMap::new();
    let mut new_placements: HashMap<GridPos, BeltPlacement> = HashMap::new();

    for (label, report) in routes {
        if report.get("success").and_then(|value| value.as_bool()) != Some(true) {
            errors.push(serde_json::json!({
                "kind": "route_not_ready",
                "route": label,
                "error": report.get("error"),
            }));
            continue;
        }

        let planned = report
            .get("planned_belts")
            .cloned()
            .and_then(|value| serde_json::from_value::<Vec<BeltPlacement>>(value).ok());
        let Some(planned) = planned else {
            errors.push(serde_json::json!({
                "kind": "missing_route_plan",
                "route": label,
            }));
            continue;
        };
        for belt in planned {
            let tile = GridPos::from_position(&belt.position);
            if let Some((owner, existing)) = occupied.get(&tile) {
                let same_segment =
                    existing.kind == belt.kind && existing.direction == belt.direction;
                if owner != label && (!allowed_shared_tiles.contains(&tile) || !same_segment) {
                    errors.push(serde_json::json!({
                        "kind": "route_overlap",
                        "tile": tile,
                        "routes": [owner, label],
                        "first": existing,
                        "second": belt,
                        "error": "Compound routes overlap outside an explicitly shared, direction-compatible handoff tile.",
                    }));
                }
            } else {
                occupied.insert(tile, ((*label).to_string(), belt));
            }
        }

        let planned_new = report
            .get("planned_new_belts")
            .cloned()
            .and_then(|value| serde_json::from_value::<Vec<BeltPlacement>>(value).ok())
            .unwrap_or_default();
        for belt in planned_new {
            let tile = GridPos::from_position(&belt.position);
            new_placements.entry(tile).or_insert(belt);
        }
    }

    let mut required = additional_items.clone();
    for belt in new_placements.values() {
        let item = match belt.kind {
            BeltKind::Surface => surface_belt_name,
            BeltKind::UndergroundEntry | BeltKind::UndergroundExit => &underground_name,
        };
        *required.entry(item.to_string()).or_default() += 1;
    }

    let mut materials = BTreeMap::new();
    for (item, needed) in &required {
        let available = available_items.get(item).copied().unwrap_or(0);
        let sufficient = available >= *needed;
        if !sufficient {
            errors.push(serde_json::json!({
                "kind": "insufficient_materials",
                "item": item,
                "needed": needed,
                "available": available,
            }));
        }
        materials.insert(
            item.clone(),
            serde_json::json!({
                "needed": needed,
                "available": available,
                "sufficient": sufficient,
            }),
        );
    }

    let mut reserved_entities: HashMap<GridPos, String> = HashMap::new();
    for (label, tile) in reserved_entity_tiles {
        if let Some((route, belt)) = occupied.get(tile) {
            errors.push(serde_json::json!({
                "kind": "route_entity_overlap",
                "tile": tile,
                "route": route,
                "entity": label,
                "belt": belt,
                "error": "A planned route occupies a reserved entity footprint.",
            }));
        }
        if let Some(first) = reserved_entities.insert(*tile, (*label).to_string()) {
            errors.push(serde_json::json!({
                "kind": "entity_footprint_overlap",
                "tile": tile,
                "entities": [first, label],
                "error": "Two planned entities reserve the same footprint.",
            }));
        }
    }

    serde_json::json!({
        "ready": errors.is_empty(),
        "materials": materials,
        "reserved_route_tiles": occupied.len(),
        "reserved_entity_tiles": reserved_entities.len(),
        "reserved_tiles": occupied.len() + reserved_entities.len(),
        "new_belt_tiles": new_placements.len(),
        "errors": errors,
    })
}

fn route_report_placed_units(report: &serde_json::Value) -> Vec<u32> {
    report
        .get("placed_entities")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|entity| entity.get("unit_number").and_then(|value| value.as_u64()))
        .filter_map(|unit| u32::try_from(unit).ok())
        .collect()
}

fn report_success(report: &serde_json::Value) -> bool {
    report.get("success").and_then(|value| value.as_bool()) == Some(true)
}

fn incremental_infrastructure_verification(
    route: &serde_json::Value,
    expected_inserter_unit: Option<u32>,
    actual_inserter: Option<&Entity>,
    expected_name: &str,
    expected_position: Position,
    expected_direction: Direction,
) -> serde_json::Value {
    let route_success = report_success(route);
    let complete_route = route
        .get("complete_route")
        .and_then(|value| value.as_bool())
        == Some(true);
    let topology_connected = route
        .get("topology")
        .and_then(|topology| topology.get("connected"))
        .and_then(|value| value.as_bool())
        == Some(true);
    let expected_tile = GridPos::from_position(&expected_position);
    let actual_tile = actual_inserter.map(|entity| GridPos::from_position(&entity.position));
    let inserter_exists = actual_inserter.is_some();
    let unit_matches = expected_inserter_unit.is_some()
        && actual_inserter.and_then(|entity| entity.unit_number) == expected_inserter_unit;
    let name_matches = actual_inserter.is_some_and(|entity| entity.name.as_str() == expected_name);
    let position_matches = actual_tile == Some(expected_tile);
    let direction_matches =
        actual_inserter.is_some_and(|entity| entity.direction_enum() == expected_direction);
    let inserter_matches_intent =
        inserter_exists && unit_matches && name_matches && position_matches && direction_matches;
    let success = route_success && complete_route && topology_connected && inserter_matches_intent;

    serde_json::json!({
        "success": success,
        "scope": "structural_infrastructure",
        "route": {
            "success": route_success,
            "complete_route": complete_route,
            "topology_connected": topology_connected,
        },
        "inserter": {
            "exists": inserter_exists,
            "matches_intent": inserter_matches_intent,
            "expected_unit_number": expected_inserter_unit,
            "actual_unit_number": actual_inserter.and_then(|entity| entity.unit_number),
            "unit_matches": unit_matches,
            "expected_name": expected_name,
            "actual_name": actual_inserter.map(|entity| entity.name.as_str()),
            "name_matches": name_matches,
            "expected_tile": expected_tile,
            "actual_tile": actual_tile,
            "position_matches": position_matches,
            "expected_direction": expected_direction,
            "actual_direction": actual_inserter.map(Entity::direction_enum),
            "direction_matches": direction_matches,
        },
    })
}

#[derive(Debug, Clone, Copy)]
enum InserterMachineFlow {
    Input,
    Output,
}

impl InserterMachineFlow {
    fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input_to_machine",
            Self::Output => "output_from_machine",
        }
    }

    fn route_endpoint_name(self) -> &'static str {
        match self {
            Self::Input => "goal",
            Self::Output => "start",
        }
    }

    fn machine_interaction_name(self) -> &'static str {
        match self {
            Self::Input => "drop",
            Self::Output => "pickup",
        }
    }
}

fn cardinal_direction_step(direction: Direction) -> Option<(i32, i32)> {
    match direction {
        Direction::North => Some((0, -1)),
        Direction::East => Some((1, 0)),
        Direction::South => Some((0, 1)),
        Direction::West => Some((-1, 0)),
        Direction::NorthEast
        | Direction::SouthEast
        | Direction::SouthWest
        | Direction::NorthWest => None,
    }
}

fn route_topology_tile(route: &serde_json::Value, field: &str) -> Option<GridPos> {
    route
        .get("topology")
        .and_then(|topology| topology.get(field))
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

/// Prove that one standard inserter joins the routed belt to the exact target
/// machine. Route connectivity and inserter intent are insufficient on their
/// own: arbitrary remote geometry can satisfy both while moving no items.
fn inserter_machine_endpoint_verification(
    route: &serde_json::Value,
    inserter_position: Option<Position>,
    inserter_direction: Option<Direction>,
    machine: &Entity,
    flow: InserterMachineFlow,
    phase: &str,
) -> serde_json::Value {
    let route_success = report_success(route);
    let topology_connected = route
        .get("topology")
        .and_then(|topology| topology.get("connected"))
        .and_then(|value| value.as_bool())
        == Some(true);
    let route_start = route_topology_tile(route, "start_tile");
    let route_goal = route_topology_tile(route, "goal_tile");
    let required_route_endpoint = match flow {
        InserterMachineFlow::Input => route_goal,
        InserterMachineFlow::Output => route_start,
    };

    let inserter_tile = inserter_position.as_ref().map(GridPos::from_position);
    let direction_step = inserter_direction.and_then(cardinal_direction_step);
    let pickup_tile = inserter_tile
        .zip(direction_step)
        .map(|(tile, (dx, dy))| GridPos::new(tile.x + dx, tile.y + dy));
    let drop_tile = inserter_tile
        .zip(direction_step)
        .map(|(tile, (dx, dy))| GridPos::new(tile.x - dx, tile.y - dy));
    let inserter_route_endpoint = match flow {
        InserterMachineFlow::Input => pickup_tile,
        InserterMachineFlow::Output => drop_tile,
    };
    let machine_interaction_tile = match flow {
        InserterMachineFlow::Input => drop_tile,
        InserterMachineFlow::Output => pickup_tile,
    };
    let endpoint_matches_route =
        inserter_route_endpoint.is_some() && inserter_route_endpoint == required_route_endpoint;

    let machine_bounds = machine_bounding_box(machine).ok();
    let machine_tiles = if machine_bounds.is_some() {
        entity_occupied_tiles(machine)
    } else {
        Vec::new()
    };
    let machine_interaction_matches = machine_interaction_tile.is_some_and(|interaction| {
        machine_tiles
            .iter()
            .any(|tile| tile.x == interaction.x && tile.y == interaction.y)
    });
    let success = route_success
        && topology_connected
        && direction_step.is_some()
        && endpoint_matches_route
        && machine_bounds.is_some()
        && machine_interaction_matches;

    serde_json::json!({
        "success": success,
        "scope": "route_inserter_machine_topology",
        "phase": phase,
        "flow": flow.as_str(),
        "route": {
            "success": route_success,
            "topology_connected": topology_connected,
            "start_tile": route_start,
            "goal_tile": route_goal,
            "required_endpoint": flow.route_endpoint_name(),
            "required_endpoint_tile": required_route_endpoint,
        },
        "inserter": {
            "position": inserter_position,
            "direction": inserter_direction,
            "cardinal_geometry": direction_step.is_some(),
            "tile": inserter_tile,
            "pickup_tile": pickup_tile,
            "drop_tile": drop_tile,
            "route_endpoint_tile": inserter_route_endpoint,
            "route_endpoint_matches": endpoint_matches_route,
        },
        "machine": {
            "unit_number": machine.unit_number,
            "name": machine.name,
            "position": machine.position,
            "bounding_box": machine_bounds,
            "footprint_known": machine_bounds.is_some(),
            "required_interaction": flow.machine_interaction_name(),
            "interaction_tile": machine_interaction_tile,
            "interaction_intersects_footprint": machine_interaction_matches,
        },
    })
}

fn attach_endpoint_verification(
    mut infrastructure: serde_json::Value,
    endpoint: serde_json::Value,
) -> serde_json::Value {
    let success = report_success(&infrastructure) && report_success(&endpoint);
    if let Some(report) = infrastructure.as_object_mut() {
        report.insert("success".to_string(), serde_json::json!(success));
        report.insert("endpoint_topology".to_string(), endpoint);
    }
    infrastructure
}

fn attach_endpoint_preflight(
    mut preflight: serde_json::Value,
    endpoint: serde_json::Value,
) -> serde_json::Value {
    let ready = preflight.get("ready").and_then(|value| value.as_bool()) == Some(true)
        && report_success(&endpoint);
    if let Some(report) = preflight.as_object_mut() {
        report.insert("ready".to_string(), serde_json::json!(ready));
        report.insert("endpoint_topology".to_string(), endpoint);
    }
    preflight
}

fn production_verification_summary(
    observation: &serde_json::Value,
    target_unit_number: Option<u32>,
) -> serde_json::Value {
    let observation_call_ok = observation.get("error").is_none();
    let production_applicable = production_unit_observed(observation, target_unit_number);
    let target_working_or_progressed = production_unit_verified(observation, target_unit_number);
    serde_json::json!({
        "success": observation_call_ok && target_working_or_progressed,
        "scope": "live_production_observation",
        "observation_call_ok": observation_call_ok,
        "target_unit_number": target_unit_number,
        "production_applicable": production_applicable,
        "target_working_or_progressed": target_working_or_progressed,
        "proof": observation.get("proof"),
    })
}

fn fuel_consumer_activity_verification_summary(
    observation: &serde_json::Value,
    target_unit_number: Option<u32>,
) -> serde_json::Value {
    let mut summary = production_verification_summary(observation, target_unit_number);
    let observation_call_ok = summary
        .get("observation_call_ok")
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    let production_applicable = summary
        .get("production_applicable")
        .and_then(serde_json::Value::as_bool)
        == Some(true);

    if observation_call_ok && !production_applicable {
        if let Some(report) = summary.as_object_mut() {
            report.insert("success".to_string(), serde_json::json!(true));
            report.insert(
                "scope".to_string(),
                serde_json::json!("live_fuel_consumer_observation"),
            );
            report.insert(
                "proof".to_string(),
                serde_json::json!("target_has_no_machine_production_counter"),
            );
        }
    }

    summary
}

fn fuel_topology_verification(
    diagnosis: &serde_json::Value,
    consumer_unit_number: u32,
    inserter_unit_number: Option<u32>,
) -> serde_json::Value {
    let diagnostic_available = diagnosis.get("error").is_none();
    let consumer = diagnosis
        .get("consumers")
        .and_then(|value| value.as_array())
        .and_then(|consumers| {
            consumers.iter().find(|consumer| {
                consumer.get("unit_number").and_then(|value| value.as_u64())
                    == Some(consumer_unit_number as u64)
            })
        });
    let exact_connection = consumer
        .and_then(|consumer| consumer.get("fuel_connections"))
        .and_then(|value| value.as_array())
        .and_then(|connections| {
            connections.iter().find(|connection| {
                inserter_unit_number.is_some()
                    && connection
                        .get("inserter_unit_number")
                        .and_then(|value| value.as_u64())
                        == inserter_unit_number.map(u64::from)
            })
        });
    let exact_proven_connection = consumer
        .and_then(|consumer| consumer.get("proven_fuel_connections"))
        .and_then(|value| value.as_array())
        .and_then(|connections| {
            connections.iter().find(|connection| {
                inserter_unit_number.is_some()
                    && connection
                        .get("inserter_unit_number")
                        .and_then(|value| value.as_u64())
                        == inserter_unit_number.map(u64::from)
            })
        });
    let topology_present = exact_connection.is_some();
    let durable_connection_verified = exact_connection
        .and_then(|connection| connection.get("durable"))
        .and_then(|value| value.as_bool())
        == Some(true);
    let live_supply_verified = exact_connection
        .and_then(|connection| connection.get("live"))
        .and_then(|value| value.as_bool())
        == Some(true);
    let structural_success = diagnostic_available
        && consumer.is_some()
        && topology_present
        && durable_connection_verified;

    serde_json::json!({
        "success": structural_success && live_supply_verified,
        "structural_success": structural_success,
        "scope": "durable_fuel_topology",
        "diagnostic_available": diagnostic_available,
        "consumer_found": consumer.is_some(),
        "consumer_unit_number": consumer_unit_number,
        "inserter_unit_number": inserter_unit_number,
        "exact_connection_present": topology_present,
        "durable_connection_verified": durable_connection_verified,
        "durable_connection_reported": exact_proven_connection.is_some(),
        "live_supply_verified": live_supply_verified,
        "connection": exact_connection,
        "diagnostic_error": diagnosis.get("error"),
    })
}

fn fuel_delivery_path_operational(topology: &serde_json::Value) -> bool {
    topology
        .pointer("/connection/inserter_operational")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        && topology
            .pointer("/connection/source/producer_operational")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
}

fn exact_fuel_feeder_transfer_observed(topology: &serde_json::Value) -> bool {
    fuel_delivery_path_operational(topology)
        && topology
            .pointer("/connection/inserter_status")
            .and_then(serde_json::Value::as_str)
            == Some("working")
        && topology
            .pointer("/connection/inserter_held_item")
            .and_then(serde_json::Value::as_str)
            == Some("coal")
}

fn tool_text_indicates_error(text: &str) -> bool {
    let payload = text
        .split("\n\n--- Player Messages ---")
        .next()
        .unwrap_or(text)
        .trim();
    if payload.starts_with("Error:") || payload.starts_with("MCP error") {
        return true;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return false;
    };
    let Some(object) = value.as_object() else {
        return false;
    };
    object.get("success").and_then(|value| value.as_bool()) == Some(false)
        || object
            .get("error")
            .is_some_and(|value| !value.is_null() && value.as_str() != Some(""))
}

fn semantic_failure(error_kind: &str, error: impl Into<String>) -> String {
    serde_json::json!({
        "success": false,
        "error_kind": error_kind,
        "error": error.into(),
    })
    .to_string()
}

fn invalid_direction_failure(field: &str, value: &str) -> String {
    semantic_failure(
        "invalid_direction",
        format!("Invalid {field} '{value}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)"),
    )
}

fn rollback_missing_identity_error(entity: &Entity) -> serde_json::Value {
    serde_json::json!({
        "unit_number": null,
        "name": entity.name,
        "position": entity.position,
        "error_kind": "missing_unit_number",
        "error": "Rollback skipped: the transaction-created entity had no unit_number, so exact identity removal was impossible; coordinate removal was not attempted.",
    })
}

fn mark_semantic_tool_errors(result: &mut rmcp::model::CallToolResult) {
    if result.content.first().is_some_and(|content| {
        content
            .as_text()
            .is_some_and(|text| tool_text_indicates_error(&text.text))
    }) {
        result.is_error = Some(true);
    }
}

fn rollback_retry_order(unit_numbers: &[u32]) -> Vec<u32> {
    let mut seen = HashSet::new();
    unit_numbers
        .iter()
        .rev()
        .copied()
        .filter(|unit_number| seen.insert(*unit_number))
        .collect()
}

fn rollback_pending_after_pass(attempted: &[u32], removed_this_pass: &HashSet<u32>) -> Vec<u32> {
    attempted
        .iter()
        .copied()
        .filter(|unit_number| !removed_this_pass.contains(unit_number))
        .collect()
}

async fn rollback_exact_units(
    client: &mut FactorioClient,
    unit_numbers: &[u32],
) -> serde_json::Value {
    let mut pending = rollback_retry_order(unit_numbers);
    let mut removed = Vec::new();
    let mut last_errors = HashMap::new();
    let mut attempts = Vec::new();
    let mut pass = 0_u32;

    // Reverse-order rollback can encounter a remote belt before a nearer belt
    // whose removal opens the physical approach. Retry only still-pending exact
    // units after every progress-making pass. A zero-progress pass terminates
    // with the real errors instead of pretending rollback succeeded.
    while !pending.is_empty() {
        pass += 1;
        let pass_units = pending;
        let mut removed_this_pass = HashSet::new();
        for unit_number in pass_units.iter().copied() {
            match client.remove_entity(unit_number).await {
                Ok(()) => {
                    removed_this_pass.insert(unit_number);
                    removed.push(unit_number);
                    last_errors.remove(&unit_number);
                    attempts.push(serde_json::json!({
                        "pass": pass,
                        "unit_number": unit_number,
                        "success": true,
                    }));
                }
                Err(error) => {
                    let error = error.to_string();
                    last_errors.insert(unit_number, error.clone());
                    attempts.push(serde_json::json!({
                        "pass": pass,
                        "unit_number": unit_number,
                        "success": false,
                        "error": error,
                    }));
                }
            }
        }
        pending = rollback_pending_after_pass(&pass_units, &removed_this_pass);
        if removed_this_pass.is_empty() {
            break;
        }
    }
    let errors: Vec<_> = pending
        .iter()
        .map(|unit_number| {
            serde_json::json!({
                "unit_number": unit_number,
                "error": last_errors
                    .get(unit_number)
                    .cloned()
                    .unwrap_or_else(|| "rollback failed without an error detail".to_string()),
            })
        })
        .collect();
    serde_json::json!({
        "success": errors.is_empty(),
        "removed_units": removed,
        "pending_units": pending,
        "passes": pass,
        "attempts": attempts,
        "errors": errors,
    })
}

#[derive(Clone, Copy)]
struct ControllerPlacement<'a> {
    label: &'a str,
    item_name: &'a str,
    entity_name: &'a str,
    position: Position,
    direction: Direction,
}

const FUEL_SOURCE_TAP_INSERTER: &str = "burner-inserter";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
struct BeltSourceTapLayout {
    source_tile: GridPos,
    outward: Direction,
    inserter_tile: GridPos,
    pickup_tile: GridPos,
    drop_tile: GridPos,
    inserter_direction: Direction,
}

impl BeltSourceTapLayout {
    fn inserter_position(self) -> Position {
        self.inserter_tile.to_position()
    }
}

fn belt_source_tap_layouts(source_tile: GridPos) -> [BeltSourceTapLayout; 4] {
    [
        Direction::North,
        Direction::East,
        Direction::South,
        Direction::West,
    ]
    .map(|outward| BeltSourceTapLayout {
        source_tile,
        outward,
        inserter_tile: source_tile.offset(outward, 1),
        pickup_tile: source_tile,
        drop_tile: source_tile.offset(outward, 2),
        // Buddy and Factorio describe an inserter direction by its pickup side.
        inserter_direction: outward.opposite(),
    })
}

fn fuel_route_protects_existing_source(mut route: serde_json::Value) -> serde_json::Value {
    let start_is_incompatible = route
        .get("endpoint_incompatibility")
        .and_then(|value| value.get("endpoint_kind"))
        .and_then(serde_json::Value::as_str)
        == Some("start");
    if !start_is_incompatible {
        return route;
    }

    if let Some(report) = route.as_object_mut() {
        report.remove("next_action");
        report.insert(
            "error_kind".to_string(),
            serde_json::json!("protected_fuel_source_endpoint"),
        );
        report.insert(
            "error".to_string(),
            serde_json::json!(
                "The diagnosed fuel source is existing infrastructure and cannot be rotated or rebuilt by this transaction."
            ),
        );
        report.insert(
            "guidance".to_string(),
            serde_json::json!(
                "Refresh diagnose_fuel_sustainability so the exact source can be tapped without mutating it. No entity was changed."
            ),
        );
    }
    route
}

fn source_tap_filter_verified(report: &serde_json::Value) -> bool {
    report_success(report)
        && report
            .get("atomic_with_placement")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        && report
            .get("readback_verified")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        && report.get("mode").and_then(serde_json::Value::as_str) == Some("whitelist")
        && report
            .get("filtering_enabled")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        && report
            .get("filters")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|filters| {
                filters.len() == 1
                    && filters[0].get("slot").and_then(serde_json::Value::as_u64) == Some(1)
                    && filters[0].get("name").and_then(serde_json::Value::as_str) == Some("coal")
            })
}

fn atomic_filtered_placement_unit(report: &serde_json::Value) -> Option<u32> {
    report
        .get("unit_number")
        .or_else(|| report.pointer("/placement/unit_number"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|unit_number| u32::try_from(unit_number).ok())
}

fn atomic_filtered_placement_local_rollback_verified(report: &serde_json::Value) -> bool {
    report
        .pointer("/rollback/success")
        .and_then(serde_json::Value::as_bool)
        == Some(true)
        && report
            .pointer("/rollback/entity_removed")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
        && report
            .pointer("/rollback/item_returned")
            .and_then(serde_json::Value::as_u64)
            == Some(1)
}

fn atomic_filtered_placement_cleanup_unit(report: &serde_json::Value) -> Option<u32> {
    let unit_number = atomic_filtered_placement_unit(report)?;
    let local_entity_removed = report
        .pointer("/rollback/entity_removed")
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    if report_success(report) || !local_entity_removed {
        Some(unit_number)
    } else {
        None
    }
}

fn verified_atomic_filtered_placement(
    report: &serde_json::Value,
) -> Result<(Entity, serde_json::Value), String> {
    if !report_success(report) {
        return Err(report
            .get("error")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("atomic filtered placement failed")
            .to_string());
    }
    let filter = report
        .get("filter")
        .cloned()
        .ok_or_else(|| "atomic filtered placement omitted filter proof".to_string())?;
    if !source_tap_filter_verified(&filter) {
        return Err(
            "atomically placed inserter did not read back an exact coal whitelist".to_string(),
        );
    }
    let entity: Entity = serde_json::from_value(report.clone())
        .map_err(|error| format!("atomic filtered placement omitted entity proof: {error}"))?;
    Ok((entity, filter))
}

fn rollback_removed_exact_unit(rollback: &serde_json::Value, unit_number: u32) -> bool {
    rollback
        .pointer("/infrastructure/units/removed_units")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|removed_units| {
            removed_units
                .iter()
                .any(|removed| removed.as_u64() == Some(u64::from(unit_number)))
        })
}

fn atomic_filtered_placement_cleanup_evidence(
    report: Option<&serde_json::Value>,
    rollback: &serde_json::Value,
) -> serde_json::Value {
    let Some(report) = report else {
        return serde_json::json!({
            "success": false,
            "outcome_known": false,
            "error": "The atomic placement response was unavailable, so exact entity and item conservation cannot be certified.",
        });
    };

    let remote_success = report_success(report);
    let unit_number = atomic_filtered_placement_unit(report);
    let local_rollback_verified = atomic_filtered_placement_local_rollback_verified(report);
    let explicit_outcome_known = report
        .get("atomic_outcome_known")
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    let no_entity_created = !remote_success
        && explicit_outcome_known
        && report
            .get("entity_created")
            .and_then(serde_json::Value::as_bool)
            == Some(false);
    let outcome_known = explicit_outcome_known || remote_success || unit_number.is_some();
    let host_cleanup_attempted = atomic_filtered_placement_cleanup_unit(report).is_some();
    let host_exact_unit_removed =
        unit_number.is_some_and(|unit_number| rollback_removed_exact_unit(rollback, unit_number));

    // A failed Lua rollback remains a failed conservation proof even when the
    // host later removes the exact entity: the missing placement item was not
    // independently recovered. A Rust-only verifier rejection is different;
    // the accepted entity is conserved when exact-unit host removal succeeds.
    let success = if !outcome_known {
        false
    } else if no_entity_created {
        true
    } else if remote_success {
        host_exact_unit_removed
    } else {
        local_rollback_verified
    };

    serde_json::json!({
        "success": success,
        "outcome_known": outcome_known,
        "remote_success": remote_success,
        "unit_number": unit_number,
        "no_entity_created": no_entity_created,
        "local_rollback_verified": local_rollback_verified,
        "local_rollback": report.get("rollback"),
        "host_cleanup_attempted": host_cleanup_attempted,
        "host_exact_unit_removed": host_exact_unit_removed,
    })
}

fn source_entity_preservation(
    before: &Entity,
    after: Option<&Entity>,
    expected_tile: GridPos,
) -> serde_json::Value {
    let unit_matches = before.unit_number.is_some()
        && after.and_then(|entity| entity.unit_number) == before.unit_number;
    let name_matches = after.is_some_and(|entity| entity.name == before.name);
    let tile_matches =
        after.map(|entity| GridPos::from_position(&entity.position)) == Some(expected_tile);
    let direction_matches = after.is_some_and(|entity| entity.direction == before.direction);
    serde_json::json!({
        "success": unit_matches && name_matches && tile_matches && direction_matches,
        "unit_number": before.unit_number,
        "unit_matches": unit_matches,
        "name": before.name,
        "name_matches": name_matches,
        "tile": expected_tile,
        "tile_matches": tile_matches,
        "direction_before": Direction::from_factorio(before.direction),
        "direction_after": after.map(Entity::direction_enum),
        "direction_matches": direction_matches,
    })
}

fn source_tap_plan_rank(
    route: &serde_json::Value,
    preflight: &serde_json::Value,
    layout_index: usize,
) -> (u8, u8, u64, usize) {
    let ready = preflight.get("ready").and_then(serde_json::Value::as_bool) == Some(true);
    let route_connected = report_success(route)
        && route
            .get("topology")
            .and_then(|value| value.get("connected"))
            .and_then(serde_json::Value::as_bool)
            == Some(true);
    let placements_allowed = ["source_tap_inserter", "fuel_inserter"]
        .into_iter()
        .all(|label| {
            preflight
                .get("placements")
                .and_then(|value| value.get(label))
                .and_then(|value| value.get("allowed"))
                .and_then(serde_json::Value::as_bool)
                == Some(true)
        });
    let geometry_valid = preflight
        .get("routes")
        .and_then(|value| value.get("errors"))
        .and_then(serde_json::Value::as_array)
        .is_none_or(|errors| {
            errors.iter().all(|error| {
                error.get("kind").and_then(serde_json::Value::as_str)
                    == Some("insufficient_materials")
            })
        });
    let belt_count = route
        .get("new_belt_count")
        .or_else(|| route.get("belt_count"))
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(u64::MAX);
    (
        u8::from(!ready),
        u8::from(!(route_connected && placements_allowed && geometry_valid)),
        belt_count,
        layout_index,
    )
}

#[derive(Clone)]
struct FuelSourceTapPlan {
    layout: BeltSourceTapLayout,
    route_params: RouteBeltParams,
    route: serde_json::Value,
    preflight: serde_json::Value,
}

#[derive(Clone)]
struct PlannedPlacement {
    label: String,
    entity_name: String,
    position: Position,
    direction: Direction,
}

#[derive(Clone, Copy)]
struct PlannedRotation {
    unit_number: u32,
    direction: Direction,
}

fn parse_controller_steps(
    steps: &[serde_json::Value],
) -> Result<(Vec<PlannedPlacement>, Vec<PlannedRotation>), String> {
    let mut placements = Vec::new();
    let mut rotations = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        let tool = step
            .get("tool")
            .and_then(|value| value.as_str())
            .ok_or_else(|| format!("step {index} is missing tool"))?;
        let args = step
            .get("tool_args")
            .ok_or_else(|| format!("step {index} is missing tool_args"))?;
        let direction_name = args
            .get("direction")
            .and_then(|value| value.as_str())
            .unwrap_or("north");
        let direction = Direction::parse(direction_name)
            .ok_or_else(|| format!("step {index} has invalid direction {direction_name}"))?;
        match tool {
            "place_entity" => {
                let entity_name = args
                    .get("entity_name")
                    .and_then(|value| value.as_str())
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| format!("step {index} is missing entity_name"))?;
                let x = args
                    .get("x")
                    .and_then(|value| value.as_f64())
                    .ok_or_else(|| format!("step {index} is missing x"))?;
                let y = args
                    .get("y")
                    .and_then(|value| value.as_f64())
                    .ok_or_else(|| format!("step {index} is missing y"))?;
                placements.push(PlannedPlacement {
                    label: format!("step_{index}_{entity_name}"),
                    entity_name: entity_name.to_string(),
                    position: Position::new(x, y),
                    direction,
                });
            }
            "rotate_entity" => {
                let unit_number = args
                    .get("unit_number")
                    .and_then(|value| value.as_u64())
                    .and_then(|value| u32::try_from(value).ok())
                    .ok_or_else(|| format!("step {index} is missing unit_number"))?;
                rotations.push(PlannedRotation {
                    unit_number,
                    direction,
                });
            }
            other => return Err(format!("step {index} uses unsupported tool {other}")),
        }
    }
    Ok((placements, rotations))
}

/// Lua reports Factorio's engine decision and Buddy's live placement policy
/// separately. Mutation preflight must require the combined decision; a stale
/// mod that does not provide it fails closed.
fn placement_report_allowed(report: &serde_json::Value) -> bool {
    report.get("allowed").and_then(serde_json::Value::as_bool) == Some(true)
}

async fn controller_preflight(
    client: &mut FactorioClient,
    routes: &[(&str, &serde_json::Value)],
    surface_belt_name: &str,
    allowed_shared_tiles: &HashSet<GridPos>,
    placements: &[ControllerPlacement<'_>],
) -> Result<serde_json::Value, String> {
    let inventory = client
        .character_inventory()
        .await
        .map_err(|error| format!("checking controller materials: {error}"))?;
    let available_items: BTreeMap<String, u32> = inventory
        .items
        .iter()
        .map(|item| (item.name.clone(), item.count))
        .collect();
    let mut additional_items = BTreeMap::new();
    for placement in placements {
        *additional_items
            .entry(placement.item_name.to_string())
            .or_default() += 1;
    }
    let reserved_entity_tiles: Vec<(&str, GridPos)> = placements
        .iter()
        .map(|placement| (placement.label, GridPos::from_position(&placement.position)))
        .collect();
    let route_preflight = compound_route_preflight(
        routes,
        &available_items,
        &additional_items,
        surface_belt_name,
        allowed_shared_tiles,
        &reserved_entity_tiles,
    );

    let mut placement_reports = serde_json::Map::new();
    let mut placements_ready = true;
    for placement in placements {
        let report = client
            .check_entity_placement(
                placement.entity_name,
                placement.position,
                placement.direction,
            )
            .await;
        let allowed = report.as_ref().ok().is_some_and(placement_report_allowed);
        placements_ready &= allowed;
        placement_reports.insert(
            placement.label.to_string(),
            match report {
                Ok(value) => serde_json::json!({
                    "allowed": allowed,
                    "item_name": placement.item_name,
                    "entity_name": placement.entity_name,
                    "position": placement.position,
                    "direction": placement.direction,
                    "report": value,
                }),
                Err(error) => serde_json::json!({
                    "allowed": false,
                    "item_name": placement.item_name,
                    "entity_name": placement.entity_name,
                    "position": placement.position,
                    "direction": placement.direction,
                    "error": error.to_string(),
                }),
            },
        );
    }

    let ready = route_preflight
        .get("ready")
        .and_then(|value| value.as_bool())
        == Some(true)
        && placements_ready;
    Ok(serde_json::json!({
        "ready": ready,
        "routes": route_preflight,
        "placements": placement_reports,
    }))
}

async fn observe_production(
    client: &mut FactorioClient,
    area: Area,
    observation_ticks: u32,
) -> anyhow::Result<serde_json::Value> {
    let before = client.verify_production(area).await?;
    client.wait_ticks(observation_ticks).await?;
    let after = client.verify_production(area).await?;
    Ok(production_observation_json(
        before,
        after,
        observation_ticks,
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecipeRestoreAction<'a> {
    Set(&'a str),
    Clear,
}

impl RecipeRestoreAction<'_> {
    fn operation(self) -> &'static str {
        match self {
            Self::Set(_) => "set",
            Self::Clear => "clear",
        }
    }
}

fn recipe_restore_action(recipe: Option<&str>) -> RecipeRestoreAction<'_> {
    match recipe {
        Some(recipe) => RecipeRestoreAction::Set(recipe),
        None => RecipeRestoreAction::Clear,
    }
}

async fn rollback_controller_transaction(
    client: &mut FactorioClient,
    unit_numbers: &[u32],
    rotated_entities: &[(u32, u8)],
    recipe_restore: Option<(u32, Option<String>)>,
) -> serde_json::Value {
    let units = rollback_exact_units(client, unit_numbers).await;
    let mut rotation_errors = Vec::new();
    let mut restored_rotations = Vec::new();
    for (unit_number, direction) in rotated_entities.iter().rev().copied() {
        match client.rotate_entity(unit_number, direction).await {
            Ok(_) => restored_rotations.push(unit_number),
            Err(error) => rotation_errors.push(serde_json::json!({
                "unit_number": unit_number,
                "direction": direction,
                "error": error.to_string(),
            })),
        }
    }

    let recipe = if let Some((unit_number, recipe)) = recipe_restore {
        let action = recipe_restore_action(recipe.as_deref());
        let restore_result = match action {
            RecipeRestoreAction::Set(recipe_name) => {
                client.set_recipe(unit_number, recipe_name).await
            }
            RecipeRestoreAction::Clear => client.clear_recipe(unit_number).await,
        };
        match restore_result {
            Ok(()) => serde_json::json!({
                "success": true,
                "unit_number": unit_number,
                "operation": action.operation(),
                "restored_recipe": recipe,
            }),
            Err(error) => serde_json::json!({
                "success": false,
                "unit_number": unit_number,
                "operation": action.operation(),
                "restored_recipe": recipe,
                "error": error.to_string(),
            }),
        }
    } else {
        serde_json::json!({"success": true, "skipped": true})
    };
    let success = units.get("success").and_then(|value| value.as_bool()) == Some(true)
        && rotation_errors.is_empty()
        && recipe.get("success").and_then(|value| value.as_bool()) == Some(true);
    serde_json::json!({
        "success": success,
        "units": units,
        "restored_rotations": restored_rotations,
        "rotation_errors": rotation_errors,
        "recipe": recipe,
    })
}

async fn rollback_failed_fuel_transaction(
    client: &mut FactorioClient,
    consumer_snapshot: &serde_json::Value,
    feeder_unit_number: Option<u32>,
    transaction_units: &[u32],
) -> serde_json::Value {
    // One Lua call first disables the exact transaction feeder and restores the
    // pre-existing consumer. Only then may slower, reach-aware entity mining
    // remove the new inserter and route without racing another delivery.
    let consumer_state = match client
        .rollback_burner_bootstrap(consumer_snapshot, feeder_unit_number)
        .await
    {
        Ok(report) => report,
        Err(error) => serde_json::json!({
            "success": false,
            "consumer_state_restored": false,
            "transaction_fuel_cleared": false,
            "feeder_unit_number": feeder_unit_number,
            "error": error.to_string(),
        }),
    };
    let infrastructure =
        rollback_controller_transaction(client, transaction_units, &[], None).await;
    let state_success = report_success(&consumer_state);
    let infrastructure_success = report_success(&infrastructure);
    serde_json::json!({
        "success": state_success && infrastructure_success,
        "transaction_fuel_cleared": consumer_state
            .get("transaction_fuel_cleared")
            .and_then(|value| value.as_bool()) == Some(true),
        "consumer_state": consumer_state,
        "infrastructure": infrastructure,
    })
}

async fn rollback_failed_atomic_fuel_placement(
    client: &mut FactorioClient,
    consumer_snapshot: &serde_json::Value,
    feeder_unit_number: Option<u32>,
    transaction_units: &[u32],
    atomic_report: Option<&serde_json::Value>,
) -> serde_json::Value {
    let cleanup_unit = atomic_report.and_then(atomic_filtered_placement_cleanup_unit);
    let mut rollback_units = transaction_units.to_vec();
    if let Some(unit_number) = cleanup_unit {
        rollback_units.push(unit_number);
    }

    // For a rejected terminal placement, disable the exact new feeder before
    // slower reach-aware removal. Source-tap failures pass the already-known
    // terminal feeder explicitly, so it remains the consumer rollback guard.
    let feeder_unit_number = feeder_unit_number.or(cleanup_unit);
    let mut rollback = rollback_failed_fuel_transaction(
        client,
        consumer_snapshot,
        feeder_unit_number,
        &rollback_units,
    )
    .await;
    let atomic_cleanup = atomic_filtered_placement_cleanup_evidence(atomic_report, &rollback);
    let success = report_success(&rollback) && report_success(&atomic_cleanup);
    if let Some(object) = rollback.as_object_mut() {
        object.insert("success".to_string(), serde_json::json!(success));
        object.insert("atomic_cleanup".to_string(), atomic_cleanup);
    }
    rollback
}

fn belt_topology_travel_tiles(topology: &BeltRouteTopology) -> u32 {
    topology.steps.iter().fold(0_u32, |distance, step| {
        distance.saturating_add(
            step.next_tile
                .map(|next| step.tile.manhattan_distance(&next))
                .unwrap_or(0),
        )
    })
}

fn compact_belt_topology(topology: Option<&BeltRouteTopology>) -> serde_json::Value {
    match topology {
        Some(topology) => serde_json::json!({
            "connected": topology.connected,
            "start_tile": topology.start_tile,
            "goal_tile": topology.goal_tile,
            "step_count": topology.steps.len(),
            "travel_distance_tiles": belt_topology_travel_tiles(topology),
            "errors": topology.errors,
        }),
        None => serde_json::Value::Null,
    }
}

fn fuel_delivery_wait_budget(route: &serde_json::Value) -> (u32, u32) {
    let transit_tiles = route
        .pointer("/topology/travel_distance_tiles")
        .or_else(|| route.get("belt_count"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|count| u32::try_from(count).ok())
        .unwrap_or(0);
    (transit_tiles, fuel_delivery_budget_ticks(transit_tiles))
}

fn fuel_delivery_budget_ticks(transit_tiles: u32) -> u32 {
    transit_tiles
        .saturating_mul(40)
        .saturating_add(600)
        .max(600)
}

fn fuel_topology_upstream_hops(topology: &serde_json::Value) -> u32 {
    topology
        .pointer("/connection/source/upstream_proof/hops")
        .and_then(serde_json::Value::as_u64)
        .and_then(|hops| u32::try_from(hops).ok())
        .unwrap_or(0)
}

fn unsupported_fuel_transport(allow_underground: bool, dry_run: bool) -> Option<serde_json::Value> {
    allow_underground.then(|| {
        serde_json::json!({
            "success": false,
            "error_kind": "unsupported_fuel_transport",
            "error": "Underground belts are not yet supported by durable fuel-topology verification; no route was planned or placed.",
            "allow_underground": true,
            "dry_run": dry_run,
            "guidance": "Retry with allow_underground=false so the complete coal path can be verified before commit.",
        })
    })
}

fn deserialize_tile_i32<'de, D>(deserializer: D) -> Result<i32, D::Error>
where
    D: Deserializer<'de>,
{
    let value = f64::deserialize(deserializer)?;
    if !value.is_finite() {
        return Err(de::Error::custom("tile coordinate must be finite"));
    }
    if value < i32::MIN as f64 || value > i32::MAX as f64 {
        return Err(de::Error::custom("tile coordinate out of i32 range"));
    }
    Ok(value.floor() as i32)
}

fn verification_statuses(verification: &serde_json::Value) -> Vec<String> {
    verification
        .get("report")
        .and_then(|report| report.get("entities"))
        .and_then(|entities| entities.as_array())
        .map(|entities| {
            entities
                .iter()
                .filter_map(|entity| entity.get("status").and_then(|status| status.as_str()))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn placed_statuses(placed_unit_statuses: &[serde_json::Value]) -> Vec<String> {
    placed_unit_statuses
        .iter()
        .filter(|status| {
            !status
                .get("ok")
                .and_then(|ok| ok.as_bool())
                .unwrap_or(false)
        })
        .filter_map(|status| status.get("status").and_then(|status| status.as_str()))
        .map(str::to_owned)
        .collect()
}

fn automation_repair_hint(
    success_tool: &str,
    context: &str,
    verification_call_ok: bool,
    verification: &serde_json::Value,
    placed_unit_statuses: &[serde_json::Value],
    route_success: Option<bool>,
) -> serde_json::Value {
    let report_statuses = verification_statuses(verification);
    let placed_statuses = placed_statuses(placed_unit_statuses);
    let has = |status: &str| {
        report_statuses.iter().any(|seen| seen == status)
            || placed_statuses.iter().any(|seen| seen == status)
    };
    let mut actions = Vec::new();

    if !verification_call_ok {
        actions.push(serde_json::json!({
            "tool": "verify_production",
            "reason": "verification call failed; inspect the target area before mutating again",
        }));
    }
    if route_success == Some(false) {
        actions.push(serde_json::json!({
            "tool": "analyze_item_flow",
            "reason": "inspect the failed route endpoints and existing belt flow before retrying",
        }));
        actions.push(serde_json::json!({
            "tool": "route_belt",
            "reason": "rerun the route with corrected endpoints or more belt material",
        }));
    }
    if has("no_power") {
        actions.push(serde_json::json!({
            "tool": "execute_entity_placement_near",
            "entity_name": "small-electric-pole",
            "reason": "a placed inserter, drill, lab, or assembler is unpowered",
        }));
        actions.push(serde_json::json!({
            "tool": "verify_production",
            "reason": "confirm the powered entity changes from no_power to working or waiting_for_source_items",
        }));
    }
    if has("no_fuel") {
        actions.push(serde_json::json!({
            "tool": "repair_fuel_sustainability",
            "reason": "diagnose and build a durable coal belt/inserter path instead of hand-feeding a small fuel buffer",
        }));
    }
    if has("no_ingredients") || has("waiting_for_source_items") {
        actions.push(serde_json::json!({
            "tool": "analyze_item_flow",
            "reason": "the consumer is alive but not receiving the expected item",
        }));
        actions.push(serde_json::json!({
            "tool": "build_assembler_feed",
            "reason": "if the empty consumer is an assembler, build a durable input feed",
        }));
        actions.push(serde_json::json!({
            "tool": "execute_direct_smelter",
            "reason": "if the empty consumer is a furnace, build or repair the drill-to-furnace feed cell",
        }));
    }
    if has("full_output") || has("waiting_for_space_in_destination") {
        actions.push(serde_json::json!({
            "tool": "build_assembler_output",
            "reason": "the producer cannot drain output; build a durable output belt/inserter",
        }));
        actions.push(serde_json::json!({
            "tool": "analyze_item_flow",
            "reason": "trace the blocked output path before placing more entities",
        }));
    }
    if has("missing_from_verification_area") {
        actions.push(serde_json::json!({
            "tool": "verify_production",
            "reason": "the newly placed unit was outside the verification radius or was not actually created",
        }));
    }
    if actions.is_empty() {
        actions.push(serde_json::json!({
            "tool": "verify_production",
            "reason": "automation did not prove working; inspect statuses before using manual transfer tools",
        }));
    }

    serde_json::json!({
        "context": context,
        "if_success": format!("{success_tool} completed durable automation; keep using automation controllers instead of manual insert/extract loops."),
        "if_failed": actions,
        "observed_statuses": {
            "verification": report_statuses,
            "placed_units": placed_statuses,
        },
        "anti_pattern": "Do not treat hand-feeding, hand-crafting, or inventory extraction as completion when automation_verified.success is false.",
    })
}

/// Connection configuration loaded from environment or config
#[derive(Clone)]
struct ConnectionConfig {
    host: String,
    port: u16,
    password: String,
}

#[cfg(test)]
mod tests {
    use super::{
        atomic_filtered_placement_cleanup_evidence, atomic_filtered_placement_cleanup_unit,
        atomic_filtered_placement_local_rollback_verified, atomic_filtered_placement_unit,
        attach_endpoint_preflight, automation_repair_hint, belt_source_tap_layouts,
        bounded_bootstrap_output_count, compact_fuel_diagnosis, compact_fuel_repair,
        compound_route_preflight, direct_placement_requires_route, endpoint_belt_incompatibility,
        exact_fuel_feeder_transfer_observed, execute_lua_refusal, existing_belt_compatibility,
        existing_underground_pair_reservations, flow_lookup, flow_scan_area,
        fuel_consumer_activity_verification_summary, fuel_delivery_budget_ticks,
        fuel_delivery_path_operational, fuel_delivery_wait_budget,
        fuel_route_protects_existing_source, fuel_topology_upstream_hops,
        fuel_topology_verification, incremental_infrastructure_verification,
        inserter_machine_endpoint_verification, invalid_direction_failure, is_existing_belt_entity,
        is_machine_output_source, machine_output_build_args, machine_side_layout,
        mark_semantic_tool_errors, model_safe_payload, parse_controller_steps,
        placement_report_allowed, production_observation_json, production_unit_verified,
        production_verification_json, production_verification_summary, raw_lua_enabled,
        ready_fuel_supply_args, recipe_restore_action, rollback_missing_identity_error,
        rollback_pending_after_pass, rollback_retry_order, route_belt_failure_json,
        route_material_shortfall, route_segment_waypoint, semantic_failure,
        source_entity_preservation, source_tap_filter_verified, source_tap_plan_rank,
        tool_text_indicates_error, unsupported_fuel_transport, BuildFuelSupplyParams, FactorioMcp,
        InserterMachineFlow, RecipeRestoreAction, RouteBeltParams, MODEL_VISIBLE_TOOLS,
    };
    use factorioctl::analyze::EntityLookup;
    use factorioctl::world::{
        Area, BeltKind, BeltPlacement, Direction, Entity, GridPos, Position, TilePos,
    };
    use std::collections::{BTreeMap, HashSet};

    #[test]
    fn bootstrap_output_collection_is_bounded_but_independent_of_source_count() {
        assert_eq!(bounded_bootstrap_output_count(0), 1);
        assert_eq!(bounded_bootstrap_output_count(39), 39);
        assert_eq!(bounded_bootstrap_output_count(1_001), 1_000);
    }

    fn assert_emitted_tool_fields_are_model_visible(value: &serde_json::Value) {
        match value {
            serde_json::Value::Array(values) => {
                for value in values {
                    assert_emitted_tool_fields_are_model_visible(value);
                }
            }
            serde_json::Value::Object(object) => {
                if let Some(tool) = object.get("tool") {
                    let tool = tool.as_str().expect("emitted tool fields must be strings");
                    assert!(
                        MODEL_VISIBLE_TOOLS.contains(&tool),
                        "response emitted unavailable tool {tool}: {value}"
                    );
                }
                for value in object.values() {
                    assert_emitted_tool_fields_are_model_visible(value);
                }
            }
            _ => {}
        }
    }

    #[test]
    fn raw_lua_enabled_only_accepts_explicit_truthy_values() {
        let cases = [
            (None, false),
            (Some(""), false),
            (Some("0"), false),
            (Some("false"), false),
            (Some("1"), true),
            (Some("true"), true),
            (Some("TRUE"), true),
            (Some("yes"), true),
            (Some("on"), true),
            (Some(" 1 "), true),
            (Some("banana"), false),
        ];

        for (env_value, expected) in cases {
            assert_eq!(raw_lua_enabled(env_value), expected, "{env_value:?}");
        }
    }

    #[test]
    fn edge_miner_steps_parse_as_one_atomic_placement_set() {
        let steps = vec![
            serde_json::json!({
                "tool": "place_entity",
                "tool_args": {
                    "entity_name": "burner-mining-drill",
                    "x": 10.0,
                    "y": 20.0,
                    "direction": "east",
                },
            }),
            serde_json::json!({
                "tool": "place_entity",
                "tool_args": {
                    "entity_name": "transport-belt",
                    "x": 12.0,
                    "y": 20.0,
                    "direction": "east",
                },
            }),
        ];

        let (placements, rotations) = parse_controller_steps(&steps).expect("valid edge plan");
        assert!(rotations.is_empty());
        assert_eq!(placements.len(), 2);
        assert_eq!(placements[0].entity_name, "burner-mining-drill");
        assert_eq!(placements[1].entity_name, "transport-belt");
        assert_eq!(placements[0].direction, Direction::East);
    }

    #[test]
    fn edge_miner_verification_requires_the_new_drill_not_an_unrelated_producer() {
        let verification = serde_json::json!({
            "success": true,
            "working_units": [99],
            "progressed_units": [88],
        });

        assert!(!production_unit_verified(&verification, Some(10)));
        assert!(production_unit_verified(&verification, Some(99)));
        assert!(production_unit_verified(&verification, Some(88)));
        assert!(!production_unit_verified(&verification, None));
    }

    #[test]
    fn production_verification_does_not_count_transport_as_production() {
        let entities = vec![
            factorioctl::world::EntityProduction {
                name: "transport-belt".to_string(),
                unit_number: Some(1),
                position: Position::new(0.5, 0.5),
                status: "working".to_string(),
                products_finished: None,
                working: true,
            },
            factorioctl::world::EntityProduction {
                name: "inserter".to_string(),
                unit_number: Some(2),
                position: Position::new(1.5, 0.5),
                status: "working".to_string(),
                products_finished: None,
                working: true,
            },
            factorioctl::world::EntityProduction {
                name: "stone-furnace".to_string(),
                unit_number: Some(3),
                position: Position::new(2.0, 0.0),
                status: "no_ingredients".to_string(),
                products_finished: Some(0),
                working: false,
            },
        ];

        let (verification, call_ok, has_working_producer) = production_verification_json(entities);
        assert!(call_ok);
        assert!(!has_working_producer);
        assert_eq!(verification["success"], false);
        assert_eq!(verification["producer_count"], 1);
    }

    #[test]
    fn production_observation_rejects_idle_and_accepts_real_progress() {
        let furnace = |status: &str, products_finished: u64, working: bool| {
            factorioctl::world::EntityProduction {
                name: "stone-furnace".to_string(),
                unit_number: Some(3),
                position: Position::new(2.0, 0.0),
                status: status.to_string(),
                products_finished: Some(products_finished),
                working,
            }
        };

        let idle = production_observation_json(
            vec![furnace("no_ingredients", 4, false)],
            vec![furnace("no_ingredients", 4, false)],
            60,
        );
        assert_eq!(idle["success"], false);
        assert_eq!(idle["proof"], "no_active_production");
        assert_eq!(idle["working_count"], 0);

        let progressed = production_observation_json(
            vec![furnace("working", 4, true)],
            vec![furnace("waiting_for_space_in_destination", 5, false)],
            60,
        );
        assert_eq!(progressed["success"], true);
        assert_eq!(progressed["proof"], "products_finished_increased");
        assert_eq!(progressed["progressed_units"], serde_json::json!([3]));
    }

    #[test]
    fn incremental_controller_keeps_verified_infrastructure_when_machine_is_idle() {
        let route = serde_json::json!({
            "success": true,
            "complete_route": true,
            "topology": {"connected": true},
        });
        let inserter = Entity {
            unit_number: Some(42),
            name: "inserter".to_string(),
            entity_type: Some("inserter".to_string()),
            position: Position::new(4.5, 7.5),
            direction: Direction::East.to_factorio(),
            health: Some(160.0),
            force: Some("player".to_string()),
            bounding_box: None,
            pickup_position: None,
            drop_position: None,
            belt_to_ground_type: None,
            underground_belt_neighbour: None,
            belt_input_neighbours: Vec::new(),
            belt_output_neighbours: Vec::new(),
            belt_neighbours_observed: false,
        };
        let infrastructure = incremental_infrastructure_verification(
            &route,
            Some(42),
            Some(&inserter),
            "inserter",
            Position::new(4.0, 7.0),
            Direction::East,
        );
        let production = production_verification_summary(
            &serde_json::json!({
                "success": false,
                "proof": "no_active_production",
                "working_units": [],
                "progressed_units": [],
                "entities": [{"unit_number": 900, "name": "stone-furnace"}],
            }),
            Some(900),
        );

        assert_eq!(infrastructure["success"], true);
        assert_eq!(production["success"], false);
        assert_eq!(production["production_applicable"], true);
        assert_eq!(production["target_working_or_progressed"], false);
    }

    #[test]
    fn production_verification_does_not_demand_machine_output_from_transport_consumer() {
        let observation = serde_json::json!({
            "success": false,
            "proof": "no_active_production",
            "working_units": [],
            "progressed_units": [],
            "entities": [],
        });
        let strict_production = production_verification_summary(&observation, Some(490));
        let production = fuel_consumer_activity_verification_summary(&observation, Some(490));

        assert_eq!(strict_production["success"], false);
        assert_eq!(production["success"], true);
        assert_eq!(production["production_applicable"], false);
        assert_eq!(production["target_working_or_progressed"], false);
        assert_eq!(
            production["proof"],
            "target_has_no_machine_production_counter"
        );
    }

    #[test]
    fn incremental_infrastructure_rejects_wrong_inserter_direction() {
        let route = serde_json::json!({
            "success": true,
            "complete_route": true,
            "topology": {"connected": true},
        });
        let inserter = Entity {
            unit_number: Some(42),
            name: "inserter".to_string(),
            entity_type: Some("inserter".to_string()),
            position: Position::new(4.5, 7.5),
            direction: Direction::West.to_factorio(),
            health: Some(160.0),
            force: Some("player".to_string()),
            bounding_box: None,
            pickup_position: None,
            drop_position: None,
            belt_to_ground_type: None,
            underground_belt_neighbour: None,
            belt_input_neighbours: Vec::new(),
            belt_output_neighbours: Vec::new(),
            belt_neighbours_observed: false,
        };

        let report = incremental_infrastructure_verification(
            &route,
            Some(42),
            Some(&inserter),
            "inserter",
            Position::new(4.0, 7.0),
            Direction::East,
        );

        assert_eq!(report["success"], false);
        assert_eq!(report["inserter"]["exists"], true);
        assert_eq!(report["inserter"]["direction_matches"], false);
        assert_eq!(report["inserter"]["matches_intent"], false);
    }

    fn endpoint_test_machine(name: &str, entity_type: &str) -> Entity {
        Entity {
            unit_number: Some(900),
            name: name.to_string(),
            entity_type: Some(entity_type.to_string()),
            position: Position::new(11.5, 11.5),
            direction: Direction::North.to_factorio(),
            health: Some(300.0),
            force: Some("player".to_string()),
            bounding_box: Some(Area::new(10.0, 10.0, 13.0, 13.0)),
            pickup_position: None,
            drop_position: None,
            belt_to_ground_type: None,
            underground_belt_neighbour: None,
            belt_input_neighbours: Vec::new(),
            belt_output_neighbours: Vec::new(),
            belt_neighbours_observed: false,
        }
    }

    fn endpoint_test_route(start: GridPos, goal: GridPos) -> serde_json::Value {
        serde_json::json!({
            "success": true,
            "complete_route": true,
            "topology": {
                "connected": true,
                "start_tile": start,
                "goal_tile": goal,
            },
        })
    }

    #[test]
    fn input_endpoint_proof_requires_route_goal_pickup_and_machine_drop() {
        let lab = endpoint_test_machine("lab", "lab");
        let route = endpoint_test_route(GridPos::new(0, 8), GridPos::new(11, 8));
        let valid = inserter_machine_endpoint_verification(
            &route,
            Some(Position::new(11.5, 9.5)),
            Some(Direction::North),
            &lab,
            InserterMachineFlow::Input,
            "planned",
        );

        assert_eq!(valid["success"], true);
        assert_eq!(valid["flow"], "input_to_machine");
        assert_eq!(valid["route"]["required_endpoint"], "goal");
        assert_eq!(valid["inserter"]["pickup_tile"]["x"], 11);
        assert_eq!(valid["inserter"]["pickup_tile"]["y"], 8);
        assert_eq!(valid["inserter"]["route_endpoint_matches"], true);
        assert_eq!(valid["machine"]["interaction_intersects_footprint"], true);

        let remote = inserter_machine_endpoint_verification(
            &route,
            Some(Position::new(40.5, 40.5)),
            Some(Direction::North),
            &lab,
            InserterMachineFlow::Input,
            "planned",
        );
        assert_eq!(remote["success"], false);
        assert_eq!(remote["inserter"]["route_endpoint_matches"], false);
        assert_eq!(remote["machine"]["interaction_intersects_footprint"], false);

        let admitted =
            attach_endpoint_preflight(serde_json::json!({"ready": true, "errors": []}), remote);
        assert_eq!(admitted["ready"], false);
        assert_eq!(admitted["endpoint_topology"]["success"], false);
    }

    #[test]
    fn output_endpoint_proof_requires_machine_pickup_and_route_start_drop() {
        let assembler = endpoint_test_machine("assembling-machine-1", "assembling-machine");
        let route = endpoint_test_route(GridPos::new(11, 8), GridPos::new(20, 8));
        let valid = inserter_machine_endpoint_verification(
            &route,
            Some(Position::new(11.5, 9.5)),
            Some(Direction::South),
            &assembler,
            InserterMachineFlow::Output,
            "persisted",
        );

        assert_eq!(valid["success"], true);
        assert_eq!(valid["flow"], "output_from_machine");
        assert_eq!(valid["route"]["required_endpoint"], "start");
        assert_eq!(valid["inserter"]["drop_tile"]["x"], 11);
        assert_eq!(valid["inserter"]["drop_tile"]["y"], 8);
        assert_eq!(valid["inserter"]["route_endpoint_matches"], true);
        assert_eq!(valid["machine"]["interaction_intersects_footprint"], true);

        let disconnected = inserter_machine_endpoint_verification(
            &endpoint_test_route(GridPos::new(12, 8), GridPos::new(20, 8)),
            Some(Position::new(11.5, 9.5)),
            Some(Direction::South),
            &assembler,
            InserterMachineFlow::Output,
            "persisted",
        );
        assert_eq!(disconnected["success"], false);
        assert_eq!(disconnected["inserter"]["route_endpoint_matches"], false);
        assert_eq!(
            disconnected["machine"]["interaction_intersects_footprint"],
            true
        );
    }

    #[test]
    fn recipe_rollback_restores_named_and_absent_recipes_distinctly() {
        assert_eq!(
            recipe_restore_action(Some("iron-gear-wheel")),
            RecipeRestoreAction::Set("iron-gear-wheel")
        );
        assert_eq!(recipe_restore_action(None), RecipeRestoreAction::Clear);
        assert_eq!(recipe_restore_action(Some("x")).operation(), "set");
        assert_eq!(recipe_restore_action(None).operation(), "clear");
    }

    #[test]
    fn rollback_retry_plan_retries_only_pending_exact_units_in_reverse_order() {
        let first_pass = rollback_retry_order(&[55, 56, 57, 58, 59, 60, 59]);
        assert_eq!(first_pass, vec![59, 60, 58, 57, 56, 55]);

        let removed_this_pass = HashSet::from([60, 56, 55]);
        let second_pass = rollback_pending_after_pass(&first_pass, &removed_this_pass);
        assert_eq!(second_pass, vec![59, 58, 57]);

        let removed_second_pass = HashSet::from([58, 57]);
        let final_pass = rollback_pending_after_pass(&second_pass, &removed_second_pass);
        assert_eq!(final_pass, vec![59]);
    }

    #[test]
    fn fuel_topology_requires_an_exact_durable_new_inserter_connection() {
        let manually_seeded = serde_json::json!({
            "consumers": [{
                "unit_number": 100,
                "fuel_connections": [{
                    "inserter_unit_number": 42,
                    "durable": false,
                    "source_durable": false,
                    "live": false,
                    "source": {"kind": "coal_belt", "operational": false},
                }],
                "proven_fuel_connections": [],
            }],
        });

        let non_durable = fuel_topology_verification(&manually_seeded, 100, Some(42));
        assert_eq!(non_durable["success"], false);
        assert_eq!(non_durable["exact_connection_present"], true);
        assert_eq!(non_durable["durable_connection_verified"], false);

        let durable = serde_json::json!({
            "consumers": [{
                "unit_number": 100,
                "fuel_connections": [{
                    "inserter_unit_number": 42,
                    "durable": true,
                    "source_durable": true,
                    "live": false,
                    "source": {"kind": "coal_belt", "operational": false},
                }],
                "proven_fuel_connections": [{
                    "inserter_unit_number": 42,
                    "durable": true,
                    "live": false,
                }],
            }],
        });
        let exact = fuel_topology_verification(&durable, 100, Some(42));
        assert_eq!(exact["success"], false);
        assert_eq!(exact["structural_success"], true);
        assert_eq!(exact["durable_connection_verified"], true);
        assert_eq!(exact["durable_connection_reported"], true);
        assert_eq!(exact["live_supply_verified"], false);

        let unrelated = fuel_topology_verification(&durable, 100, Some(99));
        assert_eq!(unrelated["success"], false);
        assert_eq!(unrelated["exact_connection_present"], false);
    }

    #[test]
    fn delivery_path_requires_the_exact_feeder_and_producer_to_remain_operational() {
        let operational = serde_json::json!({
            "connection": {
                "inserter_operational": true,
                "inserter_status": "working",
                "inserter_held_item": "coal",
                "source": {"producer_operational": true},
            },
        });
        assert!(fuel_delivery_path_operational(&operational));
        assert!(exact_fuel_feeder_transfer_observed(&operational));

        let idle = serde_json::json!({
            "connection": {
                "inserter_operational": true,
                "inserter_status": "waiting_for_source_items",
                "inserter_held_item": null,
                "source": {"producer_operational": true},
            },
        });
        assert!(fuel_delivery_path_operational(&idle));
        assert!(!exact_fuel_feeder_transfer_observed(&idle));

        let dead_feeder = serde_json::json!({
            "connection": {
                "inserter_operational": false,
                "inserter_status": "no_fuel",
                "source": {"producer_operational": true},
            },
        });
        assert!(!fuel_delivery_path_operational(&dead_feeder));
        assert!(!exact_fuel_feeder_transfer_observed(&dead_feeder));

        let wrong_item = serde_json::json!({
            "connection": {
                "inserter_operational": true,
                "inserter_status": "working",
                "inserter_held_item": "wood",
                "source": {"producer_operational": true},
            },
        });
        assert!(fuel_delivery_path_operational(&wrong_item));
        assert!(!exact_fuel_feeder_transfer_observed(&wrong_item));

        let dead_producer = serde_json::json!({
            "connection": {
                "inserter_operational": true,
                "source": {"producer_operational": false},
            },
        });
        assert!(!fuel_delivery_path_operational(&dead_producer));
    }

    #[test]
    fn fuel_delivery_budget_uses_route_travel_distance_not_entity_count() {
        let underground = serde_json::json!({
            "belt_count": 2,
            "topology": {"travel_distance_tiles": 4},
        });
        assert_eq!(fuel_delivery_wait_budget(&underground), (4, 760));

        let legacy_surface = serde_json::json!({"belt_count": 37});
        assert_eq!(fuel_delivery_wait_budget(&legacy_surface), (37, 2_080));

        let long_existing_trunk = serde_json::json!({
            "connection": {
                "source": {
                    "upstream_proof": {"hops": 100}
                }
            }
        });
        assert_eq!(fuel_topology_upstream_hops(&long_existing_trunk), 100);
        assert_eq!(fuel_delivery_budget_ticks(100), 4_600);
        let route_tiles = fuel_delivery_wait_budget(&legacy_surface).0;
        let proof_hops = fuel_topology_upstream_hops(&long_existing_trunk);
        assert_eq!(route_tiles.max(proof_hops), 100);
        assert_eq!(
            fuel_delivery_budget_ticks(route_tiles.max(proof_hops)),
            4_600
        );
    }

    #[test]
    fn fuel_transport_rejects_underground_before_planning_or_mutation() {
        assert!(unsupported_fuel_transport(false, false).is_none());
        let rejected = unsupported_fuel_transport(true, true).expect("unsupported transport");
        assert_eq!(rejected["success"], false);
        assert_eq!(rejected["error_kind"], "unsupported_fuel_transport");
        assert_eq!(rejected["dry_run"], true);
        assert_eq!(rejected["allow_underground"], true);
    }

    #[test]
    fn compound_preflight_reserves_entity_footprints_against_routes() {
        let belt = BeltPlacement {
            position: Position::new(4.5, 7.5),
            direction: Direction::East,
            kind: BeltKind::Surface,
        };
        let route = serde_json::json!({
            "success": true,
            "planned_belts": [belt.clone()],
            "planned_new_belts": [belt],
        });
        let available = BTreeMap::from([
            ("transport-belt".to_string(), 1),
            ("inserter".to_string(), 1),
        ]);
        let additional = BTreeMap::from([("inserter".to_string(), 1)]);
        let report = compound_route_preflight(
            &[("input", &route)],
            &available,
            &additional,
            "transport-belt",
            &HashSet::new(),
            &[("input_inserter", GridPos::new(4, 7))],
        );

        assert_eq!(report["ready"], false);
        assert!(report["errors"].as_array().is_some_and(|errors| {
            errors
                .iter()
                .any(|error| error["kind"] == "route_entity_overlap")
        }));
    }

    #[test]
    fn mcp_semantic_error_detection_ignores_appended_player_chat() {
        assert!(tool_text_indicates_error("Error: placement failed"));
        assert!(tool_text_indicates_error(
            r#"{"success":false,"error":"blocked"}"#
        ));
        assert!(!tool_text_indicates_error(
            "{\"success\":true,\"error\":null}\n\n--- Player Messages ---\n[giga]: Error: no"
        ));
    }

    #[test]
    fn semantic_validation_failures_are_structured_protocol_errors() {
        for failure in [
            invalid_direction_failure("direction", "sideways"),
            semantic_failure(
                "invalid_flow_reference",
                "provide source_unit_number or source_x/source_y",
            ),
        ] {
            let payload: serde_json::Value =
                serde_json::from_str(&failure).expect("semantic failure should be JSON");
            assert_eq!(payload["success"], false);
            assert!(payload["error_kind"].as_str().is_some());
            assert!(payload["error"].as_str().is_some());
            assert!(tool_text_indicates_error(&failure));

            let mut result =
                rmcp::model::CallToolResult::success(vec![rmcp::model::Content::text(failure)]);
            mark_semantic_tool_errors(&mut result);
            assert_eq!(result.is_error, Some(true));
        }
    }

    #[test]
    fn missing_route_rollback_identity_is_reported_without_coordinate_guessing() {
        let entity = Entity {
            unit_number: None,
            name: "transport-belt".to_string(),
            entity_type: Some("transport-belt".to_string()),
            position: Position::new(12.5, -4.5),
            direction: 4,
            health: None,
            force: Some("player".to_string()),
            bounding_box: None,
            pickup_position: None,
            drop_position: None,
            belt_to_ground_type: None,
            underground_belt_neighbour: None,
            belt_input_neighbours: Vec::new(),
            belt_output_neighbours: Vec::new(),
            belt_neighbours_observed: false,
        };

        let failure = rollback_missing_identity_error(&entity);
        assert_eq!(failure["unit_number"], serde_json::Value::Null);
        assert_eq!(failure["error_kind"], "missing_unit_number");
        assert!(failure["error"]
            .as_str()
            .is_some_and(|error| error.contains("coordinate removal was not attempted")));
    }

    #[test]
    fn model_tool_surface_is_the_exact_competent_gameplay_allowlist() {
        let server = FactorioMcp::new();
        let tools = server.tool_router.list_all();
        let mut visible: Vec<String> = tools.iter().map(|tool| tool.name.to_string()).collect();
        visible.sort();
        let expected: Vec<String> = MODEL_VISIBLE_TOOLS
            .iter()
            .map(|name| (*name).to_string())
            .collect();

        assert_eq!(visible, expected, "model tool surface must not drift");
        assert_eq!(visible.len(), 49);
        let schema_bytes = serde_json::to_vec(&tools)
            .expect("serialize tool schemas")
            .len();
        assert!(
            schema_bytes <= 60 * 1024,
            "model tool schemas grew to {schema_bytes} bytes"
        );

        let visible_set: HashSet<&str> = visible.iter().map(String::as_str).collect();
        for forbidden in [
            "execute_lua",
            "insert_items",
            "extract_items",
            "hand_feed_furnace",
            "place_character",
            "register_agent",
            "broadcast_thought",
            "create_zone",
            "clear_area",
        ] {
            assert!(
                !visible_set.contains(forbidden),
                "{forbidden} must never be model-visible"
            );
        }
        for required in [
            "bootstrap_burner_once",
            "collect_from_chest",
            "configure_inserter",
            "feed_lab_from_inventory",
            "file_issue",
            "get_entity_inventory",
            "wait_for_crafting",
        ] {
            assert!(
                visible_set.contains(required),
                "{required} must remain model-visible"
            );
        }
    }

    #[test]
    fn model_tool_surface_does_not_depend_on_raw_lua_environment() {
        let visible: HashSet<String> = FactorioMcp::new()
            .tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        assert_eq!(visible.len(), MODEL_VISIBLE_TOOLS.len());
        assert!(!visible.contains("execute_lua"));
    }

    #[test]
    fn visible_tool_schemas_never_reference_hidden_tool_names() {
        let registered = FactorioMcp::tool_router().list_all();
        let hidden: Vec<String> = registered
            .iter()
            .map(|tool| tool.name.to_string())
            .filter(|name| !MODEL_VISIBLE_TOOLS.contains(&name.as_str()))
            .collect();
        let visible = FactorioMcp::new().tool_router.list_all();
        let mut violations = Vec::new();

        for tool in visible {
            let schema = serde_json::to_string(&tool).expect("serialize visible tool schema");
            for hidden_name in &hidden {
                if schema.contains(hidden_name) {
                    violations.push(format!("{} references {hidden_name}", tool.name));
                }
            }
        }

        assert!(
            violations.is_empty(),
            "visible schemas reference unavailable tools: {}",
            violations.join(", ")
        );
    }

    #[test]
    fn model_safe_payload_relabels_hidden_operations_recursively() {
        let payload = serde_json::json!({
            "tool": "route_belt",
            "selected_build_fuel_supply_args": {"consumer_unit_number": 10},
            "steps": [{
                "tool": "insert_items",
                "description": "Use insert_items, then diagnose_fuel_sustainability"
            }, {
                "tool": "extract_items"
            }, {
                "tool": "wait_ticks"
            }],
        });

        let safe = model_safe_payload(payload);

        assert_emitted_tool_fields_are_model_visible(&safe);
        let encoded = serde_json::to_string(&safe).expect("serialize safe payload");
        for hidden in [
            "build_fuel_supply",
            "diagnose_fuel_sustainability",
            "insert_items",
            "extract_items",
            "wait_ticks",
        ] {
            assert!(
                !encoded.contains(hidden),
                "payload retained {hidden}: {safe}"
            );
        }
        assert_eq!(safe["steps"][0]["operation"], "load_inventory");
        assert_eq!(safe["steps"][1]["operation"], "collect_inventory");
        assert_eq!(safe["steps"][2]["operation"], "wait_for_process");
        assert!(safe.get("selected_durable_fuel_transaction_args").is_some());
    }

    #[test]
    fn automation_repair_hint_points_failed_automation_to_durable_repairs() {
        let verification = serde_json::json!({
            "success": true,
            "report": {
                "entities": [
                    {
                        "name": "inserter",
                        "unit_number": 11,
                        "status": "no_power",
                        "working": false
                    },
                    {
                        "name": "stone-furnace",
                        "unit_number": 12,
                        "status": "no_fuel",
                        "working": false
                    },
                    {
                        "name": "assembling-machine-1",
                        "unit_number": 13,
                        "status": "no_ingredients",
                        "working": false
                    },
                    {
                        "name": "assembling-machine-1",
                        "unit_number": 14,
                        "status": "waiting_for_space_in_destination",
                        "working": false
                    }
                ]
            }
        });
        let placed_statuses = vec![serde_json::json!({
            "unit_number": 11,
            "name": "inserter",
            "status": "no_power",
            "working": false,
            "ok": false,
        })];

        let hint = automation_repair_hint(
            "build_assembler_feed",
            "assembler input feed",
            true,
            &verification,
            &placed_statuses,
            Some(false),
        );
        let actions = hint["if_failed"]
            .as_array()
            .expect("repair actions should be an array");
        let tools: Vec<&str> = actions
            .iter()
            .filter_map(|action| action.get("tool").and_then(|tool| tool.as_str()))
            .collect();

        assert!(tools.contains(&"route_belt"));
        assert!(tools.contains(&"execute_entity_placement_near"));
        assert!(tools.contains(&"analyze_item_flow"));
        assert!(tools.contains(&"repair_fuel_sustainability"));
        assert!(tools.contains(&"build_assembler_feed"));
        assert!(tools.contains(&"build_assembler_output"));
        assert_emitted_tool_fields_are_model_visible(&hint);
        for unavailable in [
            "analyze_belt_gaps",
            "analyze_belt_reach",
            "build_fuel_supply",
            "diagnose_fuel_sustainability",
        ] {
            assert!(
                !serde_json::to_string(&hint)
                    .expect("serialize repair hint")
                    .contains(unavailable),
                "repair hint exposed {unavailable}"
            );
        }
        assert_eq!(
            hint["anti_pattern"],
            "Do not treat hand-feeding, hand-crafting, or inventory extraction as completion when automation_verified.success is false."
        );
    }

    #[test]
    fn execute_lua_refusal_blocks_unless_raw_lua_is_explicitly_enabled() {
        for env_value in [None, Some(""), Some("0"), Some("false")] {
            let refusal = execute_lua_refusal(env_value).expect("raw Lua should be refused");
            assert!(refusal.contains("disabled"), "{env_value:?}: {refusal}");
            assert!(
                refusal.contains("FACTORIOCTL_ALLOW_RAW_LUA"),
                "{env_value:?}: {refusal}"
            );
        }

        for env_value in [Some("1"), Some("true"), Some("yes"), Some("on")] {
            assert_eq!(execute_lua_refusal(env_value), None, "{env_value:?}");
        }
    }

    #[test]
    fn flow_lookup_requires_unit_or_complete_tile_pair() {
        assert!(matches!(
            flow_lookup(Some(42), None, None, "source").unwrap(),
            EntityLookup::Unit(42)
        ));
        assert!(matches!(
            flow_lookup(None, Some(1), Some(2), "target").unwrap(),
            EntityLookup::Tile(TilePos { x: 1, y: 2 })
        ));
        assert!(flow_lookup(Some(42), Some(1), Some(2), "source").is_err());
        assert!(flow_lookup(None, Some(1), None, "source").is_err());
        assert!(flow_lookup(None, None, None, "target").is_err());
    }

    #[test]
    fn flow_scan_area_covers_source_and_target_with_radius() {
        let area = flow_scan_area(TilePos::new(10, -2), TilePos::new(15, 4), 3);

        assert_eq!(area.left_top.x, 7.0);
        assert_eq!(area.left_top.y, -5.0);
        assert_eq!(area.right_bottom.x, 18.0);
        assert_eq!(area.right_bottom.y, 7.0);
    }

    #[test]
    fn machine_side_layout_returns_pickup_upstream_and_direction_pairs() {
        let entity = Entity {
            unit_number: Some(80),
            name: "assembling-machine-1".to_string(),
            entity_type: Some("assembling-machine".to_string()),
            position: Position::new(10.5, 20.5),
            direction: 0,
            health: None,
            force: Some("player".to_string()),
            bounding_box: Some(Area::new(9.0, 19.0, 12.0, 22.0)),
            pickup_position: None,
            drop_position: None,
            belt_to_ground_type: None,
            underground_belt_neighbour: None,
            belt_input_neighbours: Vec::new(),
            belt_output_neighbours: Vec::new(),
            belt_neighbours_observed: false,
        };

        let north = machine_side_layout(&entity, "north").expect("north side");
        assert_eq!(north.inserter_x, 10.5);
        assert_eq!(north.inserter_y, 18.5);
        assert_eq!(north.belt_x, 10);
        assert_eq!(north.belt_y, 17);
        assert_eq!(north.upstream_x, 10);
        assert_eq!(north.upstream_y, 16);
        assert_eq!(north.input_direction, "north");
        assert_eq!(north.output_direction, "south");

        let east = machine_side_layout(&entity, "east").expect("east side");
        assert_eq!(east.inserter_x, 12.5);
        assert_eq!(east.inserter_y, 20.5);
        assert_eq!(east.belt_x, 13);
        assert_eq!(east.belt_y, 20);
        assert_eq!(east.upstream_x, 14);
        assert_eq!(east.upstream_y, 20);
        assert_eq!(east.input_direction, "east");
        assert_eq!(east.output_direction, "west");

        let south = machine_side_layout(&entity, "south").expect("south side");
        assert_eq!(south.input_direction, "south");
        assert_eq!(south.output_direction, "north");

        let west = machine_side_layout(&entity, "west").expect("west side");
        assert_eq!(west.input_direction, "west");
        assert_eq!(west.output_direction, "east");

        let furnace = Entity {
            unit_number: Some(15),
            name: "stone-furnace".to_string(),
            entity_type: Some("furnace".to_string()),
            position: Position::new(42.0, -22.0),
            direction: 0,
            health: None,
            force: Some("player".to_string()),
            bounding_box: None,
            pickup_position: None,
            drop_position: None,
            belt_to_ground_type: None,
            underground_belt_neighbour: None,
            belt_input_neighbours: Vec::new(),
            belt_output_neighbours: Vec::new(),
            belt_neighbours_observed: false,
        };
        let furnace_north = machine_side_layout(&furnace, "north").expect("furnace north side");
        assert_eq!(furnace_north.inserter_x, 42.5);
        assert_eq!(furnace_north.inserter_y, -23.5);
        assert_eq!(furnace_north.belt_x, 42);
        assert_eq!(furnace_north.belt_y, -25);
        assert_eq!(furnace_north.output_direction, "south");

        let assembler_without_bbox = Entity {
            unit_number: Some(339),
            name: "assembling-machine-1".to_string(),
            entity_type: Some("assembling-machine".to_string()),
            position: Position::new(51.5, -14.5),
            direction: 0,
            health: None,
            force: Some("player".to_string()),
            bounding_box: None,
            pickup_position: None,
            drop_position: None,
            belt_to_ground_type: None,
            underground_belt_neighbour: None,
            belt_input_neighbours: Vec::new(),
            belt_output_neighbours: Vec::new(),
            belt_neighbours_observed: false,
        };
        let assembler_south =
            machine_side_layout(&assembler_without_bbox, "south").expect("assembler south side");
        assert_eq!(assembler_south.inserter_x, 51.5);
        assert_eq!(assembler_south.inserter_y, -12.5);
        assert_eq!(assembler_south.belt_x, 51);
        assert_eq!(assembler_south.belt_y, -12);
        assert_eq!(assembler_south.input_direction, "south");
    }

    #[test]
    fn route_segment_waypoint_limits_oversized_route_hints() {
        let waypoint = route_segment_waypoint(0, 0, 120, 0, 40);
        assert_eq!(waypoint, GridPos::new(40, 0));

        let diagonal = route_segment_waypoint(10, -10, 70, 50, 40);
        assert_eq!((diagonal.x - 10).abs() + (diagonal.y + 10).abs(), 40);

        let already_close = route_segment_waypoint(0, 0, 12, -3, 40);
        assert_eq!(already_close, GridPos::new(12, -3));
    }

    #[test]
    fn route_belt_failure_payload_carries_error_kind() {
        let params = RouteBeltParams {
            from_x: 73,
            from_y: -28,
            to_x: 51,
            to_y: -6,
            belt_type: "transport-belt".to_string(),
            search_radius: 10,
            dry_run: false,
            respect_zones: false,
            allow_underground: false,
            extend_existing: true,
        };

        let payload = route_belt_failure_json(&params, "route_failed", "Route failed: blocked");

        assert_eq!(payload["success"], false);
        assert_eq!(payload["error_kind"], "route_failed");
        assert_eq!(payload["error"], "Route failed: blocked");
        assert_eq!(payload["from"]["x"], 73);
        assert_eq!(payload["to"]["y"], -6);
        assert_eq!(payload["belt_type"], "transport-belt");
    }

    #[test]
    fn placement_preflight_uses_live_policy_not_factorio_permission_alone() {
        assert!(!placement_report_allowed(&serde_json::json!({
            "factorio_allowed": true,
            "policy_allowed": false,
            "allowed": false,
        })));
        assert!(placement_report_allowed(&serde_json::json!({
            "factorio_allowed": true,
            "policy_allowed": true,
            "allowed": true,
        })));
        assert!(!placement_report_allowed(&serde_json::json!({
            "factorio_allowed": true,
        })));
    }

    #[test]
    fn route_materials_are_all_or_nothing() {
        assert_eq!(
            route_material_shortfall("transport-belt", 12, 12, Some("underground-belt"), 2, 2,),
            None
        );
        assert_eq!(
            route_material_shortfall(
                "transport-belt",
                12,
                4,
                Some("underground-belt"),
                2,
                0,
            )
            .as_deref(),
            Some(
                "Insufficient materials for complete route: need 12 transport-belt, have 4; need 2 underground-belt, have 0. No belts were placed."
            )
        );
    }

    #[test]
    fn existing_route_belts_require_exact_type_kind_and_direction() {
        let entity = Entity {
            unit_number: Some(41),
            name: "transport-belt".to_string(),
            entity_type: Some("transport-belt".to_string()),
            position: Position::new(2.5, 3.5),
            direction: Direction::East.to_factorio(),
            health: Some(150.0),
            force: Some("player".to_string()),
            bounding_box: None,
            pickup_position: None,
            drop_position: None,
            belt_to_ground_type: None,
            underground_belt_neighbour: None,
            belt_input_neighbours: Vec::new(),
            belt_output_neighbours: Vec::new(),
            belt_neighbours_observed: false,
        };
        let east_surface = BeltPlacement {
            position: entity.position,
            direction: Direction::East,
            kind: BeltKind::Surface,
        };
        assert!(existing_belt_compatibility(&entity, &east_surface, "transport-belt").is_ok());

        let wrong_direction = BeltPlacement {
            direction: Direction::North,
            ..east_surface.clone()
        };
        assert!(
            existing_belt_compatibility(&entity, &wrong_direction, "transport-belt")
                .unwrap_err()
                .contains("faces east")
        );

        let underground = BeltPlacement {
            kind: BeltKind::UndergroundEntry,
            ..east_surface
        };
        assert!(existing_belt_compatibility(&entity, &underground, "transport-belt").is_err());
        assert!(existing_belt_compatibility(&entity, &underground, "fast-transport-belt").is_err());
    }

    #[test]
    fn existing_underground_pair_reserves_only_internal_endpoint_tiles() {
        let make_endpoint = |unit_number, x: f64, mode: &str, neighbour_x: f64| Entity {
            unit_number: Some(unit_number),
            name: "underground-belt".to_string(),
            entity_type: Some("underground-belt".to_string()),
            position: Position::new(x, 52.5),
            direction: Direction::East.to_factorio(),
            health: Some(150.0),
            force: Some("player".to_string()),
            bounding_box: None,
            pickup_position: None,
            drop_position: None,
            belt_to_ground_type: Some(mode.to_string()),
            underground_belt_neighbour: Some(Position::new(neighbour_x, 52.5)),
            belt_input_neighbours: Vec::new(),
            belt_output_neighbours: Vec::new(),
            belt_neighbours_observed: true,
        };
        let entities = vec![
            make_endpoint(1496, -43.5, "input", -39.5),
            make_endpoint(1497, -39.5, "output", -43.5),
        ];

        let (reserved, pairs) =
            existing_underground_pair_reservations(&entities, "underground-belt");

        assert_eq!(pairs.len(), 1);
        assert_eq!(reserved.len(), 3);
        assert!(reserved.contains(&GridPos::new(-43, 52)));
        assert!(reserved.contains(&GridPos::new(-42, 52)));
        assert!(reserved.contains(&GridPos::new(-41, 52)));
        assert!(!reserved.contains(&GridPos::new(-44, 52)));
        assert!(!reserved.contains(&GridPos::new(-40, 52)));
    }

    #[test]
    fn direct_placement_allows_splitters_but_keeps_belt_tiles_route_only() {
        for belt in [
            "transport-belt",
            "fast-transport-belt",
            "underground-belt",
            "express-underground-belt",
        ] {
            assert!(direct_placement_requires_route(belt), "{belt}");
        }
        for splitter in [
            "splitter",
            "fast-splitter",
            "express-splitter",
            "turbo-splitter",
        ] {
            assert!(!direct_placement_requires_route(splitter), "{splitter}");
            assert!(is_existing_belt_entity(splitter), "{splitter}");
        }
    }

    #[test]
    fn source_tap_layouts_pick_from_exact_source_and_start_independent_branch() {
        let source = GridPos::new(-45, 44);
        let layouts = belt_source_tap_layouts(source);
        assert_eq!(layouts.len(), 4);
        for layout in layouts {
            assert_eq!(layout.source_tile, source);
            assert_eq!(layout.pickup_tile, source);
            assert_eq!(layout.inserter_tile, source.offset(layout.outward, 1));
            assert_eq!(layout.drop_tile, source.offset(layout.outward, 2));
            assert_eq!(layout.inserter_direction, layout.outward.opposite());
            assert_eq!(
                GridPos::from_position(&layout.inserter_position()),
                layout.inserter_tile
            );
        }

        let west = layouts
            .into_iter()
            .find(|layout| layout.outward == Direction::West)
            .expect("west tap");
        assert_eq!(west.inserter_tile, GridPos::new(-46, 44));
        assert_eq!(west.drop_tile, GridPos::new(-47, 44));
        assert_eq!(west.inserter_direction, Direction::East);
    }

    #[test]
    fn fuel_controller_never_surfaces_rotation_for_existing_source_endpoint() {
        let route = serde_json::json!({
            "success": false,
            "error_kind": "incompatible_existing_belt",
            "endpoint_incompatibility": {
                "endpoint_kind": "start",
                "unit_number": 252,
                "actual_direction": "south",
                "required_direction": "north",
            },
            "next_action": {
                "tool": "rotate_entity",
                "args": {"unit_number": 252, "direction": "north"},
            },
        });

        let protected = fuel_route_protects_existing_source(route);
        assert_eq!(protected["success"], false);
        assert_eq!(protected["error_kind"], "protected_fuel_source_endpoint");
        assert!(protected.get("next_action").is_none());
        assert_eq!(protected["endpoint_incompatibility"]["unit_number"], 252);
    }

    #[test]
    fn source_tap_filter_requires_exact_verified_coal_whitelist() {
        let exact = serde_json::json!({
            "success": true,
            "atomic_with_placement": true,
            "readback_verified": true,
            "filtering_enabled": true,
            "mode": "whitelist",
            "filters": [{"slot": 1, "name": "coal"}],
        });
        assert!(source_tap_filter_verified(&exact));

        for invalid in [
            serde_json::json!({
                "success": true,
                "atomic_with_placement": false,
                "readback_verified": true,
                "filtering_enabled": true,
                "mode": "whitelist",
                "filters": [{"slot": 1, "name": "coal"}],
            }),
            serde_json::json!({
                "success": true,
                "atomic_with_placement": true,
                "readback_verified": true,
                "filtering_enabled": true,
                "mode": "whitelist",
                "filters": [],
            }),
            serde_json::json!({
                "success": true,
                "atomic_with_placement": true,
                "readback_verified": true,
                "filtering_enabled": true,
                "mode": "whitelist",
                "filters": [{"slot": 1, "name": "copper-plate"}],
            }),
            serde_json::json!({
                "success": true,
                "atomic_with_placement": true,
                "readback_verified": false,
                "filtering_enabled": true,
                "mode": "whitelist",
                "filters": [{"slot": 1, "name": "coal"}],
            }),
        ] {
            assert!(!source_tap_filter_verified(&invalid));
        }
    }

    #[test]
    fn atomic_filtered_placement_extracts_nested_failure_identity() {
        let report = serde_json::json!({
            "success": false,
            "placement": {"unit_number": 812},
            "rollback": {
                "success": true,
                "entity_removed": true,
                "item_returned": 1,
            },
        });

        assert_eq!(atomic_filtered_placement_unit(&report), Some(812));
        assert!(atomic_filtered_placement_local_rollback_verified(&report));
        assert_eq!(atomic_filtered_placement_cleanup_unit(&report), None);
    }

    #[test]
    fn failed_lua_entity_removal_schedules_exact_best_effort_cleanup() {
        let report = serde_json::json!({
            "success": false,
            "placement": {"unit_number": 813},
            "rollback": {
                "success": false,
                "entity_removed": false,
                "item_returned": 0,
            },
        });
        let host_rollback = serde_json::json!({
            "success": true,
            "infrastructure": {
                "units": {"removed_units": [813]},
            },
        });

        assert_eq!(atomic_filtered_placement_cleanup_unit(&report), Some(813));
        let evidence = atomic_filtered_placement_cleanup_evidence(Some(&report), &host_rollback);
        assert_eq!(evidence["host_cleanup_attempted"], true);
        assert_eq!(evidence["host_exact_unit_removed"], true);
        assert_eq!(evidence["local_rollback_verified"], false);
        assert_eq!(evidence["success"], false);
    }

    #[test]
    fn failed_lua_item_return_cannot_be_re_reported_as_host_success() {
        let report = serde_json::json!({
            "success": false,
            "placement": {"unit_number": 814},
            "rollback": {
                "success": false,
                "entity_removed": true,
                "item_returned": 0,
            },
        });
        let otherwise_successful_rollback = serde_json::json!({
            "success": true,
            "infrastructure": {
                "units": {"removed_units": []},
            },
        });

        assert_eq!(atomic_filtered_placement_cleanup_unit(&report), None);
        let evidence = atomic_filtered_placement_cleanup_evidence(
            Some(&report),
            &otherwise_successful_rollback,
        );
        assert_eq!(evidence["local_rollback_verified"], false);
        assert_eq!(evidence["success"], false);
    }

    #[test]
    fn rust_verifier_mismatch_requires_exact_host_removal() {
        let report = serde_json::json!({
            "success": true,
            "unit_number": 815,
            "filter": {"success": true, "filters": []},
        });
        let removed = serde_json::json!({
            "success": true,
            "infrastructure": {
                "units": {"removed_units": [815]},
            },
        });
        let not_removed = serde_json::json!({
            "success": true,
            "infrastructure": {
                "units": {"removed_units": []},
            },
        });

        assert_eq!(atomic_filtered_placement_cleanup_unit(&report), Some(815));
        assert_eq!(
            atomic_filtered_placement_cleanup_evidence(Some(&report), &removed)["success"],
            true
        );
        assert_eq!(
            atomic_filtered_placement_cleanup_evidence(Some(&report), &not_removed)["success"],
            false
        );
    }

    #[test]
    fn atomic_placement_cleanup_distinguishes_no_mutation_from_unknown_outcome() {
        let rejected_before_placement = serde_json::json!({
            "success": false,
            "error": "Cannot place entity",
            "atomic_outcome_known": true,
            "entity_created": false,
        });
        let successful_known_rollback = serde_json::json!({
            "success": true,
            "infrastructure": {
                "units": {"removed_units": []},
            },
        });

        let no_mutation = atomic_filtered_placement_cleanup_evidence(
            Some(&rejected_before_placement),
            &successful_known_rollback,
        );
        assert_eq!(no_mutation["no_entity_created"], true);
        assert_eq!(no_mutation["success"], true);

        let unmarked_lua_error = serde_json::json!({
            "success": false,
            "error_kind": "lua_error",
            "error": "unexpected exception",
        });
        let unmarked = atomic_filtered_placement_cleanup_evidence(
            Some(&unmarked_lua_error),
            &successful_known_rollback,
        );
        assert_eq!(unmarked["outcome_known"], false);
        assert_eq!(unmarked["no_entity_created"], false);
        assert_eq!(unmarked["success"], false);

        let unknown = atomic_filtered_placement_cleanup_evidence(None, &successful_known_rollback);
        assert_eq!(unknown["outcome_known"], false);
        assert_eq!(unknown["success"], false);

        let claimed_success_without_identity = serde_json::json!({"success": true});
        let missing_identity = atomic_filtered_placement_cleanup_evidence(
            Some(&claimed_success_without_identity),
            &successful_known_rollback,
        );
        assert_eq!(missing_identity["success"], false);
    }

    #[test]
    fn source_tap_requires_exact_source_unit_tile_and_direction_to_survive() {
        let before = Entity {
            unit_number: Some(252),
            name: "transport-belt".to_string(),
            entity_type: Some("transport-belt".to_string()),
            position: Position::new(-44.5, 44.5),
            direction: Direction::South.to_factorio(),
            health: Some(150.0),
            force: Some("player".to_string()),
            bounding_box: None,
            pickup_position: None,
            drop_position: None,
            belt_to_ground_type: None,
            underground_belt_neighbour: None,
            belt_input_neighbours: Vec::new(),
            belt_output_neighbours: Vec::new(),
            belt_neighbours_observed: false,
        };
        let preserved = source_entity_preservation(&before, Some(&before), GridPos::new(-45, 44));
        assert_eq!(preserved["success"], true);

        let mut rotated = before.clone();
        rotated.direction = Direction::North.to_factorio();
        let changed = source_entity_preservation(&before, Some(&rotated), GridPos::new(-45, 44));
        assert_eq!(changed["success"], false);
        assert_eq!(changed["unit_matches"], true);
        assert_eq!(changed["direction_matches"], false);
    }

    #[test]
    fn source_tap_plan_prefers_complete_compound_preflight_then_shortest_route() {
        let route = |belts| {
            serde_json::json!({
                "success": true,
                "new_belt_count": belts,
                "topology": {"connected": true},
            })
        };
        let preflight = |ready| {
            serde_json::json!({
                "ready": ready,
                "placements": {
                    "source_tap_inserter": {"allowed": true},
                    "fuel_inserter": {"allowed": true},
                },
            })
        };
        assert!(
            source_tap_plan_rank(&route(8), &preflight(true), 1)
                < source_tap_plan_rank(&route(3), &preflight(false), 0)
        );
        assert!(
            source_tap_plan_rank(&route(3), &preflight(true), 1)
                < source_tap_plan_rank(&route(8), &preflight(true), 0)
        );

        let material_shortfall = serde_json::json!({
            "ready": false,
            "placements": {
                "source_tap_inserter": {"allowed": true},
                "fuel_inserter": {"allowed": true},
            },
            "routes": {"errors": [
                {"kind": "insufficient_materials", "item": "transport-belt"}
            ]},
        });
        let self_crossing = serde_json::json!({
            "ready": false,
            "placements": {
                "source_tap_inserter": {"allowed": true},
                "fuel_inserter": {"allowed": true},
            },
            "routes": {"errors": [
                {"kind": "insufficient_materials", "item": "transport-belt"},
                {"kind": "route_entity_overlap", "entity": "source_tap_inserter"}
            ]},
        });
        assert!(
            source_tap_plan_rank(&route(8), &material_shortfall, 1)
                < source_tap_plan_rank(&route(3), &self_crossing, 0),
            "temporary material shortages must not make an impossible self-crossing tap rank first"
        );
    }

    #[test]
    fn route_belt_core_has_no_partial_prefix_path() {
        let source = include_str!("mcp.rs");
        let route_core = source
            .rsplit("    async fn route_belt_core(")
            .next()
            .and_then(|tail| tail.split("\n    async fn route_belt(").next())
            .expect("route_belt_core should exist before route_belt");

        for forbidden in [
            "partial_route",
            "partial_reason",
            "partial_route_available",
            "buildable prefix",
            "places the buildable prefix",
            "remove_entity_at(",
            "resource_endpoint_reserved",
            "resource_tiles_reserved",
            "preserves_resource_patches",
        ] {
            assert!(
                !route_core.contains(forbidden),
                "route_belt_core must not retain partial mutation path {forbidden:?}"
            );
        }
        for required in [
            "check_entity_placement",
            "Complete route preflight failed; no belts were placed.",
            "rollback_exact_units(client, &placed_unit_numbers).await",
            ".map(rollback_missing_identity_error)",
            "\"missing_identity_errors\"",
            "\"complete_route\": true",
            "\"ready_to_call\"",
            "\"disconnected_topology\"",
            "\"resource_tiles_observed\"",
            "\"planned_surface_resource_tiles_crossed\"",
            "for tile in reserved_route_tiles",
            "collision_map.block(*tile)",
        ] {
            assert!(
                route_core.contains(required),
                "route_belt_core should retain atomic-route invariant {required:?}"
            );
        }
        assert!(
            !route_core.contains("client.remove_entity(unit_number).await"),
            "route_belt_core must use the retrying exact-unit rollback helper instead of a one-pass removal loop"
        );
        let resource_observation = route_core
            .split("let mut resource_tiles = HashSet::new();")
            .nth(1)
            .and_then(|tail| tail.split("let mut existing_surface_belts").next())
            .expect("route_belt_core should record live resource tiles");
        assert!(
            !resource_observation.contains("collision_map.block"),
            "live resource observations must not become A* collision blockers"
        );

        let existing_belt_policy = route_core
            .split("let mut existing_surface_belts")
            .nth(1)
            .and_then(|tail| tail.split("let start = GridPos").next())
            .expect("route_belt_core should classify existing surface belts before A*");
        for required in [
            "if params.extend_existing",
            "collision_map.unblock(tile)",
            "existing_surface_belts.insert(tile, entity)",
            "collision_map.block(tile)",
        ] {
            assert!(
                existing_belt_policy.contains(required),
                "independent routes must reserve existing belt build space: missing {required:?}"
            );
        }
        let dry_run = route_core
            .find("if params.dry_run")
            .expect("route_belt_core should retain dry-run response");
        let placement_preflight = route_core
            .find("check_entity_placement")
            .expect("route_belt_core should preflight every planned placement");
        assert!(
            placement_preflight < dry_run,
            "dry-run must execute the same placement preflight before claiming readiness"
        );
        let topology_gate = route_core
            .find("\"disconnected_topology\"")
            .expect("route_belt_core should reject disconnected topology");
        let inventory_read = route_core
            .find("character_inventory()")
            .expect("route_belt_core should retain inventory preflight");
        assert!(
            topology_gate < inventory_read,
            "disconnected topology must fail before inventory reads or any placement path"
        );
    }

    #[test]
    fn assembler_feed_reserves_inserter_tile_for_plan_and_execution() {
        let source = include_str!("mcp.rs");
        let controller = source
            .rsplit("    async fn build_assembler_feed(")
            .next()
            .and_then(|tail| tail.split("    async fn plan_machine_output(").next())
            .expect("build_assembler_feed should precede plan_machine_output");

        assert_eq!(
            controller.matches(".route_belt_core_avoiding(").count(),
            2,
            "assembler-feed planning and execution must use the same reserved route geometry"
        );
        for required in [
            "let reserved_route_tiles = HashSet::from([GridPos::from_position(&inserter_position)])",
            "report.remove(\"ready_to_call\")",
            "\"controller_reserved_tiles\"",
            "\"tool\": \"build_assembler_feed\"",
            "\"args\": execute_args.clone()",
            "\"ready_to_call\": ready_to_call",
        ] {
            assert!(
                controller.contains(required),
                "assembler-feed executable dry-run should include {required:?}"
            );
        }
    }

    #[test]
    fn fuel_supply_tool_boundaries_structure_core_errors() {
        let source = include_str!("mcp.rs");
        assert_eq!(
            source
                .matches(".with_player_messages(semantic_failure(\"fuel_supply_failed\", e))")
                .count(),
            2,
            "both build and repair tool boundaries must convert core errors into protocol errors"
        );
    }

    #[test]
    fn machine_output_controller_accepts_furnaces_and_assemblers() {
        let assembler = Entity {
            unit_number: Some(1),
            name: "assembling-machine-1".to_string(),
            entity_type: Some("assembling-machine".to_string()),
            position: Position::new(0.5, 0.5),
            direction: 0,
            health: None,
            force: Some("player".to_string()),
            bounding_box: None,
            pickup_position: None,
            drop_position: None,
            belt_to_ground_type: None,
            underground_belt_neighbour: None,
            belt_input_neighbours: Vec::new(),
            belt_output_neighbours: Vec::new(),
            belt_neighbours_observed: false,
        };
        let furnace = Entity {
            unit_number: Some(2),
            name: "stone-furnace".to_string(),
            entity_type: Some("furnace".to_string()),
            position: Position::new(2.0, 2.0),
            direction: 0,
            health: None,
            force: Some("player".to_string()),
            bounding_box: None,
            pickup_position: None,
            drop_position: None,
            belt_to_ground_type: None,
            underground_belt_neighbour: None,
            belt_input_neighbours: Vec::new(),
            belt_output_neighbours: Vec::new(),
            belt_neighbours_observed: false,
        };
        let belt = Entity {
            unit_number: Some(3),
            name: "transport-belt".to_string(),
            entity_type: Some("transport-belt".to_string()),
            position: Position::new(3.5, 3.5),
            direction: 0,
            health: None,
            force: Some("player".to_string()),
            bounding_box: None,
            pickup_position: None,
            drop_position: None,
            belt_to_ground_type: None,
            underground_belt_neighbour: None,
            belt_input_neighbours: Vec::new(),
            belt_output_neighbours: Vec::new(),
            belt_neighbours_observed: false,
        };

        assert!(is_machine_output_source(&assembler));
        assert!(is_machine_output_source(&furnace));
        assert!(!is_machine_output_source(&belt));
    }

    #[test]
    fn ready_fuel_supply_args_prefers_ranked_consumer_ready_to_call() {
        let report = serde_json::json!({
            "consumers": [{
                "unit_number": 49,
                "ready_to_call": {
                    "tool": "repair_fuel_sustainability",
                    "args": {"x": 46.5, "y": 10.5, "radius": 64},
                    "transaction_args": {
                        "consumer_unit_number": 49,
                        "source_unit_number": 49,
                        "from_x": 78,
                        "from_y": -20,
                        "pickup_x": 46,
                        "pickup_y": 11,
                        "inserter_x": 46.5,
                        "inserter_y": 10.5,
                        "inserter_direction": "north",
                        "inserter_name": "burner-inserter",
                        "provisional_source_unit_number": 49,
                        "bootstrap_consumer_fuel_count": 5
                    }
                }
            }],
            "suggested_actions": []
        });

        let args = ready_fuel_supply_args(&report).expect("ready args");

        assert_eq!(args.consumer_unit_number, 49);
        assert_eq!(args.source_unit_number, 49);
        assert_eq!(args.from_x, 78);
        assert_eq!(args.pickup_y, 11);
        assert_eq!(args.inserter_name, "burner-inserter");
        assert_eq!(args.inserter_fuel_count, 5);
        assert_eq!(args.provisional_source_unit_number, Some(49));
        assert_eq!(args.bootstrap_consumer_fuel_count, 5);
        assert_eq!(args.belt_type, "transport-belt");
        assert!(args.extend_existing);
    }

    #[test]
    fn compact_fuel_diagnosis_removes_repeated_internal_proof_graphs() {
        let proof = serde_json::json!({
            "reason": "burner_coal_drill_fuel_not_durable",
            "fuel_proof": {
                "reason": "manual_burner_fuel_buffer",
                "huge_internal_trace": "x".repeat(8_000),
            },
        });
        let source = serde_json::json!({
            "kind": "coal_drill",
            "unit_number": 206,
            "name": "burner-mining-drill",
            "position": {"x": -54.0, "y": 51.0},
            "route_tile": {"x": -52, "y": 50},
            "durable": false,
            "operational": false,
            "self_bootstrap_capable": true,
            "upstream_proof": proof,
        });
        let consumer = serde_json::json!({
            "unit_number": 206,
            "name": "burner-mining-drill",
            "type": "mining-drill",
            "position": {"x": -54.0, "y": 51.0},
            "status": "no_fuel",
            "fuel_count": 0,
            "candidate_sources": vec![source.clone(); 8],
            "fuel_connections": [],
            "proven_fuel_connections": [],
            "ready_to_call": {
                "tool": "repair_fuel_sustainability",
                "transaction_args": {
                    "consumer_unit_number": 206,
                    "source_unit_number": 206,
                    "from_x": -52,
                    "from_y": 50,
                    "pickup_x": -54,
                    "pickup_y": 49,
                    "inserter_x": -53.5,
                    "inserter_y": 49.5,
                    "inserter_direction": "north",
                    "provisional_source_unit_number": 206,
                    "bootstrap_consumer_fuel_count": 5,
                },
            },
        });
        let report = serde_json::json!({
            "area": {"left_top": {"x": -64, "y": 40}, "right_bottom": {"x": -40, "y": 64}},
            "consumer_count": 7,
            "consumers": vec![consumer; 7],
            "coal_sources": {
                "mining_drills": vec![source.clone(); 8],
                "belts": vec![source.clone(); 8],
                "chests": vec![source; 8],
                "resource_tiles": 16,
            },
            "suggested_actions": [{
                "type": "repair_fuel_sustainability",
                "tool": "repair_fuel_sustainability",
                "target_unit_number": 206,
                "description": "close the loop",
                "connections": ["x".repeat(8_000)],
            }],
            "truncated": false,
        });

        let compact = compact_fuel_diagnosis(&report);
        let encoded = serde_json::to_string(&compact).expect("compact diagnosis JSON");

        assert!(
            encoded.len() < 12_000,
            "compact response was {} bytes",
            encoded.len()
        );
        assert!(!encoded.contains("huge_internal_trace"));
        assert!(!encoded.contains("fuel_proof"));
        assert_eq!(compact["consumers"][0]["unit_number"], 206);
        assert_eq!(
            compact["consumers"][0]["candidate_sources"][0]["upstream_reason"],
            "burner_coal_drill_fuel_not_durable"
        );
        assert_eq!(compact["coal_sources"]["mining_drills_count"], 8);
        assert_eq!(
            compact["suggested_actions"][0]["description"],
            "close the loop"
        );
    }

    #[test]
    fn fuel_payload_projection_is_bounded_and_preserves_exact_repair_evidence() {
        let bulk: Vec<_> = (0..2_000)
            .map(|unit| {
                serde_json::json!({
                    "unit_number": unit,
                    "name": "transport-belt",
                    "position": {"x": unit, "y": unit},
                    "trace": "x".repeat(128),
                })
            })
            .collect();
        let proof = serde_json::json!({
            "reason": "closed_self_sustaining_coal_cycle",
            "trace": "p".repeat(120_000),
            "nested": {"reason": "producer_operational"},
        });

        for success in [true, false] {
            let raw = serde_json::json!({
                "success": success,
                "error_kind": if success { serde_json::Value::Null } else { serde_json::json!("fuel_supply_not_live") },
                "route": {
                    "success": success,
                    "complete_route": success,
                    "belt_count": 2_000,
                    "placed_entities": bulk.clone(),
                    "planned_belts": bulk.clone(),
                    "planned_new_belts": bulk.clone(),
                    "endpoint_incompatibility": {
                        "endpoint_kind": "goal",
                        "unit_number": 991,
                        "required_direction": "east",
                    },
                    "next_action": {
                        "tool": "rotate_entity",
                        "args": {"unit_number": 991, "direction": "east"},
                        "after_success": {"tool": "repair_fuel_sustainability", "args": {"dry_run": true}},
                    },
                },
                "infrastructure_verified": {
                    "success": success,
                    "route": {"success": true, "complete_route": true, "topology_connected": true},
                    "inserter": {"expected_unit_number": 992, "actual_unit_number": 992},
                    "delivery_observation": {
                        "success": success,
                        "scope": "exact_terminal_or_filtered_feeder_transfer",
                        "terminal_coal_observed": false,
                        "exact_feeder_transfer_observed": success,
                        "delivery_path_operational": success,
                        "route_transit_tiles": 2_400,
                        "waited_ticks": 1_200,
                        "budget_ticks": 96_600,
                    },
                    "durable_fuel_topology": {
                        "success": success,
                        "structural_success": true,
                        "consumer_unit_number": 990,
                        "inserter_unit_number": 992,
                        "exact_connection_present": true,
                        "durable_connection_verified": true,
                        "live_supply_verified": success,
                        "connection": {
                            "connection_kind": "belt_inserter_fuel",
                            "inserter_unit_number": 992,
                            "durable": true,
                            "live": success,
                            "source": {
                                "kind": "coal_drill",
                                "unit_number": 990,
                                "upstream_proof": proof,
                            },
                        },
                    },
                },
                "verification": {
                    "success": success,
                    "proof": "currently_working",
                    "entities": bulk.clone(),
                    "report": {"entities": bulk.clone()},
                },
                "rollback": {
                    "success": true,
                    "transaction_fuel_cleared": true,
                    "consumer_state": {
                        "success": true,
                        "consumer_state_restored": true,
                        "feeder_quiesced": true,
                        "before": {"unit_number": 990, "fuel_total": 5, "cold": false},
                        "expected": {"unit_number": 990, "fuel_total": 0, "cold": true},
                        "after": {"unit_number": 990, "fuel_total": 0, "cold": true},
                    },
                    "infrastructure": {
                        "success": true,
                        "units": {
                            "success": true,
                            "removed_units": (0..2_000).collect::<Vec<_>>(),
                            "attempts": bulk.clone(),
                        },
                    },
                },
            });
            let raw_size = serde_json::to_string_pretty(&raw).unwrap().len();
            assert!(
                raw_size > 100_000,
                "fixture must exercise an oversized payload"
            );

            let compact = compact_fuel_repair(&raw);
            let encoded = serde_json::to_string_pretty(&compact).unwrap();
            assert!(
                encoded.len() < 65_536,
                "bounded fuel payload was {} bytes",
                encoded.len()
            );
            assert_eq!(compact["success"], success);
            assert_eq!(compact["route"]["next_action"]["args"]["unit_number"], 991);
            assert_eq!(
                compact["infrastructure_verified"]["durable_fuel_topology"]["consumer_unit_number"],
                990
            );
            assert_eq!(
                compact["infrastructure_verified"]["durable_fuel_topology"]["inserter_unit_number"],
                992
            );
            assert_eq!(
                compact["infrastructure_verified"]["delivery_observation"]["success"],
                success
            );
            assert_eq!(
                compact["infrastructure_verified"]["delivery_observation"]["route_transit_tiles"],
                2_400
            );
            assert!(
                compact["infrastructure_verified"]["durable_fuel_topology"]["proof_reasons"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|reason| reason == "closed_self_sustaining_coal_cycle")
            );
            for forbidden in [
                "placed_entities",
                "planned_belts",
                "planned_new_belts",
                "huge_internal_trace",
                "\"entities\"",
                "\"report\"",
                "\"attempts\"",
            ] {
                assert!(
                    !encoded.contains(forbidden),
                    "compact payload retained {forbidden}"
                );
            }
        }
    }

    #[test]
    fn compact_fuel_repair_preserves_atomic_failure_and_cleanup_evidence() {
        let raw = serde_json::json!({
            "success": false,
            "error_kind": "atomic_fuel_inserter_placement_failed",
            "atomic_placement": {
                "success": false,
                "error_kind": "atomic_filter_configuration_failed",
                "error": "duplicate item",
                "placement": {
                    "unit_number": 812,
                    "name": "burner-inserter",
                    "position": {"x": 4.5, "y": 8.5},
                },
                "filter": {
                    "success": false,
                    "error_kind": "invalid_allowed_items",
                    "error": "duplicate item",
                },
                "rollback": {
                    "success": false,
                    "entity_removed": true,
                    "item_returned": 0,
                },
            },
            "rollback": {
                "success": false,
                "atomic_cleanup": {
                    "success": false,
                    "outcome_known": true,
                    "remote_success": false,
                    "unit_number": 812,
                    "local_rollback_verified": false,
                    "host_cleanup_attempted": false,
                    "host_exact_unit_removed": false,
                },
            },
        });

        let compact = compact_fuel_repair(&raw);
        assert_eq!(compact["atomic_placement"]["placement"]["unit_number"], 812);
        assert_eq!(compact["atomic_placement"]["rollback"]["item_returned"], 0);
        assert_eq!(
            compact["rollback"]["atomic_cleanup"]["local_rollback_verified"],
            false
        );
        assert_eq!(compact["rollback"]["success"], false);
    }

    #[test]
    fn endpoint_conflict_prefers_goal() {
        let start = GridPos::new(1, 2);
        let goal = GridPos::new(9, 2);
        let conflicts = vec![
            (
                start,
                serde_json::json!({"unit_number": 40, "required_direction": "west"}),
            ),
            (
                goal,
                serde_json::json!({"unit_number": 41, "required_direction": "east"}),
            ),
        ];
        let selected = endpoint_belt_incompatibility(&conflicts, start, goal).unwrap();
        assert_eq!(selected["endpoint_kind"], "goal");
        assert_eq!(selected["unit_number"], 41);
        assert_eq!(selected["required_direction"], "east");
    }

    #[test]
    fn build_fuel_supply_params_require_source_identity_and_accept_factorio_centers() {
        let args: BuildFuelSupplyParams = serde_json::from_value(serde_json::json!({
            "consumer_unit_number": 49,
            "source_unit_number": 87,
            "from_x": 73.5,
            "from_y": -27.5,
            "pickup_x": 54.5,
            "pickup_y": -9.5,
            "inserter_x": 54.5,
            "inserter_y": -8.5,
            "inserter_direction": "north"
        }))
        .expect("half-tile Factorio centers should deserialize as tile coords");

        assert_eq!(args.from_x, 73);
        assert_eq!(args.from_y, -28);
        assert_eq!(args.pickup_x, 54);
        assert_eq!(args.pickup_y, -10);
        assert_eq!(args.source_unit_number, 87);

        let missing_source = serde_json::json!({
            "consumer_unit_number": 49,
            "from_x": 73.5,
            "from_y": -27.5,
            "pickup_x": 54.5,
            "pickup_y": -9.5,
            "inserter_x": 54.5,
            "inserter_y": -8.5,
            "inserter_direction": "north"
        });
        assert!(
            serde_json::from_value::<BuildFuelSupplyParams>(missing_source).is_err(),
            "fuel transactions must fail closed without an exact diagnosed source"
        );
    }

    #[test]
    fn ready_fuel_supply_args_accepts_top_level_suggested_action() {
        let report = serde_json::json!({
            "consumers": [],
            "suggested_actions": [{
                "type": "repair_fuel_sustainability",
                "tool": "repair_fuel_sustainability",
                "transaction_args": {
                    "consumer_unit_number": 73,
                    "source_unit_number": 74,
                    "from_x": 5,
                    "from_y": 6,
                    "pickup_x": 7,
                    "pickup_y": 8,
                    "inserter_x": 7.5,
                    "inserter_y": 8.5,
                    "inserter_direction": "south",
                    "inserter_name": "inserter"
                }
            }]
        });

        let args = ready_fuel_supply_args(&report).expect("suggested action args");

        assert_eq!(args.consumer_unit_number, 73);
        assert_eq!(args.inserter_direction, "south");
        assert_eq!(args.inserter_name, "inserter");
    }

    #[test]
    fn machine_output_build_args_derives_furnace_output_controller_payload() {
        let furnace = Entity {
            unit_number: Some(15),
            name: "stone-furnace".to_string(),
            entity_type: Some("furnace".to_string()),
            position: Position::new(42.0, -22.0),
            direction: 0,
            health: None,
            force: Some("player".to_string()),
            bounding_box: None,
            pickup_position: None,
            drop_position: None,
            belt_to_ground_type: None,
            underground_belt_neighbour: None,
            belt_input_neighbours: Vec::new(),
            belt_output_neighbours: Vec::new(),
            belt_neighbours_observed: false,
        };

        let args = machine_output_build_args(
            &furnace,
            "iron-plate".to_string(),
            50,
            -25,
            "north",
            "transport-belt".to_string(),
            10,
            false,
            false,
            true,
            8,
        )
        .expect("output args");

        assert_eq!(args.assembler_unit_number, 15);
        assert_eq!(args.item_name, "iron-plate");
        assert_eq!(args.drop_x, 42);
        assert_eq!(args.drop_y, -25);
        assert_eq!(args.to_x, 50);
        assert_eq!(args.inserter_x, 42.5);
        assert_eq!(args.inserter_y, -23.5);
        assert_eq!(args.inserter_direction, "south");
        assert!(args.dry_run);
    }
}

impl ConnectionConfig {
    fn from_env() -> Self {
        Self {
            host: std::env::var("FACTORIO_RCON_HOST").unwrap_or_else(|_| "localhost".to_string()),
            port: std::env::var("FACTORIO_RCON_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(27015),
            password: std::env::var("FACTORIO_RCON_PASSWORD").unwrap_or_default(),
        }
    }
}

// === Tool Parameter Types ===

/// Parameters for area-based queries
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct AreaParams {
    /// X coordinate of area center
    pub x: i32,
    /// Y coordinate of area center
    pub y: i32,
    /// Radius around center (area will be 2*radius x 2*radius)
    #[serde(default = "default_radius")]
    pub radius: u32,
}

fn default_radius() -> u32 {
    50
}

impl AreaParams {
    fn to_area(&self) -> Area {
        let r = self.radius as f64;
        Area {
            left_top: Position::new(self.x as f64 - r, self.y as f64 - r),
            right_bottom: Position::new(self.x as f64 + r, self.y as f64 + r),
        }
    }
}

fn flow_lookup(
    unit_number: Option<u32>,
    x: Option<i32>,
    y: Option<i32>,
    label: &str,
) -> Result<EntityLookup, String> {
    match (unit_number, x, y) {
        (Some(unit), None, None) => Ok(EntityLookup::Unit(unit)),
        (None, Some(x), Some(y)) => Ok(EntityLookup::Tile(TilePos::new(x, y))),
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => Err(format!(
            "Error: provide either {label}_unit_number or {label}_x/{label}_y, not both"
        )),
        (None, Some(_), None) | (None, None, Some(_)) => Err(format!(
            "Error: {label}_x and {label}_y must be provided together"
        )),
        (None, None, None) => Err(format!(
            "Error: provide {label}_unit_number or {label}_x/{label}_y"
        )),
    }
}

fn flow_scan_area(source: TilePos, target: TilePos, radius: u32) -> Area {
    let r = radius as f64;
    Area::new(
        source.x.min(target.x) as f64 - r,
        source.y.min(target.y) as f64 - r,
        source.x.max(target.x) as f64 + r,
        source.y.max(target.y) as f64 + r,
    )
}

async fn flow_reference_tile(
    client: &mut FactorioClient,
    lookup: EntityLookup,
) -> Result<TilePos, String> {
    match lookup {
        EntityLookup::Tile(tile) => Ok(tile),
        EntityLookup::Unit(unit) => client
            .get_entity(unit)
            .await
            .map(|entity| entity.position.to_tile())
            .map_err(|e| format!("Error: reading entity {unit}: {e}")),
    }
}

/// Entity-area query.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GetEntitiesParams {
    /// Center X tile.
    pub x: i32,
    /// Center Y tile.
    pub y: i32,
    /// Search radius.
    #[serde(default = "default_radius")]
    pub radius: u32,
    /// Optional prototype-name filter.
    pub name: Option<String>,
    /// Optional prototype-type filter.
    #[serde(default)]
    pub entity_type: Option<String>,
    /// Detailed result cap.
    #[serde(default = "default_entity_limit")]
    pub limit: usize,
}

/// Entity inventory query.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GetEntityInventoryParams {
    /// Exact entity unit number.
    pub unit_number: u32,
}

fn default_entity_limit() -> usize {
    100
}

/// Parameters for get_resources tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GetResourcesParams {
    /// X coordinate of area center
    pub x: i32,
    /// Y coordinate of area center
    pub y: i32,
    /// Radius around center
    #[serde(default = "default_radius")]
    pub radius: u32,
    /// Optional: filter by resource type (e.g., 'iron-ore')
    pub resource_type: Option<String>,
}

/// Parameters for situation_report tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SituationReportParams {
    /// Radius around the character to scan
    pub radius: Option<u32>,
}

/// Parameters for verify_production tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct VerifyProductionParams {
    /// X coordinate of area center (default: character position)
    pub x: Option<f64>,
    /// Y coordinate of area center (default: character position)
    pub y: Option<f64>,
    /// Radius around the center to scan
    pub radius: Option<u32>,
}

/// Parameters for diagnose_factory_blockers tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct DiagnoseFactoryBlockersParams {
    /// X coordinate of area center (default: character position)
    pub x: Option<f64>,
    /// Y coordinate of area center (default: character position)
    pub y: Option<f64>,
    /// Radius around the center to scan
    pub radius: Option<u32>,
    /// Maximum number of ranked blockers to return
    pub limit: Option<u32>,
}

/// Parameters for diagnose_fuel_sustainability tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct DiagnoseFuelSustainabilityParams {
    /// X coordinate of area center (default: character position)
    pub x: Option<f64>,
    /// Y coordinate of area center (default: character position)
    pub y: Option<f64>,
    /// Radius around the center to scan
    pub radius: Option<u32>,
    /// Maximum number of ranked fuel consumers to return
    pub limit: Option<u32>,
}

/// Parameters for find_nearest_resource tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct FindNearestResourceParams {
    /// Resource type to find (e.g., 'iron-ore', 'copper-ore', 'coal', 'stone')
    pub resource_type: String,
    /// X coordinate to search from (default: character position)
    pub x: Option<f64>,
    /// Y coordinate to search from (default: character position)
    pub y: Option<f64>,
    /// Generate and search nearby terrain, up to 512 tiles
    pub explore_radius: Option<u32>,
}

/// Parameters for position-based tools
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct PositionParams {
    /// X coordinate
    pub x: f64,
    /// Y coordinate
    pub y: f64,
}

/// Parameters for can_stand_at tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CanStandAtParams {
    /// X coordinate to check
    pub x: f64,
    /// Y coordinate to check
    pub y: f64,
    /// Nearby search radius for suggested clear positions
    #[serde(default = "default_radius")]
    pub radius: u32,
}

/// Parameters for character blockage diagnostics
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct PlayerBlockedParams {
    /// Nearby search radius for suggested clear positions
    #[serde(default = "default_radius")]
    pub radius: u32,
}

/// Parameters for unstuck tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct UnstuckParams {
    /// Nearby search radius for clear standing positions
    #[serde(default = "default_radius")]
    pub radius: u32,
    /// If true, only report the chosen recovery position without moving
    #[serde(default)]
    pub dry_run: bool,
}

/// Parameters for tile-based tools
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct TileParams {
    /// X coordinate (integer tile)
    pub x: i32,
    /// Y coordinate (integer tile)
    pub y: i32,
}

/// Parameters for belt reach analysis
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BeltReachParams {
    /// X coordinate of starting belt (integer tile)
    pub x: i32,
    /// Y coordinate of starting belt (integer tile)
    pub y: i32,
    /// Search radius
    #[serde(default = "default_radius")]
    pub radius: u32,
}

/// Parameters for item-flow analysis
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct AnalyzeItemFlowParams {
    /// Source entity unit number. If omitted, provide source_x and source_y.
    pub source_unit_number: Option<u32>,
    /// Source tile X coordinate, usually a belt tile.
    pub source_x: Option<i32>,
    /// Source tile Y coordinate, usually a belt tile.
    pub source_y: Option<i32>,
    /// Target entity unit number. If omitted, provide target_x and target_y.
    pub target_unit_number: Option<u32>,
    /// Target tile X coordinate, usually a belt tile or target entity tile.
    pub target_x: Option<i32>,
    /// Target tile Y coordinate, usually a belt tile or target entity tile.
    pub target_y: Option<i32>,
    /// Search radius around the source/target bounding area.
    #[serde(default = "default_radius")]
    pub radius: u32,
}

/// Parameters for place_entity tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct PlaceEntityParams {
    /// Entity name (e.g., 'inserter', 'splitter')
    pub entity_name: String,
    /// X coordinate to place at
    pub x: f64,
    /// Y coordinate to place at
    pub y: f64,
    /// Direction: "north", "east", "south", "west" (or shorthand/numeric).
    /// For inserters this is the pickup side; the item drops on the opposite side.
    #[serde(default)]
    pub direction: String,
}

/// Parameters for find_entity_placements tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct FindEntityPlacementsParams {
    /// Entity name (e.g., 'offshore-pump', 'steam-engine', 'boiler')
    pub entity_name: String,
    /// X coordinate of search center
    pub x: f64,
    /// Y coordinate of search center
    pub y: f64,
    /// Search radius in tiles (default: 10)
    #[serde(default = "default_placement_radius")]
    pub radius: u32,
    /// Maximum candidate placements to return (default: 20, max: 100)
    #[serde(default = "default_placement_limit")]
    pub limit: u32,
}

/// Parameters for plan_entity_placement_near tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct PlanEntityPlacementNearParams {
    /// Entity name to place
    pub entity_name: String,
    /// X coordinate of desired target area
    pub x: f64,
    /// Y coordinate of desired target area
    pub y: f64,
    /// Search radius in tiles (default: 10)
    #[serde(default = "default_placement_radius")]
    pub radius: u32,
    /// Maximum candidate placements to return (default: 20, max: 50)
    #[serde(default = "default_placement_limit")]
    pub limit: u32,
}

/// Parameters for execute_entity_placement_near tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ExecuteEntityPlacementNearParams {
    /// Entity name to place
    pub entity_name: String,
    /// X coordinate of desired target area
    pub x: f64,
    /// Y coordinate of desired target area
    pub y: f64,
    /// Search radius in tiles (default: 10)
    #[serde(default = "default_placement_radius")]
    pub radius: u32,
    /// Maximum candidate placements to return (default: 20, max: 50)
    #[serde(default = "default_placement_limit")]
    pub limit: u32,
    /// If true, only return the checked placement plan without placing anything.
    #[serde(default)]
    pub dry_run: bool,
}

/// Parameters for build_edge_miner tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BuildEdgeMinerParams {
    /// Resource type to mine (e.g., 'iron-ore', 'copper-ore', 'coal', 'stone')
    pub resource_type: String,
    /// X coordinate of target resource area center
    pub x: f64,
    /// Y coordinate of target resource area center
    pub y: f64,
    /// Search radius in tiles (default: 25, max: 40)
    #[serde(default = "default_edge_miner_radius")]
    pub radius: u32,
    /// Drill entity name (default: burner-mining-drill)
    #[serde(default = "default_drill_name")]
    pub drill_name: String,
    /// Maximum candidate placements to return (default: 10, max: 50)
    #[serde(default = "default_edge_miner_limit")]
    pub limit: u32,
}

/// Parameters for execute_edge_miner tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ExecuteEdgeMinerParams {
    /// Resource type to mine (e.g., 'iron-ore', 'copper-ore', 'coal', 'stone')
    pub resource_type: String,
    /// X coordinate of target resource area center
    pub x: f64,
    /// Y coordinate of target resource area center
    pub y: f64,
    /// Search radius in tiles (default: 25, max: 40)
    #[serde(default = "default_edge_miner_radius")]
    pub radius: u32,
    /// Drill entity name (default: burner-mining-drill)
    #[serde(default = "default_drill_name")]
    pub drill_name: String,
    /// Maximum candidate placements to return (default: 10, max: 50)
    #[serde(default = "default_edge_miner_limit")]
    pub limit: u32,
    /// If true, only return the checked plan without placing anything.
    #[serde(default)]
    pub dry_run: bool,
    /// Bootstrap fuel item for burner drills.
    #[serde(default = "default_fuel_item")]
    pub fuel_item: String,
    /// Bootstrap fuel count for burner drills.
    #[serde(default = "default_furnace_fuel_count")]
    pub fuel_count: u32,
    /// Verification radius around the placed drill after building.
    #[serde(default = "default_verify_radius")]
    pub verify_radius: u32,
}

/// Parameters for build_direct_smelter tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BuildDirectSmelterParams {
    /// Existing drill unit number. If omitted, provide output_x/output_y/output_direction from execute_edge_miner or get_machine_belt_positions.
    pub drill_unit_number: Option<u32>,
    /// X coordinate of the drill output belt tile.
    pub output_x: Option<f64>,
    /// Y coordinate of the drill output belt tile.
    pub output_y: Option<f64>,
    /// Direction the output belt should face: north, east, south, west (or 0/4/8/12).
    pub output_direction: Option<String>,
    /// Furnace entity name (default: stone-furnace)
    #[serde(default = "default_furnace_name")]
    pub furnace_name: String,
    /// Inserter entity name (default: burner-inserter)
    #[serde(default = "default_inserter_name")]
    pub inserter_name: String,
    /// Belt entity name (default: transport-belt)
    #[serde(default = "default_belt_type")]
    pub belt_name: String,
    /// Search radius for furnace placement around the output tile (default: 6, max: 12)
    #[serde(default = "default_direct_smelter_radius")]
    pub radius: u32,
}

/// Parameters for execute_direct_smelter tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ExecuteDirectSmelterParams {
    /// Existing drill unit number. If omitted, provide output_x/output_y/output_direction from execute_edge_miner or get_machine_belt_positions.
    pub drill_unit_number: Option<u32>,
    /// X coordinate of the drill output belt tile.
    pub output_x: Option<f64>,
    /// Y coordinate of the drill output belt tile.
    pub output_y: Option<f64>,
    /// Direction the output belt should face: north, east, south, west (or 0/4/8/12).
    pub output_direction: Option<String>,
    /// Furnace entity name (default: stone-furnace)
    #[serde(default = "default_furnace_name")]
    pub furnace_name: String,
    /// Inserter entity name (default: inserter)
    #[serde(default = "default_electric_inserter_name")]
    pub inserter_name: String,
    /// Belt entity name (default: transport-belt)
    #[serde(default = "default_belt_type")]
    pub belt_name: String,
    /// Search radius for furnace placement around the output tile (default: 6, max: 12)
    #[serde(default = "default_direct_smelter_radius")]
    pub radius: u32,
    /// If true, only return the checked plan without placing anything.
    #[serde(default)]
    pub dry_run: bool,
}

fn default_electric_inserter_name() -> String {
    "inserter".to_string()
}

fn default_placement_radius() -> u32 {
    10
}

fn default_placement_limit() -> u32 {
    20
}

fn default_edge_miner_radius() -> u32 {
    25
}

fn default_drill_name() -> String {
    "burner-mining-drill".to_string()
}

fn default_edge_miner_limit() -> u32 {
    10
}

fn default_furnace_name() -> String {
    "stone-furnace".to_string()
}

fn default_surface_name() -> String {
    "nauvis".to_string()
}

fn default_inserter_name() -> String {
    "burner-inserter".to_string()
}

fn default_direct_smelter_radius() -> u32 {
    6
}

/// Parameters for mine_at tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct MineAtParams {
    /// Exact X coordinate of a natural resource or loose item
    pub x: f64,
    /// Exact Y coordinate of a natural resource or loose item
    pub y: f64,
    /// Number of mining or pickup attempts
    #[serde(default = "default_count")]
    pub count: u32,
}

fn default_count() -> u32 {
    1
}

/// Parameters for craft tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CraftParams {
    /// Recipe name (e.g., 'iron-gear-wheel')
    pub recipe: String,
    /// Number to craft
    #[serde(default = "default_count")]
    pub count: u32,
}

/// Parameters for observing a previously accepted character craft.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct WaitForCraftingParams {
    /// Wait limit, from 1 through 120 seconds.
    #[serde(default = "default_crafting_timeout_seconds")]
    pub timeout_seconds: u32,
}

fn default_crafting_timeout_seconds() -> u32 {
    30
}

/// Parameters for a one-shot, bounded burner bootstrap.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BootstrapBurnerOnceParams {
    /// Existing burner drill/inserter unit number.
    pub unit_number: u32,
    /// Fuel item to transfer from the agent inventory.
    #[serde(default = "default_fuel_item")]
    pub fuel_item: String,
    /// Fuel count, from 1 through 10.
    #[serde(default = "default_bootstrap_fuel_count")]
    pub count: u32,
}

/// Chest collection request.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CollectFromChestParams {
    /// Exact chest unit number.
    pub unit_number: u32,
    /// Exact item name.
    pub item: String,
    /// Count from 1 through 1000.
    #[serde(default = "default_count")]
    pub count: u32,
}

/// Structured model-authored fields accepted by the bounded Beads reporter.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct FileIssueParams {
    /// Concise bug title.
    pub title: String,
    /// What happened in the game or tool call.
    pub observed_behavior: String,
    /// What should have happened instead.
    pub expected_behavior: String,
    /// One to ten concrete facts or errors.
    pub evidence: Vec<String>,
    /// Optional bounded reproduction steps.
    #[serde(default)]
    pub reproduction: Option<String>,
    /// Optional allowlisted labels; omit if unsure.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Priority from 0 (highest) through 4.
    #[serde(default = "default_issue_priority")]
    pub priority: u8,
}

fn default_issue_priority() -> u8 {
    2
}

/// Parameters for get_recipe tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GetRecipeParams {
    /// Recipe name (e.g., 'boiler', 'iron-gear-wheel')
    pub name: String,
}

/// Parameters for get_recipes_for_item tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GetRecipesForItemParams {
    /// Item/fluid name produced by recipes (e.g., 'boiler', 'steam-engine')
    pub item: String,
}

/// Parameters for get_recipes_by_category tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GetRecipesByCategoryParams {
    /// Recipe category (e.g., 'crafting', 'smelting')
    pub category: String,
}

/// Parameters for insert_items tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct InsertItemsParams {
    /// Target entity unit number
    pub unit_number: u32,
    /// Item name
    pub item: String,
    /// Number of items
    pub count: u32,
    /// Inventory type (e.g., 'chest', 'fuel', 'furnace_source')
    #[serde(default = "default_inventory_type")]
    pub inventory_type: String,
}

fn default_inventory_type() -> String {
    "chest".to_string()
}

/// Parameters for emergency_furnace_feed tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct HandFeedFurnaceParams {
    /// Unit number of the furnace to feed.
    pub furnace_unit_number: u32,
    /// Fuel item to insert, usually coal or wood.
    #[serde(default = "default_fuel_item")]
    pub fuel_item: String,
    /// Fuel item count to insert. Defaults to a short recovery buffer, not a
    /// durable fuel strategy.
    #[serde(default = "default_furnace_fuel_count")]
    pub fuel_count: u32,
    /// Source item to smelt, usually iron-ore or copper-ore.
    #[serde(default = "default_furnace_source_item")]
    pub source_item: String,
    /// Source item count to insert.
    #[serde(default = "default_furnace_source_count")]
    pub source_count: u32,
    /// Verification radius around the furnace after feeding.
    #[serde(default = "default_verify_radius")]
    pub verify_radius: u32,
}

/// Parameters for bootstrap_smelting_once tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BootstrapSmeltingOnceParams {
    /// Unit number of the furnace to bootstrap.
    pub furnace_unit_number: u32,
    /// Fuel item to insert, usually coal or wood.
    #[serde(default = "default_fuel_item")]
    pub fuel_item: String,
    /// Small temporary fuel buffer. This is not durable fuel automation.
    #[serde(default = "default_bootstrap_fuel_count")]
    pub fuel_count: u32,
    /// Source item to smelt, usually iron-ore or copper-ore.
    #[serde(default = "default_furnace_source_item")]
    pub source_item: String,
    /// Source item count to insert.
    #[serde(default = "default_furnace_source_count")]
    pub source_count: u32,
    /// Output item to extract after waiting.
    #[serde(default = "default_bootstrap_output_item")]
    pub output_item: String,
    /// Target output count to extract into inventory (1-1000), independent of source_count.
    #[serde(default = "default_bootstrap_output_count")]
    pub output_count: u32,
    /// Optional recipe to craft after extracting plates, such as burner-inserter.
    #[serde(default)]
    pub craft_recipe: String,
    /// Optional craft count.
    #[serde(default = "default_count")]
    pub craft_count: u32,
    /// Ticks to wait for smelting before extraction.
    #[serde(default = "default_bootstrap_wait_ticks")]
    pub wait_ticks: u32,
    /// Verification radius around the furnace after feeding.
    #[serde(default = "default_verify_radius")]
    pub verify_radius: u32,
    /// If true, only return the bounded bootstrap plan.
    #[serde(default)]
    pub dry_run: bool,
}

fn default_fuel_item() -> String {
    "coal".to_string()
}

fn default_bootstrap_fuel_count() -> u32 {
    5
}

fn default_furnace_fuel_count() -> u32 {
    25
}

fn default_furnace_source_item() -> String {
    "iron-ore".to_string()
}

fn default_furnace_source_count() -> u32 {
    20
}

fn default_bootstrap_output_item() -> String {
    "iron-plate".to_string()
}

fn default_bootstrap_output_count() -> u32 {
    5
}

const MAX_BOOTSTRAP_OUTPUT_COUNT: u32 = 1_000;

fn bounded_bootstrap_output_count(output_count: u32) -> u32 {
    output_count.clamp(1, MAX_BOOTSTRAP_OUTPUT_COUNT)
}

fn default_bootstrap_wait_ticks() -> u32 {
    1200
}

fn default_verify_radius() -> u32 {
    4
}

fn raw_lua_enabled(env_value: Option<&str>) -> bool {
    matches!(
        env_value.map(|value| value.trim().to_ascii_lowercase()),
        Some(value) if matches!(value.as_str(), "1" | "true" | "yes" | "on")
    )
}

fn execute_lua_refusal(env_value: Option<&str>) -> Option<String> {
    if raw_lua_enabled(env_value) {
        None
    } else {
        Some(
            "Error: execute_lua is disabled. Raw Lua execution is an arbitrary-code-execution surface and is off by default. Set FACTORIOCTL_ALLOW_RAW_LUA=1 to enable it for trusted operator use."
                .to_string(),
        )
    }
}

/// Parameters for extract_items tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ExtractItemsParams {
    /// Source entity unit number
    pub unit_number: u32,
    /// Item name to extract
    pub item: String,
    /// Number of items to extract
    pub count: u32,
    /// Inventory type (e.g., 'chest', 'fuel', 'furnace_result', 'output')
    #[serde(default = "default_inventory_type")]
    pub inventory_type: String,
}

/// Parameters for set_recipe tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SetRecipeParams {
    /// Target entity unit number (assembling machine, chemical plant, etc.)
    pub unit_number: u32,
    /// Recipe name to set (e.g., 'iron-gear-wheel', 'electronic-circuit'). Use empty string to clear recipe.
    pub recipe: String,
}

/// Parameters for route_belt tool - routes belts from A to B using pathfinding
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct RouteBeltParams {
    /// Starting X coordinate (integer tile)
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub from_x: i32,
    /// Starting Y coordinate (integer tile)
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub from_y: i32,
    /// Destination X coordinate (integer tile)
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub to_x: i32,
    /// Destination Y coordinate (integer tile)
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub to_y: i32,
    /// Belt type (e.g., 'transport-belt', 'fast-transport-belt')
    #[serde(default = "default_belt_type")]
    pub belt_type: String,
    /// Search radius for obstacle detection
    #[serde(default = "default_search_radius")]
    pub search_radius: u32,
    /// If true, only plan the route without placing belts
    #[serde(default)]
    pub dry_run: bool,
    /// Respect zone boundaries when routing (default: false).
    /// When true, routes around Assembly/Smelting/Power/Storage/Reserved zones
    /// and prefers Logistics zones for belt highways.
    #[serde(default)]
    pub respect_zones: bool,
    /// Allow underground belts in routing (default: false).
    /// When true, the router may use underground belts to skip obstacles.
    /// Requires the appropriate technology to be researched (logistics, logistics-2, or logistics-3).
    #[serde(default)]
    pub allow_underground: bool,
    /// Allow routing to start/end on existing belts (default: false).
    /// When true, existing belts at start/end positions are treated as valid connection points.
    /// Useful for extending or branching off existing belt networks.
    #[serde(default)]
    pub extend_existing: bool,
}

fn default_belt_type() -> String {
    "transport-belt".to_string()
}
fn default_search_radius() -> u32 {
    10
}
fn is_existing_belt_entity(name: &str) -> bool {
    matches!(
        name,
        "transport-belt"
            | "fast-transport-belt"
            | "express-transport-belt"
            | "turbo-transport-belt"
            | "underground-belt"
            | "fast-underground-belt"
            | "express-underground-belt"
            | "turbo-underground-belt"
            | "splitter"
            | "fast-splitter"
            | "express-splitter"
            | "turbo-splitter"
    )
}

fn direct_placement_requires_route(name: &str) -> bool {
    matches!(
        name,
        "transport-belt"
            | "fast-transport-belt"
            | "express-transport-belt"
            | "turbo-transport-belt"
            | "underground-belt"
            | "fast-underground-belt"
            | "express-underground-belt"
            | "turbo-underground-belt"
    )
}

fn existing_belt_compatibility(
    entity: &Entity,
    planned: &BeltPlacement,
    surface_belt_name: &str,
) -> Result<(), String> {
    if planned.kind != BeltKind::Surface {
        return Err(
            "an existing endpoint cannot replace a planned underground-belt endpoint".to_string(),
        );
    }
    if entity.name != surface_belt_name || entity.entity_type.as_deref() != Some("transport-belt") {
        return Err(format!(
            "expected {surface_belt_name} surface belt, found {} ({})",
            entity.name,
            entity.entity_type.as_deref().unwrap_or("unknown type")
        ));
    }
    let actual_direction = Direction::from_factorio(entity.direction);
    if actual_direction != planned.direction {
        return Err(format!(
            "existing {} faces {}, but the route requires {}",
            entity.name,
            actual_direction.to_name(),
            planned.direction.to_name()
        ));
    }
    Ok(())
}

fn existing_underground_pair_reservations(
    entities: &[Entity],
    underground_name: &str,
) -> (HashSet<GridPos>, Vec<serde_json::Value>) {
    let undergrounds: HashMap<GridPos, &Entity> = entities
        .iter()
        .filter(|entity| {
            entity.name == underground_name
                && entity.entity_type.as_deref() == Some("underground-belt")
        })
        .map(|entity| (GridPos::from_position(&entity.position), entity))
        .collect();
    let mut seen_pairs = HashSet::new();
    let mut reserved_tiles = HashSet::new();
    let mut reservations = Vec::new();

    for (tile, entity) in &undergrounds {
        let Some(neighbour_position) = entity.underground_belt_neighbour.as_ref() else {
            continue;
        };
        let neighbour_tile = GridPos::from_position(neighbour_position);
        let Some(neighbour) = undergrounds.get(&neighbour_tile) else {
            continue;
        };
        let pair_key = if (tile.x, tile.y) <= (neighbour_tile.x, neighbour_tile.y) {
            (*tile, neighbour_tile)
        } else {
            (neighbour_tile, *tile)
        };
        if !seen_pairs.insert(pair_key) {
            continue;
        }

        let dx = neighbour_tile.x - tile.x;
        let dy = neighbour_tile.y - tile.y;
        if dx != 0 && dy != 0 {
            continue;
        }
        let distance = dx.unsigned_abs() + dy.unsigned_abs();
        if distance < 2 {
            continue;
        }
        let step_x = dx.signum();
        let step_y = dy.signum();
        let mut pair_reserved = Vec::new();
        for step in 1..distance {
            let reserved =
                GridPos::new(tile.x + step_x * step as i32, tile.y + step_y * step as i32);
            reserved_tiles.insert(reserved);
            pair_reserved.push(reserved);
        }
        reservations.push(serde_json::json!({
            "first": {
                "unit_number": entity.unit_number,
                "position": tile,
                "belt_to_ground_type": entity.belt_to_ground_type,
            },
            "second": {
                "unit_number": neighbour.unit_number,
                "position": neighbour_tile,
                "belt_to_ground_type": neighbour.belt_to_ground_type,
            },
            "reserved_endpoint_tiles": pair_reserved,
        }));
    }

    reservations.sort_by_key(|reservation| {
        (
            reservation["first"]["position"]["x"]
                .as_i64()
                .unwrap_or_default(),
            reservation["first"]["position"]["y"]
                .as_i64()
                .unwrap_or_default(),
        )
    });
    (reserved_tiles, reservations)
}

fn endpoint_belt_incompatibility(
    incompatible: &[(GridPos, serde_json::Value)],
    start: GridPos,
    goal: GridPos,
) -> Option<serde_json::Value> {
    [(&goal, "goal"), (&start, "start")]
        .into_iter()
        .find_map(|(endpoint_tile, endpoint_kind)| {
            incompatible
                .iter()
                .find(|(tile, _)| tile == endpoint_tile)
                .map(|(tile, error)| {
                    let mut error = error.clone();
                    if let Some(object) = error.as_object_mut() {
                        object.insert(
                            "endpoint_kind".to_string(),
                            serde_json::json!(endpoint_kind),
                        );
                        object.insert("endpoint_tile".to_string(), serde_json::json!(tile));
                    }
                    error
                })
        })
}

/// Parameters for build_fuel_supply tool.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct BuildFuelSupplyParams {
    /// Fuel consumer to supply, used for verification context.
    pub consumer_unit_number: u32,
    /// Exact diagnosed coal source. Existing belt sources are tapped without mutation.
    pub source_unit_number: u32,
    /// Coal source or existing coal belt X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub from_x: i32,
    /// Coal source or existing coal belt Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub from_y: i32,
    /// Inserter pickup belt X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub pickup_x: i32,
    /// Inserter pickup belt Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub pickup_y: i32,
    /// Inserter placement X coordinate.
    pub inserter_x: f64,
    /// Inserter placement Y coordinate.
    pub inserter_y: f64,
    /// Inserter direction feeding the consumer.
    pub inserter_direction: String,
    /// Inserter entity name to place (default: burner-inserter for fuel feeds).
    #[serde(default = "default_inserter_name")]
    pub inserter_name: String,
    /// Bootstrap fuel item for a burner inserter.
    #[serde(default = "default_fuel_item")]
    pub inserter_fuel_item: String,
    /// Bootstrap fuel count for a burner inserter.
    #[serde(default = "default_bootstrap_fuel_count")]
    pub inserter_fuel_count: u32,
    /// Self-bootstrap coal drill; must equal consumer_unit_number.
    #[serde(default)]
    pub provisional_source_unit_number: Option<u32>,
    /// Bounded startup fuel for that provisional drill.
    #[serde(default)]
    pub bootstrap_consumer_fuel_count: u32,
    /// Belt type to route.
    #[serde(default = "default_belt_type")]
    pub belt_type: String,
    /// Search radius for obstacle detection.
    #[serde(default = "default_search_radius")]
    pub search_radius: u32,
    /// If true, only plan the route/inserter without placing anything.
    #[serde(default)]
    pub dry_run: bool,
    /// Respect zone boundaries when routing.
    #[serde(default)]
    pub respect_zones: bool,
    /// Unsupported for fuel repair; must be false.
    #[serde(default)]
    pub allow_underground: bool,
    /// Allow routing to start/end on existing belts.
    #[serde(default = "default_true")]
    pub extend_existing: bool,
    /// Verification radius around the consumer after building.
    #[serde(default = "default_verify_radius")]
    pub verify_radius: u32,
}

/// Parameters for repair_fuel_sustainability tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RepairFuelSustainabilityParams {
    /// X coordinate of area center (default: character position)
    pub x: Option<f64>,
    /// Y coordinate of area center (default: character position)
    pub y: Option<f64>,
    /// Radius around the center to scan
    pub radius: Option<u32>,
    /// Maximum number of ranked fuel consumers to inspect
    pub limit: Option<u32>,
    /// Search radius for route obstacle detection.
    #[serde(default = "default_search_radius")]
    pub search_radius: u32,
    /// If true, diagnose and preview the selected durable fuel transaction without placing.
    #[serde(default)]
    pub dry_run: bool,
    /// Respect zone boundaries when routing.
    #[serde(default)]
    pub respect_zones: bool,
    /// Unsupported for fuel repair; must be false.
    #[serde(default)]
    pub allow_underground: bool,
    /// Allow routing to start/end on existing belts.
    #[serde(default = "default_true")]
    pub extend_existing: bool,
    /// Verification radius around the consumer after building.
    #[serde(default = "default_verify_radius")]
    pub verify_radius: u32,
}

/// Parameters for build_lab_feed tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BuildLabFeedParams {
    /// Target lab unit number, used for research verification context.
    pub lab_unit_number: u32,
    /// Science source or existing science belt X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub from_x: i32,
    /// Science source or existing science belt Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub from_y: i32,
    /// Inserter pickup belt X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub pickup_x: i32,
    /// Inserter pickup belt Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub pickup_y: i32,
    /// Inserter placement X coordinate.
    pub inserter_x: f64,
    /// Inserter placement Y coordinate.
    pub inserter_y: f64,
    /// Inserter direction feeding the lab.
    pub inserter_direction: String,
    /// Belt type to route.
    #[serde(default = "default_belt_type")]
    pub belt_type: String,
    /// Search radius for obstacle detection.
    #[serde(default = "default_search_radius")]
    pub search_radius: u32,
    /// If true, only plan the route/inserter without placing anything.
    #[serde(default)]
    pub dry_run: bool,
    /// Respect zone boundaries when routing.
    #[serde(default)]
    pub respect_zones: bool,
    /// Allow underground belts if researched.
    #[serde(default)]
    pub allow_underground: bool,
    /// Allow routing to start/end on existing belts.
    #[serde(default = "default_true")]
    pub extend_existing: bool,
}

/// Parameters for build_assembler_feed tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BuildAssemblerFeedParams {
    /// Target assembling machine unit number.
    pub assembler_unit_number: u32,
    /// Optional recipe to set on the assembler before building the feed.
    #[serde(default)]
    pub recipe: String,
    /// Item expected on this feed lane, used for reporting and guidance.
    pub item_name: String,
    /// Item source or existing item belt X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub from_x: i32,
    /// Item source or existing item belt Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub from_y: i32,
    /// Inserter pickup belt X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub pickup_x: i32,
    /// Inserter pickup belt Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub pickup_y: i32,
    /// Inserter placement X coordinate.
    pub inserter_x: f64,
    /// Inserter placement Y coordinate.
    pub inserter_y: f64,
    /// Inserter direction feeding the assembler.
    pub inserter_direction: String,
    /// Belt type to route.
    #[serde(default = "default_belt_type")]
    pub belt_type: String,
    /// Search radius for obstacle detection.
    #[serde(default = "default_search_radius")]
    pub search_radius: u32,
    /// If true, only plan the route/recipe/inserter without placing anything.
    #[serde(default)]
    pub dry_run: bool,
    /// Respect zone boundaries when routing.
    #[serde(default)]
    pub respect_zones: bool,
    /// Allow underground belts if researched.
    #[serde(default)]
    pub allow_underground: bool,
    /// Allow routing to start/end on existing belts.
    #[serde(default = "default_true")]
    pub extend_existing: bool,
    /// Verification radius around the assembler after building.
    #[serde(default = "default_verify_radius")]
    pub verify_radius: u32,
}

/// Parameters for build_assembler_output tool.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct BuildAssemblerOutputParams {
    /// Source machine/furnace unit number. Field name is retained for compatibility.
    pub assembler_unit_number: u32,
    /// Item expected on this output lane, used for reporting and guidance.
    pub item_name: String,
    /// Inserter drop belt X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub drop_x: i32,
    /// Inserter drop belt Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub drop_y: i32,
    /// Target belt X tile to route the output toward.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub to_x: i32,
    /// Target belt Y tile to route the output toward.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub to_y: i32,
    /// Inserter placement X coordinate.
    pub inserter_x: f64,
    /// Inserter placement Y coordinate.
    pub inserter_y: f64,
    /// Inserter direction extracting from the assembler toward the output belt.
    pub inserter_direction: String,
    /// Belt type to route.
    #[serde(default = "default_belt_type")]
    pub belt_type: String,
    /// Search radius for obstacle detection.
    #[serde(default = "default_search_radius")]
    pub search_radius: u32,
    /// If true, only plan the route/inserter without placing anything.
    #[serde(default)]
    pub dry_run: bool,
    /// Respect zone boundaries when routing.
    #[serde(default)]
    pub respect_zones: bool,
    /// Allow underground belts if researched.
    #[serde(default)]
    pub allow_underground: bool,
    /// Allow routing to start/end on existing belts.
    #[serde(default = "default_true")]
    pub extend_existing: bool,
    /// Verification radius around the assembler after building.
    #[serde(default = "default_verify_radius")]
    pub verify_radius: u32,
}

/// Parameters for plan_machine_output tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct PlanMachineOutputParams {
    /// Source crafting machine/furnace unit number.
    pub source_unit_number: u32,
    /// Item expected on the output belt, such as iron-plate or automation-science-pack.
    pub item_name: String,
    /// Target belt X tile to route the output toward.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub to_x: i32,
    /// Target belt Y tile to route the output toward.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub to_y: i32,
    /// Side of the machine to extract output from.
    #[serde(default = "default_recipe_output_side")]
    pub output_side: String,
    /// Belt type to route.
    #[serde(default = "default_belt_type")]
    pub belt_type: String,
    /// Search radius for obstacle detection.
    #[serde(default = "default_search_radius")]
    pub search_radius: u32,
    /// Respect zone boundaries when routing.
    #[serde(default)]
    pub respect_zones: bool,
    /// Allow underground belts if researched.
    #[serde(default)]
    pub allow_underground: bool,
    /// Allow routing to start/end on existing belts.
    #[serde(default = "default_true")]
    pub extend_existing: bool,
    /// Verification radius around the source after building.
    #[serde(default = "default_verify_radius")]
    pub verify_radius: u32,
}

fn default_recipe_input_side() -> String {
    "west".to_string()
}

fn default_recipe_output_side() -> String {
    "east".to_string()
}

/// Parameters for plan_recipe_assembler_cell tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct PlanRecipeAssemblerCellParams {
    /// Assembling machine that will craft the component recipe.
    pub assembler_unit_number: u32,
    /// Recipe to set on the assembler (for example iron-gear-wheel).
    pub recipe: String,
    /// Recipe input item expected on the source belt.
    pub input_item_name: String,
    /// Product item expected on the output belt.
    pub output_item_name: String,
    /// Input item source or existing input belt X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub input_from_x: i32,
    /// Input item source or existing input belt Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub input_from_y: i32,
    /// Target belt X tile to route the product toward.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub output_to_x: i32,
    /// Target belt Y tile to route the product toward.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub output_to_y: i32,
    /// Side of the assembler to feed input from.
    #[serde(default = "default_recipe_input_side")]
    pub input_side: String,
    /// Side of the assembler to extract output from.
    #[serde(default = "default_recipe_output_side")]
    pub output_side: String,
    /// Belt type to route for both input and output legs.
    #[serde(default = "default_belt_type")]
    pub belt_type: String,
    /// Search radius for each routed leg.
    #[serde(default = "default_search_radius")]
    pub search_radius: u32,
    /// Respect zone boundaries when routing.
    #[serde(default)]
    pub respect_zones: bool,
    /// Allow underground belts if researched.
    #[serde(default)]
    pub allow_underground: bool,
    /// Allow routing to start/end on existing belts.
    #[serde(default = "default_true")]
    pub extend_existing: bool,
    /// Verification radius around the assembler after building.
    #[serde(default = "default_verify_radius")]
    pub verify_radius: u32,
}

/// Parameters for build_recipe_assembler_cell tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BuildRecipeAssemblerCellParams {
    /// Assembling machine that will craft the component recipe.
    pub assembler_unit_number: u32,
    /// Recipe to set on the assembler (for example iron-gear-wheel).
    pub recipe: String,
    /// Recipe input item expected on the source belt.
    pub input_item_name: String,
    /// Product item expected on the output belt.
    pub output_item_name: String,
    /// Input item source or existing input belt X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub input_from_x: i32,
    /// Input item source or existing input belt Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub input_from_y: i32,
    /// Input inserter pickup belt X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub input_pickup_x: i32,
    /// Input inserter pickup belt Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub input_pickup_y: i32,
    /// Input inserter placement X coordinate.
    pub input_inserter_x: f64,
    /// Input inserter placement Y coordinate.
    pub input_inserter_y: f64,
    /// Input inserter direction feeding the assembler.
    pub input_inserter_direction: String,
    /// Assembler output inserter drop belt X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub output_drop_x: i32,
    /// Assembler output inserter drop belt Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub output_drop_y: i32,
    /// Target belt X tile to route the product toward.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub output_to_x: i32,
    /// Target belt Y tile to route the product toward.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub output_to_y: i32,
    /// Output inserter placement X coordinate.
    pub output_inserter_x: f64,
    /// Output inserter placement Y coordinate.
    pub output_inserter_y: f64,
    /// Output inserter direction extracting from the assembler.
    pub output_inserter_direction: String,
    /// Belt type to route for both input and output legs.
    #[serde(default = "default_belt_type")]
    pub belt_type: String,
    /// Search radius for each routed leg.
    #[serde(default = "default_search_radius")]
    pub search_radius: u32,
    /// If true, only plan routes/inserters/recipe without placing anything.
    #[serde(default)]
    pub dry_run: bool,
    /// Respect zone boundaries when routing.
    #[serde(default)]
    pub respect_zones: bool,
    /// Allow underground belts if researched.
    #[serde(default)]
    pub allow_underground: bool,
    /// Allow routing to start/end on existing belts.
    #[serde(default = "default_true")]
    pub extend_existing: bool,
    /// Verification radius around the assembler after building.
    #[serde(default = "default_verify_radius")]
    pub verify_radius: u32,
}

/// Parameters for build_automation_science tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BuildAutomationScienceParams {
    /// Assembling machine that will craft automation-science-pack.
    pub assembler_unit_number: u32,
    /// Lab that should receive the science belt.
    pub lab_unit_number: u32,
    /// Iron gear source or existing gear belt X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub gear_from_x: i32,
    /// Iron gear source or existing gear belt Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub gear_from_y: i32,
    /// Gear inserter pickup belt X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub gear_pickup_x: i32,
    /// Gear inserter pickup belt Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub gear_pickup_y: i32,
    /// Gear inserter placement X coordinate.
    pub gear_inserter_x: f64,
    /// Gear inserter placement Y coordinate.
    pub gear_inserter_y: f64,
    /// Gear inserter direction feeding the assembler.
    pub gear_inserter_direction: String,
    /// Copper plate source or existing copper belt X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub copper_from_x: i32,
    /// Copper plate source or existing copper belt Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub copper_from_y: i32,
    /// Copper inserter pickup belt X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub copper_pickup_x: i32,
    /// Copper inserter pickup belt Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub copper_pickup_y: i32,
    /// Copper inserter placement X coordinate.
    pub copper_inserter_x: f64,
    /// Copper inserter placement Y coordinate.
    pub copper_inserter_y: f64,
    /// Copper inserter direction feeding the assembler.
    pub copper_inserter_direction: String,
    /// Assembler output inserter drop belt X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub science_drop_x: i32,
    /// Assembler output inserter drop belt Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub science_drop_y: i32,
    /// Intermediate science belt target X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub science_to_x: i32,
    /// Intermediate science belt target Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub science_to_y: i32,
    /// Output inserter placement X coordinate.
    pub output_inserter_x: f64,
    /// Output inserter placement Y coordinate.
    pub output_inserter_y: f64,
    /// Output inserter direction extracting science from the assembler.
    pub output_inserter_direction: String,
    /// Science belt source X tile for the lab-feed leg, usually science_to_x.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub lab_from_x: i32,
    /// Science belt source Y tile for the lab-feed leg, usually science_to_y.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub lab_from_y: i32,
    /// Lab inserter pickup belt X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub lab_pickup_x: i32,
    /// Lab inserter pickup belt Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub lab_pickup_y: i32,
    /// Lab inserter placement X coordinate.
    pub lab_inserter_x: f64,
    /// Lab inserter placement Y coordinate.
    pub lab_inserter_y: f64,
    /// Lab inserter direction feeding the lab.
    pub lab_inserter_direction: String,
    /// Belt type to route for all science-cell belts.
    #[serde(default = "default_belt_type")]
    pub belt_type: String,
    /// Search radius for each routed leg.
    #[serde(default = "default_search_radius")]
    pub search_radius: u32,
    /// If true, only plan routes/inserters/recipe without placing anything.
    #[serde(default)]
    pub dry_run: bool,
    /// Respect zone boundaries when routing.
    #[serde(default)]
    pub respect_zones: bool,
    /// Allow underground belts if researched.
    #[serde(default)]
    pub allow_underground: bool,
    /// Allow routing to start/end on existing belts.
    #[serde(default = "default_true")]
    pub extend_existing: bool,
    /// Verification radius around the assembler after building.
    #[serde(default = "default_verify_radius")]
    pub verify_radius: u32,
}

fn default_automation_gear_side() -> String {
    "north".to_string()
}

fn default_automation_copper_side() -> String {
    "south".to_string()
}

fn default_automation_output_side() -> String {
    "east".to_string()
}

fn default_automation_lab_side() -> String {
    "west".to_string()
}

/// Parameters for plan_automation_science tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct PlanAutomationScienceParams {
    /// Assembling machine that will craft automation-science-pack.
    pub assembler_unit_number: u32,
    /// Lab that should receive the science belt.
    pub lab_unit_number: u32,
    /// Iron gear source or existing gear belt X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub gear_from_x: i32,
    /// Iron gear source or existing gear belt Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub gear_from_y: i32,
    /// Copper plate source or existing copper belt X tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub copper_from_x: i32,
    /// Copper plate source or existing copper belt Y tile.
    #[serde(deserialize_with = "deserialize_tile_i32")]
    #[schemars(with = "f64")]
    pub copper_from_y: i32,
    /// Side of the assembler to feed iron gears from.
    #[serde(default = "default_automation_gear_side")]
    pub gear_side: String,
    /// Side of the assembler to feed copper plates from.
    #[serde(default = "default_automation_copper_side")]
    pub copper_side: String,
    /// Side of the assembler to extract automation science packs from.
    #[serde(default = "default_automation_output_side")]
    pub output_side: String,
    /// Side of the lab to feed automation science packs from.
    #[serde(default = "default_automation_lab_side")]
    pub lab_side: String,
    /// Belt type to route for all science-cell belts.
    #[serde(default = "default_belt_type")]
    pub belt_type: String,
    /// Search radius for each routed leg.
    #[serde(default = "default_search_radius")]
    pub search_radius: u32,
    /// Respect zone boundaries when routing.
    #[serde(default)]
    pub respect_zones: bool,
    /// Allow underground belts if researched.
    #[serde(default)]
    pub allow_underground: bool,
    /// Allow routing to start/end on existing belts.
    #[serde(default = "default_true")]
    pub extend_existing: bool,
    /// Verification radius around the assembler after building.
    #[serde(default = "default_verify_radius")]
    pub verify_radius: u32,
}

#[derive(Debug, Clone, Copy, Serialize)]
struct MachineSideLayout {
    side: &'static str,
    inserter_x: f64,
    inserter_y: f64,
    belt_x: i32,
    belt_y: i32,
    upstream_x: i32,
    upstream_y: i32,
    input_direction: &'static str,
    output_direction: &'static str,
}

fn tile_center_axis(value: f64) -> f64 {
    value.floor() + 0.5
}

fn route_segment_waypoint(
    from_x: i32,
    from_y: i32,
    to_x: i32,
    to_y: i32,
    max_span: i32,
) -> GridPos {
    let max_span = max_span.max(1);
    let dx = to_x - from_x;
    let dy = to_y - from_y;
    let manhattan = dx.abs() + dy.abs();
    if manhattan <= max_span {
        return GridPos::new(to_x, to_y);
    }

    let ratio = max_span as f64 / manhattan as f64;
    let mut step_x = (dx as f64 * ratio).round() as i32;
    let mut step_y = (dy as f64 * ratio).round() as i32;
    if step_x == 0 && dx != 0 {
        step_x = dx.signum();
    }
    if step_y == 0 && dy != 0 {
        step_y = dy.signum();
    }
    GridPos::new(from_x + step_x, from_y + step_y)
}

fn machine_side_layout(entity: &Entity, side: &str) -> Result<MachineSideLayout, String> {
    let bbox = machine_bounding_box(entity)?;
    let center = bbox.center();
    let lane_x = tile_center_axis(center.x);
    let lane_y = tile_center_axis(center.y);
    match side.to_lowercase().as_str() {
        "north" | "n" | "up" => Ok(MachineSideLayout {
            side: "north",
            inserter_x: lane_x,
            inserter_y: bbox.left_top.y - 0.5,
            belt_x: lane_x.floor() as i32,
            belt_y: (bbox.left_top.y - 1.5).floor() as i32,
            upstream_x: lane_x.floor() as i32,
            upstream_y: (bbox.left_top.y - 2.5).floor() as i32,
            input_direction: "north",
            output_direction: "south",
        }),
        "east" | "e" | "right" => Ok(MachineSideLayout {
            side: "east",
            inserter_x: bbox.right_bottom.x + 0.5,
            inserter_y: lane_y,
            belt_x: (bbox.right_bottom.x + 1.5).floor() as i32,
            belt_y: lane_y.floor() as i32,
            upstream_x: (bbox.right_bottom.x + 2.5).floor() as i32,
            upstream_y: lane_y.floor() as i32,
            input_direction: "east",
            output_direction: "west",
        }),
        "south" | "s" | "down" => Ok(MachineSideLayout {
            side: "south",
            inserter_x: lane_x,
            inserter_y: bbox.right_bottom.y + 0.5,
            belt_x: lane_x.floor() as i32,
            belt_y: (bbox.right_bottom.y + 1.5).floor() as i32,
            upstream_x: lane_x.floor() as i32,
            upstream_y: (bbox.right_bottom.y + 2.5).floor() as i32,
            input_direction: "south",
            output_direction: "north",
        }),
        "west" | "w" | "left" => Ok(MachineSideLayout {
            side: "west",
            inserter_x: bbox.left_top.x - 0.5,
            inserter_y: lane_y,
            belt_x: (bbox.left_top.x - 1.5).floor() as i32,
            belt_y: lane_y.floor() as i32,
            upstream_x: (bbox.left_top.x - 2.5).floor() as i32,
            upstream_y: lane_y.floor() as i32,
            input_direction: "west",
            output_direction: "east",
        }),
        _ => Err(format!(
            "invalid side '{}'; use north, east, south, or west",
            side
        )),
    }
}

fn machine_bounding_box(entity: &Entity) -> Result<Area, String> {
    if let Some(bbox) = entity.bounding_box {
        return Ok(bbox);
    }
    let entity_type = entity.entity_type.as_deref().unwrap_or("");
    let is_machine_like = entity.name.starts_with("assembling-machine")
        || entity_type == "assembling-machine"
        || entity.name.contains("furnace")
        || entity_type == "furnace"
        || entity.name == "lab"
        || entity_type == "lab"
        || matches!(
            entity.name.as_str(),
            "chemical-plant" | "oil-refinery" | "centrifuge" | "rocket-silo"
        );
    let (width, height) = entity_size(&entity.name);
    if !is_machine_like || (width, height) == (1, 1) {
        return Err(format!("{} has no bounding box", entity.name));
    };
    let width = width as f64;
    let height = height as f64;
    Ok(Area::new(
        entity.position.x - width / 2.0,
        entity.position.y - height / 2.0,
        entity.position.x + width / 2.0,
        entity.position.y + height / 2.0,
    ))
}

fn is_machine_output_source(entity: &Entity) -> bool {
    let entity_type = entity.entity_type.as_deref().unwrap_or("");
    entity.name.starts_with("assembling-machine")
        || entity_type == "assembling-machine"
        || entity.name.contains("furnace")
        || entity_type == "furnace"
        || entity.name == "chemical-plant"
        || entity_type == "crafting-machine"
}

#[allow(clippy::too_many_arguments)]
fn machine_output_build_args(
    entity: &Entity,
    item_name: String,
    to_x: i32,
    to_y: i32,
    output_side: &str,
    belt_type: String,
    search_radius: u32,
    respect_zones: bool,
    allow_underground: bool,
    extend_existing: bool,
    verify_radius: u32,
) -> Result<BuildAssemblerOutputParams, String> {
    if !is_machine_output_source(entity) {
        return Err(format!(
            "unit {:?} is {}, not a supported output machine/furnace",
            entity.unit_number, entity.name
        ));
    }
    let output = machine_side_layout(entity, output_side)?;
    Ok(BuildAssemblerOutputParams {
        assembler_unit_number: entity
            .unit_number
            .ok_or_else(|| format!("{} has no unit_number", entity.name))?,
        item_name,
        drop_x: output.belt_x,
        drop_y: output.belt_y,
        to_x,
        to_y,
        inserter_x: output.inserter_x,
        inserter_y: output.inserter_y,
        inserter_direction: output.output_direction.to_string(),
        belt_type,
        search_radius,
        dry_run: true,
        respect_zones,
        allow_underground,
        extend_existing,
        verify_radius,
    })
}

fn copy_json_fields(
    value: &serde_json::Value,
    fields: &[&str],
) -> serde_json::Map<String, serde_json::Value> {
    let mut result = serde_json::Map::new();
    for field in fields {
        if let Some(field_value) = value.get(*field) {
            result.insert((*field).to_string(), field_value.clone());
        }
    }
    result
}

fn compact_fuel_source(source: &serde_json::Value) -> serde_json::Value {
    let mut result = copy_json_fields(
        source,
        &[
            "kind",
            "unit_number",
            "name",
            "position",
            "route_position",
            "route_tile",
            "status",
            "coal_count",
            "durable",
            "operational",
            "producer_operational",
            "self_bootstrap_capable",
            "distance",
        ],
    );
    if let Some(reason) = source
        .get("upstream_proof")
        .and_then(|proof| proof.get("reason"))
    {
        result.insert("upstream_reason".to_string(), reason.clone());
    }
    serde_json::Value::Object(result)
}

fn compact_fuel_connection(connection: &serde_json::Value) -> serde_json::Value {
    let mut result = copy_json_fields(
        connection,
        &[
            "connection_kind",
            "inserter_unit_number",
            "inserter_name",
            "inserter_status",
            "inserter_held_item",
            "inserter_operational",
            "pickup_position",
            "drop_position",
            "source_durable",
            "source_operational",
            "durable",
            "live",
        ],
    );
    if let Some(source) = connection.get("source") {
        result.insert("source".to_string(), compact_fuel_source(source));
    }
    serde_json::Value::Object(result)
}

fn compact_fuel_consumer(consumer: &serde_json::Value) -> serde_json::Value {
    let mut result = copy_json_fields(
        consumer,
        &[
            "priority",
            "unit_number",
            "name",
            "type",
            "position",
            "status",
            "fuel_count",
            "remaining_burning_fuel",
            "issue",
            "fuel_topology_present",
            "automated",
            "ready_to_call",
        ],
    );
    for field in ["fuel_connections", "proven_fuel_connections"] {
        if let Some(values) = consumer.get(field).and_then(|value| value.as_array()) {
            result.insert(
                field.to_string(),
                serde_json::Value::Array(
                    values.iter().take(4).map(compact_fuel_connection).collect(),
                ),
            );
        }
    }
    if let Some(sources) = consumer
        .get("candidate_sources")
        .and_then(|value| value.as_array())
    {
        result.insert(
            "candidate_sources".to_string(),
            serde_json::Value::Array(sources.iter().take(3).map(compact_fuel_source).collect()),
        );
    }
    if let Some(candidates) = consumer
        .get("fuel_inserter_candidates")
        .and_then(|value| value.as_array())
    {
        let candidates = candidates
            .iter()
            .take(4)
            .map(|candidate| {
                serde_json::Value::Object(copy_json_fields(
                    candidate,
                    &[
                        "inserter_position",
                        "inserter_direction",
                        "inserter_direction_name",
                        "inserter_name",
                        "pickup_tile",
                        "can_place_inserter",
                        "placement_reason",
                        "fuel_transaction_args",
                    ],
                ))
            })
            .collect();
        result.insert(
            "fuel_inserter_candidates".to_string(),
            serde_json::Value::Array(candidates),
        );
    }
    serde_json::Value::Object(result)
}

fn compact_fuel_action(action: &serde_json::Value) -> serde_json::Value {
    serde_json::Value::Object(copy_json_fields(
        action,
        &[
            "type",
            "tool",
            "target_unit_number",
            "args",
            "transaction_args",
            "follow_up",
            "source_kind",
            "source_is_proposed",
            "coal_resource_position",
            "description",
        ],
    ))
}

/// Project the internal proof graph into the small, actionable view a model
/// needs. The complete diagnosis remains available inside the controller for
/// exact verification; it is not duplicated across every consumer response.
fn compact_fuel_diagnosis(report: &serde_json::Value) -> serde_json::Value {
    let mut result = copy_json_fields(
        report,
        &["area", "consumer_count", "truncated", "guidance", "error"],
    );
    if let Some(consumers) = report.get("consumers").and_then(|value| value.as_array()) {
        let mut selected = Vec::new();
        if let Some(ready) = consumers
            .iter()
            .find(|consumer| consumer.get("ready_to_call").is_some())
        {
            selected.push(ready);
        }
        for consumer in consumers {
            if selected.len() >= 3 {
                break;
            }
            if !selected
                .iter()
                .any(|existing| std::ptr::eq(*existing, consumer))
            {
                selected.push(consumer);
            }
        }
        result.insert(
            "consumers".to_string(),
            serde_json::Value::Array(selected.into_iter().map(compact_fuel_consumer).collect()),
        );
    }
    if let Some(sources) = report.get("coal_sources") {
        let mut compact_sources = copy_json_fields(
            sources,
            &[
                "resource_tiles",
                "mining_drills_count",
                "belts_count",
                "chests_count",
            ],
        );
        for field in ["mining_drills", "belts", "chests"] {
            if let Some(values) = sources.get(field).and_then(|value| value.as_array()) {
                compact_sources
                    .entry(format!("{field}_count"))
                    .or_insert_with(|| serde_json::json!(values.len()));
                compact_sources.insert(
                    field.to_string(),
                    serde_json::Value::Array(
                        values.iter().take(3).map(compact_fuel_source).collect(),
                    ),
                );
            }
        }
        result.insert(
            "coal_sources".to_string(),
            serde_json::Value::Object(compact_sources),
        );
    }
    if let Some(actions) = report
        .get("suggested_actions")
        .and_then(|value| value.as_array())
    {
        result.insert(
            "suggested_actions".to_string(),
            serde_json::Value::Array(actions.iter().take(2).map(compact_fuel_action).collect()),
        );
    }
    serde_json::Value::Object(result)
}

fn capped_array(value: &serde_json::Value, field: &str, limit: usize) -> serde_json::Value {
    serde_json::Value::Array(
        value
            .get(field)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .take(limit)
            .cloned()
            .collect(),
    )
}

fn compact_next_action(action: &serde_json::Value) -> serde_json::Value {
    let mut result = copy_json_fields(
        action,
        &["tool", "operation", "type", "args", "reason", "description"],
    );
    if let Some(after_success) = action.get("after_success") {
        result.insert(
            "after_success".to_string(),
            serde_json::Value::Object(copy_json_fields(
                after_success,
                &["tool", "operation", "args", "reason"],
            )),
        );
    }
    serde_json::Value::Object(result)
}

fn fuel_proof_reasons(value: &serde_json::Value) -> serde_json::Value {
    const LIMIT: usize = 8;
    const CLOSED_CYCLE: &str = "closed_self_sustaining_coal_cycle";
    let mut stack = vec![value];
    let mut reasons = Vec::new();
    let mut seen = HashSet::new();
    while let Some(current) = stack.pop() {
        match current {
            serde_json::Value::Object(object) => {
                if let Some(reason) = object.get("reason").and_then(serde_json::Value::as_str) {
                    if seen.insert(reason.to_string()) {
                        if reasons.len() < LIMIT {
                            reasons.push(reason.to_string());
                        } else if reason == CLOSED_CYCLE
                            && !reasons.iter().any(|existing| existing == CLOSED_CYCLE)
                        {
                            reasons[LIMIT - 1] = reason.to_string();
                        }
                    }
                }
                stack.extend(object.values());
            }
            serde_json::Value::Array(values) => stack.extend(values),
            _ => {}
        }
    }
    serde_json::json!(reasons)
}

fn compact_fuel_topology(topology: &serde_json::Value) -> serde_json::Value {
    let mut result = copy_json_fields(
        topology,
        &[
            "success",
            "structural_success",
            "scope",
            "diagnostic_available",
            "consumer_found",
            "consumer_unit_number",
            "inserter_unit_number",
            "exact_connection_present",
            "durable_connection_verified",
            "durable_connection_reported",
            "live_supply_verified",
            "diagnostic_error",
        ],
    );
    if let Some(connection) = topology.get("connection") {
        result.insert(
            "connection".to_string(),
            compact_fuel_connection(connection),
        );
    }
    result.insert("proof_reasons".to_string(), fuel_proof_reasons(topology));
    serde_json::Value::Object(result)
}

fn compact_controller_rollback(rollback: &serde_json::Value) -> serde_json::Value {
    let mut result = copy_json_fields(
        rollback,
        &["success", "passes", "skipped", "error", "error_kind"],
    );
    for field in [
        "removed_units",
        "pending_units",
        "errors",
        "restored_rotations",
        "rotation_errors",
        "rollback_errors",
    ] {
        if let Some(values) = rollback.get(field).and_then(serde_json::Value::as_array) {
            result.insert(format!("{field}_count"), serde_json::json!(values.len()));
            result.insert(field.to_string(), capped_array(rollback, field, 8));
        }
    }
    if let Some(attempts) = rollback
        .get("attempts")
        .and_then(serde_json::Value::as_array)
    {
        result.insert(
            "attempt_count".to_string(),
            serde_json::json!(attempts.len()),
        );
    }
    if let Some(units) = rollback.get("units") {
        result.insert("units".to_string(), compact_controller_rollback(units));
    }
    if let Some(recipe) = rollback.get("recipe") {
        result.insert(
            "recipe".to_string(),
            serde_json::Value::Object(copy_json_fields(
                recipe,
                &[
                    "success",
                    "skipped",
                    "unit_number",
                    "operation",
                    "restored_recipe",
                    "error",
                ],
            )),
        );
    }
    serde_json::Value::Object(result)
}

fn compact_fuel_route(route: &serde_json::Value) -> serde_json::Value {
    let mut result = copy_json_fields(
        route,
        &[
            "success",
            "complete_route",
            "dry_run",
            "error_kind",
            "error",
            "from",
            "to",
            "built_to",
            "belt_type",
            "belt_count",
            "new_belt_count",
            "placed",
            "skipped_existing",
            "turn_count",
            "underground_count",
            "resource_tiles_observed",
            "planned_surface_resource_tiles_crossed_count",
            "materials",
            "materials_sufficient",
            "ready_to_execute",
            "material_shortfall",
            "topology",
            "endpoint_incompatibility",
            "incompatible_existing_count",
            "guidance",
        ],
    );
    if let Some(values) = route
        .get("incompatible_existing")
        .and_then(serde_json::Value::as_array)
    {
        result
            .entry("incompatible_existing_count".to_string())
            .or_insert_with(|| serde_json::json!(values.len()));
        result.insert(
            "incompatible_existing".to_string(),
            capped_array(route, "incompatible_existing", 4),
        );
    }
    if let Some(values) = route.get("errors").and_then(serde_json::Value::as_array) {
        result.insert("error_count".to_string(), serde_json::json!(values.len()));
        result.insert("errors".to_string(), capped_array(route, "errors", 8));
    }
    if let Some(next_action) = route.get("next_action") {
        result.insert("next_action".to_string(), compact_next_action(next_action));
    }
    if let Some(ready_to_call) = route.get("ready_to_call") {
        result.insert(
            "ready_to_call".to_string(),
            serde_json::Value::Object(copy_json_fields(
                ready_to_call,
                &["tool", "execute_args", "args"],
            )),
        );
    }
    if let Some(rollback) = route.get("rollback") {
        result.insert(
            "rollback".to_string(),
            compact_controller_rollback(rollback),
        );
    }
    serde_json::Value::Object(result)
}

fn compact_fuel_preflight(preflight: &serde_json::Value) -> serde_json::Value {
    let mut result = copy_json_fields(
        preflight,
        &[
            "ready",
            "materials",
            "reserved_route_tiles",
            "reserved_entity_tiles",
            "reserved_tiles",
            "new_belt_tiles",
            "bootstrap_fuel",
        ],
    );
    if let Some(routes) = preflight.get("routes") {
        let mut compact_routes = copy_json_fields(
            routes,
            &[
                "ready",
                "materials",
                "reserved_route_tiles",
                "reserved_entity_tiles",
                "reserved_tiles",
                "new_belt_tiles",
            ],
        );
        if let Some(errors) = routes.get("errors").and_then(serde_json::Value::as_array) {
            compact_routes.insert("error_count".to_string(), serde_json::json!(errors.len()));
            compact_routes.insert("errors".to_string(), capped_array(routes, "errors", 8));
        }
        result.insert(
            "routes".to_string(),
            serde_json::Value::Object(compact_routes),
        );
    }
    if let Some(placements) = preflight
        .get("placements")
        .and_then(serde_json::Value::as_array)
    {
        result.insert(
            "placement_count".to_string(),
            serde_json::json!(placements.len()),
        );
        result.insert(
            "placements".to_string(),
            serde_json::Value::Array(
                placements
                    .iter()
                    .take(4)
                    .map(|placement| {
                        serde_json::Value::Object(copy_json_fields(
                            placement,
                            &[
                                "allowed",
                                "item_name",
                                "entity_name",
                                "position",
                                "direction",
                                "error",
                            ],
                        ))
                    })
                    .collect(),
            ),
        );
    } else if let Some(placements) = preflight
        .get("placements")
        .and_then(serde_json::Value::as_object)
    {
        result.insert(
            "placement_count".to_string(),
            serde_json::json!(placements.len()),
        );
        result.insert(
            "placements".to_string(),
            serde_json::Value::Object(
                placements
                    .iter()
                    .take(4)
                    .map(|(label, placement)| {
                        (
                            label.clone(),
                            serde_json::Value::Object(copy_json_fields(
                                placement,
                                &[
                                    "allowed",
                                    "item_name",
                                    "entity_name",
                                    "position",
                                    "direction",
                                    "error",
                                ],
                            )),
                        )
                    })
                    .collect(),
            ),
        );
    }
    if let Some(errors) = preflight
        .get("errors")
        .and_then(serde_json::Value::as_array)
    {
        result.insert("error_count".to_string(), serde_json::json!(errors.len()));
        result.insert("errors".to_string(), capped_array(preflight, "errors", 8));
    }
    if let Some(endpoint) = preflight.get("endpoint_topology") {
        result.insert("endpoint_topology".to_string(), endpoint.clone());
    }
    serde_json::Value::Object(result)
}

fn compact_production_observation(observation: &serde_json::Value) -> serde_json::Value {
    let mut result = copy_json_fields(
        observation,
        &[
            "success",
            "error",
            "error_kind",
            "proof",
            "observation_ticks",
            "producer_count",
            "working_count",
            "total",
            "status_counts",
            "scope",
            "observation_call_ok",
            "target_unit_number",
            "production_applicable",
            "target_working_or_progressed",
        ],
    );
    for field in ["progressed_units", "working_units"] {
        if let Some(values) = observation.get(field).and_then(serde_json::Value::as_array) {
            result.insert(format!("{field}_count"), serde_json::json!(values.len()));
            result.insert(field.to_string(), capped_array(observation, field, 16));
        }
    }
    serde_json::Value::Object(result)
}

fn compact_fuel_transaction(report: &serde_json::Value) -> serde_json::Value {
    let mut result = copy_json_fields(
        report,
        &[
            "operation",
            "unit_number",
            "item",
            "count",
            "inventory_type",
            "temporary_startup_buffer",
            "success",
            "skipped",
            "reason",
            "error",
            "error_kind",
        ],
    );
    if let Some(inner) = report.get("report") {
        result.insert(
            "report".to_string(),
            serde_json::Value::Object(copy_json_fields(
                inner,
                &[
                    "success",
                    "requested",
                    "available",
                    "removed",
                    "inserted",
                    "returned",
                    "partial",
                    "error",
                    "error_kind",
                ],
            )),
        );
    }
    serde_json::Value::Object(result)
}

fn compact_source_tap(report: &serde_json::Value) -> serde_json::Value {
    let mut result = copy_json_fields(
        report,
        &[
            "success",
            "source_unit_number",
            "source_direction_preserved",
            "unit_number",
            "layout",
            "route_start_matches_drop",
            "branch_extend_existing",
            "filter_readback_verified",
            "filter_atomic_with_placement",
            "allowed_items",
            "self_fueling_live",
            "source_preservation",
            "error",
            "error_kind",
        ],
    );
    for field in ["topology", "final_topology"] {
        if let Some(topology) = report.get(field) {
            result.insert(field.to_string(), compact_fuel_topology(topology));
        }
    }
    serde_json::Value::Object(result)
}

fn compact_burner_state(state: &serde_json::Value) -> serde_json::Value {
    serde_json::Value::Object(copy_json_fields(
        state,
        &[
            "unit_number",
            "name",
            "type",
            "surface_index",
            "fuel_inventory",
            "fuel_total",
            "currently_burning",
            "remaining_burning_fuel",
            "heat",
            "cold",
        ],
    ))
}

fn compact_atomic_filtered_placement(report: &serde_json::Value) -> serde_json::Value {
    let mut result = copy_json_fields(
        report,
        &[
            "success",
            "error",
            "error_kind",
            "unit_number",
            "name",
            "position",
            "atomic_filter_configuration",
            "atomic_outcome_known",
            "entity_created",
        ],
    );
    if let Some(placement) = report.get("placement") {
        result.insert(
            "placement".to_string(),
            serde_json::Value::Object(copy_json_fields(
                placement,
                &["unit_number", "name", "position", "direction"],
            )),
        );
    }
    if let Some(filter) = report.get("filter") {
        result.insert(
            "filter".to_string(),
            serde_json::Value::Object(copy_json_fields(
                filter,
                &[
                    "success",
                    "error",
                    "error_kind",
                    "unit_number",
                    "atomic_with_placement",
                    "readback_verified",
                    "entity_identity_preserved",
                    "filtering_enabled",
                    "mode",
                    "filters",
                    "held_stack_before",
                    "held_stack_after",
                    "held_stack_present_before",
                    "held_stack_present_after",
                    "held_stack_violated_whitelist",
                    "held_stack_evacuated",
                    "held_stack_returned_count",
                ],
            )),
        );
    }
    if let Some(rollback) = report.get("rollback") {
        result.insert(
            "rollback".to_string(),
            serde_json::Value::Object(copy_json_fields(
                rollback,
                &[
                    "success",
                    "entity_removed",
                    "item_returned",
                    "cleanup_completed",
                    "error",
                ],
            )),
        );
    }
    serde_json::Value::Object(result)
}

fn compact_fuel_rollback(rollback: &serde_json::Value) -> serde_json::Value {
    let mut result = copy_json_fields(
        rollback,
        &["success", "transaction_fuel_cleared", "error", "error_kind"],
    );
    if let Some(consumer_state) = rollback.get("consumer_state") {
        let mut state = copy_json_fields(
            consumer_state,
            &[
                "success",
                "classification",
                "consumer_unit_number",
                "feeder_unit_number",
                "feeder_quiesced",
                "feeder_error",
                "consumer_state_restored",
                "transaction_fuel_cleared",
                "identity_valid",
                "returned_excess",
                "spilled_excess",
                "unrecovered_excess",
                "active_fuel_voided",
                "error",
                "error_kind",
            ],
        );
        for field in ["before", "expected", "after"] {
            if let Some(value) = consumer_state.get(field) {
                state.insert(field.to_string(), compact_burner_state(value));
            }
        }
        if let Some(errors) = consumer_state
            .get("restore_errors")
            .and_then(serde_json::Value::as_array)
        {
            state.insert(
                "restore_error_count".to_string(),
                serde_json::json!(errors.len()),
            );
            state.insert(
                "restore_errors".to_string(),
                capped_array(consumer_state, "restore_errors", 8),
            );
        }
        result.insert(
            "consumer_state".to_string(),
            serde_json::Value::Object(state),
        );
    }
    if let Some(infrastructure) = rollback.get("infrastructure") {
        result.insert(
            "infrastructure".to_string(),
            compact_controller_rollback(infrastructure),
        );
    }
    if let Some(atomic_cleanup) = rollback.get("atomic_cleanup") {
        result.insert(
            "atomic_cleanup".to_string(),
            serde_json::Value::Object(copy_json_fields(
                atomic_cleanup,
                &[
                    "success",
                    "outcome_known",
                    "remote_success",
                    "unit_number",
                    "no_entity_created",
                    "local_rollback_verified",
                    "local_rollback",
                    "host_cleanup_attempted",
                    "host_exact_unit_removed",
                    "error",
                    "error_kind",
                ],
            )),
        );
    }
    serde_json::Value::Object(result)
}

fn compact_fuel_infrastructure(report: &serde_json::Value) -> serde_json::Value {
    let mut result = copy_json_fields(
        report,
        &["success", "scope", "route", "inserter", "endpoint_topology"],
    );
    if let Some(topology) = report.get("durable_fuel_topology") {
        result.insert(
            "durable_fuel_topology".to_string(),
            compact_fuel_topology(topology),
        );
    }
    if let Some(source_tap) = report.get("source_tap") {
        result.insert("source_tap".to_string(), compact_source_tap(source_tap));
    }
    if let Some(observation) = report.get("delivery_observation") {
        result.insert(
            "delivery_observation".to_string(),
            serde_json::Value::Object(copy_json_fields(
                observation,
                &[
                    "success",
                    "scope",
                    "terminal_coal_observed",
                    "exact_feeder_transfer_observed",
                    "delivery_path_operational",
                    "route_transit_tiles",
                    "certified_upstream_hops",
                    "waited_ticks",
                    "budget_ticks",
                ],
            )),
        );
    }
    serde_json::Value::Object(result)
}

fn compact_fuel_repair(repair: &serde_json::Value) -> serde_json::Value {
    let mut result = copy_json_fields(
        repair,
        &[
            "success",
            "error_kind",
            "error",
            "placement_success",
            "dry_run",
            "consumer",
            "consumer_unit_number",
            "inserter",
            "automation_verified",
            "guidance",
        ],
    );
    if let Some(route) = repair.get("route") {
        result.insert("route".to_string(), compact_fuel_route(route));
    }
    if let Some(atomic_placement) = repair.get("atomic_placement") {
        result.insert(
            "atomic_placement".to_string(),
            compact_atomic_filtered_placement(atomic_placement),
        );
    }
    if let Some(preflight) = repair.get("preflight") {
        result.insert("preflight".to_string(), compact_fuel_preflight(preflight));
    }
    for field in [
        "bootstrap_fuel",
        "bootstrap_source_tap_fuel",
        "bootstrap_consumer_fuel",
    ] {
        if let Some(value) = repair.get(field) {
            result.insert(field.to_string(), compact_fuel_transaction(value));
        }
    }
    if let Some(source_tap) = repair.get("source_tap") {
        result.insert("source_tap".to_string(), compact_source_tap(source_tap));
    }
    if let Some(source_preservation) = repair.get("source_preservation") {
        result.insert(
            "source_preservation".to_string(),
            source_preservation.clone(),
        );
    }
    if let Some(infrastructure) = repair.get("infrastructure_verified") {
        result.insert(
            "infrastructure_verified".to_string(),
            compact_fuel_infrastructure(infrastructure),
        );
    }
    if let Some(observation) = repair.get("delivery_observation") {
        result.insert(
            "delivery_observation".to_string(),
            serde_json::Value::Object(copy_json_fields(
                observation,
                &[
                    "success",
                    "scope",
                    "terminal_coal_observed",
                    "exact_feeder_transfer_observed",
                    "delivery_path_operational",
                    "route_transit_tiles",
                    "certified_upstream_hops",
                    "waited_ticks",
                    "budget_ticks",
                ],
            )),
        );
    }
    for field in ["production_verified", "verification"] {
        if let Some(value) = repair.get(field) {
            result.insert(field.to_string(), compact_production_observation(value));
        }
    }
    if let Some(diagnosis) = repair.get("fuel_diagnosis") {
        result.insert(
            "fuel_diagnosis".to_string(),
            compact_fuel_diagnosis(diagnosis),
        );
    }
    if let Some(snapshot) = repair.get("consumer_snapshot") {
        result.insert(
            "consumer_snapshot".to_string(),
            compact_burner_state(snapshot),
        );
    }
    if let Some(rollback) = repair.get("rollback") {
        result.insert("rollback".to_string(), compact_fuel_rollback(rollback));
    }
    if let Some(next_action) = repair.get("next_action") {
        result.insert("next_action".to_string(), compact_next_action(next_action));
    }
    if let Some(steps) = repair.get("steps").and_then(serde_json::Value::as_array) {
        result.insert("step_count".to_string(), serde_json::json!(steps.len()));
        result.insert(
            "steps".to_string(),
            serde_json::Value::Array(
                steps
                    .iter()
                    .take(8)
                    .map(|step| {
                        serde_json::Value::Object(copy_json_fields(
                            step,
                            &[
                                "tool",
                                "operation",
                                "args",
                                "required",
                                "unit_number",
                                "item",
                                "count",
                            ],
                        ))
                    })
                    .collect(),
            ),
        );
    }
    if let Some(repair_hint) = repair.get("repair_hint") {
        let mut hint = copy_json_fields(repair_hint, &["context", "if_success", "anti_pattern"]);
        if let Some(actions) = repair_hint
            .get("if_failed")
            .and_then(serde_json::Value::as_array)
        {
            hint.insert(
                "if_failed".to_string(),
                serde_json::Value::Array(actions.iter().take(8).map(compact_next_action).collect()),
            );
        }
        result.insert("repair_hint".to_string(), serde_json::Value::Object(hint));
    }
    serde_json::Value::Object(result)
}

fn ready_fuel_supply_args(report: &serde_json::Value) -> Option<BuildFuelSupplyParams> {
    let consumers = report.get("consumers")?.as_array()?;
    for consumer in consumers {
        if let Some(ready) = consumer.get("ready_to_call") {
            for key in ["transaction_args", "args"] {
                if let Some(args) = ready.get(key) {
                    if let Ok(params) =
                        serde_json::from_value::<BuildFuelSupplyParams>(args.clone())
                    {
                        return Some(params);
                    }
                }
            }
        }
    }
    let actions = report.get("suggested_actions")?.as_array()?;
    for action in actions {
        if let Some(args) = action.get("transaction_args") {
            if let Ok(params) = serde_json::from_value::<BuildFuelSupplyParams>(args.clone()) {
                return Some(params);
            }
        }
        let is_build_fuel_supply = action
            .get("tool")
            .and_then(|value| value.as_str())
            .map(|tool| tool == "build_fuel_supply")
            .unwrap_or(false)
            || action
                .get("type")
                .and_then(|value| value.as_str())
                .map(|action_type| action_type == "build_fuel_supply")
                .unwrap_or(false);
        if !is_build_fuel_supply {
            continue;
        }
        if let Some(args) = action.get("args") {
            if let Ok(params) = serde_json::from_value::<BuildFuelSupplyParams>(args.clone()) {
                return Some(params);
            }
        }
    }
    None
}

/// Parameters for remove_entity tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RemoveEntityParams {
    pub unit_number: u32,
    #[serde(default)]
    pub dry_run: bool,
}

/// Parameters for rotate_entity tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RotateEntityParams {
    /// Entity unit number to rotate
    pub unit_number: u32,
    /// Direction: "north", "east", "south", "west" (or shorthand/numeric).
    /// For inserters this is the pickup side; the item drops on the opposite side.
    pub direction: String,
}

/// Parameters for configuring an existing inserter's complete whitelist.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ConfigureInserterParams {
    /// Exact existing inserter unit number.
    pub unit_number: u32,
    /// Complete whitelist; [] clears and disables filtering.
    pub allowed_items: Vec<String>,
}

/// Machine connection query.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct MachineBeltPositionsParams {
    /// Exact machine unit number.
    pub unit_number: u32,
}

/// Parameters for execute_lua tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ExecuteLuaParams {
    /// Lua code to execute
    pub lua: String,
}

/// Parameters for broadcast_thought tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BroadcastThoughtParams {
    /// The message/thought to broadcast
    pub message: String,
}

/// Parameters for belt lane contents tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BeltLaneContentsParams {
    /// X coordinate of area center
    pub x: i32,
    /// Y coordinate of area center
    pub y: i32,
    /// Radius around center
    #[serde(default = "default_belt_radius")]
    pub radius: u32,
}

fn default_belt_radius() -> u32 {
    30
}

/// Parameters for sushi detection tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SushiDetectParams {
    /// X coordinate of area center
    pub x: i32,
    /// Y coordinate of area center
    pub y: i32,
    /// Radius around center
    #[serde(default = "default_radius")]
    pub radius: u32,
}

/// Parameters for belt source tracing tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct BeltSourcesParams {
    /// X coordinate of belt to trace
    pub x: i32,
    /// Y coordinate of belt to trace
    pub y: i32,
    /// Radius to search for connected belts and entities
    #[serde(default = "default_radius")]
    pub radius: u32,
}

/// Parameters for start_research tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct StartResearchParams {
    /// Technology name to research (e.g., 'automation', 'logistics')
    pub technology: String,
}

/// Parameters for feed_lab_from_inventory tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct FeedLabFromInventoryParams {
    /// Target lab unit number
    pub lab_unit_number: u32,
    /// Science pack item name to transfer from the agent inventory
    pub science_pack: String,
    /// Pack count, from 1 through 200.
    #[serde(default = "default_count")]
    pub count: u32,
    /// If true, only validate and return an execution step. Defaults to true.
    #[serde(default = "default_true")]
    pub dry_run: bool,
}

fn default_true() -> bool {
    true
}

/// Parameters for power status tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct PowerStatusParams {
    /// X coordinate to search near
    pub x: i32,
    /// Y coordinate to search near
    pub y: i32,
    /// Radius to search for electric poles
    #[serde(default = "default_power_radius")]
    pub radius: u32,
}

fn default_power_radius() -> u32 {
    50
}

/// Parameters for find_power_issues tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct FindPowerIssuesParams {
    /// X coordinate of area center
    pub x: i32,
    /// Y coordinate of area center
    pub y: i32,
    /// Radius around center to check
    #[serde(default = "default_power_radius")]
    pub radius: u32,
}

/// Parameters for steam-power layout planning.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct PlanSteamPowerParams {
    /// Left X coordinate of known or suspected water area
    pub water_x1: f64,
    /// Top Y coordinate of known or suspected water area
    pub water_y1: f64,
    /// Right X coordinate of known or suspected water area
    pub water_x2: f64,
    /// Bottom Y coordinate of known or suspected water area
    pub water_y2: f64,
    /// X coordinate that should receive power, such as a lab or factory core
    pub target_x: f64,
    /// Y coordinate that should receive power, such as a lab or factory core
    pub target_y: f64,
}

/// Parameters for dry-run steam-power repair planning.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RepairSteamPowerParams {
    /// X coordinate of repair/diagnostic area center
    pub x: i32,
    /// Y coordinate of repair/diagnostic area center
    pub y: i32,
    /// Radius around center to diagnose and repair-plan
    #[serde(default = "default_power_radius")]
    pub radius: u32,
    /// X coordinate that should ultimately receive power
    pub target_x: f64,
    /// Y coordinate that should ultimately receive power
    pub target_y: f64,
}

/// Parameters for dry-run power extension planning.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ExtendPowerToParams {
    /// X coordinate of the existing grid search area center
    pub x: i32,
    /// Y coordinate of the existing grid search area center
    pub y: i32,
    /// Radius around center to search for existing electric poles
    #[serde(default = "default_power_radius")]
    pub radius: u32,
    /// X coordinate that should receive power
    pub target_x: f64,
    /// Y coordinate that should receive power
    pub target_y: f64,
}

/// Parameters for alerts tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct AlertsParams {
    /// X coordinate of area center
    pub x: i32,
    /// Y coordinate of area center
    pub y: i32,
    /// Radius around center to check for alerts
    #[serde(default = "default_radius")]
    pub radius: u32,
}

/// Parameters for sending an agent response to the in-game chat UI.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SendChatResponseParams {
    /// Player index receiving the response.
    pub player_index: u32,
    /// Agent/tab name to route the response under.
    pub agent_name: String,
    /// Text to display.
    pub text: String,
}

/// Parameters for updating the visible tool status for an agent.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ToolStatusParams {
    /// Player index whose UI receives the status.
    pub player_index: u32,
    /// Agent/tab name.
    pub agent_name: String,
    /// Tool name to display.
    pub tool_name: String,
}

/// Parameters for setting the visible bridge status text.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SetStatusParams {
    /// Player index whose UI receives the status.
    pub player_index: u32,
    /// Formatted status text.
    pub status: String,
}

/// Parameters for registering an agent tab in the Buddy UI.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RegisterAgentParams {
    /// Agent name.
    pub agent_name: String,
    /// Optional UI label.
    pub label: Option<String>,
}

/// Parameters for unregistering an agent tab in the Buddy UI.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct UnregisterAgentParams {
    /// Agent name.
    pub agent_name: String,
}

/// Parameters for ensuring a planet surface exists.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct EnsureSurfaceParams {
    /// Surface/planet name.
    pub planet: String,
}

/// Parameters for pre-placing an agent character.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct PlaceCharacterParams {
    /// Agent id/name.
    pub agent_name: String,
    /// Surface/planet name.
    pub planet: String,
    /// Spawn X coordinate to avoid overlapping other agents.
    pub spawn_x: f64,
}

/// Parameters for toggling spectator mode for connecting players.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SetSpectatorModeParams {
    /// Whether spectator mode should be enabled.
    pub enabled: bool,
}

/// Parameters for querying compact live state for an agent.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct LiveStateParams {
    /// Agent id/name.
    pub agent_name: String,
}

/// Parameters for querying force-wide item production and consumption statistics.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ProductionStatisticsParams {
    /// Surface name to inspect.
    #[serde(default = "default_surface_name")]
    pub surface_name: String,
}

// === Zone Management Parameters ===

/// Parameters for create_zone tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CreateZoneParams {
    /// Unique ID for the zone
    pub id: String,
    /// Zone type: mining, smelting, assembly, power, storage, logistics, reserved, or custom:name
    pub zone_type: String,
    /// Left X coordinate of zone bounds
    pub x1: f64,
    /// Top Y coordinate of zone bounds
    pub y1: f64,
    /// Right X coordinate of zone bounds
    pub x2: f64,
    /// Bottom Y coordinate of zone bounds
    pub y2: f64,
    /// Optional description for the zone
    pub description: Option<String>,
}

/// Parameters for get_zone tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GetZoneParams {
    /// Zone ID to retrieve
    pub id: String,
}

/// Parameters for update_zone tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct UpdateZoneParams {
    /// Zone ID to update
    pub id: String,
    /// New zone type (optional)
    pub zone_type: Option<String>,
    /// New left X coordinate (optional)
    pub x1: Option<f64>,
    /// New top Y coordinate (optional)
    pub y1: Option<f64>,
    /// New right X coordinate (optional)
    pub x2: Option<f64>,
    /// New bottom Y coordinate (optional)
    pub y2: Option<f64>,
    /// New description (optional)
    pub description: Option<String>,
}

/// Parameters for delete_zone tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct DeleteZoneParams {
    /// Zone ID to delete
    pub id: String,
}

/// Parameters for list_zones tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ListZonesParams {
    /// Optional filter by zone type
    pub zone_type: Option<String>,
}

// === Resource Observation Parameters ===

/// Parameters for scan_resources tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ScanResourcesParams {
    /// X coordinate of area center
    pub x: i32,
    /// Y coordinate of area center
    pub y: i32,
    /// Radius around center to scan
    #[serde(default = "default_radius")]
    pub radius: u32,
    /// If true, save discovered resources as advisory layout context (default: true)
    #[serde(default = "default_save_as_protected")]
    pub save_as_protected: bool,
}

fn default_save_as_protected() -> bool {
    true
}

// === Layout Assistance Parameters ===

/// Parameters for check_placement tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CheckPlacementParams {
    /// Entity name to check (e.g., 'assembling-machine-1')
    pub entity_name: String,
    /// X coordinate to check
    pub x: f64,
    /// Y coordinate to check
    pub y: f64,
    /// Direction: "north", "east", "south", "west" (or shorthand "n", "e", "s", "w", or numbers 0/4/8/12)
    #[serde(default)]
    pub direction: String,
}

/// Parameters for find_build_area tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct FindBuildAreaParams {
    /// Zone type to find area for: mining, smelting, assembly, power, storage, logistics
    pub zone_type: String,
    /// Minimum width needed
    pub width: u32,
    /// Minimum height needed
    pub height: u32,
    /// X coordinate of search center
    pub x: i32,
    /// Y coordinate of search center
    pub y: i32,
    /// Maximum search radius
    #[serde(default = "default_radius")]
    pub radius: u32,
}

/// Parameters for render_map tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RenderMapParams {
    /// X coordinate of area center (default: character position)
    pub x: Option<i32>,
    /// Y coordinate of area center (default: character position)
    pub y: Option<i32>,
    /// Map radius in tiles (default: 15)
    #[serde(default = "default_map_radius")]
    pub radius: u32,
    /// Detail level: "minimal", "normal", or "detailed" (default: "normal")
    pub detail: Option<String>,
    /// Show power coverage overlay using network ID numbers (1-9)
    #[serde(default)]
    pub show_power: bool,
}

/// Parameters for save-state wedged/visual diagnostics
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct DebugWedgedStateParams {
    /// X coordinate of area center (default: character position)
    pub x: Option<i32>,
    /// Y coordinate of area center (default: character position)
    pub y: Option<i32>,
    /// Map radius in tiles (default: 15)
    #[serde(default = "default_map_radius")]
    pub radius: u32,
    /// Detail level: "minimal", "normal", or "detailed" (default: "detailed")
    pub detail: Option<String>,
    /// Show power coverage overlay using network ID numbers (1-9)
    #[serde(default)]
    pub show_power: bool,
}

fn default_map_radius() -> u32 {
    15
}

/// Parameters for get_blank_slate tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GetBlankSlateParams {
    /// X coordinate of area center
    pub x: i32,
    /// Y coordinate of area center
    pub y: i32,
    /// Radius around center
    #[serde(default = "default_radius")]
    pub radius: u32,
}

/// Parameters for clear_area tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct ClearAreaParams {
    /// Left X coordinate
    pub x1: f64,
    /// Top Y coordinate
    pub y1: f64,
    /// Right X coordinate
    pub x2: f64,
    /// Bottom Y coordinate
    pub y2: f64,
    /// Clear trees (default: true)
    #[serde(default = "default_clear_trees")]
    pub clear_trees: bool,
    /// Clear rocks (default: true)
    #[serde(default = "default_clear_rocks")]
    pub clear_rocks: bool,
    /// Dry run - preview without clearing (default: false)
    #[serde(default)]
    pub dry_run: bool,
}

fn default_clear_trees() -> bool {
    true
}
fn default_clear_rocks() -> bool {
    true
}

// === The MCP Server ===

#[derive(Clone)]
pub struct FactorioMcp {
    config: ConnectionConfig,
    client: Arc<Mutex<Option<FactorioClient>>>,
    issue_project_root: Arc<PathBuf>,
    tool_router: ToolRouter<Self>,
}

/// Chat message from a player
#[derive(Debug, Deserialize)]
struct ChatMessage {
    player: String,
    message: String,
    #[allow(dead_code)]
    tick: u64,
}

const MODEL_VISIBLE_TOOLS: &[&str] = &[
    "analyze_inserters",
    "analyze_item_flow",
    "bootstrap_burner_once",
    "bootstrap_smelting_once",
    "build_assembler_feed",
    "build_assembler_output",
    "build_automation_science",
    "build_lab_feed",
    "build_recipe_assembler_cell",
    "collect_from_chest",
    "configure_inserter",
    "craft",
    "diagnose_factory_blockers",
    "diagnose_steam_power",
    "execute_direct_smelter",
    "execute_edge_miner",
    "execute_entity_placement_near",
    "extend_power_to",
    "feed_lab_from_inventory",
    "file_issue",
    "find_nearest_resource",
    "get_available_research",
    "get_belt_lane_contents",
    "get_entities",
    "get_entity_inventory",
    "get_machine_belt_positions",
    "get_power_status",
    "get_recipe",
    "get_recipes_for_item",
    "get_research_status",
    "mine_at",
    "place_entity",
    "plan_automation_science",
    "plan_machine_output",
    "plan_recipe_assembler_cell",
    "plan_steam_power",
    "production_statistics",
    "remove_entity",
    "render_map",
    "repair_fuel_sustainability",
    "rotate_entity",
    "route_belt",
    "set_recipe",
    "situation_report",
    "start_research",
    "unstuck",
    "verify_production",
    "wait_for_crafting",
    "walk_to",
];

fn model_safe_operation_label(tool: &str) -> &'static str {
    match tool {
        "insert_items" => "load_inventory",
        "extract_items" => "collect_inventory",
        "wait_ticks" => "wait_for_process",
        "build_fuel_supply" => "durable_fuel_transaction",
        "diagnose_fuel_sustainability" => "fuel_sustainability_check",
        "analyze_belt_gaps" => "belt_flow_check",
        "analyze_belt_reach" => "belt_flow_trace",
        "build_edge_miner" => "edge_miner_plan",
        "build_direct_smelter" => "direct_smelter_plan",
        "plan_entity_placement_near" => "safe_placement_plan",
        _ => "internal_operation",
    }
}

fn model_safe_text(text: &str) -> String {
    [
        ("analyze_belt_gaps", "analyze_item_flow"),
        ("analyze_belt_reach", "analyze_item_flow"),
        ("build_fuel_supply", "repair_fuel_sustainability"),
        ("diagnose_fuel_sustainability", "repair_fuel_sustainability"),
        ("build_edge_miner", "execute_edge_miner"),
        ("build_direct_smelter", "execute_direct_smelter"),
        (
            "plan_entity_placement_near",
            "execute_entity_placement_near",
        ),
        ("insert_items", "bounded inventory load"),
        ("extract_items", "bounded inventory collection"),
        ("wait_ticks", "bounded process wait"),
    ]
    .into_iter()
    .fold(text.to_string(), |text, (hidden, replacement)| {
        text.replace(hidden, replacement)
    })
}

fn model_safe_key(key: &str) -> String {
    [
        ("build_fuel_supply", "durable_fuel_transaction"),
        ("diagnose_fuel_sustainability", "fuel_sustainability_check"),
        ("build_edge_miner", "edge_miner_plan"),
        ("build_direct_smelter", "direct_smelter_plan"),
        ("plan_entity_placement_near", "safe_placement_plan"),
        ("analyze_belt_gaps", "belt_flow_check"),
        ("analyze_belt_reach", "belt_flow_trace"),
        ("insert_items", "load_inventory"),
        ("extract_items", "collect_inventory"),
        ("wait_ticks", "wait_for_process"),
    ]
    .into_iter()
    .fold(key.to_string(), |key, (hidden, replacement)| {
        key.replace(hidden, replacement)
    })
}

fn sanitize_model_payload(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                sanitize_model_payload(value);
            }
        }
        serde_json::Value::Object(object) => {
            let entries = std::mem::take(object);
            for (key, mut value) in entries {
                if key == "tool" {
                    if let Some(tool) = value.as_str() {
                        if MODEL_VISIBLE_TOOLS.contains(&tool) {
                            object.insert(key, value);
                        } else {
                            object.entry("operation".to_string()).or_insert_with(|| {
                                serde_json::json!(model_safe_operation_label(tool))
                            });
                        }
                        continue;
                    }
                }
                sanitize_model_payload(&mut value);
                object.insert(model_safe_key(&key), value);
            }
        }
        serde_json::Value::String(text) => *text = model_safe_text(text),
        _ => {}
    }
}

fn model_safe_payload(mut value: serde_json::Value) -> serde_json::Value {
    sanitize_model_payload(&mut value);
    value
}

fn model_safe_json_text(text: String) -> String {
    match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(value) => serde_json::to_string_pretty(&model_safe_payload(value))
            .unwrap_or_else(|error| format!("Error: {error}")),
        Err(_) => model_safe_text(&text),
    }
}

impl FactorioMcp {
    fn new() -> Self {
        let mut tool_router = Self::tool_router();
        let registered: Vec<String> = tool_router
            .list_all()
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect();
        for name in registered {
            if !MODEL_VISIBLE_TOOLS.contains(&name.as_str()) {
                tool_router.remove_route(&name);
            }
        }
        Self {
            config: ConnectionConfig::from_env(),
            client: Arc::new(Mutex::new(None)),
            issue_project_root: Arc::new(
                std::env::var_os("FACTORIO_BUDDY_PROJECT_ROOT")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR"))),
            ),
            tool_router,
        }
    }

    async fn connect(&self) -> Result<FactorioClient, String> {
        let agent_id = AgentId::new(std::env::var("FACTORIO_AGENT_ID").ok().as_deref())
            .map_err(|e| format!("Invalid FACTORIO_AGENT_ID: {}", e))?;
        let mut cached = self.client.lock().await;
        if let Some(client) = cached.as_ref() {
            let mut candidate = client.clone().with_agent_id(agent_id.clone());
            if candidate.call_remote("ping", &[]).await.is_ok() {
                return Ok(candidate);
            }
            *cached = None;
        }

        let client =
            FactorioClient::connect(&self.config.host, self.config.port, &self.config.password)
                .await
                .map(|client| client.with_agent_id(agent_id))
                .map_err(|e| format!("Failed to connect: {}", e))?;
        *cached = Some(client.clone());
        Ok(client)
    }

    /// Fetch pending player messages and clear them from the queue.
    /// Returns formatted string if there are messages, None otherwise.
    async fn fetch_player_messages(&self) -> Option<String> {
        let mut client = self.connect().await.ok()?;

        let mut warning: Option<String> = None;
        if let Err(err) = client.call_remote("chat_capture_status", &[]).await {
            eprintln!("Failed to register chat handler: {}", err);
            warning = Some(format!(
                "\n\n[warning: chat handler registration failed: {}]",
                err
            ));
        }

        // Then fetch and clear messages
        let formatted_messages = match client.call_remote("get_chat_messages", &[]).await {
            Ok(response) => match serde_json::from_str::<Vec<ChatMessage>>(&response) {
                Ok(messages) if !messages.is_empty() => {
                    let formatted: Vec<String> = messages
                        .iter()
                        .map(|m| format!("[{}]: {}", m.player, m.message))
                        .collect();
                    Some(format!(
                        "\n\n--- Player Messages ---\n{}",
                        formatted.join("\n")
                    ))
                }
                _ => None,
            },
            Err(_) => None,
        };

        match (warning, formatted_messages) {
            (Some(warning), Some(messages)) => Some(format!("{}{}", warning, messages)),
            (Some(warning), None) => Some(warning),
            (None, Some(messages)) => Some(messages),
            (None, None) => None,
        }
    }

    /// Append any pending player messages to a result string
    async fn with_player_messages(&self, result: String) -> String {
        match self.fetch_player_messages().await {
            Some(msgs) => format!("{}{}", result, msgs),
            None => result,
        }
    }

    async fn call_lifecycle_remote(&self, fn_name: &str, args: &[serde_json::Value]) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return format!("Error: {}", e),
        };
        match client.call_remote(fn_name, args).await {
            Ok(result) if result.trim().is_empty() => "{\"success\":true}".to_string(),
            Ok(result) => result,
            Err(e) => format!("Error: {}", e),
        }
    }

    async fn render_ascii_map_snapshot(
        &self,
        client: &mut FactorioClient,
        center: Position,
        radius: u32,
        detail: Option<&str>,
        show_power: bool,
    ) -> Result<String, String> {
        let r = radius as f64;
        let area = Area {
            left_top: Position::new(center.x - r, center.y - r),
            right_bottom: Position::new(center.x + r, center.y + r),
        };

        let entities = client
            .find_entities(area, None, None)
            .await
            .map_err(|e| format!("Error: getting entities: {}", e))?;
        let tiles = client.get_tiles(area).await.unwrap_or_default();
        let char_pos = client.get_character_position().await.ok();
        let detail_level = match detail {
            Some("minimal") => factorioctl::cli::DetailLevel::Minimal,
            Some("normal") => factorioctl::cli::DetailLevel::Normal,
            _ => factorioctl::cli::DetailLevel::Detailed,
        };

        let power_coverage = if show_power {
            let agent_id = client.agent_id().as_str().to_string();
            match client
                .call_remote(
                    "get_power_coverage",
                    &[
                        serde_json::json!(center.x as i32),
                        serde_json::json!(center.y as i32),
                        serde_json::json!(radius),
                        serde_json::json!(agent_id),
                    ],
                )
                .await
            {
                Ok(result) => serde_json::from_str::<serde_json::Value>(&result)
                    .ok()
                    .and_then(|value| value.get("coverage").cloned())
                    .and_then(|coverage| {
                        if let serde_json::Value::Object(map) = coverage {
                            let mut parsed = std::collections::HashMap::new();
                            for (key, val) in map {
                                if let Some((x_str, y_str)) = key.split_once(',') {
                                    if let (Ok(x), Ok(y)) =
                                        (x_str.parse::<i32>(), y_str.parse::<i32>())
                                    {
                                        if let Some(id) = val.as_u64() {
                                            parsed.insert((x, y), id as u8);
                                        }
                                    }
                                }
                            }
                            Some(parsed)
                        } else {
                            None
                        }
                    }),
                Err(_) => None,
            }
        } else {
            None
        };

        Ok(factorioctl::cli::render_ascii_map(
            &entities,
            &tiles,
            &center,
            radius,
            char_pos.as_ref(),
            detail_level,
            power_coverage.as_ref(),
        ))
    }
}

#[tool_router]
impl FactorioMcp {
    /// Send an agent response to the in-game chat UI.
    #[tool(description = "NPC lifecycle tool: send an agent response to the Factorio Buddy UI.")]
    async fn send_chat_response(
        &self,
        Parameters(params): Parameters<SendChatResponseParams>,
    ) -> String {
        self.call_lifecycle_remote(
            "receive_response",
            &[
                serde_json::json!(params.player_index),
                serde_json::json!(params.agent_name),
                serde_json::json!(params.text),
            ],
        )
        .await
    }

    /// Update the visible tool status in the Buddy UI.
    #[tool(
        description = "NPC lifecycle tool: update the visible tool status in the Factorio Buddy UI."
    )]
    async fn tool_status(&self, Parameters(params): Parameters<ToolStatusParams>) -> String {
        self.call_lifecycle_remote(
            "tool_status",
            &[
                serde_json::json!(params.player_index),
                serde_json::json!(params.agent_name),
                serde_json::json!(params.tool_name),
            ],
        )
        .await
    }

    /// Set the visible status in the Buddy UI.
    #[tool(description = "NPC lifecycle tool: set the visible status in the Factorio Buddy UI.")]
    async fn set_status(&self, Parameters(params): Parameters<SetStatusParams>) -> String {
        self.call_lifecycle_remote(
            "set_status",
            &[
                serde_json::json!(params.player_index),
                serde_json::json!(params.status),
            ],
        )
        .await
    }

    /// Register an agent tab in the Buddy UI.
    #[tool(description = "NPC lifecycle tool: register an agent tab in the Factorio Buddy UI.")]
    async fn register_agent(&self, Parameters(params): Parameters<RegisterAgentParams>) -> String {
        let mut args = vec![serde_json::json!(params.agent_name)];
        if let Some(label) = params.label {
            args.push(serde_json::json!(label));
        }
        self.call_lifecycle_remote("register_agent", &args).await
    }

    /// Unregister an agent tab from the Buddy UI.
    #[tool(description = "NPC lifecycle tool: unregister an agent tab from the Factorio Buddy UI.")]
    async fn unregister_agent(
        &self,
        Parameters(params): Parameters<UnregisterAgentParams>,
    ) -> String {
        self.call_lifecycle_remote("unregister_agent", &[serde_json::json!(params.agent_name)])
            .await
    }

    /// Ensure a planet surface exists.
    #[tool(
        description = "NPC lifecycle tool: ensure a planet surface exists and return its status."
    )]
    async fn ensure_surface(&self, Parameters(params): Parameters<EnsureSurfaceParams>) -> String {
        self.call_lifecycle_remote("ensure_surface_result", &[serde_json::json!(params.planet)])
            .await
    }

    /// Pre-place an agent character on a planet surface.
    #[tool(
        description = "NPC lifecycle tool: create or teleport an agent character on a planet surface."
    )]
    async fn place_character(
        &self,
        Parameters(params): Parameters<PlaceCharacterParams>,
    ) -> String {
        self.call_lifecycle_remote(
            "pre_place_character_result",
            &[
                serde_json::json!(params.agent_name),
                serde_json::json!(params.planet),
                serde_json::json!(params.spawn_x),
            ],
        )
        .await
    }

    /// Toggle spectator mode for connecting players.
    #[tool(description = "NPC lifecycle tool: toggle spectator mode for connecting players.")]
    async fn set_spectator_mode(
        &self,
        Parameters(params): Parameters<SetSpectatorModeParams>,
    ) -> String {
        self.call_lifecycle_remote("set_spectator_mode", &[serde_json::json!(params.enabled)])
            .await
    }

    /// Ping the Factorio Buddy mod dispatcher.
    #[tool(description = "NPC lifecycle tool: ping the Factorio Buddy mod dispatcher.")]
    async fn ping(&self) -> String {
        self.call_lifecycle_remote("ping", &[]).await
    }

    /// Get compact live state for an agent.
    #[tool(description = "NPC lifecycle tool: get compact live state for an agent.")]
    async fn live_state(&self, Parameters(params): Parameters<LiveStateParams>) -> String {
        self.call_lifecycle_remote("live_state_result", &[serde_json::json!(params.agent_name)])
            .await
    }

    /// Count currently connected human players.
    #[tool(description = "NPC lifecycle tool: count currently connected human players.")]
    async fn connected_player_count(&self) -> String {
        self.call_lifecycle_remote("connected_player_count_result", &[])
            .await
    }

    /// Query force-wide item production and consumption statistics.
    #[tool(
        description = "Read force-wide item production and consumption totals plus one-minute produced, consumed, and net rates. Use this to see what the factory is actually making and using."
    )]
    async fn production_statistics(
        &self,
        Parameters(params): Parameters<ProductionStatisticsParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(client) => client,
            Err(error) => return format!("Error: {}", error),
        };
        let agent_id = client.agent_id().as_str().to_string();
        match client
            .call_remote(
                "production_statistics",
                &[
                    serde_json::json!(params.surface_name),
                    serde_json::json!(agent_id),
                ],
            )
            .await
        {
            Ok(result) => result,
            Err(error) => format!("Error: {}", error),
        }
    }

    // --- Query Tools ---

    /// Get all entities in an area. Returns entity names, positions, and types.
    #[tool(
        description = "Find entities near x,y. Prefer prototype name/type filters; narrow radius or limit when results are summarized."
    )]
    async fn get_entities(&self, Parameters(params): Parameters<GetEntitiesParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let area = Area {
            left_top: Position::new(
                params.x as f64 - params.radius as f64,
                params.y as f64 - params.radius as f64,
            ),
            right_bottom: Position::new(
                params.x as f64 + params.radius as f64,
                params.y as f64 + params.radius as f64,
            ),
        };

        let result = match client
            .find_entities(area, None, params.name.as_deref())
            .await
        {
            Ok(entities) => {
                let type_filter = params.entity_type.as_deref();
                let info: Vec<serde_json::Value> = entities
                    .into_iter()
                    .filter(|e| match type_filter {
                        Some(t) => e.entity_type.as_deref() == Some(t),
                        None => true,
                    })
                    .map(|e| {
                        // Calculate size from bounding box if available
                        let size = e.bounding_box.as_ref().map(|bb| {
                            let width = (bb.right_bottom.x - bb.left_top.x).round() as i32;
                            let height = (bb.right_bottom.y - bb.left_top.y).round() as i32;
                            serde_json::json!({ "width": width, "height": height })
                        });
                        serde_json::json!({
                            "unit_number": e.unit_number,
                            "name": e.name,
                            "type": e.entity_type,
                            "x": e.position.x,
                            "y": e.position.y,
                            "direction": e.direction,
                            "size": size,
                        })
                    })
                    .collect();
                let total = info.len();
                let limit = params.limit.clamp(1, 500);
                if total > limit {
                    let mut summary_by_name: BTreeMap<String, usize> = BTreeMap::new();
                    let mut summary_by_type: BTreeMap<String, usize> = BTreeMap::new();
                    for entity in &info {
                        if let Some(name) = entity.get("name").and_then(|v| v.as_str()) {
                            *summary_by_name.entry(name.to_string()).or_insert(0) += 1;
                        }
                        if let Some(entity_type) = entity.get("type").and_then(|v| v.as_str()) {
                            *summary_by_type.entry(entity_type.to_string()).or_insert(0) += 1;
                        }
                    }
                    let page: Vec<serde_json::Value> = info.into_iter().take(limit).collect();
                    let capped = serde_json::json!({
                        "truncated": true,
                        "total": total,
                        "returned": page.len(),
                        "limit": limit,
                        "summary_by_name": summary_by_name,
                        "summary_by_type": summary_by_type,
                        "entities": page,
                        "guidance": "Narrow by name/entity_type, reduce radius, or pass limit for a smaller page."
                    });
                    serde_json::to_string_pretty(&capped)
                        .unwrap_or_else(|e| format!("Error: {}", e))
                } else {
                    serde_json::to_string_pretty(&info).unwrap_or_else(|e| format!("Error: {}", e))
                }
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Inspect supported item inventories on one exact entity.
    #[tool(
        description = "Read an entity's supported inventories. Before collect_from_chest, inspect inventories.chest; its absence or emptiness proves a container empty. An item-specific miss does not."
    )]
    async fn get_entity_inventory(
        &self,
        Parameters(params): Parameters<GetEntityInventoryParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(client) => client,
            Err(error) => return self.with_player_messages(format!("Error: {error}")).await,
        };

        let result = match client.get_entity_inventory(params.unit_number).await {
            Ok(inventory) => serde_json::to_string_pretty(&inventory)
                .unwrap_or_else(|error| format!("Error: {error}")),
            Err(error) => format!("Error: {error}"),
        };
        self.with_player_messages(result).await
    }

    /// Get belt and inserter positions for a machine.
    #[tool(
        description = "Return exact belt/inserter tiles for a machine. Drills include drop/output tiles; furnaces and assemblers include input/output tiles. Use before routing."
    )]
    async fn get_machine_belt_positions(
        &self,
        Parameters(params): Parameters<MachineBeltPositionsParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        // Get the entity
        let entity = match client.get_entity(params.unit_number).await {
            Ok(e) => e,
            Err(e) => {
                return self
                    .with_player_messages(format!("Error: getting entity: {}", e))
                    .await
            }
        };

        // Check if this is a mining drill - they have special drop position handling
        let is_drill = entity.name.contains("mining-drill");

        if is_drill {
            // For drills, query the actual drop_position from Factorio
            let drop_result = match client
                .call_remote(
                    "get_entity_drop_position",
                    &[serde_json::json!(params.unit_number)],
                )
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    return self
                        .with_player_messages(format!("Error: querying drop position: {}", e))
                        .await
                }
            };

            // Parse the drop position result
            if let Ok(drop_info) = serde_json::from_str::<serde_json::Value>(&drop_result) {
                if let Some(error) = drop_info.get("error") {
                    return self.with_player_messages(format!("Error: {}", error)).await;
                }

                let drop_x = drop_info["drop_x"].as_f64().unwrap_or(0.0);
                let drop_y = drop_info["drop_y"].as_f64().unwrap_or(0.0);
                let drill_dir = drop_info["drill_direction"].as_u64().unwrap_or(0) as u8;

                // Calculate the tile where a belt should be placed
                // Items drop at a position, belt tile is floor of that position
                let belt_tile_x = drop_x.floor() as i32;
                let belt_tile_y = drop_y.floor() as i32;

                // Belt direction should carry items away from drill
                // Drill direction: 0=N, 4=E, 8=S, 12=W
                // If drill faces East, belt should go East (or turn)
                let belt_direction = drill_dir;
                let dir_name = match drill_dir {
                    0 => "North",
                    4 => "East",
                    8 => "South",
                    12 => "West",
                    _ => "Unknown",
                };

                let result = serde_json::json!({
                    "entity_type": "mining-drill",
                    "drill": {
                        "unit_number": entity.unit_number,
                        "name": entity.name,
                        "position": { "x": entity.position.x, "y": entity.position.y },
                        "facing": dir_name,
                        "direction": drill_dir
                    },
                    "output": {
                        "drop_position": { "x": drop_x, "y": drop_y },
                        "belt_tile": { "x": belt_tile_x, "y": belt_tile_y },
                        "belt_direction": belt_direction,
                        "description": format!(
                            "Place belt at tile ({}, {}) facing {} (direction={}) to catch drill output",
                            belt_tile_x, belt_tile_y, dir_name, belt_direction
                        )
                    },
                    "routing_tip": format!(
                        "To connect this drill: route_belt from_x={} from_y={} to_x=<destination> to_y=<destination>",
                        belt_tile_x, belt_tile_y
                    )
                });

                return self
                    .with_player_messages(serde_json::to_string_pretty(&result).unwrap_or_default())
                    .await;
            } else {
                // Lua failed - calculate output position from direction and size
                // Burner-mining-drills are 2x2, electric are 3x3
                let drill_size = if entity.name.contains("burner") { 2 } else { 3 };
                let half_size = drill_size / 2;
                let cx = entity.position.x.floor() as i32;
                let cy = entity.position.y.floor() as i32;

                // Calculate belt tile based on direction
                // Empirically tested drop positions for 2x2 burner drills:
                //   North at (36,-102) -> drops at (35.5,-103.3) -> belt at (35,-104)
                //   East at (42,-102) -> drops at (43.3,-102.5) -> belt at (43,-103)
                //   South at (48,-102) -> drops at (48.5,-100.7) -> belt at (48,-101)
                //   West at (54,-102) -> drops at (52.7,-101.5) -> belt at (52,-102)
                let (belt_x, belt_y, dir_name) = match entity.direction {
                    0 => (cx - 1, cy - half_size - 1, "North"), // North
                    4 => (cx + half_size, cy - 1, "East"),      // East
                    8 => (cx, cy + half_size, "South"),         // South
                    12 => (cx - half_size - 1, cy, "West"),     // West
                    _ => (cx + half_size, cy - 1, "East"),      // Default to east
                };

                let result = serde_json::json!({
                    "entity_type": "mining-drill",
                    "drill": {
                        "unit_number": entity.unit_number,
                        "name": entity.name,
                        "position": { "x": cx, "y": cy },
                        "facing": dir_name,
                        "direction": entity.direction,
                        "size": drill_size
                    },
                    "output": {
                        "belt_tile": { "x": belt_x, "y": belt_y },
                        "belt_direction": entity.direction,
                        "description": format!(
                            "Place belt at tile ({}, {}) facing {} to catch drill output",
                            belt_x, belt_y, dir_name
                        )
                    },
                    "routing_tip": format!(
                        "To connect this drill: route_belt from_x={} from_y={} to_x=<destination> to_y=<destination>",
                        belt_x, belt_y
                    ),
                    "note": "Belt tile calculated from drill size and direction"
                });

                return self
                    .with_player_messages(serde_json::to_string_pretty(&result).unwrap_or_default())
                    .await;
            }
        }

        let bbox = match machine_bounding_box(&entity) {
            Ok(bbox) => bbox,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let width = bbox.right_bottom.x - bbox.left_top.x;
        let height = bbox.right_bottom.y - bbox.left_top.y;
        let south = match machine_side_layout(&entity, "south") {
            Ok(layout) => layout,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let north = match machine_side_layout(&entity, "north") {
            Ok(layout) => layout,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = serde_json::json!({
            "entity_type": "machine",
            "machine": {
                "unit_number": entity.unit_number,
                "name": entity.name,
                "position": { "x": entity.position.x, "y": entity.position.y },
                "bounding_box": {
                    "left_top": { "x": bbox.left_top.x, "y": bbox.left_top.y },
                    "right_bottom": { "x": bbox.right_bottom.x, "y": bbox.right_bottom.y }
                },
                "size": { "width": width, "height": height }
            },
            "input": {
                "side": "south",
                "belt_tile": { "x": south.belt_x, "y": south.belt_y },
                "belt_route_endpoint": {
                    "role": "to",
                    "x": south.belt_x,
                    "y": south.belt_y
                },
                "inserter_position": { "x": south.inserter_x, "y": south.inserter_y },
                "inserter_place_entity_args": {
                    "entity_name": "inserter",
                    "x": south.inserter_x,
                    "y": south.inserter_y,
                    "direction": south.input_direction
                },
                "belt_tile_y": south.belt_y,
                "inserter_tile_y": south.inserter_y,
                "inserter_direction": south.input_direction,
                "description": format!(
                    "Input on south side: place belt at ({}, {}) and inserter at ({:.1}, {:.1}) facing {} to pick from belt and drop into machine",
                    south.belt_x, south.belt_y, south.inserter_x, south.inserter_y, south.input_direction
                )
            },
            "output": {
                "side": "north",
                "belt_tile": { "x": north.belt_x, "y": north.belt_y },
                "belt_route_endpoint": {
                    "role": "from",
                    "x": north.belt_x,
                    "y": north.belt_y
                },
                "inserter_position": { "x": north.inserter_x, "y": north.inserter_y },
                "inserter_place_entity_args": {
                    "entity_name": "inserter",
                    "x": north.inserter_x,
                    "y": north.inserter_y,
                    "direction": north.output_direction
                },
                "belt_tile_y": north.belt_y,
                "inserter_tile_y": north.inserter_y,
                "inserter_direction": north.output_direction,
                "description": format!(
                    "Output on north side: place inserter at ({:.1}, {:.1}) facing {} to pick from machine and drop to belt at ({}, {})",
                    north.inserter_x, north.inserter_y, north.output_direction, north.belt_x, north.belt_y
                )
            },
            "routing_tip": format!(
                "For a row of furnaces at y={}: route input belt to y={}, route output belt to y={}",
                entity.position.y, south.belt_y, north.belt_y
            ),
            "coordinate_note": "Use belt_route_endpoint as a route_belt endpoint; direct belt placement is disabled. Inserter place_entity args may be used directly. Belt tiles and inserter centers are intentionally different and should not collide."
        });

        self.with_player_messages(serde_json::to_string_pretty(&result).unwrap_or_default())
            .await
    }

    /// Render an ASCII map of an area.
    #[tool(
        description = "Render an ASCII map showing entities in an area. Returns a visual representation \
        useful for understanding layouts at a glance. Legend: @=you ^v<>=belt D=drill F=furnace A=assembler \
        i=inserter I=iron C=copper c=coal S=stone B=chest P=pole ~=water X=wreck o=rock. \
        Use show_power=true to overlay power coverage with network ID numbers (1-9)."
    )]
    async fn render_map(&self, Parameters(params): Parameters<RenderMapParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        // Get center position - use provided or character position
        let center = if let (Some(x), Some(y)) = (params.x, params.y) {
            Position::new(x as f64 + 0.5, y as f64 + 0.5)
        } else {
            match client.get_character_position().await {
                Ok(pos) => pos,
                Err(e) => {
                    return self
                        .with_player_messages(format!("Error: getting position: {}", e))
                        .await
                }
            }
        };

        let map = match self
            .render_ascii_map_snapshot(
                &mut client,
                center,
                params.radius,
                params.detail.as_deref(),
                params.show_power,
            )
            .await
        {
            Ok(map) => map,
            Err(e) => e,
        };
        self.with_player_messages(map).await
    }

    /// Capture one read-only visual/collision snapshot for wedged-state debugging.
    #[tool(
        description = "Read-only save-state debug probe for stuck NPCs. Returns current position, is_player_blocked collision diagnostics, dry-run unstuck recommendation, optional can_stand_at for the map center, and a local ASCII map with @ marking the character. Use when the NPC is standing still, appears under an entity, or screenshots suggest the character is physically wedged."
    )]
    async fn debug_wedged_state(
        &self,
        Parameters(params): Parameters<DebugWedgedStateParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let character_position = match client.get_character_position().await {
            Ok(pos) => pos,
            Err(e) => {
                return self
                    .with_player_messages(format!("Error: getting position: {}", e))
                    .await
            }
        };
        let center = if let (Some(x), Some(y)) = (params.x, params.y) {
            Position::new(x as f64 + 0.5, y as f64 + 0.5)
        } else {
            character_position
        };
        let radius = params.radius.clamp(1, 30);

        let blocked = match client.is_player_blocked(radius.min(12)).await {
            Ok(value) => value,
            Err(e) => serde_json::json!({ "error": e.to_string() }),
        };
        let current_stand = match client
            .can_stand_at(character_position, radius.min(12))
            .await
        {
            Ok(value) => value,
            Err(e) => serde_json::json!({ "error": e.to_string() }),
        };
        let center_stand = match client.can_stand_at(center, radius.min(12)).await {
            Ok(value) => value,
            Err(e) => serde_json::json!({ "error": e.to_string() }),
        };
        let unstuck_preview = match client.unstuck(radius.min(12), true).await {
            Ok(value) => value,
            Err(e) => serde_json::json!({ "error": e.to_string() }),
        };
        let map = match self
            .render_ascii_map_snapshot(
                &mut client,
                center,
                radius,
                params.detail.as_deref(),
                params.show_power,
            )
            .await
        {
            Ok(map) => map,
            Err(e) => format!("Error: rendering map: {}", e),
        };

        let result = serde_json::json!({
            "success": true,
            "character_position": {
                "x": character_position.x,
                "y": character_position.y,
            },
            "map_center": {
                "x": center.x,
                "y": center.y,
            },
            "radius": radius,
            "blocked": blocked,
            "current_stand": current_stand,
            "center_stand": center_stand,
            "unstuck_preview": unstuck_preview,
            "visual": {
                "format": "ascii_map",
                "legend": "@=agent, D=drill, F=furnace, i=inserter, P=pole, ^=belt north, >=belt east, v=belt south, <=belt west, I=iron, C=copper, c=coal, S=stone, ~=water",
                "map": map,
            },
            "guidance": "If blocked.blocked is true or blocked.blocker_count > 0, call unstuck with dry_run=false or walk_to the first unstuck_preview candidate before placing more entities nearby."
        });
        let rendered =
            serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(rendered).await
    }

    /// Get resource patches (ore, oil) in an area.
    #[tool(
        description = "Get resource patches (ore, oil) in an area. Returns patch locations and amounts."
    )]
    async fn get_resources(&self, Parameters(params): Parameters<GetResourcesParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let area = Area {
            left_top: Position::new(
                params.x as f64 - params.radius as f64,
                params.y as f64 - params.radius as f64,
            ),
            right_bottom: Position::new(
                params.x as f64 + params.radius as f64,
                params.y as f64 + params.radius as f64,
            ),
        };

        let result = match client
            .find_resources(area, params.resource_type.as_deref())
            .await
        {
            Ok(resources) => {
                let info: Vec<serde_json::Value> = resources
                    .into_iter()
                    .map(|r| {
                        serde_json::json!({
                            "name": r.name,
                            "center_x": r.center.x,
                            "center_y": r.center.y,
                            "total_amount": r.total_amount,
                            "tile_count": r.tile_count,
                        })
                    })
                    .collect();
                serde_json::to_string_pretty(&info).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Find the nearest resource patch of a specific type.
    #[tool(
        description = "Find a resource across all generated chunks. Set explore_radius to generate and search nearby terrain."
    )]
    async fn find_nearest_resource(
        &self,
        Parameters(params): Parameters<FindNearestResourceParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        // Get search origin - use provided position or character position
        let from = if let (Some(x), Some(y)) = (params.x, params.y) {
            Position::new(x, y)
        } else {
            match client.get_character_position().await {
                Ok(pos) => pos,
                Err(e) => {
                    return self
                        .with_player_messages(format!("Error: getting position: {}", e))
                        .await
                }
            }
        };

        let result = match client
            .find_nearest_resource_report(&params.resource_type, from, params.explore_radius)
            .await
        {
            Ok(report) => {
                serde_json::to_string_pretty(&report).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Get current character status including position and health.
    #[tool(
        description = "Get current character status including position, health, and walking state. TIP: Only check when you need to - avoid over-verifying after every action."
    )]
    async fn get_character(&self) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client.character_status().await {
            Ok(status) => {
                let info = serde_json::json!({
                    "valid": status.valid,
                    "x": status.position.as_ref().map(|p| p.x),
                    "y": status.position.as_ref().map(|p| p.y),
                    "health": status.health,
                    "walking": status.walking,
                });
                serde_json::to_string_pretty(&info).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Check whether the character can stand at a position.
    #[tool(
        description = "Read-only collision diagnostic: check whether the agent character can stand at a world position. Returns blockers plus nearby clear positions and a walk_to recommendation when blocked. Use before placing entities near the character or when diagnosing wedged/stuck movement."
    )]
    async fn can_stand_at(&self, Parameters(params): Parameters<CanStandAtParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let radius = params.radius.clamp(1, 12);
        let position = Position::new(params.x, params.y);
        let result = match client.can_stand_at(position, radius).await {
            Ok(value) => {
                serde_json::to_string_pretty(&value).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Diagnose whether the current character position is blocked.
    #[tool(
        description = "Read-only unstuck diagnostic: report whether the agent character is currently blocked by entity collision, list blockers, and suggest nearby clear walk_to positions. Use when the agent is standing still, placing entities on itself, or movement/placement seems wedged."
    )]
    async fn is_player_blocked(
        &self,
        Parameters(params): Parameters<PlayerBlockedParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let radius = params.radius.clamp(1, 12);
        let result = match client.is_player_blocked(radius).await {
            Ok(value) => {
                serde_json::to_string_pretty(&value).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Move the character out of a physical collision wedge.
    #[tool(
        description = "Recovery action for a physically wedged agent character. Checks the current character footprint, finds the nearest verified clear standing position, and clears stale walk/mining state. Pass dry_run=true to diagnose and preview without moving."
    )]
    async fn unstuck(&self, Parameters(params): Parameters<UnstuckParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let radius = params.radius.clamp(1, 12);
        let result = match client.unstuck(radius, params.dry_run).await {
            Ok(value) => {
                serde_json::to_string_pretty(&value).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Get character inventory contents.
    #[tool(description = "Get character inventory contents. Returns item names and counts.")]
    async fn get_inventory(&self) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client.character_inventory().await {
            Ok(inventory) => {
                let items: Vec<serde_json::Value> = inventory
                    .items
                    .into_iter()
                    .map(|i| {
                        serde_json::json!({
                            "name": i.name,
                            "count": i.count,
                        })
                    })
                    .collect();
                serde_json::to_string_pretty(&items).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Get a compact one-call situational snapshot for orientation.
    #[tool(
        description = "Compact one-call situational snapshot with position, health, walking, tick, inventory, nearby entity counts, and resource patches. Use this first to orient; call render_map only when spatial detail is needed."
    )]
    async fn situation_report(
        &self,
        Parameters(params): Parameters<SituationReportParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let radius = params.radius.unwrap_or(32);
        let status = match client.character_status().await {
            Ok(status) => status,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let position = match status.position {
            Some(position) => position,
            None => match client.get_character_position().await {
                Ok(position) => position,
                Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
            },
        };
        let r = radius as f64;
        let area = Area {
            left_top: Position::new(position.x - r, position.y - r),
            right_bottom: Position::new(position.x + r, position.y + r),
        };
        let inventory = match client.character_inventory().await {
            Ok(inventory) => inventory,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let entities = match client.find_entities(area, None, None).await {
            Ok(entities) => entities,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let resources = match client.find_resources(area, None).await {
            Ok(resources) => resources,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let tick = match client.get_tick().await {
            Ok(tick) => tick,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let report = build_situation_report(
            position,
            status.health,
            status.walking,
            tick.tick,
            inventory.items,
            entities,
            resources,
            radius,
        );
        let result =
            serde_json::to_string_pretty(&report).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(result).await
    }

    /// Verify producing entities are actually working after building.
    #[tool(
        description = "Observe real production over 60 game ticks after building or modifying a factory. success is true only when a producing machine either remains actively working or its products_finished counter increases; idle, no-input, no-fuel, no-power, disabled, and output-blocked machines return success=false. Belts and inserters are never counted as producers."
    )]
    async fn verify_production(
        &self,
        Parameters(params): Parameters<VerifyProductionParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let radius = params.radius.unwrap_or(32);
        let position = match (params.x, params.y) {
            (Some(x), Some(y)) => Position::new(x, y),
            (None, None) => {
                let status = match client.character_status().await {
                    Ok(status) => status,
                    Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
                };
                match status.position {
                    Some(position) => position,
                    None => match client.get_character_position().await {
                        Ok(position) => position,
                        Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
                    },
                }
            }
            _ => {
                return self
                    .with_player_messages("Error: x and y must be provided together".to_string())
                    .await;
            }
        };
        let r = radius as f64;
        let area = Area {
            left_top: Position::new(position.x - r, position.y - r),
            right_bottom: Position::new(position.x + r, position.y + r),
        };
        let before = match client.verify_production(area).await {
            Ok(entities) => entities,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        if let Err(error) = client.wait_ticks(60).await {
            return self
                .with_player_messages(format!("Error: observing production: {error}"))
                .await;
        }
        let after = match client.verify_production(area).await {
            Ok(entities) => entities,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let report = production_observation_json(before, after, 60);
        let result =
            serde_json::to_string_pretty(&report).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(result).await
    }

    /// Diagnose ranked factory blockers and likely causal repairs in an area.
    #[tool(
        description = "Diagnose ranked production blockers near a point. Returns non-working entities, likely root causes such as unfueled boilers causing downstream no_power, and concrete suggested tool actions. Use this before spending turns manually debugging no_power, no_fuel, no_ingredients, output blockage, or idle labs."
    )]
    async fn diagnose_factory_blockers(
        &self,
        Parameters(params): Parameters<DiagnoseFactoryBlockersParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let radius = params.radius.unwrap_or(32);
        let limit = params.limit.unwrap_or(10).clamp(1, 50);
        let position = match (params.x, params.y) {
            (Some(x), Some(y)) => Position::new(x, y),
            (None, None) => {
                let status = match client.character_status().await {
                    Ok(status) => status,
                    Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
                };
                match status.position {
                    Some(position) => position,
                    None => match client.get_character_position().await {
                        Ok(position) => position,
                        Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
                    },
                }
            }
            _ => {
                return self
                    .with_player_messages("Error: x and y must be provided together".to_string())
                    .await;
            }
        };
        let r = radius as f64;
        let area = Area {
            left_top: Position::new(position.x - r, position.y - r),
            right_bottom: Position::new(position.x + r, position.y + r),
        };
        let report = match client.diagnose_factory_blockers(area, limit).await {
            Ok(report) => report,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let result = serde_json::to_string_pretty(&model_safe_payload(report))
            .unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(result).await
    }

    /// Diagnose durable fuel automation, not one-off hand feeding.
    #[tool(
        description = "Diagnose whether burners, furnaces, boilers, and burner inserters have durable coal supply. Returns ranked fuel consumers, nearby coal drills/belts/chests/resources, and concrete durable repair actions. Use this whenever no_fuel appears or after bootstrap fuel; do not treat insert_items/hand_feed_furnace as completion unless this reports a durable coal source path to the consumer."
    )]
    async fn diagnose_fuel_sustainability(
        &self,
        Parameters(params): Parameters<DiagnoseFuelSustainabilityParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let radius = params.radius.unwrap_or(64);
        let limit = params.limit.unwrap_or(20).clamp(1, 100);
        let position = match (params.x, params.y) {
            (Some(x), Some(y)) => Position::new(x, y),
            (None, None) => {
                let status = match client.character_status().await {
                    Ok(status) => status,
                    Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
                };
                match status.position {
                    Some(position) => position,
                    None => match client.get_character_position().await {
                        Ok(position) => position,
                        Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
                    },
                }
            }
            _ => {
                return self
                    .with_player_messages("Error: x and y must be provided together".to_string())
                    .await;
            }
        };
        let r = radius as f64;
        let area = Area {
            left_top: Position::new(position.x - r, position.y - r),
            right_bottom: Position::new(position.x + r, position.y + r),
        };
        let report = match client.diagnose_fuel_sustainability(area, limit).await {
            Ok(report) => report,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let result =
            serde_json::to_string_pretty(&report).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(result).await
    }

    /// Get current game tick.
    #[tool(description = "Get current game tick and elapsed time.")]
    async fn get_tick(&self) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client.get_tick().await {
            Ok(tick) => format!("Tick: {} ({:.1} seconds)", tick.tick, tick.to_seconds()),
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    // --- Analysis Tools ---

    /// Analyze belt reachability from a position.
    #[tool(
        description = "Analyze belt connectivity from a position. Shows all upstream (feeding) and downstream (fed) belts."
    )]
    async fn analyze_belt_reach(&self, Parameters(params): Parameters<BeltReachParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let area = Area {
            left_top: Position::new(
                params.x as f64 - params.radius as f64,
                params.y as f64 - params.radius as f64,
            ),
            right_bottom: Position::new(
                params.x as f64 + params.radius as f64,
                params.y as f64 + params.radius as f64,
            ),
        };

        let result = match client.find_entities(area, None, None).await {
            Ok(entities) => {
                let graph = BeltGraph::from_entities(&entities);
                let start = TilePos::new(params.x, params.y);

                match analyze_belt_reach(&graph, start) {
                    Some(r) => {
                        serde_json::to_string_pretty(&r).unwrap_or_else(|e| format!("Error: {}", e))
                    }
                    None => format!("No belt found at ({}, {})", params.x, params.y),
                }
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Find all connected belt networks in an area.
    #[tool(
        description = "Find all separate belt networks in an area. Shows network sizes and input/output counts."
    )]
    async fn analyze_belt_networks(&self, Parameters(params): Parameters<AreaParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client.find_entities(params.to_area(), None, None).await {
            Ok(entities) => {
                let graph = BeltGraph::from_entities(&entities);
                let r = find_belt_networks(&graph);
                serde_json::to_string_pretty(&r).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Find gaps in belt lines.
    #[tool(description = "Find gaps in belt lines - missing, misaligned, or blocked connections.")]
    async fn analyze_belt_gaps(&self, Parameters(params): Parameters<AreaParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client.find_entities(params.to_area(), None, None).await {
            Ok(entities) => {
                let graph = BeltGraph::from_entities(&entities);
                let r = find_belt_gaps(&graph, &entities);
                serde_json::to_string_pretty(&r).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Analyze inserters in an area.
    #[tool(
        description = "Analyze inserters - shows pickup/dropoff positions and what entities they interact with."
    )]
    async fn analyze_inserters(&self, Parameters(params): Parameters<AreaParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client.find_entities(params.to_area(), None, None).await {
            Ok(entities) => {
                let r = analyze_inserters(&entities);
                serde_json::to_string_pretty(&r).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Analyze item flow between a source and target.
    #[tool(
        description = "Analyze item flow from a source entity/tile to a target entity/tile. Returns whether belts connect, current items on the reachable belt path, source/target belt tiles, the first missing/wrong-way/blocked belt break, and a concrete repair action using route_belt or rotate_entity. Use before manually squinting at belt directions."
    )]
    async fn analyze_item_flow(
        &self,
        Parameters(params): Parameters<AnalyzeItemFlowParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let source = match flow_lookup(
            params.source_unit_number,
            params.source_x,
            params.source_y,
            "source",
        ) {
            Ok(lookup) => lookup,
            Err(e) => {
                return self
                    .with_player_messages(semantic_failure("invalid_flow_reference", e))
                    .await;
            }
        };
        let target = match flow_lookup(
            params.target_unit_number,
            params.target_x,
            params.target_y,
            "target",
        ) {
            Ok(lookup) => lookup,
            Err(e) => {
                return self
                    .with_player_messages(semantic_failure("invalid_flow_reference", e))
                    .await;
            }
        };
        let source_tile = match flow_reference_tile(&mut client, source).await {
            Ok(tile) => tile,
            Err(e) => {
                return self
                    .with_player_messages(semantic_failure("flow_reference_unavailable", e))
                    .await;
            }
        };
        let target_tile = match flow_reference_tile(&mut client, target).await {
            Ok(tile) => tile,
            Err(e) => {
                return self
                    .with_player_messages(semantic_failure("flow_reference_unavailable", e))
                    .await;
            }
        };
        let area = flow_scan_area(source_tile, target_tile, params.radius.clamp(1, 100));

        let result = match client.find_entities(area, None, None).await {
            Ok(entities) => match client.get_belt_lane_contents(area).await {
                Ok(belt_contents) => {
                    let report = analyze_item_flow(&entities, &belt_contents.belts, source, target);
                    serde_json::to_string_pretty(&report)
                        .unwrap_or_else(|e| format!("Error: {}", e))
                }
                Err(e) => format!("Error: reading belt contents: {}", e),
            },
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    // --- Action Tools ---

    /// Walk character to a position.
    #[tool(
        description = "Walk character to a position using the mod's direct stepped movement target."
    )]
    async fn walk_to(&self, Parameters(params): Parameters<PositionParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let position = Position::new(params.x, params.y);
        let result = match client.walk_to(position, true).await {
            Ok(r) => serde_json::to_string_pretty(&r).unwrap_or_else(|e| format!("Error: {}", e)),
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Place an entity from character inventory.
    #[tool(
        description = "Place an entity exactly. Use route_belt for belt tiles; splitters may fast-replace compatible belts. Inserters face their pickup side. Factorio collision and resource rules are authoritative."
    )]
    async fn place_entity(&self, Parameters(params): Parameters<PlaceEntityParams>) -> String {
        if direct_placement_requires_route(&params.entity_name) {
            let result = serde_json::json!({
                "success": false,
                "error_kind": "belt_requires_route",
                "error": "Direct belt placement is disabled because isolated belt tiles create disconnected or purposeless logistics. Use route_belt or a durable automation controller.",
                "rejected_entity": params.entity_name,
                "next_action": {
                    "tool": "route_belt",
                    "required": ["from_x", "from_y", "to_x", "to_y"],
                },
            });
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                .await;
        }
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let position = Position::new(params.x, params.y);
        let direction = if params.direction.is_empty() {
            Direction::North
        } else {
            match Direction::parse(&params.direction) {
                Some(d) => d,
                None => {
                    return self
                        .with_player_messages(semantic_failure(
                            "invalid_direction",
                            format!(
                                "Invalid direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                                params.direction
                            ),
                        ))
                        .await
                }
            }
        };

        let result = match client
            .call_remote(
                "place_entity",
                &[
                    serde_json::json!(client.agent_id().as_str()),
                    serde_json::json!(params.entity_name),
                    serde_json::json!(position.x),
                    serde_json::json!(position.y),
                    serde_json::json!(direction.to_factorio()),
                ],
            )
            .await
        {
            Ok(response) => match serde_json::from_str::<serde_json::Value>(&response) {
                Ok(value) => {
                    serde_json::to_string_pretty(&value).unwrap_or_else(|e| format!("Error: {}", e))
                }
                Err(_) => response,
            },
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Find nearby valid placements for an entity.
    #[tool(
        description = "Find nearby Factorio-valid placements in all directions. Resource overlap is advisory for ordinary infrastructure; extractor-category compatibility is mandatory. Drill outlets prefer clear terrain but may use ore."
    )]
    async fn find_entity_placements(
        &self,
        Parameters(params): Parameters<FindEntityPlacementsParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let memory = AgentMemory::load();
        let center = Position::new(params.x, params.y);
        let radius = params.radius.clamp(1, 25);
        let limit = params.limit.clamp(1, 100);
        let candidate_limit = limit.saturating_mul(10).clamp(limit, 500);

        let mut result = match client
            .find_entity_placements(&params.entity_name, center, radius, candidate_limit)
            .await
        {
            Ok(value) => value,
            Err(e) => serde_json::json!({
                "success": false,
                "error": format!("Placement search failed: {}", e),
                "entity": params.entity_name,
                "center": { "x": params.x, "y": params.y },
                "radius": radius,
                "placements": []
            }),
        };

        if let Some(placements) = result.get_mut("placements").and_then(|v| v.as_array_mut()) {
            for placement in &mut *placements {
                let Some(position) = placement.get("position") else {
                    continue;
                };
                let x = position
                    .get("x")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(params.x);
                let y = position
                    .get("y")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(params.y);
                let policy = memory.check_placement(&params.entity_name, &Position::new(x, y));
                if let Some(obj) = placement.as_object_mut() {
                    obj.insert(
                        "policy_allowed".to_string(),
                        serde_json::json!(policy.allowed),
                    );
                    obj.insert("allowed".to_string(), serde_json::json!(policy.allowed));
                    obj.insert("warnings".to_string(), serde_json::json!(policy.warnings));
                    obj.insert("errors".to_string(), serde_json::json!(policy.errors));
                    obj.insert(
                        "recorded_resource_overlaps".to_string(),
                        serde_json::json!(policy.overlapping_resources),
                    );
                }
            }
            placements.sort_by(|a, b| {
                let a_allowed = a.get("allowed").and_then(|v| v.as_bool()).unwrap_or(false);
                let b_allowed = b.get("allowed").and_then(|v| v.as_bool()).unwrap_or(false);
                let a_output_clear = a
                    .get("output_clear")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let b_output_clear = b
                    .get("output_clear")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let a_output_buildable = a
                    .get("output_buildable")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let b_output_buildable = b
                    .get("output_buildable")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let a_resource_overlap = a
                    .get("resource_overlap")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let b_resource_overlap = b
                    .get("resource_overlap")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                b_allowed
                    .cmp(&a_allowed)
                    .then_with(|| b_output_clear.cmp(&a_output_clear))
                    .then_with(|| b_output_buildable.cmp(&a_output_buildable))
                    .then_with(|| a_resource_overlap.cmp(&b_resource_overlap))
                    .then_with(|| {
                        let a_distance = a
                            .get("distance")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(f64::INFINITY);
                        let b_distance = b
                            .get("distance")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(f64::INFINITY);
                        a_distance
                            .partial_cmp(&b_distance)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    })
                    .then_with(|| {
                        let a_direction = a.get("direction").and_then(|v| v.as_i64()).unwrap_or(0);
                        let b_direction = b.get("direction").and_then(|v| v.as_i64()).unwrap_or(0);
                        a_direction.cmp(&b_direction)
                    })
            });
            placements.truncate(limit as usize);
        }
        if let Some(obj) = result.as_object_mut() {
            let returned = obj
                .get("placements")
                .and_then(|v| v.as_array())
                .map(|placements| placements.len())
                .unwrap_or(0);
            let total = obj
                .get("total")
                .and_then(|v| v.as_u64())
                .unwrap_or(returned as u64);
            obj.insert("returned".to_string(), serde_json::json!(returned));
            obj.insert(
                "truncated".to_string(),
                serde_json::json!(total > returned as u64),
            );
            obj.insert(
                "candidate_limit".to_string(),
                serde_json::json!(candidate_limit),
            );
            obj.insert("limit".to_string(), serde_json::json!(limit));
        }

        let result =
            serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(result).await
    }

    /// Plan a safe entity placement near a target.
    #[tool(
        description = "Read-only footprint-aware placement planner. Returns concrete place_entity steps near a target while explicitly avoiding the agent character footprint and placements that trap the agent or block drill output. Use before placing entities near yourself or near crowded builds; selected.footprint shows the collision area, selected.post_placement.nearest_clear_standing_position shows where the agent can stand afterward, and selected.can_place_and_keep_working confirms the placement is operationally safe."
    )]
    async fn plan_entity_placement_near(
        &self,
        Parameters(params): Parameters<PlanEntityPlacementNearParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let target = Position::new(params.x, params.y);
        let radius = params.radius.clamp(1, 25);
        let limit = params.limit.clamp(1, 50);
        let result = match client
            .plan_entity_placement_near(&params.entity_name, target, radius, limit)
            .await
        {
            Ok(value) => {
                serde_json::to_string_pretty(&value).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Execute a safe entity placement selected by plan_entity_placement_near.
    #[tool(
        description = "Place one non-belt entity near a target so the NPC remains mobile. Ordinary infrastructure may overlap resources; live extractor-category checks still apply. Use dry_run=true to preview."
    )]
    async fn execute_entity_placement_near(
        &self,
        Parameters(params): Parameters<ExecuteEntityPlacementNearParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let target = Position::new(params.x, params.y);
        let radius = params.radius.clamp(1, 25);
        let limit = params.limit.clamp(1, 50);
        let plan = match client
            .plan_entity_placement_near(&params.entity_name, target, radius, limit)
            .await
        {
            Ok(value) => value,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let selected = plan
            .get("selected")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let compact_plan = serde_json::json!({
            "success": plan.get("success").and_then(|value| value.as_bool()).unwrap_or(false),
            "dry_run": true,
            "entity": plan.get("entity"),
            "target": plan.get("target"),
            "radius": plan.get("radius"),
            "checked": plan.get("checked"),
            "total": plan.get("total"),
            "returned": plan.get("returned"),
            "truncated": plan.get("truncated"),
            "inventory_count": plan.get("inventory_count"),
            "selected": {
                "tool": selected.get("tool"),
                "tool_args": selected.get("tool_args"),
                "position": selected.get("position"),
                "direction": selected.get("direction"),
                "distance": selected.get("distance"),
                "distance_from_character": selected.get("distance_from_character"),
                "output_clear": selected.get("output_clear"),
                "output_buildable": selected.get("output_buildable"),
                "post_placement": selected.get("post_placement"),
            },
            "error": plan.get("error"),
            "next_action": plan.get("next_action"),
        });

        if params.dry_run {
            let result = serde_json::json!({
                "success": plan.get("success").and_then(|value| value.as_bool()).unwrap_or(false),
                "dry_run": true,
                "plan": compact_plan,
                "guidance": "If success is true, call execute_entity_placement_near again with dry_run=false. Use the returned placed_unit_number in follow-up tools.",
            });
            let msg =
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
            return self.with_player_messages(msg).await;
        }

        if !plan
            .get("success")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        {
            let msg = serde_json::to_string_pretty(&compact_plan)
                .unwrap_or_else(|e| format!("Error: {}", e));
            return self.with_player_messages(msg).await;
        }

        let args = selected
            .get("tool_args")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let entity_name = args
            .get("entity_name")
            .and_then(|value| value.as_str())
            .unwrap_or(&params.entity_name);
        let x = args
            .get("x")
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0);
        let y = args
            .get("y")
            .and_then(|value| value.as_f64())
            .unwrap_or(0.0);
        let direction_name = args
            .get("direction")
            .and_then(|value| value.as_str())
            .unwrap_or("north");
        let direction = match Direction::parse(direction_name) {
            Some(direction) => direction,
            None => {
                return self
                    .with_player_messages(semantic_failure(
                        "invalid_direction",
                        format!(
                            "Invalid selected direction '{}'. Re-run execute_entity_placement_near with dry_run=true.",
                            direction_name
                        ),
                    ))
                    .await;
            }
        };

        let placed = client
            .place_entity(entity_name, Position::new(x, y), direction)
            .await;
        let verify_area = Area::new(x - 6.0, y - 6.0, x + 6.0, y + 6.0);
        let verification = match client.verify_production(verify_area).await {
            Ok(entities) => production_verification_json(entities).0,
            Err(e) => serde_json::json!({
                "success": false,
                "error": e.to_string(),
            }),
        };
        let compact_selected = compact_plan.get("selected").cloned();
        let result = serde_json::json!({
            "success": placed.is_ok(),
            "placement_success": placed.is_ok(),
            "dry_run": false,
            "plan": compact_plan,
            "selected": compact_selected,
            "action": {
                "tool": "place_entity",
                "args": args,
                "success": placed.is_ok(),
                "entity": placed.as_ref().ok(),
                "error": placed.as_ref().err().map(|e| e.to_string()),
            },
            "placed_unit_number": placed.as_ref().ok().and_then(|entity| entity.unit_number),
            "verification": verification,
            "guidance": "Use placed_unit_number for set_recipe, plan_automation_science, build_lab_feed, or other follow-up automation controllers.",
        });
        let msg = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(msg).await
    }

    /// Plan an edge mining drill and output belt without mutating the game.
    #[tool(
        description = "Plan a resource-backed drill and Factorio-buildable output belt without mutation. Returns ordered steps and missing items. Clear output is preferred, but ore is valid."
    )]
    async fn build_edge_miner(
        &self,
        Parameters(params): Parameters<BuildEdgeMinerParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let center = Position::new(params.x, params.y);
        let radius = params.radius.clamp(1, 40);
        let limit = params.limit.clamp(1, 50);
        let result = match client
            .build_edge_miner(
                &params.resource_type,
                center,
                radius,
                &params.drill_name,
                limit,
            )
            .await
        {
            Ok(value) => {
                serde_json::to_string_pretty(&value).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Execute a checked edge mining drill and output belt plan.
    #[tool(
        description = "Atomically plan/build a resource-backed drill plus Factorio-buildable output belt, bootstrap fuel, and verify production. Clear output is preferred, but ore is valid. dry_run previews the transaction."
    )]
    async fn execute_edge_miner(
        &self,
        Parameters(params): Parameters<ExecuteEdgeMinerParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let center = Position::new(params.x, params.y);
        let radius = params.radius.clamp(1, 40);
        let limit = params.limit.clamp(1, 50);
        let plan = match client
            .build_edge_miner(
                &params.resource_type,
                center,
                radius,
                &params.drill_name,
                limit,
            )
            .await
        {
            Ok(value) => value,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let model_plan = model_safe_payload(plan.clone());

        if !plan
            .get("success")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        {
            let msg = serde_json::to_string_pretty(&model_plan)
                .unwrap_or_else(|e| format!("Error: {}", e));
            return self.with_player_messages(msg).await;
        }

        let selected = plan
            .get("selected")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let steps = plan
            .get("steps")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        let (planned_placements, planned_rotations) = match parse_controller_steps(&steps) {
            Ok(steps) => steps,
            Err(error) => return self.with_player_messages(format!("Error: {error}")).await,
        };
        if !planned_rotations.is_empty() {
            return self
                .with_player_messages(
                    "Error: edge miner plan unexpectedly contains a rotation step".to_string(),
                )
                .await;
        }
        let placement_reservations: Vec<ControllerPlacement<'_>> = planned_placements
            .iter()
            .map(|placement| ControllerPlacement {
                label: &placement.label,
                item_name: &placement.entity_name,
                entity_name: &placement.entity_name,
                position: placement.position,
                direction: placement.direction,
            })
            .collect();
        let preflight = match controller_preflight(
            &mut client,
            &[],
            "transport-belt",
            &HashSet::new(),
            &placement_reservations,
        )
        .await
        {
            Ok(preflight) => preflight,
            Err(error) => return self.with_player_messages(format!("Error: {error}")).await,
        };
        let preflight_ready = preflight["ready"].as_bool() == Some(true);

        if params.dry_run {
            let result = serde_json::json!({
                "success": preflight_ready,
                "dry_run": true,
                "plan": model_plan,
                "preflight": preflight,
                "guidance": "Execute only when preflight.ready is true; the drill and output belt are reserved and placed as one transaction.",
            });
            let msg =
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
            return self.with_player_messages(msg).await;
        }
        if !preflight_ready {
            let result = serde_json::json!({
                "success": false,
                "error_kind": "compound_preflight_failed",
                "error": "Edge miner failed complete material or placement preflight. Nothing was placed.",
                "plan": model_plan,
                "preflight": preflight,
            });
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                .await;
        }

        let mut transaction_units = Vec::new();
        let mut actions = Vec::new();
        let mut placed_drill_unit = None;
        let mut placed_belt_unit = None;
        for placement in planned_placements {
            let entity = match client
                .place_entity(
                    &placement.entity_name,
                    placement.position,
                    placement.direction,
                )
                .await
            {
                Ok(entity) => entity,
                Err(error) => {
                    let rollback =
                        rollback_controller_transaction(&mut client, &transaction_units, &[], None)
                            .await;
                    let result = serde_json::json!({
                        "success": false,
                        "error_kind": "placement_execution_failed",
                        "failed_entity": placement.entity_name,
                        "error": error.to_string(),
                        "rollback": rollback,
                    });
                    return self
                        .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                        .await;
                }
            };
            if entity.name.contains("mining-drill") {
                placed_drill_unit = entity.unit_number;
            }
            if entity.name.contains("transport-belt") {
                placed_belt_unit = entity.unit_number;
            }
            if let Some(unit_number) = entity.unit_number {
                transaction_units.push(unit_number);
            }
            actions.push(serde_json::json!({
                "tool": "place_entity",
                "success": true,
                "entity_name": entity.name,
                "unit_number": entity.unit_number,
            }));
        }

        let mut fuel_report = serde_json::json!({
            "skipped": true,
            "reason": "not a burner drill or drill placement failed",
        });
        if params.drill_name.contains("burner") {
            if let Some(unit) = placed_drill_unit {
                let inserted = client
                    .insert_items(unit, &params.fuel_item, params.fuel_count, "fuel")
                    .await;
                if let Err(error) = &inserted {
                    let rollback =
                        rollback_controller_transaction(&mut client, &transaction_units, &[], None)
                            .await;
                    let result = serde_json::json!({
                        "success": false,
                        "error_kind": "bootstrap_fuel_failed",
                        "error": error.to_string(),
                        "rollback": rollback,
                    });
                    return self
                        .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                        .await;
                }
                fuel_report = serde_json::json!({
                    "operation": "bootstrap_burner_fuel",
                    "unit_number": unit,
                    "item": params.fuel_item,
                    "count": params.fuel_count,
                    "inventory_type": "fuel",
                    "temporary": true,
                    "success": inserted.is_ok(),
                    "error": inserted.as_ref().err().map(|e| e.to_string()),
                });
            }
        }

        let verify_radius = params.verify_radius.clamp(1, 50) as f64;
        let verify_area = Area::new(
            center.x - verify_radius,
            center.y - verify_radius,
            center.x + verify_radius,
            center.y + verify_radius,
        );
        let verification = match observe_production(&mut client, verify_area, 180).await {
            Ok(verification) => verification,
            Err(error) => serde_json::json!({"success": false, "error": error.to_string()}),
        };
        let drill_working = production_unit_verified(&verification, placed_drill_unit);
        let mut placed_units_exist = placed_drill_unit.is_some() && placed_belt_unit.is_some();
        for unit_number in [placed_drill_unit, placed_belt_unit].into_iter().flatten() {
            placed_units_exist &= client.get_entity(unit_number).await.is_ok();
        }
        let repair_hint = automation_repair_hint(
            "execute_edge_miner",
            "edge miner output belt",
            verification.get("error").is_none(),
            &verification,
            &[],
            Some(true),
        );

        let success = drill_working && placed_units_exist;
        if !success {
            let rollback =
                rollback_controller_transaction(&mut client, &transaction_units, &[], None).await;
            let result = serde_json::json!({
                "success": false,
                "error_kind": "verification_failed",
                "error": "Edge miner did not prove the newly placed drill actively producing; the drill and output belt were rolled back.",
                "verification": verification,
                "rollback": rollback,
                "repair_hint": repair_hint,
            });
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                .await;
        }
        let result = serde_json::json!({
            "success": true,
            "dry_run": false,
            "plan": model_plan,
            "preflight": preflight,
            "selected": selected,
            "placed_drill_unit_number": placed_drill_unit,
            "placed_belt_unit_number": placed_belt_unit,
            "actions": actions,
            "bootstrap_fuel": fuel_report,
            "automation_verified": {
                "success": true,
                "placed_drill_working": drill_working,
                "placed_units_exist": placed_units_exist,
            },
            "verification": verification,
            "repair_hint": repair_hint,
            "guidance": "If this is coal production, call repair_fuel_sustainability to diagnose and build durable consumer feeds. Temporary burner fuel is not durable automation completion.",
        });
        let msg = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(msg).await
    }

    /// Plan a direct drill-output smelter without mutating the game.
    #[tool(
        description = "Plan a checked direct smelter from a mining drill output without mutating the game. Accepts either drill_unit_number or output_x/output_y/output_direction from build_edge_miner/get_machine_belt_positions. Returns ordered place_entity steps for belt, furnace, inserter, after-place fuel steps, missing_items, and a verify_production step."
    )]
    async fn build_direct_smelter(
        &self,
        Parameters(params): Parameters<BuildDirectSmelterParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let output = match (params.output_x, params.output_y) {
            (Some(x), Some(y)) => {
                let direction_name = params.output_direction.unwrap_or_default();
                let direction = match Direction::parse(&direction_name) {
                    Some(direction) => direction,
                    None => {
                        return self
                            .with_player_messages(semantic_failure(
                                "invalid_direction",
                                format!(
                                    "Invalid output_direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                                    direction_name
                                ),
                            ))
                            .await
                    }
                };
                Some((Position::new(x, y), direction))
            }
            (None, None) => None,
            _ => {
                return self
                    .with_player_messages(
                        "Error: output_x and output_y must be provided together".to_string(),
                    )
                    .await
            }
        };

        let result = match client
            .build_direct_smelter(
                params.drill_unit_number,
                output,
                &params.furnace_name,
                &params.inserter_name,
                &params.belt_name,
                params.radius.clamp(2, 12),
            )
            .await
        {
            Ok(value) => {
                serde_json::to_string_pretty(&value).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Execute a checked direct drill-output smelter plan.
    #[tool(
        description = "Plan and atomically build a direct drill-output smelter cell: derive checked geometry, place or align the output belt, furnace, and inserter, bootstrap burner fuel, then verify the new cell. Rolls the whole cell back if it cannot be proven."
    )]
    async fn execute_direct_smelter(
        &self,
        Parameters(params): Parameters<ExecuteDirectSmelterParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let output = match (params.output_x, params.output_y) {
            (Some(x), Some(y)) => {
                let direction_name = params.output_direction.clone().unwrap_or_default();
                let direction = match Direction::parse(&direction_name) {
                    Some(direction) => direction,
                    None => {
                        return self
                            .with_player_messages(semantic_failure(
                                "invalid_direction",
                                format!(
                                    "Invalid output_direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                                    direction_name
                                ),
                            ))
                            .await;
                    }
                };
                Some((Position::new(x, y), direction))
            }
            (None, None) => None,
            _ => {
                return self
                    .with_player_messages(
                        "Error: output_x and output_y must be provided together".to_string(),
                    )
                    .await;
            }
        };

        let plan = match client
            .build_direct_smelter(
                params.drill_unit_number,
                output,
                &params.furnace_name,
                &params.inserter_name,
                &params.belt_name,
                params.radius.clamp(2, 12),
            )
            .await
        {
            Ok(value) => value,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let model_plan = model_safe_payload(plan.clone());

        if !plan
            .get("success")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
        {
            let msg = serde_json::to_string_pretty(&model_plan)
                .unwrap_or_else(|e| format!("Error: {}", e));
            return self.with_player_messages(msg).await;
        }

        let steps = plan
            .get("steps")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();
        let (planned_placements, planned_rotations) = match parse_controller_steps(&steps) {
            Ok(steps) => steps,
            Err(error) => return self.with_player_messages(format!("Error: {error}")).await,
        };
        let placement_reservations: Vec<ControllerPlacement<'_>> = planned_placements
            .iter()
            .map(|placement| ControllerPlacement {
                label: &placement.label,
                item_name: &placement.entity_name,
                entity_name: &placement.entity_name,
                position: placement.position,
                direction: placement.direction,
            })
            .collect();
        let mut preflight = match controller_preflight(
            &mut client,
            &[],
            &params.belt_name,
            &HashSet::new(),
            &placement_reservations,
        )
        .await
        {
            Ok(preflight) => preflight,
            Err(error) => return self.with_player_messages(format!("Error: {error}")).await,
        };
        let mut rotation_reports = Vec::new();
        let mut rotations_ready = true;
        for rotation in &planned_rotations {
            match client.get_entity(rotation.unit_number).await {
                Ok(entity) => rotation_reports.push(serde_json::json!({
                    "success": true,
                    "unit_number": rotation.unit_number,
                    "name": entity.name,
                    "previous_direction": entity.direction,
                    "requested_direction": rotation.direction,
                })),
                Err(error) => {
                    rotations_ready = false;
                    rotation_reports.push(serde_json::json!({
                        "success": false,
                        "unit_number": rotation.unit_number,
                        "error": error.to_string(),
                    }));
                }
            }
        }
        let preflight_ready = preflight["ready"].as_bool() == Some(true) && rotations_ready;
        if let Some(object) = preflight.as_object_mut() {
            object.insert("ready".to_string(), serde_json::json!(preflight_ready));
            object.insert("rotations".to_string(), serde_json::json!(rotation_reports));
        }

        if params.dry_run {
            let result = serde_json::json!({
                "success": preflight_ready,
                "dry_run": true,
                "plan": model_plan,
                "preflight": preflight,
                "guidance": "Execute only when preflight.ready is true; every placement, inventory reservation, and existing-belt rotation is one transaction.",
            });
            let msg =
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
            return self.with_player_messages(msg).await;
        }
        if !preflight_ready {
            let result = serde_json::json!({
                "success": false,
                "error_kind": "compound_preflight_failed",
                "error": "Direct smelter failed complete placement, material, or rotation preflight. Nothing was changed.",
                "plan": model_plan,
                "preflight": preflight,
            });
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                .await;
        }

        let mut transaction_units = Vec::new();
        let mut rotated_entities = Vec::new();
        let mut actions = Vec::new();
        for rotation in planned_rotations {
            let previous = match client.get_entity(rotation.unit_number).await {
                Ok(entity) => entity.direction,
                Err(error) => {
                    let rollback = rollback_controller_transaction(
                        &mut client,
                        &transaction_units,
                        &rotated_entities,
                        None,
                    )
                    .await;
                    let result = serde_json::json!({
                        "success": false,
                        "error_kind": "rotation_execution_failed",
                        "error": error.to_string(),
                        "rollback": rollback,
                    });
                    return self
                        .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                        .await;
                }
            };
            if let Err(error) = client
                .rotate_entity(rotation.unit_number, rotation.direction.to_factorio())
                .await
            {
                let rollback = rollback_controller_transaction(
                    &mut client,
                    &transaction_units,
                    &rotated_entities,
                    None,
                )
                .await;
                let result = serde_json::json!({
                    "success": false,
                    "error_kind": "rotation_execution_failed",
                    "error": error.to_string(),
                    "rollback": rollback,
                });
                return self
                    .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                    .await;
            }
            rotated_entities.push((rotation.unit_number, previous));
            actions.push(serde_json::json!({
                "tool": "rotate_entity",
                "success": true,
                "unit_number": rotation.unit_number,
                "previous_direction": previous,
                "direction": rotation.direction,
            }));
        }

        let mut placed_furnace_unit = None;
        let mut placed_inserter_unit = None;
        for placement in planned_placements {
            let entity = match client
                .place_entity(
                    &placement.entity_name,
                    placement.position,
                    placement.direction,
                )
                .await
            {
                Ok(entity) => entity,
                Err(error) => {
                    let rollback = rollback_controller_transaction(
                        &mut client,
                        &transaction_units,
                        &rotated_entities,
                        None,
                    )
                    .await;
                    let result = serde_json::json!({
                        "success": false,
                        "error_kind": "placement_execution_failed",
                        "failed_entity": placement.entity_name,
                        "error": error.to_string(),
                        "rollback": rollback,
                    });
                    return self
                        .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                        .await;
                }
            };
            if placement.entity_name == params.furnace_name {
                placed_furnace_unit = entity.unit_number;
            }
            if placement.entity_name == params.inserter_name {
                placed_inserter_unit = entity.unit_number;
            }
            if let Some(unit_number) = entity.unit_number {
                transaction_units.push(unit_number);
            }
            actions.push(serde_json::json!({
                "tool": "place_entity",
                "success": true,
                "entity_name": placement.entity_name,
                "unit_number": entity.unit_number,
            }));
        }

        let mut bootstrap_fuel = Vec::new();
        if params.furnace_name != "electric-furnace" {
            if let Some(unit) = placed_furnace_unit {
                let inserted = client.insert_items(unit, "coal", 25, "fuel").await;
                if let Err(error) = &inserted {
                    let rollback = rollback_controller_transaction(
                        &mut client,
                        &transaction_units,
                        &rotated_entities,
                        None,
                    )
                    .await;
                    let result = serde_json::json!({
                        "success": false,
                        "error_kind": "bootstrap_fuel_failed",
                        "error": error.to_string(),
                        "rollback": rollback,
                    });
                    return self
                        .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                        .await;
                }
                bootstrap_fuel.push(serde_json::json!({
                    "unit_number": unit,
                    "item": "coal",
                    "count": 25,
                    "inventory_type": "fuel",
                    "temporary": true,
                    "success": inserted.is_ok(),
                    "error": inserted.as_ref().err().map(|e| e.to_string()),
                }));
            }
        }
        if params.inserter_name.contains("burner") {
            if let Some(unit) = placed_inserter_unit {
                let inserted = client.insert_items(unit, "coal", 5, "fuel").await;
                if let Err(error) = &inserted {
                    let rollback = rollback_controller_transaction(
                        &mut client,
                        &transaction_units,
                        &rotated_entities,
                        None,
                    )
                    .await;
                    let result = serde_json::json!({
                        "success": false,
                        "error_kind": "bootstrap_fuel_failed",
                        "error": error.to_string(),
                        "rollback": rollback,
                    });
                    return self
                        .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                        .await;
                }
                bootstrap_fuel.push(serde_json::json!({
                    "unit_number": unit,
                    "item": "coal",
                    "count": 5,
                    "inventory_type": "fuel",
                    "temporary": true,
                    "success": inserted.is_ok(),
                    "error": inserted.as_ref().err().map(|e| e.to_string()),
                }));
            }
        }

        let selected = plan.get("selected").unwrap_or(&serde_json::Value::Null);
        let belt_position = selected
            .get("output_belt")
            .and_then(|value| value.get("position"));
        let (verify_x, verify_y) = belt_position
            .and_then(|position| Some((position.get("x")?.as_f64()?, position.get("y")?.as_f64()?)))
            .unwrap_or((0.0, 0.0));
        let verify_area = Area::new(
            verify_x - 8.0,
            verify_y - 8.0,
            verify_x + 8.0,
            verify_y + 8.0,
        );
        let verification = match observe_production(&mut client, verify_area, 180).await {
            Ok(verification) => verification,
            Err(error) => serde_json::json!({"success": false, "error": error.to_string()}),
        };
        let fuel_sustainability = match client.diagnose_fuel_sustainability(verify_area, 20).await {
            Ok(report) => serde_json::json!({
                "success": true,
                "report": model_safe_payload(report),
            }),
            Err(e) => serde_json::json!({
                "success": false,
                "error": e.to_string(),
            }),
        };

        let furnace_working = production_unit_verified(&verification, placed_furnace_unit);
        let mut placed_units_exist =
            placed_furnace_unit.is_some() && placed_inserter_unit.is_some();
        for unit_number in [placed_furnace_unit, placed_inserter_unit]
            .into_iter()
            .flatten()
        {
            placed_units_exist &= client.get_entity(unit_number).await.is_ok();
        }
        let repair_hint = automation_repair_hint(
            "execute_direct_smelter",
            "direct drill-to-furnace smelter cell",
            verification.get("error").is_none(),
            &verification,
            &[],
            Some(true),
        );
        let automation_verified = furnace_working && placed_units_exist;
        if !automation_verified {
            let rollback = rollback_controller_transaction(
                &mut client,
                &transaction_units,
                &rotated_entities,
                None,
            )
            .await;
            let result = serde_json::json!({
                "success": false,
                "error_kind": "verification_failed",
                "error": "Direct smelter did not prove the newly placed furnace actively producing; all placements and rotations were rolled back.",
                "verification": verification,
                "rollback": rollback,
                "repair_hint": repair_hint,
            });
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                .await;
        }
        let result = serde_json::json!({
            "success": true,
            "placement_success": true,
            "dry_run": false,
            "plan": model_plan,
            "preflight": preflight,
            "actions": actions,
            "bootstrap_fuel": bootstrap_fuel,
            "automation_verified": {
                "success": true,
                "furnace_working": furnace_working,
                "placed_units_exist": placed_units_exist,
            },
            "verification": verification,
            "fuel_sustainability": fuel_sustainability,
            "repair_hint": repair_hint,
            "guidance": "If fuel_sustainability reports consumers without durable supply, call repair_fuel_sustainability next; temporary bootstrap fuel is not automation completion.",
        });
        let msg = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(msg).await
    }

    /// Mine natural entities or pick up loose items at an exact position.
    #[tool(
        description = "Mine natural resources, trees, or rocks, or pick up loose items at an exact position. This never removes placed infrastructure; use remove_entity with a unit number for that. Character will walk there first if needed."
    )]
    async fn mine_at(&self, Parameters(params): Parameters<MineAtParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let position = Position::new(params.x, params.y);
        let result = match client.mine_at(position, params.count).await {
            Ok(result) => {
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Admit a character-crafting request.
    #[tool(
        description = "Start character crafting. Success means accepted/queued, not produced; call wait_for_crafting before using the output or trusting craft triggers."
    )]
    async fn craft(&self, Parameters(params): Parameters<CraftParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client.craft(&params.recipe, params.count).await {
            Ok(result) => {
                let admission = result.status_evidence();
                serde_json::to_string_pretty(&serde_json::json!({
                    "success": result.success,
                    "completed": false,
                    "operation_id": result.operation_id.as_deref(),
                    "admission_persisted_in_save": result.operation_id.is_some(),
                    "admission": admission,
                    "craft_result": result,
                    "next_action": if admission.status == factorioctl::world::CraftingStatus::Rejected {
                        "inspect the craft error and recipe availability"
                    } else {
                        "call wait_for_crafting before using the output"
                    },
                }))
                .unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Verify and complete the exact persisted character-crafting transaction.
    #[tool(
        description = "Wait for the persisted craft transaction. Completion requires queue drain, exact output evidence, and full produced/consumed flow accounting; timeout remains resumable."
    )]
    async fn wait_for_crafting(
        &self,
        Parameters(params): Parameters<WaitForCraftingParams>,
    ) -> String {
        if !(1..=120).contains(&params.timeout_seconds) {
            return semantic_failure(
                "invalid_crafting_timeout",
                "timeout_seconds must be between 1 and 120",
            );
        }

        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return format!("Error: {}", e),
        };
        let payload = match client
            .complete_craft_admission_with_options(
                Duration::from_secs(u64::from(params.timeout_seconds)),
                Duration::from_millis(250),
            )
            .await
        {
            Ok(payload) => payload,
            Err(error) => serde_json::json!({
                "success": false,
                "completed": false,
                "status": "observation_failed",
                "error_kind": "craft_observation_failed",
                "error": error.to_string(),
            }),
        };
        serde_json::to_string_pretty(&payload).unwrap_or_else(|error| format!("Error: {error}"))
    }

    /// Add one bounded fuel buffer to an existing burner entity.
    #[tool(
        description = "Put 1-10 fuel items into an existing burner drill/inserter without replacing it. Temporary bootstrap only; then repair_fuel_sustainability. Not a substitute for a pending next_action."
    )]
    async fn bootstrap_burner_once(
        &self,
        Parameters(params): Parameters<BootstrapBurnerOnceParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(client) => client,
            Err(error) => return format!("Error: {error}"),
        };
        let result = match client
            .bootstrap_burner_once(params.unit_number, &params.fuel_item, params.count)
            .await
        {
            Ok(value) => serde_json::to_string_pretty(&value)
                .unwrap_or_else(|error| format!("Error: {error}")),
            Err(error) => format!("Error: {error}"),
        };
        self.with_player_messages(result).await
    }

    /// Collect a bounded item count from an existing chest.
    #[tool(
        description = "Collect 1-1000 of a known chest item without mining. Call get_entity_inventory first; item_not_found means that item is absent, not that the chest is empty. Reports conservation, not automation."
    )]
    async fn collect_from_chest(
        &self,
        Parameters(params): Parameters<CollectFromChestParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(client) => client,
            Err(error) => return format!("Error: {error}"),
        };
        let result = match client
            .collect_from_chest(params.unit_number, &params.item, params.count)
            .await
        {
            Ok(value) => serde_json::to_string_pretty(&value)
                .unwrap_or_else(|error| format!("Error: {error}")),
            Err(error) => format!("Error: {error}"),
        };
        self.with_player_messages(result).await
    }

    /// File a bounded bug report in the repository's Beads tracker.
    #[tool(
        description = "File one structured bug in this repo's fixed Beads tracker, or return an exact-title duplicate. Concrete evidence required; priority is 0-4; labels are allowlisted and optional."
    )]
    async fn file_issue(&self, Parameters(params): Parameters<FileIssueParams>) -> String {
        let reporter = match BeadsIssueReporter::new(self.issue_project_root.as_ref()) {
            Ok(reporter) => reporter,
            Err(error) => {
                return serde_json::json!({
                    "success": false,
                    "error_kind": error.kind(),
                    "error": error.to_string(),
                })
                .to_string()
            }
        };

        let request = IssueReportRequest {
            title: params.title,
            observed_behavior: params.observed_behavior,
            expected_behavior: params.expected_behavior,
            evidence: params.evidence,
            reproduction: params.reproduction,
            labels: params.labels,
            priority: params.priority,
        };
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| format!("unix:{}", duration.as_secs()))
            .unwrap_or_else(|_| "unix:0".to_string());
        let context = TrustedIssueContext {
            agent_id: std::env::var("FACTORIO_AGENT_ID").unwrap_or_else(|_| "default".to_string()),
            session_id: std::env::var("FACTORIO_BUDDY_SESSION_ID").ok(),
            commit_sha: option_env!("GIT_COMMIT_SHA").map(str::to_string),
            timestamp,
            factorio_version: std::env::var("FACTORIO_VERSION").ok(),
        };

        match reporter.file_issue(request, context).await {
            Ok(result) => serde_json::to_string_pretty(&result)
                .unwrap_or_else(|error| format!("Error: {error}")),
            Err(error) => serde_json::to_string_pretty(&serde_json::json!({
                "success": false,
                "error_kind": error.kind(),
                "error": error.to_string(),
            }))
            .unwrap_or_else(|serialization_error| format!("Error: {serialization_error}")),
        }
    }

    /// Get a recipe by exact name.
    #[tool(
        description = "Look up one recipe by exact name. Use before guessing recipe names or when craft reports an unknown/disabled recipe."
    )]
    async fn get_recipe(&self, Parameters(params): Parameters<GetRecipeParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client
            .call_remote("get_recipe", &[serde_json::json!(params.name)])
            .await
        {
            Ok(response) => response,
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Find recipes that produce an item or fluid.
    #[tool(
        description = "Find recipes that produce the requested item/fluid, such as 'boiler', 'steam-engine', or 'iron-plate'. Use this instead of guessing recipe names."
    )]
    async fn get_recipes_for_item(
        &self,
        Parameters(params): Parameters<GetRecipesForItemParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client.get_recipes_for_item(&params.item).await {
            Ok(recipes) => {
                serde_json::to_string_pretty(&recipes).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// List recipes in a crafting category.
    #[tool(
        description = "List recipes in a category such as 'crafting' or 'smelting'. Prefer get_recipes_for_item for a specific desired output."
    )]
    async fn get_recipes_by_category(
        &self,
        Parameters(params): Parameters<GetRecipesByCategoryParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client.get_recipes_by_category(&params.category).await {
            Ok(recipes) => {
                serde_json::to_string_pretty(&recipes).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Insert items into an entity.
    #[tool(
        description = "Insert items from character inventory into an entity (furnace, chest, etc)."
    )]
    async fn insert_items(&self, Parameters(params): Parameters<InsertItemsParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client
            .insert_items(
                params.unit_number,
                &params.item,
                params.count,
                &params.inventory_type,
            )
            .await
        {
            Ok(transfer) => serde_json::to_string_pretty(&transfer)
                .unwrap_or_else(|e| format!("Error: serializing transfer result: {}", e)),
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Temporarily hand-feed a furnace with fuel and source items, then verify production.
    #[tool(
        description = "Emergency recovery controller for a stalled furnace, not a normal automation step. Walks near a furnace, inserts a temporary fuel buffer into fuel inventory, inserts ore into furnace_source, then runs verify_production. Use only to recover bootstrap production or unjam a line; after success, plan a durable input/fuel belt, inserter, chest, or logistics repair instead of repeatedly calling this tool."
    )]
    async fn hand_feed_furnace(
        &self,
        Parameters(params): Parameters<HandFeedFurnaceParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let furnace = match client.get_entity(params.furnace_unit_number).await {
            Ok(entity) => entity,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let target = furnace.position;
        let verify_radius = params.verify_radius.clamp(1, 25) as f64;
        let verify_area = Area::new(
            target.x - verify_radius,
            target.y - verify_radius,
            target.x + verify_radius,
            target.y + verify_radius,
        );

        let mut actions = Vec::new();
        let mut success = true;
        let walk = client.approach_entity(params.furnace_unit_number).await;
        success &= walk.is_ok();
        actions.push(serde_json::json!({
            "operation": "approach_entity",
            "success": walk.is_ok(),
            "error": walk.as_ref().err().map(|e| e.to_string()),
        }));

        let mut fuel_inserted = false;
        if walk.is_ok() {
            let fuel = client
                .insert_items(
                    params.furnace_unit_number,
                    &params.fuel_item,
                    params.fuel_count,
                    "fuel",
                )
                .await;
            fuel_inserted = fuel.is_ok();
            success &= fuel.is_ok();
            actions.push(serde_json::json!({
                "tool": "insert_items",
                "inventory_type": "fuel",
                "item": params.fuel_item,
                "count": params.fuel_count,
                "success": fuel.is_ok(),
                "error": fuel.as_ref().err().map(|e| e.to_string()),
            }));
        } else {
            actions.push(serde_json::json!({
                "tool": "insert_items",
                "inventory_type": "fuel",
                "item": params.fuel_item,
                "count": params.fuel_count,
                "success": false,
                "skipped": true,
                "error": walk.as_ref().err().map(|error| error.to_string())
                    .unwrap_or_else(|| "authoritative entity approach failed".to_string()),
            }));
        }

        if fuel_inserted {
            let source = client
                .insert_items(
                    params.furnace_unit_number,
                    &params.source_item,
                    params.source_count,
                    "furnace_source",
                )
                .await;
            success &= source.is_ok();
            actions.push(serde_json::json!({
                "tool": "insert_items",
                "inventory_type": "furnace_source",
                "item": params.source_item,
                "count": params.source_count,
                "success": source.is_ok(),
                "error": source.as_ref().err().map(|e| e.to_string()),
            }));
        } else {
            success = false;
            actions.push(serde_json::json!({
                "tool": "insert_items",
                "inventory_type": "furnace_source",
                "item": params.source_item,
                "count": params.source_count,
                "success": false,
                "skipped": true,
                "error": "fuel insert failed or was skipped",
            }));
        }

        let verification = client.verify_production(verify_area).await;
        let result = serde_json::json!({
            "success": success,
            "furnace_unit_number": params.furnace_unit_number,
            "furnace": {
                "name": furnace.name,
                "position": furnace.position,
            },
            "actions": actions,
            "verification": match verification {
                Ok(entities) => serde_json::json!({
                    "success": true,
                    "entities": entities,
                }),
                Err(e) => serde_json::json!({
                    "success": false,
                    "error": e.to_string(),
                }),
            },
            "guidance": "If success is true and verification shows the furnace working, continue the objective. If not, fix the first failed action."
        });
        self.with_player_messages(
            serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e)),
        )
        .await
    }

    /// Perform one bounded bootstrap smelt to create the first automation parts.
    #[tool(
        description = "Bounded first-inserter bootstrap only: walk to one furnace, add a short fuel/ore buffer, wait for a small plate batch, and optionally craft one bootstrap component. This is temporary recovery, never automation completion; immediately use durable mining, fuel, input, and output controllers afterward."
    )]
    async fn bootstrap_smelting_once(
        &self,
        Parameters(params): Parameters<BootstrapSmeltingOnceParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let furnace = match client.get_entity(params.furnace_unit_number).await {
            Ok(entity) => entity,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let verify_radius = params.verify_radius.clamp(1, 25) as f64;
        let wait_ticks = params.wait_ticks.clamp(1, 7200);
        let output_count = bounded_bootstrap_output_count(params.output_count);
        let craft_recipe = params.craft_recipe.trim().to_string();
        let verify_area = Area::new(
            furnace.position.x - verify_radius,
            furnace.position.y - verify_radius,
            furnace.position.x + verify_radius,
            furnace.position.y + verify_radius,
        );

        if params.dry_run {
            let result = serde_json::json!({
                "success": true,
                "dry_run": true,
                "bounded_bootstrap": true,
                "furnace": {
                    "unit_number": furnace.unit_number,
                    "name": furnace.name,
                    "position": furnace.position,
                },
                "steps": [
                    {"operation": "approach_entity", "internal": true, "args": {"unit_number": params.furnace_unit_number}, "semantics": "move only if Factorio reports the exact furnace out of reach"},
                    {"operation": "load_furnace_fuel", "internal": true, "args": {"unit_number": params.furnace_unit_number, "item": params.fuel_item, "count": params.fuel_count, "inventory_type": "fuel"}},
                    {"operation": "load_furnace_source", "internal": true, "args": {"unit_number": params.furnace_unit_number, "item": params.source_item, "count": params.source_count, "inventory_type": "furnace_source"}},
                    {"operation": "wait_for_bootstrap_output", "internal": true, "args": {"ticks": wait_ticks}},
                    {"operation": "collect_furnace_output", "internal": true, "args": {"unit_number": params.furnace_unit_number, "item": params.output_item, "count": output_count, "inventory_type": "furnace_result"}},
                    {"tool": "craft", "internal": true, "args": {"recipe": craft_recipe, "count": params.craft_count}, "skipped_if_empty_recipe": true},
                    {"tool": "verify_production", "args": {"x": furnace.position.x, "y": furnace.position.y, "radius": params.verify_radius}}
                ],
                "guidance": "If dry_run looks right, call bootstrap_smelting_once with dry_run=false once. Do not repeat it as production; use repair_fuel_sustainability for fuel, execute_direct_smelter for ore input, plan_machine_output/build_assembler_output for plate output, or the assembler-cell controllers.",
            });
            let msg =
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
            return self.with_player_messages(msg).await;
        }

        let mut actions = Vec::new();
        let mut success = true;

        let walk = client.approach_entity(params.furnace_unit_number).await;
        success &= walk.is_ok();
        actions.push(serde_json::json!({
            "operation": "approach_entity",
            "success": walk.is_ok(),
            "error": walk.as_ref().err().map(|e| e.to_string()),
        }));

        let fuel = if walk.is_ok() {
            client
                .insert_items(
                    params.furnace_unit_number,
                    &params.fuel_item,
                    params.fuel_count,
                    "fuel",
                )
                .await
        } else {
            Err(anyhow::anyhow!(
                "authoritative entity approach failed: {}",
                walk.as_ref()
                    .err()
                    .map(|error| error.to_string())
                    .unwrap_or_else(|| "unknown approach failure".to_string())
            ))
        };
        success &= fuel.is_ok();
        actions.push(serde_json::json!({
            "operation": "load_furnace_fuel",
            "internal": true,
            "inventory_type": "fuel",
            "item": params.fuel_item,
            "count": params.fuel_count,
            "success": fuel.is_ok(),
            "error": fuel.as_ref().err().map(|e| e.to_string()),
        }));

        let source = if fuel.is_ok() {
            client
                .insert_items(
                    params.furnace_unit_number,
                    &params.source_item,
                    params.source_count,
                    "furnace_source",
                )
                .await
        } else {
            Err(anyhow::anyhow!("fuel insert failed"))
        };
        success &= source.is_ok();
        actions.push(serde_json::json!({
            "operation": "load_furnace_source",
            "internal": true,
            "inventory_type": "furnace_source",
            "item": params.source_item,
            "count": params.source_count,
            "success": source.is_ok(),
            "error": source.as_ref().err().map(|e| e.to_string()),
        }));

        let waited = if source.is_ok() {
            client.wait_ticks(wait_ticks).await
        } else {
            Err(anyhow::anyhow!("source insert failed"))
        };
        success &= waited.is_ok();
        actions.push(serde_json::json!({
            "operation": "wait_for_bootstrap_output",
            "internal": true,
            "ticks": wait_ticks,
            "success": waited.is_ok(),
            "error": waited.as_ref().err().map(|e| e.to_string()),
        }));

        let extracted = if waited.is_ok() {
            client
                .extract_items(
                    params.furnace_unit_number,
                    &params.output_item,
                    output_count,
                    "furnace_result",
                )
                .await
        } else {
            Err(anyhow::anyhow!("wait_ticks failed"))
        };
        success &= extracted.is_ok();
        actions.push(serde_json::json!({
            "operation": "collect_furnace_output",
            "internal": true,
            "inventory_type": "furnace_result",
            "item": params.output_item,
            "requested_count": output_count,
            "success": extracted.is_ok(),
            "result": extracted.as_ref().ok(),
            "error": extracted.as_ref().err().map(|e| e.to_string()),
        }));

        let craft = if craft_recipe.is_empty() {
            None
        } else if extracted.is_ok() {
            Some(client.craft(&craft_recipe, params.craft_count).await)
        } else {
            Some(Err(anyhow::anyhow!("plate extraction failed")))
        };
        if let Some(craft_result) = &craft {
            match craft_result {
                Ok(result) => {
                    let admitted = result.success;
                    success &= admitted;
                    actions.push(serde_json::json!({
                        "tool": "craft",
                        "internal": true,
                        "recipe": craft_recipe,
                        "count": params.craft_count,
                        "success": admitted,
                        "completed": false,
                        "admission": result.status_evidence(),
                        "result": result,
                        "error": result.error,
                    }));
                    if admitted {
                        let completion = match client.complete_craft_admission().await {
                            Ok(completion) => completion,
                            Err(error) => serde_json::json!({
                                "success": false,
                                "completed": false,
                                "status": "observation_failed",
                                "error_kind": "craft_observation_failed",
                                "error": error.to_string(),
                            }),
                        };
                        let completed = completion
                            .get("completed")
                            .and_then(serde_json::Value::as_bool)
                            == Some(true);
                        success &= completed;
                        actions.push(serde_json::json!({
                            "tool": "wait_for_crafting",
                            "internal": true,
                            "success": completed,
                            "completed": completed,
                            "result": completion,
                        }));
                    }
                }
                Err(error) => {
                    success = false;
                    actions.push(serde_json::json!({
                        "tool": "craft",
                        "internal": true,
                        "recipe": craft_recipe,
                        "count": params.craft_count,
                        "success": false,
                        "completed": false,
                        "error": error.to_string(),
                    }));
                }
            }
        } else {
            actions.push(serde_json::json!({
                "tool": "craft",
                "skipped": true,
                "reason": "craft_recipe empty",
            }));
        }

        let (verification, verification_call_ok, _) =
            match client.verify_production(verify_area).await {
                Ok(entities) => production_verification_json(entities),
                Err(e) => (
                    serde_json::json!({
                        "success": false,
                        "error": e.to_string(),
                    }),
                    false,
                    false,
                ),
            };

        let inventory = client.character_inventory().await.ok();
        let result = serde_json::json!({
            "success": success,
            "bounded_bootstrap": true,
            "temporary_recovery_not_automation": true,
            "furnace": {
                "unit_number": furnace.unit_number,
                "name": furnace.name,
                "position": furnace.position,
            },
            "actions": actions,
            "verification": verification,
            "verification_call_ok": verification_call_ok,
            "inventory_after": inventory,
            "guidance": "This is a one-shot bootstrap, not durable production. Next call should build automation: repair_fuel_sustainability for fuel, execute_direct_smelter for ore input, plan_machine_output/build_assembler_output for plate output, or the assembler-cell controllers. Do not loop bootstrap_smelting_once as a production strategy.",
        });
        let msg = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(msg).await
    }

    /// Extract items from an entity into player inventory.
    #[tool(
        description = "Extract items from an entity (furnace, chest, etc) into character inventory."
    )]
    async fn extract_items(&self, Parameters(params): Parameters<ExtractItemsParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client
            .extract_items(
                params.unit_number,
                &params.item,
                params.count,
                &params.inventory_type,
            )
            .await
        {
            Ok(extracted) => format!("Extracted {} {} from entity", extracted, params.item),
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Set recipe on a crafting machine.
    #[tool(
        description = "Set or clear the recipe on an assembling machine, chemical plant, or other crafting entity. Use empty string to clear the recipe."
    )]
    async fn set_recipe(&self, Parameters(params): Parameters<SetRecipeParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = if params.recipe.is_empty() {
            match client.clear_recipe(params.unit_number).await {
                Ok(()) => "Recipe cleared".to_string(),
                Err(e) => format!("Error: {}", e),
            }
        } else {
            match client.set_recipe(params.unit_number, &params.recipe).await {
                Ok(()) => format!("Recipe set to '{}'", params.recipe),
                Err(e) => format!("Error: {}", e),
            }
        };
        self.with_player_messages(result).await
    }

    /// Remove an entity.
    #[tool(description = "Preview/remove an exact unit with dependency advisory.")]
    async fn remove_entity(&self, Parameters(params): Parameters<RemoveEntityParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client
            .remove_entity_with_report(params.unit_number, params.dry_run)
            .await
        {
            Ok(report) => serde_json::to_string_pretty(&report)
                .unwrap_or_else(|error| format!("Error: {error}")),
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Rotate an existing entity by unit number.
    #[tool(
        description = "Rotate an existing entity by exact unit number. For inserters, direction is the PICKUP side and the result reports Factorio's exact pickup_position and drop_position. Never use coordinate-based guessing when changing existing infrastructure."
    )]
    async fn rotate_entity(&self, Parameters(params): Parameters<RotateEntityParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let direction = match Direction::parse(&params.direction) {
            Some(d) => d,
            None => {
                return self
                    .with_player_messages(semantic_failure(
                        "invalid_direction",
                        format!(
                            "Invalid direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                            params.direction
                        ),
                    ))
                    .await
            }
        };

        let result = match client
            .rotate_entity(params.unit_number, direction.to_factorio())
            .await
        {
            Ok(value) => {
                serde_json::to_string_pretty(&value).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Replace an existing inserter's complete item whitelist.
    #[tool(
        description = "Replace an exact inserter whitelist atomically; [] disables filtering. Readback or held-item return failure rolls back. A held item excluded by the new whitelist is returned intact to the character without replacing the inserter. Filtering does not purify upstream belts."
    )]
    async fn configure_inserter(
        &self,
        Parameters(params): Parameters<ConfigureInserterParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(client) => client,
            Err(error) => {
                return self
                    .with_player_messages(semantic_failure("connection_failed", error))
                    .await
            }
        };

        let result = match client
            .configure_inserter(params.unit_number, &params.allowed_items)
            .await
        {
            Ok(value) => serde_json::to_string_pretty(&value).unwrap_or_else(|error| {
                semantic_failure("serialization_failed", error.to_string())
            }),
            Err(error) => semantic_failure("configure_inserter_failed", error.to_string()),
        };
        self.with_player_messages(result).await
    }

    async fn route_belt_core(
        &self,
        client: &mut FactorioClient,
        params: &RouteBeltParams,
    ) -> Result<serde_json::Value, String> {
        self.route_belt_core_avoiding(client, params, &HashSet::new())
            .await
    }

    async fn route_belt_core_avoiding(
        &self,
        client: &mut FactorioClient,
        params: &RouteBeltParams,
        reserved_route_tiles: &HashSet<GridPos>,
    ) -> Result<serde_json::Value, String> {
        let padding = params.search_radius as i32;
        let area = Area {
            left_top: Position::new(
                (params.from_x.min(params.to_x) - padding) as f64,
                (params.from_y.min(params.to_y) - padding) as f64,
            ),
            right_bottom: Position::new(
                (params.from_x.max(params.to_x) + padding + 1) as f64,
                (params.from_y.max(params.to_y) + padding + 1) as f64,
            ),
        };

        let (mut collision_map, route_entities) = match client
            .build_collision_map_with_entities(area)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(e) => {
                let error = e.to_string();
                if error.contains("Packet too large") {
                    let waypoint = route_segment_waypoint(
                        params.from_x,
                        params.from_y,
                        params.to_x,
                        params.to_y,
                        40,
                    );
                    return Ok(serde_json::json!({
                        "success": false,
                        "dry_run": params.dry_run,
                        "error_kind": "route_area_too_large",
                        "error": format!("Error: building collision map: {}", error),
                        "from": { "x": params.from_x, "y": params.from_y },
                        "to": { "x": params.to_x, "y": params.to_y },
                        "belt_type": params.belt_type,
                        "search_radius": params.search_radius,
                        "span": {
                            "x": (params.to_x - params.from_x).abs(),
                            "y": (params.to_y - params.from_y).abs(),
                            "manhattan": (params.to_x - params.from_x).abs()
                                + (params.to_y - params.from_y).abs(),
                        },
                        "materials_sufficient": false,
                        "next_segment": {
                            "from": { "x": params.from_x, "y": params.from_y },
                            "to": { "x": waypoint.x, "y": waypoint.y },
                            "route_belt_args": {
                                "from_x": params.from_x,
                                "from_y": params.from_y,
                                "to_x": waypoint.x,
                                "to_y": waypoint.y,
                                "belt_type": params.belt_type,
                                "search_radius": params.search_radius,
                                "dry_run": params.dry_run,
                                "respect_zones": params.respect_zones,
                                "allow_underground": params.allow_underground,
                                "extend_existing": params.extend_existing,
                            },
                            "after_success": {
                                "retry_from_x": waypoint.x,
                                "retry_from_y": waypoint.y,
                                "retry_to_x": params.to_x,
                                "retry_to_y": params.to_y,
                            }
                        },
                        "guidance": "Route is too large for one collision-map request. Call route_belt with next_segment.route_belt_args to extend the belt highway, then retry the original durable controller from next_segment.after_success.retry_from_* toward the final target. Use route_belt alone for intermediate waypoints; place the terminal inserter only when the final endpoint is reachable.",
                    }));
                }
                return Ok(route_belt_failure_json(
                    params,
                    "infrastructure_failure",
                    format!("Error: building collision map: {}", e),
                ));
            }
        };

        let underground_config = if params.allow_underground {
            if let Some(config) = UndergroundConfig::from_belt_type(&params.belt_type) {
                match client.is_tech_researched(&config.required_tech).await {
                    Ok(true) => Some(config),
                    Ok(false) | Err(_) => None,
                }
            } else {
                None
            }
        } else {
            None
        };
        let underground_entity_name = underground_config
            .as_ref()
            .map(|config| config.entity_name.as_str());
        let (mut underground_forbidden_tiles, preserved_underground_pairs) =
            underground_entity_name.map_or_else(
                || (HashSet::new(), Vec::new()),
                |name| existing_underground_pair_reservations(&route_entities, name),
            );

        // Resource entities are valid terrain for ordinary logistics. Record
        // their exact live tiles for route diagnostics without blocking A*;
        // Factorio/Lua placement preflights below remain authoritative.
        let mut resource_tiles = HashSet::new();
        for entity in &route_entities {
            if entity.entity_type.as_deref() != Some("resource") {
                continue;
            }
            if let Some(bounds) = &entity.bounding_box {
                for x in bounds.left_top.x.floor() as i32..bounds.right_bottom.x.ceil() as i32 {
                    for y in bounds.left_top.y.floor() as i32..bounds.right_bottom.y.ceil() as i32 {
                        let tile = GridPos::new(x, y);
                        resource_tiles.insert(tile);
                    }
                }
            } else {
                let tile = GridPos::from_position(&entity.position);
                resource_tiles.insert(tile);
            }
        }

        let mut existing_surface_belts: HashMap<GridPos, Entity> = HashMap::new();
        for entity in route_entities {
            if !is_existing_belt_entity(&entity.name)
                || entity.entity_type.as_deref() != Some("transport-belt")
            {
                continue;
            }
            let tile = GridPos::from_position(&entity.position);
            if params.extend_existing {
                collision_map.unblock(tile);
                existing_surface_belts.insert(tile, entity);
            } else {
                // The shared collision map models where a character can walk,
                // so ordinary belts are intentionally absent from it. An
                // independent route cannot treat those occupied tiles as
                // empty: doing so makes Factorio's manual placement check
                // advertise a fast replacement that this controller neither
                // requested nor may safely perform.
                collision_map.block(tile);
            }
        }

        let start = GridPos::new(params.from_x, params.from_y);
        let goal = GridPos::new(params.to_x, params.to_y);

        if params.respect_zones {
            let memory = AgentMemory::load();
            for zone in memory.zones.values() {
                match zone.zone_type.belt_routing() {
                    BeltRouting::Blocked => collision_map.block_area(&zone.bounds),
                    BeltRouting::Preferred => collision_map.prefer_area(&zone.bounds),
                    BeltRouting::Allowed => {}
                }
            }
        }

        if existing_surface_belts.contains_key(&start) {
            underground_forbidden_tiles.insert(start);
        }
        if existing_surface_belts.contains_key(&goal) {
            underground_forbidden_tiles.insert(goal);
        }
        for tile in reserved_route_tiles {
            collision_map.block(*tile);
            underground_forbidden_tiles.insert(*tile);
        }

        let routing_options = RoutingOptions {
            allow_underground: underground_config.is_some(),
            underground_config: underground_config.clone(),
            underground_penalty: 0.5,
            underground_skip_cost: 0.05,
            underground_forbidden_tiles,
        };
        let mut rejected_existing = Vec::new();
        let result = loop {
            let candidate =
                find_belt_route_with_options(start, goal, &collision_map, &routing_options);
            if !candidate.success {
                if !rejected_existing.is_empty() {
                    let incompatible_existing_count = rejected_existing.len();
                    let incompatible_existing: Vec<_> =
                        rejected_existing.iter().take(8).cloned().collect();
                    return Ok(serde_json::json!({
                        "success": false,
                        "complete_route": false,
                        "dry_run": params.dry_run,
                        "error_kind": "incompatible_existing_belt",
                        "error": "No complete route can reuse the encountered existing belts without violating their type or flow direction. No belts were placed.",
                        "from": { "x": params.from_x, "y": params.from_y },
                        "to": { "x": params.to_x, "y": params.to_y },
                        "belt_type": params.belt_type,
                        "incompatible_existing_count": incompatible_existing_count,
                        "incompatible_existing": incompatible_existing,
                        "materials_sufficient": false,
                        "guidance": "Use a correctly aligned belt highway, or explicitly rotate/rebuild the exact incompatible units before retrying. The router will not silently skip wrong-facing belts.",
                    }));
                }
                return Ok(route_belt_failure_json(
                    params,
                    "route_failed",
                    format!(
                        "Route failed: {}",
                        candidate
                            .error
                            .unwrap_or_else(|| "unknown error".to_string())
                    ),
                ));
            }

            let mut incompatible = Vec::new();
            for planned in &candidate.belts {
                let tile = GridPos::from_position(&planned.position);
                let Some(existing) = existing_surface_belts.get(&tile) else {
                    continue;
                };
                if let Err(error) =
                    existing_belt_compatibility(existing, planned, &params.belt_type)
                {
                    incompatible.push((
                        tile,
                        serde_json::json!({
                            "position": planned.position,
                            "unit_number": existing.unit_number,
                            "name": existing.name,
                            "actual_direction": Direction::from_factorio(existing.direction),
                            "required_direction": planned.direction,
                            "error": error,
                        }),
                    ));
                }
            }
            if incompatible.is_empty() {
                break candidate;
            }

            let endpoint_incompatibility =
                endpoint_belt_incompatibility(&incompatible, start, goal);
            for (tile, error) in incompatible {
                collision_map.block(tile);
                rejected_existing.push(error);
            }
            if let Some(endpoint_incompatibility) = endpoint_incompatibility {
                let incompatible_existing_count = rejected_existing.len();
                let incompatible_existing: Vec<_> =
                    rejected_existing.iter().take(4).cloned().collect();
                let required_direction = endpoint_incompatibility
                    .get("required_direction")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let unit_number = endpoint_incompatibility
                    .get("unit_number")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let retry_args =
                    serde_json::to_value(params).unwrap_or_else(|_| serde_json::json!({}));
                return Ok(serde_json::json!({
                    "success": false,
                    "complete_route": false,
                    "dry_run": params.dry_run,
                    "error_kind": "incompatible_existing_belt",
                    "error": "An existing endpoint belt is not directionally compatible with the planned route. No belts were placed.",
                    "from": { "x": params.from_x, "y": params.from_y },
                    "to": { "x": params.to_x, "y": params.to_y },
                    "belt_type": params.belt_type,
                    "endpoint_incompatibility": endpoint_incompatibility,
                    "incompatible_existing_count": incompatible_existing_count,
                    "incompatible_existing": incompatible_existing,
                    "materials_sufficient": false,
                    "topology": compact_belt_topology(candidate.topology.as_ref()),
                    "next_action": {
                        "tool": "rotate_entity",
                        "args": {
                            "unit_number": unit_number,
                            "direction": required_direction,
                        },
                        "reason": "Align the exact existing endpoint belt with this planned route.",
                        "after_success": {
                            "tool": "route_belt",
                            "args": retry_args,
                        },
                    },
                    "guidance": "Rotate only endpoint_incompatibility.unit_number to its required direction, then retry this same dry-run. Other rejected candidates are diagnostic only.",
                }));
            }
        };

        if result
            .topology
            .as_ref()
            .is_none_or(|topology| !topology.connected)
        {
            return Ok(serde_json::json!({
                "success": false,
                "complete_route": false,
                "dry_run": params.dry_run,
                "error_kind": "disconnected_topology",
                "error": "The planned belt topology is not connected end to end. No belts were placed.",
                "from": { "x": params.from_x, "y": params.from_y },
                "to": { "x": params.to_x, "y": params.to_y },
                "belt_type": params.belt_type,
                "belt_count": result.belt_count,
                "placed": 0,
                "materials_sufficient": false,
                "topology": compact_belt_topology(result.topology.as_ref()),
                "guidance": "Choose different endpoints or disable underground routing, then dry-run the complete route again.",
            }));
        }

        let existing_belt_tiles: HashSet<GridPos> = result
            .belts
            .iter()
            .filter_map(|planned| {
                let tile = GridPos::from_position(&planned.position);
                existing_surface_belts
                    .get(&tile)
                    .and_then(|existing| {
                        existing_belt_compatibility(existing, planned, &params.belt_type).ok()
                    })
                    .map(|()| tile)
            })
            .collect();

        let inventory = match client.character_inventory().await {
            Ok(inventory) => inventory,
            Err(e) => {
                return Ok(route_belt_failure_json(
                    params,
                    "infrastructure_failure",
                    format!("Error: checking inventory: {}", e),
                ));
            }
        };
        let surface_belts_needed = result
            .belts
            .iter()
            .filter(|belt| {
                belt.kind == BeltKind::Surface
                    && !existing_belt_tiles.contains(&GridPos::from_position(&belt.position))
            })
            .count() as u32;
        let underground_belts_needed = result
            .belts
            .iter()
            .filter(|belt| {
                belt.kind != BeltKind::Surface
                    && !existing_belt_tiles.contains(&GridPos::from_position(&belt.position))
            })
            .count() as u32;
        let surface_belts_have = inventory
            .items
            .iter()
            .find(|i| i.name == params.belt_type)
            .map(|i| i.count)
            .unwrap_or(0);
        let underground_belt_name = underground_config.as_ref().map(|c| c.entity_name.as_str());
        let underground_belts_have = underground_belt_name
            .and_then(|name| inventory.items.iter().find(|i| i.name == name))
            .map(|i| i.count)
            .unwrap_or(0);

        let build_belts = result.belts.clone();
        let mut planned_surface_resource_tiles_crossed: Vec<GridPos> = build_belts
            .iter()
            .filter(|belt| belt.kind == BeltKind::Surface)
            .map(|belt| GridPos::from_position(&belt.position))
            .filter(|tile| resource_tiles.contains(tile))
            .collect();
        planned_surface_resource_tiles_crossed.sort_by_key(|tile| (tile.x, tile.y));
        planned_surface_resource_tiles_crossed.dedup();
        let materials_sufficient = surface_belts_have >= surface_belts_needed
            && underground_belts_have >= underground_belts_needed;
        let material_shortfall = route_material_shortfall(
            &params.belt_type,
            surface_belts_needed,
            surface_belts_have,
            underground_belt_name,
            underground_belts_needed,
            underground_belts_have,
        );

        let materials = serde_json::json!({
            "surface": {
                "item": params.belt_type,
                "needed": surface_belts_needed,
                "available": surface_belts_have,
                "sufficient": surface_belts_have >= surface_belts_needed,
            },
            "underground": {
                "item": underground_belt_name,
                "needed": underground_belts_needed,
                "available": underground_belts_have,
                "sufficient": underground_belts_have >= underground_belts_needed,
            },
        });

        let underground_entity = underground_config.as_ref().map(|c| c.entity_name.as_str());
        let skipped_existing = build_belts
            .iter()
            .filter(|belt| existing_belt_tiles.contains(&GridPos::from_position(&belt.position)))
            .count();
        let mut preflight_errors = Vec::new();

        // A dry-run is an executable placement plan, not just an A* sketch.
        // Check every new tile with the same script-build semantics used by
        // the mutation path before advertising it as ready to execute.
        for belt in &build_belts {
            if existing_belt_tiles.contains(&GridPos::from_position(&belt.position)) {
                continue;
            }
            let entity_name = match belt.kind {
                BeltKind::Surface => &params.belt_type,
                BeltKind::UndergroundEntry | BeltKind::UndergroundExit => {
                    underground_entity.unwrap_or(&params.belt_type)
                }
            };
            match client
                .check_entity_placement(entity_name, belt.position, belt.direction)
                .await
            {
                Ok(report) if placement_report_allowed(&report) => {}
                Ok(report) => preflight_errors.push(serde_json::json!({
                    "position": belt.position,
                    "entity": entity_name,
                    "direction": belt.direction,
                    "error": report.get("error"),
                    "occupied_by": report.get("occupied_by"),
                })),
                Err(error) => preflight_errors.push(serde_json::json!({
                    "position": belt.position,
                    "entity": entity_name,
                    "direction": belt.direction,
                    "error": error.to_string(),
                })),
            }
        }

        if !preflight_errors.is_empty() {
            return Ok(serde_json::json!({
                "success": false,
                "complete_route": false,
                "dry_run": params.dry_run,
                "error_kind": "placement_preflight_failed",
                "error": "Complete route preflight failed; no belts were placed.",
                "from": { "x": params.from_x, "y": params.from_y },
                "to": { "x": params.to_x, "y": params.to_y },
                "belt_type": params.belt_type,
                "belt_count": result.belt_count,
                "new_belt_count": surface_belts_needed + underground_belts_needed,
                "placed": 0,
                "skipped_existing": skipped_existing,
                "materials": materials,
                "materials_sufficient": materials_sufficient,
                "topology": compact_belt_topology(result.topology.as_ref()),
                "resource_tiles_observed": resource_tiles.len(),
                "planned_surface_resource_tiles_crossed": &planned_surface_resource_tiles_crossed,
                "planned_surface_resource_tiles_crossed_count": planned_surface_resource_tiles_crossed.len(),
                "preflight_errors": preflight_errors,
                "guidance": "Fix the reported blocker or choose different endpoints, then retry the complete route. Do not place around the failure one tile at a time.",
            }));
        }

        if params.dry_run {
            let planned_new_belts: Vec<&BeltPlacement> = build_belts
                .iter()
                .filter(|belt| {
                    !existing_belt_tiles.contains(&GridPos::from_position(&belt.position))
                })
                .collect();
            let mut execute_args =
                serde_json::to_value(params).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(object) = execute_args.as_object_mut() {
                object.insert("dry_run".to_string(), serde_json::json!(false));
            }
            return Ok(serde_json::json!({
                "success": true,
                "dry_run": true,
                "from": { "x": params.from_x, "y": params.from_y },
                "to": { "x": params.to_x, "y": params.to_y },
                "belt_type": params.belt_type,
                "belt_count": result.belt_count,
                "new_belt_count": surface_belts_needed + underground_belts_needed,
                "turn_count": result.turn_count,
                "underground_count": result.underground_count,
                "resource_tiles_observed": resource_tiles.len(),
                "planned_surface_resource_tiles_crossed": &planned_surface_resource_tiles_crossed,
                "planned_surface_resource_tiles_crossed_count": planned_surface_resource_tiles_crossed.len(),
                "preserved_underground_pair_count": preserved_underground_pairs.len(),
                "preserved_underground_pairs": &preserved_underground_pairs,
                "materials": materials,
                "materials_sufficient": materials_sufficient,
                "ready_to_execute": materials_sufficient,
                "material_shortfall": material_shortfall,
                "topology": compact_belt_topology(result.topology.as_ref()),
                "planned_belts": build_belts,
                "planned_new_belts": planned_new_belts,
                "ready_to_call": {
                    "tool": "route_belt",
                    "execute_args": execute_args,
                },
                "guidance": if materials_sufficient {
                    "Call route_belt with ready_to_call.execute_args to place the complete route atomically. Do not place its belts one tile at a time."
                } else {
                    "Craft the missing belts, then rerun route_belt. No partial route will be placed."
                },
            }));
        }

        if let Some(shortfall) = material_shortfall {
            let mut failure = route_belt_failure_json(params, "insufficient_materials", shortfall);
            if let Some(object) = failure.as_object_mut() {
                object.insert("materials".to_string(), materials);
                object.insert("placed".to_string(), serde_json::json!(0));
                object.insert(
                    "topology".to_string(),
                    compact_belt_topology(result.topology.as_ref()),
                );
                object.insert(
                    "resource_tiles_observed".to_string(),
                    serde_json::json!(resource_tiles.len()),
                );
                object.insert(
                    "planned_surface_resource_tiles_crossed".to_string(),
                    serde_json::json!(&planned_surface_resource_tiles_crossed),
                );
                object.insert(
                    "planned_surface_resource_tiles_crossed_count".to_string(),
                    serde_json::json!(planned_surface_resource_tiles_crossed.len()),
                );
                object.insert(
                    "guidance".to_string(),
                    serde_json::json!(
                        "Craft the missing belts and retry the same complete route. Do not place a shorter prefix."
                    ),
                );
            }
            return Ok(failure);
        }

        let mut placed_entities = Vec::new();
        let mut placement_error: Option<String> = None;

        for belt in &build_belts {
            let belt_tile = GridPos::from_position(&belt.position);
            if existing_belt_tiles.contains(&belt_tile) {
                continue;
            }
            let entity_name = match belt.kind {
                BeltKind::Surface => &params.belt_type,
                BeltKind::UndergroundEntry | BeltKind::UndergroundExit => {
                    underground_entity.unwrap_or(&params.belt_type)
                }
            };
            let place_result = if belt.kind == BeltKind::UndergroundEntry
                || belt.kind == BeltKind::UndergroundExit
            {
                let ug_type = match belt.kind {
                    BeltKind::UndergroundEntry => "input",
                    BeltKind::UndergroundExit => "output",
                    _ => "input",
                };
                client
                    .place_underground_belt(entity_name, belt.position, belt.direction, ug_type)
                    .await
            } else {
                client
                    .place_entity(entity_name, belt.position, belt.direction)
                    .await
            };

            match place_result {
                Ok(entity) => placed_entities.push(entity),
                Err(error) => {
                    placement_error = Some(format!(
                        "({}, {}): {}",
                        belt.position.x, belt.position.y, error
                    ));
                    break;
                }
            }
        }

        if let Some(error) = placement_error {
            let attempted_placements = placed_entities.len();
            let placed_unit_numbers: Vec<u32> = placed_entities
                .iter()
                .filter_map(|entity| entity.unit_number)
                .collect();
            let missing_identity_errors: Vec<serde_json::Value> = placed_entities
                .iter()
                .filter(|entity| entity.unit_number.is_none())
                .map(rollback_missing_identity_error)
                .collect();
            let mut rollback = rollback_exact_units(client, &placed_unit_numbers).await;
            let rolled_back = rollback
                .get("removed_units")
                .and_then(serde_json::Value::as_array)
                .map_or(0, Vec::len);
            let mut rollback_errors = rollback
                .get("errors")
                .and_then(serde_json::Value::as_array)
                .cloned()
                .unwrap_or_default();
            for rollback_error in &mut rollback_errors {
                let Some(unit_number) = rollback_error
                    .get("unit_number")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|unit_number| u32::try_from(unit_number).ok())
                else {
                    continue;
                };
                let Some(entity) = placed_entities
                    .iter()
                    .find(|entity| entity.unit_number == Some(unit_number))
                else {
                    continue;
                };
                if let Some(error) = rollback_error.as_object_mut() {
                    error.insert("name".to_string(), serde_json::json!(entity.name));
                    error.insert("position".to_string(), serde_json::json!(entity.position));
                }
            }
            rollback_errors.extend(missing_identity_errors.iter().cloned());
            let rollback_success = rollback
                .get("success")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false)
                && missing_identity_errors.is_empty();
            if let Some(report) = rollback.as_object_mut() {
                report.insert(
                    "success".to_string(),
                    serde_json::Value::Bool(rollback_success),
                );
                report.insert(
                    "identified_units".to_string(),
                    serde_json::json!(placed_unit_numbers),
                );
                report.insert(
                    "missing_identity_errors".to_string(),
                    serde_json::json!(missing_identity_errors),
                );
            }
            return Ok(serde_json::json!({
                "success": false,
                "complete_route": false,
                "dry_run": false,
                "error_kind": "placement_failed",
                "error": error,
                "from": { "x": params.from_x, "y": params.from_y },
                "to": { "x": params.to_x, "y": params.to_y },
                "belt_type": params.belt_type,
                "belt_count": result.belt_count,
                "new_belt_count": surface_belts_needed + underground_belts_needed,
                "placement_attempted": attempted_placements,
                "rolled_back": rolled_back,
                "placed": attempted_placements.saturating_sub(rolled_back),
                "skipped_existing": skipped_existing,
                "materials": materials,
                "materials_sufficient": true,
                "topology": compact_belt_topology(result.topology.as_ref()),
                "resource_tiles_observed": resource_tiles.len(),
                "planned_surface_resource_tiles_crossed": &planned_surface_resource_tiles_crossed,
                "planned_surface_resource_tiles_crossed_count": planned_surface_resource_tiles_crossed.len(),
                "rollback": rollback,
                "rollback_errors": rollback_errors,
                "guidance": if rollback_success {
                    "The failed route was rolled back. Inspect the reported tile and retry the complete route."
                } else {
                    "Rollback was incomplete. Inspect rollback_errors; entries missing unit identity were deliberately not removed by coordinates."
                },
            }));
        }

        Ok(serde_json::json!({
            "success": true,
            "complete_route": true,
            "dry_run": false,
            "error_kind": serde_json::Value::Null,
            "from": { "x": params.from_x, "y": params.from_y },
            "to": { "x": params.to_x, "y": params.to_y },
            "built_to": build_belts.last().map(|belt| serde_json::json!({
                "x": belt.position.x,
                "y": belt.position.y,
            })),
            "belt_type": params.belt_type,
            "belt_count": result.belt_count,
            "new_belt_count": surface_belts_needed + underground_belts_needed,
            "placed": placed_entities.len(),
            "placed_entities": placed_entities.iter().map(|entity| serde_json::json!({
                "unit_number": entity.unit_number,
                "name": entity.name,
                "position": entity.position,
            })).collect::<Vec<_>>(),
            "skipped_existing": skipped_existing,
            "turn_count": result.turn_count,
            "underground_count": result.underground_count,
            "resource_tiles_observed": resource_tiles.len(),
            "planned_surface_resource_tiles_crossed": &planned_surface_resource_tiles_crossed,
            "planned_surface_resource_tiles_crossed_count": planned_surface_resource_tiles_crossed.len(),
            "preserved_underground_pair_count": preserved_underground_pairs.len(),
            "preserved_underground_pairs": &preserved_underground_pairs,
            "materials": materials,
            "materials_sufficient": materials_sufficient,
            "topology": compact_belt_topology(result.topology.as_ref()),
            "errors": [],
        }))
    }

    /// Route belts from point A to point B using A* pathfinding.
    #[tool(
        description = "Plan or atomically build a complete A* belt route. Surface belts may cross resources and use ore endpoints; reports include observed resources and exact surface crossings. dry_run returns executable args. Live placement preflight, all-or-nothing materials, reuse, and rollback still apply."
    )]
    async fn route_belt(&self, Parameters(params): Parameters<RouteBeltParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        match self.route_belt_core(&mut client, &params).await {
            Ok(value) => {
                self.with_player_messages(
                    serde_json::to_string_pretty(&value)
                        .unwrap_or_else(|e| format!("Error: {}", e)),
                )
                .await
            }
            Err(error) => {
                let value = route_belt_failure_json(&params, "infrastructure_failure", error);
                self.with_player_messages(
                    serde_json::to_string_pretty(&value)
                        .unwrap_or_else(|e| format!("Error: {}", e)),
                )
                .await
            }
        }
    }

    async fn build_fuel_supply_core(
        &self,
        client: &mut FactorioClient,
        params: &BuildFuelSupplyParams,
    ) -> Result<serde_json::Value, String> {
        if let Some(failure) = unsupported_fuel_transport(params.allow_underground, params.dry_run)
        {
            return Ok(failure);
        }
        let consumer = match client.get_entity(params.consumer_unit_number).await {
            Ok(entity) => entity,
            Err(e) => return Err(format!("Error: {}", e)),
        };
        let source_unit_number = params.source_unit_number;
        let provisional_self_bootstrap = match params.provisional_source_unit_number {
            Some(source_unit)
                if source_unit == params.consumer_unit_number
                    && source_unit == source_unit_number
                    && consumer.unit_number == Some(source_unit)
                    && consumer.entity_type.as_deref() == Some("mining-drill")
                    && consumer.name == "burner-mining-drill"
                    && params.bootstrap_consumer_fuel_count <= 10 =>
            {
                true
            }
            Some(source_unit) => {
                return Ok(serde_json::json!({
                    "success": false,
                    "error_kind": "invalid_provisional_self_source",
                    "error": "A provisional coal source must be the exact burner mining-drill consumer and use a bounded 0-10 item startup buffer.",
                    "consumer_unit_number": params.consumer_unit_number,
                    "provisional_source_unit_number": source_unit,
                    "bootstrap_consumer_fuel_count": params.bootstrap_consumer_fuel_count,
                }));
            }
            None if params.bootstrap_consumer_fuel_count == 0 => false,
            None => {
                return Ok(serde_json::json!({
                    "success": false,
                    "error_kind": "bootstrap_without_provisional_source",
                    "error": "Consumer startup fuel is only valid for the exact provisional coal-drill self-source selected by diagnose_fuel_sustainability.",
                    "consumer_unit_number": params.consumer_unit_number,
                    "bootstrap_consumer_fuel_count": params.bootstrap_consumer_fuel_count,
                }));
            }
        };
        if provisional_self_bootstrap {
            let drop = client
                .call_remote(
                    "get_entity_drop_position",
                    &[serde_json::json!(params.consumer_unit_number)],
                )
                .await
                .map_err(|error| format!("validating provisional coal-drill output: {error}"))?;
            let drop: serde_json::Value = serde_json::from_str(&drop)
                .map_err(|error| format!("decoding provisional coal-drill output: {error}"))?;
            let expected_from_x = drop
                .get("drop_x")
                .and_then(|value| value.as_f64())
                .map(|value| value.floor() as i32);
            let expected_from_y = drop
                .get("drop_y")
                .and_then(|value| value.as_f64())
                .map(|value| value.floor() as i32);
            if expected_from_x != Some(params.from_x) || expected_from_y != Some(params.from_y) {
                return Ok(serde_json::json!({
                    "success": false,
                    "error_kind": "provisional_source_output_mismatch",
                    "error": "The provisional route must start at the exact output tile of the same burner coal drill.",
                    "consumer_unit_number": params.consumer_unit_number,
                    "requested_from": {"x": params.from_x, "y": params.from_y},
                    "expected_from": {"x": expected_from_x, "y": expected_from_y},
                }));
            }
        }
        let inserter_direction = match Direction::parse(&params.inserter_direction) {
            Some(direction) => direction,
            None => {
                return Err(format!(
                    "Invalid inserter_direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                    params.inserter_direction
                ));
            }
        };
        if params.inserter_name.contains("burner")
            && !(1..=10).contains(&params.inserter_fuel_count)
        {
            return Ok(serde_json::json!({
                "success": false,
                "error_kind": "invalid_inserter_bootstrap_count",
                "error": "A burner fuel inserter requires a bounded 1-10 item startup buffer.",
                "inserter_fuel_count": params.inserter_fuel_count,
            }));
        }
        let inserter_position = Position::new(params.inserter_x, params.inserter_y);
        let fuel_inserter_placement = ControllerPlacement {
            label: "fuel_inserter",
            item_name: &params.inserter_name,
            entity_name: &params.inserter_name,
            position: inserter_position,
            direction: inserter_direction,
        };
        let inserter_args = serde_json::json!({
            "entity_name": params.inserter_name,
            "x": params.inserter_x,
            "y": params.inserter_y,
            "direction": params.inserter_direction,
        });

        let diagnosed_source = match client.get_entity(source_unit_number).await {
            Ok(entity) => entity,
            Err(error) => {
                return Ok(serde_json::json!({
                    "success": false,
                    "error_kind": "fuel_source_identity_changed",
                    "error": error.to_string(),
                    "source_unit_number": source_unit_number,
                    "guidance": "The diagnosed source no longer exists. Refresh diagnose_fuel_sustainability; no infrastructure was changed.",
                }));
            }
        };
        let source_tap_entity = Some(&diagnosed_source).filter(|source| {
            matches!(
                source.entity_type.as_deref(),
                Some("transport-belt" | "underground-belt" | "container" | "logistic-container")
            )
        });
        if let Some(source) = source_tap_entity {
            let actual_source_tile = GridPos::from_position(&source.position);
            let requested_source_tile = GridPos::new(params.from_x, params.from_y);
            if actual_source_tile != requested_source_tile {
                return Ok(serde_json::json!({
                    "success": false,
                    "error_kind": "fuel_source_tile_mismatch",
                    "error": "The exact diagnosed source is no longer at the transaction's source tile.",
                    "source_unit_number": source.unit_number,
                    "requested_source_tile": requested_source_tile,
                    "actual_source_tile": actual_source_tile,
                    "guidance": "Refresh diagnose_fuel_sustainability; no source or route entity was changed.",
                }));
            }
        } else if diagnosed_source.entity_type.as_deref() == Some("mining-drill") {
            let drop = client
                .call_remote(
                    "get_entity_drop_position",
                    &[serde_json::json!(source_unit_number)],
                )
                .await
                .map_err(|error| format!("validating coal-drill output: {error}"))?;
            let drop: serde_json::Value = serde_json::from_str(&drop)
                .map_err(|error| format!("decoding coal-drill output: {error}"))?;
            let actual_route_tile = GridPos::new(
                drop.get("drop_x")
                    .and_then(serde_json::Value::as_f64)
                    .map(|value| value.floor() as i32)
                    .unwrap_or(i32::MIN),
                drop.get("drop_y")
                    .and_then(serde_json::Value::as_f64)
                    .map(|value| value.floor() as i32)
                    .unwrap_or(i32::MIN),
            );
            let requested_route_tile = GridPos::new(params.from_x, params.from_y);
            if actual_route_tile != requested_route_tile {
                return Ok(serde_json::json!({
                    "success": false,
                    "error_kind": "fuel_source_output_mismatch",
                    "error": "The fuel route must start at the exact output tile of the diagnosed coal drill.",
                    "source_unit_number": source_unit_number,
                    "source_name": diagnosed_source.name,
                    "requested_from": requested_route_tile,
                    "expected_from": actual_route_tile,
                    "guidance": "Refresh diagnose_fuel_sustainability; no source or route entity was changed.",
                }));
            }
        } else {
            return Ok(serde_json::json!({
                "success": false,
                "error_kind": "unsupported_fuel_source_tap",
                "error": "This diagnosed source cannot be safely tapped by the durable fuel controller.",
                "source_unit_number": source_unit_number,
                "source_name": diagnosed_source.name,
                "source_type": diagnosed_source.entity_type,
                "guidance": "Choose a mining drill, surface belt, underground belt, or 1x1 coal container source. No source entity was rotated or removed.",
            }));
        }

        let source_tap_plan = if let Some(source) = source_tap_entity {
            let source_tile = GridPos::from_position(&source.position);
            let mut selected: Option<((u8, u8, u64, usize), FuelSourceTapPlan)> = None;
            for (layout_index, layout) in
                belt_source_tap_layouts(source_tile).into_iter().enumerate()
            {
                let route_params = RouteBeltParams {
                    from_x: layout.drop_tile.x,
                    from_y: layout.drop_tile.y,
                    to_x: params.pickup_x,
                    to_y: params.pickup_y,
                    belt_type: params.belt_type.clone(),
                    search_radius: params.search_radius,
                    dry_run: true,
                    respect_zones: params.respect_zones,
                    allow_underground: params.allow_underground,
                    // A tap creates an independent, coal-only branch. It must
                    // not merge into an unrelated or mixed existing belt.
                    extend_existing: false,
                };
                let route = self
                    .route_belt_core(client, &route_params)
                    .await
                    .unwrap_or_else(|error| {
                        route_belt_failure_json(&route_params, "infrastructure_failure", error)
                    });
                let source_tap_placement = ControllerPlacement {
                    label: "source_tap_inserter",
                    item_name: FUEL_SOURCE_TAP_INSERTER,
                    entity_name: FUEL_SOURCE_TAP_INSERTER,
                    position: layout.inserter_position(),
                    direction: layout.inserter_direction,
                };
                let preflight = controller_preflight(
                    client,
                    &[("fuel", &route)],
                    &params.belt_type,
                    &HashSet::new(),
                    &[source_tap_placement, fuel_inserter_placement],
                )
                .await?;
                let rank = source_tap_plan_rank(&route, &preflight, layout_index);
                let plan = FuelSourceTapPlan {
                    layout,
                    route_params,
                    route,
                    preflight,
                };
                if selected
                    .as_ref()
                    .is_none_or(|(best_rank, _)| rank < *best_rank)
                {
                    selected = Some((rank, plan));
                }
            }
            selected.map(|(_, plan)| plan)
        } else {
            None
        };

        let (route_params, route, mut preflight, source_tap_layout) =
            if let Some(plan) = source_tap_plan {
                (
                    plan.route_params,
                    plan.route,
                    plan.preflight,
                    Some(plan.layout),
                )
            } else {
                let route_params = RouteBeltParams {
                    from_x: params.from_x,
                    from_y: params.from_y,
                    to_x: params.pickup_x,
                    to_y: params.pickup_y,
                    belt_type: params.belt_type.clone(),
                    search_radius: params.search_radius,
                    dry_run: true,
                    respect_zones: params.respect_zones,
                    allow_underground: params.allow_underground,
                    extend_existing: params.extend_existing,
                };
                let route = self
                    .route_belt_core(client, &route_params)
                    .await
                    .map(fuel_route_protects_existing_source)
                    .unwrap_or_else(|error| {
                        route_belt_failure_json(&route_params, "infrastructure_failure", error)
                    });
                let preflight = controller_preflight(
                    client,
                    &[("fuel", &route)],
                    &params.belt_type,
                    &HashSet::new(),
                    &[fuel_inserter_placement],
                )
                .await?;
                (route_params, route, preflight, None)
            };
        let inserter_bootstrap_count = if params.inserter_name.contains("burner") {
            params.inserter_fuel_count
        } else {
            0
        };
        let source_tap_bootstrap_count = if source_tap_layout.is_some() {
            params.inserter_fuel_count
        } else {
            0
        };
        let total_bootstrap_fuel = inserter_bootstrap_count
            .saturating_add(source_tap_bootstrap_count)
            .saturating_add(params.bootstrap_consumer_fuel_count);
        let available_bootstrap_fuel = client
            .character_inventory()
            .await
            .map_err(|error| format!("checking bootstrap fuel: {error}"))?
            .items
            .iter()
            .find(|item| item.name == params.inserter_fuel_item)
            .map(|item| item.count)
            .unwrap_or(0);
        let bootstrap_fuel_ready = available_bootstrap_fuel >= total_bootstrap_fuel;
        if let Some(report) = preflight.as_object_mut() {
            let route_and_placement_ready =
                report.get("ready").and_then(|value| value.as_bool()) == Some(true);
            report.insert(
                "ready".to_string(),
                serde_json::json!(route_and_placement_ready && bootstrap_fuel_ready),
            );
            report.insert(
                "bootstrap_fuel".to_string(),
                serde_json::json!({
                    "item": params.inserter_fuel_item,
                    "available": available_bootstrap_fuel,
                    "required": total_bootstrap_fuel,
                    "inserter_required": inserter_bootstrap_count,
                    "source_tap_required": source_tap_bootstrap_count,
                    "consumer_required": params.bootstrap_consumer_fuel_count,
                    "ready": bootstrap_fuel_ready,
                }),
            );
        }
        let preflight_ready =
            preflight.get("ready").and_then(|value| value.as_bool()) == Some(true);

        if params.dry_run {
            return Ok(serde_json::json!({
                "success": preflight_ready,
                "dry_run": true,
                "consumer": {
                    "unit_number": consumer.unit_number,
                    "name": consumer.name,
                    "position": consumer.position,
                },
                "route": route,
                "preflight": preflight,
                "next_action": route.get("next_action"),
                "source_tap": source_tap_layout.map(|layout| serde_json::json!({
                    "source_unit_number": params.source_unit_number,
                    "source_direction_preserved": true,
                    "layout": layout,
                    "route_start_matches_drop": route_topology_tile(&route, "start_tile")
                        == Some(layout.drop_tile),
                    "branch_extend_existing": false,
                })),
                "steps": [{
                    "tool": "route_belt",
                    "args": route_params,
                }, {
                    "tool": "place_entity",
                    "required": source_tap_layout.is_some(),
                    "args": source_tap_layout.map(|layout| serde_json::json!({
                        "entity_name": FUEL_SOURCE_TAP_INSERTER,
                        "x": layout.inserter_position().x,
                        "y": layout.inserter_position().y,
                        "direction": layout.inserter_direction,
                    })),
                }, {
                    "tool": "configure_inserter",
                    "required": source_tap_layout.is_some(),
                    "args": {
                        "unit_number": serde_json::Value::Null,
                        "allowed_items": ["coal"],
                    },
                }, {
                    "tool": "place_entity",
                    "args": inserter_args,
                }, {
                    "operation": "bootstrap_provisional_consumer",
                    "required": provisional_self_bootstrap,
                    "unit_number": params.provisional_source_unit_number,
                    "item": params.inserter_fuel_item,
                    "count": params.bootstrap_consumer_fuel_count,
                }, {
                    "tool": "verify_production",
                    "args": {
                        "x": consumer.position.x,
                        "y": consumer.position.y,
                        "radius": params.verify_radius,
                    },
                }],
                "guidance": "Execute only when preflight.ready is true; the independent route, filtered source tap, and terminal inserter are reserved as one transaction.",
            }));
        }

        if !preflight_ready {
            return Ok(serde_json::json!({
                "success": false,
                "error_kind": "compound_preflight_failed",
                "error": "Fuel route, shared materials, or inserter placement failed preflight. Nothing was placed.",
                "dry_run": false,
                "consumer": {
                    "unit_number": consumer.unit_number,
                    "name": consumer.name,
                    "position": consumer.position,
                },
                "route": route,
                "preflight": preflight,
                "next_action": route.get("next_action"),
                "source_tap": source_tap_layout.map(|layout| serde_json::json!({
                    "source_unit_number": params.source_unit_number,
                    "layout": layout,
                    "branch_extend_existing": false,
                })),
                "guidance": "Resolve every preflight error before retrying; no source entity or partial fuel infrastructure was changed.",
            }));
        }

        let consumer_snapshot = match client
            .snapshot_burner_state(params.consumer_unit_number)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return Ok(serde_json::json!({
                    "success": false,
                    "error_kind": "consumer_snapshot_failed",
                    "error": error.to_string(),
                    "consumer_unit_number": params.consumer_unit_number,
                    "guidance": "Refresh fuel diagnosis before retrying; no infrastructure was changed.",
                }));
            }
        };
        if params.bootstrap_consumer_fuel_count > 0
            && consumer_snapshot
                .get("cold")
                .and_then(|value| value.as_bool())
                != Some(true)
        {
            return Ok(serde_json::json!({
                "success": false,
                "error_kind": "consumer_state_changed_before_bootstrap",
                "error": "The provisional burner consumer is no longer cold, so the stale bootstrap transaction was not started.",
                "consumer_unit_number": params.consumer_unit_number,
                "consumer_snapshot": consumer_snapshot,
                "guidance": "Rerun repair_fuel_sustainability dry_run to use current burner state.",
            }));
        }

        let mut route_execute = route_params.clone();
        route_execute.dry_run = false;
        let route = match self.route_belt_core(client, &route_execute).await {
            Ok(report) if report_success(&report) => report,
            Ok(report) => {
                return Ok(serde_json::json!({
                    "success": false,
                    "error_kind": "route_execution_failed",
                    "route": report,
                    "rollback": {"success": true, "units": {"success": true, "removed_units": [], "errors": []}},
                }));
            }
            Err(error) => return Err(format!("Error: executing fuel route: {error}")),
        };
        let mut transaction_units = route_report_placed_units(&route);
        let terminal_placement_report = match client
            .place_filtered_inserter(
                &params.inserter_name,
                inserter_position,
                inserter_direction,
                &["coal".to_string()],
            )
            .await
        {
            Ok(report) => report,
            Err(error) => {
                let rollback = rollback_failed_atomic_fuel_placement(
                    client,
                    &consumer_snapshot,
                    None,
                    &transaction_units,
                    None,
                )
                .await;
                return Ok(serde_json::json!({
                    "success": false,
                    "error_kind": "atomic_fuel_inserter_placement_failed",
                    "error": error.to_string(),
                    "route": route,
                    "rollback": rollback,
                }));
            }
        };
        let (inserter, terminal_filter) =
            match verified_atomic_filtered_placement(&terminal_placement_report) {
                Ok(placement) => placement,
                Err(error) => {
                    let remote_success = report_success(&terminal_placement_report);
                    let rollback = rollback_failed_atomic_fuel_placement(
                        client,
                        &consumer_snapshot,
                        None,
                        &transaction_units,
                        Some(&terminal_placement_report),
                    )
                    .await;
                    return Ok(serde_json::json!({
                        "success": false,
                        "error_kind": if remote_success {
                            "fuel_inserter_filter_verification_failed"
                        } else {
                            "atomic_fuel_inserter_placement_failed"
                        },
                        "error": error,
                        "atomic_placement": terminal_placement_report,
                        "route": route,
                        "rollback": rollback,
                    }));
                }
            };
        let placed_inserter_unit = inserter.unit_number;
        if let Some(unit_number) = placed_inserter_unit {
            transaction_units.push(unit_number);
        }
        if placed_inserter_unit.is_none() {
            let rollback = rollback_failed_atomic_fuel_placement(
                client,
                &consumer_snapshot,
                None,
                &transaction_units,
                Some(&terminal_placement_report),
            )
            .await;
            return Ok(serde_json::json!({
                "success": false,
                "error_kind": "fuel_inserter_missing_identity",
                "error": "The atomically placed terminal inserter had no exact unit identity.",
                "atomic_placement": terminal_placement_report,
                "route": route,
                "rollback": rollback,
            }));
        }

        let source_tap_placement = if let Some(layout) = source_tap_layout {
            let source_tap_placement_report = match client
                .place_filtered_inserter(
                    FUEL_SOURCE_TAP_INSERTER,
                    layout.inserter_position(),
                    layout.inserter_direction,
                    &["coal".to_string()],
                )
                .await
            {
                Ok(report) => report,
                Err(error) => {
                    let rollback = rollback_failed_atomic_fuel_placement(
                        client,
                        &consumer_snapshot,
                        placed_inserter_unit,
                        &transaction_units,
                        None,
                    )
                    .await;
                    return Ok(serde_json::json!({
                        "success": false,
                        "error_kind": "atomic_source_tap_placement_failed",
                        "error": error.to_string(),
                        "source_unit_number": params.source_unit_number,
                        "layout": layout,
                        "route": route,
                        "rollback": rollback,
                    }));
                }
            };
            match verified_atomic_filtered_placement(&source_tap_placement_report) {
                Ok((entity, filter)) => Some((entity, filter, source_tap_placement_report)),
                Err(error) => {
                    let remote_success = report_success(&source_tap_placement_report);
                    let rollback = rollback_failed_atomic_fuel_placement(
                        client,
                        &consumer_snapshot,
                        placed_inserter_unit,
                        &transaction_units,
                        Some(&source_tap_placement_report),
                    )
                    .await;
                    return Ok(serde_json::json!({
                        "success": false,
                        "error_kind": if remote_success {
                            "source_tap_filter_verification_failed"
                        } else {
                            "atomic_source_tap_placement_failed"
                        },
                        "error": error,
                        "atomic_placement": source_tap_placement_report,
                        "source_unit_number": params.source_unit_number,
                        "layout": layout,
                        "route": route,
                        "rollback": rollback,
                    }));
                }
            }
        } else {
            None
        };
        let source_tap_unit = source_tap_placement
            .as_ref()
            .and_then(|(entity, _filter, _report)| entity.unit_number);
        if source_tap_placement.is_some() && source_tap_unit.is_none() {
            let source_tap_placement_report = source_tap_placement
                .as_ref()
                .map(|(_entity, _filter, report)| report)
                .expect("source tap placement checked as present");
            let rollback = rollback_failed_atomic_fuel_placement(
                client,
                &consumer_snapshot,
                placed_inserter_unit,
                &transaction_units,
                Some(source_tap_placement_report),
            )
            .await;
            return Ok(serde_json::json!({
                "success": false,
                "error_kind": "source_tap_missing_identity",
                "error": "The placed source tap had no exact unit identity and cannot enter an atomic transaction.",
                "atomic_placement": source_tap_placement_report,
                "route": route,
                "rollback": rollback,
            }));
        }
        if let Some(unit_number) = source_tap_unit {
            transaction_units.push(unit_number);
        }
        let source_tap_filter = source_tap_placement.map(|(_entity, filter, _report)| filter);
        let bootstrap_fuel = if params.inserter_name.contains("burner") {
            match placed_inserter_unit {
                Some(unit) => {
                    let inserted = client
                        .insert_items(
                            unit,
                            &params.inserter_fuel_item,
                            params.inserter_fuel_count,
                            "fuel",
                        )
                        .await;
                    match inserted {
                        Ok(report) => {
                            let inserter_bootstrap_inserted = report
                                .get("inserted")
                                .and_then(|value| value.as_u64())
                                .and_then(|value| u32::try_from(value).ok())
                                .unwrap_or(0);
                            if inserter_bootstrap_inserted != params.inserter_fuel_count {
                                let rollback = rollback_failed_fuel_transaction(
                                    client,
                                    &consumer_snapshot,
                                    placed_inserter_unit,
                                    &transaction_units,
                                )
                                .await;
                                return Ok(serde_json::json!({
                                    "success": false,
                                    "error_kind": "bootstrap_fuel_incomplete",
                                    "error": "The burner inserter did not accept its complete bounded startup buffer.",
                                    "bootstrap_fuel": report,
                                    "route": route,
                                    "rollback": rollback,
                                }));
                            }
                            serde_json::json!({
                                "operation": "bootstrap_inserter_fuel",
                                "unit_number": unit,
                                "item": params.inserter_fuel_item,
                                "count": params.inserter_fuel_count,
                                "inventory_type": "fuel",
                                "temporary_startup_buffer": true,
                                "success": true,
                                "report": report,
                            })
                        }
                        Err(error) => {
                            let rollback = rollback_failed_fuel_transaction(
                                client,
                                &consumer_snapshot,
                                placed_inserter_unit,
                                &transaction_units,
                            )
                            .await;
                            return Ok(serde_json::json!({
                                "success": false,
                                "error_kind": "bootstrap_fuel_failed",
                                "error": error.to_string(),
                                "route": route,
                                "rollback": rollback,
                            }));
                        }
                    }
                }
                None => serde_json::json!({
                    "skipped": true,
                    "reason": "burner inserter placement failed",
                }),
            }
        } else {
            serde_json::json!({
                "skipped": true,
                "reason": "inserter is not burner-powered",
            })
        };
        let bootstrap_source_tap_fuel = match source_tap_unit {
            Some(unit_number) => match client
                .insert_items(
                    unit_number,
                    &params.inserter_fuel_item,
                    params.inserter_fuel_count,
                    "fuel",
                )
                .await
            {
                Ok(report)
                    if report.get("inserted").and_then(serde_json::Value::as_u64)
                        == Some(u64::from(params.inserter_fuel_count)) =>
                {
                    serde_json::json!({
                        "operation": "bootstrap_source_tap_fuel",
                        "unit_number": unit_number,
                        "item": params.inserter_fuel_item,
                        "count": params.inserter_fuel_count,
                        "inventory_type": "fuel",
                        "temporary_startup_buffer": true,
                        "success": true,
                        "report": report,
                    })
                }
                Ok(report) => {
                    let rollback = rollback_failed_fuel_transaction(
                        client,
                        &consumer_snapshot,
                        placed_inserter_unit,
                        &transaction_units,
                    )
                    .await;
                    return Ok(serde_json::json!({
                        "success": false,
                        "error_kind": "source_tap_bootstrap_incomplete",
                        "error": "The source tap did not accept its complete bounded startup buffer.",
                        "bootstrap_source_tap_fuel": report,
                        "route": route,
                        "rollback": rollback,
                    }));
                }
                Err(error) => {
                    let rollback = rollback_failed_fuel_transaction(
                        client,
                        &consumer_snapshot,
                        placed_inserter_unit,
                        &transaction_units,
                    )
                    .await;
                    return Ok(serde_json::json!({
                        "success": false,
                        "error_kind": "source_tap_bootstrap_failed",
                        "error": error.to_string(),
                        "route": route,
                        "rollback": rollback,
                    }));
                }
            },
            None => serde_json::json!({
                "skipped": true,
                "reason": "the selected source does not require a source-side tap",
            }),
        };
        let inserter_report = serde_json::json!({
            "tool": "place_entity",
            "args": inserter_args,
            "success": true,
            "unit_number": placed_inserter_unit,
            "atomic_filter_configuration": true,
            "filter": terminal_filter,
            "error": serde_json::Value::Null,
        });

        let bootstrap_consumer_fuel = if provisional_self_bootstrap
            && params.bootstrap_consumer_fuel_count > 0
        {
            match client
                .insert_items(
                    params.consumer_unit_number,
                    &params.inserter_fuel_item,
                    params.bootstrap_consumer_fuel_count,
                    "fuel",
                )
                .await
            {
                Ok(report) => {
                    let consumer_bootstrap_inserted = report
                        .get("inserted")
                        .and_then(|value| value.as_u64())
                        .and_then(|value| u32::try_from(value).ok())
                        .unwrap_or(0);
                    if consumer_bootstrap_inserted != params.bootstrap_consumer_fuel_count {
                        let rollback = rollback_failed_fuel_transaction(
                            client,
                            &consumer_snapshot,
                            placed_inserter_unit,
                            &transaction_units,
                        )
                        .await;
                        return Ok(serde_json::json!({
                            "success": false,
                            "error_kind": "consumer_bootstrap_fuel_incomplete",
                            "error": "The exact provisional coal drill did not accept its complete bounded startup buffer.",
                            "bootstrap_consumer_fuel": report,
                            "route": route,
                            "rollback": rollback,
                        }));
                    }
                    serde_json::json!({
                        "operation": "bootstrap_provisional_consumer",
                        "unit_number": params.consumer_unit_number,
                        "item": params.inserter_fuel_item,
                        "count": params.bootstrap_consumer_fuel_count,
                        "inventory_type": "fuel",
                        "temporary_startup_buffer": true,
                        "success": true,
                        "report": report,
                    })
                }
                Err(error) => {
                    let rollback = rollback_failed_fuel_transaction(
                        client,
                        &consumer_snapshot,
                        placed_inserter_unit,
                        &transaction_units,
                    )
                    .await;
                    return Ok(serde_json::json!({
                        "success": false,
                        "error_kind": "consumer_bootstrap_fuel_failed",
                        "error": error.to_string(),
                        "route": route,
                        "rollback": rollback,
                    }));
                }
            }
        } else if provisional_self_bootstrap {
            serde_json::json!({
                "skipped": true,
                "reason": "the provisional coal drill already has startup fuel",
                "unit_number": params.consumer_unit_number,
                "count": 0,
            })
        } else {
            serde_json::json!({
                "skipped": true,
                "reason": "fuel source is already durable; no consumer bootstrap needed",
            })
        };

        let verify_radius = params.verify_radius.clamp(1, 50) as f64;
        let verify_area = Area::new(
            consumer.position.x - verify_radius,
            consumer.position.y - verify_radius,
            consumer.position.x + verify_radius,
            consumer.position.y + verify_radius,
        );
        // A connected route is only infrastructure. Poll the exact terminal
        // connection until coal reaches its pickup belt, with enough time for
        // one mining cycle plus basic-belt transit across the complete route.
        let (route_transit_tiles, mut delivery_wait_budget) = fuel_delivery_wait_budget(&route);
        let mut delivery_waited_ticks = 0_u32;
        let mut certified_upstream_hops = 0_u32;
        let (fuel_diagnosis, durable_fuel_topology) = loop {
            let diagnosis = client
                .diagnose_fuel_sustainability(verify_area, 100)
                .await
                .unwrap_or_else(|error| serde_json::json!({"error": error.to_string()}));
            let topology = fuel_topology_verification(
                &diagnosis,
                params.consumer_unit_number,
                placed_inserter_unit,
            );
            certified_upstream_hops =
                certified_upstream_hops.max(fuel_topology_upstream_hops(&topology));
            // The recursive proof starts at the terminal belt, so its hop
            // count already includes the new surface route plus any existing
            // upstream trunk. Keep the larger of that full proof distance and
            // the route's physical span (important for underground segments).
            delivery_wait_budget = delivery_wait_budget.max(fuel_delivery_budget_ticks(
                route_transit_tiles.max(certified_upstream_hops),
            ));
            let terminal_coal_observed = topology
                .get("live_supply_verified")
                .and_then(serde_json::Value::as_bool)
                == Some(true);
            let exact_feeder_transfer_observed = exact_fuel_feeder_transfer_observed(&topology);
            let delivery_observed = terminal_coal_observed || exact_feeder_transfer_observed;
            if delivery_observed || delivery_waited_ticks >= delivery_wait_budget {
                break (diagnosis, topology);
            }

            // Inserter swings can begin and finish between one-second samples.
            // Quarter-second polling observes the exact filtered feeder without
            // treating unrelated consumer inventory changes as route delivery.
            let step = 15.min(delivery_wait_budget - delivery_waited_ticks);
            if let Err(error) = client.wait_ticks(step).await {
                let rollback = rollback_failed_fuel_transaction(
                    client,
                    &consumer_snapshot,
                    placed_inserter_unit,
                    &transaction_units,
                )
                .await;
                let error_kind = if provisional_self_bootstrap {
                    "self_bootstrap_observation_failed"
                } else if source_tap_layout.is_some() {
                    "source_tap_startup_observation_failed"
                } else {
                    "fuel_delivery_observation_failed"
                };
                return Ok(serde_json::json!({
                    "success": false,
                    "error_kind": error_kind,
                    "error": error.to_string(),
                    "route": route,
                    "bootstrap_fuel": bootstrap_fuel,
                    "bootstrap_source_tap_fuel": bootstrap_source_tap_fuel,
                    "bootstrap_consumer_fuel": bootstrap_consumer_fuel,
                    "delivery_observation": {
                        "route_transit_tiles": route_transit_tiles,
                        "certified_upstream_hops": certified_upstream_hops,
                        "waited_ticks": delivery_waited_ticks,
                        "budget_ticks": delivery_wait_budget,
                    },
                    "rollback": rollback,
                }));
            }
            delivery_waited_ticks = delivery_waited_ticks.saturating_add(step);
        };
        let terminal_coal_observed = durable_fuel_topology
            .get("live_supply_verified")
            .and_then(serde_json::Value::as_bool)
            == Some(true);
        let delivery_path_operational_during_observation =
            fuel_delivery_path_operational(&durable_fuel_topology);
        let exact_feeder_transfer_observed =
            exact_fuel_feeder_transfer_observed(&durable_fuel_topology);
        let fuel_delivery_observed = terminal_coal_observed || exact_feeder_transfer_observed;
        let delivery_observation = serde_json::json!({
            "success": fuel_delivery_observed,
            "scope": "exact_terminal_or_filtered_feeder_transfer",
            "terminal_coal_observed": terminal_coal_observed,
            "exact_feeder_transfer_observed": exact_feeder_transfer_observed,
            "delivery_path_operational": delivery_path_operational_during_observation,
            "route_transit_tiles": route_transit_tiles,
            "certified_upstream_hops": certified_upstream_hops,
            "waited_ticks": delivery_waited_ticks,
            "budget_ticks": delivery_wait_budget,
        });
        let persisted_inserter = match placed_inserter_unit {
            Some(unit_number) => client.get_entity(unit_number).await.ok(),
            None => None,
        };
        let persisted_source_tap = match source_tap_unit {
            Some(unit_number) => client.get_entity(unit_number).await.ok(),
            None => None,
        };
        let mut infrastructure_verified = incremental_infrastructure_verification(
            &route,
            placed_inserter_unit,
            persisted_inserter.as_ref(),
            &params.inserter_name,
            inserter_position,
            inserter_direction,
        );
        let structural_fuel_topology = durable_fuel_topology
            .get("structural_success")
            .and_then(|value| value.as_bool())
            == Some(true);
        let source_after_build = client.get_entity(source_unit_number).await.ok();
        let source_preservation = source_tap_entity.map(|source| {
            source_entity_preservation(
                source,
                source_after_build.as_ref(),
                GridPos::new(params.from_x, params.from_y),
            )
        });
        let mut source_tap_infrastructure = match (source_tap_layout, source_tap_unit) {
            (Some(layout), Some(unit_number)) => {
                let placement = incremental_infrastructure_verification(
                    &route,
                    Some(unit_number),
                    persisted_source_tap.as_ref(),
                    FUEL_SOURCE_TAP_INSERTER,
                    layout.inserter_position(),
                    layout.inserter_direction,
                );
                let tap_radius = 6.0;
                let tap_area = Area::new(
                    layout.inserter_position().x - tap_radius,
                    layout.inserter_position().y - tap_radius,
                    layout.inserter_position().x + tap_radius,
                    layout.inserter_position().y + tap_radius,
                );
                let tap_diagnosis = client
                    .diagnose_fuel_sustainability(tap_area, 100)
                    .await
                    .unwrap_or_else(|error| serde_json::json!({"error": error.to_string()}));
                let topology =
                    fuel_topology_verification(&tap_diagnosis, unit_number, Some(unit_number));
                let route_start_matches_drop =
                    route_topology_tile(&route, "start_tile") == Some(layout.drop_tile);
                let source_preserved = source_preservation.as_ref().is_some_and(report_success);
                let filter_verified = source_tap_filter
                    .as_ref()
                    .is_some_and(source_tap_filter_verified);
                let success = report_success(&placement)
                    && topology
                        .get("structural_success")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true)
                    && route_start_matches_drop
                    && source_preserved
                    && filter_verified;
                Some(serde_json::json!({
                    "success": success,
                    "source_unit_number": params.source_unit_number,
                    "source_direction_preserved": source_preserved,
                    "unit_number": unit_number,
                    "layout": layout,
                    "route_start_matches_drop": route_start_matches_drop,
                    "branch_extend_existing": false,
                    "filter_readback_verified": filter_verified,
                    "filter_atomic_with_placement": filter_verified,
                    "allowed_items": ["coal"],
                    "placement": placement,
                    "topology": topology,
                    "source_preservation": source_preservation,
                }))
            }
            (None, None) => None,
            _ => Some(serde_json::json!({
                "success": false,
                "error": "source tap layout and exact unit identity did not agree",
            })),
        };
        let source_tap_structural_success = source_tap_infrastructure
            .as_ref()
            .is_none_or(report_success);
        let infrastructure_success = report_success(&infrastructure_verified)
            && structural_fuel_topology
            && fuel_delivery_observed
            && source_tap_structural_success;
        if let Some(report) = infrastructure_verified.as_object_mut() {
            report.insert(
                "success".to_string(),
                serde_json::json!(infrastructure_success),
            );
            report.insert(
                "durable_fuel_topology".to_string(),
                durable_fuel_topology.clone(),
            );
            report.insert(
                "delivery_observation".to_string(),
                delivery_observation.clone(),
            );
            if let Some(source_tap) = source_tap_infrastructure.as_ref() {
                report.insert("source_tap".to_string(), source_tap.clone());
            }
        }
        if !infrastructure_success {
            let rollback = rollback_failed_fuel_transaction(
                client,
                &consumer_snapshot,
                placed_inserter_unit,
                &transaction_units,
            )
            .await;
            return Ok(serde_json::json!({
                "success": false,
                "error_kind": if !fuel_delivery_observed {
                    "fuel_delivery_not_observed"
                } else {
                    "infrastructure_verification_failed"
                },
                "error": if !fuel_delivery_observed {
                    "Coal delivery was not observed at the exact terminal pickup belt or in the newly placed filtered feeder within the route-length-aware observation window; the route and inserter were rolled back."
                } else {
                    "Fuel controller could not verify its complete route, exact inserter placement, and target fuel connection; the route and inserter were rolled back."
                },
                "dry_run": false,
                "consumer": consumer,
                "route": route,
                "infrastructure_verified": infrastructure_verified,
                "delivery_observation": delivery_observation,
                "source_tap": source_tap_infrastructure,
                "source_preservation": source_preservation,
                "bootstrap_fuel": bootstrap_fuel,
                "bootstrap_source_tap_fuel": bootstrap_source_tap_fuel,
                "bootstrap_consumer_fuel": bootstrap_consumer_fuel,
                "fuel_diagnosis": compact_fuel_diagnosis(&fuel_diagnosis),
                "rollback": rollback,
            }));
        }

        let verification = match observe_production(client, verify_area, 180).await {
            Ok(verification) => verification,
            Err(error) => serde_json::json!({"success": false, "error": error.to_string()}),
        };
        let production_verified =
            fuel_consumer_activity_verification_summary(&verification, consumer.unit_number);
        let production_success = report_success(&production_verified);
        let final_fuel_diagnosis = client
            .diagnose_fuel_sustainability(verify_area, 100)
            .await
            .unwrap_or_else(|error| serde_json::json!({"error": error.to_string()}));
        let final_fuel_topology = fuel_topology_verification(
            &final_fuel_diagnosis,
            params.consumer_unit_number,
            placed_inserter_unit,
        );
        let final_structural_success = final_fuel_topology
            .get("structural_success")
            .and_then(|value| value.as_bool())
            == Some(true);
        // Delivery was observed over time above. A later between-items sample
        // may be empty, but the exact producer and feeder must still be able to
        // operate when the transaction commits.
        let final_delivery_path_operational = fuel_delivery_path_operational(&final_fuel_topology);
        let fuel_supply_live = fuel_delivery_observed && final_delivery_path_operational;
        let final_source_after = client.get_entity(source_unit_number).await.ok();
        let final_source_preservation = source_tap_entity.map(|source| {
            source_entity_preservation(
                source,
                final_source_after.as_ref(),
                GridPos::new(params.from_x, params.from_y),
            )
        });
        let final_source_tap_topology = match (source_tap_layout, source_tap_unit) {
            (Some(layout), Some(unit_number)) => {
                let tap_radius = 6.0;
                let tap_position = layout.inserter_position();
                let tap_area = Area::new(
                    tap_position.x - tap_radius,
                    tap_position.y - tap_radius,
                    tap_position.x + tap_radius,
                    tap_position.y + tap_radius,
                );
                let diagnosis = client
                    .diagnose_fuel_sustainability(tap_area, 100)
                    .await
                    .unwrap_or_else(|error| serde_json::json!({"error": error.to_string()}));
                Some(fuel_topology_verification(
                    &diagnosis,
                    unit_number,
                    Some(unit_number),
                ))
            }
            _ => None,
        };
        let source_tap_final_success = if source_tap_layout.is_some() {
            final_source_preservation
                .as_ref()
                .is_some_and(report_success)
                && final_source_tap_topology.as_ref().is_some_and(|topology| {
                    topology
                        .get("structural_success")
                        .and_then(serde_json::Value::as_bool)
                        == Some(true)
                        && fuel_delivery_path_operational(topology)
                })
                && fuel_delivery_observed
        } else {
            true
        };
        let automation_success = infrastructure_success
            && final_structural_success
            && production_success
            && fuel_supply_live
            && source_tap_final_success;
        if let Some(source_tap) = source_tap_infrastructure
            .as_mut()
            .and_then(serde_json::Value::as_object_mut)
        {
            source_tap.insert(
                "success".to_string(),
                serde_json::json!(source_tap_final_success),
            );
            source_tap.insert(
                "self_fueling_live".to_string(),
                serde_json::json!(source_tap_final_success),
            );
            source_tap.insert(
                "final_topology".to_string(),
                final_source_tap_topology
                    .clone()
                    .unwrap_or(serde_json::Value::Null),
            );
            source_tap.insert(
                "source_preservation".to_string(),
                final_source_preservation
                    .clone()
                    .unwrap_or(serde_json::Value::Null),
            );
        }
        if let Some(report) = infrastructure_verified.as_object_mut() {
            report.insert(
                "success".to_string(),
                serde_json::json!(
                    infrastructure_success && final_structural_success && source_tap_final_success
                ),
            );
            report.insert(
                "durable_fuel_topology".to_string(),
                final_fuel_topology.clone(),
            );
            if let Some(source_tap) = source_tap_infrastructure.as_ref() {
                report.insert("source_tap".to_string(), source_tap.clone());
            }
        }
        let repair_hint = automation_repair_hint(
            "repair_fuel_sustainability",
            "durable fuel delivery",
            verification.get("error").is_none(),
            &verification,
            &[],
            Some(true),
        );
        if (provisional_self_bootstrap || source_tap_layout.is_some()) && !automation_success {
            let rollback = rollback_failed_fuel_transaction(
                client,
                &consumer_snapshot,
                placed_inserter_unit,
                &transaction_units,
            )
            .await;
            return Ok(serde_json::json!({
                "success": false,
                "error_kind": if !final_structural_success {
                    "fuel_topology_lost"
                } else if !fuel_supply_live {
                    "fuel_supply_not_live"
                } else if !source_tap_final_success {
                    "source_tap_not_live"
                } else {
                    "target_production_not_verified"
                },
                "error": if provisional_self_bootstrap {
                    "The provisional coal-drill loop did not prove live self-sustaining production, so its startup fuel and new infrastructure were rolled back."
                } else {
                    "The source-tap transaction did not prove preserved source flow, live durable fuel delivery, and target production, so the complete new branch was rolled back."
                },
                "placement_success": true,
                "dry_run": false,
                "consumer": {
                    "unit_number": consumer.unit_number,
                    "name": consumer.name,
                    "position": consumer.position,
                },
                "route": route,
                "bootstrap_fuel": bootstrap_fuel,
                "bootstrap_source_tap_fuel": bootstrap_source_tap_fuel,
                "bootstrap_consumer_fuel": bootstrap_consumer_fuel,
                "source_tap": source_tap_infrastructure,
                "source_preservation": final_source_preservation,
                "infrastructure_verified": infrastructure_verified,
                "production_verified": production_verified,
                "automation_verified": {
                    "success": false,
                    "infrastructure_success": infrastructure_success && final_structural_success,
                    "production_success": production_success,
                    "fuel_supply_live": fuel_supply_live,
                    "source_tap_success": source_tap_final_success,
                },
                "fuel_diagnosis": compact_fuel_diagnosis(&final_fuel_diagnosis),
                "rollback": rollback,
                "repair_hint": repair_hint,
            }));
        }
        let error_kind = if automation_success {
            serde_json::Value::Null
        } else if !final_structural_success {
            serde_json::json!("fuel_topology_lost")
        } else if !fuel_supply_live {
            serde_json::json!("fuel_supply_not_live")
        } else if !source_tap_final_success {
            serde_json::json!("source_tap_not_live")
        } else {
            serde_json::json!("target_production_not_verified")
        };
        Ok(serde_json::json!({
            "success": automation_success,
            "error_kind": error_kind,
            "placement_success": true,
            "dry_run": false,
            "consumer": {
                "unit_number": consumer.unit_number,
                "name": consumer.name,
                "position": consumer.position,
            },
            "route": route,
            "preflight": preflight,
            "inserter": inserter_report,
            "bootstrap_fuel": bootstrap_fuel,
            "bootstrap_source_tap_fuel": bootstrap_source_tap_fuel,
            "bootstrap_consumer_fuel": bootstrap_consumer_fuel,
            "source_tap": source_tap_infrastructure,
            "source_preservation": final_source_preservation,
            "infrastructure_verified": infrastructure_verified,
            "production_verified": production_verified,
            "automation_verified": {
                "success": automation_success,
                "infrastructure_success": infrastructure_success && final_structural_success,
                "production_success": production_success,
                "fuel_supply_live": fuel_supply_live,
                "source_tap_success": source_tap_final_success,
            },
            "verification": verification,
            "repair_hint": repair_hint,
            "guidance": if automation_success {
                "Fuel delivery infrastructure and target production are both verified."
            } else {
                "Structurally valid fuel infrastructure was retained, but live fuel delivery and target production were not both verified. Inspect automation_verified and repair_hint without claiming success."
            },
        }))
    }

    /// Build a coal fuel belt plus inserter feed for one burnable consumer.
    #[tool(
        description = "Execute a durable coal fuel-supply plan for one burner/furnace/boiler. Use args from diagnose_fuel_sustainability: route coal to the pickup tile, place the inserter feeding the consumer, then verify production. Prefer this over insert_items/hand_feed_furnace once coal supply exists. Use dry_run=true during planner turns."
    )]
    async fn build_fuel_supply(
        &self,
        Parameters(params): Parameters<BuildFuelSupplyParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let result = match self.build_fuel_supply_core(&mut client, &params).await {
            Ok(result) => model_safe_payload(compact_fuel_repair(&result)),
            Err(e) => {
                return self
                    .with_player_messages(semantic_failure("fuel_supply_failed", e))
                    .await;
            }
        };
        let msg = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(msg).await
    }

    /// Diagnose and repair the highest-priority missing durable fuel supply.
    #[tool(
        description = "Build and verify one durable coal feed. A cold burner coal drill may bootstrap its own return loop, but success still requires strict closed-cycle proof. Existing coal belts are tapped without rotation. Use dry_run=true to preview."
    )]
    async fn repair_fuel_sustainability(
        &self,
        Parameters(params): Parameters<RepairFuelSustainabilityParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let radius = params.radius.unwrap_or(64);
        let limit = params.limit.unwrap_or(20).clamp(1, 100);
        let position = match (params.x, params.y) {
            (Some(x), Some(y)) => Position::new(x, y),
            (None, None) => {
                let status = match client.character_status().await {
                    Ok(status) => status,
                    Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
                };
                match status.position {
                    Some(position) => position,
                    None => match client.get_character_position().await {
                        Ok(position) => position,
                        Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
                    },
                }
            }
            _ => {
                return self
                    .with_player_messages("Error: x and y must be provided together".to_string())
                    .await;
            }
        };
        let r = radius as f64;
        let area = Area {
            left_top: Position::new(position.x - r, position.y - r),
            right_bottom: Position::new(position.x + r, position.y + r),
        };
        let diagnosis = match client.diagnose_fuel_sustainability(area, limit).await {
            Ok(report) => report,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let model_diagnosis = compact_fuel_diagnosis(&diagnosis);
        let mut selected_args = match ready_fuel_supply_args(&diagnosis) {
            Some(args) => args,
            None => {
                let next_action = model_diagnosis
                    .get("suggested_actions")
                    .and_then(|value| value.as_array())
                    .and_then(|actions| actions.first())
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({
                        "type": "inspect_ranked_consumer",
                        "description": "No executable fuel transaction is available in this area. Inspect the primary consumer and establish a coal drill or clear one adjacent inserter position."
                    }));
                let result = serde_json::json!({
                    "success": false,
                    "error_kind": "no_ready_fuel_transaction",
                    "dry_run": params.dry_run,
                    "selected": null,
                    "diagnosis": model_diagnosis,
                    "next_action": next_action,
                    "guidance": "No executable durable fuel transaction was found. Follow next_action for the ranked consumer, then rerun repair_fuel_sustainability.",
                });
                let msg = serde_json::to_string_pretty(&result)
                    .unwrap_or_else(|e| format!("Error: {}", e));
                return self.with_player_messages(msg).await;
            }
        };
        selected_args.dry_run = params.dry_run;
        selected_args.search_radius = params.search_radius;
        selected_args.respect_zones = params.respect_zones;
        selected_args.allow_underground = params.allow_underground;
        selected_args.extend_existing = params.extend_existing;
        selected_args.verify_radius = params.verify_radius;

        let repair = match self
            .build_fuel_supply_core(&mut client, &selected_args)
            .await
        {
            Ok(result) => result,
            Err(e) => {
                return self
                    .with_player_messages(semantic_failure("fuel_supply_failed", e))
                    .await;
            }
        };
        let success = repair
            .get("success")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let model_repair = compact_fuel_repair(&repair);
        let result = model_safe_payload(serde_json::json!({
            "success": success,
            "dry_run": params.dry_run,
            "selected_transaction": selected_args,
            "diagnosis": model_diagnosis,
            "repair": model_repair,
            "guidance": if success {
                "Durable fuel repair succeeded. Rerun repair_fuel_sustainability with dry_run=true before manually touching fuel again."
            } else {
                "Durable fuel repair did not verify success. Inspect repair, repair_hint, or route diagnostics; do not fall back to repeated manual fuel insertion."
            },
        }));
        let msg = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(msg).await
    }

    /// Build a science belt plus inserter feed for one lab.
    #[tool(
        description = "Build a durable science-pack belt and inserter into one lab as a single transaction, then report both infrastructure and live research state. Use dry_run=true to preview the complete route and placement."
    )]
    async fn build_lab_feed(&self, Parameters(params): Parameters<BuildLabFeedParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let lab = match client.get_entity(params.lab_unit_number).await {
            Ok(entity) => entity,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        if lab.name != "lab" {
            return self
                .with_player_messages(format!(
                    "Error: unit {} is {}, not lab",
                    params.lab_unit_number, lab.name
                ))
                .await;
        }

        let inserter_direction = match Direction::parse(&params.inserter_direction) {
            Some(direction) => direction,
            None => {
                return self
                    .with_player_messages(invalid_direction_failure(
                        "inserter_direction",
                        &params.inserter_direction,
                    ))
                    .await;
            }
        };

        let route_params = RouteBeltParams {
            from_x: params.from_x,
            from_y: params.from_y,
            to_x: params.pickup_x,
            to_y: params.pickup_y,
            belt_type: params.belt_type.clone(),
            search_radius: params.search_radius,
            dry_run: true,
            respect_zones: params.respect_zones,
            allow_underground: params.allow_underground,
            extend_existing: params.extend_existing,
        };

        let route = match self.route_belt_core(&mut client, &route_params).await {
            Ok(report) => report,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let inserter_position = Position::new(params.inserter_x, params.inserter_y);
        let inserter_args = serde_json::json!({
            "entity_name": "inserter",
            "x": params.inserter_x,
            "y": params.inserter_y,
            "direction": params.inserter_direction,
        });
        let preflight = match controller_preflight(
            &mut client,
            &[("lab_feed", &route)],
            &params.belt_type,
            &HashSet::new(),
            &[ControllerPlacement {
                label: "lab_inserter",
                item_name: "inserter",
                entity_name: "inserter",
                position: inserter_position,
                direction: inserter_direction,
            }],
        )
        .await
        {
            Ok(preflight) => preflight,
            Err(error) => return self.with_player_messages(format!("Error: {error}")).await,
        };
        let planned_endpoint_topology = inserter_machine_endpoint_verification(
            &route,
            Some(inserter_position),
            Some(inserter_direction),
            &lab,
            InserterMachineFlow::Input,
            "planned",
        );
        let preflight = attach_endpoint_preflight(preflight, planned_endpoint_topology);
        let preflight_ready = preflight["ready"].as_bool() == Some(true);

        if params.dry_run {
            let result = serde_json::json!({
                "success": preflight_ready,
                "dry_run": true,
                "lab": {
                    "unit_number": lab.unit_number,
                    "name": lab.name,
                    "position": lab.position,
                },
                "route": route,
                "preflight": preflight,
                "steps": [{
                    "tool": "route_belt",
                    "args": route_params,
                }, {
                    "tool": "place_entity",
                    "args": inserter_args,
                }, {
                    "tool": "get_research_status",
                    "args": {},
                }],
                "guidance": "Execute only when preflight.ready is true; the complete route and lab inserter are reserved together.",
            });
            let msg =
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
            return self.with_player_messages(msg).await;
        }

        if !preflight_ready {
            let result = serde_json::json!({
                "success": false,
                "error_kind": "compound_preflight_failed",
                "error": "Lab route, endpoint topology, shared materials, or inserter placement failed preflight. Nothing was placed.",
                "route": route,
                "preflight": preflight,
            });
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                .await;
        }

        let mut route_execute = route_params.clone();
        route_execute.dry_run = false;
        let route = match self.route_belt_core(&mut client, &route_execute).await {
            Ok(report) if report_success(&report) => report,
            Ok(report) => {
                let result = serde_json::json!({
                    "success": false,
                    "error_kind": "route_execution_failed",
                    "route": report,
                });
                return self
                    .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                    .await;
            }
            Err(error) => return self.with_player_messages(format!("Error: {error}")).await,
        };
        let mut transaction_units = route_report_placed_units(&route);
        let inserter = match client
            .place_entity("inserter", inserter_position, inserter_direction)
            .await
        {
            Ok(entity) => entity,
            Err(error) => {
                let rollback =
                    rollback_controller_transaction(&mut client, &transaction_units, &[], None)
                        .await;
                let result = serde_json::json!({
                    "success": false,
                    "error_kind": "inserter_placement_failed",
                    "error": error.to_string(),
                    "route": route,
                    "rollback": rollback,
                });
                return self
                    .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                    .await;
            }
        };
        let placed_inserter_unit = inserter.unit_number;
        if let Some(unit_number) = placed_inserter_unit {
            transaction_units.push(unit_number);
        }
        let inserter_report = serde_json::json!({
            "tool": "place_entity",
            "args": inserter_args,
            "success": true,
            "unit_number": placed_inserter_unit,
            "error": serde_json::Value::Null,
        });

        let verify_area = Area::new(
            lab.position.x - 8.0,
            lab.position.y - 8.0,
            lab.position.x + 8.0,
            lab.position.y + 8.0,
        );
        let persisted_inserter = match placed_inserter_unit {
            Some(unit_number) => client.get_entity(unit_number).await.ok(),
            None => None,
        };
        let infrastructure_verified = incremental_infrastructure_verification(
            &route,
            placed_inserter_unit,
            persisted_inserter.as_ref(),
            "inserter",
            inserter_position,
            inserter_direction,
        );
        let persisted_endpoint_topology = inserter_machine_endpoint_verification(
            &route,
            persisted_inserter.as_ref().map(|entity| entity.position),
            persisted_inserter.as_ref().map(Entity::direction_enum),
            &lab,
            InserterMachineFlow::Input,
            "persisted",
        );
        let infrastructure_verified =
            attach_endpoint_verification(infrastructure_verified, persisted_endpoint_topology);
        if !report_success(&infrastructure_verified) {
            let rollback =
                rollback_controller_transaction(&mut client, &transaction_units, &[], None).await;
            let result = serde_json::json!({
                "success": false,
                "error_kind": "infrastructure_verification_failed",
                "error": "Lab feed could not verify its complete route, exact inserter placement, and route-to-lab endpoint topology; the route and inserter were rolled back.",
                "route": route,
                "infrastructure_verified": infrastructure_verified,
                "rollback": rollback,
            });
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                .await;
        }

        let verification = match observe_production(&mut client, verify_area, 180).await {
            Ok(verification) => verification,
            Err(error) => serde_json::json!({"success": false, "error": error.to_string()}),
        };
        let production_verified = production_verification_summary(&verification, lab.unit_number);
        let production_success = report_success(&production_verified);
        let repair_hint = automation_repair_hint(
            "build_lab_feed",
            "science belt to lab feed",
            verification.get("error").is_none(),
            &verification,
            &[],
            Some(true),
        );

        let agent_id = client.agent_id().as_str().to_string();
        let research_status = match client
            .call_remote("get_research_status", &[serde_json::json!(agent_id)])
            .await
        {
            Ok(response) => match serde_json::from_str::<serde_json::Value>(&response) {
                Ok(report) => serde_json::json!({
                    "success": true,
                    "report": report,
                }),
                Err(e) => serde_json::json!({
                    "success": false,
                    "error": e.to_string(),
                    "raw": response,
                }),
            },
            Err(e) => serde_json::json!({
                "success": false,
                "error": e.to_string(),
            }),
        };

        let research_status_success = report_success(&research_status);
        let automation_success = production_success && research_status_success;
        let result = serde_json::json!({
            "success": true,
            "error_kind": serde_json::Value::Null,
            "placement_success": true,
            "dry_run": false,
            "lab": {
                "unit_number": lab.unit_number,
                "name": lab.name,
                "position": lab.position,
            },
            "route": route,
            "preflight": preflight,
            "inserter": inserter_report,
            "infrastructure_verified": infrastructure_verified,
            "production_verified": production_verified,
            "automation_verified": {
                "success": automation_success,
                "infrastructure_success": true,
                "production_success": production_success,
                "research_status_success": research_status_success,
            },
            "verification": verification,
            "research_status": research_status,
            "repair_hint": repair_hint,
            "guidance": if automation_success {
                "Lab feed infrastructure and live research consumption are both verified."
            } else {
                "Lab feed infrastructure is verified and retained. Live research consumption is not yet proven; ensure research is selected and science packs reach the belt without rebuilding this feed."
            },
        });
        let msg = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(msg).await
    }

    /// Build one belt plus inserter feed into an assembling machine.
    #[tool(
        description = "Execute a durable item feed into an assembling machine: optionally set its recipe, route an item belt to the inserter pickup tile, place the inserter feeding the assembler, then verify production. Use this for automation-science-pack inputs such as iron-gear-wheel and copper-plate instead of hand-feeding assemblers. Use dry_run=true during planner turns."
    )]
    async fn build_assembler_feed(
        &self,
        Parameters(params): Parameters<BuildAssemblerFeedParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let assembler = match client.get_entity(params.assembler_unit_number).await {
            Ok(entity) => entity,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        if !assembler.name.starts_with("assembling-machine") {
            return self
                .with_player_messages(format!(
                    "Error: unit {} is {}, not an assembling machine",
                    params.assembler_unit_number, assembler.name
                ))
                .await;
        }
        let inserter_direction = match Direction::parse(&params.inserter_direction) {
            Some(direction) => direction,
            None => {
                return self
                    .with_player_messages(invalid_direction_failure(
                        "inserter_direction",
                        &params.inserter_direction,
                    ))
                    .await;
            }
        };

        let route_params = RouteBeltParams {
            from_x: params.from_x,
            from_y: params.from_y,
            to_x: params.pickup_x,
            to_y: params.pickup_y,
            belt_type: params.belt_type.clone(),
            search_radius: params.search_radius,
            dry_run: true,
            respect_zones: params.respect_zones,
            allow_underground: params.allow_underground,
            extend_existing: params.extend_existing,
        };

        let inserter_position = Position::new(params.inserter_x, params.inserter_y);
        let reserved_route_tiles = HashSet::from([GridPos::from_position(&inserter_position)]);
        let mut route = match self
            .route_belt_core_avoiding(&mut client, &route_params, &reserved_route_tiles)
            .await
        {
            Ok(report) => report,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        if let Some(report) = route.as_object_mut() {
            report.remove("ready_to_call");
            report.insert(
                "controller_reserved_tiles".to_string(),
                serde_json::json!(&reserved_route_tiles),
            );
        }

        let recipe_args = serde_json::json!({
            "unit_number": params.assembler_unit_number,
            "recipe": params.recipe.clone(),
        });
        let inserter_args = serde_json::json!({
            "entity_name": "inserter",
            "x": params.inserter_x,
            "y": params.inserter_y,
            "direction": params.inserter_direction,
        });
        let preflight = match controller_preflight(
            &mut client,
            &[("assembler_feed", &route)],
            &params.belt_type,
            &HashSet::new(),
            &[ControllerPlacement {
                label: "input_inserter",
                item_name: "inserter",
                entity_name: "inserter",
                position: inserter_position,
                direction: inserter_direction,
            }],
        )
        .await
        {
            Ok(preflight) => preflight,
            Err(error) => return self.with_player_messages(format!("Error: {error}")).await,
        };
        let planned_endpoint_topology = inserter_machine_endpoint_verification(
            &route,
            Some(inserter_position),
            Some(inserter_direction),
            &assembler,
            InserterMachineFlow::Input,
            "planned",
        );
        let preflight = attach_endpoint_preflight(preflight, planned_endpoint_topology);
        let preflight_ready = preflight["ready"].as_bool() == Some(true);

        if params.dry_run {
            let mut execute_args =
                serde_json::to_value(&params).unwrap_or_else(|_| serde_json::json!({}));
            if let Some(args) = execute_args.as_object_mut() {
                args.insert("dry_run".to_string(), serde_json::json!(false));
            }
            let execute_step = serde_json::json!({
                "tool": "build_assembler_feed",
                "args": execute_args.clone(),
            });
            let ready_to_call = serde_json::json!({
                "tool": "build_assembler_feed",
                "execute_args": execute_args,
            });
            let result = serde_json::json!({
                "success": preflight_ready,
                "dry_run": true,
                "item_name": params.item_name.clone(),
                "assembler": {
                    "unit_number": assembler.unit_number,
                    "name": assembler.name,
                    "position": assembler.position,
                },
                "route": route,
                "preflight": preflight,
                "steps": [execute_step],
                "ready_to_call": ready_to_call,
                "guidance": "Execute only ready_to_call when preflight.ready is true so the route keeps the inserter footprint reserved. For multi-input recipes prefer a complete compound cell so verification can prove the assembler working.",
            });
            let msg =
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
            return self.with_player_messages(msg).await;
        }

        if !preflight_ready {
            let result = serde_json::json!({
                "success": false,
                "error_kind": "compound_preflight_failed",
                "error": "Assembler feed route, endpoint topology, shared materials, or inserter placement failed preflight. Nothing was changed.",
                "route": route,
                "preflight": preflight,
            });
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                .await;
        }

        let previous_recipe = if params.recipe.trim().is_empty() {
            None
        } else {
            match client.get_entity_recipe(params.assembler_unit_number).await {
                Ok(recipe) => Some(recipe),
                Err(error) => {
                    return self
                        .with_player_messages(format!("Error: reading previous recipe: {error}"))
                        .await
                }
            }
        };
        let mut route_execute = route_params.clone();
        route_execute.dry_run = false;
        let route = match self
            .route_belt_core_avoiding(&mut client, &route_execute, &reserved_route_tiles)
            .await
        {
            Ok(report) if report_success(&report) => report,
            Ok(report) => {
                let result = serde_json::json!({
                    "success": false,
                    "error_kind": "route_execution_failed",
                    "route": report,
                });
                return self
                    .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                    .await;
            }
            Err(error) => return self.with_player_messages(format!("Error: {error}")).await,
        };
        let mut transaction_units = route_report_placed_units(&route);
        let inserter = match client
            .place_entity("inserter", inserter_position, inserter_direction)
            .await
        {
            Ok(entity) => entity,
            Err(error) => {
                let rollback =
                    rollback_controller_transaction(&mut client, &transaction_units, &[], None)
                        .await;
                let result = serde_json::json!({
                    "success": false,
                    "error_kind": "inserter_placement_failed",
                    "error": error.to_string(),
                    "route": route,
                    "rollback": rollback,
                });
                return self
                    .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                    .await;
            }
        };
        let placed_inserter_unit = inserter.unit_number;
        if let Some(unit_number) = placed_inserter_unit {
            transaction_units.push(unit_number);
        }

        let recipe_report = if params.recipe.trim().is_empty() {
            serde_json::json!({
                "tool": "set_recipe",
                "skipped": true,
                "reason": "recipe was empty",
            })
        } else {
            let set_recipe = client
                .set_recipe(params.assembler_unit_number, params.recipe.trim())
                .await;
            if let Err(error) = &set_recipe {
                let recipe_restore = previous_recipe
                    .clone()
                    .map(|recipe| (params.assembler_unit_number, recipe));
                let rollback = rollback_controller_transaction(
                    &mut client,
                    &transaction_units,
                    &[],
                    recipe_restore,
                )
                .await;
                let result = serde_json::json!({
                    "success": false,
                    "error_kind": "set_recipe_failed",
                    "error": error.to_string(),
                    "route": route,
                    "rollback": rollback,
                });
                return self
                    .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                    .await;
            }
            serde_json::json!({
                "tool": "set_recipe",
                "args": recipe_args,
                "success": set_recipe.is_ok(),
                "error": set_recipe.as_ref().err().map(|e| e.to_string()),
            })
        };
        let inserter_report = serde_json::json!({
            "tool": "place_entity",
            "args": inserter_args,
            "success": true,
            "unit_number": placed_inserter_unit,
            "error": serde_json::Value::Null,
        });

        let verify_radius = params.verify_radius.clamp(1, 50) as f64;
        let verify_area = Area::new(
            assembler.position.x - verify_radius,
            assembler.position.y - verify_radius,
            assembler.position.x + verify_radius,
            assembler.position.y + verify_radius,
        );
        let persisted_inserter = match placed_inserter_unit {
            Some(unit_number) => client.get_entity(unit_number).await.ok(),
            None => None,
        };
        let infrastructure_verified = incremental_infrastructure_verification(
            &route,
            placed_inserter_unit,
            persisted_inserter.as_ref(),
            "inserter",
            inserter_position,
            inserter_direction,
        );
        let persisted_endpoint_topology = inserter_machine_endpoint_verification(
            &route,
            persisted_inserter.as_ref().map(|entity| entity.position),
            persisted_inserter.as_ref().map(Entity::direction_enum),
            &assembler,
            InserterMachineFlow::Input,
            "persisted",
        );
        let infrastructure_verified =
            attach_endpoint_verification(infrastructure_verified, persisted_endpoint_topology);
        if !report_success(&infrastructure_verified) {
            let recipe_restore = previous_recipe
                .clone()
                .map(|recipe| (params.assembler_unit_number, recipe));
            let rollback = rollback_controller_transaction(
                &mut client,
                &transaction_units,
                &[],
                recipe_restore,
            )
            .await;
            let result = serde_json::json!({
                "success": false,
                "error_kind": "infrastructure_verification_failed",
                "error": "Assembler feed could not verify its complete route, exact inserter placement, and route-to-assembler endpoint topology; the route, inserter, and recipe change were rolled back.",
                "route": route,
                "infrastructure_verified": infrastructure_verified,
                "rollback": rollback,
            });
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                .await;
        }

        let verification = match observe_production(&mut client, verify_area, 180).await {
            Ok(verification) => verification,
            Err(error) => serde_json::json!({"success": false, "error": error.to_string()}),
        };
        let production_verified =
            production_verification_summary(&verification, assembler.unit_number);
        let production_success = report_success(&production_verified);
        let repair_hint = automation_repair_hint(
            "build_assembler_feed",
            "item belt to assembler input feed",
            verification.get("error").is_none(),
            &verification,
            &[],
            Some(true),
        );
        let result = serde_json::json!({
            "success": true,
            "error_kind": serde_json::Value::Null,
            "placement_success": true,
            "dry_run": false,
            "item_name": params.item_name.clone(),
            "assembler": {
                "unit_number": assembler.unit_number,
                "name": assembler.name,
                "position": assembler.position,
            },
            "recipe": recipe_report,
            "route": route,
            "preflight": preflight,
            "inserter": inserter_report,
            "infrastructure_verified": infrastructure_verified,
            "production_verified": production_verified,
            "automation_verified": {
                "success": production_success,
                "infrastructure_success": true,
                "production_success": production_success,
            },
            "verification": verification,
            "repair_hint": repair_hint,
            "guidance": if production_success {
                "Assembler input infrastructure and target production are both verified."
            } else {
                "Assembler input infrastructure is verified and retained. Target production is not yet live; add or repair the other required inputs or power without rebuilding this feed."
            },
        });
        let msg = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(msg).await
    }

    /// Plan a machine/furnace output belt and inserter without hand-authored geometry.
    #[tool(
        description = "Read-only layout planner for extracting output from a crafting machine or furnace. Takes a source unit, item name, target belt tile, and output side; returns ready_to_call build_assembler_output args. Use this before build_assembler_output instead of hand-deriving inserter/drop coordinates."
    )]
    async fn plan_machine_output(
        &self,
        Parameters(params): Parameters<PlanMachineOutputParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let source_machine = match client.get_entity(params.source_unit_number).await {
            Ok(entity) => entity,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let build_args = match machine_output_build_args(
            &source_machine,
            params.item_name.clone(),
            params.to_x,
            params.to_y,
            &params.output_side,
            params.belt_type,
            params.search_radius,
            params.respect_zones,
            params.allow_underground,
            params.extend_existing,
            params.verify_radius,
        ) {
            Ok(args) => args,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let route_params = RouteBeltParams {
            from_x: build_args.drop_x,
            from_y: build_args.drop_y,
            to_x: build_args.to_x,
            to_y: build_args.to_y,
            belt_type: build_args.belt_type.clone(),
            search_radius: build_args.search_radius,
            dry_run: true,
            respect_zones: build_args.respect_zones,
            allow_underground: build_args.allow_underground,
            extend_existing: build_args.extend_existing,
        };
        let route = match self.route_belt_core(&mut client, &route_params).await {
            Ok(report) => report,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let mut execute_args =
            serde_json::to_value(&build_args).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(object) = execute_args.as_object_mut() {
            object.insert("dry_run".to_string(), serde_json::json!(false));
        }
        let success = route
            .get("success")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
            && route
                .get("materials_sufficient")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
        let result = serde_json::json!({
            "success": success,
            "dry_run": true,
            "item_name": params.item_name,
            "source_machine": {
                "unit_number": source_machine.unit_number,
                "name": source_machine.name,
                "position": source_machine.position,
            },
            "output_side": params.output_side,
            "route": route,
            "ready_to_call": {
                "tool": "build_assembler_output",
                "dry_run_args": build_args,
                "execute_args": execute_args,
            },
            "guidance": "If success is true, call build_assembler_output with ready_to_call.execute_args. If false, choose a different output_side or target belt tile; do not hand-extract products.",
        });
        let msg = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(msg).await
    }

    /// Build one output belt plus inserter from a crafting machine or furnace.
    #[tool(
        description = "Execute a durable output from a crafting machine or furnace: route a belt from the output inserter drop tile to a target belt tile, place the inserter extracting from the machine, then verify production. Use this for furnace plate output, assembler ingredient/output belts, and automation-science-pack output instead of hand-extracting products. Use dry_run=true during planner turns."
    )]
    async fn build_assembler_output(
        &self,
        Parameters(params): Parameters<BuildAssemblerOutputParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let source_machine = match client.get_entity(params.assembler_unit_number).await {
            Ok(entity) => entity,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        if !is_machine_output_source(&source_machine) {
            return self
                .with_player_messages(format!(
                    "Error: unit {} is {}, not a supported output machine/furnace",
                    params.assembler_unit_number, source_machine.name
                ))
                .await;
        }
        let inserter_direction = match Direction::parse(&params.inserter_direction) {
            Some(direction) => direction,
            None => {
                return self
                    .with_player_messages(invalid_direction_failure(
                        "inserter_direction",
                        &params.inserter_direction,
                    ))
                    .await;
            }
        };

        let route_params = RouteBeltParams {
            from_x: params.drop_x,
            from_y: params.drop_y,
            to_x: params.to_x,
            to_y: params.to_y,
            belt_type: params.belt_type.clone(),
            search_radius: params.search_radius,
            dry_run: true,
            respect_zones: params.respect_zones,
            allow_underground: params.allow_underground,
            extend_existing: params.extend_existing,
        };

        let route = match self.route_belt_core(&mut client, &route_params).await {
            Ok(report) => report,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let inserter_args = serde_json::json!({
            "entity_name": "inserter",
            "x": params.inserter_x,
            "y": params.inserter_y,
            "direction": params.inserter_direction,
        });
        let inserter_position = Position::new(params.inserter_x, params.inserter_y);
        let preflight = match controller_preflight(
            &mut client,
            &[("machine_output", &route)],
            &params.belt_type,
            &HashSet::new(),
            &[ControllerPlacement {
                label: "output_inserter",
                item_name: "inserter",
                entity_name: "inserter",
                position: inserter_position,
                direction: inserter_direction,
            }],
        )
        .await
        {
            Ok(preflight) => preflight,
            Err(error) => return self.with_player_messages(format!("Error: {error}")).await,
        };
        let planned_endpoint_topology = inserter_machine_endpoint_verification(
            &route,
            Some(inserter_position),
            Some(inserter_direction),
            &source_machine,
            InserterMachineFlow::Output,
            "planned",
        );
        let preflight = attach_endpoint_preflight(preflight, planned_endpoint_topology);
        let preflight_ready = preflight["ready"].as_bool() == Some(true);

        if params.dry_run {
            let result = serde_json::json!({
                "success": preflight_ready,
                "dry_run": true,
                "item_name": params.item_name.clone(),
                "source_machine": {
                    "unit_number": source_machine.unit_number,
                    "name": source_machine.name,
                    "position": source_machine.position,
                },
                "assembler": {
                    "unit_number": source_machine.unit_number,
                    "name": source_machine.name,
                    "position": source_machine.position,
                },
                "route": route,
                "preflight": preflight,
                "steps": [{
                    "tool": "route_belt",
                    "args": route_params,
                }, {
                    "tool": "place_entity",
                    "args": inserter_args,
                }, {
                    "tool": "verify_production",
                    "args": {
                        "x": source_machine.position.x,
                        "y": source_machine.position.y,
                        "radius": params.verify_radius,
                    },
                }],
                "guidance": "Execute only when preflight.ready is true; the route and output inserter are one transaction.",
            });
            let msg =
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
            return self.with_player_messages(msg).await;
        }

        if !preflight_ready {
            let result = serde_json::json!({
                "success": false,
                "error_kind": "compound_preflight_failed",
                "error": "Output route, endpoint topology, shared materials, or inserter placement failed preflight. Nothing was placed.",
                "route": route,
                "preflight": preflight,
            });
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                .await;
        }
        let mut route_execute = route_params.clone();
        route_execute.dry_run = false;
        let route = match self.route_belt_core(&mut client, &route_execute).await {
            Ok(report) if report_success(&report) => report,
            Ok(report) => {
                let result = serde_json::json!({
                    "success": false,
                    "error_kind": "route_execution_failed",
                    "route": report,
                });
                return self
                    .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                    .await;
            }
            Err(error) => return self.with_player_messages(format!("Error: {error}")).await,
        };
        let mut transaction_units = route_report_placed_units(&route);
        let inserter = match client
            .place_entity("inserter", inserter_position, inserter_direction)
            .await
        {
            Ok(entity) => entity,
            Err(error) => {
                let rollback =
                    rollback_controller_transaction(&mut client, &transaction_units, &[], None)
                        .await;
                let result = serde_json::json!({
                    "success": false,
                    "error_kind": "inserter_placement_failed",
                    "error": error.to_string(),
                    "route": route,
                    "rollback": rollback,
                });
                return self
                    .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                    .await;
            }
        };
        let placed_inserter_unit = inserter.unit_number;
        if let Some(unit_number) = placed_inserter_unit {
            transaction_units.push(unit_number);
        }
        let inserter_report = serde_json::json!({
            "tool": "place_entity",
            "args": inserter_args,
            "success": true,
            "unit_number": placed_inserter_unit,
            "error": serde_json::Value::Null,
        });

        let verify_radius = params.verify_radius.clamp(1, 50) as f64;
        let verify_area = Area::new(
            source_machine.position.x - verify_radius,
            source_machine.position.y - verify_radius,
            source_machine.position.x + verify_radius,
            source_machine.position.y + verify_radius,
        );
        let persisted_inserter = match placed_inserter_unit {
            Some(unit_number) => client.get_entity(unit_number).await.ok(),
            None => None,
        };
        let infrastructure_verified = incremental_infrastructure_verification(
            &route,
            placed_inserter_unit,
            persisted_inserter.as_ref(),
            "inserter",
            inserter_position,
            inserter_direction,
        );
        let persisted_endpoint_topology = inserter_machine_endpoint_verification(
            &route,
            persisted_inserter.as_ref().map(|entity| entity.position),
            persisted_inserter.as_ref().map(Entity::direction_enum),
            &source_machine,
            InserterMachineFlow::Output,
            "persisted",
        );
        let infrastructure_verified =
            attach_endpoint_verification(infrastructure_verified, persisted_endpoint_topology);
        if !report_success(&infrastructure_verified) {
            let rollback =
                rollback_controller_transaction(&mut client, &transaction_units, &[], None).await;
            let result = serde_json::json!({
                "success": false,
                "error_kind": "infrastructure_verification_failed",
                "error": "Machine output could not verify its complete route, exact inserter placement, and machine-to-route endpoint topology; the route and inserter were rolled back.",
                "route": route,
                "infrastructure_verified": infrastructure_verified,
                "rollback": rollback,
            });
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                .await;
        }

        let verification = match observe_production(&mut client, verify_area, 180).await {
            Ok(verification) => verification,
            Err(error) => serde_json::json!({"success": false, "error": error.to_string()}),
        };
        let production_verified =
            production_verification_summary(&verification, source_machine.unit_number);
        let production_success = report_success(&production_verified);
        let repair_hint = automation_repair_hint(
            "build_assembler_output",
            "machine output belt",
            verification.get("error").is_none(),
            &verification,
            &[],
            Some(true),
        );

        let result = serde_json::json!({
            "success": true,
            "error_kind": serde_json::Value::Null,
            "placement_success": true,
            "dry_run": false,
            "item_name": params.item_name.clone(),
            "source_machine": {
                "unit_number": source_machine.unit_number,
                "name": source_machine.name,
                "position": source_machine.position,
            },
            "assembler": {
                "unit_number": source_machine.unit_number,
                "name": source_machine.name,
                "position": source_machine.position,
            },
            "route": route,
            "preflight": preflight,
            "inserter": inserter_report,
            "infrastructure_verified": infrastructure_verified,
            "production_verified": production_verified,
            "automation_verified": {
                "success": production_success,
                "infrastructure_success": true,
                "production_success": production_success,
            },
            "verification": verification,
            "repair_hint": repair_hint,
            "guidance": if production_success {
                "Machine output infrastructure and source production are both verified."
            } else {
                "Machine output infrastructure is verified and retained. Source production is not yet live; repair its inputs, recipe, fuel, power, or downstream capacity without rebuilding this output."
            },
        });
        let msg = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(msg).await
    }

    /// Plan a one-input recipe assembler cell.
    #[tool(
        description = "Read-only layout planner for a one-input assembler component cell, such as iron-gear-wheel from iron-plate. Derives input/output belt and inserter coordinates from an assembler and side choices, dry-runs both belt routes, and returns ready_to_call build_recipe_assembler_cell payloads. Use this before hand-crafting gears, cables, or circuits for science automation."
    )]
    async fn plan_recipe_assembler_cell(
        &self,
        Parameters(params): Parameters<PlanRecipeAssemblerCellParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let assembler = match client.get_entity(params.assembler_unit_number).await {
            Ok(entity) => entity,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        if !assembler.name.starts_with("assembling-machine") {
            return self
                .with_player_messages(format!(
                    "Error: unit {} is {}, not an assembling machine",
                    params.assembler_unit_number, assembler.name
                ))
                .await;
        }

        let input = match machine_side_layout(&assembler, &params.input_side) {
            Ok(layout) => layout,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let output = match machine_side_layout(&assembler, &params.output_side) {
            Ok(layout) => layout,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        if input.side == output.side {
            return self
                .with_player_messages(format!(
                    "Error: input_side and output_side both resolve to {}; choose different sides",
                    input.side
                ))
                .await;
        }

        let build_args = BuildRecipeAssemblerCellParams {
            assembler_unit_number: params.assembler_unit_number,
            recipe: params.recipe.clone(),
            input_item_name: params.input_item_name.clone(),
            output_item_name: params.output_item_name.clone(),
            input_from_x: params.input_from_x,
            input_from_y: params.input_from_y,
            input_pickup_x: input.belt_x,
            input_pickup_y: input.belt_y,
            input_inserter_x: input.inserter_x,
            input_inserter_y: input.inserter_y,
            input_inserter_direction: input.input_direction.to_string(),
            output_drop_x: output.belt_x,
            output_drop_y: output.belt_y,
            output_to_x: params.output_to_x,
            output_to_y: params.output_to_y,
            output_inserter_x: output.inserter_x,
            output_inserter_y: output.inserter_y,
            output_inserter_direction: output.output_direction.to_string(),
            belt_type: params.belt_type.clone(),
            search_radius: params.search_radius,
            dry_run: true,
            respect_zones: params.respect_zones,
            allow_underground: params.allow_underground,
            extend_existing: params.extend_existing,
            verify_radius: params.verify_radius,
        };
        let input_route_params = RouteBeltParams {
            from_x: params.input_from_x,
            from_y: params.input_from_y,
            to_x: input.belt_x,
            to_y: input.belt_y,
            belt_type: params.belt_type.clone(),
            search_radius: params.search_radius,
            dry_run: true,
            respect_zones: params.respect_zones,
            allow_underground: params.allow_underground,
            extend_existing: params.extend_existing,
        };
        let output_route_params = RouteBeltParams {
            from_x: output.belt_x,
            from_y: output.belt_y,
            to_x: params.output_to_x,
            to_y: params.output_to_y,
            belt_type: params.belt_type.clone(),
            search_radius: params.search_radius,
            dry_run: true,
            respect_zones: params.respect_zones,
            allow_underground: params.allow_underground,
            extend_existing: params.extend_existing,
        };

        let input_route = match self.route_belt_core(&mut client, &input_route_params).await {
            Ok(report) => report,
            Err(error) => serde_json::json!({"success": false, "error": error}),
        };
        let output_route = match self
            .route_belt_core(&mut client, &output_route_params)
            .await
        {
            Ok(report) => report,
            Err(error) => serde_json::json!({"success": false, "error": error}),
        };
        let routes = serde_json::json!({
            "input": input_route,
            "output": output_route,
        });
        let inventory = client.character_inventory().await.ok();
        let available_items: BTreeMap<String, u32> = inventory
            .as_ref()
            .map(|inventory| {
                inventory
                    .items
                    .iter()
                    .map(|item| (item.name.clone(), item.count))
                    .collect()
            })
            .unwrap_or_default();
        let additional_items = BTreeMap::from([("inserter".to_string(), 2)]);
        let route_reports = [("input", &routes["input"]), ("output", &routes["output"])];
        let compound_preflight = compound_route_preflight(
            &route_reports,
            &available_items,
            &additional_items,
            &params.belt_type,
            &HashSet::new(),
            &[
                (
                    "input_inserter",
                    GridPos::from_position(&Position::new(input.inserter_x, input.inserter_y)),
                ),
                (
                    "output_inserter",
                    GridPos::from_position(&Position::new(output.inserter_x, output.inserter_y)),
                ),
            ],
        );
        let placement_preflight = serde_json::json!({
            "input_inserter": client.check_entity_placement(
                "inserter",
                Position::new(input.inserter_x, input.inserter_y),
                Direction::parse(input.input_direction).unwrap_or(Direction::North),
            ).await.ok(),
            "output_inserter": client.check_entity_placement(
                "inserter",
                Position::new(output.inserter_x, output.inserter_y),
                Direction::parse(output.output_direction).unwrap_or(Direction::North),
            ).await.ok(),
        });
        let placements_ready = placement_preflight
            .as_object()
            .is_some_and(|checks| checks.values().all(placement_report_allowed));
        let route_ready = compound_preflight["ready"].as_bool() == Some(true)
            && placements_ready
            && inventory.is_some();

        let mut execute_args =
            serde_json::to_value(&build_args).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(object) = execute_args.as_object_mut() {
            object.insert("dry_run".to_string(), serde_json::json!(false));
        }

        let result = serde_json::json!({
            "success": route_ready,
            "dry_run": true,
            "recipe": params.recipe,
            "input_item_name": params.input_item_name,
            "output_item_name": params.output_item_name,
            "assembler": {
                "unit_number": assembler.unit_number,
                "name": assembler.name,
                "position": assembler.position,
            },
            "sides": {
                "input": input,
                "output": output,
            },
            "routes": routes,
            "compound_preflight": compound_preflight,
            "placement_preflight": placement_preflight,
            "ready_to_call": {
                "tool": "build_recipe_assembler_cell",
                "dry_run_args": build_args,
                "execute_args": execute_args,
            },
            "guidance": "If success is true, call build_recipe_assembler_cell with ready_to_call.execute_args. Use its output belt as the source for downstream plan_automation_science/build_automation_science instead of hand-crafting components.",
        });
        let msg = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(msg).await
    }

    /// Build a one-input recipe assembler cell.
    #[tool(
        description = "Execute a durable one-input assembler component cell: set the recipe, route the input item belt to an inserter, place the input inserter, route the product output belt, place the output inserter, then verify production. Use this for iron-gear-wheel or copper-cable cells instead of hand-crafting science ingredients. Use dry_run=true during planner turns."
    )]
    async fn build_recipe_assembler_cell(
        &self,
        Parameters(params): Parameters<BuildRecipeAssemblerCellParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let assembler = match client.get_entity(params.assembler_unit_number).await {
            Ok(entity) => entity,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        if !assembler.name.starts_with("assembling-machine") {
            return self
                .with_player_messages(format!(
                    "Error: unit {} is {}, not an assembling machine",
                    params.assembler_unit_number, assembler.name
                ))
                .await;
        }
        let input_direction = match Direction::parse(&params.input_inserter_direction) {
            Some(direction) => direction,
            None => {
                return self
                    .with_player_messages(invalid_direction_failure(
                        "input_inserter_direction",
                        &params.input_inserter_direction,
                    ))
                    .await;
            }
        };
        let output_direction = match Direction::parse(&params.output_inserter_direction) {
            Some(direction) => direction,
            None => {
                return self
                    .with_player_messages(invalid_direction_failure(
                        "output_inserter_direction",
                        &params.output_inserter_direction,
                    ))
                    .await;
            }
        };

        let input_route_params = RouteBeltParams {
            from_x: params.input_from_x,
            from_y: params.input_from_y,
            to_x: params.input_pickup_x,
            to_y: params.input_pickup_y,
            belt_type: params.belt_type.clone(),
            search_radius: params.search_radius,
            dry_run: true,
            respect_zones: params.respect_zones,
            allow_underground: params.allow_underground,
            extend_existing: params.extend_existing,
        };
        let output_route_params = RouteBeltParams {
            from_x: params.output_drop_x,
            from_y: params.output_drop_y,
            to_x: params.output_to_x,
            to_y: params.output_to_y,
            belt_type: params.belt_type.clone(),
            search_radius: params.search_radius,
            dry_run: true,
            respect_zones: params.respect_zones,
            allow_underground: params.allow_underground,
            extend_existing: params.extend_existing,
        };
        let input_route = match self.route_belt_core(&mut client, &input_route_params).await {
            Ok(report) => report,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let output_route = match self
            .route_belt_core(&mut client, &output_route_params)
            .await
        {
            Ok(report) => report,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let inventory = match client.character_inventory().await {
            Ok(inventory) => inventory,
            Err(error) => {
                return self
                    .with_player_messages(format!("Error: checking compound materials: {error}"))
                    .await
            }
        };
        let available_items: BTreeMap<String, u32> = inventory
            .items
            .iter()
            .map(|item| (item.name.clone(), item.count))
            .collect();
        let additional_items = BTreeMap::from([("inserter".to_string(), 2)]);
        let compound_preflight = compound_route_preflight(
            &[("input", &input_route), ("output", &output_route)],
            &available_items,
            &additional_items,
            &params.belt_type,
            &HashSet::new(),
            &[
                (
                    "input_inserter",
                    GridPos::from_position(&Position::new(
                        params.input_inserter_x,
                        params.input_inserter_y,
                    )),
                ),
                (
                    "output_inserter",
                    GridPos::from_position(&Position::new(
                        params.output_inserter_x,
                        params.output_inserter_y,
                    )),
                ),
            ],
        );
        let input_placement = client
            .check_entity_placement(
                "inserter",
                Position::new(params.input_inserter_x, params.input_inserter_y),
                input_direction,
            )
            .await;
        let output_placement = client
            .check_entity_placement(
                "inserter",
                Position::new(params.output_inserter_x, params.output_inserter_y),
                output_direction,
            )
            .await;
        let placement_allowed = |check: &anyhow::Result<serde_json::Value>| {
            check.as_ref().ok().is_some_and(placement_report_allowed)
        };
        let placement_preflight = serde_json::json!({
            "input_inserter": input_placement.as_ref().map_err(|error| error.to_string()),
            "output_inserter": output_placement.as_ref().map_err(|error| error.to_string()),
        });
        let preflight_ready = compound_preflight["ready"].as_bool() == Some(true)
            && placement_allowed(&input_placement)
            && placement_allowed(&output_placement);

        let recipe_args = serde_json::json!({
            "unit_number": params.assembler_unit_number,
            "recipe": params.recipe.clone(),
        });
        let input_inserter_args = serde_json::json!({
            "entity_name": "inserter",
            "x": params.input_inserter_x,
            "y": params.input_inserter_y,
            "direction": params.input_inserter_direction,
        });
        let output_inserter_args = serde_json::json!({
            "entity_name": "inserter",
            "x": params.output_inserter_x,
            "y": params.output_inserter_y,
            "direction": params.output_inserter_direction,
        });

        if params.dry_run {
            let result = serde_json::json!({
                "success": preflight_ready,
                "dry_run": true,
                "recipe": params.recipe,
                "input_item_name": params.input_item_name,
                "output_item_name": params.output_item_name,
                "assembler": {
                    "unit_number": assembler.unit_number,
                    "name": assembler.name,
                    "position": assembler.position,
                },
                "routes": {
                    "input": input_route,
                    "output": output_route,
                },
                "compound_preflight": compound_preflight,
                "placement_preflight": placement_preflight,
                "steps": [{
                    "tool": "set_recipe",
                    "args": recipe_args,
                }, {
                    "tool": "route_belt",
                    "item": params.input_item_name,
                    "args": input_route_params,
                }, {
                    "tool": "place_entity",
                    "item": params.input_item_name,
                    "args": input_inserter_args,
                }, {
                    "tool": "route_belt",
                    "item": params.output_item_name,
                    "args": output_route_params,
                }, {
                    "tool": "place_entity",
                    "item": params.output_item_name,
                    "args": output_inserter_args,
                }, {
                    "tool": "verify_production",
                    "args": {
                        "x": assembler.position.x,
                        "y": assembler.position.y,
                        "radius": params.verify_radius,
                    },
                }],
                "guidance": "If both routes have materials_sufficient=true and inserter placements are clear, call build_recipe_assembler_cell again with dry_run=false.",
            });
            let msg =
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
            return self.with_player_messages(msg).await;
        }

        if !preflight_ready {
            let result = serde_json::json!({
                "success": false,
                "dry_run": false,
                "error_kind": "compound_preflight_failed",
                "error": "The complete assembler cell failed shared material, route, or inserter preflight. Nothing was placed.",
                "routes": { "input": input_route, "output": output_route },
                "compound_preflight": compound_preflight,
                "placement_preflight": placement_preflight,
            });
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                .await;
        }

        let previous_recipe = match client.get_entity_recipe(params.assembler_unit_number).await {
            Ok(recipe) => recipe,
            Err(error) => {
                return self
                    .with_player_messages(format!("Error: reading previous recipe: {error}"))
                    .await
            }
        };

        let mut input_execute = input_route_params.clone();
        input_execute.dry_run = false;
        let mut output_execute = output_route_params.clone();
        output_execute.dry_run = false;
        let input_route = match self.route_belt_core(&mut client, &input_execute).await {
            Ok(report) if report_success(&report) => report,
            Ok(report) => {
                let result = serde_json::json!({
                    "success": false,
                    "error_kind": "input_route_execution_failed",
                    "route": report,
                    "rollback": { "success": true, "removed_units": [], "errors": [] },
                });
                return self
                    .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                    .await;
            }
            Err(error) => return self.with_player_messages(format!("Error: {error}")).await,
        };
        let output_route = match self.route_belt_core(&mut client, &output_execute).await {
            Ok(report) if report_success(&report) => report,
            Ok(report) => {
                let rollback =
                    rollback_exact_units(&mut client, &route_report_placed_units(&input_route))
                        .await;
                let result = serde_json::json!({
                    "success": false,
                    "error_kind": "output_route_execution_failed",
                    "route": report,
                    "rollback": rollback,
                });
                return self
                    .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                    .await;
            }
            Err(error) => {
                let rollback =
                    rollback_exact_units(&mut client, &route_report_placed_units(&input_route))
                        .await;
                let result = serde_json::json!({
                    "success": false,
                    "error_kind": "output_route_execution_failed",
                    "error": error,
                    "rollback": rollback,
                });
                return self
                    .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                    .await;
            }
        };

        let mut transaction_units = route_report_placed_units(&input_route);
        transaction_units.extend(route_report_placed_units(&output_route));
        let input_inserter = client
            .place_entity(
                "inserter",
                Position::new(params.input_inserter_x, params.input_inserter_y),
                input_direction,
            )
            .await;
        if let Err(error) = &input_inserter {
            let rollback = rollback_exact_units(&mut client, &transaction_units).await;
            let result = serde_json::json!({
                "success": false,
                "error_kind": "input_inserter_placement_failed",
                "error": error.to_string(),
                "rollback": rollback,
            });
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                .await;
        }
        if let Some(unit) = input_inserter
            .as_ref()
            .ok()
            .and_then(|entity| entity.unit_number)
        {
            transaction_units.push(unit);
        }
        let output_inserter = client
            .place_entity(
                "inserter",
                Position::new(params.output_inserter_x, params.output_inserter_y),
                output_direction,
            )
            .await;
        if let Err(error) = &output_inserter {
            let rollback = rollback_exact_units(&mut client, &transaction_units).await;
            let result = serde_json::json!({
                "success": false,
                "error_kind": "output_inserter_placement_failed",
                "error": error.to_string(),
                "rollback": rollback,
            });
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                .await;
        }
        let input_inserter_unit = input_inserter
            .as_ref()
            .ok()
            .and_then(|entity| entity.unit_number);
        let output_inserter_unit = output_inserter
            .as_ref()
            .ok()
            .and_then(|entity| entity.unit_number);
        if let Some(unit) = output_inserter_unit {
            transaction_units.push(unit);
        }
        let set_recipe = client
            .set_recipe(params.assembler_unit_number, params.recipe.trim())
            .await;
        if let Err(error) = &set_recipe {
            let rollback = rollback_controller_transaction(
                &mut client,
                &transaction_units,
                &[],
                Some((params.assembler_unit_number, previous_recipe.clone())),
            )
            .await;
            let result = serde_json::json!({
                "success": false,
                "error_kind": "set_recipe_failed",
                "error": error.to_string(),
                "rollback": rollback,
            });
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                .await;
        }
        let recipe_report = serde_json::json!({
            "tool": "set_recipe",
            "args": recipe_args,
            "success": true,
            "error": serde_json::Value::Null,
        });

        let verify_radius = params.verify_radius.clamp(1, 50) as f64;
        let verify_area = Area::new(
            assembler.position.x - verify_radius,
            assembler.position.y - verify_radius,
            assembler.position.x + verify_radius,
            assembler.position.y + verify_radius,
        );
        let verification = match observe_production(&mut client, verify_area, 180).await {
            Ok(verification) => verification,
            Err(error) => serde_json::json!({"success": false, "error": error.to_string()}),
        };
        let assembler_working = production_unit_verified(&verification, assembler.unit_number);
        let mut inserters_exist = input_inserter_unit.is_some() && output_inserter_unit.is_some();
        for unit_number in [input_inserter_unit, output_inserter_unit]
            .into_iter()
            .flatten()
        {
            inserters_exist &= client.get_entity(unit_number).await.is_ok();
        }
        let route_ok = |value: &serde_json::Value| {
            value
                .get("success")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        };
        let routes_success = route_ok(&input_route) && route_ok(&output_route);
        let repair_hint = automation_repair_hint(
            "build_recipe_assembler_cell",
            "one-input assembler component cell",
            verification.get("error").is_none(),
            &verification,
            &[],
            Some(routes_success),
        );
        let success = routes_success && assembler_working && inserters_exist;
        if !success {
            let rollback = rollback_controller_transaction(
                &mut client,
                &transaction_units,
                &[],
                Some((params.assembler_unit_number, previous_recipe)),
            )
            .await;
            let result = serde_json::json!({
                "success": false,
                "error_kind": "verification_failed",
                "error": "Assembler cell did not prove the target assembler actively producing; both routes, both inserters, and the recipe change were rolled back.",
                "routes": {"input": input_route, "output": output_route},
                "verification": verification,
                "rollback": rollback,
                "repair_hint": repair_hint,
            });
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                .await;
        }

        let result = serde_json::json!({
            "success": success,
            "placement_success": input_inserter.is_ok() && output_inserter.is_ok(),
            "dry_run": false,
            "recipe": params.recipe,
            "input_item_name": params.input_item_name,
            "output_item_name": params.output_item_name,
            "assembler": {
                "unit_number": assembler.unit_number,
                "name": assembler.name,
                "position": assembler.position,
            },
            "recipe_set": recipe_report,
            "routes": {
                "input": input_route,
                "output": output_route,
            },
            "inserters": {
                "input": {
                    "tool": "place_entity",
                    "args": input_inserter_args,
                    "success": input_inserter.is_ok(),
                    "unit_number": input_inserter_unit,
                    "error": input_inserter.as_ref().err().map(|e| e.to_string()),
                },
                "output": {
                    "tool": "place_entity",
                    "args": output_inserter_args,
                    "success": output_inserter.is_ok(),
                    "unit_number": output_inserter_unit,
                    "error": output_inserter.as_ref().err().map(|e| e.to_string()),
                },
            },
            "automation_verified": {
                "success": true,
                "assembler_working": assembler_working,
                "inserters_exist": inserters_exist,
            },
            "verification": verification,
            "repair_hint": repair_hint,
            "guidance": "If success is true, the component is being assembled onto the output belt. Use output_drop/output target as the source belt for downstream assembler feeds.",
        });
        let msg = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(msg).await
    }

    /// Plan the payload for a complete automation-science assembler-to-lab cell.
    #[tool(
        description = "Read-only layout planner for automation-science-pack automation. Takes an assembler, lab, gear source belt tile, and copper source belt tile; chooses side-based inserter/pickup/drop coordinates; dry-runs all belt routes; and returns a ready_to_call build_automation_science payload. Use this in planner turns instead of hand-deriving 30 coordinates."
    )]
    async fn plan_automation_science(
        &self,
        Parameters(params): Parameters<PlanAutomationScienceParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let assembler = match client.get_entity(params.assembler_unit_number).await {
            Ok(entity) => entity,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        if !assembler.name.starts_with("assembling-machine") {
            return self
                .with_player_messages(format!(
                    "Error: unit {} is {}, not an assembling machine",
                    params.assembler_unit_number, assembler.name
                ))
                .await;
        }

        let lab = match client.get_entity(params.lab_unit_number).await {
            Ok(entity) => entity,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        if lab.name != "lab" {
            return self
                .with_player_messages(format!(
                    "Error: unit {} is {}, not lab",
                    params.lab_unit_number, lab.name
                ))
                .await;
        }

        let assembler_sides = [
            params.gear_side.to_ascii_lowercase(),
            params.copper_side.to_ascii_lowercase(),
            params.output_side.to_ascii_lowercase(),
        ];
        if assembler_sides.iter().collect::<HashSet<_>>().len() != assembler_sides.len() {
            return self
                .with_player_messages(
                    "Error: gear_side, copper_side, and output_side must be three different assembler sides"
                        .to_string(),
                )
                .await;
        }

        let gear = match machine_side_layout(&assembler, &params.gear_side) {
            Ok(layout) => layout,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let copper = match machine_side_layout(&assembler, &params.copper_side) {
            Ok(layout) => layout,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let output = match machine_side_layout(&assembler, &params.output_side) {
            Ok(layout) => layout,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let lab_feed = match machine_side_layout(&lab, &params.lab_side) {
            Ok(layout) => layout,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let build_args = BuildAutomationScienceParams {
            assembler_unit_number: params.assembler_unit_number,
            lab_unit_number: params.lab_unit_number,
            gear_from_x: params.gear_from_x,
            gear_from_y: params.gear_from_y,
            gear_pickup_x: gear.belt_x,
            gear_pickup_y: gear.belt_y,
            gear_inserter_x: gear.inserter_x,
            gear_inserter_y: gear.inserter_y,
            gear_inserter_direction: gear.input_direction.to_string(),
            copper_from_x: params.copper_from_x,
            copper_from_y: params.copper_from_y,
            copper_pickup_x: copper.belt_x,
            copper_pickup_y: copper.belt_y,
            copper_inserter_x: copper.inserter_x,
            copper_inserter_y: copper.inserter_y,
            copper_inserter_direction: copper.input_direction.to_string(),
            science_drop_x: output.belt_x,
            science_drop_y: output.belt_y,
            science_to_x: lab_feed.upstream_x,
            science_to_y: lab_feed.upstream_y,
            output_inserter_x: output.inserter_x,
            output_inserter_y: output.inserter_y,
            output_inserter_direction: output.output_direction.to_string(),
            lab_from_x: lab_feed.upstream_x,
            lab_from_y: lab_feed.upstream_y,
            lab_pickup_x: lab_feed.belt_x,
            lab_pickup_y: lab_feed.belt_y,
            lab_inserter_x: lab_feed.inserter_x,
            lab_inserter_y: lab_feed.inserter_y,
            lab_inserter_direction: lab_feed.input_direction.to_string(),
            belt_type: params.belt_type,
            search_radius: params.search_radius,
            dry_run: true,
            respect_zones: params.respect_zones,
            allow_underground: params.allow_underground,
            extend_existing: params.extend_existing,
            verify_radius: params.verify_radius,
        };

        let gear_route_params = RouteBeltParams {
            from_x: build_args.gear_from_x,
            from_y: build_args.gear_from_y,
            to_x: build_args.gear_pickup_x,
            to_y: build_args.gear_pickup_y,
            belt_type: build_args.belt_type.clone(),
            search_radius: build_args.search_radius,
            dry_run: true,
            respect_zones: build_args.respect_zones,
            allow_underground: build_args.allow_underground,
            extend_existing: build_args.extend_existing,
        };
        let copper_route_params = RouteBeltParams {
            from_x: build_args.copper_from_x,
            from_y: build_args.copper_from_y,
            to_x: build_args.copper_pickup_x,
            to_y: build_args.copper_pickup_y,
            belt_type: build_args.belt_type.clone(),
            search_radius: build_args.search_radius,
            dry_run: true,
            respect_zones: build_args.respect_zones,
            allow_underground: build_args.allow_underground,
            extend_existing: build_args.extend_existing,
        };
        let output_route_params = RouteBeltParams {
            from_x: build_args.science_drop_x,
            from_y: build_args.science_drop_y,
            to_x: build_args.science_to_x,
            to_y: build_args.science_to_y,
            belt_type: build_args.belt_type.clone(),
            search_radius: build_args.search_radius,
            dry_run: true,
            respect_zones: build_args.respect_zones,
            allow_underground: build_args.allow_underground,
            extend_existing: build_args.extend_existing,
        };
        let lab_route_params = RouteBeltParams {
            from_x: build_args.lab_from_x,
            from_y: build_args.lab_from_y,
            to_x: build_args.lab_pickup_x,
            to_y: build_args.lab_pickup_y,
            belt_type: build_args.belt_type.clone(),
            search_radius: build_args.search_radius,
            dry_run: true,
            respect_zones: build_args.respect_zones,
            allow_underground: build_args.allow_underground,
            extend_existing: build_args.extend_existing,
        };

        let gear_route = self.route_belt_core(&mut client, &gear_route_params).await;
        let copper_route = self
            .route_belt_core(&mut client, &copper_route_params)
            .await;
        let output_route = self
            .route_belt_core(&mut client, &output_route_params)
            .await;
        let lab_route = self.route_belt_core(&mut client, &lab_route_params).await;

        let route_value = |route: Result<serde_json::Value, String>| match route {
            Ok(value) => value,
            Err(error) => serde_json::json!({
                "success": false,
                "dry_run": true,
                "error": error,
            }),
        };
        let routes = serde_json::json!({
            "iron_gear_wheel": route_value(gear_route),
            "copper_plate": route_value(copper_route),
            "automation_science_pack": route_value(output_route),
            "lab_feed": route_value(lab_route),
        });
        let inventory = client.character_inventory().await.ok();
        let available_items: BTreeMap<String, u32> = inventory
            .as_ref()
            .map(|inventory| {
                inventory
                    .items
                    .iter()
                    .map(|item| (item.name.clone(), item.count))
                    .collect()
            })
            .unwrap_or_default();
        let additional_items = BTreeMap::from([("inserter".to_string(), 4)]);
        let mut allowed_shared_tiles = HashSet::new();
        if build_args.science_to_x == build_args.lab_from_x
            && build_args.science_to_y == build_args.lab_from_y
        {
            allowed_shared_tiles.insert(GridPos::new(
                build_args.science_to_x,
                build_args.science_to_y,
            ));
        }
        let route_reports = [
            ("iron_gear_wheel", &routes["iron_gear_wheel"]),
            ("copper_plate", &routes["copper_plate"]),
            (
                "automation_science_pack",
                &routes["automation_science_pack"],
            ),
            ("lab_feed", &routes["lab_feed"]),
        ];
        let compound_preflight = compound_route_preflight(
            &route_reports,
            &available_items,
            &additional_items,
            &build_args.belt_type,
            &allowed_shared_tiles,
            &[
                (
                    "gear_inserter",
                    GridPos::from_position(&Position::new(gear.inserter_x, gear.inserter_y)),
                ),
                (
                    "copper_inserter",
                    GridPos::from_position(&Position::new(copper.inserter_x, copper.inserter_y)),
                ),
                (
                    "output_inserter",
                    GridPos::from_position(&Position::new(output.inserter_x, output.inserter_y)),
                ),
                (
                    "lab_inserter",
                    GridPos::from_position(&Position::new(
                        lab_feed.inserter_x,
                        lab_feed.inserter_y,
                    )),
                ),
            ],
        );
        let placement_preflight = serde_json::json!({
            "gear_inserter": client.check_entity_placement(
                "inserter",
                Position::new(gear.inserter_x, gear.inserter_y),
                Direction::parse(gear.input_direction).unwrap_or(Direction::North),
            ).await.ok(),
            "copper_inserter": client.check_entity_placement(
                "inserter",
                Position::new(copper.inserter_x, copper.inserter_y),
                Direction::parse(copper.input_direction).unwrap_or(Direction::North),
            ).await.ok(),
            "output_inserter": client.check_entity_placement(
                "inserter",
                Position::new(output.inserter_x, output.inserter_y),
                Direction::parse(output.output_direction).unwrap_or(Direction::North),
            ).await.ok(),
            "lab_inserter": client.check_entity_placement(
                "inserter",
                Position::new(lab_feed.inserter_x, lab_feed.inserter_y),
                Direction::parse(lab_feed.input_direction).unwrap_or(Direction::North),
            ).await.ok(),
        });
        let placements_ready = placement_preflight
            .as_object()
            .is_some_and(|checks| checks.values().all(placement_report_allowed));
        let route_success = compound_preflight["ready"].as_bool() == Some(true)
            && placements_ready
            && inventory.is_some();

        let mut execute_args =
            serde_json::to_value(&build_args).unwrap_or_else(|_| serde_json::json!({}));
        if let Some(object) = execute_args.as_object_mut() {
            object.insert("dry_run".to_string(), serde_json::json!(false));
        }

        let result = serde_json::json!({
            "success": route_success,
            "dry_run": true,
            "recipe": "automation-science-pack",
            "assembler": {
                "unit_number": assembler.unit_number,
                "name": assembler.name,
                "position": assembler.position,
            },
            "lab": {
                "unit_number": lab.unit_number,
                "name": lab.name,
                "position": lab.position,
            },
            "sides": {
                "gear": gear,
                "copper": copper,
                "science_output": output,
                "lab_feed": lab_feed,
            },
            "routes": routes,
            "compound_preflight": compound_preflight,
            "placement_preflight": placement_preflight,
            "ready_to_call": {
                "tool": "build_automation_science",
                "dry_run_args": build_args,
                "execute_args": execute_args,
            },
            "guidance": "If success is true, call build_automation_science with ready_to_call.execute_args. If false, choose different sides or source belt tiles and call plan_automation_science again; do not hand-craft or hand-feed automation science packs.",
        });
        let msg = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(msg).await
    }

    /// Build a complete automation-science assembler-to-lab cell.
    #[tool(
        description = "Execute a complete durable automation-science-pack cell: set an assembler to automation-science-pack, route iron-gear-wheel and copper-plate belts into it, route science output toward a lab, place all inserters, then verify assembler/research state. Prefer this over hand-crafting or hand-feeding red science. Use dry_run=true during planner turns."
    )]
    async fn build_automation_science(
        &self,
        Parameters(params): Parameters<BuildAutomationScienceParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let assembler = match client.get_entity(params.assembler_unit_number).await {
            Ok(entity) => entity,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        if !assembler.name.starts_with("assembling-machine") {
            return self
                .with_player_messages(format!(
                    "Error: unit {} is {}, not an assembling machine",
                    params.assembler_unit_number, assembler.name
                ))
                .await;
        }

        let lab = match client.get_entity(params.lab_unit_number).await {
            Ok(entity) => entity,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        if lab.name != "lab" {
            return self
                .with_player_messages(format!(
                    "Error: unit {} is {}, not lab",
                    params.lab_unit_number, lab.name
                ))
                .await;
        }

        let gear_direction = match Direction::parse(&params.gear_inserter_direction) {
            Some(direction) => direction,
            None => {
                return self
                    .with_player_messages(invalid_direction_failure(
                        "gear_inserter_direction",
                        &params.gear_inserter_direction,
                    ))
                    .await;
            }
        };
        let copper_direction = match Direction::parse(&params.copper_inserter_direction) {
            Some(direction) => direction,
            None => {
                return self
                    .with_player_messages(invalid_direction_failure(
                        "copper_inserter_direction",
                        &params.copper_inserter_direction,
                    ))
                    .await;
            }
        };
        let output_direction = match Direction::parse(&params.output_inserter_direction) {
            Some(direction) => direction,
            None => {
                return self
                    .with_player_messages(invalid_direction_failure(
                        "output_inserter_direction",
                        &params.output_inserter_direction,
                    ))
                    .await;
            }
        };
        let lab_direction = match Direction::parse(&params.lab_inserter_direction) {
            Some(direction) => direction,
            None => {
                return self
                    .with_player_messages(invalid_direction_failure(
                        "lab_inserter_direction",
                        &params.lab_inserter_direction,
                    ))
                    .await;
            }
        };

        let gear_route_params = RouteBeltParams {
            from_x: params.gear_from_x,
            from_y: params.gear_from_y,
            to_x: params.gear_pickup_x,
            to_y: params.gear_pickup_y,
            belt_type: params.belt_type.clone(),
            search_radius: params.search_radius,
            dry_run: true,
            respect_zones: params.respect_zones,
            allow_underground: params.allow_underground,
            extend_existing: params.extend_existing,
        };
        let copper_route_params = RouteBeltParams {
            from_x: params.copper_from_x,
            from_y: params.copper_from_y,
            to_x: params.copper_pickup_x,
            to_y: params.copper_pickup_y,
            belt_type: params.belt_type.clone(),
            search_radius: params.search_radius,
            dry_run: true,
            respect_zones: params.respect_zones,
            allow_underground: params.allow_underground,
            extend_existing: params.extend_existing,
        };
        let output_route_params = RouteBeltParams {
            from_x: params.science_drop_x,
            from_y: params.science_drop_y,
            to_x: params.science_to_x,
            to_y: params.science_to_y,
            belt_type: params.belt_type.clone(),
            search_radius: params.search_radius,
            dry_run: true,
            respect_zones: params.respect_zones,
            allow_underground: params.allow_underground,
            extend_existing: params.extend_existing,
        };
        let lab_route_params = RouteBeltParams {
            from_x: params.lab_from_x,
            from_y: params.lab_from_y,
            to_x: params.lab_pickup_x,
            to_y: params.lab_pickup_y,
            belt_type: params.belt_type.clone(),
            search_radius: params.search_radius,
            dry_run: true,
            respect_zones: params.respect_zones,
            allow_underground: params.allow_underground,
            extend_existing: params.extend_existing,
        };

        let gear_route = match self.route_belt_core(&mut client, &gear_route_params).await {
            Ok(report) => report,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let copper_route = match self
            .route_belt_core(&mut client, &copper_route_params)
            .await
        {
            Ok(report) => report,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let output_route = match self
            .route_belt_core(&mut client, &output_route_params)
            .await
        {
            Ok(report) => report,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let lab_route = match self.route_belt_core(&mut client, &lab_route_params).await {
            Ok(report) => report,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let inventory = match client.character_inventory().await {
            Ok(inventory) => inventory,
            Err(error) => {
                return self
                    .with_player_messages(format!("Error: checking compound materials: {error}"))
                    .await
            }
        };
        let available_items: BTreeMap<String, u32> = inventory
            .items
            .iter()
            .map(|item| (item.name.clone(), item.count))
            .collect();
        let additional_items = BTreeMap::from([("inserter".to_string(), 4)]);
        let mut allowed_shared_tiles = HashSet::new();
        if params.science_to_x == params.lab_from_x && params.science_to_y == params.lab_from_y {
            allowed_shared_tiles.insert(GridPos::new(params.science_to_x, params.science_to_y));
        }
        let compound_preflight = compound_route_preflight(
            &[
                ("iron_gear_wheel", &gear_route),
                ("copper_plate", &copper_route),
                ("automation_science_pack", &output_route),
                ("lab_feed", &lab_route),
            ],
            &available_items,
            &additional_items,
            &params.belt_type,
            &allowed_shared_tiles,
            &[
                (
                    "gear_inserter",
                    GridPos::from_position(&Position::new(
                        params.gear_inserter_x,
                        params.gear_inserter_y,
                    )),
                ),
                (
                    "copper_inserter",
                    GridPos::from_position(&Position::new(
                        params.copper_inserter_x,
                        params.copper_inserter_y,
                    )),
                ),
                (
                    "output_inserter",
                    GridPos::from_position(&Position::new(
                        params.output_inserter_x,
                        params.output_inserter_y,
                    )),
                ),
                (
                    "lab_inserter",
                    GridPos::from_position(&Position::new(
                        params.lab_inserter_x,
                        params.lab_inserter_y,
                    )),
                ),
            ],
        );
        let gear_placement = client
            .check_entity_placement(
                "inserter",
                Position::new(params.gear_inserter_x, params.gear_inserter_y),
                gear_direction,
            )
            .await;
        let copper_placement = client
            .check_entity_placement(
                "inserter",
                Position::new(params.copper_inserter_x, params.copper_inserter_y),
                copper_direction,
            )
            .await;
        let output_placement = client
            .check_entity_placement(
                "inserter",
                Position::new(params.output_inserter_x, params.output_inserter_y),
                output_direction,
            )
            .await;
        let lab_placement = client
            .check_entity_placement(
                "inserter",
                Position::new(params.lab_inserter_x, params.lab_inserter_y),
                lab_direction,
            )
            .await;
        let placement_allowed = |check: &anyhow::Result<serde_json::Value>| {
            check.as_ref().ok().is_some_and(placement_report_allowed)
        };
        let placement_preflight = serde_json::json!({
            "gear_inserter": gear_placement.as_ref().map_err(|error| error.to_string()),
            "copper_inserter": copper_placement.as_ref().map_err(|error| error.to_string()),
            "output_inserter": output_placement.as_ref().map_err(|error| error.to_string()),
            "lab_inserter": lab_placement.as_ref().map_err(|error| error.to_string()),
        });
        let preflight_ready = compound_preflight["ready"].as_bool() == Some(true)
            && placement_allowed(&gear_placement)
            && placement_allowed(&copper_placement)
            && placement_allowed(&output_placement)
            && placement_allowed(&lab_placement);

        let recipe_args = serde_json::json!({
            "unit_number": params.assembler_unit_number,
            "recipe": "automation-science-pack",
        });
        let gear_inserter_args = serde_json::json!({
            "entity_name": "inserter",
            "x": params.gear_inserter_x,
            "y": params.gear_inserter_y,
            "direction": params.gear_inserter_direction,
        });
        let copper_inserter_args = serde_json::json!({
            "entity_name": "inserter",
            "x": params.copper_inserter_x,
            "y": params.copper_inserter_y,
            "direction": params.copper_inserter_direction,
        });
        let output_inserter_args = serde_json::json!({
            "entity_name": "inserter",
            "x": params.output_inserter_x,
            "y": params.output_inserter_y,
            "direction": params.output_inserter_direction,
        });
        let lab_inserter_args = serde_json::json!({
            "entity_name": "inserter",
            "x": params.lab_inserter_x,
            "y": params.lab_inserter_y,
            "direction": params.lab_inserter_direction,
        });

        if params.dry_run {
            let result = serde_json::json!({
                "success": preflight_ready,
                "dry_run": true,
                "recipe": "automation-science-pack",
                "assembler": {
                    "unit_number": assembler.unit_number,
                    "name": assembler.name,
                    "position": assembler.position,
                },
                "lab": {
                    "unit_number": lab.unit_number,
                    "name": lab.name,
                    "position": lab.position,
                },
                "routes": {
                    "iron_gear_wheel": gear_route,
                    "copper_plate": copper_route,
                    "automation_science_pack": output_route,
                    "lab_feed": lab_route,
                },
                "compound_preflight": compound_preflight,
                "placement_preflight": placement_preflight,
                "steps": [{
                    "tool": "set_recipe",
                    "args": recipe_args,
                }, {
                    "tool": "route_belt",
                    "item": "iron-gear-wheel",
                    "args": gear_route_params,
                }, {
                    "tool": "place_entity",
                    "item": "iron-gear-wheel",
                    "args": gear_inserter_args,
                }, {
                    "tool": "route_belt",
                    "item": "copper-plate",
                    "args": copper_route_params,
                }, {
                    "tool": "place_entity",
                    "item": "copper-plate",
                    "args": copper_inserter_args,
                }, {
                    "tool": "route_belt",
                    "item": "automation-science-pack",
                    "args": output_route_params,
                }, {
                    "tool": "place_entity",
                    "item": "automation-science-pack-output",
                    "args": output_inserter_args,
                }, {
                    "tool": "route_belt",
                    "item": "automation-science-pack-to-lab",
                    "args": lab_route_params,
                }, {
                    "tool": "place_entity",
                    "item": "automation-science-pack-to-lab",
                    "args": lab_inserter_args,
                }, {
                    "tool": "verify_production",
                    "args": {
                        "x": assembler.position.x,
                        "y": assembler.position.y,
                        "radius": params.verify_radius,
                    },
                }, {
                    "tool": "get_research_status",
                    "args": {},
                }],
                "guidance": "If all route materials_sufficient values are true and inserter placements are clear, call build_automation_science again with dry_run=false. Do not hand-craft or hand-feed automation science packs.",
            });
            let msg =
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
            return self.with_player_messages(msg).await;
        }

        if !preflight_ready {
            let result = serde_json::json!({
                "success": false,
                "dry_run": false,
                "error_kind": "compound_preflight_failed",
                "error": "The complete science cell failed shared material, route, or inserter preflight. Nothing was placed.",
                "routes": {
                    "iron_gear_wheel": gear_route,
                    "copper_plate": copper_route,
                    "automation_science_pack": output_route,
                    "lab_feed": lab_route,
                },
                "compound_preflight": compound_preflight,
                "placement_preflight": placement_preflight,
            });
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                .await;
        }

        let previous_recipe = match client.get_entity_recipe(params.assembler_unit_number).await {
            Ok(recipe) => recipe,
            Err(error) => {
                return self
                    .with_player_messages(format!("Error: reading previous recipe: {error}"))
                    .await
            }
        };

        let mut gear_execute = gear_route_params.clone();
        gear_execute.dry_run = false;
        let mut copper_execute = copper_route_params.clone();
        copper_execute.dry_run = false;
        let mut output_execute = output_route_params.clone();
        output_execute.dry_run = false;
        let mut lab_execute = lab_route_params.clone();
        lab_execute.dry_run = false;

        let mut transaction_units = Vec::new();
        let mut executed_routes = BTreeMap::new();
        for (label, route_params) in [
            ("iron_gear_wheel", gear_execute),
            ("copper_plate", copper_execute),
            ("automation_science_pack", output_execute),
            ("lab_feed", lab_execute),
        ] {
            let report = match self.route_belt_core(&mut client, &route_params).await {
                Ok(report) => report,
                Err(error) => {
                    let rollback = rollback_exact_units(&mut client, &transaction_units).await;
                    let result = serde_json::json!({
                        "success": false,
                        "error_kind": "route_execution_error",
                        "failed_route": label,
                        "error": error,
                        "routes": executed_routes,
                        "rollback": rollback,
                    });
                    return self
                        .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                        .await;
                }
            };
            if !report_success(&report) {
                let rollback = rollback_exact_units(&mut client, &transaction_units).await;
                let result = serde_json::json!({
                    "success": false,
                    "error_kind": "route_execution_failed",
                    "failed_route": label,
                    "route": report,
                    "routes": executed_routes,
                    "rollback": rollback,
                });
                return self
                    .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                    .await;
            }
            transaction_units.extend(route_report_placed_units(&report));
            executed_routes.insert(label.to_string(), report);
        }

        let gear_route = executed_routes.remove("iron_gear_wheel").unwrap();
        let copper_route = executed_routes.remove("copper_plate").unwrap();
        let output_route = executed_routes.remove("automation_science_pack").unwrap();
        let lab_route = executed_routes.remove("lab_feed").unwrap();

        let gear_inserter = match client
            .place_entity(
                "inserter",
                Position::new(params.gear_inserter_x, params.gear_inserter_y),
                gear_direction,
            )
            .await
        {
            Ok(entity) => entity,
            Err(error) => {
                let rollback = rollback_exact_units(&mut client, &transaction_units).await;
                let result = serde_json::json!({
                    "success": false,
                    "error_kind": "inserter_execution_failed",
                    "failed_inserter": "iron_gear_wheel",
                    "error": error.to_string(),
                    "rollback": rollback,
                });
                return self
                    .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                    .await;
            }
        };
        if let Some(unit_number) = gear_inserter.unit_number {
            transaction_units.push(unit_number);
        }
        let copper_inserter = match client
            .place_entity(
                "inserter",
                Position::new(params.copper_inserter_x, params.copper_inserter_y),
                copper_direction,
            )
            .await
        {
            Ok(entity) => entity,
            Err(error) => {
                let rollback = rollback_exact_units(&mut client, &transaction_units).await;
                let result = serde_json::json!({
                    "success": false,
                    "error_kind": "inserter_execution_failed",
                    "failed_inserter": "copper_plate",
                    "error": error.to_string(),
                    "rollback": rollback,
                });
                return self
                    .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                    .await;
            }
        };
        if let Some(unit_number) = copper_inserter.unit_number {
            transaction_units.push(unit_number);
        }
        let output_inserter = match client
            .place_entity(
                "inserter",
                Position::new(params.output_inserter_x, params.output_inserter_y),
                output_direction,
            )
            .await
        {
            Ok(entity) => entity,
            Err(error) => {
                let rollback = rollback_exact_units(&mut client, &transaction_units).await;
                let result = serde_json::json!({
                    "success": false,
                    "error_kind": "inserter_execution_failed",
                    "failed_inserter": "automation_science_pack_output",
                    "error": error.to_string(),
                    "rollback": rollback,
                });
                return self
                    .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                    .await;
            }
        };
        if let Some(unit_number) = output_inserter.unit_number {
            transaction_units.push(unit_number);
        }
        let lab_inserter = match client
            .place_entity(
                "inserter",
                Position::new(params.lab_inserter_x, params.lab_inserter_y),
                lab_direction,
            )
            .await
        {
            Ok(entity) => entity,
            Err(error) => {
                let rollback = rollback_exact_units(&mut client, &transaction_units).await;
                let result = serde_json::json!({
                    "success": false,
                    "error_kind": "inserter_execution_failed",
                    "failed_inserter": "lab_feed",
                    "error": error.to_string(),
                    "rollback": rollback,
                });
                return self
                    .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                    .await;
            }
        };
        if let Some(unit_number) = lab_inserter.unit_number {
            transaction_units.push(unit_number);
        }

        let set_recipe = client
            .set_recipe(params.assembler_unit_number, "automation-science-pack")
            .await;
        if let Err(error) = &set_recipe {
            let rollback = rollback_controller_transaction(
                &mut client,
                &transaction_units,
                &[],
                Some((params.assembler_unit_number, previous_recipe.clone())),
            )
            .await;
            let result = serde_json::json!({
                "success": false,
                "error_kind": "set_recipe_failed",
                "error": error.to_string(),
                "rollback": rollback,
            });
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                .await;
        }
        let recipe_report = serde_json::json!({
            "tool": "set_recipe",
            "args": recipe_args,
            "success": true,
            "error": serde_json::Value::Null,
        });

        let gear_inserter_unit = gear_inserter.unit_number;
        let copper_inserter_unit = copper_inserter.unit_number;
        let output_inserter_unit = output_inserter.unit_number;
        let lab_inserter_unit = lab_inserter.unit_number;

        let verify_radius = params.verify_radius.clamp(1, 50) as f64;
        let verify_area = Area::new(
            assembler.position.x.min(lab.position.x) - verify_radius,
            assembler.position.y.min(lab.position.y) - verify_radius,
            assembler.position.x.max(lab.position.x) + verify_radius,
            assembler.position.y.max(lab.position.y) + verify_radius,
        );
        let verification = match observe_production(&mut client, verify_area, 180).await {
            Ok(verification) => verification,
            Err(error) => serde_json::json!({"success": false, "error": error.to_string()}),
        };
        let placed_units: Vec<u32> = [
            gear_inserter_unit,
            copper_inserter_unit,
            output_inserter_unit,
            lab_inserter_unit,
        ]
        .into_iter()
        .flatten()
        .collect();
        let mut inserters_exist = placed_units.len() == 4;
        for unit_number in &placed_units {
            inserters_exist &= client.get_entity(*unit_number).await.is_ok();
        }

        let assembler_working = production_unit_verified(&verification, assembler.unit_number);
        let lab_working = production_unit_verified(&verification, lab.unit_number);

        let agent_id = client.agent_id().as_str().to_string();
        let research_status = match client
            .call_remote("get_research_status", &[serde_json::json!(agent_id)])
            .await
        {
            Ok(response) => match serde_json::from_str::<serde_json::Value>(&response) {
                Ok(report) => serde_json::json!({
                    "success": true,
                    "report": report,
                }),
                Err(e) => serde_json::json!({
                    "success": false,
                    "error": e.to_string(),
                    "raw": response,
                }),
            },
            Err(e) => serde_json::json!({
                "success": false,
                "error": e.to_string(),
            }),
        };

        let route_ok = |value: &serde_json::Value| {
            value
                .get("success")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        };
        let routes_success = route_ok(&gear_route)
            && route_ok(&copper_route)
            && route_ok(&output_route)
            && route_ok(&lab_route);
        let repair_hint = automation_repair_hint(
            "build_automation_science",
            "complete automation-science assembler-to-lab cell",
            verification.get("error").is_none(),
            &verification,
            &[],
            Some(routes_success),
        );
        let success = routes_success
            && assembler_working
            && lab_working
            && inserters_exist
            && research_status
                .get("success")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
        if !success {
            let rollback = rollback_controller_transaction(
                &mut client,
                &transaction_units,
                &[],
                Some((params.assembler_unit_number, previous_recipe)),
            )
            .await;
            let result = serde_json::json!({
                "success": false,
                "error_kind": "verification_failed",
                "error": "Automation-science cell did not prove both pack production and lab consumption; every route, inserter, and recipe change was rolled back.",
                "verification": verification,
                "research_status": research_status,
                "rollback": rollback,
                "repair_hint": repair_hint,
            });
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap())
                .await;
        }

        let result = serde_json::json!({
            "success": success,
            "placement_success": true,
            "dry_run": false,
            "recipe": "automation-science-pack",
            "assembler": {
                "unit_number": assembler.unit_number,
                "name": assembler.name,
                "position": assembler.position,
            },
            "lab": {
                "unit_number": lab.unit_number,
                "name": lab.name,
                "position": lab.position,
            },
            "recipe_set": recipe_report,
            "routes": {
                "iron_gear_wheel": gear_route,
                "copper_plate": copper_route,
                "automation_science_pack": output_route,
                "lab_feed": lab_route,
            },
            "compound_preflight": compound_preflight,
            "placement_preflight": placement_preflight,
            "inserters": {
                "iron_gear_wheel": {
                    "tool": "place_entity",
                    "args": gear_inserter_args,
                    "success": true,
                    "unit_number": gear_inserter_unit,
                    "error": serde_json::Value::Null,
                },
                "copper_plate": {
                    "tool": "place_entity",
                    "args": copper_inserter_args,
                    "success": true,
                    "unit_number": copper_inserter_unit,
                    "error": serde_json::Value::Null,
                },
                "automation_science_pack_output": {
                    "tool": "place_entity",
                    "args": output_inserter_args,
                    "success": true,
                    "unit_number": output_inserter_unit,
                    "error": serde_json::Value::Null,
                },
                "lab_feed": {
                    "tool": "place_entity",
                    "args": lab_inserter_args,
                    "success": true,
                    "unit_number": lab_inserter_unit,
                    "error": serde_json::Value::Null,
                },
            },
            "automation_verified": {
                "success": true,
                "assembler_working": assembler_working,
                "lab_working": lab_working,
                "inserters_exist": inserters_exist,
            },
            "verification": verification,
            "research_status": research_status,
            "repair_hint": repair_hint,
            "guidance": "If success is true, automation science production and lab delivery are built. Keep research running from this belt instead of feeding packs from inventory.",
        });
        let msg = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(msg).await
    }

    /// Get belt contents with lane separation.
    #[tool(
        description = "Get items on transport belts with left/right lane separation. \
        Shows what items are on each lane of each belt, useful for diagnosing sushi belts or lane balancing issues."
    )]
    async fn get_belt_lane_contents(
        &self,
        Parameters(params): Parameters<BeltLaneContentsParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let area = Area {
            left_top: Position::new(
                params.x as f64 - params.radius as f64,
                params.y as f64 - params.radius as f64,
            ),
            right_bottom: Position::new(
                params.x as f64 + params.radius as f64,
                params.y as f64 + params.radius as f64,
            ),
        };

        let result = match client.get_belt_lane_contents(area).await {
            Ok(r) => serde_json::to_string_pretty(&r).unwrap_or_else(|e| format!("Error: {}", e)),
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Detect sushi belts (mixed items on same lane).
    #[tool(
        description = "Detect sushi belts - belts with multiple item types mixed on the same lane. \
        Also identifies lane-separated belts (different items on left vs right lane) and pure belts (single item type). \
        Detects circular loop networks common in sushi setups."
    )]
    async fn detect_sushi_belts(
        &self,
        Parameters(params): Parameters<SushiDetectParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let area = Area {
            left_top: Position::new(
                params.x as f64 - params.radius as f64,
                params.y as f64 - params.radius as f64,
            ),
            right_bottom: Position::new(
                params.x as f64 + params.radius as f64,
                params.y as f64 + params.radius as f64,
            ),
        };

        // Get belt lane contents
        let lane_contents = match client.get_belt_lane_contents(area).await {
            Ok(r) => r,
            Err(e) => {
                return self
                    .with_player_messages(format!("Error: getting belt contents: {}", e))
                    .await
            }
        };

        // Get entities for belt graph
        let entities = match client.find_entities(area, None, None).await {
            Ok(e) => e,
            Err(e) => {
                return self
                    .with_player_messages(format!("Error: getting entities: {}", e))
                    .await
            }
        };

        let graph = BeltGraph::from_entities(&entities);
        let result = detect_sushi_belts(&lane_contents, &graph);

        let result_str =
            serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(result_str).await
    }

    /// Trace upstream sources for a belt.
    #[tool(
        description = "Trace upstream to find all sources (inserters, drills, other belts) that can feed items onto a belt. \
        Shows which lane each source feeds and detects circular loops. Useful for debugging why certain items appear on a belt."
    )]
    async fn trace_belt_sources(
        &self,
        Parameters(params): Parameters<BeltSourcesParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let area = Area {
            left_top: Position::new(
                params.x as f64 - params.radius as f64,
                params.y as f64 - params.radius as f64,
            ),
            right_bottom: Position::new(
                params.x as f64 + params.radius as f64,
                params.y as f64 + params.radius as f64,
            ),
        };

        let entities = match client.find_entities(area, None, None).await {
            Ok(e) => e,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let graph = BeltGraph::from_entities(&entities);
        let origin = TilePos::new(params.x, params.y);

        let result = match trace_belt_sources(origin, &graph, &entities) {
            Some(r) => serde_json::to_string_pretty(&r).unwrap_or_else(|e| format!("Error: {}", e)),
            None => format!("No belt found at position ({}, {})", params.x, params.y),
        };
        self.with_player_messages(result).await
    }

    // --- Research Tools ---

    /// Get research status.
    #[tool(
        description = "Get overall research status including current research progress, researched count, and research queue. \
        Also shows lab count, power status, and science packs currently in labs. \
        IMPORTANT: Research requires labs with power and science packs inserted - this tool shows if you're set up correctly."
    )]
    async fn get_research_status(&self) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let agent_id = client.agent_id().as_str().to_string();
        let result = match client
            .call_remote("get_research_status", &[serde_json::json!(agent_id)])
            .await
        {
            Ok(result) => result,
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Get available research.
    #[tool(
        description = "Get technologies that can be researched now (enabled, prerequisites met, not yet researched). \
        Returns name, ingredients (science packs needed), effects, and whether you're ready to research. \
        Shows 'ready' or 'blocked' status with specific blockers (no labs, no power, missing science packs). \
        IMPORTANT: To actually research you need: 1) Labs built, 2) Labs powered, 3) Science packs in labs."
    )]
    async fn get_available_research(&self) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client
            .call_remote(
                "get_available_research",
                &[serde_json::json!(client.agent_id().as_str())],
            )
            .await
        {
            Ok(result) => result,
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Feed science packs from the agent inventory into a lab.
    #[tool(
        description = "Dry-run or transfer 1-200 packs into an exact lab for bootstrap/recovery. Preserves identity and items; then automate with build_automation_science/build_lab_feed."
    )]
    async fn feed_lab_from_inventory(
        &self,
        Parameters(params): Parameters<FeedLabFromInventoryParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client
            .feed_lab_from_inventory(
                params.lab_unit_number,
                &params.science_pack,
                params.count,
                params.dry_run,
            )
            .await
        {
            Ok(value) => {
                serde_json::to_string_pretty(&value).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Start researching a technology.
    #[tool(
        description = "Queue a technology for research. Uses proper research queue (not cheating). \
        REQUIREMENTS: 1) Technology enabled with prerequisites met, 2) At least one lab built, \
        3) Lab connected to power, 4) Required science packs inserted into lab. \
        Will return specific error if any requirement is missing with guidance on what to do."
    )]
    async fn start_research(&self, Parameters(params): Parameters<StartResearchParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let agent_id = client.agent_id().as_str().to_string();
        let result = match client
            .call_remote(
                "start_research",
                &[
                    serde_json::json!(params.technology),
                    serde_json::json!(agent_id),
                ],
            )
            .await
        {
            Ok(result) => result,
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    // --- Power Network Tools ---

    /// Get power network status at a location.
    #[tool(
        description = "Get power network status near a position. Returns network ID, connected pole info, \
        and power flow statistics (production/consumption)."
    )]
    async fn get_power_status(&self, Parameters(params): Parameters<PowerStatusParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let agent_id = client.agent_id().as_str().to_string();
        let result = match client
            .call_remote(
                "get_power_status",
                &[
                    serde_json::json!(params.x),
                    serde_json::json!(params.y),
                    serde_json::json!(params.radius),
                    serde_json::json!(agent_id),
                ],
            )
            .await
        {
            Ok(result) => result,
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Get all power networks in an area.
    #[tool(
        description = "Find all electric power networks in an area. Returns network IDs and pole counts. \
        Useful for understanding power grid layout."
    )]
    async fn get_power_networks(&self, Parameters(params): Parameters<AreaParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let agent_id = client.agent_id().as_str().to_string();
        let result = match client
            .call_remote(
                "get_power_networks",
                &[
                    serde_json::json!(params.x),
                    serde_json::json!(params.y),
                    serde_json::json!(params.radius),
                    serde_json::json!(agent_id),
                ],
            )
            .await
        {
            Ok(result) => result,
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Find power issues - entities without power or with low power.
    #[tool(
        description = "Find actionable power problems: entities with no_power or low_power status, \
        their positions, and suggested fixes (nearest pole location or need for more generators). \
        Use this to diagnose and fix power grid issues."
    )]
    async fn find_power_issues(
        &self,
        Parameters(params): Parameters<FindPowerIssuesParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let agent_id = client.agent_id().as_str().to_string();
        let result = match client
            .call_remote(
                "find_power_issues",
                &[
                    serde_json::json!(params.x),
                    serde_json::json!(params.y),
                    serde_json::json!(params.radius),
                    serde_json::json!(agent_id),
                ],
            )
            .await
        {
            Ok(result) => result,
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Diagnose steam-power fluid and electric connectivity.
    #[tool(
        description = "Diagnose a steam-power build before modifying it. Reports offshore pumps, boilers, steam engines, pipes, electric poles, fluidbox/segment connectivity, fuel, statuses, and suggested actions. Use this before placing/removing fluid entities or rebuilding power."
    )]
    async fn diagnose_steam_power(&self, Parameters(params): Parameters<AreaParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let agent_id = client.agent_id().as_str().to_string();
        let result = match client
            .call_remote(
                "diagnose_steam_power",
                &[
                    serde_json::json!(params.x),
                    serde_json::json!(params.y),
                    serde_json::json!(params.radius),
                    serde_json::json!(agent_id),
                ],
            )
            .await
        {
            Ok(result) => model_safe_json_text(result),
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Plan a checked steam-power layout before placing fluid entities.
    #[tool(
        description = "Plan starter steam power without mutating the game. Given a water bounding box and target position, returns checked offshore-pump, boiler, steam-engine, pipe, fuel, and pole placement arguments plus blockers/missing materials. Use before placing or rebuilding pump/boiler/engine layouts."
    )]
    async fn plan_steam_power(
        &self,
        Parameters(params): Parameters<PlanSteamPowerParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let water_area = Area::new(
            params.water_x1,
            params.water_y1,
            params.water_x2,
            params.water_y2,
        );
        let target = Position::new(params.target_x, params.target_y);
        let result = match client.plan_steam_power(water_area, target).await {
            Ok(value) => {
                serde_json::to_string_pretty(&value).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Plan dry-run repairs for an existing steam-power plant.
    #[tool(
        description = "Plan safe repairs for an existing steam-power plant without mutating the game. Consumes diagnose_steam_power output and returns ordered low-level repair_steps such as insert_items for boiler fuel or place_entity for missing pole reach. Use this before moving or rebuilding pump/boiler/engine layouts."
    )]
    async fn repair_steam_power(
        &self,
        Parameters(params): Parameters<RepairSteamPowerParams>,
    ) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let target = Position::new(params.target_x, params.target_y);
        let result = match client
            .repair_steam_power(params.x, params.y, params.radius, target)
            .await
        {
            Ok(value) => {
                serde_json::to_string_pretty(&value).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Plan dry-run pole placement to extend an existing power grid to a target.
    #[tool(
        description = "Plan how to extend an existing electric pole network to a target without mutating the game. Returns ordered place_entity small-electric-pole steps, missing_items, and blockers. Use before hand-placing long power lines."
    )]
    async fn extend_power_to(&self, Parameters(params): Parameters<ExtendPowerToParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let target = Position::new(params.target_x, params.target_y);
        let result = match client
            .extend_power_to(params.x, params.y, params.radius, target)
            .await
        {
            Ok(value) => {
                serde_json::to_string_pretty(&value).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    // --- Alert Tools ---

    /// Get alerts for urgent conditions.
    #[tool(
        description = "Check for urgent conditions in an area: empty drills, entities without fuel, \
        machines without power/ingredients, nearby enemies. Useful for monitoring factory health."
    )]
    async fn get_alerts(&self, Parameters(params): Parameters<AlertsParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let agent_id = client.agent_id().as_str().to_string();
        let result = match client
            .call_remote(
                "get_alerts",
                &[
                    serde_json::json!(params.x),
                    serde_json::json!(params.y),
                    serde_json::json!(params.radius),
                    serde_json::json!(agent_id),
                ],
            )
            .await
        {
            Ok(result) => result,
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Execute raw Lua command.
    #[tool(
        description = "Execute a raw Lua command. Disabled by default because raw Lua is arbitrary code execution; requires FACTORIOCTL_ALLOW_RAW_LUA=1 for trusted operator use."
    )]
    async fn execute_lua(&self, Parameters(params): Parameters<ExecuteLuaParams>) -> String {
        if let Some(refusal) =
            execute_lua_refusal(std::env::var("FACTORIOCTL_ALLOW_RAW_LUA").ok().as_deref())
        {
            return self.with_player_messages(refusal).await;
        }

        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client.execute_lua(&params.lua).await {
            Ok(result) => result,
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Broadcast a thought or message to the human player.
    #[tool(description = "Broadcast a thought or message to the human player. \
        Displays in-game (console and/or flying text) and speaks via TTS based on config. \
        IMPORTANT: Call this frequently and IN PARALLEL with action tools like walk_to. Good streamers narrate constantly - fill the silence!")]
    async fn broadcast_thought(
        &self,
        Parameters(params): Parameters<BroadcastThoughtParams>,
    ) -> String {
        use std::process::Stdio;
        use tokio::process::Command;

        // Load config for defaults
        let config = factorioctl::config::Config::load().unwrap_or_default();
        let broadcast_config = config.broadcast.unwrap_or_default();

        let mut results = Vec::new();

        // In-game display
        if broadcast_config.console || broadcast_config.flying_text {
            let mut client = match self.connect().await {
                Ok(c) => c,
                Err(e) => return format!("Error: connecting: {}", e),
            };

            if broadcast_config.console {
                if let Err(e) = client
                    .call_remote("broadcast_console", &[serde_json::json!(params.message)])
                    .await
                {
                    results.push(format!("Console error: {}", e));
                } else {
                    results.push("Console: displayed".to_string());
                }
            }

            if broadcast_config.flying_text {
                if let Err(e) = client
                    .call_remote(
                        "broadcast_flying_text",
                        &[serde_json::json!(params.message)],
                    )
                    .await
                {
                    results.push(format!("Flying text error: {}", e));
                } else {
                    results.push("Flying text: displayed".to_string());
                }
            }
        }

        // TTS (spawn in background to not block MCP response)
        if let Some(ref tts_config) = broadcast_config.tts {
            if tts_config.enabled {
                let message = params.message.clone();
                let backend = tts_config.backend.clone();
                let voice = tts_config.voice.clone();
                let rate = tts_config.rate;
                let openai_key = tts_config
                    .openai_api_key
                    .clone()
                    .or_else(|| std::env::var("OPENAI_API_KEY").ok());

                tokio::spawn(async move {
                    match backend.as_str() {
                        "say" => {
                            let mut cmd = Command::new("say");
                            if let Some(ref v) = voice {
                                cmd.arg("-v").arg(v);
                            }
                            if let Some(r) = rate {
                                let wpm = (175.0 * r) as u32;
                                cmd.arg("-r").arg(wpm.to_string());
                            }
                            cmd.arg(&message);
                            let _ = cmd
                                .stdout(Stdio::null())
                                .stderr(Stdio::null())
                                .status()
                                .await;
                        }
                        "openai" => {
                            if let Some(api_key) = openai_key {
                                let voice = voice.as_deref().unwrap_or("nova");
                                let speed = rate.unwrap_or(1.0);
                                let body = serde_json::json!({
                                    "model": "tts-1",
                                    "input": message,
                                    "voice": voice,
                                    "speed": speed
                                });

                                let mut curl = Command::new("curl");
                                curl.args([
                                    "-s",
                                    "-X",
                                    "POST",
                                    "https://api.openai.com/v1/audio/speech",
                                    "-H",
                                    &format!("Authorization: Bearer {}", api_key),
                                    "-H",
                                    "Content-Type: application/json",
                                    "-d",
                                    &body.to_string(),
                                    "--output",
                                    "-",
                                ]);

                                if let Ok(output) = curl.output().await {
                                    if output.status.success() {
                                        let mut play = Command::new("afplay");
                                        play.arg("-").stdin(Stdio::piped());
                                        if let Ok(mut child) = play.spawn() {
                                            if let Some(mut stdin) = child.stdin.take() {
                                                use tokio::io::AsyncWriteExt;
                                                let _ = stdin.write_all(&output.stdout).await;
                                            }
                                            let _ = child.wait().await;
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                });

                results.push("TTS: speaking (background)".to_string());
            }
        }

        let result = if results.is_empty() {
            "No output enabled (check broadcast config in .factorioctl.json)".to_string()
        } else {
            results.join(", ")
        };
        self.with_player_messages(result).await
    }

    // === Zone Management Tools ===

    /// Create a zone to organize factory areas.
    #[tool(description = "Create a named zone to organize your factory. \
        Zones help track what areas are designated for (mining, smelting, assembly, etc.). \
        Zone types: mining, smelting, assembly, power, storage, logistics, reserved, or custom:name")]
    async fn create_zone(&self, Parameters(params): Parameters<CreateZoneParams>) -> String {
        let mut memory = AgentMemory::load();

        // Parse zone type
        let zone_type = parse_zone_type(&params.zone_type);

        let zone = Zone {
            id: params.id.clone(),
            zone_type,
            bounds: Area::new(params.x1, params.y1, params.x2, params.y2),
            description: params.description,
            created_tick: 0, // Could get from game if connected
        };

        memory.set_zone(zone);

        let result = match memory.save() {
            Ok(()) => format!("Zone '{}' created successfully", params.id),
            Err(e) => format!("Error: saving zone: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// List all defined zones.
    #[tool(description = "List all defined zones in agent memory. Optionally filter by zone type.")]
    async fn list_zones(&self, Parameters(params): Parameters<ListZonesParams>) -> String {
        let memory = AgentMemory::load();

        let zones: Vec<serde_json::Value> = memory
            .zones
            .values()
            .filter(|z| {
                params
                    .zone_type
                    .as_ref()
                    .is_none_or(|t| z.zone_type.to_string() == *t)
            })
            .map(|z| {
                serde_json::json!({
                    "id": z.id,
                    "zone_type": z.zone_type.to_string(),
                    "bounds": {
                        "x1": z.bounds.left_top.x,
                        "y1": z.bounds.left_top.y,
                        "x2": z.bounds.right_bottom.x,
                        "y2": z.bounds.right_bottom.y
                    },
                    "description": z.description
                })
            })
            .collect();

        let result =
            serde_json::to_string_pretty(&zones).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(result).await
    }

    /// Get details of a specific zone.
    #[tool(description = "Get details of a specific zone by ID.")]
    async fn get_zone(&self, Parameters(params): Parameters<GetZoneParams>) -> String {
        let memory = AgentMemory::load();

        let result = match memory.get_zone(&params.id) {
            Some(z) => serde_json::to_string_pretty(&serde_json::json!({
                "id": z.id,
                "zone_type": z.zone_type.to_string(),
                "bounds": {
                    "x1": z.bounds.left_top.x,
                    "y1": z.bounds.left_top.y,
                    "x2": z.bounds.right_bottom.x,
                    "y2": z.bounds.right_bottom.y
                },
                "description": z.description,
                "allowed_entities": z.zone_type.allowed_entities()
            }))
            .unwrap_or_else(|e| format!("Error: {}", e)),
            None => format!("Zone '{}' not found", params.id),
        };
        self.with_player_messages(result).await
    }

    /// Update an existing zone.
    #[tool(description = "Update an existing zone's properties (type, bounds, description).")]
    async fn update_zone(&self, Parameters(params): Parameters<UpdateZoneParams>) -> String {
        let mut memory = AgentMemory::load();

        let result = match memory.zones.get_mut(&params.id) {
            Some(zone) => {
                if let Some(ref t) = params.zone_type {
                    zone.zone_type = parse_zone_type(t);
                }
                if let Some(x1) = params.x1 {
                    zone.bounds.left_top.x = x1;
                }
                if let Some(y1) = params.y1 {
                    zone.bounds.left_top.y = y1;
                }
                if let Some(x2) = params.x2 {
                    zone.bounds.right_bottom.x = x2;
                }
                if let Some(y2) = params.y2 {
                    zone.bounds.right_bottom.y = y2;
                }
                if params.description.is_some() {
                    zone.description = params.description.clone();
                }

                match memory.save() {
                    Ok(()) => format!("Zone '{}' updated successfully", params.id),
                    Err(e) => format!("Error: saving: {}", e),
                }
            }
            None => format!("Zone '{}' not found", params.id),
        };
        self.with_player_messages(result).await
    }

    /// Delete a zone.
    #[tool(description = "Delete a zone by ID.")]
    async fn delete_zone(&self, Parameters(params): Parameters<DeleteZoneParams>) -> String {
        let mut memory = AgentMemory::load();

        let result = match memory.remove_zone(&params.id) {
            Some(_) => match memory.save() {
                Ok(()) => format!("Zone '{}' deleted", params.id),
                Err(e) => format!("Error: saving: {}", e),
            },
            None => format!("Zone '{}' not found", params.id),
        };
        self.with_player_messages(result).await
    }

    // === Resource Observation Tools ===

    /// Scan for resources and optionally retain them as layout context.
    #[tool(
        description = "Scan an area for resource patches (ore, oil) and optionally save them as advisory layout context. Saved overlap warns about future mining access but does not veto ordinary infrastructure placement."
    )]
    async fn scan_resources(&self, Parameters(params): Parameters<ScanResourcesParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let area = Area {
            left_top: Position::new(
                params.x as f64 - params.radius as f64,
                params.y as f64 - params.radius as f64,
            ),
            right_bottom: Position::new(
                params.x as f64 + params.radius as f64,
                params.y as f64 + params.radius as f64,
            ),
        };

        let resources = match client.find_resources(area, None).await {
            Ok(r) => r,
            Err(e) => {
                return self
                    .with_player_messages(format!("Error: scanning: {}", e))
                    .await
            }
        };

        let mut memory = AgentMemory::load();
        let mut saved_count = 0;

        let info: Vec<serde_json::Value> = resources
            .into_iter()
            .map(|r| {
                if params.save_as_protected {
                    memory.add_protected_resource(ProtectedResource {
                        resource_type: r.name.clone(),
                        bounds: r.bounding_box,
                        center: r.center,
                        total_amount: r.total_amount,
                        tile_count: r.tile_count,
                    });
                    saved_count += 1;
                }

                serde_json::json!({
                    "name": r.name,
                    "center": { "x": r.center.x, "y": r.center.y },
                    "total_amount": r.total_amount,
                    "tile_count": r.tile_count,
                    "bounds": {
                        "x1": r.bounding_box.left_top.x,
                        "y1": r.bounding_box.left_top.y,
                        "x2": r.bounding_box.right_bottom.x,
                        "y2": r.bounding_box.right_bottom.y
                    }
                })
            })
            .collect();

        if params.save_as_protected {
            if let Err(e) = memory.save() {
                return self
                    .with_player_messages(format!("Error: saving memory: {}", e))
                    .await;
            }
        }

        let result = serde_json::json!({
            "resources_found": info.len(),
            "resources_saved": saved_count,
            "saved_resource_policy": "advisory_layout_context",
            "resources": info
        });

        let result_str =
            serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(result_str).await
    }

    /// Get all recorded resource observations.
    #[tool(
        description = "List saved resource-patch observations. These are advisory layout context, not placement reservations; ordinary infrastructure may overlap them."
    )]
    async fn get_protected_resources(&self) -> String {
        let memory = AgentMemory::load();

        let resources: Vec<serde_json::Value> = memory
            .protected_resources
            .iter()
            .map(|r| {
                serde_json::json!({
                    "resource_type": r.resource_type,
                    "advisory_only": true,
                    "center": { "x": r.center.x, "y": r.center.y },
                    "total_amount": r.total_amount,
                    "tile_count": r.tile_count,
                    "bounds": {
                        "x1": r.bounds.left_top.x,
                        "y1": r.bounds.left_top.y,
                        "x2": r.bounds.right_bottom.x,
                        "y2": r.bounds.right_bottom.y
                    }
                })
            })
            .collect();

        let result =
            serde_json::to_string_pretty(&resources).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(result).await
    }

    // === Layout Assistance Tools ===

    /// Check if a placement is appropriate and possible.
    #[tool(
        description = "Check an exact placement against advisory zones/resources and authoritative live Factorio/Lua placement rules. Resource overlap alone is nonfatal for ordinary infrastructure; incompatible extractors remain rejected. Returns policy_allowed and factorio_allowed separately."
    )]
    async fn check_placement(
        &self,
        Parameters(params): Parameters<CheckPlacementParams>,
    ) -> String {
        let memory = AgentMemory::load();
        let pos = Position::new(params.x, params.y);
        let policy_check = memory.check_placement(&params.entity_name, &pos);

        let direction = if params.direction.is_empty() {
            Direction::North
        } else {
            match Direction::parse(&params.direction) {
                Some(d) => d,
                None => {
                    let result = serde_json::json!({
                        "allowed": false,
                        "policy_allowed": policy_check.allowed,
                        "factorio_allowed": false,
                        "entity": params.entity_name,
                        "position": { "x": params.x, "y": params.y },
                        "direction": params.direction,
                        "warnings": policy_check.warnings,
                        "errors": ["Invalid direction. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)"],
                        "overlapping_zones": policy_check.overlapping_zones,
                        "overlapping_resources": policy_check.overlapping_resources,
                        "resource_overlap_is_advisory": true
                    });
                    let result_str = serde_json::to_string_pretty(&result)
                        .unwrap_or_else(|e| format!("Error: {}", e));
                    return self.with_player_messages(result_str).await;
                }
            }
        };

        let factorio_check = match self.connect().await {
            Ok(mut client) => match client
                .check_entity_placement(&params.entity_name, pos, direction)
                .await
            {
                Ok(value) => value,
                Err(e) => serde_json::json!({
                    "factorio_allowed": false,
                    "error": format!("Factorio placement check failed: {}", e)
                }),
            },
            Err(e) => serde_json::json!({
                "factorio_allowed": false,
                "error": format!("Factorio connection failed: {}", e)
            }),
        };
        let factorio_allowed = factorio_check
            .get("factorio_allowed")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let live_policy_allowed = factorio_check
            .get("policy_allowed")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);
        let live_allowed = placement_report_allowed(&factorio_check);

        let result = serde_json::json!({
            "allowed": policy_check.allowed && live_allowed,
            "policy_allowed": policy_check.allowed && live_policy_allowed,
            "factorio_allowed": factorio_allowed,
            "entity": params.entity_name,
            "position": { "x": params.x, "y": params.y },
            "direction": direction.to_factorio(),
            "warnings": policy_check.warnings,
            "errors": policy_check.errors,
            "overlapping_zones": policy_check.overlapping_zones,
            "overlapping_resources": policy_check.overlapping_resources,
            "resource_overlap_is_advisory": true,
            "factorio": factorio_check
        });

        let result_str =
            serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(result_str).await
    }

    /// Find a suitable empty area for building.
    #[tool(
        description = "Find an entity-clear area for a zone. Permanent non-mining sites prefer recorded-resource-free terrain, but if none exists in range the nearest clear resource-overlapping site is returned with an explicit advisory instead of failing. Reserved zones remain excluded."
    )]
    async fn find_build_area(&self, Parameters(params): Parameters<FindBuildAreaParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let memory = AgentMemory::load();
        let search_area = Area {
            left_top: Position::new(
                params.x as f64 - params.radius as f64,
                params.y as f64 - params.radius as f64,
            ),
            right_bottom: Position::new(
                params.x as f64 + params.radius as f64,
                params.y as f64 + params.radius as f64,
            ),
        };

        // Get existing entities to avoid
        let entities = match client.find_entities(search_area, None, None).await {
            Ok(e) => e,
            Err(e) => {
                return self
                    .with_player_messages(format!("Error: getting entities: {}", e))
                    .await
            }
        };

        // Build a simple occupancy grid
        let width = params.width as i32;
        let height = params.height as i32;

        // Search in a spiral pattern from center
        let center_x = params.x;
        let center_y = params.y;
        let prefer_off_resource = params.zone_type != "mining";
        let mut resource_overlap_fallback: Option<(i32, i32, Vec<String>)> = None;

        let found_result = |check_x: i32,
                            check_y: i32,
                            overlapping_resources: Vec<String>,
                            selection: &str,
                            advisory: Option<&str>| {
            serde_json::json!({
                "found": true,
                "area": {
                    "x1": check_x,
                    "y1": check_y,
                    "x2": check_x + width,
                    "y2": check_y + height
                },
                "center": {
                    "x": check_x + width / 2,
                    "y": check_y + height / 2
                },
                "selection": selection,
                "resource_overlap": !overlapping_resources.is_empty(),
                "overlapping_resources": overlapping_resources,
                "advisory": advisory,
            })
        };

        for dist in 0..params.radius as i32 {
            for dx in -dist..=dist {
                for dy in -dist..=dist {
                    if dx.abs() != dist && dy.abs() != dist {
                        continue; // Only check perimeter of this distance
                    }

                    let check_x = center_x + dx;
                    let check_y = center_y + dy;

                    // Check if this area is clear
                    let candidate = Area::new(
                        check_x as f64,
                        check_y as f64,
                        (check_x + width) as f64,
                        (check_y + height) as f64,
                    );

                    // Check for entity overlap
                    let has_entity = entities.iter().any(|e| candidate.contains(&e.position));

                    if has_entity {
                        continue;
                    }

                    // Check for existing zone overlap
                    let overlapping_zones = memory.zones_overlapping(&candidate);
                    let has_incompatible_zone = overlapping_zones
                        .iter()
                        .any(|z| z.zone_type == ZoneType::Reserved);
                    if has_incompatible_zone {
                        continue;
                    }

                    let mut overlapping_resources: Vec<String> = memory
                        .resources_overlapping(&candidate)
                        .into_iter()
                        .map(|resource| resource.resource_type.clone())
                        .collect();
                    overlapping_resources.sort();
                    overlapping_resources.dedup();

                    if prefer_off_resource && !overlapping_resources.is_empty() {
                        if resource_overlap_fallback.is_none() {
                            resource_overlap_fallback =
                                Some((check_x, check_y, overlapping_resources));
                        }
                        continue;
                    }

                    let selection = if prefer_off_resource {
                        "off_resource_preferred"
                    } else {
                        "first_clear_site"
                    };
                    let result =
                        found_result(check_x, check_y, overlapping_resources, selection, None);
                    return self
                        .with_player_messages(
                            serde_json::to_string_pretty(&result).unwrap_or_default(),
                        )
                        .await;
                }
            }
        }

        if let Some((check_x, check_y, overlapping_resources)) = resource_overlap_fallback {
            let result = found_result(
                check_x,
                check_y,
                overlapping_resources,
                "resource_overlap_fallback",
                Some(
                    "No entity-clear off-resource site was found within the search radius. This fallback overlaps recorded resource terrain; ordinary infrastructure is legal here, but permanent construction may reduce future mining access. Check each entity with live placement rules before building.",
                ),
            );
            return self
                .with_player_messages(serde_json::to_string_pretty(&result).unwrap_or_default())
                .await;
        }

        let result = serde_json::json!({
            "found": false,
            "message": format!("No suitable {}x{} area found within radius {}", width, height, params.radius)
        });
        self.with_player_messages(serde_json::to_string_pretty(&result).unwrap_or_default())
            .await
    }

    /// Get a blank slate view of constraints only.
    #[tool(
        description = "Get terrain, resource context, and zones without existing buildings. Resources are advisory terrain that ordinary logistics may cross or occupy; reserved zones and live Factorio collision checks remain authoritative. Useful for fresh layout planning without the existing factory."
    )]
    async fn get_blank_slate(&self, Parameters(params): Parameters<GetBlankSlateParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let memory = AgentMemory::load();
        let area = Area {
            left_top: Position::new(
                params.x as f64 - params.radius as f64,
                params.y as f64 - params.radius as f64,
            ),
            right_bottom: Position::new(
                params.x as f64 + params.radius as f64,
                params.y as f64 + params.radius as f64,
            ),
        };

        // Get resources in area
        let resources = match client.find_resources(area, None).await {
            Ok(r) => r,
            Err(e) => {
                return self
                    .with_player_messages(format!("Error: getting resources: {}", e))
                    .await
            }
        };

        // Get zones overlapping this area
        let zones: Vec<serde_json::Value> = memory
            .zones_overlapping(&area)
            .iter()
            .map(|z| {
                serde_json::json!({
                    "id": z.id,
                    "zone_type": z.zone_type.to_string(),
                    "bounds": {
                        "x1": z.bounds.left_top.x,
                        "y1": z.bounds.left_top.y,
                        "x2": z.bounds.right_bottom.x,
                        "y2": z.bounds.right_bottom.y
                    }
                })
            })
            .collect();

        // Format resources
        let resource_info: Vec<serde_json::Value> = resources
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name,
                    "center": { "x": r.center.x, "y": r.center.y },
                    "bounds": {
                        "x1": r.bounding_box.left_top.x,
                        "y1": r.bounding_box.left_top.y,
                        "x2": r.bounding_box.right_bottom.x,
                        "y2": r.bounding_box.right_bottom.y
                    },
                    "total_amount": r.total_amount
                })
            })
            .collect();

        let result = serde_json::json!({
            "area": {
                "x1": area.left_top.x,
                "y1": area.left_top.y,
                "x2": area.right_bottom.x,
                "y2": area.right_bottom.y
            },
            "constraints": {
                "resources": resource_info,
                "zones": zones
            },
            "resource_overlap_policy": "advisory_for_ordinary_infrastructure",
            "tip": "Resources are layout context, not immovable terrain: ordinary logistics may cross or occupy them. Prefer clear permanent sites when practical, but use live placement checks for actual collision and extractor compatibility. Reserved zones remain hard constraints."
        });

        let result_str =
            serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(result_str).await
    }

    /// Clear trees and rocks in an area.
    #[tool(
        description = "Clear trees and rocks in a rectangular area to make space for building. \
        Use dry_run=true to preview what will be cleared before actually clearing."
    )]
    async fn clear_area(&self, Parameters(params): Parameters<ClearAreaParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let area = Area::new(params.x1, params.y1, params.x2, params.y2);
        let result = match client
            .call_remote(
                "clear_area",
                &[
                    serde_json::json!(client.agent_id().as_str()),
                    serde_json::json!(area.left_top.x),
                    serde_json::json!(area.left_top.y),
                    serde_json::json!(area.right_bottom.x),
                    serde_json::json!(area.right_bottom.y),
                    serde_json::json!(params.clear_trees),
                    serde_json::json!(params.clear_rocks),
                    serde_json::json!(params.dry_run),
                ],
            )
            .await
        {
            Ok(result) => result,
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }
}

/// Parse a zone type string into ZoneType enum
fn parse_zone_type(s: &str) -> ZoneType {
    match s.to_lowercase().as_str() {
        "mining" => ZoneType::Mining,
        "smelting" => ZoneType::Smelting,
        "assembly" => ZoneType::Assembly,
        "power" => ZoneType::Power,
        "storage" => ZoneType::Storage,
        "logistics" => ZoneType::Logistics,
        "reserved" => ZoneType::Reserved,
        other => {
            if let Some(name) = other.strip_prefix("custom:") {
                ZoneType::Custom(name.to_string())
            } else {
                ZoneType::Custom(other.to_string())
            }
        }
    }
}

impl ServerHandler for FactorioMcp {
    async fn call_tool(
        &self,
        request: rmcp::model::CallToolRequestParams,
        context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<rmcp::model::CallToolResult, rmcp::ErrorData> {
        let context = rmcp::handler::server::tool::ToolCallContext::new(self, request, context);
        let mut result = self.tool_router.call(context).await?;
        mark_semantic_tool_errors(&mut result);
        Ok(result)
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: rmcp::service::RequestContext<rmcp::service::RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        Ok(rmcp::model::ListToolsResult::with_all_items(
            self.tool_router.list_all(),
        ))
    }

    fn get_info(&self) -> rmcp::model::ServerInfo {
        rmcp::model::ServerInfo {
            protocol_version: rmcp::model::ProtocolVersion::LATEST,
            capabilities: rmcp::model::ServerCapabilities {
                tools: Some(rmcp::model::ToolsCapability::default()),
                ..Default::default()
            },
            server_info: rmcp::model::Implementation {
                name: "factorio-mcp".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                title: None,
                icons: None,
                website_url: None,
            },
            instructions: Some("Factorio game control server. Use these tools to interact with a running Factorio game.".to_string()),
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging to stderr (stdout is for MCP protocol)
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::WARN.into()),
        )
        .init();

    let service = FactorioMcp::new();
    let server = service.serve(rmcp::transport::stdio()).await?;
    server.waiting().await?;

    Ok(())
}
