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

## Space Age for one character: robots, a platform, Vulcanus

After the first rocket, Buddy continues into Space Age with one character. Nauvis is run by construction robots while the character is away. Enemies stay off (peaceful map; Vulcanus inherits it).

### Changes

- **Three model tools** (Lua in `mod/claude-interface/space.lua`):
  - `robot_logistics`: roboports, robots, ghosts, ghosts outside construction range and the items ghosts still lack, on any surface. On a platform it adds the hub stock and build queue.
  - `place_ghosts`: entity and tile ghosts on any surface, all-or-nothing, in `build_layout`'s format. On a planet every position must be in construction range (`no_construction_coverage` otherwise); trees and rocks are marked for removal; recipes are set on the ghosts. On a platform the hub builds from its own stock, including foundation tiles.
  - `space_platform`: `status`, `create`, `ship` (items from the inventory in a ready rocket; the starter pack for a new platform), `request` (landing-pad requests), `schedule`, `board` and `land`. A silo out of reach is approached once and the call retried.
- **One character across surfaces:**
  - Walking on a platform is refused (`on_space_platform`).
  - Research status counts labs on every planet surface, not only the character's.
  - The snapshot and `evaluation_sample` describe Nauvis while the character is on a platform or another planet. Test surfaces without a planet keep their old behaviour.
- **Progression:** the old "a rocket has been launched" rung is gone. After the first rocket the ladder goes: construction robotics, a home roboport network with 10 robots, a platform, space science on it, a landing pad with a request, the research path to `planet-discovery-vulcanus` (`progression.space_path`), thrusters, turrets, boarding, course, landing. Recipe text in rungs is read from the prototypes. New warnings cover ghosts outside roboport range, items the home robots lack, and damaged platform tiles.
- The trial `summary.json` records `.space` from the final sample.
- The model tool schema cap rose from 60 KiB to 72 KiB. With the three tools it is 64 824 bytes.

**Found while testing:** a character that arrives on a platform by rocket sits inside the hub (`driving` true, `teleport` refused). A cargo pod then silently ignores it as a passenger and flies empty. `land` now forces it out of the hub first (`set_driving(false, true)`) and refuses when the pod does not hold the character.

Gates: fmt, clippy, 438 cargo tests, `luac`, `live_regressions.sh` 503 passed and 0 failed (`fb-evidence/git-gud/live21`, with 19 new Space Age checks), and `buddy_runtime.sh` passed. On long19's final save, `next_goal` is "Research construction-robotics: …", with 0 roboports and no platforms.

### `long20-cont19-opus-open-2590060469-60m`

long20 continues from long19's save: 60 minutes, 98 turns, none provider-limited, 247 tool calls with 17 errors, 0 invariant failures. `plate_automation` and `powered_production` were achieved.
- Buddy created platform `buddy-1` and shipped its starter pack; the hub was built and `space-platform` completed. All 8 `space_platform` calls succeeded.
- It never got construction robots. `start_research construction-robotics` failed three times with "Failed to queue research". On the final save the technology was already second in the queue, behind `mining-productivity-3`, whose packs were not made. Factorio refuses to queue a technology twice, and the old message blamed "another research in progress".
- `diagnose_steam_power` twice returned 75–80 KB, which Claude refused to read (119 entities, 47 KB in `entities`).
- 52 `collect_from_chest` calls: most of the hour went to hand logistics for crafting.

**Fixes after long20:**
- `start_research` puts the technology first in the queue, behind only its own queued prerequisites; an already-queued technology is moved to the front (`moved_to_front`). The result lists the queue. On long20's save, `construction-robotics` moved from second to first. A live check covers the move.
- `diagnose_steam_power` keeps its first 40 entities and reports `entities_omitted`.

Gates: 438 cargo tests, clippy, `luac`, `live_regressions.sh` 505 passed and 0 failed (`fb-evidence/git-gud/live22`).

### `long21-cont20-opus-open-2590060469-60m`

long21 continues from long20 with the queue fix: 60 minutes, 25 turns (several near the 300 s cut), none provider-limited, 30 tool errors, 0 invariant failures. `plate_automation` and `powered_production` held.
- `start_research construction-robotics` succeeded first time and the technology finished. A roboport with 10 construction robots now runs at home.
- Buddy crafted an asteroid collector, shipped it with `space_platform ship`, and placed it and 6 foundation tiles with `place_ghosts surface=platform-1`; the hub built them. Building the collector completed `space-science-pack`. It scheduled the platform at Nauvis.
- Most errors were hand logistics, not the new tools: 11 `wait_for_crafting` timeouts behind long crafts (low-density structure, roboport), 5 "stuck" walks inside the dense base, and 5 `route_belt` dry runs refused at occupied endpoints. None of the 44 `space_platform`/`place_ghosts`/`robot_logistics` calls failed.
- End state: next rung "Make space science on buddy-1" (crushers, furnace, assembler, inserters, power on the platform); no landing pad yet.

### `long22-cont21-opus-open-2590060469-60m`

60 minutes, 27 turns, none provider-limited, 38 tool errors, 0 invariant failures; `plate_automation` and `powered_production` held.
- On `buddy-1` Buddy built an asteroid collector, 2 crushers on carbonic and oxide crushing, 9 inserters, poles and solar panels with `place_ghosts`, all built by the hub. The hub holds 317 carbon, 85 ice and 36 iron plate. The assembler and electric furnace for space science were shipped but not yet placed: one `place_ghosts` call was refused (`placement_blocked`, nothing placed) and the retry at another spot was cut by the turn limit.
- **Rockets are the bottleneck.** 12 of the 38 errors were `space_platform ship` refused with `rocket_not_ready`, mostly repeated within a turn while the silo refilled (28/50 parts at the end). One was `rocket_cargo_full` for 74 foundation (a rocket lifts 50).
- `analyze_item_flow` returned 81 KB, which Claude refused to read.

**Fixes after long22:**
- `rocket_not_ready` now reports `parts_required` and says to fill each rocket and do other work instead of retrying. `rocket_cargo_full` reports `max_per_rocket` from the item weight (1 t per rocket: 50 foundation, 10 crushers, 100 magazines).
- `analyze_item_flow` keeps the first 40 `reachable_belts` and `items_on_path`, with `_omitted` counts.

### `long23-cont22-opus-open-2590060469-60m`

60 minutes, 39 turns, none provider-limited, 57 tool errors, 0 invariant failures; `plate_automation` and `powered_production` held.
- `buddy-1` now has an assembler set to `space-science-pack` and crushers on metallic and oxide crushing; the hub holds 420 carbon, 135 ice and 150 iron ore. The electric furnace for iron plate was never placed, so no space science was made.
- **The furnace placement failed five times** (dry runs refused with `placement_blocked`). On the final save the refusals were correct: each spot overlapped a crusher, solar panels or the collector, and the model could not see which. Checking this also showed a real defect: a platform ghost over empty space passed the check (a 3x3 furnace at 12.5,0.5 with no foundation), and the hub would never have built it.
- Rockets again: 13 `ship`/`board` refusals with `rocket_not_ready` and 7 `launch_rocket` refusals, despite the new "do not retry" guidance.
- 60 `walk_to` calls and 7 "stuck" failures inside the dense base.

