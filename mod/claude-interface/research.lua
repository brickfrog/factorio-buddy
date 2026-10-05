local entities = require("entities")
local characters = require("characters")

local M = {}
local MAX_LAB_FEED_COUNT = 200
local MAX_MACHINE_FEED_CRAFTS = 20
local MACHINE_FEED_GUIDANCE = "One-time bootstrap (e.g. a first craft that triggers research on a new planet); feed it durably with inserters and belts afterwards."

local function pos_table(pos)
    if not pos then return nil end
    return {x = pos.x, y = pos.y}
end

local function research_ingredients(tech)
    local ingredients = {}
    for _, ing in pairs(tech.research_unit_ingredients or {}) do
        table.insert(ingredients, {name = ing.name, amount = ing.amount})
    end
    return ingredients
end

local function research_needs_science(tech)
    for _, _ in pairs(tech.research_unit_ingredients or {}) do
        return true
    end
    return false
end

local function trigger_value(trigger, field)
    local ok, value = pcall(function() return trigger[field] end)
    if not ok or value == nil then return nil end
    if type(value) == "string" or type(value) == "number" or type(value) == "boolean" then
        return value
    end
    local name_ok, name = pcall(function() return value.name end)
    if name_ok and name then return name end
    return tostring(value)
end

local function research_trigger(tech)
    local ok, trigger = pcall(function() return tech.prototype.research_trigger end)
    if not ok or not trigger then return nil end
    local result = {}
    for _, field in ipairs({"type", "item", "entity", "fluid", "count", "amount", "quality"}) do
        local value = trigger_value(trigger, field)
        if value ~= nil then result[field] = value end
    end
    return result
end

local function research_effects(tech)
    local effects = {}
    for _, eff in pairs(tech.prototype.effects) do
        if eff.type == "unlock-recipe" then
            table.insert(effects, {
                type = "unlock-recipe",
                recipe = eff.recipe,
            })
        elseif eff.type == "turret-attack" then
            table.insert(effects, {
                type = "turret-attack",
                turret_id = eff.turret_id,
                modifier = eff.modifier,
            })
        else
            table.insert(effects, {
                type = eff.type,
                modifier = eff.modifier,
            })
        end
    end
    return effects
end

local function lab_has_power(lab)
    local status = lab.status
    return status ~= defines.entity_status.no_power and status ~= defines.entity_status.low_power
end

local function science_totals_from_labs(labs)
    local science_totals = {}
    for _, lab in pairs(labs) do
        local inv = lab.get_inventory(defines.inventory.lab_input)
        if inv then
            for i = 1, #inv do
                local stack = inv[i]
                if stack and stack.valid_for_read then
                    science_totals[stack.name] = (science_totals[stack.name] or 0) + stack.count
                end
            end
        end
    end
    return science_totals
end

local function science_totals_list(science_totals)
    local result = {}
    for name, count in pairs(science_totals) do
        table.insert(result, {name = name, count = count})
    end
    return result
end

local function count_science_from_inventory(inv, science_totals, science_available)
    if not inv then return end
    for _, item in pairs(inv.get_contents()) do
        if item.name:find("science%-pack") or item.name == "automation-science-pack" or item.name == "logistic-science-pack" then
            science_totals[item.name] = (science_totals[item.name] or 0) + item.count
            local found = false
            for _, sci in pairs(science_available) do
                if sci.name == item.name then
                    sci.count = sci.count + item.count
                    sci.in_inventory = item.count
                    found = true
                    break
                end
            end
            if not found then
                table.insert(science_available, {name = item.name, count = item.count, in_inventory = item.count})
            end
        end
    end
end

local function lab_summary(lab)
    return {
        unit_number = lab.unit_number,
        name = lab.name,
        type = lab.type,
        position = pos_table(lab.position),
        powered = lab_has_power(lab),
    }
end

local function add_blocker(result, blocker_type, message)
    table.insert(result.blockers, {
        type = blocker_type,
        message = message,
    })
end

local function expected_miss(result, next_action)
    result.expected_miss = true
    result.next_action = next_action
    return result
end

