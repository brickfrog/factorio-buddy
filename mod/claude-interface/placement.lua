local characters = require("characters")
local entities = require("entities")
local resource_policy = require("resource_policy")

local M = {}

local function pos_table(pos)
    if not pos then return nil end
    return {x = pos.x, y = pos.y}
end

local function placement_entity_result(entity)
    local result = {
        unit_number = entity.unit_number,
        name = entity.name,
        type = entity.type,
        entity_type = entity.type,
        position = pos_table(entity.position),
        direction = entity.direction,
        health = entity.health,
        force = entity.force and entity.force.name or nil,
    }
    if entity.type == "inserter" then
        result.pickup_position = pos_table(entity.pickup_position)
        result.drop_position = pos_table(entity.drop_position)
    end
    return result
end

local function bounding_box_table(entity)
    if not (entity and entity.valid and entity.bounding_box) then return nil end
    return {
        left_top = pos_table(entity.bounding_box.left_top),
        right_bottom = pos_table(entity.bounding_box.right_bottom),
    }
end

local function entity_summary(entity)
    if not (entity and entity.valid) then return nil end
    return {
        unit_number = entity.unit_number,
        name = entity.name,
        type = entity.type,
        entity_type = entity.type,
        position = pos_table(entity.position),
        direction = entity.direction,
        force = entity.force and entity.force.name or nil,
        bounding_box = bounding_box_table(entity),
    }
end

local function placement_area(entity_name, position, margin)
    local proto = prototypes.entity[entity_name]
    margin = margin or 0.05
    if not (proto and proto.collision_box) then
        return {
            {position[1] - 0.5 - margin, position[2] - 0.5 - margin},
            {position[1] + 0.5 + margin, position[2] + 0.5 + margin},
        }
    end

    local cb = proto.collision_box
    return {
        {position[1] + cb.left_top.x - margin, position[2] + cb.left_top.y - margin},
        {position[1] + cb.right_bottom.x + margin, position[2] + cb.right_bottom.y + margin},
    }
end

local function area_table(area)
    if not area then return nil end
    return {
        left_top = {x = area[1][1], y = area[1][2]},
        right_bottom = {x = area[2][1], y = area[2][2]},
    }
end

local function boxes_overlap_area(area, box)
    if not (area and box) then return false end
    return area[1][1] < box.right_bottom.x
        and area[2][1] > box.left_top.x
        and area[1][2] < box.right_bottom.y
        and area[2][2] > box.left_top.y
end

local function point_overlaps_area(point, area)
    if not (point and area) then return false end
    return point.x > area[1][1]
        and point.x < area[2][1]
        and point.y > area[1][2]
        and point.y < area[2][2]
end

local function areas_overlap(a, b)
    if not (a and b) then return false end
    return a[1][1] < b[2][1]
        and a[2][1] > b[1][1]
        and a[1][2] < b[2][2]
        and a[2][2] > b[1][2]
end

local function character_standing_area(x, y)
    return {{x - 0.3, y - 0.3}, {x + 0.3, y + 0.3}}
end

local function character_can_stand_at(surface, force, x, y)
    local ok, can_place_or_error = pcall(function()
        return surface.can_place_entity{
            name = "character",
            position = {x, y},
            force = force,
            build_check_type = defines.build_check_type.manual,
        }
    end)
    if ok then return can_place_or_error == true, nil end
    return false, tostring(can_place_or_error)
end

local function character_placement_blocker(character, entity_name, position)
    if not (character and character.valid and character.bounding_box) then return nil end
    local area = placement_area(entity_name, position, 0.05)
    if not boxes_overlap_area(area, character.bounding_box) then return nil end

    local summary = entity_summary(character)
    if summary then
        summary.type = "character"
        summary.entity_type = "character"
        summary.blocker_type = "agent_character"
        summary.message = "Requested placement footprint overlaps the agent character."
        summary.placement_area = area_table(area)
    end
    return summary
end

local function is_mining_drill(entity_name)
    return type(entity_name) == "string" and string.find(entity_name, "mining-drill", 1, true) ~= nil
end

local function direction_name(direction)
    if direction == 0 then return "North" end
    if direction == 4 then return "East" end
    if direction == 8 then return "South" end
    if direction == 12 then return "West" end
    return "Unknown"
end

local function direction_arg_name(direction)
    if direction == 0 then return "north" end
    if direction == 4 then return "east" end
    if direction == 8 then return "south" end
    if direction == 12 then return "west" end
    return "north"
end

local function direction_vector(direction)
    if direction == 0 then return {x = 0, y = -1} end
    if direction == 4 then return {x = 1, y = 0} end
    if direction == 8 then return {x = 0, y = 1} end
    if direction == 12 then return {x = -1, y = 0} end
    return {x = 0, y = -1}
end

local function normalize_direction(direction)
    if type(direction) == "number" then
        if direction == 0 or direction == 4 or direction == 8 or direction == 12 then
            return direction
        end
        return 0
    end
    if type(direction) == "string" then
        local value = string.lower(direction)
        if value == "north" or value == "n" or value == "0" then return 0 end
        if value == "east" or value == "e" or value == "4" then return 4 end
        if value == "south" or value == "s" or value == "8" then return 8 end
        if value == "west" or value == "w" or value == "12" then return 12 end
    end
    return nil
end

local function entity_tile_dimensions(entity_name)
    local proto = prototypes.entity[entity_name]
    if not proto then return 1, 1 end
    if proto.tile_width and proto.tile_height then
        return proto.tile_width, proto.tile_height
    end
    if not proto.collision_box then return 1, 1 end
    local cb = proto.collision_box
    local width = math.ceil(cb.right_bottom.x - cb.left_top.x)
    local height = math.ceil(cb.right_bottom.y - cb.left_top.y)
    return math.max(1, width), math.max(1, height)
end

local function entity_tile_size(entity_name)
    local width, height = entity_tile_dimensions(entity_name)
    return math.max(1, width, height)
end

local function entity_tile_position(entity_name, tile_x, tile_y)
    local width, height = entity_tile_dimensions(entity_name)
    local x_offset = width % 2 == 1 and 0.5 or 0.0
    local y_offset = height % 2 == 1 and 0.5 or 0.0
    return {tile_x + x_offset, tile_y + y_offset}
end

local function mining_drill_output_tile(entity_name, position, direction)
    local size = entity_tile_size(entity_name)
    local half = math.floor(size / 2)
    local cx = math.floor(position[1])
    local cy = math.floor(position[2])

    if direction == 0 then
        return {x = cx - 1, y = cy - half - 1}
    elseif direction == 4 then
        return {x = cx + half, y = cy - 1}
    elseif direction == 8 then
        return {x = cx, y = cy + half}
    elseif direction == 12 then
        return {x = cx - half - 1, y = cy}
    end
    return {x = cx + half, y = cy - 1}
end

local function resources_on_tile(surface, x, y)
    local resources = {}
    local seen = {}
    local found = surface.find_entities_filtered{
        area = {{x, y}, {x + 1, y + 1}},
        type = "resource",
    }
    for _, resource in pairs(found) do
        if resource.valid and not seen[resource.name] then
            seen[resource.name] = true
            table.insert(resources, resource.name)
        end
    end
    table.sort(resources)
    return resources
end

local function inspect_placement_blockers(surface, entity_name, position, direction)
    local blockers = {}
    local area = placement_area(entity_name, position, 0.05)
    for _, entity in pairs(surface.find_entities_filtered{area = area}) do
        if entity.valid and entity.type ~= "item-entity" then
            local summary = entity_summary(entity)
            if summary then table.insert(blockers, summary) end
            if #blockers >= 8 then break end
        end
    end

    if #blockers == 0 then return nil end

    local details = {
        occupied_by = blockers[1],
        blockers = blockers,
    }
    if blockers[1].name == entity_name then
        details.recommended_action = "rotate_entity"
        if blockers[1].unit_number then
            details.rotate_entity = {
                unit_number = blockers[1].unit_number,
                direction = direction,
            }
        end
    end
    return details
end

local function placement_diagnostics(surface, force, entity_name, position, direction, character)
    local details = inspect_placement_blockers(surface, entity_name, position, direction) or {}
    local character_blocker = character_placement_blocker(character, entity_name, position)
    if character_blocker then
        details.blockers = details.blockers or {}
        table.insert(details.blockers, 1, character_blocker)
        details.occupied_by = character_blocker
        details.character_overlap = true
        details.recommended_action = "walk_to_clear_placement"
        details.guidance = "Move the agent outside the proposed build footprint before placing this entity."
    end
    if entity_name == "transport-belt" and not details.recommended_action then
        details.recommended_action = "route_belt"
        details.guidance = "The requested belt tile is blocked. Route the complete source-to-destination connection with route_belt; do not place an unrelated nearby belt."
    end

    for _, _ in pairs(details) do
        return details
    end
    return nil
end

