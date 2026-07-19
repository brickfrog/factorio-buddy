local M = {}

local function area_table(x1, y1, x2, y2)
    return {{x1, y1}, {x2, y2}}
end

local function resource_patch_result(patch)
    return {
        name = patch.name,
        total_amount = patch.total_amount,
        tile_count = patch.tile_count,
        center = {
            x = (patch.min_x + patch.max_x) / 2,
            y = (patch.min_y + patch.max_y) / 2,
        },
        bounding_box = {
            left_top = {x = patch.min_x, y = patch.min_y},
            right_bottom = {x = patch.max_x, y = patch.max_y},
        },
    }
end

local function aggregate_resource_patches(resources)
    local by_name_and_tile = {}
    local ordered = {}
    for _, resource in pairs(resources) do
        local tile_x = math.floor(resource.position.x)
        local tile_y = math.floor(resource.position.y)
        local key = resource.name .. ":" .. tile_x .. "," .. tile_y
        by_name_and_tile[key] = resource
        table.insert(ordered, {resource = resource, tile_x = tile_x, tile_y = tile_y, key = key})
    end

    local result = {}
    local visited = {}
    for _, start in ipairs(ordered) do
        if not visited[start.key] then
            local patch = {
                name = start.resource.name,
                total_amount = 0,
                tile_count = 0,
                min_x = start.resource.position.x,
                max_x = start.resource.position.x,
                min_y = start.resource.position.y,
                max_y = start.resource.position.y,
            }
            local queue = {start}
            visited[start.key] = true
            local cursor = 1
            while cursor <= #queue do
                local current = queue[cursor]
                cursor = cursor + 1
                local resource = current.resource
                patch.total_amount = patch.total_amount + (resource.amount or 0)
                patch.tile_count = patch.tile_count + 1
                patch.min_x = math.min(patch.min_x, resource.position.x)
                patch.max_x = math.max(patch.max_x, resource.position.x)
                patch.min_y = math.min(patch.min_y, resource.position.y)
                patch.max_y = math.max(patch.max_y, resource.position.y)

                for dx = -1, 1 do
                    for dy = -1, 1 do
                        if dx ~= 0 or dy ~= 0 then
                            local neighbour_key = resource.name .. ":" .. (current.tile_x + dx) .. "," .. (current.tile_y + dy)
                            local neighbour = by_name_and_tile[neighbour_key]
                            if neighbour and not visited[neighbour_key] then
                                visited[neighbour_key] = true
                                table.insert(queue, {
                                    resource = neighbour,
                                    tile_x = current.tile_x + dx,
                                    tile_y = current.tile_y + dy,
                                    key = neighbour_key,
                                })
                            end
                        end
                    end
                end
            end
            table.insert(result, resource_patch_result(patch))
        end
    end
    table.sort(result, function(a, b)
        if a.name ~= b.name then return a.name < b.name end
        if a.center.x ~= b.center.x then return a.center.x < b.center.x end
        return a.center.y < b.center.y
    end)
    return result
end

function M.find_resources(surface, x1, y1, x2, y2, resource_type)
    if not surface then return {error = "agent surface not found"} end
    local filters = {
        type = "resource",
        area = area_table(x1, y1, x2, y2),
    }
    if resource_type then filters.name = resource_type end

    local resources = surface.find_entities_filtered(filters)
    return aggregate_resource_patches(resources)
end

local MAX_RESOURCE_EXPLORE_RADIUS = 512
local STRATEGIC_RESOURCE_CLEARANCE = 32
local MAX_STRATEGIC_RESOURCE_CANDIDATES = 12
local STRATEGIC_PATCH_SAMPLE_RADIUS = 50

local function generated_chunk_count(surface)
    local count = 0
    for _ in surface.get_chunks() do count = count + 1 end
    return count
end

local function generated_world_summary(surface)
    local chunk_count = 0
    local min_x, min_y, max_x, max_y = nil, nil, nil, nil
    for chunk in surface.get_chunks() do
        chunk_count = chunk_count + 1
        min_x = min_x and math.min(min_x, chunk.x) or chunk.x
        min_y = min_y and math.min(min_y, chunk.y) or chunk.y
        max_x = max_x and math.max(max_x, chunk.x) or chunk.x
        max_y = max_y and math.max(max_y, chunk.y) or chunk.y
    end

    local generated_bounds = nil
    if min_x then
        generated_bounds = {
            left_top = {x = min_x * 32, y = min_y * 32},
            right_bottom = {x = (max_x + 1) * 32, y = (max_y + 1) * 32},
        }
    end
    return {
        generated_chunk_count = chunk_count,
        generated_bounds = generated_bounds,
    }
end

local function point_to_bounds_distance(position, bounds)
    local dx = math.max(
        bounds.left_top.x - position.x,
        0,
        position.x - bounds.right_bottom.x
    )
    local dy = math.max(
        bounds.left_top.y - position.y,
        0,
        position.y - bounds.right_bottom.y
    )
    return math.sqrt(dx * dx + dy * dy)
end

