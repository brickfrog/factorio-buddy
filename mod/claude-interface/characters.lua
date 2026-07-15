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

local function area_table(area)
    if not area then return nil end
    return {
        left_top = {x = area[1][1], y = area[1][2]},
        right_bottom = {x = area[2][1], y = area[2][2]},
    }
end

local function prototype_collision_area(entity_name, position, margin)
    local proto = prototypes.entity[entity_name]
    margin = margin or 0.02
    if not (proto and proto.collision_box) then
        return {
            {position.x - 0.3 - margin, position.y - 0.3 - margin},
            {position.x + 0.3 + margin, position.y + 0.3 + margin},
        }
    end

    local cb = proto.collision_box
    return {
        {position.x + cb.left_top.x - margin, position.y + cb.left_top.y - margin},
        {position.x + cb.right_bottom.x + margin, position.y + cb.right_bottom.y + margin},
    }
end

local function boxes_overlap_area(area, box)
    if not (area and box) then return false end
    return area[1][1] < box.right_bottom.x
        and area[2][1] > box.left_top.x
        and area[1][2] < box.right_bottom.y
        and area[2][2] > box.left_top.y
end

local function collision_box_for_entity(entity, margin)
    if not (entity and entity.valid) then return nil end
    local box = entity.bounding_box
    if not box then return nil end
    if box.left_top.x == box.right_bottom.x or box.left_top.y == box.right_bottom.y then
        return nil
    end
    margin = margin or 0.02
    return {
        left_top = {
            x = box.left_top.x - margin,
            y = box.left_top.y - margin,
        },
        right_bottom = {
            x = box.right_bottom.x + margin,
            y = box.right_bottom.y + margin,
        },
    }
end

local function same_collision_layers(left, right)
    local left_layers = (left and left.layers) or {}
    local right_layers = (right and right.layers) or {}
    for layer in pairs(left_layers) do
        if not right_layers[layer] then return false end
    end
    for layer in pairs(right_layers) do
        if not left_layers[layer] then return false end
    end
    return true
end

-- Mirror Factorio's entity-to-entity CollisionMask semantics. This keeps
-- standing diagnostics correct for every prototype (including modded ones)
-- instead of maintaining an entity-type exclusion list.
local function entity_prototypes_collide(left, right)
    local left_mask = left and left.collision_mask
    local right_mask = right and right.collision_mask
    if not (left_mask and right_mask) then return false end
    if left_mask.colliding_with_tiles_only or right_mask.colliding_with_tiles_only then
        return false
    end
    if left_mask.not_colliding_with_itself
        and right_mask.not_colliding_with_itself
        and same_collision_layers(left_mask, right_mask)
    then
        return false
    end
    local right_layers = right_mask.layers or {}
    for layer in pairs(left_mask.layers or {}) do
        if right_layers[layer] then return true end
    end
    return false
end

local function entity_blocker_summary(entity, box)
    return {
        unit_number = entity.unit_number,
        name = entity.name,
        type = entity.type,
        entity_type = entity.type,
        position = pos_table(entity.position),
        direction = entity.direction,
        force = entity.force and entity.force.name or nil,
        bounding_box = box and {
            left_top = {x = box.left_top.x, y = box.left_top.y},
            right_bottom = {x = box.right_bottom.x, y = box.right_bottom.y},
        } or nil,
    }
end

local function stand_blockers(character, position)
    local area = prototype_collision_area("character", position, 0.02)
    local blockers = {}
    for _, entity in pairs(character.surface.find_entities_filtered{area = area}) do
        if entity.valid
            and entity ~= character
            and entity_prototypes_collide(character.prototype, entity.prototype)
        then
            local box = collision_box_for_entity(entity, 0.02)
            if box and boxes_overlap_area(area, box) then
                table.insert(blockers, entity_blocker_summary(entity, box))
                if #blockers >= 12 then break end
            end
        end
    end
    return blockers, area
end

local function can_stand_result(character, x, y)
    local position = {x = x, y = y}
    local blockers, area = stand_blockers(character, position)
    local current_distance = math.sqrt(
        (x - character.position.x) * (x - character.position.x)
            + (y - character.position.y) * (y - character.position.y)
    )
    local factorio_ok = true
    local factorio_error = nil
    if current_distance > 0.2 then
        local ok, can_place_or_error = pcall(function()
            return character.surface.can_place_entity{
                name = "character",
                position = {x, y},
                force = character.force,
                build_check_type = defines.build_check_type.manual,
            }
        end)
        factorio_ok = ok and can_place_or_error == true
        if not ok then factorio_error = tostring(can_place_or_error) end
    end
    return {
        can_stand = #blockers == 0 and factorio_ok,
        factorio_can_place_character = factorio_ok,
        factorio_error = factorio_error,
        position = position,
        checked_area = area_table(area),
        blockers = blockers,
        blocker_count = #blockers,
    }
