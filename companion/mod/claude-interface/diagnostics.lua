local M = {}

function M.eval_production_snapshot(surface_name)
    local surface = game.surfaces[surface_name or "nauvis"]
    if not surface then
        return {produced = {}, rate_per_min = {}}
    end

    local stats = game.forces.player.get_item_production_statistics(surface)
    local precision = defines.flow_precision_index.one_minute
    local produced = {}
    local rate_per_min = {}

    for item, count in pairs(stats.input_counts or {}) do
        local name = type(item) == "string" and item or item.name
        if name then
            produced[name] = count
            rate_per_min[name] = stats.get_flow_count{
                name = name,
                category = "input",
                precision_index = precision,
            }
        end
    end

    return {produced = produced, rate_per_min = rate_per_min}
end

return M
