//! MCP (Model Context Protocol) server for factorioctl
//!
//! Exposes Factorio control as MCP tools for LLM agents.

use std::sync::Arc;
use tokio::sync::Mutex;

use rmcp::{
    ServerHandler, ServiceExt,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    schemars::{self, JsonSchema},
    tool, tool_handler, tool_router,
};
use serde::{Deserialize, Serialize};

use factorioctl::analyze::{
    analyze_belt_reach, analyze_inserters, detect_sushi_belts, find_belt_gaps, find_belt_networks,
    trace_belt_sources, BeltGraph,
};
use factorioctl::client::FactorioClient;
use factorioctl::memory::{AgentMemory, BeltRouting, ProtectedResource, Zone, ZoneType};
use factorioctl::world::{
    find_belt_route_with_options, Area, BeltKind, Direction, GridPos, Position, RoutingOptions,
    TilePos, UndergroundConfig,
};

/// Connection configuration loaded from environment or config
#[derive(Clone)]
struct ConnectionConfig {
    host: String,
    port: u16,
    password: String,
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

fn default_radius() -> u32 { 50 }

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

fn default_count() -> u32 { 1 }

/// Parameters for craft tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CraftParams {
    /// Recipe name (e.g., 'iron-gear-wheel')
    pub recipe: String,
    /// Number to craft
    #[serde(default = "default_count")]
    pub count: u32,
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

fn default_inventory_type() -> String { "chest".to_string() }

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

fn default_belt_type() -> String { "transport-belt".to_string() }
fn default_search_radius() -> u32 { 10 }

/// Parameters for remove_entity tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RemoveEntityParams {
    /// Entity unit number to remove
    pub unit_number: u32,
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

fn default_belt_radius() -> u32 { 30 }

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

fn default_power_radius() -> u32 { 50 }

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

fn default_save_as_protected() -> bool { true }

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
}

fn default_map_radius() -> u32 { 15 }

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

fn default_clear_trees() -> bool { true }
fn default_clear_rocks() -> bool { true }

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
        FactorioClient::connect(&self.config.host, self.config.port, &self.config.password)
            .await
            .map_err(|e| format!("Failed to connect: {}", e))
    }

    /// Fetch pending player messages and clear them from the queue.
    /// Returns formatted string if there are messages, None otherwise.
    async fn fetch_player_messages(&self) -> Option<String> {
        let mut client = self.connect().await.ok()?;

        // First ensure the chat handler is registered
        let register_lua = factorioctl::client::lua::LuaCommand::register_chat_handler();
        let _ = client.execute_lua(&register_lua).await;

        // Then fetch and clear messages
        let fetch_lua = factorioctl::client::lua::LuaCommand::get_and_clear_chat_messages();
        let response = client.execute_lua(&fetch_lua).await.ok()?;

        let messages: Vec<ChatMessage> = serde_json::from_str(&response).ok()?;

        if messages.is_empty() {
            None
        } else {
            let formatted: Vec<String> = messages
                .iter()
                .map(|m| format!("[{}]: {}", m.player, m.message))
                .collect();
            Some(format!("\n\n--- Player Messages ---\n{}", formatted.join("\n")))
        }
    }

    /// Append any pending player messages to a result string
    async fn with_player_messages(&self, result: String) -> String {
        match self.fetch_player_messages().await {
            Some(msgs) => format!("{}{}", result, msgs),
            None => result,
        }
    }
}

#[tool_router]
impl FactorioMcp {
    // --- Query Tools ---

