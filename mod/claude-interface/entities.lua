local M = {}

local function pos_table(pos)
    if not pos then return nil end
    return {x = pos.x, y = pos.y}
end

local function area_table(x1, y1, x2, y2)
    return {{x1, y1}, {x2, y2}}
end

local function bounding_box_table(bb)
    if not bb then return nil end
    return {
        left_top = pos_table(bb.left_top),
        right_bottom = pos_table(bb.right_bottom),
    }
end

local function belt_neighbour_positions(neighbours)
    local positions = {}
    for _, neighbour in pairs(neighbours or {}) do
        if neighbour and neighbour.valid then
            table.insert(positions, pos_table(neighbour.position))
        end
    end
    return positions
end

local function status_name(status_value)
    if status_value == nil then return nil end
    for name, value in pairs(defines.entity_status) do
        if value == status_value then return name end
    end
    return tostring(status_value)
end

local function raw_entity_status(entity)
    local ok, value = pcall(function() return entity.status end)
    if ok then return value end
    return nil
end

-- verify_production is a machine-level diagnostic, not a census of every
-- status-bearing entity in an area. Belts and inserters expose status too, but
-- including them makes a working transport line look like production.
local PRODUCTION_ENTITY_TYPES = {
    ["assembling-machine"] = true,
    ["furnace"] = true,
    ["mining-drill"] = true,
    ["lab"] = true,
    ["rocket-silo"] = true,
    ["agricultural-tower"] = true,
    ["asteroid-collector"] = true,
}

local function is_production_entity(entity)
    return entity and PRODUCTION_ENTITY_TYPES[entity.type] == true
end

function M.find_by_unit_number(unit_number)
    unit_number = tonumber(unit_number)
    if not unit_number then return nil end
    storage.factorioctl_entities = storage.factorioctl_entities or {}
    local registered = storage.factorioctl_entities[unit_number]
    if registered and registered.valid then return registered end
    storage.factorioctl_entities[unit_number] = nil

    local entity = game.get_entity_by_unit_number(unit_number)
    if entity and entity.valid then
        storage.factorioctl_entities[unit_number] = entity
        return entity
    end

    -- LuaGameScript::get_entity_by_unit_number is only implemented for
    -- prototypes whose get-by-unit-number flag is enabled. Ordinary furnaces,
    -- assemblers, and belts can have a unit_number while still returning nil
    -- above. On a cache miss, scan every surface without a coordinate bound,
    -- populate the cache for all unit-numbered entities encountered, and return
    -- the exact match. This is deliberately global: a fixed-radius fallback
    -- silently loses valid remote entities and can redirect later mutations.
    for _, surface in pairs(game.surfaces) do
        for _, candidate in pairs(surface.find_entities()) do
            if candidate.valid and candidate.unit_number then
                storage.factorioctl_entities[candidate.unit_number] = candidate
                if candidate.unit_number == unit_number then return candidate end
            end
        end
    end
    return nil
end

function M.summary(entity, include_bounding_box)
    local result = {
        unit_number = entity.unit_number,
        name = entity.name,
        type = entity.type,
        position = pos_table(entity.position),
        direction = entity.direction,
        health = entity.health,
        force = entity.force and entity.force.name or nil,
        surface = entity.surface and entity.surface.name or nil,
    }

    if include_bounding_box then
        result.bounding_box = bounding_box_table(entity.bounding_box)
    end

    if entity.type == "inserter" then
        result.pickup_position = pos_table(entity.pickup_position)
        result.drop_position = pos_table(entity.drop_position)
    end

    if entity.type == "transport-belt" or entity.type == "underground-belt" then
        local neighbours = entity.belt_neighbours
        result.belt_neighbours_observed = true
        result.belt_input_neighbours = belt_neighbour_positions(neighbours.inputs)
        result.belt_output_neighbours = belt_neighbour_positions(neighbours.outputs)
    end

    if entity.type == "underground-belt" then
        result.belt_to_ground_type = entity.belt_to_ground_type
        -- Factorio 2.0.77 exposes the paired endpoint through `neighbours`.
        local neighbour = entity.neighbours
        result.underground_belt_neighbour = neighbour and pos_table(neighbour.position) or nil
    end

    return result
end

function M.get_surfaces()
    local result = {}
    for _, surface in pairs(game.surfaces) do
        table.insert(result, {
            name = surface.name,
            index = surface.index,
            daytime = surface.daytime,
            darkness = surface.darkness,
        })
    end
    return result
end

function M.find_entities(surface, x1, y1, x2, y2, entity_type, name)
    if not surface then return {error = "agent surface not found"} end
    storage.factorioctl_entities = storage.factorioctl_entities or {}
    local filters = {area = area_table(x1, y1, x2, y2)}
    if entity_type then filters.type = entity_type end
    if name then filters.name = name end

    local result = {}
    for _, entity in pairs(surface.find_entities_filtered(filters)) do
        if entity.unit_number then
            storage.factorioctl_entities[entity.unit_number] = entity
        end
        table.insert(result, M.summary(entity, true))
    end
    return result
end

function M.verify_production(surface, force, x1, y1, x2, y2)
    if not (surface and force) then return {error = "agent surface or force not found"} end
    local result = {}
    local found = surface.find_entities_filtered{
        area = area_table(x1, y1, x2, y2),
        force = force,
    }

    for _, entity in pairs(found) do
        if is_production_entity(entity) then
            local status_value = raw_entity_status(entity)
            local products_finished = nil
            local products_ok, products_value = pcall(function()
                return entity.products_finished
            end)
            if products_ok then
                products_finished = products_value
            end

            table.insert(result, {
                name = entity.name,
                type = entity.type,
                unit_number = entity.unit_number,
                position = pos_table(entity.position),
                status = status_name(status_value),
                products_finished = products_finished,
                working = status_value == defines.entity_status.working,
            })
        end
    end

    return result
end

local function inventory_total(inv)
    if not inv then return nil end
    local ok, count = pcall(function() return inv.get_item_count() end)
    if ok then return count end
    return nil
end

local function safe_fuel_inventory(entity)
    local ok, inv = pcall(function() return entity.get_fuel_inventory() end)
    if ok then return inv end
    return nil
end

local function safe_inventory(entity, inventory_define)
    local ok, inv = pcall(function() return entity.get_inventory(inventory_define) end)
    if ok then return inv end
    return nil
end

