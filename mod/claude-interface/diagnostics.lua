local M = {}

local function item_name(item)
    return type(item) == "string" and item or item.name
end

local function total_for(counts, target_name)
    for item, count in pairs(counts or {}) do
        if item_name(item) == target_name then return count end
    end
    return 0
end

local function round_rate(value)
    if value >= 0 then return math.floor(value * 1000 + 0.5) / 1000 end
    return math.ceil(value * 1000 - 0.5) / 1000
end

local function flow_for(stats, target_name, category, precision)
    local ok, value = pcall(function()
        return stats.get_flow_count{
            name = target_name,
            category = category,
            precision_index = precision,
        }
    end)
    if ok and type(value) == "number" then return round_rate(value) end
    return 0
end

function M.item_flow(surface, target_name)
    if not (surface and target_name and target_name ~= "") then
        return {
            item = target_name,
            produced_total = 0,
            consumed_total = 0,
            produced_per_minute = 0,
            consumed_per_minute = 0,
            net_per_minute = 0,
            recent_window = "five_seconds",
            recent_produced_per_minute = 0,
            recent_consumed_per_minute = 0,
            recent_net_per_minute = 0,
        }
    end

    local stats = game.forces.player.get_item_production_statistics(surface)
    local precision = defines.flow_precision_index.one_minute
    local recent_precision = defines.flow_precision_index.five_seconds
    local produced_per_minute = flow_for(stats, target_name, "input", precision)
    local consumed_per_minute = flow_for(stats, target_name, "output", precision)
    local recent_produced_per_minute = flow_for(stats, target_name, "input", recent_precision)
    local recent_consumed_per_minute = flow_for(stats, target_name, "output", recent_precision)
    return {
        item = target_name,
        produced_total = total_for(stats.input_counts, target_name),
        consumed_total = total_for(stats.output_counts, target_name),
        produced_per_minute = produced_per_minute,
        consumed_per_minute = consumed_per_minute,
        net_per_minute = produced_per_minute - consumed_per_minute,
        recent_window = "five_seconds",
        recent_produced_per_minute = recent_produced_per_minute,
        recent_consumed_per_minute = recent_consumed_per_minute,
        recent_net_per_minute = recent_produced_per_minute - recent_consumed_per_minute,
    }
end

function M.eval_production_snapshot(surface_name)
    local surface = game.surfaces[surface_name or "nauvis"]
    if not surface then
        return {
            error = "surface not found",
            surface = surface_name,
            produced = {},
            consumed = {},
            rate_per_min = {},
            consumed_per_min = {},
            net_per_min = {},
            items = {},
        }
    end

    local stats = game.forces.player.get_item_production_statistics(surface)
    local precision = defines.flow_precision_index.one_minute
    local produced = {}
    local consumed = {}
    local rate_per_min = {}
    local consumed_per_min = {}
    local net_per_min = {}
    local names = {}

    for item, count in pairs(stats.input_counts or {}) do
        local name = item_name(item)
        if name then
            names[name] = true
            produced[name] = count
        end
    end

    for item, count in pairs(stats.output_counts or {}) do
        local name = item_name(item)
        if name then
            names[name] = true
            consumed[name] = count
        end
    end

    local items = {}
    for name, _ in pairs(names) do
        local input_rate = flow_for(stats, name, "input", precision)
        local output_rate = flow_for(stats, name, "output", precision)
        rate_per_min[name] = input_rate
        consumed_per_min[name] = output_rate
        net_per_min[name] = input_rate - output_rate
        table.insert(items, {
            name = name,
            produced_total = produced[name] or 0,
            consumed_total = consumed[name] or 0,
            produced_per_minute = input_rate,
            consumed_per_minute = output_rate,
            net_per_minute = input_rate - output_rate,
        })
    end

    table.sort(items, function(a, b)
        local a_activity = a.produced_per_minute + a.consumed_per_minute
        local b_activity = b.produced_per_minute + b.consumed_per_minute
        if a_activity == b_activity then return a.name < b.name end
        return a_activity > b_activity
    end)

    return {
        surface = surface.name,
        force = game.forces.player.name,
        tick = game.tick,
        window = "one_minute",
        produced = produced,
        consumed = consumed,
        rate_per_min = rate_per_min,
        consumed_per_min = consumed_per_min,
        net_per_min = net_per_min,
        items = items,
    }
end

return M
