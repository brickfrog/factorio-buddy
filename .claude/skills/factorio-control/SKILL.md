---
name: factorio-control
description: Use when controlling Factorio through factorioctl MCP tools; gives live-state, placement, recipe, and verification discipline without hard-coded layouts.
---

# Factorio Control

Use the game tools as the source of truth. Do not rely on memorized recipe names,
fixed entity orientations, or hard-coded build coordinates when a factorioctl
tool can inspect the current game state.

## Operating Rules

1. Inspect before mutating.
   Use `situation_report`, `render_map`, `get_inventory`, recipe/prototype
   lookups, `check_placement`, or `find_entity_placements` to choose the next
   action from live state. If the current position is far from the objective
   site, local absence is not global absence; inspect the target/resource/build
   area with read-only tools before deciding infrastructure is missing.

2. Mutate one dependent step at a time.
   Wait for the result of a world- or inventory-changing tool before issuing the
   next dependent mutating command. Use `count` parameters for repeated mining,
   crafting, or extraction instead of many tiny repeated calls.

3. Prefer derived placement.
   For drills, assemblers, power, fluids, belts, and inserters, use the helper
   tools to derive input, output, and valid placement positions. Do not assume a
   fixed orientation or copied coordinate layout.

4. Reuse existing infrastructure before building duplicates.
   For power and fluid work, audit existing `offshore-pump`, `boiler`,
   `steam-engine`, `pipe`, and electric pole entities before crafting or
   placing new ones. Search near the base, near known water, and near any
   partially built power plant. If relevant entities exist, inspect and repair
   their connections first; only place a duplicate after verifying the existing
   entity cannot be reused.

5. Verify what changed.
   After placing or changing production, call `verify_production` or the
   relevant status tool. If verification reports a problem, fix that concrete
   problem before expanding the build.

6. Preserve resource patches for extraction.
   Treat authoritative live resource tiles as extraction reserves. Put only a
   compatible mining drill or pumpjack on them. Keep assemblers, furnaces,
   labs, chests, power, and ordinary logistics outside the patch. Use
   `execute_edge_miner` to derive a miner with a clear output tile, and use
   `route_belt` so new belts route around or underground. Existing overlap is
   not permission to extend the occupied resource footprint.

7. Build durable automation instead of repeating manual cycles.
   Manual `insert_items`, `extract_items`, `craft`, `hand_feed_furnace`, and
   `feed_lab_from_inventory` are bootstrap or recovery actions, not finished
   factory work. If the same ingredient, fuel, plate, or science-pack transfer
   will be needed again, spend the next actionable turn building the durable
   route:
   - use `execute_direct_smelter` for drill-to-furnace cells
   - use `execute_edge_miner` for patch-edge drill plus output-belt cells
   - use `execute_entity_placement_near` for safe assembler, lab, pole, chest,
     or crowded-build placement
   - use `diagnose_fuel_sustainability` then `build_fuel_supply` for boilers,
     furnaces, and burner drills; when diagnostics return
     `build_fuel_supply_args`, pass those directly to `build_fuel_supply`
   - use `plan_automation_science` to derive a complete red-science cell, then
     pass its `ready_to_call.execute_args` to `build_automation_science`
   - use `plan_recipe_assembler_cell` then `build_recipe_assembler_cell` to
     create component belts such as `iron-gear-wheel` from an `iron-plate` belt
   - use `build_assembler_feed` for assembler input belts
   - use `build_assembler_output` for assembler product belts
   - use `build_lab_feed` for science belts into labs
   A plan that only hand-crafts and hand-delivers more science packs is stale
   once assemblers, inserters, belts, and power exist.
   A plan that only inserts coal or ore into an existing furnace, boiler, or
   burner drill is stale once belts, inserters, power poles, drills, labs, or
   assemblers exist. In that state, diagnose the consumer and build the coal
   delivery path with `build_fuel_supply` instead of refilling it by hand.

8. Automate science production as a complete cell.
   For `automation-science-pack`, do not stop at crafting packs in inventory.
   Place missing assemblers or labs with `execute_entity_placement_near`
   first, then use the returned `placed_unit_number`. Prefer
   `plan_recipe_assembler_cell` before `plan_automation_science` when no
   `iron-gear-wheel` belt exists: place a small gear assembler, plan the
   `iron-gear-wheel` cell from an `iron-plate` belt, execute
   `build_recipe_assembler_cell`, and use that output belt as the gear source.
   Then prefer
   `plan_automation_science` during planning. It takes the assembler, lab, gear
   source belt tile, and copper source belt tile, then returns exact
   `build_automation_science` arguments plus dry-run route checks. If
   `plan_automation_science.success` is true, call `build_automation_science`
   with `ready_to_call.execute_args`. Use `build_automation_science` with
   hand-written coordinates only when repairing a known custom layout. Use
   `build_assembler_feed`, `build_assembler_output`, and `build_lab_feed` only
   for repair or custom layouts that the composite planner cannot cover. Verify
   the assembler and lab before starting another research objective.

9. Keep belt contents explicit at every branch.
   Prefer dedicated item belts or deliberate lane separation; never infer that
   a branch is pure because one sampled tile currently shows one item. Before
   tapping an existing belt, inspect the exact source tile's lanes. If a
   consumer must accept only selected items, call `configure_inserter` on that
   exact inserter unit with the complete item whitelist and verify its readback.
   An empty whitelist clears the configuration. A filter constrains future
   pickups; it does not purify a mixed upstream belt or undo an item already in
   the inserter's hand.

10. Treat research and recipes as runtime data.
   If a craft fails or a recipe seems unavailable, query the recipe/technology
   state and follow the reported blockers. Avoid guessing alternate recipe
   names.

11. Keep replies short.
   The player sees in-game text. Report the operational result and any real
   blocker, not an internal chain of thought.
