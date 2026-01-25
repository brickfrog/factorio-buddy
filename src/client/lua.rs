//! Lua command builders for Factorio interactions
//!
//! These builders generate Lua code that can be executed via RCON.
//! All commands use rcon.print() to return JSON-formatted results.

use crate::world::{Area, Direction, Position};

/// Builder for Lua commands
pub struct LuaCommand;

impl LuaCommand {
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
    table.insert(result, {{
        unit_number = e.unit_number,
        name = e.name,
        type = e.type,
        position = {{ x = e.position.x, y = e.position.y }},
        direction = e.direction,
        health = e.health,
        force = e.force.name
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
        format!(
            r#"
-- Find entity by unit_number via search
local e = nil
for _, entity in pairs(game.surfaces[1].find_entities_filtered{{area={{{{-500,-500}},{{500,500}}}}}}) do
    if entity.unit_number == {} then
        e = entity
        break
    end
end
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
            unit_number
        )
        .trim()
        .to_string()
    }

    /// Get an entity's inventories
    pub fn get_entity_inventory(unit_number: u32) -> String {
        format!(
            r#"
-- Find entity by unit_number via search
local e = nil
for _, entity in pairs(game.surfaces[1].find_entities_filtered{{area={{{{-500,-500}},{{500,500}}}}}}) do
    if entity.unit_number == {} then
        e = entity
        break
    end
end
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
            unit_number
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
            collides_with_player = tile.collides_with("player-layer")
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
    collides_with_player = tile.collides_with("player-layer")
}}))
"#,
            position.x as i32, position.y as i32, position.x as i32, position.y as i32
        )
        .trim()
        .to_string()
    }

    /// Initialize character entity
    pub fn init_character() -> String {
        r#"
if not global then global = {} end
if global.factorioctl_character and global.factorioctl_character.valid then
    local c = global.factorioctl_character
    rcon.print(helpers.table_to_json({
        unit_number = c.unit_number,
        name = c.name,
        type = c.type,
        position = { x = c.position.x, y = c.position.y },
        direction = c.direction,
        health = c.health,
        force = c.force.name
    }))
else
    -- Create new character at spawn
    local c = game.surfaces[1].create_entity{
        name = "character",
        position = {0, 0},
        force = game.forces.player
    }
    if c then
        global.factorioctl_character = c
        rcon.print(helpers.table_to_json({
            unit_number = c.unit_number,
            name = c.name,
            type = c.type,
            position = { x = c.position.x, y = c.position.y },
            direction = c.direction,
            health = c.health,
            force = c.force.name
        }))
    else
        rcon.print('{"error": "Failed to create character"}')
    end
end
"#
        .trim()
        .to_string()
    }

    /// Teleport character to position
    pub fn teleport_character(position: Position) -> String {
        format!(
            r#"
if not global then global = {{}} end local c = global.factorioctl_character
if c and c.valid then
    c.teleport({{ {}, {} }})
    rcon.print("ok")
else
    rcon.print('{{"error": "No character"}}')
end
"#,
            position.x, position.y
        )
        .trim()
        .to_string()
    }

    /// Start walking character to position
    pub fn walk_character(position: Position) -> String {
        format!(
            r#"
if not global then global = {{}} end local c = global.factorioctl_character
if c and c.valid then
    -- Calculate direction to target
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
else
    rcon.print('{{"error": "No character"}}')
end
"#,
            position.x, position.y
        )
        .trim()
        .to_string()
    }

    /// Get character status
    pub fn character_status() -> String {
        r#"
if not global then global = {} end local c = global.factorioctl_character
if c and c.valid then
    rcon.print(helpers.table_to_json({
        valid = true,
        unit_number = c.unit_number,
        position = { x = c.position.x, y = c.position.y },
        health = c.health,
        crafting_queue_size = c.crafting_queue_size,
        walking = c.walking_state.walking
    }))
else
    rcon.print('{"valid": false}')
end
"#
        .trim()
        .to_string()
    }

    /// Get character inventory
    pub fn character_inventory() -> String {
        r#"
if not global then global = {} end local c = global.factorioctl_character
if c and c.valid then
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
end
"#
        .trim()
        .to_string()
    }

    /// Mine entity at position
    pub fn mine_at(position: Position, count: u32) -> String {
        format!(
            r#"
local c = nil
for _, p in pairs(game.connected_players) do if p.character and p.character.valid then c = p.character break end end
if not c then if not global then global = {{}} end c = global.factorioctl_character end
if not (c and c.valid) then
    rcon.print('{{"success": false, "error": "No character"}}')
    return
end

-- Count inventory before mining
local inv = c.get_main_inventory()
local before_count = 0
if inv then
    for _, item in pairs(inv.get_contents()) do
        before_count = before_count + item.count
    end
end

local mined = 0

for i = 1, {} do
    -- Try to find resources first (iron-ore, coal, etc.)
    local resources = game.surfaces[1].find_entities_filtered{{
        position = {{ {}, {} }},
        radius = 2,
        type = "resource"
    }}

    local target = nil
    if #resources > 0 then
        target = resources[1]
    else
        -- Fall back to other minable entities
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
    success = items_gained > 0,
    mined_count = items_gained,
    inventory = items
}}))
"#,
            count, position.x, position.y, position.x, position.y
        )
        .trim()
        .to_string()
    }

    /// Mine nearest entity of type
    pub fn mine_nearest(entity_type: &str, count: u32) -> String {
        format!(
            r#"
local c = nil
for _, p in pairs(game.connected_players) do if p.character and p.character.valid then c = p.character break end end
if not c then if not global then global = {{}} end c = global.factorioctl_character end
if not (c and c.valid) then
    rcon.print('{{"success": false, "error": "No character"}}')
    return
end

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
            count, entity_type
        )
        .trim()
        .to_string()
    }

    /// Start crafting a recipe
    pub fn craft(recipe: &str, count: u32) -> String {
        format!(
            r#"
if not global then global = {{}} end local c = global.factorioctl_character
if not (c and c.valid) then
    rcon.print('{{"success": false, "error": "No character"}}')
    return
end

local crafted = c.begin_crafting{{ recipe = "{}", count = {} }}
rcon.print(helpers.table_to_json({{
    success = crafted > 0,
    queued = crafted,
    queue_size = c.crafting_queue_size
}}))
"#,
            recipe, count
        )
        .trim()
        .to_string()
    }

    /// Wait for crafting to complete (poll-based, handled in client)
    pub fn wait_for_crafting() -> String {
        r#"
if not global then global = {} end local c = global.factorioctl_character
if c and c.valid then
    rcon.print(tostring(c.crafting_queue_size))
else
    rcon.print("0")
end
"#
        .trim()
        .to_string()
    }

    /// Place an entity from inventory
    pub fn place_entity(entity_name: &str, position: Position, direction: Direction) -> String {
        format!(
            r#"
local c = nil
for _, p in pairs(game.connected_players) do if p.character and p.character.valid then c = p.character break end end
if not c then if not global then global = {{}} end c = global.factorioctl_character end
if not (c and c.valid) then
    rcon.print('{{"error": "No character"}}')
    return
end

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
    force = c.force
}}

