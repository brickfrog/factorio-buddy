local characters = require("characters")
local entities = require("entities")
local inventory = require("inventory")

local M = {}

local MAX_BOOTSTRAP_FUEL_COUNT = 10
local MAX_CHEST_COLLECTION_COUNT = 1000

local BOOTSTRAP_BURNER_TYPES = {
    ["burner-inserter"] = "inserter",
    ["burner-mining-drill"] = "mining-drill",
}

local COLLECTABLE_CHEST_TYPES = {
    ["container"] = true,
    ["logistic-container"] = true,
}

local function fail(error_kind, message, action_needed, extra)
    local result = extra or {}
    result.success = false
    result.error_kind = error_kind
    result.error = message
    result.action_needed = action_needed
    return result
end

local function position_table(position)
    return {x = position.x, y = position.y}
end

local function target_summary(entity)
    return {
        unit_number = entity.unit_number,
        name = entity.name,
        type = entity.type,
        position = position_table(entity.position),
        surface = entity.surface.name,
    }
end

local function validate_count(count, maximum, action_needed)
    if type(count) ~= "number"
        or count ~= count
        or count == math.huge
        or count == -math.huge
        or count <= 0
        or count ~= math.floor(count)
    then
        return fail(
            "invalid_count",
            "count must be a positive integer",
            action_needed,
            {requested_count = count, maximum_count = maximum}
        )
    end
    if count > maximum then
        return fail(
            "count_exceeds_limit",
            "count exceeds the bounded operation limit of " .. tostring(maximum),
            action_needed,
            {requested_count = count, maximum_count = maximum}
        )
    end
    return nil
end

local function validate_request(agent_id, unit_number, item, count, maximum, retry_action)
    local count_error = validate_count(count, maximum, retry_action)
    if count_error then return nil, nil, count_error end

    if type(unit_number) ~= "number"
        or unit_number ~= unit_number
        or unit_number <= 0
        or unit_number ~= math.floor(unit_number)
    then
        return nil, nil, fail(
            "invalid_unit_number",
            "unit_number must be an exact positive integer",
            "inspect_exact_entity",
            {unit_number = unit_number}
        )
    end

    if type(item) ~= "string" or item == "" or not prototypes.item[item] then
        return nil, nil, fail(
            "unknown_item",
            "unknown item prototype: " .. tostring(item),
            retry_action,
            {item = item}
        )
    end

    local character = characters.find(agent_id)
    if not (character and character.valid) then
        return nil, nil, fail(
            "no_character",
            "no character for agent " .. tostring(agent_id) .. "; spawn first",
            "spawn_character"
        )
    end

    local character_inventory = character.get_main_inventory()
    if not character_inventory then
        return nil, nil, fail(
            "no_character_inventory",
            "character has no main inventory",
            "spawn_character"
        )
    end

    local entity = entities.find_by_unit_number(unit_number)
    if not (entity and entity.valid) then
        return nil, nil, fail(
            "entity_not_found",
            "no valid entity with unit_number " .. tostring(unit_number),
            "inspect_exact_entity",
            {unit_number = unit_number}
        )
    end

    local reach_error = characters.require_entity_reach(character, entity)
    if reach_error then
        reach_error.success = false
        reach_error.action_needed = reach_error.action_needed or "walk_to"
        reach_error.unit_number = unit_number
        return nil, nil, reach_error
    end

    return character_inventory, entity, nil
end

local function conservation_record(removed, inserted, returned)
    return {
        removed = removed,
        inserted = inserted,
        returned = returned,
        balanced = removed == inserted + returned,
    }
end

local function prototype_name(value)
    if value == nil then return nil end
    if type(value) == "string" then return value end
    local ok, name = pcall(function() return value.name end)
    if not ok or name == nil or name == value then return nil end
    if type(name) == "string" then return name end
    return prototype_name(name)
end

local function current_burning_record(burner)
    local ok, burning = pcall(function() return burner.currently_burning end)
    if not ok or burning == nil then return nil end
    local name = prototype_name(burning.name)
    if not name then return nil end
    return {
        name = name,
        quality = prototype_name(burning.quality) or "normal",
    }
end