local function held_stack_item_name(entity)
    local ok, stack = pcall(function() return entity.held_stack end)
    if ok and stack and stack.valid_for_read then return stack.name end
    return nil
end

local function entity_status_string(entity)
    return status_name(raw_entity_status(entity))
end

local function first_inventory(entity, inventory_defines)
    for _, inventory_define in ipairs(inventory_defines) do
        local inv = safe_inventory(entity, inventory_define)
        if inv then return inv end
    end
    return nil
end

local function inventory_item_count(inv, item)
    if not inv then return nil end
    local ok, count = pcall(function() return inv.get_item_count(item) end)
    if ok then return count end
    return nil
end

local function distance_sq(a, b)
    if not a or not b then return nil end
    local dx = (a.x or 0) - (b.x or 0)
    local dy = (a.y or 0) - (b.y or 0)
    return dx * dx + dy * dy
end

local function belt_line_item_count(belt, item)
    local total = 0
    local ok = pcall(function()
        for line_index = 1, 2 do
            local line = belt.get_transport_line(line_index)
            if line then
                total = total + line.get_item_count(item)
            end
        end
    end)
    if ok then return total end
    return nil
end

local function direction_name(direction)
    if direction == defines.direction.north then return "north" end
    if direction == defines.direction.east then return "east" end
    if direction == defines.direction.south then return "south" end
    if direction == defines.direction.west then return "west" end
    return tostring(direction)
end

local function tile_coord(value)
    return math.floor(value)
end

local function route_source_position(entity)
    if entity and entity.drop_position then
        return entity.drop_position
    end
    return entity and entity.position or nil
end

local function route_source_tile(entity)
    local source = route_source_position(entity)
    if not source then return nil end
    return {x = tile_coord(source.x), y = tile_coord(source.y)}
end

local function point_in_bounding_box(point, box)
    return point and box
        and point.x >= box.left_top.x
        and point.x <= box.right_bottom.x
        and point.y >= box.left_top.y
        and point.y <= box.right_bottom.y
end

-- Factorio resolves an inserter or drill's actual recipient using the tile
-- under its drop position, which can extend slightly beyond a small target's
-- collision box (notably another inserter). Prefer that engine-owned target
-- over reconstructing interaction geometry from collision bounds.
local function entity_drops_to(source, target)
    if not (source and source.valid and target) then return false end
    local ok, drop_target = pcall(function() return source.drop_target end)
    if ok and drop_target and drop_target.valid then
        if source == target then return false end
        if drop_target.unit_number and target.unit_number then
            return drop_target.unit_number == target.unit_number
        end
        return drop_target == target
    end
    return point_in_bounding_box(source.drop_position, target.bounding_box)
end

local function inserter_can_operate(status)
    -- Fail closed. Factorio has many non-operational statuses (including
    -- script-, circuit-, and freeze-driven variants), so a denylist silently
    -- turns new engine statuses into false liveness proofs.
    return status == "working"
        or status == "waiting_for_source_items"
        or status == "waiting_for_space_in_destination"
end

local function expanded_box(box, margin)
    return {
        {box.left_top.x - margin, box.left_top.y - margin},
        {box.right_bottom.x + margin, box.right_bottom.y + margin},
    }
end

local function entity_trace_key(entity)
    if entity.unit_number then return "unit:" .. tostring(entity.unit_number) end
    return table.concat({
        tostring(entity.surface and entity.surface.index or "?"),
        tostring(entity.name),
        tostring(entity.position and entity.position.x or "?"),
        tostring(entity.position and entity.position.y or "?"),
    }, ":")
end

local function direction_step(direction)
    if direction == defines.direction.north then return 0, -1 end
    if direction == defines.direction.east then return 1, 0 end
    if direction == defines.direction.south then return 0, 1 end
    if direction == defines.direction.west then return -1, 0 end
    return nil, nil
end

local function same_tile(a, b)
    return a and b and tile_coord(a.x) == tile_coord(b.x) and tile_coord(a.y) == tile_coord(b.y)
end

local function operational_coal_drill(entity)
    if not (entity and entity.valid and entity.type == "mining-drill") then return false end
    if not (entity.mining_target and entity.mining_target.name == "coal") then return false end
    local status = entity_status_string(entity)
    return status == "working" or status == "waiting_for_space_in_destination"
end

local coal_upstream_proof

local function mining_drill_energy_source(entity)
    local burner_ok, burner = pcall(function() return entity.burner end)
    if burner_ok and burner then return "burner" end

    local prototype = entity and entity.prototype or nil
    local electric_ok, electric = pcall(function()
        return prototype and prototype.electric_energy_source_prototype
    end)
    if electric_ok and electric then return "electric" end
    return "unsupported"
end

local function electric_coal_drill_power_proof(entity)
    local operational = operational_coal_drill(entity)
    local connected_ok, connected = pcall(function()
        return entity.is_connected_to_electric_network()
    end)
    local energy_ok, energy = pcall(function() return entity.energy end)
    local network_ok, network_id = pcall(function() return entity.electric_network_id end)
    local powered = operational
        and connected_ok
        and connected == true
        and energy_ok
        and (tonumber(energy) or 0) > 0

    local reason = "powered_operational_electric_coal_drill"
    if not operational then
        reason = "electric_coal_drill_not_operational"
    elseif not connected_ok or connected ~= true then
        reason = "electric_coal_drill_not_connected"
    elseif not energy_ok or (tonumber(energy) or 0) <= 0 then
        reason = "electric_coal_drill_no_energy"
    end

    return {
        certified = powered,
        live = powered,
        reason = reason,
        energy_source = "electric",
        connected = connected_ok and connected == true,
        energy = energy_ok and energy or nil,
        electric_network_id = network_ok and network_id or nil,
        producer_unit_number = entity.unit_number,
        producer_operational = operational,
        hops = 0,
    }
end

