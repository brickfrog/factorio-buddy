local characters = require("characters")

local M = {}

local MAX_CRAFT_COUNT = 10000
local MAX_FLOW_COUNT = 100000
local MAX_ITEM_FLOWS = 32

local function state()
    local admissions = storage.factorio_buddy_craft_admissions or {}
    admissions.by_agent = admissions.by_agent or {}
    admissions.terminal_by_agent = admissions.terminal_by_agent or {}
    admissions.next_sequence = admissions.next_sequence or 0
    storage.factorio_buddy_craft_admissions = admissions
    return admissions
end

local function shallow_copy(value)
    local result = {}
    for key, item in pairs(value or {}) do result[key] = item end
    return result
end

local function character_context(character)
    if not (character and character.valid) then return {} end
    return {
        character_unit_number = character.unit_number,
        force_name = character.force and character.force.name or nil,
        surface_name = character.surface and character.surface.name or nil,
    }
end

local function identity_context(agent_id, admission)
    local current_character = characters.find(agent_id)
    local current = character_context(current_character)
    local expected = {
        character_unit_number = admission.character_unit_number,
        force_name = admission.force_name,
        surface_name = admission.surface_name,
    }
    local valid = current_character ~= nil
        and expected.character_unit_number ~= nil
        and current.character_unit_number == expected.character_unit_number
        and current.force_name == expected.force_name
        and current.surface_name == expected.surface_name
    local result = {
        identity_valid = valid,
        current_character_unit_number = current.character_unit_number,
        current_force_name = current.force_name,
        current_surface_name = current.surface_name,
    }
    if not valid then
        result.identity_error = {
            error_kind = "craft_character_changed",
            error = "the persisted craft belongs to a different or missing character context",
            expected = expected,
            current = current,
        }
    end
    return result, current_character
end

local function record_with_identity(agent_id, record)
    local result = shallow_copy(record)
    local identity = identity_context(agent_id, record)
    result.identity_valid = identity.identity_valid
    result.current_character_unit_number = identity.current_character_unit_number
    result.current_force_name = identity.current_force_name
    result.current_surface_name = identity.current_surface_name
    result.identity_error = identity.identity_error
    return result
end

local function queue_summary(character)
    local queue = {}
    if not character then return queue end
    for _, item in ipairs(character.crafting_queue or {}) do
        local recipe = item.recipe
        if type(recipe) ~= "string" then recipe = recipe and recipe.name end
        table.insert(queue, {recipe = recipe, count = item.count})
    end
    return queue
end

local function queue_snapshot(character, operation_id)
    local queue = queue_summary(character)
    return {
        success = true,
        operation_id = operation_id,
        queue_size = character.crafting_queue_size or 0,
        current_recipe = queue[1] and queue[1].recipe or nil,
        queue = queue,
    }
end

local function failure(character, recipe_name, error_kind, message, extra)
    local result = extra or {}
    result.success = false
    result.queued = 0
    result.queue_size = character and character.valid and character.crafting_queue_size or 0
    result.queue = queue_summary(character)
    result.recipe = recipe_name
    result.error_kind = error_kind
    result.error = message
    return result
end

local function bounded_integer(value, maximum)
    return type(value) == "number"
        and value == value
        and value ~= math.huge
        and value ~= -math.huge
        and value > 0
        and value == math.floor(value)
        and value <= maximum
end

local function deterministic_item_amount(item_type, amount, probability)
    local chance = probability or 1
    if item_type ~= "item"
        or chance ~= 1
        or not bounded_integer(amount, MAX_FLOW_COUNT)
    then
        return nil
    end
    return amount
end

local function deterministic_product_amount(product)
    if (product.extra_count_fraction or 0) ~= 0 then return nil end
    return deterministic_item_amount(product.type, product.amount, product.probability)
end

local function requested_product_expectations(recipe, inventory, crafts)
    local products = {}
    local totals = {}
    for _, product in pairs(recipe.products or {}) do
        local amount = deterministic_product_amount(product)
        if not amount then return {}, false end
        local total = amount * crafts
        if total > MAX_FLOW_COUNT then return {}, false end
        totals[product.name] = (totals[product.name] or 0) + total
        if totals[product.name] > MAX_FLOW_COUNT then return {}, false end
    end
    for name, expected_increase in pairs(totals) do
        table.insert(products, {
            name = name,
            before_count = inventory.get_item_count(name),
            expected_increase = expected_increase,
        })
    end
    table.sort(products, function(a, b) return a.name < b.name end)
    return products, #products > 0
