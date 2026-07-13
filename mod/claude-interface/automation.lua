local M = {}
local diagnostics = require("diagnostics")
local logistics = require("logistics")

local MAX_EVENTS = 256
local DEFAULT_RADIUS = 96
local DEFAULT_VERIFY_TICKS = 3600
local MIN_VERIFY_TICKS = 600

local function init()
    storage.automation = storage.automation or {}
    storage.automation.events = storage.automation.events or {}
    storage.automation.manual_event_counts = storage.automation.manual_event_counts or {}
    storage.automation.milestones = storage.automation.milestones or {}
end

local function event_location(kind, unit_number)
    if kind == "insert" or kind == "lab_feed" then
        return "entity:" .. tostring(unit_number)
    end
    if kind == "extract" then return "character" end
    return nil
end

function M.record_manual(agent_id, kind, details)
    init()
    details = details or {}
    local agent_key = tostring(agent_id)
    storage.automation.manual_event_counts[agent_key] =
        (storage.automation.manual_event_counts[agent_key] or 0) + 1
    local event = {
        tick = game.tick,
        agent_id = agent_key,
        sequence = storage.automation.manual_event_counts[agent_key],
        kind = kind,
        item = details.item,
        count = tonumber(details.count) or 0,
        source = details.source,
        target = details.target or event_location(kind, details.unit_number),
        unit_number = details.unit_number,
        recipe = details.recipe,
    }
    table.insert(storage.automation.events, event)
    while #storage.automation.events > MAX_EVENTS do
        table.remove(storage.automation.events, 1)
    end
    return event
end

local function research_score()
    local force = game.forces.player
    local researched = 0
    for _, technology in pairs(force.technologies) do
        if technology.researched then researched = researched + 1 end
    end
    return researched * 1000 + math.floor((force.research_progress or 0) * 1000)
end

local function area_for(milestone)
    local radius = milestone.radius or DEFAULT_RADIUS
    return {
        {milestone.center.x - radius, milestone.center.y - radius},
        {milestone.center.x + radius, milestone.center.y + radius},
    }
end

local function inventory_count(entity, item_name)
    local inventory = entity.get_inventory(defines.inventory.chest)
    if not inventory then return 0 end
    return inventory.get_item_count(item_name)
end

local function stockpile_total(surface, area, item_name)
    if not item_name or item_name == "" then return 0 end
    local total = 0
    local containers = surface.find_entities_filtered{
        area = area,
        type = {"container", "logistic-container"},
        force = game.forces.player,
    }
    for _, entity in pairs(containers) do
        total = total + inventory_count(entity, item_name)
    end
    return total
end

local function events_since(agent_id, tick)
    init()
    local result = {}
    for _, event in ipairs(storage.automation.events) do
        if event.agent_id == tostring(agent_id) and event.tick >= tick then
            table.insert(result, event)
        end
    end
    return result
end

local function manual_handoffs(events)
    local handoffs = {}
    local pending = {}
    for _, event in ipairs(events) do
        if event.kind == "extract" and event.item then
            pending[event.item] = event
        elseif (event.kind == "insert" or event.kind == "lab_feed") and event.item then
            local source = pending[event.item]
            if source then
                table.insert(handoffs, {
                    item = event.item,
                    count = math.min(source.count or 0, event.count or 0),
                    source = source.source,
                    target = event.target,
                    via = "character:" .. tostring(event.agent_id),
                    first_tick = source.tick,
                    last_tick = event.tick,
                })
                pending[event.item] = nil
            end
        end
    end
    return handoffs
end

local function current_sample(milestone)
    local surface = game.surfaces[1]
    local area = area_for(milestone)
    local throughput = diagnostics.item_flow(surface, milestone.target_item)
    return {
        tick = game.tick,
        produced = throughput.produced_total,
        consumed = throughput.consumed_total,
        stockpiled = stockpile_total(surface, area, milestone.target_item),
        research_score = research_score(),
        manual_event_count = storage.automation.manual_event_counts[milestone.agent_id] or 0,
        throughput = throughput,
    }
end

local function first_profiles(profiles, predicate, limit)
    local result = {}
    for _, profile in ipairs(profiles or {}) do
        if predicate(profile) then
            table.insert(result, profile)
            if #result >= (limit or 5) then break end
        end
    end
    return result
end

