local M = {}

local MAX_PATH_EXAMPLES = 5

local function position_table(position)
    if not position then return nil end
    return {x = position.x, y = position.y}
end

local function entity_key(entity)
    if not (entity and entity.valid and entity.unit_number) then return nil end
    return tostring(entity.unit_number)
end

local function entity_ref(entity)
    if not (entity and entity.valid) then return nil end
    return {
        unit_number = entity.unit_number,
        name = entity.name,
        type = entity.type,
        position = position_table(entity.position),
    }
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
    return ok and status_name(value) or nil
end

local function collection_has_item(collection, item_name)
    for _, entry in pairs(collection or {}) do
        if entry.name == item_name then return true end
    end
    return false
end

local function safe_recipe(entity)
    local ok, recipe = pcall(function() return entity.get_recipe() end)
    if ok then return recipe end
    return nil
end

local function ingredient_list(recipe, ingredient_type)
    local result = {}
    for _, ingredient in pairs(recipe and recipe.ingredients or {}) do
        if (ingredient.type or "item") == ingredient_type then
            table.insert(result, ingredient.name)
        end
    end
    table.sort(result)
    return result
end

local function product_list(entity)
    local result = {}
    local seen = {}
    local recipe = safe_recipe(entity)
    for _, product in pairs(recipe and recipe.products or {}) do
        if (product.type or "item") == "item" and not seen[product.name] then
            seen[product.name] = true
            table.insert(result, product.name)
        end
    end
    if entity.type == "mining-drill" then
        local ok, target = pcall(function() return entity.mining_target end)
        local properties = ok and target and target.valid
            and target.prototype
            and target.prototype.mineable_properties
            or nil
        for _, product in pairs(properties and properties.products or {}) do
            if (product.type or "item") == "item" and not seen[product.name] then
                seen[product.name] = true
                table.insert(result, product.name)
            end
        end
        if ok and target and target.valid and not seen[target.name] then
            table.insert(result, target.name)
        end
    end
    table.sort(result)
    return result
end

local function list_has(values, target)
    for _, value in ipairs(values or {}) do
        if value == target then return true end
    end
    return false
end

local function mining_produces(entity, item_name)
    if entity.type ~= "mining-drill" then return false end
    local ok, target = pcall(function() return entity.mining_target end)
    if not (ok and target and target.valid) then return false end
    if target.name == item_name then return true end
    local prototype = target.prototype
    local properties = prototype and prototype.mineable_properties
    return properties and collection_has_item(properties.products, item_name) or false
end

local function lab_consumes(item_name)
    local technology = game.forces.player.current_research
    return technology
        and collection_has_item(technology.research_unit_ingredients, item_name)
        or false
end

local function burner_categories(entity)
    local ok, burner = pcall(function() return entity.burner end)
    if not (ok and burner) then return {} end
    local categories_ok, categories = pcall(function()
        return entity.prototype.burner_prototype.fuel_categories
    end)
    if not (categories_ok and categories) then return {} end
    local result = {}
    for key, value in pairs(categories) do
        if type(key) == "string" and value == true then
            result[key] = true
        elseif type(value) == "string" then
            result[value] = true
        end
    end
    return result
end

local function burner_consumes(entity, item_name)
    local item = prototypes.item[item_name]
    if not (item and item.fuel_category) then return false end
    return burner_categories(entity)[item.fuel_category] == true
end

local function chest_count(entity, item_name)
    if entity.type ~= "container" and entity.type ~= "logistic-container" then return nil end
    local ok, inventory = pcall(function()
        return entity.get_inventory(defines.inventory.chest)
    end)
    if not (ok and inventory) then return 0 end
    local count_ok, count = pcall(function() return inventory.get_item_count(item_name) end)
    return count_ok and count or 0
end

local function target_profile(entity, item_name)
    local recipe = safe_recipe(entity)
    local products = product_list(entity)
    local produces = list_has(products, item_name)
    local consumes = recipe and collection_has_item(recipe.ingredients, item_name) or false
    local producer_kind = produces and "recipe" or nil
    local consumer_kind = consumes and "recipe" or nil

    if mining_produces(entity, item_name) then
        produces = true
        producer_kind = "mining"
    end
    if entity.type == "lab" and lab_consumes(item_name) then
        consumes = true
        consumer_kind = "research"
    elseif burner_consumes(entity, item_name) then
        consumes = true
        consumer_kind = "fuel"
    end

    return {
        entity = entity_ref(entity),
        status = entity_status(entity),
        recipe = recipe and recipe.name or nil,
        products = products,
        item_ingredients = ingredient_list(recipe, "item"),
        fluid_ingredients = ingredient_list(recipe, "fluid"),
        fuel_categories = burner_categories(entity),
        produces_target = produces,
        producer_kind = producer_kind,
        consumes_target = consumes,
        consumer_kind = consumer_kind,
        target_inventory = chest_count(entity, item_name),
    }
