#!/bin/bash
# Cleanup the isolated Factorio test server.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
RCON_PORT="${RCON_PORT:-27016}"

PID_FILE="$PROJECT_ROOT/logs/test-server.pid"
STDIN_KEEPER_PID_FILE="$PROJECT_ROOT/logs/test-server.stdin.pid"
STDIN_FIFO="$PROJECT_ROOT/logs/test-server.stdin"

if [ -f "$PID_FILE" ]; then
    PID="$(cat "$PID_FILE")"
    if kill -0 "$PID" 2>/dev/null; then
        echo "Stopping isolated server (PID: $PID)..."
        kill "$PID" 2>/dev/null || true
        sleep 2
        if kill -0 "$PID" 2>/dev/null; then
            kill -9 "$PID" 2>/dev/null || true
        fi
    else
        echo "Isolated server not running (stale PID file)."
    fi
    rm -f "$PID_FILE"
else
    PIDS="$(pgrep -f "factorio.*--rcon-port $RCON_PORT" || true)"
    if [ -n "$PIDS" ]; then
        echo "Found isolated test server(s): $PIDS"
        kill $PIDS 2>/dev/null || true
    else
        echo "No isolated test server running."
    fi
fi

if [ -f "$STDIN_KEEPER_PID_FILE" ]; then
    KEEPER_PID="$(cat "$STDIN_KEEPER_PID_FILE")"
    kill "$KEEPER_PID" 2>/dev/null || true
    rm -f "$STDIN_KEEPER_PID_FILE"
fi
rm -f "$STDIN_FIFO"