local function burner_coal_drill_fuel_proof(surface, force, drill, state)
    state.producer_frames = state.producer_frames or {}
    local frame = {
        producer_unit_number = drill.unit_number,
        producer_operational = operational_coal_drill(drill),
        targets = {},
    }
    -- Only a return to the path that already proved this drill's coal output
    -- closes a self-sustaining producer cycle. A new loop encountered solely
    -- inside the drill's fuel path is just manually stocked transport and must
    -- remain uncertified.
    for key in pairs(state.visited) do frame.targets[key] = true end
    table.insert(state.producer_frames, frame)

    local best = nil
    local function consider(proof)
        if proof and proof.certified
            and (not best or (proof.live and not best.live))
        then
            best = proof
        end
    end

    for _, source in pairs(surface.find_entities_filtered{
        type = "mining-drill",
        force = force,
        area = expanded_box(drill.bounding_box, 4),
    }) do
        if source ~= drill and entity_drops_to(source, drill) then
            local proof = coal_upstream_proof(surface, force, source, state)
            if proof.certified then
                consider({
                    certified = true,
                    live = proof.live == true,
                    reason = "direct_durable_coal_drill_fuel",
                    producer_unit_number = proof.producer_unit_number,
                    producer_operational = proof.producer_operational == true,
                    via_unit_number = source.unit_number,
                    hops = (proof.hops or 0) + 1,
                    upstream_proof = proof,
                })
            end
        end
    end

    for _, feeder in pairs(surface.find_entities_filtered{
        type = "inserter",
        force = force,
        area = expanded_box(drill.bounding_box, 3),
    }) do
        local feeder_status = entity_status_string(feeder)
        if entity_drops_to(feeder, drill)
            and inserter_can_operate(feeder_status)
        then
            local pickup = feeder.pickup_position
            local pickup_area = {{pickup.x - 0.25, pickup.y - 0.25}, {pickup.x + 0.25, pickup.y + 0.25}}
            for _, source in pairs(surface.find_entities_filtered{area = pickup_area, force = force}) do
                if source ~= feeder
                    and source ~= drill
                    and point_in_bounding_box(pickup, source.bounding_box)
                then
                    local proof = coal_upstream_proof(surface, force, source, state)
                    if proof.certified then
                        consider({
                            certified = true,
                            live = proof.live == true,
                            reason = "operational_inserter_durable_fuel",
                            producer_unit_number = proof.producer_unit_number,
                            producer_operational = proof.producer_operational == true,
                            via_unit_number = feeder.unit_number,
                            inserter_status = feeder_status,
                            hops = (proof.hops or 0) + 1,
                            upstream_proof = proof,
                        })
                    end
                end
            end
        end
    end

    table.remove(state.producer_frames)
    if best then return best end

    local fuel_count = inventory_total(safe_fuel_inventory(drill)) or 0
    local remaining_ok, remaining = pcall(function()
        return drill.burner and drill.burner.remaining_burning_fuel or 0
    end)
    remaining = remaining_ok and (tonumber(remaining) or 0) or 0
    return {
        certified = false,
        live = false,
        reason = (fuel_count > 0 or remaining > 0)
            and "manual_burner_fuel_buffer"
            or "burner_coal_drill_fuel_not_durable",
        energy_source = "burner",
        fuel_count = fuel_count,
        remaining_burning_fuel = remaining,
        producer_unit_number = drill.unit_number,
        hops = 0,
    }
end

coal_upstream_proof = function(surface, force, entity, state)
    state = state or {visited = {}, cache = {}, nodes = 0, max_nodes = 512, producer_frames = {}}
    state.producer_frames = state.producer_frames or {}
    if not (entity and entity.valid) then
        return {certified = false, live = false, reason = "invalid_source", hops = 0}
    end

    local key = entity_trace_key(entity)
    if state.cache[key] then return state.cache[key] end
    if state.visited[key] then
        for index = #state.producer_frames, 1, -1 do
            local frame = state.producer_frames[index]
            if frame.targets[key] then
                return {
                    certified = true,
                    live = true,
                    reason = "closed_self_sustaining_coal_cycle",
                    producer_unit_number = frame.producer_unit_number,
                    producer_operational = frame.producer_operational == true,
                    cycle_unit_number = entity.unit_number,
                    hops = 0,
                }
            end
        end
        return {certified = false, live = false, reason = "transport_cycle", hops = 0}
    end
    if state.nodes >= state.max_nodes then
        return {certified = false, live = false, reason = "trace_limit", hops = 0}
    end
    state.nodes = state.nodes + 1
    state.visited[key] = true

    local function finish(result)
        state.visited[key] = nil
        if result.certified then state.cache[key] = result end
        return result
    end

    if entity.type == "mining-drill" then
        if not (entity.mining_target and entity.mining_target.name == "coal") then
            return finish({
                certified = false,
                live = false,
                reason = "mining_drill_not_on_coal",
                producer_unit_number = entity.unit_number,
                hops = 0,
            })
        end

        local energy_source = mining_drill_energy_source(entity)
        if energy_source == "electric" then
            return finish(electric_coal_drill_power_proof(entity))
        end
        if energy_source == "burner" then
            local fuel_proof = burner_coal_drill_fuel_proof(surface, force, entity, state)
            local operational = operational_coal_drill(entity)
            return finish({
                certified = fuel_proof.certified == true,
                live = fuel_proof.certified == true and fuel_proof.live == true and operational,
                reason = fuel_proof.certified
                    and (operational
                        and "durably_fueled_operational_burner_coal_drill"
                        or "durably_fueled_burner_coal_drill_not_operational")
                    or "burner_coal_drill_fuel_not_durable",
                energy_source = "burner",
                operational = operational,
                producer_unit_number = entity.unit_number,
                producer_operational = operational,
                fuel_proof = fuel_proof,
                hops = (fuel_proof.hops or 0) + 1,
            })
        end
        return finish({
            certified = false,
            live = false,
            reason = "unsupported_coal_drill_energy_source",
            energy_source = energy_source,
            producer_unit_number = entity.unit_number,
            hops = 0,
        })
    end

    local supported_belt = entity.type == "transport-belt"
    local supported_chest = entity.type == "container" or entity.type == "logistic-container"
    if not supported_belt and not supported_chest then
        return finish({
            certified = false,
            live = false,
            reason = (entity.type == "underground-belt" or entity.type == "splitter")
                and "unsupported_transport_kind"
                or "not_a_coal_logistics_source",
            hops = 0,
        })
    end

    local coal_count = supported_belt
        and (belt_line_item_count(entity, "coal") or 0)
        or (inventory_item_count(first_inventory(entity, {defines.inventory.chest}), "coal") or 0)

    local failed_producer_proof = nil
    for _, drill in pairs(surface.find_entities_filtered{
        type = "mining-drill",
        force = force,
        area = expanded_box(entity.bounding_box, 4),
    }) do
        if entity_drops_to(drill, entity) then
            local proof = coal_upstream_proof(surface, force, drill, state)
            if proof.certified then
                return finish({
                    certified = true,
                    -- Topology proves that this tile can be supplied; liveness
                    -- requires coal to have reached this exact tile.
                    live = coal_count > 0 and proof.producer_operational == true,
                    reason = coal_count > 0 and "direct_durable_coal_drill" or "upstream_ready_but_source_empty",
                    producer_unit_number = drill.unit_number,
                    producer_operational = proof.producer_operational == true,
                    via_unit_number = entity.unit_number,
                    hops = (proof.hops or 0) + 1,
                    upstream_proof = proof,
                })
            end
            failed_producer_proof = failed_producer_proof or proof
        end
    end

    if supported_belt then
        for _, upstream in pairs(surface.find_entities_filtered{
            type = "transport-belt",
            force = force,
            position = entity.position,
            radius = 1.6,
        }) do
            if upstream ~= entity then
                local dx, dy = direction_step(upstream.direction)
                local output = dx and {x = upstream.position.x + dx, y = upstream.position.y + dy} or nil
                if same_tile(output, entity.position) then
                    local proof = coal_upstream_proof(surface, force, upstream, state)
                    if proof.certified then
                        return finish({
                            certified = true,
                            live = coal_count > 0 and proof.producer_operational == true,
                            reason = coal_count > 0 and "connected_surface_belt" or "upstream_ready_but_source_empty",
                            producer_unit_number = proof.producer_unit_number,
                            producer_operational = proof.producer_operational == true,
                            via_unit_number = upstream.unit_number,
                            hops = (proof.hops or 0) + 1,
                            upstream_proof = proof,
                        })
                    end
                end
            end
        end
    end

    for _, feeder in pairs(surface.find_entities_filtered{
        type = "inserter",
        force = force,
        area = expanded_box(entity.bounding_box, 3),
    }) do
        if entity_drops_to(feeder, entity)
            and inserter_can_operate(entity_status_string(feeder))
        then
            local pickup = feeder.pickup_position
            local pickup_area = {{pickup.x - 0.25, pickup.y - 0.25}, {pickup.x + 0.25, pickup.y + 0.25}}
            for _, upstream in pairs(surface.find_entities_filtered{area = pickup_area, force = force}) do
                if upstream ~= feeder and point_in_bounding_box(pickup, upstream.bounding_box) then
                    local proof = coal_upstream_proof(surface, force, upstream, state)
                    if proof.certified then
                        return finish({
                            certified = true,
                            live = coal_count > 0 and proof.producer_operational == true,
                            reason = coal_count > 0 and "operational_inserter_feed" or "upstream_ready_but_source_empty",
                            producer_unit_number = proof.producer_unit_number,
                            producer_operational = proof.producer_operational == true,
                            via_unit_number = feeder.unit_number,
                            hops = (proof.hops or 0) + 1,
                            upstream_proof = proof,
                        })
                    end
                end
            end
        end
    end

    return finish({
        certified = false,
        live = false,
        reason = failed_producer_proof
            and "coal_drill_upstream_not_durable"
            or (coal_count > 0 and "stocked_without_proven_upstream" or "empty_without_proven_upstream"),
        upstream_proof = failed_producer_proof,
        hops = 0,
    })
