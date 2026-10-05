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

-- The Vulcanus ladder, read from what stands there and the trigger
-- technologies: rocks and calcite, solar power, acid, one tungsten carbide
-- (unlocks foundry), a foundry (unlocks big-mining-drill), tungsten plate
-- (unlocks metallurgic science).
local function vulcanus_rung(here, force, character)
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
    return "Automate metallurgic science on Vulcanus.",
        recipe_text("metallurgic-science-pack") .. " in a foundry (molten copper from " .. recipe_text("molten-copper-from-lava")
        .. "); tungsten ore from big mining drills on tungsten-ore patches (" .. recipe_text("big-mining-drill") .. "). "
        .. (here.made["metallurgic-science-pack"] or 0) .. " packs made here." .. REMOTE_HOME
end

-- Rungs for a character away from home: on a platform or on Vulcanus.
local function travel_rung(S, character, here, force)
    if here then
        if here.surface == "vulcanus" then return vulcanus_rung(here, force, character) end
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

-- Space Age rungs after the first rocket, up to boarding for Vulcanus. Nil
-- when the generic research rungs (on space_path) are the right next step.
local function space_rung(facts, force, surface)
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
    if not vulcanus_found and ((P.recipes or {})["space-science-pack"] or 0) == 0 then
        return "Make space science on " .. P.name .. ".",
            recipe_text("space-science-pack") .. ", made only at zero gravity. On the platform: asteroid collectors at the foundation edge, crushers ("
            .. recipe_text("metallic-asteroid-crushing") .. "; " .. recipe_text("carbonic-asteroid-crushing") .. "; " .. recipe_text("oxide-asteroid-crushing")
            .. "), an electric furnace for iron plate, an assembler on space-science-pack, inserters from and into the hub, solar panels. Ship the parts (action=ship), then lay them out with place_ghosts surface="
            .. tostring(P.surface) .. " (add space-platform-foundation tiles to grow it)."
    end
    if not vulcanus_found and S.landing_pads == 0 then
        return "Build a cargo landing pad at home near the labs.",
            "build_layout; then space_platform action=request items=[{name:\"space-science-pack\",count:100}]."
    end
    if not vulcanus_found then
        local requested = false
        for _, name in ipairs(S.landing_pad_requests) do
            if name == "space-science-pack" then requested = true end
        end
        if not requested then
            return "Request space science on the landing pad.",
                "space_platform action=request items=[{name:\"space-science-pack\",count:100}]. Platforms in orbit drop requested items on their own."
        end
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
    if (P.stops or {})[1] ~= "vulcanus" then
        return "Set " .. P.name .. "'s course for Vulcanus.",
            "space_platform action=schedule stops=[\"vulcanus\"]. It stays in orbit until you are aboard (and armed and stocked), then leaves by itself."
    end
    return "Board " .. P.name .. " for Vulcanus.",
        "It waits in orbit until you are aboard, then leaves. Carry what you need to start there: Vulcanus has no water, so solar panels (4x power there) and accumulators, not steam; a roboport, construction robots and a storage chest so place_ghosts works there; chemical plants and assembling-machine-2s (carbon, tungsten carbide), a pumpjack (sulfuric acid geysers), pipes, drills, steel furnaces, belts, inserters, poles. Stand within reach of a silo and call space_platform action=board once: with no rocket ready it books the next one and launches you when it is ready. Do not unship cargo to board."
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
    if not goal and force.rockets_launched > 0 then space_goal, space_how = space_rung(facts, force, surface) end
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
            .. facts.starved_assemblers .. " assemblers lack ingredients: production and consumers are not connected. "
            .. "Carry items from the full machines to the starved assemblers with output inserters and belts (route_belt, build_layout) instead of moving them by hand."
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
                .. " slots): its collectors and crushers stop and rocket cargo cannot be delivered. Jettison what it cannot use soon (space_platform action=jettison: uncrushed chunks, surplus iron-ore or ice), then ship what unblocks it (space-platform-foundation, the missing machines)."
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
        if platform.stranded_ghosts and platform.stranded_ghosts[1] then
            warnings[#warnings + 1] = platform.name .. " has ghosts the hub will never build: "
                .. table.concat(platform.stranded_ghosts, ", ")
                .. ". Remove them with space_platform action=clear_ghosts, then place what you need where it fits (place_ghosts; over empty space, include space-platform-foundation tiles under it)."
        end
        if platform.ghosts_missing_items and platform.ghosts_missing_items[1] then
            local lacking = {}
            for _, item in ipairs(platform.ghosts_missing_items) do
                lacking[#lacking + 1] = item.name .. " x" .. (item.needed - item.available)
            end
            warnings[#warnings + 1] = platform.name .. "'s hub lacks " .. table.concat(lacking, ", ")
                .. " for its ghosts: ship them (space_platform action=ship)."
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
        output_blocked = 0, starved_assemblers = 0,
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
                end
                if entity.type == "assembling-machine" then
                    local recipe_ok, recipe = pcall(function() return entity.get_recipe() end)
                    if recipe_ok and recipe and recipe.name:find("science%-pack$") then
                        facts.full_science_assemblers = (facts.full_science_assemblers or 0) + 1
                        facts.full_science_packs = facts.full_science_packs or {}
                        facts.full_science_packs[recipe.name] = true
                    end
                end
            elseif status == "item_ingredient_shortage" and entity.type == "assembling-machine" then
                facts.starved_assemblers = facts.starved_assemblers + 1
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
