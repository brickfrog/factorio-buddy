#!/bin/bash
# Unified launcher for claude-in-factorio
# Usage:
#   ./run.sh                  # start bridge (nauvis only, scale=1)
#   SCALE=3 ./run.sh          # start bridge with 3 agents (nauvis, vulcanus, fulgora)
#   SCALE=5 ./run.sh          # all 5 planets
#   ./run.sh fresh            # fresh world + setup surfaces + bridge
#   ./run.sh restart          # stop server, start server, bridge
#   ./run.sh restart fresh    # stop, fresh world, bridge
#   ./run.sh server           # just start the server
#   ./run.sh server fresh     # just start fresh server
#   ./run.sh bridge           # just start the bridge
#   ./run.sh stop             # stop server
#   ./run.sh sync             # sync mod only

set -e
trap 'echo ""; echo "Interrupted."; exit 130' INT TERM
PROJECT_ROOT="$(cd "$(dirname "$0")" && pwd)"
SERVER_DATA_DIR="${FACTORIO_SERVER_DATA:-$PROJECT_ROOT/.factorio-server-data}"
BRIDGE_STATE_DIR="${FACTORIOCTL_BRIDGE_STATE_DIR:-$SERVER_DATA_DIR/bridge-state}"

CMD="${1:-bridge}"
FRESH=false

# Parse args
for arg in "$@"; do
    case "$arg" in
        fresh) FRESH=true ;;
    esac
done

# Defaults
GROUP="${GROUP:-doug-squad}"
SCALE="${SCALE:-1}"
MODEL="${MODEL:-}"
AUTONOMY_REQUIRES_PLAYER="${AUTONOMY_REQUIRES_PLAYER:-false}"
EXTRA_ARGS=""

if [ -n "$MODEL" ]; then
    EXTRA_ARGS="$EXTRA_ARGS --model $MODEL"
fi

sync_mod() {
    uv run --project "$PROJECT_ROOT" python "$PROJECT_ROOT/bridge/pipe.py" --sync-mod
}

start_server() {
    if pgrep -f "factorio.*--start-server" > /dev/null; then
        echo "Server already running."
        return
    fi
    if [ "$FRESH" = true ]; then
        "$PROJECT_ROOT/start-server.sh" --fresh
    else
        "$PROJECT_ROOT/start-server.sh"
    fi
}

stop_server() {
    "$PROJECT_ROOT/stop-server.sh"
}

start_bridge() {
    local flags="--group $GROUP --scale $SCALE"
    if [ "$FRESH" = true ]; then
        flags="$flags --setup-surfaces"
    fi
    case "${AUTONOMY_REQUIRES_PLAYER,,}" in
        1|true|yes|on)
            flags="$flags --autonomy-requires-player"
            ;;
        *)
            flags="$flags --no-autonomy-requires-player"
            ;;
    esac
    echo ""
    echo "Starting bridge (scale=$SCALE)..."
    exec uv run --project "$PROJECT_ROOT" python "$PROJECT_ROOT/bridge/pipe.py" $flags $EXTRA_ARGS
}

case "$CMD" in
    stop)
        stop_server
        ;;
    sync)
        sync_mod
        ;;
    server)
        sync_mod
        start_server
        ;;
    bridge)
        start_bridge
        ;;
    restart)
        stop_server
        rm -f "$PROJECT_ROOT/bridge/.session-"*.json \
              "$PROJECT_ROOT/bridge/.sessions.json" \
              "$BRIDGE_STATE_DIR/.session-"*.json \
              "$BRIDGE_STATE_DIR/.sessions.json"
        echo "Cleared agent sessions"
        sleep 2
        sync_mod
        start_server
        start_bridge
        ;;
    fresh)
        # Shortcut: fresh = restart fresh
        FRESH=true
        stop_server 2>/dev/null || true
        # Fresh world = blank slate: wipe ALL persistent agent memory, or the
        # agent boots believing it already finished a factory that no longer
        # exists (stale ledger/journal) and just spins re-scanning.
        rm -f "$PROJECT_ROOT/bridge/.session-"*.json \
              "$PROJECT_ROOT/bridge/.sessions.json" \
              "$PROJECT_ROOT/bridge/.ledger-"*.json \
              "$PROJECT_ROOT/bridge/.journal-"*.jsonl \
              "$PROJECT_ROOT/bridge/.reflection-"*.json \
              "$PROJECT_ROOT/bridge/.skills.json" \
              "$BRIDGE_STATE_DIR/.session-"*.json \
              "$BRIDGE_STATE_DIR/.sessions.json" \
              "$BRIDGE_STATE_DIR/.ledger-"*.json \
              "$BRIDGE_STATE_DIR/.journal-"*.jsonl \
              "$BRIDGE_STATE_DIR/.reflection-"*.json \
              "$BRIDGE_STATE_DIR/.skills.json"
        echo "Cleared agent memory (sessions, ledger, journal, reflection, skills)"
        sleep 2
        sync_mod
        start_server
        start_bridge
        ;;
    *)
        echo "Usage: ./run.sh [command] [fresh]"
        echo ""
        echo "Commands:"
        echo "  bridge         Start bridge only (default)"
        echo "  server         Start server only"
        echo "  restart        Stop server, sync mod, start server + bridge"
        echo "  fresh          Full fresh restart (new world + surfaces)"
        echo "  stop           Stop server"
        echo "  sync           Sync mod to Factorio mods dir"
        echo ""
        echo "Environment:"
        echo "  SCALE=1            Number of agents by planet order (default: 1 = nauvis only)"
        echo "                     1=nauvis  2=+vulcanus  3=+fulgora  4=+gleba  5=+aquilo"
        echo "  GROUP=doug-squad   Agent group (default: doug-squad)"
        echo "  MODEL=sonnet       Claude model override"
        echo "  AUTONOMY_REQUIRES_PLAYER=true  Wait for a connected player before autonomy"
        exit 1
        ;;
esac
