# Testing factorioctl

This directory contains test infrastructure for the factorioctl CLI.

## Quick Start

```bash
# Build, start a disposable isolated server, exercise the Rust/Lua/RCON path,
# and clean it up automatically.
just test-live
```

## Test Files

- `setup.sh` - Builds the Rust binaries, creates an isolated map, and starts Factorio
- `run_tests.sh` - Runs the test suite against the running server
- `live_regressions.sh` - Verifies high-risk mod and MCP contracts in Factorio
- `buddy_runtime.sh` - Verifies Buddy's managed-server security and lifecycle
- `../scripts/smoke_agent_binding.sh` - Proves independent NPC character binding
- `cleanup.sh` - Stops server and cleans up

The live regressions use raw Lua only to create disposable fixtures through the
explicitly enabled trusted-operator path. Behavior under test goes through the
shipped `/claude` mod dispatcher or model-facing MCP server: research triggers,
reach, item conservation, surface scoping, entity lookup, production
verification, route reuse, protocol errors, and RCON connection reuse.

## Server Ports

- RCON: `127.0.0.1:27016` (test server)
- Game: `34198` (for spectating)
- Buddy lifecycle RCON: `127.0.0.1:27217` (override with `BUDDY_TEST_RCON_PORT`)
- Buddy lifecycle game: `34399` (override with `BUDDY_TEST_GAME_PORT`)

`buddy_runtime.sh` launches Buddy twice with a temporary HOME, write-data
directory, and save. It proves that managed RCON is loopback-only with private
generated credentials, a same-agent controller cannot take the active lease,
unexpected owned-server death terminates Buddy, and clean shutdown leaves no
Factorio process behind.