local function mining_drill_output_diagnostics(surface, force, entity_name, position, direction, character)
    if not is_mining_drill(entity_name) then return nil end

    local tile = mining_drill_output_tile(entity_name, position, direction)
    local belt_position = {tile.x, tile.y}
    local character_blocker = character_placement_blocker(character, "transport-belt", belt_position)
    local ok, can_place_or_error = pcall(function()
        return surface.can_place_entity{
            name = "transport-belt",
            position = belt_position,
            direction = direction,
            force = force,
            build_check_type = defines.build_check_type.script,
        }
    end)
    local resources = resources_on_tile(surface, tile.x, tile.y)
    local blockers = nil
    if character_blocker then
        blockers = {character_blocker}
    elseif not ok or can_place_or_error ~= true then
        local details = placement_diagnostics(surface, force, "transport-belt", belt_position, direction, character)
        if details then blockers = details.blockers end
    end

    local belt_can_place = ok and can_place_or_error == true and not character_blocker
    local output_clear = belt_can_place and #resources == 0
    local diagnostic = {
        belt_tile = tile,
        belt_direction = direction,
        belt_direction_name = direction_name(direction),
        belt_can_place = belt_can_place,
        output_clear = output_clear,
        overlapping_resources = resources,
        description = "Place transport-belt at tile ("
            .. tostring(tile.x)
            .. ", "
            .. tostring(tile.y)
            .. ") facing "
            .. direction_name(direction)
            .. " to catch drill output.",
    }
    if character_blocker then
        diagnostic.error = "Transport belt output tile overlaps the agent character."
    elseif not ok then
        diagnostic.error = tostring(can_place_or_error)
    elseif can_place_or_error ~= true then
        diagnostic.error = "Transport belt cannot be placed on drill output tile."
    end
    if blockers then diagnostic.blockers = blockers end
    if not belt_can_place then
        diagnostic.warning = "Drill output tile cannot accept a transport belt; prefer another placement or rotate the drill."
    elseif #resources > 0 then
        diagnostic.warning = "Drill output tile overlaps resource; prefer a patch-edge placement for belt/furnace routing."
    end
    return diagnostic
end

local function count_matching_resources(surface, resource_name, area)
    local resources = surface.find_entities_filtered{
        area = area,
        type = "resource",
        name = resource_name,
    }
    local total_amount = 0
    for _, resource in pairs(resources) do
        total_amount = total_amount + (resource.amount or 0)
    end
    return #resources, total_amount
end

local function resource_summary(resources)
    if #resources == 0 then return nil end
    local min_x = resources[1].position.x
    local max_x = resources[1].position.x
    local min_y = resources[1].position.y
    local max_y = resources[1].position.y
    local total_amount = 0
    for _, resource in pairs(resources) do
        min_x = math.min(min_x, resource.position.x)
        max_x = math.max(max_x, resource.position.x)
        min_y = math.min(min_y, resource.position.y)
        max_y = math.max(max_y, resource.position.y)
        total_amount = total_amount + (resource.amount or 0)
    end
    return {
        tile_count = #resources,
        total_amount = total_amount,
        bounding_box = {
            left_top = {x = min_x, y = min_y},
            right_bottom = {x = max_x, y = max_y},
        },
        center = {
            x = (min_x + max_x) / 2,
            y = (min_y + max_y) / 2,
        },
    }
end

local function add_missing_item(result, item_name, available, required)
    if available >= required then return end
    result.missing_items[item_name] = {
        available = available,
        required = required,
    }
end

local function placement_failure(entity_name, position, direction, inventory_count, can_place, error, details)
    local result = {
        success = false,
        error = error,
        entity = entity_name,
        position = {x = position[1], y = position[2]},
        direction = direction,
        inventory_count = inventory_count,
        can_place = can_place,
    }
    if details then
        for key, value in pairs(details) do
            result[key] = value
        end
    end
    return result
end

local function add_resource_policy_details(target, policy)
    target.policy_allowed = policy.policy_allowed
    target.preserves_resource_patch = policy.preserves_resource_patch
    target.resource_footprint = policy.footprint
    target.overlapping_resources = policy.overlapping_resources
    target.overlapping_resource_tiles = policy.overlapping_resource_tiles
    target.overlapping_resource_amount = policy.overlapping_resource_amount
    target.resource_overlap = policy.resource_overlap
    target.resource_overlap_tile_count = policy.resource_overlap_tile_count
    target.resource_advisory = policy.advisory
    target.warning = policy.warning
    target.guidance = policy.guidance
    target.extractor_exception = policy.extractor_exception
    if not policy.policy_allowed then
        target.error_kind = policy.error_kind
        target.resource_policy_error = policy.error
    end
end

