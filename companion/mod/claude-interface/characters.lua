local entities = require("entities")
local inventory = require("inventory")

local M = {}

local LIVE_STATE_ENTITY_NAMES = {
    "burner-mining-drill",
    "electric-mining-drill",
    "stone-furnace",
    "assembling-machine-1",
    "transport-belt",
    "burner-inserter",
    "inserter",
    "small-electric-pole",
    "medium-electric-pole",
    "offshore-pump",
    "boiler",
    "steam-engine",
    "pipe",
    "lab",
}

local function pos_table(pos)
    if not pos then return nil end
    return {x = pos.x, y = pos.y}
end

local function live_state_entity_counts(character)
    local counts = {}
    for _, name in ipairs(LIVE_STATE_ENTITY_NAMES) do
        local count = #character.surface.find_entities_filtered{force = character.force, name = name}
        if count > 0 then counts[name] = count end
    end
    return counts
end

local function live_state_entity_parts(counts)
    local parts = {}
    for _, name in ipairs(LIVE_STATE_ENTITY_NAMES) do
        local count = counts[name]
        if count and count > 0 then parts[#parts + 1] = name .. "=" .. count end
    end
    return parts
end

function M.find(agent_id)
    if storage.characters then
        local character = storage.characters[agent_id]
        if character and character.valid then return character end
        if agent_id == "default" then
            character = storage.characters["__player__"]
            if character and character.valid then return character end
        elseif agent_id == "__player__" then
            character = storage.characters["default"]
            if character and character.valid then return character end
        end
    end

    if agent_id == "default" or agent_id == "__player__" then
        for _, player in pairs(game.connected_players) do
            if player.character and player.character.valid then
                return player.character
            end
        end
    end

    return nil
end

function M.remember(agent_id, character)
    storage.characters = storage.characters or {}
    storage.factorioctl_entities = storage.factorioctl_entities or {}
    storage.characters[agent_id] = character
    if agent_id == "__player__" then storage.characters["default"] = character end
    if agent_id == "default" then storage.characters["__player__"] = character end
    if character and character.valid and character.unit_number then
        storage.factorioctl_entities[character.unit_number] = character
    end
end

function M.register(agent_id, character)
    storage.characters = storage.characters or {}
    storage.characters[agent_id] = character
end

function M.ensure_surface(planet_name)
    local planet = game.planets[planet_name]
    if not planet then return "no_planet" end
    if game.surfaces[planet_name] then return "exists" end
    planet.create_surface()
    return "created"
end

function M.ensure_surface_result(planet_name)
    return helpers.table_to_json({
        planet = planet_name,
        status = M.ensure_surface(planet_name),
    })
end

function M.pre_place(agent_id, planet_name, spawn_x)
    local target_surface = game.surfaces[planet_name]
    if not target_surface then return "surface_not_found" end

    target_surface.request_to_generate_chunks({spawn_x, 0}, 4)
    target_surface.force_generate_chunk_requests()

    local status = nil
    local character = M.find(agent_id)
    if character and character.valid then
        if character.surface.name == planet_name then
            status = "already_placed"
        else
            character.teleport({spawn_x, 0}, target_surface)
            status = "teleported"
        end
    else
        character = target_surface.create_entity{
            name = "character",
            position = {spawn_x, 0},
            force = game.forces.player,
        }
        if character then status = "created" end
    end

    if character and character.valid then
        M.remember(agent_id, character)
        return status
    end

    return "creation_failed"
end

function M.pre_place_result(agent_id, planet_name, spawn_x)
    return helpers.table_to_json({
        agent_name = agent_id,
        planet = planet_name,
        status = M.pre_place(agent_id, planet_name, spawn_x),
    })
end

function M.live_state_line(agent_id)
    local character = M.find(agent_id)
    if not (character and character.valid) then return "" end

    local parts = live_state_entity_parts(live_state_entity_counts(character))
    local summary = ""
    if #parts > 0 then summary = "; player entities: " .. table.concat(parts, ", ") end
    return "Live state: "
        .. character.surface.name
        .. " @ "
        .. string.format("%.1f,%.1f", character.position.x, character.position.y)
        .. summary
end

function M.live_state_result(agent_id)
    local character = M.find(agent_id)
    if not (character and character.valid) then
        return helpers.table_to_json({
            found = false,
            entity_counts = {},
        })
    end

    return helpers.table_to_json({
        found = true,
        surface = character.surface.name,
        x = character.position.x,
        y = character.position.y,
        entity_counts = live_state_entity_counts(character),
    })
end

function M.connected_player_count()
    return #game.connected_players
end

function M.connected_player_count_result()
    return helpers.table_to_json({
        count = M.connected_player_count(),
    })
end

function M.set_walk_target(agent_id, x, y)
    local character = M.find(agent_id)
    if not (character and character.valid) then
        return {
            success = false,
            error = "no character for agent " .. tostring(agent_id) .. "; spawn first",
        }
    end

    M.remember(agent_id, character)
    storage.walk_targets = storage.walk_targets or {}
    if storage.walk_state then storage.walk_state[agent_id] = nil end
    storage.walk_targets[agent_id] = {
        x = x,
        y = y,
        stuck_ticks = 0,
        expires_tick = game.tick + 7200,
        last_x = character.position.x,
        last_y = character.position.y,
    }
    character.walking_state = {walking = false}
    return {success = true}
end

function M.clear_walk_target(agent_id)
    if storage.walk_targets then storage.walk_targets[agent_id] = nil end
    if storage.walk_state then storage.walk_state[agent_id] = nil end
    local character = M.find(agent_id)
    if character and character.valid then
        M.remember(agent_id, character)
        character.walking_state = {walking = false}
    end
    return {success = true}
end

function M.init(agent_id, x, y)
    local character = M.find(agent_id)
    if not (character and character.valid) then
        character = game.surfaces[1].create_entity{
            name = "character",
            position = {x, y},
            force = "player",
        }
        if not character then
            return {error = "Failed to create character"}
        end
    end

    M.remember(agent_id, character)
    return entities.summary(character, false)
end

function M.teleport(agent_id, x, y)
    local character = M.find(agent_id)
    if not (character and character.valid) then
        return {error = "no character for agent " .. tostring(agent_id) .. "; spawn first"}
    end

    if character.teleport({x, y}) then
        return "ok"
    end
    return {error = "Teleport blocked (target obstructed)"}
end

function M.status(agent_id)
    local character = M.find(agent_id)
    if not (character and character.valid) then
        return {valid = false}
    end

    local walking = false
    if character.walking_state then walking = character.walking_state.walking end
    local mining = false
    if character.mining_state then mining = character.mining_state.mining end

    return {
        valid = true,
        unit_number = character.unit_number,
        position = pos_table(character.position),
        health = character.health,
        crafting_queue_size = character.crafting_queue_size,
        walking = walking,
        mining = mining,
    }
end

function M.inventory(agent_id)
    local character = M.find(agent_id)
    if not (character and character.valid) then
        return {items = {}, free_slots = 0}
    end

    local inv = character.get_main_inventory()
    if not inv then
        return {items = {}, free_slots = 0}
    end

    return {
        items = inventory.contents(inv),
        free_slots = inv.count_empty_stacks() or 0,
    }
end

return M
