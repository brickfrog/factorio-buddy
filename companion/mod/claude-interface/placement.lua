local characters = require("characters")

local M = {}

local function pos_table(pos)
    if not pos then return nil end
    return {x = pos.x, y = pos.y}
end

local function placement_entity_result(entity)
    return {
        unit_number = entity.unit_number,
        name = entity.name,
        type = entity.type,
        entity_type = entity.type,
        position = pos_table(entity.position),
        direction = entity.direction,
        health = entity.health,
        force = entity.force and entity.force.name or nil,
    }
end

local function placement_failure(entity_name, position, direction, inventory_count, can_place, error)
    return {
        success = false,
        error = error,
        entity = entity_name,
        position = {x = position[1], y = position[2]},
        direction = direction,
        inventory_count = inventory_count,
        can_place = can_place,
    }
end

local function clear_ground_items_for_placement(character, surface, entity_name, position)
    local proto = prototypes.entity[entity_name]
    if not (proto and proto.collision_box) then return end

    local cb = proto.collision_box
    local clear_area = {
        {position[1] + cb.left_top.x - 0.1, position[2] + cb.left_top.y - 0.1},
        {position[1] + cb.right_bottom.x + 0.1, position[2] + cb.right_bottom.y + 0.1},
    }
    local items_on_ground = surface.find_entities_filtered{
        area = clear_area,
        type = "item-entity",
    }
    for _, item in pairs(items_on_ground) do
        local stack = item.stack
        if stack and stack.valid_for_read then
            local before_count = stack.count
            local inserted = character.insert(stack)
            if inserted > 0 then
                if inserted >= before_count then
                    item.destroy()
                else
                    stack.count = before_count - inserted
                end
            else
                item.destroy()
            end
        else
            item.destroy()
        end
    end
end

function M.place_entity(agent_id, entity_name, x, y, direction)
    local character = characters.find(agent_id)
    if not (character and character.valid) then
        return placement_failure(entity_name, {x, y}, direction, 0, false, "no character for agent " .. tostring(agent_id) .. "; spawn first")
    end

    local inv = character.get_main_inventory()
    local inventory_count = 0
    if inv then inventory_count = inv.get_item_count(entity_name) end
    local position = {x, y}
    if not inv or inventory_count < 1 then
        return placement_failure(entity_name, position, direction, inventory_count, false, "Item not in inventory")
    end

    if not prototypes.entity[entity_name] then
        return placement_failure(entity_name, position, direction, inventory_count, false, "Unknown entity prototype")
    end

    local surface = character.surface
    clear_ground_items_for_placement(character, surface, entity_name, position)

    local can_place_ok, can_place_or_error = pcall(function()
        return surface.can_place_entity{
            name = entity_name,
            position = position,
            direction = direction,
            force = character.force,
            build_check_type = defines.build_check_type.manual,
        }
    end)

    if not can_place_ok or can_place_or_error ~= true then
        return placement_failure(
            entity_name,
            position,
            direction,
            inventory_count,
            false,
            can_place_ok and "Cannot place entity here" or tostring(can_place_or_error)
        )
    end

    local create_ok, created_or_error = pcall(function()
        return surface.create_entity{
            name = entity_name,
            position = position,
            direction = direction,
            force = character.force,
        }
    end)

    if not create_ok then
        return placement_failure(entity_name, position, direction, inventory_count, true, tostring(created_or_error))
    end

    local entity = created_or_error
    if not entity then
        return placement_failure(
            entity_name,
            position,
            direction,
            inventory_count,
            true,
            "create_entity returned nil after can_place_entity succeeded"
        )
    end

    if entity.unit_number then
        storage.factorioctl_entities = storage.factorioctl_entities or {}
        storage.factorioctl_entities[entity.unit_number] = entity
    end
    inv.remove{name = entity_name, count = 1}
    return placement_entity_result(entity)
end

function M.place_underground_belt(agent_id, entity_name, x, y, direction, belt_type)
    local character = characters.find(agent_id)
    if not (character and character.valid) then
        return {error = "no character for agent " .. tostring(agent_id) .. "; spawn first"}
    end

    local inv = character.get_main_inventory()
    local inventory_count = 0
    if inv then inventory_count = inv.get_item_count(entity_name) end
    if not inv or inventory_count < 1 then
        return {error = "Item not in inventory"}
    end

    local position = {x, y}
    local surface = character.surface
    local can_place = surface.can_place_entity{
        name = entity_name,
        position = position,
        direction = direction,
        force = character.force,
        build_check_type = defines.build_check_type.manual,
    }

    if not can_place then
        return {error = "Cannot place underground belt here"}
    end

    local entity = surface.create_entity{
        name = entity_name,
        position = position,
        direction = direction,
        type = belt_type,
        force = character.force,
    }

    if not entity then
        return {error = "Failed to create underground belt"}
    end

    if entity.unit_number then
        storage.factorioctl_entities = storage.factorioctl_entities or {}
        storage.factorioctl_entities[entity.unit_number] = entity
    end
    inv.remove{name = entity_name, count = 1}

    local result = placement_entity_result(entity)
    result.belt_to_ground_type = entity.belt_to_ground_type
    return result