local function target_buffer_endpoints(flow, target_kind)
    if target_kind == "stockpile" or flow.complete_path_count > 0 then return {} end
    return first_profiles(flow.buffers, function(buffer)
        return (buffer.target_inventory or 0) > 0
            and buffer.automated_input
            and not buffer.automated_output
    end, 10)
end

local function add_physical_dependencies(result, milestone, graph, sample, endpoints)
    local flow = graph.target_flow
    local throughput = sample.throughput
    if flow.producer_count == 0 then
        local outside = throughput.produced_per_minute > 0
        table.insert(result, {
            kind = outside and "target_producer_outside_audit_area" or "missing_target_producer",
            item = milestone.target_item,
            guidance = outside
                and "Production exists globally but no producer is inside the milestone area. Inspect or recenter the existing factory before building a duplicate."
                or "Build or configure a machine that produces the target item before routing downstream logistics.",
        })
    else
        local supplied = first_profiles(flow.producers, function(producer)
            return producer.item_supply_connected
        end, 5)
        if #supplied == 0 then
            table.insert(result, {
                kind = "target_producer_supply_chain_incomplete",
                item = milestone.target_item,
                producers = first_profiles(flow.producers, function() return true end, 5),
                guidance = "Work backward through the producer's reported item and fuel dependencies. Connect the first missing upstream producer-to-consumer path instead of hand-feeding it.",
            })
        end

        local outputs = first_profiles(flow.producers, function(producer)
            return producer.automated_output
        end, 5)
        if #outputs == 0 then
            table.insert(result, {
                kind = "target_producer_has_no_automated_output",
                item = milestone.target_item,
                producers = first_profiles(flow.producers, function() return true end, 5),
                guidance = "Add a directed machine output through an inserter, belt, or direct machine connection. Inventory accumulation is not delivery.",
            })
        end
    end

    if milestone.target_kind == "consumption" or milestone.target_kind == "research" then
        if flow.consumer_count == 0 then
            local outside = throughput.consumed_per_minute > 0
            table.insert(result, {
                kind = outside and "target_consumer_outside_audit_area" or "missing_target_consumer",
                item = milestone.target_item,
                guidance = outside
                    and "Consumption exists globally but no consumer is inside the milestone area. Inspect or recenter the existing factory before building a duplicate."
                    or "Build or configure the downstream consumer before extending another producer island.",
            })
        elseif flow.complete_path_count == 0 and flow.producer_count > 0 then
            table.insert(result, {
                kind = "disconnected_target_logistics",
                item = milestone.target_item,
                nearest_pair = flow.nearest_unconnected_pair,
                guidance = "Connect the nearest existing producer and consumer with a directed automated path, then audit the path again.",
            })
        end
    elseif milestone.target_kind == "stockpile" then
        if flow.buffer_count == 0 then
            table.insert(result, {
                kind = "missing_stockpile_endpoint",
                item = milestone.target_item,
                guidance = "Choose and connect an explicit storage endpoint for this stockpile milestone.",
            })
        elseif flow.producers_reaching_buffer == 0 and flow.producer_count > 0 then
            table.insert(result, {
                kind = "disconnected_stockpile_logistics",
                item = milestone.target_item,
                guidance = "Connect an existing producer to the storage endpoint without using the character as the transport edge.",
            })
        end
    end

    for _, endpoint in ipairs(endpoints) do
        table.insert(result, {
            kind = "buffer_only_endpoint",
            item = milestone.target_item,
            source = "entity:" .. tostring(endpoint.entity.unit_number),
            target = "downstream consumer",
            guidance = "This chest terminates the only observed target path. Add an automated outbound path or make stockpiling the explicit milestone.",
        })
    end

    if #result == 0 and throughput.recent_produced_per_minute <= 0 then
        table.insert(result, {
            kind = "target_production_stalled",
            item = milestone.target_item,
            producers = first_profiles(flow.producers, function() return true end, 5),
            guidance = "Topology is present but the recent production rate is zero. Diagnose the reported machine statuses, power, ingredients, fuel, and output blockage before expanding.",
        })
    elseif #result == 0
        and (milestone.target_kind == "consumption" or milestone.target_kind == "research")
        and throughput.recent_consumed_per_minute <= 0
    then
        table.insert(result, {
            kind = "target_consumption_stalled",
            item = milestone.target_item,
            consumers = first_profiles(flow.consumers, function() return true end, 5),
            guidance = "The directed path exists but recent consumption is zero. Diagnose the downstream consumer before adding capacity.",
        })
    end