    /// Get all entities in an area. Returns entity names, positions, and types.
    #[tool(description = "Get all entities in an area. Returns entity names, positions, types, and unit numbers. TIP: Don't scan excessively - trust your memory of recent scans.")]
    async fn get_entities(&self, Parameters(params): Parameters<GetEntitiesParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let area = Area {
            left_top: Position::new(params.x as f64 - params.radius as f64, params.y as f64 - params.radius as f64),
            right_bottom: Position::new(params.x as f64 + params.radius as f64, params.y as f64 + params.radius as f64),
        };

        let result = match client.find_entities(area, None, params.name.as_deref()).await {
            Ok(entities) => {
                let info: Vec<serde_json::Value> = entities
                    .into_iter()
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
                serde_json::to_string_pretty(&info).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Get belt and inserter positions for a machine.
    #[tool(description = "Get the correct belt and inserter positions for connecting to a machine. \
        For DRILLS: Returns the exact drop position (where items come out) and the tile where a belt should be placed. \
        For FURNACES/ASSEMBLERS: Returns input_belt, input_inserter, output_belt, output_inserter positions. \
        ALWAYS use this tool before routing belts to/from machines!")]
    async fn get_machine_belt_positions(&self, Parameters(params): Parameters<MachineBeltPositionsParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        // Get the entity
        let entity = match client.get_entity(params.unit_number).await {
            Ok(e) => e,
            Err(e) => return self.with_player_messages(format!("Error getting entity: {}", e)).await,
        };

        // Check if this is a mining drill - they have special drop position handling
        let is_drill = entity.name.contains("mining-drill");

        if is_drill {
            // For drills, query the actual drop_position from Factorio
            let lua = format!(
                r#"
                local e = game.get_entity_by_unit_number({})
                if e and e.valid and e.drop_position then
                    local dp = e.drop_position
                    local dir = e.direction
                    -- Calculate belt direction based on drill facing
                    -- Drill faces a direction, belt should run perpendicular or away
                    local belt_dir = dir  -- Belt runs in same direction as drill faces
                    rcon.print(game.table_to_json({{
                        drop_x = dp.x,
                        drop_y = dp.y,
                        drill_direction = dir,
                        belt_direction = belt_dir
                    }}))
                else
                    rcon.print(game.table_to_json({{error = "Entity not found or has no drop_position"}}))
                end
                "#,
                params.unit_number
            );

            let drop_result = match client.execute_lua(&lua).await {
                Ok(r) => r,
                Err(e) => return self.with_player_messages(format!("Error querying drop position: {}", e)).await,
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

                return self.with_player_messages(serde_json::to_string_pretty(&result).unwrap_or_default()).await;
            } else {
                // Lua failed - calculate output position from direction and size
                // Burner-mining-drills are 2x2, electric are 3x3
                let drill_size = if entity.name.contains("burner") { 2 } else { 3 };
                let half_size = drill_size / 2;
                let cx = entity.position.x.floor() as i32;
                let cy = entity.position.y.floor() as i32;

                // Calculate drop position based on direction
                // Direction: 0=N, 4=E, 8=S, 12=W
                let (drop_x, drop_y, dir_name) = match entity.direction {
                    0 => (cx, cy - half_size - 1, "North"),  // North
                    4 => (cx + half_size + 1, cy, "East"),   // East
                    8 => (cx, cy + half_size + 1, "South"),  // South
                    12 => (cx - half_size - 1, cy, "West"),  // West
                    _ => (cx + half_size + 1, cy, "East"),   // Default to east
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
                        "drop_position": { "x": drop_x, "y": drop_y },
                        "belt_tile": { "x": drop_x, "y": drop_y },
                        "belt_direction": entity.direction,
                        "description": format!(
                            "Place belt at tile ({}, {}) facing {} to catch drill output (calculated fallback)",
                            drop_x, drop_y, dir_name
                        )
                    },
                    "routing_tip": format!(
                        "To connect this drill: route_belt from_x={} from_y={} to_x=<destination> to_y=<destination>",
                        drop_x, drop_y
                    ),
                    "note": "Output position calculated from drill size and direction (Lua query failed)"
                });

                return self.with_player_messages(serde_json::to_string_pretty(&result).unwrap_or_default()).await;
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
        let input_inserter_y = cy + half_h;  // On the south edge tile

        // Output belt goes 1 tile beyond north edge (for inserter gap)
        let output_belt_y = cy - half_h - 1;
        let output_inserter_y = cy - half_h;  // On the north edge tile (adjusted from -1)

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

        self.with_player_messages(serde_json::to_string_pretty(&result).unwrap_or_default()).await
    }

    /// Render an ASCII map of an area.
    #[tool(description = "Render an ASCII map showing entities in an area. Returns a visual representation \
        useful for understanding layouts at a glance. Legend: @=you ^v<>=belt D=drill F=furnace A=assembler \
        i=inserter I=iron C=copper c=coal S=stone B=chest P=pole ~=water X=wreck o=rock")]
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
                Err(e) => return self.with_player_messages(format!("Error getting position: {}", e)).await,
            }
        };

        let r = params.radius as f64;
        let area = Area {
            left_top: Position::new(center.x - r, center.y - r),
            right_bottom: Position::new(center.x + r, center.y + r),
        };

        // Query entities in the area
        let entities = match client.find_entities(area, None, None).await {
            Ok(e) => e,
            Err(e) => return self.with_player_messages(format!("Error getting entities: {}", e)).await,
        };

        // Query tiles for water/terrain
        let tiles = client.get_tiles(area).await.unwrap_or_default();

        // Get character position for marking
        let char_pos = client.get_character_position().await.ok();

        // Parse detail level
        let detail = match params.detail.as_deref() {
            Some("minimal") => factorioctl::cli::DetailLevel::Minimal,
            Some("detailed") => factorioctl::cli::DetailLevel::Detailed,
            _ => factorioctl::cli::DetailLevel::Normal,
        };

        // Render the map
        let map = factorioctl::cli::render_ascii_map(&entities, &tiles, &center, params.radius, char_pos.as_ref(), detail);
        self.with_player_messages(map).await
    }