end

local function coal_source_record(surface, force, entity, proof_cache)
    local proof = coal_upstream_proof(surface, force, entity, {
        visited = {},
        cache = proof_cache or {},
        nodes = 0,
        max_nodes = 512,
    })
    if entity.type == "transport-belt"
        or entity.type == "underground-belt"
        or entity.type == "splitter"
    then
        local count = belt_line_item_count(entity, "coal") or 0
        return {
            kind = "coal_belt",
            unit_number = entity.unit_number,
            name = entity.name,
            position = pos_table(entity.position),
            coal_count = count,
            durable = proof.certified,
            operational = proof.live,
            producer_operational = proof.producer_operational == true,
            upstream_proof = proof,
        }
    end
    if entity.type == "container" or entity.type == "logistic-container" then
        local count = inventory_item_count(first_inventory(entity, {defines.inventory.chest}), "coal") or 0
        return {
            kind = "coal_chest",
            unit_number = entity.unit_number,
            name = entity.name,
            position = pos_table(entity.position),
            coal_count = count,
            durable = proof.certified,
            operational = proof.live,
            producer_operational = proof.producer_operational == true,
            upstream_proof = proof,
        }
    end
    if entity.type == "mining-drill" and entity.mining_target and entity.mining_target.name == "coal" then
        local status = entity_status_string(entity)
        local energy_source = mining_drill_energy_source(entity)
        local self_bootstrap_capable = energy_source == "burner"
            and proof.live ~= true
            and status ~= "disabled"
            and status ~= "marked_for_deconstruction"
        return {
            kind = "coal_drill",
            unit_number = entity.unit_number,
            name = entity.name,
            position = pos_table(entity.position),
            route_position = pos_table(route_source_position(entity)),
            route_tile = route_source_tile(entity),
            status = status,
            durable = proof.certified,
            operational = proof.live,
            producer_operational = proof.producer_operational == true,
            self_bootstrap_capable = self_bootstrap_capable,
            upstream_proof = proof,
        }
    end
    return nil
end

