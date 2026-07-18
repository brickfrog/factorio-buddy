//! Factorio client for communicating with the game server

pub mod lua;
pub mod rcon;
pub mod server;

use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use crate::world::{
    Area, BeltContentsResult, BeltLaneContentsResult, BeltLaneSummary, BuildResult,
    CharacterStatus, CollisionMap, CraftAdmissionRecord, CraftResult, CraftingQueueSnapshot,
    CraftingStatus, CraftingStatusEvidence, Direction, Entity, EntityProduction, GatherResult,
    GridPos, Inventory, InventoryItem, LaneContents, MineResult, PlacementSpec, Position,
    Prototype, Recipe, RecipeSummary, ResourcePatch, Surface, Tick, Tile, TilePos, WalkResult,
};
use rcon::RconClient;

/// Default wall-clock budget for observing a character crafting queue.
pub const DEFAULT_CRAFTING_WAIT_TIMEOUT: Duration = Duration::from_secs(120);
/// Default delay between character crafting queue observations.
pub const DEFAULT_CRAFTING_POLL_INTERVAL: Duration = Duration::from_millis(250);

type CraftingQueueFuture<'a> =
    Pin<Box<dyn Future<Output = Result<CraftingQueueSnapshot>> + Send + 'a>>;

#[derive(Debug, serde::Deserialize)]
struct WalkStatus {
    success: bool,
    #[serde(default)]
    walk_id: Option<u64>,
    #[serde(default)]
    active: bool,
    #[serde(default)]
    arrived: bool,
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    final_position: Option<Position>,
    #[serde(default)]
    distance_walked: f64,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct ReachStatus {
    success: bool,
    #[serde(default)]
    reachable: bool,
    #[serde(default)]
    target_position: Option<Position>,
    #[serde(default)]
    max_distance: Option<f64>,
    #[serde(default)]
    walk_arrival_distance: Option<f64>,
    #[serde(default)]
    error: Option<String>,
}

fn elapsed_millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn crafting_poll_status(queue_size: u32, timed_out: bool) -> CraftingStatus {
    if queue_size == 0 {
        CraftingStatus::Completed
    } else if timed_out {
        CraftingStatus::TimedOut
    } else {
        CraftingStatus::Pending
    }
}

fn classify_craft_completion(
    queue_drained: bool,
    admission_present: bool,
    product_proof_available: bool,
    products_verified: bool,
) -> (bool, &'static str, Option<&'static str>) {
    if queue_drained && products_verified {
        (true, "completed", None)
    } else if !queue_drained {
        (false, "timed_out", Some("crafting_timeout"))
    } else if !admission_present {
        (false, "unverified", Some("missing_craft_admission"))
    } else if !product_proof_available {
        (false, "unverified", Some("craft_completion_unverifiable"))
    } else {
        (false, "output_missing", Some("craft_output_missing"))
    }
}

fn craft_terminal_receipt_response(admission: &CraftAdmissionRecord) -> Value {
    let terminal_status = admission
        .terminal_status
        .as_deref()
        .unwrap_or("craft_terminal_status_missing");
    let completed = terminal_status == "completed";
    let queue_drained = completed
        || matches!(
            terminal_status,
            "craft_output_missing"
                | "craft_completion_unverifiable"
                | "craft_flow_accounting_unverifiable"
        );
    let status = if completed {
        "completed"
    } else {
        "terminal_failure"
    };
    json!({
        "success": completed,
        "completed": completed,
        "queue_drained": queue_drained,
        "status": status,
        "operation_id": admission.operation_id,
        "admission_persisted_in_save": true,
        "admission_cleared": true,
        "terminal_receipt_persisted": true,
        "receipt_replayed": true,
        "terminal_status": terminal_status,
        "error_kind": if completed { Value::Null } else { json!(terminal_status) },
        "error": if completed {
            Value::Null
        } else {
            json!(format!("craft operation previously ended with {terminal_status}"))
        },
        "identity_valid": admission.identity_valid,
        "identity_error": admission.identity_error,
        "flows": admission.flows,
    })
}

fn parse_crafting_queue_snapshot(response: &str) -> Result<CraftingQueueSnapshot> {
    ensure_lua_success(response)?;
    serde_json::from_str(response).map_err(|error| {
        anyhow::anyhow!("invalid crafting queue snapshot from Factorio: {error}: {response:?}")
    })
}

async fn poll_crafting_queue<C, F>(
    context: &mut C,
    timeout: Duration,
    poll_interval: Duration,
    mut read_queue_size: F,
) -> Result<CraftingStatusEvidence>
where
    F: for<'a> FnMut(&'a mut C) -> CraftingQueueFuture<'a>,
{
    if poll_interval.is_zero() {
        bail!("crafting poll interval must be greater than zero");
    }

    let started = Instant::now();
    let mut polls = 0_u32;
    let mut initial_queue_size = None;
    let mut remaining_queue_size = 0_u32;
    let mut current_recipe = None;
    let mut remaining_queue = Vec::new();

    loop {
        // Every remote queue observation is independently bounded by the RCON
        // client. Check the overall crafting deadline before starting another
        // one so polling itself cannot run indefinitely.
        if polls > 0 && started.elapsed() >= timeout {
            return Ok(CraftingStatusEvidence {
                status: CraftingStatus::TimedOut,
                recipe: None,
                accepted_count: 0,
                current_recipe,
                remaining_queue,
                initial_queue_size: initial_queue_size.unwrap_or(remaining_queue_size),
                remaining_queue_size,
                polls,
                elapsed_ms: elapsed_millis(started),
            });
        }

        let snapshot = read_queue_size(context).await?;
        remaining_queue_size = snapshot.queue_size;
        current_recipe = snapshot.current_recipe;
        remaining_queue = snapshot.queue;
        polls = polls.saturating_add(1);
        let initial_queue_size = *initial_queue_size.get_or_insert(remaining_queue_size);
        let timed_out = started.elapsed() >= timeout;
        let status = crafting_poll_status(remaining_queue_size, timed_out);
        let evidence = CraftingStatusEvidence {
            status,
            recipe: None,
            accepted_count: 0,
            current_recipe: current_recipe.clone(),
            remaining_queue: remaining_queue.clone(),
            initial_queue_size,
            remaining_queue_size,
            polls,
            elapsed_ms: elapsed_millis(started),
        };

        if status != CraftingStatus::Pending {
            return Ok(evidence);
        }

        let remaining_time = timeout.saturating_sub(started.elapsed());
        tokio::time::sleep(poll_interval.min(remaining_time)).await;
    }
}

/// Deserialize a `helpers.table_to_json` array response into a `Vec<T>`.
///
/// Factorio encodes an *empty* Lua table as the JSON object `{}` rather than an
/// array `[]` (Lua can't tell the two apart), so any query that finds nothing
/// returns `{}` and a plain `serde_json::from_str::<Vec<T>>` blows up with
/// `invalid type: map, expected a sequence`. Treat `{}`/empty as an empty vec;
/// anything else deserializes normally.
fn parse_lua_array<T: serde::de::DeserializeOwned>(response: &str) -> Result<Vec<T>> {
    let trimmed = response.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return Ok(Vec::new());
    }
    Ok(serde_json::from_str(trimmed)?)
}