if not can_place then
    rcon.print('{{"error": "Cannot place entity here"}}')
    return
end

-- Create the entity
local e = game.surfaces[1].create_entity{{
    name = "{}",
    position = {{ {}, {} }},
    direction = {},
    force = c.force
}}

if e then
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
            entity_name,
            entity_name,
            position.x,
            position.y,
            direction.to_factorio(),
            entity_name,
            position.x,
            position.y,
            direction.to_factorio(),
            entity_name
        )
        .trim()
        .to_string()
    }

    /// Remove entity at position
    pub fn remove_entity_at(position: Position) -> String {
        format!(
            r#"
local entities = game.surfaces[1].find_entities_filtered{{
    position = {{ {}, {} }},
    radius = 0.5
}}

for _, e in pairs(entities) do
    if e.type ~= "character" then
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
        format!(
            r#"
-- Find entity by unit_number via search
local e = nil
for _, entity in pairs(game.surfaces[1].find_entities_filtered{{area={{{{-500,-500}},{{500,500}}}}}}) do
    if entity.unit_number == {} then
        e = entity
        break
    end
end
if e then
    e.destroy()
    rcon.print("ok")
else
    rcon.print('{{"error": "Entity not found"}}')
end
"#,
            unit_number
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
            _ => "defines.inventory.fuel",
        };

        format!(
            r#"
-- Find entity by unit_number via search (get_entity_by_unit_number doesn't work for Lua-created entities)
local e = nil
for _, entity in pairs(game.surfaces[1].find_entities_filtered{{area={{{{-500,-500}},{{500,500}}}}}}) do
    if entity.unit_number == {} then
        e = entity
        break
    end
end
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
            unit_number, inv_define, item, count
        )
        .trim()
        .to_string()
    }

    /// Set recipe on an assembling machine
    pub fn set_recipe(unit_number: u32, recipe: &str) -> String {
        format!(
            r#"
-- Find entity by unit_number via search
local e = nil
for _, entity in pairs(game.surfaces[1].find_entities_filtered{{area={{{{-500,-500}},{{500,500}}}}}}) do
    if entity.unit_number == {} then
        e = entity
        break
    end
end
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
            unit_number, recipe
        )
        .trim()
        .to_string()
    }
}
