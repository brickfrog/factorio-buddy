local diagnostics = require("diagnostics")
local entities = require("entities")
local inventory = require("inventory")
local research = require("research")
local space = require("space")
local world = require("world")

local M = {}

local function position_table(position)
    if not position then return nil end
    return {x = position.x, y = position.y}
end

local function status_name(status_value)
    if status_value == nil then return nil end
    for name, value in pairs(defines.entity_status) do
        if value == status_value then return name end
    end
    return tostring(status_value)
end

local function entity_status(entity)
    local ok, value = pcall(function() return entity.status end)
    if not ok then return nil end
    return status_name(value)
end

local function energy_source(entity)
    local burner_ok, burner = pcall(function() return entity.burner end)
    if burner_ok and burner then return "burner" end

    local prototype = entity.prototype
    local electric_ok, electric = pcall(function()
        return prototype.electric_energy_source_prototype
    end)
    if electric_ok and electric then return "electric" end

    local heat_ok, heat = pcall(function()
        return prototype.heat_energy_source_prototype
    end)
    if heat_ok and heat then return "heat" end

    local fluid_ok, fluid = pcall(function()
        return prototype.fluid_energy_source_prototype
    end)
    if fluid_ok and fluid then return "fluid" end

    return "other"
end

local function sorted_counts(counts)
    local result = {}
    for name, count in pairs(counts) do
        table.insert(result, {name = name, count = count})
    end
    table.sort(result, function(a, b)
        if a.count == b.count then return a.name < b.name end
        return a.count > b.count
    end)
    return result
end

local function compact_production(surface_name, force)
    local statistics = diagnostics.production_statistics(surface_name, force)
    local active_items = {}
    local active_item_count = 0
    for _, item in ipairs(statistics.items or {}) do
        if (item.produced_per_minute or 0) ~= 0 or (item.consumed_per_minute or 0) ~= 0 then
            active_item_count = active_item_count + 1
            if #active_items < 50 then table.insert(active_items, item) end
        end
    end
    return {
        window = statistics.window,
        active_items = active_items,
        active_item_count = active_item_count,
        truncated = active_item_count > #active_items,
    }
end

local function character_snapshot(character)
    if not character or not character.valid then return nil end
    local main_inventory = character.get_main_inventory()
    return {
        position = position_table(character.position),
        health = character.health,
        inventory = inventory.contents(main_inventory),
    }
end

local function is_factory_entity(entity)
    if not (entity and entity.valid) or entity.type == "character" then return false end
    if entity.type == "entity-ghost" or entity.type == "tile-ghost" then return true end
    local prototype_ok, items_to_place = pcall(function()
        return entity.prototype.items_to_place_this
    end)
    return prototype_ok and items_to_place ~= nil and #items_to_place > 0
end

-- Below this many fuel items a burner drill/furnace is reported as running dry.
local LOW_BURNER_FUEL = 5

-- Machines counted for home roboport coverage.
local COVERAGE_TYPES = {
    ["assembling-machine"] = true, ["furnace"] = true, ["mining-drill"] = true,
    ["lab"] = true, ["rocket-silo"] = true,
}

local function tech_done(force, name)
    local tech = force.technologies[name]
    return tech ~= nil and tech.researched
end

local function produced_count(force, surface, item)
    local ok, count = pcall(function()
        return force.get_item_production_statistics(surface).get_input_count(item)
    end)
    return ok and count or 0
end

-- Units of `name` (an item, or a fluid when `fluid` is true) the force
-- produced on `surface` in the last ten minutes.
local function made_last_ten_minutes(force, surface, name, fluid)
    local ok, count = pcall(function()
        local stats = fluid and force.get_fluid_production_statistics(surface)
            or force.get_item_production_statistics(surface)
        return stats.get_flow_count{
            name = name,
            category = "input",
            precision_index = defines.flow_precision_index.ten_minutes,
            count = true,
        }
    end)
    return ok and math.floor(count or 0) or 0
end

-- The ingredients behind `item` that were not made in the last ten minutes,
-- following only the unmade ones (up to three recipe levels), each with its
-- recipe depth below `item`. Fluids are reported but not expanded: their
-- recipes (oil processing) have several outputs.
local function missing_supply_chain(force, surface, facts, item)
    local chain, listed = {}, {[item] = true}
    local function visit(name, depth)
        local recipe = prototypes.recipe[name]
        for _, ingredient in pairs(recipe and recipe.ingredients or {}) do
            local is_fluid = ingredient.type == "fluid"
            if not listed[ingredient.name]
                and made_last_ten_minutes(force, surface, ingredient.name, is_fluid) == 0
            then
                listed[ingredient.name] = true
                chain[#chain + 1] = {
                    name = ingredient.name,
                    fluid = is_fluid or nil,
                    needed_by = name,
                    depth = depth,
                    assemblers = facts.recipe_assemblers[ingredient.name] or 0,
                }
                if not is_fluid and depth < 3 then visit(ingredient.name, depth + 1) end
            end
        end
    end
    visit(item, 1)
    return chain
end

-- Assemblers set to `recipe` at home plus on every space platform.
local function recipe_machines(facts, recipe)
    local count = facts.recipe_assemblers[recipe] or 0
    for _, platform in ipairs(facts.space and facts.space.platforms or {}) do
        count = count + ((platform.recipes or {})[recipe] or 0)
    end
    return count
end

-- Units of `item` made in ten minutes at home plus on every space platform.
local function made_with_platforms(force, surface, facts, item)
    local count = made_last_ten_minutes(force, surface, item)
    for _, platform in ipairs(facts.space and facts.space.platforms or {}) do
        local platform_surface = platform.surface and game.get_surface(platform.surface)
        if platform_surface then count = count + made_last_ten_minutes(force, platform_surface, item) end
    end
    return count
end