end

local function add_flow(flows, name, produced, consumed)
    local flow = flows[name] or {name = name, produced = 0, consumed = 0}
    flow.produced = flow.produced + produced
    flow.consumed = flow.consumed + consumed
    if flow.produced > MAX_FLOW_COUNT or flow.consumed > MAX_FLOW_COUNT then return false end
    flows[name] = flow
    return true
end

local function recipe_flows(recipe, crafts, flows)
    if recipe.hidden_from_flow_stats then return false end
    for _, ingredient in pairs(recipe.ingredients or {}) do
        local amount = deterministic_item_amount(ingredient.type, ingredient.amount, 1)
        local ignored = tonumber(ingredient.ignored_by_stats or 0)
        if not amount
            or not ignored
            or ignored < 0
            or ignored ~= math.floor(ignored)
            or ignored > amount
            or not add_flow(flows, ingredient.name, 0, (amount - ignored) * crafts)
        then
            return false
        end
    end
    for _, product in pairs(recipe.products or {}) do
        local amount = deterministic_product_amount(product)
        local ignored = tonumber(product.ignored_by_stats or 0)
        if not amount
            or not ignored
            or ignored < 0
            or ignored ~= math.floor(ignored)
            or ignored > amount
        then
            return false
        end
        local produced = (amount - ignored) * crafts
        if not add_flow(flows, product.name, produced, 0) then
            return false
        end
    end
    return true
end

local function admitted_flows(requested_recipe, accepted_count, queue)
    local flows = {}
    local entries = queue
    if #entries == 0 then
        entries = {{recipe = requested_recipe.name, count = accepted_count}}
    end
    for _, item in ipairs(entries) do
        local recipe = item.recipe and prototypes.recipe[item.recipe] or nil
        if not recipe or not bounded_integer(item.count, MAX_CRAFT_COUNT)
            or not recipe_flows(recipe, item.count, flows)
        then
            return {}, false
        end
    end
    local result = {}
    for _, flow in pairs(flows) do
        if flow.produced > 0 or flow.consumed > 0 then table.insert(result, flow) end
    end
    table.sort(result, function(a, b) return a.name < b.name end)
    return result, #result > 0 and #result <= MAX_ITEM_FLOWS
end

local function capture_flow_baselines(character, flows)
    local statistics = character.force.get_item_production_statistics(character.surface)
    for _, flow in ipairs(flows) do
        flow.production_before = statistics.get_input_count(flow.name)
        flow.consumption_before = statistics.get_output_count(flow.name)
    end
end

local function terminal_status_valid(terminal_status)
    return type(terminal_status) == "string"
        and #terminal_status >= 1
        and #terminal_status <= 64
        and terminal_status:match("^[a-z][a-z0-9_%-]*$") ~= nil
end

