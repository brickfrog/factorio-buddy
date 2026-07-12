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
    cargo test
    find mod/claude-interface -name '*.lua' -print0 | xargs -0 luac -p

doctor:
    @command -v claude >/dev/null && echo "ok  claude  $(claude --version)" || echo "!!  claude CLI missing"
    @test -x "${FACTORIO_BIN:-/mnt/games/SteamLibrary/steamapps/common/Factorio/bin/x64/factorio}" && echo "ok  Factorio" || echo "!!  Factorio missing; set FACTORIO_BIN"
    @test -f mod/claude-interface/control.lua && echo "ok  Factorio Buddy mod" || echo "!!  mod missing"
