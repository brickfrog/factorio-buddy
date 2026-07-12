local M = {}
local inventory_contents = require("inventory").contents

local DIRECTIONS = {
    {name = "north", value = 0, dx = 0, dy = -1, px = 1, py = 0},
    {name = "east", value = 4, dx = 1, dy = 0, px = 0, py = 1},
    {name = "south", value = 8, dx = 0, dy = 1, px = -1, py = 0},
    {name = "west", value = 12, dx = -1, dy = 0, px = 0, py = -1},
}

local function pos(x, y)
    return {x = x, y = y}
end

local BOILER_FLUID_LAYOUTS = {
    {
        direction = 0,
        water_targets = {pos(-2, 0.5), pos(2, 0.5)},
        steam_target = pos(0, -1.5),
        engine_direction = 0,
        engine_input = pos(0, 2),
    },
    {
        direction = 4,
        water_targets = {pos(-0.5, -2), pos(-0.5, 2)},
        steam_target = pos(1.5, 0),
        engine_direction = 4,
        engine_input = pos(-2, 0),
    },
    {
        direction = 8,
        water_targets = {pos(2, -0.5), pos(-2, -0.5)},
        steam_target = pos(0, 1.5),
        engine_direction = 0,
        engine_input = pos(0, -2),
    },
    {
        direction = 12,
        water_targets = {pos(0.5, 2), pos(0.5, -2)},
        steam_target = pos(-1.5, 0),
        engine_direction = 4,
        engine_input = pos(2, 0),
    },
}

local POLE_SUPPLY_AREAS = {
    ["small-electric-pole"] = 2.5,
    ["medium-electric-pole"] = 3.5,
    ["big-electric-pole"] = 2.0,
    ["substation"] = 9.0,
}

local function pos_table(position)
    if not position then return nil end
    return pos(position.x, position.y)
end

local function fluid_table(fluid)
    if not fluid then return nil end
    if type(fluid) == "string" then
        return {name = fluid}
    end
    return {
        name = fluid.name,
        amount = fluid.amount,
        temperature = fluid.temperature,
    }
end

local function fluid_filter_name(filter)
    if not filter then return nil end
    if type(filter) == "string" then return filter end
    if type(filter) == "table" then
        if type(filter.name) == "string" then return filter.name end
        if type(filter.name) == "table" and filter.name.name then return filter.name.name end
    end
    return tostring(filter)
end

local function status_name(status_value)
    if status_value == nil then return nil end
    for name, value in pairs(defines.entity_status) do
        if value == status_value then return name end
    end
    return tostring(status_value)
end

local function safe_entity_status(entity)
    local ok, value = pcall(function() return entity.status end)
    if ok then return status_name(value) end
    return nil
end

local function raw_entity_status(entity)
    local ok, value = pcall(function() return entity.status end)
    if ok then return value end
    return nil
end

local function direction_name(direction)
    for _, dir in ipairs(DIRECTIONS) do
        if dir.value == direction then return dir.name end
    end
    return tostring(direction)
end

local function opposite_direction(dir)
    local opposite_value = (dir.value + 8) % 16
    for _, candidate in ipairs(DIRECTIONS) do
        if candidate.value == opposite_value then return candidate end
    end
    return dir
end

local function normalize_area(x1, y1, x2, y2)
    return {
        left_top = pos(math.min(x1, x2), math.min(y1, y2)),
        right_bottom = pos(math.max(x1, x2), math.max(y1, y2)),
    }
end

local function distance_sq(a, b)
    local dx = a.x - b.x
    local dy = a.y - b.y
    return dx * dx + dy * dy
end

local function inventory_count(character, item_name)
    local inv = character.get_main_inventory()
    if not inv then return 0 end
    return inv.get_item_count(item_name)
end

local function can_place(surface, force, entity_name, position, direction)
    local ok, allowed_or_error = pcall(function()
        return surface.can_place_entity{
            name = entity_name,
            position = {position.x, position.y},
            direction = direction,
            force = force,
            build_check_type = defines.build_check_type.manual,
        }
    end)
    if not ok then
        return false, tostring(allowed_or_error)
    end
    if allowed_or_error ~= true then
        return false, "Factorio cannot place entity here"
    end
    return true, nil
end

local function checked_entity(surface, force, entity_name, position, direction)
    local allowed, error = can_place(surface, force, entity_name, position, direction)
    return {
        entity_name = entity_name,
        position = pos(position.x, position.y),
        direction = direction,
        direction_name = direction_name(direction),
        factorio_allowed = allowed,
        error = error,
        place_args = {
            entity_name = entity_name,
            x = position.x,
            y = position.y,
            direction = direction_name(direction),
        },
    }
end

local function append_blocker(blockers, entity)
    if entity.factorio_allowed then return end
    table.insert(blockers, {
        entity_name = entity.entity_name,
        position = entity.position,
        direction = entity.direction,
        direction_name = entity.direction_name,
        reason = entity.error or "not placeable",
    })
end

local function missing_items(character, pipe_count, pole_count)
    local required = {
        ["offshore-pump"] = 1,
        ["boiler"] = 1,
        ["steam-engine"] = 1,
        ["pipe"] = pipe_count or 0,
        ["small-electric-pole"] = pole_count or 0,
    }
    local missing = {}
    for name, count in pairs(required) do
        local have = inventory_count(character, name)
        if have < count then
            table.insert(missing, {name = name, required = count, available = have})
        end
    end
    return missing
end

local function checked_pipe_path(surface, force, positions)
    local pipes = {}
    for _, pipe_pos in ipairs(positions) do
        table.insert(pipes, checked_entity(surface, force, "pipe", pipe_pos, 0))
    end
    return pipes
end

local function cleanup_simulated_entities(created)
    for i = #created, 1, -1 do
        local entity = created[i]
        if entity and entity.valid then
            pcall(function() entity.destroy{raise_destroy = false} end)
        end
    end
end

local function validate_cumulative_placement(surface, force, entries)
    local created = {}
    local ok, failure = pcall(function()
        for _, entry in ipairs(entries) do
            local allowed, error = can_place(surface, force, entry.entity_name, entry.position, entry.direction)
            if not allowed then
                return {
                    entity_name = entry.entity_name,
                    position = entry.position,
                    direction = entry.direction,
                    direction_name = entry.direction_name,
                    reason = error or "Cannot place entity here",
                }
            end

            local entity = surface.create_entity{
                name = entry.entity_name,
                position = {entry.position.x, entry.position.y},
                direction = entry.direction,
                force = force,
            }
            if not entity then
                return {
                    entity_name = entry.entity_name,
                    position = entry.position,
                    direction = entry.direction,
                    direction_name = entry.direction_name,
                    reason = "create_entity returned nil during cumulative validation",
                }
            end
            table.insert(created, entity)
        end
        return nil
    end)

    cleanup_simulated_entities(created)

    if not ok then
        return {
            entity_name = "layout",
            position = nil,
            direction = nil,
            direction_name = nil,
            reason = tostring(failure),
        }
    end
    return failure
