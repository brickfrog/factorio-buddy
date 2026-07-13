local M = {}

local function item_name(item)
    return type(item) == "string" and item or item.name
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

function M.production_statistics(surface_name)
    local surface = game.surfaces[surface_name or "nauvis"]
    if not surface then
        return {
            error = "surface not found",
            surface = surface_name,
            produced = {},
            consumed = {},
            produced_per_minute = {},
            consumed_per_minute = {},
            net_per_minute = {},
            items = {},
        }
    end

    local stats = game.forces.player.get_item_production_statistics(surface)
    local precision = defines.flow_precision_index.one_minute
    local produced = {}
    local consumed = {}
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

    local produced_per_minute = {}
    local consumed_per_minute = {}
    local net_per_minute = {}
    local items = {}
    for name, _ in pairs(names) do
        local input_rate = flow_for(stats, name, "input", precision)
        local output_rate = flow_for(stats, name, "output", precision)
        produced_per_minute[name] = input_rate
        consumed_per_minute[name] = output_rate
        net_per_minute[name] = input_rate - output_rate
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
        produced_per_minute = produced_per_minute,
        consumed_per_minute = consumed_per_minute,
        net_per_minute = net_per_minute,
        items = items,
    }
end

return M