end

local function inside_box(position, box)
    return position.x >= box.left_top.x and position.x <= box.right_bottom.x
        and position.y >= box.left_top.y and position.y <= box.right_bottom.y
end

local function entity_at(surface, position)
    if not position then return nil end
    local found = surface.find_entities_filtered{
        position = position,
        radius = 0.35,
        force = game.forces.player,
    }
    local fallback = nil
    for _, entity in pairs(found) do
        if entity.valid and entity.type ~= "character" and entity.type ~= "inserter" then
            if inside_box(position, entity.bounding_box) then return entity end
            fallback = fallback or entity
        end
    end
    return fallback
end

local function inserter_target(inserter, property, fallback_position, surface)
    local ok, target = pcall(function() return inserter[property] end)
    if ok and target and target.valid then return target end
    return entity_at(surface, fallback_position)
end

local function add_entity(entities, entity)
    local key = entity_key(entity)
    if key then entities[key] = entity end
    return key
end

local function add_edge(edges, edge_keys, adjacency, reverse, kind, source, target, transporter)
    local source_key = entity_key(source)
    local target_key = entity_key(target)
    if not (source_key and target_key) or source_key == target_key then return end
    local edge_key = kind .. ":" .. source_key .. ":" .. target_key
    if edge_keys[edge_key] then return end
    edge_keys[edge_key] = true
    adjacency[source_key] = adjacency[source_key] or {}
    reverse[target_key] = reverse[target_key] or {}
    table.insert(adjacency[source_key], target_key)
    table.insert(reverse[target_key], source_key)
    table.insert(edges, {
        kind = kind,
        source_key = source_key,
        target_key = target_key,
        source = entity_ref(source),
        target = entity_ref(target),
        transporter = entity_ref(transporter),
    })
end

local function discover_transport(surface, area, item_name)
    local entities = {}
    local belt_entities = {}
    local belt_item_counts = {}
    local edges = {}
    local edge_keys = {}
    local adjacency = {}
    local reverse = {}
    local found = surface.find_entities_filtered{area = area, force = game.forces.player}
    for _, entity in pairs(found) do add_entity(entities, entity) end

    for _, entity in pairs(found) do
        local max_ok, max_index = pcall(function()
            return entity.get_max_transport_line_index()
        end)
        if max_ok and type(max_index) == "number" and max_index > 0 then
            local key = add_entity(entities, entity)
            belt_entities[key] = entity
            belt_item_counts[key] = 0
            for line_index = 1, max_index do
                local line_ok, line = pcall(function()
                    return entity.get_transport_line(line_index)
                end)
                if line_ok and line then
                    local count_ok, count = pcall(function()
                        return line.get_item_count(item_name)
                    end)
                    if count_ok then belt_item_counts[key] = belt_item_counts[key] + count end
                    local output_ok, outputs = pcall(function() return line.output_lines end)
                    if output_ok then
                        for _, output in pairs(outputs or {}) do
                            local owner_ok, owner = pcall(function() return output.owner end)
                            if owner_ok and owner and owner.valid then
                                add_entity(entities, owner)
                                belt_entities[entity_key(owner)] = owner
                                add_edge(
                                    edges,
                                    edge_keys,
                                    adjacency,
                                    reverse,
                                    "transport_line",
                                    entity,
                                    owner,
                                    nil
                                )
                            end
                        end
                    end
                end
            end
            local neighbours_ok, neighbours = pcall(function()
                return entity.belt_neighbours
            end)
            if neighbours_ok and neighbours then
                for _, output in pairs(neighbours.outputs or {}) do
                    if output and output.valid then
                        add_entity(entities, output)
                        belt_entities[entity_key(output)] = output
                        add_edge(
                            edges,
                            edge_keys,
                            adjacency,
                            reverse,
                            "belt_connection",
                            entity,
                            output,
                            nil
                        )
                    end
                end
            end
            if entity.type == "underground-belt" and entity.belt_to_ground_type == "input" then
                local underground_ok, underground = pcall(function()
                    return entity.neighbours
                end)
                if not (underground_ok and underground and underground.valid) then
                    underground_ok, underground = pcall(function()
                        return entity.underground_belt_neighbour
                    end)
                end
                if underground_ok and underground and underground.valid then
                    add_entity(entities, underground)
                    belt_entities[entity_key(underground)] = underground
                    add_edge(
                        edges,
                        edge_keys,
                        adjacency,
                        reverse,
                        "underground_connection",
                        entity,
                        underground,
                        nil
                    )
                end
            end
        end
    end

    local inserters = surface.find_entities_filtered{
        area = area,
        type = "inserter",
        force = game.forces.player,
    }
    for _, inserter in pairs(inserters) do
        local source = inserter_target(inserter, "pickup_target", inserter.pickup_position, surface)
        local target = inserter_target(inserter, "drop_target", inserter.drop_position, surface)
        if source then add_entity(entities, source) end
        if target then add_entity(entities, target) end
        add_edge(edges, edge_keys, adjacency, reverse, "inserter", source, target, inserter)
    end

    for _, entity in pairs(found) do
        if entity.type == "mining-drill" then
            local ok, drop_position = pcall(function() return entity.drop_position end)
            local target = ok and entity_at(surface, drop_position) or nil
            if target then add_entity(entities, target) end
            add_edge(edges, edge_keys, adjacency, reverse, "direct_output", entity, target, nil)
        end
    end

    return {
        entities = entities,
        belt_entities = belt_entities,
        belt_item_counts = belt_item_counts,
        edges = edges,
        adjacency = adjacency,
        reverse = reverse,
    }
