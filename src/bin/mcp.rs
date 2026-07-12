//! MCP (Model Context Protocol) server for factorioctl
//!
//! Exposes Factorio control as MCP tools for LLM agents.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use tokio::sync::Mutex;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    schemars::{self, JsonSchema},
    tool, tool_handler, tool_router, ServerHandler, ServiceExt,
};
use serde::{de, Deserialize, Deserializer, Serialize};

use factorioctl::analyze::{
    analyze_belt_reach, analyze_inserters, analyze_item_flow, detect_sushi_belts, find_belt_gaps,
    find_belt_networks, trace_belt_sources, BeltGraph, EntityLookup,
};
use factorioctl::client::{AgentId, FactorioClient};
use factorioctl::memory::{AgentMemory, BeltRouting, ProtectedResource, Zone, ZoneType};
use factorioctl::world::{
    build_production_report, build_situation_report, entity_size, find_belt_route_with_options,
    Area, BeltKind, Direction, Entity, EntityProduction, GridPos, Position, RoutingOptions,
    TilePos, UndergroundConfig,
};

fn production_verification_json(
    entities: Vec<EntityProduction>,
) -> (serde_json::Value, bool, bool) {
    let report = build_production_report(entities);
    let has_working_entity = report.working_count > 0;
    (
        serde_json::json!({
            "success": true,
            "report": report,
        }),
        true,
        has_working_entity,
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

fn placed_units_not_dead(
    verification: &serde_json::Value,
    unit_numbers: &[u32],
) -> (bool, Vec<serde_json::Value>) {
    let entities = verification
        .get("report")
        .and_then(|report| report.get("entities"))
        .and_then(|entities| entities.as_array());
    let mut statuses = Vec::new();
    let mut ok = true;
    for unit_number in unit_numbers {
        let entity = entities.and_then(|entities| {
            entities.iter().find(|entity| {
                entity
                    .get("unit_number")
                    .and_then(|value| value.as_u64())
                    .map(|value| value as u32)
                    == Some(*unit_number)
            })
        });
        match entity {
            Some(entity) => {
                let status = entity
                    .get("status")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown");
                let dead = matches!(status, "no_power" | "no_fuel" | "disabled");
                ok &= !dead;
                statuses.push(serde_json::json!({
                    "unit_number": unit_number,
                    "name": entity.get("name").cloned().unwrap_or(serde_json::Value::Null),
                    "status": status,
                    "working": entity.get("working").cloned().unwrap_or(serde_json::Value::Bool(false)),
                    "ok": !dead,
                }));
            }
            None => {
                ok = false;
                statuses.push(serde_json::json!({
                    "unit_number": unit_number,
                    "status": "missing_from_verification_area",
                    "ok": false,
                }));
            }
        }
    }
    (ok, statuses)
}

fn placed_unit_working(verification: &serde_json::Value, unit_number: Option<u32>) -> bool {
    let Some(unit_number) = unit_number else {
        return false;
    };
    verification
        .get("report")
        .and_then(|report| report.get("entities"))
        .and_then(|entities| entities.as_array())
        .and_then(|entities| {
            entities.iter().find(|entity| {
                entity
                    .get("unit_number")
                    .and_then(|value| value.as_u64())
                    .map(|value| value as u32)
                    == Some(unit_number)
            })
        })
        .and_then(|entity| entity.get("working"))
        .and_then(|working| working.as_bool())
        .unwrap_or(false)
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
            "tool": "analyze_belt_gaps",
            "reason": "the routed belt segment did not build completely",
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
            "tool": "diagnose_fuel_sustainability",
            "reason": "a burner consumer has no durable coal path",
        }));
        actions.push(serde_json::json!({
            "tool": "build_fuel_supply",
            "reason": "build a coal belt/inserter path instead of hand-feeding a small fuel buffer",
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
            "tool": "analyze_belt_reach",
            "reason": "trace the blocked output lane before placing more entities",
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
        automation_repair_hint, execute_lua_refusal, flow_lookup, flow_scan_area,
        is_machine_output_source, machine_output_build_args, machine_side_layout,
        placed_unit_working, placed_units_not_dead, raw_lua_enabled, ready_fuel_supply_args,
        route_belt_failure_json, route_segment_waypoint, BuildFuelSupplyParams, RouteBeltParams,
    };
    use factorioctl::analyze::EntityLookup;
    use factorioctl::world::{Area, Entity, GridPos, Position, TilePos};

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
    fn placed_unit_status_helpers_require_matching_healthy_units() {
        let verification = serde_json::json!({
            "success": true,
            "report": {
                "entities": [
                    {
                        "name": "electric-mining-drill",
                        "unit_number": 10,
                        "status": "working",
                        "working": true
                    },
                    {
                        "name": "inserter",
                        "unit_number": 11,
                        "status": "no_power",
                        "working": false
                    }
                ]
            }
        });

        assert!(placed_unit_working(&verification, Some(10)));
        assert!(!placed_unit_working(&verification, Some(11)));
        assert!(!placed_unit_working(&verification, Some(99)));
        assert!(!placed_unit_working(&verification, None));

        let (ok, statuses) = placed_units_not_dead(&verification, &[10, 11, 99]);
        assert!(!ok);
        assert_eq!(statuses.len(), 3);
        assert_eq!(statuses[0]["ok"], true);
        assert_eq!(statuses[1]["ok"], false);
        assert_eq!(statuses[1]["status"], "no_power");
        assert_eq!(statuses[2]["ok"], false);
        assert_eq!(statuses[2]["status"], "missing_from_verification_area");
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
                        "status": "no_ingredients",
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

        assert!(tools.contains(&"analyze_belt_gaps"));
        assert!(tools.contains(&"execute_entity_placement_near"));
        assert!(tools.contains(&"analyze_item_flow"));
        assert!(tools.contains(&"build_assembler_feed"));
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
        };

        let north = machine_side_layout(&entity, "north").expect("north side");
        assert_eq!(north.inserter_x, 10.5);
        assert_eq!(north.inserter_y, 18.5);
        assert_eq!(north.belt_x, 10);
        assert_eq!(north.belt_y, 17);
        assert_eq!(north.upstream_x, 10);
        assert_eq!(north.upstream_y, 16);
        assert_eq!(north.input_direction, "south");
        assert_eq!(north.output_direction, "north");

        let east = machine_side_layout(&entity, "east").expect("east side");
        assert_eq!(east.inserter_x, 12.5);
        assert_eq!(east.inserter_y, 20.5);
        assert_eq!(east.belt_x, 13);
        assert_eq!(east.belt_y, 20);
        assert_eq!(east.upstream_x, 14);
        assert_eq!(east.upstream_y, 20);
        assert_eq!(east.input_direction, "west");
        assert_eq!(east.output_direction, "east");

        let furnace = Entity {
            unit_number: Some(15),
            name: "stone-furnace".to_string(),
            entity_type: Some("furnace".to_string()),
            position: Position::new(42.0, -22.0),
            direction: 0,
            health: None,
            force: Some("player".to_string()),
            bounding_box: None,
        };
        let furnace_north = machine_side_layout(&furnace, "north").expect("furnace north side");
        assert_eq!(furnace_north.inserter_x, 42.5);
        assert_eq!(furnace_north.inserter_y, -23.5);
        assert_eq!(furnace_north.belt_x, 42);
        assert_eq!(furnace_north.belt_y, -25);
        assert_eq!(furnace_north.output_direction, "north");

        let assembler_without_bbox = Entity {
            unit_number: Some(339),
            name: "assembling-machine-1".to_string(),
            entity_type: Some("assembling-machine".to_string()),
            position: Position::new(51.5, -14.5),
            direction: 0,
            health: None,
            force: Some("player".to_string()),
            bounding_box: None,
        };
        let assembler_south =
            machine_side_layout(&assembler_without_bbox, "south").expect("assembler south side");
        assert_eq!(assembler_south.inserter_x, 51.5);
        assert_eq!(assembler_south.inserter_y, -12.5);
        assert_eq!(assembler_south.belt_x, 51);
        assert_eq!(assembler_south.belt_y, -12);
        assert_eq!(assembler_south.input_direction, "north");
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
                    "tool": "build_fuel_supply",
                    "args": {
                        "consumer_unit_number": 49,
                        "from_x": 78,
                        "from_y": -20,
                        "pickup_x": 46,
                        "pickup_y": 11,
                        "inserter_x": 46.5,
                        "inserter_y": 10.5,
                        "inserter_direction": "north",
                        "inserter_name": "burner-inserter"
                    }
                }
            }],
            "suggested_actions": []
        });

        let args = ready_fuel_supply_args(&report).expect("ready args");

        assert_eq!(args.consumer_unit_number, 49);
        assert_eq!(args.from_x, 78);
        assert_eq!(args.pickup_y, 11);
        assert_eq!(args.inserter_name, "burner-inserter");
        assert_eq!(args.belt_type, "transport-belt");
        assert!(args.extend_existing);
    }

    #[test]
    fn build_fuel_supply_params_accept_factorio_center_coordinates_for_tiles() {
        let args: BuildFuelSupplyParams = serde_json::from_value(serde_json::json!({
            "consumer_unit_number": 49,
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
    }

    #[test]
    fn ready_fuel_supply_args_accepts_top_level_suggested_action() {
        let report = serde_json::json!({
            "consumers": [],
            "suggested_actions": [{
                "type": "build_fuel_supply",
                "tool": "build_fuel_supply",
                "args": {
                    "consumer_unit_number": 73,
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
        assert_eq!(args.inserter_direction, "north");
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
            .map_err(|e| format!("Error reading entity {unit}: {e}")),
    }
}

/// Parameters for get_entities tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct GetEntitiesParams {
    /// X coordinate of area center
    pub x: i32,
    /// Y coordinate of area center
    pub y: i32,
    /// Radius around center
    #[serde(default = "default_radius")]
    pub radius: u32,
    /// Optional: filter by entity name (e.g., 'transport-belt')
    pub name: Option<String>,
    /// Optional: filter by entity type (e.g., 'container', 'resource', 'lab')
    #[serde(default)]
    pub entity_type: Option<String>,
    /// Maximum entities to return before summarizing (default: 100)
    #[serde(default = "default_entity_limit")]
    pub limit: usize,
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
    /// Entity name (e.g., 'transport-belt', 'inserter')
    pub entity_name: String,
    /// X coordinate to place at
    pub x: f64,
    /// Y coordinate to place at
    pub y: f64,
    /// Direction: "north", "east", "south", "west" (or shorthand "n", "e", "s", "w", or numbers 0/4/8/12)
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
    /// Existing drill unit number. If omitted, provide output_x/output_y/output_direction from build_edge_miner or get_machine_belt_positions.
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
    /// Existing drill unit number. If omitted, provide output_x/output_y/output_direction from build_edge_miner or get_machine_belt_positions.
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

/// Parameters for mine_at tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct MineAtParams {
    /// X coordinate to mine at
    pub x: f64,
    /// Y coordinate to mine at
    pub y: f64,
    /// Number of entities to mine
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
    /// Target output count to extract into inventory.
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
            | "underground-belt"
            | "fast-underground-belt"
            | "express-underground-belt"
    )
}

/// Parameters for build_fuel_supply tool.
#[derive(Clone, Debug, Deserialize, Serialize, JsonSchema)]
pub struct BuildFuelSupplyParams {
    /// Fuel consumer to supply, used for verification context.
    pub consumer_unit_number: u32,
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
    #[serde(default = "default_furnace_fuel_count")]
    pub inserter_fuel_count: u32,
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
    /// If true, diagnose and return the selected build_fuel_supply call without placing.
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
            input_direction: "south",
            output_direction: "north",
        }),
        "east" | "e" | "right" => Ok(MachineSideLayout {
            side: "east",
            inserter_x: bbox.right_bottom.x + 0.5,
            inserter_y: lane_y,
            belt_x: (bbox.right_bottom.x + 1.5).floor() as i32,
            belt_y: lane_y.floor() as i32,
            upstream_x: (bbox.right_bottom.x + 2.5).floor() as i32,
            upstream_y: lane_y.floor() as i32,
            input_direction: "west",
            output_direction: "east",
        }),
        "south" | "s" | "down" => Ok(MachineSideLayout {
            side: "south",
            inserter_x: lane_x,
            inserter_y: bbox.right_bottom.y + 0.5,
            belt_x: lane_x.floor() as i32,
            belt_y: (bbox.right_bottom.y + 1.5).floor() as i32,
            upstream_x: lane_x.floor() as i32,
            upstream_y: (bbox.right_bottom.y + 2.5).floor() as i32,
            input_direction: "north",
            output_direction: "south",
        }),
        "west" | "w" | "left" => Ok(MachineSideLayout {
            side: "west",
            inserter_x: bbox.left_top.x - 0.5,
            inserter_y: lane_y,
            belt_x: (bbox.left_top.x - 1.5).floor() as i32,
            belt_y: lane_y.floor() as i32,
            upstream_x: (bbox.left_top.x - 2.5).floor() as i32,
            upstream_y: lane_y.floor() as i32,
            input_direction: "east",
            output_direction: "west",
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

fn ready_fuel_supply_args(report: &serde_json::Value) -> Option<BuildFuelSupplyParams> {
    let consumers = report.get("consumers")?.as_array()?;
    for consumer in consumers {
        if let Some(args) = consumer
            .get("ready_to_call")
            .and_then(|ready| ready.get("args"))
        {
            if let Ok(params) = serde_json::from_value::<BuildFuelSupplyParams>(args.clone()) {
                return Some(params);
            }
        }
    }
    let actions = report.get("suggested_actions")?.as_array()?;
    for action in actions {
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
    /// Entity unit number to remove
    pub unit_number: u32,
}

/// Parameters for rotate_entity tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RotateEntityParams {
    /// Entity unit number to rotate
    pub unit_number: u32,
    /// Direction: "north", "east", "south", "west" (or shorthand "n", "e", "s", "w", or numbers 0/4/8/12)
    pub direction: String,
}

/// Parameters for get_machine_belt_positions tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct MachineBeltPositionsParams {
    /// Unit number of the machine (furnace, assembler, etc.)
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
    /// Number of packs to transfer
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

/// Parameters for querying force production statistics for eval/report tools.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct EvalProductionSnapshotParams {
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

// === Resource Protection Parameters ===

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
    /// If true, save discovered resources as protected (default: true)
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

/// The MCP server for Factorio control
#[derive(Clone)]
pub struct FactorioMcp {
    config: ConnectionConfig,
    #[allow(dead_code)]
    client: Arc<Mutex<Option<FactorioClient>>>,
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

impl FactorioMcp {
    fn new() -> Self {
        Self {
            config: ConnectionConfig::from_env(),
            client: Arc::new(Mutex::new(None)),
            tool_router: Self::tool_router(),
        }
    }

    async fn connect(&self) -> Result<FactorioClient, String> {
        let agent_id = AgentId::new(std::env::var("FACTORIO_AGENT_ID").ok().as_deref())
            .map_err(|e| format!("Invalid FACTORIO_AGENT_ID: {}", e))?;
        FactorioClient::connect(&self.config.host, self.config.port, &self.config.password)
            .await
            .map(|client| client.with_agent_id(agent_id))
            .map_err(|e| format!("Failed to connect: {}", e))
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
            .map_err(|e| format!("Error getting entities: {}", e))?;
        let tiles = client.get_tiles(area).await.unwrap_or_default();
        let char_pos = client.get_character_position().await.ok();
        let detail_level = match detail {
            Some("minimal") => factorioctl::cli::DetailLevel::Minimal,
            Some("normal") => factorioctl::cli::DetailLevel::Normal,
            _ => factorioctl::cli::DetailLevel::Detailed,
        };

        let power_coverage = if show_power {
            match client
                .call_remote(
                    "get_power_coverage",
                    &[
                        serde_json::json!(center.x as i32),
                        serde_json::json!(center.y as i32),
                        serde_json::json!(radius),
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

    /// Query production statistics snapshot for eval/report scoring.
    #[tool(
        description = "Read force item production totals and one-minute rates for eval/report scoring."
    )]
    async fn eval_production_snapshot(
        &self,
        Parameters(params): Parameters<EvalProductionSnapshotParams>,
    ) -> String {
        self.call_lifecycle_remote(
            "eval_production_snapshot",
            &[serde_json::json!(params.surface_name)],
        )
        .await
    }

    // --- Query Tools ---

    /// Get all entities in an area. Returns entity names, positions, and types.
    #[tool(
        description = "Get entities in an area. Prefer name/type filters. Large results are capped and summarized; use a smaller radius or limit for details."
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

    /// Get belt and inserter positions for a machine.
    #[tool(
        description = "Get the correct belt and inserter positions for connecting to a machine. \
        For DRILLS: Returns the exact drop position (where items come out) and the tile where a belt should be placed. \
        For FURNACES/ASSEMBLERS: Returns input_belt, input_inserter, output_belt, output_inserter positions. \
        ALWAYS use this tool before routing belts to/from machines!"
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
                    .with_player_messages(format!("Error getting entity: {}", e))
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
                        .with_player_messages(format!("Error querying drop position: {}", e))
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
                "belt_place_entity_args": {
                    "entity_name": "transport-belt",
                    "x": south.belt_x,
                    "y": south.belt_y,
                    "direction": south.input_direction
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
                "belt_place_entity_args": {
                    "entity_name": "transport-belt",
                    "x": north.belt_x,
                    "y": north.belt_y,
                    "direction": north.output_direction
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
            "coordinate_note": "Use *_place_entity_args directly. belt_tile is the lower-left tile coordinate accepted by place_entity for belts; inserter_position is the actual inserter center. These positions are intentionally different and should not collide."
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
                        .with_player_messages(format!("Error getting position: {}", e))
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
                    .with_player_messages(format!("Error getting position: {}", e))
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
            Err(e) => format!("Error rendering map: {}", e),
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
        description = "Find the nearest resource patch (ore, oil) of a specific type from a position. \
        Returns the patch center, total amount, tile count, and bounding box. Searches within 200 tiles. \
        Use this to locate resources for mining operations."
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
                        .with_player_messages(format!("Error getting position: {}", e))
                        .await
                }
            }
        };

        let result = match client
            .find_nearest_resource(&params.resource_type, from)
            .await
        {
            Ok(resource) => {
                let bb = &resource.bounding_box;
                let info = serde_json::json!({
                    "name": resource.name,
                    "center_x": resource.center.x,
                    "center_y": resource.center.y,
                    "total_amount": resource.total_amount,
                    "tile_count": resource.tile_count,
                    "bounding_box": {
                        "left_top": { "x": bb.left_top.x, "y": bb.left_top.y },
                        "right_bottom": { "x": bb.right_bottom.x, "y": bb.right_bottom.y }
                    },
                    "distance": ((resource.center.x - from.x).powi(2) + (resource.center.y - from.y).powi(2)).sqrt(),
                });
                serde_json::to_string_pretty(&info).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("No {} found within 200 tiles: {}", params.resource_type, e),
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
        description = "Recovery action for a physically wedged agent character. If the current character footprint is blocked, moves to the nearest verified clear standing position and clears stale walk/mining state. Use is_player_blocked first when diagnosing; pass dry_run=true to preview."
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
        description = "Compact one-call situational snapshot with position, health, walking, tick, inventory, nearby entity counts, and resource patches. Use this to orient instead of separate render_map + get_inventory + get_resources scans."
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
        description = "Call this after building or modifying production to confirm the intended outcome. Reports each producing entity's status (working/no_power/no_fuel/no_ingredients/full_output/etc.) plus products_finished so you can diagnose failures and derive rates by calling twice."
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
        let entities = match client.verify_production(area).await {
            Ok(entities) => entities,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };
        let report = build_production_report(entities);
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
        let result =
            serde_json::to_string_pretty(&report).unwrap_or_else(|e| format!("Error: {}", e));
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
        description = "Analyze item flow from a source entity/tile to a target entity/tile. Returns whether belts connect, current items on the reachable belt path, source/target belt tiles, the first missing/wrong-way/blocked belt break, and a concrete repair action such as place_entity or rotate_entity. Use before manually squinting at belt directions."
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
            Err(e) => return self.with_player_messages(e).await,
        };
        let target = match flow_lookup(
            params.target_unit_number,
            params.target_x,
            params.target_y,
            "target",
        ) {
            Ok(lookup) => lookup,
            Err(e) => return self.with_player_messages(e).await,
        };
        let source_tile = match flow_reference_tile(&mut client, source).await {
            Ok(tile) => tile,
            Err(e) => return self.with_player_messages(e).await,
        };
        let target_tile = match flow_reference_tile(&mut client, target).await {
            Ok(tile) => tile,
            Err(e) => return self.with_player_messages(e).await,
        };
        let area = flow_scan_area(source_tile, target_tile, params.radius.clamp(1, 100));

        let result = match client.find_entities(area, None, None).await {
            Ok(entities) => match client.get_belt_lane_contents(area).await {
                Ok(belt_contents) => {
                    let report = analyze_item_flow(&entities, &belt_contents.belts, source, target);
                    serde_json::to_string_pretty(&report)
                        .unwrap_or_else(|e| format!("Error: {}", e))
                }
                Err(e) => format!("Error reading belt contents: {}", e),
            },
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    // --- Action Tools ---

    /// Walk character to a position.
    #[tool(
        description = "Walk character to a position using the mod's direct stepped movement target. TIP: Call broadcast_thought in the SAME response to narrate your movement while walking."
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
        description = "Place an entity from character inventory at a position. On failure, returns structured diagnostics including can_place, inventory_count, direction, and position."
    )]
    async fn place_entity(&self, Parameters(params): Parameters<PlaceEntityParams>) -> String {
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
                        .with_player_messages(format!(
                    "Invalid direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                    params.direction
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
        description = "Find nearby Factorio-valid placements for an entity in all cardinal directions. Mining-drill results include output belt diagnostics and prefer clear patch-edge outlets. Use for fussy entities like drills, offshore-pump, boiler, and steam-engine instead of guessing coordinates."
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
                b_allowed
                    .cmp(&a_allowed)
                    .then_with(|| b_output_clear.cmp(&a_output_clear))
                    .then_with(|| b_output_buildable.cmp(&a_output_buildable))
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
        description = "Place one entity using the selected safe placement from plan_entity_placement_near. Avoids agent-overlap/trapping placements and returns the placed unit number. Prefer this over manual place_entity for assemblers, labs, poles, chests, and crowded builds. Use dry_run=true during planner turns."
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
                    .with_player_messages(format!(
                        "Invalid selected direction '{}'. Re-run plan_entity_placement_near.",
                        direction_name
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
        description = "Plan a patch-edge mining setup without mutating the game. Returns a resource-backed drill placement whose output belt tile is clear/buildable, plus ordered place_entity steps and missing_items. Use before placing burner/electric drills on large ore patches."
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
        description = "Build a patch-edge mining setup from build_edge_miner geometry: places the selected drill, places the output belt, bootstraps burner drill fuel, verifies production, and returns the placed drill unit/output tile. Prefer this over manually replaying build_edge_miner steps. Use dry_run=true during planner turns."
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

        if params.dry_run {
            let result = serde_json::json!({
                "success": plan.get("success").and_then(|value| value.as_bool()).unwrap_or(false),
                "dry_run": true,
                "plan": plan,
                "guidance": "If success/ready are true and missing_items is empty, call execute_edge_miner again with dry_run=false. For coal, follow with diagnose_fuel_sustainability/build_fuel_supply for nearby consumers.",
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
            let msg =
                serde_json::to_string_pretty(&plan).unwrap_or_else(|e| format!("Error: {}", e));
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
        let mut actions = Vec::new();
        let mut placed_drill_unit = None;
        let mut placed_belt_unit = None;

        for step in steps {
            let tool = step
                .get("tool")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let args = step.get("tool_args").unwrap_or(&serde_json::Value::Null);
            if tool != "place_entity" {
                actions.push(serde_json::json!({
                    "tool": tool,
                    "args": args,
                    "success": false,
                    "skipped": true,
                    "error": "unsupported step in execute_edge_miner",
                }));
                continue;
            }

            let entity_name = args
                .get("entity_name")
                .and_then(|value| value.as_str())
                .unwrap_or("");
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
                    actions.push(serde_json::json!({
                        "tool": "place_entity",
                        "args": args,
                        "success": false,
                        "error": format!("invalid direction '{}'", direction_name),
                    }));
                    continue;
                }
            };
            let placed = client
                .place_entity(entity_name, Position::new(x, y), direction)
                .await;
            match &placed {
                Ok(entity) if entity.name.contains("mining-drill") => {
                    placed_drill_unit = entity.unit_number;
                }
                Ok(entity) if entity.name.contains("transport-belt") => {
                    placed_belt_unit = entity.unit_number;
                }
                _ => {}
            }
            actions.push(serde_json::json!({
                "tool": "place_entity",
                "args": args,
                "success": placed.is_ok(),
                "entity": placed.as_ref().ok(),
                "error": placed.as_ref().err().map(|e| e.to_string()),
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
                fuel_report = serde_json::json!({
                    "tool": "insert_items",
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
        let drill_working = placed_unit_working(&verification, placed_drill_unit);
        let placed_units: Vec<u32> = placed_drill_unit.into_iter().collect();
        let (_, placed_unit_statuses) = placed_units_not_dead(&verification, &placed_units);
        let action_success = actions.iter().all(|action| {
            action
                .get("success")
                .and_then(|value| value.as_bool())
                .unwrap_or(false)
        });
        let repair_hint = automation_repair_hint(
            "execute_edge_miner",
            "edge miner output belt",
            verification_call_ok,
            &verification,
            &placed_unit_statuses,
            Some(action_success),
        );

        let success = action_success
            && fuel_report
                .get("success")
                .and_then(|value| value.as_bool())
                .unwrap_or(true)
            && verification_call_ok
            && drill_working;
        let result = serde_json::json!({
            "success": success,
            "dry_run": false,
            "plan": plan,
            "selected": selected,
            "placed_drill_unit_number": placed_drill_unit,
            "placed_belt_unit_number": placed_belt_unit,
            "actions": actions,
            "bootstrap_fuel": fuel_report,
            "automation_verified": {
                "placed_drill_working": drill_working,
                "verification_call_ok": verification_call_ok,
                "placed_unit_statuses": placed_unit_statuses,
            },
            "verification": verification,
            "repair_hint": repair_hint,
            "guidance": "If this is coal production, route this belt to consumers with diagnose_fuel_sustainability/build_fuel_supply. Temporary burner fuel is not durable automation completion.",
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
                            .with_player_messages(format!(
                                "Invalid output_direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                                direction_name
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
        description = "Build a direct drill-output smelter cell from build_direct_smelter geometry: places/rotates the output belt, places furnace and inserter, bootstraps any burner fuel, verifies production, and diagnoses durable fuel supply. Prefer this over manually executing individual smelter placement steps."
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
                            .with_player_messages(format!(
                                "Invalid output_direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                                direction_name
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

        if params.dry_run {
            let result = serde_json::json!({
                "success": plan.get("success").and_then(|value| value.as_bool()).unwrap_or(false),
                "dry_run": true,
                "plan": plan,
                "guidance": "If success/ready are true and missing_items is empty, call execute_direct_smelter again with dry_run=false.",
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
            let msg =
                serde_json::to_string_pretty(&plan).unwrap_or_else(|e| format!("Error: {}", e));
            return self.with_player_messages(msg).await;
        }

        let mut actions = Vec::new();
        let mut placed_furnace_unit = None;
        let mut placed_inserter_unit = None;
        let steps = plan
            .get("steps")
            .and_then(|value| value.as_array())
            .cloned()
            .unwrap_or_default();

        for step in steps {
            let tool = step
                .get("tool")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let args = step.get("tool_args").unwrap_or(&serde_json::Value::Null);
            match tool {
                "place_entity" => {
                    let entity_name = args
                        .get("entity_name")
                        .and_then(|value| value.as_str())
                        .unwrap_or("");
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
                            actions.push(serde_json::json!({
                                "tool": tool,
                                "args": args,
                                "success": false,
                                "error": format!("invalid direction {}", direction_name),
                            }));
                            continue;
                        }
                    };
                    let placed = client
                        .place_entity(entity_name, Position::new(x, y), direction)
                        .await;
                    match placed {
                        Ok(entity) => {
                            if entity_name == params.furnace_name {
                                placed_furnace_unit = entity.unit_number;
                            }
                            if entity_name == params.inserter_name {
                                placed_inserter_unit = entity.unit_number;
                            }
                            actions.push(serde_json::json!({
                                "tool": tool,
                                "args": args,
                                "success": true,
                                "unit_number": entity.unit_number,
                            }));
                        }
                        Err(e) => actions.push(serde_json::json!({
                            "tool": tool,
                            "args": args,
                            "success": false,
                            "error": e.to_string(),
                        })),
                    }
                }
                "rotate_entity" => {
                    let unit_number = args
                        .get("unit_number")
                        .and_then(|value| value.as_u64())
                        .unwrap_or(0) as u32;
                    let direction_name = args
                        .get("direction")
                        .and_then(|value| value.as_str())
                        .unwrap_or("north");
                    let direction = match Direction::parse(direction_name) {
                        Some(direction) => direction,
                        None => {
                            actions.push(serde_json::json!({
                                "tool": tool,
                                "args": args,
                                "success": false,
                                "error": format!("invalid direction {}", direction_name),
                            }));
                            continue;
                        }
                    };
                    let rotated = client
                        .rotate_entity(unit_number, direction.to_factorio())
                        .await;
                    actions.push(serde_json::json!({
                        "tool": tool,
                        "args": args,
                        "success": rotated.is_ok(),
                        "error": rotated.as_ref().err().map(|e| e.to_string()),
                    }));
                }
                _ => actions.push(serde_json::json!({
                    "tool": tool,
                    "args": args,
                    "success": false,
                    "error": "unsupported execute_direct_smelter step",
                })),
            }
        }

        let mut bootstrap_fuel = Vec::new();
        if params.furnace_name != "electric-furnace" {
            if let Some(unit) = placed_furnace_unit {
                let inserted = client.insert_items(unit, "coal", 25, "fuel").await;
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
        let (verification, verification_call_ok, has_working_entity) =
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
        let fuel_sustainability = match client.diagnose_fuel_sustainability(verify_area, 20).await {
            Ok(report) => serde_json::json!({
                "success": true,
                "report": report,
            }),
            Err(e) => serde_json::json!({
                "success": false,
                "error": e.to_string(),
            }),
        };

        let action_success = actions.iter().all(|action| {
            action
                .get("success")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
        });
        let placed_units: Vec<u32> = [placed_furnace_unit, placed_inserter_unit]
            .into_iter()
            .flatten()
            .collect();
        let (placed_units_ready, placed_unit_statuses) =
            placed_units_not_dead(&verification, &placed_units);
        let automation_verified = verification_call_ok && has_working_entity && placed_units_ready;
        let repair_hint = automation_repair_hint(
            "execute_direct_smelter",
            "direct drill-to-furnace smelter cell",
            verification_call_ok,
            &verification,
            &placed_unit_statuses,
            Some(action_success),
        );
        let result = serde_json::json!({
            "success": action_success && automation_verified,
            "placement_success": action_success,
            "dry_run": false,
            "plan": plan,
            "actions": actions,
            "bootstrap_fuel": bootstrap_fuel,
            "automation_verified": {
                "success": automation_verified,
                "verification_call_ok": verification_call_ok,
                "has_working_entity_near_cell": has_working_entity,
                "placed_units_ready": placed_units_ready,
                "placed_unit_statuses": placed_unit_statuses,
            },
            "verification": verification,
            "fuel_sustainability": fuel_sustainability,
            "repair_hint": repair_hint,
            "guidance": "If fuel_sustainability reports ranked consumers without durable supply, run build_fuel_supply next; temporary bootstrap fuel is not automation completion.",
        });
        let msg = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(msg).await
    }

    /// Mine entities at a position.
    #[tool(description = "Mine entities at a position. Character will walk there first if needed.")]
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

    /// Craft items.
    #[tool(description = "Craft items using character's crafting ability.")]
    async fn craft(&self, Parameters(params): Parameters<CraftParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client.craft(&params.recipe, params.count).await {
            Ok(result) => {
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
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
            Ok(()) => format!("Inserted {} {} into entity", params.count, params.item),
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
        let walk = client.walk_to(target, false).await;
        success &= walk.is_ok();
        actions.push(serde_json::json!({
            "tool": "walk_to",
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
                "error": "walk_to failed",
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
        description = "Bounded bootstrap controller for breaking first-inserter deadlocks. Internally walks to one furnace, inserts a short fuel/ore buffer, waits for smelting, extracts a small plate batch, and optionally crafts one bootstrap component such as burner-inserter. Use this instead of raw insert_items/extract_items/hand_feed_furnace when no inserter exists yet; after it succeeds, immediately build durable fuel/output automation."
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
        let output_count = params.output_count.clamp(1, params.source_count.max(1));
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
                    {"tool": "walk_to", "args": {"x": furnace.position.x, "y": furnace.position.y}},
                    {"tool": "insert_items", "internal": true, "args": {"unit_number": params.furnace_unit_number, "item": params.fuel_item, "count": params.fuel_count, "inventory_type": "fuel"}},
                    {"tool": "insert_items", "internal": true, "args": {"unit_number": params.furnace_unit_number, "item": params.source_item, "count": params.source_count, "inventory_type": "furnace_source"}},
                    {"tool": "wait_ticks", "internal": true, "args": {"ticks": wait_ticks}},
                    {"tool": "extract_items", "internal": true, "args": {"unit_number": params.furnace_unit_number, "item": params.output_item, "count": output_count, "inventory_type": "furnace_result"}},
                    {"tool": "craft", "internal": true, "args": {"recipe": craft_recipe, "count": params.craft_count}, "skipped_if_empty_recipe": true},
                    {"tool": "verify_production", "args": {"x": furnace.position.x, "y": furnace.position.y, "radius": params.verify_radius}}
                ],
                "guidance": "If dry_run looks right, call bootstrap_smelting_once with dry_run=false once. Do not repeat it as production; use the resulting plates/component for build_fuel_supply, execute_direct_smelter, plan_machine_output/build_assembler_output, or assembler cells.",
            });
            let msg =
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
            return self.with_player_messages(msg).await;
        }

        let mut actions = Vec::new();
        let mut success = true;

        let walk = client.walk_to(furnace.position, false).await;
        success &= walk.is_ok();
        actions.push(serde_json::json!({
            "tool": "walk_to",
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
            Err(anyhow::anyhow!("walk_to failed"))
        };
        success &= fuel.is_ok();
        actions.push(serde_json::json!({
            "tool": "insert_items",
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
            "tool": "insert_items",
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
            "tool": "wait_ticks",
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
            "tool": "extract_items",
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
            success &= craft_result.is_ok();
            actions.push(serde_json::json!({
                "tool": "craft",
                "internal": true,
                "recipe": craft_recipe,
                "count": params.craft_count,
                "success": craft_result.is_ok(),
                "result": craft_result.as_ref().ok(),
                "error": craft_result.as_ref().err().map(|e| e.to_string()),
            }));
            if craft_result.is_ok() {
                let wait_craft = client.wait_for_crafting().await;
                success &= wait_craft.is_ok();
                actions.push(serde_json::json!({
                    "tool": "wait_for_crafting",
                    "internal": true,
                    "success": wait_craft.is_ok(),
                    "error": wait_craft.as_ref().err().map(|e| e.to_string()),
                }));
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
            "guidance": "This is a one-shot bootstrap, not durable production. Next call should build automation: build_fuel_supply for fuel, execute_direct_smelter for ore input, plan_machine_output/build_assembler_output for plate output, or plan/build assembler cells. Do not loop bootstrap_smelting_once as a production strategy.",
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
            match client.set_recipe(params.unit_number, "").await {
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
    #[tool(description = "Remove/mine an entity by its unit number.")]
    async fn remove_entity(&self, Parameters(params): Parameters<RemoveEntityParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client.remove_entity(params.unit_number).await {
            Ok(()) => "Entity removed successfully".to_string(),
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Rotate an existing entity by unit number.
    #[tool(
        description = "Rotate an existing entity by unit number. Use when place_entity/check_placement recommends rotate_entity for same-tile belts or other rotatable entities."
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
                    .with_player_messages(format!(
                    "Invalid direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                    params.direction
                ))
                    .await
            }
        };

        let result = match client
            .call_remote(
                "rotate_entity",
                &[
                    serde_json::json!(params.unit_number),
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

    async fn route_belt_core(
        &self,
        client: &mut FactorioClient,
        params: &RouteBeltParams,
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

        let mut collision_map = match client.build_collision_map(area).await {
            Ok(map) => map,
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
                        "error": format!("Error building collision map: {}", error),
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
                        "guidance": "Route is too large for one collision-map request. Call route_belt with next_segment.route_belt_args to extend the belt highway, then retry the original durable controller from next_segment.after_success.retry_from_* toward the final target. Do not call build_fuel_supply for intermediate waypoints because that would place the consumer inserter before fuel reaches it.",
                    }));
                }
                return Ok(route_belt_failure_json(
                    params,
                    "infrastructure_failure",
                    format!("Error building collision map: {}", e),
                ));
            }
        };

        let mut existing_belt_tiles: HashSet<GridPos> = HashSet::new();
        if params.extend_existing {
            collision_map.unblock(GridPos::new(params.from_x, params.from_y));
            collision_map.unblock(GridPos::new(params.to_x, params.to_y));
            let entities = match client.find_entities(area, None, None).await {
                Ok(entities) => entities,
                Err(e) => {
                    return Ok(route_belt_failure_json(
                        params,
                        "infrastructure_failure",
                        format!("Error checking existing route entities: {}", e),
                    ));
                }
            };
            for entity in entities {
                if !is_existing_belt_entity(&entity.name) {
                    continue;
                }
                if let Some(bb) = &entity.bounding_box {
                    let min_x = bb.left_top.x.floor() as i32;
                    let max_x = bb.right_bottom.x.ceil() as i32;
                    let min_y = bb.left_top.y.floor() as i32;
                    let max_y = bb.right_bottom.y.ceil() as i32;
                    for x in min_x..max_x {
                        for y in min_y..max_y {
                            let tile = GridPos::new(x, y);
                            existing_belt_tiles.insert(tile);
                            collision_map.unblock(tile);
                        }
                    }
                } else {
                    let tile = GridPos::from_position(&entity.position);
                    existing_belt_tiles.insert(tile);
                    collision_map.unblock(tile);
                }
            }
        }

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

        let routing_options = RoutingOptions {
            allow_underground: underground_config.is_some(),
            underground_config: underground_config.clone(),
            underground_penalty: 0.5,
            underground_skip_cost: 0.05,
        };
        let result = find_belt_route_with_options(
            GridPos::new(params.from_x, params.from_y),
            GridPos::new(params.to_x, params.to_y),
            &collision_map,
            &routing_options,
        );

        if !result.success {
            return Ok(route_belt_failure_json(
                params,
                "route_failed",
                format!(
                    "Route failed: {}",
                    result.error.unwrap_or_else(|| "unknown error".to_string())
                ),
            ));
        }

        let inventory = match client.character_inventory().await {
            Ok(inventory) => inventory,
            Err(e) => {
                return Ok(route_belt_failure_json(
                    params,
                    "infrastructure_failure",
                    format!("Error checking inventory: {}", e),
                ));
            }
        };
        let surface_belts_needed = result
            .belts
            .iter()
            .filter(|b| b.kind == BeltKind::Surface)
            .count() as u32;
        let underground_belts_needed = result
            .belts
            .iter()
            .filter(|b| b.kind != BeltKind::Surface)
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

        let full_route_belts = result.belts.clone();
        let mut build_belts = full_route_belts.clone();
        let mut partial_route = false;
        let mut partial_reason: Option<String> = None;
        let materials_sufficient = surface_belts_have >= surface_belts_needed
            && underground_belts_have >= underground_belts_needed;

        if !materials_sufficient {
            let underground_short =
                underground_belts_needed > 0 && underground_belts_have < underground_belts_needed;
            if underground_short && !params.dry_run {
                let ug_name = underground_belt_name.unwrap_or("underground-belt");
                return Ok(route_belt_failure_json(
                    params,
                    "insufficient_materials",
                    format!(
                        "Insufficient materials: need {} {}, have {}. Craft more underground belts first.",
                        underground_belts_needed, ug_name, underground_belts_have
                    ),
                ));
            }
            if surface_belts_have == 0 && !params.dry_run {
                return Ok(route_belt_failure_json(
                    params,
                    "insufficient_materials",
                    format!(
                        "Insufficient materials: need {} {}, have 0. Craft more belts first.",
                        surface_belts_needed, params.belt_type
                    ),
                ));
            }
            if !underground_short && surface_belts_have > 0 {
                let buildable_count = surface_belts_have.min(surface_belts_needed) as usize;
                build_belts = full_route_belts.into_iter().take(buildable_count).collect();
                partial_route = !params.dry_run;
                partial_reason = Some(format!(
                    "Only {} of {} required {} available; {} buildable prefix.",
                    surface_belts_have,
                    surface_belts_needed,
                    params.belt_type,
                    if params.dry_run {
                        "previewing"
                    } else {
                        "placing"
                    }
                ));
            }
        }

        let steps: Vec<serde_json::Value> = build_belts
            .iter()
            .map(|belt| {
                let entity_name = match belt.kind {
                    BeltKind::Surface => params.belt_type.as_str(),
                    BeltKind::UndergroundEntry | BeltKind::UndergroundExit => {
                        underground_belt_name.unwrap_or(params.belt_type.as_str())
                    }
                };
                serde_json::json!({
                    "tool": "place_entity",
                    "tool_args": {
                        "entity_name": entity_name,
                        "x": belt.position.x,
                        "y": belt.position.y,
                        "direction": belt.direction.to_factorio()
                    },
                    "kind": belt.kind,
                    "description": format!(
                        "Place {} at ({:.1},{:.1}) facing {:?}",
                        entity_name, belt.position.x, belt.position.y, belt.direction
                    )
                })
            })
            .collect();

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

        if params.dry_run {
            let mut response = serde_json::json!({
                "success": true,
                "dry_run": true,
                "from": { "x": params.from_x, "y": params.from_y },
                "to": { "x": params.to_x, "y": params.to_y },
                "belt_type": params.belt_type,
                "belt_count": result.belt_count,
                "buildable_belt_count": build_belts.len(),
                "turn_count": result.turn_count,
                "underground_count": result.underground_count,
                "materials": materials,
                "materials_sufficient": materials_sufficient,
                "partial_route_available": !materials_sufficient
                    && underground_belts_needed == 0
                    && surface_belts_have > 0,
                "partial_reason": partial_reason,
                "topology": result.topology,
            });
            if let Some(object) = response.as_object_mut() {
                if build_belts.len() <= 80 {
                    object.insert("steps".to_string(), serde_json::json!(steps));
                    object.insert("belts".to_string(), serde_json::json!(build_belts));
                } else {
                    let first_belts: Vec<_> = build_belts.iter().take(8).collect();
                    let mut last_belts: Vec<_> = build_belts.iter().rev().take(8).collect();
                    last_belts.reverse();
                    object.insert("steps_truncated".to_string(), serde_json::json!(true));
                    object.insert("belts_truncated".to_string(), serde_json::json!(true));
                    object.insert(
                        "preview".to_string(),
                        serde_json::json!({
                            "first_belts": first_belts,
                            "last_belts": last_belts,
                            "omitted_belts": build_belts.len().saturating_sub(16),
                        }),
                    );
                    object.insert(
                        "guidance".to_string(),
                        serde_json::json!(
                            "Route is long; response omits full step list. If materials are insufficient or placement reliability matters, build it in shorter route_belt segments, then use build_fuel_supply only for the final consumer feed."
                        ),
                    );
                }
            }
            return Ok(response);
        }

        let mut placed = 0;
        let mut skipped_existing = 0;
        let mut errors = Vec::new();
        let underground_entity = underground_config.as_ref().map(|c| c.entity_name.as_str());

        for belt in &build_belts {
            let belt_tile = GridPos::from_position(&belt.position);
            if existing_belt_tiles.contains(&belt_tile) {
                skipped_existing += 1;
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
                Ok(_) => placed += 1,
                Err(e) => errors.push(format!("({}, {}): {}", belt.position.x, belt.position.y, e)),
            }
        }

        Ok(serde_json::json!({
            "success": errors.is_empty(),
            "dry_run": false,
            "error_kind": if errors.is_empty() { serde_json::Value::Null } else { serde_json::json!("placement_failed") },
            "from": { "x": params.from_x, "y": params.from_y },
            "to": { "x": params.to_x, "y": params.to_y },
            "built_to": build_belts.last().map(|belt| serde_json::json!({
                "x": belt.position.x,
                "y": belt.position.y,
            })),
            "belt_type": params.belt_type,
            "belt_count": result.belt_count,
            "buildable_belt_count": build_belts.len(),
            "placed": placed,
            "skipped_existing": skipped_existing,
            "partial_route": partial_route,
            "partial_reason": partial_reason,
            "turn_count": result.turn_count,
            "underground_count": result.underground_count,
            "materials": materials,
            "materials_sufficient": materials_sufficient,
            "topology": result.topology,
            "errors": errors,
        }))
    }

    /// Route belts from point A to point B using A* pathfinding.
    #[tool(
        description = "Route belts from one position to another using A* pathfinding to avoid obstacles. \
        This is the recommended way to create belt connections. Use dry_run=true to preview the path before placing. \
        If the full surface-belt route is longer than available belts, this places the buildable prefix and reports built_to."
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
        let consumer = match client.get_entity(params.consumer_unit_number).await {
            Ok(entity) => entity,
            Err(e) => return Err(format!("Error: {}", e)),
        };
        let inserter_direction = match Direction::parse(&params.inserter_direction) {
            Some(direction) => direction,
            None => {
                return Err(format!(
                    "Invalid inserter_direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                    params.inserter_direction
                ));
            }
        };

        let route_params = RouteBeltParams {
            from_x: params.from_x,
            from_y: params.from_y,
            to_x: params.pickup_x,
            to_y: params.pickup_y,
            belt_type: params.belt_type.clone(),
            search_radius: params.search_radius,
            dry_run: params.dry_run,
            respect_zones: params.respect_zones,
            allow_underground: params.allow_underground,
            extend_existing: params.extend_existing,
        };

        let route = match self.route_belt_core(client, &route_params).await {
            Ok(report) => report,
            Err(e) => route_belt_failure_json(&route_params, "infrastructure_failure", e),
        };
        let route_success = route
            .get("success")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let route_materials_sufficient = route
            .get("materials_sufficient")
            .and_then(|value| value.as_bool())
            .unwrap_or(true);

        let inserter_args = serde_json::json!({
            "entity_name": params.inserter_name,
            "x": params.inserter_x,
            "y": params.inserter_y,
            "direction": params.inserter_direction,
        });

        if params.dry_run {
            return Ok(serde_json::json!({
                "success": route_success,
                "dry_run": true,
                "consumer": {
                    "unit_number": consumer.unit_number,
                    "name": consumer.name,
                    "position": consumer.position,
                },
                "route": route,
                "steps": [{
                    "tool": "route_belt",
                    "args": route_params,
                }, {
                    "tool": "place_entity",
                    "args": inserter_args,
                }, {
                    "tool": "verify_production",
                    "args": {
                        "x": consumer.position.x,
                        "y": consumer.position.y,
                        "radius": params.verify_radius,
                    },
                }],
                "guidance": if route_success && route_materials_sufficient {
                    "If route.materials_sufficient is true and inserter placement is clear, call build_fuel_supply again with dry_run=false."
                } else if route_success {
                    "Route is geometrically viable but materials are missing. If the missing item is the first inserter or belts needed to escape bootstrap, call bootstrap_smelting_once exactly once to make the first plates/component, then retry build_fuel_supply. Do not execute this build_fuel_supply yet."
                } else {
                    "Route planning failed. Follow route.guidance; for long coal-to-boiler paths, build shorter route_belt segments from the coal output toward the consumer, then call build_fuel_supply for the final consumer feed."
                },
            }));
        }

        if !route_success {
            return Ok(serde_json::json!({
                "success": false,
                "error_kind": route.get("error_kind")
                    .and_then(|value| value.as_str())
                    .unwrap_or("route_failed"),
                "dry_run": false,
                "consumer": {
                    "unit_number": consumer.unit_number,
                    "name": consumer.name,
                    "position": consumer.position,
                },
                "route": route,
                "inserter": {
                    "skipped": true,
                    "reason": "route planning failed",
                    "args": inserter_args,
                },
                "guidance": "No inserter placed because the fuel belt route failed. If route.next_segment is present, call route_belt with route.next_segment.route_belt_args, then retry build_fuel_supply for the final consumer feed.",
            }));
        }

        if !route_materials_sufficient {
            return Ok(serde_json::json!({
                "success": false,
                "error_kind": route.get("error_kind")
                    .and_then(|value| value.as_str())
                    .unwrap_or("insufficient_materials"),
                "dry_run": false,
                "consumer": {
                    "unit_number": consumer.unit_number,
                    "name": consumer.name,
                    "position": consumer.position,
                },
                "route": route,
                "inserter": {
                    "skipped": true,
                    "reason": "route materials insufficient",
                    "args": inserter_args,
                },
                "next_tool": {
                    "tool": "bootstrap_smelting_once",
                    "reason": "first inserter or belt materials are missing; make a bounded plate/component batch before durable fuel routing",
                },
                "guidance": "Do not test-place a fuel supply when route.materials_sufficient is false. If this is a first-inserter deadlock, call bootstrap_smelting_once exactly once, optionally craft_recipe=burner-inserter, then retry build_fuel_supply.",
            }));
        }

        let inserter_position = Position::new(params.inserter_x, params.inserter_y);
        let inserter = client
            .place_entity(&params.inserter_name, inserter_position, inserter_direction)
            .await;
        let placed_inserter_unit = inserter.as_ref().ok().and_then(|entity| entity.unit_number);
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
                    serde_json::json!({
                        "tool": "insert_items",
                        "unit_number": unit,
                        "item": params.inserter_fuel_item,
                        "count": params.inserter_fuel_count,
                        "inventory_type": "fuel",
                        "temporary_startup_buffer": true,
                        "success": inserted.is_ok(),
                        "error": inserted.as_ref().err().map(|e| e.to_string()),
                    })
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
        let inserter_report = serde_json::json!({
            "tool": "place_entity",
            "args": inserter_args,
            "success": inserter.is_ok(),
            "unit_number": placed_inserter_unit,
            "error": inserter.as_ref().err().map(|e| e.to_string()),
        });

        let verify_radius = params.verify_radius.clamp(1, 50) as f64;
        let verify_area = Area::new(
            consumer.position.x - verify_radius,
            consumer.position.y - verify_radius,
            consumer.position.x + verify_radius,
            consumer.position.y + verify_radius,
        );
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
        let placed_units: Vec<u32> = placed_inserter_unit.into_iter().collect();
        let (fuel_inserter_ready, placed_unit_statuses) =
            placed_units_not_dead(&verification, &placed_units);
        let repair_hint = automation_repair_hint(
            "build_fuel_supply",
            "durable fuel delivery",
            verification_call_ok,
            &verification,
            &placed_unit_statuses,
            Some(route_success),
        );
        let bootstrap_ok = bootstrap_fuel
            .get("skipped")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
            || bootstrap_fuel
                .get("success")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);

        let success = route_success
            && inserter.is_ok()
            && bootstrap_ok
            && verification_call_ok
            && fuel_inserter_ready;
        Ok(serde_json::json!({
            "success": success,
            "error_kind": if success { serde_json::Value::Null } else { serde_json::json!("placement_failed") },
            "placement_success": inserter.is_ok(),
            "dry_run": false,
            "consumer": {
                "unit_number": consumer.unit_number,
                "name": consumer.name,
                "position": consumer.position,
            },
            "route": route,
            "inserter": inserter_report,
            "bootstrap_fuel": bootstrap_fuel,
            "automation_verified": {
                "success": verification_call_ok && fuel_inserter_ready,
                "verification_call_ok": verification_call_ok,
                "fuel_inserter_ready": fuel_inserter_ready,
                "placed_unit_statuses": placed_unit_statuses,
            },
            "verification": verification,
            "repair_hint": repair_hint,
            "guidance": "If success is true, fuel delivery is built; rerun diagnose_fuel_sustainability and verify_production before moving on.",
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
            Ok(result) => result,
            Err(e) => return self.with_player_messages(e).await,
        };
        let msg = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(msg).await
    }

    /// Diagnose and repair the highest-priority missing durable fuel supply.
    #[tool(
        description = "One-call durable fuel repair. Diagnoses the nearby factory, selects the ranked fuel consumer with ready build_fuel_supply args, then routes coal and places the fuel inserter. Prefer this over manually calling insert_items or hand_feed_furnace when boilers/furnaces/burners run out of fuel. Use dry_run=true during planner turns."
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
        let mut selected_args = match ready_fuel_supply_args(&diagnosis) {
            Some(args) => args,
            None => {
                let result = serde_json::json!({
                    "success": false,
                    "dry_run": params.dry_run,
                    "selected": null,
                    "diagnosis": diagnosis,
                    "guidance": "No ready build_fuel_supply args were found. Build a coal source or clear inserter placement near the ranked consumer, then rerun repair_fuel_sustainability.",
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
            Err(e) => return self.with_player_messages(e).await,
        };
        let success = repair
            .get("success")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let result = serde_json::json!({
            "success": success,
            "dry_run": params.dry_run,
            "selected_build_fuel_supply_args": selected_args,
            "diagnosis": diagnosis,
            "repair": repair,
            "guidance": if success {
                "Durable fuel repair succeeded. Rerun diagnose_fuel_sustainability before manually touching fuel again."
            } else {
                "Durable fuel repair did not verify success. Follow repair.repair_hint or route.next_segment if present; do not fall back to repeated manual fuel insertion."
            },
        });
        let msg = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(msg).await
    }

    /// Build a science belt plus inserter feed for one lab.
    #[tool(
        description = "Execute a durable science-pack feed into a lab: route a science belt to the inserter pickup tile, place the inserter feeding the lab, then check research status. Prefer this over repeated feed_lab_from_inventory once science is on a belt or staged source. Use dry_run=true during planner turns."
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
                    .with_player_messages(format!(
                        "Invalid inserter_direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                        params.inserter_direction
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
            dry_run: params.dry_run,
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

        if params.dry_run {
            let result = serde_json::json!({
                "success": true,
                "dry_run": true,
                "lab": {
                    "unit_number": lab.unit_number,
                    "name": lab.name,
                    "position": lab.position,
                },
                "route": route,
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
                "guidance": "If route.materials_sufficient is true and inserter placement is clear, call build_lab_feed again with dry_run=false.",
            });
            let msg =
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
            return self.with_player_messages(msg).await;
        }

        let inserter_position = Position::new(params.inserter_x, params.inserter_y);
        let inserter = client
            .place_entity("inserter", inserter_position, inserter_direction)
            .await;
        let placed_inserter_unit = inserter.as_ref().ok().and_then(|entity| entity.unit_number);
        let inserter_report = serde_json::json!({
            "tool": "place_entity",
            "args": inserter_args,
            "success": inserter.is_ok(),
            "unit_number": placed_inserter_unit,
            "error": inserter.as_ref().err().map(|e| e.to_string()),
        });

        let verify_area = Area::new(
            lab.position.x - 8.0,
            lab.position.y - 8.0,
            lab.position.x + 8.0,
            lab.position.y + 8.0,
        );
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
        let placed_units: Vec<u32> = placed_inserter_unit.into_iter().collect();
        let (feed_inserter_ready, placed_unit_statuses) =
            placed_units_not_dead(&verification, &placed_units);
        let route_success = route
            .get("success")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let repair_hint = automation_repair_hint(
            "build_lab_feed",
            "science belt to lab feed",
            verification_call_ok,
            &verification,
            &placed_unit_statuses,
            Some(route_success),
        );

        let research_status = match client.call_remote("get_research_status", &[]).await {
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

        let success = route_success
            && inserter.is_ok()
            && verification_call_ok
            && feed_inserter_ready
            && research_status
                .get("success")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
        let result = serde_json::json!({
            "success": success,
            "placement_success": inserter.is_ok(),
            "dry_run": false,
            "lab": {
                "unit_number": lab.unit_number,
                "name": lab.name,
                "position": lab.position,
            },
            "route": route,
            "inserter": inserter_report,
            "automation_verified": {
                "success": verification_call_ok && feed_inserter_ready,
                "verification_call_ok": verification_call_ok,
                "feed_inserter_ready": feed_inserter_ready,
                "placed_unit_statuses": placed_unit_statuses,
            },
            "verification": verification,
            "research_status": research_status,
            "repair_hint": repair_hint,
            "guidance": "If success is true, lab feed infrastructure is built; ensure science production feeds this belt and research is selected.",
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
                    .with_player_messages(format!(
                        "Invalid inserter_direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                        params.inserter_direction
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
            dry_run: params.dry_run,
            respect_zones: params.respect_zones,
            allow_underground: params.allow_underground,
            extend_existing: params.extend_existing,
        };

        let route = match self.route_belt_core(&mut client, &route_params).await {
            Ok(report) => report,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

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

        if params.dry_run {
            let mut steps = Vec::new();
            if !params.recipe.trim().is_empty() {
                steps.push(serde_json::json!({
                    "tool": "set_recipe",
                    "args": recipe_args,
                }));
            }
            steps.push(serde_json::json!({
                "tool": "route_belt",
                "args": route_params,
            }));
            steps.push(serde_json::json!({
                "tool": "place_entity",
                "args": inserter_args,
            }));
            steps.push(serde_json::json!({
                "tool": "verify_production",
                "args": {
                    "x": assembler.position.x,
                    "y": assembler.position.y,
                    "radius": params.verify_radius,
                },
            }));
            let result = serde_json::json!({
                "success": true,
                "dry_run": true,
                "item_name": params.item_name.clone(),
                "assembler": {
                    "unit_number": assembler.unit_number,
                    "name": assembler.name,
                    "position": assembler.position,
                },
                "route": route,
                "steps": steps,
                "guidance": "If route.materials_sufficient is true and inserter placement is clear, call build_assembler_feed again with dry_run=false. Repeat once per recipe input belt.",
            });
            let msg =
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
            return self.with_player_messages(msg).await;
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
            serde_json::json!({
                "tool": "set_recipe",
                "args": recipe_args,
                "success": set_recipe.is_ok(),
                "error": set_recipe.as_ref().err().map(|e| e.to_string()),
            })
        };

        let inserter_position = Position::new(params.inserter_x, params.inserter_y);
        let inserter = client
            .place_entity("inserter", inserter_position, inserter_direction)
            .await;
        let placed_inserter_unit = inserter.as_ref().ok().and_then(|entity| entity.unit_number);
        let inserter_report = serde_json::json!({
            "tool": "place_entity",
            "args": inserter_args,
            "success": inserter.is_ok(),
            "unit_number": placed_inserter_unit,
            "error": inserter.as_ref().err().map(|e| e.to_string()),
        });

        let verify_radius = params.verify_radius.clamp(1, 50) as f64;
        let verify_area = Area::new(
            assembler.position.x - verify_radius,
            assembler.position.y - verify_radius,
            assembler.position.x + verify_radius,
            assembler.position.y + verify_radius,
        );
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
        let placed_units: Vec<u32> = placed_inserter_unit.into_iter().collect();
        let (feed_inserter_ready, placed_unit_statuses) =
            placed_units_not_dead(&verification, &placed_units);
        let route_success = route
            .get("success")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let repair_hint = automation_repair_hint(
            "build_assembler_feed",
            "item belt to assembler input feed",
            verification_call_ok,
            &verification,
            &placed_unit_statuses,
            Some(route_success),
        );

        let recipe_ok = recipe_report
            .get("skipped")
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
            || recipe_report
                .get("success")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);
        let success = route_success
            && recipe_ok
            && inserter.is_ok()
            && verification_call_ok
            && feed_inserter_ready;
        let result = serde_json::json!({
            "success": success,
            "placement_success": inserter.is_ok(),
            "dry_run": false,
            "item_name": params.item_name.clone(),
            "assembler": {
                "unit_number": assembler.unit_number,
                "name": assembler.name,
                "position": assembler.position,
            },
            "recipe": recipe_report,
            "route": route,
            "inserter": inserter_report,
            "automation_verified": {
                "success": verification_call_ok && feed_inserter_ready,
                "verification_call_ok": verification_call_ok,
                "feed_inserter_ready": feed_inserter_ready,
                "placed_unit_statuses": placed_unit_statuses,
            },
            "verification": verification,
            "repair_hint": repair_hint,
            "guidance": "If success is true, this assembler input feed is built. Add the other recipe input feeds, then route assembler output toward the lab feed.",
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
                    .with_player_messages(format!(
                        "Invalid inserter_direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                        params.inserter_direction
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
            dry_run: params.dry_run,
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

        if params.dry_run {
            let result = serde_json::json!({
                "success": true,
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
                "guidance": "If route.materials_sufficient is true and inserter placement is clear, call build_assembler_output again with dry_run=false. For furnace plates, use the target belt as the plate source for assembler feeds; for science output, connect the target belt to build_lab_feed.",
            });
            let msg =
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
            return self.with_player_messages(msg).await;
        }

        let inserter_position = Position::new(params.inserter_x, params.inserter_y);
        let inserter = client
            .place_entity("inserter", inserter_position, inserter_direction)
            .await;
        let placed_inserter_unit = inserter.as_ref().ok().and_then(|entity| entity.unit_number);
        let inserter_report = serde_json::json!({
            "tool": "place_entity",
            "args": inserter_args,
            "success": inserter.is_ok(),
            "unit_number": placed_inserter_unit,
            "error": inserter.as_ref().err().map(|e| e.to_string()),
        });

        let verify_radius = params.verify_radius.clamp(1, 50) as f64;
        let verify_area = Area::new(
            source_machine.position.x - verify_radius,
            source_machine.position.y - verify_radius,
            source_machine.position.x + verify_radius,
            source_machine.position.y + verify_radius,
        );
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
        let placed_units: Vec<u32> = placed_inserter_unit.into_iter().collect();
        let (output_inserter_ready, placed_unit_statuses) =
            placed_units_not_dead(&verification, &placed_units);
        let route_success = route
            .get("success")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        let repair_hint = automation_repair_hint(
            "build_assembler_output",
            "machine output belt",
            verification_call_ok,
            &verification,
            &placed_unit_statuses,
            Some(route_success),
        );

        let success =
            route_success && inserter.is_ok() && verification_call_ok && output_inserter_ready;
        let result = serde_json::json!({
            "success": success,
            "placement_success": inserter.is_ok(),
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
            "inserter": inserter_report,
            "automation_verified": {
                "success": verification_call_ok && output_inserter_ready,
                "verification_call_ok": verification_call_ok,
                "output_inserter_ready": output_inserter_ready,
                "placed_unit_statuses": placed_unit_statuses,
            },
            "verification": verification,
            "repair_hint": repair_hint,
            "guidance": "If success is true, machine output is on the routed belt. For furnace plates, use that belt as an assembler input source; for science packs, use build_lab_feed to consume from that belt.",
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

        let input_route = self.route_belt_core(&mut client, &input_route_params).await;
        let output_route = self
            .route_belt_core(&mut client, &output_route_params)
            .await;
        let routes = serde_json::json!({
            "input": match input_route {
                Ok(report) => report,
                Err(error) => serde_json::json!({"success": false, "error": error}),
            },
            "output": match output_route {
                Ok(report) => report,
                Err(error) => serde_json::json!({"success": false, "error": error}),
            },
        });
        let route_ready = routes
            .as_object()
            .map(|object| {
                object.values().all(|value| {
                    value
                        .get("success")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false)
                        && value
                            .get("materials_sufficient")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(false)
                })
            })
            .unwrap_or(false);

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
                    .with_player_messages(format!(
                        "Invalid input_inserter_direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                        params.input_inserter_direction
                    ))
                    .await;
            }
        };
        let output_direction = match Direction::parse(&params.output_inserter_direction) {
            Some(direction) => direction,
            None => {
                return self
                    .with_player_messages(format!(
                        "Invalid output_inserter_direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                        params.output_inserter_direction
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
            dry_run: params.dry_run,
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
            dry_run: params.dry_run,
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
                "success": true,
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

        let set_recipe = client
            .set_recipe(params.assembler_unit_number, params.recipe.trim())
            .await;
        let recipe_report = serde_json::json!({
            "tool": "set_recipe",
            "args": recipe_args,
            "success": set_recipe.is_ok(),
            "error": set_recipe.as_ref().err().map(|e| e.to_string()),
        });
        let input_inserter = client
            .place_entity(
                "inserter",
                Position::new(params.input_inserter_x, params.input_inserter_y),
                input_direction,
            )
            .await;
        let output_inserter = client
            .place_entity(
                "inserter",
                Position::new(params.output_inserter_x, params.output_inserter_y),
                output_direction,
            )
            .await;
        let input_inserter_unit = input_inserter
            .as_ref()
            .ok()
            .and_then(|entity| entity.unit_number);
        let output_inserter_unit = output_inserter
            .as_ref()
            .ok()
            .and_then(|entity| entity.unit_number);

        let verify_radius = params.verify_radius.clamp(1, 50) as f64;
        let verify_area = Area::new(
            assembler.position.x - verify_radius,
            assembler.position.y - verify_radius,
            assembler.position.x + verify_radius,
            assembler.position.y + verify_radius,
        );
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
        let placed_units: Vec<u32> = [input_inserter_unit, output_inserter_unit]
            .into_iter()
            .flatten()
            .collect();
        let (inserters_ready, placed_unit_statuses) =
            placed_units_not_dead(&verification, &placed_units);
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
            verification_call_ok,
            &verification,
            &placed_unit_statuses,
            Some(routes_success),
        );
        let success = set_recipe.is_ok()
            && routes_success
            && input_inserter.is_ok()
            && output_inserter.is_ok()
            && verification_call_ok
            && inserters_ready;

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
                "success": verification_call_ok && inserters_ready,
                "verification_call_ok": verification_call_ok,
                "inserters_ready": inserters_ready,
                "placed_unit_statuses": placed_unit_statuses,
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
        let route_success = routes
            .as_object()
            .map(|object| {
                object.values().all(|value| {
                    value
                        .get("success")
                        .and_then(|value| value.as_bool())
                        .unwrap_or(false)
                        && value
                            .get("materials_sufficient")
                            .and_then(|value| value.as_bool())
                            .unwrap_or(false)
                })
            })
            .unwrap_or(false);

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
                    .with_player_messages(format!(
                        "Invalid gear_inserter_direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                        params.gear_inserter_direction
                    ))
                    .await;
            }
        };
        let copper_direction = match Direction::parse(&params.copper_inserter_direction) {
            Some(direction) => direction,
            None => {
                return self
                    .with_player_messages(format!(
                        "Invalid copper_inserter_direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                        params.copper_inserter_direction
                    ))
                    .await;
            }
        };
        let output_direction = match Direction::parse(&params.output_inserter_direction) {
            Some(direction) => direction,
            None => {
                return self
                    .with_player_messages(format!(
                        "Invalid output_inserter_direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                        params.output_inserter_direction
                    ))
                    .await;
            }
        };
        let lab_direction = match Direction::parse(&params.lab_inserter_direction) {
            Some(direction) => direction,
            None => {
                return self
                    .with_player_messages(format!(
                        "Invalid lab_inserter_direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                        params.lab_inserter_direction
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
            dry_run: params.dry_run,
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
            dry_run: params.dry_run,
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
            dry_run: params.dry_run,
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
            dry_run: params.dry_run,
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
                "success": true,
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

        let set_recipe = client
            .set_recipe(params.assembler_unit_number, "automation-science-pack")
            .await;
        let recipe_report = serde_json::json!({
            "tool": "set_recipe",
            "args": recipe_args,
            "success": set_recipe.is_ok(),
            "error": set_recipe.as_ref().err().map(|e| e.to_string()),
        });

        let gear_inserter = client
            .place_entity(
                "inserter",
                Position::new(params.gear_inserter_x, params.gear_inserter_y),
                gear_direction,
            )
            .await;
        let copper_inserter = client
            .place_entity(
                "inserter",
                Position::new(params.copper_inserter_x, params.copper_inserter_y),
                copper_direction,
            )
            .await;
        let output_inserter = client
            .place_entity(
                "inserter",
                Position::new(params.output_inserter_x, params.output_inserter_y),
                output_direction,
            )
            .await;
        let lab_inserter = client
            .place_entity(
                "inserter",
                Position::new(params.lab_inserter_x, params.lab_inserter_y),
                lab_direction,
            )
            .await;
        let gear_inserter_unit = gear_inserter
            .as_ref()
            .ok()
            .and_then(|entity| entity.unit_number);
        let copper_inserter_unit = copper_inserter
            .as_ref()
            .ok()
            .and_then(|entity| entity.unit_number);
        let output_inserter_unit = output_inserter
            .as_ref()
            .ok()
            .and_then(|entity| entity.unit_number);
        let lab_inserter_unit = lab_inserter
            .as_ref()
            .ok()
            .and_then(|entity| entity.unit_number);

        let verify_radius = params.verify_radius.clamp(1, 50) as f64;
        let verify_area = Area::new(
            assembler.position.x.min(lab.position.x) - verify_radius,
            assembler.position.y.min(lab.position.y) - verify_radius,
            assembler.position.x.max(lab.position.x) + verify_radius,
            assembler.position.y.max(lab.position.y) + verify_radius,
        );
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
        let placed_units: Vec<u32> = [
            gear_inserter_unit,
            copper_inserter_unit,
            output_inserter_unit,
            lab_inserter_unit,
        ]
        .into_iter()
        .flatten()
        .collect();
        let (inserters_ready, placed_unit_statuses) =
            placed_units_not_dead(&verification, &placed_units);

        let research_status = match client.call_remote("get_research_status", &[]).await {
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
            verification_call_ok,
            &verification,
            &placed_unit_statuses,
            Some(routes_success),
        );
        let success = set_recipe.is_ok()
            && routes_success
            && gear_inserter.is_ok()
            && copper_inserter.is_ok()
            && output_inserter.is_ok()
            && lab_inserter.is_ok()
            && verification_call_ok
            && inserters_ready
            && research_status
                .get("success")
                .and_then(|value| value.as_bool())
                .unwrap_or(false);

        let result = serde_json::json!({
            "success": success,
            "placement_success": gear_inserter.is_ok()
                && copper_inserter.is_ok()
                && output_inserter.is_ok()
                && lab_inserter.is_ok(),
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
            "inserters": {
                "iron_gear_wheel": {
                    "tool": "place_entity",
                    "args": gear_inserter_args,
                    "success": gear_inserter.is_ok(),
                    "unit_number": gear_inserter_unit,
                    "error": gear_inserter.as_ref().err().map(|e| e.to_string()),
                },
                "copper_plate": {
                    "tool": "place_entity",
                    "args": copper_inserter_args,
                    "success": copper_inserter.is_ok(),
                    "unit_number": copper_inserter_unit,
                    "error": copper_inserter.as_ref().err().map(|e| e.to_string()),
                },
                "automation_science_pack_output": {
                    "tool": "place_entity",
                    "args": output_inserter_args,
                    "success": output_inserter.is_ok(),
                    "unit_number": output_inserter_unit,
                    "error": output_inserter.as_ref().err().map(|e| e.to_string()),
                },
                "lab_feed": {
                    "tool": "place_entity",
                    "args": lab_inserter_args,
                    "success": lab_inserter.is_ok(),
                    "unit_number": lab_inserter_unit,
                    "error": lab_inserter.as_ref().err().map(|e| e.to_string()),
                },
            },
            "automation_verified": {
                "success": verification_call_ok && inserters_ready,
                "verification_call_ok": verification_call_ok,
                "inserters_ready": inserters_ready,
                "placed_unit_statuses": placed_unit_statuses,
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
                    .with_player_messages(format!("Error getting belt contents: {}", e))
                    .await
            }
        };

        // Get entities for belt graph
        let entities = match client.find_entities(area, None, None).await {
            Ok(e) => e,
            Err(e) => {
                return self
                    .with_player_messages(format!("Error getting entities: {}", e))
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

        let result = match client.call_remote("get_research_status", &[]).await {
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
        description = "Validate or execute science-pack transfer from the agent inventory into a lab. Defaults to dry_run=true and returns a guarded feed_lab_from_inventory dry_run=false step. With dry_run=false it removes packs from the character inventory and inserts them into the lab_input inventory, returning explicit expected misses for missing packs or invalid lab inventories."
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

        let result = match client
            .call_remote("start_research", &[serde_json::json!(params.technology)])
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

        let result = match client
            .call_remote(
                "get_power_status",
                &[
                    serde_json::json!(params.x),
                    serde_json::json!(params.y),
                    serde_json::json!(params.radius),
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

        let result = match client
            .call_remote(
                "get_power_networks",
                &[
                    serde_json::json!(params.x),
                    serde_json::json!(params.y),
                    serde_json::json!(params.radius),
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

        let result = match client
            .call_remote(
                "find_power_issues",
                &[
                    serde_json::json!(params.x),
                    serde_json::json!(params.y),
                    serde_json::json!(params.radius),
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

        let result = match client
            .call_remote(
                "diagnose_steam_power",
                &[
                    serde_json::json!(params.x),
                    serde_json::json!(params.y),
                    serde_json::json!(params.radius),
                ],
            )
            .await
        {
            Ok(result) => result,
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

        let result = match client
            .call_remote(
                "get_alerts",
                &[
                    serde_json::json!(params.x),
                    serde_json::json!(params.y),
                    serde_json::json!(params.radius),
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
                Err(e) => return format!("Error connecting: {}", e),
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
            Err(e) => format!("Error saving zone: {}", e),
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
                    .map_or(true, |t| z.zone_type.to_string() == *t)
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
                    Err(e) => format!("Error saving: {}", e),
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
                Err(e) => format!("Error saving: {}", e),
            },
            None => format!("Zone '{}' not found", params.id),
        };
        self.with_player_messages(result).await
    }

    // === Resource Protection Tools ===

    /// Scan for resources and optionally protect them.
    #[tool(
        description = "Scan an area for resource patches (ore, oil) and save them as protected. \
        Protected resources will generate warnings when you try to place non-mining buildings on them."
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
                    .with_player_messages(format!("Error scanning: {}", e))
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
                        total_amount: r.total_amount as u64,
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
                    .with_player_messages(format!("Error saving memory: {}", e))
                    .await;
            }
        }

        let result = serde_json::json!({
            "resources_found": info.len(),
            "resources_saved": saved_count,
            "resources": info
        });

        let result_str =
            serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(result_str).await
    }

    /// Get all protected resources.
    #[tool(
        description = "List all protected resource patches that have been saved. \
        These are areas where only mining-related buildings should be placed."
    )]
    async fn get_protected_resources(&self) -> String {
        let memory = AgentMemory::load();

        let resources: Vec<serde_json::Value> = memory
            .protected_resources
            .iter()
            .map(|r| {
                serde_json::json!({
                    "resource_type": r.resource_type,
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
        description = "Check if placing an entity at a position is both policy-appropriate and actually placeable by Factorio. Returns policy_allowed and factorio_allowed separately."
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
                        "overlapping_resources": policy_check.overlapping_resources
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

        let result = serde_json::json!({
            "allowed": policy_check.allowed && factorio_allowed,
            "policy_allowed": policy_check.allowed,
            "factorio_allowed": factorio_allowed,
            "entity": params.entity_name,
            "position": { "x": params.x, "y": params.y },
            "direction": direction.to_factorio(),
            "warnings": policy_check.warnings,
            "errors": policy_check.errors,
            "overlapping_zones": policy_check.overlapping_zones,
            "overlapping_resources": policy_check.overlapping_resources,
            "factorio": factorio_check
        });

        let result_str =
            serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(result_str).await
    }

    /// Find a suitable empty area for building.
    #[tool(description = "Find a suitable empty area for a specific zone type. \
        Searches for space that doesn't overlap with protected resources or existing zones.")]
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
                    .with_player_messages(format!("Error getting entities: {}", e))
                    .await
            }
        };

        // Build a simple occupancy grid
        let width = params.width as i32;
        let height = params.height as i32;

        // Search in a spiral pattern from center
        let center_x = params.x;
        let center_y = params.y;

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

                    // Check for protected resource overlap
                    let has_resource = memory.resources_overlapping(&candidate).len() > 0;
                    if has_resource && params.zone_type != "mining" {
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

                    // Found a suitable area!
                    let result = serde_json::json!({
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
                        }
                    });
                    return self
                        .with_player_messages(
                            serde_json::to_string_pretty(&result).unwrap_or_default(),
                        )
                        .await;
                }
            }
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
        description = "Get only the immovable constraints in an area (terrain, resources, zones) without showing existing buildings. \
        Useful for thinking fresh about layout without being distracted by existing messy layouts."
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
                    .with_player_messages(format!("Error getting resources: {}", e))
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
            "tip": "This shows only immovable constraints. Plan your layout around these, then create zones before building."
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

#[tool_handler]
impl ServerHandler for FactorioMcp {
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
