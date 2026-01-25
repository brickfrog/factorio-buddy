//! Factorio client for communicating with the game server

pub mod lua;
pub mod rcon;
pub mod server;

use anyhow::Result;

use crate::world::{
    Area, BuildResult, CharacterStatus, CraftResult, Direction, Entity, GatherResult, Inventory,
    MineResult, PlacementSpec, Position, ResourcePatch, Surface, Tick, Tile, WalkResult,
};
use lua::LuaCommand;
use rcon::RconClient;

/// High-level client for interacting with Factorio
pub struct FactorioClient {
    rcon: RconClient,
}

impl FactorioClient {
    /// Connect to a Factorio server
    pub async fn connect(host: &str, port: u16, password: &str) -> Result<Self> {
        let mut rcon = RconClient::connect(host, port, password).await?;

        // Send warmup command (first command after connection may get dropped)
        let _ = rcon.execute("/c").await;

        Ok(Self { rcon })
    }

    /// Close the connection
    pub async fn close(&mut self) -> Result<()> {
        self.rcon.close().await
    }

    /// Execute a raw Lua command
    pub async fn execute_lua(&mut self, lua: &str) -> Result<String> {
        // RCON doesn't handle newlines well, convert to single line
        let single_line: String = lua
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty() && !line.starts_with("--"))
            .collect::<Vec<_>>()
            .join(" ");
        self.rcon.execute(&format!("/c {}", single_line)).await
    }

    /// Execute a silent Lua command (no console output)
    pub async fn execute_lua_silent(&mut self, lua: &str) -> Result<String> {
        // RCON doesn't handle newlines well, convert to single line
        let single_line: String = lua
            .lines()
            .map(|line| line.trim())
            .filter(|line| !line.is_empty() && !line.starts_with("--"))
            .collect::<Vec<_>>()
            .join(" ");
        self.rcon.execute(&format!("/silent-command {}", single_line)).await
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
        let surfaces: Vec<Surface> = serde_json::from_str(&response)?;
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
        let entities: Vec<Entity> = serde_json::from_str(&response)?;
        Ok(entities)
    }

    /// Get a specific entity by unit number
    pub async fn get_entity(&mut self, unit_number: u32) -> Result<Entity> {
        let lua = LuaCommand::get_entity(unit_number);
        let response = self.execute_lua(&lua).await?;
        let entity: Entity = serde_json::from_str(&response)?;
        Ok(entity)
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
        let resources: Vec<ResourcePatch> = serde_json::from_str(&response)?;
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
        let tiles: Vec<Tile> = serde_json::from_str(&response)?;
        Ok(tiles)
    }

    /// Get a specific tile
    pub async fn get_tile(&mut self, position: Position) -> Result<Tile> {
        let lua = LuaCommand::get_tile(position);
        let response = self.execute_lua(&lua).await?;
        let tile: Tile = serde_json::from_str(&response)?;
        Ok(tile)
    }

    // --- Character Control ---

    /// Initialize character entity
    pub async fn init_character(&mut self) -> Result<Entity> {
        let lua = LuaCommand::init_character();
        let response = self.execute_lua(&lua).await?;
        let entity: Entity = serde_json::from_str(&response)?;
        Ok(entity)
    }

    /// Teleport character to position
    pub async fn teleport_character(&mut self, position: Position) -> Result<()> {
        let lua = LuaCommand::teleport_character(position);
        self.execute_lua(&lua).await?;
        Ok(())
    }

    /// Start walking character to position
    pub async fn walk_character(&mut self, position: Position) -> Result<()> {
        let lua = LuaCommand::walk_character(position);
        self.execute_lua(&lua).await?;
        Ok(())
    }

    /// Get character status
    pub async fn character_status(&mut self) -> Result<CharacterStatus> {
        let lua = LuaCommand::character_status();
        let response = self.execute_lua(&lua).await?;
        let status: CharacterStatus = serde_json::from_str(&response)?;
        Ok(status)
    }

    /// Get character inventory
    pub async fn character_inventory(&mut self) -> Result<Inventory> {
        let lua = LuaCommand::character_inventory();
        let response = self.execute_lua(&lua).await?;
        let inventory: Inventory = serde_json::from_str(&response)?;
        Ok(inventory)
    }

    // --- Mining ---

    /// Mine entity at position
    pub async fn mine_at(&mut self, position: Position, count: u32) -> Result<MineResult> {
        let lua = LuaCommand::mine_at(position, count);
        let response = self.execute_lua(&lua).await?;
        let result: MineResult = serde_json::from_str(&response)?;
        Ok(result)
    }

    /// Mine nearest entity of type
    pub async fn mine_nearest(&mut self, entity_type: &str, count: u32) -> Result<MineResult> {
        let lua = LuaCommand::mine_nearest(entity_type, count);
        let response = self.execute_lua(&lua).await?;
        let result: MineResult = serde_json::from_str(&response)?;
        Ok(result)
    }

    // --- Crafting ---

    /// Start crafting a recipe
    pub async fn craft(&mut self, recipe: &str, count: u32) -> Result<CraftResult> {
        let lua = LuaCommand::craft(recipe, count);
        let response = self.execute_lua(&lua).await?;
        let result: CraftResult = serde_json::from_str(&response)?;
        Ok(result)
    }

    /// Wait for crafting to complete
    pub async fn wait_for_crafting(&mut self) -> Result<()> {
        let lua = LuaCommand::wait_for_crafting();
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
        let lua = LuaCommand::place_entity(entity_name, position, direction);
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

    /// Remove entity at position
    pub async fn remove_entity_at(&mut self, position: Position) -> Result<()> {
        let lua = LuaCommand::remove_entity_at(position);
        self.execute_lua(&lua).await?;
        Ok(())
    }

    /// Remove entity by unit number
    pub async fn remove_entity(&mut self, unit_number: u32) -> Result<()> {
        let lua = LuaCommand::remove_entity(unit_number);
        self.execute_lua(&lua).await?;
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
        self.execute_lua(&lua).await?;
        Ok(())
    }

    /// Set recipe on an assembling machine
    pub async fn set_recipe(&mut self, unit_number: u32, recipe: &str) -> Result<()> {
        let lua = LuaCommand::set_recipe(unit_number, recipe);
        self.execute_lua(&lua).await?;
        Ok(())
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

    // --- High-Level Operations ---

    /// Get character's current position (uses first connected player or spawned character)
    pub async fn get_character_position(&mut self) -> Result<Position> {
        let lua = r#"
local c = nil
for _, p in pairs(game.connected_players) do
    if p.character and p.character.valid then
        c = p.character
        break
    end
end
if not c then
    if not global then global = {} end
    c = global.factorioctl_character
end
if c and c.valid then
    rcon.print(c.position.x .. "," .. c.position.y)
else
    rcon.print("error")
end
"#;
        let response = self.execute_lua_silent(lua).await?;
        let parts: Vec<&str> = response.trim().split(',').collect();
        if parts.len() != 2 {
            anyhow::bail!("No character available");
        }
        Ok(Position {
            x: parts[0].parse()?,
            y: parts[1].parse()?,
        })
    }

    /// Smooth walk to a target position
    pub async fn walk_to(&mut self, target: Position, _run: bool) -> Result<WalkResult> {
        let mut total_distance = 0.0;
        let start_pos = self.get_character_position().await?;
        let mut last_pos = start_pos;
        let mut last_dist = start_pos.distance(&target);
        let mut stuck_count = 0;
        let mut overshoot_count = 0;

        // Helper to stop walking
        let stop_lua = "local c = nil for _, p in pairs(game.connected_players) do if p.character and p.character.valid then c = p.character break end end if not c then if not global then global = {} end c = global.factorioctl_character end if c then c.walking_state = {walking=false} end";

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
                self.execute_lua_silent(stop_lua).await?;
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
                    self.execute_lua_silent(stop_lua).await?;
                    return Ok(WalkResult {
                        arrived: dist < 5.0, // Close enough
                        final_position: pos,
                        distance_walked: total_distance,
                        reason: if dist < 5.0 { None } else { Some("Overshot target".to_string()) },
                    });
                }
            } else {
                overshoot_count = 0;
            }

            // Check if stuck
            if i > 3 && step_dist < 0.01 && dist > 3.0 {
                stuck_count += 1;
                if stuck_count >= 3 {
                    self.execute_lua_silent(stop_lua).await?;
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
                if dy < 0.0 { "north" } else { "south" }
            } else if dy.abs() < 0.5 {
                if dx > 0.0 { "east" } else { "west" }
            } else {
                let ratio = dy.abs() / dx.abs();
                if ratio < 0.414 {
                    if dx > 0.0 { "east" } else { "west" }
                } else if ratio > 2.414 {
                    if dy < 0.0 { "north" } else { "south" }
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
            let lua = format!(
                r#"local c = nil
for _, p in pairs(game.connected_players) do if p.character and p.character.valid then c = p.character break end end
if not c then if not global then global = {{}} end c = global.factorioctl_character end
if c then c.walking_state = {{walking=true, direction=defines.direction.{}}} end"#,
                dir_name
            );
            self.execute_lua_silent(&lua).await?;

            tokio::time::sleep(tokio::time::Duration::from_millis(150)).await;
        }

        // Timeout
        self.execute_lua_silent(
            "local c = nil for _, p in pairs(game.connected_players) do if p.character and p.character.valid then c = p.character break end end if not c then if not global then global = {} end c = global.factorioctl_character end if c then c.walking_state = {walking=false} end"
        ).await?;
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
        let start_pos = self.get_character_position().await?;
        let mut total_distance = 0.0;
        let mut gathered = 0u32;

        for _ in 0..amount {
            // Find nearest resource
            let find_lua = format!(
                r#"
local c = nil
for _, p in pairs(game.connected_players) do if p.character and p.character.valid then c = p.character break end end
if not c then if not global then global = {{}} end c = global.factorioctl_character end
if not (c and c.valid) then rcon.print("error") return end

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
                resource_name, radius
            );

            let response = self.execute_lua_silent(&find_lua).await?;
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
            let mine_lua = format!(
                r#"
local c = nil
for _, p in pairs(game.connected_players) do if p.character and p.character.valid then c = p.character break end end
if not c then if not global then global = {{}} end c = global.factorioctl_character end
if not (c and c.valid) then
    rcon.print("no_char")
    return
end

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
                target_pos.x, target_pos.y
            );
            let mine_result = self.execute_lua_silent(&mine_lua).await?;

            match mine_result.trim() {
                "mined" => gathered += 1,
                "no_char" | "no_resource" => break,
                _ => {}
            }
        }

        // Get final inventory
        let inv_lua = r#"
local c = nil
for _, p in pairs(game.connected_players) do if p.character and p.character.valid then c = p.character break end end
if not c then if not global then global = {} end c = global.factorioctl_character end
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
"#;
        let inv_response = self.execute_lua_silent(inv_lua).await?;
        #[derive(serde::Deserialize)]
        struct InvResult {
            items: Vec<crate::world::InventoryItem>,
        }
        let inv_result: InvResult = serde_json::from_str(&inv_response).unwrap_or(InvResult {
            items: Vec::new(),
        });

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

        let lua = format!(
            r#"
if not global then global = {{}} end
local c = global.factorioctl_character
if not (c and c.valid) then
    rcon.print('{{"placed":0,"total":{count},"entities":[],"errors":["No character"]}}')
    return
end

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
            drill_type = drill_type,
            resource = resource,
            near_x = near_pos.0,
            near_y = near_pos.1,
            direction = dir.to_factorio()
        );

        let response = self.execute_lua_silent(&lua).await?;
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

        let lua = format!(
            r#"
if not global then global = {{}} end
local c = global.factorioctl_character
if not (c and c.valid) then
    rcon.print('{{"placed":0,"total":{count},"entities":[],"errors":["No character"]}}')
    return
end

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
            furnace_type = furnace_type,
            start_x = start.0,
            start_y = start.1,
            dx = dx,
            dy = dy
        );

        let response = self.execute_lua_silent(&lua).await?;
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
}
