#!/usr/bin/env bash
# Exercise Buddy's owned-server lifecycle against a real, disposable Factorio.
#
# Every writable path (including HOME) lives below one temporary directory. The
# normal ~/.factorio installation and the repository's .factorio-buddy state are
# never used.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(dirname "$SCRIPT_DIR")"
cd "$ROOT"

BUDDY_BIN="${BUDDY_BIN:-$ROOT/target/release/buddy}"
MCP_BIN="${FACTORIO_MCP_BIN:-$ROOT/target/release/mcp}"
RCON_PORT="${BUDDY_TEST_RCON_PORT:-27217}"
GAME_PORT="${BUDDY_TEST_GAME_PORT:-34399}"
TEST_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/factorio-buddy-runtime.XXXXXX")"
CURRENT_BUDDY_PID=""
CURRENT_SERVER_PID=""

find_factorio_bin() {
    if [[ -n "${FACTORIO_BIN:-}" ]]; then
        printf '%s\n' "$FACTORIO_BIN"
    elif command -v factorio >/dev/null 2>&1; then
        command -v factorio
    elif [[ -x "/mnt/games/SteamLibrary/steamapps/common/Factorio/bin/x64/factorio" ]]; then
        printf '%s\n' "/mnt/games/SteamLibrary/steamapps/common/Factorio/bin/x64/factorio"
    elif [[ -x "$HOME/.local/share/Steam/steamapps/common/Factorio/bin/x64/factorio" ]]; then
        printf '%s\n' "$HOME/.local/share/Steam/steamapps/common/Factorio/bin/x64/factorio"
    elif [[ -x "/opt/factorio/bin/x64/factorio" ]]; then
        printf '%s\n' "/opt/factorio/bin/x64/factorio"
    else
        return 1
    fi
}

FACTORIO_BIN="$(find_factorio_bin)" || {
    printf 'ERROR: Factorio binary not found; set FACTORIO_BIN=/path/to/factorio\n' >&2
    exit 1
}

fail() {
    printf 'FAIL: %s\n' "$*" >&2
    return 1
}

pass() {
    printf 'PASS: %s\n' "$*"
}

process_active() {
    local pid="$1"
    [[ -r "/proc/$pid/stat" ]] || return 1
    [[ "$(awk '{ print $3 }' "/proc/$pid/stat")" != "Z" ]]
}

wait_for_process_stop() {
    local pid="$1"
    local timeout_seconds="$2"
    local deadline=$((SECONDS + timeout_seconds))
    while process_active "$pid"; do
        (( SECONDS < deadline )) || return 1
        sleep 0.25
    done
}

wait_for_log() {
    local pid="$1"
    local log="$2"
    local pattern="$3"
    local timeout_seconds="$4"
    local deadline=$((SECONDS + timeout_seconds))
    while (( SECONDS < deadline )); do
        if grep -Fq -- "$pattern" "$log" 2>/dev/null; then
            return 0
        fi
        process_active "$pid" || return 1
        sleep 0.25
    done
    return 1
}

find_owned_server_pid() {
    local scenario_root="$1"
    local port="$2"
    local proc
    local args
    for proc in /proc/[0-9]*; do
        [[ -r "$proc/cmdline" ]] || continue
        args="$(tr '\0' '\n' < "$proc/cmdline")"
        if grep -Fxq -- "--start-server" <<< "$args" \
            && grep -Fxq -- "--rcon-bind" <<< "$args" \
            && grep -Fxq -- "127.0.0.1:$port" <<< "$args" \
            && grep -Fq -- "$scenario_root" <<< "$args"; then
            printf '%s\n' "${proc##*/}"
            return 0
        fi
    done
    return 1
}

wait_for_server_pid() {
    local scenario_root="$1"
    local port="$2"
    local deadline=$((SECONDS + 10))
    local pid
    while (( SECONDS < deadline )); do
        if pid="$(find_owned_server_pid "$scenario_root" "$port")"; then
            printf '%s\n' "$pid"
            return 0
        fi
        sleep 0.1
    done
    return 1
}

rcon_listener() {
    ss -H -ltn "sport = :$RCON_PORT" 2>/dev/null || true
}

game_listener() {
    ss -H -lun "sport = :$GAME_PORT" 2>/dev/null || true
}

