# GOAL: Make Unattended Doug Boring

Last updated: 2026-06-30 05:19 CDT, after provider-free report progress
truthfulness cleanup, power-diagnostic compaction, and read-only gate audit.

Doug should be able to run under `just resume` for hours without burning the
provider budget on preventable loops, stale sessions, stale plans, or opaque
Factorio tool failures.

The target experience is modest and ruthless: leave the game running, come back
later, and see either real factory progress or a compact, useful reason why the
agent paused. No retry storms. No context-window death spiral. No rebuilding the
same pump because an old plan forgot it exists.

## Non-Negotiables

- Runtime Lua behavior matters more than Rust compile success.
- Smoke tests must exercise the synced mod copy that Factorio actually loads.
- Planner and reflection turns are read-only. If they mutate the world, that is
  a bridge bug, not a prompt issue.
- Provider limits, timeouts, max-turn caps, and context-window failures are
  infrastructure state. They must not become durable gameplay lessons.
- The agent should discover geometry through tools. Do not teach fixed map
  coordinates or exact build placements as the main strategy.
- Prefer structural guards over "Doug, please be careful" wording.
- Keep higher-level helpers dry-run/planning-first unless the operator
  explicitly asks for mutation.

## Current Status

Provider-free hardening is in tree and passing local verification.

Still not done: live unattended proof. The provider was capped during this work,
so overnight behavior still has to be validated after reset with:

```sh
cd companion
just resume
```

Then inspect:

```sh
just report
```

## Implemented Runtime Guards

- Context-window failures clear the saved SDK session before the next autonomy
  tick.
- Repeated context-window failures after session reset set a bridge-side
  cooldown, including for human-triggered messages, so the bridge does not spawn
  doomed SDK attempts every heartbeat.
- Usage-limit reset times are parsed and treated as scheduler pauses.
- Planner and reflection ticks use a bridge-side read-only tool gate.
- That gate explicitly permits dry-run/planning helpers such as
  `build_edge_miner`, `build_direct_smelter`, `repair_steam_power`, and
  `extend_power_to`, while still blocking actual execution-turn mutations such
  as `feed_lab_from_inventory(..., dry_run=false)`.
- Recent-event memory filters transient provider, timeout, and SDK failure
  noise.
- Recent-event prompt injection coalesces repeated adjacent failures and
  truncates oversized event text while preserving the latest useful non-failure
  event.
- Completion signals in recent useful events force the next autonomy tick back
  into read-only planning, even if later gameplay failures were journaled.
- Compact live-state entity counts can force a read-only planner tick when they
  prove a stale early-game objective is already physically built.
- Reflection memory is normalized before persistence and rendering: transient
  provider/session failures and fresh-start non-lessons are dropped, duplicate
  lessons collapse, and each durable structure or error tip is capped to a short
  prompt-safe line.
- Active SDK ticks have a bridge-owned stuck-tick watchdog for repeated
  identical gameplay rejections and long no-progress ticks.
- Live-state stale-plan detection now requires actual build/deployment intent
  before existing steam entities force another planner tick. Existing pump,
  boiler, and steam-engine evidence no longer prevents execution of repair
  objectives such as fueling the boiler or bridging a pole gap.
- Repeated "no change / plan validated / awaiting execution" planner progress
  is treated as low-value cadence noise in both journal and ledger memory. It
  remains available in raw logs, but it is not re-injected as durable progress
  on the next autonomy prompt.
- "No changes" phrasing is no longer a plan-complete signal. Only explicit
  objective/plan completion or "nothing left/to do" style wording can force a
  read-only re-plan from recent progress memory.

## Implemented Report Loop

- `just report` summarizes the latest structured bridge JSONL log.
- The report includes SDK attempts, successes, provider pauses, context resets,
  watchdog aborts, technologies researched, latest ledger state, entity counts
  observed in prompts, top gameplay rejection signatures, and an operator
  verdict.
- `just report` attempts fast, nonfatal live RCON enrichment by default. When a
  server is running, it includes current `live_state_line` entity counts and the
  mod's `get_power_status` summary from the save.
