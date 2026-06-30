local M = {}

local function pos_table(pos)
    if not pos then return nil end
    return {x = pos.x, y = pos.y}
end

local function area_table(x1, y1, x2, y2)
    return {{x1, y1}, {x2, y2}}
end

local function bounding_box_table(bb)
    if not bb then return nil end
    return {
        left_top = pos_table(bb.left_top),
        right_bottom = pos_table(bb.right_bottom),
    }
end

local function status_name(status_value)
    if status_value == nil then return nil end
    for name, value in pairs(defines.entity_status) do
        if value == status_value then return name end
    end
    return tostring(status_value)
end

local function raw_entity_status(entity)
    local ok, value = pcall(function() return entity.status end)
    if ok then return value end
    return nil
end

function M.find_by_unit_number(unit_number)
    storage.factorioctl_entities = storage.factorioctl_entities or {}
    local registered = storage.factorioctl_entities[unit_number]
    if registered and registered.valid then return registered end
    storage.factorioctl_entities[unit_number] = nil

    for _, surface in pairs(game.surfaces) do
        local found = surface.find_entities_filtered{area = {{-500, -500}, {500, 500}}}
        for _, entity in pairs(found) do
            if entity.unit_number == unit_number then
                storage.factorioctl_entities[unit_number] = entity
                return entity
            end
        end
    end
    return nil
end

function M.summary(entity, include_bounding_box)
    local result = {
        unit_number = entity.unit_number,
        name = entity.name,
        type = entity.type,
        position = pos_table(entity.position),
        direction = entity.direction,
        health = entity.health,
        force = entity.force and entity.force.name or nil,
    }

    if include_bounding_box then
        result.bounding_box = bounding_box_table(entity.bounding_box)
    end

    return result
end

function M.get_surfaces()
    local result = {}
    for _, surface in pairs(game.surfaces) do
        table.insert(result, {
            name = surface.name,
            index = surface.index,
            daytime = surface.daytime,
            darkness = surface.darkness,
        })
    end
    return result
end

function M.find_entities(x1, y1, x2, y2, entity_type, name)
    local filters = {area = area_table(x1, y1, x2, y2)}
    if entity_type then filters.type = entity_type end
    if name then filters.name = name end

    local result = {}
    for _, entity in pairs(game.surfaces[1].find_entities_filtered(filters)) do
        table.insert(result, M.summary(entity, true))
    end
    return result
end

function M.verify_production(x1, y1, x2, y2)
    local result = {}
    local found = game.surfaces[1].find_entities_filtered{
        area = area_table(x1, y1, x2, y2),
        force = game.forces.player,
    }

    for _, entity in pairs(found) do
        local status_value = raw_entity_status(entity)
        if status_value ~= nil then
            local products_finished = nil
            local products_ok, products_value = pcall(function()
                return entity.products_finished
            end)
            if products_ok then
                products_finished = products_value
            end

            table.insert(result, {
                name = entity.name,
                position = pos_table(entity.position),
                status = status_name(status_value),
                products_finished = products_finished,
                working = status_value == defines.entity_status.working,
            })
        end
    end

    return result
end

function M.get_entity(unit_number)
    local entity = M.find_by_unit_number(unit_number)
    if not entity then return nil end
    return M.summary(entity, false)
end

function M.get_drop_position(unit_number)
    local entity = M.find_by_unit_number(unit_number)
    if not entity then
        return {error = "Entity not found or has no drop_position"}
    end
    if not entity.drop_position then
        return {error = "Entity not found or has no drop_position"}
    end

    local drop_position = entity.drop_position
    local direction = entity.direction
    return {
        drop_x = drop_position.x,
        drop_y = drop_position.y,
        drill_direction = direction,
        belt_direction = direction,
    }
end

return M
