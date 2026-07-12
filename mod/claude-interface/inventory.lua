local M = {}

function M.contents(inv)
    local result = {}
    if not inv then return result end
    for _, item in pairs(inv.get_contents()) do
        table.insert(result, {name = item.name, count = item.count})
    end
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