end

local function nearby_stand_candidates(character, center, radius, limit)
    radius = math.max(1, math.min(12, math.floor(radius or 6)))
    limit = math.max(1, math.min(20, math.floor(limit or 8)))
    local candidates = {}
    local seen = {}
    for r = 1, radius do
        for dx = -r, r do
            for dy = -r, r do
                if math.abs(dx) == r or math.abs(dy) == r then
                    local x = math.floor(center.x) + dx + 0.5
                    local y = math.floor(center.y) + dy + 0.5
                    local key = tostring(x) .. "," .. tostring(y)
                    if not seen[key] then
                        seen[key] = true
                        local result = can_stand_result(character, x, y)
                        if result.can_stand then
                            result.distance = math.sqrt((x - center.x) * (x - center.x) + (y - center.y) * (y - center.y))
                            table.insert(candidates, result)
                        end
                    end
                end
            end
        end
    end
    table.sort(candidates, function(a, b)
        if a.distance == b.distance then
            if a.position.x == b.position.x then
                return a.position.y < b.position.y
            end
            return a.position.x < b.position.x
        end
        return a.distance < b.distance
    end)
    local returned = {}
    for i = 1, math.min(#candidates, limit) do
        table.insert(returned, candidates[i])
    end
    return returned, #candidates
end

local function live_state_entity_counts(character)
    local counts = {}
    local found = character.surface.find_entities_filtered{
        force = character.force,
        name = LIVE_STATE_ENTITY_NAMES,
    }
    for _, entity in pairs(found) do
        counts[entity.name] = (counts[entity.name] or 0) + 1
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

local function is_player_character(character)
    if not (character and character.valid) then return false end
    for _, player in pairs(game.players) do
        if player.character == character then return true end
    end
    return false
end

function M.find(agent_id)
    if type(agent_id) ~= "string" or agent_id == "" then return nil end
    if not storage.characters then return nil end
    local character = storage.characters[agent_id]
    if character and character.valid and not is_player_character(character) then
        return character
    end
    storage.characters[agent_id] = nil
    return nil
end

function M.remember(agent_id, character)
    storage.characters = storage.characters or {}
    storage.factorioctl_entities = storage.factorioctl_entities or {}
    if type(agent_id) ~= "string" or agent_id == "" then return false end
    if not (character and character.valid) or is_player_character(character) then
        storage.characters[agent_id] = nil
        return false
    end
    storage.characters[agent_id] = character
    if character.unit_number then
        storage.factorioctl_entities[character.unit_number] = character
    end
    return true
end

function M.register(agent_id, character)
    return M.remember(agent_id, character)
end

local function position_distance(character, x, y)
    local dx = x - character.position.x
    local dy = y - character.position.y
    return math.sqrt(dx * dx + dy * dy)
end

local function reach_limit(character, reach_kind)
    if reach_kind == "build" then
        return character.build_distance or character.reach_distance or 0
    end
    if reach_kind == "resource" then
        return character.resource_reach_distance or character.reach_distance or 0
    end
    return character.reach_distance or 0
end

local function out_of_reach(character, target, distance, max_distance, unit_number)
    return {
        success = false,
        error = "target is out of character reach",
        error_kind = "out_of_reach",
        action_needed = "walk_to",
        surface = character.surface.name,
        character_position = pos_table(character.position),
        target_position = pos_table(target),
        distance = distance,
        max_distance = max_distance,
        unit_number = unit_number,
    }
end

function M.require_position_reach(character, x, y, reach_kind)
    if not (character and character.valid) then
        return {
            success = false,
            error = "no valid character",
            error_kind = "no_character",
            action_needed = "spawn_character",
        }
    end
    local distance = position_distance(character, x, y)
    local max_distance = reach_limit(character, reach_kind)
    if distance <= max_distance then return nil end
    return out_of_reach(character, {x = x, y = y}, distance, max_distance, nil)
end

function M.position_reach_status(character, x, y, reach_kind)
    local reach_error = M.require_position_reach(character, x, y, reach_kind)
    if reach_error then
        reach_error.reachable = false
        if character and character.valid then
            reach_error.walk_arrival_distance = M.walk_arrival_distance(character, 0)
        end
        if reach_error.error_kind == "out_of_reach" then reach_error.success = true end
        return reach_error
    end
    local distance = position_distance(character, x, y)
    return {
        success = true,
        reachable = true,
        character_position = pos_table(character.position),
        target_position = {x = x, y = y},
        distance = distance,
        max_distance = reach_limit(character, reach_kind),
        walk_arrival_distance = M.walk_arrival_distance(character, 0),
        reach_kind = reach_kind or "interact",
    }
end

function M.require_entity_reach(character, entity)
    if not (character and character.valid) then
        return {
            success = false,
            error = "no valid character",
            error_kind = "no_character",
            action_needed = "spawn_character",
        }
    end
    if not (entity and entity.valid) then
        return {
            success = false,
            error = "entity not found",
            error_kind = "entity_not_found",
        }
    end
    if entity.surface ~= character.surface then
        return {
            success = false,
            error = "entity is on a different surface",
            error_kind = "wrong_surface",
            action_needed = "travel_to_surface",
            surface = character.surface.name,
            entity_surface = entity.surface.name,
            unit_number = entity.unit_number,
            target_position = pos_table(entity.position),
        }
    end
    local ok, reachable = pcall(function() return character.can_reach_entity(entity) end)
    if ok and reachable == true then return nil end
    local distance = position_distance(character, entity.position.x, entity.position.y)
    return out_of_reach(
        character,
        entity.position,
        distance,
        character.reach_distance or 0,
        entity.unit_number
    )
end

function M.entity_reach_status(character, entity)
    local reach_error = M.require_entity_reach(character, entity)
    if reach_error then
        reach_error.reachable = false
        if character and character.valid then
            reach_error.walk_arrival_distance = M.walk_arrival_distance(character, 0)
        end
        if reach_error.error_kind == "out_of_reach" then
            -- The reach query itself succeeded; the entity is simply not yet
            -- reachable. Preserve the structured movement guidance.
            reach_error.success = true
        end
        return reach_error
    end
    local distance = position_distance(character, entity.position.x, entity.position.y)
    return {
        success = true,
        reachable = true,
        unit_number = entity.unit_number,
        character_position = pos_table(character.position),
        target_position = pos_table(entity.position),
        distance = distance,
        max_distance = character.reach_distance or 0,
        walk_arrival_distance = M.walk_arrival_distance(character, 0),
    }
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
    local character = M.find(agent_id)
    if character and character.valid then
        -- Buddy startup is idempotent. Never move an established NPC back to
        -- the configured spawn surface; explicit game travel is a separate act.
        M.remember(agent_id, character)
        return "already_placed"
    end

    local target_surface = game.surfaces[planet_name]
    if not target_surface then return "surface_not_found" end

    target_surface.request_to_generate_chunks({spawn_x, 0}, 4)
    target_surface.force_generate_chunk_requests()

    local spawn_position = target_surface.find_non_colliding_position(
        "character",
        {spawn_x, 0},
        32,
        0.5
    )
    if not spawn_position then return "creation_failed" end

    local status = nil
    character = target_surface.create_entity{
        name = "character",
        position = spawn_position,
        force = game.forces.player,
    }
    if character then status = "created" end

    if character and character.valid and M.remember(agent_id, character) then
        return status
    end

    return "creation_failed"
end

function M.pre_place_result(agent_id, planet_name, spawn_x)
    local status = M.pre_place(agent_id, planet_name, spawn_x)
    local character = M.find(agent_id)
    return helpers.table_to_json({
        agent_name = agent_id,
        requested_planet = planet_name,
        planet = character and character.valid and character.surface.name or nil,
        position = character and character.valid and pos_table(character.position) or nil,
        status = status,
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

local function requested_walk_arrival_distance(value)
    value = tonumber(value)
    if not value or value < 0 or value ~= value or value == math.huge then return 0 end
    return math.min(value, 64)
end

local function effective_walk_arrival_distance(character, requested)
    local speed = character and character.valid and character.character_running_speed or 0.15
    return math.max(0.2, speed * 1.5, requested_walk_arrival_distance(requested))
end

function M.walk_arrival_distance(character, requested)
    return effective_walk_arrival_distance(character, requested)
end

function M.finish_walk(agent_id, character, reason)
    storage.walk_targets = storage.walk_targets or {}
    storage.walk_results = storage.walk_results or {}
    local target = storage.walk_targets[agent_id]
    if not target then
        if storage.walk_state then storage.walk_state[agent_id] = nil end
        if character and character.valid then character.walking_state = {walking = false} end
        return nil
    end

    local final_position = character and character.valid and pos_table(character.position) or nil
    local remaining_distance = nil
    if final_position then
        local dx = target.x - final_position.x
        local dy = target.y - final_position.y
        remaining_distance = math.sqrt(dx * dx + dy * dy)
    end
    local arrival_distance = effective_walk_arrival_distance(
        character,
        target.requested_arrival_distance
    )
    local result = {
        success = true,
        walk_id = target.walk_id,
        active = false,
        arrived = reason == "arrived",
        reason = reason,
        target = {x = target.x, y = target.y},
        final_position = final_position,
        remaining_distance = remaining_distance,
        arrival_distance = arrival_distance,
        distance_walked = target.distance_walked or 0,
        started_position = target.started_position,
        started_tick = target.started_tick,
        finished_tick = game.tick,
    }
    storage.walk_results[agent_id] = result
    storage.walk_targets[agent_id] = nil
    if storage.walk_state then storage.walk_state[agent_id] = nil end
    if character and character.valid then
        M.remember(agent_id, character)
        character.walking_state = {walking = false}
    end
    return result
end

function M.get_walk_status(agent_id, walk_id)
    storage.walk_targets = storage.walk_targets or {}
    storage.walk_results = storage.walk_results or {}
    walk_id = tonumber(walk_id)
    local target = storage.walk_targets[agent_id]
    local result = storage.walk_results[agent_id]

    if target and (walk_id == nil or target.walk_id == walk_id) then
        local character = M.find(agent_id)
        local final_position = character and character.valid and pos_table(character.position) or nil
        local remaining_distance = nil
        if final_position then
            local dx = target.x - final_position.x
            local dy = target.y - final_position.y
            remaining_distance = math.sqrt(dx * dx + dy * dy)
        end
        local arrival_distance = effective_walk_arrival_distance(
            character,
            target.requested_arrival_distance
        )
        target.arrival_distance = arrival_distance
        return {
            success = true,
            walk_id = target.walk_id,
            active = true,
            arrived = false,
            reason = "walking",
            target = {x = target.x, y = target.y},
            final_position = final_position,
            remaining_distance = remaining_distance,
            arrival_distance = arrival_distance,
            distance_walked = target.distance_walked or 0,
            started_position = target.started_position,
            started_tick = target.started_tick,
        }
    end

    if result and (walk_id == nil or result.walk_id == walk_id) then return result end
    return {
        success = true,
        walk_id = walk_id,
        active = false,
        arrived = false,
        reason = (walk_id ~= nil and (target or result)) and "superseded" or "idle",
        target = nil,
        final_position = nil,
        remaining_distance = nil,
        arrival_distance = nil,
        distance_walked = 0,
    }
end

function M.set_walk_target(agent_id, x, y, arrival_distance)
    local character = M.find(agent_id)
    if not (character and character.valid) then
        return {
            success = false,
            error = "no character for agent " .. tostring(agent_id) .. "; spawn first",
        }
    end

    M.remember(agent_id, character)
    storage.walk_targets = storage.walk_targets or {}
    storage.walk_results = storage.walk_results or {}
    storage.walk_sequence = storage.walk_sequence or 0
    if storage.walk_targets[agent_id] then
        M.finish_walk(agent_id, character, "superseded")
    end
    storage.walk_sequence = storage.walk_sequence + 1
    if storage.walk_state then storage.walk_state[agent_id] = nil end
    storage.walk_targets[agent_id] = {
        walk_id = storage.walk_sequence,
        x = x,
        y = y,
        requested_arrival_distance = requested_walk_arrival_distance(arrival_distance),
        arrival_distance = effective_walk_arrival_distance(character, arrival_distance),
        distance_walked = 0,
        stuck_ticks = 0,
        expires_tick = game.tick + 7200,
        last_x = character.position.x,
        last_y = character.position.y,
        started_position = pos_table(character.position),
        started_tick = game.tick,
    }
    character.walking_state = {walking = false}
    return M.get_walk_status(agent_id, storage.walk_sequence)
end

function M.clear_walk_target(agent_id, walk_id)
    walk_id = tonumber(walk_id)
    local character = M.find(agent_id)
    local target = storage.walk_targets and storage.walk_targets[agent_id] or nil
    if target then
        if walk_id ~= nil and target.walk_id ~= walk_id then
            return M.get_walk_status(agent_id, walk_id)
        end
        return M.finish_walk(agent_id, character, "cancelled")
    end
    if storage.walk_state then storage.walk_state[agent_id] = nil end
    if character and character.valid then character.walking_state = {walking = false} end
    return M.get_walk_status(agent_id, walk_id)
end

function M.init(agent_id, x, y)
    local character = M.find(agent_id)
    if not (character and character.valid) then
        local surface = game.get_surface("nauvis")
        if not surface then
            for _, candidate in pairs(game.surfaces) do
                surface = candidate
                break
            end
        end
        if not surface then return {error = "No surface available for character creation"} end
        character = surface.create_entity{
            name = "character",
            position = {x, y},
            force = game.forces.player,
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

function M.can_stand_at(agent_id, x, y, radius)
    local character = M.find(agent_id)
    if not (character and character.valid) then
        return {
            success = false,
            error = "no character for agent " .. tostring(agent_id) .. "; spawn first",
            can_stand = false,
            blockers = {"no_character"},
        }
    end

    local result = can_stand_result(character, x, y)
    result.success = true
    result.surface = character.surface.name
    result.character_position = pos_table(character.position)
    if not result.can_stand then
        local candidates, total = nearby_stand_candidates(character, {x = x, y = y}, radius or 6, 8)
        result.unstuck_candidates = candidates
        result.total_unstuck_candidates = total
        if #candidates > 0 then
            result.recommended_action = "walk_to"
            result.walk_to_clear_position = candidates[1].position
        else
            result.recommended_action = "clear_blockers_or_teleport"
        end
    end
    return result
end

function M.is_player_blocked(agent_id, radius)
    local character = M.find(agent_id)
    if not (character and character.valid) then
        return {
            success = false,
            error = "no character for agent " .. tostring(agent_id) .. "; spawn first",
            blocked = true,
            blockers = {"no_character"},
        }
    end

    local result = can_stand_result(character, character.position.x, character.position.y)
    local candidates, total = nearby_stand_candidates(character, character.position, radius or 6, 8)
    return {
        success = true,
        surface = character.surface.name,
        position = pos_table(character.position),
        checked_area = result.checked_area,
        blocked = not result.can_stand,
        can_stand_at_current_position = result.can_stand,
        blockers = result.blockers,
        blocker_count = result.blocker_count,
        unstuck_candidates = candidates,
        total_unstuck_candidates = total,
        recommended_action = (not result.can_stand and #candidates > 0) and "walk_to" or nil,
        walk_to_clear_position = (#candidates > 0) and candidates[1].position or nil,
    }
end

function M.unstuck(agent_id, radius, dry_run)
    local character = M.find(agent_id)
    if not (character and character.valid) then
        return {
            success = false,
            moved = false,
            error = "no character for agent " .. tostring(agent_id) .. "; spawn first",
            blockers = {"no_character"},
        }
    end

    local before = pos_table(character.position)
    local current = can_stand_result(character, character.position.x, character.position.y)
    local candidates, total = nearby_stand_candidates(character, character.position, radius or 8, 12)
    local result = {
        success = true,
        moved = false,
        dry_run = dry_run == true,
        surface = character.surface.name,
        from = before,
        blocked = not current.can_stand,
        can_stand_at_current_position = current.can_stand,
        blockers = current.blockers,
        blocker_count = current.blocker_count,
        unstuck_candidates = candidates,
        total_unstuck_candidates = total,
    }

    if current.can_stand then
        result.reason = "character already has a clear standing footprint"
        result.recommended_action = "none"
        return result
    end

    if #candidates == 0 then
        result.success = false
        result.error = "no nearby clear standing position found"
        result.recommended_action = "clear_blockers_or_expand_radius"
        return result
    end

    local target = candidates[1].position
    result.to = target
    result.recommended_action = "walk_to"
    if dry_run == true then
        result.reason = "dry_run"
        return result
    end

    local started = M.set_walk_target(agent_id, target.x, target.y)
    if not started.success then
        result.success = false
        result.error = started.error or "could not start movement to clear standing position"
        result.position = pos_table(character.position)
        return result
    end

    character.mining_state = {mining = false}
    result.action_started = true
    result.action_needed = "wait_for_walk"
    result.reason = "walking to nearest verified clear standing position"
    return result
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
