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
    local by_name = {}
    for _, resource in pairs(resources) do
        local key = resource.name
        if not by_name[key] then
            by_name[key] = {
                name = resource.name,
                total_amount = 0,
                tile_count = 0,
                min_x = resource.position.x,
                max_x = resource.position.x,
                min_y = resource.position.y,
                max_y = resource.position.y,
            }
        end

        local patch = by_name[key]
        patch.total_amount = patch.total_amount + (resource.amount or 0)
        patch.tile_count = patch.tile_count + 1
        patch.min_x = math.min(patch.min_x, resource.position.x)
        patch.max_x = math.max(patch.max_x, resource.position.x)
        patch.min_y = math.min(patch.min_y, resource.position.y)
        patch.max_y = math.max(patch.max_y, resource.position.y)
    end

    local result = {}
    for _, patch in pairs(by_name) do
        table.insert(result, resource_patch_result(patch))
    end
    return result
end

function M.find_resources(x1, y1, x2, y2, resource_type)
    local filters = {
        type = "resource",
        area = area_table(x1, y1, x2, y2),
    }
    if resource_type then filters.name = resource_type end

    local resources = game.surfaces[1].find_entities_filtered(filters)
    return aggregate_resource_patches(resources)
end

function M.find_nearest_resource(resource_name, from_x, from_y)
    local nearest = nil
    local nearest_dist = math.huge
    local resources = game.surfaces[1].find_entities_filtered{
        type = "resource",
        name = resource_name,
        position = {from_x, from_y},
        radius = 200,
    }

    for _, resource in pairs(resources) do
        local dx = resource.position.x - from_x
        local dy = resource.position.y - from_y
        local dist = dx * dx + dy * dy
        if dist < nearest_dist then
            nearest = resource
            nearest_dist = dist
        end
    end

    if not nearest then return nil end

    local patch_resources = game.surfaces[1].find_entities_filtered{
        type = "resource",
        name = resource_name,
        position = nearest.position,
        radius = 50,
    }
    local patches = aggregate_resource_patches(patch_resources)
    return patches[1]
end

local function tile_summary(tile, x, y)
    return {
        name = tile.name,
        position = {x = x, y = y},
        collides_with_player = tile.collides_with("player"),
    }
end

function M.get_tiles(x1, y1, x2, y2)
    local result = {}
    for x = x1, x2 do
        for y = y1, y2 do
            local tile = game.surfaces[1].get_tile(x, y)
            table.insert(result, tile_summary(tile, x, y))
        end
    end
    return result
end

function M.get_tile(x, y)
    local tile = game.surfaces[1].get_tile(x, y)
    return tile_summary(tile, x, y)
end

return M