end

local function find_pole_position(surface, force, ideal)
    local offsets = {
        {0, 0},
        {1, 0},
        {-1, 0},
        {0, 1},
        {0, -1},
        {1, 1},
        {-1, -1},
        {1, -1},
        {-1, 1},
    }
    for _, offset in ipairs(offsets) do
        local candidate = pos(ideal.x + offset[1], ideal.y + offset[2])
        local entity = checked_entity(surface, force, "small-electric-pole", candidate, 0)
        if entity.factorio_allowed then return entity end
    end
    return checked_entity(surface, force, "small-electric-pole", ideal, 0)
end

local function find_machine_connection_pole(surface, force, machine_pos)
    local offsets = {
        {-2, 0},
        {2, 0},
        {0, -3},
        {0, 3},
        {-2, -1},
        {-2, 1},
        {2, -1},
        {2, 1},
        {-3, 0},
        {3, 0},
        {-3, -1},
        {-3, 1},
        {3, -1},
        {3, 1},
        {0, -4},
        {0, 4},
    }
    for _, offset in ipairs(offsets) do
        local candidate = pos(machine_pos.x + offset[1], machine_pos.y + offset[2])
        local entity = checked_entity(surface, force, "small-electric-pole", candidate, 0)
        if entity.factorio_allowed then return entity end
    end
    return find_pole_position(surface, force, machine_pos)
end

local function pole_path(surface, force, from_pos, to_pos)
    local poles = {}
    local first_pole = find_machine_connection_pole(surface, force, from_pos)
    table.insert(poles, first_pole)

    local start_pos = first_pole.position
    local dx = to_pos.x - start_pos.x
    local dy = to_pos.y - start_pos.y
    local distance = math.sqrt(dx * dx + dy * dy)
    local step = 6
    local count = math.max(1, math.ceil(distance / step))
    for i = 1, count do
        local t = count == 0 and 0 or i / count
        local ideal = pos(
            math.floor(start_pos.x + dx * t + 0.5),
            math.floor(start_pos.y + dy * t + 0.5)
        )
        table.insert(poles, find_pole_position(surface, force, ideal))
    end
    return poles
end

local function candidate_layouts(pump, dir)
    local pump_pos = pump.position
    local pipe_pos = pos(pump_pos.x + dir.dx, pump_pos.y + dir.dy)
    local pipe_center = pos(pipe_pos.x + 0.5, pipe_pos.y + 0.5)
    local layouts = {}

    for _, fluid_layout in ipairs(BOILER_FLUID_LAYOUTS) do
        for water_index, water_target in ipairs(fluid_layout.water_targets) do
            local boiler_pos = pos(
                pipe_center.x - water_target.x,
                pipe_center.y - water_target.y
            )
            local steam_target = pos(
                boiler_pos.x + fluid_layout.steam_target.x,
                boiler_pos.y + fluid_layout.steam_target.y
            )
            local engine_pos = pos(
                steam_target.x - fluid_layout.engine_input.x,
                steam_target.y - fluid_layout.engine_input.y
            )
            table.insert(layouts, {
                name = "fluid_" .. direction_name(fluid_layout.direction) .. "_" .. tostring(water_index),
                boiler = {position = boiler_pos, direction = fluid_layout.direction},
                steam_engine = {position = engine_pos, direction = fluid_layout.engine_direction},
                pipe_positions = {pipe_pos},
            })
        end
    end

    return layouts
end

local function build_plan(surface, force, pump, layout, target)
    local offshore_pump = checked_entity(surface, force, "offshore-pump", pump.position, pump.direction)
    local boiler = checked_entity(surface, force, "boiler", layout.boiler.position, layout.boiler.direction)
    local steam_engine = checked_entity(surface, force, "steam-engine", layout.steam_engine.position, layout.steam_engine.direction)
    local pipes = checked_pipe_path(surface, force, layout.pipe_positions)
    local poles = pole_path(surface, force, steam_engine.position, target)
    local blockers = {}
    local build_steps = {
        offshore_pump.place_args,
        boiler.place_args,
        steam_engine.place_args,
    }

    append_blocker(blockers, offshore_pump)
    append_blocker(blockers, boiler)
    append_blocker(blockers, steam_engine)
    for _, pipe in ipairs(pipes) do
        append_blocker(blockers, pipe)
        table.insert(build_steps, pipe.place_args)
    end
    for _, pole in ipairs(poles) do
        append_blocker(blockers, pole)
        table.insert(build_steps, pole.place_args)
    end

    if #blockers == 0 then
        local placement_entries = {offshore_pump, boiler, steam_engine}
        for _, pipe in ipairs(pipes) do table.insert(placement_entries, pipe) end
        for _, pole in ipairs(poles) do table.insert(placement_entries, pole) end
        local cumulative_failure = validate_cumulative_placement(surface, force, placement_entries)
        if cumulative_failure then table.insert(blockers, cumulative_failure) end
    end

    return {
        success = #blockers == 0,
        layout = layout.name,
        offshore_pump = offshore_pump,
        boiler = boiler,
        steam_engine = steam_engine,
        pipes = pipes,
        pole_positions = poles,
        fuel_target = {
            entity_name = "boiler",
            position = boiler.position,
            inventory_type = "fuel",
        },
        blockers = blockers,
        build_steps = build_steps,
    }
end

local function append_steam_issue(result, issue_type, severity, entity, message, action, details)
    local issue = {
        type = issue_type,
        severity = severity,
        entity = entity and {
            unit_number = entity.unit_number,
            name = entity.name,
            position = pos_table(entity.position),
        } or nil,
        message = message,
        action = action,
    }
    if details then issue.details = details end
    table.insert(result.issues, issue)
    if action then table.insert(result.suggested_actions, action) end
end

local function finish_steam_diagnostic(result)
    local steam_entity_count = result.summary.offshore_pumps
        + result.summary.boilers
        + result.summary.steam_engines
        + result.summary.pipes
    local critical_issues = 0
    local warning_issues = 0
    for _, issue in ipairs(result.issues) do
        if issue.severity == "critical" then
            critical_issues = critical_issues + 1
        elseif issue.severity == "warning" then
            warning_issues = warning_issues + 1
        end
    end

    result.summary.steam_power_entities = steam_entity_count
    result.summary.issue_count = #result.issues
    result.summary.critical_issues = critical_issues
    result.summary.warning_issues = warning_issues
    result.has_existing_plant = steam_entity_count > 0

    if steam_entity_count == 0 then
        result.status = "no_plant"
        result.next_action = "build_steam_power"
    elseif critical_issues > 0 then
        result.status = "critical"
        result.next_action = "repair_existing_steam_power"
    elseif warning_issues > 0 then
        result.status = "warning"
        result.next_action = "inspect_existing_steam_power"
    elseif result.summary.offshore_pumps > 0
        and result.summary.boilers > 0
        and result.summary.steam_engines > 0 then
        result.status = "ok"
        result.next_action = "verify_power_status"
    else
        result.status = "incomplete"
        result.next_action = "complete_existing_steam_power"
    end
