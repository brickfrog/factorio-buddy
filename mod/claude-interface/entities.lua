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
    unit_number = tonumber(unit_number)
    if not unit_number then return nil end
    storage.factorioctl_entities = storage.factorioctl_entities or {}
    local registered = storage.factorioctl_entities[unit_number]
    if registered and registered.valid then return registered end
    storage.factorioctl_entities[unit_number] = nil

    local entity = game.get_entity_by_unit_number(unit_number)
    if entity and entity.valid then
        storage.factorioctl_entities[unit_number] = entity
        return entity
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
        surface = entity.surface and entity.surface.name or nil,
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

function M.find_entities(surface, x1, y1, x2, y2, entity_type, name)
    if not surface then return {error = "agent surface not found"} end
    local filters = {area = area_table(x1, y1, x2, y2)}
    if entity_type then filters.type = entity_type end
    if name then filters.name = name end

    local result = {}
    for _, entity in pairs(surface.find_entities_filtered(filters)) do
        table.insert(result, M.summary(entity, true))
    end
    return result
end

function M.verify_production(surface, force, x1, y1, x2, y2)
    if not (surface and force) then return {error = "agent surface or force not found"} end
    local result = {}
    local found = surface.find_entities_filtered{
        area = area_table(x1, y1, x2, y2),
        force = force,
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
                unit_number = entity.unit_number,
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

local function entity_status_string(entity)
    return status_name(raw_entity_status(entity))
end

local function first_inventory(entity, inventory_defines)
    for _, inventory_define in ipairs(inventory_defines) do
        local inv = safe_inventory(entity, inventory_define)
        if inv then return inv end
    end
    return nil
end

local function inventory_item_count(inv, item)
    if not inv then return nil end
    local ok, count = pcall(function() return inv.get_item_count(item) end)
    if ok then return count end
    return nil
end

local function distance_sq(a, b)
    if not a or not b then return nil end
    local dx = (a.x or 0) - (b.x or 0)
    local dy = (a.y or 0) - (b.y or 0)
    return dx * dx + dy * dy
end

local function belt_line_item_count(belt, item)
    local total = 0
    local ok = pcall(function()
        for line_index = 1, 2 do
            local line = belt.get_transport_line(line_index)
            if line then
                total = total + line.get_item_count(item)
            end
        end
    end)
    if ok then return total end
    return nil
end

local function direction_name(direction)
    if direction == defines.direction.north then return "north" end
    if direction == defines.direction.east then return "east" end
    if direction == defines.direction.south then return "south" end
    if direction == defines.direction.west then return "west" end
    return tostring(direction)
end

local function tile_coord(value)
    return math.floor(value)
end

local function route_source_position(entity)
    if entity and entity.drop_position then
        return entity.drop_position
    end
    return entity and entity.position or nil
end

local function route_source_tile(entity)
    local source = route_source_position(entity)
    if not source then return nil end
    return {x = tile_coord(source.x), y = tile_coord(source.y)}
end

local function point_in_bounding_box(point, box)
    return point and box
        and point.x >= box.left_top.x
        and point.x <= box.right_bottom.x
        and point.y >= box.left_top.y
        and point.y <= box.right_bottom.y
end

local function coal_source_record(entity)
    if entity.type == "transport-belt"
        or entity.type == "underground-belt"
        or entity.type == "splitter"
    then
        local count = belt_line_item_count(entity, "coal") or 0
        return {
            kind = "coal_belt",
            unit_number = entity.unit_number,
            name = entity.name,
            position = pos_table(entity.position),
            coal_count = count,
            operational = count > 0,
        }
    end
    if entity.type == "container" or entity.type == "logistic-container" then
        local count = inventory_item_count(first_inventory(entity, {defines.inventory.chest}), "coal") or 0
        return {
            kind = "coal_chest",
            unit_number = entity.unit_number,
            name = entity.name,
            position = pos_table(entity.position),
            coal_count = count,
            operational = count > 0,
        }
    end
    if entity.type == "mining-drill" and entity.mining_target and entity.mining_target.name == "coal" then
        local status = entity_status_string(entity)
        return {
            kind = "coal_drill",
            unit_number = entity.unit_number,
            name = entity.name,
            position = pos_table(entity.position),
            route_position = pos_table(route_source_position(entity)),
            route_tile = route_source_tile(entity),
            status = status,
            operational = status == "working" or status == "waiting_for_space_in_destination",
        }
    end
    return nil
end

local function proven_fuel_connections(surface, force, consumer)
    local box = consumer.bounding_box
    if not box then return {} end
    local search_area = {
        {box.left_top.x - 2.5, box.left_top.y - 2.5},
        {box.right_bottom.x + 2.5, box.right_bottom.y + 2.5},
    }
    local result = {}
    for _, inserter in pairs(surface.find_entities_filtered{type = "inserter", area = search_area, force = force}) do
        if point_in_bounding_box(inserter.drop_position, box) then
            local pickup = inserter.pickup_position
            local pickup_area = {{pickup.x - 0.25, pickup.y - 0.25}, {pickup.x + 0.25, pickup.y + 0.25}}
            for _, source in pairs(surface.find_entities_filtered{area = pickup_area, force = force}) do
                if source ~= inserter and point_in_bounding_box(pickup, source.bounding_box) then
                    local source_record = coal_source_record(source)
                    if source_record and source_record.operational then
                        table.insert(result, {
                            inserter_unit_number = inserter.unit_number,
                            inserter_name = inserter.name,
                            inserter_status = entity_status_string(inserter),
                            pickup_position = pos_table(pickup),
                            drop_position = pos_table(inserter.drop_position),
                            source = source_record,
                        })
                    end
                end
            end
        end
    end
    table.sort(result, function(a, b)
        return tostring(a.inserter_unit_number or "") < tostring(b.inserter_unit_number or "")
    end)
    return result
end

local function ranked_coal_sources(origin, drills, belts, chests)
    local result = {}
    local function append(kind, source)
        local copy = {}
        for key, value in pairs(source) do copy[key] = value end
        copy.kind = kind
        copy.distance_sq = distance_sq(origin, source.position) or math.huge
        if copy.operational == nil then
            copy.operational = (copy.coal_count or 0) > 0
                or copy.status == "working"
                or copy.status == "waiting_for_space_in_destination"
        end
        table.insert(result, copy)
    end
    for _, source in ipairs(drills) do append("coal_drill", source) end
    for _, source in ipairs(belts) do append("coal_belt", source) end
    for _, source in ipairs(chests) do append("coal_chest", source) end
    table.sort(result, function(a, b)
        if a.operational ~= b.operational then return a.operational == true end
        if a.distance_sq ~= b.distance_sq then return a.distance_sq < b.distance_sq end
        if a.kind ~= b.kind then return a.kind < b.kind end
        return tostring(a.unit_number or "") < tostring(b.unit_number or "")
    end)
    return result
end

local function inserter_fuel_candidates(surface, force, entity)
    local bb = entity.bounding_box
    if not bb then return {} end
    local center = entity.position
    local candidates = {
        {
            side = "north",
            inserter = {x = center.x, y = bb.left_top.y - 0.5},
            pickup = {x = center.x, y = bb.left_top.y - 1.5},
            direction = defines.direction.north,
        },
        {
            side = "east",
            inserter = {x = bb.right_bottom.x + 0.5, y = center.y},
            pickup = {x = bb.right_bottom.x + 1.5, y = center.y},
            direction = defines.direction.east,
        },
        {
            side = "south",
            inserter = {x = center.x, y = bb.right_bottom.y + 0.5},
            pickup = {x = center.x, y = bb.right_bottom.y + 1.5},
            direction = defines.direction.south,
        },
        {
            side = "west",
            inserter = {x = bb.left_top.x - 0.5, y = center.y},
            pickup = {x = bb.left_top.x - 1.5, y = center.y},
            direction = defines.direction.west,
        },
    }

    local viable = {}
    for _, candidate in ipairs(candidates) do
        local inserter_name = "burner-inserter"
        local can_place_inserter = surface.can_place_entity{
            name = inserter_name,
            position = candidate.inserter,
            direction = candidate.direction,
            force = force,
        }
        local pickup_belt_can_place = surface.can_place_entity{
            name = "transport-belt",
            position = candidate.pickup,
            force = force,
        }
        table.insert(viable, {
            side = candidate.side,
            inserter_position = candidate.inserter,
            pickup_tile = candidate.pickup,
            inserter_direction = candidate.direction,
            inserter_direction_name = direction_name(candidate.direction),
            inserter_name = inserter_name,
            can_place_inserter = can_place_inserter,
            pickup_belt_can_place = pickup_belt_can_place,
            place_inserter_args = {
                entity_name = inserter_name,
                x = candidate.inserter.x,
                y = candidate.inserter.y,
                direction = direction_name(candidate.direction),
            },
        })
    end
    return viable
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
            type = "build_durable_fuel_supply",
            tool = "diagnose_fuel_sustainability",
            description = "Locate coal production/storage and route coal to this consumer with belts and inserters. Do not hand-feed it.",
        })
    elseif status == "no_ingredients" then
        add_action(blocker, {
            type = "build_durable_ingredient_supply",
            description = "Inspect the recipe and connect its inputs with belts and inserters. Do not repeatedly transfer ingredients by hand.",
        })
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
                type = "build_durable_boiler_fuel_supply",
                tool = "diagnose_fuel_sustainability",
                description = "Route coal from mining/storage to the boiler with a belt and inserter before treating power as repaired.",
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