- `just report --no-live` remains available for pure log-only diagnosis.
- Research-status payloads such as `researched_count`, `research_progress`, and
  `research_queue` are counted as factory state, not gameplay rejection noise,
  even when old logs contain them inside a bad `game_rejected` prefix.
- Tool-contract/schema payloads such as `value for required field 'category' is
  missing`, `failed to deserialize`, `invalid type`, and `missing field` are
  classified as invalid requests and filtered out of top gameplay rejections,
  even when old logs contain them inside a stale `game_rejected` prefix.
- `power:` in the report ignores prompt echoes, aggregate `reply:` lines,
  tool-call lines, and read-only gate refusals. Structured power tool results
  are compacted into status, issue counts, issue types, and next action instead
  of being printed as truncated JSON or narrative map descriptions.
- Repeated "no change / plan validated / awaiting execution" planner chatter is
  not counted as `recent_progress` and cannot overwrite `latest_progress` in
  reports. The operator report now distinguishes real factory diagnosis from
  read-only planner churn.

## Implemented Placement Tools

- Placement diagnostics include blockers, occupied entity details, and
  same-entity `rotate_entity` recommendations.
- `rotate_entity` is exposed as an MCP/autonomy tool, backed by `placement.lua`,
  and covered by isolated runtime smoke on same-tile belt repair.
- Mining-drill placement search annotates candidate output belt tiles and
  prefers clear, buildable output positions before valid-but-annoying center
  placements.
- `build_edge_miner` is a read-only dry-run helper that chooses a
  resource-backed patch-edge drill placement with a clear, buildable output belt
  tile. It returns ordered `place_entity` steps for the drill and first output
  belt, reports missing drill/belt/fuel inventory, and leaves actual placement
  and fueling visible to the LLM/operator.
- `build_direct_smelter` is a read-only dry-run helper that accepts either a
  mining-drill unit number or an output belt tile from `build_edge_miner` /
  `get_machine_belt_positions`, then plans a checked belt, inserter, furnace,
  after-place fuel steps, missing inventory, and `verify_production` step without
  mutating the save.
- Belt placement failures include blocker bounding boxes, nearby alternate belt
  placements, and a compact `candidate_alternate_path` hint when an alternate
  tile is available.

## Implemented Power Tools

- `get_power_status` uses the Factorio 2.0 `get_flow_count` category API.
- `plan_steam_power` lives in the Factorio mod remote surface and returns a
  complete build plan from a water area and target position: offshore pump,
  boiler, steam engine, pipes, poles, fuel target, missing items, blockers, and
  exact `place_entity` arguments.
- `plan_steam_power` refuses to plan a rebuild over an existing steam plant and
  returns an `existing_plant` diagnostic with an `existing_steam_power_found`
  blocker.
- `diagnose_steam_power` returns compact plant status, issue counts,
  `next_action`, no-plant state, existing-plant state, boiler fuel issues,
  missing water/steam issues, pole-route issues, and guarded fluidbox-alignment
  repair hints.
- `repair_steam_power` is a read-only dry-run helper that consumes
  `diagnose_steam_power` and returns ordered low-level repair steps for safe
  cases such as boiler fuel and missing pole reach. It leaves uncertain fluid
  alignment as an explicit blocker.
- `extend_power_to` is a read-only dry-run helper that plans small-electric-pole
  extension from an existing pole to a target. It reports `no_power_grid_found`
  instead of inventing a source grid, reports missing poles from inventory, and
  emits ordered `place_entity` steps for the LLM/operator to execute.

## Implemented Research Tools

- `feed_lab_from_inventory(lab_unit, science_pack, count)` validates a specific
  lab unit and science pack before mutating.
- The helper defaults to `dry_run=true`, reports available character inventory,
  lab inventory before the transfer, missing items, blockers, and a guarded
  `dry_run=false` self-call step.
- With `dry_run=false`, it removes packs from the agent character inventory,
  inserts into the lab input inventory, returns any rejected remainder, and
  reports `lab_after` plus `inventory_after`.
- Missing packs, nonexistent labs, wrong entity unit numbers, and invalid lab
  inventories are explicit `expected_miss` results instead of warning-worthy
  tool failures.
