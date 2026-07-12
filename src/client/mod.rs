//! Factorio client for communicating with the game server

pub mod lua;
pub mod rcon;
pub mod server;

use anyhow::{bail, Result};
use serde_json::{json, Value};

use crate::world::{
    Area, BeltContentsResult, BeltLaneContentsResult, BeltLaneSummary, BuildResult,
    CharacterStatus, CollisionMap, CraftResult, Direction, Entity, EntityProduction, GatherResult,
    GridPos, Inventory, InventoryItem, LaneContents, MineResult, PlacementSpec, Position,
    Prototype, Recipe, RecipeSummary, ResourcePatch, Surface, Tick, Tile, TilePos, WalkResult,
};
use rcon::RconClient;

/// Maximum distance for placing entities
pub const PROXIMITY_RANGE_PLACE: f64 = 10.0;
/// Maximum distance for inserting items
pub const PROXIMITY_RANGE_INSERT: f64 = 5.0;
/// Maximum distance for setting recipes
pub const PROXIMITY_RANGE_INTERACT: f64 = 5.0;

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
pub struct FactorioClient {
    rcon: RconClient,
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
            rcon,
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
        self.rcon.close().await
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
            .execute(&format!("{} {}", prefix, single_line))
            .await
    }

    pub async fn call_remote(&mut self, fn_name: &str, args: &[Value]) -> Result<String> {
        let command = claude_command(fn_name, args);
        let response = self.rcon.execute(&command).await?;
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
                ],
            )
            .await?;
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
                &[json!(resource_name), json!(from.x), json!(from.y)],
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
                ],
            )
            .await?;
        let tiles = parse_lua_array::<Tile>(&response)?;
        Ok(tiles)
    }

    /// Get a specific tile
    pub async fn get_tile(&mut self, position: Position) -> Result<Tile> {
        let response = self
            .call_remote("get_tile", &[json!(position.x), json!(position.y)])
            .await?;
        let tile: Tile = serde_json::from_str(&response)?;
        Ok(tile)
    }

    // --- Pathfinding Support ---

    /// Build a collision map for pathfinding in an area
    pub async fn build_collision_map(&mut self, area: Area) -> Result<CollisionMap> {
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
        for entity in entities {
            // Skip resources (can build on top of ore)
            if entity.entity_type.as_deref() == Some("resource") {
                continue;
            }
            // Skip character
            if entity.name == "character" {
                continue;
            }
            // Skip items on ground
            if entity.entity_type.as_deref() == Some("item-entity") {
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

    /// Mine entity at position
    /// Walks to the entity if needed, then mines with mine_entity
    pub async fn mine_at(&mut self, position: Position, count: u32) -> Result<MineResult> {
        // Walk to the target first
        let char_pos = self.get_character_position().await?;
        let dist = ((position.x - char_pos.x).powi(2) + (position.y - char_pos.y).powi(2)).sqrt();
        if dist > 2.5 {
            let _ = self.walk_to(position, false).await?;
        }

        let response = self
            .call_remote(
                "mine_at",
                &[
                    json!(self.agent_id.as_str()),
                    json!(position.x),
                    json!(position.y),
                    json!(count),
                    json!(3),
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

    /// Wait for crafting to complete
    pub async fn wait_for_crafting(&mut self) -> Result<()> {
        self.call_remote("wait_for_crafting", &[json!(self.agent_id.as_str())])
            .await?;
        Ok(())
    }

    // --- Entity Actions ---

    /// Place an entity from inventory
    pub async fn place_entity(
        &mut self,
        entity_name: &str,
        position: Position,
        direction: Direction,
    ) -> Result<Entity> {
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
    pub async fn rotate_entity(&mut self, unit_number: u32, direction: u8) -> Result<()> {
        let response = self
            .call_remote("rotate_entity", &[json!(unit_number), json!(direction)])
            .await?;
        if response.contains("error") {
            anyhow::bail!("Failed to rotate entity: {}", response);
        }
        Ok(())
    }

    /// Insert items into an entity
    pub async fn insert_items(
        &mut self,
        unit_number: u32,
        item: &str,
        count: u32,
        inventory_type: &str,
    ) -> Result<serde_json::Value> {
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

    /// Extract items from an entity into player inventory
    pub async fn extract_items(
        &mut self,
        unit_number: u32,
        item: &str,
        count: u32,
        inventory_type: &str,
    ) -> Result<u32> {
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

    /// Set recipe on an assembling machine
    pub async fn set_recipe(&mut self, unit_number: u32, recipe: &str) -> Result<()> {
        let response = self
            .call_remote("set_recipe", &[json!(unit_number), json!(recipe)])
            .await?;
        ensure_lua_success(&response)?;
        Ok(())
    }

    pub async fn feed_lab_from_inventory(
        &mut self,
        lab_unit_number: u32,
        science_pack: &str,
        count: u32,
        dry_run: bool,
    ) -> Result<serde_json::Value> {
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
            .call_remote("is_tech_researched", &[json!(tech_name)])
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

        loop {
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
            let current = self.get_tick().await?.tick;
            if current >= target {
                break;
            }
        }

        Ok(())
    }

    // --- Proximity Checks ---

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
        use crate::world::find_walk_path;

        let start_pos = self.get_character_position().await?;
        let start_grid = GridPos::from_position(&start_pos);
        let end_grid = GridPos::from_position(&target);

        // If already at target, return immediately
        if start_pos.distance(&target) < 1.5 {
            return Ok(WalkResult {
                arrived: true,
                final_position: start_pos,
                distance_walked: 0.0,
                reason: None,
            });
        }

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

        // Find path using A*
        let path_result = find_walk_path(start_grid, end_grid, &collision_map);

        if !path_result.success {
            // Fall back to direct walking
            return self.walk_to(target, false).await;
        }

        // Walk through each waypoint
        let mut total_distance = 0.0;

        for waypoint in &path_result.path {
            let waypoint_pos = waypoint.to_position();

            // Walk to this waypoint using the direct method
            let result = self.walk_to(waypoint_pos, false).await?;
            total_distance += result.distance_walked;

            if !result.arrived && result.final_position.distance(&waypoint_pos) > 3.0 {
                // Got stuck, return current result
                return Ok(WalkResult {
                    arrived: false,
                    final_position: result.final_position,
                    distance_walked: total_distance,
                    reason: Some("Blocked on path".to_string()),
                });
            }
        }

        // Check if we arrived
        let final_pos = self.get_character_position().await?;
        let arrived = final_pos.distance(&target) < 3.0;

        Ok(WalkResult {
            arrived,
            final_position: final_pos,
            distance_walked: total_distance,
            reason: if arrived {
                None
            } else {
                Some("Did not reach target".to_string())
            },
        })
    }

    /// Smooth walk to a target position (direct, no pathfinding)
    pub async fn walk_to(&mut self, target: Position, _run: bool) -> Result<WalkResult> {
        let mut total_distance = 0.0;
        let start_pos = self.get_character_position().await?;
        let mut last_pos = start_pos;

        let target_response = self
            .call_remote(
                "set_walk_target",
                &[
                    json!(self.agent_id.as_str()),
                    json!(target.x),
                    json!(target.y),
                ],
            )
            .await?;
        ensure_lua_success(&target_response)?;

        for _ in 0..500 {
            let pos = self.get_character_position().await?;
            total_distance += pos.distance(&last_pos);

            if pos.distance(&target) < 3.0 {
                self.call_remote("clear_walk_target", &[json!(self.agent_id.as_str())])
                    .await?;
                return Ok(WalkResult {
                    arrived: true,
                    final_position: pos,
                    distance_walked: total_distance,
                    reason: None,
                });
            }

            let target_active = self
                .call_remote("has_walk_target", &[json!(self.agent_id.as_str())])
                .await?;
            if target_active.trim() != "true" {
                return Ok(WalkResult {
                    arrived: pos.distance(&target) < 3.0,
                    final_position: pos,
                    distance_walked: total_distance,
                    reason: Some("Walk target cleared".to_string()),
                });
            }

            last_pos = pos;
            tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
        }

        self.call_remote("clear_walk_target", &[json!(self.agent_id.as_str())])
            .await?;
        let pos = self.get_character_position().await?;
        Ok(WalkResult {
            arrived: false,
            final_position: pos,
            distance_walked: total_distance,
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

            let walk_result = self.walk_to(target_pos, false).await?;
            total_distance += walk_result.distance_walked;

            let mine_result = self.mine_at(target_pos, 1).await?;
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
}