    /// Get resource patches (ore, oil) in an area.
    #[tool(description = "Get resource patches (ore, oil) in an area. Returns patch locations and amounts.")]
    async fn get_resources(&self, Parameters(params): Parameters<GetResourcesParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let area = Area {
            left_top: Position::new(params.x as f64 - params.radius as f64, params.y as f64 - params.radius as f64),
            right_bottom: Position::new(params.x as f64 + params.radius as f64, params.y as f64 + params.radius as f64),
        };

        let result = match client.find_resources(area, params.resource_type.as_deref()).await {
            Ok(resources) => {
                let info: Vec<serde_json::Value> = resources
                    .into_iter()
                    .map(|r| serde_json::json!({
                        "name": r.name,
                        "center_x": r.center.x,
                        "center_y": r.center.y,
                        "total_amount": r.total_amount,
                        "tile_count": r.tile_count,
                    }))
                    .collect();
                serde_json::to_string_pretty(&info).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Find the nearest resource patch of a specific type.
    #[tool(description = "Find the nearest resource patch (ore, oil) of a specific type from a position. \
        Returns the patch center, total amount, tile count, and bounding box. Searches within 200 tiles. \
        Use this to locate resources for mining operations.")]
    async fn find_nearest_resource(&self, Parameters(params): Parameters<FindNearestResourceParams>) -> String {
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
                Err(e) => return self.with_player_messages(format!("Error getting position: {}", e)).await,
            }
        };

        let result = match client.find_nearest_resource(&params.resource_type, from).await {
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
    #[tool(description = "Get current character status including position, health, and walking state. TIP: Only check when you need to - avoid over-verifying after every action.")]
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
                    .map(|i| serde_json::json!({
                        "name": i.name,
                        "count": i.count,
                    }))
                    .collect();
                serde_json::to_string_pretty(&items).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        };
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
    #[tool(description = "Analyze belt connectivity from a position. Shows all upstream (feeding) and downstream (fed) belts.")]
    async fn analyze_belt_reach(&self, Parameters(params): Parameters<BeltReachParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let area = Area {
            left_top: Position::new(params.x as f64 - params.radius as f64, params.y as f64 - params.radius as f64),
            right_bottom: Position::new(params.x as f64 + params.radius as f64, params.y as f64 + params.radius as f64),
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
    #[tool(description = "Find all separate belt networks in an area. Shows network sizes and input/output counts.")]
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
    #[tool(description = "Analyze inserters - shows pickup/dropoff positions and what entities they interact with.")]
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
    #[tool(description = "Walk character to a position using pathfinding. TIP: Call broadcast_thought in the SAME response to narrate your movement while walking.")]
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
    #[tool(description = "Place an entity from character inventory at a position.")]
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
                None => return self.with_player_messages(format!(
                    "Invalid direction '{}'. Use: north/n, east/e, south/s, west/w (or 0/4/8/12)",
                    params.direction
                )).await,
            }
        };

        let result = match client.place_entity(&params.entity_name, position, direction).await {
            Ok(r) => serde_json::to_string_pretty(&r).unwrap_or_else(|e| format!("Error: {}", e)),
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
            Ok(result) => serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e)),
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
            Ok(result) => serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e)),
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Insert items into an entity.
    #[tool(description = "Insert items from character inventory into an entity (furnace, chest, etc).")]
    async fn insert_items(&self, Parameters(params): Parameters<InsertItemsParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client.insert_items(params.unit_number, &params.item, params.count, &params.inventory_type).await {
            Ok(()) => format!("Inserted {} {} into entity", params.count, params.item),
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Extract items from an entity into player inventory.
    #[tool(description = "Extract items from an entity (furnace, chest, etc) into character inventory.")]
    async fn extract_items(&self, Parameters(params): Parameters<ExtractItemsParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let result = match client.extract_items(params.unit_number, &params.item, params.count, &params.inventory_type).await {
            Ok(extracted) => format!("Extracted {} {} from entity", extracted, params.item),
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Set recipe on a crafting machine.
    #[tool(description = "Set or clear the recipe on an assembling machine, chemical plant, or other crafting entity. Use empty string to clear the recipe.")]
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

    /// Route belts from point A to point B using A* pathfinding.
    #[tool(description = "Route belts from one position to another using A* pathfinding to avoid obstacles. \
        This is the recommended way to create belt connections. Use dry_run=true to preview the path before placing.")]
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
            Err(e) => return self.with_player_messages(format!("Error building collision map: {}", e)).await,
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
            return self.with_player_messages(format!("Route failed: {}", result.error.unwrap_or_else(|| "unknown error".to_string()))).await;
        }

        // Check inventory for required materials
        let inventory = match client.character_inventory().await {
            Ok(inv) => inv,
            Err(e) => return self.with_player_messages(format!("Error checking inventory: {}", e)).await,
        };

        // Count surface belts and underground belts needed
        let surface_belts_needed = result.belts.iter().filter(|b| b.kind == BeltKind::Surface).count() as u32;
        let underground_belts_needed = result.belts.iter().filter(|b| b.kind != BeltKind::Surface).count() as u32;

        // Count belts in inventory
        let surface_belts_have = inventory.items.iter()
            .find(|i| i.name == params.belt_type)
            .map(|i| i.count)
            .unwrap_or(0);

        let underground_belt_name = underground_config.as_ref().map(|c| c.entity_name.as_str());
        let underground_belts_have = underground_belt_name
            .and_then(|name| inventory.items.iter().find(|i| i.name == name))
            .map(|i| i.count)
            .unwrap_or(0);

        if params.dry_run {
            let mut msg = format!(
                "Dry run - would place {} belts with {} turns",
                result.belt_count,
                result.turn_count
            );
            if result.underground_count > 0 {
                msg.push_str(&format!(", {} underground pairs", result.underground_count));
            }
            // Show inventory status
            msg.push_str(&format!("\nInventory: {} {} (need {})",
                surface_belts_have, params.belt_type, surface_belts_needed));
            if underground_belts_needed > 0 {
                if let Some(ug_name) = underground_belt_name {
                    msg.push_str(&format!(", {} {} (need {})",
                        underground_belts_have, ug_name, underground_belts_needed));
                }
            }
            if surface_belts_have < surface_belts_needed || underground_belts_have < underground_belts_needed {
                msg.push_str("\nWARNING: INSUFFICIENT MATERIALS - route will fail");
            }
            msg.push_str(&format!("\n{}", serde_json::to_string_pretty(&result.belts).unwrap_or_default()));
            return self.with_player_messages(msg).await;
        }

        // Pre-check: fail early if not enough materials
        if surface_belts_have < surface_belts_needed {
            return self.with_player_messages(format!(
                "Insufficient materials: need {} {}, have {}. Craft more belts first.",
                surface_belts_needed, params.belt_type, surface_belts_have
            )).await;
        }
        if underground_belts_needed > 0 && underground_belts_have < underground_belts_needed {
            let ug_name = underground_belt_name.unwrap_or("underground-belt");
            return self.with_player_messages(format!(
                "Insufficient materials: need {} {}, have {}. Craft more underground belts first.",
                underground_belts_needed, ug_name, underground_belts_have
            )).await;
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
            let place_result = if belt.kind == BeltKind::UndergroundEntry || belt.kind == BeltKind::UndergroundExit {
                let ug_type = match belt.kind {
                    BeltKind::UndergroundEntry => "input",
                    BeltKind::UndergroundExit => "output",
                    _ => "input",
                };
                // Place underground belt with type
                client.place_underground_belt(entity_name, belt.position, belt.direction, ug_type).await
            } else {
                client.place_entity(entity_name, belt.position, belt.direction).await
            };

            match place_result {
                Ok(_) => placed += 1,
                Err(e) => errors.push(format!("({}, {}): {}", belt.position.x, belt.position.y, e)),
            }
        }

        let mut result_msg = if errors.is_empty() {
            format!("Successfully placed {} belts with {} turns", placed, result.turn_count)
        } else {
            format!(
                "Placed {}/{} belts. Errors:\n{}",
                placed,
                result.belt_count,
                errors.join("\n")
            )
        };
        if result.underground_count > 0 {
            result_msg.push_str(&format!(" ({} underground pairs)", result.underground_count));
        }
        self.with_player_messages(result_msg).await
    }

    /// Get belt contents with lane separation.
    #[tool(description = "Get items on transport belts with left/right lane separation. \
        Shows what items are on each lane of each belt, useful for diagnosing sushi belts or lane balancing issues.")]
    async fn get_belt_lane_contents(&self, Parameters(params): Parameters<BeltLaneContentsParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let area = Area {
            left_top: Position::new(params.x as f64 - params.radius as f64, params.y as f64 - params.radius as f64),
            right_bottom: Position::new(params.x as f64 + params.radius as f64, params.y as f64 + params.radius as f64),
        };

        let result = match client.get_belt_lane_contents(area).await {
            Ok(r) => serde_json::to_string_pretty(&r).unwrap_or_else(|e| format!("Error: {}", e)),
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Detect sushi belts (mixed items on same lane).
    #[tool(description = "Detect sushi belts - belts with multiple item types mixed on the same lane. \
        Also identifies lane-separated belts (different items on left vs right lane) and pure belts (single item type). \
        Detects circular loop networks common in sushi setups.")]
    async fn detect_sushi_belts(&self, Parameters(params): Parameters<SushiDetectParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let area = Area {
            left_top: Position::new(params.x as f64 - params.radius as f64, params.y as f64 - params.radius as f64),
            right_bottom: Position::new(params.x as f64 + params.radius as f64, params.y as f64 + params.radius as f64),
        };

        // Get belt lane contents
        let lane_contents = match client.get_belt_lane_contents(area).await {
            Ok(r) => r,
            Err(e) => return self.with_player_messages(format!("Error getting belt contents: {}", e)).await,
        };

        // Get entities for belt graph
        let entities = match client.find_entities(area, None, None).await {
            Ok(e) => e,
            Err(e) => return self.with_player_messages(format!("Error getting entities: {}", e)).await,
        };

        let graph = BeltGraph::from_entities(&entities);
        let result = detect_sushi_belts(&lane_contents, &graph);

        let result_str = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(result_str).await
    }

    /// Trace upstream sources for a belt.
    #[tool(description = "Trace upstream to find all sources (inserters, drills, other belts) that can feed items onto a belt. \
        Shows which lane each source feeds and detects circular loops. Useful for debugging why certain items appear on a belt.")]
    async fn trace_belt_sources(&self, Parameters(params): Parameters<BeltSourcesParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let area = Area {
            left_top: Position::new(params.x as f64 - params.radius as f64, params.y as f64 - params.radius as f64),
            right_bottom: Position::new(params.x as f64 + params.radius as f64, params.y as f64 + params.radius as f64),
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
    #[tool(description = "Get overall research status including current research progress, researched count, and research queue. \
        Also shows lab count, power status, and science packs currently in labs. \
        IMPORTANT: Research requires labs with power and science packs inserted - this tool shows if you're set up correctly.")]
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
    #[tool(description = "Get technologies that can be researched now (enabled, prerequisites met, not yet researched). \
        Returns name, ingredients (science packs needed), effects, and whether you're ready to research. \
        Shows 'ready' or 'blocked' status with specific blockers (no labs, no power, missing science packs). \
        IMPORTANT: To actually research you need: 1) Labs built, 2) Labs powered, 3) Science packs in labs.")]
    async fn get_available_research(&self) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let lua = factorioctl::client::lua::LuaCommand::get_available_research();
        let result = match client.execute_lua(&lua).await {
            Ok(result) => result,
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Start researching a technology.
    #[tool(description = "Queue a technology for research. Uses proper research queue (not cheating). \
        REQUIREMENTS: 1) Technology enabled with prerequisites met, 2) At least one lab built, \
        3) Lab connected to power, 4) Required science packs inserted into lab. \
        Will return specific error if any requirement is missing with guidance on what to do.")]
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
    #[tool(description = "Get power network status near a position. Returns network ID, connected pole info, \
        and power flow statistics (production/consumption).")]
    async fn get_power_status(&self, Parameters(params): Parameters<PowerStatusParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let lua = factorioctl::client::lua::LuaCommand::get_power_status(params.x, params.y, params.radius);
        let result = match client.execute_lua(&lua).await {
            Ok(result) => result,
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Get all power networks in an area.
    #[tool(description = "Find all electric power networks in an area. Returns network IDs and pole counts. \
        Useful for understanding power grid layout.")]
    async fn get_power_networks(&self, Parameters(params): Parameters<AreaParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let lua = factorioctl::client::lua::LuaCommand::get_power_networks(params.x, params.y, params.radius);
        let result = match client.execute_lua(&lua).await {
            Ok(result) => result,
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    // --- Alert Tools ---

    /// Get alerts for urgent conditions.
    #[tool(description = "Check for urgent conditions in an area: empty drills, entities without fuel, \
        machines without power/ingredients, nearby enemies. Useful for monitoring factory health.")]
    async fn get_alerts(&self, Parameters(params): Parameters<AlertsParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let lua = factorioctl::client::lua::LuaCommand::get_alerts(params.x, params.y, params.radius);
        let result = match client.execute_lua(&lua).await {
            Ok(result) => result,
            Err(e) => format!("Error: {}", e),
        };
        self.with_player_messages(result).await
    }

    /// Execute raw Lua command.
    #[tool(description = "Execute a raw Lua command. Use with caution - for advanced operations.")]
    async fn execute_lua(&self, Parameters(params): Parameters<ExecuteLuaParams>) -> String {
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
    async fn broadcast_thought(&self, Parameters(params): Parameters<BroadcastThoughtParams>) -> String {
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
                let escaped = params.message.replace('\\', "\\\\").replace('"', "\\\"");
                let lua = format!(r#"game.print("[Agent] {}")"#, escaped);
                if let Err(e) = client.execute_lua(&lua).await {
                    results.push(format!("Console error: {}", e));
                } else {
                    results.push("Console: displayed".to_string());
                }
            }

            if broadcast_config.flying_text {
                let escaped = params.message.replace('\\', "\\\\").replace('"', "\\\"");
                let lua = format!(
                    r#"
local player = game.players[1]
if player and player.character and player.character.valid then
    player.create_local_flying_text{{
        text = "{}",
        position = {{ player.character.position.x, player.character.position.y - 2 }},
        color = {{ r = 0.8, g = 0.8, b = 1.0 }},
        speed = 0.3,
        time_to_live = 300
    }}
end
"#,
                    escaped
                );
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
                let openai_key = tts_config.openai_api_key.clone()
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
                            let _ = cmd.stdout(Stdio::null()).stderr(Stdio::null()).status().await;
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
                                    "-s", "-X", "POST",
                                    "https://api.openai.com/v1/audio/speech",
                                    "-H", &format!("Authorization: Bearer {}", api_key),
                                    "-H", "Content-Type: application/json",
                                    "-d", &body.to_string(),
                                    "--output", "-",
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
                params.zone_type.as_ref().map_or(true, |t| {
                    z.zone_type.to_string() == *t
                })
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

        let result = serde_json::to_string_pretty(&zones).unwrap_or_else(|e| format!("Error: {}", e));
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
            })).unwrap_or_else(|e| format!("Error: {}", e)),
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
    #[tool(description = "Scan an area for resource patches (ore, oil) and save them as protected. \
        Protected resources will generate warnings when you try to place non-mining buildings on them.")]
    async fn scan_resources(&self, Parameters(params): Parameters<ScanResourcesParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let area = Area {
            left_top: Position::new(params.x as f64 - params.radius as f64, params.y as f64 - params.radius as f64),
            right_bottom: Position::new(params.x as f64 + params.radius as f64, params.y as f64 + params.radius as f64),
        };

        let resources = match client.find_resources(area, None).await {
            Ok(r) => r,
            Err(e) => return self.with_player_messages(format!("Error scanning: {}", e)).await,
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
                return self.with_player_messages(format!("Error saving memory: {}", e)).await;
            }
        }

        let result = serde_json::json!({
            "resources_found": info.len(),
            "resources_saved": saved_count,
            "resources": info
        });

        let result_str = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(result_str).await
    }

    /// Get all protected resources.
    #[tool(description = "List all protected resource patches that have been saved. \
        These are areas where only mining-related buildings should be placed.")]
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

        let result = serde_json::to_string_pretty(&resources).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(result).await
    }

    // === Layout Assistance Tools ===

    /// Check if a placement is appropriate.
    #[tool(description = "Check if placing an entity at a position is appropriate. \
        Validates against zones and protected resources. Use this before placing buildings to avoid bad locations.")]
    async fn check_placement(&self, Parameters(params): Parameters<CheckPlacementParams>) -> String {
        let memory = AgentMemory::load();
        let pos = Position::new(params.x, params.y);
        let check = memory.check_placement(&params.entity_name, &pos);

        let result = serde_json::json!({
            "allowed": check.allowed,
            "entity": params.entity_name,
            "position": { "x": params.x, "y": params.y },
            "warnings": check.warnings,
            "errors": check.errors,
            "overlapping_zones": check.overlapping_zones,
            "overlapping_resources": check.overlapping_resources
        });

        let result_str = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
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
            left_top: Position::new(params.x as f64 - params.radius as f64, params.y as f64 - params.radius as f64),
            right_bottom: Position::new(params.x as f64 + params.radius as f64, params.y as f64 + params.radius as f64),
        };

        // Get existing entities to avoid
        let entities = match client.find_entities(search_area, None, None).await {
            Ok(e) => e,
            Err(e) => return self.with_player_messages(format!("Error getting entities: {}", e)).await,
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
                    let has_entity = entities.iter().any(|e| {
                        candidate.contains(&e.position)
                    });

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
                    let has_incompatible_zone = overlapping_zones.iter().any(|z| {
                        z.zone_type == ZoneType::Reserved
                    });
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
                    return self.with_player_messages(
                        serde_json::to_string_pretty(&result).unwrap_or_default()
                    ).await;
                }
            }
        }

        let result = serde_json::json!({
            "found": false,
            "message": format!("No suitable {}x{} area found within radius {}", width, height, params.radius)
        });
        self.with_player_messages(serde_json::to_string_pretty(&result).unwrap_or_default()).await
    }

    /// Get a blank slate view of constraints only.
    #[tool(description = "Get only the immovable constraints in an area (terrain, resources, zones) without showing existing buildings. \
        Useful for thinking fresh about layout without being distracted by existing messy layouts.")]
    async fn get_blank_slate(&self, Parameters(params): Parameters<GetBlankSlateParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let memory = AgentMemory::load();
        let area = Area {
            left_top: Position::new(params.x as f64 - params.radius as f64, params.y as f64 - params.radius as f64),
            right_bottom: Position::new(params.x as f64 + params.radius as f64, params.y as f64 + params.radius as f64),
        };

        // Get resources in area
        let resources = match client.find_resources(area, None).await {
            Ok(r) => r,
            Err(e) => return self.with_player_messages(format!("Error getting resources: {}", e)).await,
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

        let result_str = serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e));
        self.with_player_messages(result_str).await
    }

    /// Clear trees and rocks in an area.
    #[tool(description = "Clear trees and rocks in a rectangular area to make space for building. \
        Use dry_run=true to preview what will be cleared before actually clearing.")]
    async fn clear_area(&self, Parameters(params): Parameters<ClearAreaParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return self.with_player_messages(format!("Error: {}", e)).await,
        };

        let area = Area::new(params.x1, params.y1, params.x2, params.y2);
        let lua = factorioctl::client::lua::LuaCommand::clear_area(
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
