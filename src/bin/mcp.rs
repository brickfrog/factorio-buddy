//! MCP (Model Context Protocol) server for factorioctl
//!
//! Exposes Factorio control as MCP tools for LLM agents.

use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    schemars::{self, JsonSchema},
    tool, tool_handler, tool_router, ServerHandler, ServiceExt,
};
use serde::{Deserialize, Serialize};

use factorioctl::analyze::{
    analyze_belt_reach, analyze_inserters, detect_sushi_belts, find_belt_gaps, find_belt_networks,
    trace_belt_sources, BeltGraph,
};
use factorioctl::client::{lua::LuaCommand, AgentId, FactorioClient};
use factorioctl::memory::{AgentMemory, BeltRouting, ProtectedResource, Zone, ZoneType};
use factorioctl::world::{
    build_production_report, build_situation_report, find_belt_route_with_options, Area, BeltKind,
    Direction, GridPos, Position, RoutingOptions, TilePos, UndergroundConfig,
};

/// Connection configuration loaded from environment or config
#[derive(Clone)]
struct ConnectionConfig {
    host: String,
    port: u16,
    password: String,
}

#[cfg(test)]
mod tests {
    use super::{execute_lua_refusal, raw_lua_enabled};

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
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RouteBeltParams {
    /// Starting X coordinate (integer tile)
    pub from_x: i32,
    /// Starting Y coordinate (integer tile)
    pub from_y: i32,
    /// Destination X coordinate (integer tile)
    pub to_x: i32,
    /// Destination Y coordinate (integer tile)
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

        // First ensure the chat handler is registered
        let register_lua = factorioctl::client::lua::LuaCommand::register_chat_handler();
        let mut warning: Option<String> = None;
        if let Err(err) = client.execute_lua(&register_lua).await {
            eprintln!("Failed to register chat handler: {}", err);
            warning = Some(format!(
                "\n\n[warning: chat handler registration failed: {}]",
                err
            ));
        }

        // Then fetch and clear messages
        let fetch_lua = factorioctl::client::lua::LuaCommand::get_and_clear_chat_messages();
        let formatted_messages = match client.execute_lua(&fetch_lua).await {
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
            let lua = LuaCommand::get_power_coverage(center.x as i32, center.y as i32, radius);
            match client.execute_lua(&lua).await {
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
            let lua = LuaCommand::get_entity_drop_position(params.unit_number);

            let drop_result = match client.execute_lua(&lua).await {
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

        // For non-drill machines (furnaces, assemblers, etc.)
        // Calculate machine size from bounding box or use defaults
        let (width, height) = if let Some(bb) = &entity.bounding_box {
            let w = (bb.right_bottom.x - bb.left_top.x).round() as i32;
            let h = (bb.right_bottom.y - bb.left_top.y).round() as i32;
            (w, h)
        } else {
            // Default sizes for common machines
            match entity.name.as_str() {
                "stone-furnace" | "steel-furnace" | "electric-furnace" => (2, 2),
                "assembling-machine-1" | "assembling-machine-2" | "assembling-machine-3" => (3, 3),
                "chemical-plant" => (3, 3),
                "oil-refinery" => (5, 5),
                "lab" => (3, 3),
                _ => (1, 1),
            }
        };

        let cx = entity.position.x.floor() as i32;
        let cy = entity.position.y.floor() as i32;

        // Calculate positions based on machine size
        // For a 2x2 machine centered at (cx, cy):
        //   - Machine occupies tiles from (cx, cy) to (cx+1, cy+1)
        //   - South edge is at cy + height/2
        //   - North edge is at cy - height/2
        let half_h = height / 2;

        // Input belt goes 1 tile beyond south edge (for inserter gap)
        let input_belt_y = cy + half_h + 1;
        let input_inserter_y = cy + half_h; // On the south edge tile

        // Output belt goes 2 tiles beyond north edge (belt, then inserter, then furnace)
        // For furnace at cy=-107 with half_h=1: inserter at -109, belt at -110
        let output_belt_y = cy - half_h - 2;
        let output_inserter_y = cy - half_h - 1;

        let result = serde_json::json!({
            "entity_type": "machine",
            "machine": {
                "unit_number": entity.unit_number,
                "name": entity.name,
                "position": { "x": cx, "y": cy },
                "size": { "width": width, "height": height }
            },
            "input": {
                "belt_tile_y": input_belt_y,
                "inserter_tile_y": input_inserter_y,
                "inserter_direction": "south",  // Faces south to pick from belt, drops north to machine
                "description": format!(
                    "Place input belt at y={}, inserter at y={} facing south to pick from belt",
                    input_belt_y, input_inserter_y
                )
            },
            "output": {
                "belt_tile_y": output_belt_y,
                "inserter_tile_y": output_inserter_y,
                "inserter_direction": "south",  // Faces south to pick from machine, drops north to belt
                "description": format!(
                    "Place output belt at y={}, inserter at y={} facing south to pick from machine",
                    output_belt_y, output_inserter_y
                )
            },
            "routing_tip": format!(
                "For a row of furnaces at y={}: route input belt to y={}, route output belt to y={}",
                cy, input_belt_y, output_belt_y
            )
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
        description = "Read-only save-state debug probe for stuck agents. Returns current position, is_player_blocked collision diagnostics, dry-run unstuck recommendation, optional can_stand_at for the map center, and a local ASCII map with @ marking the character. Use when Doug is standing still, appears under an entity, or screenshots suggest the character is physically wedged."
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

    // --- Action Tools ---

    /// Walk character to a position.
    #[tool(
        description = "Walk character to a position using pathfinding. TIP: Call broadcast_thought in the SAME response to narrate your movement while walking."
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

        let lua = LuaCommand::place_entity(
            &client.agent_id().clone(),
            &params.entity_name,
            position,
            direction,
        );
        let result = match client.execute_lua(&lua).await {
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

        let lua = LuaCommand::get_recipe(&params.name);
        let result = match client.execute_lua(&lua).await {
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

        let lua = LuaCommand::rotate_entity(params.unit_number, direction.to_factorio());
        let result = match client.execute_lua(&lua).await {
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

    /// Route belts from point A to point B using A* pathfinding.
    #[tool(
        description = "Route belts from one position to another using A* pathfinding to avoid obstacles. \
        This is the recommended way to create belt connections. Use dry_run=true to preview the path before placing."
    )]
    async fn route_belt(&self, Parameters(params): Parameters<RouteBeltParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        // Calculate search area
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

        // Build collision map
        let mut collision_map = match client.build_collision_map(area).await {
            Ok(cm) => cm,
            Err(e) => {
                return self
                    .with_player_messages(format!("Error building collision map: {}", e))
                    .await
            }
        };

        // If extend_existing is enabled, unblock start/end positions if they have existing belts
        if params.extend_existing {
            let start_grid = GridPos::new(params.from_x, params.from_y);
            let goal_grid = GridPos::new(params.to_x, params.to_y);
            // Check if positions are currently blocked (e.g., by existing belts)
            // If so, unblock them to allow routing to connect to existing belts
            collision_map.unblock(start_grid);
            collision_map.unblock(goal_grid);
        }

        // Apply zone constraints if respect_zones is enabled
        if params.respect_zones {
            let memory = AgentMemory::load();
            for zone in memory.zones.values() {
                match zone.zone_type.belt_routing() {
                    BeltRouting::Blocked => collision_map.block_area(&zone.bounds),
                    BeltRouting::Preferred => collision_map.prefer_area(&zone.bounds),
                    BeltRouting::Allowed => {} // No change to collision map
                }
            }
        }

        // Determine underground config (only if requested AND tech is researched)
        let underground_config = if params.allow_underground {
            if let Some(config) = UndergroundConfig::from_belt_type(&params.belt_type) {
                // Check if required technology is researched
                match client.is_tech_researched(&config.required_tech).await {
                    Ok(true) => Some(config),
                    Ok(false) => None, // Tech not researched, fall back to surface only
                    Err(_) => None,    // Error checking tech, fall back to surface only
                }
            } else {
                None
            }
        } else {
            None
        };

        // Build routing options
        let routing_options = RoutingOptions {
            allow_underground: underground_config.is_some(),
            underground_config: underground_config.clone(),
            underground_penalty: 0.5,
            underground_skip_cost: 0.05,
        };

        // Find path
        let start = GridPos::new(params.from_x, params.from_y);
        let goal = GridPos::new(params.to_x, params.to_y);
        let result = find_belt_route_with_options(start, goal, &collision_map, &routing_options);

        if !result.success {
            return self
                .with_player_messages(format!(
                    "Route failed: {}",
                    result.error.unwrap_or_else(|| "unknown error".to_string())
                ))
                .await;
        }

        // Check inventory for required materials
        let inventory = match client.character_inventory().await {
            Ok(inv) => inv,
            Err(e) => {
                return self
                    .with_player_messages(format!("Error checking inventory: {}", e))
                    .await
            }
        };

        // Count surface belts and underground belts needed
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

        // Count belts in inventory
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

        if params.dry_run {
            let steps: Vec<serde_json::Value> = result
                .belts
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
                        "description": format!("Place {} at ({:.1},{:.1}) facing {:?}", entity_name, belt.position.x, belt.position.y, belt.direction)
                    })
                })
                .collect();
            let plan = serde_json::json!({
                "success": true,
                "dry_run": true,
                "from": { "x": params.from_x, "y": params.from_y },
                "to": { "x": params.to_x, "y": params.to_y },
                "belt_type": params.belt_type,
                "belt_count": result.belt_count,
                "turn_count": result.turn_count,
                "underground_count": result.underground_count,
                "materials": {
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
                },
                "materials_sufficient": surface_belts_have >= surface_belts_needed
                    && underground_belts_have >= underground_belts_needed,
                "topology": result.topology,
                "steps": steps,
                "belts": result.belts,
                "guidance": "If topology.connected is true and materials_sufficient is true, execute steps in order, then analyze_belt_reach or verify_production."
            });
            let msg =
                serde_json::to_string_pretty(&plan).unwrap_or_else(|e| format!("Error: {}", e));
            return self.with_player_messages(msg).await;
        }

        // Pre-check: fail early if not enough materials
        if surface_belts_have < surface_belts_needed {
            return self
                .with_player_messages(format!(
                    "Insufficient materials: need {} {}, have {}. Craft more belts first.",
                    surface_belts_needed, params.belt_type, surface_belts_have
                ))
                .await;
        }
        if underground_belts_needed > 0 && underground_belts_have < underground_belts_needed {
            let ug_name = underground_belt_name.unwrap_or("underground-belt");
            return self
                .with_player_messages(format!(
                "Insufficient materials: need {} {}, have {}. Craft more underground belts first.",
                underground_belts_needed, ug_name, underground_belts_have
            ))
                .await;
        }

        // Place the belts
        let mut placed = 0;
        let mut errors = Vec::new();

        // Get underground entity name if available
        let underground_entity = underground_config.as_ref().map(|c| c.entity_name.as_str());

        for belt in &result.belts {
            // Determine which entity to place based on belt kind
            let entity_name = match belt.kind {
                BeltKind::Surface => &params.belt_type,
                BeltKind::UndergroundEntry | BeltKind::UndergroundExit => {
                    underground_entity.unwrap_or(&params.belt_type)
                }
            };

            // For underground belts, we need to set the type (input vs output)
            // Factorio uses direction + type to distinguish entry/exit
            // Entry: direction points in flow direction, type = "input"
            // Exit: direction points in flow direction, type = "output"
            let place_result = if belt.kind == BeltKind::UndergroundEntry
                || belt.kind == BeltKind::UndergroundExit
            {
                let ug_type = match belt.kind {
                    BeltKind::UndergroundEntry => "input",
                    BeltKind::UndergroundExit => "output",
                    _ => "input",
                };
                // Place underground belt with type
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

        let mut result_msg = if errors.is_empty() {
            format!(
                "Successfully placed {} belts with {} turns",
                placed, result.turn_count
            )
        } else {
            format!(
                "Placed {}/{} belts. Errors:\n{}",
                placed,
                result.belt_count,
                errors.join("\n")
            )
        };
        if result.underground_count > 0 {
            result_msg.push_str(&format!(
                " ({} underground pairs)",
                result.underground_count
            ));
        }
        self.with_player_messages(result_msg).await
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

        let lua = factorioctl::client::lua::LuaCommand::get_research_status();
        let result = match client.execute_lua(&lua).await {
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

        let lua = factorioctl::client::lua::LuaCommand::get_available_research(client.agent_id());
        let result = match client.execute_lua(&lua).await {
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

        let lua = factorioctl::client::lua::LuaCommand::start_research(&params.technology);
        let result = match client.execute_lua(&lua).await {
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

        let lua = factorioctl::client::lua::LuaCommand::get_power_status(
            params.x,
            params.y,
            params.radius,
        );
        let result = match client.execute_lua(&lua).await {
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

        let lua = factorioctl::client::lua::LuaCommand::get_power_networks(
            params.x,
            params.y,
            params.radius,
        );
        let result = match client.execute_lua(&lua).await {
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

        let lua = factorioctl::client::lua::LuaCommand::find_power_issues(
            params.x,
            params.y,
            params.radius,
        );
        let result = match client.execute_lua(&lua).await {
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

        let lua = factorioctl::client::lua::LuaCommand::diagnose_steam_power(
            params.x,
            params.y,
            params.radius,
        );
        let result = match client.execute_lua(&lua).await {
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

        let lua =
            factorioctl::client::lua::LuaCommand::get_alerts(params.x, params.y, params.radius);
        let result = match client.execute_lua(&lua).await {
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
                let lua = factorioctl::client::lua::LuaCommand::broadcast_console(&params.message);
                if let Err(e) = client.execute_lua(&lua).await {
                    results.push(format!("Console error: {}", e));
                } else {
                    results.push("Console: displayed".to_string());
                }
            }

            if broadcast_config.flying_text {
                let lua =
                    factorioctl::client::lua::LuaCommand::broadcast_flying_text(&params.message);
                if let Err(e) = client.execute_lua(&lua).await {
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
        let lua = factorioctl::client::lua::LuaCommand::clear_area(
            client.agent_id(),
            area,
            params.clear_trees,
            params.clear_rocks,
            params.dry_run,
        );

        let result = match client.execute_lua(&lua).await {
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
