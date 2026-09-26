local M = {}

function M.quality_name(item)
    if not item or item.quality == nil then return nil end
    if type(item.quality) == "string" then return item.quality end
    local ok, name = pcall(function() return item.quality.name end)
    if ok then return name end
    return tostring(item.quality)
end

function M.item_record(item)
    return {
        name = item.name,
        count = item.count,
        quality = M.quality_name(item),
    }
end

function M.contents(inv)
    local result = {}
    if not inv then return result end
    for _, item in pairs(inv.get_contents()) do
        table.insert(result, M.item_record(item))
    end
    table.sort(result, function(a, b)
        if a.name ~= b.name then return a.name < b.name end
        return tostring(a.quality or "normal") < tostring(b.quality or "normal")
    end)
    return result
end

local function finite_number(value)
    return type(value) == "number" and value == value and value ~= math.huge and value ~= -math.huge
end
M.finite_number = finite_number

-- Validate a bounded operation count before any mutation. Out-of-range counts
-- are rejected, never clamped, so the caller sees exactly what was refused.
-- `maximum` is optional for transfers already bounded by what is available.
function M.validate_count(count, maximum, action_needed)
    local reported = count
    if type(count) == "number" and not finite_number(count) then reported = tostring(count) end
    if not finite_number(count) or count <= 0 or count ~= math.floor(count) then
        return {
            success = false,
            error_kind = "invalid_count",
            error = "count must be a positive integer",
            action_needed = action_needed,
            requested_count = reported,
            maximum_count = maximum,
        }
    end
    if maximum ~= nil and count > maximum then
        return {
            success = false,
            error_kind = "count_exceeds_limit",
            error = "count exceeds the bounded operation limit of " .. tostring(maximum),
            action_needed = action_needed,
            requested_count = reported,
            maximum_count = maximum,
        }
    end
    return nil
end

-- Return one ItemWithQualityCount to a character inventory and spill only the
-- remainder at `position`. Every count is reported so callers can prove item
-- conservation instead of assuming it. The spill passes no force: a force
-- would mark every spilled item for deconstruction by that force's robots,
-- which is not what a player's overflow spill does.
function M.give_or_spill(target_inventory, surface, position, item)
    local quality = item.quality or "normal"
    local count = item.count or 0
    local inserted = 0
    if target_inventory and count > 0 then
        inserted = target_inventory.insert{name = item.name, quality = quality, count = count}
    end
    local remainder = count - inserted
    local spilled = 0
    if remainder > 0 then
        local spilled_entities = surface.spill_item_stack{
            position = position,
            stack = {name = item.name, quality = quality, count = remainder},
            enable_looted = false,
            allow_belts = false,
            use_start_position_on_failure = true,
            drop_full_stack = true,
        }
        for _, entity in pairs(spilled_entities or {}) do
            local stack = entity and entity.valid and entity.stack or nil
            if stack and stack.valid_for_read
                and stack.name == item.name
                and (M.quality_name(stack) or "normal") == quality
            then
                spilled = spilled + stack.count
            end
        end
    end
    return {
        name = item.name,
        quality = quality,
        count = count,
        inserted = inserted,
        spilled = spilled,
        unrecovered = remainder - spilled,
    }
end

function M.define_for(inventory_type, default_type)
    local normalized = inventory_type or default_type
    if normalized == "fuel" then return defines.inventory.fuel end
    if normalized == "input" then return defines.inventory.assembling_machine_input end
    if normalized == "output" then return defines.inventory.assembling_machine_output end
    if normalized == "chest" then return defines.inventory.chest end
    if normalized == "furnace_source" then return defines.inventory.furnace_source end
    if normalized == "furnace_result" then return defines.inventory.furnace_result end
    if normalized == "lab_input" then return defines.inventory.lab_input end
    if normalized == "lab_modules" then return defines.inventory.lab_modules end
    return M.define_for(default_type, default_type)
end

return M