**Fixes after long23:**
- On a platform, `place_ghosts` refuses an entity whose footprint is empty space not covered by a planned foundation tile.
- `placement_blocked` names the blockers in the error text and returns `nearest_free`, the closest spot within 12 tiles where the entity fits. When a platform has none, it says to add foundation tiles in the same call. On long23's save, the furnace at 6.5,0.5 is "blocked by crusher; nearest free spot … at 8.5,-1.5" before the foundation rule and has no free spot after it (the platform is full), so the message asks for foundation.
- Two live checks cover both. Gates: 438 cargo tests, clippy, `luac`, `live_regressions.sh` 507 passed and 0 failed (`fb-evidence/git-gud/live23`).

### `long24-cont23-opus-open-2590060469-60m`

Stopped after 40 minutes by `max_autonomous_turns`: 120 turns, of which **108 made no tool call**. None provider-limited, 14 tool errors, 0 invariant failures; `plate_automation` and `powered_production` held.
- In its working turns Buddy traced why the silo was stuck at 14/50 rocket parts: the low-density-structure assembler got no copper because iron filled the copper lane. It built a dedicated copper feed and reworked side-fed underground belts. The silo reached 35/50.
- It then followed the new `rocket_not_ready` guidance literally: "holding the inserter and pole next to the silo, will ship as soon as the rocket is ready. No other changes this turn." Buddy starts an autonomy turn about 10 s after the last one ends, so the waiting turns spent the 120-turn budget in minutes. Nothing was shipped; the platform is unchanged.

