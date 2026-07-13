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
to open the Buddy panel.

`just play` builds the Rust binaries, installs the included Lua mod into an
isolated `.factorio-buddy` write-data directory, replaces the Buddy save with a
new game, starts the headless server, waits for RCON, registers one NPC
character, and starts the model/tool loop. Ctrl+C stops both the NPC and the
server.

Use `just resume` to continue the existing isolated Buddy world. Use `just npc`
if Factorio is already running with RCON and the mod installed.

## Autonomy

Autonomy runs only while a human player is connected. The default autonomous
interval is 30 seconds. Override the interval when launching if needed:

```bash
BUDDY_HEARTBEAT_SECONDS=60 just play
```

Set `BUDDY_HEARTBEAT_SECONDS=0` or run `just chat` for chat-only operation.

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

Factory control uses two independent observations. Force-wide production and
consumption statistics measure whether the milestone is making sustained
progress. A directed item graph derived from live belts, underground belts,
splitters, inserters, direct mining outputs, recipes, and burner fuel paths
identifies the first broken producer-to-consumer dependency. The NPC receives a
compact audit every turn and can inspect the full graph through MCP; placement
helpers execute repairs but do not choose strategy or fixed layouts.

TODO: Extend the directed material graph beyond item transport. Pipe and fluid
networks, train and station routing, and logistic-robot provider/requester
networks are not yet modeled as causal graph edges. Until those adapters exist,
the automation audit can observe their effects through force-wide production
and consumption rates, but it cannot prove or diagnose their physical paths.
