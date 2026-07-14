local characters = require("characters")
local crafting = require("crafting")

local M = {}

local MAX_ITEM_FLOWS = 32
local MAX_FLOW_COUNT = 100000
local MAX_RECORDED_OPERATIONS = 1024
local MAX_SAFE_INTEGER = 9007199254740991

local function fail(error_kind, message, action_needed, extra)
    local result = extra or {}
    result.success = false
    result.error_kind = error_kind
    result.error = message
    result.action_needed = action_needed
    return result
end

local function accounting_state()
    local state = storage.factorio_buddy_craft_accounting or {}
    state.by_operation = state.by_operation or {}
    state.order = state.order or {}
    storage.factorio_buddy_craft_accounting = state
    return state
end

local function validate_flows(flows)
    if type(flows) ~= "table" or #flows == 0 or #flows > MAX_ITEM_FLOWS then
        return nil, fail(
            "invalid_craft_flows",
            "flows must contain 1 to " .. tostring(MAX_ITEM_FLOWS) .. " entries",
            "retry_verified_craft_accounting"
        )
    end

    local validated = {}
    local seen = {}
    for index, flow in ipairs(flows) do
        if type(flow) ~= "table" then
            return nil, fail(
                "invalid_craft_flow",
                "flow " .. tostring(index) .. " must be an object",
                "retry_verified_craft_accounting"
            )
        end
        local name = flow.name
        local produced = tonumber(flow.produced or 0)
        local consumed = tonumber(flow.consumed or 0)
        local production_before = tonumber(flow.production_before)
        local consumption_before = tonumber(flow.consumption_before)
        if type(name) ~= "string" or name == "" or not prototypes.item[name] then
            return nil, fail(
                "invalid_craft_flow",
                "flow " .. tostring(index) .. " has an unknown item name",
                "retry_verified_craft_accounting",
                {flow_index = index, item = name}
            )
        end
        local function invalid_count(count)
            return not count
                or count ~= count
                or count == math.huge
                or count == -math.huge
                or count < 0
                or count ~= math.floor(count)
                or count > MAX_FLOW_COUNT
        end
        if invalid_count(produced) or invalid_count(consumed) or produced + consumed == 0 then
            return nil, fail(
                "invalid_craft_flow",
                "flow " .. tostring(index) .. " must have bounded nonnegative produced/consumed counts and at least one nonzero count",
                "retry_verified_craft_accounting",
                {
                    flow_index = index,
                    item = name,
                    produced = produced,
                    consumed = consumed,
                    maximum_count = MAX_FLOW_COUNT,
                }
            )
        end
        local function invalid_baseline(count)
            return not count
                or count ~= count
                or count == math.huge
                or count == -math.huge
                or count < 0
                or count > MAX_SAFE_INTEGER
        end
        if invalid_baseline(production_before) or invalid_baseline(consumption_before) then
            return nil, fail(
                "invalid_craft_flow_baseline",
                "flow " .. tostring(index) .. " must contain bounded nonnegative production and consumption baselines",
                "retry_verified_craft_accounting",
                {
                    flow_index = index,
                    item = name,
                    production_before = production_before,
                    consumption_before = consumption_before,
                }
            )
        end
        if seen[name] then
            return nil, fail(
                "duplicate_craft_flow",
                "each craft flow item may appear only once",
                "retry_verified_craft_accounting",
                {flow_index = index, item = name}
            )
        end
        seen[name] = true
        table.insert(validated, {
            name = name,
            produced = produced,
            consumed = consumed,
            production_before = production_before,
            consumption_before = consumption_before,
        })
    end
    return validated, nil
end

local function remember(state, operation_id, result)
    state.by_operation[operation_id] = result
    table.insert(state.order, operation_id)
    local evictable = 0
    for _, candidate in ipairs(state.order) do
        if not crafting.operation_is_referenced(candidate) then evictable = evictable + 1 end
    end
    local index = 1
    while evictable > MAX_RECORDED_OPERATIONS and index <= #state.order do
        local candidate = state.order[index]
        if not crafting.operation_is_referenced(candidate) then
            table.remove(state.order, index)
            state.by_operation[candidate] = nil
            evictable = evictable - 1
        else
            index = index + 1
        end
    end
end

local function flows_match(admitted, supplied)
    if type(admitted) ~= "table" or #admitted ~= #supplied then return false end
    local expected_by_name = {}
    for _, flow in ipairs(admitted) do expected_by_name[flow.name] = flow end
    for _, flow in ipairs(supplied) do
        local expected = expected_by_name[flow.name]
        if not expected
            or expected.produced ~= flow.produced
            or expected.consumed ~= flow.consumed
            or expected.production_before ~= flow.production_before
            or expected.consumption_before ~= flow.consumption_before
        then
            return false
        end
    end
    return true
end