function M.diagnose_factory_blockers(surface, force, x1, y1, x2, y2, limit)
    if not (surface and force) then return {error = "agent surface or force not found"} end
    limit = limit or 10
    local area = area_table(x1, y1, x2, y2)
    local found = surface.find_entities_filtered{
        area = area,
        force = force,
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

function M.diagnose_fuel_sustainability(surface, force, x1, y1, x2, y2, limit)
    if not (surface and force) then return {error = "agent surface or force not found"} end
    limit = limit or 20
    local area = area_table(x1, y1, x2, y2)
    local found = surface.find_entities_filtered{
        area = area,
        force = force,
    }
    local consumers = {}
    local coal_drills = {}
    local coal_chests = {}
    local coal_belts = {}
    local coal_resources = surface.find_entities_filtered{
        area = area,
        name = "coal",
    }

    for _, entity in pairs(found) do
        if entity.valid then
            if entity.burner then
                local fuel_inv = safe_fuel_inventory(entity)
                local fuel_count = inventory_total(fuel_inv) or 0
                local status = entity_status_string(entity)
                local priority = 40
                if fuel_count == 0 then priority = 10
                elseif fuel_count < 5 then priority = 20
                elseif status == "no_fuel" then priority = 10
                end
                table.insert(consumers, {
                    priority = priority,
                    unit_number = entity.unit_number,
                    name = entity.name,
                    type = entity.type,
                    position = pos_table(entity.position),
                    bounding_box = bounding_box_table(entity.bounding_box),
                    status = status,
                    fuel_count = fuel_count,
                    fuel_inserter_candidates = inserter_fuel_candidates(surface, entity.force, entity),
                    durable_actions = {{
                        type = "route_coal_supply",
                        description = "Build a durable coal belt/chest/inserter fuel feed to unit " .. tostring(entity.unit_number) .. " instead of repeating insert_items.",
                    }},
                })
            end

            if entity.type == "mining-drill" and entity.mining_target and entity.mining_target.name == "coal" then
                local source_tile = route_source_tile(entity)
                table.insert(coal_drills, {
                    unit_number = entity.unit_number,
                    name = entity.name,
                    position = pos_table(entity.position),
                    route_position = pos_table(route_source_position(entity)),
                    route_tile = source_tile,
                    status = entity_status_string(entity),
                    operational = entity_status_string(entity) == "working"
                        or entity_status_string(entity) == "waiting_for_space_in_destination",
                })
            elseif entity.type == "transport-belt"
                or entity.type == "underground-belt"
                or entity.type == "splitter"
            then
                local source = coal_source_record(entity)
                if source and source.coal_count > 0 then table.insert(coal_belts, source) end
            elseif entity.type == "container" or entity.type == "logistic-container" then
                local source = coal_source_record(entity)
                if source and source.coal_count > 0 then table.insert(coal_chests, source) end
            end
        end
    end

    table.sort(consumers, function(a, b)
        if a.priority ~= b.priority then return a.priority < b.priority end
        return tostring(a.unit_number or "") < tostring(b.unit_number or "")
    end)
    local total_consumers = #consumers
    local truncated = false
    if #consumers > limit then
        truncated = true
        while #consumers > limit do table.remove(consumers) end
    end

    for _, consumer in ipairs(consumers) do
        consumer.proven_fuel_connections = proven_fuel_connections(surface, force, consumer)
        consumer.automated = #consumer.proven_fuel_connections > 0
        if consumer.automated then
            consumer.issue = consumer.fuel_count == 0 and "automated_supply_starved" or nil
        else
            consumer.issue = consumer.fuel_count == 0 and "empty_fuel"
                or (consumer.fuel_count < 5 and "low_fuel" or "manual_or_unknown_fuel_supply")
        end

        local ranked_sources = ranked_coal_sources(consumer.position, coal_drills, coal_belts, coal_chests)
        consumer.candidate_sources = {}
        for i = 1, math.min(#ranked_sources, 8) do
            local source = ranked_sources[i]
            table.insert(consumer.candidate_sources, {
                kind = source.kind,
                unit_number = source.unit_number,
                name = source.name,
                position = source.position,
                route_position = source.route_position,
                route_tile = source.route_tile,
                coal_count = source.coal_count,
                status = source.status,
                operational = source.operational,
                distance = math.sqrt(source.distance_sq),
            })
        end

        local source = ranked_sources[1]
        if source and source.operational and not consumer.automated and consumer.fuel_inserter_candidates then
            for _, candidate in ipairs(consumer.fuel_inserter_candidates) do
                if candidate.can_place_inserter then
                    local source_position = source.route_position or source.position
                    local source_tile = source.route_tile or {x = tile_coord(source_position.x), y = tile_coord(source_position.y)}
                    local pickup_x = tile_coord(candidate.pickup_tile.x)
                    local pickup_y = tile_coord(candidate.pickup_tile.y)
                    candidate.route_coal_to_pickup_args = {
                        from_x = source_tile.x,
                        from_y = source_tile.y,
                        to_x = pickup_x,
                        to_y = pickup_y,
                    }
                    candidate.build_fuel_supply_args = {
                        consumer_unit_number = consumer.unit_number,
                        from_x = source_tile.x,
                        from_y = source_tile.y,
                        pickup_x = pickup_x,
                        pickup_y = pickup_y,
                        inserter_x = candidate.inserter_position.x,
                        inserter_y = candidate.inserter_position.y,
                        inserter_direction = candidate.inserter_direction_name,
                        inserter_name = candidate.inserter_name,
                    }
                    candidate.automation_steps = {{
                        tool = "build_fuel_supply",
                        args = candidate.build_fuel_supply_args,
                    }, {
                        tool = "verify_production",
                        args = {x = consumer.position.x, y = consumer.position.y, radius = 8},
                    }}
                    consumer.ready_to_call = {
                        tool = "build_fuel_supply",
                        args = candidate.build_fuel_supply_args,
                        source_kind = source.kind,
                        source_is_proposed = true,
                        follow_up = {
                            tool = "verify_production",
                            args = {x = consumer.position.x, y = consumer.position.y, radius = 8},
                        },
                    }
                    break
                end
            end
        end
    end

    local suggested_actions = {}
    if #consumers > 0 then
        local target = consumers[1]
        if target.automated and not target.issue then
            table.insert(suggested_actions, {
                type = "fuel_supply_verified",
                target_unit_number = target.unit_number,
                description = "An adjacent inserter with an operational coal source is feeding this consumer; no manual fuel action is needed.",
            })
        elseif target.automated then
            table.insert(suggested_actions, {
                type = "repair_upstream_coal_flow",
                target_unit_number = target.unit_number,
                connections = target.proven_fuel_connections,
                description = "The fuel inserter topology exists, but the consumer is starved. Repair coal flow upstream instead of hand-feeding it.",
            })
        elseif target.ready_to_call then
            table.insert(suggested_actions, {
                type = "build_fuel_supply",
                target_unit_number = target.unit_number,
                tool = target.ready_to_call.tool,
                args = target.ready_to_call.args,
                follow_up = target.ready_to_call.follow_up,
                source_kind = target.ready_to_call.source_kind,
                description = "Build the proposed route from the nearest operational source, then verify the actual adjacent inserter and coal flow. The source is not considered durable until proven_fuel_connections reports it.",
            })
        elseif #coal_resources > 0 then
            table.insert(suggested_actions, {
                type = "build_coal_mining_setup",
                target_unit_number = target.unit_number,
                coal_resource_position = pos_table(coal_resources[1].position),
                description = "Build coal mining first, then route coal to fuel " .. tostring(target.name) .. " unit " .. tostring(target.unit_number) .. ".",
            })
        else
            table.insert(suggested_actions, {
                type = "find_coal_source",
                target_unit_number = target.unit_number,
                description = "No coal source was found in this scan; locate coal before treating fuel problems as solved.",
            })
        end
    end

    return {
        area = {left_top = {x = math.min(x1, x2), y = math.min(y1, y2)}, right_bottom = {x = math.max(x1, x2), y = math.max(y1, y2)}},
        consumer_count = total_consumers,
        consumers = consumers,
        coal_sources = {
            mining_drills = coal_drills,
            belts = coal_belts,
            chests = coal_chests,
            resource_tiles = #coal_resources,
        },
        suggested_actions = suggested_actions,
        truncated = truncated,
        guidance = "Do not mark fuel as solved by repeated insert_items. Build or repair durable coal delivery to the ranked consumer, then verify production.",
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
