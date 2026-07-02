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

local function inventory_total(inv)
    if not inv then return nil end
    local ok, count = pcall(function() return inv.get_item_count() end)
    if ok then return count end
    return nil
end

local function safe_fuel_inventory(entity)
    local ok, inv = pcall(function() return entity.get_fuel_inventory() end)
    if ok then return inv end
    return nil
end

local function safe_inventory(entity, inventory_define)
    local ok, inv = pcall(function() return entity.get_inventory(inventory_define) end)
    if ok then return inv end
    return nil
end

local function add_action(blocker, action)
    blocker.actions = blocker.actions or {}
    table.insert(blocker.actions, action)
end

local function blocker_priority(status, entity)
    if entity.name == "boiler" and status == "no_fuel" then return 10 end
    if status == "no_power" then return 20 end
    if status == "no_fuel" then return 30 end
    if status == "no_ingredients" then return 40 end
    if status == "waiting_for_space_in_destination" then return 50 end
    if status == "no_research_in_progress" then return 60 end
    return 90
end

local function status_message(status, entity)
    if entity.name == "boiler" and status == "no_fuel" then
        return "Boiler has no fuel; downstream electric entities may report no_power until it is fueled."
    end
    if status == "no_power" then return "Entity has no electric power." end
    if status == "no_fuel" then return "Entity has no burnable fuel." end
    if status == "no_ingredients" then return "Entity has no input ingredients." end
    if status == "waiting_for_space_in_destination" then return "Entity output is blocked or backed up." end
    if status == "no_research_in_progress" then return "Lab is idle because no research is selected." end
    return "Entity is not working: " .. tostring(status)
end

local function enrich_actions(blocker, entity, status)
    if status == "no_fuel" then
        add_action(blocker, {
            type = "insert_fuel",
            tool = "insert_items",
            unit_number = entity.unit_number,
            inventory_type = "fuel",
            item = "coal",
            count = 5,
            description = "Insert coal or another fuel into unit " .. tostring(entity.unit_number) .. ".",
        })
    elseif status == "no_ingredients" then
        if entity.type == "furnace" then
            add_action(blocker, {
                type = "insert_furnace_source",
                tool = "insert_items",
                unit_number = entity.unit_number,
                inventory_type = "furnace_source",
                item = "iron-ore",
                count = 10,
                description = "Insert ore into the furnace source inventory, or connect an input belt/inserter.",
            })
        else
            add_action(blocker, {
                type = "provide_ingredients",
                description = "Inspect recipe/input inventory and provide missing ingredients.",
            })
        end
    elseif status == "waiting_for_space_in_destination" then
        add_action(blocker, {
            type = "clear_output",
            description = "Clear or extend the output belt/chest/tile, then verify production again.",
        })
    elseif status == "no_power" then
        add_action(blocker, {
            type = "diagnose_power",
            tool = "get_power_status",
            description = "Check power coverage and generation near this entity.",
        })
    elseif status == "no_research_in_progress" then
        add_action(blocker, {
            type = "start_research",
            tool = "start_research",
            description = "Start an available research if science production is ready.",
        })
    end
end

local function add_inventory_clues(blocker, entity)
    local fuel_inv = safe_fuel_inventory(entity)
    local fuel_count = inventory_total(fuel_inv)
    if fuel_count ~= nil then blocker.fuel_count = fuel_count end

    if entity.type == "furnace" then
        local source = safe_inventory(entity, defines.inventory.furnace_source)
        local result = safe_inventory(entity, defines.inventory.furnace_result)
        blocker.furnace_source_count = inventory_total(source)
        blocker.furnace_result_count = inventory_total(result)
    elseif entity.type == "lab" then
        blocker.lab_input_count = inventory_total(safe_inventory(entity, defines.inventory.lab_input))
    end
end