local function near_mining_target(resource, mining_targets_by_name)
    for _, target in ipairs(mining_targets_by_name[resource.name] or {}) do
        local dx = target.position.x - resource.position.x
        local dy = target.position.y - resource.position.y
        if dx * dx + dy * dy <= STRATEGIC_PATCH_SAMPLE_RADIUS ^ 2 then
            return true
        end
    end
    return false
end

local function patch_contains_position(patch, position)
    local bounds = patch.bounding_box
    return position.x >= bounds.left_top.x
        and position.x <= bounds.right_bottom.x
        and position.y >= bounds.left_top.y
        and position.y <= bounds.right_bottom.y
end

local function patch_is_mined(patch, mining_targets)
    for _, target in ipairs(mining_targets) do
        if target.name == patch.name and patch_contains_position(patch, target.position) then
            return true
        end
    end
    return false
end

local function compass_direction(from, destination)
    local dx = destination.x - from.x
    local dy = destination.y - from.y
    local horizontal = dx >= 0 and "east" or "west"
    local vertical = dy >= 0 and "south" or "north"
    if math.abs(dx) > math.abs(dy) * 2 then return horizontal end
    if math.abs(dy) > math.abs(dx) * 2 then return vertical end
    return vertical .. "-" .. horizontal
end

local function sampled_patch(surface, seed)
    local resources = surface.find_entities_filtered{
        type = "resource",
        name = seed.resource.name,
        position = seed.resource.position,
        radius = STRATEGIC_PATCH_SAMPLE_RADIUS,
    }
    for _, patch in ipairs(aggregate_resource_patches(resources)) do
        if patch_contains_position(patch, seed.resource.position) then return patch end
    end
    return nil
end

local function strategic_candidate(patch, seed, factory_center, mining_targets)
    return {
        name = patch.name,
        total_amount = patch.total_amount,
        tile_count = patch.tile_count,
        center = patch.center,
        bounding_box = patch.bounding_box,
        distance_from_factory = seed.distance_from_factory,
        direction = compass_direction(factory_center, patch.center),
        currently_mined = patch_is_mined(patch, mining_targets),
        sample_radius = STRATEGIC_PATCH_SAMPLE_RADIUS,
        selection_reasons = {},
    }
end

function M.strategic_summary(surface, factory_bounds, mining_targets)
    local world = generated_world_summary(surface)
    local factory_center = {
        x = (factory_bounds.left_top.x + factory_bounds.right_bottom.x) / 2,
        y = (factory_bounds.left_top.y + factory_bounds.right_bottom.y) / 2,
    }
    -- Keep the global pass lightweight: retain only representative resource
    -- entities, then flood-fill bounded neighborhoods for the few candidates
    -- that can reach the autonomy prompt.
    local resources = surface.find_entities_filtered{type = "resource"}
    local scanned_resource_entity_count = #resources
    local external_resource_entity_count = 0
    local nearest_by_name = {}
    local richest_by_name = {}
    local mining_targets_by_name = {}
    for _, target in ipairs(mining_targets) do
        mining_targets_by_name[target.name] = mining_targets_by_name[target.name] or {}
        table.insert(mining_targets_by_name[target.name], target)
    end

    for _, resource in ipairs(resources) do
        local distance = point_to_bounds_distance(resource.position, factory_bounds)
        if distance >= STRATEGIC_RESOURCE_CLEARANCE
            and not near_mining_target(resource, mining_targets_by_name)
        then
            external_resource_entity_count = external_resource_entity_count + 1
            local seed = {
                resource = resource,
                distance_from_factory = distance,
            }
            local nearest = nearest_by_name[resource.name]
            if not nearest or distance < nearest.distance_from_factory then
                nearest_by_name[resource.name] = seed
            end
            local richest = richest_by_name[resource.name]
            if not richest
                or (resource.amount or 0) > (richest.resource.amount or 0)
                or ((resource.amount or 0) == (richest.resource.amount or 0)
                    and distance < richest.distance_from_factory)
            then
                richest_by_name[resource.name] = seed
            end
        end
    end
    resources = nil

    local resource_names = {}
    for name, _ in pairs(nearest_by_name) do table.insert(resource_names, name) end
    table.sort(resource_names, function(a, b)
        local a_nearest = nearest_by_name[a].distance_from_factory
        local b_nearest = nearest_by_name[b].distance_from_factory
        if a_nearest ~= b_nearest then return a_nearest < b_nearest end
        return a < b
    end)

    local candidates = {}
    local candidates_by_key = {}
    local function select_candidate(seed, reason)
        if #candidates >= MAX_STRATEGIC_RESOURCE_CANDIDATES then return end
        local patch = sampled_patch(surface, seed)
        if not patch then return end
        local key = patch.name .. ":" .. patch.center.x .. "," .. patch.center.y
        local selected = candidates_by_key[key]
        if selected then
            table.insert(selected.selection_reasons, reason)
            return
        end
        selected = strategic_candidate(patch, seed, factory_center, mining_targets)
        table.insert(selected.selection_reasons, reason)
        candidates_by_key[key] = selected
        table.insert(candidates, selected)
    end

    for _, name in ipairs(resource_names) do
        select_candidate(nearest_by_name[name], "nearest_external")
    end
    for _, name in ipairs(resource_names) do
        select_candidate(richest_by_name[name], "richest_external_sample")
    end

    table.sort(candidates, function(a, b)
        if a.currently_mined ~= b.currently_mined then return not a.currently_mined end
        if a.distance_from_factory ~= b.distance_from_factory then
            return a.distance_from_factory < b.distance_from_factory
        end
        if a.total_amount ~= b.total_amount then return a.total_amount > b.total_amount end
        if a.name ~= b.name then return a.name < b.name end
        if a.center.x ~= b.center.x then return a.center.x < b.center.x end
        return a.center.y < b.center.y
    end)
    for rank, candidate in ipairs(candidates) do candidate.rank = rank end

    return {
        world = world,
        expansion = {
            discovery_scope = "all_generated_chunks",
            minimum_factory_clearance = STRATEGIC_RESOURCE_CLEARANCE,
            scanned_resource_entity_count = scanned_resource_entity_count,
            external_resource_entity_count = external_resource_entity_count,
            candidate_resource_type_count = #resource_names,
            resource_candidates = candidates,
            candidate_limit = MAX_STRATEGIC_RESOURCE_CANDIDATES,
            candidate_limit_reached = #candidates >= MAX_STRATEGIC_RESOURCE_CANDIDATES,
        },
    }