function M.feed_lab_from_inventory(character, lab_unit_number, science_pack, count, dry_run)
    local parsed_count = tonumber(count)
    local do_dry_run = dry_run ~= false
    local result = {
        success = false,
        dry_run = do_dry_run,
        lab_unit_number = tonumber(lab_unit_number),
        science_pack = science_pack,
        requested_count = parsed_count or count,
        maximum_count = MAX_LAB_FEED_COUNT,
        inserted = 0,
        missing_items = {},
        blockers = {},
        steps = {},
        classification = "bootstrap_science_transfer",
        bootstrap = true,
        automation_complete = false,
    }

    if not parsed_count
        or parsed_count ~= parsed_count
        or parsed_count == math.huge
        or parsed_count == -math.huge
        or parsed_count <= 0
        or parsed_count ~= math.floor(parsed_count)
        or parsed_count > MAX_LAB_FEED_COUNT
    then
        result.error_kind = parsed_count and parsed_count > MAX_LAB_FEED_COUNT
            and "count_exceeds_limit" or "invalid_count"
        result.error = "count must be a positive integer no greater than " .. tostring(MAX_LAB_FEED_COUNT)
        result.action_needed = "choose_bounded_science_pack_count"
        add_blocker(result, result.error_kind, result.error)
        return result
    end
    count = parsed_count

    if not (character and character.valid) then
        add_blocker(result, "no_character", "No character for agent; spawn first.")
        return expected_miss(result, "spawn_character")
    end

    local lab = entities.find_by_unit_number(tonumber(lab_unit_number))
    if not (lab and lab.valid) then
        add_blocker(result, "lab_not_found", "No valid lab entity with unit_number " .. tostring(lab_unit_number) .. ".")
        return expected_miss(result, "get_entities")
    end
    result.lab = lab_summary(lab)

    if lab.type ~= "lab" then
        add_blocker(result, "not_a_lab", "Entity " .. tostring(lab_unit_number) .. " is " .. tostring(lab.name) .. ", not a lab.")
        return expected_miss(result, "choose_valid_lab")
    end

    local reach_error = characters.require_entity_reach(character, lab)
    if reach_error then
        result.error = reach_error.error
        result.error_kind = reach_error.error_kind
        result.action_needed = reach_error.action_needed
        result.character_position = reach_error.character_position
        result.target_position = reach_error.target_position
        result.distance = reach_error.distance
        result.max_distance = reach_error.max_distance
        add_blocker(result, "out_of_reach", "Walk to the lab before transferring science packs.")
        return result
    end

    local lab_inv = lab.get_inventory(defines.inventory.lab_input)
    if not lab_inv then
        add_blocker(result, "no_lab_inventory", "Lab has no lab_input inventory.")
        return expected_miss(result, "choose_valid_lab")
    end

    local player_inv = character.get_main_inventory()
    if not player_inv then
        add_blocker(result, "no_character_inventory", "Character has no main inventory.")
        return expected_miss(result, "spawn_character")
    end

    if not prototypes.item[science_pack] then
        add_blocker(result, "unknown_item", "Unknown item prototype: " .. tostring(science_pack))
        return expected_miss(result, "choose_valid_science_pack")
    end

    local can_insert_ok, can_insert_value = pcall(function()
        return lab_inv.can_insert{name = science_pack, count = math.min(count, 1)}
    end)
    if not can_insert_ok or can_insert_value ~= true then
        add_blocker(result, "lab_rejects_item", "Lab input inventory does not accept " .. tostring(science_pack) .. ".")
        return expected_miss(result, "choose_valid_science_pack")
    end

    local available = player_inv.get_item_count(science_pack)
    local lab_before = lab_inv.get_item_count(science_pack)
    result.available = available
    result.lab_before = lab_before
    if available < count then
        result.missing_items[science_pack] = {
            available = available,
            required = count,
        }
        add_blocker(result, "missing_science_pack", "Character inventory has " .. tostring(available) .. " " .. tostring(science_pack) .. ", need " .. tostring(count) .. ".")
        return expected_miss(result, "craft_science_pack")
    end

    result.transfer_count = count
    result.verify_step = {
        tool = "get_research_status",
        tool_args = {},
        description = "After feeding the lab, check current research and science packs in labs.",
    }

    if do_dry_run then
        result.ready_to_call = {
            tool = "feed_lab_from_inventory",
            args = {
                lab_unit_number = result.lab_unit_number,
                science_pack = science_pack,
                count = count,
                dry_run = false,
            },
        }
        result.steps = {{
            tool = result.ready_to_call.tool,
            args = result.ready_to_call.args,
            description = "Execute the validated one-time science-pack transfer into this exact lab.",
        }}
        result.success = true
        result.ready = true
        result.manual_bootstrap_available = true
        result.next_action = "feed_lab_from_inventory"
        result.follow_up_action = "automate_science_delivery"
        result.guidance = "Execute ready_to_call for the required one-time bootstrap transfer, then use belts and inserters for durable lab supply."
        return result
    end

    local removed = player_inv.remove{name = science_pack, count = count}
    if removed == 0 then
        add_blocker(result, "remove_failed", "Could not remove " .. tostring(science_pack) .. " from character inventory.")
        return expected_miss(result, "refresh_inventory")
    end

    local inserted = lab_inv.insert{name = science_pack, count = removed}
    local returned = 0
    if inserted < removed then
        returned = player_inv.insert{name = science_pack, count = removed - inserted}
    end

    result.inserted = inserted
    result.returned_to_inventory = returned
    result.lab_after = lab_inv.get_item_count(science_pack)
    result.inventory_after = player_inv.get_item_count(science_pack)
    result.lab_identity_preserved = lab.valid and lab.unit_number == result.lab_unit_number
    result.conservation = {
        removed = removed,
        inserted = inserted,
        returned = returned,
        balanced = removed == inserted + returned,
        lab_increase = result.lab_after - lab_before,
        character_decrease = available - result.inventory_after,
    }
    result.conservation.measured_balanced = result.conservation.lab_increase == inserted
        and result.conservation.character_decrease == inserted
    if not result.conservation.balanced or not result.conservation.measured_balanced then
        result.error_kind = "item_conservation_failure"
        result.error = "science-pack transfer did not conserve the measured lab and character inventories"
        result.action_needed = "stop_and_inspect_inventories"
        add_blocker(result, result.error_kind, result.error)
        return result
    end
    if not result.lab_identity_preserved then
        result.error_kind = "entity_identity_changed"
        result.error = "lab identity changed during science-pack transfer"
        result.action_needed = "stop_and_inspect_lab"
        add_blocker(result, result.error_kind, result.error)
        return result
    end
    if inserted == 0 then
        add_blocker(result, "lab_inventory_full", "Lab input inventory accepted 0 " .. tostring(science_pack) .. ".")
        return expected_miss(result, "free_lab_inventory_or_choose_another_lab")
    end

    result.success = true
    result.next_action = "get_research_status"
    result.follow_up_actions = {"build_automation_science", "build_lab_feed"}
    result.guidance = "Science packs transferred once. Use build_automation_science and build_lab_feed before treating research logistics as complete."
    return result
