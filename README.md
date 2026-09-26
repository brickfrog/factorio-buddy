# factorio-buddy

One autonomous NPC that plays Factorio beside you.

The NPC has its own character. It observes the real world, walks, mines,
crafts, builds, and responds to messages from the in-game Buddy panel. There
is no Python runtime, planner service, ledger, journal, learning framework,
telemetry relay, or multi-agent layer.

## Run it

Requirements:

- Factorio 2.0
- Rust
- An authenticated `claude` CLI

Then:

```bash
just play
```

Join `localhost:34197` from Factorio's multiplayer menu. Press `Ctrl+Shift+C`
to open the Buddy panel. The managed server tolerates long background-client
stalls instead of dropping the local graphical client after Factorio's default
20-second timeout.

`just play` builds the Rust binaries, installs the included Lua mod into an
isolated `.factorio-buddy` write-data directory, replaces the Buddy save with a
new game, starts the headless server, waits for RCON, registers one NPC
character, and starts the model/tool loop. Ctrl+C stops both the NPC and the
server. New games use Factorio's peaceful mode, so enemy bases do not attack
unless provoked.

New games default to map seed `2590060468`, a
[community-recommended Factorio 2.0 seed](https://www.reddit.com/r/factorio/comments/1jmwklg/best_starting_seed_i_have_found_so_far/)
published with fully default settings, separated starting ores, useful water,
and room to explore. It has also been previewed locally with this repository's
Factorio 2.0.77, Space Age, Buddy mod, and map-generation settings. Set
`FACTORIO_MAP_SEED` in `.env` or pass `--map-seed` directly to choose another
seed. The seed is used only while creating a save; `just resume` never changes
the existing world's seed.

Use `just resume` to continue the existing isolated Buddy world. Use `just npc`
if Factorio is already running with RCON and the mod installed.

## Autonomy

Autonomy continues while Buddy is running, whether or not a human player is
currently connected. The default autonomous interval is 30 seconds. Override
the interval when launching if needed:

```bash
BUDDY_HEARTBEAT_SECONDS=60 just play
```

Set `BUDDY_HEARTBEAT_SECONDS=0` or run `just chat` for chat-only operation.

Chat from players always comes first. A new message preempts an autonomous turn
but never cancels another player's request: player requests run in arrival
order, and each one gets exactly one final reply. When a turn is cancelled,
Buddy stops Claude and its MCP server before any new turn starts. The next
prompt names the tool calls whose outcome is unknown, so the model inspects the
world before it acts. Buddy never repeats those calls automatically. If a
resumed Claude session turns out to be missing, Buddy retries the turn once in a
fresh session only when the failed attempt made no tool calls; otherwise the
turn fails and the next turn gets the outcome-unknown note.

Other runtime options (each flag also has an environment variable):

| Flag | Environment | Default | Purpose |
|---|---|---|---|
| `--model` | `MODEL` | `claude-opus-5-5` | Claude model passed to Claude Code |
| `--issue-project-root` | `BUDDY_ISSUE_PROJECT_ROOT` | this repository | Beads tracker that receives `file_issue` calls |
| `--max-autonomous-turns` | `BUDDY_MAX_AUTONOMOUS_TURNS` | `0` (unlimited) | Stop starting autonomous turns after N; the server keeps running |
| `--autonomy-deadline-seconds` | `BUDDY_AUTONOMY_DEADLINE_SECONDS` | `0` (unlimited) | Stop autonomy after S seconds online; cancels the turn that is running at that time |
| `--evidence-log` | `BUDDY_EVIDENCE_LOG` | none | Append JSONL evidence for each turn, tool outcome, decision, and budget event |
| `--decision-mode` | `BUDDY_DECISION_MODE` | `off` | Experimental fuel-maintenance decision: `off`, `deterministic`, `jev-shadow`, `jev` |

Buddy keeps the last 8 tool outcomes in memory and adds a summary of them to the
next autonomy prompt. If one call fails 3 times within those 8 outcomes with the
same arguments and the same error, and the same call has not succeeded since,
the summary tells the model to inspect the world and make a new plan. Other
calls in between (such as inspections) do not reset the count. It does not
block the call.

`--decision-mode` is an experiment and is not for production use. Before an
autonomous turn, Buddy can do one dry run of `repair_fuel_sustainability`. Then
it chooses one of two actions: run that controller, or give control back to
Opus. Opus always takes the first turn, and at most one maintenance action runs
between Opus turns. `jev-shadow` records the Jev decision only and never changes
the world. `jev` runs the repair only if Jev returns a valid answer with
confidence of 0.90 or more, and the new dry run must give the same preview
again. Jev's choice must also be its highest-probability option. After a repair
runs, Buddy compares the transaction the controller actually executed with the
approved preview; a different one is logged as `transaction_changed` and control
goes back to Opus. A consumer whose repair failed is skipped until the next
Opus turn completes. The two Jev modes require `TYPESAFE_API_KEY`.
`BUDDY_JEV_BUDGET_USD` (default `5`) sets the spending cap.

## Persona

Buddy's built-in system prompt owns the gameplay, tooling, verification, and
safety rules. You can append a custom strategic temperament without replacing
those rules. Copy the example and edit it:

```bash
cp .env.example .env
```

The normal `just play`, `just resume`, `just npc`, and `just chat` commands load
`.env` automatically. Set `BUDDY_PERSONA` there to describe the kind of factory
manager you want. The included example emphasizes root-cause repairs, scalable
throughput, expansion, and switching away from unproductive fixation.

For a one-off run, use either the environment or the equivalent CLI option:

```bash
BUDDY_PERSONA="Build boldly and optimize for sustained expansion." just resume
./target/release/buddy --persona "Build boldly and optimize for sustained expansion."
```

## Current limitations

- TODO: implement end-to-end fluid logistics. Pipe entities can be observed and
  placed, but the NPC does not yet have a trustworthy pipe-routing, fluid-flow,
  pump, or fluid-production verifier. It must not claim oil or chemical
  production is automated until those checks exist.
- Train routing and logistic-robot network planning are not yet supported as
  complete, verified automation controllers.

## Architecture

```text
Claude CLI
    ↕ MCP
Rust buddy + factorioctl-mcp
    ↕ RCON
Factorio Buddy Lua mod
    ↕
NPC character in Factorio
```

The Lua mod is required because Factorio exposes its runtime world and entity
APIs to mods. All model hosting, tool serving, RCON communication, NPC
lifecycle, server startup, and autonomy are owned by Rust.