local function summarize_power_cause(blockers, boilers)
    local has_no_power = false
    for _, blocker in ipairs(blockers) do
        if blocker.status == "no_power" then
            has_no_power = true
            break
        end
    end
    if not has_no_power then return nil end

    local empty_boilers = {}
    for _, boiler in ipairs(boilers) do
        if boiler.fuel_count == 0 then
            table.insert(empty_boilers, boiler)
        end
    end
    if #empty_boilers > 0 then
        return {
            type = "unfueled_boiler",
            severity = "critical",
            message = "One or more boilers in the scan area have no fuel; fix this before debugging downstream no_power entities.",
            primary_unit_number = empty_boilers[1].unit_number,
            actions = {{
                type = "insert_fuel",
                tool = "insert_items",
                unit_number = empty_boilers[1].unit_number,
                inventory_type = "fuel",
                item = "coal",
                count = 5,
                description = "Fuel boiler unit " .. tostring(empty_boilers[1].unit_number) .. " and rerun diagnose_factory_blockers.",
            }},
        }
    end

    return {
        type = "no_power_cause_unknown",
        severity = "warning",
        message = "No-power entities found, but no unfueled boiler was detected in this scan area. Check pole coverage or generation outside the radius.",
        actions = {{
            type = "inspect_power",
            tool = "get_power_status",
            description = "Run get_power_status near the no_power entity and expand the scan radius if needed.",
        }},
    }
end

function M.diagnose_factory_blockers(x1, y1, x2, y2, limit)
    limit = limit or 10
    local area = area_table(x1, y1, x2, y2)
    local found = game.surfaces[1].find_entities_filtered{
        area = area,
        force = game.forces.player,
    }
    local blockers = {}
    local boilers = {}
    local scanned = 0

    for _, entity in pairs(found) do
        local status_value = raw_entity_status(entity)
        if status_value ~= nil then
            scanned = scanned + 1
            local status = status_name(status_value)
            local working = status_value == defines.entity_status.working
            local fuel_inv = safe_fuel_inventory(entity)
            local fuel_count = inventory_total(fuel_inv)
            if entity.name == "boiler" then
                table.insert(boilers, {
                    unit_number = entity.unit_number,
                    position = pos_table(entity.position),
                    fuel_count = fuel_count,
                    status = status,
                })
            end

            if not working then
                local blocker = {
                    rank = 0,
                    priority = blocker_priority(status, entity),
                    unit_number = entity.unit_number,
                    name = entity.name,
                    type = entity.type,
                    position = pos_table(entity.position),
                    status = status,
                    working = false,
                    message = status_message(status, entity),
                }
                add_inventory_clues(blocker, entity)
                enrich_actions(blocker, entity, status)
                table.insert(blockers, blocker)
            end
        end
    end

    table.sort(blockers, function(a, b)
        if a.priority ~= b.priority then return a.priority < b.priority end
        return tostring(a.unit_number or "") < tostring(b.unit_number or "")
    end)

    local truncated = false
    if #blockers > limit then
        truncated = true
        while #blockers > limit do table.remove(blockers) end
    end
    for index, blocker in ipairs(blockers) do blocker.rank = index end

    local root_cause = summarize_power_cause(blockers, boilers)
    local suggested_actions = {}
    if root_cause and root_cause.actions then
        for _, action in ipairs(root_cause.actions) do table.insert(suggested_actions, action) end
    elseif #blockers > 0 and blockers[1].actions then
        for _, action in ipairs(blockers[1].actions) do table.insert(suggested_actions, action) end
    end

    return {
        area = {left_top = {x = math.min(x1, x2), y = math.min(y1, y2)}, right_bottom = {x = math.max(x1, x2), y = math.max(y1, y2)}},
        scanned_entities = scanned,
        blocker_count = #blockers,
        blockers = blockers,
        root_cause = root_cause,
        suggested_actions = suggested_actions,
        truncated = truncated,
        guidance = "Handle rank 1 or root_cause first, then rerun diagnose_factory_blockers and verify_production.",
    }
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
