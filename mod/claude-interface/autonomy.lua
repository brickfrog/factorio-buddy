local diagnostics = require("diagnostics")
local entities = require("entities")
local inventory = require("inventory")
local research = require("research")
local world = require("world")

local M = {}

local function position_table(position)
    if not position then return nil end
    return {x = position.x, y = position.y}
end

local function status_name(status_value)
    if status_value == nil then return nil end
    for name, value in pairs(defines.entity_status) do
        if value == status_value then return name end
    end
    return tostring(status_value)
end

local function entity_status(entity)
    local ok, value = pcall(function() return entity.status end)
    if not ok then return nil end
    return status_name(value)
end

local function energy_source(entity)
    local burner_ok, burner = pcall(function() return entity.burner end)
    if burner_ok and burner then return "burner" end

    local prototype = entity.prototype
    local electric_ok, electric = pcall(function()
        return prototype.electric_energy_source_prototype
    end)
    if electric_ok and electric then return "electric" end

    local heat_ok, heat = pcall(function()
        return prototype.heat_energy_source_prototype
    end)
    if heat_ok and heat then return "heat" end

    local fluid_ok, fluid = pcall(function()
        return prototype.fluid_energy_source_prototype
    end)
    if fluid_ok and fluid then return "fluid" end

    return "other"
end

local function sorted_counts(counts)
    local result = {}
    for name, count in pairs(counts) do
        table.insert(result, {name = name, count = count})
    end
    table.sort(result, function(a, b)
        if a.count == b.count then return a.name < b.name end
        return a.count > b.count
    end)
    return result
end

local function compact_production(surface_name, force)
    local statistics = diagnostics.production_statistics(surface_name, force)
    local active_items = {}
    local active_item_count = 0
    for _, item in ipairs(statistics.items or {}) do
        if (item.produced_per_minute or 0) ~= 0 or (item.consumed_per_minute or 0) ~= 0 then
            active_item_count = active_item_count + 1
            if #active_items < 50 then table.insert(active_items, item) end
        end
    end
    return {
        window = statistics.window,
        active_items = active_items,
        active_item_count = active_item_count,
        truncated = active_item_count > #active_items,
    }
end

local function character_snapshot(character)
    if not character or not character.valid then return nil end
    local main_inventory = character.get_main_inventory()
    return {
        position = position_table(character.position),
        health = character.health,
        inventory = inventory.contents(main_inventory),
    }
end

local function is_factory_entity(entity)
    if not (entity and entity.valid) or entity.type == "character" then return false end
    if entity.type == "entity-ghost" or entity.type == "tile-ghost" then return true end
    local prototype_ok, items_to_place = pcall(function()
        return entity.prototype.items_to_place_this
    end)
    return prototype_ok and items_to_place ~= nil and #items_to_place > 0
end

function M.snapshot(character)
    if not (character and character.valid) then
        return {success = false, error = "no character; spawn first"}
    end
    local surface = character.surface
    local force = character.force
    local found = surface.find_entities_filtered{force = force}
    local counts_by_name = {}
    local counts_by_type = {}
    local statuses = {}
    local mining_drills = {}
    local mining_targets = {}
    local power_networks_by_id = {}
    local entity_count = 0
    local min_x, min_y, max_x, max_y = nil, nil, nil, nil

    for _, entity in pairs(found) do
        if is_factory_entity(entity) then
            entity_count = entity_count + 1
            counts_by_name[entity.name] = (counts_by_name[entity.name] or 0) + 1
            counts_by_type[entity.type] = (counts_by_type[entity.type] or 0) + 1

            local status = entity_status(entity)
            if status then statuses[status] = (statuses[status] or 0) + 1 end

            local x, y = entity.position.x, entity.position.y
            min_x = min_x and math.min(min_x, x) or x
            min_y = min_y and math.min(min_y, y) or y
            max_x = max_x and math.max(max_x, x) or x
            max_y = max_y and math.max(max_y, y) or y

            if entity.type == "mining-drill" then
                local target_ok, target = pcall(function() return entity.mining_target end)
                if target_ok and target and target.valid then
                    table.insert(mining_targets, {
                        name = target.name,
                        position = position_table(target.position),
                    })
                end
                table.insert(mining_drills, {
                    unit_number = entity.unit_number,
                    name = entity.name,
                    position = position_table(entity.position),
                    resource = target_ok and target and target.name or nil,
                    energy_source = energy_source(entity),
                    status = status,
                })
            elseif entity.type == "electric-pole" then
                local network_ok, network_id = pcall(function()
                    return entity.electric_network_id
                end)
                local key = network_ok and network_id and tostring(network_id) or "disconnected"
                local network = power_networks_by_id[key]
                if not network then
                    network = {
                        network_id = network_ok and network_id or nil,
                        pole_count = 0,
                        sample_position = position_table(entity.position),
                    }
                    power_networks_by_id[key] = network
                end
                network.pole_count = network.pole_count + 1
            end
        end
    end

    local origin = character and character.valid and character.position or {x = 0, y = 0}
    min_x = min_x or origin.x - 32
    min_y = min_y or origin.y - 32
    max_x = max_x or origin.x + 32
    max_y = max_y or origin.y + 32
    local factory_bounds = {
        left_top = {x = min_x, y = min_y},
        right_bottom = {x = max_x, y = max_y},
    }

    table.sort(mining_drills, function(a, b)
        if a.name ~= b.name then return a.name < b.name end
        if a.position.x ~= b.position.x then return a.position.x < b.position.x end
        return a.position.y < b.position.y
    end)
    local mining_drill_count = #mining_drills
    while #mining_drills > 50 do table.remove(mining_drills) end

    local power_networks = {}
    for _, network in pairs(power_networks_by_id) do
        table.insert(power_networks, network)
    end
    table.sort(power_networks, function(a, b)
        if a.pole_count ~= b.pole_count then return a.pole_count > b.pole_count end
        return tostring(a.network_id or "") < tostring(b.network_id or "")
    end)

    local margin = 8
    local blockers = entities.diagnose_factory_blockers(
        surface,
        force,
        min_x - margin,
        min_y - margin,
        max_x + margin,
        max_y + margin,
        25
    )
    local strategic = world.strategic_summary(
        surface,
        factory_bounds,
        mining_targets
    )

    return {
        tick = game.tick,
        surface = surface.name,
        character = character_snapshot(character),
        research = research.get_research_status(character),
        production = compact_production(surface.name, force),
        world = strategic.world,
        expansion = strategic.expansion,
        factory = {
            bounds = factory_bounds,
            entity_count = entity_count,
            entities_by_name = sorted_counts(counts_by_name),
            entities_by_type = sorted_counts(counts_by_type),
            statuses = sorted_counts(statuses),
            mining_drill_count = mining_drill_count,
            mining_drills = mining_drills,
            mining_drills_truncated = mining_drill_count > #mining_drills,
            power_networks = power_networks,
            blockers = blockers,
        },
    }
end

return M