end

function M.set_milestone(agent_id, objective, target_item, target_kind, center_x, center_y, radius, verify_ticks, minimum_delta)
    init()
    local character = storage.characters and storage.characters[agent_id]
    local center = {
        x = tonumber(center_x) or (character and character.valid and character.position.x) or 0,
        y = tonumber(center_y) or (character and character.valid and character.position.y) or 0,
    }
    local kind = tostring(target_kind or "production")
    if kind ~= "production" and kind ~= "consumption" and kind ~= "research" and kind ~= "stockpile" then
        return {error = "target_kind must be production, consumption, research, or stockpile"}
    end
    if type(objective) ~= "string" or objective == "" then
        return {error = "objective is required"}
    end
    if type(target_item) ~= "string" or target_item == "" then
        return {error = "target_item is required"}
    end

    local milestone = {
        id = tostring(agent_id) .. ":" .. tostring(game.tick),
        agent_id = tostring(agent_id),
        objective = objective,
        target_item = target_item,
        target_kind = kind,
        center = center,
        radius = math.max(8, math.min(256, math.floor(tonumber(radius) or DEFAULT_RADIUS))),
        verify_ticks = math.max(MIN_VERIFY_TICKS, math.floor(tonumber(verify_ticks) or DEFAULT_VERIFY_TICKS)),
        minimum_delta = math.max(1, math.floor(tonumber(minimum_delta) or 1)),
        created_tick = game.tick,
        status = "in_progress",
        observation = nil,
    }
    storage.automation.milestones[tostring(agent_id)] = milestone
    return {success = true, milestone = milestone}
end

function M.get_milestone(agent_id)
    init()
    local milestone = storage.automation.milestones[tostring(agent_id)]
    if not milestone then
        return {
            configured = false,
            guidance = "Set one concrete factory milestone before expanding the factory.",
        }
    end
    return {configured = true, milestone = milestone}
end

function M.audit(agent_id)
    init()
    local milestone = storage.automation.milestones[tostring(agent_id)]
    if not milestone then return M.get_milestone(agent_id) end

    local surface = game.surfaces[1]
    local area = area_for(milestone)
    local graph = logistics.snapshot(surface, area, milestone.target_item)
    local sample = current_sample(milestone)
    local endpoints = target_buffer_endpoints(graph.target_flow, milestone.target_kind)
    local structural_dependencies = {}
    add_physical_dependencies(structural_dependencies, milestone, graph, sample, endpoints)
    local since_tick = milestone.observation
        and milestone.observation.tick
        or math.max(milestone.created_tick, game.tick - milestone.verify_ticks)
    local manual = events_since(agent_id, since_tick)
    local handoffs = manual_handoffs(manual)
    local open_dependencies = {}

    for _, handoff in ipairs(handoffs) do
        table.insert(open_dependencies, {
            kind = "manual_material_path",
            item = handoff.item,
            source = handoff.source,
            target = handoff.target,
            via = handoff.via,
            guidance = "Replace this character-mediated transfer with a continuous machine, belt, inserter, pipe, or logistics path.",
        })
    end
    if #handoffs == 0 then
        for _, event in ipairs(manual) do
            table.insert(open_dependencies, {
                kind = "manual_" .. tostring(event.kind),
                item = event.item,
                source = event.source,
                target = event.target,
                guidance = "Manual actions are bootstrap or recovery only; close the corresponding steady-state material dependency before completing the milestone.",
            })
        end
    end
    for _, dependency in ipairs(structural_dependencies) do
        table.insert(open_dependencies, dependency)
    end

    local status = "ready_for_observation"
    if milestone.status == "complete" then status = "complete"
    elseif #manual > 0 then status = "manual_dependency"
    elseif milestone.target_kind ~= "stockpile" and #endpoints > 0 then status = "buffer_only"
    elseif #structural_dependencies > 0 then status = "open_dependency"
    elseif milestone.observation then status = "observing"
    end

    return {
        configured = true,
        status = status,
        tick = game.tick,
        milestone = milestone,
        manual_window = {
            since_tick = since_tick,
            event_count = #manual,
            events = manual,
            handoffs = handoffs,
        },
        production_flow = sample.throughput,
        material_graph = graph,
        target_buffer_endpoints = endpoints,
        structural_dependency_count = #structural_dependencies,
        structural_dependencies = structural_dependencies,
        open_dependency_count = #open_dependencies,
        open_dependencies = open_dependencies,
        current_sample = sample,
        guidance = #open_dependencies > 0
            and "Close the first open dependency and audit again. Do not expand an unrelated production island."
            or "Begin or continue sustained verification with verify_factory_milestone.",
    }
