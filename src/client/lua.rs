//! Lua command builders for Factorio interactions
//!
//! These builders generate Lua code that can be executed via RCON.
//! All commands use rcon.print() to return JSON-formatted results.

use crate::client::AgentId;
use crate::world::{Area, Direction, Position};

/// Builder for Lua commands
pub struct LuaCommand;

impl LuaCommand {
    pub fn resolve_required(agent_id: &AgentId) -> String {
        if agent_id.is_legacy() {
            r#"storage.factorioctl_characters = storage.factorioctl_characters or {}
local c = nil
for _, p in pairs(game.connected_players) do if p.character and p.character.valid then c = p.character break end end
if not c then c = storage.factorioctl_characters["__player__"] end
if not (c and c.valid) then rcon.print('{"error":"no character for agent __player__; spawn first"}') return end"#
                .to_string()
        } else {
            format!(
                r#"storage.factorioctl_characters = storage.factorioctl_characters or {{}}
local c = storage.factorioctl_characters["{}"]
if not (c and c.valid) then rcon.print('{{"error":"no character for agent {}; spawn first"}}') return end"#,
                agent_id.as_str(),
                agent_id.as_str()
            )
        }
    }

    pub fn resolve_optional(agent_id: &AgentId) -> String {
        if agent_id.is_legacy() {
            r#"storage.factorioctl_characters = storage.factorioctl_characters or {}
local c = nil
for _, p in pairs(game.connected_players) do if p.character and p.character.valid then c = p.character break end end
if not c then c = storage.factorioctl_characters["__player__"] end"#
                .to_string()
        } else {
            format!(
                r#"storage.factorioctl_characters = storage.factorioctl_characters or {{}}
local c = storage.factorioctl_characters["{}"]"#,
                agent_id.as_str()
            )
        }
    }

    fn entity_lookup(unit_number: u32) -> String {
        format!(
            r#"storage.factorioctl_entities = storage.factorioctl_entities or {{}}
local e = storage.factorioctl_entities[{}]
if e and not e.valid then storage.factorioctl_entities[{}] = nil e = nil end
if not e then
    for _, entity in pairs(game.surfaces[1].find_entities_filtered{{area={{{{-500,-500}},{{500,500}}}}}}) do
        if entity.unit_number == {} then
            e = entity
            break
        end
    end
end"#,
            unit_number, unit_number, unit_number
        )
    }

    fn register_entity(var_name: &str) -> String {
        format!(
            "storage.factorioctl_entities = storage.factorioctl_entities or {{}}\nstorage.factorioctl_entities[{0}.unit_number] = {0}",
            var_name
        )
    }

    /// Get list of surfaces
    pub fn get_surfaces() -> String {
        r#"
local result = {}
for _, surface in pairs(game.surfaces) do
    table.insert(result, {
        name = surface.name,
        index = surface.index,
        daytime = surface.daytime,
        darkness = surface.darkness
    })
end
rcon.print(helpers.table_to_json(result))
"#
        .trim()
        .to_string()
    }

    /// Find entities in an area
    pub fn find_entities(area: Area, entity_type: Option<&str>, name: Option<&str>) -> String {
        let mut filters = vec![format!(
            "area={{{{{},{}}},{{{},{}}}}}",
            area.left_top.x, area.left_top.y, area.right_bottom.x, area.right_bottom.y
        )];

        if let Some(t) = entity_type {
            filters.push(format!("type=\"{}\"", t));
        }
        if let Some(n) = name {
            filters.push(format!("name=\"{}\"", n));
        }

        format!(
            r#"
local result = {{}}
local entities = game.surfaces[1].find_entities_filtered{{{}}}
for _, e in pairs(entities) do
    local bb = e.bounding_box
    table.insert(result, {{
        unit_number = e.unit_number,
        name = e.name,
        type = e.type,
        position = {{ x = e.position.x, y = e.position.y }},
        direction = e.direction,
        health = e.health,
        force = e.force.name,
        bounding_box = {{
            left_top = {{ x = bb.left_top.x, y = bb.left_top.y }},
            right_bottom = {{ x = bb.right_bottom.x, y = bb.right_bottom.y }}
        }}
    }})
end
rcon.print(helpers.table_to_json(result))
"#,
            filters.join(", ")
        )
        .trim()
        .to_string()
    }

    /// Get a specific entity by unit number
    pub fn get_entity(unit_number: u32) -> String {
        let lookup = Self::entity_lookup(unit_number);
        format!(
            r#"
{}
if e then
    rcon.print(helpers.table_to_json({{
        unit_number = e.unit_number,
        name = e.name,
        type = e.type,
        position = {{ x = e.position.x, y = e.position.y }},
        direction = e.direction,
        health = e.health,
        force = e.force.name
    }}))
else
    rcon.print("null")
end
"#,
            lookup
        )
        .trim()
        .to_string()
    }

    /// Get an entity's inventories
    pub fn get_entity_inventory(unit_number: u32) -> String {
        let lookup = Self::entity_lookup(unit_number);
        format!(
            r#"
{}
if not e then
    rcon.print('{{"error": "Entity not found"}}')
    return
end

local result = {{
    unit_number = e.unit_number,
    name = e.name,
    inventories = {{}}
}}

-- Try common inventory types
local inv_types = {{
    {{ name = "fuel", define = defines.inventory.fuel }},
    {{ name = "chest", define = defines.inventory.chest }},
    {{ name = "furnace_source", define = defines.inventory.furnace_source }},
    {{ name = "furnace_result", define = defines.inventory.furnace_result }},
    {{ name = "assembling_machine_input", define = defines.inventory.assembling_machine_input }},
    {{ name = "assembling_machine_output", define = defines.inventory.assembling_machine_output }},
    {{ name = "burnt_result", define = defines.inventory.burnt_result }},
}}

for _, inv_type in pairs(inv_types) do
    local inv = e.get_inventory(inv_type.define)
    if inv then
        local contents = inv.get_contents()
        local items = {{}}
        for item, count in pairs(contents) do
            table.insert(items, {{ name = item, count = count }})
        end
        if #items > 0 then
            result.inventories[inv_type.name] = items
        end
    end
end

rcon.print(helpers.table_to_json(result))
"#,
            lookup
        )
        .trim()
        .to_string()
    }

    /// Find resources in an area and aggregate by type
    pub fn find_resources(area: Area, resource_type: Option<&str>) -> String {
        let name_filter = resource_type
            .map(|t| format!(", name=\"{}\"", t))
            .unwrap_or_default();

        format!(
            r#"
local patches = {{}}
local resources = game.surfaces[1].find_entities_filtered{{
    type="resource",
    area={{{{{},{}}},{{{},{}}}}}{}
}}

-- Group by resource name and aggregate
local by_name = {{}}
for _, r in pairs(resources) do
    local key = r.name
    if not by_name[key] then
        by_name[key] = {{
            name = r.name,
            total_amount = 0,
            tile_count = 0,
            min_x = r.position.x,
            max_x = r.position.x,
            min_y = r.position.y,
            max_y = r.position.y,
            positions = {{}}
        }}
    end
    local patch = by_name[key]
    patch.total_amount = patch.total_amount + (r.amount or 0)
    patch.tile_count = patch.tile_count + 1
    patch.min_x = math.min(patch.min_x, r.position.x)
    patch.max_x = math.max(patch.max_x, r.position.x)
    patch.min_y = math.min(patch.min_y, r.position.y)
    patch.max_y = math.max(patch.max_y, r.position.y)
end

local result = {{}}
for _, patch in pairs(by_name) do
    table.insert(result, {{
        name = patch.name,
        total_amount = patch.total_amount,
        tile_count = patch.tile_count,
        center = {{
            x = (patch.min_x + patch.max_x) / 2,
            y = (patch.min_y + patch.max_y) / 2
        }},
        bounding_box = {{
            left_top = {{ x = patch.min_x, y = patch.min_y }},
            right_bottom = {{ x = patch.max_x, y = patch.max_y }}
        }}
    }})
end
rcon.print(helpers.table_to_json(result))
"#,
            area.left_top.x, area.left_top.y, area.right_bottom.x, area.right_bottom.y, name_filter
        )
        .trim()
        .to_string()
    }

    /// Find nearest resource from a position
    pub fn find_nearest_resource(resource_name: &str, from: Position) -> String {
        format!(
            r#"
local nearest = nil
local nearest_dist = math.huge
local resources = game.surfaces[1].find_entities_filtered{{
    type="resource",
    name="{}",
    position={{ {}, {} }},
    radius=200
}}

for _, r in pairs(resources) do
    local dx = r.position.x - {}
    local dy = r.position.y - {}
    local dist = dx*dx + dy*dy
    if dist < nearest_dist then
        nearest = r
        nearest_dist = dist
    end
end

if nearest then
    -- Now find the full patch around this resource
    local patch_resources = game.surfaces[1].find_entities_filtered{{
        type="resource",
        name="{}",
        position=nearest.position,
        radius=50
    }}

    local total = 0
    local min_x, max_x = nearest.position.x, nearest.position.x
    local min_y, max_y = nearest.position.y, nearest.position.y

    for _, r in pairs(patch_resources) do
        total = total + (r.amount or 0)
        min_x = math.min(min_x, r.position.x)
        max_x = math.max(max_x, r.position.x)
        min_y = math.min(min_y, r.position.y)
        max_y = math.max(max_y, r.position.y)
    end

    rcon.print(helpers.table_to_json({{
        name = nearest.name,
        total_amount = total,
        tile_count = #patch_resources,
        center = {{
            x = (min_x + max_x) / 2,
            y = (min_y + max_y) / 2
        }},
        bounding_box = {{
            left_top = {{ x = min_x, y = min_y }},
            right_bottom = {{ x = max_x, y = max_y }}
        }}
    }}))
else
    rcon.print("null")
end
"#,
            resource_name, from.x, from.y, from.x, from.y, resource_name
        )
        .trim()
        .to_string()
    }