end

local function belt_networks(transport)
    local undirected = {}
    for _, edge in ipairs(transport.edges) do
        if edge.kind == "transport_line"
            or edge.kind == "belt_connection"
            or edge.kind == "underground_connection"
        then
            undirected[edge.source_key] = undirected[edge.source_key] or {}
            undirected[edge.target_key] = undirected[edge.target_key] or {}
            table.insert(undirected[edge.source_key], edge.target_key)
            table.insert(undirected[edge.target_key], edge.source_key)
        end
    end

    local keys = {}
    for key, _ in pairs(transport.belt_entities) do table.insert(keys, key) end
    table.sort(keys)
    local visited = {}
    local network_by_belt = {}
    local networks = {}
    for _, start in ipairs(keys) do
        if not visited[start] then
            local queue = {start}
            local index = 1
            local members = {}
            local item_count = 0
            visited[start] = true
            while index <= #queue do
                local key = queue[index]
                index = index + 1
                table.insert(members, key)
                item_count = item_count + (transport.belt_item_counts[key] or 0)
                for _, neighbour in ipairs(undirected[key] or {}) do
                    if not visited[neighbour] then
                        visited[neighbour] = true
                        table.insert(queue, neighbour)
                    end
                end
            end
            local network_id = #networks + 1
            for _, key in ipairs(members) do network_by_belt[key] = network_id end
            table.insert(networks, {
                id = network_id,
                belt_count = #members,
                target_item_count = item_count,
                input_edge_count = 0,
                output_edge_count = 0,
            })
        end
    end

    for _, edge in ipairs(transport.edges) do
        if edge.kind ~= "transport_line"
            and edge.kind ~= "belt_connection"
            and edge.kind ~= "underground_connection"
        then
            local target_network = network_by_belt[edge.target_key]
            local source_network = network_by_belt[edge.source_key]
            if target_network and not source_network then
                networks[target_network].input_edge_count = networks[target_network].input_edge_count + 1
            end
            if source_network and not target_network then
                networks[source_network].output_edge_count = networks[source_network].output_edge_count + 1
            end
        end
    end
    return networks, network_by_belt
end

local function find_path(start_key, target_keys, adjacency)
    local queue = {start_key}
    local index = 1
    local previous = {[start_key] = false}
    local found = nil
    while index <= #queue and not found do
        local key = queue[index]
        index = index + 1
        for _, next_key in ipairs(adjacency[key] or {}) do
            if previous[next_key] == nil then
                previous[next_key] = key
                if target_keys[next_key] then
                    found = next_key
                    break
                end
                table.insert(queue, next_key)
            end
        end
    end
    if not found then return nil end
    local path = {}
    local cursor = found
    while cursor do
        table.insert(path, 1, cursor)
        cursor = previous[cursor]
    end
    return path
end