**Fix after long24:** an autonomy turn with no tool call delays the next one by 6, then 12, at most 18 heartbeats (capped at 180 s; with the trial's 5 s heartbeat: 30, 60, 90 s). Any turn that calls a tool resets it; player messages are unaffected. A unit test pins the steps and caps.

### `long25-cont24-opus-open-2590060469-60m` (provider-limited, invalid as a trial)

The five-hour subscription limit was hit 10 minutes in: 10 of 17 turns were provider-limited, so this does not count. Nauvis held `plate_automation` and `powered_production` through the hour without the model.
- In the valid 10 minutes Buddy shipped 3 inserters and 3 poles (the silo had filled), then asked to ship 6 inserters and 150 iron plate **18 times within one turn** while the next rocket built. The zero-call backoff from long24 does not catch this: each turn did make calls.

**Fix after long25: shipments queue at the silo.** `space_platform ship` no longer needs a ready rocket. Beside any silo, it takes the items from the inventory into a shipment the mod holds, and a once-a-second handler loads each ready rocket with as much as it lifts (1 t) and launches it until the shipment is empty. The result reports `rockets_launched_now` and `rockets_still_needed`; `status` shows waiting cargo as `queued_cargo` per platform. Cargo for a platform that no longer exists goes back to the character. This also removes `rocket_cargo_full`: large shipments split across rockets. `board` still needs a ready rocket with empty cargo.
- Sandbox on long25's save, silo at 37/50: 120 foundation and 5 solar panels queued (3 rockets needed); after three refills the hub received them in 50/50/rest batches and `queued_cargo` emptied.
- A live check ships while the rocket is not ready, sees the cargo queued and out of the inventory, then fills the silo and sees it arrive. Gates: 439 cargo tests, clippy, `luac`, `live_regressions.sh` 508 passed and 0 failed (`fb-evidence/git-gud/live24`), `buddy_runtime.sh` passed (after the long24 fix).

### `long26-cont25-opus-open-2590060469-60m`

60 minutes, 51 turns (10 without a tool call, now backed off), none provider-limited, 47 tool errors, 0 invariant failures; `plate_automation` and `powered_production` held.
- Buddy used the queued `ship` as intended: 6 and 5 inserters, then 800 iron plate (16 rockets). It no longer retried `ship`; it called `launch_rocket` 27 times instead (22 refused while the silo refilled).
- **All of that cargo was lost.** Loading the final save: the hub's 59 slots had been full since long23 (38 metallic asteroid chunks at one per slot, 420 carbon, ice, iron ore). A rocket delivering to a full hub parks its pod and the cargo disappears. Reproduced on the save: 20 iron plate shipped, the pod parked, the hub count stayed 0. The full hub is also why the platform had not changed since long23: collectors and crushers stop when the hub cannot take their output.
- `walk_to` once failed with "Packet too large: 17613457 bytes" (target 33,386 across the base). Not fixed yet.

**Fixes after long26:**
- Queued shipments load each rocket only with what the hub can take now: room in partly filled stacks plus free slots shared across the rocket. The rest keeps waiting (`cargo_blocked: hub_full` in status). On long26's save, 100 iron plate and 40 carbon queued; only 30 carbon (the free space in a carbon stack) left, the rest waited.
- `ship` refuses with `hub_full` when the hub has no room for any of the items, with the hub contents and how to free slots.
- Platform status reports `hub_slots` and `hub_free_slots`; the snapshot warns when a hub is full.
- A live check fills a hub and sees `ship` refused. Gates: `luac`, 55 golden tests, `live_regressions.sh` 509 passed and 0 failed (`fb-evidence/git-gud/live25`).

### `long27-cont26-opus-open-2590060469-60m`

60 minutes, 43 turns, none provider-limited, 42 tool errors, 0 invariant failures; `plate_automation` and `powered_production` held. The character stayed on Nauvis.
- **Space science reached the labs:** `space-platform-thruster` was researched by the end. `planet-discovery-vulcanus` was queued twice (and `planet-discovery-fulgora` once) but was not finished. Buddy also requested iron ore and carbonic asteroid chunks on the landing pad.
- **The hub-full fix worked:** Buddy freed 16 of the 59 hub slots, and cargo now arrives: the hub held 336 iron plate at the end, where long26's 800 had vanished. One `ship` was refused with `hub_full` before slots were freed; the retry was accepted. 600 iron plate and a few smaller items were still queued at the silo at the end.
- The platform was still missing its electric furnace, which sat in the hub. 11 `place_ghosts` dry runs were blocked by the crushers, the collector and the solar panels. Buddy then added foundation tiles east of the hub, and 4 ghosts were waiting at the end.
- The other errors were Nauvis base work: `verify_production` ×7, `launch_rocket` ×6 while the silo refilled, `build_layout` ×4 (blocked tiles, or out of reach). There was also one `start_research calcite-processing`, which is a trigger tech.

The chain stopped here as agreed: two runs after long25. To reach Vulcanus, Buddy still has to:
1. finish the planet-discovery-vulcanus research;
2. build thrusters with their fuel and oxidizer chain, and turrets;
3. schedule the platform for Vulcanus, board it, fly there and land.

**Fixes after long27** (the user extended the chain until the quota runs out):
- Loading long27's save showed why the furnace never got placed. The electric-furnace ghost at 3.5,-6.5, and two pole ghosts, sat over empty space with no foundation under them; they were placed before the long23 foundation check. The hub never builds such a ghost, yet `place_ghosts` counted it as `already_built` and other ghosts collided with it ("blocked by ghost of electric-furnace").
- Four foundation tile ghosts at x=8–9 did not touch the platform, so the hub never laid them either.
- `place_ghosts` on a platform now treats a ghost standing over empty space with no tile ghost under it as stranded: it is neither `already_built` nor a blocker. A call that places over it removes it when it executes (`removed_stranded_ghosts`; a dry run reports `would_remove_stranded_ghosts`) and puts it back on rollback. Dry runs and refused calls change nothing.
- New foundation tiles must join the platform, directly or through other tiles in the same call or existing tile ghosts; otherwise the call fails with `disconnected_foundation` and lists the tiles.
- Checked on long27's save: two loose tiles were refused. The furnace was placed over its stranded ghost with 9 foundation tiles: the dry run removed nothing, the execute replaced the ghost, and the hub built the tiles and the furnace. Two live checks added. Gates: `luac`, 55 golden tests, `live_regressions.sh` 511 passed and 0 failed (`fb-evidence/git-gud/live26`).

### `long28-cont27-opus-open-2590060469-60m`

60 minutes, 25 turns, none provider-limited, 47 tool errors, 0 invariant failures; `plate_automation` and `powered_production` held. The character stayed on Nauvis; `planet-discovery-vulcanus` was still not researched.
- Most of the run went into chemical science on Nauvis. The snapshot's goal had been "Get chemical-science-pack made: … the missing link is pipe, needed by engine-unit". Errors: `route_belt` ×14, `verify_production` ×9, `wait_for_crafting` ×5.
- The hub filled again (0 of 59 slots free) and Buddy never tried `place_ghosts`. The hub held 832 iron ore and 32 carbonic chunks: the platform crushes metallic and oxide chunks, but nothing crushes carbonic chunks, and they take one slot each. Buddy tried to ship a third crusher, which was refused with `hub_full`: a deadlock, since the part that would free the hub could not be delivered. It also set landing-pad requests for iron ore and chunks repeatedly, to drain the hub.

**Fixes after long28:**
- `space_platform action=jettison` throws asteroid chunks out of the hub, as an inserter over the platform edge would. Other items are refused with `not_jettisonable` and pointed to landing-pad requests.
- The snapshot warns about each chunk type in the hub that no crusher on the platform processes. The hub-full warning and the `hub_full` guidance now name `jettison`.
- On long28's save, the warning named `carbonic-asteroid-chunk x31`; jettisoning them freed 32 slots, and iron ore was refused. Two live checks added. Gates: `luac`, clippy, all cargo tests, release build, `live_regressions.sh` 513 passed and 0 failed (`fb-evidence/git-gud/live27`).

### `long29-cont28-opus-open-2590060469-60m`

60 minutes, 61 turns, none provider-limited, 39 tool errors, 0 invariant failures; `plate_automation` and `powered_production` held. Still on Nauvis, `planet-discovery-vulcanus` not researched.
- Buddy used `jettison` at once (31 carbonic chunks), shipped a third crusher, and placed it with `place_ghosts` on carbonic crushing, together with a foundation tile.
- It then jettisoned 1–5 carbonic chunks at a time in more than 25 calls, because the collector kept bringing more while the new crusher was not built. The hub had no foundation for the crusher's tile, and the `ship` of foundation could not get through a hub that was full again. This time the hub held 1932 iron ore and 648 ice: metallic crushing makes ore, and the electric furnace was still a stranded ghost.
- Loading the save: the **home landing pad was full** (80 of 80 slots: 2422 iron ore, 350 ice, chunks). Buddy's requests for iron ore (min 5000) had drained the hub into it, so the 200 space science requested could never land.

**Fixes after long29:**
- `jettison` takes any hub item, as an inserter over the platform edge would (all items can be thrown overboard in the game).
- Platform status adds `stranded_ghosts` and `ghosts_missing_items`, and the snapshot warns on both. On the save it named the furnace and the two poles, and "lacks space-platform-foundation x8".
- Space status adds `landing_pad_free_slots`, and the snapshot warns when the pad is full and says to drop bulk requests.
- On long29's save, jettisoning 1500 iron ore freed 30 slots. The `not_jettisonable` live check was deleted. Gates: `luac`, clippy, all cargo tests, release build, `live_regressions.sh` 512 passed and 0 failed (`fb-evidence/git-gud/live28`).

### `long30-cont29-opus-open-2590060469-60m`

60 minutes, 71 turns, none provider-limited, 51 tool errors, 0 invariant failures; `plate_automation` and `powered_production` held. Still on Nauvis.
- The hub stayed usable (32 free slots at the end), the landing pad had room again (19 free) and requested only space science. There were 35 `jettison` calls, the carbonic chunks again.
- The platform was still stuck on foundation: its ghosts lacked 8 `space-platform-foundation`. 9 foundation had been queued at the silo since long27, but behind 43 carbonic chunks from the same `ship` call, at 100 kg each (10 per rocket). The silo built about 1 rocket in 30 minutes, so the foundation would have waited hours. The other errors: `verify_production` ×18 and `launch_rocket` ×15 while the silo refilled.

**Fixes after long30:**
- A rocket loads the items the platform's ghosts lack first. On long30's save, the next rocket carried the 9 foundation and only 8 chunks.
- `space_platform action=unship` takes queued cargo back into the character's inventory, standing by the silo; with no items it takes everything back. It fails with `inventory_full` when nothing fits, and `nothing_queued` when there is no queue. A live check unships 2 of 10 queued panels and ships them again.
- Cargo shipped to a platform joins the cargo already waiting for it, rather than forming a second queue that needs its own rocket. The first live run with unship caught this: the 2 panels shipped again left in their own rocket. Gates: `luac`, clippy, all cargo tests, release build, `live_regressions.sh` 514 passed and 0 failed (`fb-evidence/git-gud/live29`).

### `long31-cont30-opus-open-2590060469-60m`

A first attempt hit the usage cap after 6 provider-limited turns. It is kept as `long31-…-60m-capped` and does not count. The rerun from long30's save: 60 minutes, 41 turns, none provider-limited, 58 tool errors, 0 invariant failures; `plate_automation` and `powered_production` held.
- **`planet-discovery-vulcanus` is researched.** Space science landed through the pad again (49 slots free at the end).
- The platform now crushes carbonic chunks too (3 crushers), and the hub stayed below full (21 slots free) with 3 `jettison` calls. Buddy used `unship` three times to reorder the silo queue.
- Buddy started on thrusters. It placed foundation and a thruster ghost at the back (1, 16.5), and queued 2 thrusters, 3 chemical plants, pipes and pipe-to-ground at the silo. The platform's ghosts still lacked the thruster at the end.
- Errors: `wait_for_crafting` ×11, `place_ghosts` ×9 (blocked footprints and 2 refused loose foundation tiles), `launch_rocket` ×8 while the silo refilled. No tool defect was found, so long32 continues from this save unchanged.

### `long32-cont31-opus-open-2590060469-60m`

60 minutes, 37 turns, none provider-limited, 64 tool errors, 0 invariant failures; `plate_automation` and `powered_production` held.
- **Buddy boarded the platform** (`character_surface = platform-1`) and scheduled it for Vulcanus.
- The platform had 1 thruster, 3 chemical plants (ice melting, thruster fuel, thruster oxidizer), 25 pipes and 1 gun turret. It sat in `waiting_for_departure` because the thruster had no fluid: the fuel and oxidizer pipes stopped one tile short of the thruster inputs, over empty space, and the oxidizer line had a one-tile gap. Errors: `place_ghosts` ×17 (blocked footprints, pipes over empty space), `route_belt` ×16, `space_platform board` refused ×5 while the silo refilled.

Sandbox runs on long32's save:
- Filling in the 4 missing pipes (and foundation) fed the thruster, and the platform left. With its single turret and 10 magazines, **the platform was destroyed on the way, and the character with it**.
- Four turrets clustered at one corner, mostly unfed, were also destroyed.
- Six gun turrets across the front, preloaded with 620 magazines between them, arrived at Vulcanus with every turret destroyed and the hub at 657/1000. `space_platform land` then put the character on Vulcanus, and the snapshot said "You are on Vulcanus." So the full path works once the platform is fed and armed.
- **The hub never lays a foundation tile that would close off empty space.** Tile ghosts at a fjord's mouth (2,11), (1,10), (1,11), and in front at (−1..1, −9..−8), stayed ghosts with foundation in the hub. Script `set_tiles` placed them, and filling the enclosed cells let the hub finish the rest. The hub does lay tiles that touch the platform only diagonally.

**Fixes after long32:**
- The snapshot warns on each thruster input with no fluid. It names the fluid and the tiles to pipe to (from `fluidbox.get_pipe_connections`), or says the touching pipe carries none of it.
- When the platform is scheduled for Vulcanus, the rung moves on from "Set course" to "Get <platform> moving", or to arming it first.
- Departures are held until the platform is armed. A platform about to leave orbit has its schedule kept aside (status still shows `stops`, plus `departure_held: "unarmed"`) until it has 6 gun turrets fed by inserters, 4 of them in front of the hub, and 1000 magazines in turrets and hub. These minimums are a judgement from the runs above. A paused platform does not build its ghosts, so the hold is not a pause. `armament` in platform status gives the counts and a `needs` line, and `schedule` explains the hold.
- `place_ghosts` refuses foundation that closes off empty space (`encloses_space`, listing the holes). Platform status reports `foundation_holes` left by existing tile ghosts, and the snapshot warns about them. Foundation connectivity counts diagonal neighbours.

### `long33-cont32-opus-open-2590060469-60m`

60 minutes, 28 turns, none provider-limited, 44 tool errors, 0 invariant failures; `plate_automation` and `powered_production` held.
- The hold worked as intended. The platform stayed at Nauvis (`departure_held: "unarmed"`, stops `["vulcanus"]`). Buddy followed the rung: it landed on Nauvis and queued 522 magazines, 15 foundation and 4 inserters for the platform (11 `ship` calls, 1 `unship`).
- Its foundation plan now leaves no holes (`foundation_holes` empty). One `place_ghosts` was refused with `encloses_space` and corrected.
- At the end the platform still had 1 fed turret, both thruster inputs unfed, and ghosts lacking 33 foundation. Rocket throughput was the limit.
- The other errors were Nauvis work: `route_belt` ×14, `wait_for_crafting` ×6, `collect_from_chest` ×6. No tool defect was found.

### `long34-cont33-opus-open-2590060469-60m-capped` (provider-limited, invalid as a trial)

31 turns, 4 of them provider-limited (the usage cap), 28 tool errors.
- The platform was still held (`unarmed`): 3/6 fed turrets, 1/4 in front, 340/1000 magazines on board, and 763 more magazines plus 4 inserters queued at the silo. Both thruster inputs were still unfed.
- `powered_production` was false in the holdout, although 26 machines were working (125 products). 65 machines sat at `full_output` and 53 at `no_ingredients`. This was not investigated, because the trial is invalid anyway.
- Rocket throughput is the slow part: 5, 3 and 2 rockets in long31, long32 and long33.

### `long34-cont33-opus-open-2590060469-60m` (rerun from long33)

60 minutes, 56 turns, none provider-limited, 33 tool errors, 0 invariant failures; `plate_automation` and `powered_production` held.
- Both thruster inputs are now fed (`thrusters_unfed` empty), and there are no foundation holes. The platform is still held as `unarmed`: 3/6 fed turrets, 1/4 in front, 336/1000 magazines on board.
- 780 magazines and 4 inserters still wait at the silo. Only 2 rockets went up in the hour (31 → 33). At the end the silo had 8/50 parts. In the last 10 minutes it had 11 processing units, 5 low-density structures and 10 rocket fuel to build them from.
- 15 `place_ghosts` errors, mostly dry runs of thruster pipes over empty space. The refusal pointed at the "nearest free spot", which is the wrong fix when a pipe must reach an exact input tile.

### Fixes after long34

- After the first launch, nothing told Buddy why cargo was not leaving. The snapshot now warns whenever a platform has queued cargo but no rocket is ready. It gives the silo's part count and how many of each rocket-part ingredient were made in the last 10 minutes, so the bottleneck is named. Before the first launch, the "Fill the rocket silo" rung gives the same ingredient list.
- `place_ghosts` refusing an entity over empty space on a platform now says first to add `space-platform-foundation` tiles under it in the same call. The nearest free spot follows as an alternative.
- `walk_to` across a grown base failed with `Packet too large` (long10, long19, long26, long33). One collision map covered the whole walk plus padding, and the RCON reply exceeded 16 MiB. Pathfinding now goes in legs of at most 96 tiles, each with its own map. An intermediate leg passes if it gains at least half a leg; the final leg keeps the exact arrival check.
- Sandbox on long34's save: the walk from (51,156) to (386,-109) arrived (502 tiles walked, 75 s), and so did the walk back. The new silo warning reads "the silo has 8/50 rocket parts … low-density-structure (5 made in 10 min)".
- Gates: `luac`, clippy, all cargo tests, release build, `live_regressions.sh` 517 passed and 0 failed (`fb-evidence/git-gud/live31`), `buddy_runtime.sh` passed.

### `long35-cont34-opus-open-2590060469-60m`

60 minutes, 43 turns, none provider-limited, 31 tool errors, 0 invariant failures; `plate_automation` and `powered_production` held.
- The platform was still held (`unarmed`): 3/6 fed turrets, 1/4 in front. Its ghosts lacked no items, yet nothing new was built. 2 rockets went up (33 → 35); 600 magazines were still queued.
- Most `place_ghosts` errors were dry runs for front turrets refused by a crusher, the collector or other ghosts. Buddy found no free spot and spent the rest of the hour reading inventories and belt lanes.

### Sandbox on long35's save, and the fixes

- **Ghosts on built entities never build.** 7 entity ghosts (turrets on the crusher and the collector, an inserter on a pole, a pole on an inserter) overlapped built entities. The hub built a fresh ghost on free foundation within 20 s, but these never. In long35, `place_ghosts` reported a turret at (-3,-6) as `already_built` because a dead ghost of the same name stood there. `stranded_ghosts` now also covers ghosts overlapping a built entity that is not marked for deconstruction or fast-replaceable. Status lists each one with its reason, `place_ghosts` no longer counts them as built and replaces those its plan overlaps, and new `space_platform action=clear_ghosts` removes them all.
- **No room by the hub.** The hub's front edge holds the crusher and collector inserters, and each side fits two turrets in the front half. Two findings removed the need for adjacency. The hub fills item requests on its platform: an empty turret went from 0 to 100 magazines in 20 s. A turret ghost's insert plan survives construction. `place_ghosts` now puts a one-stack ammo request on every new platform turret ghost, and new `space_platform action=load_turrets` requests ammo for built turrets. A turret counts as ready when fed or holding 50+ magazines. Platform status lists `turret_slots`: hub-fed turret + inserter pairs, then free 2×2 spots for loaded turrets. Both are front-half only until the front quota is met. "Front" now means ahead of the hub centre rather than the hub's top edge; the turret at (-6,-4) had failed that boundary.
- **Flights from Nauvis (one thruster, speed 1.1–1.5):**
  - 6 ready turrets (4 in front) with 1116 yellow magazines, no military research: 2 turrets lost by 0.6 of the way, and the platform and character were destroyed by 0.8. Turret ammo fell from 586 to 213 in one minute, while the hub's magazines never reached the loaded turrets.
  - The same, with the hub topping up every turret below 50 each second: hub magazines fell from 530 to 183 into the turrets, and the platform was still destroyed by 0.8. Destroyed turrets were not rebuilt despite spares in the hub.
  - The same, with piercing rounds, military-2, physical-projectile-damage-2 and weapon-shooting-speed-2: **arrived at Vulcanus with all 6 turrets and the hub at full health**, using about 410 magazines. The 400 iron ore shipped for oxidizer ran out on arrival.
- **The arm rule now follows those flights:** 6 ready turrets, 4 in the front half, 600+ magazines of piercing or better (yellow listed separately as too weak), and the three researches. These are red and green science; piercing rounds are 2 yellow magazines + 1 steel + 2 copper. The hub loads turrets with the strongest ammo it holds. Turret ammo requests are a standing order: once `load_turrets` or a turret `place_ghosts` has run, the mod re-requests ammo every second for any turret below 50 magazines, in flight too. `guard_departures` became `tend_platforms`.
- **Crew:** once armed, the platform left without the character, who was on Nauvis. The guard now also holds a departure until an agent character is on the platform (`departure_held: "no_crew"`). The Board rung says the platform waits in orbit.
- **Thrust supply:** the oxidizer plant was starved (hub: 0 iron ore, 0 metallic chunks). The thruster's buffer (900) let the platform start, burn out within a minute, and stall back at Nauvis. Status now has `thrust_short`: thruster-fluid plants whose item ingredients the hub holds under 500 of, with the asteroid-crushing recipe that makes each. A "Stock … thruster fuel before boarding" rung comes before Board. When an unfed input's pipe is connected, the warning now names the starved ingredient instead of blaming pipe gaps.
- Smaller fixes: `hub_ammo_for` read the ammo category from the wrong field (`get_ammo_type().category`; the 2.0 field is `ammo_category`), so no ghost got an ammo request until fixed. `turret_slots` ignores never-built ghosts, because `place_ghosts` replaces them.
- Gates: `luac`, clippy, all cargo tests, release build, `live_regressions.sh` 519 passed and 0 failed (`fb-evidence/git-gud/live32`; new checks: a ghost on a built entity is listed, and `clear_ghosts` removes it), `buddy_runtime.sh` passed.

### `long36-cont35-opus-open-2590060469-60m`

60 minutes, 27 turns, none provider-limited, 32 tool errors, 0 invariant failures; `plate_automation` and `powered_production` held.
- Buddy followed the new arm rung end to end:
  - researched military, military-2, physical-projectile-damage-1 and weapon-shooting-speed-1, and queued both level-2 upgrades;
  - hand-crafted piercing rounds and shipped them (508 queued, 92 aboard);
  - unshipped yellow magazines;
  - ran `clear_ghosts` and `load_turrets`, then placed a slot turret.
- At the end: 5/6 ready turrets, 4/4 in front, 92/600 piercing on board; `thrust_short` iron-ore 0; no never-built ghosts left. Rockets: 2 (35 → 37).
- Errors were long hand-crafts outlasting `wait_for_crafting` timeouts (17) and `launch_rocket` before a rocket was ready (2). No tool defect.

### `long37-cont36-opus-open-2590060469-60m`

60 minutes, 23 turns, none provider-limited, 33 tool errors, 0 invariant failures; `plate_automation` and `powered_production` held.
- All three researches done. The platform had 5/6 ready turrets, 4/4 in front, 192/600 piercing aboard and 408 queued, with `thrust_short` iron-ore 0. Rockets: 2 (37 → 39).
- Errors were again hand-craft waits (20) and early `launch_rocket` (4).

### Sandbox on long37's save, and the fix

- Flight with what was aboard (6 turrets holding 516 yellow, 192 piercing in the hub, upgrades researched; only 500 iron ore added): the turrets fired yellow first and drew piercing top-ups from 0.6 of the way. Two turrets were lost and the platform was destroyed at about 0.85. So mixed ammo is not enough, and the 600-piercing rule stands.
- A piercing magazine weighs 20 kg: one rocket lifts 50 (500 iron ore, 50 gun turrets). The 408 queued rounds need 9 rockets, about 3.3 h at the measured 2.8 rockets/h. Low-density structure is the scarcest rocket-part input (18 per 10 min).
- Platform status now reports `queued_rockets` (`rockets_for`, moved above `platform_summary`). While the research is done, at least 2 rockets of cargo wait, and the platform is unarmed or short of thrust stock, the space rung becomes "Launch rockets faster". It gives the rockets waiting, hours at the current rate (from rocket-part production in the last 10 min), the rocket-part inputs with their rates, the per-rocket lift, and what the platform still needs.
- Gates: `luac`, all cargo tests, `live_regressions.sh` 519 passed and 0 failed (`fb-evidence/git-gud/live33`).

### `long38-cont37-opus-open-2590060469-60m`

60 minutes, 34 turns, none provider-limited, 41 tool errors, 0 invariant failures; `plate_automation` and `powered_production` held.
- Buddy took the "Launch rockets faster" rung and worked on the rocket-part lines (12 `build_layout`, 18 `verify_production`). Rockets doubled: 4 this hour (39 → 43); measured afterwards at 4.4 per hour.
- Aboard: 392/600 piercing. 208 still queued (5 rockets with the iron ore). `thrust_short` iron-ore 0.
- 24 `launch_rocket` calls all failed with `rocket_not_ready`: Buddy polled the silo although queued cargo launches by itself. When cargo is queued on that surface, the refusal now adds guidance to that effect, pointing at the rocket-part inputs (`silos` already gives `rocket_parts`).
- Gates: `luac`, all cargo tests, `live_regressions.sh` 519 passed and 0 failed (`fb-evidence/git-gud/live34`); sandbox on long38's save shows the guidance and "5 rockets of cargo … about 1.1 h at 4.4 rockets/h".

### `long39-cont38-opus-open-2590060469-60m`

60 minutes, 39 turns, none provider-limited, 31 tool errors, 0 invariant failures; `plate_automation` and `powered_production` held.
- 4 rockets again (43 → 47). `launch_rocket` was called once (24 times in long38).
- At the end: 6/6 ready turrets, 4/4 in front, 592/600 piercing aboard, the last 8 queued. Still to come are the 500 iron ore for oxidizer (`thrust_short`, the rung after arming) and boarding.
- Errors were Nauvis building (`route_belt` ×7, `remove_entity` ×6) and two turret dry runs blocked by long-handed inserters.

### `long40-cont39-opus-open-2590060469-60m`

60 minutes, 61 turns, none provider-limited, 23 tool errors, 0 invariant failures. `plate_automation` held; `powered_production` was false (see below).
- **The platform was armed** (7/6 ready, 4/4 front, 600/600 piercing). 3 rockets (47 → 50).
- Buddy queued 498 iron ore for oxidizer. `board` then failed 6 times with `rocket_not_ready`, because every ready rocket took queued cargo, so Buddy unshipped the ore and boarded. Aboard, armed and crewed, the platform left at once without oxidizer stock. Buddy rescheduled it to Nauvis mid-flight ("between planets" ×3 on `land`), landed, and queued the ore again. It ended at Nauvis scheduled for Nauvis.
- `powered_production` false: the holdout samples a radius around the character, who ended by the silo among starved rocket-part machines (70 machines sampled, against 242 in long34). The base had not stopped. The evaluator was left unchanged mid-series.

### Fixes after long40

- **Boarding is booked.** `board` with no ready rocket now succeeds with `booked`: the next ready, empty rocket on that surface is kept for the agent (queued cargo waits for the one after), and the mod launches the character as soon as it is ready, if still in reach of the silo. Bookings lapse after 10 minutes. `launch_character` is shared by `board` and the booking.
- **The hold also waits for thrust stock** (`departure_held: "thrust_stock"`): an armed, crewed platform stays parked while its thruster-fluid ingredients are under the reserve (`thrust_shortages`, shared with status). The reserve is now 450, reported as `thrust_reserve`. Crushing sources are cached.
- **Rungs.** On Nauvis, queued ingredients count toward the reserve, so with the ore queued the next rung is boarding. A platform not scheduled for Vulcanus first gets "Set course". Aboard with a `thrust_stock` hold, the rung says queued shipments still go up without you. The Board rung says to call `board` once and not to unship.
- **Sandbox on long40's save:**
  - "Set course" → schedule (held `thrust_stock`) → "Board" → `board` (booked) → with the silo filled by script, the booked rocket launched the character while the ore stayed queued → the next rocket brought the ore (hub 482) → the hold released and the platform left with the character.
  - Flown with long40's real loadout: 7 turrets became 5, the hub fell to 531 HP, and the ore ran out at 0.88; the platform coasted in at speed 0.17 and arrived. `land` → "You are on Vulcanus."
- Gates: `luac`, all cargo tests, `live_regressions.sh` 519 passed and 0 failed (`fb-evidence/git-gud/live35`; the board check now books before the rocket is ready and expects the automatic launch).

### `long41-cont40-opus-open-2590060469-60m`: Buddy on Vulcanus

60 minutes, 41 turns, none provider-limited, 12 tool errors, 0 invariant failures. `plate_automation` and `powered_production` held. **`space.character_surface == "vulcanus"`**: the plan's acceptance condition is met.
- Sequence (minutes from the first `space_platform` call):
  - 0: `schedule` for Vulcanus (held);
  - 8: `ship` (one refused out of reach, then accepted);
  - 13: `board`;
  - 44: `load_turrets` and `clear_ghosts` on the platform;
  - 54: `land`.
  
  Rockets: 3 (50 → 53).
- At the end the platform waits at Vulcanus with 5 turrets (3 in front) and 153 piercing left. Losses on the way match the sandbox flights.
- 12 errors, none in the space path except the one out-of-reach `ship`.

## Working on Vulcanus and from Vulcanus

### `long42-cont41-opus-open-2590060469-60m`: the first hour on Vulcanus

60 minutes, 38 turns, none provider-limited, 37 tool errors, 0 invariant failures. `plate_automation` and `powered_production` held; Buddy stayed on Vulcanus.
- **Rung:** a fixed string ("Build power and a roboport from what you carried"). The snapshot described only Nauvis.
- **What Buddy did:** worked out the trigger chain itself (tungsten-carbide unlocks foundry research). It smelted steel, hand-crafted 2 solar panels and a chemical plant (carbon), and spent about 40 minutes routing one coal belt from the coal patch to the plant through the cliff terraces. Carbon flowed late in the hour.
- **Blockers:**
  - *No way to load a machine by hand.* With 80 tungsten ore and carbon in hand, the tungsten-carbide assembler could not be loaded, because `insert_items` is deliberately hidden from the model. Buddy filed evaltrial-51h and idled for many turns.
  - *`route_belt` through cliffs:* 50 calls, most failing preflight with underground ends on cliff tiles. Buddy filed an issue.
  - *It arrived without a kit:* no roboport, robots or chemical plant; the Board rung's carry list was generic and named steam power.
  - *The platform stayed in Vulcanus orbit* with no crew and no supply: ammo ran out, asteroids wore it down, and thruster ingredients reached 0. Nothing from Vulcanus can reach it until a silo stands there.

### Fixes after long42

- **`feed_machine_from_inventory` (new bootstrap tool, model-visible):**
  - loads whole crafts (1–20) of an assembler, chemical plant or foundry's current recipe from the character's inventory: item ingredients only, the same count of crafts for all of them;
  - reports fluid ingredients it cannot load;
  - checks reach, dry-runs by default and reports conservation;
  - counts as a manual transfer, like the lab feed.
  - `collect_from_chest` also takes an assembling machine's output, so a first carbon can go from the plant into the assembler by hand.
- **Cliffs in `route_belt`:** Factorio reports corner cliffs' collision boxes with an `orientation` (rotated 45°), and the mod dropped it. The planner therefore blocked the wrong tiles: it allowed underground ends on cliff tiles, and blocked some free tiles next to cliffs.
  - `entities.lua`/`placement.lua` now emit `bounding_box_orientation`.
  - `src/world/mod.rs` `collision_box_tiles` rasterises rotated boxes with a separating-axis test, used by the collision map and `entity_occupied_tiles`.
  - Unit regression: `rotated_cliff_box_keeps_underground_exit_off_its_off_corner_tiles`.
- **Vulcanus facts and ladder:**
  - The snapshot now has `here`: entities, statuses, recipes per machine, acid pumpjacks and items made on the planet the character stands on, when that is not home.
  - `vulcanus_rung` goes: mine a big volcanic rock → mine calcite → solar power (the planet's 400 % solar; no water for steam) → pumpjack on a sulfuric-acid geyser → one tungsten-carbide (carbon plant + assembler, loaded by hand) → a foundry (recipe chain read from prototypes) → tungsten plate → metallurgic science.
  - Each Vulcanus rung ends by pointing Buddy at Nauvis through robots.
  - The Board rung now lists a Vulcanus kit.
- **Steam planner:** offshore-pump spots must border water and no other fluid tile; lava pumps are rejected.
- **Robots:**
  - Ghost shortfalls are per network: each covered ghost counts against the network that builds it.
  - `home_logistics.construction_robots_available` is reported, with a warning when every robot is busy and ghosts wait.
  - A coverage fact counts home machines in construction range, with a warning below half coverage that names the uncovered machine nearest the network and how to extend it remotely (a roboport ghost inside the range).
  - Away from home, the missing-items warnings and the `place_ghosts` guidance tell Buddy to have Nauvis make the items (assembler into a passive-provider chest), not to stock a chest by hand. Hand-refuel warnings for home are dropped while away.
- **Summary:** `away_planet_entities` and the Vulcanus trigger techs (`calcite_processing`, `tungsten_carbide`, `foundry`, `big_mining_drill`, `metallurgic_science_pack`) under `.space.techs`.
- **Sandbox on long41's save:**
  - `route_belt` from (-53,-48) to (-28.5,79.5), Buddy's failing route, now preflights clean and placed all 71 pieces in one call (`connected: true`).
  - The rung read "Make one tungsten-carbide" with the live machine states.
  - With a script-placed chemical plant and assembler (power and acid also by script):
    - `feed_machine_from_inventory` loaded 10 coal;
    - `collect_from_chest` took 2 carbon from the plant;
    - feeding carbon and tungsten ore to the assembler crafted tungsten carbide, and `foundry` was researched;
    - the rung moved on to "Craft a foundry".
  - On Nauvis (1 roboport, 111/297 machines covered): a roboport and pole ghost placed with `place_ghosts surface=nauvis` from Vulcanus reported the shortfall in the away wording. Once a provider chest held the items, robots built it; coverage rose to 137/297, and the warning named the next spot.
- Gates: `luac`, clippy, all cargo tests, `live_regressions.sh` 522 passed and 0 failed (`fb-evidence/git-gud/live36`; new live checks: whole-craft machine feed leaves the remainder with the character, and `collect_from_chest` takes machine output).

### `long43-cont42-opus-open-2590060469-60m`: foundry and big-mining-drill researched

60 minutes, 39 turns, none provider-limited, 88 tool errors, 0 invariant failures. `plate_automation` and `powered_production` held. Vulcanus now has 284 force entities (`away_planet_entities`).
- **Trigger chain:**
  - `feed_machine_from_inventory` (17 calls) loaded the first tungsten carbide → **foundry researched**.
  - Buddy then gathered the foundry's inputs:
    - 51 tungsten carbide (ore from huge volcanic rocks);
    - steel from an automated coal-fed furnace chain it built;
    - 30 circuits;
    - refined concrete from water made by acid neutralisation → steam condensation;
    - lubricant from an oil refinery on simple coal liquefaction.
  - It crafted the foundry in an assembler → **big-mining-drill researched**.
- **Blocker at the end: lava.** Buddy hunted lava for molten iron with `plan_steam_power` pump-spot scans (about 2 million positions). The post-long42 water check rightly rejects lava pumps there, and no tool located lava tiles. It idled the last turns.
- Errors: `mine_at` 105 calls, many stuck walking among cliffs toward rocks; full inventory cut mining short. The cliff-box fix also changes the walking collision map, but this run used it and still got stuck at cliff pockets [INFERENCE: pathing across terraces remains hard].

### Fixes after long43

- `find_nearest_resource resource_type=lava` (any fluid whose tiles an offshore pump draws from, read from the tile prototypes) returns the nearest tile and shore guidance; water keeps its own result and steam box. On long43's save it found lava 104 tiles from Buddy (82 lava tiles within 8).
- The tungsten-plate rung says it needs two foundries (one makes molten iron, piped into the tungsten-plate one). It offers `molten-iron` from iron ore when that recipe is enabled (it is after foundry research), and lava via `find_nearest_resource`.
- Gates: `luac`, clippy, all cargo tests, `live_regressions.sh` 522 passed and 0 failed (`fb-evidence/git-gud/live37`).

### `long44-cont43-opus-open-2590060469-60m`: metallurgic science researched

60 minutes, 37 turns, none provider-limited, 56 tool errors, 0 invariant failures. `plate_automation` and `powered_production` held. Vulcanus: 434 force entities.
- Buddy found lava on the east shore, piped it into a foundry (molten iron, then molten copper from lava), cast iron plates, and powered the site with steam engines on acid neutralisation.
- It crafted a big mining drill → tungsten-steel researched → a tungsten plate → **metallurgic-science-pack researched**. It hand-made the first metallurgic science pack and started automating carbide (output inserter and chest).
- The rung was wrong at one step: it said a tungsten plate unlocks next, but the tungsten-plate recipe is locked behind tungsten-steel, which crafting a big mining drill unlocks. Buddy read the tech tree and worked it out itself.
- One foundry did both jobs: Buddy made molten iron first, held it in pipes, then switched the recipe.
- Walking: 41 of 81 `walk_to` calls arrived. The platform buddy-1 was destroyed in Vulcanus orbit (no ammo, no supply).

### Fixes after long44

- **Rungs:** after big-mining-drill: craft a big mining drill (unlocks tungsten-steel) → make a tungsten plate (unlocks metallurgic science) → automate metallurgic science. These read from the real trigger tree, checked on long44's save: `tungsten-steel` is triggered by crafting `big-mining-drill`; `metallurgic-science-pack` by crafting `tungsten-plate`. The molten-iron text offers ore or lava, and one foundry held in pipes or two foundries. Metallurgic packs made on the planet are tracked.
- **Walking through cliff mazes:** when the windowed A* finds no route, a waypoint walk sticks, or a long-walk leg stops gaining ground, the walker now asks Factorio's own pathfinder:
  - new remotes `request_walk_path`/`get_walk_path`, with the result from `on_script_path_request_finished`;
  - it walks the engine's turning points, then falls back to the straight walk as before.
  - A character wedged between corner cliffs (`can_stand` false) starts the engine path from the nearest clear spot; the engine finds no path from a start touching a cliff. Long44's wedge at (27.4,48.5) sat between four 45° cliff boxes.
  - Sandbox on long44's save: 17 walks, including long43's stuck `mine_at` spots and long44's failed `walk_to` targets. 14 arrived, e.g. 252 tiles to the coal patch across the cliff terraces, and 331 tiles to the west lava shore. The 3 misses stopped 0.3–1.8 tiles from targets in occupied tiles.
- Gates: `luac`, clippy, all cargo tests, `live_regressions.sh` 522 passed and 0 failed (`fb-evidence/git-gud/live38`).

### `long45-cont44-opus-open-2590060469-60m`: metallurgic science made by machines

60 minutes, 28 turns, none provider-limited, 48 tool errors, 0 invariant failures. `plate_automation` and `powered_production` held. Vulcanus: 664 force entities.
- **Walking:** no `Could not move within … reach` failures (long43: about 15, from `mine_at` and `collect_from_chest` approaches). Only 1 `walk_to` call.
- **Second foundry:** Buddy built it, its refined concrete fed by its own foundry's molten iron and the water chain, then swapped the foundries to reach lava past a cliff. It got metallurgic science running in a foundry with molten copper from lava: **72 packs made**, and a big mining drill on tungsten ore.
- **Hand-fed:** inputs (carbide, tungsten plate, calcite, coal) are still loaded by hand: 61 `feed_machine_from_inventory` and 55 `collect_from_chest` calls. Buddy said so every turn and started a carbide belt.
- **Nauvis:** Buddy checked whether it could fix Nauvis remotely and found the home network holds no items. All 18 home labs have lacked logistic-science-pack since about long42, so research (fluid-wagon) has stalled.
- **What the packs unlock:** every technology researchable with metallurgic packs (coal-liquefaction, asteroid-reprocessing, low-density-structure-productivity, …) also needs red, green and blue packs, plus space or production packs. The packs count only once they share a lab with Nauvis science, which needs interplanetary logistics:
  - a silo on Vulcanus, whose rocket parts must be made there from shipped processing units, low-density structures and rocket fuel;
  - a platform shuttle;
  - landing pads with requests.
  
  The harness supports none of that without the agent at the Nauvis silo.

### Fixes after long45

- The idle-lab warning names the current research and the packs the labs lack, with a remote fix while away (research what Nauvis still makes, or rebuild the pack's supply with `place_ghosts surface=nauvis`). On long45's save: "Research (fluid-wagon) is queued but no lab at home is working: they lack logistic-science-pack (18 labs)…".
- Gates: `luac`, all cargo tests, `live_regressions.sh` 522 passed and 0 failed (`fb-evidence/git-gud/live39`).

## Supply line: prepare before leaving

Long45 showed the limit: metallurgic packs are only useful in a lab beside Nauvis science, and Buddy on Vulcanus could not move anything between planets.
- The Nauvis robot network held no items, so ghosts placed from Vulcanus could not be built.
- Leaving Vulcanus needs a silo and 50 rocket parts made there.

The supply line therefore has to be set up before departure. A new chain branches from long40's save (Buddy on Nauvis, armed platform at Nauvis).

### Mechanics, verified in the sandbox

- **Vanilla automatic requests do not engage here.** With `silo.use_transitional_requests` on and a hub request `import_from = "nauvis"`, a covered silo (long40 save) got no `transitional_request_target`, and its rocket inventory refused inserts.
- **Requester chest + inserter into the silo works with real robots** (manual mode): robots filled the chest from the network and the inserter loaded the ready rocket (24 → 150 gears). Vanilla does not launch a partly filled rocket.
- **Landing pads pull from platforms in orbit.** On long45's save, a Vulcanus pad requesting 100 processing units and 50 gears took both from a platform orbiting Vulcanus, though the hub itself imported processing units from Nauvis.
- **Platform schedules take wait conditions:** `all_requests_satisfied`, `time` and `inactivity`, with `compare_type = "or"`.

### What the mod does now

- **`space_platform action=supply`** sets the hub's own vanilla import requests (a logistic section per platform and planet; `stops[1]` picks the planet, default home; 0 drops an item).
- **`process_supply`** runs every second after `process_shipments`:
  - It serves each platform orbiting a planet where it has unmet imports. Cargo launched in the last 75 s counts as delivered.
  - It uses a silo with a **supply chest** (a requester or buffer chest whose inserter drops into the silo). The chest's request is set to the shortfall not yet in the rocket or the chests, written only when it changes.
  - The rocket launches to the hub once it holds the shortfall, is full, or the network has none left of what it lacks.
  - The agent's queued shipments and boarding bookings on that surface go first.
  - Chests no longer needed stop requesting.
- **Schedules with two or more stops are shuttles:**
  - where the hub imports from the planet, it waits for `all_requests_satisfied` (or 10 min);
  - elsewhere, it waits for 30 s of inactivity (or 5 min) while landing pads take what they request.
  
  `supply` re-derives these waits.
- **Departure holds:**
  - `unarmed` and `thrust_stock` hold only where the platform can be restocked (a silo with a supply chest, or an agent beside or above a silo), so a shuttle that spent its ammo at Vulcanus still flies home.
  - `no_crew` holds only the first trip to a planet where the force has built nothing.
  - A parked schedule keeps its current stop.
- **Status:** `platform.supply` (`from`, `requests`, `missing`, silos with a supply chest); `space.home_supply` (silos in logistic range, silos with a supply chest, logistic robots, storage chests, imports the network holds none of).
- **Rungs before "Set course":**
  1. silo inside logistic range;
  2. 20 logistic robots;
  3. a storage chest;
  4. a supply chest at the silo;
  5. the platform's standing supply (piercing magazines, iron ore, foundation);
  6. stock the network with what it lacks.
  
  The Board kit adds a cargo-landing-pad and logistic robots.
- **On Vulcanus:** place the carried landing pad, then turn the orbiting platform into a shuttle (`stops=["nauvis","vulcanus"]`) and add what Vulcanus needs to its supply and the pad's requests.
- **Sandbox on long40's save:**
  - `supply` of 200 gears through a scripted network (roboport, 20 logistic robots, storage chest, requester chest, inserter): the chest's request went 164 → 134 → … as the rocket filled, the rocket launched at the shortfall, and **202 gears reached the hub**.
  - The new rungs advance one by one as each piece is placed, ending at "Set buddy-1's course for Vulcanus". Supply waits while Buddy's own iron-ore shipment is queued.
- Gates: `luac`, clippy, all cargo tests, `live_regressions.sh` 524 passed and 0 failed (`fb-evidence/git-gud/live40`; new checks: `supply` sets a hub import request and reports what is missing, and a count of 0 drops it).

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