/// High-level client for interacting with Factorio
#[derive(Clone)]
pub struct FactorioClient {
    rcon: Arc<Mutex<RconClient>>,
    /// Use /c instead of /silent-command (shows commands in console)
    debug_commands: bool,
    agent_id: AgentId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentId(String);

impl AgentId {
    pub fn new(raw: Option<&str>) -> Result<Self> {
        let normalized = match raw {
            Some(value) if !value.is_empty() => value,
            _ => "__player__",
        };

        let valid_len = (1..=64).contains(&normalized.len());
        let valid_chars = normalized
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'));
        if !valid_len || !valid_chars {
            bail!("invalid agent id");
        }

        Ok(Self(normalized.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_legacy(&self) -> bool {
        self.0 == "default" || self.0 == "__player__"
    }
}

#[derive(serde::Deserialize)]
struct LuaErrorResponse {
    error: Option<String>,
}

#[derive(serde::Deserialize)]
struct NearestMinableResponse {
    #[serde(default)]
    found: bool,
    position: Option<Position>,
    #[serde(default)]
    error: Option<String>,
}

fn ensure_lua_success(response: &str) -> Result<()> {
    if let Ok(parsed) = serde_json::from_str::<LuaErrorResponse>(response) {
        if let Some(error) = parsed.error {
            anyhow::bail!(error);
        }
    }
    Ok(())
}

fn parse_entity_response(response: &str) -> Result<Entity> {
    let value: serde_json::Value = serde_json::from_str(response)?;
    match value {
        serde_json::Value::Null => anyhow::bail!("Entity not found"),
        serde_json::Value::Object(ref obj) => {
            if let Some(error) = obj.get("error").and_then(|v| v.as_str()) {
                anyhow::bail!("{}", error);
            }
        }
        _ => {}
    }
    Ok(serde_json::from_value(value)?)
}

fn claude_command(fn_name: &str, args: &[Value]) -> String {
    let request = json!({
        "fn": fn_name,
        "args": args,
        "n": args.len(),
    });
    format!("/claude {}", request)
}

fn old_mod_claude_command_skew_response(fn_name: &str, response: &str) -> Option<String> {
    let normalized = response.to_ascii_lowercase();
    if !(normalized.contains("unknown command") && normalized.contains("claude")) {
        return None;
    }
    Some(
        json!({
            "success": false,
            "error_kind": "unknown_function",
            "error": format!("claude-interface mod does not expose /claude for {fn_name}"),
            "action_needed": "sync_or_restart_mod",
            "guidance": "Run just sync/resume so the updated claude-interface mod is loaded before using Factorio remotes.",
        })
        .to_string(),
    )
}

fn character_storage_key(agent_id: &AgentId) -> &str {
    if agent_id.is_legacy() {
        "__player__"
    } else {
        agent_id.as_str()
    }
}

impl FactorioClient {
    /// Connect to a Factorio server
    pub async fn connect(host: &str, port: u16, password: &str) -> Result<Self> {
        let mut rcon = RconClient::connect(host, port, password).await?;

        // Load config to check debug_commands setting
        let debug_commands = crate::config::Config::load()
            .map(|c| c.debug_commands)
            .unwrap_or(false);

        Ok(Self {
            rcon: Arc::new(Mutex::new(rcon)),
            debug_commands,
            agent_id: AgentId::new(None)?,
        })
    }

    pub fn with_agent_id(mut self, agent_id: AgentId) -> Self {
        self.agent_id = agent_id;
        self
    }

    pub fn agent_id(&self) -> &AgentId {
        &self.agent_id
    }

    /// Close the connection
    pub async fn close(&mut self) -> Result<()> {
        self.rcon.lock().await.close().await
    }

    /// Execute a Lua command (silent by default, verbose if debug_commands is enabled)
    pub async fn execute_lua(&mut self, lua: &str) -> Result<String> {
        // RCON doesn't handle newlines well, convert to single line
        let single_line: String = lua
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty() && !line.starts_with("--"))
            .collect::<Vec<_>>()
            .join(" ");
        let prefix = if self.debug_commands {
            "/c"
        } else {
            "/silent-command"
        };
        self.rcon
            .lock()
            .await
            .execute(&format!("{} {}", prefix, single_line))
            .await
    }

    /// Execute a Lua command with explicit visibility control
    pub async fn execute_lua_visible(&mut self, lua: &str, visible: bool) -> Result<String> {
        let single_line: String = lua
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty() && !line.starts_with("--"))
            .collect::<Vec<_>>()
            .join(" ");
        let prefix = if visible { "/c" } else { "/silent-command" };
        self.rcon
            .lock()
            .await
            .execute(&format!("{} {}", prefix, single_line))
            .await
    }

    pub async fn call_remote(&mut self, fn_name: &str, args: &[Value]) -> Result<String> {
        let command = claude_command(fn_name, args);
        let response = self.rcon.lock().await.execute(&command).await?;
        if let Some(skew_response) = old_mod_claude_command_skew_response(fn_name, &response) {
            return Ok(skew_response);
        }
        Ok(response)
    }

    // --- Game State Queries ---

    /// Get current game tick
    pub async fn get_tick(&mut self) -> Result<Tick> {
        let response = self.call_remote("get_tick", &[]).await?;
        ensure_lua_success(&response)?;
        Ok(serde_json::from_str(&response)?)
    }

    /// Get list of surfaces
    pub async fn get_surfaces(&mut self) -> Result<Vec<Surface>> {
        let response = self.call_remote("get_surfaces", &[]).await?;
        let surfaces = parse_lua_array::<Surface>(&response)?;
        Ok(surfaces)
    }

    // --- Entity Queries ---

    /// Find entities in an area
    pub async fn find_entities(
        &mut self,
        area: Area,
        entity_type: Option<&str>,
        name: Option<&str>,
    ) -> Result<Vec<Entity>> {
        let response = self
            .call_remote(
                "find_entities",
                &[
                    json!(area.left_top.x),
                    json!(area.left_top.y),
                    json!(area.right_bottom.x),
                    json!(area.right_bottom.y),
                    entity_type.map_or(Value::Null, |value| json!(value)),
                    name.map_or(Value::Null, |value| json!(value)),
                    json!(self.agent_id.as_str()),
                ],
            )
            .await?;
        let entities = parse_lua_array::<Entity>(&response)?;
        Ok(entities)
    }

    /// Get a specific entity by unit number
    pub async fn get_entity(&mut self, unit_number: u32) -> Result<Entity> {
        let response = self
            .call_remote("get_entity", &[json!(unit_number)])
            .await?;
        parse_entity_response(&response)
    }

    /// Get an entity's inventories
    pub async fn get_entity_inventory(&mut self, unit_number: u32) -> Result<serde_json::Value> {
        let response = self
            .call_remote("get_entity_inventory", &[json!(unit_number)])
            .await?;
        let result: serde_json::Value = serde_json::from_str(&response)?;
        Ok(result)
    }

    /// Verify production status for producing entities in an area
    pub async fn verify_production(&mut self, area: Area) -> Result<Vec<EntityProduction>> {
        let response = self
            .call_remote(
                "verify_production",
                &[
                    json!(area.left_top.x),
                    json!(area.left_top.y),
                    json!(area.right_bottom.x),
                    json!(area.right_bottom.y),
                    json!(self.agent_id.as_str()),
                ],
            )
            .await?;
        let entities = parse_lua_array::<EntityProduction>(&response)?;
        Ok(entities)
    }

    /// Diagnose ranked production blockers and likely root causes in an area.
    pub async fn diagnose_factory_blockers(
        &mut self,
        area: Area,
        limit: u32,
    ) -> Result<serde_json::Value> {
        let response = self
            .call_remote(
                "diagnose_factory_blockers",
                &[
                    json!(area.left_top.x),
                    json!(area.left_top.y),
                    json!(area.right_bottom.x),
                    json!(area.right_bottom.y),
                    json!(limit),
                    json!(self.agent_id.as_str()),
                ],
            )
            .await?;
        ensure_lua_success(&response)?;
        Ok(serde_json::from_str(&response)?)
    }

    /// Diagnose burnable fuel consumers and durable coal supply options in an area.
    pub async fn diagnose_fuel_sustainability(
        &mut self,
        area: Area,
        limit: u32,
    ) -> Result<serde_json::Value> {
        let response = self
            .call_remote(
                "diagnose_fuel_sustainability",
                &[
                    json!(area.left_top.x),
                    json!(area.left_top.y),
                    json!(area.right_bottom.x),
                    json!(area.right_bottom.y),
                    json!(limit),
                    json!(self.agent_id.as_str()),
                ],
            )
            .await?;
        Ok(serde_json::from_str(&response)?)
    }

    // --- Resource Queries ---

    /// Find resources in an area
    pub async fn find_resources(
        &mut self,
        area: Area,
        resource_type: Option<&str>,
    ) -> Result<Vec<ResourcePatch>> {
        let response = self
            .call_remote(
                "find_resources",
                &[
                    json!(area.left_top.x),
                    json!(area.left_top.y),
                    json!(area.right_bottom.x),
                    json!(area.right_bottom.y),
                    resource_type.map_or(Value::Null, |value| json!(value)),
                    json!(self.agent_id.as_str()),
                ],
            )
            .await?;
        let resources = parse_lua_array::<ResourcePatch>(&response)?;
        Ok(resources)
    }

    /// Find nearest resource from a position
    pub async fn find_nearest_resource(
        &mut self,
        resource_name: &str,
        from: Position,
    ) -> Result<ResourcePatch> {
        let response = self
            .call_remote(
                "find_nearest_resource",
                &[
                    json!(resource_name),
                    json!(from.x),
                    json!(from.y),
                    json!(self.agent_id.as_str()),
                ],
            )
            .await?;
        let resource: ResourcePatch = serde_json::from_str(&response)?;
        Ok(resource)
    }

    // --- Tile Queries ---

    /// Get tiles in an area
    pub async fn get_tiles(&mut self, area: Area) -> Result<Vec<Tile>> {
        let response = self
            .call_remote(
                "get_tiles",
                &[
                    json!(area.left_top.x),
                    json!(area.left_top.y),
                    json!(area.right_bottom.x),
                    json!(area.right_bottom.y),
                    json!(self.agent_id.as_str()),
                ],
            )
            .await?;
        let tiles = parse_lua_array::<Tile>(&response)?;
        Ok(tiles)
    }

    /// Get a specific tile
    pub async fn get_tile(&mut self, position: Position) -> Result<Tile> {
        let response = self
            .call_remote(
                "get_tile",
                &[
                    json!(position.x),
                    json!(position.y),
                    json!(self.agent_id.as_str()),
                ],
            )
            .await?;
        let tile: Tile = serde_json::from_str(&response)?;
        Ok(tile)
    }

    // --- Pathfinding Support ---

    /// Build a collision map and return the same authoritative entity snapshot
    /// used to construct it. Route planners use the snapshot to reserve live
    /// resource tiles without paying for a second large RCON query.
    pub async fn build_collision_map_with_entities(
        &mut self,
        area: Area,
    ) -> Result<(CollisionMap, Vec<Entity>)> {
        let mut collision_map = CollisionMap::new(area);

        // Query tiles for terrain obstacles (water, cliffs)
        let tiles = self.get_tiles(area).await?;
        for tile in tiles {
            if tile.collides_with_player {
                let grid_pos = GridPos::from_position(&tile.position);
                collision_map.block(grid_pos);
            }
        }

        // Query entities for structure obstacles
        let entities = self.find_entities(area, None, None).await?;
        for entity in &entities {
            if !entity_blocks_character_path(entity) {
                continue;
            }

            // Use actual bounding box if available, otherwise fall back to padding
            if let Some(bb) = &entity.bounding_box {
                // Block all tiles covered by the bounding box
                let min_x = bb.left_top.x.floor() as i32;
                let max_x = bb.right_bottom.x.ceil() as i32;
                let min_y = bb.left_top.y.floor() as i32;
                let max_y = bb.right_bottom.y.ceil() as i32;
                for x in min_x..max_x {
                    for y in min_y..max_y {
                        collision_map.block(GridPos::new(x, y));
                    }
                }
            } else {
                // Fallback: use hardcoded padding
                let padding = entity_collision_padding(&entity.name);
                let center = GridPos::from_position(&entity.position);
                for dx in -padding..=padding {
                    for dy in -padding..=padding {
                        collision_map.block(GridPos::new(center.x + dx, center.y + dy));
                    }
                }
            }
        }

        Ok((collision_map, entities))
    }

    /// Build a collision map for pathfinding in an area.
    pub async fn build_collision_map(&mut self, area: Area) -> Result<CollisionMap> {
        let (collision_map, _) = self.build_collision_map_with_entities(area).await?;
        Ok(collision_map)
    }

    // --- Prototype Queries ---

    /// Get a recipe by name
    pub async fn get_recipe(&mut self, name: &str) -> Result<Recipe> {
        let response = self.call_remote("get_recipe", &[json!(name)]).await?;
        let recipe: Recipe = serde_json::from_str(&response)?;
        Ok(recipe)
    }

    /// Get all recipes in a category
    pub async fn get_recipes_by_category(&mut self, category: &str) -> Result<Vec<RecipeSummary>> {
        let response = self
            .call_remote("get_recipes_by_category", &[json!(category)])
            .await?;
        let recipes = parse_lua_array::<RecipeSummary>(&response)?;
        Ok(recipes)
    }

    /// Get all recipes that produce a specific item
    pub async fn get_recipes_for_item(&mut self, item: &str) -> Result<Vec<Recipe>> {
        let response = self
            .call_remote("get_recipes_for_item", &[json!(item)])
            .await?;
        let recipes = parse_lua_array::<Recipe>(&response)?;
        Ok(recipes)
    }

    /// Get an entity prototype by name
    pub async fn get_prototype(&mut self, name: &str) -> Result<Prototype> {
        let response = self.call_remote("get_prototype", &[json!(name)]).await?;
        let prototype: Prototype = serde_json::from_str(&response)?;
        Ok(prototype)
    }

    // --- Native Blueprint Operations ---

    /// Create a native Factorio blueprint string from entities in an area
    pub async fn create_native_blueprint(
        &mut self,
        agent_id: &AgentId,
        area: Area,
    ) -> Result<crate::world::NativeBlueprintExport> {
        let response = self
            .call_remote(
                "create_native_blueprint",
                &[
                    json!(agent_id.as_str()),
                    json!(area.left_top.x),
                    json!(area.left_top.y),
                    json!(area.right_bottom.x),
                    json!(area.right_bottom.y),
                ],
            )
            .await?;
        if response.contains("\"error\"") {
            #[derive(serde::Deserialize)]
            struct ErrorResponse {
                error: String,
            }
            let err: ErrorResponse = serde_json::from_str(&response)?;
            anyhow::bail!("{}", err.error);
        }
        let result: crate::world::NativeBlueprintExport = serde_json::from_str(&response)?;
        Ok(result)
    }

    /// Save a blueprint to storage with a name
    pub async fn save_blueprint(
        &mut self,
        agent_id: &AgentId,
        name: &str,
        area: Area,
    ) -> Result<crate::world::BlueprintSaveResult> {
        let response = self
            .call_remote(
                "save_blueprint",
                &[
                    json!(agent_id.as_str()),
                    json!(name),
                    json!(area.left_top.x),
                    json!(area.left_top.y),
                    json!(area.right_bottom.x),
                    json!(area.right_bottom.y),
                ],
            )
            .await?;
        let result: crate::world::BlueprintSaveResult = serde_json::from_str(&response)?;
        Ok(result)
    }

    /// List all saved blueprints
    pub async fn list_blueprints(&mut self) -> Result<Vec<crate::world::StoredBlueprint>> {
        let response = self.call_remote("list_blueprints", &[]).await?;
        let result = parse_lua_array::<crate::world::StoredBlueprint>(&response)?;
        Ok(result)
    }

    /// Get a saved blueprint string by name
    pub async fn get_blueprint(&mut self, name: &str) -> Result<crate::world::BlueprintGetResult> {
        let response = self.call_remote("get_blueprint", &[json!(name)]).await?;
        let result: crate::world::BlueprintGetResult = serde_json::from_str(&response)?;
        Ok(result)
    }

    /// Place a saved blueprint at a position
    pub async fn place_blueprint(
        &mut self,
        agent_id: &AgentId,
        name: &str,
        position: Position,
        direction: u8,
    ) -> Result<crate::world::BlueprintPlaceResult> {
        let response = self
            .call_remote(
                "place_blueprint",
                &[
                    json!(agent_id.as_str()),
                    json!(name),
                    json!(position.x),
                    json!(position.y),
                    json!(direction),
                ],
            )
            .await?;
        let result: crate::world::BlueprintPlaceResult = serde_json::from_str(&response)?;
        Ok(result)
    }

    /// Import and place a blueprint from a string
    pub async fn import_blueprint(
        &mut self,
        agent_id: &AgentId,
        bp_string: &str,
        position: Position,
        direction: u8,
    ) -> Result<crate::world::BlueprintPlaceResult> {
        let response = self
            .call_remote(
                "import_blueprint",
                &[
                    json!(agent_id.as_str()),
                    json!(bp_string),
                    json!(position.x),
                    json!(position.y),
                    json!(direction),
                ],
            )
            .await?;
        let result: crate::world::BlueprintPlaceResult = serde_json::from_str(&response)?;
        Ok(result)
    }

    /// Delete a saved blueprint
    pub async fn delete_blueprint(&mut self, name: &str) -> Result<bool> {
        let response = self.call_remote("delete_blueprint", &[json!(name)]).await?;
        #[derive(serde::Deserialize)]
        struct DeleteResult {
            success: bool,
        }
        let result: DeleteResult = serde_json::from_str(&response)?;
        Ok(result.success)
    }

    // --- Character Control ---

    /// Initialize character entity
    pub async fn init_character(&mut self, x: f64, y: f64) -> Result<Entity> {
        let response = self
            .call_remote(
                "init_character",
                &[
                    json!(character_storage_key(&self.agent_id)),
                    json!(x),
                    json!(y),
                ],
            )
            .await?;
        let entity: Entity = serde_json::from_str(&response)?;
        Ok(entity)
    }

    /// Teleport character to position
    pub async fn teleport_character(&mut self, position: Position) -> Result<()> {
        let response = self
            .call_remote(
                "teleport_character",
                &[
                    json!(self.agent_id.as_str()),
                    json!(position.x),
                    json!(position.y),
                ],
            )
            .await?;
        ensure_lua_success(&response)?;
        Ok(())
    }

    /// Start walking character to position
    pub async fn walk_character(&mut self, position: Position) -> Result<()> {
        let response = self
            .call_remote(
                "set_walk_target",
                &[
                    json!(self.agent_id.as_str()),
                    json!(position.x),
                    json!(position.y),
                    Value::Null,
                ],
            )
            .await?;
        ensure_lua_success(&response)?;
        Ok(())
    }

    /// Get character status
    pub async fn character_status(&mut self) -> Result<CharacterStatus> {
        let response = self
            .call_remote("character_status", &[json!(self.agent_id.as_str())])
            .await?;
        let status: CharacterStatus = serde_json::from_str(&response)?;
        Ok(status)
    }

    /// Get character inventory
    pub async fn character_inventory(&mut self) -> Result<Inventory> {
        let response = self
            .call_remote("character_inventory", &[json!(self.agent_id.as_str())])
            .await?;
        let inventory: Inventory = serde_json::from_str(&response)?;
        Ok(inventory)
    }

    /// Check whether the character can stand at a world position.
    pub async fn can_stand_at(
        &mut self,
        position: Position,
        radius: u32,
    ) -> Result<serde_json::Value> {
        let response = self
            .call_remote(
                "can_stand_at",
                &[
                    json!(self.agent_id.as_str()),
                    json!(position.x),
                    json!(position.y),
                    json!(radius),
                ],
            )
            .await?;
        Ok(serde_json::from_str(&response)?)
    }

    /// Diagnose whether the current character position is blocked and suggest clear positions.
    pub async fn is_player_blocked(&mut self, radius: u32) -> Result<serde_json::Value> {
        let response = self
            .call_remote(
                "is_player_blocked",
                &[json!(self.agent_id.as_str()), json!(radius)],
            )
            .await?;
        Ok(serde_json::from_str(&response)?)
    }

    /// Move a physically blocked character to the nearest verified clear standing position.
    pub async fn unstuck(&mut self, radius: u32, dry_run: bool) -> Result<serde_json::Value> {
        let response = self
            .call_remote(
                "unstuck",
                &[json!(self.agent_id.as_str()), json!(radius), json!(dry_run)],
            )
            .await?;
        Ok(serde_json::from_str(&response)?)
    }

    // --- Mining ---

    /// Mine a natural entity or pick up loose items at an exact position.
    /// Walks to the target first if needed.
    pub async fn mine_at(&mut self, position: Position, count: u32) -> Result<MineResult> {
        self.approach_position(position, "resource").await?;

        let response = self
            .call_remote(
                "mine_at",
                &[
                    json!(self.agent_id.as_str()),
                    json!(position.x),
                    json!(position.y),
                    json!(count),
                    json!(0.5),
                ],
            )
            .await?;
        Ok(serde_json::from_str(&response)?)
    }

    /// Mine nearest entity of a type
    /// Walks to nearest entity and mines it
    pub async fn mine_nearest(&mut self, entity_type: &str, count: u32) -> Result<MineResult> {
        // Get initial inventory
        let inv_before = self.character_inventory().await?;
        let count_before: u32 = inv_before.items.iter().map(|i| i.count).sum();

        for _ in 0..count {
            let response = self
                .call_remote(
                    "find_nearest_minable",
                    &[
                        json!(self.agent_id.as_str()),
                        json!(entity_type),
                        json!(100),
                    ],
                )
                .await?;
            let nearest: NearestMinableResponse = serde_json::from_str(&response)?;
            if let Some(error) = nearest.error {
                anyhow::bail!(error);
            }
            if !nearest.found {
                break;
            }
            let Some(target_pos) = nearest.position else {
                break;
            };

            // Walk to and mine
            let result = self.mine_at(target_pos, 1).await?;
            if !result.success {
                break;
            }
        }

        // Get final inventory
        let inv_after = self.character_inventory().await?;
        let count_after: u32 = inv_after.items.iter().map(|i| i.count).sum();
        let items_gained = count_after.saturating_sub(count_before);

        Ok(MineResult {
            success: items_gained > 0,
            mined_count: items_gained,
            error: None,
            inventory: inv_after.items,
        })
    }

    // --- Crafting ---

    /// Start crafting a recipe
    pub async fn craft(&mut self, recipe: &str, count: u32) -> Result<CraftResult> {
        let response = self
            .call_remote(
                "craft",
                &[json!(self.agent_id.as_str()), json!(recipe), json!(count)],
            )
            .await?;
        let result: CraftResult = serde_json::from_str(&response)?;
        Ok(result)
    }

    /// Read the exact craft transaction persisted by the Factorio mod.
    ///
    /// A missing admission is an expected state after acknowledgement, while
    /// every other Lua or transport failure remains an operation error.
    pub async fn craft_admission_optional(&mut self) -> Result<Option<CraftAdmissionRecord>> {
        let response = self
            .call_remote("get_craft_admission", &[json!(self.agent_id.as_str())])
            .await?;
        let value: Value = serde_json::from_str(&response)?;
        if value.get("success").and_then(Value::as_bool) == Some(false)
            && value.get("error_kind").and_then(Value::as_str) == Some("missing_craft_admission")
        {
            return Ok(None);
        }
        ensure_lua_success(&response)?;
        Ok(Some(serde_json::from_value(value)?))
    }

    /// Read the exact craft transaction persisted by the Factorio mod,
    /// requiring one to exist.
    pub async fn craft_admission(&mut self) -> Result<CraftAdmissionRecord> {
        self.craft_admission_optional()
            .await?
            .ok_or_else(|| anyhow::anyhow!("no persisted craft admission for this agent"))
    }

    /// Clear one exact terminal craft transaction after its result was
    /// successfully observed by the caller.
    pub async fn clear_craft_admission(
        &mut self,
        operation_id: &str,
        terminal_status: &str,
    ) -> Result<Value> {
        let response = self
            .call_remote(
                "clear_craft_admission",
                &[
                    json!(self.agent_id.as_str()),
                    json!(operation_id),
                    json!(terminal_status),
                ],
            )
            .await?;
        ensure_lua_success(&response)?;
        Ok(serde_json::from_str(&response)?)
    }

    /// Account a complete production/consumption flow whose requested output
    /// was already proven by a standalone character inventory delta. Factorio
    /// consumes this generic production flow when evaluating craft-item
    /// technology triggers; the mod never mutates technologies.
    pub async fn record_verified_craft_flows(
        &mut self,
        operation_id: &str,
        flows: &serde_json::Value,
    ) -> Result<serde_json::Value> {
        let response = self
            .call_remote(
                "record_verified_craft_flows",
                &[
                    json!(self.agent_id.as_str()),
                    json!(operation_id),
                    flows.clone(),
                ],
            )
            .await?;
        Ok(serde_json::from_str(&response)?)
    }

    async fn crafting_queue_snapshot(&mut self) -> Result<CraftingQueueSnapshot> {
        let response = self
            .call_remote("wait_for_crafting", &[json!(self.agent_id.as_str())])
            .await?;
        parse_crafting_queue_snapshot(&response)
    }

    /// Observe the character crafting queue once.
    ///
    /// A non-empty queue is `Pending`; only an observed zero is `Completed`.
    pub async fn crafting_status(&mut self) -> Result<CraftingStatusEvidence> {
        let started = Instant::now();
        let snapshot = self.crafting_queue_snapshot().await?;
        Ok(CraftingStatusEvidence {
            status: crafting_poll_status(snapshot.queue_size, false),
            recipe: None,
            accepted_count: 0,
            current_recipe: snapshot.current_recipe,
            remaining_queue: snapshot.queue,
            initial_queue_size: snapshot.queue_size,
            remaining_queue_size: snapshot.queue_size,
            polls: 1,
            elapsed_ms: elapsed_millis(started),
        })
    }

    /// Poll until the character crafting queue is empty or `timeout` expires.
    ///
    /// This lower-level form returns structured `TimedOut` evidence instead of
    /// converting it into an error so callers such as MCP can report the exact
    /// remaining queue. Use [`Self::wait_for_crafting`] when timeout should be
    /// an operation error.
    pub async fn wait_for_crafting_with_options(
        &mut self,
        timeout: Duration,
        poll_interval: Duration,
    ) -> Result<CraftingStatusEvidence> {
        poll_crafting_queue(self, timeout, poll_interval, |client| {
            Box::pin(async move { client.crafting_queue_snapshot().await })
        })
        .await
    }

    /// Wait for verified crafting completion using the bounded defaults.
    ///
    /// Compatibility callers still receive a `Result`, but success now carries
    /// completion evidence and timeout is an error rather than false success.
    pub async fn wait_for_crafting(&mut self) -> Result<CraftingStatusEvidence> {
        let evidence = self
            .wait_for_crafting_with_options(
                DEFAULT_CRAFTING_WAIT_TIMEOUT,
                DEFAULT_CRAFTING_POLL_INTERVAL,
            )
            .await?;

        if !evidence.is_completed() {
            bail!(
                "timed out waiting for crafting after {} ms; {} queue entries remain after {} polls",
                evidence.elapsed_ms,
                evidence.remaining_queue_size,
                evidence.polls
            );
        }
        Ok(evidence)
    }

    async fn terminate_changed_craft_identity(
        &mut self,
        admission: &CraftAdmissionRecord,
    ) -> Value {
        let clear = self
            .clear_craft_admission(&admission.operation_id, "craft_character_changed")
            .await;
        let admission_cleared = clear.is_ok();
        let clear_error = clear.as_ref().err().map(ToString::to_string);
        json!({
            "success": false,
            "completed": false,
            "queue_drained": false,
            "status": "identity_changed",
            "operation_id": admission.operation_id,
            "admission_persisted_in_save": true,
            "admission_cleared": admission_cleared,
            "terminal_receipt_persisted": admission_cleared,
            "receipt_replayed": false,
            "terminal_status": "craft_character_changed",
            "error_kind": "craft_character_changed",
            "error": "the persisted craft belongs to a different or missing character context",
            "identity_valid": false,
            "identity_error": admission.identity_error,
            "clear_result": clear.ok(),
            "clear_error": clear_error,
        })
    }

    /// Complete the exact save-persisted craft transaction for this agent.
    ///
    /// Queue drain alone is not enough: this verifies the admitted recipe's
    /// deterministic inventory increase, records the complete produced and
    /// consumed item flow for standalone NPC characters, gives Factorio a
    /// bounded trigger-evaluation window, and only then acknowledges the
    /// admission. Timeouts and retryable accounting failures keep the
    /// admission in the save so a later MCP/CLI process can resume it.
    pub async fn complete_craft_admission_with_options(
        &mut self,
        timeout: Duration,
        poll_interval: Duration,
    ) -> Result<Value> {
        let mut admission = match self.craft_admission_optional().await? {
            Some(admission) => admission,
            None => {
                return Ok(json!({
                    "success": false,
                    "completed": false,
                    "queue_drained": false,
                    "status": "unverified",
                    "error_kind": "missing_craft_admission",
                    "error": "the crafting state has no matching save-persisted admission",
                    "admission_persisted_in_save": false,
                    "admission_cleared": false,
                    "terminal_receipt_persisted": false,
                    "receipt_replayed": false,
                }));
            }
        };
        if admission.completion_receipt {
            return Ok(craft_terminal_receipt_response(&admission));
        }
        if !admission.identity_valid {
            return Ok(self.terminate_changed_craft_identity(&admission).await);
        }

        let mut evidence = match self
            .wait_for_crafting_with_options(timeout, poll_interval)
            .await
        {
            Ok(evidence) => evidence,
            Err(error) => {
                if let Ok(Some(current)) = self.craft_admission_optional().await {
                    if current.operation_id == admission.operation_id
                        && !current.completion_receipt
                        && !current.identity_valid
                    {
                        return Ok(self.terminate_changed_craft_identity(&current).await);
                    }
                }
                return Ok(json!({
                    "success": false,
                    "completed": false,
                    "status": "observation_failed",
                    "error_kind": "craft_observation_failed",
                    "error": error.to_string(),
                    "admission_persisted_in_save": true,
                    "admission_cleared": false,
                    "terminal_receipt_persisted": false,
                    "receipt_replayed": false,
                    "operation_id": &admission.operation_id,
                }));
            }
        };
        evidence = evidence.with_craft_result(&admission.result);

        // Re-read the save-owned record after queue observation. This closes
        // the race where the character mapping, force, or surface changes
        // between admission and inventory/accounting verification.
        let current = match self.craft_admission_optional().await? {
            Some(current) => current,
            None => {
                return Ok(json!({
                    "success": false,
                    "completed": false,
                    "queue_drained": evidence.is_completed(),
                    "status": "unverified",
                    "error_kind": "missing_craft_admission",
                    "error": "the persisted craft admission disappeared before completion verification",
                    "operation_id": &admission.operation_id,
                    "admission_persisted_in_save": false,
                    "admission_cleared": false,
                    "terminal_receipt_persisted": false,
                    "receipt_replayed": false,
                    "evidence": evidence,
                }));
            }
        };
        if current.operation_id != admission.operation_id {
            return Ok(json!({
                "success": false,
                "completed": false,
                "queue_drained": evidence.is_completed(),
                "status": "operation_changed",
                "error_kind": "craft_operation_mismatch",
                "error": "a different craft transaction replaced the admitted operation",
                "operation_id": &admission.operation_id,
                "current_operation_id": &current.operation_id,
                "admission_persisted_in_save": true,
                "admission_cleared": false,
                "terminal_receipt_persisted": current.completion_receipt,
                "receipt_replayed": false,
                "evidence": evidence,
            }));
        }
        if current.completion_receipt {
            return Ok(craft_terminal_receipt_response(&current));
        }
        if !current.identity_valid {
            return Ok(self.terminate_changed_craft_identity(&current).await);
        }
        admission = current;

        let queue_drained = evidence.is_completed();
        let inventory_after = match self.character_inventory().await {
            Ok(inventory) => Some(inventory),
            Err(error) => {
                return Ok(json!({
                    "success": false,
                    "completed": false,
                    "queue_drained": queue_drained,
                    "status": "observation_failed",
                    "error_kind": "craft_inventory_observation_failed",
                    "error": error.to_string(),
                    "operation_id": &admission.operation_id,
                    "admission_persisted_in_save": true,
                    "admission_cleared": false,
                    "terminal_receipt_persisted": false,
                    "receipt_replayed": false,
                    "evidence": evidence,
                }));
            }
        };
        let product_evidence: Vec<Value> = admission
            .products
            .iter()
            .map(|product| {
                let observed_after = inventory_after
                    .as_ref()
                    .map(|inventory| inventory.get_count(&product.name));
                let observed_increase =
                    observed_after.map(|count| count.saturating_sub(product.before_count));
                json!({
                    "name": product.name,
                    "before_count": product.before_count,
                    "expected_increase": product.expected_increase,
                    "expected_after_minimum": product.before_count.saturating_add(product.expected_increase),
                    "observed_after": observed_after,
                    "observed_increase": observed_increase,
                    "satisfied": observed_increase.is_some_and(|increase| increase >= product.expected_increase),
                })
            })
            .collect();
        let product_proof_available = admission.product_proof_complete
            && !admission.products.is_empty()
            && inventory_after.is_some();
        let products_verified = product_proof_available
            && product_evidence
                .iter()
                .all(|product| product.get("satisfied").and_then(Value::as_bool) == Some(true));
        let flow_proof_available =
            admission.flow_accounting_complete && !admission.flows.is_empty();

        let mut accounting = Value::Null;
        let mut accounting_verified = false;
        let mut trigger_evaluation_tick_observed = false;
        if queue_drained && products_verified && flow_proof_available {
            let flows = serde_json::to_value(&admission.flows).unwrap_or_default();
            match self
                .record_verified_craft_flows(&admission.operation_id, &flows)
                .await
            {
                Ok(result) => {
                    accounting_verified = result.get("success").and_then(Value::as_bool)
                        == Some(true)
                        && result.get("accounted").and_then(Value::as_bool) == Some(true);
                    // Factorio evaluates craft-item research triggers
                    // asynchronously after production accounting. Allow a
                    // conservative full-second window, plus one tick, for
                    // the engine-owned evaluation to run.
                    const TRIGGER_EVALUATION_TICKS: u32 = 61;
                    let tick_result = if accounting_verified {
                        self.wait_ticks(TRIGGER_EVALUATION_TICKS).await
                    } else {
                        Err(anyhow::anyhow!("craft flow accounting was rejected"))
                    };
                    trigger_evaluation_tick_observed = tick_result.is_ok();
                    accounting = json!({
                        "result": result,
                        "trigger_evaluation_ticks": TRIGGER_EVALUATION_TICKS,
                        "tick_advanced": trigger_evaluation_tick_observed,
                        "tick_error": tick_result.err().map(|error| error.to_string()),
                    });
                }
                Err(error) => {
                    accounting = json!({
                        "success": false,
                        "error": error.to_string(),
                    });
                }
            }
        }

        let (mut completed, mut status, mut error_kind) = classify_craft_completion(
            queue_drained,
            true,
            product_proof_available,
            products_verified,
        );
        if completed && !flow_proof_available {
            completed = false;
            status = "accounting_unverifiable";
            error_kind = Some("craft_flow_accounting_unverifiable");
        } else if completed && !accounting_verified {
            completed = false;
            status = "accounting_failed";
            error_kind = Some("craft_accounting_failed");
        } else if completed && !trigger_evaluation_tick_observed {
            completed = false;
            status = "accounting_pending";
            error_kind = Some("craft_trigger_evaluation_pending");
        }

        let terminal_without_retry = queue_drained
            && matches!(
                error_kind,
                Some("craft_output_missing")
                    | Some("craft_completion_unverifiable")
                    | Some("craft_flow_accounting_unverifiable")
            );
        let mut admission_cleared = false;
        let mut clear_error = None;
        let terminal_status = if completed {
            Some("completed")
        } else if terminal_without_retry {
            error_kind
        } else {
            None
        };
        let mut clear_result = None;
        if let Some(terminal_status) = terminal_status {
            match self
                .clear_craft_admission(&admission.operation_id, terminal_status)
                .await
            {
                Ok(result) => {
                    admission_cleared = true;
                    clear_result = Some(result);
                }
                Err(error) => clear_error = Some(error.to_string()),
            }
        }
        if completed && !admission_cleared {
            completed = false;
            status = "acknowledgement_failed";
            error_kind = Some("craft_admission_clear_failed");
        }

        let timeout_seconds = timeout.as_secs();
        let error = match error_kind {
            None => Value::Null,
            Some("crafting_timeout") => json!(format!(
                "crafting did not complete within {} seconds; {} queue entries remain",
                timeout_seconds, evidence.remaining_queue_size
            )),
            Some("missing_craft_admission") => {
                json!("the crafting state has no matching save-persisted admission")
            }
            Some("craft_completion_unverifiable") => json!(
                "the queue is empty, but deterministic requested-product evidence is unavailable"
            ),
            Some("craft_output_missing") => json!(
                "the queue drained without the admitted craft's expected inventory increase; it may have been cancelled"
            ),
            Some("craft_flow_accounting_unverifiable") => json!(
                "the craft output exists, but its complete deterministic production and consumption flow is unavailable"
            ),
            Some("craft_accounting_failed") => json!(
                "the craft output exists, but standalone-character flow accounting failed"
            ),
            Some("craft_trigger_evaluation_pending") => json!(
                "craft flows were accounted, but Factorio did not advance a trigger-evaluation window"
            ),
            Some("craft_admission_clear_failed") => json!(
                "craft completion was verified, but its persisted admission could not be acknowledged"
            ),
            Some(other) => json!(format!("craft completion failed: {other}")),
        };
        let mut evidence_json = serde_json::to_value(&evidence).unwrap_or_default();
        evidence_json["status"] = json!(status);
        Ok(json!({
            "success": completed,
            "completed": completed,
            "queue_drained": queue_drained,
            "status": status,
            "operation_id": &admission.operation_id,
            "admission_persisted_in_save": true,
            "admission_cleared": admission_cleared,
            "terminal_receipt_persisted": admission_cleared,
            "receipt_replayed": false,
            "terminal_status": terminal_status,
            "clear_result": clear_result,
            "clear_error": clear_error,
            "error_kind": error_kind,
            "error": error,
            "evidence": evidence_json,
            "product_evidence": product_evidence,
            "flow_accounting_complete": flow_proof_available,
            "flows": &admission.flows,
            "accounting": accounting,
        }))
    }

    /// Complete the exact save-persisted craft transaction with the bounded
    /// production defaults.
    pub async fn complete_craft_admission(&mut self) -> Result<Value> {
        self.complete_craft_admission_with_options(
            DEFAULT_CRAFTING_WAIT_TIMEOUT,
            DEFAULT_CRAFTING_POLL_INTERVAL,
        )
        .await
    }

    // --- Entity Actions ---

    /// Place an entity from inventory
    pub async fn place_entity(
        &mut self,
        entity_name: &str,
        position: Position,
        direction: Direction,
    ) -> Result<Entity> {
        self.approach_position(position, "build").await?;
        let response = self
            .call_remote(
                "place_entity",
                &[
                    json!(self.agent_id.as_str()),
                    json!(entity_name),
                    json!(position.x),
                    json!(position.y),
                    json!(direction.to_factorio()),
                ],
            )
            .await?;
        // Check for error response
        if response.contains("\"error\"") {
            #[derive(serde::Deserialize)]
            struct ErrorResponse {
                error: String,
            }
            let err: ErrorResponse = serde_json::from_str(&response)?;
            anyhow::bail!("{}", err.error);
        }
        let entity: Entity = serde_json::from_str(&response)?;
        Ok(entity)
    }

    /// Place a new inserter and install its whitelist in the same Factorio
    /// remote call, before the simulation can advance and pick up an item.
    pub async fn place_filtered_inserter(
        &mut self,
        entity_name: &str,
        position: Position,
        direction: Direction,
        allowed_items: &[String],
    ) -> Result<serde_json::Value> {
        self.approach_position(position, "build").await?;
        let response = self
            .call_remote(
                "place_filtered_inserter",
                &[
                    json!(self.agent_id.as_str()),
                    json!(entity_name),
                    json!(position.x),
                    json!(position.y),
                    json!(direction.to_factorio()),
                    json!(allowed_items),
                ],
            )
            .await?;
        // Semantic failures are structured transaction reports: they retain
        // the exact placed unit and Lua-side rollback evidence. Do not flatten
        // them into anyhow text before the controller can finish rollback.
        Ok(serde_json::from_str(&response)?)
    }

    pub async fn check_entity_placement(
        &mut self,
        entity_name: &str,
        position: Position,
        direction: Direction,
    ) -> Result<serde_json::Value> {
        let response = self
            .call_remote(
                "check_entity_placement",
                &[
                    json!(self.agent_id.as_str()),
                    json!(entity_name),
                    json!(position.x),
                    json!(position.y),
                    json!(direction.to_factorio()),
                ],
            )
            .await?;
        Ok(serde_json::from_str(&response)?)
    }

    pub async fn find_entity_placements(
        &mut self,
        entity_name: &str,
        center: Position,
        radius: u32,
        limit: u32,
    ) -> Result<serde_json::Value> {
        let response = self
            .call_remote(
                "find_entity_placements",
                &[
                    json!(self.agent_id.as_str()),
                    json!(entity_name),
                    json!(center.x),
                    json!(center.y),
                    json!(radius),
                    json!(limit),
                ],
            )
            .await?;
        Ok(serde_json::from_str(&response)?)
    }

    pub async fn plan_entity_placement_near(
        &mut self,
        entity_name: &str,
        target: Position,
        radius: u32,
        limit: u32,
    ) -> Result<serde_json::Value> {
        let response = self
            .call_remote(
                "plan_entity_placement_near",
                &[
                    json!(self.agent_id.as_str()),
                    json!(entity_name),
                    json!(target.x),
                    json!(target.y),
                    json!(radius),
                    json!(limit),
                ],
            )
            .await?;
        Ok(serde_json::from_str(&response)?)
    }

    pub async fn build_edge_miner(
        &mut self,
        resource_name: &str,
        center: Position,
        radius: u32,
        drill_name: &str,
        limit: u32,
    ) -> Result<serde_json::Value> {
        let response = self
            .call_remote(
                "build_edge_miner",
                &[
                    json!(self.agent_id.as_str()),
                    json!(resource_name),
                    json!(center.x),
                    json!(center.y),
                    json!(radius),
                    json!(drill_name),
                    json!(limit),
                ],
            )
            .await?;
        Ok(serde_json::from_str(&response)?)
    }

    pub async fn build_direct_smelter(
        &mut self,
        drill_unit_number: Option<u32>,
        output: Option<(Position, Direction)>,
        furnace_name: &str,
        inserter_name: &str,
        belt_name: &str,
        radius: u32,
    ) -> Result<serde_json::Value> {
        let (output_x, output_y, output_direction) = match output {
            Some((position, direction)) => (
                json!(position.x),
                json!(position.y),
                json!(direction.to_factorio()),
            ),
            None => (Value::Null, Value::Null, Value::Null),
        };
        let response = self
            .call_remote(
                "build_direct_smelter",
                &[
                    json!(self.agent_id.as_str()),
                    drill_unit_number.map_or(Value::Null, |unit| json!(unit)),
                    output_x,
                    output_y,
                    output_direction,
                    json!(furnace_name),
                    json!(inserter_name),
                    json!(belt_name),
                    json!(radius),
                ],
            )
            .await?;
        Ok(serde_json::from_str(&response)?)
    }

    pub async fn plan_steam_power(
        &mut self,
        water_area: Area,
        target: Position,
    ) -> Result<serde_json::Value> {
        let response = self
            .call_remote(
                "plan_steam_power",
                &[
                    json!(self.agent_id.as_str()),
                    json!(water_area.left_top.x),
                    json!(water_area.left_top.y),
                    json!(water_area.right_bottom.x),
                    json!(water_area.right_bottom.y),
                    json!(target.x),
                    json!(target.y),
                ],
            )
            .await?;
        Ok(serde_json::from_str(&response)?)
    }

    pub async fn repair_steam_power(
        &mut self,
        x: i32,
        y: i32,
        radius: u32,
        target: Position,
    ) -> Result<serde_json::Value> {
        let response = self
            .call_remote(
                "repair_steam_power",
                &[
                    json!(self.agent_id.as_str()),
                    json!(x),
                    json!(y),
                    json!(radius),
                    json!(target.x),
                    json!(target.y),
                ],
            )
            .await?;
        Ok(serde_json::from_str(&response)?)
    }

    pub async fn extend_power_to(
        &mut self,
        x: i32,
        y: i32,
        radius: u32,
        target: Position,
    ) -> Result<serde_json::Value> {
        let response = self
            .call_remote(
                "extend_power_to",
                &[
                    json!(self.agent_id.as_str()),
                    json!(x),
                    json!(y),
                    json!(radius),
                    json!(target.x),
                    json!(target.y),
                ],
            )
            .await?;
        Ok(serde_json::from_str(&response)?)
    }

    /// Place a ghost entity (for planning, doesn't require items)
    pub async fn place_ghost(
        &mut self,
        entity_name: &str,
        position: Position,
        direction: Direction,
    ) -> Result<Entity> {
        self.approach_position(position, "build").await?;
        let response = self
            .call_remote(
                "place_ghost",
                &[
                    json!(self.agent_id.as_str()),
                    json!(entity_name),
                    json!(position.x),
                    json!(position.y),
                    json!(direction.to_factorio()),
                ],
            )
            .await?;
        if response.contains("\"error\"") {
            #[derive(serde::Deserialize)]
            struct ErrorResponse {
                error: String,
            }
            let err: ErrorResponse = serde_json::from_str(&response)?;
            anyhow::bail!("{}", err.error);
        }
        let entity: Entity = serde_json::from_str(&response)?;
        Ok(entity)
    }

    /// Remove entity at position
    pub async fn remove_entity_at(&mut self, position: Position) -> Result<()> {
        let response = self
            .call_remote(
                "remove_entity_at",
                &[
                    json!(self.agent_id.as_str()),
                    json!(position.x),
                    json!(position.y),
                ],
            )
            .await?;
        ensure_lua_success(&response)?;
        Ok(())
    }

    /// Remove entity by unit number
    pub async fn remove_entity(&mut self, unit_number: u32) -> Result<()> {
        self.approach_entity(unit_number).await?;
        let response = self
            .call_remote(
                "remove_entity",
                &[json!(self.agent_id.as_str()), json!(unit_number)],
            )
            .await?;
        ensure_lua_success(&response)?;
        Ok(())
    }

    /// Rotate entity to a new direction
    pub async fn rotate_entity(
        &mut self,
        unit_number: u32,
        direction: u8,
    ) -> Result<serde_json::Value> {
        self.approach_entity(unit_number).await?;
        let response = self
            .call_remote(
                "rotate_entity",
                &[
                    json!(self.agent_id.as_str()),
                    json!(unit_number),
                    json!(direction),
                ],
            )
            .await?;
        ensure_lua_success(&response)?;
        Ok(serde_json::from_str(&response)?)
    }

    /// Replace an existing inserter's complete item whitelist. An empty list
    /// clears all slots and disables filtering.
    pub async fn configure_inserter(
        &mut self,
        unit_number: u32,
        allowed_items: &[String],
    ) -> Result<serde_json::Value> {
        self.approach_entity(unit_number).await?;
        let response = self
            .call_remote(
                "configure_inserter",
                &[
                    json!(self.agent_id.as_str()),
                    json!(unit_number),
                    json!(allowed_items),
                ],
            )
            .await?;
        // Preserve semantic failure kinds and rollback/readback evidence for
        // the MCP caller. The server's common semantic-error marker turns a
        // returned `{success:false}` payload into an MCP tool error.
        Ok(serde_json::from_str(&response)?)
    }

    /// Insert items into an entity
    pub async fn insert_items(
        &mut self,
        unit_number: u32,
        item: &str,
        count: u32,
        inventory_type: &str,
    ) -> Result<serde_json::Value> {
        self.approach_entity(unit_number).await?;
        let response = self
            .call_remote(
                "insert_items",
                &[
                    json!(self.agent_id.as_str()),
                    json!(unit_number),
                    json!(item),
                    json!(count),
                    json!(inventory_type),
                ],
            )
            .await?;
        ensure_lua_success(&response)?;
        Ok(serde_json::from_str(&response)?)
    }

    /// Add a bounded fuel buffer to an existing burner drill or inserter.
    pub async fn bootstrap_burner_once(
        &mut self,
        unit_number: u32,
        fuel_item: &str,
        count: u32,
    ) -> Result<serde_json::Value> {
        self.approach_entity(unit_number).await?;
        let response = self
            .call_remote(
                "bootstrap_burner_once",
                &[
                    json!(self.agent_id.as_str()),
                    json!(unit_number),
                    json!(fuel_item),
                    json!(count),
                ],
            )
            .await?;
        Ok(serde_json::from_str(&response)?)
    }

    /// Snapshot exact burner state before a compound controller transaction.
    pub async fn snapshot_burner_state(&mut self, unit_number: u32) -> Result<serde_json::Value> {
        let response = self
            .call_remote("snapshot_burner_state", &[json!(unit_number)])
            .await?;
        ensure_lua_success(&response)?;
        Ok(serde_json::from_str(&response)?)
    }

    /// Quiesce the new feeder and restore exact pre-transaction burner state.
    pub async fn rollback_burner_bootstrap(
        &mut self,
        snapshot: &serde_json::Value,
        feeder_unit_number: Option<u32>,
    ) -> Result<serde_json::Value> {
        let response = self
            .call_remote(
                "rollback_burner_bootstrap",
                &[
                    json!(self.agent_id.as_str()),
                    snapshot.clone(),
                    feeder_unit_number.map_or(serde_json::Value::Null, |unit| json!(unit)),
                ],
            )
            .await?;
        ensure_lua_success(&response)?;
        Ok(serde_json::from_str(&response)?)
    }

    /// Collect bounded construction or recovery stock from an existing chest.
    pub async fn collect_from_chest(
        &mut self,
        unit_number: u32,
        item: &str,
        count: u32,
    ) -> Result<serde_json::Value> {
        self.approach_entity(unit_number).await?;
        let response = self
            .call_remote(
                "collect_from_chest",
                &[
                    json!(self.agent_id.as_str()),
                    json!(unit_number),
                    json!(item),
                    json!(count),
                ],
            )
            .await?;
        Ok(serde_json::from_str(&response)?)
    }

    /// Extract items from an entity into player inventory
    pub async fn extract_items(
        &mut self,
        unit_number: u32,
        item: &str,
        count: u32,
        inventory_type: &str,
    ) -> Result<u32> {
        self.approach_entity(unit_number).await?;
        let response = self
            .call_remote(
                "extract_items",
                &[
                    json!(self.agent_id.as_str()),
                    json!(unit_number),
                    json!(item),
                    json!(count),
                    json!(inventory_type),
                ],
            )
            .await?;

        #[derive(serde::Deserialize)]
        struct ExtractResult {
            extracted: Option<u32>,
            #[allow(dead_code)]
            available: Option<u32>,
            error: Option<String>,
        }

        let result: ExtractResult = serde_json::from_str(&response)?;
        if let Some(err) = result.error {
            if result.extracted.unwrap_or(0) == 0 {
                anyhow::bail!(err);
            }
        }
        Ok(result.extracted.unwrap_or(0))
    }

    async fn set_recipe_value(&mut self, unit_number: u32, recipe: Option<&str>) -> Result<()> {
        self.approach_entity(unit_number).await?;
        let recipe = recipe.map_or(Value::Null, |name| json!(name));
        let response = self
            .call_remote(
                "set_recipe",
                &[json!(self.agent_id.as_str()), json!(unit_number), recipe],
            )
            .await?;
        ensure_lua_success(&response)?;
        Ok(())
    }

    /// Set recipe on an assembling machine.
    pub async fn set_recipe(&mut self, unit_number: u32, recipe: &str) -> Result<()> {
        if recipe.is_empty() {
            self.clear_recipe(unit_number).await
        } else {
            self.set_recipe_value(unit_number, Some(recipe)).await
        }
    }

    /// Clear the recipe on an assembling machine using a JSON null/Lua nil
    /// recipe argument. An empty recipe name is not a valid Factorio recipe.
    pub async fn clear_recipe(&mut self, unit_number: u32) -> Result<()> {
        self.set_recipe_value(unit_number, None).await
    }

    /// Get the recipe currently configured on a crafting machine, if any.
    pub async fn get_entity_recipe(&mut self, unit_number: u32) -> Result<Option<String>> {
        let response = self
            .call_remote("get_entity_recipe", &[json!(unit_number)])
            .await?;
        #[derive(serde::Deserialize)]
        struct EntityRecipeResponse {
            success: bool,
            #[serde(default)]
            recipe: Option<String>,
            #[serde(default)]
            error: Option<String>,
        }
        let result: EntityRecipeResponse = serde_json::from_str(&response)?;
        if !result.success {
            anyhow::bail!(
                "{}",
                result
                    .error
                    .unwrap_or_else(|| "failed to read entity recipe".to_string())
            );
        }
        Ok(result.recipe)
    }

    pub async fn feed_lab_from_inventory(
        &mut self,
        lab_unit_number: u32,
        science_pack: &str,
        count: u32,
        dry_run: bool,
    ) -> Result<serde_json::Value> {
        self.approach_entity(lab_unit_number).await?;
        let response = self
            .call_remote(
                "feed_lab_from_inventory",
                &[
                    json!(self.agent_id.as_str()),
                    json!(lab_unit_number),
                    json!(science_pack),
                    json!(count),
                    json!(dry_run),
                ],
            )
            .await?;
        Ok(serde_json::from_str(&response)?)
    }

    /// Check if a technology has been researched
    pub async fn is_tech_researched(&mut self, tech_name: &str) -> Result<bool> {
        let response = self
            .call_remote(
                "is_tech_researched",
                &[json!(tech_name), json!(self.agent_id.as_str())],
            )
            .await?;
        #[derive(serde::Deserialize)]
        struct TechState {
            researched: bool,
        }

        let result: TechState = serde_json::from_str(&response)?;
        Ok(result.researched)
    }

    /// Place an underground belt with specified type (input or output)
    pub async fn place_underground_belt(
        &mut self,
        entity_name: &str,
        position: Position,
        direction: Direction,
        belt_type: &str, // "input" for entry, "output" for exit
    ) -> Result<Entity> {
        self.approach_position(position, "build").await?;
        let response = self
            .call_remote(
                "place_underground_belt",
                &[
                    json!(self.agent_id.as_str()),
                    json!(entity_name),
                    json!(position.x),
                    json!(position.y),
                    json!(direction.to_factorio()),
                    json!(belt_type),
                ],
            )
            .await?;
        // Check for error response
        if response.contains("\"error\"") {
            #[derive(serde::Deserialize)]
            struct ErrorResponse {
                error: String,
            }
            let err: ErrorResponse = serde_json::from_str(&response)?;
            anyhow::bail!("{}", err.error);
        }
        let entity: Entity = serde_json::from_str(&response)?;
        Ok(entity)
    }

    // --- Tick Control ---

    /// Pause the game
    pub async fn pause_game(&mut self) -> Result<()> {
        let response = self.call_remote("set_tick_paused", &[json!(true)]).await?;
        ensure_lua_success(&response)?;
        Ok(())
    }

    /// Resume the game
    pub async fn resume_game(&mut self) -> Result<()> {
        let response = self.call_remote("set_tick_paused", &[json!(false)]).await?;
        ensure_lua_success(&response)?;
        Ok(())
    }

    /// Set game speed
    pub async fn set_game_speed(&mut self, speed: f64) -> Result<()> {
        let response = self.call_remote("set_game_speed", &[json!(speed)]).await?;
        ensure_lua_success(&response)?;
        Ok(())
    }

    /// Wait for N ticks
    pub async fn wait_ticks(&mut self, ticks: u32) -> Result<()> {
        let start = self.get_tick().await?.tick;
        let target = start + ticks as u64;
        let wall_limit =
            tokio::time::Duration::from_secs(((ticks as u64 / 60).saturating_mul(4)).clamp(5, 120));

        tokio::time::timeout(wall_limit, async {
            let mut last_tick = start;
            let mut unchanged_samples = 0_u32;
            loop {
                tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                let current = self.get_tick().await?.tick;
                if current >= target {
                    return Ok(());
                }
                if current == last_tick {
                    unchanged_samples += 1;
                    if unchanged_samples >= 100 {
                        anyhow::bail!(
                            "Game ticks are not advancing (paused or stalled at tick {current})"
                        );
                    }
                } else {
                    last_tick = current;
                    unchanged_samples = 0;
                }
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("Timed out waiting for {ticks} game ticks"))?
    }

    // --- Proximity Checks ---

    async fn get_entity_reach_status(&mut self, unit_number: u32) -> Result<ReachStatus> {
        let response = self
            .call_remote(
                "get_entity_reach",
                &[json!(self.agent_id.as_str()), json!(unit_number)],
            )
            .await?;
        Ok(serde_json::from_str(&response)?)
    }

    async fn get_position_reach_status(
        &mut self,
        target: Position,
        reach_kind: &str,
    ) -> Result<ReachStatus> {
        let response = self
            .call_remote(
                "get_position_reach",
                &[
                    json!(self.agent_id.as_str()),
                    json!(target.x),
                    json!(target.y),
                    json!(reach_kind),
                ],
            )
            .await?;
        Ok(serde_json::from_str(&response)?)
    }

    /// Move only when Factorio says an exact entity is out of reach, then
    /// re-check the engine-owned reach predicate after the walk.
    pub async fn approach_entity(&mut self, unit_number: u32) -> Result<()> {
        let initial = self.get_entity_reach_status(unit_number).await?;
        if !initial.success {
            anyhow::bail!(
                "{}",
                initial
                    .error
                    .unwrap_or_else(|| "failed to inspect entity reach".to_string())
            );
        }
        if initial.reachable {
            return Ok(());
        }

        let target = initial
            .target_position
            .ok_or_else(|| anyhow::anyhow!("entity reach response omitted target_position"))?;
        let max_distance = initial
            .max_distance
            .filter(|distance| distance.is_finite() && *distance > 0.0)
            .ok_or_else(|| {
                anyhow::anyhow!("entity reach response omitted a usable max_distance")
            })?;
        let physical_arrival_distance = initial.walk_arrival_distance.unwrap_or(0.0).max(0.0);

        let walk = self
            .walk_to_pathfind_with_tolerance(target, 16, max_distance, physical_arrival_distance)
            .await;
        let final_status = self.get_entity_reach_status(unit_number).await?;
        if final_status.success && final_status.reachable {
            return Ok(());
        }

        let walk_reason = match walk {
            Ok(result) => result
                .reason
                .unwrap_or_else(|| "walk completed outside native reach".to_string()),
            Err(error) => error.to_string(),
        };
        anyhow::bail!(
            "Could not move within Factorio reach of entity {}: {}; final reach check: {}",
            unit_number,
            walk_reason,
            final_status
                .error
                .unwrap_or_else(|| "entity remains out of reach".to_string())
        )
    }

    async fn approach_position(&mut self, target: Position, reach_kind: &str) -> Result<()> {
        let initial = self.get_position_reach_status(target, reach_kind).await?;
        if !initial.success {
            anyhow::bail!(
                "{}",
                initial
                    .error
                    .unwrap_or_else(|| "failed to inspect position reach".to_string())
            );
        }
        if initial.reachable {
            return Ok(());
        }
        let max_distance = initial
            .max_distance
            .filter(|distance| distance.is_finite() && *distance > 0.0)
            .ok_or_else(|| {
                anyhow::anyhow!("position reach response omitted a usable max_distance")
            })?;
        let physical_arrival_distance = initial.walk_arrival_distance.unwrap_or(0.0).max(0.0);
        let walk = self
            .walk_to_pathfind_with_tolerance(target, 16, max_distance, physical_arrival_distance)
            .await;
        let final_status = self.get_position_reach_status(target, reach_kind).await?;
        if final_status.success && final_status.reachable {
            return Ok(());
        }
        let walk_reason = match walk {
            Ok(result) => result
                .reason
                .unwrap_or_else(|| "walk completed outside native reach".to_string()),
            Err(error) => error.to_string(),
        };
        anyhow::bail!(
            "Could not move within Factorio {} reach of ({:.1}, {:.1}): {}; final reach check: {}",
            reach_kind,
            target.x,
            target.y,
            walk_reason,
            final_status
                .error
                .unwrap_or_else(|| "position remains out of reach".to_string())
        )
    }

    /// Check if player is within range of a position, return error if not
    pub async fn ensure_proximity_to_position(
        &mut self,
        target: Position,
        max_distance: f64,
    ) -> Result<()> {
        let char_pos = self.get_character_position().await?;
        let distance = char_pos.distance(&target);
        if distance > max_distance {
            anyhow::bail!(
                "Player is {:.1} tiles away from target (max: {:.0}). Use 'walk-to {:.0},{:.0}' first.",
                distance,
                max_distance,
                target.x,
                target.y
            );
        }
        Ok(())
    }

    /// Check if player is within range of an entity, return error if not
    pub async fn ensure_proximity_to_entity(
        &mut self,
        unit_number: u32,
        max_distance: f64,
    ) -> Result<()> {
        let entity = self.get_entity(unit_number).await?;
        self.ensure_proximity_to_position(entity.position, max_distance)
            .await
    }

    // --- High-Level Operations ---

    /// Get character's current position (uses first connected player or spawned character)
    pub async fn get_character_position(&mut self) -> Result<Position> {
        let response = self
            .call_remote("get_character_pos", &[json!(self.agent_id.as_str())])
            .await?;
        let parts: Vec<&str> = response.trim().split(',').collect();
        if parts.len() != 2 {
            anyhow::bail!("No character available");
        }
        Ok(Position {
            x: parts[0].parse()?,
            y: parts[1].parse()?,
        })
    }

    /// Walk to a target position using A* pathfinding to avoid obstacles
    pub async fn walk_to_pathfind(
        &mut self,
        target: Position,
        search_radius: u32,
    ) -> Result<WalkResult> {
        self.walk_to_pathfind_with_tolerance(target, search_radius, 0.0, 0.0)
            .await
    }

    async fn walk_to_pathfind_with_tolerance(
        &mut self,
        target: Position,
        search_radius: u32,
        final_tolerance: f64,
        physical_arrival_distance: f64,
    ) -> Result<WalkResult> {
        use crate::world::{find_walk_path, find_walk_path_to_any};
        use std::collections::HashSet;

        let start_pos = self.get_character_position().await?;
        let start_grid = GridPos::from_position(&start_pos);
        let end_grid = GridPos::from_position(&target);

        // Build collision map for the area
        let padding = search_radius as f64;
        let area = Area {
            left_top: Position {
                x: start_pos.x.min(target.x) - padding,
                y: start_pos.y.min(target.y) - padding,
            },
            right_bottom: Position {
                x: start_pos.x.max(target.x) + padding,
                y: start_pos.y.max(target.y) + padding,
            },
        };

        let collision_map = self.build_collision_map(area).await?;

        // Exact navigation has one exact goal. Interaction movement instead
        // pathfinds to any walkable standing tile inside the authoritative
        // Factorio reach radius, because an entity's center is normally
        // collision-blocked by the entity itself.
        let (path_result, path_goal) = if final_tolerance > 0.0 {
            let candidate_radius = final_tolerance.min(padding).ceil().max(1.0) as i32;
            let safe_goal_radius = (final_tolerance - physical_arrival_distance).max(0.0);
            let mut goals = HashSet::new();
            for x in end_grid.x - candidate_radius..=end_grid.x + candidate_radius {
                for y in end_grid.y - candidate_radius..=end_grid.y + candidate_radius {
                    let candidate = GridPos::new(x, y);
                    if candidate.to_position().distance(&target) <= safe_goal_radius
                        && collision_map.is_walkable(&candidate)
                    {
                        goals.insert(candidate);
                    }
                }
            }
            let result = find_walk_path_to_any(start_grid, &goals, &collision_map);
            let goal = result.path.last().copied().unwrap_or(end_grid);
            (result, goal)
        } else {
            (
                find_walk_path(start_grid, end_grid, &collision_map),
                end_grid,
            )
        };

        if !path_result.success {
            // Preserve the ordinary walking fallback when the collision map
            // cannot find any route. The terminal receipt still reports a
            // truthful `stuck` result rather than fabricating arrival.
            return self.walk_to_with_tolerance(target, final_tolerance).await;
        }

        // Walk through each waypoint
        let mut total_distance = 0.0;
        let waypoints = path_result
            .path
            .iter()
            .copied()
            .filter(|waypoint| *waypoint != start_grid)
            .collect::<Vec<_>>();

        for (index, waypoint) in waypoints.iter().enumerate() {
            let last_waypoint = index + 1 == waypoints.len();
            let raw_waypoint = if last_waypoint && final_tolerance == 0.0 {
                target
            } else if last_waypoint {
                path_goal.to_position()
            } else {
                waypoint.to_position()
            };
            let distance_from_target = raw_waypoint.distance(&target);
            let enters_final_radius = final_tolerance > 0.0
                && distance_from_target + physical_arrival_distance <= final_tolerance;
            let final_hop = last_waypoint || enters_final_radius;
            let tolerance = if enters_final_radius {
                // Stop along the collision-free A* segment as soon as the
                // character enters the requested radius. Walking exactly to
                // this near-target turn can put the character on a later
                // placement tile.
                final_tolerance - distance_from_target
            } else if last_waypoint {
                final_tolerance
            } else {
                0.0
            };

            let result = self.walk_to_with_tolerance(raw_waypoint, tolerance).await?;
            total_distance += result.distance_walked;

            if !result.arrived {
                return Ok(WalkResult {
                    arrived: false,
                    final_position: result.final_position,
                    distance_walked: total_distance,
                    reason: result.reason,
                });
            }
            if final_hop {
                let inside_target_radius = final_tolerance == 0.0
                    || result.final_position.distance(&target) <= final_tolerance;
                return Ok(WalkResult {
                    arrived: result.arrived && inside_target_radius,
                    final_position: result.final_position,
                    distance_walked: total_distance,
                    reason: if inside_target_radius {
                        None
                    } else {
                        Some("Walk stopped outside requested arrival radius".to_string())
                    },
                });
            }
        }

        // Same-tile paths contain no waypoint after filtering. The mod still
        // owns the exact floating-point arrival decision for that final hop.
        if waypoints.is_empty() {
            return self.walk_to_with_tolerance(target, final_tolerance).await;
        }

        let final_pos = self.get_character_position().await?;
        Ok(WalkResult {
            arrived: true,
            final_position: final_pos,
            distance_walked: total_distance,
            reason: None,
        })
    }

    /// Smooth walk to a target position (direct, no pathfinding)
    pub async fn walk_to(&mut self, target: Position, _run: bool) -> Result<WalkResult> {
        self.walk_to_with_tolerance(target, 0.0).await
    }

    async fn walk_to_with_tolerance(
        &mut self,
        target: Position,
        arrival_distance: f64,
    ) -> Result<WalkResult> {
        let start_pos = self.get_character_position().await?;

        let target_response = self
            .call_remote(
                "set_walk_target",
                &[
                    json!(self.agent_id.as_str()),
                    json!(target.x),
                    json!(target.y),
                    json!(arrival_distance.max(0.0)),
                ],
            )
            .await?;
        ensure_lua_success(&target_response)?;
        let started: WalkStatus = serde_json::from_str(&target_response)?;
        if !started.success {
            anyhow::bail!(
                "{}",
                started
                    .error
                    .unwrap_or_else(|| "failed to start walk".to_string())
            );
        }
        let walk_id = started
            .walk_id
            .ok_or_else(|| anyhow::anyhow!("walk start response omitted walk_id"))?;

        for _ in 0..500 {
            let response = self
                .call_remote(
                    "get_walk_status",
                    &[json!(self.agent_id.as_str()), json!(walk_id)],
                )
                .await?;
            let status: WalkStatus = serde_json::from_str(&response)?;
            if !status.success {
                anyhow::bail!(
                    "{}",
                    status
                        .error
                        .unwrap_or_else(|| "failed to read walk status".to_string())
                );
            }
            if !status.active {
                let final_position = match status.final_position {
                    Some(position) => position,
                    None => self.get_character_position().await.unwrap_or(start_pos),
                };
                return Ok(WalkResult {
                    arrived: status.arrived,
                    final_position,
                    distance_walked: status.distance_walked,
                    reason: if status.arrived { None } else { status.reason },
                });
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
        }

        let _ = self
            .call_remote(
                "clear_walk_target",
                &[json!(self.agent_id.as_str()), json!(walk_id)],
            )
            .await?;
        let pos = self.get_character_position().await?;
        Ok(WalkResult {
            arrived: false,
            final_position: pos,
            distance_walked: pos.distance(&start_pos),
            reason: Some("Timeout".to_string()),
        })
    }

    /// Gather resources by walking to them and mining (with animations)
    pub async fn gather_resource(
        &mut self,
        resource_name: &str,
        amount: u32,
        radius: u32,
    ) -> Result<GatherResult> {
        let mut total_distance = 0.0;
        let mut gathered = 0u32;

        for _ in 0..amount {
            let response = self
                .call_remote(
                    "find_nearest_minable",
                    &[
                        json!(self.agent_id.as_str()),
                        json!(resource_name),
                        json!(radius),
                    ],
                )
                .await?;
            let nearest: NearestMinableResponse = serde_json::from_str(&response)?;
            if let Some(error) = nearest.error {
                anyhow::bail!(error);
            }
            if !nearest.found {
                break;
            }
            let Some(target_pos) = nearest.position else {
                break;
            };

            let before = self.get_character_position().await?;
            let mine_result = self.mine_at(target_pos, 1).await?;
            let after = self.get_character_position().await?;
            total_distance += before.distance(&after);
            if mine_result.success {
                gathered += 1;
            } else {
                break;
            }
        }

        let inv_result = self.character_inventory().await?;

        Ok(GatherResult {
            success: gathered > 0,
            resource_name: resource_name.to_string(),
            gathered,
            distance_walked: total_distance,
            inventory: inv_result.items,
            error: None,
        })
    }

    /// Build an array of drills on a resource patch
    pub async fn build_drill_array(
        &mut self,
        count: u32,
        resource: &str,
        near: Option<(f64, f64)>,
        drill_type: &str,
        direction: &str,
    ) -> Result<BuildResult> {
        let (near_x, near_y) = match near {
            Some((x, y)) => (json!(x), json!(y)),
            None => (Value::Null, Value::Null),
        };
        let response = self
            .call_remote(
                "build_drill_array",
                &[
                    json!(self.agent_id.as_str()),
                    json!(count),
                    json!(resource),
                    near_x,
                    near_y,
                    json!(drill_type),
                    json!(direction),
                ],
            )
            .await?;
        let result: BuildResult = serde_json::from_str(&response)?;
        Ok(result)
    }

    /// Build a line of smelters
    pub async fn build_smelter_line(
        &mut self,
        count: u32,
        start: (f64, f64),
        furnace_type: &str,
        line_direction: &str,
        spacing: u32,
    ) -> Result<BuildResult> {
        let response = self
            .call_remote(
                "build_smelter_line",
                &[
                    json!(self.agent_id.as_str()),
                    json!(count),
                    json!(start.0),
                    json!(start.1),
                    json!(furnace_type),
                    json!(line_direction),
                    json!(spacing),
                ],
            )
            .await?;
        let result: BuildResult = serde_json::from_str(&response)?;
        Ok(result)
    }

    /// Build from a JSON plan
    pub async fn build_from_plan(&mut self, plan_json: &str) -> Result<BuildResult> {
        let specs: Vec<PlacementSpec> = serde_json::from_str(plan_json)?;

        let mut placed = 0;
        let mut entities = Vec::new();
        let mut errors = Vec::new();

        for spec in &specs {
            let direction = spec
                .direction
                .as_ref()
                .and_then(|d| Direction::from_name(d))
                .unwrap_or(Direction::North);

            let pos = Position {
                x: spec.position.0,
                y: spec.position.1,
            };

            match self.place_entity(&spec.name, pos, direction).await {
                Ok(entity) => {
                    placed += 1;
                    entities.push(entity);
                }
                Err(e) => {
                    errors.push(format!(
                        "Failed to place {} at ({}, {}): {}",
                        spec.name, spec.position.0, spec.position.1, e
                    ));
                }
            }
        }

        Ok(BuildResult {
            placed,
            total: specs.len() as u32,
            entities,
            errors,
        })
    }

    /// Get items on transport belts in an area
    pub async fn get_belt_contents(&mut self, area: Area) -> Result<BeltContentsResult> {
        let response = self
            .call_remote(
                "get_belt_contents",
                &[
                    json!(area.left_top.x),
                    json!(area.left_top.y),
                    json!(area.right_bottom.x),
                    json!(area.right_bottom.y),
                    json!(self.agent_id.as_str()),
                ],
            )
            .await?;
        let result: BeltContentsResult = serde_json::from_str(&response)?;
        Ok(result)
    }

    /// Get items on transport belts with lane separation
    pub async fn get_belt_lane_contents(&mut self, area: Area) -> Result<BeltLaneContentsResult> {
        let response = self
            .call_remote(
                "get_belt_lane_contents",
                &[
                    json!(area.left_top.x),
                    json!(area.left_top.y),
                    json!(area.right_bottom.x),
                    json!(area.right_bottom.y),
                    json!(self.agent_id.as_str()),
                ],
            )
            .await?;

        // Parse the raw belt data
        #[derive(serde::Deserialize)]
        struct RawBeltLane {
            position: RawPos,
            unit_number: u32,
            direction: u8,
            belt_type: String,
            left_lane: RawLane,
            right_lane: RawLane,
        }
        #[derive(serde::Deserialize)]
        struct RawPos {
            x: i32,
            y: i32,
        }
        #[derive(serde::Deserialize)]
        struct RawLane {
            lane: u8,
            #[serde(default, deserialize_with = "crate::world::deserialize_lua_empty_vec")]
            items: Vec<InventoryItem>,
            item_count: u32,
        }

        let raw_belts = parse_lua_array::<RawBeltLane>(&response)?;

        // Build the result with aggregated summary
        let mut total_items = 0u32;
        let mut item_totals: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();

        let belts: Vec<BeltLaneSummary> = raw_belts
            .into_iter()
            .map(|raw| {
                // Aggregate items
                for item in &raw.left_lane.items {
                    *item_totals.entry(item.name.clone()).or_insert(0) += item.count;
                    total_items += item.count;
                }
                for item in &raw.right_lane.items {
                    *item_totals.entry(item.name.clone()).or_insert(0) += item.count;
                    total_items += item.count;
                }

                BeltLaneSummary {
                    position: TilePos::new(raw.position.x, raw.position.y),
                    unit_number: raw.unit_number,
                    direction: raw.direction,
                    belt_type: raw.belt_type,
                    left_lane: LaneContents {
                        lane: raw.left_lane.lane,
                        items: raw.left_lane.items,
                        item_count: raw.left_lane.item_count,
                    },
                    right_lane: LaneContents {
                        lane: raw.right_lane.lane,
                        items: raw.right_lane.items,
                        item_count: raw.right_lane.item_count,
                    },
                }
            })
            .collect();

        let item_summary: Vec<InventoryItem> = item_totals
            .into_iter()
            .map(|(name, count)| InventoryItem { name, count })
            .collect();

        Ok(BeltLaneContentsResult {
            belt_count: belts.len() as u32,
            total_items,
            item_summary,
            belts,
        })
    }
}

/// Whether an entity should occupy tiles in the character walking map.
///
/// Entity bounding boxes are placement/collision metadata, but surface
/// transport belts deliberately remain walkable by characters in Factorio.
/// Treating their boxes as walls turns an ordinary belt row into an artificial
/// impassable barrier for physical interaction and rollback movement.
fn entity_blocks_character_path(entity: &Entity) -> bool {
    if entity.name == "character" {
        return false;
    }
    !matches!(
        entity.entity_type.as_deref(),
        Some("resource" | "item-entity" | "entity-ghost" | "tile-ghost" | "transport-belt")
    )
}

/// Get collision padding for entity types based on their size
/// Returns the half-size rounded down (0 for 1x1, 1 for 2x2 or 3x3)
fn entity_collision_padding(entity_name: &str) -> i32 {
    match entity_name {
        // 2x2 entities
        "burner-mining-drill" | "electric-mining-drill" => 1,
        "stone-furnace" | "steel-furnace" | "electric-furnace" => 1,
        "boiler" | "steam-engine" => 1,
        "offshore-pump" => 1,
        "radar" => 1,
        "lab" => 1,

        // 3x3 entities
        name if name.starts_with("assembling-machine") => 1,
        "chemical-plant" => 1,
        "oil-refinery" => 2,
        "centrifuge" => 1,
        "pumpjack" => 1,

        // 1x1 entities (belts, inserters, chests, poles)
        _ if entity_name.contains("belt") => 0,
        _ if entity_name.contains("inserter") => 0,
        _ if entity_name.contains("chest") => 0,
        _ if entity_name.contains("pole") => 0,
        _ if entity_name.contains("splitter") => 0, // 2x1 but we'll be conservative
        _ if entity_name.contains("pipe") => 0,

        // Default to 0 (1x1)
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue_snapshot(queue_size: u32, recipes: &[(&str, u32)]) -> CraftingQueueSnapshot {
        let queue = recipes
            .iter()
            .map(|(recipe, count)| crate::world::CraftQueueItem {
                recipe: (*recipe).to_string(),
                count: *count,
            })
            .collect::<Vec<_>>();
        CraftingQueueSnapshot {
            queue_size,
            current_recipe: queue.first().map(|item| item.recipe.clone()),
            queue,
        }
    }

    #[test]
    fn craft_completion_requires_admission_queue_drain_and_product_evidence() {
        assert_eq!(
            classify_craft_completion(true, true, true, true),
            (true, "completed", None)
        );
        assert_eq!(
            classify_craft_completion(false, true, true, false),
            (false, "timed_out", Some("crafting_timeout"))
        );
        assert_eq!(
            classify_craft_completion(true, false, false, false),
            (false, "unverified", Some("missing_craft_admission"))
        );
        assert_eq!(
            classify_craft_completion(true, true, false, false),
            (false, "unverified", Some("craft_completion_unverifiable"))
        );
        assert_eq!(
            classify_craft_completion(true, true, true, false),
            (false, "output_missing", Some("craft_output_missing"))
        );
    }

    #[test]
    fn terminal_craft_receipts_replay_exact_success_or_failure() {
        let mut receipt: CraftAdmissionRecord = serde_json::from_value(json!({
            "operation_id": "craft-42-7",
            "admitted_at_tick": 42,
            "character_unit_number": 99,
            "force_name": "player",
            "surface_name": "nauvis",
            "identity_valid": true,
            "completion_receipt": true,
            "terminal_status": "completed",
            "result": {
                "success": true,
                "queued": 1,
                "queue_size": 1,
                "queue": [],
                "operation_id": "craft-42-7",
                "recipe": "lab"
            },
            "flows": [{
                "name": "lab",
                "produced": 1,
                "consumed": 0,
                "production_before": 3.5,
                "consumption_before": 0
            }]
        }))
        .expect("terminal receipt should deserialize with fractional statistics baselines");

        let completed = craft_terminal_receipt_response(&receipt);
        assert_eq!(completed["success"], true);
        assert_eq!(completed["completed"], true);
        assert_eq!(completed["receipt_replayed"], true);
        assert_eq!(completed["terminal_status"], "completed");
        assert!(completed["error_kind"].is_null());

        receipt.terminal_status = Some("craft_output_missing".to_string());
        let failed = craft_terminal_receipt_response(&receipt);
        assert_eq!(failed["success"], false);
        assert_eq!(failed["completed"], false);
        assert_eq!(failed["receipt_replayed"], true);
        assert_eq!(failed["terminal_status"], "craft_output_missing");
        assert_eq!(failed["error_kind"], "craft_output_missing");
    }

    fn collision_test_entity(name: &str, entity_type: &str) -> Entity {
        Entity {
            unit_number: Some(1),
            name: name.to_string(),
            entity_type: Some(entity_type.to_string()),
            position: Position::new(0.5, 0.5),
            direction: 0,
            health: Some(100.0),
            force: Some("player".to_string()),
            bounding_box: Some(Area::new(0.1, 0.1, 0.9, 0.9)),
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
    fn character_collision_map_keeps_surface_transport_belts_walkable() {
        for name in [
            "transport-belt",
            "fast-transport-belt",
            "express-transport-belt",
        ] {
            assert!(
                !entity_blocks_character_path(&collision_test_entity(name, "transport-belt")),
                "surface belt {name} should not become an artificial walking wall"
            );
        }

        assert!(entity_blocks_character_path(&collision_test_entity(
            "assembling-machine-1",
            "assembling-machine"
        )));
        assert!(entity_blocks_character_path(&collision_test_entity(
            "underground-belt",
            "underground-belt"
        )));
        assert!(!entity_blocks_character_path(&collision_test_entity(
            "iron-ore", "resource"
        )));
        assert!(!entity_blocks_character_path(&collision_test_entity(
            "entity-ghost",
            "entity-ghost"
        )));
    }

    #[test]
    fn claude_command_uses_json_envelope_with_explicit_arg_count() {
        let command = claude_command(
            "example",
            &[
                json!("quote \" and newline\n kept"),
                Value::Null,
                json!("unicode \u{2603}"),
            ],
        );

        let payload = command
            .strip_prefix("/claude ")
            .expect("remote commands should use the /claude console command");
        let parsed: Value = serde_json::from_str(payload).expect("command payload should be JSON");
        assert_eq!(parsed["fn"], "example");
        assert_eq!(parsed["n"], 3);
        assert_eq!(parsed["args"][0], "quote \" and newline\n kept");
        assert!(parsed["args"][1].is_null());
        assert_eq!(parsed["args"][2], "unicode \u{2603}");
        assert!(
            !command.contains("remote.call") && !command.contains("/silent-command"),
            "normal remote requests should not generate Lua"
        );
    }

    #[test]
    fn recipe_remote_protocol_distinguishes_set_from_clear() {
        let set = claude_command(
            "set_recipe",
            &[json!("default"), json!(47), json!("copper-cable")],
        );
        let clear = claude_command("set_recipe", &[json!("default"), json!(47), Value::Null]);
        let set: Value = serde_json::from_str(
            set.strip_prefix("/claude ")
                .expect("set command should use the /claude envelope"),
        )
        .expect("set command should contain JSON");
        let clear: Value = serde_json::from_str(
            clear
                .strip_prefix("/claude ")
                .expect("clear command should use the /claude envelope"),
        )
        .expect("clear command should contain JSON");

        assert_eq!(set["args"][2], "copper-cable");
        assert!(clear["args"][2].is_null());
        assert_eq!(set["n"], 3);
        assert_eq!(clear["n"], 3);
    }

    #[test]
    fn old_mod_claude_command_skew_returns_structured_sync_error() {
        let response = old_mod_claude_command_skew_response(
            "get_tick",
            "Unknown command \"claude\". Type /help for more help.",
        )
        .expect("unknown /claude command should be classified as old mod skew");
        let parsed: Value = serde_json::from_str(&response).expect("skew response should be JSON");
        assert_eq!(parsed["success"], false);
        assert_eq!(parsed["error_kind"], "unknown_function");
        assert_eq!(parsed["action_needed"], "sync_or_restart_mod");
        assert!(parsed["error"]
            .as_str()
            .expect("error should be a string")
            .contains("get_tick"));

        assert!(old_mod_claude_command_skew_response("get_tick", "pong").is_none());
    }

    #[test]
    fn mutating_lua_response_errors_are_promoted_to_rust_errors() {
        for response in [
            r#"{"error":"Entity not found"}"#,
            r#"{"success":false,"error":"No path"}"#,
            r#"{"inserted":0,"error":"Inventory full"}"#,
        ] {
            let err = ensure_lua_success(response).expect_err("error JSON should be rejected");
            assert!(err.to_string().contains("error") || !err.to_string().is_empty());
        }
    }

    #[test]
    fn mutating_lua_success_shapes_stay_accepted() {
        for response in ["ok", r#"{"inserted": 1}"#, r#"{"success": true}"#] {
            ensure_lua_success(response).expect("success response should remain accepted");
        }
    }

    #[test]
    fn null_entity_lookup_reports_not_found() {
        let err = parse_entity_response("null").expect_err("Lua null is not an entity");
        assert!(err.to_string().contains("Entity not found"));

        let err = parse_entity_response(r#"{"error":"Entity not found"}"#)
            .expect_err("explicit Lua entity errors must stay errors");
        assert!(err.to_string().contains("Entity not found"));
    }

    #[test]
    fn empty_lua_table_deserializes_as_empty_vec() {
        // helpers.table_to_json({}) returns "{}" (object), not "[]". The situation
        // report and every find_* query relied on this NOT exploding.
        for response in ["{}", " {} ", ""] {
            let parsed = parse_lua_array::<Surface>(response)
                .expect("empty Lua table must yield an empty vec, not an error");
            assert!(parsed.is_empty());
        }
    }

    #[test]
    fn populated_lua_array_still_deserializes() {
        let parsed = parse_lua_array::<Surface>(
            r#"[{"name":"nauvis","index":1},{"name":"orbit","index":2}]"#,
        )
        .expect("a real array must still deserialize");
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].name, "nauvis");
    }

    #[test]
    fn lua_empty_object_vec_fields_deserialize_in_result_structs() {
        let build: BuildResult =
            serde_json::from_str(r#"{"placed":0,"total":1,"entities":{},"errors":{}}"#)
                .expect("empty build result lists should accept Lua {}");
        assert!(build.entities.is_empty());
        assert!(build.errors.is_empty());

        let belts: BeltContentsResult = serde_json::from_str(
            r#"{"belt_count":1,"total_items":0,"item_summary":{},"belts":[{"position":{"x":0.5,"y":0.5},"unit_number":7,"items":{}}]}"#,
        )
        .expect("empty belt summary lists should accept Lua {}");
        assert!(belts.item_summary.is_empty());
        assert!(belts.belts[0].items.is_empty());

        let inventory: Inventory = serde_json::from_str(r#"{"items":{},"free_slots":10}"#)
            .expect("empty inventory list should accept Lua {}");
        assert!(inventory.items.is_empty());

        let mined: MineResult =
            serde_json::from_str(r#"{"success":false,"mined_count":0,"inventory":{}}"#)
                .expect("empty mine inventory should accept Lua {}");
        assert!(mined.inventory.is_empty());

        let crafted: CraftResult =
            serde_json::from_str(r#"{"success":true,"queued":0,"queue_size":0,"queue":{}}"#)
                .expect("empty crafting queue should accept Lua {}");
        assert!(crafted.queue.is_empty());

        let recipe: Recipe = serde_json::from_str(
            r#"{"name":"boiler","category":"crafting","energy":0.5,"enabled":false,"unlocked_by":["steam-power"],"ingredients":{},"products":{}}"#,
        )
        .expect("recipe metadata and Lua empty lists should deserialize");
        assert!(!recipe.enabled);
        assert_eq!(recipe.unlocked_by, vec!["steam-power"]);
        assert!(recipe.ingredients.is_empty());
        assert!(recipe.products.is_empty());

        #[derive(serde::Deserialize)]
        struct RawLane {
            #[serde(default, deserialize_with = "crate::world::deserialize_lua_empty_vec")]
            items: Vec<InventoryItem>,
        }
        let lane: RawLane =
            serde_json::from_str(r#"{"items":{}}"#).expect("raw belt lanes should accept Lua {}");
        assert!(lane.items.is_empty());
    }

    #[test]
    fn crafting_queue_snapshot_requires_structured_current_and_remaining_queue() {
        let snapshot = parse_crafting_queue_snapshot(
            r#"{"success":true,"queue_size":3,"current_recipe":"copper-cable","queue":[{"recipe":"copper-cable","count":2},{"recipe":"electronic-circuit","count":1}]}"#,
        )
        .unwrap();
        assert_eq!(snapshot.queue_size, 3);
        assert_eq!(snapshot.current_recipe.as_deref(), Some("copper-cable"));
        assert_eq!(snapshot.queue.len(), 2);
        assert_eq!(snapshot.queue[1].recipe, "electronic-circuit");

        for response in [
            "0",
            "17",
            "not-a-number",
            r#"{"success":false,"error":"no character"}"#,
        ] {
            assert!(
                parse_crafting_queue_snapshot(response).is_err(),
                "response must not be accepted: {response}"
            );
        }
    }

    #[test]
    fn crafting_poll_state_requires_an_observed_empty_queue() {
        assert_eq!(crafting_poll_status(2, false), CraftingStatus::Pending);
        assert_eq!(crafting_poll_status(2, true), CraftingStatus::TimedOut);
        assert_eq!(crafting_poll_status(0, false), CraftingStatus::Completed);
        assert_eq!(crafting_poll_status(0, true), CraftingStatus::Completed);
    }

    #[tokio::test]
    async fn crafting_poll_waits_through_pending_samples_until_zero() {
        use std::collections::VecDeque;

        let mut samples = VecDeque::from([
            queue_snapshot(3, &[("copper-cable", 2), ("electronic-circuit", 1)]),
            queue_snapshot(2, &[("electronic-circuit", 1)]),
            queue_snapshot(0, &[]),
        ]);
        let evidence = poll_crafting_queue(
            &mut samples,
            Duration::from_secs(1),
            Duration::from_nanos(1),
            |samples| {
                Box::pin(async move {
                    samples
                        .pop_front()
                        .ok_or_else(|| anyhow::anyhow!("poll read past scripted samples"))
                })
            },
        )
        .await
        .expect("scripted queue should complete");

        assert_eq!(evidence.status, CraftingStatus::Completed);
        assert_eq!(evidence.initial_queue_size, 3);
        assert_eq!(evidence.remaining_queue_size, 0);
        assert!(evidence.current_recipe.is_none());
        assert!(evidence.remaining_queue.is_empty());
        assert_eq!(evidence.polls, 3);
        assert!(samples.is_empty());
    }

    #[tokio::test]
    async fn crafting_poll_returns_structured_timeout_evidence() {
        let mut snapshot = queue_snapshot(4, &[("copper-cable", 3), ("electronic-circuit", 1)]);
        let evidence = poll_crafting_queue(
            &mut snapshot,
            Duration::ZERO,
            Duration::from_millis(1),
            |snapshot| Box::pin(async move { Ok(snapshot.clone()) }),
        )
        .await
        .expect("craft timeout is a status at the lower-level API");

        assert_eq!(evidence.status, CraftingStatus::TimedOut);
        assert_eq!(evidence.initial_queue_size, 4);
        assert_eq!(evidence.remaining_queue_size, 4);
        assert_eq!(evidence.current_recipe.as_deref(), Some("copper-cable"));
        assert_eq!(evidence.remaining_queue.len(), 2);
        assert_eq!(evidence.remaining_queue[1].recipe, "electronic-circuit");
        assert_eq!(evidence.polls, 1);
        assert!(!evidence.is_completed());
    }
}