local function burner_state(entity)
    local fuel_inventory = entity and entity.valid
        and entity.get_inventory(defines.inventory.fuel)
        or nil
    local burner = entity and entity.valid and entity.burner or nil
    if not (fuel_inventory and burner) then return nil end

    local contents = inventory.contents(fuel_inventory)
    local fuel_total = 0
    for _, item in ipairs(contents) do fuel_total = fuel_total + item.count end
    local currently_burning = current_burning_record(burner)
    local remaining_burning_fuel = tonumber(burner.remaining_burning_fuel) or 0
    local heat = tonumber(burner.heat) or 0
    return {
        unit_number = entity.unit_number,
        name = entity.name,
        type = entity.type,
        surface_index = entity.surface.index,
        fuel_inventory = contents,
        fuel_total = fuel_total,
        currently_burning = currently_burning,
        remaining_burning_fuel = remaining_burning_fuel,
        heat = heat,
        cold = fuel_total == 0
            and currently_burning == nil
            and remaining_burning_fuel <= 0
            and heat <= 0,
    }
end

local function item_key(item)
    return tostring(item.name) .. "\0" .. tostring(item.quality or "normal")
end

local function contents_map(contents)
    local result = {}
    for _, item in ipairs(contents or {}) do
        result[item_key(item)] = {
            name = item.name,
            quality = item.quality or "normal",
            count = item.count,
        }
    end
    return result
end

local function same_item_record(left, right)
    if left == nil or right == nil then return left == right end
    return left.name == right.name
        and (left.quality or "normal") == (right.quality or "normal")
end

local function same_burner_state(left, right)
    if not (left and right) then return false end
    if left.unit_number ~= right.unit_number
        or left.name ~= right.name
        or left.type ~= right.type
        or left.surface_index ~= right.surface_index
        or not same_item_record(left.currently_burning, right.currently_burning)
        or math.abs((left.remaining_burning_fuel or 0) - (right.remaining_burning_fuel or 0)) > 0.000001
        or math.abs((left.heat or 0) - (right.heat or 0)) > 0.000001
    then
        return false
    end
    local left_items = contents_map(left.fuel_inventory)
    local right_items = contents_map(right.fuel_inventory)
    for key, item in pairs(left_items) do
        if not right_items[key] or right_items[key].count ~= item.count then return false end
    end
    for key, item in pairs(right_items) do
        if not left_items[key] or left_items[key].count ~= item.count then return false end
    end
    return true
end

