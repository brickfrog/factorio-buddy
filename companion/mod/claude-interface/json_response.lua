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

return M