-- Account only a complete deterministic recipe flow whose requested product
-- inventory increase and queue drain were already verified by the Rust craft
-- transaction. Standalone character crafting already updates native ingredient
-- consumption, so that evidence is verified but never injected. Only missing
-- production is added positively, allowing Factorio to evaluate craft-item
-- triggers asynchronously without this code ever mutating technologies.
function M.record_verified_flows(agent_id, operation_id, flows)
    if type(operation_id) ~= "string"
        or operation_id == ""
        or #operation_id > 128
        or not operation_id:match("^[A-Za-z0-9:_%-]+$")
    then
        return fail(
            "invalid_craft_operation_id",
            "operation_id must be a bounded generated identifier",
            "retry_verified_craft_accounting"
        )
    end

    local admission, admission_error = crafting.require_active_admission(agent_id, operation_id)
    if admission_error then return admission_error end

    local state = accounting_state()
    local previous = state.by_operation[operation_id]
    if previous then
        return {
            success = true,
            accounted = true,
            duplicate = true,
            operation_id = operation_id,
            flows = previous.flows,
            force = previous.force,
            surface = previous.surface,
            accounted_at_tick = previous.accounted_at_tick,
            technology_progression = "owned_by_factorio",
            native_consumption_verified = true,
            consumption_accounting = "owned_by_factorio",
            production_accounting = "exact_positive_npc_flow",
        }
    end

    local character = characters.find(agent_id)
    if not (character and character.valid) then
        return fail(
            "no_character",
            "no valid character for craft accounting",
            "spawn_character"
        )
    end
    if character.player then
        return fail(
            "player_craft_accounting_owned_by_engine",
            "player-associated character crafts are already accounted by Factorio",
            "use_engine_player_craft_accounting"
        )
    end

    local validated, validation_error = validate_flows(flows)
    if validation_error then return validation_error end
    if not flows_match(admission.flows, validated) then
        return fail(
            "craft_flow_mismatch",
            "supplied craft flows do not exactly match the save-persisted admission",
            "reload_craft_admission",
            {operation_id = operation_id}
        )
    end

    local statistics = character.force.get_item_production_statistics(character.surface)
    local observations = {}
    for _, flow in ipairs(validated) do
        local production_observed = statistics.get_input_count(flow.name)
        local consumption_observed = statistics.get_output_count(flow.name)
        local consumption_target = flow.consumption_before + flow.consumed
        if flow.production_before + flow.produced > MAX_SAFE_INTEGER
            or consumption_target > MAX_SAFE_INTEGER
        then
            return fail(
                "craft_flow_target_overflow",
                "craft flow baseline plus admitted count exceeds the safe integer range",
                "inspect_craft_flow",
                {operation_id = operation_id, item = flow.name}
            )
        end
        if production_observed < flow.production_before
            or consumption_observed < flow.consumption_before
        then
            return fail(
                "craft_statistics_reset",
                "production statistics moved below the save-persisted craft baseline",
                "inspect_force_and_surface_context",
                {
                    operation_id = operation_id,
                    item = flow.name,
                    production_before = flow.production_before,
                    production_observed = production_observed,
                    consumption_before = flow.consumption_before,
                    consumption_observed = consumption_observed,
                }
            )
        end
        if consumption_observed < consumption_target then
            return fail(
                "craft_native_consumption_missing",
                "Factorio has not recorded the admitted craft's native ingredient consumption",
                "retry_verified_craft_accounting",
                {
                    operation_id = operation_id,
                    item = flow.name,
                    consumed = flow.consumed,
                    consumption_before = flow.consumption_before,
                    consumption_observed = consumption_observed,
                    consumption_target = consumption_target,
                    consumption_increase = consumption_observed - flow.consumption_before,
                }
            )
        end
        table.insert(observations, {
            flow = flow,
            production_observed = production_observed,
            consumption_observed = consumption_observed,
            consumption_target = consumption_target,
        })
    end

    local evidence = {}
    local production_verified = true
    for _, observation in ipairs(observations) do
        local flow = observation.flow
        -- Standalone NPC character crafting consumes ingredients natively but
        -- does not publish its products to force flow statistics. Add this
        -- operation's exact admitted production once; never infer it from a
        -- global delta that unrelated factory activity can satisfy.
        if flow.produced > 0 then statistics.on_flow(flow.name, flow.produced) end
        local production_after = statistics.get_input_count(flow.name)
        local consumption_after = statistics.get_output_count(flow.name)
        local production_minimum = observation.production_observed + flow.produced
        if production_after < production_minimum then production_verified = false end
        table.insert(evidence, {
            name = flow.name,
            produced = flow.produced,
            consumed = flow.consumed,
            production_before = flow.production_before,
            production_observed_before_accounting = observation.production_observed,
            production_minimum_after_accounting = production_minimum,
            production_injected = flow.produced,
            production_after = production_after,
            production_increase = production_after - flow.production_before,
            consumption_before = flow.consumption_before,
            consumption_target = observation.consumption_target,
            consumption_after = consumption_after,
            consumption_increase = consumption_after - flow.consumption_before,
            native_consumption_verified = consumption_after >= observation.consumption_target,
            consumption_injected = 0,
        })
    end
    if not production_verified then
        return fail(
            "craft_production_accounting_pending",
            "Factorio did not yet reflect all missing positive production flows",
            "retry_verified_craft_accounting",
            {operation_id = operation_id, flows = evidence}
        )
    end

    local record = {
        flows = evidence,
        force = character.force.name,
        surface = character.surface.name,
        accounted_at_tick = game.tick,
    }
    remember(state, operation_id, record)
    return {
        success = true,
        accounted = true,
        duplicate = false,
        operation_id = operation_id,
        flows = evidence,
        force = record.force,
        surface = record.surface,
        accounted_at_tick = record.accounted_at_tick,
        technology_progression = "owned_by_factorio",
        native_consumption_verified = true,
        consumption_accounting = "owned_by_factorio",
        production_accounting = "exact_positive_npc_flow",
        next_action = "wait_for_factorio_trigger_evaluation",
    }
end

return M