function M.bootstrap_burner_once(agent_id, unit_number, fuel_item, count)
    local character_inventory, entity, request_error = validate_request(
        agent_id,
        unit_number,
        fuel_item,
        count,
        MAX_BOOTSTRAP_FUEL_COUNT,
        "bootstrap_burner_once"
    )
    if request_error then return request_error end

    local expected_type = BOOTSTRAP_BURNER_TYPES[entity.name]
    if expected_type == nil or entity.type ~= expected_type then
        return fail(
            "wrong_entity_type",
            "bootstrap_burner_once accepts only burner-mining-drill or burner-inserter",
            "choose_existing_burner_entity",
            {target = target_summary(entity), allowed_entities = {"burner-mining-drill", "burner-inserter"}}
        )
    end

    local fuel_inventory = entity.get_inventory(defines.inventory.fuel)
    if not fuel_inventory then
        return fail(
            "missing_fuel_inventory",
            "target burner has no fuel inventory",
            "choose_existing_burner_entity",
            {target = target_summary(entity)}
        )
    end

    local available_before = character_inventory.get_item_count(fuel_item)
    if available_before < count then
        return fail(
            "insufficient_fuel_items",
            "character does not have the full requested bootstrap fuel count",
            "obtain_fuel_items",
            {
                target = target_summary(entity),
                item = fuel_item,
                requested = count,
                available = available_before,
            }
        )
    end

    local can_insert_ok, can_insert = pcall(function()
        return fuel_inventory.can_insert{name = fuel_item, count = 1}
    end)
    if not can_insert_ok or can_insert ~= true then
        return fail(
            "fuel_not_accepted",
            "target fuel inventory rejects this item or has no free capacity",
            "choose_valid_fuel_or_free_capacity",
            {
                target = target_summary(entity),
                item = fuel_item,
                requested = count,
            }
        )
    end

    local target_before = fuel_inventory.get_item_count(fuel_item)
    local removed = character_inventory.remove{name = fuel_item, count = count}
    if removed ~= count then
        local restored = character_inventory.insert{name = fuel_item, count = removed}
        return fail(
            "fuel_remove_failed",
            "could not remove the full requested fuel count from character inventory",
            "refresh_inventory",
            {
                target = target_summary(entity),
                item = fuel_item,
                requested = count,
                removed = removed,
                restored = restored,
            }
        )
    end

    local inserted = fuel_inventory.insert{name = fuel_item, count = removed}
    local remainder = removed - inserted
    local returned = 0
    if remainder > 0 then
        returned = character_inventory.insert{name = fuel_item, count = remainder}
    end
    local conservation = conservation_record(removed, inserted, returned)
    local target_after = fuel_inventory.get_item_count(fuel_item)
    local character_after = character_inventory.get_item_count(fuel_item)
    conservation.target_increase = target_after - target_before
    conservation.character_decrease = available_before - character_after
    conservation.measured_balanced = conservation.target_increase == inserted
        and conservation.character_decrease == inserted
    if not conservation.balanced or not conservation.measured_balanced then
        return fail(
            "item_conservation_failure",
            "fuel transfer did not conserve the measured target and character inventories",
            "stop_and_inspect_inventories",
            {
                target = target_summary(entity),
                item = fuel_item,
                requested = count,
                conservation = conservation,
            }
        )
    end

    local identity_preserved = entity.valid and entity.unit_number == unit_number
    if not identity_preserved then
        return fail(
            "entity_identity_changed",
            "target entity identity changed during bootstrap fueling",
            "stop_and_inspect_target",
            {
                expected_unit_number = unit_number,
                item = fuel_item,
                requested = count,
                conservation = conservation,
                entity_identity_preserved = false,
            }
        )
    end
    if inserted == 0 then
        return fail(
            "fuel_not_inserted",
            "target fuel inventory accepted no items",
            "choose_valid_fuel_or_free_capacity",
            {
                target = target_summary(entity),
                item = fuel_item,
                requested = count,
                conservation = conservation,
                entity_identity_preserved = identity_preserved,
            }
        )
    end

    return {
        success = true,
        classification = "temporary_bootstrap",
        purpose = "temporary bootstrap",
        temporary_bootstrap = true,
        automation_complete = false,
        action_needed = "repair_fuel_sustainability",
        next_action = "repair_fuel_sustainability",
        target = target_summary(entity),
        entity_identity_preserved = identity_preserved,
        item = fuel_item,
        requested = count,
        available_before = available_before,
        target_before = target_before,
        target_after = target_after,
        character_after = character_after,
        inserted = inserted,
        partial = inserted < count,
        conservation = conservation,
        guidance = "Temporary bootstrap fuel only. Repair durable fuel delivery with repair_fuel_sustainability next.",
    }
end

-- Capture the exact pre-transaction burner state. The controller keeps this
-- ordinary JSON value only until its compound fuel build commits or rolls back.
function M.snapshot_burner_state(unit_number)
    local entity = entities.find_by_unit_number(unit_number)
    local snapshot = burner_state(entity)
    if not snapshot then
        return fail(
            "missing_burner_state",
            "fuel transaction target is missing or has no burner state",
            "refresh_fuel_diagnosis",
            {unit_number = unit_number}
        )
    end
    snapshot.success = true
    return snapshot
end