- Planner/reflection read-only gating allows only the dry-run form; executing
  the transfer remains an execution-turn mutation.

## Higher-Level Factory Actions

Done:

- `repair_steam_power(area, target_pos)`
- `extend_power_to(area, target_pos)`
- `build_edge_miner(resource_name, target_area)`
- `build_direct_smelter(drill_unit_or_position)`
- `feed_lab_from_inventory(lab_unit, science_pack, count)`

Rules for the next helpers:

- Every action needs a dry-run or planning mode.
- Every action must return a compact success/failure report suitable for prompt
  memory.
- Actions should compose existing low-level tools instead of hiding errors.
- Actions should stay manual/operator tools unless explicitly exposed to the
  autonomous agent.

## Latest Provider-Free Verification

Run from this tree:

```sh
luac -p companion/mod/claude-interface/research.lua companion/mod/claude-interface/control.lua companion/mod/claude-interface/placement.lua companion/mod/claude-interface/power.lua
python -m py_compile tests/runtime_smoke.py companion/bridge/pipe.py companion/bridge/test_journal.py companion/bridge/test_skills.py
cargo fmt --check
cargo test -q --test lua_golden
cd companion/bridge && ../.venv/bin/python -m unittest test_journal test_skills
python -m unittest tests/test_runtime_smoke.py
bash -n tests/smoke.sh tests/setup.sh tests/cleanup.sh companion/run.sh
cargo test -q
cd companion && just smoke
```

Latest result:

- Lua syntax passed.
- Python bytecode compile passed.
- Rust formatting check passed after formatting.
- Lua golden tests passed: 41 checks.
- Bridge journal/skill tests passed in the project venv: 60 checks.
- Runtime smoke unit tests passed: 4 checks.
- Shell syntax checks passed.
- Full Rust test suite passed.
- `just smoke` passed 37 isolated runtime checks against a disposable save and
  synced mod copy.

Additional cadence verification after the latest log audit:

- `just report --no-live` and `just report` successfully summarized the latest
  run and exposed the real problem: fifty-plus repeated read-only planner ticks
  validating the same power-repair plan without advancing to execution.
- The same report audit exposed old false `game_rejected` noise for
  `get_research_status`; bridge classification and report parsing now treat
  unpowered-lab research status as normal diagnostic state while still counting
  `max_research_count`.
- The follow-up report audit exposed old `get_power_status` schema noise
  (`value for required field 'category' is missing`) in top gameplay
  rejections. Bridge classification now treats that as an invalid tool request,
  and report parsing filters both complete and truncated stale copies out of
  gameplay rejection signatures.
- `just report --no-live` and `just report` now show only real top gameplay
  blockers from the latest run: electric-drill placement, belt `create_entity`
  mismatch, and insufficient belts.
- Live-enriched `just report` confirms the current save state while provider
  capped: one burner drill, one electric drill, two furnaces, sixteen belts, one
  inserter, twenty-one poles, one steam plant, and one lab. `live_power` reports
  three consumers with no power and critical satisfaction, so the next live
  execution should repair/extend the grid rather than rebuild steam.
- `power:` now summarizes the latest structured steam diagnostic from the log:
  `steam_power status=critical`, two critical issues
  (`steam_engine_no_steam`, `boiler_no_fuel`), and
  `next=repair_existing_steam_power`.
- `progress:` now points at the last useful diagnosis
  (`boiler_no_fuel` plus the confirmed pole gap) instead of the old
  fifty-fourth read-only planning tick, and `recent_progress` is now `0` for
  that stale planner window.
- The current on-disk `doug-nauvis` ledger now renders without the old
  fifty-four repeated "planning tick" progress notes; future autonomy prompts
  keep the repair plan but stop feeding the model stale read-only-loop history.
- Added regressions proving "plan validated and ready for execution" does not
  force a planner tick, and low-value planning progress is dropped from loaded
  ledger state and future ledger updates.
- Added regressions proving report power diagnostics compact structured MCP
  tool results and ignore autonomy prompts, aggregate replies, tool calls, and
  read-only gate refusals.
