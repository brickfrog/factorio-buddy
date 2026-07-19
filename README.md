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
