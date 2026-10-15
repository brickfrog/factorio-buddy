-- When a parked platform's own producers should stop filling its hub. Pure
-- (no game API): space.lua reads the hub, applies the result with
-- disabled_by_script, and keeps `state` in storage; tests run it outside the
-- game.
local M = {}

-- Collectors stop at this many free hub slots or fewer and start again from
-- SLOTS_RESUME. Chunks stack to 1, so a running collector fills the last
-- slots within minutes (p2 sandbox: 12 free slots to 1 in 4 game minutes).
M.SLOTS_PAUSE = 6
M.SLOTS_RESUME = 10

-- How much of an item is enough (stop at `high`, run again under `low`).
-- Thruster-fluid ingredients keep the departure reserve plus 100, and run
-- again under reserve + 50, before a thrust_stock hold could start. Anything
-- else stops at 5 stacks (500 iron plate) and runs again under 4: with three
-- thrust inputs at 11 stacks each, a 59-slot hub keeps its free slots.
function M.caps(stack_size, thrust_reserve)
    if thrust_reserve then return thrust_reserve + 100, thrust_reserve + 50 end
    return 5 * stack_size, 4 * stack_size
end

-- One step for a parked platform. `state` ({slots_low, full}) holds the
-- previous step's verdicts, for the hysteresis, and is updated in place.
-- `free`: empty hub slots. `items`: name -> {count, high, low} for every
-- product below. `machines`: {id, collector, products}; a collector's
-- products are what the crushers aboard make of its chunks. Returns
-- id -> reason for the machines that should stand still now:
-- "hub_slots_low" for collectors while free slots are low, "hub_has_enough"
-- for any machine whose products are all enough. A machine with no listed
-- products only stops for low slots.
function M.step(state, free, items, machines)
    if state.slots_low then
        state.slots_low = free < M.SLOTS_RESUME
    else
        state.slots_low = free <= M.SLOTS_PAUSE
    end
    local was_full = state.full or {}
    local full = {}
    for name, item in pairs(items) do
        if item.count >= item.high or (was_full[name] and item.count >= item.low) then full[name] = true end
    end
    state.full = full
    local stop = {}
    for _, machine in ipairs(machines) do
        local enough = machine.products[1] ~= nil
        for _, product in ipairs(machine.products) do
            if not full[product] then enough = false end
        end
        if machine.collector and state.slots_low then
            stop[machine.id] = "hub_slots_low"
        elseif enough then
            stop[machine.id] = "hub_has_enough"
        end
    end
    return stop
end

return M