local function fuel_connections(surface, force, consumer)
    local box = consumer.bounding_box
    if not box then return {} end
    local search_area = {
        {box.left_top.x - 2.5, box.left_top.y - 2.5},
        {box.right_bottom.x + 2.5, box.right_bottom.y + 2.5},
    }
    local result = {}
    local proof_cache = {}

    -- A burner inserter moving coal fuels itself from that same pickup. Model
    -- this as its durable connection so it does not recursively request a
    -- second inserter to fuel the first one.
    if consumer.type == "inserter" and consumer.burner_powered == true and consumer.pickup_position then
        local pickup = consumer.pickup_position
        local pickup_area = {{pickup.x - 0.25, pickup.y - 0.25}, {pickup.x + 0.25, pickup.y + 0.25}}
        for _, source in pairs(surface.find_entities_filtered{area = pickup_area, force = force}) do
            if source.unit_number ~= consumer.unit_number
                and point_in_bounding_box(pickup, source.bounding_box)
            then
                local source_record = coal_source_record(surface, force, source, proof_cache)
                if source_record then
                    local status = entity_status_string(consumer)
                    local operational = inserter_can_operate(status)
                    table.insert(result, {
                        connection_kind = "self_fueling_coal_pickup",
                        inserter_unit_number = consumer.unit_number,
                        inserter_name = consumer.name,
                        inserter_status = status,
                        inserter_held_item = held_stack_item_name(consumer),
                        inserter_operational = operational,
                        pickup_position = pos_table(pickup),
                        drop_position = pos_table(consumer.drop_position),
                        source = source_record,
                        source_durable = source_record.durable == true,
                        source_operational = source_record.operational == true,
                        durable = source_record.durable == true,
                        live = operational and source_record.operational == true,
                    })
                end
            end
        end
    end

    for _, inserter in pairs(surface.find_entities_filtered{type = "inserter", area = search_area, force = force}) do
        if entity_drops_to(inserter, consumer) then
            local pickup = inserter.pickup_position
            local pickup_area = {{pickup.x - 0.25, pickup.y - 0.25}, {pickup.x + 0.25, pickup.y + 0.25}}
            for _, source in pairs(surface.find_entities_filtered{area = pickup_area, force = force}) do
                if source ~= inserter and point_in_bounding_box(pickup, source.bounding_box) then
                    local source_record = coal_source_record(surface, force, source, proof_cache)
                    if source_record then
                        local inserter_status = entity_status_string(inserter)
                        local inserter_operational = inserter_can_operate(inserter_status)
                        table.insert(result, {
                            inserter_unit_number = inserter.unit_number,
                            inserter_name = inserter.name,
                            inserter_status = inserter_status,
                            inserter_held_item = held_stack_item_name(inserter),
                            inserter_operational = inserter_operational,
                            pickup_position = pos_table(pickup),
                            drop_position = pos_table(inserter.drop_position),
                            source = source_record,
                            source_durable = source_record.durable == true,
                            source_operational = source_record.operational == true,
                            durable = source_record.durable == true,
                            live = inserter_operational
                                and source_record.durable == true
                                and source_record.operational == true,
                        })
                    end
                end
            end
        end
    end
    table.sort(result, function(a, b)
        return tostring(a.inserter_unit_number or "") < tostring(b.inserter_unit_number or "")
    end)
    return result
end

local function ranked_coal_sources(origin, drills, belts, chests)
    local result = {}
    local function append(kind, source)
        local copy = {}
        for key, value in pairs(source) do copy[key] = value end
        copy.kind = kind
        copy.distance_sq = distance_sq(origin, source.position) or math.huge
        if copy.operational == nil then
            copy.operational = (copy.coal_count or 0) > 0
                or copy.status == "working"
                or copy.status == "waiting_for_space_in_destination"
        end
        table.insert(result, copy)
    end
    for _, source in ipairs(drills) do append("coal_drill", source) end
    for _, source in ipairs(belts) do append("coal_belt", source) end
    for _, source in ipairs(chests) do append("coal_chest", source) end
    table.sort(result, function(a, b)
        if a.operational ~= b.operational then return a.operational == true end
        if a.distance_sq ~= b.distance_sq then return a.distance_sq < b.distance_sq end
        if a.kind ~= b.kind then return a.kind < b.kind end
        return tostring(a.unit_number or "") < tostring(b.unit_number or "")
    end)
    return result
end

local function inserter_fuel_candidates(surface, force, entity)
    local bb = entity.bounding_box
    if not bb then return {} end
    local center = entity.position
    local lane_x = math.floor(center.x) + 0.5
    local lane_y = math.floor(center.y) + 0.5
    local north_edge = math.floor(bb.left_top.y)
    local east_edge = math.ceil(bb.right_bottom.x)
    local south_edge = math.ceil(bb.right_bottom.y)
    local west_edge = math.floor(bb.left_top.x)
    local candidates = {
        {
            side = "north",
            inserter = {x = lane_x, y = north_edge - 0.5},
            pickup = {x = lane_x, y = north_edge - 1.5},
            direction = defines.direction.north,
        },
        {
            side = "east",
            inserter = {x = east_edge + 0.5, y = lane_y},
            pickup = {x = east_edge + 1.5, y = lane_y},
            direction = defines.direction.east,
        },
        {
            side = "south",
            inserter = {x = lane_x, y = south_edge + 0.5},
            pickup = {x = lane_x, y = south_edge + 1.5},
            direction = defines.direction.south,
        },
        {
            side = "west",
            inserter = {x = west_edge - 0.5, y = lane_y},
            pickup = {x = west_edge - 1.5, y = lane_y},
            direction = defines.direction.west,
        },
    }

    local viable = {}
    for _, candidate in ipairs(candidates) do
        local inserter_name = "burner-inserter"
        local can_place_inserter = surface.can_place_entity{
            name = inserter_name,
            position = candidate.inserter,
            direction = candidate.direction,
            force = force,
            build_check_type = defines.build_check_type.manual,
        }
        local pickup_belt_can_place = surface.can_place_entity{
            name = "transport-belt",
            position = candidate.pickup,
            force = force,
            build_check_type = defines.build_check_type.manual,
        }
        table.insert(viable, {
            side = candidate.side,
            inserter_position = candidate.inserter,
            pickup_tile = candidate.pickup,
            inserter_direction = candidate.direction,
            inserter_direction_name = direction_name(candidate.direction),
            inserter_name = inserter_name,
            can_place_inserter = can_place_inserter,
            pickup_belt_can_place = pickup_belt_can_place,
            place_inserter_args = {
                entity_name = inserter_name,
                x = candidate.inserter.x,
                y = candidate.inserter.y,
                direction = direction_name(candidate.direction),
            },
        })
    end
    return viable
end

local function add_action(blocker, action)
    blocker.actions = blocker.actions or {}
    table.insert(blocker.actions, action)
end

local function blocker_priority(status, entity)
    if entity.name == "boiler" and status == "no_fuel" then return 10 end
    if status == "no_power" then return 20 end
    if status == "no_fuel" then return 30 end
    if status == "no_ingredients" then return 40 end
    if status == "waiting_for_space_in_destination" then return 50 end
    if status == "no_research_in_progress" then return 60 end
    return 90
end

local function status_message(status, entity)
    if entity.name == "boiler" and status == "no_fuel" then
        return "Boiler has no fuel; downstream electric entities may report no_power until it is fueled."
    end
    if status == "no_power" then return "Entity has no electric power." end
    if status == "no_fuel" then return "Entity has no burnable fuel." end
    if status == "no_ingredients" then return "Entity has no input ingredients." end
    if status == "waiting_for_space_in_destination" then return "Entity output is blocked or backed up." end
    if status == "no_research_in_progress" then return "Lab is idle because no research is selected." end
    return "Entity is not working: " .. tostring(status)