end

local function fluidbox_neighbours(entity, index)
    local neighbours = {}
    local ok, records = pcall(function()
        return entity.get_fluid_box_neighbours(index)
    end)
    if ok and type(records) == "table" then
        for _, record in pairs(records) do
            if record.entity then
                table.insert(neighbours, {
                    name = record.entity.name,
                    unit_number = record.entity.unit_number,
                    position = pos_table(record.entity.position),
                    fluidbox_index = record.index,
                })
            end
        end
    end
    return neighbours
end

local function fluidbox_pipe_connections(entity, index)
    local connections = {}
    local ok, records = pcall(function()
        return entity.get_fluid_box_pipe_connections(index)
    end)
    if ok and type(records) == "table" then
        for _, connection in pairs(records) do
            local target = connection.target
            table.insert(connections, {
                flow_direction = tostring(connection.flow_direction),
                connection_type = tostring(connection.connection_type),
                position = pos_table(connection.position),
                target_position = pos_table(connection.target_position),
                target = target and {
                    name = target.name,
                    unit_number = target.unit_number,
                    position = pos_table(target.position),
                } or nil,
                target_fluidbox_index = connection.target_fluidbox_index,
                target_pipe_connection_index = connection.target_pipe_connection_index,
            })
        end
    end
    return connections
end

local function describe_fluidboxes(entity, result)
    local boxes = {}
    for index = 1, 12 do
        local info = {
            index = index,
            neighbours = {},
            pipe_connections = {},
        }
        local has_box = false

        local ok_capacity, capacity = pcall(function()
            return entity.get_fluid_capacity(index)
        end)
        if ok_capacity and capacity ~= nil then
            info.capacity = capacity
            has_box = true
        end

        local ok_filter, filter = pcall(function()
            return entity.get_fluid_filter(index)
        end)
        if ok_filter and filter ~= nil then
            info.filter = fluid_filter_name(filter)
            has_box = true
        end

        local ok_fluid, fluid = pcall(function()
            return entity.get_fluid(index)
        end)
        if ok_fluid and fluid ~= nil then
            info.fluid = fluid_table(fluid)
            has_box = true
        end

        local ok_has_segment, has_segment = pcall(function()
            return entity.has_fluid_segment(index)
        end)
        if ok_has_segment and has_segment then
            info.has_segment = true
            has_box = true

            local ok_segment_id, segment_id = pcall(function()
                return entity.get_fluid_segment_id(index)
            end)
            if ok_segment_id and segment_id ~= nil then
                info.segment_id = segment_id
            end

            local ok_segment_fluid, segment_fluid = pcall(function()
                return entity.get_fluid_segment_fluid(index)
            end)
            if ok_segment_fluid and segment_fluid ~= nil then
                info.segment_fluid = fluid_table(segment_fluid)
            end

            local ok_segment_capacity, segment_capacity = pcall(function()
                return entity.get_fluid_segment_capacity(index)
            end)
            if ok_segment_capacity and segment_capacity ~= nil then
                info.segment_capacity = segment_capacity
            end

            local ok_extent, extent = pcall(function()
                return entity.get_fluid_segment_extent_bounding_box(index)
            end)
            if ok_extent and extent then
                info.segment_extent = {
                    left_top = pos_table(extent.left_top),
                    right_bottom = pos_table(extent.right_bottom),
                }
            end

            if info.segment_id then
                local key = tostring(info.segment_id)
                if not result.fluid_segments[key] then
                    result.fluid_segments[key] = {
                        id = info.segment_id,
                        fluid = info.segment_fluid,
                        capacity = info.segment_capacity,
                        members = {},
                    }
                end
                table.insert(result.fluid_segments[key].members, {
                    unit_number = entity.unit_number,
                    name = entity.name,
                    position = pos_table(entity.position),
                    fluidbox_index = index,
                })
                result.fluid_segments[key].member_count = #result.fluid_segments[key].members
            end
        end

        local neighbours = fluidbox_neighbours(entity, index)
        if #neighbours > 0 then
            info.neighbours = neighbours
            has_box = true
        end

        local pipe_connections = fluidbox_pipe_connections(entity, index)
        if #pipe_connections > 0 then
            info.pipe_connections = pipe_connections
            has_box = true
        end

        if has_box then table.insert(boxes, info) end
    end
    return boxes
end

local function fluidbox_has_neighbour(item, names)
    for _, box in ipairs(item.fluidboxes or {}) do
        for _, neighbour in ipairs(box.neighbours or {}) do
            if names[neighbour.name] then return true, neighbour end
        end
        for _, connection in ipairs(box.pipe_connections or {}) do
            local target = connection.target
            if target and names[target.name] then return true, target end
        end
    end
    return false, nil
end

local function nearest_entity_record(entities, from_position)
    local from = pos_table(from_position)
    local best = nil
    local best_distance_sq = nil
    for _, entity in pairs(entities or {}) do
        local candidate = pos_table(entity.position)
        local candidate_distance_sq = distance_sq(from, candidate)
        if best_distance_sq == nil or candidate_distance_sq < best_distance_sq then
            best = entity
            best_distance_sq = candidate_distance_sq
        end
    end
    if not best then return nil end
    return {
        name = best.name,
        unit_number = best.unit_number,
        position = pos_table(best.position),
        distance = math.sqrt(best_distance_sq),
    }
end

local function append_repair_step(result, step_type, description, tool_name, tool_args, entity)
    table.insert(result.repair_steps, {
        type = step_type,
        description = description,
        tool = tool_name,
        tool_args = tool_args,
        entity = entity and {
            unit_number = entity.unit_number,
            name = entity.name,
            position = pos_table(entity.position),
        } or nil,
    })
end

local function add_missing_item(result, character, item_name, required)
    if required <= 0 then return end
    local available = inventory_count(character, item_name)
    if available < required then
        table.insert(result.missing_items, {
            name = item_name,
            required = required,
            available = available,
        })
    end
end

