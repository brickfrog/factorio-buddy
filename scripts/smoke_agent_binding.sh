#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="${FACTORIOCTL_BIN:-$ROOT/target/debug/factorioctl}"
HOST="${FACTORIO_RCON_HOST:-localhost}"
PORT="${FACTORIO_RCON_PORT:-27015}"
PASSWORD="${FACTORIO_RCON_PASSWORD:-}"

SERVER_PID=""
cleanup() {
    if [[ -n "$SERVER_PID" ]]; then
        kill "$SERVER_PID" 2>/dev/null || true
        wait "$SERVER_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

if [[ -n "${FACTORIO_SERVER_START:-}" ]]; then
    bash -lc "$FACTORIO_SERVER_START" &
    SERVER_PID="$!"
    sleep "${FACTORIO_SERVER_STARTUP_SLEEP:-8}"
fi

if [[ ! -x "$BIN" ]]; then
    (cd "$ROOT" && cargo build)
fi

ctl() {
    "$BIN" \
        --host "$HOST" \
        --port "$PORT" \
        --password "$PASSWORD" \
        --output json \
        "$@"
}

json_field() {
    python3 - "$1" "$2" <<'PY'
import json
import sys

data = json.loads(sys.argv[1])
value = data
for part in sys.argv[2].split("."):
    value = value[part]
print(value)
PY
}

position_json() {
    ctl --agent-id "$1" character status
}

if ! ctl get tick >/dev/null 2>&1; then
    echo "SKIP: no reachable Factorio RCON at ${HOST}:${PORT}" >&2
    exit 2
fi

a_init="$(ctl --agent-id a character init --x 5 --y 0)"
b_init="$(ctl --agent-id b character init --x 30 --y 0)"
a_unit="$(json_field "$a_init" unit_number)"
b_unit="$(json_field "$b_init" unit_number)"

if [[ "$a_unit" == "$b_unit" ]]; then
    echo "FAIL: agents a and b resolved to the same unit_number $a_unit" >&2
    exit 1
fi

ctl --agent-id a walk-to 8,0 >/dev/null
ctl --agent-id b walk-to 33,0 >/dev/null
ctl tick wait 120 >/dev/null 2>&1 || sleep 3

a_status="$(position_json a)"
b_status="$(position_json b)"
a_after_unit="$(json_field "$a_status" unit_number)"
b_after_unit="$(json_field "$b_status" unit_number)"

if [[ "$a_after_unit" != "$a_unit" || "$b_after_unit" != "$b_unit" ]]; then
    echo "FAIL: agent unit_number changed after walking" >&2
    exit 1
fi

a_x="$(json_field "$a_status" position.x)"
b_x="$(json_field "$b_status" position.x)"
python3 - "$a_x" "$b_x" <<'PY'
import sys

a_x = float(sys.argv[1])
b_x = float(sys.argv[2])
if not a_x > 5.0:
    raise SystemExit("FAIL: agent a did not move east from x=5")
if not b_x > 30.0:
    raise SystemExit("FAIL: agent b did not move east from x=30")
PY

human_before="$(FACTORIO_AGENT_ID=__player__ ctl character status || true)"
human_valid="$(json_field "$human_before" valid 2>/dev/null || echo false)"
if [[ "$human_valid" == "false" || "$human_valid" == "False" ]]; then
    echo "SKIP: no connected or initialized __player__ character; player isolation sub-check not applicable" >&2
else
    human_unit="$(json_field "$human_before" unit_number)"
    human_x="$(json_field "$human_before" position.x)"
fi
ctl --agent-id a walk-to 10,0 >/dev/null
ctl tick wait 60 >/dev/null 2>&1 || sleep 2
if [[ "$human_valid" != "false" && "$human_valid" != "False" ]]; then
    human_after="$(FACTORIO_AGENT_ID=__player__ ctl character status)"
    human_after_unit="$(json_field "$human_after" unit_number)"
    human_after_x="$(json_field "$human_after" position.x)"
    if [[ "$human_after_unit" != "$human_unit" || "$human_after_x" != "$human_x" ]]; then
        echo "FAIL: legacy/player character changed while named agent moved" >&2
        exit 1
    fi
fi

reconnected_a="$(position_json a)"
reconnected_unit="$(json_field "$reconnected_a" unit_number)"
if [[ "$reconnected_unit" != "$a_unit" ]]; then
    echo "FAIL: storage did not persist agent a across reconnect" >&2
    exit 1
fi

echo "PASS: agent-scoped character binding smoke scenario completed"