end

local function enrich_actions(blocker, entity, status)
    if status == "no_fuel" then
        add_action(blocker, {
            type = "build_durable_fuel_supply",
            tool = "repair_fuel_sustainability",
            description = "Use repair_fuel_sustainability near this consumer to locate an operational coal source and build a verified belt/inserter feed. Do not hand-feed it.",
        })
    elseif status == "no_ingredients" then
        add_action(blocker, {
            type = "build_durable_ingredient_supply",
            description = "Inspect the recipe and connect its inputs with belts and inserters. Do not repeatedly transfer ingredients by hand.",
        })
    elseif status == "waiting_for_space_in_destination" then
        add_action(blocker, {
            type = "clear_output",
            description = "Clear or extend the output belt/chest/tile, then verify production again.",
        })
    elseif status == "no_power" then
        add_action(blocker, {
            type = "diagnose_power",
            tool = "get_power_status",
            description = "Check power coverage and generation near this entity.",
        })
    elseif status == "no_research_in_progress" then
        add_action(blocker, {
            type = "start_research",
            tool = "start_research",
            description = "Start an available research if science production is ready.",
        })
    end
end

local function add_inventory_clues(blocker, entity)
    local fuel_inv = safe_fuel_inventory(entity)
    local fuel_count = inventory_total(fuel_inv)
    if fuel_count ~= nil then blocker.fuel_count = fuel_count end

    if entity.type == "furnace" then
        local source = safe_inventory(entity, defines.inventory.furnace_source)
        local result = safe_inventory(entity, defines.inventory.furnace_result)
        blocker.furnace_source_count = inventory_total(source)
        blocker.furnace_result_count = inventory_total(result)
    elseif entity.type == "lab" then
        blocker.lab_input_count = inventory_total(safe_inventory(entity, defines.inventory.lab_input))
    end
end

local function summarize_power_cause(blockers, boilers)
    local has_no_power = false
    for _, blocker in ipairs(blockers) do
        if blocker.status == "no_power" then
            has_no_power = true
            break
        end
    end
    if not has_no_power then return nil end

    local empty_boilers = {}
    for _, boiler in ipairs(boilers) do
        if boiler.fuel_count == 0 then
            table.insert(empty_boilers, boiler)
        end
    end
    if #empty_boilers > 0 then
        return {
            type = "unfueled_boiler",
            severity = "critical",
            message = "One or more boilers in the scan area have no fuel; fix this before debugging downstream no_power entities.",
            primary_unit_number = empty_boilers[1].unit_number,
            actions = {{
                type = "build_durable_boiler_fuel_supply",
                tool = "repair_fuel_sustainability",
                description = "Use repair_fuel_sustainability near the boiler to route coal from an operational source before treating power as repaired.",
            }},
        }
    end

    return {
        type = "no_power_cause_unknown",
        severity = "warning",
        message = "No-power entities found, but no unfueled boiler was detected in this scan area. Check pole coverage or generation outside the radius.",
        actions = {{
            type = "inspect_power",
            tool = "get_power_status",
            description = "Run get_power_status near the no_power entity and expand the scan radius if needed.",
        }},
    }
end

function M.diagnose_factory_blockers(surface, force, x1, y1, x2, y2, limit)
    if not (surface and force) then return {error = "agent surface or force not found"} end
    limit = limit or 10
    local area = area_table(x1, y1, x2, y2)
    local found = surface.find_entities_filtered{
        area = area,
        force = force,
    }
    local blockers = {}
    local boilers = {}
    local scanned = 0

    for _, entity in pairs(found) do
        local status_value = raw_entity_status(entity)
        if status_value ~= nil then
            scanned = scanned + 1
            local status = status_name(status_value)
            local working = status_value == defines.entity_status.working
            local fuel_inv = safe_fuel_inventory(entity)
            local fuel_count = inventory_total(fuel_inv)
            if entity.name == "boiler" then
                table.insert(boilers, {
                    unit_number = entity.unit_number,
                    position = pos_table(entity.position),
                    fuel_count = fuel_count,
                    status = status,
                })
            end

            if not working then
                local blocker = {
                    rank = 0,
                    priority = blocker_priority(status, entity),
                    unit_number = entity.unit_number,
                    name = entity.name,
                    type = entity.type,
                    position = pos_table(entity.position),
                    status = status,
                    working = false,
                    message = status_message(status, entity),
                }
                add_inventory_clues(blocker, entity)
                enrich_actions(blocker, entity, status)
                table.insert(blockers, blocker)
            end
        end
    end

    table.sort(blockers, function(a, b)
        if a.priority ~= b.priority then return a.priority < b.priority end
        return tostring(a.unit_number or "") < tostring(b.unit_number or "")
    end)

    local truncated = false
    if #blockers > limit then
        truncated = true
        while #blockers > limit do table.remove(blockers) end
    end
    for index, blocker in ipairs(blockers) do blocker.rank = index end

    local root_cause = summarize_power_cause(blockers, boilers)
    local suggested_actions = {}
    if root_cause and root_cause.actions then
        for _, action in ipairs(root_cause.actions) do table.insert(suggested_actions, action) end
    elseif #blockers > 0 and blockers[1].actions then
        for _, action in ipairs(blockers[1].actions) do table.insert(suggested_actions, action) end
    end

    return {
        area = {left_top = {x = math.min(x1, x2), y = math.min(y1, y2)}, right_bottom = {x = math.max(x1, x2), y = math.max(y1, y2)}},
        scanned_entities = scanned,
        blocker_count = #blockers,
        blockers = blockers,
        root_cause = root_cause,
        suggested_actions = suggested_actions,
        truncated = truncated,
        guidance = "Handle rank 1 or root_cause first, then rerun diagnose_factory_blockers and verify_production.",
    }
end