end

local function machine_summary(machine)
    return {
        unit_number = machine.unit_number,
        name = machine.name,
        type = machine.type,
        position = pos_table(machine.position),
    }
end

-- Load whole crafts of a crafting machine's current recipe (item ingredients
-- only, all together) from the character inventory. Fluids must be piped.
function M.feed_machine_from_inventory(character, unit_number, crafts, dry_run)
    local parsed_crafts = tonumber(crafts)
    local do_dry_run = dry_run ~= false
    local result = {
        success = false,
        dry_run = do_dry_run,
        unit_number = tonumber(unit_number),
        requested_crafts = parsed_crafts or crafts,
        maximum_crafts = MAX_MACHINE_FEED_CRAFTS,
        crafts_loaded = 0,
        inserted = {},
        missing_items = {},
        fluid_ingredients = {},
        blockers = {},
        classification = "bootstrap_machine_transfer",
        bootstrap = true,
        automation_complete = false,
        guidance = MACHINE_FEED_GUIDANCE,
    }

    if not parsed_crafts
        or parsed_crafts ~= parsed_crafts
        or parsed_crafts == math.huge
        or parsed_crafts == -math.huge
        or parsed_crafts <= 0
        or parsed_crafts ~= math.floor(parsed_crafts)
        or parsed_crafts > MAX_MACHINE_FEED_CRAFTS
    then
        result.error_kind = parsed_crafts and parsed_crafts > MAX_MACHINE_FEED_CRAFTS
            and "crafts_exceed_limit" or "invalid_crafts"
        result.error = "crafts must be a positive integer no greater than " .. tostring(MAX_MACHINE_FEED_CRAFTS)
        result.action_needed = "choose_bounded_craft_count"
        add_blocker(result, result.error_kind, result.error)
        return result
    end

    if not (character and character.valid) then
        add_blocker(result, "no_character", "No character for agent; spawn first.")
        return expected_miss(result, "spawn_character")
    end

    local machine = entities.find_by_unit_number(result.unit_number)
    if not (machine and machine.valid) then
        add_blocker(result, "machine_not_found", "No valid entity with unit_number " .. tostring(unit_number) .. ".")
        return expected_miss(result, "get_entities")
    end
    result.machine = machine_summary(machine)

    if machine.type ~= "assembling-machine" then
        add_blocker(result, "not_a_crafting_machine", "Entity " .. tostring(unit_number) .. " is " .. tostring(machine.name)
            .. " (" .. tostring(machine.type) .. "), not an assembler, chemical plant, or foundry.")
        return expected_miss(result, "choose_crafting_machine")
    end

    local recipe, quality = machine.get_recipe()
    if not recipe then
        add_blocker(result, "no_recipe", tostring(machine.name) .. " " .. tostring(unit_number) .. " has no recipe; set one first.")
        return expected_miss(result, "set_recipe")
    end
    local quality_name = quality and quality.name or "normal"
    result.recipe = recipe.name
    result.recipe_quality = quality_name

    local item_ingredients = {}
    for _, ingredient in ipairs(recipe.ingredients) do
        if ingredient.type == "fluid" then
            table.insert(result.fluid_ingredients, {name = ingredient.name, amount = ingredient.amount})
        else
            table.insert(item_ingredients, {name = ingredient.name, amount = ingredient.amount})
        end
    end
    if #result.fluid_ingredients > 0 then
        result.fluid_guidance = "Fluid ingredients cannot be loaded from inventory; pipe them into the machine."
    end
    if #item_ingredients == 0 then
        add_blocker(result, "no_item_ingredients", "Recipe " .. recipe.name .. " has no item ingredients; pipe its fluids in instead.")
        return expected_miss(result, "pipe_fluid_ingredients")
    end

    local reach_error = characters.require_entity_reach(character, machine)
    if reach_error then
        result.error = reach_error.error
        result.error_kind = reach_error.error_kind
        result.action_needed = reach_error.action_needed
        result.character_position = reach_error.character_position
        result.target_position = reach_error.target_position
        result.distance = reach_error.distance
        result.max_distance = reach_error.max_distance
        add_blocker(result, "out_of_reach", "Walk to the machine before loading ingredients.")
        return result
    end

    local machine_inv = machine.get_inventory(defines.inventory.assembling_machine_input)
    if not machine_inv then
        add_blocker(result, "no_machine_input_inventory", "Machine has no assembling_machine_input inventory.")
        return expected_miss(result, "choose_crafting_machine")
    end

    local player_inv = character.get_main_inventory()
    if not player_inv then
        add_blocker(result, "no_character_inventory", "Character has no main inventory.")
        return expected_miss(result, "spawn_character")
    end

    local loadable = parsed_crafts
    for _, ingredient in ipairs(item_ingredients) do
        local accepts_ok, accepts = pcall(function()
            return machine_inv.can_insert{name = ingredient.name, quality = quality_name, count = ingredient.amount}
        end)
        if not accepts_ok or accepts ~= true then
            add_blocker(result, "machine_rejects_item", "Machine input does not accept more " .. ingredient.name .. ".")
            return expected_miss(result, "wait_for_machine_to_consume_input")
        end
        local id = {name = ingredient.name, quality = quality_name}
        ingredient.available = player_inv.get_item_count(id)
        ingredient.machine_before = machine_inv.get_item_count(id)
        loadable = math.min(loadable, math.floor(ingredient.available / ingredient.amount))
        if ingredient.available < ingredient.amount * parsed_crafts then
            result.missing_items[ingredient.name] = {
                available = ingredient.available,
                required_per_craft = ingredient.amount,
                required = ingredient.amount * parsed_crafts,
            }
        end
    end
    if loadable <= 0 then
        add_blocker(result, "missing_ingredients", "Character inventory lacks one whole craft of " .. recipe.name .. "; see missing_items.")
        return expected_miss(result, "craft_or_collect")
    end
    result.crafts_to_load = loadable
    result.planned_items = {}
    for _, ingredient in ipairs(item_ingredients) do
        table.insert(result.planned_items, {name = ingredient.name, count = ingredient.amount * loadable})
    end

    if do_dry_run then
        result.ready_to_call = {
            tool = "feed_machine_from_inventory",
            args = {
                unit_number = result.unit_number,
                crafts = loadable,
                dry_run = false,
            },
        }
        result.steps = {{
            tool = result.ready_to_call.tool,
            args = result.ready_to_call.args,
            description = "Execute the validated one-time ingredient load into this exact machine.",
        }}
        result.success = true
        result.ready = true
        result.manual_bootstrap_available = true
        result.next_action = "feed_machine_from_inventory"
        return result
    end

    local crafts_loaded = loadable
    local balanced = true
    result.conservation = {}
    for _, ingredient in ipairs(item_ingredients) do
        local id = {name = ingredient.name, quality = quality_name}
        local wanted = ingredient.amount * loadable
        local removed = player_inv.remove{name = ingredient.name, quality = quality_name, count = wanted}
        local inserted = 0
        if removed > 0 then
            inserted = machine_inv.insert{name = ingredient.name, quality = quality_name, count = removed}
        end
        local returned = 0
        if inserted < removed then
            returned = player_inv.insert{name = ingredient.name, quality = quality_name, count = removed - inserted}
        end
        local record = {
            name = ingredient.name,
            wanted = wanted,
            removed = removed,
            inserted = inserted,
            returned = returned,
            balanced = removed == inserted + returned,
            machine_increase = machine_inv.get_item_count(id) - ingredient.machine_before,
            character_decrease = ingredient.available - player_inv.get_item_count(id),
        }
        record.measured_balanced = record.machine_increase == inserted and record.character_decrease == inserted
        balanced = balanced and record.balanced and record.measured_balanced
        table.insert(result.conservation, record)
        if inserted > 0 then
            table.insert(result.inserted, {name = ingredient.name, count = inserted})
        end
        crafts_loaded = math.min(crafts_loaded, math.floor(inserted / ingredient.amount))
    end
    result.crafts_loaded = crafts_loaded
    result.machine_identity_preserved = machine.valid and machine.unit_number == result.unit_number

    if not balanced then
        result.error_kind = "item_conservation_failure"
        result.error = "ingredient transfer did not conserve the measured machine and character inventories"
        result.action_needed = "stop_and_inspect_inventories"
        add_blocker(result, result.error_kind, result.error)
        return result
    end
    if not result.machine_identity_preserved then
        result.error_kind = "entity_identity_changed"
        result.error = "machine identity changed during ingredient transfer"
        result.action_needed = "stop_and_inspect_machine"
        add_blocker(result, result.error_kind, result.error)
        return result
    end
    if crafts_loaded == 0 then
        add_blocker(result, "machine_input_full", "Machine input accepted no whole craft; leftovers returned to your inventory.")
        return expected_miss(result, "wait_for_machine_to_consume_input")
    end
    if crafts_loaded < loadable then
        add_blocker(result, "partial_load", "Machine input accepted " .. tostring(crafts_loaded) .. " of " .. tostring(loadable)
            .. " crafts; leftovers returned to your inventory.")
    end

    result.success = true
    result.next_action = "build_assembler_feed"
    result.follow_up_actions = {"build_assembler_feed", "verify_production"}
    return result