function M.craft(agent_id, recipe_name, count)
    local character = characters.find(agent_id)
    if not (character and character.valid) then
        return failure(nil, recipe_name, "no_character", "no character for agent " .. tostring(agent_id) .. "; spawn first")
    end
    if not bounded_integer(count, MAX_CRAFT_COUNT) then
        return failure(character, recipe_name, "invalid_craft_count", "count must be a positive integer no greater than " .. tostring(MAX_CRAFT_COUNT))
    end

    local admissions = state()
    local previous = admissions.by_agent[agent_id]
    if previous then
        return failure(
            character,
            recipe_name,
            "craft_admission_pending",
            "resolve the existing craft admission with wait_for_crafting before starting another",
            {operation_id = previous.operation_id}
        )
    end
    if character.crafting_queue_size ~= 0 then
        return failure(
            character,
            recipe_name,
            "untracked_crafting_queue",
            "the character already has an untracked crafting queue; wait for it to drain before starting a verified craft"
        )
    end

    local recipe = prototypes.recipe[recipe_name]
    if not recipe then
        return failure(character, recipe_name, "unknown_recipe", "unknown recipe")
    end
    local force_recipe = character.force.recipes[recipe_name]
    if force_recipe and not force_recipe.enabled then
        return failure(character, recipe_name, "recipe_disabled", "recipe is disabled")
    end
    local inventory = character.get_main_inventory()
    if not inventory then
        return failure(character, recipe_name, "no_character_inventory", "character has no main inventory")
    end

    local before_products, product_proof_complete = requested_product_expectations(recipe, inventory, 1)
    local ok, crafted_or_error = pcall(function()
        return character.begin_crafting{recipe = recipe_name, count = count}
    end)
    if not ok then
        return failure(character, recipe_name, "craft_start_failed", tostring(crafted_or_error))
    end

    local crafted = crafted_or_error
    local result = {
        success = crafted > 0,
        queued = crafted,
        queue_size = character.crafting_queue_size,
        queue = queue_summary(character),
        recipe = recipe_name,
    }
    if crafted <= 0 then
        result.error_kind = "craft_not_started"
        result.error = "crafting did not start; check ingredients, recipe category, or character craftability"
        return result
    end

    -- Preserve the pre-admission inventory baseline while scaling only the
    -- deterministic output amount to the exact accepted count.
    for _, product in ipairs(before_products) do
        product.expected_increase = product.expected_increase * crafted
        if product.expected_increase > MAX_FLOW_COUNT then
            product_proof_complete = false
            before_products = {}
            break
        end
    end
    local flows, flow_accounting_complete = admitted_flows(recipe, crafted, result.queue)
    if flow_accounting_complete then
        local baseline_ok = pcall(capture_flow_baselines, character, flows)
        if not baseline_ok then
            flows = {}
            flow_accounting_complete = false
        end
    end
    admissions.next_sequence = admissions.next_sequence + 1
    local operation_id = "craft-" .. tostring(game.tick) .. "-" .. tostring(admissions.next_sequence)
    result.operation_id = operation_id
    local context = character_context(character)
    admissions.by_agent[agent_id] = {
        operation_id = operation_id,
        admitted_at_tick = game.tick,
        character_unit_number = context.character_unit_number,
        force_name = context.force_name,
        surface_name = context.surface_name,
        identity_valid = true,
        completion_receipt = false,
        result = result,
        products = before_products,
        product_proof_complete = product_proof_complete,
        flows = flows,
        flow_accounting_complete = flow_accounting_complete,
    }
    -- A successfully admitted operation is the acknowledgement boundary for
    -- the preceding terminal receipt. Rejected requests leave it replayable.
    admissions.terminal_by_agent[agent_id] = nil
    return result
end

function M.get_admission(agent_id)
    local admissions = state()
    local admission = admissions.by_agent[agent_id]
    if admission then return record_with_identity(agent_id, admission) end
    local receipt = admissions.terminal_by_agent[agent_id]
    if receipt then return record_with_identity(agent_id, receipt) end
    if not admission then
        return {
            success = false,
            error_kind = "missing_craft_admission",
            error = "no persisted craft admission for agent " .. tostring(agent_id),
        }
    end
end

function M.clear_admission(agent_id, operation_id, terminal_status)
    if not terminal_status_valid(terminal_status) then
        return failure(
            characters.find(agent_id),
            nil,
            "invalid_craft_terminal_status",
            "terminal_status must be a bounded generated snake-case identifier"
        )
    end
    local admissions = state()
    local admission = admissions.by_agent[agent_id]
    if not admission then
        local receipt = admissions.terminal_by_agent[agent_id]
        if receipt and receipt.operation_id == operation_id then
            if receipt.terminal_status ~= terminal_status then
                return failure(
                    characters.find(agent_id),
                    nil,
                    "craft_terminal_status_mismatch",
                    "refusing to acknowledge one operation with two terminal statuses",
                    {
                        operation_id = operation_id,
                        expected_terminal_status = receipt.terminal_status,
                        requested_terminal_status = terminal_status,
                    }
                )
            end
            return {
                success = true,
                cleared = true,
                duplicate = true,
                completion_receipt = true,
                operation_id = operation_id,
                terminal_status = terminal_status,
                receipt = shallow_copy(receipt),
            }
        end
        return failure(
            characters.find(agent_id),
            nil,
            "missing_craft_admission",
            "no matching active craft admission or terminal receipt",
            {
                requested_operation_id = operation_id,
                existing_receipt_operation_id = receipt and receipt.operation_id or nil,
            }
        )
    end
    if admission.operation_id ~= operation_id then
        return failure(
            characters.find(agent_id),
            nil,
            "craft_operation_mismatch",
            "refusing to clear a different craft admission",
            {expected_operation_id = admission.operation_id, requested_operation_id = operation_id}
        )
    end
    local identity = identity_context(agent_id, admission)
    if not identity.identity_valid and terminal_status ~= "craft_character_changed" then
        return failure(
            characters.find(agent_id),
            admission.result and admission.result.recipe or nil,
            "craft_character_changed",
            "refusing to complete a craft against a different or missing character context",
            {
                operation_id = operation_id,
                identity_valid = false,
                identity_error = identity.identity_error,
            }
        )
    end
    local receipt = record_with_identity(agent_id, admission)
    receipt.completion_receipt = true
    receipt.terminal_status = terminal_status
    receipt.completed_at_tick = game.tick
    admissions.by_agent[agent_id] = nil
    admissions.terminal_by_agent[agent_id] = receipt
    return {
        success = true,
        cleared = true,
        duplicate = false,
        completion_receipt = true,
        operation_id = operation_id,
        terminal_status = terminal_status,
        receipt = shallow_copy(receipt),
    }
