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
    analyze_belt_reach, analyze_inserters, find_belt_gaps, find_belt_networks, BeltGraph,
};
use factorioctl::client::FactorioClient;
use factorioctl::world::{find_belt_route, Area, Direction, GridPos, Position, TilePos};

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
    /// Direction: 0=North, 2=East, 4=South, 6=West
    #[serde(default)]
    pub direction: u8,
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
}

fn default_belt_type() -> String { "transport-belt".to_string() }
fn default_search_radius() -> u32 { 10 }

/// Parameters for remove_entity tool
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct RemoveEntityParams {
    /// Entity unit number to remove
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

// === The MCP Server ===

/// The MCP server for Factorio control
#[derive(Clone)]
pub struct FactorioMcp {
    config: ConnectionConfig,
    #[allow(dead_code)]
    client: Arc<Mutex<Option<FactorioClient>>>,
    tool_router: ToolRouter<Self>,
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
}

#[tool_router]
impl FactorioMcp {
    // --- Query Tools ---

    /// Get all entities in an area. Returns entity names, positions, and types.
    #[tool(description = "Get all entities in an area. Returns entity names, positions, types, and unit numbers.")]
    async fn get_entities(&self, Parameters(params): Parameters<GetEntitiesParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return format!("Error: {}", e),
        };

        let area = Area {
            left_top: Position::new(params.x as f64 - params.radius as f64, params.y as f64 - params.radius as f64),
            right_bottom: Position::new(params.x as f64 + params.radius as f64, params.y as f64 + params.radius as f64),
        };

        match client.find_entities(area, None, params.name.as_deref()).await {
            Ok(entities) => {
                let info: Vec<serde_json::Value> = entities
                    .into_iter()
                    .map(|e| serde_json::json!({
                        "unit_number": e.unit_number,
                        "name": e.name,
                        "type": e.entity_type,
                        "x": e.position.x,
                        "y": e.position.y,
                        "direction": e.direction,
                    }))
                    .collect();
                serde_json::to_string_pretty(&info).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    /// Get resource patches (ore, oil) in an area.
    #[tool(description = "Get resource patches (ore, oil) in an area. Returns patch locations and amounts.")]
    async fn get_resources(&self, Parameters(params): Parameters<GetResourcesParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return format!("Error: {}", e),
        };

        let area = Area {
            left_top: Position::new(params.x as f64 - params.radius as f64, params.y as f64 - params.radius as f64),
            right_bottom: Position::new(params.x as f64 + params.radius as f64, params.y as f64 + params.radius as f64),
        };

        match client.find_resources(area, params.resource_type.as_deref()).await {
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
        }
    }

    /// Get current character status including position and health.
    #[tool(description = "Get current character status including position, health, and walking state.")]
    async fn get_character(&self) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return format!("Error: {}", e),
        };

        match client.character_status().await {
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
        }
    }

    /// Get character inventory contents.
    #[tool(description = "Get character inventory contents. Returns item names and counts.")]
    async fn get_inventory(&self) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return format!("Error: {}", e),
        };

        match client.character_inventory().await {
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
        }
    }

    /// Get current game tick.
    #[tool(description = "Get current game tick and elapsed time.")]
    async fn get_tick(&self) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return format!("Error: {}", e),
        };

        match client.get_tick().await {
            Ok(tick) => format!("Tick: {} ({:.1} seconds)", tick.tick, tick.to_seconds()),
            Err(e) => format!("Error: {}", e),
        }
    }

    // --- Analysis Tools ---

    /// Analyze belt reachability from a position.
    #[tool(description = "Analyze belt connectivity from a position. Shows all upstream (feeding) and downstream (fed) belts.")]
    async fn analyze_belt_reach(&self, Parameters(params): Parameters<BeltReachParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return format!("Error: {}", e),
        };

        let area = Area {
            left_top: Position::new(params.x as f64 - params.radius as f64, params.y as f64 - params.radius as f64),
            right_bottom: Position::new(params.x as f64 + params.radius as f64, params.y as f64 + params.radius as f64),
        };

        match client.find_entities(area, None, None).await {
            Ok(entities) => {
                let graph = BeltGraph::from_entities(&entities);
                let start = TilePos::new(params.x, params.y);

                match analyze_belt_reach(&graph, start) {
                    Some(result) => {
                        serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e))
                    }
                    None => format!("No belt found at ({}, {})", params.x, params.y),
                }
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    /// Find all connected belt networks in an area.
    #[tool(description = "Find all separate belt networks in an area. Shows network sizes and input/output counts.")]
    async fn analyze_belt_networks(&self, Parameters(params): Parameters<AreaParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return format!("Error: {}", e),
        };

        match client.find_entities(params.to_area(), None, None).await {
            Ok(entities) => {
                let graph = BeltGraph::from_entities(&entities);
                let result = find_belt_networks(&graph);
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    /// Find gaps in belt lines.
    #[tool(description = "Find gaps in belt lines - missing, misaligned, or blocked connections.")]
    async fn analyze_belt_gaps(&self, Parameters(params): Parameters<AreaParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return format!("Error: {}", e),
        };

        match client.find_entities(params.to_area(), None, None).await {
            Ok(entities) => {
                let graph = BeltGraph::from_entities(&entities);
                let result = find_belt_gaps(&graph, &entities);
                serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    /// Analyze inserters in an area.
    #[tool(description = "Analyze inserters - shows pickup/dropoff positions and what entities they interact with.")]
    async fn analyze_inserters(&self, Parameters(params): Parameters<AreaParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return format!("Error: {}", e),
        };

        match client.find_entities(params.to_area(), None, None).await {
            Ok(entities) => {
                let results = analyze_inserters(&entities);
                serde_json::to_string_pretty(&results).unwrap_or_else(|e| format!("Error: {}", e))
            }
            Err(e) => format!("Error: {}", e),
        }
    }

    // --- Action Tools ---

    /// Walk character to a position.
    #[tool(description = "Walk character to a position using pathfinding.")]
    async fn walk_to(&self, Parameters(params): Parameters<PositionParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return format!("Error: {}", e),
        };

        let position = Position::new(params.x, params.y);
        match client.walk_to(position, true).await {
            Ok(result) => serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e)),
            Err(e) => format!("Error: {}", e),
        }
    }

    /// Place an entity from character inventory.
    #[tool(description = "Place an entity from character inventory at a position.")]
    async fn place_entity(&self, Parameters(params): Parameters<PlaceEntityParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return format!("Error: {}", e),
        };

        let position = Position::new(params.x, params.y);
        let direction = Direction::from_factorio(params.direction);

        match client.place_entity(&params.entity_name, position, direction).await {
            Ok(result) => serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e)),
            Err(e) => format!("Error: {}", e),
        }
    }

    /// Mine entities at a position.
    #[tool(description = "Mine entities at a position. Character will walk there first if needed.")]
    async fn mine_at(&self, Parameters(params): Parameters<MineAtParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return format!("Error: {}", e),
        };

        let position = Position::new(params.x, params.y);
        match client.mine_at(position, params.count).await {
            Ok(result) => serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e)),
            Err(e) => format!("Error: {}", e),
        }
    }

    /// Craft items.
    #[tool(description = "Craft items using character's crafting ability.")]
    async fn craft(&self, Parameters(params): Parameters<CraftParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return format!("Error: {}", e),
        };

        match client.craft(&params.recipe, params.count).await {
            Ok(result) => serde_json::to_string_pretty(&result).unwrap_or_else(|e| format!("Error: {}", e)),
            Err(e) => format!("Error: {}", e),
        }
    }

    /// Insert items into an entity.
    #[tool(description = "Insert items from character inventory into an entity (furnace, chest, etc).")]
    async fn insert_items(&self, Parameters(params): Parameters<InsertItemsParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return format!("Error: {}", e),
        };

        match client.insert_items(params.unit_number, &params.item, params.count, &params.inventory_type).await {
            Ok(()) => format!("Inserted {} {} into entity", params.count, params.item),
            Err(e) => format!("Error: {}", e),
        }
    }

    /// Remove an entity.
    #[tool(description = "Remove/mine an entity by its unit number.")]
    async fn remove_entity(&self, Parameters(params): Parameters<RemoveEntityParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return format!("Error: {}", e),
        };

        match client.remove_entity(params.unit_number).await {
            Ok(()) => "Entity removed successfully".to_string(),
            Err(e) => format!("Error: {}", e),
        }
    }

    /// Route belts from point A to point B using A* pathfinding.
    #[tool(description = "Route belts from one position to another using A* pathfinding to avoid obstacles. \
        This is the recommended way to create belt connections. Use dry_run=true to preview the path before placing.")]
    async fn route_belt(&self, Parameters(params): Parameters<RouteBeltParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return format!("Error: {}", e),
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
        let collision_map = match client.build_collision_map(area).await {
            Ok(cm) => cm,
            Err(e) => return format!("Error building collision map: {}", e),
        };

        // Find path
        let start = GridPos::new(params.from_x, params.from_y);
        let goal = GridPos::new(params.to_x, params.to_y);
        let result = find_belt_route(start, goal, &collision_map);

        if !result.success {
            return format!("Route failed: {}", result.error.unwrap_or_else(|| "unknown error".to_string()));
        }

        if params.dry_run {
            return format!(
                "Dry run - would place {} belts with {} turns:\n{}",
                result.belt_count,
                result.turn_count,
                serde_json::to_string_pretty(&result.belts).unwrap_or_default()
            );
        }

        // Place the belts
        let mut placed = 0;
        let mut errors = Vec::new();

        for belt in &result.belts {
            match client.place_entity(&params.belt_type, belt.position, belt.direction).await {
                Ok(_) => placed += 1,
                Err(e) => errors.push(format!("({}, {}): {}", belt.position.x, belt.position.y, e)),
            }
        }

        if errors.is_empty() {
            format!("Successfully placed {} belts with {} turns", placed, result.turn_count)
        } else {
            format!(
                "Placed {}/{} belts. Errors:\n{}",
                placed,
                result.belt_count,
                errors.join("\n")
            )
        }
    }

    /// Execute raw Lua command.
    #[tool(description = "Execute a raw Lua command. Use with caution - for advanced operations.")]
    async fn execute_lua(&self, Parameters(params): Parameters<ExecuteLuaParams>) -> String {
        let mut client = match self.connect().await {
            Ok(c) => c,
            Err(e) => return format!("Error: {}", e),
        };

        match client.execute_lua(&params.lua).await {
            Ok(result) => result,
            Err(e) => format!("Error: {}", e),
        }
    }

    /// Broadcast a thought or message to the human player.
    #[tool(description = "Broadcast a thought or message to the human player. \
        Displays in-game (console and/or flying text) and speaks via TTS based on config. \
        Use this to communicate your thinking, status updates, or observations.")]
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
                    r#"local p = game.players[1] if p and p.character and p.character.valid then \
                    p.character.surface.create_entity{{name="flying-text", \
                    position={{p.character.position.x, p.character.position.y - 2}}, \
                    text="{}", color={{r=0.8,g=0.8,b=1}}}} end"#,
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

        if results.is_empty() {
            "No output enabled (check broadcast config in .factorioctl.json)".to_string()
        } else {
            results.join(", ")
        }
    }
}

#[tool_handler]
impl ServerHandler for FactorioMcp {}

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