end

function M.check_entity_placement(agent_id, entity_name, x, y, direction)
    local character = characters.find(agent_id)
    local position = {x, y}
    if not (character and character.valid) then
        return {
            factorio_allowed = false,
            entity = entity_name,
            position = {x = x, y = y},
            direction = direction,
            inventory_count = 0,
            item_in_inventory = false,
            error = "no character for agent " .. tostring(agent_id) .. "; spawn first",
        }
    end

    if not prototypes.entity[entity_name] then
        return {
            factorio_allowed = false,
            entity = entity_name,
            position = {x = x, y = y},
            direction = direction,
            inventory_count = 0,
            item_in_inventory = false,
            error = "Unknown entity prototype",
        }
    end

    local inv = character.get_main_inventory()
    local inventory_count = 0
    if inv then inventory_count = inv.get_item_count(entity_name) end

    local ok, can_place_or_error = pcall(function()
        return character.surface.can_place_entity{
            name = entity_name,
            position = position,
            direction = direction,
            force = character.force,
            build_check_type = defines.build_check_type.manual,
        }
    end)

    if not ok then
        return {
            factorio_allowed = false,
            entity = entity_name,
            position = {x = x, y = y},
            direction = direction,
            inventory_count = inventory_count,
            item_in_inventory = inventory_count > 0,
            error = tostring(can_place_or_error),
        }
    end

    local result = {
        factorio_allowed = can_place_or_error == true,
        entity = entity_name,
        position = {x = x, y = y},
        direction = direction,
        inventory_count = inventory_count,
        item_in_inventory = inventory_count > 0,
    }
    if can_place_or_error ~= true then
        result.error = "Factorio cannot place entity here"
    end
    return result
end

function M.find_entity_placements(agent_id, entity_name, center_x, center_y, radius, limit)
    local character = characters.find(agent_id)
    local center = {center_x, center_y}
    if not (character and character.valid) then
        return {
            success = false,
            error = "no character for agent " .. tostring(agent_id) .. "; spawn first",
            entity = entity_name,
            center = {x = center_x, y = center_y},
            radius = radius,
            placements = {},
        }
    end

    if not prototypes.entity[entity_name] then
        return {
            success = false,
            error = "Unknown entity prototype",
            entity = entity_name,
            center = {x = center_x, y = center_y},
            radius = radius,
            placements = {},
        }
    end

    local inv = character.get_main_inventory()
    local inventory_count = 0
    if inv then inventory_count = inv.get_item_count(entity_name) end

    local directions = {0, 4, 8, 12}
    local placements = {}
    local checked = 0
    local surface = character.surface
    for dx = -radius, radius do
        for dy = -radius, radius do
            local position = {center[1] + dx, center[2] + dy}
            for _, dir in pairs(directions) do
                checked = checked + 1
                local ok, can_place = pcall(function()
                    return surface.can_place_entity{
                        name = entity_name,
                        position = position,
                        direction = dir,
                        force = character.force,
                        build_check_type = defines.build_check_type.manual,
                    }
                end)
                if ok and can_place == true then
                    local distance = math.sqrt(dx * dx + dy * dy)
                    table.insert(placements, {
                        entity = entity_name,
                        factorio_allowed = true,
                        position = {x = position[1], y = position[2]},
                        direction = dir,
                        distance = distance,
                        inventory_count = inventory_count,
                        item_in_inventory = inventory_count > 0,
                    })
                end
            end
        end
    end

    table.sort(placements, function(a, b)
        if a.distance == b.distance then
            return a.direction < b.direction
        end
        return a.distance < b.distance
    end)

    local returned = {}
    for i = 1, math.min(#placements, limit) do
        table.insert(returned, placements[i])
    end

    return {
        success = true,
        entity = entity_name,
        center = {x = center[1], y = center[2]},
        radius = radius,
        checked = checked,
        total = #placements,
        returned = #returned,
        truncated = #placements > #returned,
        placements = returned,
    }
end

function M.place_ghost(agent_id, entity_name, x, y, direction)
    local character = characters.find(agent_id)
    if not (character and character.valid) then
        return {error = "no character for agent " .. tostring(agent_id) .. "; spawn first"}
    end

    local entity = character.surface.create_entity{
        name = "entity-ghost",
        inner_name = entity_name,
        position = {x, y},
        direction = direction,
        force = character.force,
    }

    if not entity then
        return {error = "Failed to create ghost"}
    end
    if entity.unit_number then
        storage.factorioctl_entities = storage.factorioctl_entities or {}
        storage.factorioctl_entities[entity.unit_number] = entity
    end
    local result = placement_entity_result(entity)
    result.name = entity.ghost_name or entity_name
    result.entity_type = "entity-ghost"
    result.type = "entity-ghost"
    return result
end

return M