    /// Get tiles in an area
    pub fn get_tiles(area: Area) -> String {
        format!(
            r#"
local result = {{}}
for x = {}, {} do
    for y = {}, {} do
        local tile = game.surfaces[1].get_tile(x, y)
        table.insert(result, {{
            name = tile.name,
            position = {{ x = x, y = y }},
            collides_with_player = tile.collides_with("player")
        }})
    end
end
rcon.print(helpers.table_to_json(result))
"#,
            area.left_top.x as i32,
            area.right_bottom.x as i32,
            area.left_top.y as i32,
            area.right_bottom.y as i32
        )
        .trim()
        .to_string()
    }

    /// Get a specific tile
    pub fn get_tile(position: Position) -> String {
        format!(
            r#"
local tile = game.surfaces[1].get_tile({}, {})
rcon.print(helpers.table_to_json({{
    name = tile.name,
    position = {{ x = {}, y = {} }},
    collides_with_player = tile.collides_with("player")
}}))
"#,
            position.x as i32, position.y as i32, position.x as i32, position.y as i32
        )
        .trim()
        .to_string()
    }

    /// Initialize character entity
    pub fn init_character(agent_id: &AgentId, x: f64, y: f64) -> String {
        let key = if agent_id.is_legacy() {
            "__player__"
        } else {
            agent_id.as_str()
        };
        format!(
            r#"
storage.factorioctl_characters = storage.factorioctl_characters or {{}}
storage.factorioctl_entities = storage.factorioctl_entities or {{}}
if storage.factorioctl_characters["{key}"] and storage.factorioctl_characters["{key}"].valid then
    local c = storage.factorioctl_characters["{key}"]
    storage.factorioctl_entities[c.unit_number] = c
    rcon.print(helpers.table_to_json({{
        unit_number = c.unit_number,
        name = c.name,
        type = c.type,
        position = {{ x = c.position.x, y = c.position.y }},
        direction = c.direction,
        health = c.health,
        force = c.force.name
    }}))
else
    local c = game.surfaces[1].create_entity{{
        name = "character",
        position = {{{x}, {y}}},
        force = game.forces.player
    }}
    if c then
        storage.factorioctl_characters["{key}"] = c
        storage.factorioctl_entities[c.unit_number] = c
        rcon.print(helpers.table_to_json({{
            unit_number = c.unit_number,
            name = c.name,
            type = c.type,
            position = {{ x = c.position.x, y = c.position.y }},
            direction = c.direction,
            health = c.health,
            force = c.force.name
        }}))
    else
        rcon.print('{{"error": "Failed to create character"}}')
    end
end
"#,
            key = key,
            x = x,
            y = y
        )
        .trim()
        .to_string()
    }

    /// Teleport character to position
    pub fn teleport_character(agent_id: &AgentId, position: Position) -> String {
        let resolve = Self::resolve_required(agent_id);
        format!(
            r#"
{}
c.teleport({{ {}, {} }})
rcon.print("ok")
"#,
            resolve, position.x, position.y
        )
        .trim()
        .to_string()
    }

