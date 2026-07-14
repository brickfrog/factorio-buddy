default:
    @just --list --unsorted

# Build the Rust CLI, MCP server, and NPC runtime.
build:
    cargo build --release

# Create a new local game, then start the server and one NPC.
play: build
    ./target/release/buddy --start-server --fresh

# Resume the existing local game, then start the server and NPC.
resume: build
    ./target/release/buddy --start-server

# Start only the NPC against an already-running RCON-enabled game.
npc: build
    ./target/release/buddy

# Chat-only NPC: no autonomous model turns.
chat: build
    ./target/release/buddy --heartbeat-seconds 0

test:
    cargo test --all-targets --locked
    find mod/claude-interface -name '*.lua' -print0 | xargs -0 luac -p

# Run every static/unit gate used in CI.
check:
    cargo fmt --all -- --check
    cargo clippy --all-targets --locked -- -D warnings
    cargo test --all-targets --locked
    find mod/claude-interface -name '*.lua' -print0 | xargs -0 luac -p
    cargo build --release --all-targets --locked

# Start one disposable Factorio server and exercise the real Rust/Lua/RCON path.
test-live: build
    #!/usr/bin/env bash
    set -euo pipefail
    save="$(mktemp --suffix=.factorio-buddy-test.zip)"
    cleanup() {
        ./tests/cleanup.sh >/dev/null 2>&1 || true
        rm -f "$save"
    }
    trap cleanup EXIT
    ./tests/setup.sh "$save"
    ./tests/run_tests.sh
    FACTORIOCTL_BIN="$PWD/target/release/factorioctl" \
        FACTORIO_RCON_PORT=27016 \
        FACTORIO_RCON_PASSWORD=test_password \
        ./scripts/smoke_agent_binding.sh

doctor:
    @command -v claude >/dev/null && echo "ok  claude  $(claude --version)" || echo "!!  claude CLI missing"
    @test -x "${FACTORIO_BIN:-/mnt/games/SteamLibrary/steamapps/common/Factorio/bin/x64/factorio}" && echo "ok  Factorio" || echo "!!  Factorio missing; set FACTORIO_BIN"
    @test -f mod/claude-interface/control.lua && echo "ok  Factorio Buddy mod" || echo "!!  mod missing"