assert_ports_unused() {
    [[ -z "$(rcon_listener)" ]] || fail "RCON test port $RCON_PORT is already in use"
    [[ -z "$(game_listener)" ]] || fail "game test port $GAME_PORT is already in use"
}

assert_no_owned_server() {
    local scenario_root="$1"
    local deadline=$((SECONDS + 10))
    while (( SECONDS < deadline )); do
        if ! find_owned_server_pid "$scenario_root" "$RCON_PORT" >/dev/null \
            && [[ -z "$(rcon_listener)" ]] \
            && [[ -z "$(game_listener)" ]]; then
            return 0
        fi
        sleep 0.25
    done
    return 1
}

dump_logs() {
    local log
    for log in "$TEST_ROOT"/*/*.log; do
        [[ -f "$log" ]] || continue
        printf '\n--- %s ---\n' "$log" >&2
        tail -n 160 "$log" >&2 || true
    done
}

cleanup() {
    local status=$?
    trap - EXIT INT TERM

    if [[ -n "$CURRENT_BUDDY_PID" ]] && process_active "$CURRENT_BUDDY_PID"; then
        kill -TERM "$CURRENT_BUDDY_PID" 2>/dev/null || true
        wait_for_process_stop "$CURRENT_BUDDY_PID" 5 || true
    fi
    if [[ -n "$CURRENT_BUDDY_PID" ]] && process_active "$CURRENT_BUDDY_PID"; then
        kill -KILL "$CURRENT_BUDDY_PID" 2>/dev/null || true
    fi
    if [[ -n "$CURRENT_BUDDY_PID" ]]; then
        wait "$CURRENT_BUDDY_PID" 2>/dev/null || true
    fi

    local scenario
    local pid
    for scenario in "$TEST_ROOT"/clean "$TEST_ROOT"/no-player-autonomy "$TEST_ROOT"/server-death; do
        if pid="$(find_owned_server_pid "$scenario" "$RCON_PORT" 2>/dev/null)"; then
            kill -KILL "$pid" 2>/dev/null || true
        fi
    done

    if (( status != 0 )); then
        dump_logs
    fi
    if [[ "${KEEP_LIVE_TEST_ARTIFACTS:-0}" == "1" ]]; then
        printf 'Buddy runtime artifacts: %s\n' "$TEST_ROOT" >&2
    else
        rm -rf "$TEST_ROOT"
    fi
    exit "$status"
}

trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

for command in awk chmod cmp cp find grep jq seq sleep ss stat timeout touch tr; do
    command -v "$command" >/dev/null 2>&1 || fail "required command is missing: $command"
done
[[ -x "$FACTORIO_BIN" ]] || fail "Factorio binary is not executable: $FACTORIO_BIN"
[[ -x "$BUDDY_BIN" ]] || fail "Buddy binary is not executable: $BUDDY_BIN"
[[ -x "$MCP_BIN" ]] || fail "MCP binary is not executable: $MCP_BIN"
[[ "$RCON_PORT" =~ ^[0-9]+$ ]] && (( RCON_PORT > 1024 && RCON_PORT <= 65535 )) \
    || fail "invalid BUDDY_TEST_RCON_PORT: $RCON_PORT"
[[ "$GAME_PORT" =~ ^[0-9]+$ ]] && (( GAME_PORT > 1024 && GAME_PORT <= 65535 )) \
    || fail "invalid BUDDY_TEST_GAME_PORT: $GAME_PORT"
(( RCON_PORT != GAME_PORT )) || fail "RCON and game test ports must differ"
assert_ports_unused

start_buddy() {
    local scenario_name="$1"
    local mode="${2:-fresh}"
    local heartbeat_seconds="${3:-0}"
    local scenario="$TEST_ROOT/$scenario_name"
    local log="$scenario/buddy-$mode.log"
    local fresh_args=()
    if [[ "$mode" == "fresh" ]]; then
        fresh_args=(--fresh)
    elif [[ "$mode" != "resume" ]]; then
        fail "unknown Buddy start mode: $mode"
    fi
    mkdir -p "$scenario/bin" "$scenario/home/.factorio/mods" "$scenario/write-data"
    printf 'isolated-home\n' > "$scenario/home/.factorio/mods/runtime-test-sentinel"
    cp -a "$ROOT/mod/claude-interface" "$scenario/home/.factorio/mods/"
    printf '%s\n' \
        '#!/usr/bin/env bash' \
        'printf '\''%s\n'\'' '\''{"type":"result","subtype":"success","is_error":false,"result":"runtime fake reply","session_id":"runtime-fake-session"}'\''' \
        > "$scenario/bin/claude"
    chmod +x "$scenario/bin/claude"

    env \
        -u FACTORIO_RCON_PASSWORD \
        -u FACTORIO_RCON_HOST \
        -u FACTORIO_RCON_PORT \
        -u FACTORIO_GAME_PORT \
        -u FACTORIO_WRITE_DATA \
        -u FACTORIO_SCRIPT_OUTPUT \
        HOME="$scenario/home" \
        PATH="$scenario/bin:$PATH" \
        BUDDY_HEARTBEAT_SECONDS="$heartbeat_seconds" \
        RUST_LOG=info \
        "$BUDDY_BIN" \
            --start-server \
            "${fresh_args[@]}" \
            --heartbeat-seconds "$heartbeat_seconds" \
            --agent runtime-live \
            --rcon-host localhost \
            --rcon-port "$RCON_PORT" \
            --game-port "$GAME_PORT" \
            --factorio-bin "$FACTORIO_BIN" \
            --write-data "$scenario/write-data" \
            --save "$scenario/save.zip" \
            --mcp-bin "$MCP_BIN" \
            > "$log" 2>&1 &
    CURRENT_BUDDY_PID=$!

    wait_for_log "$CURRENT_BUDDY_PID" "$log" "Factorio buddy online" 60 \
        || fail "$scenario_name Buddy did not reach the online state"
    CURRENT_SERVER_PID="$(wait_for_server_pid "$scenario" "$RCON_PORT")" \
        || fail "$scenario_name Factorio child could not be identified"
    process_active "$CURRENT_SERVER_PID" \
        || fail "$scenario_name Factorio child exited before verification"
}

printf '=== Buddy managed-runtime live regression ===\n'
printf 'Factorio: %s\n' "$FACTORIO_BIN"
printf 'RCON: 127.0.0.1:%s\n' "$RCON_PORT"
printf 'Game: 127.0.0.1:%s\n' "$GAME_PORT"

# Clean lifecycle: security boundary, lease exclusivity, and owned cleanup.
start_buddy clean
CLEAN_ROOT="$TEST_ROOT/clean"
CLEAN_LOG="$CLEAN_ROOT/buddy-fresh.log"
PASSWORD_FILE="$CLEAN_ROOT/write-data/rcon-password"
MCP_CONFIG="$CLEAN_ROOT/write-data/mcp-runtime-live.json"
PASSWORD="$(tr -d '\r\n' < "$PASSWORD_FILE")"

[[ "$PASSWORD" =~ ^[0-9a-f]{64}$ ]] \
    || fail "managed RCON password is not a generated 256-bit hex value"
[[ "$(stat -c '%a' "$PASSWORD_FILE")" == "600" ]] \
    || fail "managed RCON password is not mode 0600"
[[ -f "$MCP_CONFIG" && "$(stat -c '%a' "$MCP_CONFIG")" == "600" ]] \
    || fail "password-bearing MCP configuration is not mode 0600"
while IFS= read -r config; do
    [[ "$(stat -c '%a' "$config")" == "600" ]] \
        || fail "managed Factorio config is not mode 0600: $config"
    grep -Fxq 'drop-detection-threshold-time=86400' "$config" \
        || fail "managed Factorio config does not tolerate background-client stalls: $config"
done < <(find "$CLEAN_ROOT/write-data/managed-runs" -name config.ini -type f)
if tr '\0' '\n' < "/proc/$CURRENT_BUDDY_PID/cmdline" | grep -Fq -- "$PASSWORD"; then
    fail "managed RCON password leaked into the Buddy process arguments"
fi

LISTENERS="$(rcon_listener)"
[[ -n "$LISTENERS" ]] || fail "managed RCON listener is missing"
if awk -v expected="127.0.0.1:$RCON_PORT" '$4 != expected { exit 1 }' <<< "$LISTENERS"; then
    pass "managed RCON listens only on 127.0.0.1"
else
    fail "managed RCON is not loopback-only: $LISTENERS"
fi
mapfile -d '' -t SERVER_ARGS < "/proc/$CURRENT_SERVER_PID/cmdline"
BIND_VALUE=""
for ((index = 0; index + 1 < ${#SERVER_ARGS[@]}; index++)); do
    if [[ "${SERVER_ARGS[index]}" == "--rcon-bind" ]]; then
        BIND_VALUE="${SERVER_ARGS[index + 1]}"
        break
    fi
done
[[ "$BIND_VALUE" == "127.0.0.1:$RCON_PORT" ]] \
    || fail "Factorio child was not launched with an explicit loopback RCON bind"
[[ -f "$CLEAN_ROOT/home/.factorio/mods/runtime-test-sentinel" ]] \
    || fail "isolated HOME sentinel was disturbed"
pass "managed credentials stay private and background-client stalls are tolerated"

FACTORIO_LOG="$(find "$CLEAN_ROOT/write-data/managed-runs" -name factorio-current.log -type f -print -quit)"
[[ -f "$FACTORIO_LOG" ]] || fail "managed Factorio runtime log is missing"
LIFECYCLE_CONNECTIONS="$(grep -c 'New RCON connection from' "$FACTORIO_LOG" 2>/dev/null || true)"
(( LIFECYCLE_CONNECTIONS == 2 )) \
    || fail "startup opened $LIFECYCLE_CONNECTIONS RCON connections; expected one readiness connection and one reused lifecycle connection"
pass "startup lifecycle calls reuse one RCON connection"

SECOND_LOG="$CLEAN_ROOT/second-controller.log"
set +e
timeout --signal=KILL 5s env \
    -u FACTORIO_RCON_PASSWORD \
    HOME="$CLEAN_ROOT/home" \
    BUDDY_HEARTBEAT_SECONDS=0 \
    "$BUDDY_BIN" \
        --start-server \
        --fresh \
        --heartbeat-seconds 0 \
        --agent runtime-live \
        --rcon-host localhost \
        --rcon-port "$RCON_PORT" \
        --game-port "$GAME_PORT" \
        --factorio-bin "$FACTORIO_BIN" \
        --write-data "$CLEAN_ROOT/write-data" \
        --save "$CLEAN_ROOT/save.zip" \
        --mcp-bin "$MCP_BIN" \
        > "$SECOND_LOG" 2>&1
SECOND_STATUS=$?
set -e
(( SECOND_STATUS != 0 && SECOND_STATUS != 124 && SECOND_STATUS != 137 )) \
    || fail "second same-agent controller did not fail promptly"
grep -Fq "another Buddy controller already owns agent lease" "$SECOND_LOG" \
    || fail "second controller failed without the agent-lease diagnostic"
process_active "$CURRENT_BUDDY_PID" \
    || fail "lease probe disturbed the owning Buddy controller"
pass "a second controller cannot acquire the same agent lease"

kill -TERM "$CURRENT_BUDDY_PID"
wait_for_process_stop "$CURRENT_BUDDY_PID" 75 \
    || fail "Buddy did not complete a clean managed shutdown within 75 seconds"
set +e
wait "$CURRENT_BUDDY_PID"
CLEAN_STATUS=$?
set -e
CURRENT_BUDDY_PID=""
CURRENT_SERVER_PID=""
(( CLEAN_STATUS == 0 )) || fail "clean Buddy shutdown exited with status $CLEAN_STATUS"
assert_no_owned_server "$CLEAN_ROOT" \
    || fail "clean Buddy shutdown left an owned Factorio process or listener"
jq -e '.version == 2 and .clean_shutdown == true' \
    "$CLEAN_ROOT/save.zip.buddy-owner.json" >/dev/null \
    || fail "clean shutdown was not recorded in the owned-save manifest"
grep -Fq "Factorio server stopped after final save" "$CLEAN_LOG" \
    || fail "clean shutdown did not complete Factorio's final-save path"
pass "clean shutdown saves and reaps the owned Factorio server"

# Autonomy belongs to the NPC runtime, not to the graphical client's
# connection state. With no multiplayer peer ever joining, a due heartbeat
# must still run a complete model turn.
assert_ports_unused
start_buddy no-player-autonomy fresh 1
NO_PLAYER_ROOT="$TEST_ROOT/no-player-autonomy"
NO_PLAYER_LOG="$NO_PLAYER_ROOT/buddy-fresh.log"
wait_for_log "$CURRENT_BUDDY_PID" "$NO_PLAYER_LOG" \
    "Claude turn finished kind=Autonomy succeeded=true" 20 \
    || fail "Buddy did not complete autonomy with zero connected players"
NO_PLAYER_FACTORIO_LOG="$(find "$NO_PLAYER_ROOT/write-data/managed-runs" -name factorio-current.log -type f -print -quit)"
[[ -f "$NO_PLAYER_FACTORIO_LOG" ]] \
    || fail "no-player autonomy Factorio log is missing"
if grep -Fq "processed PlayerJoinGame" "$NO_PLAYER_FACTORIO_LOG"; then
    fail "no-player autonomy fixture unexpectedly had a multiplayer client"
fi
kill -TERM "$CURRENT_BUDDY_PID"
wait_for_process_stop "$CURRENT_BUDDY_PID" 75 \
    || fail "no-player autonomy Buddy did not shut down cleanly"
set +e
wait "$CURRENT_BUDDY_PID"
NO_PLAYER_STATUS=$?
set -e
CURRENT_BUDDY_PID=""
CURRENT_SERVER_PID=""
(( NO_PLAYER_STATUS == 0 )) \
    || fail "no-player autonomy Buddy exited with status $NO_PLAYER_STATUS"
assert_no_owned_server "$NO_PLAYER_ROOT" \
    || fail "no-player autonomy left an owned Factorio process or listener"
pass "autonomy continues with zero connected players"

# Failure lifecycle: killing the owned child must terminate Buddy promptly and
# non-zero instead of leaving a useless controller alive.
assert_ports_unused
start_buddy server-death
DEATH_ROOT="$TEST_ROOT/server-death"
DEATH_LOG="$DEATH_ROOT/buddy-fresh.log"
kill -KILL "$CURRENT_SERVER_PID"
wait_for_process_stop "$CURRENT_BUDDY_PID" 10 \
    || fail "Buddy stayed alive after its owned Factorio server died"
set +e
wait "$CURRENT_BUDDY_PID"
DEATH_STATUS=$?
set -e
CURRENT_BUDDY_PID=""
CURRENT_SERVER_PID=""
(( DEATH_STATUS != 0 )) || fail "Buddy exited successfully after unexpected server death"
grep -Fq "owned Factorio server exited unexpectedly" "$DEATH_LOG" \
    || fail "Buddy did not report its owned Factorio server death"
assert_no_owned_server "$DEATH_ROOT" \
    || fail "server-death path left an owned Factorio process or listener"
jq -e '.version == 2 and .clean_shutdown == false' \
    "$DEATH_ROOT/save.zip.buddy-owner.json" >/dev/null \
    || fail "unexpected server death was incorrectly recorded as clean"
pass "owned server death makes Buddy exit promptly and non-zero"
pass "Buddy runtime leaves no orphaned Factorio process"

# Exercise recovery from the unclean manifest above. A newer autosave beside
# the primary belongs to no managed run and must not contaminate resume.
cp "$DEATH_ROOT/save.zip" "$DEATH_ROOT/primary-before-resume.zip"
cp "$DEATH_ROOT/save.zip" "$DEATH_ROOT/_autosave-foreign.zip"
touch -d '+2 minutes' "$DEATH_ROOT/_autosave-foreign.zip"
start_buddy server-death resume
cmp -s "$DEATH_ROOT/save.zip" "$DEATH_ROOT/primary-before-resume.zip" \
    || fail "resume replaced the primary save with an unrelated adjacent autosave"
[[ ! -e "$DEATH_ROOT/save.previous.zip" ]] \
    || fail "resume attempted to promote an autosave outside the owned run"
kill -TERM "$CURRENT_BUDDY_PID"
wait_for_process_stop "$CURRENT_BUDDY_PID" 75 \
    || fail "resumed Buddy did not complete a clean shutdown within 75 seconds"
set +e
wait "$CURRENT_BUDDY_PID"
RESUME_STATUS=$?
set -e
CURRENT_BUDDY_PID=""
CURRENT_SERVER_PID=""
(( RESUME_STATUS == 0 )) || fail "resumed Buddy shutdown exited with status $RESUME_STATUS"
assert_no_owned_server "$DEATH_ROOT" \
    || fail "resumed Buddy left an owned Factorio process or listener"
jq -e '.version == 2 and .clean_shutdown == true' \
    "$DEATH_ROOT/save.zip.buddy-owner.json" >/dev/null \
    || fail "resumed clean shutdown was not recorded"
pass "unclean resume ignores autosaves outside the primary save's owned run"

printf 'Buddy managed-runtime live regression passed.\n'