end

function M.find_nearest_resource(surface, resource_name, from_x, from_y, explore_radius)
    if not surface then return {error = "agent surface not found"} end
    if explore_radius ~= nil then
        if type(explore_radius) ~= "number"
            or explore_radius < 1
            or explore_radius > MAX_RESOURCE_EXPLORE_RADIUS
        then
            return {
                error = "explore_radius must be between 1 and "
                    .. MAX_RESOURCE_EXPLORE_RADIUS,
                error_kind = "invalid_explore_radius",
            }
        end
    end

    local chunks_before = generated_chunk_count(surface)
    local search_area = nil
    if explore_radius then
        surface.request_to_generate_chunks(
            {from_x, from_y},
            math.ceil(explore_radius / 32)
        )
        surface.force_generate_chunk_requests()
        search_area = area_table(
            from_x - explore_radius,
            from_y - explore_radius,
            from_x + explore_radius,
            from_y + explore_radius
        )
    end
    local chunks_after = generated_chunk_count(surface)

    local nearest = nil
    local nearest_dist = math.huge
    local filters = {
        type = "resource",
        name = resource_name,
    }
    if search_area then filters.area = search_area end
    local resources = surface.find_entities_filtered(filters)

    for _, resource in pairs(resources) do
        local dx = resource.position.x - from_x
        local dy = resource.position.y - from_y
        local dist = dx * dx + dy * dy
        if dist < nearest_dist then
            nearest = resource
            nearest_dist = dist
        end
    end

    local search = {
        scope = explore_radius and "generated_area" or "all_generated_chunks",
        origin = {x = from_x, y = from_y},
        explore_radius = explore_radius,
        generated_chunks_before = chunks_before,
        generated_chunks_after = chunks_after,
        generated_chunks_added = chunks_after - chunks_before,
        max_explore_radius = MAX_RESOURCE_EXPLORE_RADIUS,
    }

    if not nearest then
        return {
            success = true,
            found = false,
            resource_name = resource_name,
            search = search,
            guidance = explore_radius
                and "No matching resource was generated in the explored area. Try another origin or radius."
                or "No matching resource exists in generated chunks. Set explore_radius to generate and search nearby terrain.",
        }
    end

    local patch_resources = surface.find_entities_filtered{
        type = "resource",
        name = resource_name,
        position = nearest.position,
        radius = 50,
    }
    local patches = aggregate_resource_patches(patch_resources)
    for _, patch in ipairs(patches) do
        if nearest.position.x >= patch.bounding_box.left_top.x
            and nearest.position.x <= patch.bounding_box.right_bottom.x
            and nearest.position.y >= patch.bounding_box.left_top.y
            and nearest.position.y <= patch.bounding_box.right_bottom.y
        then
            local dx = patch.center.x - from_x
            local dy = patch.center.y - from_y
            return {
                success = true,
                found = true,
                resource_name = resource_name,
                resource = patch,
                distance = math.sqrt(dx * dx + dy * dy),
                search = search,
            }
        end
    end
    return {
        error = "nearest resource could not be aggregated into a patch",
        error_kind = "resource_patch_aggregation_failed",
    }
end

local function tile_summary(tile, x, y)
    return {
        name = tile.name,
        position = {x = x, y = y},
        collides_with_player = tile.collides_with("player"),
    }
end

function M.get_tiles(surface, x1, y1, x2, y2)
    if not surface then return {error = "agent surface not found"} end
    local result = {}
    for x = x1, x2 do
        for y = y1, y2 do
            local tile = surface.get_tile(x, y)
            table.insert(result, tile_summary(tile, x, y))
        end
    end
    return result
end

function M.get_tile(surface, x, y)
    if not surface then return {error = "agent surface not found"} end
    local tile = surface.get_tile(x, y)
    return tile_summary(tile, x, y)
end

return M