- Added a report regression proving low-value validated-planner progress does
  not count as recent progress and does not replace the latest useful progress
  diagnosis.
- Added regression coverage for the exact shape of that run: an objective to
  energize an already-built steam grid must execute even when live state already
  contains an offshore pump, boiler, steam engine, lab, and poles.
- `cd companion/bridge && ../.venv/bin/python -m unittest test_journal test_planner test_report test_skills`
  passed: 87 checks.
- `cd companion/bridge && ../.venv/bin/python -m unittest test_ledger test_journal test_planner test_report test_skills`
  passed: 105 checks.
- `cd companion/bridge && ../.venv/bin/python -m unittest test_ledger test_journal test_planner test_report test_skills`
  passed: 107 checks.
- `cd companion/bridge && ../.venv/bin/python -m unittest test_ledger test_journal test_planner test_report test_skills`
  passed: 108 checks.

`just smoke` coverage includes:

- synced mod copy verification
- no-plant steam diagnostic
- no-plant steam repair fallback
- `extend_power_to` no-grid blocker
- `extend_power_to` existing-grid pole-extension dry-run steps
- `feed_lab_from_inventory` dry-run plan against a real lab
- `feed_lab_from_inventory` execution inserting science packs into the lab
- `feed_lab_from_inventory` missing-pack expected miss
- `feed_lab_from_inventory` wrong-entity expected miss
- disposable existing steam plant detection
- `boiler_no_fuel`
- `steam_engine_pole_route_incomplete`
- `repair_steam_power` fuel and pole repair steps
- existing-plant rebuild refusal
- placement repair diagnostics
- belt rotation
- drill-output placement ranking
- `build_edge_miner` dry-run selection on the generated iron patch, proving the
  selected drill placement is resource-backed and has a clear/buildable output
  belt tile
- `build_direct_smelter` dry-run planning from that selected edge-miner output
  tile, proving the synced mod can return ordered belt/furnace/inserter/fuel
  steps and a `verify_production` follow-up with seeded inventory
- `mine_at` resource semantics

## Live Proof Still Required

After the provider reset:

```sh
cd companion
just resume
```

Then inspect:

```sh
just report
```

Watch for at least:

- one planner tick
- one execution tick
- one recovery from a stale plan, placement rejection, or provider/session
  boundary
- no repeated context-window retry storm
- no repeated doomed pump/steam-engine rebuild loop
- no planner/reflection mutation
- evidence that Doug uses `diagnose_steam_power`, `repair_steam_power`, or
  `extend_power_to` instead of manually rediscovering the entire water/power
  problem
- evidence that Doug uses `feed_lab_from_inventory` instead of hand-rolling lab
  insertion loops after science packs are available
- no repeated read-only planner ticks after a stable, validated plan; the next
  tick after planning an existing-plant repair should be an execution tick

## Next Best Work

Run the unattended proof and let the report identify the next real bottleneck.

If the live run fails on power:

- use `just report` first
- inspect the exact `diagnose_steam_power`, `repair_steam_power`, and
  `extend_power_to` results in the log
- fix the helper contract or bridge classification before touching the prompt

If the live run gets past power but still stalls before clean iron automation:

- inspect the `build_edge_miner` result first
- if drill placement is now good but smelting still loops, inspect the
  `build_direct_smelter` result and fix the helper contract or bridge guidance
  before changing the prompt
- compare returned `missing_items`, `blockers`, `steps`, `after_place_steps`,
  and `verify_step` against the actual failing log

If the live run gets to science but stalls on manual lab feeding:

- inspect `feed_lab_from_inventory` dry-run and execute results first
- missing packs, wrong lab units, and invalid lab inventories should be
  `expected_miss`, not warning spam
- fix the helper contract or bridge classification before changing the prompt

## Done Means

Doug can run unattended overnight and either:

- make visible factory progress after power and early research, or
- stop cheaply with a compact, accurate reason that points at one next fix.

If the bridge spends the night repeating the same doomed tool call, resuming the
same dead SDK context, or telling itself obsolete starter facts, this goal is not
done.