end

-- Research is force-wide: count labs on every planet surface, so status
-- stays right while the character is on a platform or another planet.
local function force_labs(force)
    local labs = {}
    for _, surface in pairs(game.surfaces) do
        if surface.platform == nil then
            for _, lab in pairs(surface.find_entities_filtered{type = "lab", force = force}) do
                labs[#labs + 1] = lab
            end
        end
    end
    return labs
end

function M.get_research_status(character)
    if not (character and character.valid) then
        return {success = false, error = "no character; spawn first"}
    end
    local force = character.force
    local result = {
        researched_count = 0,
        total_count = 0,
        current_research = nil,
        research_progress = 0,
        research_queue = {},
        labs = {
            count = 0,
            powered = 0,
            working = 0,
        },
        science_packs_in_labs = {},
    }

    for _, tech in pairs(force.technologies) do
        result.total_count = result.total_count + 1
        if tech.researched then
            result.researched_count = result.researched_count + 1
        end
    end

    if force.current_research then
        local tech = force.current_research
        result.current_research = {
            name = tech.name,
            level = tech.level,
            research_unit_count = tech.research_unit_count,
            ingredients = research_ingredients(tech),
            trigger = research_trigger(tech),
        }
        result.research_progress = force.research_progress
    end

    if force.research_queue then
        for _, tech in pairs(force.research_queue) do
            table.insert(result.research_queue, {
                name = tech.name,
                level = tech.level,
            })
        end
    end

    local labs = force_labs(force)
    result.labs.count = #labs

    local science_totals = science_totals_from_labs(labs)
    for _, lab in pairs(labs) do
        local status = lab.status
        if status == defines.entity_status.working then
            result.labs.working = result.labs.working + 1
            result.labs.powered = result.labs.powered + 1
        elseif lab_has_power(lab) then
            result.labs.powered = result.labs.powered + 1
        end
    end

    result.science_packs_in_labs = science_totals_list(science_totals)

    if result.labs.count == 0 then
        result.message = "No labs found! Build a lab and insert science packs to research."
    elseif result.labs.powered == 0 then
        result.message = "Labs have no power! Connect labs to the power grid."
    elseif result.current_research and #result.science_packs_in_labs == 0 then
        result.message = "Labs are empty! Insert science packs into labs to progress research."
    end

    return result
end

function M.get_available_research(character)
    if not (character and character.valid) then
        return {success = false, error = "no character; spawn first", technologies = {}}
    end
    local force = character.force
    local result = {
        technologies = {},
        lab_status = {
            count = 0,
            powered = 0,
        },
        science_available = {},
    }

    local labs = force_labs(force)
    result.lab_status.count = #labs

    local science_totals = science_totals_from_labs(labs)
    for _, lab in pairs(labs) do
        if lab_has_power(lab) then
            result.lab_status.powered = result.lab_status.powered + 1
        end
    end

    result.science_available = science_totals_list(science_totals)

    if character and character.valid then
        count_science_from_inventory(character.get_main_inventory(), science_totals, result.science_available)
    end

    for _, tech in pairs(force.technologies) do
        if tech.enabled and not tech.researched then
            local can_research = true
            for _, prereq in pairs(tech.prerequisites) do
                if not prereq.researched then
                    can_research = false
                    break
                end
            end

            if can_research then
                local ingredients = {}
                local needs_science = research_needs_science(tech)
                local trigger = research_trigger(tech)
                local has_all_packs = not needs_science or result.lab_status.powered > 0
                for _, ing in pairs(tech.research_unit_ingredients or {}) do
                    local have = science_totals[ing.name] or 0
                    if have < ing.amount then
                        has_all_packs = false
                    end
                    table.insert(ingredients, {
                        name = ing.name,
                        amount = ing.amount,
                        available = have,
                    })
                end

                local ready = trigger and "trigger_required" or "queueable"
                local blockers = {}
                if needs_science then
                    if result.lab_status.count == 0 then
                        table.insert(blockers, "no labs - build a lab first")
                    elseif result.lab_status.powered == 0 then
                        table.insert(blockers, "labs have no power")
                    end
                    if not has_all_packs then
                        table.insert(blockers, "missing science packs in labs")
                    end
                end

                table.insert(result.technologies, {
                    name = tech.name,
                    level = tech.level,
                    research_unit_count = tech.research_unit_count,
                    ingredients = ingredients,
                    effects = research_effects(tech),
                    requires_lab = needs_science,
                    trigger = trigger,
                    queueable = needs_science,
                    ready = ready,
                    blockers = blockers,
                })
            end
        end
    end

    local has_trigger_tech = false
    for _, tech in pairs(result.technologies) do
        if tech.trigger then
            has_trigger_tech = true
            break
        end
    end

    if has_trigger_tech then
        result.guidance = "Trigger technologies are completed only by their listed in-game trigger. Build or craft what the trigger requires; start_research will not unlock them."
    elseif result.lab_status.count == 0 then
        result.guidance = "Build and power a lab, then automate science-pack production and use build_lab_feed or build_automation_science to supply it continuously. Manual inventory transfer is bootstrap only, not completed research logistics."
    elseif result.lab_status.powered == 0 then
        result.guidance = "Labs need power! Connect them to your power grid (steam engine -> power poles -> lab)"
    elseif #result.science_available == 0 then
        result.guidance = "Automate science-pack production and continuous belt/inserter delivery into the lab with build_lab_feed or build_automation_science. Red science requires automated iron-gear-wheel and copper-plate inputs; do not use repeated hand-feeding as the production path."
    end

    return result
end

function M.start_research(character, tech_name)
    if not (character and character.valid) then
        return {success = false, error = "no character; spawn first"}
    end
    local force = character.force
    local tech = force.technologies[tech_name]

    if not tech then
        return {success = false, error = "Technology not found"}
    end

    if tech.researched then
        return {success = false, error = "Already researched"}
    end

    if not tech.enabled then
        return {success = false, error = "Technology not enabled"}
    end

    -- A prerequisite already in the research queue will finish first, so
    -- Factorio accepts the dependent technology behind it.
    local queued = {}
    local queue_ok, queue = pcall(function() return force.research_queue end)
    if queue_ok and queue then
        for _, entry in pairs(queue) do queued[entry.name] = true end
    end
    for _, prereq in pairs(tech.prerequisites) do
        if not prereq.researched and not queued[prereq.name] then
            return {success = false, error = "Prerequisites not met: " .. prereq.name}
        end
    end

    local ingredients = research_ingredients(tech)
    local needs_science = research_needs_science(tech)
    local trigger = research_trigger(tech)
    if trigger then
        return {
            success = false,
            error = "Technology requires an in-game research trigger and cannot be queued or force-completed",
            error_kind = "research_trigger_required",
            action_needed = "complete_research_trigger",
            name = tech.name,
            trigger = trigger,
            requires_lab = false,
        }
    end
    if not needs_science then
        return {
            success = false,
            error = "Technology has no science units or supported research trigger; refusing to force-complete it",
            error_kind = "unsupported_research_definition",
        }
    end

    local labs = force_labs(force)
    local powered_labs = 0
    for _, lab in pairs(labs) do
        if lab_has_power(lab) then
            powered_labs = powered_labs + 1
        end
    end
    local science_in_labs = science_totals_from_labs(labs)
    local missing_packs = {}
    for _, ing in pairs(tech.research_unit_ingredients or {}) do
        local have = science_in_labs[ing.name] or 0
        if have < ing.amount then
            table.insert(missing_packs, ing.name .. " (need " .. ing.amount .. ", have " .. have .. " in labs)")
        end
    end

    -- "Start" means next: put the technology at the front of the queue,
    -- behind only its own queued prerequisites. Appending would leave it
    -- behind a technology whose packs are not made, which never finishes.
    local was_queued = queued[tech.name] == true
    local needed = {}
    local function mark(t)
        for _, prereq in pairs(t.prerequisites) do
            if not prereq.researched and not needed[prereq.name] then
                needed[prereq.name] = true
                mark(prereq)
            end
        end
    end
    mark(tech)
    local order, rest = {}, {}
    for _, entry in pairs(queue_ok and queue or {}) do
        if needed[entry.name] then order[#order + 1] = entry.name
        elseif entry.name ~= tech.name then rest[#rest + 1] = entry.name end
    end
    order[#order + 1] = tech.name
    for _, name in ipairs(rest) do order[#order + 1] = name end
    local set_ok = pcall(function() force.research_queue = order end)
    local queue_now = {}
    local added = false
    local read_ok, current_queue = pcall(function() return force.research_queue end)
    for _, entry in pairs(read_ok and current_queue or {}) do
        queue_now[#queue_now + 1] = entry.name
        if entry.name == tech.name then added = true end
    end
    if not added and not set_ok then
        added = force.add_research(tech)
    end
    if added then
        return {
            success = true,
            name = tech.name,
            research_unit_count = tech.research_unit_count,
            ingredients = ingredients,
            lab_status = {
                count = #labs,
                powered = powered_labs,
                missing_packs = missing_packs,
            },
            moved_to_front = was_queued or nil,
            dropped_from_queue = (function()
                local kept, dropped = {}, {}
                for _, name in ipairs(queue_now) do kept[name] = true end
                for _, name in ipairs(order) do
                    if not kept[name] then dropped[#dropped + 1] = name end
                end
                return dropped[1] and dropped or nil
            end)(),
            queue = queue_now,
            queue_length = #queue_now,
            message = "Research queued first. Keep at least 3 technologies queued so labs never idle, and automate any missing lab power or science delivery.",
        }
    end

    return {
        success = false,
        error_kind = "research_queue_refused",
        error = "Factorio refused to queue it; the queue may be full",
        queue = queue_now,
    }
end

function M.is_tech_researched(character, tech_name)
    if not (character and character.valid) then
        return {researched = false, error = "no character; spawn first"}
    end
    local tech = character.force.technologies[tech_name]
    if not tech then
        return {researched = false, error = "Technology not found"}
    end
    return {researched = tech.researched == true}
end

-- Launch the rocket of one of the agent force's silos on the character's
-- surface. With `unit_number` nil, the first silo whose rocket is ready is
-- used. The rocket only leaves once the silo reports rocket_ready; the
-- force's rockets_launched counter rises when the flight ends (~20 s later).
function M.launch_rocket(character, unit_number)
    if not (character and character.valid) then
        return {success = false, error_kind = "no_character", error = "no character for this agent; spawn first"}
    end
    if unit_number ~= nil and type(unit_number) ~= "number" then
        return {success = false, error_kind = "invalid_unit_number", error = "unit_number must be a number or omitted"}
    end
    local status_names = {}
    for name, value in pairs(defines.rocket_silo_status) do status_names[value] = name end
    local silos = character.surface.find_entities_filtered{type = "rocket-silo", force = character.force}
    local summaries, chosen = {}, nil
    for _, silo in pairs(silos) do
        local status = status_names[silo.rocket_silo_status] or "unknown"
        summaries[#summaries + 1] = {
            unit_number = silo.unit_number,
            position = pos_table(silo.position),
            status = status,
            rocket_parts = silo.rocket_parts,
        }
        if (unit_number == nil or silo.unit_number == unit_number)
            and status == "rocket_ready" and not chosen then
            chosen = silo
        end
    end
    local launched_before = character.force.rockets_launched
    if not chosen then
        -- Platform cargo queued on this surface rides each ready rocket by
        -- itself (space.process_shipments); polling here only spends turns.
        local queued = false
        for _, shipment in ipairs(storage.space_shipments or {}) do
            if shipment.surface_index == character.surface.index and shipment.inventory and shipment.inventory.valid
                and not shipment.inventory.is_empty()
            then
                queued = true
            end
        end
        return {
            success = false,
            error_kind = #silos == 0 and "no_rocket_silo" or "rocket_not_ready",
            error = #silos == 0 and "the force has no rocket silo on this surface"
                or "no matching silo has a ready rocket; a rocket needs the silo's full rocket_parts and power, then about 20 s to rise",
            silos = summaries,
            rockets_launched = launched_before,
            guidance = queued and "Queued space_platform cargo launches by itself as soon as a rocket is ready: do not wait or call launch_rocket for it. Spend the time on the rocket-part inputs (silos shows rocket_parts)." or nil,
        }
    end
    local ok, launched = pcall(function() return chosen.launch_rocket() end)
    if not (ok and launched) then
        return {
            success = false,
            error_kind = "launch_refused",
            error = ok and "the silo refused to launch" or tostring(launched),
            silos = summaries,
            rockets_launched = launched_before,
        }
    end
    return {
        success = true,
        silo_unit_number = chosen.unit_number,
        status = status_names[chosen.rocket_silo_status] or "unknown",
        rockets_launched_before = launched_before,
        note = "The rocket is in flight; the force's rockets_launched rises when it reaches orbit (about 20 s).",
    }
end

return M
