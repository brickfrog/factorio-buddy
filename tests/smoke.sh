#!/bin/bash
# Run runtime smoke checks against an isolated disposable Factorio server.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
cd "$PROJECT_ROOT"

SAVE_NAME="${SAVE_NAME:-runtime_smoke}"
RCON_PORT="${RCON_PORT:-27016}"
RCON_PASSWORD="${RCON_PASSWORD:-test_password}"
GAME_PORT="${GAME_PORT:-34198}"
SYNCED_MOD_DIR="${FACTORIOCTL_SYNCED_MOD_DIR:-$PROJECT_ROOT/.factorio-test-data/mods/claude-interface}"
SAVE_PATH="${SAVE_PATH:-$PROJECT_ROOT/saves/${SAVE_NAME}.zip}"
PYTHON_BIN="${PYTHON_BIN:-$PROJECT_ROOT/companion/.venv/bin/python}"

if [ ! -x "$PYTHON_BIN" ]; then
    echo "ERROR: runtime smoke checks require the companion Python environment."
    echo "       Run: cd companion && just install"
    echo "       Or set PYTHON_BIN=/path/to/python-with-pydantic"
    exit 1
fi

cleanup() {
    "$PROJECT_ROOT/tests/cleanup.sh" >/dev/null 2>&1 || true
}

trap cleanup EXIT

rm -f "$SAVE_PATH"

SAVE_NAME="$SAVE_NAME" \
SAVE_PATH="$SAVE_PATH" \
RCON_PORT="$RCON_PORT" \
RCON_PASSWORD="$RCON_PASSWORD" \
GAME_PORT="$GAME_PORT" \
"$PROJECT_ROOT/tests/setup.sh"

FACTORIOCTL_SYNCED_MOD_DIR="$SYNCED_MOD_DIR" \
"$PYTHON_BIN" "$PROJECT_ROOT/tests/runtime_smoke.py" \
    --port "$RCON_PORT" \
    --password "$RCON_PASSWORD" \
    --synced-mod-dir "$SYNCED_MOD_DIR"