function M.diagnose_fuel_sustainability(surface, force, x1, y1, x2, y2, limit)
    if not (surface and force) then return {error = "agent surface or force not found"} end
    limit = limit or 20
    local area = area_table(x1, y1, x2, y2)
    local found = surface.find_entities_filtered{
        area = area,
        force = force,
    }
    local consumers = {}
    local coal_drills = {}
    local coal_chests = {}
    local coal_belts = {}
    local coal_resources = surface.find_entities_filtered{
        area = area,
        name = "coal",
    }
    local source_proof_cache = {}

    for _, entity in pairs(found) do
        if entity.valid then
            if entity.burner then
                local fuel_inv = safe_fuel_inventory(entity)
                local fuel_count = inventory_total(fuel_inv) or 0
                local remaining_ok, remaining_burning_fuel = pcall(function()
                    return entity.burner.remaining_burning_fuel
                end)
                remaining_burning_fuel = remaining_ok
                    and (tonumber(remaining_burning_fuel) or 0)
                    or 0
                local heat_ok, burner_heat = pcall(function() return entity.burner.heat end)
                burner_heat = heat_ok and (tonumber(burner_heat) or 0) or 0
                local has_fuel = fuel_count > 0
                    or remaining_burning_fuel > 0
                    or burner_heat > 0
                local status = entity_status_string(entity)
                local priority = 40
                if not has_fuel then priority = 10
                elseif fuel_count < 5 then priority = 20
                elseif status == "no_fuel" and remaining_burning_fuel <= 0 then priority = 10
                end
                table.insert(consumers, {
                    priority = priority,
                    unit_number = entity.unit_number,
                    name = entity.name,
                    type = entity.type,
                    position = pos_table(entity.position),
                    bounding_box = bounding_box_table(entity.bounding_box),
                    status = status,
                    burner_powered = true,
                    fuel_count = fuel_count,
                    remaining_burning_fuel = remaining_burning_fuel,
                    burner_heat = burner_heat,
                    pickup_position = entity.type == "inserter" and pos_table(entity.pickup_position) or nil,
                    drop_position = entity.type == "inserter" and pos_table(entity.drop_position) or nil,
                    fuel_inserter_candidates = inserter_fuel_candidates(surface, entity.force, entity),
                    durable_actions = {{
                        type = "route_coal_supply",
                        description = "Build a durable coal belt/chest/inserter fuel feed to unit " .. tostring(entity.unit_number) .. " instead of repeatedly moving fuel from character inventory.",
                    }},
                })
            end

            if entity.type == "mining-drill" and entity.mining_target and entity.mining_target.name == "coal" then
                local source = coal_source_record(surface, force, entity, source_proof_cache)
                if source then table.insert(coal_drills, source) end
            elseif entity.type == "transport-belt"
                or entity.type == "underground-belt"
                or entity.type == "splitter"
            then
                local source = coal_source_record(surface, force, entity, source_proof_cache)
                if source and source.coal_count > 0 then table.insert(coal_belts, source) end
            elseif entity.type == "container" or entity.type == "logistic-container" then
                local source = coal_source_record(surface, force, entity, source_proof_cache)
                if source and source.coal_count > 0 then table.insert(coal_chests, source) end
            end
        end
    end

    table.sort(consumers, function(a, b)
        if a.priority ~= b.priority then return a.priority < b.priority end
        return tostring(a.unit_number or "") < tostring(b.unit_number or "")
    end)
    local total_consumers = #consumers
    local truncated = false
    if #consumers > limit then
        truncated = true
        while #consumers > limit do table.remove(consumers) end
    end

    for _, consumer in ipairs(consumers) do
        consumer.fuel_connections = fuel_connections(surface, force, consumer)
        consumer.proven_fuel_connections = {}
        local has_live_connection = false
        for _, connection in ipairs(consumer.fuel_connections) do
            if connection.durable then table.insert(consumer.proven_fuel_connections, connection) end
            if connection.live then has_live_connection = true end
        end
        consumer.fuel_topology_present = #consumer.fuel_connections > 0
        consumer.automated = #consumer.proven_fuel_connections > 0
        local has_fuel = consumer.fuel_count > 0
            or (consumer.remaining_burning_fuel or 0) > 0
            or (consumer.burner_heat or 0) > 0
        if consumer.automated then
            consumer.issue = (not has_live_connection or not has_fuel)
                and "automated_supply_starved"
                or nil
        elseif consumer.fuel_topology_present then
            consumer.issue = "fuel_topology_not_operational"
        else
            consumer.issue = not has_fuel and "empty_fuel"
                or (consumer.fuel_count < 5 and "low_fuel" or "manual_or_unknown_fuel_supply")
        end

        local ranked_sources = ranked_coal_sources(consumer.position, coal_drills, coal_belts, coal_chests)
        consumer.candidate_sources = {}
        for i = 1, math.min(#ranked_sources, 8) do
            local source = ranked_sources[i]
            table.insert(consumer.candidate_sources, {
                kind = source.kind,
                unit_number = source.unit_number,
                name = source.name,
                position = source.position,
                route_position = source.route_position,
                route_tile = source.route_tile,
                coal_count = source.coal_count,
                status = source.status,
                durable = source.durable == true,
                operational = source.operational,
                self_bootstrap_capable = source.self_bootstrap_capable == true,
                upstream_proof = source.upstream_proof,
                distance = math.sqrt(source.distance_sq),
            })
        end

        local source = nil
        for _, candidate_source in ipairs(ranked_sources) do
            if candidate_source.operational == true then
                source = candidate_source
                break
            end
        end

        -- A cold burner coal drill cannot become a strictly durable source until
        -- its own output is routed back into its fuel inventory. When no already
        -- operational durable source exists, allow only that exact drill to
        -- propose the provisional closed-loop transaction. It is never accepted
        -- as a source for another consumer, and durability still requires the
        -- normal post-build proof.
        local provisional_self_source = false
        if not source and consumer.type == "mining-drill" then
            for _, candidate_source in ipairs(ranked_sources) do
                if candidate_source.unit_number == consumer.unit_number
                    and candidate_source.self_bootstrap_capable == true
                then
                    source = candidate_source
                    provisional_self_source = true
                    break
                end
            end
        end

        if source and not consumer.automated and consumer.fuel_inserter_candidates then
            for _, candidate in ipairs(consumer.fuel_inserter_candidates) do
                if candidate.can_place_inserter then
                    local source_position = source.route_position or source.position
                    local source_tile = source.route_tile or {x = tile_coord(source_position.x), y = tile_coord(source_position.y)}
                    local pickup_x = tile_coord(candidate.pickup_tile.x)
                    local pickup_y = tile_coord(candidate.pickup_tile.y)
                    local consumer_bootstrap_count = 0
                    if provisional_self_source
                        and consumer.fuel_count == 0
                        and (consumer.remaining_burning_fuel or 0) <= 0
                        and (consumer.burner_heat or 0) <= 0
                    then
                        consumer_bootstrap_count = 5
                    end
                    candidate.route_coal_to_pickup_args = {
                        from_x = source_tile.x,
                        from_y = source_tile.y,
                        to_x = pickup_x,
                        to_y = pickup_y,
                    }
                    candidate.fuel_transaction_args = {
                        consumer_unit_number = consumer.unit_number,
                        source_unit_number = source.unit_number,
                        from_x = source_tile.x,
                        from_y = source_tile.y,
                        pickup_x = pickup_x,
                        pickup_y = pickup_y,
                        inserter_x = candidate.inserter_position.x,
                        inserter_y = candidate.inserter_position.y,
                        inserter_direction = candidate.inserter_direction_name,
                        inserter_name = candidate.inserter_name,
                        provisional_source_unit_number = provisional_self_source
                            and source.unit_number
                            or nil,
                        bootstrap_consumer_fuel_count = provisional_self_source
                            and consumer_bootstrap_count
                            or nil,
                    }
                    candidate.automation_steps = {{
                        tool = "repair_fuel_sustainability",
                        args = {
                            x = consumer.position.x,
                            y = consumer.position.y,
                            radius = 64,
                            dry_run = false,
                        },
                    }, {
                        tool = "verify_production",
                        args = {x = consumer.position.x, y = consumer.position.y, radius = 8},
                    }}
                    consumer.ready_to_call = {
                        tool = "repair_fuel_sustainability",
                        args = {
                            x = consumer.position.x,
                            y = consumer.position.y,
                            radius = 64,
                            dry_run = false,
                        },
                        transaction_args = candidate.fuel_transaction_args,
                        source_kind = source.kind,
                        source_is_proposed = provisional_self_source,
                        follow_up = {
                            tool = "verify_production",
                            args = {x = consumer.position.x, y = consumer.position.y, radius = 8},
                        },
                    }
                    break
                end
            end
        end
    end

    local suggested_actions = {}
    if #consumers > 0 then
        local target = consumers[1]
        local cold_self_source = nil
        if target.type == "mining-drill"
            and target.fuel_count == 0
            and (target.remaining_burning_fuel or 0) <= 0
            and (target.burner_heat or 0) <= 0
        then
            for _, source in ipairs(target.candidate_sources or {}) do
                if source.unit_number == target.unit_number
                    and source.self_bootstrap_capable == true
                then
                    cold_self_source = source
                    break
                end
            end
        end
        if target.automated and not target.issue then
            table.insert(suggested_actions, {
                type = "fuel_supply_verified",
                target_unit_number = target.unit_number,
                description = "An adjacent inserter with an operational coal source is feeding this consumer; no manual fuel action is needed.",
            })
        elseif cold_self_source and target.automated then
            table.insert(suggested_actions, {
                type = "bootstrap_burner_once",
                tool = "bootstrap_burner_once",
                target_unit_number = target.unit_number,
                args = {
                    unit_number = target.unit_number,
                    fuel_item = "coal",
                    count = 5,
                },
                follow_up = {
                    tool = "repair_fuel_sustainability",
                    args = {x = target.position.x, y = target.position.y, radius = 16, dry_run = true},
                },
                description = "This already-built closed coal loop is cold. Seed only its exact burner drill once, then recheck the existing durable connection instead of building duplicate belts.",
            })
        elseif target.automated or target.fuel_topology_present then
            table.insert(suggested_actions, {
                type = "repair_upstream_coal_flow",
                target_unit_number = target.unit_number,
                connections = target.fuel_connections,
                description = "The adjacent fuel inserter topology exists, but either the inserter or its coal source is not operational. Repair that exact connection instead of hand-feeding or building a duplicate route.",
            })
        elseif target.ready_to_call then
            local description = "Build the route from the nearest operational source, then verify the actual adjacent inserter and coal flow. The source is not considered durable until proven_fuel_connections reports it."
            if target.ready_to_call.source_is_proposed then
                description = "Bootstrap this burner coal drill with a bounded fuel seed, close its own output-to-fuel loop, then verify the closed self-sustaining coal cycle. The provisional source is not considered durable until proven_fuel_connections reports it."
            end
            table.insert(suggested_actions, {
                type = "repair_fuel_sustainability",
                target_unit_number = target.unit_number,
                tool = target.ready_to_call.tool,
                args = target.ready_to_call.args,
                transaction_args = target.ready_to_call.transaction_args,
                follow_up = target.ready_to_call.follow_up,
                source_kind = target.ready_to_call.source_kind,
                source_is_proposed = target.ready_to_call.source_is_proposed,
                description = description,
            })
        elseif #coal_resources > 0 then
            table.insert(suggested_actions, {
                type = "build_coal_mining_setup",
                target_unit_number = target.unit_number,
                coal_resource_position = pos_table(coal_resources[1].position),
                description = "Build coal mining first, then route coal to fuel " .. tostring(target.name) .. " unit " .. tostring(target.unit_number) .. ".",
            })
        else
            table.insert(suggested_actions, {
                type = "find_coal_source",
                target_unit_number = target.unit_number,
                description = "No coal source was found in this scan; locate coal before treating fuel problems as solved.",
            })
        end
    end

    return {
        area = {left_top = {x = math.min(x1, x2), y = math.min(y1, y2)}, right_bottom = {x = math.max(x1, x2), y = math.max(y1, y2)}},
        consumer_count = total_consumers,
        consumers = consumers,
        coal_sources = {
            mining_drills = coal_drills,
            belts = coal_belts,
            chests = coal_chests,
            resource_tiles = #coal_resources,
        },
        suggested_actions = suggested_actions,
        truncated = truncated,
        guidance = "Do not mark fuel as solved by repeatedly moving items from character inventory. Build or repair durable coal delivery to the ranked consumer, then verify production.",
    }
end

function M.get_entity(unit_number)
    local entity = M.find_by_unit_number(unit_number)
    if not entity then return nil end
    return M.summary(entity, false)
end

function M.get_drop_position(unit_number)
    local entity = M.find_by_unit_number(unit_number)
    if not entity then
        return {error = "Entity not found or has no drop_position"}
    end
    if not entity.drop_position then
        return {error = "Entity not found or has no drop_position"}
    end

    local drop_position = entity.drop_position
    local direction = entity.direction
    return {
        drop_x = drop_position.x,
        drop_y = drop_position.y,
        drill_direction = direction,
        belt_direction = direction,
    }
end

return M