-- Atomically stop the transaction-created feeder and restore the pre-existing
-- consumer to its exact snapshot. This is an internal failure path, so it must
-- not depend on avatar reach and it must verify state equality before claiming
-- that transaction fuel was cleared.
function M.rollback_burner_bootstrap(agent_id, snapshot, feeder_unit_number)
    if type(snapshot) ~= "table" or type(snapshot.unit_number) ~= "number" then
        return fail(
            "invalid_burner_snapshot",
            "fuel rollback requires the exact pre-transaction burner snapshot",
            "stop_and_inspect_transaction"
        )
    end

    local character = characters.find(agent_id)
    if not (character and character.valid and character.get_main_inventory()) then
        return fail("no_character", "no character inventory for fuel rollback", "spawn_character")
    end

    local consumer = entities.find_by_unit_number(snapshot.unit_number)
    local identity_valid = consumer
        and consumer.valid
        and consumer.unit_number == snapshot.unit_number
        and consumer.name == snapshot.name
        and consumer.type == snapshot.type
        and consumer.surface.index == snapshot.surface_index
    if not identity_valid then
        return fail(
            "consumer_identity_changed",
            "fuel rollback refuses to restore a different entity",
            "stop_and_inspect_transaction",
            {snapshot = snapshot, current = consumer and target_summary(consumer) or nil}
        )
    end

    local feeder = nil
    local feeder_quiesced = feeder_unit_number == nil
    local feeder_error = nil
    if feeder_unit_number ~= nil then
        feeder = entities.find_by_unit_number(feeder_unit_number)
        if feeder and feeder.valid and feeder.type == "inserter" then
            local ok, error_message = pcall(function() feeder.active = false end)
            feeder_quiesced = ok and feeder.active == false
            if not ok then feeder_error = tostring(error_message) end
        else
            feeder_error = "transaction feeder is missing or no longer an inserter"
        end
    end

    local before = burner_state(consumer)
    local fuel_inventory = consumer.get_inventory(defines.inventory.fuel)
    local character_inventory = character.get_main_inventory()
    local current_items = contents_map(before and before.fuel_inventory or {})
    local snapshot_items = contents_map(snapshot.fuel_inventory or {})
    local excess = {}
    for key, item in pairs(current_items) do
        local baseline = snapshot_items[key] and snapshot_items[key].count or 0
        if item.count > baseline then
            table.insert(excess, {
                name = item.name,
                quality = item.quality,
                count = item.count - baseline,
            })
        end
    end

    fuel_inventory.clear()
    consumer.burner.currently_burning = nil
    consumer.burner.heat = 0

    local restore_errors = {}
    for _, item in ipairs(snapshot.fuel_inventory or {}) do
        local inserted = fuel_inventory.insert{
            name = item.name,
            quality = item.quality or "normal",
            count = item.count,
        }
        if inserted ~= item.count then
            table.insert(restore_errors, {
                stage = "fuel_inventory",
                name = item.name,
                quality = item.quality,
                expected = item.count,
                inserted = inserted,
            })
        end
    end
    if snapshot.currently_burning then
        consumer.burner.currently_burning = {
            name = snapshot.currently_burning.name,
            quality = snapshot.currently_burning.quality or "normal",
        }
        consumer.burner.remaining_burning_fuel = snapshot.remaining_burning_fuel or 0
    else
        consumer.burner.currently_burning = nil
    end
    consumer.burner.heat = snapshot.heat or 0

    local returned_excess = 0
    local spilled_excess = 0
    local unrecovered_excess = 0
    for _, item in ipairs(excess) do
        local returned = character_inventory.insert{
            name = item.name,
            quality = item.quality or "normal",
            count = item.count,
        }
        returned_excess = returned_excess + returned
        local remainder = item.count - returned
        if remainder > 0 then
            local spilled = consumer.surface.spill_item_stack{
                position = consumer.position,
                stack = {
                    name = item.name,
                    quality = item.quality or "normal",
                    count = remainder,
                },
                enable_looted = false,
                force = consumer.force,
                allow_belts = false,
                use_start_position_on_failure = true,
                drop_full_stack = true,
            }
            local spilled_count = 0
            for _, entity in pairs(spilled or {}) do
                local stack = entity and entity.valid and entity.stack or nil
                if stack and stack.valid_for_read
                    and stack.name == item.name
                    and (inventory.quality_name(stack) or "normal") == (item.quality or "normal")
                then
                    spilled_count = spilled_count + stack.count
                end
            end
            spilled_excess = spilled_excess + spilled_count
            unrecovered_excess = unrecovered_excess + (remainder - spilled_count)
        end
    end

    local after = burner_state(consumer)
    local consumer_state_restored = #restore_errors == 0 and same_burner_state(after, snapshot)
    local success = feeder_quiesced and consumer_state_restored and unrecovered_excess == 0
    return {
        success = success,
        classification = "failed_fuel_transaction_rollback",
        consumer_unit_number = snapshot.unit_number,
        feeder_unit_number = feeder_unit_number,
        feeder_quiesced = feeder_quiesced,
        feeder_error = feeder_error,
        consumer_state_restored = consumer_state_restored,
        transaction_fuel_cleared = consumer_state_restored,
        identity_valid = identity_valid,
        restore_errors = restore_errors,
        returned_excess = returned_excess,
        spilled_excess = spilled_excess,
        unrecovered_excess = unrecovered_excess,
        active_fuel_voided = before and before.currently_burning ~= nil
            and snapshot.currently_burning == nil,
        before = before,
        expected = snapshot,
        after = after,
    }
end

