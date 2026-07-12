local entities = require("entities")

local M = {}

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
    count = math.max(1, math.floor(tonumber(count) or 1))
    local do_dry_run = dry_run ~= false
    local result = {
        success = false,
        dry_run = do_dry_run,
        lab_unit_number = tonumber(lab_unit_number),
        science_pack = science_pack,
        requested_count = count,
        inserted = 0,
        missing_items = {},
        blockers = {},
        steps = {},
    }

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
        result.steps = {
            {
                tool = "feed_lab_from_inventory",
                tool_args = {
                    lab_unit_number = tonumber(lab_unit_number),
                    science_pack = science_pack,
                    count = count,
                    dry_run = false,
                },
                description = "Transfer science packs from the agent inventory into the target lab.",
            },
        }
        result.success = true
        result.ready = true
        result.next_action = "execute_feed_lab_from_inventory"
        result.guidance = "Call feed_lab_from_inventory again with dry_run=false, then run verify_step."
        return result
    end

    local removed = player_inv.remove{name = science_pack, count = count}
    if removed == 0 then
        add_blocker(result, "remove_failed", "Could not remove " .. tostring(science_pack) .. " from character inventory.")
        return expected_miss(result, "refresh_inventory")
    end

    local inserted = lab_inv.insert{name = science_pack, count = removed}
    if inserted < removed then
        player_inv.insert{name = science_pack, count = removed - inserted}
    end

    result.inserted = inserted
    result.returned_to_inventory = math.max(0, removed - inserted)
    result.lab_after = lab_inv.get_item_count(science_pack)
    result.inventory_after = player_inv.get_item_count(science_pack)
    if inserted == 0 then
        add_blocker(result, "lab_inventory_full", "Lab input inventory accepted 0 " .. tostring(science_pack) .. ".")
        return expected_miss(result, "free_lab_inventory_or_choose_another_lab")
    end

    result.success = true
    result.next_action = "get_research_status"
    result.guidance = "Science packs transferred from character inventory into the lab."
    return result
end

function M.get_research_status()
    local force = game.forces.player
    local surface = game.surfaces[1]
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

    local labs = surface.find_entities_filtered{type = "lab", force = force}
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
    local force = game.forces.player
    local surface = game.surfaces[1]
    local result = {
        technologies = {},
        lab_status = {
            count = 0,
            powered = 0,
        },
        science_available = {},
    }

    local labs = surface.find_entities_filtered{type = "lab", force = force}
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

                local ready = "ready"
                local blockers = {}
                if needs_science then
                    if result.lab_status.count == 0 then
                        ready = "blocked"
                        table.insert(blockers, "no labs - build a lab first")
                    elseif result.lab_status.powered == 0 then
                        ready = "blocked"
                        table.insert(blockers, "labs have no power")
                    end
                    if not has_all_packs then
                        ready = "blocked"
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
                    ready = ready,
                    blockers = blockers,
                })
            end
        end
    end

    local has_ready_free_tech = false
    for _, tech in pairs(result.technologies) do
        if tech.ready == "ready" and tech.requires_lab == false then
            has_ready_free_tech = true
            break
        end
    end

    if has_ready_free_tech then
        result.guidance = "Free bootstrap technologies need no lab or science packs. Call start_research on a ready technology before building labs."
    elseif result.lab_status.count == 0 then
        result.guidance = "To research: 1) Craft a lab (requires iron-gear-wheel, electronic-circuit, transport-belt), 2) Place it with power, 3) Craft science packs, 4) Insert science packs into lab"
    elseif result.lab_status.powered == 0 then
        result.guidance = "Labs need power! Connect them to your power grid (steam engine -> power poles -> lab)"
    elseif #result.science_available == 0 then
        result.guidance = "Craft science packs and insert them into labs. Red science (automation-science-pack) requires iron-gear-wheel + copper-plate"
    end

    return result
end

function M.start_research(tech_name)
    local force = game.forces.player
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

    for _, prereq in pairs(tech.prerequisites) do
        if not prereq.researched then
            return {success = false, error = "Prerequisites not met: " .. prereq.name}
        end
    end

    local ingredients = research_ingredients(tech)
    local needs_science = research_needs_science(tech)
    if not needs_science then
        local ok, err = pcall(function()
            tech.researched = true
        end)
        if ok and tech.researched then
            return {
                success = true,
                name = tech.name,
                research_unit_count = tech.research_unit_count,
                ingredients = ingredients,
                requires_lab = false,
                message = "Research completed. This technology requires no labs or science packs.",
            }
        end
        return {success = false, error = "Failed to complete free research: " .. tostring(err)}
    end

    local surface = game.surfaces[1]
    local labs = surface.find_entities_filtered{type = "lab", force = force}
    if #labs == 0 then
        return {
            success = false,
            error = "No labs found! Build a lab first (requires: 10 iron-gear-wheel, 10 electronic-circuit, 4 transport-belt)",
            action_needed = "build_lab",
        }
    end

    local powered_labs = 0
    for _, lab in pairs(labs) do
        if lab_has_power(lab) then
            powered_labs = powered_labs + 1
        end
    end
    if powered_labs == 0 then
        return {
            success = false,
            error = "Labs have no power! Connect labs to power grid.",
            action_needed = "power_labs",
        }
    end

    local missing_packs = {}
    local science_in_labs = science_totals_from_labs(labs)
    for _, ing in pairs(tech.research_unit_ingredients or {}) do
        local have = science_in_labs[ing.name] or 0
        if have < ing.amount then
            table.insert(missing_packs, ing.name .. " (need " .. ing.amount .. ", have " .. have .. " in labs)")
        end
    end

    if #missing_packs > 0 then
        return {
            success = false,
            error = "Missing science packs in labs: " .. table.concat(missing_packs, ", "),
            action_needed = "insert_science_packs",
            required_packs = ingredients,
            hint = "Craft the required science packs and insert them into your labs",
        }
    end

    local added = force.add_research(tech)
    if added then
        return {
            success = true,
            name = tech.name,
            research_unit_count = tech.research_unit_count,
            ingredients = ingredients,
            message = "Research queued! Labs will now consume science packs to progress.",
        }
    end

    return {success = false, error = "Failed to queue research - check if another research is in progress"}
end

function M.is_tech_researched(tech_name)
    local tech = game.forces.player.technologies[tech_name]
    if not tech then
        return {researched = false, error = "Technology not found"}
    end
    return {researched = tech.researched == true}
end

return M

