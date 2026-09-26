local M = {}

function M.error(error_kind, message, extra)
    local result = extra or {}
    result.success = false
    result.error_kind = error_kind
    result.error = message
    return helpers.table_to_json(result)
end

function M.remote_call(action_name, fn, ...)
    local ok, result_or_error = pcall(fn, ...)
    if not ok then
        return helpers.table_to_json({
            success = false,
            error_kind = "lua_error",
            error = tostring(result_or_error),
            action_needed = "fix_" .. action_name,
        })
    end
    if result_or_error == nil then return "null" end
    if type(result_or_error) == "string" then return result_or_error end
    return helpers.table_to_json(result_or_error)
end

local function lua_object_name(value)
    local kind = type(value)
    if kind ~= "userdata" and kind ~= "table" then return nil end
    local ok, name = pcall(function() return value.object_name end)
    if ok and type(name) == "string" then return name end
    if kind == "userdata" then return "userdata" end
    return nil
end

-- Serialize a remote's return value for the /claude wire. JSON strings pass
-- through; booleans and finite numbers keep their JSON wire type; plain
-- tables are encoded; Factorio objects and non-finite numbers cannot be
-- represented and become a structured error instead of a Lua error.
function M.encode_result(action_name, value)
    local kind = type(value)
    if value == nil then return nil end
    if kind == "string" then return value end
    if kind == "boolean" then return tostring(value) end
    if kind == "number" then
        if value ~= value or value == math.huge or value == -math.huge then
            return M.error("unserializable_result", action_name .. " returned a non-finite number")
        end
        return tostring(value)
    end
    local object_name = lua_object_name(value)
    if object_name then
        return M.error("unserializable_result", action_name .. " returned a Factorio object (" .. object_name .. ") that has no JSON form; use its JSON remote instead")
    end
    if kind == "table" then
        local ok, encoded = pcall(helpers.table_to_json, value)
        if ok and type(encoded) == "string" then return encoded end
        return M.error("unserializable_result", action_name .. " returned a table that cannot be encoded: " .. tostring(encoded))
    end
    return M.error("unserializable_result", action_name .. " returned an unsupported " .. kind .. " value")
end

return M

