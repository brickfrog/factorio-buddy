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
