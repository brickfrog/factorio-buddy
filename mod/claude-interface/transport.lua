local M = {}

local function pos_table(pos)
    if not pos then return nil end
    return {x = pos.x, y = pos.y}
end

function M.get_belt_contents(x1, y1, x2, y2)
    local surface = game.surfaces[1]
    local belts = surface.find_entities_filtered{
        area = {{x1, y1}, {x2, y2}},
        type = "transport-belt",
    }
    local belt_items = {}
    local item_totals = {}
    local total_items = 0

    for _, belt in pairs(belts) do
        local belt_data = {
            position = pos_table(belt.position),
            unit_number = belt.unit_number,
            items = {},
        }
        for i = 1, belt.get_max_transport_line_index() do
            local line = belt.get_transport_line(i)
            if line then
                for _, item in pairs(line.get_contents()) do
                    table.insert(belt_data.items, {name = item.name, count = item.count})
                    item_totals[item.name] = (item_totals[item.name] or 0) + item.count
                    total_items = total_items + item.count
                end
            end
        end
        if #belt_data.items > 0 then
            table.insert(belt_items, belt_data)
        end
    end

    local summary = {}
    for item_name, count in pairs(item_totals) do
        table.insert(summary, {name = item_name, count = count})
    end

    return {
        belt_count = #belts,
        total_items = total_items,
        item_summary = summary,
        belts = belt_items,
    }
end

function M.get_belt_lane_contents(x1, y1, x2, y2)
    local surface = game.surfaces[1]
    local belts = surface.find_entities_filtered{
        area = {{x1, y1}, {x2, y2}},
        type = "transport-belt",
    }
    local result = {}

    for _, belt in pairs(belts) do
        local left_items = {}
        local right_items = {}
        local left_count = 0
        local right_count = 0

        local line1 = belt.get_transport_line(1)
        if line1 then
            for _, item in pairs(line1.get_contents()) do
                table.insert(left_items, {name = item.name, count = item.count})
                left_count = left_count + item.count
            end
        end

        local line2 = belt.get_transport_line(2)
        if line2 then
            for _, item in pairs(line2.get_contents()) do
                table.insert(right_items, {name = item.name, count = item.count})
                right_count = right_count + item.count
            end
        end

        if #left_items > 0 or #right_items > 0 then
            table.insert(result, {
                position = {
                    x = math.floor(belt.position.x),
                    y = math.floor(belt.position.y),
                },
                unit_number = belt.unit_number,
                direction = belt.direction,
                belt_type = belt.name,
                left_lane = {lane = 1, items = left_items, item_count = left_count},
                right_lane = {lane = 2, items = right_items, item_count = right_count},
            })
        end
    end

    return result
end

return M

