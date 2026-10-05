-- Space Age for one character: robot logistics on any surface, ghost
-- placement for robots and platform hubs, and space platform travel.
local characters = require("characters")

local M = {}

local GHOST_SCAN_LIMIT = 2000
local MAX_PLACE_ENTITIES = 200
local MAX_PLACE_TILES = 400
local OBSTACLE_TYPES = {tree = true, ["simple-entity"] = true, fish = true}

local function fail(error_kind, message, extra)
    local result = extra or {}
    result.success = false
    result.error_kind = error_kind
    result.error = message
    return result
end

local function pos_table(position)
    if not position then return nil end
    return {x = position.x, y = position.y}
end

local function no_character(agent_id)
    return fail("no_character", "no character for agent " .. tostring(agent_id) .. "; spawn first")
end

local function state_name(value)
    for name, state in pairs(defines.space_platform_state) do
        if state == value then return name end
    end
    return "unknown"
end

-- Map of name -> count to a list of {name, count}, largest first.
local function top_counts(counts, limit)
    local list = {}
    for name, count in pairs(counts) do list[#list + 1] = {name = name, count = count} end
    table.sort(list, function(a, b)
        if a.count ~= b.count then return a.count > b.count end
        return a.name < b.name
    end)
    while #list > limit do list[#list] = nil end
    return list
end

local function contents_counts(contents)
    local counts = {}
    for _, entry in pairs(contents or {}) do
        counts[entry.name] = (counts[entry.name] or 0) + entry.count
    end
    return counts
end

local function valid_platforms(force)
    local list = {}
    for _, platform in pairs(force.platforms) do
        if platform.valid then list[#list + 1] = platform end
    end
    return list
end

local function platform_names(force)
    local names = {}
    for _, platform in ipairs(valid_platforms(force)) do names[#names + 1] = platform.name end
    return names
end

local function force_surface_names(force)
    local names, seen = {}, {}
    for _, surface in pairs(game.surfaces) do
        if surface.count_entities_filtered{force = force, limit = 1} > 0 then
            names[#names + 1] = surface.name
            seen[surface.name] = true
        end
    end
    for _, platform in ipairs(valid_platforms(force)) do
        local surface = platform.surface
        if surface and not seen[surface.name] then
            names[#names + 1] = surface.name
            seen[surface.name] = true
        end
    end
    return names
end

-- A surface by name (a platform name also resolves to its surface), or the
-- character's own surface when no name is given.
local function resolve_surface(character, surface_name)
    if type(surface_name) ~= "string" or surface_name == "" then return character.surface end
    local surface = game.get_surface(surface_name)
    if surface then return surface end
    for _, platform in ipairs(valid_platforms(character.force)) do
        if platform.name == surface_name and platform.surface then return platform.surface end
    end
    return nil, fail("unknown_surface", "no surface named " .. surface_name, {
        surfaces = force_surface_names(character.force),
    })
end

local function construction_cells(surface, force)
    local cells = {}
    for _, network in pairs(force.logistic_networks[surface.name] or {}) do
        for _, cell in pairs(network.cells) do cells[#cells + 1] = cell end
    end
    return cells
end

-- The logistic network whose construction range covers `position`, or nil.
local function covering_network(cells, position)
    for _, cell in ipairs(cells) do
        if cell.valid and cell.is_in_construction_range(position) then return cell.logistic_network end
    end
    return nil
end

local function covered(cells, position)
    return covering_network(cells, position) ~= nil
end

local function first_item(items)
    local first = items and items[1]
    if not first then return nil end
    return {name = first.name, count = first.count or 1}
end

local function ghost_item(ghost)
    local ok, items = pcall(function() return ghost.ghost_prototype.items_to_place_this end)
    if not ok then return nil end
    return first_item(items)
end

local function available_count(surface, force, name)
    if surface.platform then
        local hub = surface.platform.hub
        return (hub and hub.valid) and hub.get_item_count(name) or 0
    end
    local total = 0
    for _, network in pairs(force.logistic_networks[surface.name] or {}) do
        total = total + network.get_item_count(name)
    end
    return total
end

-- needed: map item name -> count. Returns the top 10 shortfalls.
local function shortfalls(needed, surface, force)
    local list = {}
    for name, count in pairs(needed) do
        local available = available_count(surface, force, name)
        if count > available then
            list[#list + 1] = {name = name, needed = count, available = available}
        end
    end
    table.sort(list, function(a, b)
        local sa, sb = a.needed - a.available, b.needed - b.available
        if sa ~= sb then return sa > sb end
        return a.name < b.name
    end)
    while #list > 10 do list[#list] = nil end
    return list
end

-- Ghost-item shortfalls on a planet, each covered ghost counted against the
-- network that builds it (stock in another network does not help it); same
-- shape as shortfalls().
local function network_shortfalls(needed_by_network)
    local short = {}
    for _, entry in pairs(needed_by_network) do
        for name, count in pairs(entry.needed) do
            local available = entry.network.valid and entry.network.get_item_count(name) or 0
            local item = short[name] or {name = name, needed = 0, available = 0}
            item.needed = item.needed + count
            item.available = item.available + math.min(count, available)
            short[name] = item
        end
    end
    local list = {}
    for _, item in pairs(short) do
        if item.needed > item.available then list[#list + 1] = item end
    end
    table.sort(list, function(a, b)
        local sa, sb = a.needed - a.available, b.needed - b.available
        if sa ~= sb then return sa > sb end
        return a.name < b.name
    end)
    while #list > 10 do list[#list] = nil end
    return list
end

local function surface_logistics(surface, force, detail)
    local result = {
        networks = {},
        roboports = 0,
        construction_robots = 0,
        construction_robots_available = 0,
        logistic_robots = 0,
    }
    for _, network in pairs(force.logistic_networks[surface.name] or {}) do
        local entry = {
            roboports = #network.cells,
            construction_robots = network.all_construction_robots,
            construction_robots_available = network.available_construction_robots,
            logistic_robots = network.all_logistic_robots,
            logistic_robots_available = network.available_logistic_robots,
        }
        if detail then entry.items = top_counts(contents_counts(network.get_contents()), 15) end
        result.networks[#result.networks + 1] = entry
        result.roboports = result.roboports + entry.roboports
        result.construction_robots = result.construction_robots + entry.construction_robots
        result.construction_robots_available = result.construction_robots_available + entry.construction_robots_available
        result.logistic_robots = result.logistic_robots + entry.logistic_robots
    end
    result.entity_ghosts = surface.count_entities_filtered{force = force, type = "entity-ghost"}
    result.tile_ghosts = surface.count_entities_filtered{force = force, type = "tile-ghost"}
    result.deconstruction_orders = surface.count_entities_filtered{to_be_deconstructed = true}

    local on_platform = surface.platform ~= nil
    local cells = on_platform and {} or construction_cells(surface, force)
    local ghosts = surface.find_entities_filtered{
        force = force,
        type = {"entity-ghost", "tile-ghost"},
        limit = GHOST_SCAN_LIMIT,
    }
    local needed, by_network, uncovered, sample = {}, {}, 0, {}
    for _, ghost in ipairs(ghosts) do
        local network = (not on_platform) and covering_network(cells, ghost.position) or nil
        local is_covered = on_platform or network ~= nil
        if not is_covered then uncovered = uncovered + 1 end
        local item = ghost_item(ghost)
        if item and on_platform then
            needed[item.name] = (needed[item.name] or 0) + item.count
        elseif item and network then
            local entry = by_network[network.network_id]
            if not entry then
                entry = {network = network, needed = {}}
                by_network[network.network_id] = entry
            end
            entry.needed[item.name] = (entry.needed[item.name] or 0) + item.count
        end
        if detail and #sample < 10 then
            sample[#sample + 1] = {name = ghost.ghost_name, position = pos_table(ghost.position), covered = is_covered}
        end
    end
    result.uncovered_ghosts = uncovered
    result.missing_items = on_platform and shortfalls(needed, surface, force) or network_shortfalls(by_network)
    result.ghosts_truncated = result.entity_ghosts + result.tile_ghosts > GHOST_SCAN_LIMIT
    if detail then result.ghost_sample = sample end
    return result
end

local STATUS_NAMES = {}
for name, value in pairs(defines.entity_status) do STATUS_NAMES[value] = name end

local function status_name(entity)
    return entity.status and STATUS_NAMES[entity.status] or nil
end

local function entity_recipe_name(entity)
    local ok, recipe = pcall(entity.get_recipe)
    if ok and recipe then return recipe.name end
    if entity.type == "furnace" then
        local ok_previous, previous = pcall(function() return entity.previous_recipe end)
        if ok_previous and previous then
            local name = previous.name
            if type(name) ~= "string" and name then name = name.name end
            return name
        end
    end
    return nil
end

local function safe_value(read)
    local ok, value = pcall(read)
    if ok then return value end
    return nil
end

-- Defined with the place_ghosts helpers below.
local stranded_ghosts, tile_ghost_keys, enclosed_space, thrust_shortages

-- A platform leaving orbit meets asteroids, mostly from the front (negative
-- y, the direction of travel). Flown from Nauvis to Vulcanus (one thruster,
-- speed 1.1-1.5): one fed turret with 10 magazines, and four turrets
-- clustered at one side, were destroyed on the way; six turrets (four in
-- the front half) with yellow magazines and no military research were
-- destroyed at 0.8 of the way, even with the hub topping them up; the same
-- six with piercing rounds, military-2, physical-projectile-damage-2 and
-- weapon-shooting-speed-2 arrived intact, using about 410 magazines. "Front"
-- is the front half (turret centre ahead of the hub centre): with an
-- 18-tile range, turrets beside the hub's front half cover the front edge.
local MIN_READY_TURRETS = 6
local MIN_FRONT_TURRETS = 4
local MIN_AMMO = 600
local REQUIRED_RESEARCH = {"military-2", "physical-projectile-damage-2", "weapon-shooting-speed-2"}
-- Ammo too weak to count toward MIN_AMMO.
local WEAK_AMMO = {["firearm-magazine"] = true}
-- Strongest first; the hub loads turrets with the first it holds.
local AMMO_PREFERENCE = {"uranium-rounds-magazine", "piercing-rounds-magazine", "firearm-magazine"}
-- A turret counts as ready when an inserter feeds it or it holds this many
-- magazines (a gun turret holds one stack, 100).
local MIN_LOADED_AMMO = 50

local function armament(platform)
    local surface = platform.surface
    local result = {
        turrets = 0, turrets_ready = 0, front_turrets_ready = 0, turret_ammo = 0, hub_ammo = 0, weak_ammo = 0,
        min_ready_turrets = MIN_READY_TURRETS, min_front_turrets = MIN_FRONT_TURRETS, min_ammo = MIN_AMMO,
        missing_research = {},
    }
    for _, name in ipairs(REQUIRED_RESEARCH) do
        local technology = platform.force.technologies[name]
        if technology and not technology.researched then result.missing_research[#result.missing_research + 1] = name end
    end
    if not surface then return result end
    local fed = {}
    for _, inserter in pairs(surface.find_entities_filtered{type = "inserter", force = platform.force}) do
        local target = inserter.drop_target
        if target and target.valid and target.type == "ammo-turret" and inserter.pickup_target then
            fed[target.unit_number] = true
        end
    end
    local function count_ammo(contents, field)
        for _, entry in pairs(contents) do
            local proto = prototypes.item[entry.name]
            if proto and proto.type == "ammo" then
                if WEAK_AMMO[entry.name] then
                    result.weak_ammo = result.weak_ammo + entry.count
                else
                    result[field] = result[field] + entry.count
                end
            end
        end
    end
    local hub_y = platform.hub and platform.hub.valid and platform.hub.position.y or 0
    for _, turret in pairs(surface.find_entities_filtered{type = "ammo-turret", force = platform.force}) do
        result.turrets = result.turrets + 1
        local inventory = turret.get_inventory(defines.inventory.turret_ammo)
        if inventory then count_ammo(inventory.get_contents(), "turret_ammo") end
        if fed[turret.unit_number] or (inventory and inventory.get_item_count() or 0) >= MIN_LOADED_AMMO then
            result.turrets_ready = result.turrets_ready + 1
            if turret.position.y < hub_y then result.front_turrets_ready = result.front_turrets_ready + 1 end
        end
    end
    if platform.hub and platform.hub.valid then
        count_ammo(platform.hub.get_inventory(defines.inventory.hub_main).get_contents(), "hub_ammo")
    end
    result.armed = result.turrets_ready >= MIN_READY_TURRETS and result.front_turrets_ready >= MIN_FRONT_TURRETS
        and result.hub_ammo + result.turret_ammo >= MIN_AMMO and not result.missing_research[1]
    result.needs = result.turrets_ready .. "/" .. MIN_READY_TURRETS .. " gun turrets ready (fed by an inserter or holding "
        .. MIN_LOADED_AMMO .. "+ magazines), " .. result.front_turrets_ready .. "/" .. MIN_FRONT_TURRETS
        .. " of them in the front half (ahead of the hub centre; smaller y is the direction of travel), "
        .. (result.hub_ammo + result.turret_ammo) .. "/" .. MIN_AMMO .. " piercing-rounds-magazine (or better) in turrets and hub"
        .. (result.weak_ammo > 0 and (" (" .. result.weak_ammo .. " firearm-magazine do not count: too weak)") or "")
        .. (result.missing_research[1] and (", research " .. table.concat(result.missing_research, ", ")) or "")
    return result
end

-- Where more turrets fit now (foundation under them, nothing built or
-- planned in the way), front first; only ahead of the hub centre while
-- `front_only`. First turret + inserter pairs fed straight from the hub
-- (`inserter_direction` faces the hub, as place_ghosts takes it), then free
-- spots for turrets the hub loads with magazines (place_ghosts requests
-- them; action=load_turrets for turrets already built).
local function turret_slots(platform, wanted, front_only)
    local slots = {}
    local hub = platform.hub
    if wanted <= 0 or not (hub and hub.valid) then return slots end
    local surface, force = platform.surface, platform.force
    local box = hub.bounding_box
    local l, r = math.floor(box.left_top.x), math.ceil(box.right_bottom.x)
    local t, b = math.floor(box.left_top.y), math.ceil(box.right_bottom.y)
    local hub_y = hub.position.y
    local taken = {}
    -- Ghosts the hub never builds do not count: place_ghosts replaces them.
    local dead = {}
    for _, ghost in ipairs(stranded_ghosts(surface)) do dead[ghost.unit_number] = true end
    local function cells_of(name, position)
        local c = prototypes.entity[name].collision_box
        local cells = {}
        for x = math.floor(position.x + c.left_top.x), math.ceil(position.x + c.right_bottom.x) - 1 do
            for y = math.floor(position.y + c.left_top.y), math.ceil(position.y + c.right_bottom.y) - 1 do
                cells[#cells + 1] = x .. "," .. y
            end
        end
        return cells
    end
    local function free(name, position)
        local cells = cells_of(name, position)
        for _, cell in ipairs(cells) do
            if taken[cell] then return nil end
        end
        if not surface.can_place_entity{name = name, position = position, force = force} then return nil end
        local c = prototypes.entity[name].collision_box
        for _, ghost in pairs(surface.find_entities_filtered{
            area = {{position.x + c.left_top.x, position.y + c.left_top.y}, {position.x + c.right_bottom.x, position.y + c.right_bottom.y}},
            type = "entity-ghost",
        }) do
            if not dead[ghost.unit_number] then return nil end
        end
        return cells
    end
    local function take(cells)
        for _, cell in ipairs(cells) do taken[cell] = true end
    end
    local fed = {}
    for x = l, r - 1 do
        fed[#fed + 1] = {inserter = {x = x + 0.5, y = t - 0.5}, pickup = "south", turrets = {{x = x, y = t - 2}, {x = x + 1, y = t - 2}}}
    end
    for y = t, b - 1 do
        fed[#fed + 1] = {inserter = {x = l - 0.5, y = y + 0.5}, pickup = "east", turrets = {{x = l - 2, y = y}, {x = l - 2, y = y + 1}}}
        fed[#fed + 1] = {inserter = {x = r + 0.5, y = y + 0.5}, pickup = "west", turrets = {{x = r + 2, y = y}, {x = r + 2, y = y + 1}}}
    end
    for _, candidate in ipairs(fed) do
        if #slots >= wanted then return slots end
        local inserter_cells = free("inserter", candidate.inserter)
        if inserter_cells then
            for _, turret in ipairs(candidate.turrets) do
                local cells = (turret.y < hub_y or not front_only) and free("gun-turret", turret)
                if cells then
                    take(cells)
                    take(inserter_cells)
                    slots[#slots + 1] = {turret = turret, inserter = candidate.inserter, inserter_direction = candidate.pickup}
                    break
                end
            end
        end
    end
    -- Loaded turrets: any free spot (ahead of the hub centre while
    -- front_only), front rows first.
    local spots = {}
    for y = t - 24, front_only and math.ceil(hub_y) - 1 or b + 24 do
        for x = l - 24, r + 24 do spots[#spots + 1] = {x = x, y = y} end
    end
    table.sort(spots, function(a, c)
        if a.y ~= c.y then return a.y < c.y end
        return math.abs(a.x - hub.position.x) < math.abs(c.x - hub.position.x)
    end)
    for _, spot in ipairs(spots) do
        if #slots >= wanted then break end
        local cells = free("gun-turret", spot)
        if cells then
            take(cells)
            slots[#slots + 1] = {turret = spot, loaded = true}
        end
    end
    return slots
end

-- The ammo in the hub that `turret_name` can fire: the first of
-- AMMO_PREFERENCE it holds, else the one it holds most of; nil when none.
local function hub_ammo_for(platform, turret_name)
    local hub = platform.hub
    if not (hub and hub.valid) then return nil end
    local categories = {}
    for _, category in pairs(prototypes.entity[turret_name].attack_parameters.ammo_categories or {}) do categories[category] = true end
    local inventory = hub.get_inventory(defines.inventory.hub_main)
    local function fits(name)
        local proto = prototypes.item[name]
        local category = proto and proto.type == "ammo" and proto.ammo_category
        return category and categories[category.name]
    end
    for _, name in ipairs(AMMO_PREFERENCE) do
        if prototypes.item[name] and fits(name) and inventory.get_item_count(name) > 0 then return name end
    end
    local best, most = nil, 0
    for _, entry in pairs(inventory.get_contents()) do
        if fits(entry.name) and entry.count > most then best, most = entry.name, entry.count end
    end
    return best
end

-- Ask the hub to fill a turret (or turret ghost) to one stack of ammo; the
-- hub delivers item requests on its platform. A turret already holding
-- MIN_LOADED_AMMO, or with a request pending, is left alone. Returns the
-- count requested.
local function request_turret_ammo(platform, entity)
    local is_ghost = entity.type == "entity-ghost"
    local function plan(ammo, count)
        return {{id = {name = ammo}, items = {in_inventory = {{inventory = defines.inventory.turret_ammo, stack = 0, count = count}}}}}
    end
    if is_ghost then
        local ammo = hub_ammo_for(platform, entity.ghost_name)
        if not ammo or next(entity.insert_plan) then return 0 end
        local count = prototypes.item[ammo].stack_size
        entity.insert_plan = plan(ammo, count)
        return count
    end
    local inventory = entity.get_inventory(defines.inventory.turret_ammo)
    if not inventory or entity.item_request_proxy then return 0 end
    local have = inventory.get_item_count()
    if have >= MIN_LOADED_AMMO then return 0 end
    local loaded = not inventory.is_empty() and inventory[1].valid_for_read and inventory[1].name
    local ammo = loaded or hub_ammo_for(platform, entity.name)
    if not ammo then return 0 end
    local count = prototypes.item[ammo].stack_size - have
    entity.surface.create_entity{name = "item-request-proxy", position = entity.position, force = entity.force, target = entity, modules = plan(ammo, count)}
    return count
end

-- The station the platform's schedule sends it to next, when that is not
-- where it is parked; nil when it is staying.
local function leaving_for(platform)
    local schedule = platform.schedule
    local location = platform.space_location
    if not (schedule and schedule.records and location) then return nil end
    local record = schedule.records[schedule.current or 1]
    if record and record.station and record.station ~= location.name then return record.station end
    return nil
end

-- True when an agent character is on the platform: a platform sent to
-- another planet without its crew strands the agent at home.
local function crewed(platform)
    for _, character in pairs(storage.characters or {}) do
        if character.valid and character.surface == platform.surface then return true end
    end
    return false
end

-- Why a departure is held: "unarmed", "thrust_stock" (thruster-fluid
-- ingredients under THRUST_RESERVE: the thrusters burn out within seconds
-- and the platform stalls), "no_crew", or nil when it may leave.
local function hold_reason(platform)
    if not armament(platform).armed then return "unarmed" end
    if thrust_shortages(platform)[1] then return "thrust_stock" end
    if not crewed(platform) then return "no_crew" end
    return nil
end

-- Called every second, for each platform:
-- * A platform about to leave orbit unarmed, short of thrust stock, or
--   without an agent aboard has its schedule parked in storage (pausing it
--   would also stop its hub building the turrets it needs); once all hold,
--   the schedule is put back and it leaves.
-- * With a standing ammo order (load_turrets, or turrets placed with
--   place_ghosts), turrets below MIN_LOADED_AMMO get a request the hub fills,
--   in flight too: loaded turrets otherwise run dry within two minutes, and
--   rebuilt ones start empty.
function M.tend_platforms()
    storage.departure_held = storage.departure_held or {}
    storage.turret_ammo_orders = storage.turret_ammo_orders or {}
    local held = storage.departure_held
    for _, force in pairs(game.forces) do
        for _, platform in pairs(force.platforms) do
            if platform.valid and platform.hub and platform.hub.valid then
                if storage.turret_ammo_orders[platform.index] then
                    for _, turret in pairs(platform.surface.find_entities_filtered{type = "ammo-turret", force = force}) do
                        request_turret_ammo(platform, turret)
                    end
                end
                -- leaving_for is nil in transit, and a held platform has no
                -- schedule, so it never moves while held.
                if held[platform.index] then
                    if not hold_reason(platform) then
                        platform.schedule = {current = 1, records = held[platform.index]}
                        platform.paused = false
                        held[platform.index] = nil
                    end
                elseif leaving_for(platform) and hold_reason(platform) then
                    held[platform.index] = platform.schedule.records
                    platform.schedule = nil
                    platform.paused = false
                end
            end
        end
    end
end

-- Drop a held departure, e.g. when the schedule is set again.
local function release_hold(platform)
    if storage.departure_held then storage.departure_held[platform.index] = nil end
end

-- Item stock the hub should hold for each thruster-fluid ingredient before
-- departure: one thruster from Nauvis to Vulcanus burned about 400 iron-ore
-- for oxidizer.
local THRUST_RESERVE = 450

-- Asteroid-crushing recipes (basic ones) that make `item`; prototypes do
-- not change at runtime, so the answer is cached.
local crushing_cache = {}
local function crushing_sources(item)
    if crushing_cache[item] then return crushing_cache[item] end
    local sources = {}
    for name, candidate in pairs(prototypes.recipe) do
        if name:find("asteroid%-crushing$") and not name:find("^advanced") then
            for _, product in pairs(candidate.products) do
                if product.name == item then sources[#sources + 1] = name end
            end
        end
    end
    crushing_cache[item] = sources
    return sources
end

-- Assembling machines (chemical plants) on `surface` whose recipe makes
-- `fluid`, with their status and the item ingredients the hub holds less of
-- than `reserve` (default: one craft) (`made_by`: asteroid-crushing recipes
-- for it).
local function fluid_producers(surface, force, fluid, hub_inventory, reserve)
    local producers = {}
    if not fluid then return producers end
    for _, plant in pairs(surface.find_entities_filtered{type = "assembling-machine", force = force}) do
        local recipe = plant.get_recipe()
        local makes = false
        for _, product in pairs(recipe and recipe.products or {}) do
            if product.name == fluid then makes = true end
        end
        if makes then
            local short = {}
            for _, ingredient in pairs(recipe.ingredients) do
                local have = ingredient.type == "item" and hub_inventory and hub_inventory.get_item_count(ingredient.name) or nil
                if have and have < (reserve or ingredient.amount) then
                    short[#short + 1] = {name = ingredient.name, hub = have, made_by = crushing_sources(ingredient.name)}
                end
            end
            producers[#producers + 1] = {position = plant.position, recipe = recipe.name, status = status_name(plant), short = short}
        end
    end
    return producers
end

-- Thruster-fluid plants on the platform whose item ingredients the hub
-- holds under THRUST_RESERVE of.
function thrust_shortages(platform)
    local surface, force = platform.surface, platform.force
    local hub_inventory = platform.hub and platform.hub.valid and platform.hub.get_inventory(defines.inventory.hub_main)
    local seen, short = {}, {}
    if not surface then return short end
    for _, thruster in pairs(surface.find_entities_filtered{type = "thruster", force = force}) do
        for index = 1, #thruster.fluidbox do
            local filter = safe_value(function() return thruster.fluidbox.get_filter(index) end)
            if filter and not seen[filter.name] then
                seen[filter.name] = true
                for _, producer in ipairs(fluid_producers(surface, force, filter.name, hub_inventory, THRUST_RESERVE)) do
                    if producer.short[1] then
                        producer.fluid = filter.name
                        short[#short + 1] = producer
                    end
                end
            end
        end
    end
    return short
end

local function rockets_for(counts)
    local lift = safe_value(function() return prototypes.utility_constants.rocket_lift_weight end)
    if not lift then return 1 end
    local weight = 0
    for name, count in pairs(counts) do
        weight = weight + (safe_value(function() return prototypes.item[name].weight end) or 0) * count
    end
    return math.max(1, math.ceil(weight / lift))
end

local function platform_summary(platform)
    local location = platform.space_location
    local surface = platform.surface
    local summary = {
        name = platform.name,
        state = state_name(platform.state),
        space_location = location and location.name or nil,
        surface = surface and surface.name or nil,
        paused = platform.paused,
        stops = {},
    }
    local schedule = platform.schedule
    local held = storage.departure_held and storage.departure_held[platform.index]
    for _, record in pairs(held or (schedule and schedule.records) or {}) do
        summary.stops[#summary.stops + 1] = record.station
    end
    local hub = platform.hub
    if hub and hub.valid then
        local inventory = hub.get_inventory(defines.inventory.hub_main)
        summary.hub_items = inventory and top_counts(contents_counts(inventory.get_contents()), 15) or {}
        summary.hub_slots = inventory and #inventory or nil
        summary.hub_free_slots = inventory and inventory.count_empty_stacks() or nil
    end
    summary.armament = armament(platform)
    if not summary.armament.armed then
        local a = summary.armament
        summary.armament.turret_slots = turret_slots(platform, math.max(a.min_ready_turrets - a.turrets_ready, a.min_front_turrets - a.front_turrets_ready),
            a.front_turrets_ready < a.min_front_turrets)
    end
    summary.departure_held = storage.departure_held and storage.departure_held[platform.index] and (hold_reason(platform) or "releasing") or nil
    local queued = {}
    for _, shipment in ipairs(storage.space_shipments or {}) do
        if shipment.platform_name == platform.name and shipment.inventory and shipment.inventory.valid then
            for _, entry in pairs(shipment.inventory.get_contents()) do
                queued[entry.name] = (queued[entry.name] or 0) + entry.count
            end
        end
    end
    if next(queued) then
        summary.queued_cargo = top_counts(queued, 15)
        summary.queued_rockets = rockets_for(queued)
    end
    for _, shipment in ipairs(storage.space_shipments or {}) do
        if shipment.platform_name == platform.name and shipment.blocked then summary.cargo_blocked = shipment.blocked end
    end
    if surface then
        local force = platform.force
        local counts, recipes = {}, {}
        local thrusters, turrets, collectors, thruster_inputs = 0, 0, 0, {}
        for _, entity in pairs(surface.find_entities_filtered{force = force}) do
            local kind = entity.type
            if kind ~= "entity-ghost" and kind ~= "tile-ghost" and entity.name ~= "space-platform-hub" then
                counts[entity.name] = (counts[entity.name] or 0) + 1
            end
            if kind == "thruster" then
                thrusters = thrusters + 1
                if #thruster_inputs < 5 then
                    for index = 1, #entity.fluidbox do
                        local filter = safe_value(function() return entity.fluidbox.get_filter(index) end)
                        local fluid = entity.fluidbox[index]
                        if not (fluid and fluid.amount > 0) then
                            local connect_at, connected = {}, false
                            for _, connection in pairs(entity.fluidbox.get_pipe_connections(index)) do
                                connect_at[#connect_at + 1] = connection.target_position
                                if connection.target then connected = true end
                            end
                            thruster_inputs[#thruster_inputs + 1] = {
                                thruster = entity.position,
                                fluid = filter and filter.name or nil,
                                pipe_to = connect_at,
                                pipe_connected = connected,
                            }
                        end
                    end
                end
            end
            if kind == "ammo-turret" then turrets = turrets + 1 end
            if entity.name == "asteroid-collector" then collectors = collectors + 1 end
            if kind == "assembling-machine" or kind == "furnace" then
                local recipe = entity_recipe_name(entity)
                if recipe then recipes[recipe] = (recipes[recipe] or 0) + 1 end
            end
        end
        summary.entities = top_counts(counts, 20)
        summary.recipes = recipes
        summary.entity_ghosts = surface.count_entities_filtered{force = force, type = "entity-ghost"}
        summary.tile_ghosts = surface.count_entities_filtered{force = force, type = "tile-ghost"}
        summary.foundation_tiles = surface.count_tiles_filtered{name = "space-platform-foundation"}
        summary.ghosts_missing_items = surface_logistics(surface, force, false).missing_items
        local stranded = {}
        local ghosts, why = stranded_ghosts(surface)
        for _, ghost in ipairs(ghosts) do
            if #stranded >= 10 then break end
            stranded[#stranded + 1] = ghost.ghost_name .. "@" .. ghost.position.x .. "," .. ghost.position.y
                .. " (" .. why[ghost.unit_number] .. ")"
        end
        summary.stranded_ghosts = stranded
        local holes = {}
        for _, cell in ipairs(enclosed_space(surface, tile_ghost_keys(surface))) do
            if #holes >= 10 then break end
            holes[#holes + 1] = cell
        end
        summary.foundation_holes = holes
        summary.thrusters = thrusters
        -- The plants making each thruster fluid, and the item ingredients the
        -- hub cannot supply them: a full thruster buffer lasts seconds.
        local hub_inventory = platform.hub and platform.hub.valid and platform.hub.get_inventory(defines.inventory.hub_main)
        for _, input in ipairs(thruster_inputs) do
            input.producers = fluid_producers(surface, force, input.fluid, hub_inventory)
        end
        summary.thrust_short = thrust_shortages(platform)
        summary.thrust_reserve = THRUST_RESERVE
        summary.thrusters_unfed = thruster_inputs
        summary.turrets = turrets
        summary.collectors = collectors
    end
    summary.damaged_tiles = safe_value(function() return #platform.damaged_tiles end)
    summary.speed = safe_value(function() return platform.speed end)
    summary.distance = safe_value(function() return platform.distance end)
    return summary
end

local function landing_pads(surface, force)
    local pads = surface.find_entities_filtered{name = "cargo-landing-pad", force = force}
    local requested, seen = {}, {}
    for _, pad in pairs(pads) do
        local sections = safe_value(pad.get_logistic_sections)
        for _, section in pairs(sections and sections.sections or {}) do
            for _, filter in pairs(section.filters or {}) do
                local value = filter.value
                local name = value and (type(value) == "string" and value or value.name)
                if name and (filter.min or 0) > 0 and not seen[name] then
                    seen[name] = true
                    requested[#requested + 1] = name
                end
            end
        end
    end
    return pads, requested
end

local function tech_done(force, name)
    local tech = force.technologies[name]
    return tech ~= nil and tech.researched == true
end

-- The factory the character looks after: its own surface, except on a space
-- platform or another planet, where home is Nauvis.
function M.home_surface(character)
    local surface = character.surface
    if surface.platform or (surface.planet and surface.name ~= "nauvis") then
        return game.get_surface("nauvis") or surface
    end
    return surface
end

function M.summary(force, character)
    local home = M.home_surface(character)
    local platforms = {}
    for _, platform in ipairs(valid_platforms(force)) do
        if #platforms >= 5 then break end
        platforms[#platforms + 1] = platform_summary(platform)
    end
    local here = character.surface.platform
    local pads, requests = landing_pads(home, force)
    local pad_free_slots = nil
    for _, pad in ipairs(pads) do
        local inventory = pad.get_inventory(defines.inventory.cargo_landing_pad_main)
        if inventory then pad_free_slots = (pad_free_slots or 0) + inventory.count_empty_stacks() end
    end
    local away_planet = (character.surface ~= home and not here) and character.surface or nil
    return {
        character_surface = character.surface.name,
        character_on_platform = here and here.name or nil,
        -- Force entities on the planet the character stands on, when that is
        -- not home: how much it has built there.
        away_planet_entities = away_planet and away_planet.count_entities_filtered{force = force} or nil,
        platforms = platforms,
        home_logistics = surface_logistics(home, force, false),
        landing_pads = #pads,
        landing_pad_requests = requests,
        landing_pad_free_slots = pad_free_slots,
        vulcanus_unlocked = safe_value(function() return force.is_space_location_unlocked("vulcanus") end) == true,
        techs = {
            construction_robotics = tech_done(force, "construction-robotics"),
            space_platform = tech_done(force, "space-platform"),
            space_science_pack = tech_done(force, "space-science-pack"),
            space_platform_thruster = tech_done(force, "space-platform-thruster"),
            planet_discovery_vulcanus = tech_done(force, "planet-discovery-vulcanus"),
            calcite_processing = tech_done(force, "calcite-processing"),
            tungsten_carbide = tech_done(force, "tungsten-carbide"),
            foundry = tech_done(force, "foundry"),
            big_mining_drill = tech_done(force, "big-mining-drill"),
            metallurgic_science_pack = tech_done(force, "metallurgic-science-pack"),
        },
    }
end

function M.robot_logistics(agent_id, surface_name)
    local character = characters.find(agent_id)
    if not character then return no_character(agent_id) end
    local surface, err = resolve_surface(character, surface_name)
    if not surface then return err end
    local result = surface_logistics(surface, character.force, true)
    result.success = true
    result.surface = surface.name
    result.is_platform = surface.platform ~= nil
    if surface.platform then result.platform = platform_summary(surface.platform) end
    return result
end

-- ============================================================
-- place_ghosts
-- ============================================================

local function finite(value)
    return type(value) == "number" and value == value and value ~= math.huge and value ~= -math.huge
end

local function valid_direction(direction)
    return direction == nil or (type(direction) == "number" and direction >= 0 and direction <= 15 and direction % 1 == 0)
end

-- Collision box of a prototype rotated to a cardinal direction.
local function rotated_box(proto, direction)
    local box = proto.collision_box
    local x1, y1, x2, y2 = box.left_top.x, box.left_top.y, box.right_bottom.x, box.right_bottom.y
    if direction == defines.direction.east then return -y2, x1, -y1, x2 end
    if direction == defines.direction.south then return -x2, -y2, -x1, -y1 end
    if direction == defines.direction.west then return y1, -x2, y2, -x1 end
    return x1, y1, x2, y2
end

-- Snap to the grid the engine uses: odd sizes centre on .5, even on whole.
local function snap(value, size)
    if size % 2 == 1 then return math.floor(value) + 0.5 end
    return math.floor(value + 0.5)
end

local function footprint_tiles_planned(area, planned_tiles)
    for tx = math.floor(area[1][1]), math.ceil(area[2][1]) - 1 do
        for ty = math.floor(area[1][2]), math.ceil(area[2][2]) - 1 do
            if not planned_tiles[tx .. "," .. ty] then return false end
        end
    end
    return true
end

-- On a platform, true when part of the footprint is empty space that no
-- planned tile ghost covers: the hub would never build such a ghost.
local function missing_foundation(surface, area, planned_tiles)
    if not surface.platform then return false end
    for tx = math.floor(area[1][1]), math.ceil(area[2][1]) - 1 do
        for ty = math.floor(area[1][2]), math.ceil(area[2][2]) - 1 do
            if not planned_tiles[tx .. "," .. ty] and surface.get_tile(tx, ty).name == "empty-space" then
                return true
            end
        end
    end
    return false
end

function tile_ghost_keys(surface)
    local keys = {}
    for _, ghost in pairs(surface.find_entities_filtered{type = "tile-ghost"}) do
        keys[math.floor(ghost.position.x) .. "," .. math.floor(ghost.position.y)] = true
    end
    return keys
end

-- Empty-space cells enclosed by foundation once the tiles in `extra` (a set
-- of "x,y" keys) are laid too. The hub never lays a foundation tile that
-- would close off such a hole, so ghosts that do stay unbuilt.
function enclosed_space(surface, extra)
    local solid = {}
    local x1, y1, x2, y2 = math.huge, math.huge, -math.huge, -math.huge
    local function add(x, y)
        solid[x .. "," .. y] = true
        if x < x1 then x1 = x end
        if y < y1 then y1 = y end
        if x > x2 then x2 = x end
        if y > y2 then y2 = y end
    end
    for _, tile in pairs(surface.find_tiles_filtered{name = "space-platform-foundation"}) do
        add(tile.position.x, tile.position.y)
    end
    for key in pairs(extra) do
        local x, y = key:match("^(-?%d+),(-?%d+)$")
        if x then add(tonumber(x), tonumber(y)) end
    end
    if x1 == math.huge then return {} end
    x1, y1, x2, y2 = x1 - 1, y1 - 1, x2 + 1, y2 + 1
    -- Flood the open space from the bounding box edge; what it misses is enclosed.
    local outside, queue = {}, {}
    local function visit(x, y)
        local key = x .. "," .. y
        if x < x1 or x > x2 or y < y1 or y > y2 or solid[key] or outside[key] then return end
        outside[key] = true
        queue[#queue + 1] = {x, y}
    end
    for x = x1, x2 do visit(x, y1) visit(x, y2) end
    for y = y1, y2 do visit(x1, y) visit(x2, y) end
    local head = 1
    while queue[head] do
        local cell = queue[head]
        head = head + 1
        visit(cell[1] + 1, cell[2]) visit(cell[1] - 1, cell[2])
        visit(cell[1], cell[2] + 1) visit(cell[1], cell[2] - 1)
    end
    local holes = {}
    for x = x1, x2 do
        for y = y1, y2 do
            local key = x .. "," .. y
            if not solid[key] and not outside[key] then holes[#holes + 1] = {x = x, y = y} end
        end
    end
    return holes
end

-- Entity ghosts on a platform the hub never builds, yet which block
-- placement: partly over empty space with no tile ghost under them, or
-- overlapping a built entity that is neither marked for deconstruction nor
-- fast-replaceable by the ghost. Returns the ghosts and, by unit number, why.
function stranded_ghosts(surface)
    local stranded, why = {}, {}
    if not surface.platform then return stranded, why end
    local keys = tile_ghost_keys(surface)
    for _, ghost in pairs(surface.find_entities_filtered{type = "entity-ghost"}) do
        local box = ghost.bounding_box
        if missing_foundation(surface, {{box.left_top.x, box.left_top.y}, {box.right_bottom.x, box.right_bottom.y}}, keys) then
            stranded[#stranded + 1] = ghost
            why[ghost.unit_number] = "over empty space"
        else
            local group = ghost.ghost_prototype.fast_replaceable_group
            local inner = {{box.left_top.x + 0.05, box.left_top.y + 0.05}, {box.right_bottom.x - 0.05, box.right_bottom.y - 0.05}}
            for _, found in pairs(surface.find_entities_filtered{area = inner}) do
                if found.type ~= "entity-ghost" and found.prototype.has_flag("player-creation")
                    and not found.to_be_deconstructed()
                    and not (group and found.prototype.fast_replaceable_group == group)
                then
                    stranded[#stranded + 1] = ghost
                    why[ghost.unit_number] = "on a built " .. found.name
                    break
                end
            end
        end
    end
    return stranded, why
end

-- Planned platform foundation tiles that do not join the platform: the hub
-- only lays foundation touching existing foundation, diagonals included (or
-- connected tile ghosts).
local NEIGHBOURS = {{1, 0}, {-1, 0}, {0, 1}, {0, -1}, {1, 1}, {1, -1}, {-1, 1}, {-1, -1}}
local function disconnected_tiles(surface, new_tiles)
    local pending, joined, queue = {}, {}, {}
    for _, plan in ipairs(new_tiles) do pending[plan.x .. "," .. plan.y] = plan end
    local ghosts = tile_ghost_keys(surface)
    local function solid(x, y)
        local key = x .. "," .. y
        return joined[key] or ghosts[key] or surface.get_tile(x, y).name ~= "empty-space"
    end
    for _, plan in ipairs(new_tiles) do
        for _, d in ipairs(NEIGHBOURS) do
            if solid(plan.x + d[1], plan.y + d[2]) then
                local key = plan.x .. "," .. plan.y
                if not joined[key] then joined[key] = true queue[#queue + 1] = plan end
                break
            end
        end
    end
    local head = 1
    while queue[head] do
        local plan = queue[head]
        head = head + 1
        for _, d in ipairs(NEIGHBOURS) do
            local key = (plan.x + d[1]) .. "," .. (plan.y + d[2])
            if pending[key] and not joined[key] then
                joined[key] = true
                queue[#queue + 1] = pending[key]
            end
        end
    end
    local out = {}
    for _, plan in ipairs(new_tiles) do
        if not joined[plan.x .. "," .. plan.y] and #out < 10 then
            out[#out + 1] = {index = plan.index, x = plan.x, y = plan.y}
        end
    end
    return out
end

-- The closest snapped position within 12 tiles where `plan` could be placed
-- as a ghost, as dx/dy from the requested spot, or nil.
local function nearest_free(surface, force, plan)
    local width, height = plan.proto.tile_width, plan.proto.tile_height
    if plan.direction == defines.direction.east or plan.direction == defines.direction.west then
        width, height = height, width
    end
    for radius = 1, 12 do
        for dx = -radius, radius do
            for dy = -radius, radius do
                if math.max(math.abs(dx), math.abs(dy)) == radius then
                    local position = {
                        x = snap(plan.position.x + dx, width),
                        y = snap(plan.position.y + dy, height),
                    }
                    if surface.can_place_entity{
                        name = plan.name,
                        position = position,
                        direction = plan.direction,
                        force = force,
                        build_check_type = defines.build_check_type.manual_ghost,
                        forced = true,
                    } and not missing_foundation(surface, (function()
                        local x1, y1, x2, y2 = rotated_box(plan.proto, plan.direction)
                        return {{position.x + x1, position.y + y1}, {position.x + x2, position.y + y2}}
                    end)(), {}) then
                        return {
                            position = position,
                            dx = position.x - plan.position.x,
                            dy = position.y - plan.position.y,
                        }
                    end
                end
            end
        end
    end
    return nil
end

local function add_needed(needed, items)
    local item = first_item(items)
    if item then needed[item.name] = (needed[item.name] or 0) + item.count end
end

function M.place_ghosts(agent_id, surface_name, origin_x, origin_y, entities, tiles, dry_run)
    local character = characters.find(agent_id)
    if not character then return no_character(agent_id) end
    local surface, err = resolve_surface(character, surface_name)
    if not surface then return err end
    local force = character.force
    if not (finite(origin_x) and finite(origin_y)) then
        return fail("invalid_layout", "origin_x and origin_y must be finite numbers")
    end
    entities = type(entities) == "table" and entities or {}
    tiles = type(tiles) == "table" and tiles or {}
    if #entities == 0 and #tiles == 0 then
        return fail("invalid_layout", "give at least one entity or tile")
    end
    if #entities > MAX_PLACE_ENTITIES or #tiles > MAX_PLACE_TILES then
        return fail("invalid_layout", "at most " .. MAX_PLACE_ENTITIES .. " entities and " .. MAX_PLACE_TILES .. " tiles per call")
    end

    -- Validate and resolve positions.
    local planned_entities, planned_tiles, tile_keys = {}, {}, {}
    for index, spec in ipairs(entities) do
        local proto = type(spec) == "table" and type(spec.name) == "string" and prototypes.entity[spec.name] or nil
        if not (proto and first_item(proto.items_to_place_this)) then
            return fail("invalid_entity", "entity " .. index .. " is not a placeable entity", {index = index, name = type(spec) == "table" and spec.name or nil})
        end
        if not (finite(spec.dx) and finite(spec.dy) and valid_direction(spec.direction)) then
            return fail("invalid_layout", "entity " .. index .. " needs finite dx/dy and a direction 0-15", {index = index})
        end
        local direction = spec.direction or defines.direction.north
        local width, height = proto.tile_width, proto.tile_height
        if direction == defines.direction.east or direction == defines.direction.west then
            width, height = height, width
        end
        local position = {
            x = snap(origin_x + spec.dx, width),
            y = snap(origin_y + spec.dy, height),
        }
        local x1, y1, x2, y2 = rotated_box(proto, direction)
        planned_entities[#planned_entities + 1] = {
            index = index,
            name = spec.name,
            proto = proto,
            direction = direction,
            position = position,
            area = {{position.x + x1, position.y + y1}, {position.x + x2, position.y + y2}},
            recipe = type(spec.recipe) == "string" and spec.recipe ~= "" and spec.recipe or nil,
        }
    end
    for index, spec in ipairs(tiles) do
        local proto = type(spec) == "table" and type(spec.name) == "string" and prototypes.tile[spec.name] or nil
        if not (proto and first_item(proto.items_to_place_this)) then
            return fail("invalid_tile", "tile " .. index .. " is not a placeable tile", {index = index, name = type(spec) == "table" and spec.name or nil})
        end
        if not (finite(spec.dx) and finite(spec.dy)) then
            return fail("invalid_layout", "tile " .. index .. " needs finite dx/dy", {index = index})
        end
        local tx = math.floor(origin_x) + math.floor(spec.dx)
        local ty = math.floor(origin_y) + math.floor(spec.dy)
        tile_keys[tx .. "," .. ty] = true
        planned_tiles[#planned_tiles + 1] = {index = index, name = spec.name, proto = proto, x = tx, y = ty}
    end

    -- Robots only build inside construction range; platform hubs build anywhere.
    if not surface.platform then
        local cells = construction_cells(surface, force)
        local uncovered = {}
        local function check(index, kind, position)
            if #uncovered < 10 and not covered(cells, position) then
                uncovered[#uncovered + 1] = {index = index, kind = kind, position = position}
            end
        end
        for _, plan in ipairs(planned_entities) do check(plan.index, "entity", plan.position) end
        for _, plan in ipairs(planned_tiles) do check(plan.index, "tile", {x = plan.x + 0.5, y = plan.y + 0.5}) end
        if #uncovered > 0 then
            return fail("no_construction_coverage", "some positions are outside every roboport's construction range", {
                uncovered = uncovered,
                roboports = #cells,
                guidance = "Robots only build inside a roboport's construction range: extend the roboport network there first (build_layout on your own surface), or use build_layout.",
            })
        end
    end

    -- Ghosts the hub can never build (see stranded_ghosts) may be replaced by
    -- this call; they are removed only when it executes.
    local stranded, clear, clear_keys = {}, {}, {}
    for _, ghost in ipairs(stranded_ghosts(surface)) do stranded[ghost.unit_number] = true end

    -- Preflight: nothing is placed until every position passes.
    local new_entities, new_tiles, already_built = {}, {}, {}
    local obstacles, obstacle_keys = {}, {}
    local needed = {}
    for _, plan in ipairs(planned_tiles) do
        local existing = surface.get_tile(plan.x, plan.y)
        local ghost_here = surface.count_entities_filtered{
            type = "tile-ghost",
            area = {{plan.x + 0.1, plan.y + 0.1}, {plan.x + 0.9, plan.y + 0.9}},
        } > 0
        if (existing and existing.valid and existing.name == plan.name) or ghost_here then
            already_built[#already_built + 1] = {kind = "tile", index = plan.index}
        else
            new_tiles[#new_tiles + 1] = plan
            add_needed(needed, plan.proto.items_to_place_this)
        end
    end
    if surface.platform and planned_tiles[1] and planned_tiles[1].name == "space-platform-foundation" then
        local loose = disconnected_tiles(surface, new_tiles)
        if #loose > 0 then
            return fail("disconnected_foundation", #loose .. " foundation tile(s) do not join the platform; nothing was placed", {
                tiles = loose,
                guidance = "The hub only lays space-platform-foundation touching existing foundation (diagonals count): add the tiles that connect these to the platform edge in the same call.",
            })
        end
        local ghost_keys = tile_ghost_keys(surface)
        local before = {}
        for _, cell in ipairs(enclosed_space(surface, ghost_keys)) do before[cell.x .. "," .. cell.y] = true end
        local planned = {}
        for key in pairs(ghost_keys) do planned[key] = true end
        for _, plan in ipairs(new_tiles) do planned[plan.x .. "," .. plan.y] = true end
        local holes = {}
        for _, cell in ipairs(enclosed_space(surface, planned)) do
            if not before[cell.x .. "," .. cell.y] and #holes < 20 then holes[#holes + 1] = cell end
        end
        if #holes > 0 then
            return fail("encloses_space", "these foundation tiles would close off " .. #holes .. " empty-space cell(s); nothing was placed", {
                holes = holes,
                guidance = "The hub never lays a tile that closes off empty space inside the platform, so such tiles stay ghosts forever. Add foundation tiles on the listed holes too (dx = x - origin_x, dy = y - origin_y), or leave a gap to open space.",
            })
        end
    end
    for _, plan in ipairs(planned_entities) do
        local built = false
        for _, found in pairs(surface.find_entities_filtered{position = plan.position, radius = 1, force = force}) do
            local same = found.name == plan.name or (found.type == "entity-ghost" and found.ghost_name == plan.name)
            local dx, dy = found.position.x - plan.position.x, found.position.y - plan.position.y
            if same and dx * dx + dy * dy <= 0.51 * 0.51 and not stranded[found.unit_number] then built = true break end
        end
        if built then
            already_built[#already_built + 1] = {kind = "entity", index = plan.index}
        else
            for _, found in pairs(surface.find_entities_filtered{area = plan.area, type = "entity-ghost"}) do
                if stranded[found.unit_number] and not clear_keys[found.unit_number] then
                    clear_keys[found.unit_number] = true
                    clear[#clear + 1] = found
                end
            end
            if not footprint_tiles_planned(plan.area, tile_keys) then
                local plan_obstacles = surface.find_entities_filtered{area = plan.area, type = {"tree", "simple-entity"}}
                for _, obstacle in pairs(plan_obstacles) do
                    local key = obstacle.name .. "@" .. obstacle.position.x .. "," .. obstacle.position.y
                    if not obstacle_keys[key] and not obstacle.to_be_deconstructed() then
                        obstacle_keys[key] = true
                        obstacles[#obstacles + 1] = obstacle
                    end
                end
                local placeable = surface.can_place_entity{
                    name = plan.name,
                    position = plan.position,
                    direction = plan.direction,
                    force = force,
                    build_check_type = defines.build_check_type.manual_ghost,
                    forced = true,
                } and not missing_foundation(surface, plan.area, tile_keys)
                if not placeable then
                    local blockers, removable_only, stranded_here = {}, true, {}
                    for _, found in pairs(surface.find_entities_filtered{area = plan.area}) do
                        if found.type == "entity-ghost" and stranded[found.unit_number] then
                            stranded_here[#stranded_here + 1] = found
                        elseif not OBSTACLE_TYPES[found.type] and found.type ~= "character" then
                            removable_only = false
                            blockers[#blockers + 1] = found.type == "entity-ghost" and ("ghost of " .. found.ghost_name) or found.name
                        end
                    end
                    if not (removable_only and #stranded_here > 0 and not missing_foundation(surface, plan.area, tile_keys))
                        and (not removable_only or #plan_obstacles == 0) then
                        local free = nearest_free(surface, force, plan)
                        local message = "entity " .. plan.index .. " (" .. plan.name .. ") cannot be placed there ("
                            .. (#blockers > 0 and ("blocked by " .. table.concat(blockers, ", ")) or "no foundation or wrong ground")
                            .. "); nothing was placed"
                        if surface.platform and #blockers == 0 then
                            message = message .. "; to keep this position, add space-platform-foundation tiles under it in the same call (tiles)"
                        end
                        if free then
                            message = message .. "; nearest free spot is " .. free.dx .. "," .. free.dy .. " tiles away at "
                                .. free.position.x .. "," .. free.position.y
                        elseif surface.platform and #blockers > 0 then
                            message = message .. "; no free foundation fits it within 12 tiles: add space-platform-foundation tiles for it in the same call"
                        end
                        return fail("placement_blocked", message, {
                            index = plan.index,
                            position = plan.position,
                            blockers = blockers,
                            hint = #blockers == 0 and "the ground itself blocks it (water, missing foundation, or wrong surface for this entity)" or nil,
                            nearest_free = free,
                        })
                    end
                end
            end
            new_entities[#new_entities + 1] = plan
            add_needed(needed, plan.proto.items_to_place_this)
        end
    end

    local missing_items = shortfalls(needed, surface, force)
    if dry_run then
        local would_place = {}
        for _, plan in ipairs(new_entities) do
            would_place[#would_place + 1] = {index = plan.index, name = plan.name, position = plan.position}
        end
        return {
            success = true,
            dry_run = true,
            surface = surface.name,
            would_place = would_place,
            would_place_tiles = #new_tiles,
            already_built = already_built,
            obstacles = #obstacles,
            missing_items = missing_items,
            would_remove_stranded_ghosts = #clear,
        }
    end

    -- Execute; any failure removes everything this call created.
    local created, marked, removed_stranded = {}, {}, {}
    local function rollback()
        for _, ghost in ipairs(created) do
            if ghost.valid then ghost.destroy() end
        end
        for _, obstacle in ipairs(marked) do
            if obstacle.valid then obstacle.cancel_deconstruction(force) end
        end
        for _, old in ipairs(removed_stranded) do
            surface.create_entity{name = "entity-ghost", inner_name = old.name, position = old.position, direction = old.direction, force = force}
        end
    end
    for _, obstacle in ipairs(obstacles) do
        if obstacle.valid and obstacle.order_deconstruction(force) then marked[#marked + 1] = obstacle end
    end
    for _, ghost in ipairs(clear) do
        if ghost.valid then
            removed_stranded[#removed_stranded + 1] = {name = ghost.ghost_name, position = ghost.position, direction = ghost.direction}
            ghost.destroy()
        end
    end
    for _, plan in ipairs(new_tiles) do
        local ghost = surface.create_entity{
            name = "tile-ghost",
            inner_name = plan.name,
            position = {x = plan.x + 0.5, y = plan.y + 0.5},
            force = force,
        }
        if not ghost then
            rollback()
            return fail("ghost_failed", "tile ghost " .. plan.index .. " could not be created; nothing was placed", {index = plan.index, kind = "tile"})
        end
        created[#created + 1] = ghost
    end
    local recipes, placed, recipe_failed, ammo_requested = {}, 0, false, 0
    for _, plan in ipairs(new_entities) do
        local ghost = surface.create_entity{
            name = "entity-ghost",
            inner_name = plan.name,
            position = plan.position,
            direction = plan.direction,
            force = force,
        }
        if not ghost then
            rollback()
            return fail("ghost_failed", "entity ghost " .. plan.index .. " could not be created; nothing was placed", {index = plan.index, kind = "entity"})
        end
        created[#created + 1] = ghost
        placed = placed + 1
        if plan.recipe then
            local ok = pcall(ghost.set_recipe, plan.recipe)
            if not ok then recipe_failed = true end
            recipes[#recipes + 1] = {index = plan.index, recipe = plan.recipe, recipe_set = ok}
        end
        if surface.platform and ghost.ghost_type == "ammo-turret" then
            ammo_requested = ammo_requested + request_turret_ammo(surface.platform, ghost)
            storage.turret_ammo_orders = storage.turret_ammo_orders or {}
            storage.turret_ammo_orders[surface.platform.index] = true
        end
    end

    local guidance = surface.platform
        and ("The hub builds these from its inventory; bring missing_items with space_platform action=ship."
            .. (ammo_requested > 0 and " It also loads the new turrets with " .. ammo_requested .. " magazines from the hub." or ""))
        or (surface ~= character.surface
            and "Robots build these from items in the network; you are on another surface, so if missing_items is not empty, have this planet make them: place_ghosts an assembler on each item (fed by inserters) with an inserter into a passive-provider-chest in range. Check progress with robot_logistics."
            or "Robots build these from items in the network; put missing_items into a storage or passive-provider chest in range. Check progress with robot_logistics.")
    if recipe_failed then
        guidance = guidance .. " Recipes with recipe_set=false were not applied: set them with set_recipe once the machine is built and you are there."
    end
    return {
        success = true,
        surface = surface.name,
        placed = placed,
        placed_tiles = #new_tiles,
        already_built = already_built,
        obstacles_marked = #marked,
        recipes = recipes,
        missing_items = missing_items,
        removed_stranded_ghosts = removed_stranded,
        ammo_requested = ammo_requested,
        guidance = guidance,
    }
end

-- ============================================================
-- space_platform
-- ============================================================

local ACTIONS = {status = true, create = true, ship = true, unship = true, request = true, jettison = true, clear_ghosts = true, load_turrets = true, schedule = true, board = true, land = true}

local function resolve_platform(character, platform_name)
    local force = character.force
    if type(platform_name) == "string" and platform_name ~= "" then
        for _, platform in ipairs(valid_platforms(force)) do
            if platform.name == platform_name then return platform end
        end
        return nil, fail("unknown_platform", "no platform named " .. platform_name, {platforms = platform_names(force)})
    end
    local here = character.surface.platform
    if here then return here end
    local all = valid_platforms(force)
    if #all == 1 then return all[1] end
    return nil, fail("platform_required", #all == 0
        and "the force has no space platform yet; create one with action=create"
        or "several platforms exist; name one with platform", {platforms = platform_names(force)})
end

-- Validate {name, count} entries and merge duplicates.
local function normalize_items(items)
    if type(items) ~= "table" or #items == 0 then
        return nil, fail("invalid_items", "items must list at least one {name, count}")
    end
    local merged, order = {}, {}
    for index, item in ipairs(items) do
        local name = type(item) == "table" and item.name or nil
        local count = type(item) == "table" and item.count or nil
        if type(name) ~= "string" or not prototypes.item[name] then
            return nil, fail("invalid_items", "item " .. index .. " is not a known item", {index = index, name = name})
        end
        if type(count) ~= "number" or count < 1 or count % 1 ~= 0 then
            return nil, fail("invalid_items", "item " .. index .. " needs a whole count of at least 1", {index = index})
        end
        if not merged[name] then order[#order + 1] = name end
        merged[name] = (merged[name] or 0) + count
    end
    local list = {}
    for _, name in ipairs(order) do list[#list + 1] = {name = name, count = merged[name]} end
    return list
end

local function silo_summaries(silos)
    local status_names = {}
    for name, value in pairs(defines.rocket_silo_status) do status_names[value] = name end
    local list = {}
    for _, silo in pairs(silos) do
        list[#list + 1] = {
            unit_number = silo.unit_number,
            position = pos_table(silo.position),
            status = status_names[silo.rocket_silo_status] or "unknown",
            rocket_parts = silo.rocket_parts,
        }
    end
    return list
end

-- The nearest silo with a ready rocket, beside a platform above this planet.
local function pick_silo(character, platform)
    local planet = character.surface.planet
    if not planet then
        return nil, fail("not_on_planet", "rockets leave from a planet; you are on " .. character.surface.name)
    end
    local location = platform.space_location
    if not (location and location.name == planet.name) then
        return nil, fail("platform_not_here", platform.name .. " is not above " .. planet.name, {
            platform_location = location and location.name or nil,
            planet = planet.name,
        })
    end
    local silos = character.surface.find_entities_filtered{type = "rocket-silo", force = character.force}
    local best, best_distance = nil, nil
    for _, silo in pairs(silos) do
        if silo.rocket_silo_status == defines.rocket_silo_status.rocket_ready then
            local dx, dy = silo.position.x - character.position.x, silo.position.y - character.position.y
            local distance = dx * dx + dy * dy
            if not best or distance < best_distance then best, best_distance = silo, distance end
        end
    end
    if not best then
        return nil, fail(#silos == 0 and "no_rocket_silo" or "rocket_not_ready",
            #silos == 0 and "the force has no rocket silo on this surface"
            or "no silo has a ready rocket; a rocket needs the silo's full rocket_parts and power, then about 20 s to rise",
            {
                silos = silo_summaries(silos),
                parts_required = silos[1] and silos[1].prototype.rocket_parts_required or nil,
                guidance = #silos > 0 and "Boarding needs a ready rocket with empty cargo. Do other work and board once status shows a silo rocket_ready; do not retry every few seconds." or nil,
            })
    end
    local reach = characters.require_entity_reach(character, best)
    if reach then
        reach.error_kind = "out_of_reach"
        reach.silo_unit_number = best.unit_number
        return nil, reach
    end
    return best
end


local function action_status(character)
    local platforms = {}
    for _, platform in ipairs(valid_platforms(character.force)) do
        platforms[#platforms + 1] = platform_summary(platform)
    end
    local here = character.surface.platform
    return {
        success = true,
        character_surface = character.surface.name,
        character_on_platform = here and here.name or nil,
        platforms = platforms,
    }
end

local function action_create(character, platform_name)
    local force = character.force
    if not tech_done(force, "rocket-silo") then
        return fail("rocket_silo_not_researched", "research rocket-silo first")
    end
    local planet = character.surface.planet
    if not planet then
        return fail("not_on_planet", "create a platform from a planet surface; you are on " .. character.surface.name)
    end
    local name = (type(platform_name) == "string" and platform_name ~= "") and platform_name
        or ("buddy-" .. (#valid_platforms(force) + 1))
    for _, platform in ipairs(valid_platforms(force)) do
        if platform.name == name then
            return fail("name_taken", "a platform named " .. name .. " already exists", {platforms = platform_names(force)})
        end
    end
    local platform = force.create_space_platform{
        name = name,
        planet = planet.name,
        starter_pack = "space-platform-starter-pack",
    }
    if not platform then return fail("create_failed", "the engine refused to create the platform") end
    local result = platform_summary(platform)
    result.success = true
    result.guidance = "Ship its starter pack: space_platform action=ship items=[{name:'space-platform-starter-pack',count:1}] beside a silo; it leaves with the next ready rocket."
    return result
end

-- Shipments wait in a script inventory and ride the next ready rockets:
-- each ready silo on the planet takes as much as one rocket lifts, and the
-- rest waits for the next rocket. The model never has to poll the silo.
local function shipments()
    storage.space_shipments = storage.space_shipments or {}
    return storage.space_shipments
end

local function inventory_counts(inventory)
    local counts = {}
    if inventory and inventory.valid then
        for _, entry in pairs(inventory.get_contents()) do
            counts[entry.name] = (counts[entry.name] or 0) + entry.count
        end
    end
    return counts
end

-- Hand undeliverable cargo back to its owner, or drop it by the silo.
local function return_shipment(shipment, surface, position)
    local character = characters.find(shipment.agent_id)
    for _, entry in pairs(shipment.inventory.get_contents()) do
        local left = entry.count
        if character then left = left - character.insert{name = entry.name, count = entry.count, quality = entry.quality} end
        if left > 0 and surface and position then
            surface.spill_item_stack{position = position, stack = {name = entry.name, count = left, quality = entry.quality}}
        end
    end
    shipment.inventory.destroy()
end

-- Load one ready rocket from `shipment` and launch it. True when launched.
local function launch_shipment(shipment, silo)
    local platform = shipment.platform
    local waiting = platform.state == defines.space_platform_state.waiting_for_starter_pack
    if not waiting and not (platform.hub and platform.hub.valid) then return false end
    local rocket_inventory = silo.get_inventory(defines.inventory.rocket_silo_rocket)
    if not rocket_inventory then return false end
    -- A full hub destroys delivered cargo: send only what it can take now
    -- and keep the rest waiting.
    local hub_inventory = not waiting and platform.hub.get_inventory(defines.inventory.hub_main) or nil
    local moved = {}
    shipment.blocked = nil
    local free_slots = hub_inventory and hub_inventory.count_empty_stacks() or 0
    -- Items the platform's ghosts are waiting for ride first.
    local entries = shipment.inventory.get_contents()
    if not waiting and platform.surface then
        local wanted = {}
        for _, item in ipairs(surface_logistics(platform.surface, platform.force, false).missing_items) do
            wanted[item.name] = true
        end
        table.sort(entries, function(a, b)
            local wa, wb = wanted[a.name] and 0 or 1, wanted[b.name] and 0 or 1
            if wa ~= wb then return wa < wb end
            return a.name < b.name
        end)
    end
    for _, entry in ipairs(entries) do
        local count = entry.count
        if hub_inventory then
            -- Room in partly filled stacks, then whole free slots shared by
            -- every item in this rocket.
            local stack = prototypes.item[entry.name].stack_size
            local insertable = hub_inventory.get_insertable_count{name = entry.name, quality = entry.quality}
            local partial = math.max(0, insertable - hub_inventory.count_empty_stacks() * stack)
            count = math.min(count, partial + free_slots * stack)
            free_slots = free_slots - math.ceil(math.max(0, count - partial) / stack)
        end
        local inserted = count > 0 and rocket_inventory.insert{name = entry.name, count = count, quality = entry.quality} or 0
        if inserted > 0 then
            shipment.inventory.remove{name = entry.name, count = inserted, quality = entry.quality}
            moved[#moved + 1] = {name = entry.name, count = inserted, quality = entry.quality}
        end
    end
    if #moved == 0 then
        if hub_inventory then shipment.blocked = "hub_full" end
        return false
    end
    local destination = waiting
        and {type = defines.cargo_destination.space_platform, space_platform = platform}
        or {type = defines.cargo_destination.station, station = platform.hub}
    local ok, launched = pcall(silo.launch_rocket, destination)
    if not (ok and launched) then
        for _, item in ipairs(moved) do
            local taken = rocket_inventory.remove(item)
            if taken > 0 then shipment.inventory.insert{name = item.name, count = taken, quality = item.quality} end
        end
        return false
    end
    shipment.rockets = (shipment.rockets or 0) + 1
    return true
end

-- A booking keeps the next ready rocket on its surface empty for the agent
-- this long, so queued cargo cannot take every rocket from under it.
local BOOKING_TICKS = 10 * 60 * 60

local function launch_character(agent_id, character, platform, silo)
    if storage.walk_targets and storage.walk_targets[agent_id] then
        characters.finish_walk(agent_id, character, "boarded")
    end
    local ok, launched = pcall(silo.launch_rocket, {type = defines.cargo_destination.station, station = platform.hub}, character)
    if not (ok and launched) then return false, ok and "the silo refused to launch" or tostring(launched) end
    return true
end

-- Launch booked agents from the first ready, empty rocket in their reach.
-- Returns the surfaces where a booking still waits for a rocket.
local function serve_bookings()
    local waiting = {}
    storage.board_bookings = storage.board_bookings or {}
    for agent_id, booking in pairs(storage.board_bookings) do
        local character = characters.find(agent_id)
        local platform = booking.platform
        if not (character and character.valid and platform.valid and platform.hub and platform.hub.valid)
            or game.tick - booking.tick > BOOKING_TICKS or character.surface.index ~= booking.surface_index
        then
            storage.board_bookings[agent_id] = nil
        else
            local launched = false
            local location = platform.space_location
            if location and character.surface.planet and location.name == character.surface.planet.name then
                for _, silo in pairs(character.surface.find_entities_filtered{type = "rocket-silo", force = character.force}) do
                    local cargo = silo.get_inventory(defines.inventory.rocket_silo_rocket)
                    if silo.rocket_silo_status == defines.rocket_silo_status.rocket_ready and (not cargo or cargo.is_empty())
                        and not characters.require_entity_reach(character, silo)
                    then
                        launched = launch_character(agent_id, character, platform, silo)
                        break
                    end
                end
            end
            if launched then
                storage.board_bookings[agent_id] = nil
            else
                waiting[booking.surface_index] = true
            end
        end
    end
    return waiting
end

-- Called every second: launch booked agents, then send pending cargo with
-- each ready rocket on surfaces where no booking waits.
function M.process_shipments()
    local booked = serve_bookings()
    local list = storage.space_shipments
    if not list or #list == 0 then return end
    for index = #list, 1, -1 do
        local shipment = list[index]
        local surface = game.get_surface(shipment.surface_index)
        if not (shipment.inventory and shipment.inventory.valid) then
            table.remove(list, index)
        elseif shipment.inventory.is_empty() then
            shipment.inventory.destroy()
            table.remove(list, index)
        elseif not (shipment.platform and shipment.platform.valid and surface) then
            return_shipment(shipment, surface, shipment.position)
            table.remove(list, index)
        elseif not booked[shipment.surface_index] then
            local location = shipment.platform.space_location
            if location and surface.planet and location.name == surface.planet.name then
                for _, silo in pairs(surface.find_entities_filtered{type = "rocket-silo", force = shipment.force}) do
                    if silo.rocket_silo_status == defines.rocket_silo_status.rocket_ready
                        and launch_shipment(shipment, silo)
                    then
                        break
                    end
                end
            end
        end
    end
end

local function action_ship(agent_id, character, platform, items)
    local list, err = normalize_items(items)
    if not list then return err end
    local waiting = platform.state == defines.space_platform_state.waiting_for_starter_pack
    if waiting then
        if not (#list == 1 and list[1].name == "space-platform-starter-pack" and list[1].count == 1) then
            return fail("needs_starter_pack", platform.name .. " is waiting for its starter pack; ship exactly one space-platform-starter-pack first")
        end
    elseif not (platform.hub and platform.hub.valid) then
        return fail("platform_not_ready", platform.name .. " has no hub yet (" .. state_name(platform.state) .. "); wait for the starter pack to land")
    end
    if not waiting then
        local hub_inventory = platform.hub.get_inventory(defines.inventory.hub_main)
        local room = false
        for _, item in ipairs(list) do
            if hub_inventory.get_insertable_count(item.name) > 0 then room = true break end
        end
        if not room then
            return fail("hub_full", platform.name .. "'s hub has no room for this cargo; a full hub destroys what rockets deliver, and its collectors and crushers stop", {
                hub_slots = #hub_inventory,
                hub_items = top_counts(contents_counts(hub_inventory.get_contents()), 15),
                guidance = "Free hub slots first: jettison what the platform cannot use soon (action=jettison: uncrushed asteroid chunks, surplus iron-ore or ice), or add cargo-bay ghosts touching the hub (each adds hub slots) with place_ghosts.",
            })
        end
    end
    local main = character.get_main_inventory()
    local missing = {}
    local slots = 0
    for _, item in ipairs(list) do
        local have = main.get_item_count(item.name)
        if have < item.count then missing[#missing + 1] = {name = item.name, needed = item.count, have = have} end
        slots = slots + math.ceil(item.count / prototypes.item[item.name].stack_size)
    end
    if #missing > 0 then
        return fail("missing_items", "your inventory does not hold everything to ship", {missing = missing})
    end
    local planet = character.surface.planet
    if not planet then
        return fail("not_on_planet", "rockets leave from a planet; you are on " .. character.surface.name)
    end
    local location = platform.space_location
    if not (location and location.name == planet.name) then
        return fail("platform_not_here", platform.name .. " is not above " .. planet.name, {
            platform_location = location and location.name or nil,
            planet = planet.name,
        })
    end
    -- Hand the cargo to the nearest silo: it must be in reach.
    local silo, best = nil, nil
    for _, candidate in pairs(character.surface.find_entities_filtered{type = "rocket-silo", force = character.force}) do
        local dx, dy = candidate.position.x - character.position.x, candidate.position.y - character.position.y
        if not best or dx * dx + dy * dy < best then silo, best = candidate, dx * dx + dy * dy end
    end
    if not silo then return fail("no_rocket_silo", "the force has no rocket silo on this surface") end
    local reach = characters.require_entity_reach(character, silo)
    if reach then
        reach.error_kind = "out_of_reach"
        reach.silo_unit_number = silo.unit_number
        return reach
    end
    -- One queue per platform and planet: new cargo joins cargo already
    -- waiting, so a later ship never has to wait behind a separate rocket.
    local queue = shipments()
    local shipment = nil
    for _, pending in ipairs(queue) do
        if pending.platform == platform and pending.surface_index == character.surface.index
            and pending.inventory and pending.inventory.valid
        then
            shipment = pending
            break
        end
    end
    local inventory
    if shipment then
        inventory = shipment.inventory
        inventory.resize(#inventory + math.max(1, slots))
    else
        inventory = game.create_inventory(math.max(1, slots))
        storage.space_shipment_sequence = (storage.space_shipment_sequence or 0) + 1
        shipment = {
            id = storage.space_shipment_sequence,
            agent_id = agent_id,
            force = character.force,
            platform = platform,
            platform_name = platform.name,
            surface_index = character.surface.index,
            position = pos_table(silo.position),
            inventory = inventory,
            rockets = 0,
        }
        queue[#queue + 1] = shipment
    end
    for _, item in ipairs(list) do
        local removed = main.remove{name = item.name, count = item.count}
        if removed > 0 then inventory.insert{name = item.name, count = removed} end
    end
    local rockets_before = shipment.rockets
    M.process_shipments()
    local left = inventory.valid and inventory_counts(inventory) or {}
    local rockets_left = next(left) and rockets_for(left) or 0
    return {
        success = true,
        platform = platform.name,
        shipment_id = shipment.id,
        shipped = list,
        rockets_launched_now = shipment.rockets - rockets_before,
        rockets_still_needed = rockets_left,
        silos = silo_summaries(character.surface.find_entities_filtered{type = "rocket-silo", force = character.force}),
        guidance = rockets_left > 0
            and "Handed to the silo: the cargo leaves by itself with each ready rocket (one rocket lifts 1 t). Do other work; space_platform status shows cargo still waiting as queued_cargo."
            or "Launched: it arrives in the hub in about 30 s.",
    }
end

-- Take queued cargo for `platform` back from the silo into your inventory.
-- With no items, everything queued for it comes back.
local function action_unship(character, platform, items)
    local wanted = nil
    if type(items) == "table" and #items > 0 then
        local list, err = normalize_items(items)
        if not list then return err end
        wanted = {}
        for _, item in ipairs(list) do wanted[item.name] = (wanted[item.name] or 0) + item.count end
    end
    local main = character.get_main_inventory()
    local returned, found = {}, false
    for _, shipment in ipairs(shipments()) do
        if shipment.platform == platform and shipment.inventory and shipment.inventory.valid then
            found = true
            local silo = character.surface.index == shipment.surface_index
                and character.surface.find_entities_filtered{type = "rocket-silo", position = shipment.position, radius = 1}[1]
            local reach = silo and characters.require_entity_reach(character, silo)
            if not silo or reach then
                return fail("out_of_reach", "stand by the silo holding the cargo to take it back", {
                    silo_unit_number = silo and silo.unit_number or nil,
                    silo_position = shipment.position,
                })
            end
            for _, entry in pairs(shipment.inventory.get_contents()) do
                local count = entry.count
                if wanted then count = math.min(count, wanted[entry.name] or 0) end
                if count > 0 then
                    local inserted = main.insert{name = entry.name, count = count, quality = entry.quality}
                    if inserted > 0 then
                        shipment.inventory.remove{name = entry.name, count = inserted, quality = entry.quality}
                        if wanted then wanted[entry.name] = wanted[entry.name] - inserted end
                        returned[entry.name] = (returned[entry.name] or 0) + inserted
                    end
                end
            end
        end
    end
    if not found then
        return fail("nothing_queued", "no cargo is waiting at a silo for " .. platform.name)
    end
    local list = {}
    for name, count in pairs(returned) do list[#list + 1] = {name = name, count = count} end
    if #list == 0 then
        return fail("inventory_full", "nothing came back: your inventory has no room for that cargo, or none of it is queued", {
            guidance = "Make room (put items in a chest), then unship again.",
        })
    end
    M.process_shipments()
    local left = {}
    for _, shipment in ipairs(shipments()) do
        if shipment.platform == platform and shipment.inventory and shipment.inventory.valid then
            for name, count in pairs(inventory_counts(shipment.inventory)) do left[#left + 1] = {name = name, count = count} end
        end
    end
    return {success = true, returned = list, queued_cargo = left, inventory_full = main.count_empty_stacks() == 0}
end

local function action_board(agent_id, character, platform)
    if not (platform.hub and platform.hub.valid) then
        return fail("platform_not_ready", platform.name .. " has no hub yet (" .. state_name(platform.state) .. ")")
    end
    local silo, silo_err = pick_silo(character, platform)
    if not silo then
        if silo_err.error_kind ~= "rocket_not_ready" then return silo_err end
        storage.board_bookings = storage.board_bookings or {}
        storage.board_bookings[agent_id] = {platform = platform, surface_index = character.surface.index, tick = game.tick}
        return {
            success = true,
            booked = platform.name,
            silos = silo_err.silos,
            parts_required = silo_err.parts_required,
            guidance = "No rocket is ready yet, so the next ready rocket is booked for you (queued cargo waits for the one after). Stay within reach of the silo: you are launched to "
                .. platform.name .. " the moment it is ready, without calling board again. The booking lapses after 10 minutes.",
        }
    end
    local rocket_inventory = silo.get_inventory(defines.inventory.rocket_silo_rocket)
    if rocket_inventory and not rocket_inventory.is_empty() then
        return fail("rocket_has_cargo", "the rocket already holds cargo", {
            silo_unit_number = silo.unit_number,
            guidance = "cargo riding with you is lost; ship it first",
        })
    end
    local launched, err = launch_character(agent_id, character, platform, silo)
    if not launched then return fail("launch_refused", err) end
    if storage.board_bookings then storage.board_bookings[agent_id] = nil end
    return {
        success = true,
        boarding = platform.name,
        silo_unit_number = silo.unit_number,
        arrives_in_seconds = 30,
        guidance = "You arrive at the hub in about 30 s. On the platform you cannot walk: build with place_ghosts; Nauvis keeps running through its robots.",
    }
end

local function action_request(character, items)
    local list, err = normalize_items(items)
    if not list then return err end
    if not character.surface.planet then
        return fail("no_landing_pad", "landing pad requests are set from a planet surface")
    end
    local pads = character.surface.find_entities_filtered{name = "cargo-landing-pad", force = character.force}
    if #pads == 0 then
        return fail("no_landing_pad", "build a cargo-landing-pad on " .. character.surface.name .. " first")
    end
    for _, pad in pairs(pads) do
        local sections = pad.get_logistic_sections()
        local section = nil
        for _, candidate in pairs(sections.sections) do
            if candidate.group == "buddy" then section = candidate break end
        end
        section = section or sections.add_section("buddy")
        for slot = section.filters_count, 1, -1 do section.clear_slot(slot) end
        for slot, item in ipairs(list) do
            section.set_slot(slot, {value = {type = "item", name = item.name, quality = "normal"}, min = item.count})
        end
    end
    return {success = true, pads = #pads, requests = list}
end

-- Throw items out of the hub, as an inserter over the platform edge would;
-- they are lost.
local function action_jettison(platform, items)
    local list, err = normalize_items(items)
    if not list then return err end
    if not (platform.hub and platform.hub.valid) then
        return fail("platform_not_ready", platform.name .. " has no hub yet (" .. state_name(platform.state) .. ")")
    end
    local inventory = platform.hub.get_inventory(defines.inventory.hub_main)
    local removed = {}
    for _, item in ipairs(list) do
        local count = inventory.remove{name = item.name, count = item.count}
        removed[#removed + 1] = {name = item.name, count = count}
    end
    return {
        success = true,
        jettisoned = removed,
        hub_free_slots = inventory.count_empty_stacks(),
        hub_slots = #inventory,
    }
end

-- Remove the ghosts the hub can never build (stranded_ghosts); every other
-- ghost stays.
local function action_clear_ghosts(platform)
    local removed = {}
    local ghosts, why = stranded_ghosts(platform.surface)
    for _, ghost in ipairs(ghosts) do
        removed[#removed + 1] = {name = ghost.ghost_name, position = ghost.position, reason = why[ghost.unit_number]}
        ghost.destroy()
    end
    return {success = true, removed = removed}
end

-- Have the hub fill every empty turret and turret ghost on the platform with
-- one stack of the ammo it holds most of, and keep them topped up from now
-- on (see tend_platforms).
local function action_load_turrets(platform)
    if not (platform.hub and platform.hub.valid) then
        return fail("platform_not_ready", platform.name .. " has no hub yet (" .. state_name(platform.state) .. ")")
    end
    storage.turret_ammo_orders = storage.turret_ammo_orders or {}
    storage.turret_ammo_orders[platform.index] = true
    local loading = {}
    local surface = platform.surface
    for _, entity in pairs(surface.find_entities_filtered{type = "ammo-turret", force = platform.force}) do
        local count = request_turret_ammo(platform, entity)
        if count > 0 then loading[#loading + 1] = {name = entity.name, position = entity.position, count = count} end
    end
    local dead = {}
    for _, ghost in ipairs(stranded_ghosts(surface)) do dead[ghost.unit_number] = true end
    for _, ghost in pairs(surface.find_entities_filtered{type = "entity-ghost", ghost_type = "ammo-turret", force = platform.force}) do
        local count = not dead[ghost.unit_number] and request_turret_ammo(platform, ghost) or 0
        if count > 0 then loading[#loading + 1] = {name = ghost.ghost_name, position = ghost.position, count = count, ghost = true} end
    end
    local result = {success = true, loading = loading, armament = armament(platform)}
    if not loading[1] then
        result.guidance = hub_ammo_for(platform, "gun-turret") and "Every turret already holds ammo or has a request. The hub keeps them topped up from now on."
            or "The hub has no ammo for its turrets: ship firearm-magazine (space_platform action=ship)."
    else
        result.guidance = "The hub delivers these within seconds and keeps every turret topped up from now on, in flight too; a turret holding "
            .. MIN_LOADED_AMMO .. "+ magazines counts as ready. Keep magazines and a few spare gun-turrets in the hub: it rebuilds destroyed turrets."
    end
    return result
end

local function action_schedule(character, platform, stops)
    local force = character.force
    stops = type(stops) == "table" and stops or {}
    for index, stop in ipairs(stops) do
        if type(stop) ~= "string" or not game.planets[stop] then
            return fail("locked_destination", "stop " .. index .. " is not a planet", {name = stop})
        end
        if not force.is_space_location_unlocked(stop) then
            return fail("locked_destination", stop .. " is not discovered yet", {name = stop})
        end
    end
    release_hold(platform)
    if #stops == 0 then
        platform.schedule = nil
        platform.paused = true
    else
        local records = {}
        for _, stop in ipairs(stops) do records[#records + 1] = {station = stop} end
        platform.schedule = {current = 1, records = records}
        platform.paused = false
    end
    M.tend_platforms()
    local result = platform_summary(platform)
    result.success = true
    if result.departure_held == "unarmed" then
        local a = result.armament
        result.guidance = "Course set, but " .. platform.name .. " stays parked until it is armed: asteroids destroy it on the way. It has "
            .. a.needs .. ". It leaves by itself once armed and you are aboard."
    elseif result.departure_held == "thrust_stock" then
        result.guidance = "Course set, but " .. platform.name .. " stays parked until its hub holds the thruster-fuel ingredients for the trip (thrust_short): ship them, then it leaves once you are aboard."
    elseif result.departure_held == "no_crew" then
        result.guidance = "Course set; " .. platform.name .. " waits in orbit until you are aboard (space_platform action=board beside a silo), then leaves by itself."
    end
    return result
end

local function action_land(character, platform, x, y)
    if character.surface.platform ~= platform then
        return fail("not_on_platform", "you must be on " .. platform.name .. " to land from it", {character_surface = character.surface.name})
    end
    local location = platform.space_location
    local planet = location and game.planets[location.name]
    if not planet then
        return fail("in_transit", platform.name .. " is between planets; wait until it arrives")
    end
    if (x ~= nil or y ~= nil) and not (finite(x) and finite(y)) then
        return fail("invalid_position", "x and y must both be finite numbers or both omitted")
    end
    local target = planet.surface or planet.create_surface()
    local pod = platform.hub.create_cargo_pod()
    if not pod then return fail("no_free_hatch", "the hub has no free hatch; try again shortly") end
    -- A character that arrived by rocket sits inside the hub; only a forced
    -- exit frees it, and a pod silently ignores a passenger still seated there.
    if character.hub then character.set_driving(false, true) end
    pod.set_passenger(character)
    if pod.get_passenger() ~= character then
        pod.destroy()
        return fail("boarding_failed", "you could not get into the cargo pod", {character_surface = character.surface.name})
    end
    pod.cargo_pod_destination = {
        type = defines.cargo_destination.surface,
        surface = target,
        position = (x ~= nil) and {x = x, y = y} or nil,
    }
    return {success = true, landing_on = target.name, arrives_in_seconds = 20}
end

function M.space_platform(agent_id, action, platform_name, items, stops, x, y)
    local character = characters.find(agent_id)
    if not character then return no_character(agent_id) end
    if not ACTIONS[action] then
        return fail("invalid_action", "action must be status, create, ship, unship, request, jettison, clear_ghosts, load_turrets, schedule, board or land", {action = action})
    end
    if action == "status" then return action_status(character) end
    if action == "create" then return action_create(character, platform_name) end
    if action == "request" then return action_request(character, items) end
    local platform, err = resolve_platform(character, platform_name)
    if not platform then return err end
    if action == "ship" then return action_ship(agent_id, character, platform, items) end
    if action == "board" then return action_board(agent_id, character, platform) end
    if action == "jettison" then return action_jettison(platform, items) end
    if action == "clear_ghosts" then return action_clear_ghosts(platform) end
    if action == "load_turrets" then return action_load_turrets(platform) end
    if action == "unship" then return action_unship(character, platform, items) end
    if action == "schedule" then return action_schedule(character, platform, stops) end
    return action_land(character, platform, x, y)
end

return M
