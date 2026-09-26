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
    for scenario in "$TEST_ROOT"/*/; do
        scenario="${scenario%/}"
        if pid="$(find_owned_server_pid "$scenario" "$RCON_PORT" 2>/dev/null)"; then
            kill -KILL "$pid" 2>/dev/null || true
        fi
        # Fake-Claude grandchildren are only ours; never touch other PIDs.
        if [[ -f "$scenario/claude-invocations.grandchild" ]]; then
            while IFS= read -r pid; do
                [[ "$pid" =~ ^[0-9]+$ ]] && kill -KILL "$pid" 2>/dev/null || true
            done < "$scenario/claude-invocations.grandchild"
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

BUDDY_EXTRA_ARGS=()
BUDDY_EXTRA_ENV=()

# Optional comma-separated filter for the newer scenarios, e.g.
# BUDDY_RUNTIME_SCENARIOS=human-fifo,stale-session. Empty runs everything.
want() {
    [[ -z "${BUDDY_RUNTIME_SCENARIOS:-}" ]] && return 0
    [[ ",$BUDDY_RUNTIME_SCENARIOS," == *",$1,"* ]]
}

# Every fake records its observed argv (model, --resume) and prompt, so the
# assertions check what Buddy actually launched rather than source text.
write_fake_claude() {
    local fixture="$1"
    cat <<'PRELUDE'
#!/usr/bin/env bash
state="$FAKE_CLAUDE_STATE"
model=""
resume=no
previous=""
for argument in "$@"; do
    [[ "$previous" == "--model" ]] && model="$argument"
    [[ "$argument" == "--resume" ]] && resume=yes
    previous="$argument"
done
prompt="${!#}"
printf 'invoke\n' >> "$state"
printf 'argv model=%s resume=%s\n' "$model" "$resume" >> "$state.args"
printf '%s\n----\n' "$prompt" >> "$state.prompts"
result() {
    printf '{"type":"result","subtype":"success","is_error":false,"result":"%s","session_id":"%s"}\n' "$1" "${2:-runtime-fake-session}"
}
PRELUDE
    case "$fixture" in
        success)
            printf '%s\n' 'result "runtime fake reply"'
            ;;
        provider-limit)
            cat <<'BODY'
if [[ "$resume" == yes ]]; then
    printf '%s\n' '{"type":"system","subtype":"init","session_id":"wedged-provider-session"}'
    printf '%s\n' '{"type":"system","subtype":"status","status":"compacting","session_id":"wedged-provider-session"}'
    sleep 120
    exit 0
fi
printf '%s\n' '{"type":"result","subtype":"error","is_error":true,"result":"API Error: Request rejected (429) - Weekly/Monthly Limit Exhausted. Your limit will reset at 2026-07-20 01:09:22","session_id":"wedged-provider-session"}'
BODY
            ;;
        human-fifo)
            cat <<'BODY'
case "$prompt" in
    *request-A*) sleep 6; printf 'finished request-A\n' >> "$state.done"; result "reply-A" ;;
    *request-B*) printf 'finished request-B\n' >> "$state.done"; result "reply-B" ;;
    *) result "autonomy reply" ;;
esac
BODY
            ;;
        limit-queue)
            cat <<'BODY'
case "$prompt" in
    *request-A*)
        sleep 5
        # Exact result shape Claude Code 2.1.281 emitted when the subscription
        # session window ran out during the model trials.
        printf '%s\n' '{"type":"result","subtype":"success","is_error":true,"api_error_status":429,"result":"You'"'"'ve hit your session limit · resets 4am (America/New_York)","session_id":"limit-session"}'
        ;;
    *) result "unexpected turn" ;;
esac
BODY
            ;;
        stale-session)
            cat <<'BODY'
if [[ "$resume" == yes ]]; then
    printf 'No conversation found with session ID: runtime-fake-session\n' >&2
    exit 1
fi
result "fresh reply"
BODY
            ;;
        stale-after-tool)
            cat <<'BODY'
if [[ "$resume" == yes ]]; then
    # The resumed attempt runs a tool, then reports the session missing.
    printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_stale_1","name":"mcp__factorio__place_entity","input":{"name":"stone-furnace","x":5,"y":6}}]}}'
    printf 'No conversation found with session ID: runtime-fake-session\n' >&2
    exit 1
fi
note=no
[[ "$prompt" == *"OUTCOME UNKNOWN"* && "$prompt" == *"place_entity"* ]] && note=yes
printf 'fresh note=%s\n' "$note" >> "$state.args"
result "fresh reply"
BODY
            ;;
        interrupt)
            cat <<'BODY'
if [[ "$prompt" == *"Autonomy tick"* ]]; then
    # Stand-in for Claude's MCP server: a grandchild in the same group.
    sleep 300 &
    printf '%s\n' "$!" >> "$state.grandchild"
    printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"toolu_fake_1","name":"mcp__factorio__place_entity","input":{"name":"stone-furnace","x":3,"y":4}}]}}'
    sleep 300
    exit 0
fi
alive=no
while IFS= read -r pid; do
    if [[ -r "/proc/$pid/stat" ]] && [[ "$(awk '{ print $3 }' "/proc/$pid/stat")" != "Z" ]]; then
        alive=yes
    fi
done < <(cat "$state.grandchild" 2>/dev/null)
note=no
[[ "$prompt" == *"OUTCOME UNKNOWN"* && "$prompt" == *"place_entity"* ]] && note=yes
printf 'human grandchild_alive=%s note=%s\n' "$alive" "$note" >> "$state.args"
result "human reply"
BODY
            ;;
        *)
            fail "unknown Claude fixture: $fixture"
            ;;
    esac
}

inbox_for() {
    local run_directory
    run_directory="$(find "$1/write-data/managed-runs" -mindepth 1 -maxdepth 1 -type d -print -quit)"
    [[ -n "$run_directory" ]] || fail "managed run directory is missing under $1"
    mkdir -p "$run_directory/script-output/claude-chat"
    printf '%s\n' "$run_directory/script-output/claude-chat/input.jsonl"
}

send_chat() {
    local inbox="$1"
    local id="$2"
    local message="$3"
    printf '{"id":%s,"message":"%s","player_index":0,"target_agent":"runtime-live"}\n' \
        "$id" "$message" >> "$inbox"
}

wait_for_file_line() {
    local pid="$1"
    local file="$2"
    local pattern="$3"
    local timeout_seconds="$4"
    wait_for_log "$pid" "$file" "$pattern" "$timeout_seconds"
}

stop_buddy() {
    local scenario_root="$1"
    local label="$2"
    kill -TERM "$CURRENT_BUDDY_PID"
    wait_for_process_stop "$CURRENT_BUDDY_PID" 75 \
        || fail "$label Buddy did not shut down cleanly"
    set +e
    wait "$CURRENT_BUDDY_PID"
    local status=$?
    set -e
    CURRENT_BUDDY_PID=""
    CURRENT_SERVER_PID=""
    (( status == 0 )) || fail "$label Buddy exited with status $status"
    assert_no_owned_server "$scenario_root" \
        || fail "$label left an owned Factorio process or listener"
    BUDDY_EXTRA_ARGS=()
    BUDDY_EXTRA_ENV=()
}

assert_no_player_joined() {
    local factorio_log
    factorio_log="$(find "$1/write-data/managed-runs" -name factorio-current.log -type f -print -quit)"
    [[ -f "$factorio_log" ]] || fail "Factorio log is missing under $1"
    if grep -Fq "processed PlayerJoinGame" "$factorio_log"; then
        fail "$1 unexpectedly had a connected player"
    fi
}

# MCP servers spawned by Buddy's maintenance client for this test's RCON port.
owned_mcp_pids() {
    local proc
    for proc in /proc/[0-9]*; do
        [[ -r "$proc/cmdline" && -r "$proc/environ" ]] || continue
        [[ "$(tr '\0' '\n' < "$proc/cmdline" | head -n 1)" == "$MCP_BIN" ]] || continue
        if tr '\0' '\n' < "$proc/environ" | grep -Fxq "FACTORIO_RCON_PORT=$RCON_PORT"; then
            printf '%s\n' "${proc##*/}"
        fi
    done
}

