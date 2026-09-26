# Factorio Buddy correctness investigation

Status vocabulary: `hypothesis` (suspected from source, not run), `reproduced`
(the invariant failed when run), `fixed` (failed before the change and passed after it, on the same fixture),
`disproved` (the fixture shows the invariant holds), `blocked` (a prerequisite is missing).

Evidence lives outside Git in `/mnt/data/Code/Factorio/fb-evidence/` (copied from
the `/tmp` run directories named below) and `/mnt/data/Code/Factorio/fb-trials/`
(model trials). Model trial artifacts hold game chat and model output. They are
private local files. They hold no API keys or RCON passwords: every trial scrubs
them, and the scrubs recorded zero redactions.

## Environment

| Item | Value |
|---|---|
| Source revision | `b795601` (clean tree at start) plus the changes described below |
| Factorio | 2.0.77 (build 84539, linux64, Steam), headless. The server log shows `base`, `elevated-rails`, `quality`, `space-age`, `claude-interface` enabled |
| Claude Code | 2.1.281, model `claude-opus-5-5`. Subscription auth was verified with a no-tools request |
| Beads | `bd` 1.1.0 |
| Jev | `jev-1.13.0`, $0.042 / Mtok input, output free ([models](https://docs.typesafe.ai/models.md), rechecked during the run). **No `TYPESAFE_API_KEY` is available**, so real Jev requests are blocked |

## Baseline (unmodified `b795601`)

| Gate | Result | Artifact (`fb-evidence/…`) |
|---|---|---|
| `cargo test --all-targets --locked` | pass | — |
| `tests/buddy_runtime.sh` (fake Claude) | pass | `fb-baseline/buddy_runtime.log` |
| `tests/run_tests.sh` | pass | `fb-baseline/live/run_tests.log` |
| `scripts/smoke_agent_binding.sh` | pass | `fb-baseline/live/smoke_agent_binding.log` |
| `tests/live_regressions.sh` | **abort** after 38 passes (see B-1) | `fb-baseline/live/` |
| same suite with only the MCP read timeout raised to 600 s | 268 passed / 0 failed | `fb-baseline/live-t600/live_regressions.log` |

## Findings, priority order

Each row gives the source anchor at the baseline revision. The fixture that reproduces it is in the named
artifact directory. "Before" and "after" mean the same fixture run against baseline and against changed code.

### P1 — Server crashes and hangs (a single remote call can stop the world)

| ID | Status | Anchor | Invariant | Before → after | Evidence |
|---|---|---|---|---|---|
| G3 | fixed | `control.lua` `mine_at_impl` (`for _ = 1, count` with no validation) | A single call cannot hang the server | `count=1e308` with a full inventory: RCON stopped responding and the server needed SIGKILL; `1001` mined 1001; `2.5` mined 2 → now counts must be whole numbers from 1 to 1000, anything else is rejected before mutation (not clamped), and the server stays up. The same bound applies to `mine_nearest` | `fb-artifacts/Gameplay/before-liveness_mine`, `after-all` |
| G10 | fixed | `characters.lua` `set_walk_target` stored x/y without checks; `control.lua:process_walk_targets` does arithmetic on them every tick | Malformed input never reaches tick state | `{"fn":"set_walk_target","args":[agent,"abc",0,null]}` → a non-recoverable mod error on the next tick, and the server quit → now rejected as `invalid_walk_target` (also `null`, ±`1e999`) | `before-liveness_walk/server.log` |
| G11 | fixed | `/claude` dispatcher ran `table.unpack(args,1,n)` and `rcon.print` outside `pcall` | The dispatcher always returns a structured reply | `n=1e9` → "too many results to unpack", and the server quit → `n` must be an integer from 0 to 32, the whole dispatch runs inside `pcall`, and `json_response.encode_result` keeps each type on the wire as before (booleans and numbers unchanged). Factorio objects return `unserializable_result` | `before-liveness_dispatch` |

### P2 — Item conservation

| ID | Status | Anchor | Before → after | Evidence |
|---|---|---|---|---|
| G1 | fixed | `control.lua` `set_recipe_impl` discarded `LuaEntity.set_recipe`'s return value. In Factorio 2.0.77 that value is an `ItemWithQualityCount[]` of the unloaded items, confirmed live | Changing or clearing a loaded assembler deleted everything in it (100 plates + 10 gears; uncommon-quality plates too) → the items go to the NPC, any remainder is spilled at the machine, and quality is preserved. Machine + NPC + ground totals are conserved in every probe. Re-selecting the current recipe does nothing | `Gameplay/before-main/records/set_recipe.log` vs `after-all/…` |
| G2 | fixed | same; no validation before mutation | A disabled recipe (`steel-chest`) was accepted and its 20 plates vanished. An incompatible recipe returned an error only after Factorio had already emptied the machine → now checked before any mutation: `unknown_recipe`, `recipe_disabled` (via `character.force.recipes`), `recipe_incompatible`, `recipe_fixed`. Clearing with null or `""` still works | same |
| G4 | fixed | `mine_at` force-mined into a full inventory | Ore was spilled and the call reported a false "nothing to mine" → now `inventory_full` with the world unchanged | same |
| G5 | fixed | `insert_items`/`extract_items` count validation | `2.5` moved 2, and extract `0` "succeeded" → shared `validate_count`; extract no longer drops an item it cannot put back | same |

The oversized count `1e9` on insert/extract moves only the items that exist and reports
`requested`/`available`. This is a reported partial transfer, not silent clamping.
Mining is still instant (up to 1000 units in one call). This is a harness shortcut, not human-speed mining.

### P3 — False success and unsafe overlap

| ID | Status | Anchor | Before → after | Evidence |
|---|---|---|---|---|
| MS-2 | fixed | `mcp.rs` `production_observation_json`, `production_verification_summary`, `verify_production` | A request aimed at stalled furnace B returned `success:true` because neighbor A's `products_finished` rose. Controller summaries copied that area-wide proof to their target → each unit now reports `currently_working` and `sustained_progress` separately. The optional `unit_number` scopes success to that unit. Drills report `evidence:"status_only", benchmark_grade:false` | `McpServer/section-before` vs `section-after` |
| MS-3 | fixed | rmcp runs each request as its own task, and `FactorioClient` serializes only individual RCON packets | In a pipelined `walk_to` + `place_entity` + `get_entities`, the placement ran mid-walk and failed `out_of_reach` → one operation mutex in `ServerHandler::call_tool` now covers every tool except 22 audited observation tools (fail-closed). `find_nearest_resource` bypasses the lock unless `explore_radius` is set, because only exploring generates chunks (unit test `chunk_generating_resource_search_takes_the_operation_lock`). Mutations run in request order, and observations are not blocked. Separate MCP processes or controllers can still interfere; there is no distributed transaction | same |
| MS-4 | reproduced; wording corrected | killing MCP during `route_belt` | 12–13 orphaned belts remained, and one placement that was already in flight landed after the kill. A blind retry fails with "Start position is blocked". Cancellation never rolls back. Server instructions and five tool descriptions no longer claim atomicity. Rollback on ordinary controller-detected errors is unchanged (MS-5: an obstacle added after preflight → all 82 belts rolled back and the inventory restored) | `McpServer/probes*` |

### P4 — Runtime delivery and interruption (Buddy)

| ID | Status | Anchor | Before → after | Evidence |
|---|---|---|---|---|
| R1 | fixed | `buddy.rs` `run_buddy` cancelled any active turn on chat | A second human message killed the first human request, and A never got a reply → human turns now run FIFO. Chat only preempts autonomy or maintenance, and interrupted human mutations are never replayed | `BuddyRuntime/baseline-human-fifo-*` |
| R2 | fixed | `pending.clear()` on provider limit | Queued humans were dropped without a reply. The baseline drop is inferred from source, because R1 masked it in the fixture → each queued request gets `PROVIDER_LIMIT_MESSAGE` once, with no busy loop | `baseline-limit-queue-*` |
| R3 | fixed | `invoke_claude` discarded stderr | An invalid resume reported only on stderr ("No conversation found") got no retry → a bounded stderr tail (20 lines / 4 KiB) now reaches `is_invalid_session_error`, which gives exactly one fresh-session retry. Tool errors do not match | `baseline-stale-session-*` |
| R4 | fixed | `kill_on_drop` only killed the direct child | A cancelled turn left its MCP grandchild running → Claude now runs in its own process group. The group gets SIGTERM, then SIGKILL, and Buddy waits until it is gone before the next turn. Tools still in flight are reported in the next prompt as `OUTCOME UNKNOWN — inspect before acting, do not retry blindly` | `baseline-interrupt-*` |
| R5 | fixed | no `--model` passed | Claude's own default was used → `claude-opus-5-5` by default, still overridable through `--model`/`MODEL`. Verified from the argv the fake CLI observed | `buddy_runtime.sh` |
| MS-1 | fixed | `mcp.rs` `with_player_messages` (344 call sites) | Console chat reached both the JSONL inbox and the next tool result (`--- Player Messages ---`) → the JSONL bridge is the only ingress. The wrapper, `ChatMessage`, the chat remotes/storage, the `LuaCommand` builders and golden cases are removed. The migration clears the retired table | `McpServer/section-*` |
| R6 | fixed (found by the model trials) | `buddy.rs` `is_provider_usage_limit` matched only the weekly/"usage limit reached" wording | Claude Code 2.1.281 ended invocations with `{"is_error":true,"api_error_status":429,"result":"You've hit your session limit · resets 4am (America/New_York)"}`. Buddy treated each one as an ordinary failed turn and started the next heartbeat turn 5 s later. It burned all 30 autonomous turns in about 3 minutes (`fb-trials/provider-limited/`) → an `api_error_status` of 429, or the session-limit wording, now backs off like the documented limit. Unit test plus the live `limit-queue` scenario, which now uses the exact observed payload | `fb-integrated/limit-queue.log` |

### P5 — Error semantics

| ID | Status | Anchor | Before → after |
|---|---|---|---|
| G6 | fixed | `client/mod.rs` `parse_lua_array` | A missing agent surfaced as `invalid type: map, expected a sequence`, and `""` became an empty world → `LuaRemoteError {error, error_kind, payload}`. Empty or malformed replies are errors |
| G7 | fixed | placement parsers used `contains("\"error\"")` | Blocker details were lost → `parse_lua_result` works on parsed fields, and callers can use `downcast_ref::<LuaRemoteError>()` |
| G8 | fixed | `set_recipe` missing entity | now `error_kind:"entity_not_found"` |
| G9 | fixed | `characters.lua` `require_entity_reach` | Resources reported reach 10 while Factorio enforces 2.7 → now reports 2.7 |

### P6 — Harness defects that hid regressions

| ID | Status | Anchor | Detail |
|---|---|---|---|
| B-1 | fixed | `tests/live_regressions.sh:154` fixed 20 s read timeout | The cold-coal `repair_fuel_sustainability` execution is slow but correct: 22.8 s, twice (strace: 12.9 s real-time walking over ~85 tiles and 19 waypoints, 5.4 s delivery observation over 240 ticks, 3.0 s production observation over 180 ticks, 5.0 s total RCON). The timeout aborted the default run after 38 of 268 assertions. The timeout is now set per call (90 s for this call, with the measurement in a comment) and times out loudly |
| MS-0b | fixed | `stop_mcp` ran `exec {fd}>&- 2>/dev/null` | That redirected the script's stderr for good, so every later `FAIL` line vanished (this is why the baseline abort was silent) |
| I-1 | fixed | compound-rollback fixture in `live_regressions.sh` | The fixture forced `copper-cable` onto a machine although Space Age starts that recipe locked. G2's validation then (correctly) rejected restoring it. The fixture now unlocks the recipe as normal play would |

### P7 — Second-round defects found by adversarial review of this change set

Two read-only reviewers audited the integrated diff before the model matrix was run. The matrix that had already started was stopped, and its partial runs were moved to `fb-trials/aborted-pre-audit/` and excluded from results. Everything below was fixed before the matrix restarted.

| ID | Status | Detail | Verification |
|---|---|---|---|
| A-R1 | fixed | The invalid-session retry could replay a whole prompt after the first attempt had already called mutating tools | A fresh session is retried only when the first attempt made zero tool calls; otherwise the turn fails and the next prompt gets the OUTCOME UNKNOWN note. Live scenario `stale-after-tool` |
| A-R2 | fixed | Maintenance ran the controller again without checking which consumer it actually repaired | `maintenance_result` records `transaction_changed` / `actual_consumer_unit_number`, and failures are attributed to the actual consumer |
| A-R3 | fixed | The OUTCOME UNKNOWN note was consumed before delivery, so a cancelled turn lost it | The note is acknowledged only at Claude's first stream event or on a completion that was not cancelled |
| A-R4 | fixed | The repeated-failure detector required failures to be adjacent, so it missed fail→inspect→fail loops | Failures are counted since the last success of the same tool and arguments (unit test) |
| A-R5/6 | fixed | Maintenance turns were logged as succeeded even when the repair failed; `latency_ms: 0` was logged when no Jev request was made | `succeeded` is now `null` or the repair outcome, and latency is `null`. The eval counts model turns separately from maintenance turns |
| A-R7 | fixed | Cancelling during a reap skipped the wait for the process group to die; `/proc` scans blocked the runtime | The group stays recorded until death is confirmed, and scans run in `spawn_blocking`. A theoretical window after the scan and before `kill` needs a PID wrap-around; it is documented |
| A-R8 | fixed | A follow-up Opus turn could start after the deadline | The deadline is also checked when a turn is taken from the queue |
| A-R9 | fixed | A Jev reply whose `choice` was not the most probable option passed validation | Such replies are rejected |
| A-R10 | fixed | Deterministic failure memory was never reset and was keyed on the transaction instead of the consumer | Keyed on the consumer and cleared after each Opus turn, as the plan specifies |
| A-R12 | fixed | Up to 4 KiB of Claude stderr went into the in-game panel | Only the first line (≤180 bytes) reaches the panel |
| A-G1 | fixed | The client and MCP layers dropped the `set_recipe`/`extract_items` conservation evidence, so the model saw "Recipe set" while 110 items lay on the ground | The tools return the structured JSON (`returned_items`, `spilled`, `previous_recipe`, `error_kind`). Probe: the reported spilled count equals the ground delta |
| A-G2 | fixed | Rust walked the NPC before the Lua validation rejected a count or recipe | A read-only `check_recipe_choice` remote and count validation now run before any walk. Probe: the NPC's position is unchanged on rejection |
| A-G3 | fixed | The compatibility check read `recipe.category`, ignoring `additional_categories`; that field is removed in Factorio 2.1 | Uses `has_category`. No installed 2.0.77 recipe uses `additional_categories`, so this is a forward-compatibility fix only |
| A-G4 | fixed | A full inventory with a multi-item product (tree, rock) was reported as `mine_failed` | A scratch-inventory insertion test now returns `inventory_full` |
| A-G5 | fixed | `give_or_spill` passed `force`, which marked every spilled item for deconstruction | `force` removed. Probe: `ground_marked == 0` (before: 2) |
| A-G6 | fixed | `evaluation_sample` reported a furnace's previous recipe as its current one | Separate `previous_recipe` field |
| A-G7 | fixed | The migration that clears chat storage never ran because the mod version was unchanged | Mod version 1.1.0 → 1.1.1. A seeded save loses `storage.chat_messages` on load |

Before/after probe totals for this round: the pre-audit code gives 101 PASS / 20 FAIL; after the fixes, 173 / 0 (`fb-artifacts/FixGameplay/`).

## Final deterministic and live gates (integrated tree)

| Gate | Result | Artifact |
|---|---|---|
| `cargo fmt --check`, `cargo clippy --all-targets --locked -D warnings` | clean | — |
| `cargo test --all-targets --locked` (includes `lua_golden`) | pass | — |
| `luac -p mod/claude-interface/*.lua` | pass | — |
| `tests/run_tests.sh`, `scripts/smoke_agent_binding.sh` | pass | `fb-integrated/live3/` |
| `tests/live_regressions.sh` (isolated server, explicit env, no dotenv) | **452 passed / 0 failed** (baseline: 268, most of it hidden behind B-1) | `fb-integrated/live4/live_regressions.log` (final tree, after the lock refinement below) |
| `tests/buddy_runtime.sh` (fake Claude, zero players) | **21/21** (FIFO, limit replies, stderr session reset, interruption reconciliation, reaping, budget/deadline, Jev credential/decline paths) | `fb-integrated/buddy_runtime4.log` (includes the R6 payload) |

## Measured affordances and capability gaps (not changed)

- Observation is global: `find_entities` returns entities in chunks the force has never charted. There is no fog of war.
- Cross-force access: the NPC can `extract_items` from another force's chest. No documented contract forbids it.
- Navigation: the walker does not path around obstacles. A walk into a wall ends `arrived:false, reason:"stuck"`, and the receipt says so truthfully.
- Rails cannot be placed (`rail` item vs `straight-rail` entity). Trains, fluids, and robot logistics are out of scope.
- Multi-agent binding stays isolated (probe and `smoke_agent_binding.sh`).
- Peaceful managed map, instant mining, and synchronous controllers. These are harness shortcuts, so no trial shows human-equivalent control or performance in a hostile world.

## Unattended model trials (`tests/autonomous_eval.sh`)

Fixed settings for every trial: `claude-opus-5-5`, effort medium, heartbeat 5 s, turn timeout 300 s, game speed 1, and an autonomy budget of 30 turns or 900 s. After the budget, the model is stopped and two 3600-tick holdout windows are measured with `evaluation_sample`. Every trial uses a fresh isolated HOME, save, write-data, and disposable Beads tracker.

Every trial below had zero connected players, a clean shutdown with `clean_shutdown=true`, no credential in any artifact, and no changes to the real tracker. "Cost" is Claude Code's own estimate, taken per session (the reported value is cumulative). Usage came from the subscription, not metered billing.

### Valid trials on the final code (one repetition each)

| Trial | Arm | Scenario / case | Model turns | Tool calls / errors | Interrupted tools | Plates both windows | Powered / science / research | Fuel outcome | Cost est. |
|---|---|---|---|---|---|---|---|---|---|
| p-opus-open-2590060468 | opus | open-play | 3 | 199 / 14 | 8 | yes | no / no / no | — | $2.20 |
| p-det-open-2590060468 | deterministic | open-play | 3 | 134 / 18 | 2 | no | no / no / no | — | $1.19 |
| p-jev-open-2590060468 | jev | open-play | 3 | 127 / 21 | 1 | no | no / no / no | — | $1.16 |
| p-opus-open-12345 | opus | open-play | 3 | 175 / 32 | 0 | yes | no / no / no | — | $2.84 |
| p-det-open-12345 | deterministic | open-play | 3 | 187 / 19 | 2 | yes | no / no / no | — | $2.24 |
| p-opus-open-2590060469 | opus | open-play | 3 | 198 / 18 | 6 | yes | no / no / no | — | $1.62 |
| p-det-open-2590060469 | deterministic | open-play | 5 | 165 / 15 | 0 | yes | no / no / no | — | $4.23 |
| p-opus-lab-missing-feed | opus | lab missing-feed | 5 | 136 / 17 | 0 | yes | — | recovered, 293 s | $5.93 |
| p-det-lab-missing-feed | deterministic | lab missing-feed | 7 | 92 / 15 | 0 | yes | — | recovered, 729 s (the maintenance controller executed once and succeeded) | $4.78 |
| p-jevshadow-lab-missing-feed | jev-shadow | lab missing-feed | 9 | 192 / 18 | 0 | yes | — | recovered, 509 s (by Opus) | $9.20 |
| p-jev-lab-missing-feed | jev | lab missing-feed | 4 | 154 / 12 | 0 | yes | — | recovered, 53 s (by Opus) | $4.73 |
| p-opus-lab-already-sustainable | opus | lab already-sustainable | 9 | 206 / 16 | 0 | yes | — | case failed: the fixture was not left intact and durably fed | $8.25 |
| p-jev-lab-already-sustainable | jev | lab already-sustainable | 10 | 181 / 24 | 2 | yes | — | case failed: the fixture was not left intact and durably fed | $7.34 |
| q-jev-lab-stale-consumer | jev | lab stale-consumer | 10 | 188 / 25 | 1 | yes | — | recovered, 23 s (by Opus) | $7.92 |

Excluded from all comparisons, with their evidence kept:
- `fb-trials/provider-limited/`: runs caught by the R6 subscription limit.
- `q-det-lab-already-sustainable-r2`: hit the limit again and is flagged `provider_limited` by the runner.
- `p-det-lab-already-sustainable`: every Claude tool call failed with "No such tool available". Opus called bare names such as `place_entity` instead of `mcp__factorio__place_entity` on all 30 turns.
- `aborted-pre-audit/`: stopped at the review round, before the P7 fixes.
- The pilots `opus-pilot-*`, `det-pilot-*`, and `prefix-opus-open-*`: `prefix-opus-open` ran new Buddy code against the pre-fix gameplay code and got plates in both windows.

**Finding A-P1 (fixed):** the tool-naming failure in `p-det-lab-already-sustainable` was a prompt defect. Snapshots and tool results name tools without the `mcp__factorio__` prefix, and in one session Opus copied the bare names. The system prompt now states the prefix. No tool call in the two trials run after the fix failed this way.

**Sample limits.** The plan's matrix is 3 seeds × 3 repetitions × 3 arms plus 8 lab cases × 3 repetitions × 3 arms, with held-out layouts. The Claude subscription allowed roughly 4–10 fifteen-minute Opus trials per 5-hour window before `You've hit your session limit` (twice during this run). What is reported here is one repetition per cell for the cells that ran. The comparison is descriptive, not statistical. All 8 lab cases and both layouts are live-validated with `--fixture-only` (see `findings-EvalRunner`). The remaining cells need more subscription windows and no code changes.

### Rendered final state

`render_map` on the final save of `p-opus-open-2590060468`:
- ten burner drills feeding furnaces directly on the iron patch at (−47, 8…15);
- a boiler and steam engine at (45, −14) with no electric consumers.

The powered-production milestone was therefore correctly `no`. The text renders are in `render-main/` and `render-power/`. The PNG output of `render_map` is empty (0 bytes), so the ASCII render and the numeric samples are the evidence.

### Jev decision (plan §6–7)

- **In Opus trials, Jev was never consulted.** Across the 5 Opus-planned jev/jev-shadow trials there were 35 maintenance opportunities, and none was eligible: the dry run returned `no_ready_fuel_transaction` or a failed preview. Opus's startup turn, which the plan requires so maintenance cannot starve expansion, had already repaired or re-planned the fuel every time. Jev spend was $0.
- **Component replay (`--planner noop`).** To reach the decision path, a stub planner returns every turn without tool calls. The prepared missing-feed defect therefore survives to Buddy's maintenance check. These runs are labelled as component replay and are never counted as planner results.

  | Replay | Real Jev Choice responses | Jev answer | Selection | Maintenance executions | Fuel recovered (both holdout windows) |
  |---|---|---|---|---|---|
  | `r-jevshadow-noop-missing-feed` | 29 of 29 eligible, `jev-1.13.0`, latency 117–377 ms (median 155), 135 760 input tokens, ≈ $0.006 | always `repair_fuel`, confidence 0.12–0.41 | shadow: always return to planner | 0 | no. Shadow mode made no world change |
  | `r-jev-noop-missing-feed` | 29 of 29, latency 118–305 ms (median 143), ≈ $0.006 | always `repair_fuel`, confidence 0.21–0.43 | below the 0.90 threshold: always return to planner | 0 | no |
  | `r-det-noop-missing-feed` | none (deterministic) | — | repair on 4 eligible previews | 2 succeeded (consumers 11, 12). 2 were refused because the refreshed preview no longer matched, so the pre-execution identity check worked live | **yes**, 72 s to the first verified repair, sustained in both windows |

- **What this verifies:**
  - Buddy builds, sends, validates, and records real Jev Choice responses.
  - Shadow mode never mutates.
  - Active mode refuses low-confidence answers.
  - The shared execution path (preview refresh, identity check, controller run, actual-consumer attribution) repairs the world and the repair holds through the holdout.
- **What stays unverified:** a repair *selected by Jev* has never run. Jev's confidence in this state never came close to the 0.90 threshold; the maximum was 0.43. Lowering the threshold to force the path would change the experiment, so I did not.
- **Promotion rule not met.** Active Jev recovered 0 of 1 fixture, against 1 of 1 for the deterministic replay and 1 of 1 for every Opus-planned missing-feed trial. The rule needs no fewer successes and ≥ 20 % faster recovery than both comparators.
- **Decision.** Keep `--decision-mode off` as the default and recommend against using Jev in production. The opt-in mode's safety paths have been exercised: a real shadow Choice, low-confidence refusal, and fake-HTTP tests for credentials, malformed replies, a non-maximal choice, timeouts, 429, and budget. Its only mutating path, a Jev-chosen repair, is unverified. Choice confidence measures how concentrated the probabilities are, not whether the answer is correct, so the low values here are not evidence that the input was unclear. Any follow-up must compare Jev's answers with observed repair outcomes on held-out, actually logged decision states before changing the question or the gate.
- **Question-shape smoke only.** `fb-evidence/jev-question-probe/jev_probe.py` sent 12 requests on four hand-written, answer-labelled situations. With rule-style criteria, every answer matched its label at confidence ≥ 0.96. Adding two playbook entries changed nothing measurable. The situations are synthetic and are not Buddy's logged state, so this shows neither a knowledge-base benefit nor a real-state improvement. The rules it used are fully decidable by code, which is the deterministic arm.

### Where the bottleneck is

1. **The planner turn budget.**
   - Opus turns run 60–70 tool calls, and the 300 s turn timeout cut 2–3 of the ~3 turns in most open-play trials.
   - Each cut leaves outcome-unknown tools (up to 8 per trial). R4 now reports these to the next turn truthfully.
   - In 15 minutes only about 3 planner turns complete. No trial reached powered production, science delivery, or research.
2. **Repeated ineffective manual actions.** `bootstrap_smelting_once` and `bootstrap_burner_once` together made up about 40 calls in some trials, despite guidance that they are one-shot bootstrap tools. They succeed, so the repeated-failure hint (which counts failures only) correctly does not fire.
3. **Tool latency.** Controllers walk in real time: `repair_fuel_sustainability` took 22.8 s, 12.9 s of it walking. Tool time exceeds model time in the fuel lab.
4. **Controller correctness is no longer the main limit.** No conservation, false-success, or unsafe-overlap violation was observed in any valid trial.

## Making Buddy play better (after the investigation)

### Why the baseline never reached electricity

Evidence from the 7 valid open-play trials above (1 283 tool calls):

- **Wall time went to movement and waiting.** `walk_to` 25 %, `wait_for_crafting` 14 %, `bootstrap_smelting_once` 11 % and `mine_at` 10 % of the time between tool results (this includes model time before each call).
- **Steam power was a 20-call chore.** `plan_steam_power` only plans. A plant then needed a craft and wait per part, a walk and `place_entity` per build step (up to 21 poles), and a separate fuel step. It was called 4 times, and only one trial placed any of it.
- **Affordance defects.**
  - The model-visible `place_entity` never walked, which caused 20 `out_of_reach` errors.
  - Water is a tile, so `find_nearest_resource water` raised "Unknown entity name" (3 times). No tool could locate water.
  - `bootstrap_burner_once` rejected stone furnaces and boilers (14 errors).
  - `collect_from_chest` rejected furnaces (8 errors), so the only way to take plates out was the 7-step `bootstrap_smelting_once`.
- **The prompt pushed the wrong priority.** The directive demanded durable automation first. Opus spent whole turns routing coal belts to burner drills (`route_belt` 25 errors out of 72) before it had electricity. Nothing told it the Factorio 2.0 trigger tree: 50 iron plates unlock steam power, 10 copper plates unlock electronics, and crafting a lab unlocks red science. Trigger technologies were verified live on this map.

### Changes

| Change | Where | Verified by |
|---|---|---|
| `build_steam_power`: finds water, plans, hand-crafts the missing parts, places every step (walking), and fuels the boiler. A crafting failure places nothing; a placement failure removes everything it placed. | `mcp.rs` `build_steam_power_transaction` | Live suite: no wood → `craft_failed` with the world unchanged. With wood → only the poles are crafted, 1 pump, 1 boiler and 1 engine are placed, the boiler holds ≤ 20 coal, and the engine is on a network. Dev sandbox: 50 s from raw plates to a working plant. |
| `find_nearest_resource` with `resource_type:"water"` searches water tiles in expanding squares and returns `steam_power_water_box`. | `world.lua` `find_nearest_water` | Sandbox: the water it found on seed 2590060468 matches the pump position in the old trial. |
| `place_entity` walks into build reach and retries once, only on `out_of_reach`. Other rejections return unchanged, and the Lua remote still refuses reach violations. | `mcp.rs` `place_entity`, `client.approach_build_position` | Sandbox: placement 40 tiles away took 6.6 s. The live suite's out-of-reach and serialization checks still pass. |
| `bootstrap_burner_once` accepts burner furnaces and boilers, up to 50 fuel. `collect_from_chest` reads a furnace's output slot. | `inventory_actions.lua` | Sandbox: fuel conserved; plates collected from a furnace. Live cap check updated to 51 → `count_exceeds_limit`. |
| The snapshot has a `progression` block: trigger counts from force production statistics, steam engines, boiler fuel, labs (powered/working), red-science assemblers, low-fuel burner drills and furnaces, one `next_goal` with `how`, and `warnings`. | `autonomy.lua` `progression` | Sandbox: fresh map → "Smelt 50 iron plates … (0/50)". A furnace with 2 coal → a low-fuel warning with its unit number. |
| The autonomy directive follows `progression.next_goal`. Bounded hand-fuelling is correct until research runs, and one-call controllers are preferred. | `buddy.rs` `AUTONOMY_DIRECTIVE`, system prompt | Trials below. |
| **General lever:** `build_layout` builds a design that Opus writes itself (entities with dx/dy, direction, optional recipe). It crafts what is missing, mines trees and rocks in the footprint, steps the character out of the way, places everything in order, and sets recipes. Entities already built identically are kept, so a repeated call resumes. A placement failure removes everything this call placed. | `mcp.rs` `build_layout_transaction` | Live suite: a colliding third entity → `placement_failed` at index 2 and 0 chests left; the valid layout → 2 placed; the repeat → 0 placed, `already_built: 2`. Sandbox: a 6-entity gear/science block in 1.1 s; a rollback removed 3 inserters. |
| **General lever:** `progression.rocket_path` walks the real tech tree to `rocket-silo`. It lists the technologies left (25 from automation on Space Age Nauvis), the ones researchable now, cheapest first, with their packs or trigger, and each needed science pack with its assembler count. After the opening rungs, `next_goal` becomes: automate the first needed pack that has no assembler (with `build_layout`), else start the next path technology, else scale. Hand-written late rungs were removed. | `autonomy.lua` `rocket_path` | Sandbox on the g2 final save: 25 technologies left, `steel-processing` and `logistic-science-pack` researchable, red science needed with 1 assembler. |
| `--budget-minutes N` in the runner: the deadline scales for long runs toward the rocket (turns `max(30, 2N)`, hard wall deadline + 300 s). The default of 15 keeps the numbers identical. | `tests/autonomous_eval.sh` | Flag validation rejects 0. |

Gates after these changes: `cargo fmt`, `clippy -D warnings`, and all cargo tests pass; `luac` is clean; `live_regressions.sh` has 462 passed and 0 failed (5 new steam-power and 5 new layout checks). The model-visible tool schema stays under 60 KiB: mirrored per-coordinate Y docs were removed so the extra tools fit.

### Results (same runner, model, effort and budget)

| Trial | Steam power built | Research started | Research | Plates both windows | Research progressed both windows | Cost est. |
|---|---|---|---|---|---|---|
| baseline, 7 trials | 0 / 7 | 0 / 7 | none | 5 / 7 | 0 / 7 | $1.16–4.23 |
| `g1-opus-open-2590060468` | t + 301 s | automation t + 507 s | automation done, logistics 45 % | no (burner drills out of fuel) | **yes** | $3.98 |
| `g2-opus-open-12345` | t + 325 s | automation t + 425 s | automation done, logistics 50 % | yes | no (the lab ran out of hand-fed packs) | $3.22 |

Both trials then started automating red science by hand, with assemblers and inserters.
- In g1, the science assembler made 6 packs in the first holdout window. Then its iron supply stopped because every burner drill had been given only 3–10 coal and ran dry.
- In g2, the science assembler made 5 packs, but no inserter moved them to the lab (`full_output`).

Neither trial reached `automated_science_delivery` or `powered_production`. The frontier has moved from "never gets electricity" to "keeping the science chain fuelled and connected". g1 ran before the low-fuel warnings existed; g2 ran with them. Neither had `build_layout` or `rocket_path`. Sample: one trial per seed, so this is descriptive.

Excluded: `g3-opus-open-2590060469` ran while `build_layout` was being built. Claude starts a fresh MCP process every turn, so later turns used the new binary while Buddy's prompt and the server's Lua were old. Mixed code, so it does not count. Unprompted, Opus called `build_layout` 3 times in it: a dry run and a build of a 10-entity drill/furnace row, then a 5-furnace row.

With every change in place (`build_layout`, `rocket_path`, and the updated directive):

| Trial | Steam power built | Research | Plates | Powered production | Automated science delivery | Research both windows | Turns | Cost est. |
|---|---|---|---|---|---|---|---|---|
| `g4-opus-open-2590060469` | `build_steam_power` at t + 452 s | automation (t + 529 s), then `steel-processing`, the first technology on the rocket path | yes | **yes** | **yes** | **yes** | 3 | $1.44 |

- Opus called `build_layout` 8 times: furnace rows, burner-drill rows, and a gear assembler → red-science assembler → lab block.
- In both holdout windows, the gear assembler and the science assembler each made 6 items, and the lab kept researching `steel-processing` (2 % → 12 % → 25 %) with no character inventory change.
- This is the first trial in this investigation to reach every open-play milestone. It is one run on one seed, so it shows that the path is possible, not how often it succeeds.

### First long run toward the rocket (`--budget-minutes 60`)

`long1-opus-open-12345-60m`: same code as g4, seed 12345, 18 turns, $17.11 estimated.

- **Research started, in order:** automation (7 min), logistic-science-pack (10), electric-mining-drill (17), steel-processing (25), logistics (31), fast-inserter (37). Then several technologies off the rocket path (gun-turret, military, radar, repair-pack, stone-wall, lamp, 42–49 min), then automation-2 (52 min, 61 % → 81 % during the holdout).
- **Automated production:** 9 assemblers produced items in the holdout windows: red science, **green (logistic) science**, gears, inserters, belts, circuits and cable. Opus placed them with about 30 `build_layout` calls, including a 6-assembler green-science block with 11 inserters and 20 belts.
- **Milestones:** plate automation, powered production and research progress in both windows. `automated_science_delivery` was **not** credited: in window 2 one lab's pack count fell by 1 while it researched. The metric requires the lab stock not to fall, so it cannot separate steady consumption from a starving lab. It is left strict.

Walls this run exposed, and what was done:

| Wall | Evidence | Action |
|---|---|---|
| A craft the model started and never waited on blocked `build_layout`'s crafting | "resolve the existing craft admission with wait_for_crafting" | `craft_item_shortfalls` finishes a pending admission first |
| The character stood on its own layout tile | `Placement overlaps agent character` for a pole | `build_layout` steps outside the layout and retries once |
| Research went off the path | 6 technologies unrelated to the rocket at 42–49 min | Directive: queue research only from `rocket_path.researchable_now` unless a build needs it now |
| 6 of 18 turns hit the 300 s cut | turns of 300 s with 16–69 calls; interrupted tools become outcome-unknown | Directive: end the turn after about 25 calls or once `next_goal` is done |
| Navigation stuck inside the grown base | 7 `collect_from_chest`/`mine_at` failures: "stuck; target is out of character reach" | A failed A* search is retried once over a 48-tile collision window before the straight-walk fallback. The model-visible `walk_to` now uses A* (it was straight-line only). Sandbox: a 41-tile walk arrived; live walk checks pass |
| `route_belt` pathing failures | 13 of about 60 calls ("No path found", blocked goal) | Not fixed yet |

### Second long run, after those fixes (`long2-opus-open-2590060468-60m`)

Seed 2590060468, 60 minutes, 24 turns, $22.16 estimated.

| | long1 (before fixes) | long2 (after fixes) |
|---|---|---|
| Milestones (plates / powered / science delivery / research) | yes / yes / no / yes | **yes / yes / yes / yes** |
| Turns cut at 300 s | 6 of 18 | 3 of 25 |
| Tool errors | 32 | 18 |
| Off-path research | 6 technologies | none |
| Research order | automation → logistic-science-pack → electric-mining-drill → steel → logistics → fast-inserter → (detour) → automation-2 | automation (5 min) → logistic-science-pack (9) → electric-mining-drill (19) → steel (22) → logistics + fast-inserter (31) → automation-2 (54) → advanced-material-processing |
| Holdout assemblers producing | 9 | 9: red and green science, gears, belts, inserters, circuits, cable |

Next wall: the factory still runs on the character's hands. The long2 run made 133 `bootstrap_burner_once` calls and 58 `collect_from_chest` calls, 191 of 432 tool calls (44 %). Research pace is also bounded by one red and one green science assembler. Two general changes follow from this:
- **Manual-logistics pressure.** Buddy now counts successful hand transfers (fuel, plates, packs) over the last 60 tool calls. At 30 % or more, the autonomy prompt says so and asks the model to automate the most repeated transfer. This is session feedback, not a ban. A unit test pins the boundary: 17 of 60 → silent, 18 → alert; failed transfers and sessions under 60 calls never alert.
- The fallback rung of the ladder already says to scale throughput; `rocket_path` shows what is left.

### Third long run, with the manual-logistics signal (`long3-opus-open-12345-60m`)

Same seed as long1, 60 minutes, 23 turns, $23.56 estimated, no provider limit.

| | long1 (seed 12345) | long2 (seed 2590060468) | long3 (seed 12345) |
|---|---|---|---|
| Milestones (plates / powered / science delivery / research) | yes / yes / no / yes | yes / yes / yes / yes | yes / yes / yes / yes |
| Hand transfers / tool calls | 227 / 532 (43 %) | 198 / 432 (46 %) | **134 / 429 (31 %)** |
| Turns cut at 300 s | 6 / 18 | 3 / 25 | 3 / 24 |
| `automation-2` started | 52 min | 54 min | 46 min |

The alert fired on most turns. The count of hand transfers in the 60-call window fell over the run (28 → 19). At the end, 3 labs and 9 assemblers were running (red and green science, gears, circuits, cable, inserters, belts).

**Where this leaves the rocket.** The rocket path has about 20 technologies left after automation-2, including oil processing, blue science, modules, rocket fuel and the silo. At the measured pace of about 7 technologies an hour with one red and one green assembler, research is bound by science throughput and by the number of planner turns. Each 60-minute run costs about $17–24 of estimated subscription usage, so the next measurement (multi-hour runs) is a budget decision, not a code change. The next general levers are:
- scale science assemblers and labs, with `rocket_path` pointing at the pack that bounds progress;
- make belt routing more robust (it was 13 of about 60 calls failing in long1);
- a coal/ore lane smelting-column pattern so furnaces stop needing hand fuel.

### Science throughput and reach (after long3)

- **The slowest science pack is the research bottleneck.** `rocket_path.science_packs_needed` now reports `made_last_10_min` for each pack, from force flow statistics, and names the `slowest_pack`. The fallback rung says to scale that pack and its inputs.
  - On the long3 final save: red science made 15 packs and green 21 in the last 10 minutes, with 2 red assemblers and 1 green. That is 1.5 red packs a minute from machines that can make 6 each, so the science assemblers are starved of inputs. The rung names red science.
- **Walks that end outside build reach.** long3 had 6 failures at one site: "Could not move within build reach … walk completed outside native reach". Approach now retries once toward a goal at 60 % of reach, over the wide 48-tile collision window, before failing. The exact state could not be reproduced on the final save, because the site had been built over, so this is a guarded retry, not a reproduced fix.

Gates: fmt, clippy and 436 cargo tests pass; `luac` is clean; `live_regressions.sh` has 474 passed and 0 failed.

### Validation run with the throughput signal (`long4-opus-open-2590060469-60m`)

Seed 2590060469, 60 minutes, 18 turns, $12.49 estimated.
- **Research started:** automation (16 min), steel (21), logistic-science-pack (26), electric-mining-drill (35), logistics (36), fast-inserter, gun-turret, automation-2 (41).
- **Holdout:** 10 assemblers produced items, including red and green science.
- **Milestones:** plates yes, powered production yes, **science delivery no, research no**.

**New wall: the research queue ran dry.** Throughout both holdout windows every lab reported `no_research_in_progress`. automation-2 finished and nothing was queued behind it. The factory made science, but research stopped as soon as the planner stopped. This is also the first long run where hand transfers rose, to 185 of 365 calls (51 %), and 5 of 19 turns hit the 300 s cut. One run on a new seed does not say whether that is noise.

Fix:
- The ladder warns whenever fewer than 3 technologies are queued, pointing at `rocket_path.researchable_now`.
- `start_research` accepts a technology whose unresearched prerequisite is already queued (Factorio finishes it first), and reports `queue_length`.
- The directive asks for at least 3 queued technologies.
- Sandbox on the long4 final save: the warning fired with a queue of 0. `advanced-material-processing`, then `concrete` (whose prerequisite was only queued), were both accepted, giving a queue of 2.

### Validation of the queue fix (`long5-opus-open-2590060469-60m`)

Same seed as long4, 60 minutes, 20 turns, $20.84 estimated.
- **Queue fix worked.** Three technologies were queued at 9 minutes (automation, logistic-science-pack, steel). No off-path research was started. Only 1 of 21 turns hit the 300 s cut (5 of 19 in long4), and hand transfers fell to 116 of 357 calls (32 %; 51 % in long4).
- **Milestones:** plates yes; powered production, science delivery and research **no**.
- **Why research stopped.** The research queue was not empty this time (automation-2 was queued), but no pack reached a lab. On the final save, every assembler was `full_output`, including 3 science assemblers holding finished packs, while both labs reported `missing_science_packs`. Furnaces were full and 4 of 5 electric drills were blocked on output. The factory produced everything, but nothing carried it to its consumer, so it ran only while the character moved items.

Fix: the snapshot now counts machines blocked on full output and assemblers short of ingredients, and flags science assemblers that are full while labs are starving. Two warnings follow:
- "science assemblers are full while labs lack packs: connect them";
- "machines are blocked with full output while assemblers lack ingredients: carry items with inserters and belts, not by hand".

Sandbox on the long5 final save: 16 blocked machines, and the science warning fired for 3 full science assemblers.

### Validation of the supply warnings (`long6-opus-open-2590060469-60m`)

Same seed as long4 and long5, 60 minutes, 23 turns.
- **Research started:** 3 technologies at 10 min (automation, logistic-science-pack, steel), then electric-mining-drill and logistics (17), automation-2 and advanced-material-processing (28), engine (39), electric-energy-distribution-1 and fast-inserter (50). All are on the rocket path.
- **Holdout:** 4 labs working. Research kept going with Buddy stopped: advanced-material-processing went 81 % → 91 %.
- **Milestones:** plates, powered production and research all yes. Science delivery **no**, because the lab stock fell by 1 in a window; this is the strict metric noted under long1.
- **Compared with long4 and long5 on this seed,** research progress with Buddy stopped went from no (both) to yes, and the run reached the furthest research so far (engine, electric-energy-distribution-1).
- **Still open:** hand transfers were 206 of 439 calls (47 %).

### Bulk refuelling (after long6)

In long6, 119 of the 206 hand transfers were single-machine `bootstrap_burner_once` calls, and 45 were plate collections. `refuel_burners` now tops up every burner inserter, drill, furnace and boiler in a radius from inventory, in one call:
- nearest machine first, walking between them;
- bounded to 120 s;
- each transfer is the same conservation-checked `bootstrap_burner_once`;
- a machine that got only part of its target stays pending, so running out of fuel is reported as `out_of_fuel`, not "topped up".

It reads fuel levels through a new Lua remote, `burner_fuel_levels`. It counts as a hand transfer for the manual-logistics alert, and the low-fuel warning points to it.

Live check: 3 furnaces, target 10, only 25 coal in inventory → `out_of_fuel`, all 3 fuelled, 25 coal in the furnaces, 0 carried, still 3 furnaces.

Gates: 436 cargo tests pass; `live_regressions.sh` has 477 passed and 0 failed. Model tool schemas still fit under 60 KiB; the new tool's text was cut to fit. The live layout check now teleports its character to its own site: from the new refuel site, the walk got stuck behind the steam-power fixture.

### Validation of bulk refuelling (`long7-opus-open-2590060469-60m`)

This run uses the same seed as long4–long6: 60 minutes, 18 turns, 376 calls.

- **All four holdout milestones passed for the first time:** plate automation, powered production, automated science delivery, and research progress. For science delivery, lab stock was flat (0, 0) while the assemblers made 13 and 17 packs.
- **Hand transfers:** 91 of 376 calls (24 %), down from 200 of 439 (46 %) in long6.
  - `refuel_burners`: 16 calls.
  - `bootstrap_burner_once`: 15 calls, down from 119.
- **End state** (the final save loaded into a sandbox, then `autonomy_snapshot`):
  - 22 technologies left, researching logistics-2;
  - 4 labs, all working;
  - 2 red-science assemblers made 55 packs in 10 min, and 1 green made 61.
- **Where the time went:**
  - `build_layout`: 623 s;
  - `collect_from_chest` (46 plate pickups for hand crafting): 470 s;
  - `wait_for_crafting`: 389 s;
  - `mine_at` (hand mining): 374 s.

### Chained runs (`--continue-from`)

A fresh map each hour cannot reach the rocket, so `tests/autonomous_eval.sh --continue-from SAVE_ZIP` starts a trial from a copy of an earlier trial's save, and Buddy starts without `--fresh`. The holdout milestones still measure only the new trial's windows.

`long8-cont7-opus-open-2590060469-60m` continues from long7's final save: 60 minutes, 24 turns, 318 calls.
- **Hand transfers:** 60 of 318 calls (19 %).
- **Research:** technologies left fell from 22 to 17, with 6 queued. Oil processing is done; flammables, sulfur-processing, plastics, and concrete can be researched now.
- **Science:** in 10 min, red went from 55 to 202 packs (5 assemblers) and green from 61 to 186 (3 assemblers).
- **Labs and power:** 6 labs working, 5 steam engines.
- **Holdout:** science made per window was 38 and 45, 3× long7. Science delivery still fails the strict metric: lab stock fell by 2 in window 1.
- **Open warnings at the end:**
  - 9 burner machines are low on fuel;
  - 31 machines are blocked with full output while 2 assemblers are starved.

  Production is outrunning its belts.

`long9-cont8` continues from long8's final save: 60 minutes, 22 turns, 325 calls, 7 % hand transfers.
- **Research:** 11 technologies left, researching chemical-science-pack.
- **Labs:** 18 labs, but only 3 working. Science assemblers sit full while labs wait for packs.
- **Science:** 44 and 46 packs made per holdout window. Science delivery still fails the strict check: lab stock fell by 11 and by 3.
- **Two tool defects:**
  - `repair_fuel_sustainability` failed with `recursion limit exceeded`. Each belt tile nested another `upstream_proof` level. This one area went 202 levels deep and was 945 KB.
  - A 200-tile `route_belt` dry run returned 75 k characters, which is more than the client lets the model read.

`long10-cont9` continues from long9's final save: 60 minutes, 344 calls.
- **Oil built:** crude oil found 400 tiles away. It has 1 pumpjack and 1 working refinery, with petroleum piped about 300 tiles through 80 pipe-to-ground pairs to sulfur and plastic chemical plants.
- **Blue-science parts:** advanced-circuit and engine-unit assemblers were built.
- **Remaining:** 10 technologies left.
- **Chemical science never made a pack:** 1 assembler, starved of ingredients. Sulfur is short, with 1 pumpjack feeding everything.
- **Off-path research:** Buddy queued automobilism, circuit-network, and explosives. Every rocket-path technology then needed blue packs, and the "keep 3 queued" warning pushed it to queue filler.
- **Runner log:** the console log ends at the shutdown step. `summary.json`, the samples, and `save.zip` were written. The runner was started with a plain `&` instead of as a tracked job, so its exit status was not observed.

### Fixes after long9/long10

- **Coal-supply proofs no longer nest per belt tile.** Consecutive belt hops now point straight at the proof below the belt run. `hops` still counts every tile, and the new `belt_hop` field marks these proofs.
  - On long9's save, the same diagnosis went from 945 KB, 202 levels deep, to 41 KB and 11 levels. The failing `repair_fuel_sustainability` call now returns a normal result.
- **Long route plans are shown as segments.** Model-facing `route_belt` results replace per-tile plans over 40 tiles with `planned_segments` and `planned_new_segments`. Each segment is a straight run: kind, direction, from, to, and tile count. Internal compound tools still read the full per-tile plans.
  - On long9's save, a 105-tile dry run went from 18.9 KB to 2.6 KB.
  - A unit test covers turns, gaps, and the 40-tile limit.
- **Stalled science packs have their own rung in the progression ladder.** A needed pack that has an assembler but was not made in 10 minutes becomes `next_goal`. The pack entry gets:
  - `ingredients_made_last_10_min`;
  - `missing_inputs`: the unmade ingredient chain, down to 3 recipe levels, fluids included through the fluid statistics.

  `how` names the deepest missing link.
  - On long10's save: "Get chemical-science-pack made". Sulfur 66, advanced-circuit 10, engine-unit 8.
  - On a synthetic map with an idle red-science assembler: copper-plate, then copper-ore; iron-gear-wheel, then iron-plate, then iron-ore.
- **No filler research.** When every ready rocket-path technology needs a pack made 0 times in 10 minutes, the "queue at least 3" warning is replaced by one that says off-path research only spends packs. The autonomy directive says the same.

Gates: 437 cargo tests, clippy, `luac`, `live_regressions.sh` 477 passed and 0 failed (`fb-evidence/git-gud/live14`).

### Validation of the stalled-pack rung (`long11-cont10-opus-open-2590060469-60m`)

This run continues from long10's final save: 60 minutes, 28 turns, 402 calls, 15 hand transfers (4 %).
- **Blue science is automated.**
  - 3 chemical-science assemblers made 72 packs in the last 10 minutes; long10 had 1 assembler and made 0.
  - The holdout made 8 blue packs a minute with the model stopped.
- **All four holdout milestones passed,** including strict science delivery.
- **Research stayed on the rocket path:** advanced-oil-processing, processing-unit, low-density-structure, advanced-material-processing-2, lubricant. Nothing off-path was queued.
- **End state:**
  - 9 technologies left, 4 queued;
  - red 66, green 65, blue 72 packs in 10 min;
  - 7 steam engines, 6 working.
- **Still open:**
  - Only 2 of 18 labs are working. 7 science assemblers are full while labs lack packs, so pack delivery to labs now limits research speed.
  - 19 burner machines and the boiler are low on fuel.
  - 59 machines are blocked with full output.

### Which pack the idle labs lack (after long11)

On long11's final save, 16 of 18 labs were idle, and the old warning said "7 science assemblers are full while labs lack packs: connect them". That was wrong about the cause.
- The idle labs each held red and green packs and lacked only blue.
- The full assemblers were red and green ones, full because the labs were already full of those packs.
- The flow-based `slowest_pack` named green (65 per 10 min), not blue (72).

The snapshot now counts, for each lab in `missing_science_packs`, which packs of the current research it lacks. The result is in `progression.lab_missing_packs`. The warning splits two cases:
- a lacked pack whose assemblers are full is a delivery gap;
- a lacked pack whose assemblers are not full is a supply limit.

The scaling rung names the pack that the most idle labs lack. On long11's save it now reads: "Idle labs lack chemical-science-pack (16 labs) … supply is the limit" and "scarcest pack: chemical-science-pack (17 idle labs lack it)".

Gates: `luac`, `lua_golden` 55/55, `live_regressions.sh` 477/0 (`fb-evidence/git-gud/live15`).

### `long12-cont11-opus-open-2590060469-60m`

This run continues from long11's final save. It started after a 60-minute pause for the subscription usage window. It ran 60 minutes: 36 turns, 342 calls, 9 hand transfers (3 %), and no turn was provider-limited.
- **All four holdout milestones passed.**
- **Research stayed on the rocket path.** The queue ends as advanced-material-processing-2, lubricant, rocket-fuel.
- **End state:** 7 technologies left (9 in long11); blue science at 136 packs in 10 min from 4 assemblers (72 from 3 in long11); red 128, green 101; 5 of 18 labs working (2 in long11); 8 steam engines.
- **Scarcest pack:** the snapshot names chemical-science-pack; 13 idle labs lack it.
- **Still open:** 23 burner machines and the boiler are low on fuel, and 49 machines are blocked with full output.

### `long13-cont12-opus-open-2590060469-60m` (provider-limited, invalid as a trial)

This run continues from long12's final save. Claude hit the subscription limit at 41 minutes: 4 of 22 turns were provider-limited, and the runner exited with status 1 (`provider_limited`). It does not count as a valid trial.

Its save is still a real game state, so the chain continues from it. End state:
- 3 technologies left: robotics, then its dependents up to rocket-silo;
- red 39, green 95, blue 150 packs in 10 min;
- the research queue is empty, so 0 of 18 labs are working.

Holdout science delivery failed: lab stock stayed flat because nothing was being researched.

### `long14-cont13` and `long15-cont14`: the rocket-silo technology is researched

**long14** continues from long13: 60 minutes, 31 turns, 415 calls, no provider limit.
- All four holdout milestones passed.
- Robotics and its dependents were researched, and rocket-silo was being researched at the end (37 %).
- Some filler was researched too: productivity, speed and efficiency modules, and research-speed-3.
- rocket-silo costs 1000 units of red, green and blue.

**long15** continues from long14: 60 minutes, 21 turns, no provider limit.
- rocket-silo was researched. The force then moved on to mining-productivity-2.
- The model crafted toward the rocket once each: rocket-silo, rocket-part, rocket-fuel, processing-unit, low-density-structure.
- No silo was placed.
- Holdout science delivery failed: lab stock rose by 3 and 1, and the character's inventory changed in window 2.
- The console log ends at the shutdown step, as in long10, because the run was started with a plain `&`. `summary.json`, the samples and `save.zip` are complete, and no process was left behind.

**Rocket silos never launch on their own in Space Age.** A sandbox silo with 50 rocket parts and power reached `rocket_ready` about 15 s after `rocket_parts` was set, then stayed at `rocket_ready`. Only `LuaEntity.launch_rocket()` sent the rocket up, and `force.rockets_launched` went from 0 to 1 about 20 s later. Without a launch call, Buddy could never finish the goal.

**Changes:**
- **New `launch_rocket` tool,** backed by the `launch_rocket` remote in `research.lua`. It launches the first ready silo of the agent's force, or the one given by `unit_number`. If no rocket is ready, it returns `no_rocket_silo` or `rocket_not_ready` with each silo's status and parts.
- **Rocket-stage rungs in the progression ladder,** once rocket-silo is researched:
  - no silo: build one, with the recipe's ingredients listed;
  - rocket ready: call `launch_rocket`;
  - otherwise: fill the silo, showing parts N/50 and how much of each rocket-part ingredient was made in the last 10 minutes;
  - after a launch: keep launching.
- **Research warnings silenced after rocket-silo.** The lab-supply warnings stop once rocket-silo is researched, because research speed no longer gates the goal.
- **Launch counts in samples.** `progression` reports `rocket_silos`, `rocket_parts` and `rockets_launched`. `evaluation_sample` research reports `rocket_silo_researched` and `rockets_launched`, so the trials record launches.
- **Tool descriptions trimmed.** Five long descriptions were shortened so the model-visible schemas stay under 60 KiB with the new tool.

On long15's save the rung reads: "Build a rocket silo … Craft rocket-silo (1000 steel-plate, 200 processing-unit, 200 electric-engine-unit, 100 pipe, 1000 concrete)". `launch_rocket` there returns `no_rocket_silo`.

Live checks on the regression surface:
- `launch_rocket` refuses a silo with 0 parts;
- a full, powered silo reaches `rocket_ready` and does not launch by itself;
- `launch_rocket` launches it;
- `rockets_launched` rises by exactly 1.

Gates: 437 cargo tests, clippy, `luac`, `lua_golden` 55/55, `live_regressions.sh` 482 passed and 0 failed (`fb-evidence/git-gud/live17`).

### long16–long18: building toward the silo

- **long16** (continues from long15, 60 min, 20 turns, no provider limit):
  - Buddy started concrete and electric-engine lines, and spent 738 s walking.
  - Research progress failed in the holdout. The queue had moved on to off-path productivity research, which is harmless once rocket-silo is researched.
  - End state: electric-engine-unit, processing-unit, low-density-structure and rocket-fuel had never been made.
- **Change after long16:** the silo rung lists each ingredient with its 10-minute production. After long17 it also shows how many the agent holds, counting its inventory and the force's chests.
  - On long17's save: steel 2131/1000, concrete 1600/1000, processing-unit 47/200, electric-engine-unit 65/200, pipe 0/100.
- **long17** (continues from long16, 60 min, 20 turns, no provider limit): Buddy built processing-unit lines (15 per 10 min) and electric-engine lines (34 per 10 min).
- **long18** (continues from long17): hit the five-hour subscription limit at 20 minutes, with 8 of 17 turns provider-limited. It is not a valid trial.
  - Before the cutoff, Buddy built a low-density-structure cell and a rocket-fuel plant.
  - Its report: processing units 60 of 200, electric engines 89 of 200.
  - It showed two tool defects:
    - **`build_layout` ignored `direction` for assemblers.** Factorio drops the direction of an assembler placed without a fluid recipe, so a rocket-fuel plant ordered to face west faced north. The model filed this as an issue. `build_layout` now turns each machine it placed to the requested direction after setting its recipe. It reports `direction_applied`, or a `direction_differs` entry when the machine cannot turn (no fluid box). A live check covers it.
    - **A staircase `route_belt` dry run returned 111 KB.** The route changed direction on every tile, so segmenting did not shrink it. After segmenting, `planned_segments`, `planned_new_segments` and `preserved_underground_pairs` now keep their first 20 and last 4 entries and report `<key>_omitted`. `ready_to_call` still executes the full route. A unit test covers it.

Gates: 438 cargo tests, clippy, `luac`, `live_regressions.sh` 484 passed and 0 failed (`fb-evidence/git-gud/live20`).

### `long19-cont18-opus-open-2590060469-60m`: three rockets launched

long19 continues from long18's save: 60 minutes, no provider limit, 355 calls, 0 invariant failures.
- Buddy crafted a rocket-silo and placed it at (92.5, 129.5), unit 5524.
- It called `launch_rocket` 24 times. 21 calls were refused because the rocket was not ready; 3 succeeded.
- `evaluation_sample` reports `rockets_launched` 0 before the trial and 3 after.

Loading the final save and reading the force directly gives `rockets_launched = 3` at tick 2 866 713. The silo shows `building_rocket` with 28/50 parts, so rocket parts are still being made with the model stopped.

**Lineage.** All of this happened on one map. long7 was a fresh map, seed 2590060469, and long8 through long19 each continued from the previous trial's save. That is 13 hour-long trials; long13 and long18 were cut short by the subscription limit.
- Every in-game action came from Buddy's model through the MCP tools.
- Between trials, only Buddy's code changed, as recorded in the sections above. The trial saves were never edited.

**Limits of this result:**
- It is one seed and one chained run.
- Science delivery failed its strict holdout check in this trial.
- After the launches, 83 of 120 turns ended without a tool call: the rung only says "keep launching" while the silo refills.

### Review fixes to the new controllers

A read-only review of the new code found real defects, all fixed:
- Trees and rocks are now mined by identity (`clear_area` on the obstacle's exact position), not with `mine_at`, which takes ore first when a tree stands on ore. A live check covers a tree on 500 ore: the tree goes, the ore stays at 500, and the chest is placed.
- `already_built` matches on name and snapped position. A differing direction is reported in `direction_differs` instead of causing a duplicate craft followed by a collision rollback.
- Crafting counts recipe crafts: a belt craft yields 2, so it is no longer done twice. A partially accepted craft is a failure. After each crafting round the inventory is read again, because a lab eats belts. Steam power crafts the machines before pipes and poles and re-plans after each round.
- Recipes are set only after every placement succeeded. A splitter over existing belts is refused, because rollback could not restore the replaced belts.
- Obstacles are approached to mining reach (the Lua refused tree mining at 9.2 tiles under build reach). The live tree-on-ore check failed before this and passes after.

- `find_nearest_resource water` returns the nearest water *tile*, which can sit inside a lake or a pond too small for a pump. Its result now says so. When `build_steam_power` found the box itself and the planner finds no pump site, it widens the box by 24 tiles once before failing.
- The source-substring checks in `lua_golden.rs` for `bootstrap_burner_once` and `collect_from_chest` were removed. Live world-state probes replace them:
  - an electric furnace is refused and the coal stays with the character;
  - a stone furnace takes exactly 15 coal from the character;
  - 5 plates move from a furnace's output to the character.
- **`build_steam_power` could outlast a turn.** 1 of the 6 real calls in the counted trials (long1) was cut by the 300 s turn limit mid-call, so its rollback never ran and Buddy reported it outcome-unknown. The controller now stops after crafting (`phase: crafted`; crafting changes no world state), and a second call with every part in hand only places and fuels (`phase: placed`). The live suite checks both phases: the crafting call places nothing; the next call builds a fuelled, networked plant and crafts nothing.

Gates after these fixes: fmt, clippy and 436 cargo tests pass; `luac` is clean; `live_regressions.sh` has 474 passed and 0 failed; `buddy_runtime.sh` 21/21. The long runs above predate the two-phase steam build and the widened water box.

## Comparison with other harnesses

- **[rmalde/minecraft-agent](https://github.com/rmalde/minecraft-agent)**
  - Setup: GPT-6 Astra or GPT-5.6 Sol plans, and Jev (`typesafe/jev-1.13` through OpenRouter `/api/alpha/decisions`, not the official endpoint used here) picks one bounded action per step. Mineflayer then carries it out.
  - The reported 8:43 run used Peaceful, a surveyed seed (`8398967436125155523`), known coordinates, and a naturally active End portal. It is not screenshot or keypress control, and the recordings are not in Git. It was not reproduced here.
  - Lessons adopted: a small legal action set, separate evidence files, lab tests that never count as full runs, and a pass condition that needs world-state proof rather than model prose.
  - Lessons not transferable: Jev chose among many frequent actions there. Buddy's high-level controllers leave Jev one rare decision, which is why it was never consulted in these trials.
- **[FLE](https://github.com/JackHopkins/factorio-learning-environment)** ([paper](https://arxiv.org/html/2503.09617v1); the paper used an older Factorio than the current repository)
  - Adopted: separate prepared lab tasks from open play, measure production over a holdout interval with the agent stopped, and treat repeated failed-action loops as a first-class metric.
  - Not adopted: FLE's Python REPL runtime and its privileged action semantics.
  - This harness keeps its own shortcuts, listed above: global observation, instant mining, and a peaceful map.

No continual learning is claimed. The recent-outcomes memory lasts one Buddy session only.

## Dead or misleading surfaces (reported, not changed)

- `src/tool_metadata_data.rs` is not declared as a module anywhere, so it is never compiled. It is also inaccurate: it omits `collect_from_chest`, `file_issue`, `bootstrap_burner_once`, `wait_for_crafting` and calls `repair_steam_power`/`extend_power_to` read-only. The MS-3 lock does not use it.
- `LuaCommand` in `src/client/lua.rs` has no production callers. 21 of its builders disagree with `remote_api.json` on arity (19 omit the trailing `agent_id`; `rotate_entity` and `set_recipe` omit the leading one). `tests/lua_golden.rs` pins these envelopes, so it certifies a contract that nothing uses.
- The `stop_mining` remote returns bare `"ok"`/`"error"` strings.

## Operational incidents during this investigation

- A throwaway fixture script from one worker wrote its `config.ini` `write-data` line under the wrong section. About 8 short isolated-port runs (00:10–00:26) therefore used `~/.factorio` as write-data. Factorio overwrote `factorio-current.log`/`factorio-previous.log` and, on exit, rewrote `mods/mod-list.json`, `mods/mod-settings.dat`, and `player-data.json`. The mod-list content was unchanged. `saves/` and `mods/claude-interface/` were not touched. Those runs were discarded and rerun with verified private write-data.
- Relative-path edits by two workers briefly landed in the main tree. They were reverted within minutes, before any main-tree build or test.
