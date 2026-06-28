//! Factorio client for communicating with the game server

pub mod lua;
pub mod rcon;
pub mod server;

use anyhow::{bail, Result};

use crate::world::{
    Area, BeltContentsResult, BeltLaneContentsResult, BeltLaneSummary, BuildResult,
    CharacterStatus, CollisionMap, CraftResult, Direction, Entity, EntityProduction, GatherResult,
    GridPos, Inventory, InventoryItem, LaneContents, MineResult, PlacementSpec, Position,
    Prototype, Recipe, RecipeSummary, ResourcePatch, Surface, Tick, Tile, TilePos, WalkResult,
};
use lua::LuaCommand;
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

    // --- Game State Queries ---

    /// Get current game tick
    pub async fn get_tick(&mut self) -> Result<Tick> {
        let response = self.execute_lua("rcon.print(game.tick)").await?;
        let tick: u64 = response.trim().parse()?;
        Ok(Tick { tick })
    }

    /// Get list of surfaces
    pub async fn get_surfaces(&mut self) -> Result<Vec<Surface>> {
        let lua = LuaCommand::get_surfaces();
        let response = self.execute_lua(&lua).await?;
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
        let lua = LuaCommand::find_entities(area, entity_type, name);
        let response = self.execute_lua(&lua).await?;
        let entities = parse_lua_array::<Entity>(&response)?;
        Ok(entities)
    }

    /// Get a specific entity by unit number
    pub async fn get_entity(&mut self, unit_number: u32) -> Result<Entity> {
        let lua = LuaCommand::get_entity(unit_number);
        let response = self.execute_lua(&lua).await?;
        parse_entity_response(&response)
    }

    /// Get an entity's inventories
    pub async fn get_entity_inventory(&mut self, unit_number: u32) -> Result<serde_json::Value> {
        let lua = LuaCommand::get_entity_inventory(unit_number);
        let response = self.execute_lua(&lua).await?;
        let result: serde_json::Value = serde_json::from_str(&response)?;
        Ok(result)
    }

    /// Verify production status for producing entities in an area
    pub async fn verify_production(&mut self, area: Area) -> Result<Vec<EntityProduction>> {
        let lua = LuaCommand::verify_production(area);
        let response = self.execute_lua(&lua).await?;
        let entities = parse_lua_array::<EntityProduction>(&response)?;
        Ok(entities)
    }

    // --- Resource Queries ---

    /// Find resources in an area
    pub async fn find_resources(
        &mut self,
        area: Area,
        resource_type: Option<&str>,
    ) -> Result<Vec<ResourcePatch>> {
        let lua = LuaCommand::find_resources(area, resource_type);
        let response = self.execute_lua(&lua).await?;
        let resources = parse_lua_array::<ResourcePatch>(&response)?;
        Ok(resources)
    }

    /// Find nearest resource from a position
    pub async fn find_nearest_resource(
        &mut self,
        resource_name: &str,
        from: Position,
    ) -> Result<ResourcePatch> {
        let lua = LuaCommand::find_nearest_resource(resource_name, from);
        let response = self.execute_lua(&lua).await?;
        let resource: ResourcePatch = serde_json::from_str(&response)?;
        Ok(resource)
    }

    // --- Tile Queries ---

    /// Get tiles in an area
    pub async fn get_tiles(&mut self, area: Area) -> Result<Vec<Tile>> {
        let lua = LuaCommand::get_tiles(area);
        let response = self.execute_lua(&lua).await?;
        let tiles = parse_lua_array::<Tile>(&response)?;
        Ok(tiles)
    }

    /// Get a specific tile
    pub async fn get_tile(&mut self, position: Position) -> Result<Tile> {
        let lua = LuaCommand::get_tile(position);
        let response = self.execute_lua(&lua).await?;
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
        let lua = LuaCommand::get_recipe(name);
        let response = self.execute_lua(&lua).await?;
        let recipe: Recipe = serde_json::from_str(&response)?;
        Ok(recipe)
    }

    /// Get all recipes in a category
    pub async fn get_recipes_by_category(&mut self, category: &str) -> Result<Vec<RecipeSummary>> {
        let lua = LuaCommand::get_recipes_by_category(category);
        let response = self.execute_lua(&lua).await?;
        let recipes = parse_lua_array::<RecipeSummary>(&response)?;
        Ok(recipes)
    }

    /// Get all recipes that produce a specific item
    pub async fn get_recipes_for_item(&mut self, item: &str) -> Result<Vec<Recipe>> {
        let lua = LuaCommand::get_recipes_for_item(item);
        let response = self.execute_lua(&lua).await?;
        let recipes = parse_lua_array::<Recipe>(&response)?;
        Ok(recipes)
    }

    /// Get an entity prototype by name
    pub async fn get_prototype(&mut self, name: &str) -> Result<Prototype> {
        let lua = LuaCommand::get_prototype(name);
        let response = self.execute_lua(&lua).await?;
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
        let lua = LuaCommand::create_native_blueprint(agent_id, area);
        let response = self.execute_lua(&lua).await?;
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
        let lua = LuaCommand::save_blueprint(agent_id, name, area);
        let response = self.execute_lua(&lua).await?;
        let result: crate::world::BlueprintSaveResult = serde_json::from_str(&response)?;
        Ok(result)
    }

    /// List all saved blueprints
    pub async fn list_blueprints(&mut self) -> Result<Vec<crate::world::StoredBlueprint>> {
        let lua = LuaCommand::list_blueprints();
        let response = self.execute_lua(&lua).await?;
        let result = parse_lua_array::<crate::world::StoredBlueprint>(&response)?;
        Ok(result)
    }

    /// Get a saved blueprint string by name
    pub async fn get_blueprint(&mut self, name: &str) -> Result<crate::world::BlueprintGetResult> {
        let lua = LuaCommand::get_blueprint(name);
        let response = self.execute_lua(&lua).await?;
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
        let lua = LuaCommand::place_blueprint(agent_id, name, position, direction);
        let response = self.execute_lua(&lua).await?;
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
        let lua = LuaCommand::import_blueprint(agent_id, bp_string, position, direction);
        let response = self.execute_lua(&lua).await?;
        let result: crate::world::BlueprintPlaceResult = serde_json::from_str(&response)?;
        Ok(result)
    }

    /// Delete a saved blueprint
    pub async fn delete_blueprint(&mut self, name: &str) -> Result<bool> {
        let lua = LuaCommand::delete_blueprint(name);
        let response = self.execute_lua(&lua).await?;
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
        let lua = LuaCommand::init_character(&self.agent_id, x, y);
        let response = self.execute_lua(&lua).await?;
        let entity: Entity = serde_json::from_str(&response)?;
        Ok(entity)
    }

    /// Teleport character to position
    pub async fn teleport_character(&mut self, position: Position) -> Result<()> {
        let lua = LuaCommand::teleport_character(&self.agent_id, position);
        let response = self.execute_lua(&lua).await?;
        ensure_lua_success(&response)?;
        Ok(())
    }

    /// Start walking character to position
    pub async fn walk_character(&mut self, position: Position) -> Result<()> {
        let lua = LuaCommand::walk_character(&self.agent_id, position);
        let response = self.execute_lua(&lua).await?;
        ensure_lua_success(&response)?;
        Ok(())
    }

    /// Get character status
    pub async fn character_status(&mut self) -> Result<CharacterStatus> {
        let lua = LuaCommand::character_status(&self.agent_id);
        let response = self.execute_lua(&lua).await?;
        let status: CharacterStatus = serde_json::from_str(&response)?;
        Ok(status)
    }

    /// Get character inventory
    pub async fn character_inventory(&mut self) -> Result<Inventory> {
        let lua = LuaCommand::character_inventory(&self.agent_id);
        let response = self.execute_lua(&lua).await?;
        let inventory: Inventory = serde_json::from_str(&response)?;
        Ok(inventory)
    }

    // --- Mining ---

    /// Mine entity at position
    /// Walks to the entity if needed, then mines with mine_entity
    pub async fn mine_at(&mut self, position: Position, count: u32) -> Result<MineResult> {
        // Get initial inventory count
        let inv_before = self.character_inventory().await?;
        let count_before: u32 = inv_before.items.iter().map(|i| i.count).sum();

        // Walk to the target first
        let char_pos = self.get_character_position().await?;
        let dist = ((position.x - char_pos.x).powi(2) + (position.y - char_pos.y).powi(2)).sqrt();
        if dist > 2.5 {
            let _ = self.walk_to(position, false).await?;
        }

        // Mine using mine_entity (instant but reliable), also handles items on ground
        let resolve = LuaCommand::resolve_required(&self.agent_id);
        let mine_lua = format!(
            r#"
{}

local inv = c.get_main_inventory()
local mined = 0
local picked_up = 0

for i = 1, {} do
    -- First check for items on ground (item-entity type)
    local items_on_ground = game.surfaces[1].find_entities_filtered{{
        position = {{ {}, {} }},
        radius = 3,
        type = "item-entity"
    }}

    if #items_on_ground > 0 then
        local item_entity = items_on_ground[1]
        local stack = item_entity.stack
        if stack and stack.valid_for_read and inv then
            local inserted = inv.insert(stack)
            if inserted > 0 then
                picked_up = picked_up + inserted
                item_entity.destroy()
            end
        end
    else
        -- Try resources first
        local resources = game.surfaces[1].find_entities_filtered{{
            position = {{ {}, {} }},
            radius = 3,
            type = "resource"
        }}

        local target = nil
        if #resources > 0 then
            target = resources[1]
        else
            local entities = game.surfaces[1].find_entities_filtered{{
                position = {{ {}, {} }},
                radius = 3
            }}
            for _, e in pairs(entities) do
                if e.minable and e ~= c then
                    target = e
                    break
                end
            end
        end

        if target then
            c.mine_entity(target, true)
            mined = mined + 1
        else
            break
        end
    end
end

rcon.print('{{"success": true, "mined": ' .. mined .. ', "picked_up": ' .. picked_up .. '}}')
"#,
            resolve, count, position.x, position.y, position.x, position.y, position.x, position.y
        );

        let _ = self.execute_lua(&mine_lua).await?;

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

    /// Mine nearest entity of a type
    /// Walks to nearest entity and mines it
    pub async fn mine_nearest(&mut self, entity_type: &str, count: u32) -> Result<MineResult> {
        // Get initial inventory
        let inv_before = self.character_inventory().await?;
        let count_before: u32 = inv_before.items.iter().map(|i| i.count).sum();
        let entity_type_lua = LuaCommand::lua_escape(entity_type);

        for _ in 0..count {
            // Find nearest entity of type
            let resolve = LuaCommand::resolve_required(&self.agent_id);
            let find_lua = format!(
                r#"
{}

local entities = game.surfaces[1].find_entities_filtered{{
    name = "{}",
    position = c.position,
    radius = 100
}}

local nearest = nil
local nearest_dist = math.huge
for _, e in pairs(entities) do
    if e.minable then
        local dx = e.position.x - c.position.x
        local dy = e.position.y - c.position.y
        local dist = dx*dx + dy*dy
        if dist < nearest_dist then
            nearest = e
            nearest_dist = dist
        end
    end
end

if nearest then
    rcon.print(nearest.position.x .. "," .. nearest.position.y)
else
    rcon.print("none")
end
"#,
                resolve, entity_type_lua
            );

            let response = self.execute_lua(&find_lua).await?;
            if response.trim() == "none" || response.trim() == "error" {
                break;
            }

            // Parse position
            let parts: Vec<&str> = response.trim().split(',').collect();
            if parts.len() != 2 {
                break;
            }
            let target_pos = Position {
                x: parts[0].parse().unwrap_or(0.0),
                y: parts[1].parse().unwrap_or(0.0),
            };

            // Walk to and mine
            let _ = self.mine_at(target_pos, 1).await?;
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
        let lua = LuaCommand::craft(&self.agent_id, recipe, count);
        let response = self.execute_lua(&lua).await?;
        let result: CraftResult = serde_json::from_str(&response)?;
        Ok(result)
    }

    /// Wait for crafting to complete
    pub async fn wait_for_crafting(&mut self) -> Result<()> {
        let lua = LuaCommand::wait_for_crafting(&self.agent_id);
        self.execute_lua(&lua).await?;
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
        let lua = LuaCommand::place_entity(&self.agent_id, entity_name, position, direction);
        let response = self.execute_lua(&lua).await?;
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
        let lua =
            LuaCommand::check_entity_placement(&self.agent_id, entity_name, position, direction);
        let response = self.execute_lua(&lua).await?;
        Ok(serde_json::from_str(&response)?)
    }

    pub async fn find_entity_placements(
        &mut self,
        entity_name: &str,
        center: Position,
        radius: u32,
        limit: u32,
    ) -> Result<serde_json::Value> {
        let lua =
            LuaCommand::find_entity_placements(&self.agent_id, entity_name, center, radius, limit);
        let response = self.execute_lua(&lua).await?;
        Ok(serde_json::from_str(&response)?)
    }

    /// Place a ghost entity (for planning, doesn't require items)
    pub async fn place_ghost(
        &mut self,
        entity_name: &str,
        position: Position,
        direction: Direction,
    ) -> Result<Entity> {
        let lua = LuaCommand::place_ghost(&self.agent_id, entity_name, position, direction);
        let response = self.execute_lua(&lua).await?;
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
        let lua = LuaCommand::remove_entity_at(position);
        let response = self.execute_lua(&lua).await?;
        ensure_lua_success(&response)?;
        Ok(())
    }

    /// Remove entity by unit number
    pub async fn remove_entity(&mut self, unit_number: u32) -> Result<()> {
        let lua = LuaCommand::remove_entity(unit_number);
        let response = self.execute_lua(&lua).await?;
        ensure_lua_success(&response)?;
        Ok(())
    }

    /// Rotate entity to a new direction
    pub async fn rotate_entity(&mut self, unit_number: u32, direction: u8) -> Result<()> {
        let lua = LuaCommand::rotate_entity(unit_number, direction);
        let response = self.execute_lua(&lua).await?;
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
    ) -> Result<()> {
        let lua = LuaCommand::insert_items(unit_number, item, count, inventory_type);
        let response = self.execute_lua(&lua).await?;
        ensure_lua_success(&response)?;
        Ok(())
    }

    /// Extract items from an entity into player inventory
    pub async fn extract_items(
        &mut self,
        unit_number: u32,
        item: &str,
        count: u32,
        inventory_type: &str,
    ) -> Result<u32> {
        let lua =
            LuaCommand::extract_items(&self.agent_id, unit_number, item, count, inventory_type);
        let response = self.execute_lua(&lua).await?;

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
        let lua = LuaCommand::set_recipe(unit_number, recipe);
        let response = self.execute_lua(&lua).await?;
        ensure_lua_success(&response)?;
        Ok(())
    }

    /// Check if a technology has been researched
    pub async fn is_tech_researched(&mut self, tech_name: &str) -> Result<bool> {
        let tech_name = LuaCommand::lua_escape(tech_name);
        let lua = format!(
            r#"local tech = game.forces.player.technologies["{}"]
               rcon.print(tech and tech.researched and "true" or "false")"#,
            tech_name
        );
        let result = self.execute_lua(&lua).await?;
        Ok(result.trim() == "true")
    }

    /// Place an underground belt with specified type (input or output)
    pub async fn place_underground_belt(
        &mut self,
        entity_name: &str,
        position: Position,
        direction: Direction,
        belt_type: &str, // "input" for entry, "output" for exit
    ) -> Result<Entity> {
        let lua = LuaCommand::place_underground_belt(
            &self.agent_id,
            entity_name,
            position,
            direction,
            belt_type,
        );
        let response = self.execute_lua(&lua).await?;
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
        self.execute_lua("game.tick_paused = true").await?;
        Ok(())
    }

    /// Resume the game
    pub async fn resume_game(&mut self) -> Result<()> {
        self.execute_lua("game.tick_paused = false").await?;
        Ok(())
    }

    /// Set game speed
    pub async fn set_game_speed(&mut self, speed: f64) -> Result<()> {
        let lua = format!("game.speed = {}", speed);
        self.execute_lua(&lua).await?;
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
        let lua = format!(
            "{}\n{}",
            LuaCommand::resolve_optional(&self.agent_id),
            r#"
if c and c.valid then
    rcon.print(c.position.x .. "," .. c.position.y)
else
    rcon.print("error")
end
"#
        );
        let response = self.execute_lua(&lua).await?;
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

        if !self.agent_id.is_legacy() {
            let target_lua = LuaCommand::set_walk_target(&self.agent_id, target);
            let clear_lua = LuaCommand::clear_walk_target(&self.agent_id);
            let active_lua = LuaCommand::walk_target_active(&self.agent_id);
            self.execute_lua(&target_lua).await?;
            let mut last_pos = start_pos;

            for _ in 0..500 {
                let pos = self.get_character_position().await?;
                total_distance += pos.distance(&last_pos);

                if pos.distance(&target) < 3.0 {
                    self.execute_lua(&clear_lua).await?;
                    return Ok(WalkResult {
                        arrived: true,
                        final_position: pos,
                        distance_walked: total_distance,
                        reason: None,
                    });
                }

                let target_active = self.execute_lua(&active_lua).await?;
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

            self.execute_lua(&clear_lua).await?;
            let pos = self.get_character_position().await?;
            return Ok(WalkResult {
                arrived: false,
                final_position: pos,
                distance_walked: total_distance,
                reason: Some("Timeout".to_string()),
            });
        }

        let mut last_pos = start_pos;
        let mut last_dist = start_pos.distance(&target);
        let mut stuck_count = 0;
        let mut overshoot_count = 0;

        // Helper to stop walking
        let stop_lua = format!(
            "{}\nif c then c.walking_state = {{walking=false}} end",
            LuaCommand::resolve_required(&self.agent_id)
        );

        for i in 0..500 {
            let pos = self.get_character_position().await?;
            let dx = target.x - pos.x;
            let dy = target.y - pos.y;
            let dist = (dx * dx + dy * dy).sqrt();

            // Track distance moved this step
            let step_dist = pos.distance(&last_pos);
            total_distance += step_dist;

            // Check if arrived (generous tolerance of 3 tiles)
            if dist < 3.0 {
                self.execute_lua(&stop_lua).await?;
                return Ok(WalkResult {
                    arrived: true,
                    final_position: pos,
                    distance_walked: total_distance,
                    reason: None,
                });
            }

            // Check if we're moving away from target (overshoot detection)
            if i > 2 && dist > last_dist + 0.5 {
                overshoot_count += 1;
                if overshoot_count >= 2 {
                    self.execute_lua(&stop_lua).await?;
                    return Ok(WalkResult {
                        arrived: dist < 5.0, // Close enough
                        final_position: pos,
                        distance_walked: total_distance,
                        reason: if dist < 5.0 {
                            None
                        } else {
                            Some("Overshot target".to_string())
                        },
                    });
                }
            } else {
                overshoot_count = 0;
            }

            // Check if stuck
            if i > 3 && step_dist < 0.01 && dist > 3.0 {
                stuck_count += 1;
                if stuck_count >= 3 {
                    self.execute_lua(&stop_lua).await?;
                    return Ok(WalkResult {
                        arrived: false,
                        final_position: pos,
                        distance_walked: total_distance,
                        reason: Some("Blocked or stuck".to_string()),
                    });
                }
            } else {
                stuck_count = 0;
            }

            last_pos = pos;
            last_dist = dist;

            // Calculate direction using explicit 8-direction logic
            // In Factorio: North=-Y, East=+X, South=+Y, West=-X
            let dir_name = if dx.abs() < 0.5 {
                if dy < 0.0 {
                    "north"
                } else {
                    "south"
                }
            } else if dy.abs() < 0.5 {
                if dx > 0.0 {
                    "east"
                } else {
                    "west"
                }
            } else {
                let ratio = dy.abs() / dx.abs();
                if ratio < 0.414 {
                    if dx > 0.0 {
                        "east"
                    } else {
                        "west"
                    }
                } else if ratio > 2.414 {
                    if dy < 0.0 {
                        "north"
                    } else {
                        "south"
                    }
                } else {
                    match (dx > 0.0, dy < 0.0) {
                        (true, true) => "northeast",
                        (true, false) => "southeast",
                        (false, false) => "southwest",
                        (false, true) => "northwest",
                    }
                }
            };

            // Set walking state using Factorio's defines.direction
            let resolve = LuaCommand::resolve_required(&self.agent_id);
            let lua = format!(
                r#"{}
if c then c.walking_state = {{walking=true, direction=defines.direction.{}}} end"#,
                resolve, dir_name
            );
            self.execute_lua(&lua).await?;

            tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
        }

        // Timeout
        self.execute_lua(&stop_lua).await?;
        let pos = self.get_character_position().await?;
        Ok(WalkResult {
            arrived: false,
            final_position: pos,
            distance_walked: start_pos.distance(&pos),
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
        let resource_name_lua = LuaCommand::lua_escape(resource_name);

        for _ in 0..amount {
            // Find nearest resource
            let resolve = LuaCommand::resolve_required(&self.agent_id);
            let find_lua = format!(
                r#"
{}

local entities = game.surfaces[1].find_entities_filtered{{
    name = "{}",
    position = c.position,
    radius = {}
}}

local nearest = nil
local nearest_dist = math.huge
for _, e in pairs(entities) do
    local dx = e.position.x - c.position.x
    local dy = e.position.y - c.position.y
    local dist = dx*dx + dy*dy
    if dist < nearest_dist then
        nearest = e
        nearest_dist = dist
    end
end

if nearest then
    rcon.print(nearest.position.x .. "," .. nearest.position.y)
else
    rcon.print("none")
end
"#,
                resolve, resource_name_lua, radius
            );

            let response = self.execute_lua(&find_lua).await?;
            if response.trim() == "none" || response.trim() == "error" {
                break;
            }

            // Parse position
            let parts: Vec<&str> = response.trim().split(',').collect();
            if parts.len() != 2 {
                break;
            }
            let target_pos = Position {
                x: parts[0].parse().unwrap_or(0.0),
                y: parts[1].parse().unwrap_or(0.0),
            };

            // Walk to the resource
            let walk_result = self.walk_to(target_pos, false).await?;
            total_distance += walk_result.distance_walked;

            // Mine the resource using mine_entity (instant but reliable)
            let resolve = LuaCommand::resolve_required(&self.agent_id);
            let mine_lua = format!(
                r#"
{}

local resources = game.surfaces[1].find_entities_filtered{{
    position = {{ x = {}, y = {} }},
    radius = 0.5,
    type = "resource"
}}
if #resources > 0 then
    c.mine_entity(resources[1], true)
    rcon.print("mined")
else
    rcon.print("no_resource")
end
"#,
                resolve, target_pos.x, target_pos.y
            );
            let mine_result = self.execute_lua(&mine_lua).await?;

            match mine_result.trim() {
                "mined" => gathered += 1,
                "no_char" | "no_resource" => break,
                _ => {}
            }
        }

        // Get final inventory
        let inv_lua = format!(
            "{}\n{}",
            LuaCommand::resolve_required(&self.agent_id),
            r#"
if not (c and c.valid) then rcon.print('{"items":[]}') return end
local inv = c.get_main_inventory()
local items = {}
if inv then
    for _, item in pairs(inv.get_contents()) do
        table.insert(items, { name = item.name, count = item.count })
    end
end
if #items == 0 then
    rcon.print('{"items":[]}')
else
    rcon.print(helpers.table_to_json({ items = items }))
end
"#
        );
        let inv_response = self.execute_lua(&inv_lua).await?;
        #[derive(serde::Deserialize)]
        struct InvResult {
            items: Vec<crate::world::InventoryItem>,
        }
        let inv_result: InvResult =
            serde_json::from_str(&inv_response).unwrap_or(InvResult { items: Vec::new() });

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
        let dir = Direction::from_name(direction).unwrap_or(Direction::South);
        let near_pos = near.unwrap_or((0.0, 0.0));
        let drill_type_lua = LuaCommand::lua_escape(drill_type);
        let resource_lua = LuaCommand::lua_escape(resource);

        let resolve = LuaCommand::resolve_required(&self.agent_id);
        let lua = format!(
            r#"
{resolve}

local inv = c.get_main_inventory()
local drill_count = 0
for _, item in pairs(inv.get_contents()) do
    if item.name == "{drill_type}" then drill_count = item.count end
end

if drill_count < {count} then
    rcon.print('{{"placed":0,"total":{count},"entities":[],"errors":["Not enough drills in inventory (have ' .. drill_count .. ')"]}}')
    return
end

-- Find resource tiles
local resources = game.surfaces[1].find_entities_filtered{{
    name = "{resource}",
    position = {{{near_x}, {near_y}}},
    radius = 100
}}

if #resources == 0 then
    rcon.print('{{"placed":0,"total":{count},"entities":[],"errors":["No {resource} found nearby"]}}')
    return
end

-- Sort by distance to near position
table.sort(resources, function(a, b)
    local da = (a.position.x - {near_x})^2 + (a.position.y - {near_y})^2
    local db = (b.position.x - {near_x})^2 + (b.position.y - {near_y})^2
    return da < db
end)

local placed = 0
local entities = {{}}
local errors = {{}}
local used_positions = {{}}

for _, res in pairs(resources) do
    if placed >= {count} then break end

    -- Round position to grid
    local px = math.floor(res.position.x)
    local py = math.floor(res.position.y)
    local key = px .. "," .. py

    if not used_positions[key] then
        -- Check if can place
        local can = game.surfaces[1].can_place_entity{{
            name = "{drill_type}",
            position = {{px, py}},
            direction = {direction},
            force = c.force
        }}

        if can then
            local e = game.surfaces[1].create_entity{{
                name = "{drill_type}",
                position = {{px, py}},
                direction = {direction},
                force = c.force
            }}
            if e then
                storage.factorioctl_entities = storage.factorioctl_entities or {{}}
                storage.factorioctl_entities[e.unit_number] = e
                inv.remove{{name = "{drill_type}", count = 1}}
                placed = placed + 1
                used_positions[key] = true
                table.insert(entities, {{
                    unit_number = e.unit_number,
                    name = e.name,
                    type = e.type,
                    position = {{x = e.position.x, y = e.position.y}},
                    direction = e.direction
                }})
            end
        end
    end
end

rcon.print(helpers.table_to_json({{
    placed = placed,
    total = {count},
    entities = entities,
    errors = errors
}}))
"#,
            count = count,
            resolve = resolve,
            drill_type = drill_type_lua,
            resource = resource_lua,
            near_x = near_pos.0,
            near_y = near_pos.1,
            direction = dir.to_factorio()
        );

        let response = self.execute_lua(&lua).await?;
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
        let (dx, dy) = match line_direction.to_lowercase().as_str() {
            "east" | "e" => (spacing as f64, 0.0),
            "west" | "w" => (-(spacing as f64), 0.0),
            "south" | "s" => (0.0, spacing as f64),
            "north" | "n" => (0.0, -(spacing as f64)),
            _ => (spacing as f64, 0.0),
        };
        let furnace_type_lua = LuaCommand::lua_escape(furnace_type);

        let resolve = LuaCommand::resolve_required(&self.agent_id);
        let lua = format!(
            r#"
{resolve}

local inv = c.get_main_inventory()
local furnace_count = 0
for _, item in pairs(inv.get_contents()) do
    if item.name == "{furnace_type}" then furnace_count = item.count end
end

if furnace_count < {count} then
    rcon.print('{{"placed":0,"total":{count},"entities":[],"errors":["Not enough furnaces in inventory (have ' .. furnace_count .. ')"]}}')
    return
end

local placed = 0
local entities = {{}}
local errors = {{}}

for i = 0, {count} - 1 do
    local px = {start_x} + i * {dx}
    local py = {start_y} + i * {dy}

    local can = game.surfaces[1].can_place_entity{{
        name = "{furnace_type}",
        position = {{px, py}},
        force = c.force
    }}

    if can then
        local e = game.surfaces[1].create_entity{{
            name = "{furnace_type}",
            position = {{px, py}},
            force = c.force
        }}
        if e then
            storage.factorioctl_entities = storage.factorioctl_entities or {{}}
            storage.factorioctl_entities[e.unit_number] = e
            inv.remove{{name = "{furnace_type}", count = 1}}
            placed = placed + 1
            table.insert(entities, {{
                unit_number = e.unit_number,
                name = e.name,
                type = e.type,
                position = {{x = e.position.x, y = e.position.y}}
            }})
        end
    else
        table.insert(errors, "Cannot place at " .. px .. "," .. py)
    end
end

rcon.print(helpers.table_to_json({{
    placed = placed,
    total = {count},
    entities = entities,
    errors = errors
}}))
"#,
            count = count,
            resolve = resolve,
            furnace_type = furnace_type_lua,
            start_x = start.0,
            start_y = start.1,
            dx = dx,
            dy = dy
        );

        let response = self.execute_lua(&lua).await?;
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
        let lua = format!(
            r#"local belts = game.surfaces[1].find_entities_filtered{{area = {{{{{}, {}}}, {{{}, {}}}}}, type = "transport-belt"}}
local belt_items = {{}}
local item_totals = {{}}
local total_items = 0
for _, belt in pairs(belts) do
    local belt_data = {{position = {{x = belt.position.x, y = belt.position.y}}, unit_number = belt.unit_number, items = {{}}}}
    for i = 1, belt.get_max_transport_line_index() do
        local line = belt.get_transport_line(i)
        local count = line.get_item_count()
        if count > 0 then
            for _, item in pairs(line.get_contents()) do
                table.insert(belt_data.items, {{name = item.name, count = item.count}})
                item_totals[item.name] = (item_totals[item.name] or 0) + item.count
                total_items = total_items + item.count
            end
        end
    end
    if #belt_data.items > 0 then
        table.insert(belt_items, belt_data)
    end
end
local summary = {{}}
for item_name, count in pairs(item_totals) do
    table.insert(summary, {{name = item_name, count = count}})
end
rcon.print(helpers.table_to_json({{belt_count = #belts, total_items = total_items, item_summary = summary, belts = belt_items}}))"#,
            area.left_top.x, area.left_top.y, area.right_bottom.x, area.right_bottom.y
        );

        let response = self.execute_lua(&lua).await?;
        let result: BeltContentsResult = serde_json::from_str(&response)?;
        Ok(result)
    }

    /// Get items on transport belts with lane separation
    pub async fn get_belt_lane_contents(&mut self, area: Area) -> Result<BeltLaneContentsResult> {
        let lua = LuaCommand::get_belt_lane_contents(area);
        let response = self.execute_lua(&lua).await?;

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