local function pole_repair_path(surface, force, from_position, to_position)
    local poles = {}
    local from_pos = pos_table(from_position)
    local to_pos = pos_table(to_position)
    if not (from_pos and to_pos) then return poles end

    local dx = to_pos.x - from_pos.x
    local dy = to_pos.y - from_pos.y
    local distance = math.sqrt(dx * dx + dy * dy)
    local step = 6
    local count = math.max(1, math.ceil(distance / step))
    for i = 1, count do
        local t = i / count
        local ideal = pos(
            math.floor(from_pos.x + dx * t + 0.5),
            math.floor(from_pos.y + dy * t + 0.5)
        )
        table.insert(poles, find_pole_position(surface, force, ideal))
    end
    return poles
end

local function index_entities_by_unit(entities)
    local by_unit = {}
    for _, entity in pairs(entities or {}) do
        if entity.unit_number then by_unit[tostring(entity.unit_number)] = entity end
    end
    return by_unit
end

local function pole_supply_reaches(pole, target)
    local supply_dist = POLE_SUPPLY_AREAS[pole.name] or 2.5
    return math.abs(pole.position.x - target.x) <= supply_dist
        and math.abs(pole.position.y - target.y) <= supply_dist
end

local function append_place_pole_step(result, pole, description)
    table.insert(result.steps, {
        type = "place_power_pole",
        description = description,
        tool = "place_entity",
        tool_args = pole.place_args,
    })
end

function M.diagnose_steam_power(x, y, radius)
    local surface = game.surfaces[1]
    local r = radius or 50
    local area = {{x - r, y - r}, {x + r, y + r}}
    local result = {
        area = {
            center = {x = x, y = y},
            radius = r,
        },
        summary = {
            offshore_pumps = 0,
            boilers = 0,
            steam_engines = 0,
            pipes = 0,
            electric_poles = 0,
        },
        entities = {},
        fluid_segments = {},
        issues = {},
        suggested_actions = {},
    }

    local poles = surface.find_entities_filtered{type = "electric-pole", area = area, force = "player"}
    result.summary.electric_poles = #poles

    local steam_entities = surface.find_entities_filtered{
        area = area,
        force = "player",
        name = {"offshore-pump", "boiler", "steam-engine", "pipe", "pipe-to-ground"},
    }

    local present = {}
    for _, entity in pairs(steam_entities) do
        present[entity.name] = (present[entity.name] or 0) + 1
    end

    for _, entity in pairs(steam_entities) do
        if entity.name == "offshore-pump" then result.summary.offshore_pumps = result.summary.offshore_pumps + 1 end
        if entity.name == "boiler" then result.summary.boilers = result.summary.boilers + 1 end
        if entity.name == "steam-engine" then result.summary.steam_engines = result.summary.steam_engines + 1 end
        if entity.name == "pipe" or entity.name == "pipe-to-ground" then result.summary.pipes = result.summary.pipes + 1 end

        local item = {
            unit_number = entity.unit_number,
            name = entity.name,
            type = entity.type,
            position = pos_table(entity.position),
            direction = entity.direction,
            status = safe_entity_status(entity),
            fluid_contents = {},
            fluidboxes = {},
        }

        local ok_contents, contents = pcall(function()
            return entity.get_fluid_contents()
        end)
        if ok_contents and type(contents) == "table" then
            for name, amount in pairs(contents) do
                table.insert(item.fluid_contents, {name = name, amount = amount})
            end
        end

        if entity.burner then
            local fuel_inv = entity.get_fuel_inventory()
            item.fuel = {
                total = fuel_inv and fuel_inv.get_item_count() or 0,
                inventory = inventory_contents(fuel_inv),
            }
        end

        local ok_connected, connected = pcall(function()
            return entity.is_connected_to_electric_network()
        end)
        if ok_connected then item.connected_to_electric_network = connected end

        item.fluidboxes = describe_fluidboxes(entity, result)
        table.insert(result.entities, item)

        if entity.name == "boiler" then
            if item.fuel and item.fuel.total == 0 then
                append_steam_issue(result, "boiler_no_fuel", "critical", entity, "Boiler has no fuel.", "Insert coal or another fuel into boiler unit " .. tostring(entity.unit_number) .. ".")
            end
            if item.status == "no_input_fluid" then
                local connected_to_water = fluidbox_has_neighbour(item, {["offshore-pump"] = true, ["pipe"] = true, ["pipe-to-ground"] = true})
                local has_fluidbox_data = #(item.fluidboxes or {}) > 0
                if has_fluidbox_data and not connected_to_water and ((present["offshore-pump"] or 0) > 0 or (present["pipe"] or 0) > 0 or (present["pipe-to-ground"] or 0) > 0) then
                    append_steam_issue(
                        result,
                        "boiler_water_alignment_mismatch",
                        "critical",
                        entity,
                        "Boiler is near water infrastructure but no water fluidbox is connected.",
                        "Rotate or move boiler unit " .. tostring(entity.unit_number) .. " so its water input touches the offshore pump or pipe network.",
                        {nearby_fluid_infrastructure = true}
                    )
                else
                    append_steam_issue(result, "boiler_no_water", "critical", entity, "Boiler is missing water input.", "Connect offshore pump water output to boiler unit " .. tostring(entity.unit_number) .. " water input.")
                end
            elseif item.status == "full_output" then
                append_steam_issue(result, "boiler_steam_output_blocked", "critical", entity, "Boiler has steam but cannot drain it.", "Connect boiler unit " .. tostring(entity.unit_number) .. " steam output to a steam engine input, or move the blocking engine/pipe.")
            end
        elseif entity.name == "steam-engine" then
            if item.status == "no_input_fluid" then
                local connected_to_steam = fluidbox_has_neighbour(item, {["boiler"] = true, ["pipe"] = true, ["pipe-to-ground"] = true})
                local has_fluidbox_data = #(item.fluidboxes or {}) > 0
                if has_fluidbox_data and not connected_to_steam and ((present["boiler"] or 0) > 0 or (present["pipe"] or 0) > 0 or (present["pipe-to-ground"] or 0) > 0) then
                    append_steam_issue(
                        result,
                        "steam_engine_alignment_mismatch",
                        "critical",
                        entity,
                        "Steam engine is near steam infrastructure but no steam fluidbox is connected.",
                        "Move or rotate steam engine unit " .. tostring(entity.unit_number) .. " so its input touches the boiler steam output or connected pipe.",
                        {nearby_steam_infrastructure = true}
                    )
                else
                    append_steam_issue(result, "steam_engine_no_steam", "critical", entity, "Steam engine is missing steam input.", "Connect a boiler steam output to steam engine unit " .. tostring(entity.unit_number) .. ".")
                end
            end
            local nearby_poles = surface.find_entities_filtered{type = "electric-pole", position = entity.position, radius = 8, force = "player", limit = 1}
            if #nearby_poles == 0 then
                local nearest_pole = nearest_entity_record(poles, entity.position)
                if nearest_pole then
                    append_steam_issue(
                        result,
                        "steam_engine_pole_route_incomplete",
                        "warning",
                        entity,
                        "Steam engine has poles in the diagnostic area, but none close enough to receive generated power.",
                        "Extend the pole line from nearest pole unit " .. tostring(nearest_pole.unit_number) .. " to steam engine unit " .. tostring(entity.unit_number) .. ".",
                        {nearest_pole = nearest_pole}
                    )
                else
                    append_steam_issue(result, "steam_engine_not_on_grid", "warning", entity, "Steam engine has no electric pole close enough to receive generated power.", "Place an electric pole within wire reach of steam engine unit " .. tostring(entity.unit_number) .. ".")
                end
            end
        elseif entity.name == "offshore-pump" then
            if item.status == "no_power" then
                append_steam_issue(result, "offshore_pump_no_power", "critical", entity, "Offshore pump reports no power.", "Move/rebuild pump at a valid shoreline or inspect modded pump requirements.")
            elseif item.status == "no_input_fluid" then
                append_steam_issue(result, "offshore_pump_not_on_water", "critical", entity, "Offshore pump is not receiving water.", "Rebuild offshore pump on a valid shoreline tile.")
            end
        end
    end

    if result.summary.offshore_pumps == 0 then
        table.insert(result.suggested_actions, "No offshore pump in area; locate shoreline before building steam power.")
    end
    if result.summary.boilers == 0 then
        table.insert(result.suggested_actions, "No boiler in area; build one between pump water output and steam engine input.")
    end
    if result.summary.steam_engines == 0 then
        table.insert(result.suggested_actions, "No steam engine in area; build one on boiler steam output.")
    end

    finish_steam_diagnostic(result)
    return result
