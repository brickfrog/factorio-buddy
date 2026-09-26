-- Read-only evaluation sample for unattended trials (contract C2).
--
-- One call reports, around the agent character, the machine counters and
-- status, labs, entity counts, research progress and force item production so
-- an evaluator can compare two samples taken some ticks apart. It never
-- mutates the world.
local characters = require("characters")
local inventory = require("inventory")

local M = {}

local DEFAULT_RADIUS = 128
local MIN_RADIUS = 16
local MAX_RADIUS = 512
local MAX_MACHINES = 400

local MACHINE_TYPES = {
    "assembling-machine",
    "furnace",
    "rocket-silo",
    "mining-drill",
    "boiler",
    "generator",
    "burner-generator",
}

local function failure(error_kind, message)
    return {success = false, error_kind = error_kind, error = message}
end

local function status_name(entity)
    local ok, value = pcall(function() return entity.status end)
    if not ok or value == nil then return nil end
    for name, status in pairs(defines.entity_status) do
        if status == value then return name end
    end
    return tostring(value)
end

-- Item counts keyed by item name; non-normal quality is keyed "name@quality".
local function counts(inv)
    local result = {}
    if not inv then return result end
    for _, item in pairs(inv.get_contents()) do
        local quality = inventory.quality_name(item) or "normal"
        local key = quality == "normal" and item.name or (item.name .. "@" .. quality)
        result[key] = (result[key] or 0) + item.count
    end
    return result
end

-- The recipe the machine is set to now; nil when it has none.
local function machine_recipe(entity)
    local ok, recipe = pcall(function() return entity.get_recipe() end)
    if ok and recipe then return recipe.name end
    return nil
end

-- The last recipe a furnace ran (Factorio keeps it after the furnace idles or
-- is cleared). Reported separately so "had a recipe" never reads as "has one".
local function machine_previous_recipe(entity)
    local ok, previous = pcall(function() return entity.previous_recipe end)
    if ok and type(previous) == "table" and previous.name then
        return type(previous.name) == "string" and previous.name or previous.name.name
    end
    return nil
end

local function products_finished(entity)
    local ok, value = pcall(function() return entity.products_finished end)
    if ok and type(value) == "number" then return value end
    return nil
end

local function fuel_counts(entity)
    local ok, fuel = pcall(function() return entity.get_fuel_inventory() end)
    if ok and fuel then return counts(fuel) end
    return {}
end

local function clamp_radius(radius)
    if not inventory.finite_number(radius) then return DEFAULT_RADIUS end
    return math.min(math.max(radius, MIN_RADIUS), MAX_RADIUS)
end

local function force_item_production(force, surface)
    local result = {}
    local stats = force.get_item_production_statistics(surface)
    for item, count in pairs(stats.input_counts or {}) do
        local name = type(item) == "string" and item or item.name
        if count > 0 then
            local entry = result[name] or {input_count = 0}
            entry.input_count = entry.input_count + count
            result[name] = entry
        end
    end
    return result
end

local function research_state(force)
    local researched = 0
    for _, technology in pairs(force.technologies) do
        if technology.researched then researched = researched + 1 end
    end
    local current = force.current_research
    return {
        current = current and current.name or nil,
        progress = force.research_progress or 0,
        researched_count = researched,
        rocket_silo_researched = force.technologies["rocket-silo"] ~= nil
            and force.technologies["rocket-silo"].researched or false,
        rockets_launched = force.rockets_launched,
    }
end

function M.sample(agent_id, radius)
    local character = characters.find(agent_id)
    if not (character and character.valid) then
        return failure("no_character", "no character for agent " .. tostring(agent_id) .. "; spawn first")
    end
    local surface = character.surface
    local force = character.force
    local origin = character.position
    local effective_radius = clamp_radius(radius)

    local machines = {}
    for _, entity in pairs(surface.find_entities_filtered{
        position = origin,
        radius = effective_radius,
        force = force,
        type = MACHINE_TYPES,
    }) do
        local dx = entity.position.x - origin.x
        local dy = entity.position.y - origin.y
        table.insert(machines, {entity = entity, distance = dx * dx + dy * dy})
    end
    table.sort(machines, function(a, b)
        if a.distance ~= b.distance then return a.distance < b.distance end
        return a.entity.unit_number < b.entity.unit_number
    end)
    local machine_rows = {}
    for index = 1, math.min(#machines, MAX_MACHINES) do
        local entity = machines[index].entity
        table.insert(machine_rows, {
            unit_number = entity.unit_number,
            name = entity.name,
            type = entity.type,
            position = {x = entity.position.x, y = entity.position.y},
            status = status_name(entity),
            recipe = machine_recipe(entity),
            previous_recipe = machine_previous_recipe(entity),
            products_finished = products_finished(entity),
            fuel = fuel_counts(entity),
        })
    end

    local labs = {}
    for _, lab in pairs(surface.find_entities_filtered{
        position = origin,
        radius = effective_radius,
        force = force,
        type = "lab",
    }) do
        table.insert(labs, {
            unit_number = lab.unit_number,
            status = status_name(lab),
            inventory = counts(lab.get_inventory(defines.inventory.lab_input)),
        })
    end
    table.sort(labs, function(a, b) return a.unit_number < b.unit_number end)

    local entity_counts = {}
    for _, entity in pairs(surface.find_entities_filtered{
        position = origin,
        radius = effective_radius,
        force = force,
    }) do
        if entity.type ~= "character" then
            entity_counts[entity.name] = (entity_counts[entity.name] or 0) + 1
        end
    end

    return {
        success = true,
        tick = game.tick,
        surface = surface.name,
        radius = effective_radius,
        connected_players = #game.connected_players,
        character = {
            position = {x = origin.x, y = origin.y},
            inventory = counts(character.get_main_inventory()),
        },
        research = research_state(force),
        machines = machine_rows,
        machines_total = #machines,
        machines_truncated = #machines > MAX_MACHINES,
        labs = labs,
        entity_counts = entity_counts,
        force_item_production = force_item_production(force, surface),
    }
end

return M