end

-- Internal accounting boundary: return only the exact active operation and
-- refuse to account against a replaced character, force, or surface.
function M.require_active_admission(agent_id, operation_id)
    local admissions = state()
    local admission = admissions.by_agent[agent_id]
    if not admission then
        local receipt = admissions.terminal_by_agent[agent_id]
        return nil, failure(
            characters.find(agent_id),
            nil,
            receipt and "craft_already_terminal" or "missing_craft_admission",
            receipt and "the craft operation already has a terminal receipt"
                or "no active craft admission exists for accounting",
            {
                requested_operation_id = operation_id,
                receipt_operation_id = receipt and receipt.operation_id or nil,
                terminal_status = receipt and receipt.terminal_status or nil,
            }
        )
    end
    if admission.operation_id ~= operation_id then
        return nil, failure(
            characters.find(agent_id),
            admission.result and admission.result.recipe or nil,
            "craft_operation_mismatch",
            "refusing to account a different craft admission",
            {
                expected_operation_id = admission.operation_id,
                requested_operation_id = operation_id,
            }
        )
    end
    local identity, current_character = identity_context(agent_id, admission)
    if not identity.identity_valid then
        return nil, failure(
            current_character,
            admission.result and admission.result.recipe or nil,
            "craft_character_changed",
            "the persisted craft belongs to a different or missing character context",
            {
                operation_id = operation_id,
                identity_valid = false,
                identity_error = identity.identity_error,
                character_unit_number = admission.character_unit_number,
                force_name = admission.force_name,
                surface_name = admission.surface_name,
                current_character_unit_number = identity.current_character_unit_number,
                current_force_name = identity.current_force_name,
                current_surface_name = identity.current_surface_name,
            }
        )
    end
    return admission, nil
end

-- Accounting receipts stay idempotent while either an active transaction or
-- its replayable terminal receipt references them.
function M.operation_is_referenced(operation_id)
    if type(operation_id) ~= "string" then return false end
    local admissions = state()
    for _, admission in pairs(admissions.by_agent) do
        if admission.operation_id == operation_id then return true end
    end
    for _, receipt in pairs(admissions.terminal_by_agent) do
        if receipt.operation_id == operation_id then return true end
    end
    return false
end

function M.queue_snapshot(agent_id)
    local admissions = state()
    local admission = admissions.by_agent[agent_id]
    if admission then
        local identity, character = identity_context(agent_id, admission)
        if not identity.identity_valid then
            return failure(
                character,
                admission.result and admission.result.recipe or nil,
                "craft_character_changed",
                "the persisted craft belongs to a different or missing character context",
                {
                    operation_id = admission.operation_id,
                    identity_valid = false,
                    identity_error = identity.identity_error,
                    character_unit_number = admission.character_unit_number,
                    force_name = admission.force_name,
                    surface_name = admission.surface_name,
                    current_character_unit_number = identity.current_character_unit_number,
                    current_force_name = identity.current_force_name,
                    current_surface_name = identity.current_surface_name,
                }
            )
        end
        return queue_snapshot(character, admission.operation_id)
    end
    local receipt = admissions.terminal_by_agent[agent_id]
    if receipt then
        local identity, character = identity_context(agent_id, receipt)
        if not identity.identity_valid then
            return failure(
                character,
                receipt.result and receipt.result.recipe or nil,
                "craft_character_changed",
                "the terminal craft receipt belongs to a different or missing character context",
                {
                    operation_id = receipt.operation_id,
                    completion_receipt = true,
                    terminal_status = receipt.terminal_status,
                    identity_valid = false,
                    identity_error = identity.identity_error,
                }
            )
        end
        return queue_snapshot(character, receipt.operation_id)
    end
    local character = characters.find(agent_id)
    if character and character.valid then
        return queue_snapshot(character, nil)
    end
    return {
        success = false,
        error_kind = "no_character",
        error = "no character for agent " .. tostring(agent_id) .. "; spawn first",
        action_needed = "spawn_character",
    }
end

return M