start_buddy() {
    local scenario_name="$1"
    local mode="${2:-fresh}"
    local heartbeat_seconds="${3:-0}"
    local claude_fixture="${4:-success}"
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
    write_fake_claude "$claude_fixture" > "$scenario/bin/claude"
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
        FAKE_CLAUDE_STATE="$scenario/claude-invocations" \
        BUDDY_HEARTBEAT_SECONDS="$heartbeat_seconds" \
        RUST_LOG=info \
        "${BUDDY_EXTRA_ENV[@]}" \
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
            --evidence-log "$scenario/evidence.jsonl" \
            "${BUDDY_EXTRA_ARGS[@]}" \
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
grep -Fxq "argv model=claude-opus-5-5 resume=no" "$NO_PLAYER_ROOT/claude-invocations.args" \
    || fail "Buddy did not launch Claude with the default claude-opus-5-5 model"
pass "Buddy launches Claude with --model claude-opus-5-5 by default"
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

# A terminal subscription limit must discard the resumed Claude session, keep
# the managed Factorio server alive, pause autonomy, and answer later player
# messages through the existing RCON response seam without spawning Claude.
assert_ports_unused
start_buddy provider-limit fresh 1 provider-limit
LIMIT_ROOT="$TEST_ROOT/provider-limit"
LIMIT_LOG="$LIMIT_ROOT/buddy-fresh.log"
wait_for_log "$CURRENT_BUDDY_PID" "$LIMIT_LOG" \
    "Claude provider usage limit active; pausing autonomous turns" 20 \
    || fail "Buddy did not classify and back off the terminal provider limit"
process_active "$CURRENT_BUDDY_PID" \
    || fail "provider limit terminated the Buddy controller"
process_active "$CURRENT_SERVER_PID" \
    || fail "provider limit terminated the managed Factorio server"
[[ "$(grep -c '^invoke$' "$LIMIT_ROOT/claude-invocations" 2>/dev/null || true)" == "1" ]] \
    || fail "provider-limit fixture did not execute exactly one initial Claude turn"

LIMIT_RUN_DIRECTORY="$(find "$LIMIT_ROOT/write-data/managed-runs" -mindepth 1 -maxdepth 1 -type d -print -quit)"
[[ -n "$LIMIT_RUN_DIRECTORY" ]] \
    || fail "provider-limit managed run directory is missing"
LIMIT_INBOX="$LIMIT_RUN_DIRECTORY/script-output/claude-chat/input.jsonl"
mkdir -p "$(dirname "$LIMIT_INBOX")"
printf '%s\n' \
    '{"id":1,"message":"hi","player_index":0,"target_agent":"runtime-live"}' \
    >> "$LIMIT_INBOX"
wait_for_log "$CURRENT_BUDDY_PID" "$LIMIT_LOG" \
    "responded to player without starting Claude while its usage limit is active" 5 \
    || fail "capped Buddy did not send the bounded provider-unavailable response"
sleep 1
[[ "$(grep -c '^invoke$' "$LIMIT_ROOT/claude-invocations" 2>/dev/null || true)" == "1" ]] \
    || fail "player message resumed the permanently wedged Claude session"
process_active "$CURRENT_BUDDY_PID" \
    || fail "bounded capped response terminated the Buddy controller"
process_active "$CURRENT_SERVER_PID" \
    || fail "bounded capped response disconnected the managed Factorio runtime"

kill -TERM "$CURRENT_BUDDY_PID"
wait_for_process_stop "$CURRENT_BUDDY_PID" 75 \
    || fail "provider-limit Buddy did not shut down cleanly"
set +e
wait "$CURRENT_BUDDY_PID"
LIMIT_STATUS=$?
set -e
CURRENT_BUDDY_PID=""
CURRENT_SERVER_PID=""
(( LIMIT_STATUS == 0 )) \
    || fail "provider-limit Buddy exited with status $LIMIT_STATUS"
assert_no_owned_server "$LIMIT_ROOT" \
    || fail "provider-limit path left an owned Factorio process or listener"
pass "provider limits back off without wedging chat or controlling Factorio connectivity"

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

# Human requests are FIFO: B arriving while A runs must neither cancel A nor
# be lost; each gets exactly one terminal response, A before B.
if want human-fifo; then
    assert_ports_unused
    start_buddy human-fifo fresh 0 human-fifo
    FIFO_ROOT="$TEST_ROOT/human-fifo"
    FIFO_LOG="$FIFO_ROOT/buddy-fresh.log"
    FIFO_INBOX="$(inbox_for "$FIFO_ROOT")"
    send_chat "$FIFO_INBOX" 1 "request-A"
    wait_for_file_line "$CURRENT_BUDDY_PID" "$FIFO_ROOT/claude-invocations.prompts" "request-A" 10 \
        || fail "request A never reached Claude"
    send_chat "$FIFO_INBOX" 2 "request-B"
    wait_for_log "$CURRENT_BUDDY_PID" "$FIFO_LOG" 'text="reply-B"' 30 \
        || fail "request B never received a terminal response"
    grep -Fxq "finished request-A" "$FIFO_ROOT/claude-invocations.done" \
        || fail "request A was cancelled by request B"
    (( "$(grep -c 'text="reply-A"' "$FIFO_LOG")" == 1 )) || fail "request A did not get exactly one response"
    (( "$(grep -c 'text="reply-B"' "$FIFO_LOG")" == 1 )) || fail "request B did not get exactly one response"
    A_LINE="$(grep -n 'text="reply-A"' "$FIFO_LOG" | cut -d: -f1)"
    B_LINE="$(grep -n 'text="reply-B"' "$FIFO_LOG" | cut -d: -f1)"
    (( A_LINE < B_LINE )) || fail "human responses were delivered out of order"
    if grep -Fq "cancelling active Claude turn" "$FIFO_LOG"; then
        fail "a human turn was cancelled by another human message"
    fi
    (( "$(grep -c '^invoke$' "$FIFO_ROOT/claude-invocations")" == 2 )) \
        || fail "human FIFO replayed or dropped a turn"
    stop_buddy "$FIFO_ROOT" "human-fifo"
    pass "human requests run FIFO without cancelling each other"
fi

# A provider limit hit while humans are queued answers every queued request.
if want limit-queue; then
    assert_ports_unused
    start_buddy limit-queue fresh 0 limit-queue
    QUEUE_ROOT="$TEST_ROOT/limit-queue"
    QUEUE_LOG="$QUEUE_ROOT/buddy-fresh.log"
    QUEUE_INBOX="$(inbox_for "$QUEUE_ROOT")"
    send_chat "$QUEUE_INBOX" 1 "request-A"
    wait_for_file_line "$CURRENT_BUDDY_PID" "$QUEUE_ROOT/claude-invocations.prompts" "request-A" 10 \
        || fail "limit request A never reached Claude"
    send_chat "$QUEUE_INBOX" 2 "request-B"
    send_chat "$QUEUE_INBOX" 3 "request-C"
    wait_for_log "$CURRENT_BUDDY_PID" "$QUEUE_LOG" \
        "Claude provider usage limit active; pausing autonomous turns" 30 \
        || fail "limit-queue fixture never hit the provider limit"
    sleep 1
    LIMIT_RESPONSES="$(grep -c 'response delivered to Factorio.*subscription usage limit' "$QUEUE_LOG" || true)"
    (( LIMIT_RESPONSES == 3 )) \
        || fail "expected 3 provider-limit responses (A plus queued B and C), saw $LIMIT_RESPONSES"
    (( "$(grep -c 'provider_limit_queued_response' "$QUEUE_LOG")" == 2 )) \
        || fail "queued requests were not individually answered"
    (( "$(grep -c '^invoke$' "$QUEUE_ROOT/claude-invocations")" == 1 )) \
        || fail "queued requests busy-looped into Claude during the limit"
    process_active "$CURRENT_SERVER_PID" || fail "provider limit stopped the server"
    stop_buddy "$QUEUE_ROOT" "limit-queue"
    pass "provider limit answers every queued human request once"
fi

# A resumed session that fails only on stderr is retried once, fresh.
if want stale-session; then
    assert_ports_unused
    start_buddy stale-session fresh 0 stale-session
    STALE_ROOT="$TEST_ROOT/stale-session"
    STALE_LOG="$STALE_ROOT/buddy-fresh.log"
    STALE_INBOX="$(inbox_for "$STALE_ROOT")"
    send_chat "$STALE_INBOX" 1 "first"
    wait_for_log "$CURRENT_BUDDY_PID" "$STALE_LOG" "Claude turn finished kind=Human succeeded=true" 20 \
        || fail "stale-session first turn failed"
    send_chat "$STALE_INBOX" 2 "second"
    wait_for_log "$CURRENT_BUDDY_PID" "$STALE_LOG" 'Claude turn finished kind=Human succeeded=true' 20 || true
    sleep 3
    mapfile -t STALE_ARGS < "$STALE_ROOT/claude-invocations.args"
    (( ${#STALE_ARGS[@]} == 3 )) \
        || fail "expected exactly 3 Claude launches (fresh, stale resume, one fresh retry); saw ${#STALE_ARGS[@]}"
    [[ "${STALE_ARGS[1]}" == *"resume=yes"* && "${STALE_ARGS[2]}" == *"resume=no"* ]] \
        || fail "stale session was not retried exactly once without --resume"
    (( "$(grep -c 'Claude turn finished kind=Human succeeded=true' "$STALE_LOG")" == 2 )) \
        || fail "second human turn did not succeed after the fresh-session retry"
    if grep -Fq 'text="Agent error' "$STALE_LOG"; then
        fail "stale session surfaced an Agent error instead of retrying"
    fi
    stop_buddy "$STALE_ROOT" "stale-session"
    pass "stderr-only invalid session triggers one fresh-session retry"
fi

# An invalid session reported after a tool already ran must not replay the
# prompt; the turn fails and the next turn carries the outcome-unknown note.
if want stale-after-tool; then
    assert_ports_unused
    start_buddy stale-after-tool fresh 0 stale-after-tool
    SAT_ROOT="$TEST_ROOT/stale-after-tool"
    SAT_LOG="$SAT_ROOT/buddy-fresh.log"
    SAT_INBOX="$(inbox_for "$SAT_ROOT")"
    send_chat "$SAT_INBOX" 1 "first"
    wait_for_log "$CURRENT_BUDDY_PID" "$SAT_LOG" "Claude turn finished kind=Human succeeded=true" 20 \
        || fail "stale-after-tool first turn failed"
    send_chat "$SAT_INBOX" 2 "second"
    wait_for_log "$CURRENT_BUDDY_PID" "$SAT_LOG" "Claude turn finished kind=Human succeeded=false" 20 \
        || fail "resumed turn with a tool call was not failed"
    grep -Fq "session_reset_without_retry" "$SAT_LOG" \
        || fail "session reset after a tool call was not reported"
    send_chat "$SAT_INBOX" 3 "third"
    wait_for_log "$CURRENT_BUDDY_PID" "$SAT_ROOT/claude-invocations.args" "fresh note=yes" 20 \
        || fail "turn after the unreplayed failure lacked the outcome-unknown note"
    mapfile -t SAT_ARGS < <(grep '^argv' "$SAT_ROOT/claude-invocations.args")
    (( ${#SAT_ARGS[@]} == 3 )) \
        || fail "expected 3 launches (fresh, failed resume, next fresh turn) with no replay; saw ${#SAT_ARGS[@]}"
    [[ "${SAT_ARGS[1]}" == *"resume=yes"* && "${SAT_ARGS[2]}" == *"resume=no"* ]] \
        || fail "failed resume was replayed or the next turn resumed the dead session"
    stop_buddy "$SAT_ROOT" "stale-after-tool"
    pass "invalid session after a tool call fails the turn without replaying it"
fi

# Chat preempting autonomy terminates the whole Claude process tree before
# the human turn starts, and the human prompt carries an outcome-unknown note.
if want interrupt; then
    assert_ports_unused
    start_buddy interrupt fresh 1 interrupt
    INT_ROOT="$TEST_ROOT/interrupt"
    INT_LOG="$INT_ROOT/buddy-fresh.log"
    INT_INBOX="$(inbox_for "$INT_ROOT")"
    wait_for_file_line "$CURRENT_BUDDY_PID" "$INT_ROOT/claude-invocations.grandchild" "" 20 || true
    wait_for_log "$CURRENT_BUDDY_PID" "$INT_LOG" "mcp__factorio__place_entity" 20 \
        || fail "interrupt fixture never started its tool call"
    send_chat "$INT_INBOX" 1 "hello"
    wait_for_log "$CURRENT_BUDDY_PID" "$INT_ROOT/claude-invocations.args" "human grandchild_alive=" 20 \
        || fail "human turn after interruption never started"
    grep -Fq "human grandchild_alive=no" "$INT_ROOT/claude-invocations.args" \
        || fail "cancelled autonomy left its grandchild running when the next turn started"
    grep -Fq "note=yes" "$INT_ROOT/claude-invocations.args" \
        || fail "next prompt lacked the outcome-unknown note for the interrupted tool"
    jq -e 'select(.event == "interrupted_tool" and .tool == "place_entity")' \
        "$INT_ROOT/evidence.jsonl" >/dev/null || fail "interrupted_tool evidence missing"
    jq -e 'select(.event == "turn_finished" and .kind == "autonomy" and .cancelled == true)' \
        "$INT_ROOT/evidence.jsonl" >/dev/null || fail "cancelled autonomy turn_finished evidence missing"
    stop_buddy "$INT_ROOT" "interrupt"
    while IFS= read -r pid; do
        if [[ -r "/proc/$pid/stat" ]] && [[ "$(awk '{ print $3 }' "/proc/$pid/stat")" != "Z" ]]; then
            fail "fake MCP grandchild $pid survived Buddy shutdown"
        fi
    done < "$INT_ROOT/claude-invocations.grandchild"
    pass "interruption reaps the Claude process tree and reports outcome-unknown tools"
fi

# Turn budget: after N autonomous turns Buddy idles but keeps the server up.
if want budget; then
    assert_ports_unused
    BUDDY_EXTRA_ARGS=(--max-autonomous-turns 2)
    start_buddy budget fresh 1 success
    BUDGET_ROOT="$TEST_ROOT/budget"
    BUDGET_LOG="$BUDGET_ROOT/buddy-fresh.log"
    wait_for_log "$CURRENT_BUDDY_PID" "$BUDGET_LOG" "autonomy_budget_exhausted" 30 \
        || fail "autonomy turn budget was never reported exhausted"
    sleep 4
    (( "$(grep -c '^invoke$' "$BUDGET_ROOT/claude-invocations")" == 2 )) \
        || fail "Buddy started autonomy beyond --max-autonomous-turns"
    process_active "$CURRENT_SERVER_PID" || fail "budget exhaustion stopped the server"
    jq -e 'select(.event == "autonomy_budget_exhausted" and .reason == "max_autonomous_turns" and .autonomous_turns == 2)' \
        "$BUDGET_ROOT/evidence.jsonl" >/dev/null || fail "budget evidence missing"
    (( "$(jq -c 'select(.event == "turn_finished" and .kind == "autonomy" and .succeeded == true)' "$BUDGET_ROOT/evidence.jsonl" | wc -l)" == 2 )) \
        || fail "evidence does not record exactly two successful autonomy turns"
    assert_no_player_joined "$BUDGET_ROOT"
    stop_buddy "$BUDGET_ROOT" "budget"
    pass "autonomous turn budget idles Buddy with the server alive and zero players"
fi

# Deadline: an autonomy turn still running at the deadline is cancelled with
# the same process-tree cleanup.
if want deadline; then
    assert_ports_unused
    BUDDY_EXTRA_ARGS=(--autonomy-deadline-seconds 5)
    start_buddy deadline fresh 1 interrupt
    DEADLINE_ROOT="$TEST_ROOT/deadline"
    DEADLINE_LOG="$DEADLINE_ROOT/buddy-fresh.log"
    wait_for_log "$CURRENT_BUDDY_PID" "$DEADLINE_LOG" "reason=\"autonomy_deadline\"" 30 \
        || wait_for_log "$CURRENT_BUDDY_PID" "$DEADLINE_LOG" "reason=autonomy_deadline" 5 \
        || fail "autonomy deadline was never reported"
    wait_for_log "$CURRENT_BUDDY_PID" "$DEADLINE_LOG" "process_group_reaped" 10 \
        || fail "deadline cancellation did not reap the Claude process group"
    sleep 3
    while IFS= read -r pid; do
        if [[ -r "/proc/$pid/stat" ]] && [[ "$(awk '{ print $3 }' "/proc/$pid/stat")" != "Z" ]]; then
            fail "deadline cancellation left grandchild $pid running"
        fi
    done < "$DEADLINE_ROOT/claude-invocations.grandchild"
    (( "$(grep -c '^invoke$' "$DEADLINE_ROOT/claude-invocations")" == 1 )) \
        || fail "Buddy started autonomy after its deadline"
    process_active "$CURRENT_SERVER_PID" || fail "deadline stopped the server"
    stop_buddy "$DEADLINE_ROOT" "deadline"
    pass "autonomy deadline cancels the running turn and idles with the server alive"
fi

# Jev modes require credentials before anything starts.
if want jev-credentials; then
    assert_ports_unused
    CRED_ROOT="$TEST_ROOT/jev-credentials"
    mkdir -p "$CRED_ROOT/home"
    set +e
    timeout --signal=KILL 10s env -u TYPESAFE_API_KEY HOME="$CRED_ROOT/home" \
        "$BUDDY_BIN" --start-server --fresh --agent runtime-live \
        --rcon-port "$RCON_PORT" --game-port "$GAME_PORT" \
        --factorio-bin "$FACTORIO_BIN" --write-data "$CRED_ROOT/write-data" \
        --save "$CRED_ROOT/save.zip" --mcp-bin "$MCP_BIN" --decision-mode jev \
        > "$CRED_ROOT/buddy.log" 2>&1
    CRED_STATUS=$?
    set -e
    (( CRED_STATUS != 0 && CRED_STATUS != 137 )) || fail "jev mode without a credential did not fail fast"
    grep -Fq "requires the TYPESAFE_API_KEY" "$CRED_ROOT/buddy.log" \
        || fail "missing-credential failure lacked a clear diagnostic"
    [[ ! -e "$CRED_ROOT/save.zip" ]] || fail "missing-credential startup created a save"
    assert_ports_unused
    pass "jev decision modes fail fast without TYPESAFE_API_KEY"
fi

# Maintenance runs through the real MCP binary. With no ready fuel repair in a
# fresh world, deterministic and jev-shadow must decline without mutating,
# never call Jev, reap their MCP child, and return to Opus.
for MODE in deterministic jev-shadow; do
    want "decision-$MODE" || continue
    assert_ports_unused
    BUDDY_EXTRA_ARGS=(--decision-mode "$MODE" --max-autonomous-turns 2)
    BUDDY_EXTRA_ENV=(TYPESAFE_API_KEY=fake-test-key TYPESAFE_API_URL=http://127.0.0.1:9/unreachable)
    start_buddy "decision-$MODE" fresh 1 success
    DEC_ROOT="$TEST_ROOT/decision-$MODE"
    DEC_LOG="$DEC_ROOT/buddy-fresh.log"
    wait_for_log "$CURRENT_BUDDY_PID" "$DEC_LOG" "autonomy_budget_exhausted" 90 \
        || fail "$MODE maintenance cycle never completed"
    jq -e --arg mode "$MODE" 'select(.event == "decision" and .mode == $mode and .eligible == false and .preview == null and .input_tokens == null and .latency_ms == null)' \
        "$DEC_ROOT/evidence.jsonl" >/dev/null || fail "$MODE did not record an ineligible nonmutating decision without a Jev latency"
    jq -e 'select(.event == "turn_finished" and .kind == "maintenance" and .cancelled == false and .succeeded == null)' \
        "$DEC_ROOT/evidence.jsonl" >/dev/null || fail "$MODE maintenance turn without a repair was not recorded as not executed"
    if jq -e 'select(.event == "maintenance_result")' "$DEC_ROOT/evidence.jsonl" >/dev/null; then
        fail "$MODE executed a maintenance repair without a ready preview"
    fi
    jq -c '[.event, .kind] | select(.[0] == "turn_finished")' "$DEC_ROOT/evidence.jsonl" \
        > "$DEC_ROOT/turn-order.jsonl"
    [[ "$(tr -d '\n' < "$DEC_ROOT/turn-order.jsonl")" == '["turn_finished","autonomy"]["turn_finished","maintenance"]["turn_finished","autonomy"]' ]] \
        || fail "$MODE did not run Opus, one maintenance check, then Opus: $(tr -d '\n' < "$DEC_ROOT/turn-order.jsonl")"
    [[ -z "$(owned_mcp_pids)" ]] || fail "$MODE left its maintenance MCP server running"
    stop_buddy "$DEC_ROOT" "decision-$MODE"
    pass "$MODE maintenance declines without a ready repair and reaps its MCP child"
done

printf 'Buddy managed-runtime live regression passed.\n'
