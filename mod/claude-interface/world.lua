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

local function generated_chunk_count(surface)
    local count = 0
    for _ in surface.get_chunks() do count = count + 1 end
    return count
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