local function placement_candidate(surface, force, entity_name, position, direction, character, target)
    local character_blocker = character_placement_blocker(character, entity_name, position)
    local footprint = placement_area(entity_name, position, 0.0)
    local footprint_width = footprint[2][1] - footprint[1][1]
    local footprint_height = footprint[2][2] - footprint[1][2]
    local diagnostic = {
        entity = entity_name,
        position = {x = position[1], y = position[2]},
        direction = direction,
        direction_name = direction_name(direction),
        footprint = area_table(footprint),
        footprint_size = {width = footprint_width, height = footprint_height},
        character_overlap = character_blocker ~= nil,
        avoids_character = character_blocker == nil,
    }
    local policy = resource_policy.inspect(surface, entity_name, position, direction)
    add_resource_policy_details(diagnostic, policy)
    if target then
        diagnostic.distance = math.sqrt(
            (position[1] - target[1]) * (position[1] - target[1])
                + (position[2] - target[2]) * (position[2] - target[2])
        )
    end
    if character and character.valid then
        diagnostic.distance_from_character = math.sqrt(
            (position[1] - character.position.x) * (position[1] - character.position.x)
                + (position[2] - character.position.y) * (position[2] - character.position.y)
        )
    end
    local stand_candidates = {}
    local stand_total = 0
    if character and character.valid then
        local center = {x = position[1], y = position[2]}
        local seen = {}
        for r = 1, 8 do
            for dx = -r, r do
                for dy = -r, r do
                    if math.abs(dx) == r or math.abs(dy) == r then
                        local sx = math.floor(center.x) + dx + 0.5
                        local sy = math.floor(center.y) + dy + 0.5
                        local point = {x = sx, y = sy}
                        local key = tostring(sx) .. "," .. tostring(sy)
                        if not seen[key]
                            and not point_overlaps_area(point, footprint)
                            and not areas_overlap(character_standing_area(sx, sy), footprint)
                        then
                            seen[key] = true
                            local can_stand, stand_error = character_can_stand_at(surface, force, sx, sy)
                            if can_stand then
                                stand_total = stand_total + 1
                                if #stand_candidates < 5 then
                                    table.insert(stand_candidates, {
                                        position = point,
                                        distance_from_placement = math.sqrt((sx - center.x) * (sx - center.x) + (sy - center.y) * (sy - center.y)),
                                        distance_from_character = math.sqrt((sx - character.position.x) * (sx - character.position.x) + (sy - character.position.y) * (sy - character.position.y)),
                                    })
                                end
                            elseif stand_error and not diagnostic.standing_error then
                                diagnostic.standing_error = stand_error
                            end
                        end
                    end
                end
            end
        end
        table.sort(stand_candidates, function(a, b)
            if a.distance_from_placement == b.distance_from_placement then
                return a.distance_from_character < b.distance_from_character
            end
            return a.distance_from_placement < b.distance_from_placement
        end)
        diagnostic.post_placement = {
            has_clear_standing_position = #stand_candidates > 0,
            would_trap_agent = #stand_candidates == 0,
            nearest_clear_standing_position = (#stand_candidates > 0) and stand_candidates[1].position or nil,
            standing_candidates = stand_candidates,
            total_standing_candidates = stand_total,
        }
        diagnostic.can_place_and_keep_working = false
    end
    if character_blocker then
        diagnostic.factorio_allowed = false
        diagnostic.error = "Placement overlaps agent character"
        diagnostic.blockers = {character_blocker}
        diagnostic.occupied_by = character_blocker
        diagnostic.recommended_action = "walk_to_clear_placement"
        return diagnostic
    end

    local ok, can_place_or_error = pcall(function()
        return surface.can_place_entity{
            name = entity_name,
            position = position,
            direction = direction,
            force = force,
            build_check_type = defines.build_check_type.manual,
        }
    end)
    diagnostic.factorio_allowed = ok and can_place_or_error == true
    if not ok then
        diagnostic.error = tostring(can_place_or_error)
    elseif can_place_or_error ~= true then
        diagnostic.error = "Factorio cannot place entity here"
        local details = placement_diagnostics(surface, force, entity_name, position, direction, character)
        if details then
            diagnostic.blockers = details.blockers
            diagnostic.occupied_by = details.occupied_by
            diagnostic.recommended_action = details.recommended_action
        end
    end

    local output = mining_drill_output_diagnostics(surface, force, entity_name, position, direction, character)
    if output then
        diagnostic.output = output
        diagnostic.output_buildable = output.belt_can_place
        diagnostic.output_clear = output.output_clear
        diagnostic.output_blocked = output.belt_can_place ~= true
        diagnostic.output_usable = output.belt_can_place == true
        if output.warning then diagnostic.output_warning = output.warning end
    else
        diagnostic.output_usable = true
    end

    local has_clear_stand = true
    if diagnostic.post_placement then
        has_clear_stand = diagnostic.post_placement.has_clear_standing_position == true
    end
    diagnostic.allowed = diagnostic.factorio_allowed == true and diagnostic.policy_allowed == true
    diagnostic.can_place_and_keep_working = diagnostic.allowed == true
        and diagnostic.avoids_character == true
        and has_clear_stand
        and diagnostic.output_usable == true

    return diagnostic
end

function M.build_edge_miner(agent_id, resource_name, center_x, center_y, radius, drill_name, limit)
    local character = characters.find(agent_id)
    radius = math.max(1, math.min(40, math.floor(radius or 25)))
    limit = math.max(1, math.min(50, math.floor(limit or 10)))
    drill_name = drill_name or "burner-mining-drill"

    local result = {
        success = false,
        dry_run = true,
        resource_name = resource_name,
        drill_name = drill_name,
        target_area = {
            center = {x = center_x, y = center_y},
            radius = radius,
        },
        steps = {},
        after_place_steps = {},
        candidates = {},
        missing_items = {},
        blockers = {},
    }

    if not (character and character.valid) then
        table.insert(result.blockers, {
            type = "no_character",
            message = "No character for agent " .. tostring(agent_id) .. "; spawn first.",
        })
        result.next_action = "spawn_character"
        return result
    end

    if not prototypes.entity[drill_name] then
        table.insert(result.blockers, {
            type = "unknown_drill",
            message = "Unknown drill prototype: " .. tostring(drill_name),
        })
        result.next_action = "choose_valid_drill"
        return result
    end

    if not prototypes.entity[resource_name] then
        table.insert(result.blockers, {
            type = "unknown_resource",
            message = "Unknown resource prototype: " .. tostring(resource_name),
        })
        result.next_action = "choose_valid_resource"
        return result
    end

    local surface = character.surface
    local force = character.force
    local area = {
        {center_x - radius, center_y - radius},
        {center_x + radius, center_y + radius},
    }
    local resources = surface.find_entities_filtered{
        area = area,
        type = "resource",
        name = resource_name,
    }
    result.resource_patch = resource_summary(resources)
    if #resources == 0 then
        table.insert(result.blockers, {
            type = "resource_not_found",
            message = "No " .. tostring(resource_name) .. " resources found in target area.",
        })
        result.next_action = "find_nearest_resource"
        return result
    end

    local inv = character.get_main_inventory()
    local drill_count = inv and inv.get_item_count(drill_name) or 0
    local belt_count = inv and inv.get_item_count("transport-belt") or 0
    local coal_count = inv and inv.get_item_count("coal") or 0
    add_missing_item(result, drill_name, drill_count, 1)
    add_missing_item(result, "transport-belt", belt_count, 1)
    if string.find(drill_name, "burner", 1, true) then
        add_missing_item(result, "coal", coal_count, 1)
    end
    result.inventory = {
        [drill_name] = drill_count,
        ["transport-belt"] = belt_count,
        coal = coal_count,
    }

    local directions = {0, 4, 8, 12}
    local candidates = {}
    local checked = 0
    for dx = -radius, radius do
        for dy = -radius, radius do
            local position = {center_x + dx, center_y + dy}
            for _, dir in pairs(directions) do
                checked = checked + 1
                local character_blocker = character_placement_blocker(character, drill_name, position)
                local can_place_ok, can_place_or_error = pcall(function()
                    return surface.can_place_entity{
                        name = drill_name,
                        position = position,
                        direction = dir,
                        force = force,
                        build_check_type = defines.build_check_type.script,
                    }
                end)
                if not character_blocker and can_place_ok and can_place_or_error == true then
                    local drill_area = placement_area(drill_name, position, 0.0)
                    local resource_tiles, resource_amount = count_matching_resources(surface, resource_name, drill_area)
                    if resource_tiles > 0 then
                        local output = mining_drill_output_diagnostics(surface, force, drill_name, position, dir, character)
                        local distance = math.sqrt(dx * dx + dy * dy)
                        table.insert(candidates, {
                            entity = drill_name,
                            resource_name = resource_name,
                            position = {x = position[1], y = position[2]},
                            direction = dir,
                            direction_name = direction_name(dir),
                            distance = distance,
                            resource_tiles = resource_tiles,
                            resource_amount = resource_amount,
                            output = output,
                            output_buildable = output and output.belt_can_place == true or false,
                            output_clear = output and output.output_clear == true or false,
                        })
                    end
                end
            end
        end
    end

    table.sort(candidates, function(a, b)
        if a.output_clear ~= b.output_clear then
            return a.output_clear == true
        end
        if a.output_buildable ~= b.output_buildable then
            return a.output_buildable == true
        end
        if a.resource_tiles ~= b.resource_tiles then
            return a.resource_tiles > b.resource_tiles
        end
        if a.distance == b.distance then
            return a.direction < b.direction
        end
        return a.distance < b.distance
    end)

    result.checked = checked
    result.total_candidates = #candidates
    result.returned = math.min(#candidates, limit)
    result.truncated = #candidates > limit
    for i = 1, result.returned do
        table.insert(result.candidates, candidates[i])
    end

    if #candidates == 0 then
        table.insert(result.blockers, {
            type = "no_resource_backed_drill_placement",
            message = "No Factorio-valid " .. tostring(drill_name) .. " placement was found over " .. tostring(resource_name) .. ".",
        })
        result.next_action = "find_entity_placements"
        return result
    end

    local selected = nil
    for _, candidate in pairs(candidates) do
        if candidate.output_buildable then
            selected = candidate
            break
        end
    end

    if not selected then
        table.insert(result.blockers, {
            type = "no_buildable_output_tile",
            message = "Drill placements exist, but none have a buildable output belt tile.",
        })
        result.next_action = "clear_output_blocker_or_try_another_patch"
        return result
    end

    result.selected = selected
    result.steps = {
        {
            tool = "place_entity",
            tool_args = {
                entity_name = drill_name,
                x = selected.position.x,
                y = selected.position.y,
                direction = direction_arg_name(selected.direction),
            },
            description = "Place " .. drill_name .. " over " .. resource_name .. " with a buildable output; resource-free outputs are preferred.",
        },
        {
            tool = "place_entity",
            tool_args = {
                entity_name = "transport-belt",
                x = selected.output.belt_tile.x,
                y = selected.output.belt_tile.y,
                direction = direction_arg_name(selected.output.belt_direction),
            },
            description = "Place a transport belt on the drill output tile.",
        },
    }
    if string.find(drill_name, "burner", 1, true) then
        result.after_place_steps = {
            {
                operation = "bootstrap_burner_fuel",
                operation_args = {
                    unit_number = "<placed drill unit_number>",
                    item = "coal",
                    count = 5,
                    inventory_type = "fuel",
                },
                description = "After placing the drill, fuel the returned drill unit number.",
            },
        }
    end

    for _, _ in pairs(result.missing_items) do
        result.next_action = "collect_or_craft_missing_items"
        return result
    end

    result.success = true
    result.ready = true
    result.next_action = "execute_edge_miner_steps"
    result.guidance = "Execute steps in order, then fuel burner drills and call verify_production near the selected drill."
    return result
end

local function tile_center(tile)
    return {x = tile.x + 0.5, y = tile.y + 0.5}
end

local function area_contains_tile_center(area, tile)
    local center = tile_center(tile)
    return center.x >= area[1][1]
        and center.x <= area[2][1]
        and center.y >= area[1][2]
        and center.y <= area[2][2]
end

local function entity_on_tile(surface, entity_name, x, y)
    local found = surface.find_entities_filtered{
        area = {{x, y}, {x + 1, y + 1}},
        name = entity_name,
    }
    for _, entity in pairs(found) do
        if entity.valid then return entity end
    end
    return nil
end

local function can_place(surface, force, entity_name, position, direction, character)
    local character_blocker = character_placement_blocker(character, entity_name, position)
    if character_blocker then
        return false, "Placement overlaps agent character", {
            character_overlap = true,
            blockers = {character_blocker},
            occupied_by = character_blocker,
            recommended_action = "walk_to_clear_placement",
            guidance = "Move the agent outside the proposed build footprint before placing this entity.",
        }
    end
    local ok, value = pcall(function()
        return surface.can_place_entity{
            name = entity_name,
            position = position,
            direction = direction,
            force = force,
            build_check_type = defines.build_check_type.script,
        }
    end)
    if not ok then return false, tostring(value), nil end
    if value ~= true then return false, tostring(value), nil end
    local policy = resource_policy.inspect(surface, entity_name, position, direction)
    if not policy.policy_allowed then
        return false, policy.error, policy
    end
    return true, nil, policy
end

function M.build_direct_smelter(agent_id, drill_unit_number, output_x, output_y, output_direction, furnace_name, inserter_name, belt_name, radius)
    local character = characters.find(agent_id)
    furnace_name = furnace_name or "stone-furnace"
    inserter_name = inserter_name or "burner-inserter"
    belt_name = belt_name or "transport-belt"
    radius = math.max(2, math.min(12, math.floor(radius or 6)))

    local result = {
        success = false,
        dry_run = true,
        furnace_name = furnace_name,
        inserter_name = inserter_name,
        belt_name = belt_name,
        steps = {},
        after_place_steps = {},
        missing_items = {},
        blockers = {},
        candidates = {},
    }

    if not (character and character.valid) then
        table.insert(result.blockers, {
            type = "no_character",
            message = "No character for agent " .. tostring(agent_id) .. "; spawn first.",
        })
        result.next_action = "spawn_character"
        return result
    end

    if not prototypes.entity[furnace_name] then
        table.insert(result.blockers, {
            type = "unknown_furnace",
            message = "Unknown furnace prototype: " .. tostring(furnace_name),
        })
        result.next_action = "choose_valid_furnace"
        return result
    end
    if not prototypes.entity[inserter_name] then
        table.insert(result.blockers, {
            type = "unknown_inserter",
            message = "Unknown inserter prototype: " .. tostring(inserter_name),
        })
        result.next_action = "choose_valid_inserter"
        return result
    end
    if not prototypes.entity[belt_name] then
        table.insert(result.blockers, {
            type = "unknown_belt",
            message = "Unknown belt prototype: " .. tostring(belt_name),
        })
        result.next_action = "choose_valid_belt"
        return result
    end

    local surface = character.surface
    local force = character.force
    local output = nil
    local drill_unit = tonumber(drill_unit_number)
    if drill_unit and drill_unit > 0 then
        local drill = entities.find_by_unit_number(drill_unit)
        if not (drill and drill.valid) then
            table.insert(result.blockers, {
                type = "drill_not_found",
                message = "No valid drill entity with unit_number " .. tostring(drill_unit) .. ".",
            })
            result.next_action = "get_entities"
            return result
        end
        if not is_mining_drill(drill.name) then
            table.insert(result.blockers, {
                type = "not_a_mining_drill",
                message = "Entity " .. tostring(drill_unit) .. " is " .. tostring(drill.name) .. ", not a mining drill.",
            })
            result.next_action = "choose_mining_drill"
            return result
        end
        output = mining_drill_output_diagnostics(
            drill.surface,
            drill.force,
            drill.name,
            {drill.position.x, drill.position.y},
            drill.direction
        )
        result.source = {
            mode = "drill_unit",
            drill = entity_summary(drill),
        }
        surface = drill.surface
        force = drill.force
    else
        local direction = normalize_direction(output_direction)
        if output_x == nil or output_y == nil or direction == nil then
            table.insert(result.blockers, {
                type = "missing_output_reference",
                message = "Pass either drill_unit_number or output_x, output_y, and output_direction from execute_edge_miner or get_machine_belt_positions.",
            })
            result.next_action = "get_machine_belt_positions"
            return result
        end
        output = {
            belt_tile = {x = math.floor(output_x), y = math.floor(output_y)},
            belt_direction = direction,
            belt_direction_name = direction_name(direction),
            description = "Use the provided drill output tile as the smelter input belt.",
        }
        result.source = {
            mode = "output_position",
            output = output,
        }
    end

    local inv = character.get_main_inventory()
    local belt_count = inv and inv.get_item_count(belt_name) or 0
    local furnace_count = inv and inv.get_item_count(furnace_name) or 0
    local inserter_count = inv and inv.get_item_count(inserter_name) or 0
    local coal_count = inv and inv.get_item_count("coal") or 0
    result.inventory = {
        [belt_name] = belt_count,
        [furnace_name] = furnace_count,
        [inserter_name] = inserter_count,
        coal = coal_count,
    }

    local belt_tile = output.belt_tile
    local belt_position = entity_tile_position(belt_name, belt_tile.x, belt_tile.y)
    local existing_belt = entity_on_tile(surface, belt_name, belt_tile.x, belt_tile.y)
    local belt_can_place, belt_error, belt_details = can_place(surface, force, belt_name, belt_position, output.belt_direction, character)
    local belt_ready = belt_can_place or existing_belt ~= nil
    if not belt_ready then
        table.insert(result.blockers, {
            type = "output_belt_blocked",
            message = "Cannot place " .. tostring(belt_name) .. " on the drill output tile.",
            position = {x = belt_tile.x, y = belt_tile.y},
            error = belt_error,
            diagnostics = belt_details or placement_diagnostics(surface, force, belt_name, belt_position, output.belt_direction, character),
        })
        result.next_action = "move_drill_to_clear_output"
        return result
    end

    local directions = {0, 4, 8, 12}
    local candidates = {}
    for _, pickup_dir in pairs(directions) do
        local vec = direction_vector(pickup_dir)
        local inserter_pos = entity_tile_position(
            inserter_name,
            belt_tile.x - vec.x,
            belt_tile.y - vec.y
        )
        local drop_tile = {x = belt_tile.x - (vec.x * 2), y = belt_tile.y - (vec.y * 2)}
        local inserter_ok, inserter_error = can_place(surface, force, inserter_name, inserter_pos, pickup_dir, character)
        local inserter_area = placement_area(inserter_name, inserter_pos, 0.0)
        if inserter_ok then
            for dx = -radius, radius do
                for dy = -radius, radius do
                    local furnace_pos = entity_tile_position(
                        furnace_name,
                        belt_tile.x + dx,
                        belt_tile.y + dy
                    )
                    local furnace_area = placement_area(furnace_name, furnace_pos, 0.0)
                    if area_contains_tile_center(furnace_area, drop_tile) then
                        local furnace_ok, furnace_error = can_place(surface, force, furnace_name, furnace_pos, 0, character)
                        local entity_overlap = areas_overlap(furnace_area, inserter_area)
                        local distance = math.sqrt(dx * dx + dy * dy)
                        table.insert(candidates, {
                            distance = distance,
                            output_belt = {
                                entity = belt_name,
                                position = {x = belt_position[1], y = belt_position[2]},
                                direction = output.belt_direction,
                                direction_name = direction_name(output.belt_direction),
                                existing_unit_number = existing_belt and existing_belt.unit_number or nil,
                                can_place = belt_can_place,
                            },
                            input_inserter = {
                                entity = inserter_name,
                                position = {x = inserter_pos[1], y = inserter_pos[2]},
                                direction = pickup_dir,
                                direction_name = direction_name(pickup_dir),
                                pickup_tile = {x = belt_tile.x, y = belt_tile.y},
                                drop_tile = drop_tile,
                                can_place = inserter_ok,
                            },
                            furnace = {
                                entity = furnace_name,
                                position = {x = furnace_pos[1], y = furnace_pos[2]},
                                direction = 0,
                                direction_name = direction_name(0),
                                input_tile = drop_tile,
                                can_place = furnace_ok,
                                overlaps_input_inserter = entity_overlap,
                                error = entity_overlap and "Furnace footprint overlaps the input inserter." or furnace_error,
                            },
                            ready = furnace_ok and not entity_overlap,
                        })
                    end
                end
            end
        elseif #result.candidates < 8 then
            table.insert(result.candidates, {
                input_inserter = {
                    entity = inserter_name,
                    position = {x = inserter_pos[1], y = inserter_pos[2]},
                    direction = pickup_dir,
                    direction_name = direction_name(pickup_dir),
                    error = inserter_error,
                },
                ready = false,
            })
        end
    end

    table.sort(candidates, function(a, b)
        if a.ready ~= b.ready then
            return a.ready == true
        end
        if a.distance == b.distance then
            if a.input_inserter.position.x == b.input_inserter.position.x then
                return a.input_inserter.position.y < b.input_inserter.position.y
            end
            return a.input_inserter.position.x < b.input_inserter.position.x
        end
        return a.distance < b.distance
    end)

    result.total_candidates = #candidates
    result.returned = math.min(#candidates, 10)
    result.truncated = #candidates > result.returned
    for i = 1, result.returned do
        table.insert(result.candidates, candidates[i])
    end

    local selected = nil
    for _, candidate in pairs(candidates) do
        if candidate.ready then
            selected = candidate
            break
        end
    end

    if not selected then
        table.insert(result.blockers, {
            type = "no_direct_smelter_layout",
            message = "No valid adjacent belt-inserter-furnace layout was found from the drill output tile.",
            output = output,
        })
        result.next_action = "use_route_belt_or_move_drill"
        return result
    end

    result.selected = selected
    local steps = {}
    if existing_belt then
        if existing_belt.direction ~= output.belt_direction then
            table.insert(steps, {
                tool = "rotate_entity",
                tool_args = {
                    unit_number = existing_belt.unit_number,
                    direction = direction_arg_name(output.belt_direction),
                },
                description = "Rotate the existing output belt to carry ore away from the drill.",
            })
        end
    else
        table.insert(steps, {
            tool = "place_entity",
            tool_args = {
                entity_name = belt_name,
                x = selected.output_belt.position.x,
                y = selected.output_belt.position.y,
                direction = direction_arg_name(selected.output_belt.direction),
            },
            description = "Place the belt on the drill output tile.",
        })
    end
    table.insert(steps, {
        tool = "place_entity",
        tool_args = {
            entity_name = furnace_name,
            x = selected.furnace.position.x,
            y = selected.furnace.position.y,
            direction = direction_arg_name(selected.furnace.direction),
        },
        description = "Place the furnace where the input inserter drops ore.",
    })
    table.insert(steps, {
        tool = "place_entity",
        tool_args = {
            entity_name = inserter_name,
            x = selected.input_inserter.position.x,
            y = selected.input_inserter.position.y,
            direction = direction_arg_name(selected.input_inserter.direction),
        },
        description = "Place the inserter to pick up from the output belt and feed the furnace.",
    })
    result.steps = steps

    add_missing_item(result, belt_name, belt_count, existing_belt and 0 or 1)
    add_missing_item(result, furnace_name, furnace_count, 1)
    add_missing_item(result, inserter_name, inserter_count, 1)
    if furnace_name ~= "electric-furnace" or string.find(inserter_name, "burner", 1, true) then
        result.fuel_automation_required = true
        result.after_place_steps = {{
            tool = "repair_fuel_sustainability",
            tool_args = {
                x = belt_tile.x,
                y = belt_tile.y,
                radius = 12,
                dry_run = false,
            },
            description = "Plan and build a durable coal belt/inserter feed. Do not use repeated inventory inserts as the operating fuel path.",
        }}
    end

    result.verify_step = {
        tool = "verify_production",
        tool_args = {
            x1 = belt_tile.x - 4,
            y1 = belt_tile.y - 4,
            x2 = belt_tile.x + 4,
            y2 = belt_tile.y + 4,
        },
        description = "After placement and fueling, verify the drill, belt, inserter, and furnace are working.",
    }

    for _, _ in pairs(result.missing_items) do
        result.next_action = "collect_or_craft_missing_items"
        return result
    end

    result.success = true
    result.ready = true
    result.next_action = "execute_direct_smelter_steps"
    result.guidance = "Execute placement steps in order, automate any burner fuel input through belts/inserters, then run verify_step."
    return result
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

    local prototype = prototypes.entity[entity_name]
    if not prototype then
        return placement_failure(entity_name, position, direction, inventory_count, false, "Unknown entity prototype")
    end

    local surface = character.surface
    local reach_error = characters.require_position_reach(character, x, y, "build")
    if reach_error then
        reach_error.entity = entity_name
        reach_error.direction = direction
        return reach_error
    end
    local character_blocker = character_placement_blocker(character, entity_name, position)
    if character_blocker then
        return placement_failure(
            entity_name,
            position,
            direction,
            inventory_count,
            false,
            "Placement overlaps agent character",
            placement_diagnostics(surface, character.force, entity_name, position, direction, character)
        )
    end
    local can_place_ok, can_place_or_error = pcall(function()
        return surface.can_place_entity{
            name = entity_name,
            position = position,
            direction = direction,
            force = character.force,
            build_check_type = defines.build_check_type.manual,
        }
    end)
    local fast_replace_ok, can_fast_replace = pcall(function()
        return prototype.type == "splitter" and surface.can_fast_replace{
            name = entity_name,
            position = position,
            direction = direction,
            force = character.force,
        }
    end)
    local use_fast_replace = fast_replace_ok and can_fast_replace == true

    if (not can_place_ok or can_place_or_error ~= true) and not use_fast_replace then
        local blocker_details = placement_diagnostics(surface, character.force, entity_name, position, direction, character)
        return placement_failure(
            entity_name,
            position,
            direction,
            inventory_count,
            false,
            can_place_ok and "Cannot place entity here" or tostring(can_place_or_error),
            blocker_details
        )
    end

    local policy = resource_policy.inspect(surface, entity_name, position, direction)
    if not policy.policy_allowed then
        local details = {}
        add_resource_policy_details(details, policy)
        details.factorio_allowed = true
        details.allowed = false
        return placement_failure(
            entity_name,
            position,
            direction,
            inventory_count,
            false,
            policy.error,
            details
        )
    end

    local create_ok, created_or_error = pcall(function()
        return surface.create_entity{
            name = entity_name,
            position = position,
            direction = direction,
            force = character.force,
            fast_replace = use_fast_replace,
            character = use_fast_replace and character or nil,
            spill = true,
        }
    end)

    if not create_ok then
        return placement_failure(
            entity_name,
            position,
            direction,
            inventory_count,
            true,
            tostring(created_or_error),
            placement_diagnostics(surface, character.force, entity_name, position, direction, character)
        )
    end

    local entity = created_or_error
    if not entity then
        local blocker_details = placement_diagnostics(surface, character.force, entity_name, position, direction, character) or {}
        blocker_details.create_entity_nil_after_can_place = true
        return placement_failure(
            entity_name,
            position,
            direction,
            inventory_count,
            true,
            "create_entity returned nil after can_place_entity succeeded",
            blocker_details
        )
    end

    if entity.unit_number then
        storage.factorioctl_entities = storage.factorioctl_entities or {}
        storage.factorioctl_entities[entity.unit_number] = entity
    end
    inv.remove{name = entity_name, count = 1}
    local result = placement_entity_result(entity)
    result.fast_replaced = use_fast_replace
    add_resource_policy_details(result, policy)
    return result
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
    local reach_error = characters.require_position_reach(character, x, y, "build")
    if reach_error then
        reach_error.entity = entity_name
        reach_error.direction = direction
        return reach_error
    end
    local character_blocker = character_placement_blocker(character, entity_name, position)
    if character_blocker then
        return {
            success = false,
            error = "Placement overlaps agent character",
            entity = entity_name,
            position = {x = x, y = y},
            direction = direction,
            inventory_count = inventory_count,
            character_overlap = true,
            occupied_by = character_blocker,
            blockers = {character_blocker},
            recommended_action = "walk_to_clear_placement",
            guidance = "Move the agent outside the proposed build footprint before placing this entity.",
        }
    end
    local can_place = surface.can_place_entity{
        name = entity_name,
        position = position,
        direction = direction,
        force = character.force,
        build_check_type = defines.build_check_type.script,
    }

    if not can_place then
        return {error = "Cannot place underground belt here"}
    end

    local policy = resource_policy.inspect(surface, entity_name, position, direction)
    if not policy.policy_allowed then
        local result = {
            success = false,
            error = policy.error,
            entity = entity_name,
            position = {x = x, y = y},
            direction = direction,
            inventory_count = inventory_count,
            factorio_allowed = true,
            allowed = false,
        }
        add_resource_policy_details(result, policy)
        return result
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
    add_resource_policy_details(result, policy)
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

    local character_blocker = character_placement_blocker(character, entity_name, position)
    local ok, can_place_or_error = pcall(function()
        return character.surface.can_place_entity{
            name = entity_name,
            position = position,
            direction = direction,
            force = character.force,
            build_check_type = defines.build_check_type.manual,
        }
    end)

    if character_blocker then
        return {
            factorio_allowed = false,
            entity = entity_name,
            position = {x = x, y = y},
            direction = direction,
            inventory_count = inventory_count,
            item_in_inventory = inventory_count > 0,
            error = "Placement overlaps agent character",
            character_overlap = true,
            occupied_by = character_blocker,
            blockers = {character_blocker},
            recommended_action = "walk_to_clear_placement",
            guidance = "Move the agent outside the proposed build footprint before placing this entity.",
        }
    end

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

    local policy = resource_policy.inspect(character.surface, entity_name, position, direction)
    local result = {
        factorio_allowed = can_place_or_error == true,
        policy_allowed = policy.policy_allowed,
        allowed = can_place_or_error == true and policy.policy_allowed == true,
        entity = entity_name,
        position = {x = x, y = y},
        direction = direction,
        inventory_count = inventory_count,
        item_in_inventory = inventory_count > 0,
    }
    add_resource_policy_details(result, policy)
    if can_place_or_error == true and not policy.policy_allowed then
        result.error = policy.error
    end
    if can_place_or_error ~= true then
        result.error = "Factorio cannot place entity here"
        local blocker_details = placement_diagnostics(character.surface, character.force, entity_name, position, direction, character)
        if blocker_details then
            for key, value in pairs(blocker_details) do
                result[key] = value
            end
        end
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
                local character_blocker = character_placement_blocker(character, entity_name, position)
                local ok, can_place = pcall(function()
                    return surface.can_place_entity{
                        name = entity_name,
                        position = position,
                        direction = dir,
                        force = character.force,
                        build_check_type = defines.build_check_type.manual,
                    }
                end)
                if not character_blocker and ok and can_place == true then
                    local policy = resource_policy.inspect(surface, entity_name, position, dir)
                    if policy.policy_allowed then
                        local distance = math.sqrt(dx * dx + dy * dy)
                        local placement = {
                            entity = entity_name,
                            factorio_allowed = true,
                            policy_allowed = true,
                            allowed = true,
                            position = {x = position[1], y = position[2]},
                            direction = dir,
                            distance = distance,
                            inventory_count = inventory_count,
                            item_in_inventory = inventory_count > 0,
                        }
                        add_resource_policy_details(placement, policy)
                        local output = mining_drill_output_diagnostics(surface, character.force, entity_name, position, dir, character)
                        if output then
                            placement.output = output
                            placement.output_buildable = output.belt_can_place
                            placement.output_clear = output.output_clear
                            if output.warning then placement.output_warning = output.warning end
                        end
                        table.insert(placements, placement)
                    end
                end
            end
        end
    end

    table.sort(placements, function(a, b)
        local a_resource_overlap = a.resource_overlap == true
        local b_resource_overlap = b.resource_overlap == true
        if a_resource_overlap ~= b_resource_overlap then
            return a_resource_overlap == false
        end
        if a.output_clear ~= b.output_clear then
            return a.output_clear == true
        end
        if a.output_buildable ~= b.output_buildable then
            return a.output_buildable == true
        end
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

function M.plan_entity_placement_near(agent_id, entity_name, target_x, target_y, radius, limit)
    local character = characters.find(agent_id)
    radius = math.max(1, math.min(25, math.floor(radius or 8)))
    limit = math.max(1, math.min(50, math.floor(limit or 10)))
    local target = {target_x, target_y}
    local result = {
        success = false,
        dry_run = true,
        entity = entity_name,
        target = {x = target_x, y = target_y},
        radius = radius,
        checked = 0,
        placements = {},
        rejected_character_overlap = {},
        rejected_blocked = {},
        blockers = {},
    }

    if not (character and character.valid) then
        result.error = "no character for agent " .. tostring(agent_id) .. "; spawn first"
        result.next_action = "spawn_character"
        return result
    end

    if not prototypes.entity[entity_name] then
        result.error = "Unknown entity prototype"
        result.next_action = "choose_valid_entity"
        return result
    end

    result.character_position = pos_table(character.position)
    local inv = character.get_main_inventory()
    local inventory_count = inv and inv.get_item_count(entity_name) or 0
    result.inventory_count = inventory_count
    result.item_in_inventory = inventory_count > 0

    local directions = {0, 4, 8, 12}
    local surface = character.surface
    local force = character.force
    local accepted = {}
    for dx = -radius, radius do
        for dy = -radius, radius do
            local position = {target[1] + dx, target[2] + dy}
            for _, dir in pairs(directions) do
                result.checked = result.checked + 1
                local candidate = placement_candidate(surface, force, entity_name, position, dir, character, target)
                if candidate.can_place_and_keep_working then
                    candidate.tool = "place_entity"
                    candidate.tool_args = {
                        entity_name = entity_name,
                        x = candidate.position.x,
                        y = candidate.position.y,
                        direction = direction_arg_name(dir),
                    }
                    candidate.description = "Place "
                        .. entity_name
                        .. " near target without trapping the agent and with usable output."
                    table.insert(accepted, candidate)
                elseif candidate.character_overlap then
                    if #result.rejected_character_overlap < limit then
                        table.insert(result.rejected_character_overlap, candidate)
                    end
                elseif #result.rejected_blocked < limit then
                    table.insert(result.rejected_blocked, candidate)
                end
            end
        end
    end

    table.sort(accepted, function(a, b)
        local a_resource_overlap = a.resource_overlap == true
        local b_resource_overlap = b.resource_overlap == true
        if a_resource_overlap ~= b_resource_overlap then
            return a_resource_overlap == false
        end
        local a_stand = a.post_placement and a.post_placement.has_clear_standing_position == true or false
        local b_stand = b.post_placement and b.post_placement.has_clear_standing_position == true or false
        if a_stand ~= b_stand then
            return a_stand == true
        end
        if a.output_clear ~= b.output_clear then
            return a.output_clear == true
        end
        if a.output_buildable ~= b.output_buildable then
            return a.output_buildable == true
        end
        if a.distance == b.distance then
            if a.distance_from_character == b.distance_from_character then
                if a.position.x == b.position.x then
                    return a.position.y < b.position.y
                end
                return a.position.x < b.position.x
            end
            return a.distance_from_character > b.distance_from_character
        end
        return a.distance < b.distance
    end)

    for i = 1, math.min(#accepted, limit) do
        table.insert(result.placements, accepted[i])
    end
    result.total = #accepted
    result.returned = #result.placements
    result.truncated = #accepted > #result.placements

    if #accepted > 0 then
        result.success = true
        result.selected = accepted[1]
        result.steps = {accepted[1]}
        result.next_action = "execute_place_entity_step"
        result.guidance = "Use selected.tool_args with place_entity; selected.footprint shows the collision footprint, selected.post_placement.nearest_clear_standing_position is the closest work tile, and selected.can_place_and_keep_working confirms the placement avoids trapping/output-blocking the agent."
        return result
    end

    if #result.rejected_character_overlap > 0 then
        result.error = "Only nearby Factorio-valid placements overlap the agent character."
        result.next_action = "move_agent_or_call_unstuck"
        result.recommended_action = "walk_to_clear_placement"
        result.guidance = "Move the agent away from the requested build area, then call execute_entity_placement_near with dry_run=true again."
    elseif #result.rejected_blocked > 0 then
        result.error = "No nearby placement was both Factorio-valid and operationally safe for the agent."
        result.next_action = "expand_radius_or_use_edge_planner"
        result.guidance = "Review rejected_blocked for output_blocked or post_placement.would_trap_agent, then expand radius or choose a patch-edge placement."
    else
        result.error = "No Factorio-valid placement found near target."
        result.next_action = "expand_radius_or_clear_blockers"
    end
    return result
end

function M.place_ghost(agent_id, entity_name, x, y, direction)
    local character = characters.find(agent_id)
    if not (character and character.valid) then
        return {error = "no character for agent " .. tostring(agent_id) .. "; spawn first"}
    end

    local reach_error = characters.require_position_reach(character, x, y, "build")
    if reach_error then
        reach_error.entity = entity_name
        reach_error.direction = direction
        return reach_error
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

function M.rotate_entity(agent_id, unit_number, direction)
    local character = characters.find(agent_id)
    if not (character and character.valid) then
        return {
            success = false,
            error = "no character for agent " .. tostring(agent_id) .. "; spawn first",
            error_kind = "no_character",
        }
    end
    local entity = entities.find_by_unit_number(unit_number)
    if not entity then
        return {
            success = false,
            error = "Entity not found",
            unit_number = unit_number,
            direction = direction,
        }
    end

    local reach_error = characters.require_entity_reach(character, entity)
    if reach_error then return reach_error end

    if not entity.supports_direction then
        return {
            success = false,
            error = "Entity does not support rotation",
            unit_number = unit_number,
            name = entity.name,
            entity_type = entity.type,
            position = pos_table(entity.position),
            direction = entity.direction,
            requested_direction = direction,
        }
    end

    local previous_direction = entity.direction
    local rotation_policy = resource_policy.inspect_rotation(
        entity.surface,
        entity.name,
        entity.position,
        previous_direction,
        direction
    )
    if rotation_policy.policy_allowed ~= true then
        return {
            success = false,
            error_kind = rotation_policy.error_kind or "resource_destructive_rotation",
            error = rotation_policy.error or "Requested rotation could destructively revalidate live resource tiles",
            guidance = rotation_policy.guidance,
            unit_number = unit_number,
            name = entity.name,
            entity_type = entity.type,
            position = pos_table(entity.position),
            direction = previous_direction,
            requested_direction = direction,
            resource_policy = rotation_policy,
        }
    end

    local ok, err = pcall(function()
        entity.direction = direction
    end)
    if not ok then
        return {
            success = false,
            error = tostring(err),
            unit_number = unit_number,
            name = entity.name,
            entity_type = entity.type,
            position = pos_table(entity.position),
            direction = previous_direction,
            requested_direction = direction,
        }
    end

    local result = placement_entity_result(entity)
    result.success = true
    result.previous_direction = previous_direction
    result.requested_direction = direction
    return result
end

local function clone_filter_value(value)
    if type(value) ~= "table" then return value end
    local copy = {}
    for key, nested in pairs(value) do
        copy[key] = clone_filter_value(nested)
    end
    return copy
end

local function filter_name(filter)
    if type(filter) == "string" then return filter end
    if type(filter) == "table" then
        if type(filter.name) == "string" then return filter.name end
        if type(filter.name) == "table" and type(filter.name.name) == "string" then
            return filter.name.name
        end
    end
    return nil
end

local function filter_record(slot, filter)
    if filter == nil then return nil end
    local record = {
        slot = slot,
        name = filter_name(filter),
    }
    if type(filter) == "table" then
        local quality = filter.quality
        if quality == nil and type(filter.name) == "table" then quality = filter.name.quality end
        if quality ~= nil then
            record.quality = type(quality) == "table" and quality.name or quality
        end
        if filter.comparator ~= nil then record.comparator = filter.comparator end
    end
    return record
end

local function read_inserter_filter_state(entity, slot_count)
    local state = {
        filters = {},
        raw_filters = {},
    }
    local enabled_ok, enabled_or_error = pcall(function() return entity.use_filters end)
    if not enabled_ok then return nil, tostring(enabled_or_error) end
    state.filtering_enabled = enabled_or_error == true

    local mode_ok, mode_or_error = pcall(function() return entity.inserter_filter_mode end)
    if not mode_ok then return nil, tostring(mode_or_error) end
    state.mode = mode_or_error

    for slot = 1, slot_count do
        local filter_ok, filter_or_error = pcall(function() return entity.get_filter(slot) end)
        if not filter_ok then return nil, tostring(filter_or_error) end
        if filter_or_error ~= nil then
            state.raw_filters[slot] = clone_filter_value(filter_or_error)
            table.insert(state.filters, filter_record(slot, filter_or_error))
        end
    end
    return state, nil
end

local function inserter_filter_states_match(expected, actual)
    if not (expected and actual) then return false, "filter state was unavailable" end
    if expected.filtering_enabled ~= actual.filtering_enabled then
        return false, "filtering enabled-state was not restored"
    end
    if expected.mode ~= actual.mode then return false, "filter mode was not restored" end
    if #expected.filters ~= #actual.filters then
        return false, "filter count was not restored"
    end
    for index, wanted in ipairs(expected.filters) do
        local observed = actual.filters[index]
        if not observed
            or wanted.slot ~= observed.slot
            or wanted.name ~= observed.name
            or wanted.quality ~= observed.quality
            or wanted.comparator ~= observed.comparator
        then
            return false, "filter slot " .. tostring(wanted.slot) .. " was not restored"
        end
    end
    return true, nil
end

local function restore_inserter_filter_state(entity, slot_count, state)
    local write_ok, write_error = pcall(function()
        for slot = 1, slot_count do entity.set_filter(slot, nil) end
        if state.mode ~= nil then entity.inserter_filter_mode = state.mode end
        for slot, filter in pairs(state.raw_filters) do
            entity.set_filter(slot, clone_filter_value(filter))
        end
        entity.use_filters = state.filtering_enabled == true
    end)
    if not write_ok then return false, tostring(write_error), nil end

    local restored, read_error = read_inserter_filter_state(entity, slot_count)
    if not restored then
        return false, "rollback readback failed: " .. tostring(read_error), nil
    end
    local matches, mismatch = inserter_filter_states_match(state, restored)
    if not matches then return false, "rollback readback mismatch: " .. mismatch, restored end
    return true, nil, restored
end

local function held_stack_record(entity)
    local ok, stack = pcall(function() return entity.held_stack end)
    if not ok or not (stack and stack.valid_for_read) then return nil end
    local quality = nil
    local quality_ok, quality_value = pcall(function() return stack.quality end)
    if quality_ok and quality_value ~= nil then
        quality = type(quality_value) == "table" and quality_value.name or quality_value
    end
    return {
        name = stack.name,
        count = stack.count,
        quality = quality,
    }
end

local function item_with_quality(name, quality, count)
    local item = {name = name}
    if quality ~= nil then item.quality = quality end
    if count ~= nil then item.count = count end
    return item
end

local function allowed_item_lookup(allowed_items)
    local lookup = {}
    for _, item in ipairs(allowed_items) do lookup[item] = true end
    return lookup
end

local function evacuate_disallowed_held_stack(character, entity, allowed_items, held_before)
    local outcome = {
        required = false,
        succeeded = true,
        returned_count = 0,
    }
    if not held_before or #allowed_items == 0 then return outcome end
    if allowed_item_lookup(allowed_items)[held_before.name] then return outcome end

    outcome.required = true
    local inventory = character.get_main_inventory()
    if not (inventory and inventory.valid) then
        outcome.succeeded = false
        outcome.error = "Character has no valid main inventory for the disallowed held item"
        return outcome
    end

    local item = item_with_quality(held_before.name, held_before.quality, held_before.count)
    local capacity_ok, insertable = pcall(function()
        return inventory.get_insertable_count(item_with_quality(held_before.name, held_before.quality))
    end)
    if not capacity_ok then
        outcome.succeeded = false
        outcome.error = "Could not inspect character inventory capacity: " .. tostring(insertable)
        return outcome
    end
    if insertable < held_before.count then
        outcome.succeeded = false
        outcome.error = "Character inventory cannot accept the complete disallowed held stack"
        outcome.insertable_count = insertable
        return outcome
    end

    -- LuaInventory.insert accepts a LuaItemStack and copies its complete item
    -- data. Only clear the inserter hand after the whole stack is safely in the
    -- character inventory; no simulation tick can occur during this call.
    local held_stack = entity.held_stack
    local insert_ok, insert_result = pcall(function() return inventory.insert(held_stack) end)
    local inserted = insert_ok and tonumber(insert_result) or 0
    if not insert_ok or inserted ~= held_before.count then
        local removed = inserted > 0 and inventory.remove(item_with_quality(
            held_before.name,
            held_before.quality,
            inserted
        )) or 0
        outcome.succeeded = false
        outcome.error = insert_ok
            and "Character inventory accepted only part of the disallowed held stack"
            or "Could not return the disallowed held stack: " .. tostring(insert_result)
        outcome.inserted_count = inserted
        outcome.inventory_rollback_succeeded = removed == inserted
        return outcome
    end

    local clear_ok, clear_error = pcall(function() held_stack.clear() end)
    if not clear_ok or held_stack.valid_for_read then
        local removed = inventory.remove(item)
        outcome.succeeded = false
        outcome.error = clear_ok
            and "Inserter still held the disallowed item after clearing its hand"
            or "Could not clear the disallowed held item: " .. tostring(clear_error)
        outcome.inserted_count = inserted
        outcome.inventory_rollback_succeeded = removed == inserted
        return outcome
    end

    outcome.returned_count = inserted
    return outcome
end

local function validate_allowed_items(allowed_items)
    if type(allowed_items) ~= "table" then
        return nil, "allowed_items must be a dense array"
    end
    local length = #allowed_items
    local key_count = 0
    local normalized = {}
    local seen = {}
    for key, item in pairs(allowed_items) do
        key_count = key_count + 1
        if type(key) ~= "number" or key < 1 or key % 1 ~= 0 or key > length then
            return nil, "allowed_items must be a dense array"
        end
        if type(item) ~= "string" or item == "" or item:match("^%s*(.-)%s*$") ~= item then
            return nil, "allowed_items entries must be non-empty item prototype names without surrounding whitespace"
        end
        if seen[item] then
            return nil, "allowed_items contains duplicate item: " .. item
        end
        if not prototypes.item[item] then
            return nil, "unknown item prototype: " .. item
        end
        seen[item] = true
        normalized[key] = item
    end
    if key_count ~= length then return nil, "allowed_items must be a dense array" end
    return normalized, nil
end

-- Replace an existing inserter's complete whitelist atomically. If an active
-- whitelist excludes an item already held by the arm, return that entire stack
-- to the character before reporting success so the old item cannot remain
-- permanently jammed in the inserter.
function M.configure_inserter(agent_id, unit_number, allowed_items)
    unit_number = tonumber(unit_number)
    if not unit_number or unit_number <= 0 or unit_number % 1 ~= 0 then
        return {
            success = false,
            error_kind = "invalid_unit_number",
            error = "unit_number must be a positive integer",
        }
    end

    local character = characters.find(agent_id)
    if not (character and character.valid) then
        return {
            success = false,
            error_kind = "no_character",
            error = "no character for agent " .. tostring(agent_id) .. "; spawn first",
        }
    end

    local entity = entities.find_by_unit_number(unit_number)
    if not (entity and entity.valid) then
        return {
            success = false,
            error_kind = "entity_not_found",
            error = "Entity not found",
            unit_number = unit_number,
        }
    end
    if entity.type ~= "inserter" then
        return {
            success = false,
            error_kind = "not_an_inserter",
            error = "Entity is not an inserter",
            unit_number = unit_number,
            name = entity.name,
            entity_type = entity.type,
            position = pos_table(entity.position),
        }
    end
    if entity.force ~= character.force then
        return {
            success = false,
            error_kind = "wrong_force",
            error = "Inserter belongs to a different force",
            unit_number = unit_number,
            force = character.force and character.force.name or nil,
            entity_force = entity.force and entity.force.name or nil,
        }
    end

    local reach_error = characters.require_entity_reach(character, entity)
    if reach_error then return reach_error end

    local normalized, validation_error = validate_allowed_items(allowed_items)
    if not normalized then
        return {
            success = false,
            error_kind = "invalid_allowed_items",
            error = validation_error,
            unit_number = unit_number,
        }
    end

    local slots_ok, slots_or_error = pcall(function() return entity.filter_slot_count end)
    if not slots_ok then
        return {
            success = false,
            error_kind = "filters_unsupported",
            error = tostring(slots_or_error),
            unit_number = unit_number,
        }
    end
    local slot_count = tonumber(slots_or_error) or 0
    if slot_count < 1 then
        return {
            success = false,
            error_kind = "filters_unsupported",
            error = "Inserter prototype has no filter slots",
            unit_number = unit_number,
            name = entity.name,
            filter_slot_count = slot_count,
        }
    end
    if #normalized > slot_count then
        return {
            success = false,
            error_kind = "too_many_filters",
            error = "Requested whitelist exceeds inserter filter-slot capacity",
            unit_number = unit_number,
            requested_filter_count = #normalized,
            filter_slot_count = slot_count,
        }
    end

    local before, read_error = read_inserter_filter_state(entity, slot_count)
    if not before then
        return {
            success = false,
            error_kind = "filter_read_failed",
            error = read_error,
            unit_number = unit_number,
        }
    end
    local held_before = held_stack_record(entity)

    local apply_ok, apply_error = pcall(function()
        for slot = 1, slot_count do entity.set_filter(slot, nil) end
        if #normalized > 0 then
            entity.inserter_filter_mode = "whitelist"
            for slot, item in ipairs(normalized) do entity.set_filter(slot, item) end
            entity.use_filters = true
        else
            entity.use_filters = false
        end
    end)
    if not apply_ok then
        local rollback_ok, rollback_error, rollback_observed = restore_inserter_filter_state(entity, slot_count, before)
        return {
            success = false,
            error_kind = "filter_write_failed",
            error = tostring(apply_error),
            unit_number = unit_number,
            rollback_succeeded = rollback_ok,
            rollback_readback_verified = rollback_ok,
            rollback_error = rollback_ok and nil or tostring(rollback_error),
            rollback_observed = rollback_observed,
        }
    end

    local after, after_error = read_inserter_filter_state(entity, slot_count)
    local mismatch = nil
    if not after then
        mismatch = after_error
    elseif after.filtering_enabled ~= (#normalized > 0) then
        mismatch = "filtering enabled-state readback did not match request"
    elseif #normalized > 0 and after.mode ~= "whitelist" then
        mismatch = "filter mode readback was not whitelist"
    elseif #after.filters ~= #normalized then
        mismatch = "filter-slot readback count did not match request"
    else
        for slot, item in ipairs(normalized) do
            local actual = after.filters[slot]
            if not actual or actual.slot ~= slot or actual.name ~= item then
                mismatch = "filter-slot readback did not match request at slot " .. tostring(slot)
                break
            end
        end
    end

    if mismatch then
        local rollback_ok, rollback_error, rollback_observed = restore_inserter_filter_state(entity, slot_count, before)
        return {
            success = false,
            error_kind = "filter_readback_mismatch",
            error = mismatch,
            unit_number = unit_number,
            requested_allowed_items = normalized,
            observed = after,
            rollback_succeeded = rollback_ok,
            rollback_readback_verified = rollback_ok,
            rollback_error = rollback_ok and nil or tostring(rollback_error),
            rollback_observed = rollback_observed,
        }
    end

    local evacuation = evacuate_disallowed_held_stack(character, entity, normalized, held_before)
    if not evacuation.succeeded then
        local rollback_ok, rollback_error, rollback_observed = restore_inserter_filter_state(entity, slot_count, before)
        return {
            success = false,
            error_kind = "held_stack_evacuation_failed",
            error = evacuation.error,
            unit_number = unit_number,
            held_stack_before = held_before,
            held_stack_after = held_stack_record(entity),
            held_stack_present_before = held_before ~= nil,
            held_stack_present_after = held_stack_record(entity) ~= nil,
            held_stack_violated_whitelist = evacuation.required,
            held_stack_evacuation = evacuation,
            rollback_succeeded = rollback_ok,
            rollback_readback_verified = rollback_ok,
            rollback_error = rollback_ok and nil or tostring(rollback_error),
            rollback_observed = rollback_observed,
        }
    end

    local held_after = held_stack_record(entity)
    local guidance = nil
    if evacuation.required then
        guidance = "Returned the held " .. held_before.name
            .. " to the character because the new whitelist excludes it. The whitelist applies to future pickups; inspect the upstream belt separately because filtering does not make a mixed belt pure."
    elseif #normalized > 0 then
        guidance = "The whitelist applies to future pickups. Inspect the upstream belt separately; this does not make a mixed belt pure."
    else
        guidance = "Filtering is disabled and all filter slots are clear."
    end

    return {
        success = true,
        unit_number = unit_number,
        name = entity.name,
        entity_type = entity.type,
        position = pos_table(entity.position),
        force = entity.force and entity.force.name or nil,
        filter_slot_count = slot_count,
        requested_allowed_items = normalized,
        before = {
            filtering_enabled = before.filtering_enabled,
            mode = before.mode,
            filters = before.filters,
        },
        after = {
            filtering_enabled = after.filtering_enabled,
            mode = after.mode,
            filters = after.filters,
        },
        filtering_enabled = after.filtering_enabled,
        mode = after.mode,
        filters = after.filters,
        held_stack_before = held_before,
        held_stack_after = held_after,
        held_stack_present_before = held_before ~= nil,
        held_stack_present_after = held_after ~= nil,
        held_stack_violated_whitelist = evacuation.required,
        held_stack_evacuated = evacuation.required,
        held_stack_returned_count = evacuation.returned_count,
        readback_verified = true,
        entity_identity_preserved = entity.valid and entity.unit_number == unit_number,
        guidance = guidance,
    }
end

-- Place and filter a new inserter in one remote call. Factorio does not advance
-- simulation ticks while this Lua call runs, so the arm cannot pick up a
-- pre-filter item from a mixed belt. If filter verification fails, undo the
-- placement here before returning control to the host.
function M.place_filtered_inserter(agent_id, entity_name, x, y, direction, allowed_items)
    local placement_ok, placed_or_error = pcall(
        M.place_entity,
        agent_id,
        entity_name,
        x,
        y,
        direction
    )
    if not placement_ok then
        return {
            success = false,
            error_kind = "atomic_placement_exception",
            error = tostring(placed_or_error),
            atomic_outcome_known = false,
        }
    end

    local placed = placed_or_error
    if not (placed and placed.unit_number) then
        if type(placed) == "table" then
            -- M.place_entity returns without a unit only before create_entity
            -- has succeeded. Make that no-mutation fact explicit; the host
            -- must never infer it merely from a missing identity.
            placed.atomic_outcome_known = true
            placed.entity_created = false
            return placed
        end
        return {
            success = false,
            error_kind = "atomic_placement_result_invalid",
            error = "Atomic placement returned neither a report nor an exact entity identity",
            atomic_outcome_known = false,
        }
    end
    placed.atomic_outcome_known = true
    placed.entity_created = true

    local configure_ok, configured_or_error = pcall(
        M.configure_inserter,
        agent_id,
        placed.unit_number,
        allowed_items
    )
    local configured = configure_ok and configured_or_error or {
        success = false,
        error_kind = "atomic_filter_configuration_exception",
        error = tostring(configured_or_error),
        unit_number = placed.unit_number,
    }
    if configured and configured.success == true
        and configured.held_stack_before == nil
        and configured.held_stack_after == nil
    then
        configured.atomic_with_placement = true
        placed.success = true
        placed.filter = configured
        placed.atomic_filter_configuration = true
        return placed
    end

    local removed = false
    local returned = 0
    local cleanup_ok, cleanup_error = pcall(function()
        local character = characters.find(agent_id)
        local inv = character and character.valid and character.get_main_inventory() or nil
        local entity = entities.find_by_unit_number(placed.unit_number)
        if entity and entity.valid then removed = entity.destroy() == true end
        if removed and storage.factorioctl_entities then
            storage.factorioctl_entities[placed.unit_number] = nil
        end
        returned = removed and inv and inv.insert{name = entity_name, count = 1} or 0
    end)
    return {
        success = false,
        error_kind = "atomic_filter_configuration_failed",
        error = configured and configured.error
            or "New inserter held an item before its whitelist was verified",
        placement = placed,
        filter = configured,
        atomic_outcome_known = true,
        entity_created = true,
        rollback = {
            success = cleanup_ok and removed and returned == 1,
            entity_removed = removed,
            item_returned = returned,
            cleanup_completed = cleanup_ok,
            error = cleanup_ok and nil or tostring(cleanup_error),
        },
    }
end

return M
