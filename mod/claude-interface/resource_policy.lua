local M = {}

local function xy(position)
    if type(position) ~= "table" then return nil, nil end
    return position.x or position[1], position.y or position[2]
end

local function normalized_direction(direction)
    if type(direction) == "number" then
        if direction == 0 or direction == 4 or direction == 8 or direction == 12 then
            return direction
        end
    elseif type(direction) == "string" then
        local value = string.lower(direction)
        if value == "north" or value == "n" or value == "0" then return 0 end
        if value == "east" or value == "e" or value == "4" then return 4 end
        if value == "south" or value == "s" or value == "8" then return 8 end
        if value == "west" or value == "w" or value == "12" then return 12 end
    end
    return 0
end

local function rotate(x, y, direction)
    if direction == 4 then return -y, x end
    if direction == 8 then return -x, -y end
    if direction == 12 then return y, -x end
    return x, y
end

-- Return the complete rotated collision footprint used by the live resource
-- reservation rule. This is deliberately derived from prototypes rather than
-- entity names or center tiles, so rectangular and modded entities are covered.
function M.footprint(entity_name, position, direction)
    local px, py = xy(position)
    if not px or not py then return nil end
    local prototype = prototypes.entity[entity_name]
    local box = prototype and prototype.collision_box or nil
    if not box then
        return {
            {px - 0.5, py - 0.5},
            {px + 0.5, py + 0.5},
        }
    end

    direction = normalized_direction(direction)
    local min_x, min_y, max_x, max_y = nil, nil, nil, nil
    for _, corner in ipairs({
        {box.left_top.x, box.left_top.y},
        {box.left_top.x, box.right_bottom.y},
        {box.right_bottom.x, box.left_top.y},
        {box.right_bottom.x, box.right_bottom.y},
    }) do
        local x, y = rotate(corner[1], corner[2], direction)
        min_x = min_x and math.min(min_x, x) or x
        min_y = min_y and math.min(min_y, y) or y
        max_x = max_x and math.max(max_x, x) or x
        max_y = max_y and math.max(max_y, y) or y
    end
    return {
        {px + min_x, py + min_y},
        {px + max_x, py + max_y},
    }
end

local function area_record(area)
    return {
        left_top = {x = area[1][1], y = area[1][2]},
        right_bottom = {x = area[2][1], y = area[2][2]},
    }
end

local function resource_tile_record(resource)
    return {
        name = resource.name,
        position = {x = resource.position.x, y = resource.position.y},
    }
end

local function resource_tile_key(resource)
    return table.concat({
        resource.name or "",
        tostring(resource.position and resource.position.x or ""),
        tostring(resource.position and resource.position.y or ""),
    }, "\31")
end

local function footprints_equal(left, right)
    return left
        and right
        and left.left_top.x == right.left_top.x
        and left.left_top.y == right.left_top.y
        and left.right_bottom.x == right.right_bottom.x
        and left.right_bottom.y == right.right_bottom.y
end

local function supports_category(prototype, category)
    if not (prototype and prototype.type == "mining-drill" and category) then return false end
    for key, value in pairs(prototype.resource_categories or {}) do
        if key == category or value == category then return true end
    end
    return false
end