end

function M.verify(agent_id)
    init()
    local milestone = storage.automation.milestones[tostring(agent_id)]
    if not milestone then return M.get_milestone(agent_id) end
    if milestone.status == "complete" then
        return {
            success = true,
            verified = true,
            status = "complete",
            milestone = milestone,
            guidance = "Milestone is already complete. Select the next concrete factory milestone.",
        }
    end

    local sample = current_sample(milestone)
    if not milestone.observation then
        local preflight = M.audit(agent_id)
        if (preflight.open_dependency_count or 0) > 0 then
            return {
                success = false,
                verified = false,
                status = "blocked",
                milestone = milestone,
                production_flow = preflight.production_flow,
                open_dependencies = preflight.open_dependencies,
                guidance = "Close the first reported dependency before starting a clean sustained observation window.",
            }
        end
        milestone.observation = sample
        milestone.status = "observing"
        return {
            success = true,
            verified = false,
            status = "observation_started",
            milestone = milestone,
            baseline = sample,
            remaining_ticks = milestone.verify_ticks,
            guidance = "Leave the character out of the material path. Call verify_factory_milestone again after the observation window.",
        }
    end

    local baseline = milestone.observation
    local elapsed = game.tick - baseline.tick
    if elapsed < milestone.verify_ticks then
        return {
            success = true,
            verified = false,
            status = "observing",
            milestone = milestone,
            baseline = baseline,
            current = sample,
            elapsed_ticks = elapsed,
            remaining_ticks = milestone.verify_ticks - elapsed,
        }
    end

    local manual = events_since(agent_id, baseline.tick)
    local produced_delta = sample.produced - baseline.produced
    local consumed_delta = sample.consumed - baseline.consumed
    local stockpile_delta = sample.stockpiled - baseline.stockpiled
    local research_delta = sample.research_score - baseline.research_score
    local manual_event_delta = sample.manual_event_count - (baseline.manual_event_count or 0)
    local progress_delta = produced_delta
    if milestone.target_kind == "consumption" then progress_delta = consumed_delta end
    if milestone.target_kind == "research" then progress_delta = research_delta end
    if milestone.target_kind == "stockpile" then progress_delta = stockpile_delta end
    local audit = M.audit(agent_id)
    local verified = manual_event_delta == 0
        and #manual == 0
        and progress_delta >= milestone.minimum_delta
        and (audit.open_dependency_count or 0) == 0
    local verification_dependencies = audit.open_dependencies or {}

    local result = {
        success = true,
        verified = verified,
        status = verified and "complete" or "failed",
        milestone = milestone,
        window = {
            start_tick = baseline.tick,
            end_tick = sample.tick,
            elapsed_ticks = elapsed,
        },
        evidence = {
            target_item = milestone.target_item,
            target_kind = milestone.target_kind,
            produced_delta = produced_delta,
            consumed_delta = consumed_delta,
            stockpile_delta = stockpile_delta,
            research_delta = research_delta,
            required_delta = milestone.minimum_delta,
            manual_event_delta = manual_event_delta,
            manual_transfer_count = #manual,
            manual_events = manual,
            buffer_only_endpoint_count = milestone.target_kind == "stockpile"
                and 0
                or #(audit.target_buffer_endpoints or {}),
            structural_dependency_count = audit.structural_dependency_count or 0,
            production_flow = audit.production_flow,
        },
        open_dependencies = verification_dependencies,
    }

    if verified then
        milestone.status = "complete"
        milestone.completed_tick = game.tick
        result.guidance = "Milestone is sustained without character logistics. Select the next milestone."
    else
        milestone.status = "in_progress"
        milestone.observation = nil
        result.guidance = "Milestone did not pass. Close the reported dependency, then start a new clean observation window."
    end
    return result
end

return M