function M.collect_from_chest(agent_id, unit_number, item, count)
    local character_inventory, entity, request_error = validate_request(
        agent_id,
        unit_number,
        item,
        count,
        MAX_CHEST_COLLECTION_COUNT,
        "collect_from_chest"
    )
    if request_error then return request_error end

    if COLLECTABLE_CHEST_TYPES[entity.type] ~= true then
        return fail(
            "wrong_entity_type",
            "collect_from_chest accepts only container or logistic-container entities",
            "choose_existing_chest",
            {target = target_summary(entity), allowed_types = {"container", "logistic-container"}}
        )
    end

    local chest_inventory = entity.get_inventory(defines.inventory.chest)
    if not chest_inventory then
        return fail(
            "missing_chest_inventory",
            "target chest has no chest inventory",
            "choose_existing_chest",
            {target = target_summary(entity)}
        )
    end

    local available_before = chest_inventory.get_item_count(item)
    if available_before == 0 then
        return fail(
            "item_not_found",
            "target chest contains none of the requested item",
            "choose_item_or_chest_with_stock",
            {
                target = target_summary(entity),
                item = item,
                requested = count,
                available = 0,
            }
        )
    end

    local can_insert_ok, can_insert = pcall(function()
        return character_inventory.can_insert{name = item, count = 1}
    end)
    if not can_insert_ok or can_insert ~= true then
        return fail(
            "character_inventory_full",
            "character inventory cannot accept the requested item",
            "free_character_inventory_space",
            {
                target = target_summary(entity),
                item = item,
                requested = count,
                available = available_before,
            }
        )
    end

    local character_before = character_inventory.get_item_count(item)
    local attempted = math.min(count, available_before)
    local removed = chest_inventory.remove{name = item, count = attempted}
    if removed == 0 then
        return fail(
            "chest_remove_failed",
            "could not remove the requested item from the chest",
            "refresh_chest_inventory",
            {
                target = target_summary(entity),
                item = item,
                requested = count,
                available = available_before,
            }
        )
    end

    local inserted = character_inventory.insert{name = item, count = removed}
    local remainder = removed - inserted
    local returned = 0
    if remainder > 0 then
        returned = chest_inventory.insert{name = item, count = remainder}
    end
    local conservation = conservation_record(removed, inserted, returned)
    local chest_after = chest_inventory.get_item_count(item)
    local character_after = character_inventory.get_item_count(item)
    conservation.chest_decrease = available_before - chest_after
    conservation.character_increase = character_after - character_before
    conservation.measured_balanced = conservation.chest_decrease == inserted
        and conservation.character_increase == inserted
    local identity_preserved = entity.valid and entity.unit_number == unit_number
    if not conservation.balanced or not conservation.measured_balanced then
        return fail(
            "item_conservation_failure",
            "collection did not conserve the measured chest and character inventories",
            "stop_and_inspect_inventories",
            {
                target = target_summary(entity),
                item = item,
                requested = count,
                conservation = conservation,
                entity_identity_preserved = identity_preserved,
            }
        )
    end
    if not identity_preserved then
        return fail(
            "entity_identity_changed",
            "target chest identity changed during collection",
            "stop_and_inspect_target",
            {
                expected_unit_number = unit_number,
                item = item,
                requested = count,
                conservation = conservation,
                entity_identity_preserved = false,
            }
        )
    end
    if inserted == 0 then
        return fail(
            "character_inventory_full",
            "character inventory accepted no items",
            "free_character_inventory_space",
            {
                target = target_summary(entity),
                item = item,
                requested = count,
                conservation = conservation,
                entity_identity_preserved = identity_preserved,
            }
        )
    end

    local partial_reasons = {}
    if available_before < count then table.insert(partial_reasons, "source_shortfall") end
    if inserted < removed then table.insert(partial_reasons, "character_inventory_capacity") end

    return {
        success = true,
        classification = "bounded_construction_or_recovery_collection",
        purpose = "bounded construction/recovery collection",
        bounded_collection = true,
        automation_complete = false,
        action_needed = "use_collected_items_for_construction_or_recovery",
        target = target_summary(entity),
        entity_identity_preserved = identity_preserved,
        item = item,
        requested = count,
        available_before = available_before,
        attempted = attempted,
        transferred = inserted,
        extracted = inserted,
        partial = inserted < count,
        partial_reasons = partial_reasons,
        chest_after = chest_after,
        character_before = character_before,
        character_after = character_after,
        conservation = conservation,
        guidance = "Bounded collection for construction or recovery only. This manual transfer is not automated logistics or production completion.",
    }
end

return M