    /// On_tick step-walk driver for named (orphan) agent characters.
    ///
    /// An orphan character (no controlling player) does NOT move from
    /// `walking_state` alone, since the engine only walks characters driven by a
    /// player input controller. This injected on_tick handler steps each agent's
    /// character toward its stored target at running speed (collision-aware,
    /// sliding on one axis if blocked), giving real over-time traversal rather
    /// than an instant teleport. Re-registering replaces the prior handler, so it
    /// is idempotent. (Server-side only: with a connected human client this can
    /// desync; deterministic MP movement is the bridge-mod follow-up.)
    pub fn walk_driver_lua() -> &'static str {
        r#"
script.on_event(defines.events.on_tick, function()
local targets = storage.factorioctl_walk_targets
if not targets then return end
for id, tgt in pairs(targets) do
if tgt.expires_tick and game.tick >= tgt.expires_tick then
targets[id] = nil
goto continue
end
local c = storage.factorioctl_characters and storage.factorioctl_characters[id]
if c and c.valid then
local dx = tgt.x - c.position.x
local dy = tgt.y - c.position.y
local dist = math.sqrt(dx * dx + dy * dy)
local sp = c.character_running_speed or 0.15
if dist <= sp then
c.teleport({tgt.x, tgt.y})
targets[id] = nil
c.walking_state = {walking = false}
else
local nx = c.position.x + (dx / dist) * sp
local ny = c.position.y + (dy / dist) * sp
if not c.teleport({nx, ny}) then
if not c.teleport({nx, c.position.y}) then
c.teleport({c.position.x, ny})
end
end
local moved = math.sqrt((c.position.x - (tgt.last_x or c.position.x)) * (c.position.x - (tgt.last_x or c.position.x)) + (c.position.y - (tgt.last_y or c.position.y)) * (c.position.y - (tgt.last_y or c.position.y)))
if moved < 0.001 then
tgt.stuck_ticks = (tgt.stuck_ticks or 0) + 1
else
tgt.stuck_ticks = 0
end
tgt.last_x = c.position.x
tgt.last_y = c.position.y
if tgt.stuck_ticks >= 120 then
targets[id] = nil
end
end
else
targets[id] = nil
end
::continue::
end
end)
"#
    }

    pub fn set_walk_target(agent_id: &AgentId, position: Position) -> String {
        let resolve = Self::resolve_required(agent_id);
        format!(
            r#"
{}
storage.factorioctl_walk_targets = storage.factorioctl_walk_targets or {{}}
storage.factorioctl_walk_targets["{}"] = {{ x = {}, y = {}, stuck_ticks = 0, expires_tick = game.tick + 7200, last_x = c.position.x, last_y = c.position.y }}
{}
"#,
            resolve,
            agent_id.as_str(),
            position.x,
            position.y,
            Self::walk_driver_lua(),
        )
        .trim()
        .to_string()
    }

    pub fn clear_walk_target(agent_id: &AgentId) -> String {
        format!(
            r#"
storage.factorioctl_walk_targets = storage.factorioctl_walk_targets or {{}}
storage.factorioctl_walk_targets["{}"] = nil
"#,
            agent_id.as_str()
        )
        .trim()
        .to_string()
    }

    /// Start walking character to position.
    ///
    /// Legacy/player ids use `walking_state` (the player controller moves them).
    /// Named ids store a target and ensure the on_tick step-driver is running.
    pub fn walk_character(agent_id: &AgentId, position: Position) -> String {
        let resolve = Self::resolve_required(agent_id);
        if agent_id.is_legacy() {
            format!(
                r#"
{}
local dx = {} - c.position.x
local dy = {} - c.position.y
local dir = 0
if math.abs(dx) > math.abs(dy) then
    dir = dx > 0 and defines.direction.east or defines.direction.west
else
    dir = dy > 0 and defines.direction.south or defines.direction.north
end
c.walking_state = {{ walking = true, direction = dir }}
rcon.print("ok")
"#,
                resolve, position.x, position.y
            )
            .trim()
            .to_string()
        } else {
            format!(
                r#"
{}
rcon.print("ok")
"#,
                Self::set_walk_target(agent_id, position),
            )
            .trim()
            .to_string()
        }
    }

    /// Get character status
    pub fn character_status(agent_id: &AgentId) -> String {
        format!(
            "{}\n{}",
            Self::resolve_optional(agent_id),
            r#"if c and c.valid then
    rcon.print(helpers.table_to_json({
        valid = true,
        unit_number = c.unit_number,
        position = { x = c.position.x, y = c.position.y },
        health = c.health,
        crafting_queue_size = c.crafting_queue_size,
        walking = c.walking_state.walking,
        mining = c.mining_state.mining
    }))
else
    rcon.print('{"valid": false}')
end"#
        )
        .trim()
        .to_string()
    }

    /// Get character inventory
    pub fn character_inventory(agent_id: &AgentId) -> String {
        format!(
            "{}\n{}",
            Self::resolve_optional(agent_id),
            r#"if c and c.valid then
    local inv = c.get_main_inventory()
    local items = {}
    local free_slots = 0
    if inv then
        for _, item in pairs(inv.get_contents()) do
            table.insert(items, { name = item.name, count = item.count })
        end
        free_slots = inv.count_empty_stacks() or 0
    end
    if #items == 0 then
        rcon.print('{"items": [], "free_slots": ' .. tostring(free_slots) .. '}')
    else
        rcon.print(helpers.table_to_json({ items = items, free_slots = free_slots }))
    end
else
    rcon.print('{"items": [], "free_slots": 0}')
end"#
        )
        .trim()
        .to_string()
    }

    /// Start mining at a position (uses mining_state for animations)
    pub fn start_mining(agent_id: &AgentId, position: Position) -> String {
        let resolve = Self::resolve_required(agent_id);
        format!(
            r#"
{}

-- Find a minable entity at the position
local target = nil
local resources = game.surfaces[1].find_entities_filtered{{
    position = {{ {}, {} }},
    radius = 1,
    type = "resource"
}}
if #resources > 0 then
    target = resources[1]
else
    local entities = game.surfaces[1].find_entities_filtered{{
        position = {{ {}, {} }},
        radius = 1
    }}
    for _, e in pairs(entities) do
        if e.minable and e ~= c then
            target = e
            break
        end
    end
end

if not target then
    rcon.print('{{"success": false, "error": "No minable entity at position"}}')
    return
end

-- Check if in range
local dx = target.position.x - c.position.x
local dy = target.position.y - c.position.y
local dist = math.sqrt(dx*dx + dy*dy)
if dist > c.resource_reach_distance + 0.5 then
    rcon.print('{{"success": false, "error": "Too far", "distance": ' .. dist .. ', "reach": ' .. c.resource_reach_distance .. '}}')
    return
end

-- Start mining
c.mining_state = {{ mining = true, position = target.position }}
rcon.print('{{"success": true, "target": "' .. target.name .. '", "position": {{\"x\": ' .. target.position.x .. ', \"y\": ' .. target.position.y .. '}}}}')
"#,
            resolve, position.x, position.y, position.x, position.y
        )
        .trim()
        .to_string()
    }

    /// Stop mining
    pub fn stop_mining(agent_id: &AgentId) -> String {
        format!(
            "{}\n{}",
            Self::resolve_optional(agent_id),
            r#"if c and c.valid then
c.mining_state = { mining = false }
rcon.print("ok")
else
rcon.print("error")
end"#
        )
        .trim()
        .to_string()
    }

    /// Get mining status
    pub fn get_mining_status(agent_id: &AgentId) -> String {
        format!(
            "{}\n{}",
            Self::resolve_optional(agent_id),
            r#"if c and c.valid then
    rcon.print(helpers.table_to_json({
        mining = c.mining_state.mining,
        position = { x = c.position.x, y = c.position.y }
    }))
else
    rcon.print('{"mining": false}')
end"#
        )
        .trim()
        .to_string()
    }

    /// Mine entity at position (instant - for compatibility)
    pub fn mine_at(agent_id: &AgentId, position: Position, count: u32) -> String {
        let resolve = Self::resolve_required(agent_id);
        format!(
            r#"
{}
 
-- Count inventory before mining
local inv = c.get_main_inventory()
local before_count = 0
if inv then
    for _, item in pairs(inv.get_contents()) do
        before_count = before_count + item.count
    end
end

local mined = 0
local picked_up = 0

for i = 1, {} do
    -- First check for items on ground (item-entity type)
    local items_on_ground = game.surfaces[1].find_entities_filtered{{
        position = {{ {}, {} }},
        radius = 2,
        type = "item-entity"
    }}

    if #items_on_ground > 0 then
        local item_entity = items_on_ground[1]
        local stack = item_entity.stack
        if stack and stack.valid_for_read then
            local inserted = inv.insert(stack)
            if inserted > 0 then
                picked_up = picked_up + inserted
                item_entity.destroy()
            end
        end
    else
        -- Try to find resources (iron-ore, coal, etc.)
        local resources = game.surfaces[1].find_entities_filtered{{
            position = {{ {}, {} }},
            radius = 2,
            type = "resource"
        }}

        local target = nil
        if #resources > 0 then
            target = resources[1]
        else
            -- Fall back to other minable entities (trees, rocks, buildings)
            local entities = game.surfaces[1].find_entities_filtered{{
                position = {{ {}, {} }},
                radius = 2
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

-- Count inventory after mining
local after_count = 0
local items = {{}}
if inv then
    for _, item in pairs(inv.get_contents()) do
        after_count = after_count + item.count
        table.insert(items, {{ name = item.name, count = item.count }})
    end
end

local items_gained = after_count - before_count

rcon.print(helpers.table_to_json({{
    success = items_gained > 0 or picked_up > 0,
    mined_count = items_gained,
    picked_up = picked_up,
    inventory = items
}}))
"#,
            resolve, count, position.x, position.y, position.x, position.y, position.x, position.y
        )
        .trim()
        .to_string()
    }

    /// Mine nearest entity of type
    pub fn mine_nearest(agent_id: &AgentId, entity_type: &str, count: u32) -> String {
        let resolve = Self::resolve_required(agent_id);
        format!(
            r#"
{}

local mined = 0

for i = 1, {} do
    local entities = game.surfaces[1].find_entities_filtered{{
        name = "{}",
        position = c.position,
        radius = 100
    }}

    -- Find nearest
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
        local success = c.mine_entity(nearest, true)
        if success then
            mined = mined + 1
        end
    else
        break
    end
end

-- Get inventory
local inv = c.get_main_inventory()
local items = {{}}
if inv then
    for _, item in pairs(inv.get_contents()) do
        table.insert(items, {{ name = item.name, count = item.count }})
    end
end

rcon.print(helpers.table_to_json({{
    success = mined > 0,
    mined_count = mined,
    inventory = items
}}))
"#,
            resolve, count, entity_type
        )
        .trim()
        .to_string()
    }

    /// Start crafting a recipe
    pub fn craft(agent_id: &AgentId, recipe: &str, count: u32) -> String {
        let resolve = Self::resolve_required(agent_id);
        format!(
            r#"
{}

local crafted = c.begin_crafting{{ recipe = "{}", count = {} }}

-- Build queue info
local queue = {{}}
for i, item in pairs(c.crafting_queue) do
    table.insert(queue, {{ recipe = item.recipe, count = item.count }})
end

rcon.print(helpers.table_to_json({{
    success = crafted > 0,
    queued = crafted,
    queue_size = c.crafting_queue_size,
    queue = queue
}}))
"#,
            resolve, recipe, count
        )
        .trim()
        .to_string()
    }

    /// Wait for crafting to complete (poll-based, handled in client)
    pub fn wait_for_crafting(agent_id: &AgentId) -> String {
        format!(
            "{}\n{}",
            Self::resolve_optional(agent_id),
            r#"if c and c.valid then
    rcon.print(tostring(c.crafting_queue_size))
else
    rcon.print("0")
end"#
        )
        .trim()
        .to_string()
    }

    /// Place an entity from inventory
    pub fn place_entity(
        agent_id: &AgentId,
        entity_name: &str,
        position: Position,
        direction: Direction,
    ) -> String {
        let resolve = Self::resolve_required(agent_id);
        let register = Self::register_entity("e");
        format!(
            r#"
{}

local inv = c.get_main_inventory()
if not inv or inv.get_item_count("{}") < 1 then
    rcon.print('{{"error": "Item not in inventory"}}')
    return
end

-- Clear items on ground in placement area (they would be picked up during normal placement)
local proto = prototypes.entity["{}"]
if proto and proto.collision_box then
    local cb = proto.collision_box
    local clear_area = {{
        {{ {} + cb.left_top.x - 0.1, {} + cb.left_top.y - 0.1 }},
        {{ {} + cb.right_bottom.x + 0.1, {} + cb.right_bottom.y + 0.1 }}
    }}
    local items_on_ground = game.surfaces[1].find_entities_filtered{{
        area = clear_area,
        type = "item-entity"
    }}
    for _, item in pairs(items_on_ground) do
        -- Try to pick up the item, or just destroy it if inventory is full
        local stack = item.stack
        if stack and stack.valid_for_read then
            local inserted = c.insert(stack)
            if inserted > 0 then
                if inserted >= stack.count then
                    item.destroy()
                else
                    stack.count = stack.count - inserted
                end
            else
                item.destroy()
            end
        else
            item.destroy()
        end
    end
end

-- Check if can place (use manual build check for proper collision detection)
local can_place = game.surfaces[1].can_place_entity{{
    name = "{}",
    position = {{ {}, {} }},
    direction = {},
    force = c.force,
    build_check_type = defines.build_check_type.manual
}}

if not can_place then
    rcon.print('{{"error": "Cannot place entity here"}}')
    return
end

-- Note: can_place_entity with build_check_type.manual handles collision detection properly
-- using actual collision masks, not just bounding box overlap

-- Create the entity
local e = game.surfaces[1].create_entity{{
    name = "{}",
    position = {{ {}, {} }},
    direction = {},
    force = c.force
}}

if e then
    {}
    inv.remove{{ name = "{}", count = 1 }}
    rcon.print(helpers.table_to_json({{
        unit_number = e.unit_number,
        name = e.name,
        entity_type = e.type,
        position = {{ x = e.position.x, y = e.position.y }},
        direction = e.direction,
        health = e.health,
        force = e.force.name
    }}))
else
    rcon.print('{{"error": "Failed to create entity"}}')
end
"#,
            resolve,
            entity_name,
            entity_name,
            position.x,
            position.y,
            position.x,
            position.y,
            entity_name,
            position.x,
            position.y,
            direction.to_factorio(),
            entity_name,
            position.x,
            position.y,
            direction.to_factorio(),
            register,
            entity_name
        )
        .trim()
        .to_string()
    }

    /// Place an underground belt with specified type (input or output)
    pub fn place_underground_belt(
        agent_id: &AgentId,
        entity_name: &str,
        position: Position,
        direction: Direction,
        belt_type: &str,
    ) -> String {
        let resolve = Self::resolve_required(agent_id);
        let register = Self::register_entity("e");
        format!(
            r#"
{}

local inv = c.get_main_inventory()
if not inv or inv.get_item_count("{}") < 1 then
    rcon.print('{{"error": "Item not in inventory"}}')
    return
end

-- Check if can place
local can_place = game.surfaces[1].can_place_entity{{
    name = "{}",
    position = {{ {}, {} }},
    direction = {},
    force = c.force,
    build_check_type = defines.build_check_type.manual
}}

if not can_place then
    rcon.print('{{"error": "Cannot place underground belt here"}}')
    return
end

-- Create the underground belt with type (input = entry, output = exit)
local e = game.surfaces[1].create_entity{{
    name = "{}",
    position = {{ {}, {} }},
    direction = {},
    type = "{}",
    force = c.force
}}

if e then
    {}
    inv.remove{{ name = "{}", count = 1 }}
    rcon.print(helpers.table_to_json({{
        unit_number = e.unit_number,
        name = e.name,
        entity_type = e.type,
        position = {{ x = e.position.x, y = e.position.y }},
        direction = e.direction,
        belt_to_ground_type = e.belt_to_ground_type,
        force = e.force.name
    }}))
else
    rcon.print('{{"error": "Failed to create underground belt"}}')
end
"#,
            resolve,
            entity_name,
            entity_name,
            position.x,
            position.y,
            direction.to_factorio(),
            entity_name,
            position.x,
            position.y,
            direction.to_factorio(),
            belt_type,
            register,
            entity_name
        )
        .trim()
        .to_string()
    }

    /// Place a ghost entity (for planning)
    pub fn place_ghost(
        agent_id: &AgentId,
        entity_name: &str,
        position: Position,
        direction: Direction,
    ) -> String {
        let resolve = Self::resolve_required(agent_id);
        let register = Self::register_entity("e");
        format!(
            r#"
{}

-- Create ghost entity (doesn't require items in inventory)
local e = game.surfaces[1].create_entity{{
    name = "entity-ghost",
    inner_name = "{}",
    position = {{ {}, {} }},
    direction = {},
    force = c.force
}}

if e then
    {}
    rcon.print(helpers.table_to_json({{
        unit_number = e.unit_number,
        name = e.ghost_name or "{}",
        entity_type = "entity-ghost",
        position = {{ x = e.position.x, y = e.position.y }},
        direction = e.direction,
        health = e.health,
        force = e.force.name
    }}))
else
    rcon.print('{{"error": "Failed to create ghost"}}')
end
"#,
            resolve,
            entity_name,
            position.x,
            position.y,
            direction.to_factorio(),
            register,
            entity_name
        )
        .trim()
        .to_string()
    }

    /// Remove entity at position
    pub fn remove_entity_at(position: Position) -> String {
        format!(
            r#"
storage.factorioctl_entities = storage.factorioctl_entities or {{}}
local entities = game.surfaces[1].find_entities_filtered{{
    position = {{ {}, {} }},
    radius = 0.5
}}

for _, e in pairs(entities) do
    if e.type ~= "character" then
        if e.unit_number then storage.factorioctl_entities[e.unit_number] = nil end
        e.destroy()
        rcon.print("ok")
        return
    end
end
rcon.print('{{"error": "No entity found"}}')
"#,
            position.x, position.y
        )
        .trim()
        .to_string()
    }

    /// Remove entity by unit number
    pub fn remove_entity(unit_number: u32) -> String {
        let lookup = Self::entity_lookup(unit_number);
        format!(
            r#"
{}
if e then
    storage.factorioctl_entities[{}] = nil
    e.destroy()
    rcon.print("ok")
else
    rcon.print('{{"error": "Entity not found"}}')
end
"#,
            lookup, unit_number
        )
        .trim()
        .to_string()
    }

    /// Rotate an entity to a new direction
    pub fn rotate_entity(unit_number: u32, direction: u8) -> String {
        let lookup = Self::entity_lookup(unit_number);
        format!(
            r#"
{}
if e then
    if e.supports_direction then
        e.direction = {}
        rcon.print("ok")
    else
        rcon.print('{{"error": "Entity does not support rotation"}}')
    end
else
    rcon.print('{{"error": "Entity not found"}}')
end
"#,
            lookup, direction
        )
        .trim()
        .to_string()
    }

    /// Insert items into an entity
    pub fn insert_items(unit_number: u32, item: &str, count: u32, inventory_type: &str) -> String {
        let inv_define = match inventory_type {
            "fuel" => "defines.inventory.fuel",
            "input" => "defines.inventory.assembling_machine_input",
            "output" => "defines.inventory.assembling_machine_output",
            "chest" => "defines.inventory.chest",
            "furnace_source" => "defines.inventory.furnace_source",
            "furnace_result" => "defines.inventory.furnace_result",
            "lab_input" => "defines.inventory.lab_input",
            "lab_modules" => "defines.inventory.lab_modules",
            _ => "defines.inventory.fuel",
        };

        let lookup = Self::entity_lookup(unit_number);
        format!(
            r#"
{}
if not e then
    rcon.print('{{"error": "Entity not found"}}')
    return
end

local inv = e.get_inventory({})
if not inv then
    rcon.print('{{"error": "Entity has no such inventory"}}')
    return
end

local inserted = inv.insert{{ name = "{}", count = {} }}
rcon.print(helpers.table_to_json({{ inserted = inserted }}))
"#,
            lookup, inv_define, item, count
        )
        .trim()
        .to_string()
    }

    /// Extract items from an entity's inventory into the player's inventory
    pub fn extract_items(
        agent_id: &AgentId,
        unit_number: u32,
        item: &str,
        count: u32,
        inventory_type: &str,
    ) -> String {
        let inv_define = match inventory_type {
            "fuel" => "defines.inventory.fuel",
            "input" => "defines.inventory.assembling_machine_input",
            "output" => "defines.inventory.assembling_machine_output",
            "chest" => "defines.inventory.chest",
            "furnace_source" => "defines.inventory.furnace_source",
            "furnace_result" => "defines.inventory.furnace_result",
            "lab_input" => "defines.inventory.lab_input",
            "lab_modules" => "defines.inventory.lab_modules",
            _ => "defines.inventory.chest",
        };

        let resolve = Self::resolve_required(agent_id);
        let lookup = Self::entity_lookup(unit_number);
        format!(
            r#"
{}
{}
if not e then
    rcon.print('{{"error": "Entity not found"}}')
    return
end

local inv = e.get_inventory({})
if not inv then
    rcon.print('{{"error": "Entity has no such inventory"}}')
    return
end

local player_inv = c.get_main_inventory()
if not player_inv then
    rcon.print('{{"error": "Character has no inventory"}}')
    return
end

-- Check how many items are available
local available = inv.get_item_count("{}")
local to_extract = math.min({}, available)

if to_extract == 0 then
    rcon.print('{{"extracted": 0, "error": "No items of that type in inventory"}}')
    return
end

-- Remove from entity inventory
local removed = inv.remove{{ name = "{}", count = to_extract }}

-- Insert into player inventory
local inserted = player_inv.insert{{ name = "{}", count = removed }}

-- If we couldn't insert all, put the remainder back
if inserted < removed then
    inv.insert{{ name = "{}", count = removed - inserted }}
end

rcon.print(helpers.table_to_json({{ extracted = inserted, available = available }}))
"#,
            resolve, lookup, inv_define, item, count, item, item, item
        )
        .trim()
        .to_string()
    }

    /// Set recipe on an assembling machine
    pub fn set_recipe(unit_number: u32, recipe: &str) -> String {
        let lookup = Self::entity_lookup(unit_number);
        format!(
            r#"
{}
if not e then
    rcon.print('{{"error": "Entity not found"}}')
    return
end

if not e.set_recipe then
    rcon.print('{{"error": "Entity cannot have recipes"}}')
    return
end

local result = e.set_recipe("{}")
rcon.print(helpers.table_to_json({{ success = result ~= nil }}))
"#,
            lookup, recipe
        )
        .trim()
        .to_string()
    }

    // --- Prototype Queries ---

    /// Get a recipe by name
    pub fn get_recipe(name: &str) -> String {
        format!(
            r#"
local recipe = prototypes.recipe["{}"]
if recipe then
    local ingredients = {{}}
    for _, ing in pairs(recipe.ingredients) do
        table.insert(ingredients, {{
            type = ing.type,
            name = ing.name,
            amount = ing.amount
        }})
    end
    local products = {{}}
    for _, prod in pairs(recipe.products) do
        table.insert(products, {{
            type = prod.type,
            name = prod.name,
            amount = prod.amount,
            probability = prod.probability
        }})
    end
    rcon.print(helpers.table_to_json({{
        name = recipe.name,
        category = recipe.category,
        energy = recipe.energy,
        ingredients = ingredients,
        products = products
    }}))
else
    rcon.print('{{"error": "Recipe not found"}}')
end
"#,
            name
        )
        .trim()
        .to_string()
    }

    /// Get all recipes in a category
    pub fn get_recipes_by_category(category: &str) -> String {
        format!(
            r#"
local recipes = {{}}
for name, recipe in pairs(prototypes.recipe) do
    if recipe.category == "{}" then
        table.insert(recipes, {{
            name = recipe.name,
            category = recipe.category,
            energy = recipe.energy
        }})
    end
end
rcon.print(helpers.table_to_json(recipes))
"#,
            category
        )
        .trim()
        .to_string()
    }

    /// Get all recipes that produce a specific item
    pub fn get_recipes_for_item(item: &str) -> String {
        format!(
            r#"
local recipes = {{}}
for name, recipe in pairs(prototypes.recipe) do
    for _, product in pairs(recipe.products) do
        if product.name == "{}" then
            local ingredients = {{}}
            for _, ing in pairs(recipe.ingredients) do
                table.insert(ingredients, {{
                    type = ing.type,
                    name = ing.name,
                    amount = ing.amount
                }})
            end
            local products = {{}}
            for _, prod in pairs(recipe.products) do
                table.insert(products, {{
                    type = prod.type,
                    name = prod.name,
                    amount = prod.amount,
                    probability = prod.probability
                }})
            end
            table.insert(recipes, {{
                name = recipe.name,
                category = recipe.category,
                energy = recipe.energy,
                ingredients = ingredients,
                products = products
            }})
            break
        end
    end
end
rcon.print(helpers.table_to_json(recipes))
"#,
            item
        )
        .trim()
        .to_string()
    }

    /// Get an entity prototype by name
    pub fn get_prototype(name: &str) -> String {
        format!(
            r#"
local proto = prototypes.entity["{}"]
if proto then
    local result = {{
        name = proto.name,
        type = proto.type,
    }}

    -- Helper to safely get property
    local function try_get(fn)
        local ok, val = pcall(fn)
        if ok then return val end
        return nil
    end

    -- Calculate size from collision box
    local cb = try_get(function() return proto.collision_box end)
    if cb then
        result.size = {{
            cb.right_bottom.x - cb.left_top.x,
            cb.right_bottom.y - cb.left_top.y
        }}
    end

    -- Crafting machine properties (use method for speed)
    local craft_speed = try_get(function() return proto.get_crafting_speed() end)
    if craft_speed then
        result.crafting_speed = craft_speed
    end
    local craft_cats = try_get(function() return proto.crafting_categories end)
    if craft_cats then
        result.crafting_categories = {{}}
        for cat, _ in pairs(craft_cats) do
            table.insert(result.crafting_categories, cat)
        end
    end

    -- Mining drill properties
    local mining_speed = try_get(function() return proto.mining_speed end)
    if mining_speed then
        result.mining_speed = mining_speed
    end
    local res_cats = try_get(function() return proto.resource_categories end)
    if res_cats then
        result.resource_categories = {{}}
        for cat, _ in pairs(res_cats) do
            table.insert(result.resource_categories, cat)
        end
    end

    -- Inserter properties
    local rot_speed = try_get(function() return proto.inserter_rotation_speed end)
    if rot_speed then
        result.rotation_speed = rot_speed
    end
    local ext_speed = try_get(function() return proto.inserter_extension_speed end)
    if ext_speed then
        result.extension_speed = ext_speed
    end

    -- Belt properties
    local belt_speed = try_get(function() return proto.belt_speed end)
    if belt_speed then
        result.belt_speed = belt_speed
    end

    -- Energy
    local energy = try_get(function() return proto.energy_usage end)
    if energy then
        result.energy_usage = energy
    end

    -- Energy source
    if try_get(function() return proto.burner_prototype end) then
        result.energy_source = "burner"
    elseif try_get(function() return proto.electric_energy_source_prototype end) then
        result.energy_source = "electric"
    elseif try_get(function() return proto.heat_energy_source_prototype end) then
        result.energy_source = "heat"
    elseif try_get(function() return proto.void_energy_source_prototype end) then
        result.energy_source = "void"
    end

    rcon.print(helpers.table_to_json(result))
else
    rcon.print('{{"error": "Prototype not found"}}')
end
"#,
            name
        )
        .trim()
        .to_string()
    }

    // --- Native Blueprint Commands ---

    /// Create a native Factorio blueprint string from entities in an area
    pub fn create_native_blueprint(area: Area) -> String {
        format!(
            r#"
local surface = game.surfaces[1]
local player = game.get_player(1)
if not player then
    rcon.print('{{"error": "No player"}}')
    return
end

local inv = player.get_main_inventory()
if not inv then
    rcon.print('{{"error": "No inventory"}}')
    return
end

local slot = inv[1]
local saved_item = slot.valid_for_read and slot.name or nil
slot.set_stack{{name = "blueprint"}}

local entities = slot.create_blueprint{{
    surface = surface,
    force = "player",
    area = {{{{{}, {}}}, {{{}, {}}}}},
    include_entities = true,
    include_tiles = false
}}
local count = #entities

if count == 0 then
    slot.clear()
    if saved_item then slot.set_stack{{name = saved_item}} end
    rcon.print('{{"error": "No entities in area"}}')
else
    local bp_string = slot.export_stack()
    slot.clear()
    if saved_item then slot.set_stack{{name = saved_item}} end
    rcon.print(helpers.table_to_json({{
        blueprint_string = bp_string,
        entity_count = count
    }}))
end
"#,
            area.left_top.x, area.left_top.y, area.right_bottom.x, area.right_bottom.y
        )
        .trim()
        .to_string()
    }

    /// Save a blueprint to storage with a name
    pub fn save_blueprint(name: &str, area: Area) -> String {
        format!(
            r#"
local surface = game.surfaces[1]
local player = game.get_player(1)
if not player then
    rcon.print('{{"success": false, "error": "No player"}}')
    return
end

local inv = player.get_main_inventory()
if not inv then
    rcon.print('{{"success": false, "error": "No inventory"}}')
    return
end

local slot = inv[1]
local saved_item = slot.valid_for_read and slot.name or nil
slot.set_stack{{name = "blueprint"}}

local entities = slot.create_blueprint{{
    surface = surface,
    force = "player",
    area = {{{{{}, {}}}, {{{}, {}}}}},
    include_entities = true
}}
local count = #entities

if count == 0 then
    slot.clear()
    if saved_item then slot.set_stack{{name = saved_item}} end
    rcon.print('{{"success": false, "error": "No entities in area"}}')
else
    storage.blueprints = storage.blueprints or {{}}
    storage.blueprints["{}"] = {{
        string = slot.export_stack(),
        entity_count = count
    }}
    slot.clear()
    if saved_item then slot.set_stack{{name = saved_item}} end
    rcon.print('{{"success": true, "entity_count": ' .. count .. '}}')
end
"#,
            area.left_top.x, area.left_top.y, area.right_bottom.x, area.right_bottom.y, name
        )
        .trim()
        .to_string()
    }

    /// List all saved blueprints
    pub fn list_blueprints() -> String {
        r#"
storage.blueprints = storage.blueprints or {}
local result = {}
for name, data in pairs(storage.blueprints) do
    table.insert(result, {
        name = name,
        entity_count = data.entity_count
    })
end
rcon.print(helpers.table_to_json(result))
"#
        .trim()
        .to_string()
    }

    /// Get a saved blueprint string by name
    pub fn get_blueprint(name: &str) -> String {
        format!(
            r#"
storage.blueprints = storage.blueprints or {{}}
local data = storage.blueprints["{}"]
if data then
    rcon.print(helpers.table_to_json({{
        blueprint_string = data.string,
        entity_count = data.entity_count
    }}))
else
    rcon.print('{{"error": "Blueprint not found"}}')
end
"#,
            name
        )
        .trim()
        .to_string()
    }

    /// Place a saved blueprint at a position
    pub fn place_blueprint(name: &str, position: Position, direction: u8) -> String {
        format!(
            r#"
storage.blueprints = storage.blueprints or {{}}
local data = storage.blueprints["{}"]
if not data then
    rcon.print('{{"success": false, "error": "Blueprint not found"}}')
    return
end

local player = game.get_player(1)
if not player then
    rcon.print('{{"success": false, "error": "No player"}}')
    return
end

local inv = player.get_main_inventory()
if not inv then
    rcon.print('{{"success": false, "error": "No inventory"}}')
    return
end

local slot = inv[1]
local saved_item = slot.valid_for_read and slot.name or nil
slot.set_stack{{name = "blueprint"}}
slot.import_stack(data.string)

local ghosts = slot.build_blueprint{{
    surface = game.surfaces[1],
    force = "player",
    position = {{ x = {}, y = {} }},
    direction = {},
    force_build = true
}}

storage.factorioctl_entities = storage.factorioctl_entities or {{}}
for _, ghost in pairs(ghosts) do
    if ghost.unit_number then storage.factorioctl_entities[ghost.unit_number] = ghost end
end

slot.clear()
if saved_item then slot.set_stack{{name = saved_item}} end

rcon.print(helpers.table_to_json({{
    success = true,
    ghosts_created = #ghosts
}}))
"#,
            name, position.x, position.y, direction
        )
        .trim()
        .to_string()
    }

    /// Import and place a blueprint from a string
    pub fn import_blueprint(bp_string: &str, position: Position, direction: u8) -> String {
        format!(
            r#"
local player = game.get_player(1)
if not player then
    rcon.print('{{"success": false, "error": "No player"}}')
    return
end

local inv = player.get_main_inventory()
if not inv then
    rcon.print('{{"success": false, "error": "No inventory"}}')
    return
end

local slot = inv[1]
local saved_item = slot.valid_for_read and slot.name or nil
slot.set_stack{{name = "blueprint"}}

local ok = slot.import_stack("{}")
if not ok then
    slot.clear()
    if saved_item then slot.set_stack{{name = saved_item}} end
    rcon.print('{{"success": false, "error": "Invalid blueprint string"}}')
    return
end

local ghosts = slot.build_blueprint{{
    surface = game.surfaces[1],
    force = "player",
    position = {{ x = {}, y = {} }},
    direction = {},
    force_build = true
}}

storage.factorioctl_entities = storage.factorioctl_entities or {{}}
for _, ghost in pairs(ghosts) do
    if ghost.unit_number then storage.factorioctl_entities[ghost.unit_number] = ghost end
end

slot.clear()
if saved_item then slot.set_stack{{name = saved_item}} end

rcon.print(helpers.table_to_json({{
    success = true,
    ghosts_created = #ghosts
}}))
"#,
            bp_string, position.x, position.y, direction
        )
        .trim()
        .to_string()
    }

    /// Delete a saved blueprint
    pub fn delete_blueprint(name: &str) -> String {
        format!(
            r#"
storage.blueprints = storage.blueprints or {{}}
if storage.blueprints["{}"] then
    storage.blueprints["{}"] = nil
    rcon.print('{{"success": true}}')
else
    rcon.print('{{"success": false, "error": "Blueprint not found"}}')
end
"#,
            name, name
        )
        .trim()
        .to_string()
    }

    /// Register chat message handler (captures player chat for LLM agent)
    pub fn register_chat_handler() -> String {
        r#"
if not storage.factorioctl_chat then
    storage.factorioctl_chat = { messages = {}, handler_registered = false }
end
if not storage.factorioctl_chat.handler_registered then
    script.on_event(defines.events.on_console_chat, function(event)
        local player_name = "console"
        if event.player_index then
            local p = game.get_player(event.player_index)
            if p then player_name = p.name end
        end
        table.insert(storage.factorioctl_chat.messages, {
            player = player_name,
            message = event.message,
            tick = event.tick
        })
    end)
    storage.factorioctl_chat.handler_registered = true
    rcon.print("registered")
else
    rcon.print("already_registered")
end
"#
        .trim()
        .to_string()
    }

    /// Get and clear pending chat messages
    pub fn get_and_clear_chat_messages() -> String {
        r#"
if not storage.factorioctl_chat then
    storage.factorioctl_chat = { messages = {}, handler_registered = false }
end
local msgs = storage.factorioctl_chat.messages
storage.factorioctl_chat.messages = {}
rcon.print(helpers.table_to_json(msgs))
"#
        .trim()
        .to_string()
    }

    // --- Research Commands ---

    /// Get overall research status
    pub fn get_research_status() -> String {
        r#"
local force = game.forces.player
local surface = game.surfaces[1]
local result = {
    researched_count = 0,
    total_count = 0,
    current_research = nil,
    research_progress = 0,
    research_queue = {},
    labs = {
        count = 0,
        powered = 0,
        working = 0
    },
    science_packs_in_labs = {}
}

-- Count technologies
for name, tech in pairs(force.technologies) do
    result.total_count = result.total_count + 1
    if tech.researched then
        result.researched_count = result.researched_count + 1
    end
end

-- Current research
if force.current_research then
    local tech = force.current_research
    local ingredients = {}
    for _, ing in pairs(tech.research_unit_ingredients) do
        table.insert(ingredients, {name = ing.name, amount = ing.amount})
    end
    result.current_research = {
        name = tech.name,
        level = tech.level,
        research_unit_count = tech.research_unit_count,
        ingredients = ingredients
    }
    result.research_progress = force.research_progress
end

-- Research queue
if force.research_queue then
    for i, tech in pairs(force.research_queue) do
        table.insert(result.research_queue, {
            name = tech.name,
            level = tech.level
        })
    end
end

-- Check labs
local labs = surface.find_entities_filtered{type = "lab", force = force}
result.labs.count = #labs

local science_totals = {}
for _, lab in pairs(labs) do
    -- Check if lab has power (energy > 0 or status is working)
    local status = lab.status
    if status == defines.entity_status.working then
        result.labs.working = result.labs.working + 1
        result.labs.powered = result.labs.powered + 1
    elseif status ~= defines.entity_status.no_power and status ~= defines.entity_status.low_power then
        result.labs.powered = result.labs.powered + 1
    end

    -- Count science packs in lab
    local inv = lab.get_inventory(defines.inventory.lab_input)
    if inv then
        for i = 1, #inv do
            local stack = inv[i]
            if stack and stack.valid_for_read then
                science_totals[stack.name] = (science_totals[stack.name] or 0) + stack.count
            end
        end
    end
end

for name, count in pairs(science_totals) do
    table.insert(result.science_packs_in_labs, {name = name, count = count})
end

-- Add helpful message if no labs
if result.labs.count == 0 then
    result.message = "No labs found! Build a lab and insert science packs to research."
elseif result.labs.powered == 0 then
    result.message = "Labs have no power! Connect labs to the power grid."
elseif result.current_research and #result.science_packs_in_labs == 0 then
    result.message = "Labs are empty! Insert science packs into labs to progress research."
end

rcon.print(helpers.table_to_json(result))
"#
        .trim()
        .to_string()
    }

    /// Get available research (technologies that can be researched now)
    pub fn get_available_research(agent_id: &AgentId) -> String {
        format!(
            "{}\n{}",
            Self::resolve_optional(agent_id),
            r#"local force = game.forces.player
local surface = game.surfaces[1]
local result = {
    technologies = {},
    lab_status = {
        count = 0,
        powered = 0
    },
    science_available = {}
}

-- Check labs and what science packs are available
local labs = surface.find_entities_filtered{type = "lab", force = force}
result.lab_status.count = #labs

local science_totals = {}
for _, lab in pairs(labs) do
    local status = lab.status
    if status ~= defines.entity_status.no_power and status ~= defines.entity_status.low_power then
        result.lab_status.powered = result.lab_status.powered + 1
    end
    local inv = lab.get_inventory(defines.inventory.lab_input)
    if inv then
        for i = 1, #inv do
            local stack = inv[i]
            if stack and stack.valid_for_read then
                science_totals[stack.name] = (science_totals[stack.name] or 0) + stack.count
            end
        end
    end
end

for name, count in pairs(science_totals) do
    table.insert(result.science_available, {name = name, count = count})
end

-- Get resolved character inventory science packs too
if c and c.valid then
    local inv = c.get_main_inventory()
    if inv then
        for _, item in pairs(inv.get_contents()) do
            if item.name:find("science%-pack") or item.name == "automation-science-pack" or item.name == "logistic-science-pack" then
                science_totals[item.name] = (science_totals[item.name] or 0) + item.count
                local found = false
                for _, sci in pairs(result.science_available) do
                    if sci.name == item.name then
                        sci.count = sci.count + item.count
                        sci.in_inventory = item.count
                        found = true
                        break
                    end
                end
                if not found then
                    table.insert(result.science_available, {name = item.name, count = item.count, in_inventory = item.count})
                end
            end
        end
    end
end

for name, tech in pairs(force.technologies) do
    if tech.enabled and not tech.researched then
        -- Check if all prerequisites are researched
        local can_research = true
        for _, prereq in pairs(tech.prerequisites) do
            if not prereq.researched then
                can_research = false
                break
            end
        end

        if can_research then
            local ingredients = {}
            local has_all_packs = result.lab_status.powered > 0
            for _, ing in pairs(tech.research_unit_ingredients) do
                local have = science_totals[ing.name] or 0
                if have < ing.amount then
                    has_all_packs = false
                end
                table.insert(ingredients, {
                    name = ing.name,
                    amount = ing.amount,
                    available = have
                })
            end

            local effects = {}
            for _, eff in pairs(tech.prototype.effects) do
                if eff.type == "unlock-recipe" then
                    table.insert(effects, {
                        type = "unlock-recipe",
                        recipe = eff.recipe
                    })
                elseif eff.type == "turret-attack" then
                    table.insert(effects, {
                        type = "turret-attack",
                        turret_id = eff.turret_id,
                        modifier = eff.modifier
                    })
                else
                    table.insert(effects, {
                        type = eff.type,
                        modifier = eff.modifier
                    })
                end
            end

            -- Determine readiness
            local ready = "ready"
            local blockers = {}
            if result.lab_status.count == 0 then
                ready = "blocked"
                table.insert(blockers, "no labs - build a lab first")
            elseif result.lab_status.powered == 0 then
                ready = "blocked"
                table.insert(blockers, "labs have no power")
            end
            if not has_all_packs then
                ready = "blocked"
                table.insert(blockers, "missing science packs in labs")
            end

            table.insert(result.technologies, {
                name = tech.name,
                level = tech.level,
                research_unit_count = tech.research_unit_count,
                ingredients = ingredients,
                effects = effects,
                ready = ready,
                blockers = blockers
            })
        end
    end
end

-- Add guidance message
if result.lab_status.count == 0 then
    result.guidance = "To research: 1) Craft a lab (requires iron-gear-wheel, electronic-circuit, transport-belt), 2) Place it with power, 3) Craft science packs, 4) Insert science packs into lab"
elseif result.lab_status.powered == 0 then
    result.guidance = "Labs need power! Connect them to your power grid (steam engine -> power poles -> lab)"
elseif #result.science_available == 0 then
    result.guidance = "Craft science packs and insert them into labs. Red science (automation-science-pack) requires iron-gear-wheel + copper-plate"
end

rcon.print(helpers.table_to_json(result))
"#
        )
        .trim()
        .to_string()
    }

    /// Start researching a technology (queues it properly)
    pub fn start_research(tech_name: &str) -> String {
        format!(
            r#"
local force = game.forces.player
local surface = game.surfaces[1]
local tech = force.technologies["{}"]

if not tech then
    rcon.print('{{"success": false, "error": "Technology not found"}}')
    return
end

if tech.researched then
    rcon.print('{{"success": false, "error": "Already researched"}}')
    return
end

if not tech.enabled then
    rcon.print('{{"success": false, "error": "Technology not enabled"}}')
    return
end

-- Check prerequisites
for _, prereq in pairs(tech.prerequisites) do
    if not prereq.researched then
        rcon.print('{{"success": false, "error": "Prerequisites not met: ' .. prereq.name .. '"}}')
        return
    end
end

-- Check for labs
local labs = surface.find_entities_filtered{{type = "lab", force = force}}
if #labs == 0 then
    rcon.print('{{"success": false, "error": "No labs found! Build a lab first (requires: 10 iron-gear-wheel, 10 electronic-circuit, 4 transport-belt)", "action_needed": "build_lab"}}')
    return
end

-- Check if any lab has power
local powered_labs = 0
for _, lab in pairs(labs) do
    local status = lab.status
    if status ~= defines.entity_status.no_power and status ~= defines.entity_status.low_power then
        powered_labs = powered_labs + 1
    end
end
if powered_labs == 0 then
    rcon.print('{{"success": false, "error": "Labs have no power! Connect labs to power grid.", "action_needed": "power_labs"}}')
    return
end

-- Check for required science packs
local ingredients = {{}}
local missing_packs = {{}}
local science_in_labs = {{}}

-- Count science packs in labs
for _, lab in pairs(labs) do
    local inv = lab.get_inventory(defines.inventory.lab_input)
    if inv then
        for i = 1, #inv do
            local stack = inv[i]
            if stack and stack.valid_for_read then
                science_in_labs[stack.name] = (science_in_labs[stack.name] or 0) + stack.count
            end
        end
    end
end

for _, ing in pairs(tech.research_unit_ingredients) do
    table.insert(ingredients, {{name = ing.name, amount = ing.amount}})
    local have = science_in_labs[ing.name] or 0
    if have < ing.amount then
        table.insert(missing_packs, ing.name .. " (need " .. ing.amount .. ", have " .. have .. " in labs)")
    end
end

if #missing_packs > 0 then
    rcon.print(helpers.table_to_json({{
        success = false,
        error = "Missing science packs in labs: " .. table.concat(missing_packs, ", "),
        action_needed = "insert_science_packs",
        required_packs = ingredients,
        hint = "Craft the required science packs and insert them into your labs"
    }}))
    return
end

-- Queue the research properly (not cheating)
local added = force.add_research(tech)
if added then
    rcon.print(helpers.table_to_json({{
        success = true,
        name = tech.name,
        research_unit_count = tech.research_unit_count,
        ingredients = ingredients,
        message = "Research queued! Labs will now consume science packs to progress."
    }}))
else
    rcon.print('{{"success": false, "error": "Failed to queue research - check if another research is in progress"}}')
end
"#,
            tech_name
        )
        .trim()
        .to_string()
    }

    // --- Power Network Commands ---

    /// Get power status at a location (enhanced version with generator/consumer details)
    pub fn get_power_status(x: i32, y: i32, radius: u32) -> String {
        format!(
            r#"
local surface = game.surfaces[1]
local r = {}
local area = {{ {{ {} - r, {} - r }}, {{ {} + r, {} + r }} }}

local poles = surface.find_entities_filtered{{
    type = "electric-pole",
    area = area
}}

if #poles == 0 then
    rcon.print('{{"error": "No electric poles found in area"}}')
    return
end

-- Find the network from the first pole
local pole = poles[1]
local network_id = pole.electric_network_id

local result = {{
    network_id = network_id,
    pole_count = #poles,
    generators = {{}},
    consumers = {{
        working = 0,
        low_power = 0,
        no_power = 0,
        total = 0
    }},
    production_kw = 0,
    consumption_kw = 0,
    satisfaction = "unknown"
}}

-- Count generators by type in this network
local generator_counts = {{}}
local total_production = 0

-- Find all generators (steam engines, solar panels, etc.)
local generators = surface.find_entities_filtered{{
    area = area,
    type = {{ "generator", "solar-panel", "accumulator" }}
}}

for _, gen in pairs(generators) do
    -- Check if connected to same network
    local connected_pole = surface.find_entities_filtered{{
        type = "electric-pole",
        position = gen.position,
        radius = 10,
        limit = 1
    }}[1]

    if connected_pole and connected_pole.electric_network_id == network_id then
        generator_counts[gen.name] = (generator_counts[gen.name] or 0) + 1

        if gen.type == "generator" then
            local energy = gen.energy_generated_last_tick or 0
            total_production = total_production + energy * 60 / 1000
        elseif gen.type == "solar-panel" then
            total_production = total_production + 60 * surface.daytime
        end
    end
end

for name, count in pairs(generator_counts) do
    table.insert(result.generators, {{ name = name, count = count }})
end

-- Find all electric consumers and check their status
local consumer_types = {{ "assembling-machine", "furnace", "lab", "mining-drill", "inserter", "beacon", "radar" }}
local total_consumption = 0
local consumers_by_status = {{ working = {{}}, low_power = {{}}, no_power = {{}} }}

for _, etype in pairs(consumer_types) do
    local entities = surface.find_entities_filtered{{
        area = area,
        type = etype
    }}

    for _, ent in pairs(entities) do
        -- Check if entity uses electricity
        local proto = prototypes.entity[ent.name]
        local uses_electric = false
        pcall(function()
            uses_electric = proto.electric_energy_source_prototype ~= nil
        end)

        if uses_electric then
            result.consumers.total = result.consumers.total + 1

            local status = ent.status
            if status == defines.entity_status.no_power then
                result.consumers.no_power = result.consumers.no_power + 1
                table.insert(consumers_by_status.no_power, {{
                    name = ent.name,
                    x = ent.position.x,
                    y = ent.position.y,
                    unit_number = ent.unit_number
                }})
            elseif status == defines.entity_status.low_power then
                result.consumers.low_power = result.consumers.low_power + 1
                table.insert(consumers_by_status.low_power, {{
                    name = ent.name,
                    x = ent.position.x,
                    y = ent.position.y,
                    unit_number = ent.unit_number
                }})
            elseif status == defines.entity_status.working then
                result.consumers.working = result.consumers.working + 1
            end

            -- Estimate consumption
            pcall(function()
                local usage = proto.energy_usage or 0
                if status == defines.entity_status.working then
                    total_consumption = total_consumption + usage * 60 / 1000
                end
            end)
        end
    end
end

result.production_kw = math.floor(total_production)
result.consumption_kw = math.floor(total_consumption)

-- Determine satisfaction
if result.consumers.no_power > 0 then
    result.satisfaction = "critical"
elseif result.consumers.low_power > 0 then
    result.satisfaction = "low"
elseif result.consumers.working > 0 then
    result.satisfaction = "ok"
else
    result.satisfaction = "idle"
end

-- Add sample of problem entities (up to 5 each)
if #consumers_by_status.no_power > 0 then
    result.no_power_entities = {{}}
    for i = 1, math.min(5, #consumers_by_status.no_power) do
        table.insert(result.no_power_entities, consumers_by_status.no_power[i])
    end
end

if #consumers_by_status.low_power > 0 then
    result.low_power_entities = {{}}
    for i = 1, math.min(5, #consumers_by_status.low_power) do
        table.insert(result.low_power_entities, consumers_by_status.low_power[i])
    end
end

-- Get flow statistics if available
local stats = pole.electric_network_statistics
if stats then
    local input_flow = {{}}
    local output_flow = {{}}

    for name, count in pairs(stats.input_counts) do
        local flow = stats.get_flow_count{{
            name = name,
            input = true,
            precision_index = defines.flow_precision_index.five_seconds
        }}
        if flow > 0 then
            table.insert(input_flow, {{ name = name, flow = flow }})
        end
    end

    for name, count in pairs(stats.output_counts) do
        local flow = stats.get_flow_count{{
            name = name,
            input = false,
            precision_index = defines.flow_precision_index.five_seconds
        }}
        if flow > 0 then
            table.insert(output_flow, {{ name = name, flow = flow }})
        end
    end

    if #input_flow > 0 then result.input_flow = input_flow end
    if #output_flow > 0 then result.output_flow = output_flow end
end

rcon.print(helpers.table_to_json(result))
"#,
            radius, x, y, x, y
        )
        .trim()
        .to_string()
    }

    /// Get all power networks in an area
    pub fn get_power_networks(x: i32, y: i32, radius: u32) -> String {
        format!(
            r#"
local surface = game.surfaces[1]
local r = {}
local area = {{ {{ {} - r, {} - r }}, {{ {} + r, {} + r }} }}
local poles = surface.find_entities_filtered{{
    type = "electric-pole",
    area = area
}}

-- Group by network ID
local networks = {{}}
for _, pole in pairs(poles) do
    local net_id = pole.electric_network_id
    if net_id then
        if not networks[net_id] then
            networks[net_id] = {{
                network_id = net_id,
                pole_count = 0,
                poles = {{}}
            }}
        end
        networks[net_id].pole_count = networks[net_id].pole_count + 1
        if #networks[net_id].poles < 3 then
            table.insert(networks[net_id].poles, {{
                name = pole.name,
                position = {{ x = pole.position.x, y = pole.position.y }}
            }})
        end
    end
end

local result = {{}}
for net_id, data in pairs(networks) do
    table.insert(result, data)
end

rcon.print(helpers.table_to_json(result))
"#,
            radius, x, y, x, y
        )
        .trim()
        .to_string()
    }

    /// Find power issues - entities without power or with low power
    pub fn find_power_issues(x: i32, y: i32, radius: u32) -> String {
        format!(
            r#"
local surface = game.surfaces[1]
local r = {}
local area = {{ {{ {} - r, {} - r }}, {{ {} + r, {} + r }} }}

local result = {{
    unpowered_entities = {{}},
    low_power_entities = {{}},
    suggested_actions = {{}}
}}

-- Find all poles for coverage analysis
local poles = surface.find_entities_filtered{{
    type = "electric-pole",
    area = area
}}

-- Build pole coverage map (which tiles have power)
local coverage = {{}}
local pole_supply_areas = {{
    ["small-electric-pole"] = 2.5,
    ["medium-electric-pole"] = 3.5,
    ["big-electric-pole"] = 2.0,
    ["substation"] = 9.0
}}

for _, pole in pairs(poles) do
    local supply_dist = pole_supply_areas[pole.name] or 2.5
    local px, py = math.floor(pole.position.x), math.floor(pole.position.y)
    local sd = math.ceil(supply_dist)
    for dx = -sd, sd do
        for dy = -sd, sd do
            if dx*dx + dy*dy <= supply_dist * supply_dist then
                local key = (px + dx) .. "," .. (py + dy)
                coverage[key] = pole.electric_network_id
            end
        end
    end
end

-- Check all electric consumers
local consumer_types = {{ "assembling-machine", "furnace", "lab", "mining-drill", "inserter", "beacon", "radar", "lamp", "roboport" }}

for _, etype in pairs(consumer_types) do
    local entities = surface.find_entities_filtered{{
        area = area,
        type = etype
    }}

    for _, ent in pairs(entities) do
        -- Check if entity uses electricity
        local proto = prototypes.entity[ent.name]
        local uses_electric = false
        pcall(function()
            uses_electric = proto.electric_energy_source_prototype ~= nil
        end)

        if uses_electric then
            local status = ent.status
            local ex, ey = math.floor(ent.position.x), math.floor(ent.position.y)
            local key = ex .. "," .. ey

            if status == defines.entity_status.no_power then
                table.insert(result.unpowered_entities, {{
                    unit_number = ent.unit_number,
                    name = ent.name,
                    x = ent.position.x,
                    y = ent.position.y,
                    in_coverage = coverage[key] ~= nil
                }})

                -- Suggest action
                if not coverage[key] then
                    table.insert(result.suggested_actions,
                        "Place pole near (" .. ex .. ", " .. ey .. ") to power " .. ent.name)
                else
                    table.insert(result.suggested_actions,
                        ent.name .. " at (" .. ex .. ", " .. ey .. ") is in coverage but has no power - check generator capacity")
                end
            elseif status == defines.entity_status.low_power then
                table.insert(result.low_power_entities, {{
                    unit_number = ent.unit_number,
                    name = ent.name,
                    x = ent.position.x,
                    y = ent.position.y
                }})

                table.insert(result.suggested_actions,
                    ent.name .. " at (" .. ex .. ", " .. ey .. ") has low power - add more generators")
            end
        end
    end
end

-- Summary
result.summary = {{
    unpowered_count = #result.unpowered_entities,
    low_power_count = #result.low_power_entities,
    pole_count = #poles
}}

-- Limit suggested actions to top 10
if #result.suggested_actions > 10 then
    local limited = {{}}
    for i = 1, 10 do
        limited[i] = result.suggested_actions[i]
    end
    result.suggested_actions = limited
    result.summary.more_issues = #result.suggested_actions - 10
end

rcon.print(helpers.table_to_json(result))
"#,
            radius, x, y, x, y
        )
        .trim()
        .to_string()
    }

    /// Get power coverage data for map visualization
    pub fn get_power_coverage(x: i32, y: i32, radius: u32) -> String {
        format!(
            r#"
local surface = game.surfaces[1]
local r = {}
local area = {{ {{ {} - r, {} - r }}, {{ {} + r, {} + r }} }}

-- Find all poles
local poles = surface.find_entities_filtered{{
    type = "electric-pole",
    area = area
}}

-- Supply area distances for different pole types
local pole_supply_areas = {{
    ["small-electric-pole"] = 2.5,
    ["medium-electric-pole"] = 3.5,
    ["big-electric-pole"] = 2.0,
    ["substation"] = 9.0
}}

-- Assign network IDs to single digits (1-9)
local network_map = {{}}
local next_id = 1

local result = {{
    poles = {{}},
    coverage = {{}},
    networks = {{}}
}}

for _, pole in pairs(poles) do
    local net_id = pole.electric_network_id
    if net_id and not network_map[net_id] then
        network_map[net_id] = next_id
        result.networks[tostring(next_id)] = net_id
        next_id = next_id + 1
        if next_id > 9 then next_id = 9 end
    end

    local supply_dist = pole_supply_areas[pole.name] or 2.5
    local display_id = network_map[net_id] or 0

    table.insert(result.poles, {{
        name = pole.name,
        x = pole.position.x,
        y = pole.position.y,
        network_id = net_id,
        display_id = display_id,
        supply_area = supply_dist
    }})

    local px, py = math.floor(pole.position.x), math.floor(pole.position.y)
    local sd = math.ceil(supply_dist)
    for dx = -sd, sd do
        for dy = -sd, sd do
            if dx*dx + dy*dy <= supply_dist * supply_dist then
                local tx, ty = px + dx, py + dy
                local key = tx .. "," .. ty
                if tx >= {} - r and tx <= {} + r and ty >= {} - r and ty <= {} + r then
                    result.coverage[key] = display_id
                end
            end
        end
    end
end

rcon.print(helpers.table_to_json(result))
"#,
            radius, x, y, x, y, x, x, y, y
        )
        .trim()
        .to_string()
    }

    // --- Alerts/Notifications Commands ---

    /// Get alerts for urgent conditions in an area
    pub fn get_alerts(x: i32, y: i32, radius: u32) -> String {
        format!(
            r#"
local surface = game.surfaces[1]
local r = {}
local area = {{ {{ {} - r, {} - r }}, {{ {} + r, {} + r }} }}
local alerts = {{}}

-- 1. Check for low power (poles with low satisfaction)
local poles = surface.find_entities_filtered{{ type = "electric-pole", area = area }}
local checked_networks = {{}}
for _, pole in pairs(poles) do
    local net_id = pole.electric_network_id
    if net_id and not checked_networks[net_id] then
        checked_networks[net_id] = true
        -- Check if any connected entity has insufficient power
        -- Note: Factorio 2.0 doesn't expose satisfaction directly,
        -- so we check for entities with low_power status
    end
end

-- 2. Empty drills (mining drills with no resources)
local drills = surface.find_entities_filtered{{ type = "mining-drill", area = area }}
for _, drill in pairs(drills) do
    if drill.mining_target == nil and drill.status == defines.entity_status.no_minable_resources then
        table.insert(alerts, {{
            type = "empty_drill",
            entity_name = drill.name,
            position = {{ x = drill.position.x, y = drill.position.y }},
            unit_number = drill.unit_number
        }})
    end
end

-- 3. Furnaces/boilers out of fuel
local furnaces = surface.find_entities_filtered{{ type = "furnace", area = area }}
for _, furnace in pairs(furnaces) do
    if furnace.burner then
        local fuel_inv = furnace.get_fuel_inventory()
        if fuel_inv and fuel_inv.is_empty() then
            table.insert(alerts, {{
                type = "no_fuel",
                entity_name = furnace.name,
                position = {{ x = furnace.position.x, y = furnace.position.y }},
                unit_number = furnace.unit_number
            }})
        end
    end
end

local boilers = surface.find_entities_filtered{{ type = "boiler", area = area }}
for _, boiler in pairs(boilers) do
    if boiler.burner then
        local fuel_inv = boiler.get_fuel_inventory()
        if fuel_inv and fuel_inv.is_empty() then
            table.insert(alerts, {{
                type = "no_fuel",
                entity_name = boiler.name,
                position = {{ x = boiler.position.x, y = boiler.position.y }},
                unit_number = boiler.unit_number
            }})
        end
    end
end

-- 4. Assemblers without power or ingredients
local assemblers = surface.find_entities_filtered{{ type = "assembling-machine", area = area }}
for _, asm in pairs(assemblers) do
    if asm.status == defines.entity_status.no_power then
        table.insert(alerts, {{
            type = "no_power",
            entity_name = asm.name,
            position = {{ x = asm.position.x, y = asm.position.y }},
            unit_number = asm.unit_number
        }})
    elseif asm.status == defines.entity_status.no_ingredients then
        table.insert(alerts, {{
            type = "no_ingredients",
            entity_name = asm.name,
            position = {{ x = asm.position.x, y = asm.position.y }},
            unit_number = asm.unit_number,
            recipe = asm.get_recipe() and asm.get_recipe().name or nil
        }})
    end
end

-- 5. Nearby enemies
local enemies = surface.find_entities_filtered{{
    force = "enemy",
    area = area,
    limit = 10
}}
for _, enemy in pairs(enemies) do
    table.insert(alerts, {{
        type = "enemy_nearby",
        entity_name = enemy.name,
        position = {{ x = enemy.position.x, y = enemy.position.y }},
        health = enemy.health
    }})
end

rcon.print(helpers.table_to_json(alerts))
"#,
            radius, x, y, x, y
        )
        .trim()
        .to_string()
    }

    /// Get items on transport belts with lane separation
    pub fn get_belt_lane_contents(area: Area) -> String {
        format!(
            r#"
local belts = game.surfaces[1].find_entities_filtered{{
    area={{{{{},{}}},{{{},{}}}}},
    type="transport-belt"
}}
local result = {{}}

for _, belt in pairs(belts) do
    local left_items = {{}}
    local right_items = {{}}
    local left_count = 0
    local right_count = 0

    local line1 = belt.get_transport_line(1)
    if line1 then
        for name, count in pairs(line1.get_contents()) do
            table.insert(left_items, {{name=name, count=count}})
            left_count = left_count + count
        end
    end

    local line2 = belt.get_transport_line(2)
    if line2 then
        for name, count in pairs(line2.get_contents()) do
            table.insert(right_items, {{name=name, count=count}})
            right_count = right_count + count
        end
    end

    if #left_items > 0 or #right_items > 0 then
        table.insert(result, {{
            position = {{x=math.floor(belt.position.x), y=math.floor(belt.position.y)}},
            unit_number = belt.unit_number,
            direction = belt.direction,
            belt_type = belt.name,
            left_lane = {{lane=1, items=left_items, item_count=left_count}},
            right_lane = {{lane=2, items=right_items, item_count=right_count}}
        }})
    end
end
rcon.print(helpers.table_to_json(result))
"#,
            area.left_top.x, area.left_top.y, area.right_bottom.x, area.right_bottom.y
        )
        .trim()
        .to_string()
    }

    /// Clear trees and rocks in an area by mining them (player gets the items)
    /// Returns the count of cleared entities and items gained
    /// Requires player to be within proximity of the area
    pub fn clear_area(
        agent_id: &AgentId,
        area: Area,
        clear_trees: bool,
        clear_rocks: bool,
        dry_run: bool,
    ) -> String {
        let trees_filter = if clear_trees { "true" } else { "false" };
        let rocks_filter = if clear_rocks { "true" } else { "false" };
        let dry_run_str = if dry_run { "true" } else { "false" };
        let resolve = Self::resolve_required(agent_id);

        format!(
            r#"
{}
local surface = game.surfaces[1]
local area = {{{{{},{}}},{{{},{}}}}}
local clear_trees = {}
local clear_rocks = {}
local dry_run = {}
local max_distance = 30

local result = {{
    trees_found = 0,
    rocks_found = 0,
    trees_mined = 0,
    rocks_mined = 0,
    dry_run = dry_run,
    too_far = false,
    items_gained = {{}}
}}

-- Check proximity to area center
local cx, cy = c.position.x, c.position.y
local area_center_x = (area[1][1] + area[2][1]) / 2
local area_center_y = (area[1][2] + area[2][2]) / 2
local dx = cx - area_center_x
local dy = cy - area_center_y
local dist = math.sqrt(dx*dx + dy*dy)

if dist > max_distance and not dry_run then
    result.too_far = true
    result.distance = dist
    result.max_distance = max_distance
    rcon.print(helpers.table_to_json(result))
    return
end

-- Count inventory before mining
local inv = c.get_main_inventory()
local before = {{}}
if inv then
    for _, item in pairs(inv.get_contents()) do
        before[item.name] = item.count
    end
end

-- Find and mine trees
if clear_trees then
    local trees = surface.find_entities_filtered{{ type = "tree", area = area }}
    result.trees_found = #trees
    if not dry_run then
        for _, tree in pairs(trees) do
            if c.mine_entity(tree, true) then
                result.trees_mined = result.trees_mined + 1
            end
        end
    end
end

-- Find and mine rocks (simple-entity with rock in name)
if clear_rocks then
    local entities = surface.find_entities_filtered{{ type = "simple-entity", area = area }}
    for _, e in pairs(entities) do
        if e.name:find("rock") then
            result.rocks_found = result.rocks_found + 1
            if not dry_run then
                if c.mine_entity(e, true) then
                    result.rocks_mined = result.rocks_mined + 1
                end
            end
        end
    end
end

-- Count inventory after and calculate gained items
if not dry_run and inv then
    for _, item in pairs(inv.get_contents()) do
        local gained = item.count - (before[item.name] or 0)
        if gained > 0 then
            table.insert(result.items_gained, {{ name = item.name, count = gained }})
        end
    end
end

rcon.print(helpers.table_to_json(result))
"#,
            resolve,
            area.left_top.x,
            area.left_top.y,
            area.right_bottom.x,
            area.right_bottom.y,
            trees_filter,
            rocks_filter,
            dry_run_str
        )
        .trim()
        .to_string()
    }
}
