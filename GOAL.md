# Factorioctl Next Goals

## Direction

Keep moving the project away from prompt-dependent behavior and toward explicit
runtime contracts. The bridge should become boring to operate: fewer false
warnings, clearer tool failures, typed boundaries, and game-aware planners for
the places where the LLM currently burns turns.

## Priority 1: Normalize Tool Result Semantics

The bridge currently logs many different outcomes as `tool_result ERROR`:
expected empty results, normal game rejections, invalid requests, SDK failures,
MCP deserialization failures, and real infrastructure problems.

Add a small classifier for tool and SDK results:

- `ok`: successful result, including successful mutation.
- `expected_miss`: valid request but nothing to do or nothing found.
- `invalid_request`: malformed parameters or schema mismatch.
- `game_rejected`: Factorio refused a valid action, such as invalid placement.
- `sdk_failure`: model or Claude Code SDK failed the invocation.
- `infrastructure_failure`: bridge, RCON, MCP, server, or mod unavailable.

Acceptance criteria:

- Expected gameplay misses such as "no items in inventory" and mined count zero
  do not log at warning level by default.
- Real malformed requests still log as warnings or errors with a useful class.
- The journal stores compact classified failures instead of noisy raw blobs.
- Tests cover representative examples from the painful logs.

## Priority 2: Add Typed Bridge Schemas

The Python bridge still handles too many soft JSON-shaped dictionaries. Add
typed models for the bridge boundaries where shape errors have been expensive.

Candidate typed surfaces:

- Agent config.
- Ledger blocks.
- Learning proposals.
- Tool calls.
- Tool results.
- Journal events.
- Bridge log event records.

Use Pydantic or a similarly explicit validation layer on the Python side. Rust
already has stronger types; the bridge needs the same discipline at its edges.

Acceptance criteria:

- Bad tool parameters fail before reaching the MCP server when possible.
- Shape errors report the exact field and expected type.
- Tests cover the old "invalid type: map, expected sequence" failure mode.
- Typed objects serialize back to the existing log/journal formats cleanly.

## Priority 3: Remote API Manifest

Now that gameplay Lua lives in the `claude_interface` mod, add a manifest for
the remote API so Rust wrappers, Lua remote names, and golden tests cannot drift
quietly.

Possible manifest shape:

```json
{
  "place_entity": {
    "args": ["agent_id", "entity_name", "x", "y", "direction"],
    "returns": "PlaceEntityResult"
  }
}
```

Acceptance criteria:

- Every Rust wrapper in `src/client/lua.rs` maps to a manifest entry.
- Every manifest entry is exposed by `companion/mod/claude-interface/control.lua`.
- Golden tests fail if a wrapper references a missing remote.
- Stale-mod fallback errors name the missing remote and the sync action.

## Priority 4: Split Mod Lua By Domain

`control.lua` now owns the right logic, but it is too large. Split it into Lua
modules once the remote API manifest gives us a stable map.

Candidate modules:

- `json_response.lua`
- `characters.lua`
- `inventory.lua`
- `entities.lua`
- `placement.lua`
- `recipes.lua`
- `research.lua`
- `transport.lua`
- `power.lua`
- `diagnostics.lua`

Acceptance criteria:

- `control.lua` remains the mod entrypoint and remote interface registration
  point.
- Domain modules do not create hidden storage initialization order issues.
- `luac -p` checks every Lua file.
- Golden tests and live smoke tests still pass.

## Priority 5: Build A Steam Power Planner

Power generation against water is still a turn sink. Add a planner that returns
a valid layout before mutating the game.

Proposed tool:

```text
plan_steam_power(water_area, target_pos)
```

It should return:

- Offshore pump position and direction.
- Boiler position and direction.
- Steam engine position and direction.
- Required pipe path.
- Fuel insertion target.
- Pole positions from generator to target.
- A list of blockers or missing materials.

Acceptance criteria:

- The plan is checked before any placement.
- `build_steam_power` can consume the plan directly.
- The planner handles shoreline orientation instead of relying on explicit
  hand-authored coordinates.
- Live smoke verifies pump, boiler, steam engine, pipe, fuel, and pole
  connectivity.

## Priority 6: Make Skill Usage Auditable

The bridge now has SDK skill plumbing, but it should be obvious whether the
model actually sees and uses the control skill.

Acceptance criteria:

- SDK init logs show configured skills, visible skills, and whether the Skill
  tool is available.
- Factorio MCP tool use is blocked until the required control skill has been
  read when skill gating is enabled.
- Legacy prompt-injected skill snippets stay disabled unless explicitly used as
  a fallback.
- Tests prove autonomy and execution prompts do not reintroduce the old skill
  library spam.

## Priority 7: Reviewable Self-Improvement Inbox

Let agents propose reusable knowledge without direct repo writes.

Proposal block types:

- `<skill_proposal>`
- `<diagnostic_proposal>`
- `<script_proposal>`
- `<bug_report>`

Acceptance criteria:

- Proposals are parsed out of final replies and hidden from player-facing text.
- Proposals land in a reviewable folder or JSONL file.
- Promotion requires an operator or trusted command.
- Rejected proposals are recorded so the same bad idea does not repeat forever.

## Priority 8: Operator Watchdog

Make the normal play loop resilient to server drops, RCON reconnects, rate
limits, and accidental disconnects.

Acceptance criteria:

- One operator-facing command resumes without wiping the save.
- RCON reconnects and retries are visible but not noisy.
- 429 reset times are rendered in local time.
- Autonomy pauses during rate-limit windows and resumes afterward.
- The bridge does not go idle just because the player disconnected briefly.

## Suggested Order

1. Normalize tool result semantics.
2. Add typed bridge schemas around the noisy boundaries.
3. Add a remote API manifest and wrapper validation.
4. Split the mod Lua by domain.
5. Build the steam power planner.
6. Finish auditable SDK skill usage.
7. Add the reviewable self-improvement inbox.
8. Wrap the whole thing in an operator watchdog.