-- " Not made in 10 min: x (for y, N assemblers), ..." for the unmade inputs
-- behind `item`, or "" when all are made.
local function supply_chain_text(force, surface, facts, item)
    local parts = {}
    for _, link in ipairs(missing_supply_chain(force, surface, facts, item)) do
        if #parts >= 6 then break end
        parts[#parts + 1] = link.name .. " (for " .. link.needed_by .. ", " .. link.assemblers .. " assemblers)"
    end
    return parts[1] and (" Not made in the last 10 min: " .. table.concat(parts, ", ") .. ".") or ""
end

-- A machine's recipe, or a furnace's last one while it sits idle.
local function machine_recipe(entity)
    local ok, recipe = pcall(function() return entity.get_recipe() end)
    if ok and recipe then return recipe end
    if entity.type ~= "furnace" then return nil end
    local prev_ok, previous = pcall(function() return entity.previous_recipe end)
    local name = prev_ok and previous and previous.name
    if type(name) ~= "string" then name = name and name.name end
    return name and prototypes.recipe[name] or nil
end

-- Items a machine blocked with full output holds back.
local function held_products(entity)
    if entity.type == "mining-drill" then
        local ok, target = pcall(function() return entity.mining_target end)
        return (ok and target and prototypes.item[target.name]) and {target.name} or {}
    end
    local items = {}
    local output = entity.get_inventory(defines.inventory.crafter_output)
    for _, stack in pairs(output and output.get_contents() or {}) do items[#items + 1] = stack.name end
    return items
end

-- Item ingredients a starved machine holds less of than one craft needs.
local function lacked_ingredients(entity)
    local recipe = machine_recipe(entity)
    local input = entity.get_inventory(defines.inventory.crafter_input)
    local items = {}
    for _, ingredient in pairs(recipe and input and recipe.ingredients or {}) do
        if ingredient.type == "item" and input.get_item_count(ingredient.name) < ingredient.amount then items[#items + 1] = ingredient.name end
    end
    return items
end

local function note_gap(by_item, item, position)
    local entry = by_item[item] or {count = 0, at = {}}
    entry.count = entry.count + 1
    if #entry.at < 2 then entry.at[#entry.at + 1] = math.floor(position.x) .. "," .. math.floor(position.y) end
    by_item[item] = entry
end

-- Items some machines hold back with full output while others starve for
-- them: the link between them is missing. Most starved first, at most three.
local function supply_gaps_text(facts)
    local gaps = {}
    for item, starved in pairs(facts.starved_for or {}) do
        local held = facts.held_output[item]
        if held then gaps[#gaps + 1] = {item = item, held = held, starved = starved} end
    end
    table.sort(gaps, function(a, b) return a.starved.count > b.starved.count or (a.starved.count == b.starved.count and a.item < b.item) end)
    local parts = {}
    for i = 1, math.min(3, #gaps) do
        local g = gaps[i]
        parts[#parts + 1] = g.item .. " (" .. g.held.count .. " full at " .. table.concat(g.held.at, " ") .. "; "
            .. g.starved.count .. " starved at " .. table.concat(g.starved.at, " ") .. ")"
    end
    return parts[1] and (" Made but not delivered: " .. table.concat(parts, ", ") .. ".") or ""
end

-- Rocket-part ingredients with how many of each were made in the last ten minutes.
local function rocket_part_inputs(force, surface)
    local recipe = prototypes.recipe["rocket-part"]
    local inputs = {}
    for _, ingredient in pairs(recipe and recipe.ingredients or {}) do
        inputs[#inputs + 1] = ingredient.name .. " (" .. made_last_ten_minutes(force, surface, ingredient.name) .. " made in 10 min)"
    end
    return table.concat(inputs, ", ")
end

-- Unresearched technologies on the way to `target_name`, prerequisites
-- first, and the ones whose prerequisites are all researched.
local function tech_path(force, surface, facts, target_name)
    local target = force.technologies[target_name]
    if not target then return nil end
    local needed, seen = {}, {}
    local function visit(tech)
        if seen[tech.name] then return end
        seen[tech.name] = true
        if tech.researched then return end
        for _, prerequisite in pairs(tech.prerequisites) do visit(prerequisite) end
        needed[#needed + 1] = tech
    end
    visit(target)
    local ready = {}
    for _, tech in ipairs(needed) do
        local all_done = true
        for _, prerequisite in pairs(tech.prerequisites) do
            if not prerequisite.researched then all_done = false break end
        end
        if all_done then ready[#ready + 1] = tech end
    end
    local function cost(tech)
        local ok, units = pcall(function() return tech.research_unit_count end)
        return (ok and units or 0) * math.max(1, #tech.research_unit_ingredients)
    end
    table.sort(ready, function(a, b)
        local ca, cb = cost(a), cost(b)
        if ca ~= cb then return ca < cb end
        return a.name < b.name
    end)
    local next_techs, packs, pack_order = {}, {}, {}
    for index, tech in ipairs(ready) do
        local trigger = tech.prototype.research_trigger
        local entry = {name = tech.name}
        if trigger then
            entry.trigger = trigger
        else
            entry.units = tech.research_unit_count
            entry.packs = {}
            for _, ingredient in pairs(tech.research_unit_ingredients) do
                entry.packs[#entry.packs + 1] = ingredient.name
                if not packs[ingredient.name] then
                    packs[ingredient.name] = {
                        name = ingredient.name,
                        assemblers = recipe_machines(facts, ingredient.name),
                        made_last_10_min = made_with_platforms(force, surface, facts, ingredient.name),
                        first_needed_by = tech.name,
                    }
                    pack_order[#pack_order + 1] = ingredient.name
                end
            end
        end
        if index <= 5 then next_techs[#next_techs + 1] = entry end
    end
    local pack_list = {}
    local bottleneck = nil
    for _, name in ipairs(pack_order) do
        local pack = packs[name]
        if pack.made_last_10_min == 0 then
            local missing = missing_supply_chain(force, surface, facts, name)
            pack.missing_inputs = missing[1] and missing or nil
            local recipe = prototypes.recipe[name]
            pack.ingredients_made_last_10_min = {}
            for _, ingredient in pairs(recipe and recipe.ingredients or {}) do
                pack.ingredients_made_last_10_min[ingredient.name] =
                    made_last_ten_minutes(force, surface, ingredient.name, ingredient.type == "fluid")
            end
        end
        pack_list[#pack_list + 1] = pack
        if not bottleneck or pack.made_last_10_min < bottleneck.made_last_10_min then
            bottleneck = pack
        end
    end
    -- True when no ready technology can progress on packs made recently:
    -- queueing more then only spends packs on whatever is off this path.
    local all_next_stalled = #ready > 0
    for _, tech in ipairs(ready) do
        local stalled = false
        if not tech.prototype.research_trigger then
            for _, ingredient in pairs(tech.research_unit_ingredients) do
                if packs[ingredient.name].made_last_10_min == 0 then stalled = true break end
            end
        end
        if not stalled then all_next_stalled = false break end
    end
    return {
        target = target_name,
        techs_remaining = #needed,
        researchable_now = next_techs,
        science_packs_needed = pack_list,
        slowest_pack = bottleneck and bottleneck.name or nil,
        all_next_stalled = all_next_stalled,
        labs = facts.labs,
    }
end

-- "<recipe>: <amount> <ingredient>, ... -> <amount> <product>, ..." read
-- from the prototype, so rung text never drifts from the game data.
local function recipe_text(name)
    local recipe = prototypes.recipe[name]
    if not recipe then return name .. ": (no recipe)" end
    local function list(entries)
        local parts = {}
        for _, entry in pairs(entries or {}) do
            local amount = entry.amount
            if not amount and entry.amount_min and entry.amount_max then
                amount = entry.amount_min == entry.amount_max and entry.amount_min
                    or (entry.amount_min .. "-" .. entry.amount_max)
            end
            local text = tostring(amount or 1) .. " " .. entry.name
            if entry.probability and entry.probability < 1 then
                text = text .. " (" .. math.floor(entry.probability * 100 + 0.5) .. "%)"
            end
            parts[#parts + 1] = text
        end
        return table.concat(parts, ", ")
    end
    return name .. ": " .. list(recipe.ingredients) .. " -> " .. list(recipe.products)
end

-- How to add the turrets a platform still needs: the free front-half slots
-- the platform status found (hub-fed pairs first, then turrets the hub
-- loads with magazines), else how to make room.
local function turret_plan(arms, surface)
    local slots = arms.turret_slots or {}
    local load = ((arms.missing_research or {})[1]
            and ("Research " .. table.concat(arms.missing_research, ", ") .. " (red and green science, start_research): flown tests lost six turrets with yellow magazines and no upgrades, while piercing rounds with these upgrades arrived intact. Make piercing-rounds-magazine and ship them. ") or "")
        .. ((arms.turrets or 0) > (arms.turrets_ready or 0)
            and "Turrets that are neither fed nor loaded: space_platform action=load_turrets has the hub fill them with magazines and keep them topped up. " or "")
    if slots[1] then
        local entities = {}
        for _, slot in ipairs(slots) do
            entities[#entities + 1] = string.format('{name:"gun-turret",dx:%s,dy:%s}', slot.turret.x, slot.turret.y)
            if slot.inserter then
                entities[#entities + 1] = string.format('{name:"inserter",dx:%s,dy:%s,direction:"%s"}',
                    slot.inserter.x, slot.inserter.y, slot.inserter_direction)
            end
        end
        local wanted = math.max((arms.min_ready_turrets or 0) - (arms.turrets_ready or 0), (arms.min_front_turrets or 0) - (arms.front_turrets_ready or 0))
        return load .. "Free turret slots, front first (an inserter beside the hub feeds a turret; turrets without one are loaded by the hub with magazines when built): place_ghosts surface="
            .. tostring(surface) .. " origin_x=0 origin_y=0 entities=[" .. table.concat(entities, ",") .. "]."
            .. (#slots < wanted and " That is not enough: extend space-platform-foundation forward (smaller y) for more turrets." or "")
    end
    return load .. "No free spot in the front half: extend space-platform-foundation forward (smaller y) with place_ghosts surface="
        .. tostring(surface) .. " and put gun turrets on it; the hub loads them with magazines."
end

-- Items whose all-time production on the away planet the rungs read.
local PLANET_MADE = {"carbon", "tungsten-carbide", "tungsten-plate", "foundry", "steel-plate", "metallurgic-science-pack"}

-- What stands on the planet the character is on, when that is not home and
-- not a platform (nil otherwise): entity counts, statuses, recipes by
-- machine, pumpjacks on sulfuric-acid geysers, and items made there.
local function planet_facts(character, force)
    local surface = character.surface
    if surface.platform or surface == space.home_surface(character) then return nil end
    local here = {
        surface = surface.name, counts = {}, statuses = {}, recipes = {},
        acid_pumpjacks = 0, acid_pumpjacks_working = 0, made = {}, ghosts = 0,
    }
    for _, entity in pairs(surface.find_entities_filtered{force = force}) do
        if entity.type == "entity-ghost" or entity.type == "tile-ghost" then
            here.ghosts = here.ghosts + 1
        elseif is_factory_entity(entity) then
            here.counts[entity.name] = (here.counts[entity.name] or 0) + 1
            local status = entity_status(entity)
            if status then here.statuses[status] = (here.statuses[status] or 0) + 1 end
            if entity.type == "assembling-machine" then
                local ok, recipe = pcall(function() return entity.get_recipe() end)
                if ok and recipe then
                    local entry = here.recipes[recipe.name] or {machines = 0, working = 0, statuses = {}}
                    entry.machines = entry.machines + 1
                    if status == "working" then entry.working = entry.working + 1 end
                    if status then entry.statuses[status] = (entry.statuses[status] or 0) + 1 end
                    here.recipes[recipe.name] = entry
                end
            elseif entity.type == "mining-drill" then
                local ok, target = pcall(function() return entity.mining_target end)
                if ok and target and target.valid and target.name == "sulfuric-acid-geyser" then
                    here.acid_pumpjacks = here.acid_pumpjacks + 1
                    if status == "working" then here.acid_pumpjacks_working = here.acid_pumpjacks_working + 1 end
                end
            end
        end
    end
    for _, item in ipairs(PLANET_MADE) do here.made[item] = produced_count(force, surface, item) end
    local ok, solar = pcall(function() return surface.planet.prototype.surface_properties["solar-power"] end)
    here.solar_power_percent = ok and solar or nil
    return here
end

-- "machines (statuses)" for one recipe on the away planet, or "none".
local function recipe_state(here, recipe)
    local entry = here.recipes[recipe]
    if not entry then return "none placed" end
    local parts = {}
    for status, count in pairs(entry.statuses) do parts[#parts + 1] = count .. " " .. status end
    table.sort(parts)
    return entry.machines .. " placed (" .. table.concat(parts, ", ") .. ")"
end

local REMOTE_HOME = " Nauvis runs without you: check it with robot_logistics surface=nauvis and build there with place_ghosts surface=nauvis."

-- How to stock an empty home network from another planet.
local BOOTSTRAP_TEXT = " If robots lack the parts to build that (robot_logistics surface=nauvis missing_items), harvest them: get_entities surface=nauvis finds machines and chests in roboport range (machines blocked with full output hold their products), and place_ghosts surface=nauvis deconstruct=[{name,dx,dy}] has robots carry them and their contents into storage chests. set_recipe also works on Nauvis machines from here: an assembler fed from the network's chests can make what robots lack (requester chests, inserters, poles), one recipe after another. Robots work while you are away: keep advancing Vulcanus meanwhile."

-- True when a crusher on platform `P` makes iron ore (metallic asteroid
-- crushing): its thruster oxidizer then needs no ore from Nauvis, and a
-- standing ore order only fills its hub (p2).
local function crushes_ore(P)
    for recipe in pairs(P.recipes or {}) do
        if recipe:find("metallic%-asteroid%-crushing$") then return true end
    end
    return false
end

-- The standing supply a shuttle needs from Nauvis: the items argument, and
-- what they keep topped up.
local function supply_order_text(P)
    if crushes_ore(P) then
        return "items=[piercing-rounds-magazine 1100, space-platform-foundation 20]", "its ammo and spare foundation (its metallic crusher makes the thruster ore)"
    end
    return "items=[piercing-rounds-magazine 1100, iron-ore 1000, space-platform-foundation 20]", "its ammo, thruster ore and spare foundation"
end

-- Building a new shuttle over Nauvis while you stay on Vulcanus: no armed
-- platform orbits Vulcanus and no silo stands there, so nothing can bring
-- parts down or lift anything up. `stranded` is the unarmed platform still
-- orbiting Vulcanus (its layout is copied), or nil once it is lost. The
-- first missing step, or nil while the new shuttle is on its way.
local function home_shuttle_rung(S, stranded, force)
    local H = S.home_supply or {}
    local why = stranded
        and (stranded.name .. " is stuck over Vulcanus: " .. tostring((stranded.armament or {}).needs)
            .. ", and with no rocket silo here nothing can arm it. It cannot fly home, and asteroids wear it down.")
        or "No armed platform serves Vulcanus and no rocket silo stands here: nothing can bring parts down or take products up."
    if (H.silos_with_supply_chest or 0) == 0 then
        return "Give the Nauvis silo a supply chest from here: Vulcanus needs a new shuttle.",
            why .. " A new shuttle is built over Nauvis by its supply silo: place_ghosts surface=nauvis a requester-chest beside the rocket silo and an inserter from the chest into the silo, inside the roboport network."
    end
    local P = nil
    for _, platform in ipairs(S.platforms or {}) do
        if platform.space_location ~= "vulcanus" then P = platform end
    end
    if not P then
        return "Build a new shuttle over Nauvis from here.",
            why .. " space_platform action=create stops=[\"nauvis\"] creates one over Nauvis; Nauvis' supply silo launches its starter pack by itself once the home network holds one ("
            .. recipe_text("space-platform-starter-pack") .. "). Make it there with place_ghosts surface=nauvis if nothing does."
    end
    if P.space_location == nil then
        return nil
    end
    if P.state == "waiting_for_starter_pack" or P.state == "starter_pack_on_the_way" or P.state == "starter_pack_requested" then
        return "Get a space-platform-starter-pack into the Nauvis network for " .. P.name .. ".",
            recipe_text("space-platform-starter-pack") .. ". The supply silo launches it once robots can bring one to its supply chest"
            .. ((H.network_lacks and H.network_lacks[1]) and (". The network lacks: " .. table.concat(H.network_lacks, ", ")) or "")
            .. ". From here: place_ghosts surface=nauvis an assembler with that recipe, an inserter from it into a passive-provider-chest, and its inputs (requester chests or belts), inside the roboport network." .. BOOTSTRAP_TEXT
    end
    local supplied = {}
    for _, request in ipairs(P.supply and P.supply.requests or {}) do supplied[request.name] = true end
    if not ((supplied["piercing-rounds-magazine"] or supplied["copper-plate"]) and (supplied["iron-ore"] or crushes_ore(P))) then
        local items, keeps = supply_order_text(P)
        return "Set " .. P.name .. "'s standing supply from Nauvis.",
            "space_platform action=supply platform=" .. P.name .. " " .. items .. ": rockets keep " .. keeps .. " topped up while it orbits Nauvis, and from then on also send what its ghosts lack. A leg to Vulcanus spends 300-400 magazines and it leaves only with 600+ piercing rounds aboard."
    end
    -- A rocket lifts 50 piercing rounds but the plates for 160: a shuttle
    -- makes its own.
    if (P.ammo_lines or 0) == 0 and P.ammo_line_fits then
        return "Give " .. P.name .. " its own ammo line: space_platform action=ammo_line platform=" .. P.name .. ".",
            "One rocket lifts 1 t: 50 piercing-rounds-magazine (20 kg each), or the iron, steel and copper plates for about 160 made aboard. ammo_line places ghosts behind the hub of two assembling-machine-1 (firearm-magazine, piercing-rounds-magazine) with inserters to and from the hub, a medium pole and two solar panels on foundation, and switches the standing supply from rounds to iron-plate 800, steel-plate 100 and copper-plate 200. The line keeps making rounds in flight too."
    end
    if (P.thrusters or 0) == 0 and not (P.ghosts_missing_items and P.ghosts_missing_items[1]) then
        if stranded then
            return "Lay out " .. P.name .. " like " .. stranded.name .. ".",
                "space_platform action=copy_layout platform=" .. P.name .. " stops=[\"" .. stranded.name .. "\"] places ghosts of its foundation, turrets, thrusters and machines; the hub builds them from what the supply silo brings. Add what "
                .. stranded.name .. " lacked (asteroid collectors at the foundation edge for thruster fuel, more turrets at the front) with place_ghosts surface=" .. tostring(P.surface) .. "."
        end
        return "Lay out " .. P.name .. " as a shuttle with place_ghosts surface=" .. tostring(P.surface) .. ".",
            "Its ghosts' parts come up from Nauvis' network through the supply silo. It needs: " .. turret_plan(P.armament or {}, P.surface)
            .. " Thrusters at the back edge (" .. recipe_text("thruster") .. "), fed by pipes from chemical plants on " .. recipe_text("thruster-fuel") .. " and " .. recipe_text("thruster-oxidizer")
            .. "; asteroid collectors at the foundation edge with crushers for the carbon, iron and ice they need; solar panels and poles; inserters to and from the hub; space-platform-foundation tiles under all of it."
    end
    -- Everything the supply silo sends waits on its next rocket.
    local stalled = H.stalled_silo
    if stalled and H.imports and H.imports[1] then
        local home = game.get_surface("nauvis")
        return "Finish the Nauvis supply silo's rocket: " .. stalled.rocket_parts .. "/" .. stalled.parts_required .. " parts, it lacks " .. table.concat(stalled.lacks, ", ") .. ".",
            "Nothing reaches " .. P.name .. " until it launches. Each rocket part needs " .. (home and rocket_part_inputs(force, home) or "its ingredients")
            .. ". From here: diagnose_factory_blockers surface=nauvis x=" .. stalled.x .. " y=" .. stalled.y .. " shows what starves the silo's feed; then place_ghosts surface=nauvis assemblers for the missing ingredients (and their inputs) with inserters and belts into the silo." .. BOOTSTRAP_TEXT
    end
    if H.network_lacks and H.network_lacks[1] then
        return "Stock the Nauvis network with " .. table.concat(H.network_lacks, ", ") .. " for " .. P.name .. ".",
            "The supply silo lifts only what robots can bring. From here: place_ghosts surface=nauvis assemblers for each of these with an inserter into a passive-provider-chest inside the roboport network, fed from the existing lines; check with robot_logistics surface=nauvis." .. BOOTSTRAP_TEXT
    end
    if (P.stops or {})[1] == nil then
        return "Send " .. P.name .. " to shuttle between Nauvis and Vulcanus.",
            "space_platform action=schedule platform=" .. P.name .. " stops=[\"nauvis\",\"vulcanus\"]: it stays in orbit until armed and stocked (see departure_held), then leaves by itself; nobody needs to be aboard. Then request what Vulcanus needs on the pad here (space_platform action=request)."
    end
    if P.departure_held then
        return "Get " .. P.name .. " out of Nauvis orbit (held: " .. P.departure_held .. ").",
            (P.armament and not P.armament.armed) and ("It needs " .. tostring(P.armament.needs) .. "; the supply silo brings the magazines and turrets once the Nauvis network holds them.")
                or "Follow the platform warnings: thruster fuel and its ghosts' parts come from Nauvis' network through the supply silo."
    end
    return nil
end

-- The Vulcanus ladder, read from what stands there and the trigger
-- technologies: rocks and calcite, solar power, acid, one tungsten carbide
-- (unlocks foundry), a foundry (unlocks big-mining-drill), tungsten plate
-- (unlocks metallurgic science).
local function vulcanus_rung(here, force, character, S)
    local inventory = character.get_main_inventory()
    local function have(item) return inventory and inventory.get_item_count(item) or 0 end
    if not tech_done(force, "tungsten-carbide") then
        return "Mine a big volcanic rock: it unlocks tungsten-carbide research.",
            "find_nearest_minable big-volcanic-rock, walk_to it, mine_at it. Rocks drop tungsten-ore (no drill you have can mine tungsten ore patches), calcite, coal and iron and copper ore." .. REMOTE_HOME
    end
    if not tech_done(force, "calcite-processing") then
        return "Mine calcite: it unlocks calcite-processing research.",
            "find_nearest_resource calcite, walk_to it, mine_at it." .. REMOTE_HOME
    end
    local generators = (here.counts["solar-panel"] or 0) + (here.counts["steam-engine"] or 0) + (here.counts["steam-turbine"] or 0)
    if generators == 0 then
        return "Power Vulcanus with solar panels.",
            "There is no water here for steam; solar panels give " .. tostring(here.solar_power_percent or "?") .. "% power on Vulcanus. "
            .. recipe_text("solar-panel") .. " (you have " .. have("solar-panel") .. "). Place them with build_layout beside the machines and connect with poles; accumulators carry the night." .. REMOTE_HOME
    end
    -- The supply line: a landing pad here takes what the shuttle brings.
    if (here.counts["cargo-landing-pad"] or 0) == 0 and have("cargo-landing-pad") > 0 then
        return "Place your cargo-landing-pad on Vulcanus.",
            "build_layout it inside the power network. Platforms orbiting Vulcanus drop what it requests (space_platform action=request items=[...])." .. REMOTE_HOME
    end
    -- The carried silo is the only way anything leaves Vulcanus.
    local silo_here = character.surface.count_entities_filtered{type = "rocket-silo", force = force, limit = 1} > 0
    if not silo_here and have("rocket-silo") > 0 then
        return "Place the rocket silo you carried on Vulcanus.",
            "build_layout a rocket-silo (9x9) inside the power network with solar panels to spare (6x solar here). Then load one launch of rocket parts with feed_machine_from_inventory: "
            .. recipe_text("rocket-part") .. " x50. Metallurgic packs leave Vulcanus only from it." .. REMOTE_HOME
    end
    -- The pad takes from any platform in orbit: requests for a shuttle's
    -- thruster inputs keep it from ever flying home (p7, and a sandbox run
    -- from p7: 450 iron ore put in the hub was gone to the pad in seconds).
    for _, platform in ipairs(S.platforms or {}) do
        if platform.space_location == "vulcanus" and platform.thrust_short and platform.thrust_short[1] then
            local short = {}
            for _, producer in ipairs(platform.thrust_short) do
                for _, item in ipairs(producer.short or {}) do short[item.name] = true end
            end
            local drained = {}
            for _, pad in pairs(character.surface.find_entities_filtered{name = "cargo-landing-pad", force = force}) do
                for _, section in pairs(pad.get_logistic_sections().sections) do
                    if section.active then
                        for slot = 1, section.filters_count do
                            local filter = section.get_slot(slot)
                            local name = filter.value and filter.value.name
                            if name and short[name] and (filter.min or 0) > 0 and not drained[name] then
                                drained[name] = true
                                drained[#drained + 1] = name
                            end
                        end
                    end
                end
            end
            if drained[1] then
                return "Stop the landing pad taking " .. platform.name .. "'s thruster fuel: drop " .. table.concat(drained, ", ") .. " from its requests.",
                    "The pad takes requested items from any platform in orbit, and " .. platform.name .. " cannot leave until its hub holds " .. tostring(platform.thrust_reserve or 450)
                    .. " of each thruster input. space_platform action=request replaces the pad's requests: call it with the list you still want, without these. Its collectors and crushers then refill them in orbit. Get them here from rocks and coal instead." .. REMOTE_HOME
            end
        end
    end
    -- A placed silo builds nothing until its rocket parts' inputs reach it
    -- (p7: the carried ones stayed in the inventory for 35 minutes). Ask
    -- again whenever a full set of inputs is gone and parts are still short,
    -- so a partial load (a silo that took only 40 fuel) gets topped up.
    if silo_here then
        local silo = character.surface.find_entities_filtered{type = "rocket-silo", force = force, limit = 1}[1]
        local recipe = prototypes.recipe["rocket-part"]
        local input = silo.get_inventory(defines.inventory.crafter_input)
        local carried, loaded, required = math.huge, math.huge, silo.prototype.rocket_parts_required or 50
        for _, ingredient in pairs(recipe and recipe.ingredients or {}) do
            carried = math.min(carried, math.floor(have(ingredient.name) / ingredient.amount))
            loaded = math.min(loaded, math.floor((input and input.get_item_count(ingredient.name) or 0) / ingredient.amount))
        end
        local room = required - silo.rocket_parts - loaded
        if carried > 0 and room > 0 and loaded == 0 then
            return "Load the rocket silo at " .. math.floor(silo.position.x) .. "," .. math.floor(silo.position.y) .. " with the rocket-part inputs you carried.",
                "feed_machine_from_inventory unit_number=" .. silo.unit_number .. " crafts=" .. math.min(carried, room) .. " dry_run=false: " .. recipe_text("rocket-part")
                .. ". It builds the parts by itself; once it holds " .. required .. " the rocket can carry metallurgic packs to an orbiting platform." .. REMOTE_HOME
        end
    end
    -- A shuttle short of thruster-fuel inputs over Vulcanus cannot fly home;
    -- the silo here is the only way up (p7: the pad requests drained them).
    if silo_here then
        for _, platform in ipairs(S.platforms or {}) do
            if platform.space_location == "vulcanus" and platform.departure_held == "thrust_stock" then
                local items, seen = {}, {}
                for _, producer in ipairs(platform.thrust_short or {}) do
                    for _, item in ipairs(producer.short or {}) do
                        if not seen[item.name] then
                            seen[item.name] = true
                            items[#items + 1] = item.name .. " (hub has " .. item.hub .. ", keep " .. tostring(platform.thrust_reserve or 450) .. "+)"
                        end
                    end
                end
                if items[1] then
                    return "Send " .. platform.name .. " its thruster fuel from Vulcanus: " .. table.concat(items, ", ") .. ".",
                        "It cannot fly home without them. Gather them here (big volcanic rocks drop iron ore; coal makes carbon), then space_platform action=ship platform=" .. platform.name
                        .. " items=[...] from beside the silo once its rocket is ready. Drop thruster inputs from the landing pad's requests (action=request) so the pad does not take them again." .. REMOTE_HOME
                end
            end
        end
    end
    -- No armed platform here and no silo: nothing reaches orbit from here,
    -- so the next shuttle is built over Nauvis from here (copying the
    -- unarmed one still orbiting, if any).
    local stranded, armed_here = nil, false
    for _, platform in ipairs(S.platforms or {}) do
        if platform.space_location == "vulcanus" then
            if (platform.armament or {}).armed then armed_here = true else stranded = platform end
        end
    end
    if not armed_here and not silo_here and (here.counts["cargo-landing-pad"] or 0) > 0 then
        local goal, how = home_shuttle_rung(S, stranded, force)
        if goal then return goal, how end
    end
    for _, platform in ipairs(S.platforms or {}) do
        local stops = platform.stops or {}
        if platform.space_location == "vulcanus" and (platform.armament or {}).armed and #stops < 2 and (here.counts["cargo-landing-pad"] or 0) > 0 then
            return "Turn " .. platform.name .. " into a shuttle between Nauvis and Vulcanus.",
                "space_platform action=schedule platform=" .. platform.name .. " stops=[\"nauvis\",\"vulcanus\"]: at Nauvis it waits until its supply requests are filled (rockets bring them while you are here), at Vulcanus until the landing pad has taken what it requests. "
                .. "Then add what Vulcanus needs to its supply (space_platform action=supply items=[...]) and to the pad's requests here (action=request). Orbiting Vulcanus with no supply, it is worn down by asteroids." .. REMOTE_HOME
        end
    end
    if here.acid_pumpjacks == 0 then
        return "Pump sulfuric acid: put a pumpjack on a sulfuric-acid-geyser.",
            "find_nearest_resource sulfuric-acid-geyser; place a pumpjack (you have " .. have("pumpjack") .. "; " .. recipe_text("pumpjack")
            .. ") on it with build_layout, power it, and pipe the acid toward where the chemical plant and assembler will stand." .. REMOTE_HOME
    end
    if not tech_done(force, "foundry") then
        local carbon = here.made["carbon"] or 0
        return "Make one tungsten-carbide: crafting it unlocks foundry research.",
            "Acid: " .. here.acid_pumpjacks_working .. "/" .. here.acid_pumpjacks .. " geyser pumpjacks working. "
            .. "1) A chemical-plant set to carbon (" .. recipe_text("carbon") .. "): " .. recipe_state(here, "carbon") .. ", " .. carbon .. " carbon made here. "
            .. "2) An assembling-machine-2 set to tungsten-carbide (" .. recipe_text("tungsten-carbide") .. "): " .. recipe_state(here, "tungsten-carbide") .. ". "
            .. "Pipe acid into both. One craft needs no belts: load the solids with feed_machine_from_inventory (coal into the carbon plant; carbon and tungsten-ore into the assembler), take carbon out with collect_from_chest. "
            .. "You carry " .. have("coal") .. " coal, " .. have("carbon") .. " carbon, " .. have("tungsten-ore") .. " tungsten-ore (more from big volcanic rocks)." .. REMOTE_HOME
    end
    if not tech_done(force, "big-mining-drill") then
        return "Craft a foundry on Vulcanus: crafting it unlocks big-mining-drill research.",
            recipe_text("foundry") .. " in an assembling-machine-2 on Vulcanus (it needs Vulcanus pressure and piped lubricant). Inputs: "
            .. recipe_text("tungsten-carbide") .. " (" .. (here.made["tungsten-carbide"] or 0) .. " made here); "
            .. recipe_text("refined-concrete") .. "; " .. recipe_text("lubricant") .. "; heavy oil from " .. recipe_text("simple-coal-liquefaction")
            .. " (oil refinery); water from " .. recipe_text("steam-condensation") .. " fed by " .. recipe_text("acid-neutralisation") .. "." .. REMOTE_HOME
    end
    -- Molten iron (a fluid every later foundry recipe needs), from iron ore
    -- or from lava.
    local function molten_iron_text()
        local ore_route = force.recipes["molten-iron"] and force.recipes["molten-iron"].enabled
        return "Molten iron comes from " .. (ore_route and (recipe_text("molten-iron") .. " (no lava; load the ore with feed_machine_from_inventory) or ") or "")
            .. recipe_text("molten-iron-from-lava") .. " (find_nearest_resource resource_type=lava; an offshore-pump on its shore pumps lava), in a second foundry piped into the first, "
            .. "or in the same foundry first, held in pipes and storage tanks, before you switch its recipe. Foundries placed: " .. (here.counts["foundry"] or 0) .. "."
    end
    if not tech_done(force, "tungsten-steel") then
        return "Craft a big mining drill in a foundry: crafting it unlocks tungsten-steel research (the tungsten-plate recipe).",
            recipe_text("big-mining-drill") .. " (a foundry recipe: set_recipe, then load the solids with feed_machine_from_inventory and pipe in the molten iron). "
            .. molten_iron_text() .. REMOTE_HOME
    end
    if not tech_done(force, "metallurgic-science-pack") then
        return "Make a tungsten-plate in a foundry: it unlocks metallurgic-science-pack research.",
            recipe_text("tungsten-plate") .. " (a foundry recipe). " .. molten_iron_text() .. " " .. (here.made["tungsten-plate"] or 0) .. " tungsten-plate made." .. REMOTE_HOME
    end
    -- Packs reach Nauvis labs only by rocket, shuttle and the home pad.
    local packs = have("metallurgic-science-pack")
    if silo_here and packs >= 50 then
        local shuttle = nil
        for _, platform in ipairs(S.platforms or {}) do
            for _, stop in ipairs(platform.stops or {}) do
                if stop == "nauvis" and (platform.armament or {}).armed then shuttle = platform end
            end
        end
        if shuttle then
            return "Send your " .. packs .. " metallurgic science packs to Nauvis.",
                "1) Beside the silo: space_platform action=ship platform=" .. shuttle.name .. " items=[{name:\"metallurgic-science-pack\",count:" .. packs .. "}]; they go up with the next ready rocket while "
                .. shuttle.name .. " orbits Vulcanus (keep the silo fed with rocket parts). 2) space_platform action=request stops=[\"nauvis\"] items=[{name:\"metallurgic-science-pack\",count:" .. packs
                .. "}] so the Nauvis landing pad takes them when the shuttle is home." .. REMOTE_HOME
        end
    end
    return "Automate metallurgic science on Vulcanus.",
        recipe_text("metallurgic-science-pack") .. " in a foundry (molten copper from " .. recipe_text("molten-copper-from-lava")
        .. "); tungsten ore from big mining drills on tungsten-ore patches (" .. recipe_text("big-mining-drill") .. "). "
        .. (here.made["metallurgic-science-pack"] or 0) .. " packs made here." .. REMOTE_HOME
end

-- Rungs for a character away from home: on a platform or on Vulcanus.
local function travel_rung(S, character, here, force)
    if here then
        if here.surface == "vulcanus" then return vulcanus_rung(here, force, character, S) end
        return "You are on " .. here.surface .. ".", "Build power and a roboport from what you carried." .. REMOTE_HOME
    end
    if not S.character_on_platform then return nil end
    local here = nil
    for _, platform in ipairs(S.platforms) do
        if platform.name == S.character_on_platform then here = platform end
    end
    here = here or {}
    if here.space_location == "vulcanus" then
        return "Land on Vulcanus.", "space_platform action=land."
    end
    if here.space_location == nil then
        return "Flying to " .. tostring((here.stops or {})[1] or "the next stop") .. ".",
            "Watch space_platform action=status (ammo, fuel, damaged_tiles); keep Nauvis running with robot_logistics / place_ghosts surface=nauvis."
    end
    -- Aboard at home before the supply line exists: leaving would strand
    -- you, and the platform can be changed from the ground.
    local H = S.home_supply or {}
    local supply_ready = (H.silos_with_supply_chest or 0) > 0 and here.supply ~= nil
    if here.space_location == "nauvis" and (here.departure_held == "no_supply_line"
        or ((here.stops or {})[1] ~= "vulcanus" and not supply_ready))
    then
        return "Land on Nauvis: the supply line is not ready, so " .. here.name .. " must not leave yet.",
            "space_platform action=land, then follow progression (a silo supply chest and the platform's standing supply come first). You can change the platform from the ground: place_ghosts surface="
            .. tostring(here.surface) .. " (a machine with a recipe pastes it onto the built one) and ship what it needs."
    end
    if S.vulcanus_unlocked and (here.stops or {})[1] == "vulcanus" then
        local arms = here.armament or {}
        if not arms.armed then
            return "Arm " .. here.name .. " before it leaves (" .. tostring(arms.needs) .. ").",
                "It stays parked until armed, because asteroids destroy it and you on the way. " .. turret_plan(arms, here.surface)
                .. " You are aboard and cannot ship: land on Nauvis (space_platform action=land) to craft and ship the turrets, inserters, belts and magazines, then board again."
        end
        if here.departure_held == "thrust_stock" then
            local items = {}
            for _, producer in ipairs(here.thrust_short or {}) do
                for _, item in ipairs(producer.short) do items[#items + 1] = item.name .. " (hub has " .. item.hub .. ")" end
            end
            return "Stock " .. here.name .. "'s thruster fuel: it stays parked until the hub holds " .. tostring(here.thrust_reserve) .. "+ of " .. table.concat(items, ", ") .. ".",
                "Queued shipments keep going up without you. If none is queued, you are aboard and cannot ship: land on Nauvis (space_platform action=land), ship them, then board again (board books the next ready rocket for you)."
        end
        return "Get " .. here.name .. " moving to Vulcanus (" .. tostring(here.state) .. ").",
            "It leaves once its thrusters get both thruster-fuel and thruster-oxidizer: follow the thruster warnings, building pipe and foundation with place_ghosts on " .. tostring(here.surface) .. ", and ship what its ghosts lack."
    end
    if S.vulcanus_unlocked then
        return "Set course for Vulcanus.", "space_platform action=schedule stops=[\"vulcanus\"]."
    end
    return nil
end

-- Space science made on the platform and landed at home: the next missing
-- step, or nil when it is running.
local function space_science_rung(P, S)
    if ((P.recipes or {})["space-science-pack"] or 0) == 0 then
        return "Make space science on " .. P.name .. ".",
            recipe_text("space-science-pack") .. ", made only at zero gravity. On the platform: asteroid collectors at the foundation edge, crushers ("
            .. recipe_text("metallic-asteroid-crushing") .. "; " .. recipe_text("carbonic-asteroid-crushing") .. "; " .. recipe_text("oxide-asteroid-crushing")
            .. "), an electric furnace for iron plate, an assembler on space-science-pack, inserters from and into the hub, solar panels. Ship the parts (action=ship), then lay them out with place_ghosts surface="
            .. tostring(P.surface) .. " (add space-platform-foundation tiles to grow it); all of this works from the ground, without boarding. An assembler already there without a recipe takes one from place_ghosts with its name, position and recipe."
    end
    if S.landing_pads == 0 then
        return "Build a cargo landing pad at home near the labs.",
            "build_layout; then space_platform action=request items=[{name:\"space-science-pack\",count:100}]."
    end
    for _, name in ipairs(S.landing_pad_requests) do
        if name == "space-science-pack" then return nil end
    end
    return "Request space science on the landing pad.",
        "space_platform action=request items=[{name:\"space-science-pack\",count:100}]. Platforms in orbit drop requested items on their own."
end

-- Rocket parts Nauvis must make per 10 minutes before you leave it to run
-- alone: 30 is 3.6 rockets an hour (long40 made 22).
local MIN_ROCKET_PARTS_BEFORE_LEAVING = 30

-- Carried to Vulcanus in your inventory: its rocket silo, one launch of
-- rocket parts, and a landing pad for the shuttle's drops.
local VULCANUS_KIT = {
    {name = "rocket-silo", count = 1},
    {name = "processing-unit", count = 50},
    {name = "low-density-structure", count = 50},
    {name = "rocket-fuel", count = 50},
    {name = "cargo-landing-pad", count = 1},
}

-- Space Age rungs after the first rocket, up to boarding for Vulcanus. Nil
-- when the generic research rungs (on space_path) are the right next step.
local function space_rung(facts, force, surface, character)
    local S = facts.space
    local P = S.platforms[1]
    local vulcanus_found = S.techs.planet_discovery_vulcanus
    if not S.techs.construction_robotics then
        return "Research construction-robotics: robots build for you, even while you are away.",
            "start_research construction-robotics; meanwhile craft roboports, construction and logistic robots."
    end
    if (facts.roboports or 0) == 0 then
        return "Build a roboport network at home.",
            "Place a roboport in the power network with build_layout, place robots beside it with place_entity (a placed robot flies into the roboport), and a storage-chest in range. From now on build at home with place_ghosts; check with robot_logistics."
    end
    if S.home_logistics.construction_robots < 10 then
        return "Give the home network at least 10 construction robots (" .. S.home_logistics.construction_robots .. " now).",
            "Craft them and place them with place_entity."
    end
    if not P then
        return "Create a space platform.",
            recipe_text("space-platform-starter-pack") .. ". Craft it (build lines for the inputs), then space_platform action=create and action=ship the starter pack beside a silo; it leaves with the next ready rocket. Creating it completes the space-platform research."
    end
    if P.state == "waiting_for_starter_pack" then
        return "Ship the starter pack to " .. P.name .. ".",
            recipe_text("space-platform-starter-pack") .. ". space_platform action=ship items=[{name:\"space-platform-starter-pack\",count:1}] beside a silo; it leaves with the next ready rocket."
    end
    if P.state == "starter_pack_on_the_way" then
        return "The starter pack is on its way to " .. P.name .. " (about 30 s).",
            "Meanwhile craft platform parts: asteroid collectors, crushers, solar panels, an electric furnace, an assembler, inserters, belts."
    end
    if not vulcanus_found then
        local goal, how = space_science_rung(P, S)
        if goal then return goal, how end
        return nil
    end
    if (P.thrusters or 0) == 0 then
        return "Make " .. P.name .. " fly: thrusters.",
            recipe_text("thruster") .. "; " .. recipe_text("thruster-fuel") .. "; " .. recipe_text("thruster-oxidizer") .. "; " .. recipe_text("ice-melting")
            .. ". Build thrusters at the back edge, fed by pipes from chemical plants on the platform; place with place_ghosts surface=" .. tostring(P.surface) .. "."
    end
    local arms = P.armament or {}
    -- Cargo the platform still waits for, when it outweighs what the silo
    -- launches soon: a bigger rocket-part line is then the next step.
    if (P.queued_rockets or 0) >= 2 and not (arms.missing_research or {})[1]
        and (not arms.armed or (P.thrust_short and P.thrust_short[1]))
    then
        local parts_required = prototypes.entity["rocket-silo"] and prototypes.entity["rocket-silo"].rocket_parts_required or 50
        local per_hour = made_last_ten_minutes(force, surface, "rocket-part") * 6 / parts_required
        local hours = per_hour > 0 and string.format("about %.1f h at %.1f rockets/h", P.queued_rockets / per_hour, per_hour) or "no rocket parts made in the last 10 min"
        return "Launch rockets faster: " .. P.queued_rockets .. " rockets of cargo wait for " .. P.name .. " (" .. hours .. ").",
            "Each rocket part needs " .. rocket_part_inputs(force, surface)
            .. ". Add assemblers for the scarcest of these and for its own inputs (build_layout, route_belt), and feed all three into the silo with inserters. One rocket lifts 1 t: 50 piercing-rounds-magazine or 500 iron-ore. Still needed on "
            .. P.name .. ": " .. tostring(arms.needs) .. "."
    end
    if not arms.armed then
        return "Arm " .. P.name .. " before leaving (" .. tostring(arms.needs) .. ").",
            "Asteroids destroy an unarmed platform on the way, and you with it; it will not leave orbit until armed. " .. turret_plan(arms, P.surface)
            .. " Ship the turrets, inserters and magazines. Keep spare space-platform-foundation and parts in the hub: it rebuilds what asteroids break."
    end
    -- Ingredients already queued for the platform count: once they cover the
    -- reserve, boarding is next (the hold keeps the platform parked until
    -- they land).
    local queued = {}
    for _, entry in ipairs(P.queued_cargo or {}) do queued[entry.name] = (queued[entry.name] or 0) + entry.count end
    local parts, items = {}, {}
    for _, producer in ipairs(P.thrust_short or {}) do
        for _, item in ipairs(producer.short) do
            if item.hub + (queued[item.name] or 0) < (P.thrust_reserve or 0) then
                parts[#parts + 1] = "the " .. producer.recipe .. " plant at " .. producer.position.x .. "," .. producer.position.y
                    .. " is low on " .. item.name .. " (hub has " .. item.hub .. ", " .. (queued[item.name] or 0) .. " queued; keep " .. tostring(P.thrust_reserve) .. "+ for the trip"
                    .. (item.made_by[1] and ("; made by " .. table.concat(item.made_by, ", ")) or "") .. ")"
                items[#items + 1] = item.name
            end
        end
    end
    if parts[1] then
        return "Stock " .. P.name .. "'s thruster fuel before boarding: " .. table.concat(parts, "; ") .. ".",
            "Without it the thrusters burn out within seconds and the platform stalls. Ship " .. tostring(P.thrust_reserve) .. "+ " .. table.concat(items, ", ")
            .. " now (space_platform action=ship), and make it on the platform: asteroid-collectors at the foundation edge and a crusher on that recipe, with inserters to and from the hub (place_ghosts surface="
            .. tostring(P.surface) .. ")."
    end
    -- The supply line keeps running after you leave: robots load a silo
    -- with what the platform's hub requests from Nauvis, so it can shuttle
    -- to Vulcanus and back without you.
    local H = S.home_supply or {}
    if (H.silos_in_logistic_range or 0) == 0 then
        return "Bring the rocket silo into the roboport network's logistic range.",
            "Logistic robots serve only a roboport's logistic area (" .. recipe_text("roboport")
            .. "; its logistic area is much smaller than its build area): place a powered roboport beside the silo (build_layout, or place_ghosts surface=nauvis inside current coverage)."
    end
    if (H.logistic_robots or 0) < 20 then
        return "Give the home network at least 20 logistic robots (" .. (H.logistic_robots or 0) .. " now).",
            recipe_text("logistic-robot") .. ". Craft them and place them beside a roboport with place_entity."
    end
    if (H.storage_chests or 0) == 0 then
        return "Add a storage-chest to the home network.", "build_layout a storage-chest inside the roboport network: robots put what they carry back there."
    end
    -- Requester chests come with logistic-system, which needs space science.
    if not tech_done(force, "logistic-system") then
        local goal, how = space_science_rung(P, S)
        if goal then return goal, how end
        local tech = force.technologies["logistic-system"]
        local packs = {}
        for _, ingredient in pairs(tech and tech.research_unit_ingredients or {}) do packs[#packs + 1] = ingredient.name end
        -- A pack nothing makes comes first, then one whose machines stalled
        -- while labs lack it (idle because the labs are full is fine).
        for _, name in ipairs(packs) do
            if recipe_machines(facts, name) == 0 then
                for _, machine in ipairs(facts.unset_machines or {}) do
                    if machine.cleared_recipe == name then
                        return "Set " .. name .. " back on " .. machine.name .. " " .. tostring(machine.unit_number) .. ".",
                            "You cleared its recipe " .. machine.cleared_minutes_ago .. " min ago and nothing else makes " .. name
                            .. " for logistic-system: set_recipe unit_number=" .. tostring(machine.unit_number) .. " recipe=" .. name .. ", then verify_production."
                    end
                end
                return "Automate " .. name .. " for logistic-system: nothing makes it.",
                    recipe_text(name) .. ". Design a compact block with build_layout (assemblers for the pack and its intermediates, inserters, belts from the plate lines, poles) and feed the labs; then verify_production." .. supply_chain_text(force, surface, facts, name)
            end
        end
        for _, name in ipairs(packs) do
            if made_with_platforms(force, surface, facts, name) == 0 and (facts.lab_missing_packs or {})[name] then
                return "Get " .. name .. " made again: labs lack it and its machines made none in 10 minutes.",
                    recipe_text(name) .. ". diagnose_factory_blockers on its assemblers (inputs, power, full output) and fix the feed." .. supply_chain_text(force, surface, facts, name)
            end
        end
        return "Research logistic-system: the silo's supply chest is a requester chest.",
            "start_research logistic-system (" .. tostring(tech and tech.research_unit_count) .. " units of " .. table.concat(packs, ", ")
            .. "). Space science lands on the pad from " .. P.name .. "'s assembler; keep its crushers and the labs fed."
    end
    if (H.silos_with_supply_chest or 0) == 0 then
        return "Give the silo a supply chest.",
            "Place a requester-chest beside the rocket silo with an inserter from the chest into the silo (build_layout, inside logistic range). The mod sets the chest's requests to what platforms' supply orders still miss; robots fill it, the inserter loads the rocket and the rocket goes up."
    end
    local supplied = {}
    for _, request in ipairs(P.supply and P.supply.requests or {}) do supplied[request.name] = true end
    if not ((supplied["piercing-rounds-magazine"] or supplied["copper-plate"]) and (supplied["iron-ore"] or crushes_ore(P))) then
        local items, keeps = supply_order_text(P)
        return "Set " .. P.name .. "'s standing supply from Nauvis.",
            "space_platform action=supply platform=" .. P.name .. " " .. items .. ": whenever it orbits Nauvis, rockets keep " .. keeps .. " topped up, also while you are away. A leg to Vulcanus spends 300-400 magazines and the platform only leaves with 600+ piercing rounds aboard, so 1100 brings it home again. Add what Vulcanus will need later the same way."
    end
    if H.network_lacks and H.network_lacks[1] then
        return "Stock the home network with " .. table.concat(H.network_lacks, ", ") .. " for " .. P.name .. "'s supply.",
            "Robots load only what the network holds: from each line making these, add an inserter into a passive-provider-chest inside the roboport network (build_layout), or put a stock of them into a storage chest."
    end
    -- A shuttle that makes its own rounds needs only plates from Nauvis.
    if (P.ammo_lines or 0) == 0 and P.ammo_line_fits then
        return "Give " .. P.name .. " its own ammo line: space_platform action=ammo_line platform=" .. P.name .. ".",
            "One rocket lifts 1 t: 50 piercing-rounds-magazine (20 kg each), or the iron, steel and copper plates for about 160 made aboard, in flight too. ammo_line places ghosts behind the hub (two assembling-machine-1, inserters, a medium pole, two solar panels, foundation) and switches the standing supply from rounds to iron-plate 800, steel-plate 100 and copper-plate 200."
    end
    -- Once you leave, Nauvis runs alone and every shuttle load rides its
    -- rockets: s49-s57 stalled at one rocket per 45-90 minutes when copper
    -- ran short and nobody was there to fix it. The gate counts what Nauvis
    -- makes for rocket parts, not what its silo uses: in p4 and p5, taking
    -- processing units for the kit dropped the parts count and sent Buddy
    -- back here to undo it.
    local parts_capacity, part_recipe = math.huge, prototypes.recipe["rocket-part"]
    for _, ingredient in pairs(part_recipe and part_recipe.ingredients or {}) do
        parts_capacity = math.min(parts_capacity, math.floor(made_last_ten_minutes(force, surface, ingredient.name) / ingredient.amount))
    end
    if parts_capacity == math.huge then parts_capacity = 0 end
    if parts_capacity < MIN_ROCKET_PARTS_BEFORE_LEAVING then
        return "Raise Nauvis' rocket rate before you leave: its last 10 minutes made inputs for " .. parts_capacity .. " rocket parts, " .. MIN_ROCKET_PARTS_BEFORE_LEAVING .. "+ needed.",
            "After you leave only robots can change Nauvis, and they reach only roboport range. Each part needs " .. rocket_part_inputs(force, surface)
            .. ". Fix the scarcest in person now: connect the ore patches and smelters that stand full to the lines that starve (route_belt, build_layout), add assemblers for it and its inputs, and extend roboport coverage over the outposts." .. supply_gaps_text(facts)
    end
    if (P.stops or {})[1] ~= "vulcanus" and (P.stops or {})[2] ~= "vulcanus" then
        return "Set " .. P.name .. "'s course: a shuttle between Nauvis and Vulcanus.",
            "space_platform action=schedule stops=[\"vulcanus\",\"nauvis\"]. It stays in orbit until you are aboard (and armed and stocked), then shuttles by itself: at Nauvis it waits for its supply, at Vulcanus for the landing pad."
    end
    -- Nothing leaves Vulcanus without a rocket silo there, and a silo's
    -- 10 t would take ten rockets: carry it, and one launch of parts.
    local inventory = character and character.get_main_inventory()
    local function have(name) return inventory and inventory.get_item_count(name) or 0 end
    -- Without a silo in hand its ingredients are part of the kit: p4 packed
    -- 19/50 processing units for an hour, blind to the silo's 200.
    local needed, order = {}, {}
    local function need(name, count)
        if not needed[name] then order[#order + 1] = name end
        needed[name] = (needed[name] or 0) + count
    end
    for _, entry in ipairs(VULCANUS_KIT) do need(entry.name, entry.count) end
    local silo_recipe = prototypes.recipe["rocket-silo"]
    if have("rocket-silo") < 1 and silo_recipe then
        for _, ingredient in pairs(silo_recipe.ingredients) do
            if ingredient.type ~= "fluid" then need(ingredient.name, ingredient.amount) end
        end
    end
    local missing, rocket_inputs, starved = {}, {}, nil
    for _, ingredient in pairs(prototypes.recipe["rocket-part"] and prototypes.recipe["rocket-part"].ingredients or {}) do rocket_inputs[ingredient.name] = true end
    for _, name in ipairs(order) do
        local count = have(name)
        if count < needed[name] then
            missing[#missing + 1] = name .. " " .. count .. "/" .. needed[name]
            local short, made = needed[name] - count, made_last_ten_minutes(force, surface, name)
            if rocket_inputs[name] and short > made and not (starved and starved.short >= short) then
                starved = {name = name, short = short, made = made}
            end
        end
    end
    if missing[1] then
        local extra = ""
        if starved then
            extra = " Nauvis makes " .. starved.made .. " " .. starved.name .. " in 10 min, all of it for the home silo. Take them from that line while you pack:"
                .. " point its inserter into a chest, or set_recipe on one of its assemblers to fill a chest; the rate check counts what is made, not what the silo uses."
                .. " At that rate " .. starved.short .. " more take about " .. math.ceil(starved.short / math.max(starved.made, 1) * 10) .. " minutes, so add assemblers for it in the meantime: " .. recipe_text(starved.name) .. "."
                .. " Turn the line back to the silo before you board."
        end
        return "Pack the Vulcanus launch kit in your inventory: " .. table.concat(missing, ", ") .. ".",
            "You ride the rocket with your whole inventory, but cargo sent up costs 1 t per rocket and a rocket-silo weighs 10 t. With a silo and one launch of parts on Vulcanus, its metallurgic packs go up to the shuttle. "
            .. recipe_text("rocket-silo") .. " (craft it by hand once you carry the ingredients; collect them from chests and machines). Rocket parts: " .. rocket_part_inputs(force, surface) .. "." .. extra
    end
    -- Whatever you took for the kit must flow to the home silo again.
    local stalled = H.stalled_silo
    if stalled then
        return "Feed the Nauvis silo again before you board: " .. stalled.rocket_parts .. "/" .. stalled.parts_required .. " parts, it lacks " .. table.concat(stalled.lacks, ", ") .. ".",
            "Turn back any inserter or recipe you changed to pack the kit, so the silo at " .. stalled.x .. "," .. stalled.y .. " gets " .. table.concat(stalled.lacks, ", ")
            .. " again; after you leave nothing can fix it. Each part needs " .. rocket_part_inputs(force, surface) .. "."
    end
    return "Board " .. P.name .. " for Vulcanus.",
        "It waits in orbit until you are aboard, then leaves. Carry what you need to start there besides the launch kit: a cargo-landing-pad (the shuttle drops what you request there); Vulcanus has no water, so solar panels (6x power there) and accumulators, not steam; roboports, construction and logistic robots and a storage chest so place_ghosts works there; chemical plants and assembling-machine-2s (carbon, tungsten carbide), a pumpjack (sulfuric acid geysers), pipes, drills, steel furnaces, belts, inserters, poles. Stand within reach of a silo and call space_platform action=board once: with no rocket ready it books the next one and launches you when it is ready. Do not unship cargo to board."
        .. " Book first and craft while you wait: in p7 the next rocket left without you during a 5-minute crafting wait."
        .. " Over Vulcanus, keep carbon and iron ore off the landing pad's requests: the pad takes them from the shuttle, which then cannot fly home."
end

-- Code-computed tech-progression ladder for the early game. Each rung names
-- one observed gap and the controller that closes it; the model still
-- chooses, but no longer has to rediscover the Factorio 2.0 trigger tree.
local function progression(surface, force, facts, character)
    local iron = produced_count(force, surface, "iron-plate")
    local copper = produced_count(force, surface, "copper-plate")
    local current = force.current_research
    local state = {
        iron_plates_made = iron,
        copper_plates_made = copper,
        steam_power_unlocked = tech_done(force, "steam-power"),
        electronics_unlocked = tech_done(force, "electronics"),
        red_science_unlocked = tech_done(force, "automation-science-pack"),
        automation_researched = tech_done(force, "automation"),
        current_research = current and current.name or nil,
        steam_engines = facts.steam_engines,
        steam_engines_working = facts.steam_engines_working,
        boilers = facts.boilers,
        boiler_fuel_min = facts.boiler_fuel_min,
        labs = facts.labs,
        labs_powered = facts.labs_powered,
        labs_working = facts.labs_working,
        red_science_assemblers = facts.red_science_assemblers,
        burner_machines = facts.burner_machines,
        low_fuel_burner_machines = facts.low_fuel_count,
        rocket_silos = facts.rocket_silos or 0,
        rocket_parts = facts.rocket_parts,
        rockets_launched = force.rockets_launched,
    }
    state.rocket_path = tech_path(force, surface, facts, "rocket-silo")
    if tech_done(force, "rocket-silo") then
        state.space_path = tech_path(force, surface, facts, "planet-discovery-vulcanus")
    end
    local path = state.space_path or state.rocket_path
    local path_field = state.space_path and "progression.space_path" or "progression.rocket_path"
    local path_target = path and path.target or "rocket-silo"
    local unautomated, stalled = nil, nil
    for _, pack in ipairs(path and path.science_packs_needed or {}) do
        if pack.assemblers == 0 then unautomated = unautomated or pack
        elseif pack.made_last_10_min == 0 then stalled = stalled or pack end
    end
    local goal, how = travel_rung(facts.space, character, facts.here, force)
    local space_goal, space_how = nil, nil
    if not goal and force.rockets_launched > 0 then space_goal, space_how = space_rung(facts, force, surface, character) end
    local silo_prototype = prototypes.entity["rocket-silo"]
    local parts_required = silo_prototype and silo_prototype.rocket_parts_required or 50
    if goal then
        -- Away from home: the travel rung decides.
    elseif tech_done(force, "rocket-silo") and (facts.rocket_silos or 0) == 0 then
        local recipe = prototypes.recipe["rocket-silo"]
        -- Stock the agent can craft from: its own inventory plus the force's chests.
        local stock = {}
        local function add_stock(inventory)
            if not inventory then return end
            for _, item in pairs(inventory.get_contents()) do
                stock[item.name] = (stock[item.name] or 0) + item.count
            end
        end
        if character and character.valid then add_stock(character.get_main_inventory()) end
        for _, chest in pairs(surface.find_entities_filtered{type = "container", force = force}) do
            add_stock(chest.get_inventory(defines.inventory.chest))
        end
        local needs = {}
        for _, ingredient in pairs(recipe and recipe.ingredients or {}) do
            needs[#needs + 1] = ingredient.amount .. " " .. ingredient.name .. " (have "
                .. (stock[ingredient.name] or 0) .. ", " .. made_last_ten_minutes(force, surface, ingredient.name) .. " made in 10 min)"
        end
        goal = "Build a rocket silo (rocket-silo is researched)."
        how = "Craft rocket-silo from " .. table.concat(needs, ", ")
            .. ". For each ingredient short of the amount, build or extend its assembler line (and its inputs) and collect the output into chests; "
            .. "then craft the silo and place it (9x9) with build_layout inside the power network, leaving room for inserters on its sides."
    elseif force.rockets_launched == 0 and tech_done(force, "rocket-silo") and facts.rocket_ready then
        goal = "Launch the rocket: the silo's rocket is ready."
        how = "Call launch_rocket. Space Age silos never launch on their own."
    elseif force.rockets_launched == 0 and tech_done(force, "rocket-silo") then
        goal = "Fill the rocket silo: " .. (facts.rocket_parts or 0) .. "/" .. parts_required .. " rocket parts."
        how = "Each rocket part needs " .. rocket_part_inputs(force, surface)
            .. ". Automate the scarcest with build_layout (assemblers, chemical plants, their inputs and power) and feed all three into the silo with inserters; the silo builds the parts itself. Then launch_rocket."
    elseif space_goal then
        goal, how = space_goal, space_how
    elseif not state.steam_power_unlocked then
        goal = "Smelt 50 iron plates to unlock steam-power (" .. iron .. "/50 made)."
        how = "mine_at stone and coal, place stone furnaces fed by burner drills on iron ore (execute_direct_smelter / execute_edge_miner), and hand-fuel them with bootstrap_burner_once (up to 50 coal each). Hand-smelting with bootstrap_smelting_once also counts. Do not build belt fuel feeds yet."
    elseif not state.electronics_unlocked then
        goal = "Smelt 10 copper plates to unlock electronics (" .. copper .. "/10 made)."
        how = "Put a burner drill + stone furnace on copper ore, or hand-smelt copper ore with bootstrap_smelting_once."
    elseif facts.steam_engines == 0 then
        goal = "Build steam power."
        how = "build_steam_power with target_x/target_y where the lab and assemblers will go (near the plate smelters). It finds water and crafts the parts (phase crafted), then a second call with the same arguments places and fuels everything. Have about 50 iron plates, 15 copper plates, 5 stone and 5 wood (mine one tree) in inventory first."
    elseif facts.labs == 0 then
        goal = "Craft and place a lab inside the power network (crafting the first lab unlocks automation-science-pack)."
        how = "craft lab (10 circuits, 10 gears, 4 belts: about 36 iron and 15 copper plates), then place_entity it next to the pole at build_steam_power's power_target."
    elseif facts.labs_powered == 0 then
        goal = "Connect the lab to electricity."
        how = "Place small-electric-poles from the nearest pole to the lab (each covers a 5x5 area, wire reach 7.5)."
    elseif not state.automation_researched then
        goal = state.current_research and "Automation research is running; grow plate production while it finishes."
            or "Start researching automation."
        how = "Hand-craft automation-science-pack, feed_lab_from_inventory, start_research automation. Meanwhile add burner drills/furnaces on iron and copper and keep every burner machine and the boiler fuelled (up to 50 coal)."
    elseif unautomated then
        goal = "Automate " .. unautomated.name .. " (needed by " .. unautomated.first_needed_by .. ") and deliver it to the labs."
        how = "Design a compact block and place it with build_layout: assemblers for the pack and its intermediates, inserters between them, belts from the plate lines, poles, and an inserter or belt into a lab. Then connect plates, verify_production, and keep research queued."
    elseif stalled then
        local deepest = nil
        for _, input in ipairs(stalled.missing_inputs or {}) do
            if not deepest or input.depth > deepest.depth then deepest = input end
        end
        goal = "Get " .. stalled.name .. " made: " .. stalled.assemblers .. " assembler(s) have it set but none was made in 10 minutes."
        how = deepest
            and ("The missing link is " .. deepest.name .. (deepest.fluid and " (fluid)" or "") .. ", needed by " .. deepest.needed_by
                .. " (see science_packs_needed missing_inputs). Build or feed the machines that make it, connect them with belts, inserters or pipes, then verify_production up the chain.")
            or ("Every ingredient was made recently, so the shortest one or its delivery is the gap: compare science_packs_needed ingredients_made_last_10_min against the recipe, "
                .. "then raise the smallest supply or connect the idle producers to the " .. stalled.name .. " assembler with inserters or belts, and verify_production.")
    elseif state.current_research == nil and path and path.researchable_now[1] then
        goal = "Start the next research toward " .. path_target .. ": " .. path.researchable_now[1].name .. "."
        how = "start_research it (trigger technologies need their trigger instead). Keep labs supplied."
    else
        -- The pack most idle labs lack is the real limit; flow counts alone
        -- favour a pack whose assemblers are merely blocked by full labs.
        local limit, limit_labs = path and path.slowest_pack, 0
        for pack, count in pairs(facts.lab_missing_packs or {}) do
            if count > limit_labs or (count == limit_labs and pack < limit) then limit, limit_labs = pack, count end
        end
        goal = "Scale science throughput toward " .. path_target .. " (" .. (path and path.techs_remaining or 0) .. " technologies left)."
        how = "Research speed is set by the scarcest pack"
            .. (limit and (": " .. limit .. (limit_labs > 0 and (" (" .. limit_labs .. " idle labs lack it)") or " (see science_packs_needed made_last_10_min)")) or "")
            .. ". Add assemblers for it and its intermediates, the plate and power supply they need, with build_layout, and deliver it to every lab; replace hand-fed fuel with belts or electric machines."
    end
    local warnings = {}
    -- A character lost and replaced within the last 30 minutes.
    for _, respawn in pairs(storage.character_respawns or {}) do
        if character and respawn.unit_number == character.unit_number and game.tick - respawn.tick < 30 * 3600 then
            local platform_lost = not game.get_surface(respawn.lost_on)
            warnings[#warnings + 1] = "Your previous character was lost on " .. tostring(respawn.lost_on)
                .. (platform_lost and " (that surface no longer exists: its space platform was destroyed)" or "")
                .. "; you respawned at the Nauvis spawn " .. math.floor((game.tick - respawn.tick) / 3600)
                .. " min ago with an empty inventory. Everything you carried is gone."
                .. (platform_lost and " Before flying again, check the replacement platform's defence: turrets loaded with piercing-rounds-magazine (load_turrets), spare turrets and magazines in the hub." or "")
        end
    end
    -- Away from home, hand refuelling at home is out of reach.
    local away = facts.space.character_surface ~= surface.name
    if not away and facts.boilers > 0 and (facts.boiler_fuel_min or 0) < 10 then
        warnings[#warnings + 1] = "A boiler has under 10 fuel: top it up with refuel_burners, then belt coal to it with an inserter so power never stops."
    end
    if not away and facts.low_fuel_count > 0 then
        local units = {}
        for _, unit in ipairs(facts.low_fuel_units) do
            units[#units + 1] = unit.name .. " " .. tostring(unit.unit_number) .. " (" .. unit.fuel .. ")"
        end
        warnings[#warnings + 1] = facts.low_fuel_count .. " burner drills/furnaces have under "
            .. LOW_BURNER_FUEL .. " fuel and will stop: " .. table.concat(units, ", ")
            .. ". Top them all up with one refuel_burners call, then feed coal automatically."
    end
    local queue_ok, queue = pcall(function() return force.research_queue end)
    local queued = queue_ok and queue and #queue or (state.current_research and 1 or 0)
    state.research_queue_length = queued
    if path and path.all_next_stalled and path.slowest_pack then
        warnings[#warnings + 1] = "Every researchable technology toward " .. path_target .. " needs " .. path.slowest_pack
            .. ", which was not made in the last 10 minutes. Research off " .. path_field .. " only spends packs; get "
            .. path.slowest_pack .. " made first."
    elseif facts.labs > 0 and state.steam_power_unlocked and queued < 3 and path and path.researchable_now[1] then
        warnings[#warnings + 1] = "Only " .. queued .. " technologies are queued; labs go idle when the queue empties while you are away. "
            .. "Queue at least 3 with start_research, taking them from the active path's researchable_now (" .. path_field
            .. ".researchable_now; space_path once rocket-silo is researched; a technology whose prerequisites are queued ahead of it may be queued too)."
    end
    state.output_blocked_machines = facts.output_blocked
    state.starved_assemblers = facts.starved_assemblers
    local labs_starved = facts.labs_powered > facts.labs_working
    state.lab_missing_packs = facts.lab_missing_packs
    local lacked, delivery_gap = {}, {}
    for pack, count in pairs(facts.lab_missing_packs or {}) do
        lacked[#lacked + 1] = pack .. " (" .. count .. " labs)"
        if (facts.full_science_packs or {})[pack] then delivery_gap[#delivery_gap + 1] = pack end
    end
    table.sort(lacked)
    table.sort(delivery_gap)
    -- Once rocket-silo is researched, research speed no longer gates the goal.
    if lacked[1] and not tech_done(force, "rocket-silo") then
        local text = "Idle labs lack " .. table.concat(lacked, ", ") .. ". "
        if delivery_gap[1] then
            text = text .. "Assemblers of " .. table.concat(delivery_gap, ", ")
                .. " are full, so those packs are made but not delivered: chain them to every lab with inserters or a belt (build_layout, route_belt, build_lab_feed). "
        end
        if #delivery_gap < #lacked then
            text = text .. "For a lacked pack whose assemblers are not full, supply is the limit: add assemblers and their inputs for it before adding labs or other packs."
        end
        warnings[#warnings + 1] = text
    elseif (facts.full_science_assemblers or 0) > 0 and labs_starved and not tech_done(force, "rocket-silo") then
        warnings[#warnings + 1] = facts.full_science_assemblers .. " science assemblers are full while labs lack packs: "
            .. "the packs are not reaching the labs. Connect each science assembler to the labs with an inserter chain or belt (build_layout, route_belt, build_lab_feed)."
    end
    if facts.output_blocked >= 2 and facts.starved_assemblers >= 1 then
        warnings[#warnings + 1] = facts.output_blocked .. " machines are blocked with full output while "
            .. facts.starved_assemblers .. " assemblers lack ingredients: production and consumers are not connected." .. supply_gaps_text(facts)
            .. " Carry items from the full machines to the starved ones with output inserters and belts (route_belt, build_layout) instead of moving them by hand; a line outside roboport range needs the roboport network extended to it before robots can build there."
    end
    if facts.unset_machines and facts.unset_machines[1] then
        local idle = {}
        for _, machine in ipairs(facts.unset_machines) do
            idle[#idle + 1] = machine.name .. " " .. tostring(machine.unit_number) .. " at "
                .. machine.position.x .. "," .. machine.position.y
                .. (machine.cleared_recipe and (" (you cleared " .. machine.cleared_recipe .. " " .. machine.cleared_minutes_ago .. " min ago)")
                    or (" (made " .. machine.products_finished .. " items before)"))
        end
        warnings[#warnings + 1] = "Machines that produced before now have no recipe and stand idle: " .. table.concat(idle, "; ")
            .. ". set_recipe each back to what its inserters and belts feed, or remove it if it is no longer needed."
    end
    if facts.labs_powered > 0 and facts.labs_working == 0 and state.current_research ~= nil then
        warnings[#warnings + 1] = "Research (" .. state.current_research .. ") is queued but no lab at home is working"
            .. (lacked[1] and (": they lack " .. table.concat(lacked, ", ") .. ". ") or ": check lab science packs and power. ")
            .. (away and "From here, start_research a technology whose packs Nauvis still makes, or rebuild the missing pack's supply with place_ghosts surface=nauvis."
                or "Queue technologies whose packs are made, or restore the missing pack's supply.")
    end
    local S = facts.space
    if S.home_logistics.uncovered_ghosts > 0 then
        warnings[#warnings + 1] = S.home_logistics.uncovered_ghosts
            .. " ghosts at home are outside roboport range and will never be built: extend the roboport network or remove them."
    end
    if S.home_logistics.missing_items[1] then
        local lacking = {}
        for _, item in ipairs(S.home_logistics.missing_items) do
            lacking[#lacking + 1] = item.name .. " x" .. (item.needed - item.available)
        end
        warnings[#warnings + 1] = "Robots at home lack " .. table.concat(lacking, ", ")
            .. (away and " for placed ghosts, in the network covering them: from here, have Nauvis make them (place_ghosts surface=nauvis an assembler on the item, fed by inserters, with an inserter into a passive-provider-chest in that network)."
                or " for placed ghosts, in the network covering them: stock them in a storage chest in that network.")
    end
    local buildable = (S.home_logistics.entity_ghosts or 0) + (S.home_logistics.tile_ghosts or 0) - S.home_logistics.uncovered_ghosts
    if buildable > 0 and S.home_logistics.construction_robots > 0 and S.home_logistics.construction_robots_available == 0 then
        warnings[#warnings + 1] = "All " .. S.home_logistics.construction_robots .. " construction robots at home are busy with "
            .. buildable .. " ghosts: add robots (place_entity at home, or a construction-robot assembler with an inserter into a passive-provider-chest in the network)."
    end
    local coverage = facts.robot_coverage
    if coverage and S.home_logistics.roboports > 0 and coverage.covered * 2 < coverage.machines then
        local toward = coverage.uncovered_example
        warnings[#warnings + 1] = "Only " .. coverage.covered .. "/" .. coverage.machines
            .. " home machines are in roboport construction range, so robots and place_ghosts reach little of Nauvis. Extend it"
            .. (toward and (" toward " .. math.floor(toward.x) .. "," .. math.floor(toward.y)) or "")
            .. ": place_ghosts surface=nauvis a roboport (with poles to power it) inside the current range near its edge; robots build it from network stock (stock roboports there), and each roboport adds a 110x110 build area."
    end
    for _, platform in ipairs(S.platforms) do
        if (platform.damaged_tiles or 0) > 0 then
            warnings[#warnings + 1] = platform.name .. " has " .. platform.damaged_tiles .. " damaged tiles: add turrets and repair packs."
        end
        if (platform.machines_unpowered or 0) > 0 then
            local spare = 0
            for _, item in ipairs(platform.hub_items or {}) do
                if item.name == "solar-panel" then spare = item.count end
            end
            warnings[#warnings + 1] = platform.name .. " has " .. platform.machines_unpowered .. " machines short of power on "
                .. (platform.solar_panels or 0) .. " solar panels: place more solar panels (and poles) on it with place_ghosts surface="
                .. tostring(platform.surface) .. (spare > 0 and (" (its hub holds " .. spare .. ")") or ", shipping them first") .. ", from the ground."
        end
        if platform.foundation_holes and platform.foundation_holes[1] then
            local cells = {}
            for _, cell in ipairs(platform.foundation_holes) do cells[#cells + 1] = cell.x .. "," .. cell.y end
            warnings[#warnings + 1] = platform.name .. "'s foundation ghosts would close off empty space at " .. table.concat(cells, " ")
                .. ", so the hub never lays them (and nothing on them gets built): add space-platform-foundation tiles on those cells with place_ghosts."
        end
        for _, input in ipairs(platform.thrusters_unfed or {}) do
            local spots = {}
            for _, at in ipairs(input.pipe_to or {}) do spots[#spots + 1] = at.x .. "," .. at.y end
            local starved = {}
            for _, producer in ipairs(input.producers or {}) do
                for _, item in ipairs(producer.short or {}) do
                    starved[#starved + 1] = item.name .. " (hub has " .. item.hub .. (item.made_by[1] and ("; made by " .. table.concat(item.made_by, ", ")) or "") .. ")"
                end
            end
            local reason
            if not input.pipe_connected then
                reason = ": nothing is connected; extend that fluid's pipe to one of " .. table.concat(spots, " or ")
                    .. " with place_ghosts (surface coordinates; add space-platform-foundation tiles in the same call wherever the pipe crosses empty space)."
            elseif starved[1] then
                reason = ": its " .. tostring(input.fluid) .. " plant lacks " .. table.concat(starved, ", ")
                    .. ". Ship it (space_platform action=ship) and keep it coming: more asteroid-collectors at the foundation edge and a crusher on that recipe, fed from the hub."
            elseif input.producers and not input.producers[1] then
                reason = ": a pipe touches this input but no chemical plant on the platform makes " .. tostring(input.fluid) .. "; add one with place_ghosts and pipe it here."
            else
                reason = ": a pipe touches this input but carries none of it; check that the pipe line from the " .. tostring(input.fluid) .. " chemical plant has no gaps and does not mix fluids."
            end
            warnings[#warnings + 1] = platform.name .. "'s thruster at " .. input.thruster.x .. "," .. input.thruster.y
                .. " gets no " .. tostring(input.fluid or "fluid") .. ", so the platform cannot leave" .. reason
        end
        if platform.hub_free_slots == 0 then
            warnings[#warnings + 1] = platform.name .. "'s hub is full (" .. tostring(platform.hub_slots)
                .. " slots): its crushers stop and rocket cargo cannot be delivered. Parked, its collectors and furnaces stop by themselves before that (throttled in its status); in flight they keep running for thrust. Jettison what it cannot use (space_platform action=jettison: uncrushed chunks, surplus plates), and lower standing supply counts that fill it (space_platform action=supply, 0 drops an item)."
        end
        local uncrushed = {}
        for _, item in ipairs(platform.hub_items or {}) do
            local kind = item.name:match("^(.+)%-asteroid%-chunk$")
            if kind and item.count > 0 and not (platform.recipes or {})[kind .. "-asteroid-crushing"] then
                uncrushed[#uncrushed + 1] = item.name .. " x" .. item.count .. " (needs a crusher on " .. kind .. "-asteroid-crushing)"
            end
        end
        if #uncrushed > 0 then
            warnings[#warnings + 1] = platform.name .. " collects chunks no crusher uses and they fill its hub: "
                .. table.concat(uncrushed, ", ") .. ". Add those crushers with place_ghosts or jettison the chunks."
        end
        for _, stuck in ipairs(platform.resmelting or {}) do
            local feeders = {}
            for _, feeder in ipairs(stuck.feeders or {}) do feeders[#feeders + 1] = tostring(feeder.unit_number) end
            local aboard = facts.space.character_surface == platform.surface
            local fix = "configure_inserter unit_number=" .. table.concat(feeders, "/") .. " allowed_items=[" .. stuck.should_take .. "]"
                .. " (works from anywhere), then "
                .. (aboard
                    and ("extract_items unit_number=" .. tostring(stuck.furnace.unit_number) .. " inventory_type=furnace_source item=" .. stuck.holds .. ".")
                    or ("space_platform action=deconstruct x=" .. stuck.furnace.position.x .. " y=" .. stuck.furnace.position.y
                        .. " (its " .. stuck.holds .. " goes to the hub) and place_ghosts surface=" .. tostring(platform.surface)
                        .. " the " .. stuck.furnace.name .. " back at the same spot. No need to board."))
            warnings[#warnings + 1] = platform.name .. "'s " .. stuck.furnace.name .. " " .. tostring(stuck.furnace.unit_number)
                .. " at " .. stuck.furnace.position.x .. "," .. stuck.furnace.position.y .. " holds " .. stuck.count .. " " .. stuck.holds
                .. " from the hub, so it smelts " .. stuck.holds .. " again instead of the hub's " .. stuck.should_take .. ". " .. fix
        end
        if platform.stranded_ghosts and platform.stranded_ghosts[1] then
            warnings[#warnings + 1] = platform.name .. " has ghosts the hub will never build or must not build: "
                .. table.concat(platform.stranded_ghosts, ", ")
                .. ". Remove them with space_platform action=clear_ghosts, then place what you need where it fits (place_ghosts; over empty space, include space-platform-foundation tiles under it)."
        end
        if platform.ghosts_missing_items and platform.ghosts_missing_items[1] then
            local lacking = {}
            for _, item in ipairs(platform.ghosts_missing_items) do
                lacking[#lacking + 1] = item.name .. " x" .. (item.needed - item.available)
            end
            local fix = "ship them (space_platform action=ship)."
            if platform.supply and (platform.supply.silos_with_supply_chest or 0) > 0 then
                fix = platform.supply.from .. "'s supply silo sends them once its logistic network holds them."
            elseif platform.space_location and (platform.orbit_silos or 0) == 0 then
                fix = "no rocket silo on " .. platform.space_location .. " can send them, so they arrive only where a silo stands below the platform."
            end
            warnings[#warnings + 1] = platform.name .. "'s hub lacks " .. table.concat(lacking, ", ") .. " for its ghosts: " .. fix
        end
        if platform.queued_cargo and platform.queued_cargo[1] and (facts.rocket_silos or 0) > 0 and not facts.rocket_ready then
            warnings[#warnings + 1] = "Cargo for " .. platform.name .. " waits for rockets, and the silo has "
                .. (facts.rocket_parts or 0) .. "/" .. parts_required .. " rocket parts. Each part needs "
                .. rocket_part_inputs(force, surface)
                .. ": the scarcest input sets how fast cargo leaves, so add assemblers (and their inputs) for it and feed the silo with inserters."
        end
    end
    if S.landing_pad_free_slots == 0 then
        warnings[#warnings + 1] = "The landing pad at home is full, so nothing more lands there (space science included): take items out with inserters, and drop requests for bulk items such as iron-ore or asteroid chunks (space_platform action=request with only what you need)."
    end
    state.next_goal = goal
    state.how = how
    state.warnings = warnings
    return state
end

function M.snapshot(character)
    if not (character and character.valid) then
        return {success = false, error = "no character; spawn first"}
    end
    -- The factory, production and progression describe home (Nauvis)
    -- wherever the character is.
    local surface = space.home_surface(character)
    local force = character.force
    local found = surface.find_entities_filtered{force = force}
    local counts_by_name = {}
    local counts_by_type = {}
    local statuses = {}
    local mining_drills = {}
    local mining_targets = {}
    local power_networks_by_id = {}
    local machine_positions = {}
    local entity_count = 0
    local facts = {
        steam_engines = 0, steam_engines_working = 0,
        boilers = 0, boiler_fuel_min = nil,
        labs = 0, labs_powered = 0, labs_working = 0,
        assemblers = 0, red_science_assemblers = 0, recipe_assemblers = {},
        burner_machines = 0, low_fuel_units = {}, low_fuel_count = 0,
        output_blocked = 0, starved_assemblers = 0, held_output = {}, starved_for = {},
    }
    local min_x, min_y, max_x, max_y = nil, nil, nil, nil

    for _, entity in pairs(found) do
        if is_factory_entity(entity) then
            entity_count = entity_count + 1
            counts_by_name[entity.name] = (counts_by_name[entity.name] or 0) + 1
            counts_by_type[entity.type] = (counts_by_type[entity.type] or 0) + 1
            if (entity.type == "mining-drill" or entity.type == "furnace") and entity.burner then
                facts.burner_machines = facts.burner_machines + 1
                local fuel_ok, fuel = pcall(function() return entity.get_fuel_inventory().get_item_count() end)
                if fuel_ok and fuel < LOW_BURNER_FUEL then
                    facts.low_fuel_count = facts.low_fuel_count + 1
                    if #facts.low_fuel_units < 12 then
                        facts.low_fuel_units[#facts.low_fuel_units + 1] = {
                            unit_number = entity.unit_number, name = entity.name, fuel = fuel,
                        }
                    end
                end
            end

            local status = entity_status(entity)
            if status == "full_output" or status == "waiting_for_space_in_destination" then
                if entity.type == "furnace" or entity.type == "mining-drill" or entity.type == "assembling-machine" then
                    facts.output_blocked = facts.output_blocked + 1
                    for _, item in ipairs(held_products(entity)) do note_gap(facts.held_output, item, entity.position) end
                end
                if entity.type == "assembling-machine" then
                    local recipe_ok, recipe = pcall(function() return entity.get_recipe() end)
                    if recipe_ok and recipe and recipe.name:find("science%-pack$") then
                        facts.full_science_assemblers = (facts.full_science_assemblers or 0) + 1
                        facts.full_science_packs = facts.full_science_packs or {}
                        facts.full_science_packs[recipe.name] = true
                    end
                end
            elseif status == "item_ingredient_shortage" or status == "no_ingredients" then
                if entity.type == "assembling-machine" and status == "item_ingredient_shortage" then
                    facts.starved_assemblers = facts.starved_assemblers + 1
                end
                if entity.type == "assembling-machine" or entity.type == "furnace" then
                    for _, item in ipairs(lacked_ingredients(entity)) do note_gap(facts.starved_for, item, entity.position) end
                end
            end
            if status then statuses[status] = (statuses[status] or 0) + 1 end
            if COVERAGE_TYPES[entity.type] then machine_positions[#machine_positions + 1] = entity.position end

            local x, y = entity.position.x, entity.position.y
            min_x = min_x and math.min(min_x, x) or x
            min_y = min_y and math.min(min_y, y) or y
            max_x = max_x and math.max(max_x, x) or x
            max_y = max_y and math.max(max_y, y) or y

            if entity.type == "mining-drill" then
                local target_ok, target = pcall(function() return entity.mining_target end)
                if target_ok and target and target.valid then
                    table.insert(mining_targets, {
                        name = target.name,
                        position = position_table(target.position),
                    })
                end
                table.insert(mining_drills, {
                    unit_number = entity.unit_number,
                    name = entity.name,
                    position = position_table(entity.position),
                    resource = target_ok and target and target.name or nil,
                    energy_source = energy_source(entity),
                    status = status,
                })
            elseif entity.type == "generator" then
                facts.steam_engines = facts.steam_engines + 1
                if status == "working" then facts.steam_engines_working = facts.steam_engines_working + 1 end
            elseif entity.type == "boiler" then
                facts.boilers = facts.boilers + 1
                local fuel_ok, fuel = pcall(function() return entity.get_fuel_inventory().get_item_count() end)
                if fuel_ok and fuel then
                    facts.boiler_fuel_min = facts.boiler_fuel_min and math.min(facts.boiler_fuel_min, fuel) or fuel
                end
            elseif entity.type == "lab" then
                facts.labs = facts.labs + 1
                if status ~= "no_power" then facts.labs_powered = facts.labs_powered + 1 end
                if status == "working" then facts.labs_working = facts.labs_working + 1 end
                if status == "missing_science_packs" then
                    -- Which packs of the current research this idle lab lacks.
                    local research = entity.force.current_research
                    local inventory = entity.get_inventory(defines.inventory.lab_input)
                    if research and inventory then
                        facts.lab_missing_packs = facts.lab_missing_packs or {}
                        for _, ingredient in pairs(research.research_unit_ingredients) do
                            if inventory.get_item_count(ingredient.name) == 0 then
                                facts.lab_missing_packs[ingredient.name] = (facts.lab_missing_packs[ingredient.name] or 0) + 1
                            end
                        end
                    end
                end
            elseif entity.type == "assembling-machine" then
                facts.assemblers = facts.assemblers + 1
                local recipe_ok, recipe = pcall(function() return entity.get_recipe() end)
                if recipe_ok and recipe then
                    facts.recipe_assemblers[recipe.name] = (facts.recipe_assemblers[recipe.name] or 0) + 1
                    if recipe.name == "automation-science-pack" then
                        facts.red_science_assemblers = facts.red_science_assemblers + 1
                    end
                elseif recipe_ok and not recipe and entity.products_finished > 0 then
                    -- Produced before, now without a recipe: usually cleared
                    -- and forgotten.
                    facts.unset_machines = facts.unset_machines or {}
                    if #facts.unset_machines < 5 then
                        local cleared = (storage.cleared_recipes or {})[entity.unit_number]
                        table.insert(facts.unset_machines, {
                            unit_number = entity.unit_number,
                            name = entity.name,
                            position = position_table(entity.position),
                            products_finished = entity.products_finished,
                            cleared_recipe = cleared and cleared.recipe or nil,
                            cleared_minutes_ago = cleared and math.floor((game.tick - cleared.tick) / 3600) or nil,
                        })
                    end
                end
            elseif entity.type == "rocket-silo" then
                facts.rocket_silos = (facts.rocket_silos or 0) + 1
                local parts_ok, parts = pcall(function() return entity.rocket_parts end)
                local ready_ok, ready = pcall(function()
                    return entity.rocket_silo_status == defines.rocket_silo_status.rocket_ready
                end)
                if parts_ok and parts and parts > (facts.rocket_parts or -1) then facts.rocket_parts = parts end
                if ready_ok and ready then facts.rocket_ready = true end
            elseif entity.type == "roboport" then
                facts.roboports = (facts.roboports or 0) + 1
            elseif entity.type == "electric-pole" then
                local network_ok, network_id = pcall(function()
                    return entity.electric_network_id
                end)
                local key = network_ok and network_id and tostring(network_id) or "disconnected"
                local network = power_networks_by_id[key]
                if not network then
                    network = {
                        network_id = network_ok and network_id or nil,
                        pole_count = 0,
                        sample_position = position_table(entity.position),
                    }
                    power_networks_by_id[key] = network
                end
                network.pole_count = network.pole_count + 1
            end
        end
    end

    local origin = character and character.valid and character.position or {x = 0, y = 0}
    min_x = min_x or origin.x - 32
    min_y = min_y or origin.y - 32
    max_x = max_x or origin.x + 32
    max_y = max_y or origin.y + 32
    local factory_bounds = {
        left_top = {x = min_x, y = min_y},
        right_bottom = {x = max_x, y = max_y},
    }

    -- Home machines inside roboport construction range: robots (and so
    -- place_ghosts) reach only those, which matters most while away.
    local cells = {}
    for _, network in pairs(force.logistic_networks[surface.name] or {}) do
        for _, cell in pairs(network.cells) do cells[#cells + 1] = cell end
    end
    local covered_machines, uncovered_example, best = 0, nil, nil
    for _, position in ipairs(machine_positions) do
        local hit, nearest = false, nil
        for _, cell in ipairs(cells) do
            if cell.valid then
                if cell.is_in_construction_range(position) then hit = true break end
                local owner = cell.owner.position
                local d = (owner.x - position.x) ^ 2 + (owner.y - position.y) ^ 2
                if not nearest or d < nearest then nearest = d end
            end
        end
        if hit then covered_machines = covered_machines + 1
        elseif nearest and (not best or nearest < best) then
            best, uncovered_example = nearest, position_table(position)
        end
    end
    facts.robot_coverage = {machines = #machine_positions, covered = covered_machines, uncovered_example = uncovered_example}

    table.sort(mining_drills, function(a, b)
        if a.name ~= b.name then return a.name < b.name end
        if a.position.x ~= b.position.x then return a.position.x < b.position.x end
        return a.position.y < b.position.y
    end)
    local mining_drill_count = #mining_drills
    while #mining_drills > 50 do table.remove(mining_drills) end

    local power_networks = {}
    for _, network in pairs(power_networks_by_id) do
        table.insert(power_networks, network)
    end
    table.sort(power_networks, function(a, b)
        if a.pole_count ~= b.pole_count then return a.pole_count > b.pole_count end
        return tostring(a.network_id or "") < tostring(b.network_id or "")
    end)

    local margin = 8
    local blockers = entities.diagnose_factory_blockers(
        surface,
        force,
        min_x - margin,
        min_y - margin,
        max_x + margin,
        max_y + margin,
        25
    )
    local strategic = world.strategic_summary(
        surface,
        factory_bounds,
        mining_targets
    )

    facts.space = space.summary(force, character)
    facts.here = planet_facts(character, force)
    local here = facts.here and {
        surface = facts.here.surface,
        entities_by_name = sorted_counts(facts.here.counts),
        statuses = sorted_counts(facts.here.statuses),
        recipes = facts.here.recipes,
        acid_pumpjacks = facts.here.acid_pumpjacks,
        acid_pumpjacks_working = facts.here.acid_pumpjacks_working,
        ghosts = facts.here.ghosts,
        made = facts.here.made,
    } or nil
    return {
        tick = game.tick,
        surface = surface.name,
        character_surface = character.surface.name,
        -- The planet the character stands on when that is not home.
        here = here,
        space = facts.space,
        character = character_snapshot(character),
        research = research.get_research_status(character),
        production = compact_production(surface.name, force),
        progression = progression(surface, force, facts, character),
        world = strategic.world,
        expansion = strategic.expansion,
        factory = {
            bounds = factory_bounds,
            entity_count = entity_count,
            entities_by_name = sorted_counts(counts_by_name),
            entities_by_type = sorted_counts(counts_by_type),
            statuses = sorted_counts(statuses),
            mining_drill_count = mining_drill_count,
            mining_drills = mining_drills,
            mining_drills_truncated = mining_drill_count > #mining_drills,
            power_networks = power_networks,
            blockers = blockers,
        },
    }
end

return M