local function path_summary(path, transport, network_by_belt)
    local networks = {}
    local seen = {}
    for _, key in ipairs(path) do
        local network_id = network_by_belt[key]
        if network_id and not seen[network_id] then
            seen[network_id] = true
            table.insert(networks, network_id)
        end
    end
    return {
        source = entity_ref(transport.entities[path[1]]),
        target = entity_ref(transport.entities[path[#path]]),
        hop_count = #path - 1,
        belt_networks = networks,
    }
end

local function nearest_pair(producers, consumers)
    local best = nil
    local best_distance = nil
    for _, producer in ipairs(producers) do
        for _, consumer in ipairs(consumers) do
            local a = producer.entity.position
            local b = consumer.entity.position
            local dx = a.x - b.x
            local dy = a.y - b.y
            local distance = dx * dx + dy * dy
            if not best_distance or distance < best_distance then
                best_distance = distance
                best = {producer = producer.entity, consumer = consumer.entity}
            end
        end
    end
    return best
end

function M.snapshot(surface, area, item_name)
    local transport = discover_transport(surface, area, item_name)
    local networks, network_by_belt = belt_networks(transport)
    local profile_cache = {}
    local producer_cache = {}

    local function profile_for(key, target_item)
        profile_cache[target_item] = profile_cache[target_item] or {}
        if profile_cache[target_item][key] == nil then
            local entity = transport.entities[key]
            profile_cache[target_item][key] = entity and target_profile(entity, target_item) or false
        end
        local profile = profile_cache[target_item][key]
        return profile ~= false and profile or nil
    end

    local function producers_for(target_item)
        if producer_cache[target_item] then return producer_cache[target_item] end
        local result = {}
        for key, _ in pairs(transport.entities) do
            if not transport.belt_entities[key] then
                local profile = profile_for(key, target_item)
                if profile and profile.produces_target then result[key] = profile end
            end
        end
        producer_cache[target_item] = result
        return result
    end

    local supply_cache = {}
    local function producer_has_supply(key, target_item, depth, trail)
        local cache_key = target_item .. ":" .. key
        if supply_cache[cache_key] ~= nil then return supply_cache[cache_key] end
        if depth <= 0 or trail[cache_key] then return false end
        local profile = profile_for(key, target_item)
        if not profile then return false end

        local dependencies = {}
        local connected = true
        local next_trail = {}
        for seen, value in pairs(trail) do next_trail[seen] = value end
        next_trail[cache_key] = true
        for _, ingredient in ipairs(profile.item_ingredients or {}) do
            local ingredient_connected = false
            local source_ref = nil
            local candidates = producers_for(ingredient)
            for source_key, _ in pairs(candidates) do
                if source_key ~= key then
                    local path = find_path(source_key, {[key] = true}, transport.adjacency)
                    if path and producer_has_supply(source_key, ingredient, depth - 1, next_trail) then
                        ingredient_connected = true
                        source_ref = entity_ref(transport.entities[source_key])
                        break
                    end
                end
            end
            table.insert(dependencies, {
                item = ingredient,
                connected = ingredient_connected,
                source = source_ref,
                candidate_producer_count = (function()
                    local count = 0
                    for _, _ in pairs(candidates) do count = count + 1 end
                    return count
                end)(),
            })
            if not ingredient_connected then connected = false end
        end

        local requires_fuel = false
        for _, _ in pairs(profile.fuel_categories or {}) do
            requires_fuel = true
            break
        end
        local fuel_connected = not requires_fuel
        local fuel_source = nil
        local fuel_item = nil
        if requires_fuel then
            for source_key, source_entity in pairs(transport.entities) do
                if source_key ~= key and not transport.belt_entities[source_key] then
                    for _, candidate_item in ipairs(product_list(source_entity)) do
                        local item = prototypes.item[candidate_item]
                        if item
                            and item.fuel_category
                            and profile.fuel_categories[item.fuel_category]
                        then
                            local path = find_path(source_key, {[key] = true}, transport.adjacency)
                            local source_cache_key = candidate_item .. ":" .. source_key
                            if path
                                and (
                                    next_trail[source_cache_key]
                                    or producer_has_supply(
                                        source_key,
                                        candidate_item,
                                        depth - 1,
                                        next_trail
                                    )
                                )
                            then
                                fuel_connected = true
                                fuel_source = entity_ref(source_entity)
                                fuel_item = candidate_item
                                break
                            end
                        end
                    end
                end
                if fuel_connected then break end
            end
        end

        profile.item_supply_dependencies = dependencies
        profile.fuel_supply_required = requires_fuel
        profile.fuel_supply_connected = fuel_connected
        profile.fuel_source = fuel_source
        profile.fuel_item = fuel_item
        profile.item_supply_connected = connected and fuel_connected
        profile.fluid_supply_unverified = #(profile.fluid_ingredients or {}) > 0
        supply_cache[cache_key] = connected and fuel_connected
        return connected and fuel_connected
    end

    local producers = {}
    local consumers = {}
    local buffers = {}
    local producer_keys = {}
    local consumer_keys = {}
    local buffer_keys = {}

    for key, entity in pairs(transport.entities) do
        if not transport.belt_entities[key] then
            local profile = profile_for(key, item_name)
            if profile.produces_target then
                profile.automated_output = #(transport.adjacency[key] or {}) > 0
                profile.outgoing_edge_count = #(transport.adjacency[key] or {})
                producer_has_supply(key, item_name, 8, {})
                table.insert(producers, profile)
                producer_keys[key] = profile
            end
            if profile.consumes_target then
                profile.automated_input = #(transport.reverse[key] or {}) > 0
                profile.incoming_edge_count = #(transport.reverse[key] or {})
                table.insert(consumers, profile)
                consumer_keys[key] = profile
            end
            if profile.target_inventory ~= nil then
                profile.automated_input = #(transport.reverse[key] or {}) > 0
                profile.automated_output = #(transport.adjacency[key] or {}) > 0
                table.insert(buffers, profile)
                buffer_keys[key] = profile
            end
        end
    end

    table.sort(producers, function(a, b) return a.entity.unit_number < b.entity.unit_number end)
    table.sort(consumers, function(a, b) return a.entity.unit_number < b.entity.unit_number end)
    table.sort(buffers, function(a, b) return a.entity.unit_number < b.entity.unit_number end)

    local connected_producers = 0
    local producers_reaching_buffer = 0
    local path_examples = {}
    for key, profile in pairs(producer_keys) do
        local path = find_path(key, consumer_keys, transport.adjacency)
        profile.reaches_consumer = path ~= nil
        if path then
            connected_producers = connected_producers + 1
            if #path_examples < MAX_PATH_EXAMPLES then
                table.insert(path_examples, path_summary(path, transport, network_by_belt))
            end
        end
        local buffer_path = find_path(key, buffer_keys, transport.adjacency)
        profile.reaches_buffer = buffer_path ~= nil
        if buffer_path then producers_reaching_buffer = producers_reaching_buffer + 1 end
    end

    local non_belt_edges = {}
    for _, edge in ipairs(transport.edges) do
        if edge.kind ~= "transport_line"
            and edge.kind ~= "belt_connection"
            and edge.kind ~= "underground_connection"
        then
            table.insert(non_belt_edges, edge)
        end
    end

    local active_networks = 0
    local target_items_on_belts = 0
    for _, network in ipairs(networks) do
        target_items_on_belts = target_items_on_belts + network.target_item_count
        if network.target_item_count > 0 then active_networks = active_networks + 1 end
    end

    return {
        target_item = item_name,
        topology = {
            node_count = (function()
                local count = 0
                for _, _ in pairs(transport.entities) do count = count + 1 end
                return count
            end)(),
            directed_edge_count = #transport.edges,
            automated_transfer_edges = non_belt_edges,
            belt_entity_count = (function()
                local count = 0
                for _, _ in pairs(transport.belt_entities) do count = count + 1 end
                return count
            end)(),
            belt_network_count = #networks,
            belt_networks = networks,
        },
        target_flow = {
            producer_count = #producers,
            producers = producers,
            consumer_count = #consumers,
            consumers = consumers,
            buffer_count = #buffers,
            buffers = buffers,
            producers_reaching_consumer = connected_producers,
            producers_reaching_buffer = producers_reaching_buffer,
            complete_path_count = connected_producers,
            path_examples = path_examples,
            target_items_on_belts = target_items_on_belts,
            active_belt_network_count = active_networks,
            nearest_unconnected_pair = connected_producers == 0
                and nearest_pair(producers, consumers)
                or nil,
        },
        evidence_scope = {
            item_transport = {"belts", "underground-belts", "splitters", "inserters", "direct-mining-output"},
            note = "Topology proves directed machine transport. Global production statistics prove rate; topology alone does not prove that the target item traversed every edge.",
        },
    }
end

return M
