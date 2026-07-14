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
