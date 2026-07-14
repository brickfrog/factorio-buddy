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
- `../scripts/smoke_agent_binding.sh` - Proves independent NPC character binding
- `cleanup.sh` - Stops server and cleans up

## Server Ports

- RCON: `127.0.0.1:27016` (test server)
- Game: `34198` (for spectating)