end

function M.extend_power_to(character, x, y, radius, target_x, target_y)
    if not (character and character.valid) then
        return {success = false, error = "no character; spawn first", blockers = {"no_character"}}
    end

    local surface = character.surface
    local force = character.force
    local r = radius or 50
    local target = pos(target_x, target_y)
    local area = {{x - r, y - r}, {x + r, y + r}}
    local existing_poles = surface.find_entities_filtered{
        type = "electric-pole",
        area = area,
        force = force,
    }
    local result = {
        success = false,
        dry_run = true,
        area = {
            center = {x = x, y = y},
            radius = r,
        },
        target = target,
        source_pole = nil,
        steps = {},
        missing_items = {},
        blockers = {},
    }

    if #existing_poles == 0 then
        table.insert(result.blockers, {
            type = "no_power_grid_found",
            message = "No electric poles were found in the extension area.",
        })
        result.next_action = "build_power_source_or_expand_search"
        result.guidance = "Find an existing powered pole or build steam power before extending the grid."
        return result
    end

    local nearest = nearest_entity_record(existing_poles, target)
    result.source_pole = nearest

    for _, pole in pairs(existing_poles) do
        if pole_supply_reaches(pole, target) then
            result.success = true
            result.ready = true
            result.already_powered = true
            result.covering_pole = {
                name = pole.name,
                unit_number = pole.unit_number,
                position = pos_table(pole.position),
            }
            result.next_action = "verify_power_status"
            result.guidance = "Target is already inside an electric pole supply area."
            return result
        end
    end

    local pole_plan = pole_repair_path(surface, force, nearest.position, target)
    for _, pole in ipairs(pole_plan) do
        if pole.factorio_allowed then
            append_place_pole_step(
                result,
                pole,
                "Place a small electric pole to extend power toward target (" .. tostring(target_x) .. ", " .. tostring(target_y) .. ")."
            )
        else
            table.insert(result.blockers, {
                type = "no_pole_placement",
                message = "Could not find a placeable pole position while extending power.",
                attempted_position = pole.position,
                reason = pole.error,
            })
        end
    end

    add_missing_item(result, character, "small-electric-pole", #result.steps)
    result.ready = #result.steps > 0 and #result.missing_items == 0 and #result.blockers == 0
    result.success = result.ready
    if result.ready then
        result.next_action = "execute_power_extension_steps"
        result.guidance = "Place steps in order, then call get_power_status or find_power_issues near the target."
    elseif #result.steps > 0 then
        result.next_action = "gather_missing_items_or_clear_blockers"
        result.guidance = "Power extension steps were found, but missing_items or blockers must be resolved first."
    else
        result.next_action = "manual_inspection_required"
        result.guidance = "No safe pole extension steps were found."
    end
    return result
end

function M.repair_steam_power(character, x, y, radius, target_x, target_y)
    if not (character and character.valid) then
        return {success = false, error = "no character; spawn first", blockers = {"no_character"}}
    end

    local surface = character.surface
    local force = character.force
    local r = radius or 50
    local area = {{x - r, y - r}, {x + r, y + r}}
    local diagnostic = M.diagnose_steam_power(x, y, r)
    local result = {
        success = false,
        dry_run = true,
        status = diagnostic.status,
        next_action = diagnostic.next_action,
        diagnostic = diagnostic,
        repair_steps = {},
        missing_items = {},
        blockers = {},
        notes = {},
    }

    if target_x ~= nil and target_y ~= nil then
        result.target = {x = target_x, y = target_y}
    end

    if not diagnostic.has_existing_plant then
        table.insert(result.blockers, {
            type = "no_steam_power_found",
            message = "No existing steam-power entities were found in the repair area.",
        })
        if target_x ~= nil and target_y ~= nil then
            result.suggested_next_tool = {
                tool = "plan_steam_power",
                tool_args = {
                    water_x1 = x - r,
                    water_y1 = y - r,
                    water_x2 = x + r,
                    water_y2 = y + r,
                    target_x = target_x,
                    target_y = target_y,
                },
            }
        end
        result.guidance = "No plant exists to repair; use suggested_next_tool to plan a new build."
        result.next_action = "plan_steam_power"
        return result
    end

    local entities = surface.find_entities_filtered{
        area = area,
        force = force,
        name = {"offshore-pump", "boiler", "steam-engine", "pipe", "pipe-to-ground", "small-electric-pole", "medium-electric-pole", "big-electric-pole", "substation"},
    }
    local by_unit = index_entities_by_unit(entities)
    local poles = surface.find_entities_filtered{type = "electric-pole", area = area, force = force}
    local needed = {
        ["coal"] = 0,
        ["small-electric-pole"] = 0,
    }

    local issue_types = {}
    for _, issue in ipairs(diagnostic.issues or {}) do
        issue_types[issue.type] = true
    end

    for _, issue in ipairs(diagnostic.issues or {}) do
        local entity = issue.entity and by_unit[tostring(issue.entity.unit_number)] or nil
        if issue.type == "boiler_no_fuel" and entity then
            local count = 5
            needed["coal"] = needed["coal"] + count
            append_repair_step(
                result,
                "fuel_boiler",
                "Insert fuel into boiler unit " .. tostring(entity.unit_number) .. ".",
                "insert_items",
                {
                    unit_number = entity.unit_number,
                    item = "coal",
                    count = count,
                    inventory_type = "fuel",
                },
                entity
            )
        elseif (issue.type == "steam_engine_pole_route_incomplete" or issue.type == "steam_engine_not_on_grid") and entity then
            local nearest = nearest_entity_record(poles, entity.position)
            local pole_plan
            if nearest then
                pole_plan = pole_repair_path(surface, force, nearest.position, entity.position)
            else
                pole_plan = {find_machine_connection_pole(surface, force, entity.position)}
            end
            for _, pole in ipairs(pole_plan) do
                if pole.factorio_allowed then
                    needed["small-electric-pole"] = needed["small-electric-pole"] + 1
                    append_repair_step(
                        result,
                        "place_power_pole",
                        "Place a small electric pole to connect steam engine unit " .. tostring(entity.unit_number) .. " to the grid.",
                        "place_entity",
                        pole.place_args,
                        entity
                    )
                else
                    table.insert(result.blockers, {
                        type = "no_pole_placement",
                        message = "Could not find a placeable pole position near steam engine unit " .. tostring(entity.unit_number) .. ".",
                        attempted_position = pole.position,
                        reason = pole.error,
                    })
                end
            end
        elseif issue.type == "steam_engine_no_steam" and issue_types["boiler_no_fuel"] then
            table.insert(result.notes, {
                type = "steam_engine_no_steam_may_clear_after_fuel",
                message = "Steam engine has no steam, but boiler fuel is also missing; fuel the boiler first and re-run diagnostics.",
            })
        elseif issue.type == "boiler_no_water"
            or issue.type == "steam_engine_no_steam"
            or issue.type == "boiler_water_alignment_mismatch"
            or issue.type == "steam_engine_alignment_mismatch" then
            table.insert(result.blockers, {
                type = issue.type,
                message = issue.message,
                action = issue.action,
                entity = issue.entity,
            })
        end
    end

    add_missing_item(result, character, "coal", needed["coal"])
    add_missing_item(result, character, "small-electric-pole", needed["small-electric-pole"])

    result.ready = #result.repair_steps > 0 and #result.missing_items == 0 and #result.blockers == 0
    result.success = result.ready
    if result.ready then
        result.next_action = "execute_repair_steps"
        result.guidance = "Execute repair_steps in order, then call diagnose_steam_power and get_power_status again."
    elseif #result.repair_steps > 0 then
        result.next_action = "gather_missing_items_or_clear_blockers"
        result.guidance = "Repair steps were found, but missing_items or blockers must be resolved before executing them."
    else
        result.next_action = "manual_inspection_required"
        result.guidance = "No safe automatic dry-run repair steps were found; inspect diagnostic issues before moving fluid entities."
    end
    return result
end

local POWER_CONSUMER_TYPES = {
    "assembling-machine",
    "furnace",
    "lab",
    "mining-drill",
    "inserter",
    "beacon",
    "radar",
}

local POWER_ISSUE_CONSUMER_TYPES = {
    "assembling-machine",
    "furnace",
    "lab",
    "mining-drill",
    "inserter",
    "beacon",
    "radar",
    "lamp",
    "roboport",
}

local function area_around(x, y, radius)
    local r = radius or 50
    return r, {{x - r, y - r}, {x + r, y + r}}
end

local function entity_uses_electricity(entity)
    local proto = prototypes.entity[entity.name]
    if not proto then return false end
    local ok, uses_electric = pcall(function()
        return proto.electric_energy_source_prototype ~= nil
    end)
    return ok and uses_electric
end

local function entity_position_record(entity)
    return {
        name = entity.name,
        x = entity.position.x,
        y = entity.position.y,
        unit_number = entity.unit_number,
    }
end

local function build_power_coverage(surface, area, x, y, radius, display_ids)
    local poles = surface.find_entities_filtered{
        type = "electric-pole",
        area = area,
        force = "player",
    }
    local coverage = {}
    local network_map = {}
    local next_display_id = 1
    local networks = {}
    local pole_records = {}

    for _, pole in pairs(poles) do
        local network_id = pole.electric_network_id
        if display_ids and network_id and not network_map[network_id] then
            network_map[network_id] = next_display_id
            networks[tostring(next_display_id)] = network_id
            next_display_id = next_display_id + 1
            if next_display_id > 9 then next_display_id = 9 end
        end

        local supply_dist = POLE_SUPPLY_AREAS[pole.name] or 2.5
        local coverage_id = display_ids and (network_map[network_id] or 0) or network_id
        if display_ids then
            table.insert(pole_records, {
                name = pole.name,
                x = pole.position.x,
                y = pole.position.y,
                network_id = network_id,
                display_id = coverage_id,
                supply_area = supply_dist,
            })
        end

        local px, py = math.floor(pole.position.x), math.floor(pole.position.y)
        local sd = math.ceil(supply_dist)
        for dx = -sd, sd do
            for dy = -sd, sd do
                if dx * dx + dy * dy <= supply_dist * supply_dist then
                    local tx, ty = px + dx, py + dy
                    if not display_ids or (tx >= x - radius and tx <= x + radius and ty >= y - radius and ty <= y + radius) then
                        coverage[tx .. "," .. ty] = coverage_id
                    end
                end
            end
        end
    end

    return coverage, poles, pole_records, networks
end

function M.get_power_status(x, y, radius)
    local surface = game.surfaces[1]
    local r, area = area_around(x, y, radius)
    local poles = surface.find_entities_filtered{
        type = "electric-pole",
        area = area,
        force = "player",
    }

    if #poles == 0 then
        return {error = "No electric poles found in area"}
    end

    local pole = poles[1]
    local network_id = pole.electric_network_id
    local result = {
        network_id = network_id,
        pole_count = #poles,
        generators = {},
        consumers = {
            working = 0,
            low_power = 0,
            no_power = 0,
            total = 0,
        },
        production_kw = 0,
        consumption_kw = 0,
        satisfaction = "unknown",
    }

    local generator_counts = {}
    local total_production = 0
    local generators = surface.find_entities_filtered{
        area = area,
        type = {"generator", "solar-panel", "accumulator"},
        force = "player",
    }

    for _, gen in pairs(generators) do
        local connected_pole = surface.find_entities_filtered{
            type = "electric-pole",
            position = gen.position,
            radius = 10,
            force = "player",
            limit = 1,
        }[1]
        if connected_pole and connected_pole.electric_network_id == network_id then
            generator_counts[gen.name] = (generator_counts[gen.name] or 0) + 1
            if gen.type == "generator" then
                total_production = total_production + (gen.energy_generated_last_tick or 0) * 60 / 1000
            elseif gen.type == "solar-panel" then
                total_production = total_production + 60 * surface.daytime
            end
        end
    end

    for name, count in pairs(generator_counts) do
        table.insert(result.generators, {name = name, count = count})
    end

    local total_consumption = 0
    local consumers_by_status = {working = {}, low_power = {}, no_power = {}}
    for _, entity_type in pairs(POWER_CONSUMER_TYPES) do
        local entities = surface.find_entities_filtered{
            area = area,
            type = entity_type,
            force = "player",
        }
        for _, ent in pairs(entities) do
            if entity_uses_electricity(ent) then
                result.consumers.total = result.consumers.total + 1
                local status = raw_entity_status(ent)
                if status == defines.entity_status.no_power then
                    result.consumers.no_power = result.consumers.no_power + 1
                    table.insert(consumers_by_status.no_power, entity_position_record(ent))
                elseif status == defines.entity_status.low_power then
                    result.consumers.low_power = result.consumers.low_power + 1
                    table.insert(consumers_by_status.low_power, entity_position_record(ent))
                elseif status == defines.entity_status.working then
                    result.consumers.working = result.consumers.working + 1
                end

                local proto = prototypes.entity[ent.name]
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
    if result.consumers.no_power > 0 then
        result.satisfaction = "critical"
    elseif result.consumers.low_power > 0 then
        result.satisfaction = "low"
    elseif result.consumers.working > 0 then
        result.satisfaction = "ok"
    else
        result.satisfaction = "idle"
    end

    if #consumers_by_status.no_power > 0 then
        result.no_power_entities = {}
        for i = 1, math.min(5, #consumers_by_status.no_power) do
            table.insert(result.no_power_entities, consumers_by_status.no_power[i])
        end
    end
    if #consumers_by_status.low_power > 0 then
        result.low_power_entities = {}
        for i = 1, math.min(5, #consumers_by_status.low_power) do
            table.insert(result.low_power_entities, consumers_by_status.low_power[i])
        end
    end

    local stats = pole.electric_network_statistics
    if stats then
        local input_flow = {}
        local output_flow = {}
        local precision = defines.flow_precision_index.five_seconds
        local function flow_count(name, category)
            local stat_name = type(name) == "string" and name or name.name
            if not stat_name then return 0 end
            local ok, flow = pcall(function()
                return stats.get_flow_count{
                    name = stat_name,
                    category = category,
                    precision_index = precision,
                }
            end)
            if ok and type(flow) == "number" then return flow end
            return 0
        end
        for name, _ in pairs(stats.input_counts) do
            local stat_name = type(name) == "string" and name or name.name
            local flow = flow_count(name, "input")
            if stat_name and flow > 0 then table.insert(input_flow, {name = stat_name, flow = flow}) end
        end
        for name, _ in pairs(stats.output_counts) do
            local stat_name = type(name) == "string" and name or name.name
            local flow = flow_count(name, "output")
            if stat_name and flow > 0 then table.insert(output_flow, {name = stat_name, flow = flow}) end
        end
        if #input_flow > 0 then result.input_flow = input_flow end
        if #output_flow > 0 then result.output_flow = output_flow end
    end

    return result
end

function M.get_power_networks(x, y, radius)
    local surface = game.surfaces[1]
    local _, area = area_around(x, y, radius)
    local poles = surface.find_entities_filtered{
        type = "electric-pole",
        area = area,
        force = "player",
    }
    local networks = {}
    for _, pole in pairs(poles) do
        local network_id = pole.electric_network_id
        if network_id then
            if not networks[network_id] then
                networks[network_id] = {
                    network_id = network_id,
                    pole_count = 0,
                    poles = {},
                }
            end
            networks[network_id].pole_count = networks[network_id].pole_count + 1
            if #networks[network_id].poles < 3 then
                table.insert(networks[network_id].poles, {
                    name = pole.name,
                    position = pos_table(pole.position),
                })
            end
        end
    end

    local result = {}
    for _, data in pairs(networks) do
        table.insert(result, data)
    end
    return result
end

function M.find_power_issues(x, y, radius)
    local surface = game.surfaces[1]
    local r, area = area_around(x, y, radius)
    local coverage, poles = build_power_coverage(surface, area, x, y, r, false)
    local result = {
        unpowered_entities = {},
        low_power_entities = {},
        suggested_actions = {},
    }

    for _, entity_type in pairs(POWER_ISSUE_CONSUMER_TYPES) do
        local entities = surface.find_entities_filtered{
            area = area,
            type = entity_type,
            force = "player",
        }
        for _, ent in pairs(entities) do
            if entity_uses_electricity(ent) then
                local status = raw_entity_status(ent)
                local ex, ey = math.floor(ent.position.x), math.floor(ent.position.y)
                local key = ex .. "," .. ey
                if status == defines.entity_status.no_power then
                    table.insert(result.unpowered_entities, {
                        unit_number = ent.unit_number,
                        name = ent.name,
                        x = ent.position.x,
                        y = ent.position.y,
                        in_coverage = coverage[key] ~= nil,
                    })
                    if not coverage[key] then
                        table.insert(result.suggested_actions, "Place pole near (" .. ex .. ", " .. ey .. ") to power " .. ent.name)
                    else
                        table.insert(result.suggested_actions, ent.name .. " at (" .. ex .. ", " .. ey .. ") is in coverage but has no power - check generator capacity")
                    end
                elseif status == defines.entity_status.low_power then
                    table.insert(result.low_power_entities, {
                        unit_number = ent.unit_number,
                        name = ent.name,
                        x = ent.position.x,
                        y = ent.position.y,
                    })
                    table.insert(result.suggested_actions, ent.name .. " at (" .. ex .. ", " .. ey .. ") has low power - add more generators")
                end
            end
        end
    end

    result.summary = {
        unpowered_count = #result.unpowered_entities,
        low_power_count = #result.low_power_entities,
        pole_count = #poles,
    }
    local original_action_count = #result.suggested_actions
    if original_action_count > 10 then
        local limited = {}
        for i = 1, 10 do
            limited[i] = result.suggested_actions[i]
        end
        result.suggested_actions = limited
        result.summary.more_issues = original_action_count - 10
    end
    return result
end

function M.get_power_coverage(x, y, radius)
    local surface = game.surfaces[1]
    local r, area = area_around(x, y, radius)
    local coverage, _, poles, networks = build_power_coverage(surface, area, x, y, r, true)
    return {
        poles = poles,
        coverage = coverage,
        networks = networks,
    }
end

function M.get_alerts(x, y, radius)
    local surface = game.surfaces[1]
    local _, area = area_around(x, y, radius)
    local alerts = {}

    for _, entity_type in pairs(POWER_ISSUE_CONSUMER_TYPES) do
        local entities = surface.find_entities_filtered{
            area = area,
            type = entity_type,
            force = "player",
        }
        for _, ent in pairs(entities) do
            if entity_uses_electricity(ent) then
                local status = raw_entity_status(ent)
                if status == defines.entity_status.no_power then
                    table.insert(alerts, {
                        type = "no_power",
                        entity_name = ent.name,
                        position = pos_table(ent.position),
                        unit_number = ent.unit_number,
                    })
                elseif status == defines.entity_status.low_power then
                    table.insert(alerts, {
                        type = "low_power",
                        entity_name = ent.name,
                        position = pos_table(ent.position),
                        unit_number = ent.unit_number,
                    })
                end
            end
        end
    end

    local drills = surface.find_entities_filtered{type = "mining-drill", area = area, force = "player"}
    for _, drill in pairs(drills) do
        if drill.mining_target == nil and raw_entity_status(drill) == defines.entity_status.no_minable_resources then
            table.insert(alerts, {
                type = "empty_drill",
                entity_name = drill.name,
                position = pos_table(drill.position),
                unit_number = drill.unit_number,
            })
        end
    end

    local fuel_entities = surface.find_entities_filtered{
        area = area,
        force = "player",
        type = {"furnace", "boiler"},
    }
    for _, entity in pairs(fuel_entities) do
        if entity.burner then
            local fuel_inv = entity.get_fuel_inventory()
            if fuel_inv and fuel_inv.is_empty() then
                table.insert(alerts, {
                    type = "no_fuel",
                    entity_name = entity.name,
                    position = pos_table(entity.position),
                    unit_number = entity.unit_number,
                })
            end
        end
    end

    local assemblers = surface.find_entities_filtered{type = "assembling-machine", area = area, force = "player"}
    for _, assembler in pairs(assemblers) do
        local status = raw_entity_status(assembler)
        if status == defines.entity_status.no_ingredients then
            local recipe = assembler.get_recipe()
            table.insert(alerts, {
                type = "no_ingredients",
                entity_name = assembler.name,
                position = pos_table(assembler.position),
                unit_number = assembler.unit_number,
                recipe = recipe and recipe.name or nil,
            })
        end
    end

    local enemies = surface.find_entities_filtered{
        force = "enemy",
        area = area,
        limit = 10,
    }
    for _, enemy in pairs(enemies) do
        table.insert(alerts, {
            type = "enemy_nearby",
            entity_name = enemy.name,
            position = pos_table(enemy.position),
            health = enemy.health,
        })
    end
    return alerts
end

function M.plan_steam_power(character, water_x1, water_y1, water_x2, water_y2, target_x, target_y)
    if not (character and character.valid) then
        return {success = false, error = "no character; spawn first", blockers = {"no_character"}}
    end
    local surface = character.surface
    local force = character.force
    local water_area = normalize_area(water_x1, water_y1, water_x2, water_y2)
    local target = pos(target_x, target_y)
    local diagnostic_center = pos(
        (water_area.left_top.x + water_area.right_bottom.x) / 2,
        (water_area.left_top.y + water_area.right_bottom.y) / 2
    )
    local diagnostic_corner_radius = math.max(
        math.sqrt(distance_sq(diagnostic_center, water_area.left_top)),
        math.sqrt(distance_sq(diagnostic_center, water_area.right_bottom))
    )
    local diagnostic_radius = math.max(
        24,
        math.ceil(diagnostic_corner_radius) + 16,
        math.ceil(math.sqrt(distance_sq(diagnostic_center, target))) + 16
    )
    local existing_diagnostic = M.diagnose_steam_power(
        diagnostic_center.x,
        diagnostic_center.y,
        diagnostic_radius
    )
    if existing_diagnostic.has_existing_plant then
        return {
            success = false,
            placement_success = false,
            water_area = water_area,
            target = target,
            checked = 0,
            pump_candidates = 0,
            missing_items = {},
            plan = nil,
            existing_plant = existing_diagnostic,
            blockers = {
                {
                    type = "existing_steam_power_found",
                    message = "Existing steam-power entities found in the requested area; diagnose and repair them before planning a rebuild.",
                },
            },
            guidance = "Existing steam power was found. Use existing_plant.issues and suggested_actions, then call diagnose_steam_power and get_power_status again before rebuilding.",
        }
    end

    local search_pad = 4
    local pump_candidates = {}
    local checked = 0

    for x = math.floor(water_area.left_top.x) - search_pad, math.ceil(water_area.right_bottom.x) + search_pad do
        for y = math.floor(water_area.left_top.y) - search_pad, math.ceil(water_area.right_bottom.y) + search_pad do
            for _, dir in ipairs(DIRECTIONS) do
                checked = checked + 1
                local pump_pos = pos(x, y)
                local allowed = can_place(surface, force, "offshore-pump", pump_pos, dir.value)
                if allowed then
                    table.insert(pump_candidates, {
                        position = pump_pos,
                        direction = dir.value,
                        direction_name = dir.name,
                        dir = dir,
                        score = distance_sq(pump_pos, target),
                    })
                end
            end
        end
    end

    table.sort(pump_candidates, function(a, b)
        if a.score == b.score then return a.direction < b.direction end
        return a.score < b.score
    end)

    local best_plan = nil
    local first_blocked_plan = nil
    for _, pump in ipairs(pump_candidates) do
        local land_dir = opposite_direction(pump.dir)
        for _, layout in ipairs(candidate_layouts(pump, land_dir)) do
            local plan = build_plan(surface, force, pump, layout, target)
            if plan.success then
                best_plan = plan
                break
            end
            if not first_blocked_plan then first_blocked_plan = plan end
        end
        if best_plan then break end
    end

    local selected_plan = best_plan or first_blocked_plan
    local missing = {}
    if selected_plan then
        missing = missing_items(character, #selected_plan.pipes, #selected_plan.pole_positions)
    end

    local result = {
        success = best_plan ~= nil and #missing == 0,
        placement_success = best_plan ~= nil,
        water_area = water_area,
        target = target,
        checked = checked,
        pump_candidates = #pump_candidates,
        missing_items = missing,
        plan = selected_plan,
        blockers = {},
        guidance = "Place components using plan.*.place_args, insert fuel into fuel_target, then call diagnose_steam_power and get_power_status.",
    }

    if not best_plan then
        if #pump_candidates == 0 then
            table.insert(result.blockers, {
                type = "no_offshore_pump_placement",
                message = "No Factorio-valid offshore pump placement found near the supplied water area.",
            })
        elseif first_blocked_plan then
            result.blockers = first_blocked_plan.blockers
        end
    end

    return result
end

return M