-- Inspect authoritative live resources under the proposed rotated footprint.
-- No scan, memory, item-name allowlist, or caller override is involved.
function M.inspect(surface, entity_name, position, direction)
    local footprint = M.footprint(entity_name, position, direction)
    local result = {
        policy_allowed = true,
        preserves_resource_patch = true,
        footprint = footprint and area_record(footprint) or nil,
        overlapping_resources = {},
        overlapping_resource_tiles = {},
    }
    if not (surface and footprint) then
        result.policy_allowed = false
        result.preserves_resource_patch = false
        result.error_kind = "resource_policy_unavailable"
        result.error = "Unable to inspect the proposed entity footprint"
        return result
    end

    local prototype = prototypes.entity[entity_name]
    local extractor = prototype and prototype.type == "mining-drill" or false
    local by_name = {}
    local resources = surface.find_entities_filtered{
        area = footprint,
        type = "resource",
    }
    for _, resource in pairs(resources) do
        if resource.valid then
            table.insert(result.overlapping_resource_tiles, resource_tile_record(resource))
            local resource_prototype = prototypes.entity[resource.name]
            local category = resource_prototype and resource_prototype.resource_category or nil
            local compatible = extractor and supports_category(prototype, category)
            local summary = by_name[resource.name]
            if not summary then
                summary = {
                    name = resource.name,
                    category = category,
                    tile_count = 0,
                    total_amount = 0,
                    compatible_with_extractor = compatible,
                }
                by_name[resource.name] = summary
                table.insert(result.overlapping_resources, summary)
            end
            summary.tile_count = summary.tile_count + 1
            summary.total_amount = summary.total_amount + (resource.amount or 0)
            summary.compatible_with_extractor = summary.compatible_with_extractor and compatible
        end
    end
    table.sort(result.overlapping_resources, function(a, b) return a.name < b.name end)
    table.sort(result.overlapping_resource_tiles, function(a, b)
        if a.name ~= b.name then return a.name < b.name end
        if a.position.x ~= b.position.x then return a.position.x < b.position.x end
        return a.position.y < b.position.y
    end)

    if #result.overlapping_resources == 0 then return result end

    local all_compatible = extractor
    for _, resource in ipairs(result.overlapping_resources) do
        if not resource.compatible_with_extractor then
            all_compatible = false
            break
        end
    end
    if all_compatible then
        result.extractor_exception = true
        result.preserves_resource_patch = true
        return result
    end

    result.policy_allowed = false
    result.preserves_resource_patch = false
    result.error_kind = "resource_footprint_reserved"
    result.error = "Proposed placement overlaps a live resource patch reserved for compatible extraction"
    result.guidance = "Move processing, storage, power, and ordinary logistics off the resource patch. Use execute_edge_miner for extraction and route belts around or underground."
    return result
end

-- Rotating a legacy entity already on resources is permitted only when the
-- requested collision footprint is unchanged. Factorio may remove resources
-- when a changed footprint is revalidated, even if every requested resource
-- also overlapped the old orientation. Harmless no-op/square rotations remain
-- available without allowing a rectangular footprint to consume the patch.
function M.inspect_rotation(surface, entity_name, position, current_direction, requested_direction)
    local current = M.inspect(surface, entity_name, position, current_direction)
    local requested = M.inspect(surface, entity_name, position, requested_direction)
    if requested.error_kind == "resource_policy_unavailable" then
        return {
            policy_allowed = false,
            preserves_resource_patch = false,
            error_kind = requested.error_kind,
            error = requested.error,
            current = current,
            requested = requested,
            newly_overlapped_resources = {},
        }
    end
    if #(requested.overlapping_resource_tiles or {}) == 0 then
        return {
            policy_allowed = true,
            preserves_resource_patch = true,
            current = current,
            requested = requested,
            newly_overlapped_resources = {},
        }
    end

    local current_tiles = {}
    for _, resource in ipairs(current.overlapping_resource_tiles or {}) do
        current_tiles[resource_tile_key(resource)] = true
    end
    local introduced = {}
    for _, resource in ipairs(requested.overlapping_resource_tiles or {}) do
        if not current_tiles[resource_tile_key(resource)] then
            table.insert(introduced, resource)
        end
    end
    local footprint_changed = not footprints_equal(current.footprint, requested.footprint)
    if #introduced == 0 and not footprint_changed then
        return {
            policy_allowed = true,
            preserves_resource_patch = true,
            current = current,
            requested = requested,
            newly_overlapped_resources = {},
            footprint_changed = false,
        }
    end

    return {
        policy_allowed = false,
        preserves_resource_patch = false,
        error_kind = "resource_footprint_reserved",
        error = #introduced > 0
            and "Requested rotation would expand this entity onto a live resource patch"
            or "Requested rotation would change the footprint of an entity already overlapping live resources",
        guidance = "Move the entity off the resource patch before changing its footprint, or keep an orientation with the same collision footprint.",
        current = current,
        requested = requested,
        newly_overlapped_resources = introduced,
        footprint_changed = footprint_changed,
    }
end

return M
