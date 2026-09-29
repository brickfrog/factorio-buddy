#!/usr/bin/env bash
# jq programs deliberately use $variables that must reach jq unexpanded, and
# trap/command-substitution callers are invisible to shellcheck's reachability.
# shellcheck disable=SC2016,SC2329
# Unattended real-model evaluation of Factorio Buddy.
#
# Every repetition runs Buddy (Claude Code, claude-opus-5-5) against its own
# fresh headless Factorio save with no connected players. Buddy's autonomy
# budget (30 autonomous turns or 15 minutes) ends all model and maintenance
# mutations; the runner then observes two consecutive 3600-tick holdout
# windows through the read-only `evaluation_sample` remote and records what
# the factory sustained on its own. Model prose is never evidence of success.
#
# Isolation: each trial has a private HOME, write-data, save, mod copy, Beads
# tracker and working directory under the new --output directory. The user's
# `.factorio-buddy`, `.env`, saves, repository tracker and listeners are never
# touched. Busy ports fail the run; nothing is ever killed that this script did
# not start. Raw Lua is used only by this operator process for fixture setup
# and independent read-only assertions, with FACTORIOCTL_ALLOW_RAW_LUA scoped
# to each single factorioctl command; it never reaches Buddy, Claude or MCP.

set -euo pipefail
umask 077

: "${HOME:?HOME must be set (the real HOME is needed for Claude authentication)}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(dirname "$SCRIPT_DIR")"

PLANNER_MODEL="claude-opus-5-5"
EFFORT="medium"
HEARTBEAT_SECONDS=5
TURN_TIMEOUT_SECONDS=300
MAX_AUTONOMOUS_TURNS=30
AUTONOMY_DEADLINE_SECONDS=900
HARD_WALL_SECONDS=1200
HOLDOUT_WINDOW_TICKS=3600
MONITOR_INTERVAL_SECONDS=10
SHUTDOWN_GRACE_SECONDS=75
ONLINE_TIMEOUT_SECONDS=300
TICK_STALL_SECONDS=120
GAME_SPEED_EXPECTED=1
AGENT_ID="eval"
SAMPLE_RADIUS=256
JEV_MODEL="jev-1.13.0"
JEV_INPUT_USD_PER_MILLION="0.042"
FIXTURE_CASES="missing-feed already-sustainable insufficient-materials blocked-route mixed-source-belt stale-consumer failed-unchanged-repair competing-consumers"
FIXTURE_VERSION=2
MCP_EXEC_TIMEOUT_SECONDS=240

RCON_PORT="${AUTONOMOUS_EVAL_RCON_PORT:-27217}"
GAME_PORT="${AUTONOMOUS_EVAL_GAME_PORT:-34399}"
BUDDY_BIN="${BUDDY_BIN:-$ROOT/target/release/buddy}"
MCP_BIN="${FACTORIO_MCP_BIN:-$ROOT/target/release/mcp}"
CLI_BIN="${FACTORIOCTL_BIN:-$ROOT/target/release/factorioctl}"
MAP_GEN="$ROOT/configs/map-gen.json"
SERVER_SETTINGS="$ROOT/configs/server.json"
MOD_SOURCE="$ROOT/mod/claude-interface"
REMOTE_API="$MOD_SOURCE/remote_api.json"

usage() {
    cat <<EOF
Usage: tests/autonomous_eval.sh --arm ARM --scenario SCENARIO --seed SEED \\
                                --repeat N --output NEW_DIR \\
                                [--fixture CASE] [--layout LAYOUT] [--fixture-only]
                                [--planner opus|noop] [--budget-minutes N]
                                [--continue-from SAVE_ZIP]

Run Buddy with ${PLANNER_MODEL} unattended against fresh isolated Factorio
saves and measure sustained, player-free production.

  --arm ARM            opus | deterministic | jev-shadow | jev
                       (maps to Buddy --decision-mode off | deterministic |
                       jev-shadow | jev; jev arms require TYPESAFE_API_KEY;
                       not needed with --fixture-only)
  --scenario SCENARIO  open-play   fresh map, managed map settings
                       fuel-repair lab-only prepared durable-fuel fixture
  --seed SEED          map generation seed (unsigned 32-bit integer)
  --repeat N           serial repetitions (positive integer)
  --output NEW_DIR     artifact directory; must not exist yet
  --fixture CASE       fuel-repair lab case (default missing-feed):
                       missing-feed | already-sustainable |
                       insufficient-materials | blocked-route |
                       mixed-source-belt | stale-consumer |
                       failed-unchanged-repair | competing-consumers
  --layout LAYOUT      primary | heldout (default primary). heldout rotates
                       the same case a quarter turn, moves it and changes its
                       obstacle geometry; never tune a policy on it
  --fixture-only       fuel-repair only: build, save and validate the fixture
                       (diagnosis, controller dry-run and case probes through
                       the MCP binary), then exit without Buddy or Claude
  --planner PLANNER    opus (default) or noop. noop is a fuel-repair-only
                       component replay: a stub planner returns every turn
                       without tool calls, so Buddy's maintenance decision
                       path (deterministic, Jev shadow, Jev) is exercised
                       against the prepared fixture without Opus fixing it
                       first. Never counts as a planner result.
  --budget-minutes N   autonomy deadline in minutes (default 15; 1-1440).
                       Turns scale to max(30, 2N); the hard wall is the
                       deadline plus 5 minutes. Use long budgets for runs
                       toward the rocket; compare only equal budgets
  --continue-from SAVE_ZIP
                       open-play only: start each trial from a copy of an
                       earlier trial's save.zip instead of a fresh map, so
                       runs toward the rocket accumulate. Buddy starts
                       without --fresh; the source file is never modified.
                       --seed must be the seed that save was made with
  -h, --help           show this help

Fixed trial settings: --effort ${EFFORT}, --heartbeat-seconds ${HEARTBEAT_SECONDS},
--turn-timeout-seconds ${TURN_TIMEOUT_SECONDS}, game speed ${GAME_SPEED_EXPECTED},
default autonomy budget 30 turns / 900 s (see --budget-minutes),
hard wall deadline + 300 s, holdout 2 x ${HOLDOUT_WINDOW_TICKS} ticks.

Environment:
  AUTONOMOUS_EVAL_RCON_PORT  RCON port (default 27217; must be free)
  AUTONOMOUS_EVAL_GAME_PORT  game UDP port (default 34399; must be free)
  BUDDY_BIN, FACTORIO_MCP_BIN, FACTORIOCTL_BIN
                             release binaries built from this tree
  FACTORIO_BIN               Factorio binary (otherwise discovered)
  TYPESAFE_API_KEY           required for jev-shadow and jev arms

Prerequisites: jq, ss, bd, git, timeout, sha256sum, an authenticated Claude
Code CLI with ${PLANNER_MODEL} access (subscription auth is verified once with a
minimal no-tools request), Factorio, and release buddy/mcp/factorioctl binaries.

Artifacts per trial (<output>/<arm>-<scenario>[-<case>-<layout>]-<seed>-r<i>/):
manifest.json, claude-stream.jsonl, buddy.log, server.log, evidence.jsonl,
samples/, save.zip + save.zip.buddy-owner.json, tracker/ (disposable Beads),
summary.json; fuel-repair adds fixture.json, fixture-dryrun.json and
fixture-validation.json. --fixture-only trials are named
fixture-only-<case>-<layout>-<seed>-r<i>. <output>/matrix.jsonl holds one
summary per trial; <output>/preflight.json the prerequisite checks. Exit
status: 0 all trials free of invariant failures, 1 an invariant failure
(unclean shutdown, contamination, fixture premise violated, ...), 2 usage or
preflight failure.
EOF
}

usage_error() {
    printf 'ERROR: %s\n' "$*" >&2
    printf 'Run tests/autonomous_eval.sh --help for usage.\n' >&2
    exit 2
}

log() {
    printf '[%s] %s\n' "$(date -u +%H:%M:%S)" "$*" >&2
}

unix_ms() {
    date +%s%3N
}

ARM=""
SCENARIO=""
SEED=""
REPEAT=""
OUTPUT=""
FIXTURE_CASE=""
LAYOUT=""
FIXTURE_ONLY=0
PLANNER="opus"
BUDGET_MINUTES=15
CONTINUE_FROM=""

while (( $# > 0 )); do
    option="$1"
    value=""
    has_inline_value=0
    if [[ "$option" == --*=* ]]; then
        value="${option#*=}"
        option="${option%%=*}"
        has_inline_value=1
    fi
    case "$option" in
        -h|--help)
            usage
            exit 0
            ;;
        --fixture-only)
            (( ! has_inline_value )) || usage_error "--fixture-only takes no value"
            FIXTURE_ONLY=1
            ;;
        --arm|--scenario|--seed|--repeat|--output|--fixture|--layout|--planner|--budget-minutes|--continue-from)
            if (( ! has_inline_value )); then
                (( $# >= 2 )) || usage_error "$option requires a value"
                value="$2"
                shift
            fi
            case "$option" in
                --arm) ARM="$value" ;;
                --scenario) SCENARIO="$value" ;;
                --seed) SEED="$value" ;;
                --repeat) REPEAT="$value" ;;
                --output) OUTPUT="$value" ;;
                --fixture) FIXTURE_CASE="$value" ;;
                --layout) LAYOUT="$value" ;;
                --planner) PLANNER="$value" ;;
                --budget-minutes) BUDGET_MINUTES="$value" ;;
                --continue-from) CONTINUE_FROM="$value" ;;
            esac
            ;;
        *)
            usage_error "unknown argument: $1"
            ;;
    esac
    shift
done

# Long runs (toward the rocket) scale the autonomy deadline; turns scale at
# two per minute, never below the default 30. The hard wall keeps 5 minutes
# of slack over the deadline.
[[ "$BUDGET_MINUTES" =~ ^[1-9][0-9]*$ ]] && (( BUDGET_MINUTES <= 1440 )) \
    || usage_error "--budget-minutes must be an integer from 1 to 1440"
AUTONOMY_DEADLINE_SECONDS=$(( BUDGET_MINUTES * 60 ))
MAX_AUTONOMOUS_TURNS=$(( BUDGET_MINUTES * 2 > 30 ? BUDGET_MINUTES * 2 : 30 ))
HARD_WALL_SECONDS=$(( AUTONOMY_DEADLINE_SECONDS + 300 ))

case "$ARM" in
    opus) DECISION_MODE="off" ;;
    deterministic) DECISION_MODE="deterministic" ;;
    jev-shadow) DECISION_MODE="jev-shadow" ;;
    jev) DECISION_MODE="jev" ;;
    "")
        (( FIXTURE_ONLY )) || usage_error "--arm is required"
        DECISION_MODE="none"
        ;;
    *) usage_error "invalid --arm '$ARM' (expected opus|deterministic|jev-shadow|jev)" ;;
esac
case "$PLANNER" in
    opus) ;;
    noop)
        [[ "$SCENARIO" == "fuel-repair" ]] || usage_error "--planner noop is only valid with --scenario fuel-repair"
        PLANNER_MODEL="noop-stub"
        ;;
    *) usage_error "invalid --planner '$PLANNER' (expected opus|noop)" ;;
esac
case "$SCENARIO" in
    open-play) LAB_ONLY=false ;;
    fuel-repair) LAB_ONLY=true ;;
    "") usage_error "--scenario is required" ;;
    *) usage_error "invalid --scenario '$SCENARIO' (expected open-play|fuel-repair)" ;;
esac
if [[ "$SCENARIO" == "fuel-repair" ]]; then
    FIXTURE_CASE="${FIXTURE_CASE:-missing-feed}"
    LAYOUT="${LAYOUT:-primary}"
    [[ " $FIXTURE_CASES " == *" $FIXTURE_CASE "* ]] \
        || usage_error "invalid --fixture '$FIXTURE_CASE' (expected one of: $FIXTURE_CASES)"
    [[ "$LAYOUT" == "primary" || "$LAYOUT" == "heldout" ]] \
        || usage_error "invalid --layout '$LAYOUT' (expected primary|heldout)"
else
    [[ -z "$FIXTURE_CASE" && -z "$LAYOUT" ]] || usage_error "--fixture and --layout require --scenario fuel-repair"
    (( ! FIXTURE_ONLY )) || usage_error "--fixture-only requires --scenario fuel-repair"
fi
if [[ -n "$CONTINUE_FROM" ]]; then
    [[ "$SCENARIO" == "open-play" ]] || usage_error "--continue-from requires --scenario open-play"
    [[ -f "$CONTINUE_FROM" ]] || usage_error "--continue-from '$CONTINUE_FROM' is not a file"
    CONTINUE_FROM="$(realpath -- "$CONTINUE_FROM")"
fi
if (( FIXTURE_ONLY )); then
    ARM="${ARM:-none}"
    TRIAL_PREFIX="fixture-only"
else
    TRIAL_PREFIX="$ARM"
fi
[[ -n "$SEED" ]] || usage_error "--seed is required"
if ! [[ "$SEED" =~ ^[0-9]{1,10}$ ]] || (( 10#$SEED > 4294967295 )); then
    usage_error "invalid --seed '$SEED' (expected an unsigned 32-bit integer)"
fi
SEED="$((10#$SEED))"
if [[ "$SCENARIO" == "fuel-repair" ]]; then
    TRIAL_STEM="$SCENARIO-$FIXTURE_CASE-$LAYOUT-$SEED"
else
    TRIAL_STEM="$SCENARIO-$SEED"
fi
[[ -n "$REPEAT" ]] || usage_error "--repeat is required"
[[ "$REPEAT" =~ ^[1-9][0-9]{0,3}$ ]] \
    || usage_error "invalid --repeat '$REPEAT' (expected a positive integer up to 9999)"
[[ -n "$OUTPUT" ]] || usage_error "--output is required"
if [[ -e "$OUTPUT" || -L "$OUTPUT" ]]; then
    usage_error "--output '$OUTPUT' already exists; choose a new directory"
fi
for port_name in RCON_PORT GAME_PORT; do
    port_value="${!port_name}"
    if ! [[ "$port_value" =~ ^[1-9][0-9]{0,4}$ ]] || (( port_value <= 1024 || port_value > 65535 )); then
        usage_error "invalid AUTONOMOUS_EVAL_$port_name: $port_value"
    fi
done
(( RCON_PORT != GAME_PORT )) || usage_error "RCON and game ports must differ"

OUTPUT_PARENT="$(dirname -- "$OUTPUT")"
mkdir -p -- "$OUTPUT_PARENT"
OUTPUT="$(cd -- "$OUTPUT_PARENT" && pwd)/$(basename -- "$OUTPUT")"
# A plain mkdir fails if another process created the directory meanwhile.
mkdir -- "$OUTPUT" 2>/dev/null || usage_error "--output '$OUTPUT' already exists or cannot be created"

# ---------------------------------------------------------------------------
# Shared helpers (process, port and ownership patterns from buddy_runtime.sh)
# ---------------------------------------------------------------------------

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

process_active() {
    local pid="$1"
    [[ -n "$pid" && -r "/proc/$pid/stat" ]] || return 1
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
    local log_file="$2"
    local pattern="$3"
    local timeout_seconds="$4"
    local deadline=$((SECONDS + timeout_seconds))
    while (( SECONDS < deadline )); do
        if grep -Fq -- "$pattern" "$log_file" 2>/dev/null; then
            return 0
        fi
        process_active "$pid" || return 1
        sleep 0.25
    done
    return 1
}

rcon_listener() {
    ss -H -ltn "sport = :$RCON_PORT" 2>/dev/null || true
}

game_listener() {
    ss -H -lun "sport = :$GAME_PORT" 2>/dev/null || true
}

ports_free() {
    [[ -z "$(rcon_listener)" && -z "$(game_listener)" ]]
}

wait_for_ports_free() {
    local timeout_seconds="$1"
    local deadline=$((SECONDS + timeout_seconds))
    until ports_free; do
        (( SECONDS < deadline )) || return 1
        sleep 0.25
    done
}

# A Factorio server owned by one trial is identified by its explicit loopback
# RCON bind and a command line that references the trial directory.
find_owned_server_pid() {
    local trial_root="$1"
    local proc
    local args
    for proc in /proc/[0-9]*; do
        [[ -r "$proc/cmdline" ]] || continue
        args="$(tr '\0' '\n' < "$proc/cmdline" 2>/dev/null)" || continue
        if grep -Fxq -- "--start-server" <<< "$args" \
            && grep -Fxq -- "--rcon-bind" <<< "$args" \
            && grep -Fxq -- "127.0.0.1:$RCON_PORT" <<< "$args" \
            && grep -Fq -- "$trial_root" <<< "$args"; then
            printf '%s\n' "${proc##*/}"
            return 0
        fi
    done
    return 1
}

no_owned_server() {
    local trial_root="$1"
    local deadline=$((SECONDS + 10))
    while (( SECONDS < deadline )); do
        if ! find_owned_server_pid "$trial_root" >/dev/null && ports_free; then
            return 0
        fi
        sleep 0.25
    done
    return 1
}

file_sha256() {
    if [[ -f "$1" ]]; then
        sha256sum -- "$1" | awk '{ print $1 }'
    else
        printf 'missing\n'
    fi
}

tree_sha256() {
    (cd -- "$1" && find . -type f -print0 | sort -z | xargs -0 sha256sum) \
        | sha256sum | awk '{ print $1 }'
}

json_or_null() {
    local file="$1"
    if [[ -s "$file" ]] && jq -e . "$file" >/dev/null 2>&1; then
        jq -c . "$file"
    else
        printf 'null'
    fi
}

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------

PREFLIGHT_JSON="$OUTPUT/preflight.json"
PREFLIGHT_CHECKS="$OUTPUT/.preflight-checks.jsonl"
: > "$PREFLIGHT_CHECKS"
PREFLIGHT_OK=1
CLAUDE_AUTH_JSON="null"

record_check() {
    local name="$1"
    local ok="$2"
    local detail="$3"
    if command -v jq >/dev/null 2>&1; then
        jq -cn --arg name "$name" --argjson ok "$ok" --arg detail "$detail" \
            '{name:$name, ok:$ok, detail:$detail}' >> "$PREFLIGHT_CHECKS"
    else
        printf '%s\t%s\t%s\n' "$name" "$ok" "$detail" >> "$PREFLIGHT_CHECKS"
    fi
    if [[ "$ok" != "true" ]]; then
        PREFLIGHT_OK=0
        printf 'PREFLIGHT FAIL: %s: %s\n' "$name" "$detail" >&2
    fi
}

write_preflight() {
    if command -v jq >/dev/null 2>&1; then
        jq -s \
            --arg arm "$ARM" \
            --arg scenario "$SCENARIO" \
            --argjson seed "$SEED" \
            --argjson repeat "$REPEAT" \
            --arg output "$OUTPUT" \
            --argjson ok "$([[ "$PREFLIGHT_OK" == 1 ]] && echo true || echo false)" \
            --argjson claude_auth "$CLAUDE_AUTH_JSON" \
            --argjson rcon_port "$RCON_PORT" \
            --argjson game_port "$GAME_PORT" \
            --arg fixture_case "$FIXTURE_CASE" \
            --arg layout "$LAYOUT" \
            --argjson fixture_only "$([[ "$FIXTURE_ONLY" == 1 ]] && echo true || echo false)" \
            --arg checked_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
            '{arm:$arm, scenario:$scenario, seed:$seed, repeat:$repeat, output:$output,
              fixture_case:(if $fixture_case == "" then null else $fixture_case end),
              layout:(if $layout == "" then null else $layout end), fixture_only:$fixture_only,
              checked_at:$checked_at, rcon_port:$rcon_port, game_port:$game_port,
              ok:$ok, checks:., claude_auth:$claude_auth}' \
            "$PREFLIGHT_CHECKS" > "$PREFLIGHT_JSON"
        rm -f -- "$PREFLIGHT_CHECKS"
    else
        mv -- "$PREFLIGHT_CHECKS" "$OUTPUT/preflight.tsv"
    fi
}

for command in awk date find git grep jq od sha256sum sleep sort ss tee timeout tr xargs; do
    if command -v "$command" >/dev/null 2>&1; then
        record_check "command:$command" true "$(command -v "$command")"
    else
        record_check "command:$command" false "required command is missing"
    fi
done
if (( ! FIXTURE_ONLY )); then
    if command -v bd >/dev/null 2>&1; then
        record_check "command:bd" true "$(command -v bd); disposable per-trial trackers only"
    else
        record_check "command:bd" false "bd is required so trial file_issue calls reach a disposable tracker instead of the real one"
    fi
fi

# Resolve the real Claude executable before any wrapper directory is placed
# first on PATH.
REAL_CLAUDE=""
if (( ! FIXTURE_ONLY )); then
    REAL_CLAUDE="$(command -v claude 2>/dev/null || true)"
    if [[ -n "$REAL_CLAUDE" && -x "$REAL_CLAUDE" ]]; then
        REAL_CLAUDE="$(readlink -f -- "$REAL_CLAUDE")"
        record_check "command:claude" true "$REAL_CLAUDE"
    else
        REAL_CLAUDE=""
        record_check "command:claude" false "Claude Code CLI not found on PATH"
    fi
fi

FACTORIO_BIN_RESOLVED=""
if FACTORIO_BIN_RESOLVED="$(find_factorio_bin)" && [[ -x "$FACTORIO_BIN_RESOLVED" ]]; then
    FACTORIO_BIN_RESOLVED="$(readlink -f -- "$FACTORIO_BIN_RESOLVED")"
    record_check "factorio" true "$FACTORIO_BIN_RESOLVED"
else
    FACTORIO_BIN_RESOLVED=""
    record_check "factorio" false "Factorio binary not found; set FACTORIO_BIN=/path/to/factorio"
fi

REQUIRED_BINARIES=("$MCP_BIN" "$CLI_BIN")
(( FIXTURE_ONLY )) || REQUIRED_BINARIES=("$BUDDY_BIN" "${REQUIRED_BINARIES[@]}")
for binary in "${REQUIRED_BINARIES[@]}"; do
    if [[ -x "$binary" ]]; then
        record_check "binary:$(basename -- "$binary")" true "$binary"
    else
        record_check "binary:$(basename -- "$binary")" false "not executable: $binary (build release binaries first)"
    fi
done
if (( ! FIXTURE_ONLY )) && [[ -x "$BUDDY_BIN" ]]; then
    BUDDY_HELP="$("$BUDDY_BIN" --help 2>&1 || true)"
    for flag in --issue-project-root --decision-mode --max-autonomous-turns \
        --autonomy-deadline-seconds --evidence-log --map-seed --turn-timeout-seconds; do
        if grep -Fq -- "$flag" <<< "$BUDDY_HELP"; then
            record_check "buddy-flag:$flag" true "supported"
        else
            record_check "buddy-flag:$flag" false "Buddy binary predates the evaluation contract (missing $flag)"
        fi
    done
fi
for file in "$MAP_GEN" "$SERVER_SETTINGS" "$MOD_SOURCE/info.json" "$REMOTE_API"; do
    if [[ -f "$file" ]]; then
        record_check "file:${file#"$ROOT"/}" true "$file"
    else
        record_check "file:${file#"$ROOT"/}" false "missing $file"
    fi
done
if [[ -f "$REMOTE_API" ]] && command -v jq >/dev/null 2>&1; then
    for remote in evaluation_sample connected_player_count_result register_agent \
        pre_place_character_result get_character diagnose_fuel_sustainability; do
        if jq -e --arg name "$remote" '.remotes | has($name)' "$REMOTE_API" >/dev/null 2>&1; then
            record_check "remote:$remote" true "listed in remote_api.json"
        else
            record_check "remote:$remote" false "claude_interface remote $remote is not in remote_api.json"
        fi
    done
fi

if (( ! FIXTURE_ONLY )) && [[ "$ARM" == jev-shadow || "$ARM" == jev ]]; then
    if [[ -n "${TYPESAFE_API_KEY:-}" ]]; then
        record_check "credential:TYPESAFE_API_KEY" true "present (value not recorded)"
    else
        record_check "credential:TYPESAFE_API_KEY" false "required for the $ARM arm"
    fi
fi
if [[ -n "${TYPESAFE_API_URL:-}" ]]; then
    record_check "env:TYPESAFE_API_URL" true "operator override ignored; trials use Buddy's default official endpoint"
fi

if ports_free; then
    record_check "ports" true "127.0.0.1:$RCON_PORT/tcp and $GAME_PORT/udp are free"
else
    record_check "ports" false "RCON $RCON_PORT or game $GAME_PORT is already in use; refusing to disturb an existing listener"
fi

# Claude runs with a minimal environment: the real HOME for subscription
# authentication, no ANTHROPIC_* API credentials, no raw-Lua opt-in.
CLAUDE_ENV=(
    "PATH=$PATH"
    "HOME=$HOME"
    "USER=${USER:-$(id -un)}"
    "LOGNAME=${LOGNAME:-${USER:-$(id -un)}}"
    "LANG=${LANG:-C.UTF-8}"
)
if [[ -n "${CLAUDE_CONFIG_DIR:-}" ]]; then
    CLAUDE_ENV+=("CLAUDE_CONFIG_DIR=$CLAUDE_CONFIG_DIR")
fi

if (( PREFLIGHT_OK && ! FIXTURE_ONLY )) && [[ "$PLANNER" == "opus" ]]; then
    mkdir -p "$OUTPUT/preflight-cwd"
    log "validating Claude subscription authentication for $PLANNER_MODEL"
    auth_started="$(unix_ms)"
    set +e
    (
        cd "$OUTPUT/preflight-cwd" && env -i "${CLAUDE_ENV[@]}" \
            timeout --signal=TERM --kill-after=10s 180s \
            "$REAL_CLAUDE" --print --model "$PLANNER_MODEL" --tools '' \
                --setting-sources '' --strict-mcp-config --output-format json \
                -- 'Reply with exactly: OK'
    ) < /dev/null > "$OUTPUT/preflight-claude.json" 2> "$OUTPUT/preflight-claude.stderr"
    auth_status=$?
    set -e
    auth_elapsed=$(( $(unix_ms) - auth_started ))
    if jq -e . "$OUTPUT/preflight-claude.json" >/dev/null 2>&1; then
        CLAUDE_AUTH_JSON="$(jq -c \
            --argjson exit_status "$auth_status" \
            --argjson elapsed_ms "$auth_elapsed" \
            --arg model "$PLANNER_MODEL" \
            '{exit_status:$exit_status, elapsed_ms:$elapsed_ms,
              is_error:(if has("is_error") then .is_error else null end), subtype:(.subtype // null),
              reply_ok:((.result // "") | gsub("^\\s+|\\s+$"; "") | test("^OK\\.?$")),
              models:((.modelUsage // {}) | keys),
              requested_model_used:(((.modelUsage // {}) | keys) | any(startswith($model))),
              total_cost_usd:(.total_cost_usd // null)}' \
            "$OUTPUT/preflight-claude.json")"
    else
        CLAUDE_AUTH_JSON="$(jq -cn --argjson exit_status "$auth_status" \
            --argjson elapsed_ms "$auth_elapsed" \
            '{exit_status:$exit_status, elapsed_ms:$elapsed_ms, parse_error:true}')"
    fi
    if jq -e '.exit_status == 0 and .is_error == false and .reply_ok == true and .requested_model_used == true' \
        <<< "$CLAUDE_AUTH_JSON" >/dev/null 2>&1; then
        record_check "claude-auth" true "subscription request succeeded with $PLANNER_MODEL"
    else
        record_check "claude-auth" false "minimal $PLANNER_MODEL request failed (see preflight-claude.json/.stderr); model arms are blocked and no other model is substituted"
    fi
fi

write_preflight
if (( ! PREFLIGHT_OK )); then
    printf 'Preflight failed; no trial was started. See %s\n' "$PREFLIGHT_JSON" >&2
    exit 2
fi

FACTORIO_DATA_ROOT="$(dirname -- "$(dirname -- "$(dirname -- "$FACTORIO_BIN_RESOLVED")")")"
GIT_REV="$(git -C "$ROOT" rev-parse HEAD 2>/dev/null || echo unknown)"
GIT_PORCELAIN="$(git -C "$ROOT" status --porcelain=v1 2>/dev/null || true)"
GIT_PORCELAIN_SHA256="$(printf '%s' "$GIT_PORCELAIN" | sha256sum | awk '{ print $1 }')"
GIT_DIRTY_ENTRIES="$(printf '%s' "$GIT_PORCELAIN" | grep -c . || true)"
GIT_DIFF_SHA256="$(git -C "$ROOT" diff HEAD --binary 2>/dev/null | sha256sum | awk '{ print $1 }')"
MOD_VERSION="$(jq -r '.version' "$MOD_SOURCE/info.json")"
MOD_TREE_SHA256="$(tree_sha256 "$MOD_SOURCE")"

# ---------------------------------------------------------------------------
# jq programs
# ---------------------------------------------------------------------------

# One holdout window from two evaluation_sample snapshots.
jq_window_program() {
    cat <<'JQ'
def total: if type == "object" then ([.[] | numbers] | add // 0) else 0 end;
def science_total: if type == "object"
    then ([to_entries[] | select(.key | endswith("-science-pack")) | .value | numbers] | add // 0)
    else 0 end;
def by_unit(f): [f[]? | select(.unit_number != null) | {key: (.unit_number | tostring), value: .}] | from_entries;
def prod($s; $k): ($s.force_item_production[$k].input_count // 0);
def science_keys($s): [($s.force_item_production // {}) | keys[] | select(endswith("-science-pack"))];
["iron-plate", "copper-plate", "coal", "automation-science-pack", "stone", "iron-ore", "copper-ore"] as $tracked
| $a[0] as $s0 | $b[0] as $s1
| by_unit($s0.machines) as $m0 | by_unit($s1.machines) as $m1
| by_unit($s0.labs) as $l0 | by_unit($s1.labs) as $l1
| (($tracked + science_keys($s0) + science_keys($s1)) | unique) as $items
| (($s1.research.researched_count // 0) - ($s0.research.researched_count // 0)) as $researched_delta
| [$l1 | to_entries[] | .key as $k | .value as $e | $l0[$k] as $p | select($p != null) | {
      unit_number: $e.unit_number,
      status_start: $p.status,
      status_end: $e.status,
      science_start: ($p.inventory | science_total),
      science_end: ($e.inventory | science_total),
      science_delta: (($e.inventory | science_total) - ($p.inventory | science_total))
  }] as $labs
| ([$items[] | {key: ., value: (prod($s1; .) - prod($s0; .))}] | from_entries) as $production
| {
    start_tick: $s0.tick,
    end_tick: $s1.tick,
    ticks: ($s1.tick - $s0.tick),
    connected_players: [$s0.connected_players, $s1.connected_players],
    machines: [$m1 | to_entries[] | .key as $k | .value as $e | $m0[$k] as $p | select($p != null) | {
        unit_number: $e.unit_number,
        name: $e.name,
        type: $e.type,
        recipe: ($e.recipe // $p.recipe),
        status_start: $p.status,
        status_end: $e.status,
        products_delta: (if ($e.products_finished | type) == "number" and ($p.products_finished | type) == "number"
            then $e.products_finished - $p.products_finished else null end),
        fuel_start: ($p.fuel | total),
        fuel_end: ($e.fuel | total)
    }],
    machines_added: ([$m1 | keys[] | select($m0[.] == null)] | length),
    machines_removed: ([$m0 | keys[] | select($m1[.] == null)] | length),
    labs: $labs,
    lab_science_delta: ([$labs[].science_delta] | add // 0),
    force_production_delta: $production,
    science_production_delta: ([$production | to_entries[] | select(.key | endswith("-science-pack")) | .value] | add // 0),
    research: {
        current_start: $s0.research.current,
        current_end: $s1.research.current,
        progress_start: $s0.research.progress,
        progress_end: $s1.research.progress,
        researched_count_delta: $researched_delta,
        progressed: ($researched_delta > 0
            or ($s0.research.current != null
                and $s0.research.current == $s1.research.current
                and (($s1.research.progress // 0) > ($s0.research.progress // 0))))
    },
    character_inventory_changed: ($s0.character.inventory != $s1.character.inventory),
    character_moved: ($s0.character.position != $s1.character.position)
  }
JQ
}

# Milestones from the two holdout windows. Only production observed while no
# model or maintenance turn can run counts; status-only evidence is reported
# separately and never promoted to success.
jq_milestones_program() {
    cat <<'JQ'
def plates: ["iron-plate", "copper-plate", "steel-plate"];
def electric: (.type == "assembling-machine" or .type == "lab"
    or .name == "electric-mining-drill" or .name == "electric-furnace" or .name == "big-mining-drill");
def units(f): [.machines[] | select(f) | .unit_number];
def both(f): ($w[0] | units(f)) as $x | ($w[1] | units(f)) as $y | [$x[] | select(. as $u | any($y[]; . == $u))];
def working_labs: [.labs[] | select(.status_end == "working") | .unit_number];
# Delivery is judged at the labs: packs arrived (lab inventory grew) or labs
# consumed packs for research while their stock did not drain, which needs
# replenishment. Force-wide science production alone can come from an
# assembler that feeds nothing, so it is reported but never sufficient.
def science_delivered: (any(.labs[]; .science_delta > 0)
        or (.research.progressed and any(.labs[]; .status_end == "working") and (.lab_science_delta >= 0)))
    and (.character_inventory_changed | not);
both(.type == "furnace" and ((.products_delta // 0) > 0)
    and (.recipe == null or (.recipe as $r | any(plates[]; . == $r)))) as $plate_units
| both(electric and ((.products_delta // 0) > 0)) as $powered_units
| both(electric and .products_delta == null and .status_end == "working") as $status_only_units
| (if ($w[0].research.progressed and $w[1].research.progressed)
    then (($w[0] | working_labs) as $x | ($w[1] | working_labs) as $y | [$x[] | select(. as $u | any($y[]; . == $u))])
    else [] end) as $research_labs
| {
    evaluated: true,
    basis: "two consecutive holdout windows with model and maintenance turns stopped",
    plate_automation: (
        # A hand-stocked furnace can smelt through both 60 s windows from its
        # buffer. Require ore to be mined during each window as well; the
        # force counter cannot attribute ore to one furnace, so this is a
        # necessary condition rather than per-furnace proof.
        ([$w[] | ((.force_production_delta["iron-ore"] // 0) + (.force_production_delta["copper-ore"] // 0)) > 0] | all) as $ore_mined
        | {
            achieved: (($plate_units | length > 0) and $ore_mined),
            furnace_units: $plate_units,
            ore_mined_each_window: $ore_mined,
            furnace_fuel: [$w[1].machines[] | .unit_number as $u | select(any($plate_units[]; . == $u))
                | {unit_number, fuel_start_window1: ([$w[0].machines[] | select(.unit_number == $u) | .fuel_start] | first), fuel_end_window2: .fuel_end}]
        }
    ),
    powered_production: {
        achieved: (($powered_units | length > 0) or ($research_labs | length > 0)),
        product_units: $powered_units,
        researching_lab_units: $research_labs,
        status_only_units_not_counted: $status_only_units
    },
    automated_science_delivery: {
        achieved: (($w[0] | science_delivered) and ($w[1] | science_delivered)),
        science_production_delta: [$w[0].science_production_delta, $w[1].science_production_delta],
        lab_science_delta: [$w[0].lab_science_delta, $w[1].lab_science_delta],
        character_inventory_changed: [$w[0].character_inventory_changed, $w[1].character_inventory_changed]
    },
    research_progress: {
        achieved: ($w[0].research.progressed and $w[1].research.progressed),
        per_window: [$w[0].research, $w[1].research]
    }
  }
JQ
}

# Case-aware durable-fuel outcome for the lab fixtures. Inputs: $fixture (the
# fixture record with its per-case expectations), $current (fixture targets
# after any runner perturbation), $r (fixture readbacks at the start, middle
# and end of the holdout), $diagnosis (fuel diagnosis at holdout end), $ev
# (evidence summary), $perturbation (stale-consumer replacement or null) and
# $buddy_start_ms. A target is measured only by its own products_finished or
# by the amount mined from its own isolated resource patch; status or fuel
# stock alone never makes a target sustained.
jq_fuel_program() {
    cat <<'JQ'
def by_role($rb; $role): ($rb.targets // []) | map(select(.role == $role)) | first;
def num: type == "number";
def consumer($u): (($diagnosis.consumers // []) | map(select(.unit_number == $u)) | first);
($fixture.case) as $case
| ($fixture.expected // {}) as $expected
| [$current[] as $t
   | by_role($r[0]; $t.role) as $x0 | by_role($r[1]; $t.role) as $x1 | by_role($r[2]; $t.role) as $x2
   | [$x0, $x1, $x2] as $xs
   | (all($xs[]; . != null and .valid == true)) as $present
   | (if $present and all($xs[]; .products_finished | num) then
          {kind: "products_finished", deltas: [$x1.products_finished - $x0.products_finished, $x2.products_finished - $x1.products_finished]}
      elif $present and all($xs[]; (.patch_amount | num) and .other_drills_on_patch == 0) then
          {kind: "own_patch_mined", deltas: [$x0.patch_amount - $x1.patch_amount, $x1.patch_amount - $x2.patch_amount]}
      else null end) as $m
   | consumer($t.unit_number) as $d
   | [$xs[] | .status] as $statuses
   | {
       role: $t.role,
       unit_number: $t.unit_number,
       primary: ($t.primary == true),
       present_in_all_samples: $present,
       statuses: $statuses,
       fuel_counts: [$xs[] | .fuel_count],
       measure: (if $m == null then "status_only_not_benchmark_grade" else $m.kind end),
       window_output: (if $m == null then null else $m.deltas end),
       automated_at_holdout_end: (if $d == null then null else $d.automated end),
       feeders_holding_other_items: [($x2.feeding_inserters // [])[] | select(.held_item != null and .held_item != "coal") | .unit_number],
       sustained: (if $m == null then null
           else ($present and ($d.automated == true)
               and (any($statuses[]; . == null or . == "no_fuel") | not)
               and $m.deltas[0] > 0 and $m.deltas[1] > 0) end)
     }
  ] as $targets
| [$targets[] | select(.primary)] as $primary
| [$primary[] | select(.sustained != null)] as $measured
| [$primary[] | select(.sustained == true)] as $sustained
| ($ev.repair_executions // []) as $execs
| [$execs[] | select(.success == true)] as $claims
| [$execs[] | select(.success != true)] as $failed
| (any($targets[]; .automated_at_holdout_end == true)) as $any_automated
| ($r[2] // {}) as $end
| (if $case == "already-sustainable" then
      ((($end.units_missing // []) | length) == 0 and (($end.units_moved // []) | length) == 0) as $intact
      | ([$targets[] | select(.automated_at_holdout_end != null) | .automated_at_holdout_end] | all) as $all_fed
      | {achieved: (if ($measured | length) == 0 then null else ($intact and $all_fed and ($sustained | length) == ($primary | length)) end),
         reason: (if ($measured | length) == 0 then "outcome_unknown: the fed drill had no measured output" else null end),
         fixture_intact: $intact, units_missing: ($end.units_missing // []), units_moved: ($end.units_moved // []),
         every_burner_target_automated: $all_fed, repair_claims: ($claims | length)}
   elif $case == "insufficient-materials" then
      (($claims | length) > 0 and ($any_automated | not)) as $false_success
      | {achieved: ($false_success | not),
         outcome: (if $false_success then "false_success" elif ($claims | length) > 0 then "repaired_after_acquiring_materials" else "no_repair_claimed" end),
         false_success: $false_success, repair_claims: ($claims | length), failed_executions: ($failed | length),
         inventory_start: ($fixture.inventory // null), inventory_end: ($end.character_inventory // null)}
   elif $case == "stale-consumer" then
      if ($perturbation == null or $perturbation.success != true) then
          {achieved: null, reason: "outcome_unknown: the replacement perturbation was not triggered (no repair preview observed)"}
      else
          ([$expected.stale_unit_number, $perturbation.old_unit_number] | map(select(. != null))) as $stale
          | [$claims[] | select(.consumer_unit_number as $c | $c != null and any($stale[]; . == $c))] as $stale_claims
          | {achieved: (if ($measured | length) == 0 then null else (($stale_claims | length) == 0 and ($sustained | length) > 0) end),
             reason: (if ($measured | length) == 0 then "outcome_unknown: the replacement drill had no measured output" else null end),
             stale_unit_numbers: $stale, stale_success_claims: $stale_claims, replacement: $perturbation}
      end
   elif $case == "failed-unchanged-repair" then
      {achieved: (($failed | length) <= 1),
       failed_executions: ($failed | length),
       repeated_unchanged_failure: (($failed | length) > 1),
       caveat: "every failed execution after the first is counted as a repeat; the runner does not check whether the world changed in between",
       furnace_later_sustained: (($sustained | length) > 0)}
   elif $case == "competing-consumers" then
      ([$primary[] | select(.automated_at_holdout_end == true)] | length) as $fed
      | (($claims | length) > $fed) as $false_success
      | {achieved: (if ($measured | length) == 0 then null else (($sustained | length) >= 1 and ($false_success | not)) end),
         reason: (if ($measured | length) == 0 then "outcome_unknown: no drill had measured output" else null end),
         sustained_consumers: ($sustained | length), automated_consumers: $fed,
         repair_claims: ($claims | length), false_success: $false_success}
   else
      {achieved: (if ($measured | length) == 0 then null else (($sustained | length) > 0) end),
       reason: (if ($measured | length) == 0 then "outcome_unknown: no primary target had measured output in both holdout windows" else null end),
       jammed_feeders: [$targets[] | select((.feeders_holding_other_items | length) > 0) | {role, feeders: .feeders_holding_other_items}]}
   end) as $outcome
| ([$sustained[] | .unit_number]) as $sustained_units
| ([$claims[] | select(.consumer_unit_number as $c | $c != null and any($sustained_units[]; . == $c))] | sort_by(.unix_ms) | first) as $matched
| ([$claims[] | select(($ev.exhausted.unix_ms // null) == null or .unix_ms <= $ev.exhausted.unix_ms)] | sort_by(.unix_ms) | last) as $last_claim
| ($expected.repair_expected == true or $expected.repair_expected == "exactly_one") as $repair_case
| $outcome + {
    case: $case,
    layout: $fixture.layout,
    expected: $expected,
    lab_only: true,
    basis: "measured output (products_finished, or the amount mined from the target's own resource patch) greater than zero in both holdout windows, a durable fuel topology at holdout end and no no_fuel status; status and fuel stock alone never count",
    targets: $targets,
    repair_executions: $execs,
    time_to_verified_fuel_recovery_ms: (
        if ($outcome.achieved == true) and $repair_case and $buddy_start_ms != null then
            (if $matched != null then $matched.unix_ms - $buddy_start_ms
             elif $last_claim != null then $last_claim.unix_ms - $buddy_start_ms else null end)
        else null end),
    time_basis: (if ($outcome.achieved != true) or ($repair_case | not) or $buddy_start_ms == null then null
        elif $matched != null then "Buddy launch to the first successful repair of a consumer that the holdout confirmed sustained"
        elif $last_claim != null then "upper bound: Buddy launch to the last successful repair before the budget ended (repair results did not identify the consumer)"
        else null end)
  }
JQ
}

jq_evidence_program() {
    cat <<'JQ'
def tool_name: ((.tool // "") | sub("^mcp__factorio__"; ""));
def count(f): [.[] | select(f)] | length;
. as $ev
| ([$ev[] | select(.event == "autonomy_budget_exhausted")] | first) as $exhausted
| [$ev[] | select(.event == "turn_finished")] as $turns
| [$ev[] | select(.event == "maintenance_result")] as $maintenance
# Buddy's maintenance loop logs its own MCP call as a tool_outcome ~100 ms
# before the matching maintenance_result; attribute those to maintenance so
# they are neither counted as model tool calls nor as a second execution.
| [$ev[] | select(.event == "tool_outcome")] as $all_tools
| [$all_tools[] | . as $t | select(tool_name == "repair_fuel_sustainability"
      and any($maintenance[]; .unix_ms >= $t.unix_ms and .unix_ms - $t.unix_ms <= 1000))] as $maintenance_tools
| [$all_tools[] | . as $t | select(any($maintenance_tools[]; . == $t) | not)] as $tools
| [$ev[] | select(.event == "decision")] as $decisions
# executed is either top level or inside result_summary.
| def executed: if has("executed") then .executed
    else (.result_summary | if type == "string" then (fromjson? // {}) elif type == "object" then . else {} end | .executed) end;
[$maintenance[] | select(.success == true and executed != false)] as $maintenance_executed_ok
# Every executed (non-dry-run) fuel repair, from Buddy's maintenance loop or a
# model tool call. Model tool outcomes do not name the consumer.
| ([($maintenance[] | select(executed != false)
        | {unix_ms, source: "maintenance_result", success: (.success == true), consumer_unit_number: (.actual_consumer_unit_number // .consumer_unit_number // null), transaction_changed: (.transaction_changed // false)}),
    ($tools[] | select(tool_name == "repair_fuel_sustainability" and ((.arguments.dry_run // false) == false))
        | {unix_ms, source: "tool_outcome", success: (.is_error != true), consumer_unit_number: null})]
   | sort_by(.unix_ms)) as $execs
| {
    events: ($ev | length),
    # Model turns only: non-cancelled autonomy and human turns. Maintenance
    # turns are controller actions, reported separately below.
    completed_turns: ($turns | count((.kind == "autonomy" or .kind == "human") and .cancelled != true)),
    turns_by_kind: ($turns | group_by(.kind // "unknown") | map({key: (.[0].kind // "unknown"), value: length}) | from_entries),
    succeeded_turns: ($turns | count((.kind == "autonomy" or .kind == "human") and .succeeded == true)),
    # succeeded is the executed repair outcome; null when nothing executed.
    maintenance_turns: {
        total: ($turns | count(.kind == "maintenance")),
        cancelled: ($turns | count(.kind == "maintenance" and .cancelled == true)),
        repair_succeeded: ($turns | count(.kind == "maintenance" and .succeeded == true)),
        repair_failed: ($turns | count(.kind == "maintenance" and .cancelled != true and .succeeded == false)),
        not_executed: ($turns | count(.kind == "maintenance" and .cancelled != true and .succeeded == null))
    },
    cancelled_turns: ($turns | count(.cancelled == true)),
    provider_limited_turns: ($turns | count(.provider_limited == true)),
    tool_calls: ($tools | length),
    tool_errors: ($tools | count(.is_error == true)),
    tool_errors_by_tool: ([$tools[] | select(.is_error == true) | tool_name] | group_by(.) | map({key: .[0], value: length}) | from_entries),
    repeated_failures: ($ev | count(.event == "repeated_failure_hint")),
    repeated_failure_hints: ([$ev[] | select(.event == "repeated_failure_hint") | {tool: tool_name, error: .error, count: .count, unix_ms: .unix_ms}] | .[:20]),
    interrupted_tools: ([$ev[] | select(.event == "interrupted_tool") | {tool: tool_name, unix_ms}]),
    exhausted: $exhausted,
    post_exhaustion_tool_outcomes: (if $exhausted == null then null
        else ($all_tools | count((.unix_ms // 0) > $exhausted.unix_ms)) end),
    maintenance_tool_outcomes: ($maintenance_tools | length),
    decisions: {
        count: ($decisions | length),
        eligible: ($decisions | count(.eligible == true)),
        by_choice: ($decisions | group_by(.choice // "none") | map({key: (.[0].choice // "none"), value: length}) | from_entries),
        unavailable: ($decisions | count(.unavailable_reason != null)),
        unavailable_reasons: ([$decisions[] | .unavailable_reason | select(. != null)] | group_by(.) | map({key: .[0], value: length}) | from_entries),
        models: ([$decisions[] | .model | select(. != null)] | unique),
        confidences: [$decisions[] | .confidence | select(. != null)],
        latency_ms: [$decisions[] | .latency_ms | select(. != null)],
        input_tokens: (if any($decisions[]; (.input_tokens | type) == "number")
            then ([$decisions[] | .input_tokens | numbers] | add) else null end),
        requests_without_usage: ($decisions | count(.model != null and (.input_tokens | type) != "number")),
        by_selection: ($decisions | group_by(.selection // "none") | map({key: (.[0].selection // "none" | tostring), value: length}) | from_entries),
        jev_spent_usd_estimate: ([$decisions[] | .jev_spent_usd_estimate | numbers] | max)
    },
    maintenance: {
        results: ($maintenance | length),
        executed: ($maintenance | count(executed != false)),
        skipped: ($maintenance | count(executed == false)),
        executed_successes: ($maintenance_executed_ok | length),
        consumers: ([$maintenance_executed_ok[] | (.actual_consumer_unit_number // .consumer_unit_number)] | unique)
    },
    repair_executions: $execs
  }
JQ
}

jq_claude_usage_program() {
    cat <<'JQ'
[split("\n")[] | fromjson? | select(type == "object")] as $events
| [$events[] | select(.type == "result")] as $results
| [$events[] | select(.type == "system" and .subtype == "init")] as $inits
| def sum(f): ([$results[] | f | numbers] | add // 0);
  if ($results | length) == 0 then {
    claude: null,
    claude_explanation: "no Claude result events were captured in claude-stream.jsonl",
    claude_invocations: ($inits | length),
    observed_models: ([$inits[] | .model | select(. != null)] | unique),
    api_key_sources: ([$inits[] | .apiKeySource | select(. != null)] | unique)
  } else {
    claude: {
        result_events: ($results | length),
        invocations: ($inits | length),
        invocations_without_result: (($inits | length) - ($results | length)),
        input_tokens: sum(.usage.input_tokens),
        output_tokens: sum(.usage.output_tokens),
        cache_creation_input_tokens: sum(.usage.cache_creation_input_tokens),
        cache_read_input_tokens: sum(.usage.cache_read_input_tokens),
        # Claude Code reports total_cost_usd cumulatively for a resumed
        # session, so summing result events overcounts; take each session's
        # maximum instead.
        total_cost_usd: (if all($results[]; (.total_cost_usd | type) == "number")
            then ([$results | group_by(.session_id)[] | map(.total_cost_usd) | max] | add) else null end),
        provider_limited_results: ([$results[] | select(.is_error == true
            and (.api_error_status == 429 or ((.result // "") | test("limit"; "i"))))] | length),
        num_turns: sum(.num_turns),
        duration_ms: sum(.duration_ms),
        duration_api_ms: sum(.duration_api_ms),
        models: ([$results[] | (.modelUsage // {}) | keys[]] | unique)
    },
    claude_explanation: ("token counts sum Claude Code result events; total_cost_usd is the per-session maximum of Claude Code's cumulative estimate (subscription use is quota-based)"
        + (if ($inits | length) > ($results | length)
            then "; cancelled/interrupted invocations emitted no result event and are not counted" else "" end)),
    observed_models: ([$inits[] | .model | select(. != null)] | unique),
    api_key_sources: ([$inits[] | .apiKeySource | select(. != null)] | unique)
  } end
JQ
}

# ---------------------------------------------------------------------------
# Trial
# ---------------------------------------------------------------------------

run_trial() {
    local repetition="$1"
    TRIAL="$OUTPUT/$TRIAL_PREFIX-$TRIAL_STEM-r$repetition"
    SAMPLES="$TRIAL/samples"
    EVIDENCE="$TRIAL/evidence.jsonl"
    BUDDY_LOG="$TRIAL/buddy.log"
    FAILURES="$TRIAL/invariant-failures.jsonl"
    SAVE="$TRIAL/save.zip"
    WRITE_DATA="$TRIAL/write-data"
    TRACKER="$TRIAL/tracker"
    BUDDY_PID=""
    FIXTURE_PID=""
    RCON_SECRET=""
    FIXTURE_SECRET=""
    STOP_REASON="not_started"
    BUDDY_START_MS=""
    BUDDY_STOP_MS=""
    BEFORE_TICK="null"
    STOP_TICK="null"
    BUDDY_EXIT_STATUS="null"
    SHUTDOWN_FORCED=false
    OBSERVED_GAME_SPEED="null"
    MONITOR_ERRORS=0
    HOLDOUT_REASON="not reached"
    SHUTDOWN_DONE=0
    MCP_PID=""
    PERTURBATION_DONE=0

    mkdir -p "$TRIAL" "$SAMPLES" "$TRIAL/bin" "$TRIAL/work" \
        "$TRIAL/home/.factorio/mods" "$WRITE_DATA/mods" "$TRACKER"
    : > "$FAILURES"
    : > "$SAMPLES/players.jsonl"
    trap finalize_trial EXIT
    trap 'invariant_failure interrupted "trial received a termination signal"; STOP_REASON=interrupted; exit 143' TERM INT

    log "trial $TRIAL"
    write_manifest "$repetition"
    prepare_trial_home
    if (( ! FIXTURE_ONLY )); then
        prepare_tracker
        write_claude_wrapper
    fi

    ports_free || fail_trial port_conflict "RCON $RCON_PORT or game $GAME_PORT became busy before the trial"

    if [[ "$SCENARIO" == "fuel-repair" ]]; then
        prepare_fuel_fixture
    elif [[ -n "$CONTINUE_FROM" ]]; then
        cp -f -- "$CONTINUE_FROM" "$SAVE"
        update_manifest '.continue_from = {save: $src, sha256: $sha}' \
            --arg src "$CONTINUE_FROM" --arg sha "$(file_sha256 "$CONTINUE_FROM")"
        log "continuing from $CONTINUE_FROM"
    fi
    if (( FIXTURE_ONLY )); then
        STOP_REASON="fixture_only"
        return 0
    fi

    start_buddy
    take_before_sample
    verify_issue_root
    monitor_until_budget
    if [[ "$STOP_REASON" == autonomy_budget_exhausted* ]]; then
        run_holdout
    fi
    take_sample after || invariant_failure sample_failed "final after-sample failed"
    if [[ "$STOP_TICK" == "null" && -f "$SAMPLES/after.json" ]]; then
        STOP_TICK="$(jq '.tick' "$SAMPLES/after.json")"
    fi
    shutdown_buddy
}

invariant_failure() {
    jq -cn --arg code "$1" --arg detail "$2" --argjson unix_ms "$(unix_ms)" \
        '{code:$code, detail:$detail, unix_ms:$unix_ms}' >> "$FAILURES"
    log "INVARIANT FAILURE [$1]: $2"
}

fail_trial() {
    invariant_failure "$1" "$2"
    exit 1
}

write_manifest() {
    local peaceful
    peaceful="$(jq -c '.peaceful_mode // null' "$MAP_GEN")"
    jq -n \
        --arg arm "$ARM" \
        --arg scenario "$SCENARIO" \
        --argjson seed "$SEED" \
        --argjson repeat "$1" \
        --arg trial_dir "$TRIAL" \
        --arg decision_mode "$DECISION_MODE" \
        --arg model "$PLANNER_MODEL" \
        --arg effort "$EFFORT" \
        --argjson heartbeat "$HEARTBEAT_SECONDS" \
        --argjson turn_timeout "$TURN_TIMEOUT_SECONDS" \
        --argjson max_turns "$MAX_AUTONOMOUS_TURNS" \
        --argjson deadline "$AUTONOMY_DEADLINE_SECONDS" \
        --argjson hard_wall "$HARD_WALL_SECONDS" \
        --argjson window_ticks "$HOLDOUT_WINDOW_TICKS" \
        --argjson speed "$GAME_SPEED_EXPECTED" \
        --arg jev_model "$JEV_MODEL" \
        --argjson lab_only "$LAB_ONLY" \
        --arg map_gen "$MAP_GEN" \
        --arg server_settings "$SERVER_SETTINGS" \
        --argjson peaceful "$peaceful" \
        --arg factorio_bin "$FACTORIO_BIN_RESOLVED" \
        --arg mod_version "$MOD_VERSION" \
        --arg mod_tree_sha256 "$MOD_TREE_SHA256" \
        --arg git_rev "$GIT_REV" \
        --arg git_porcelain_sha256 "$GIT_PORCELAIN_SHA256" \
        --argjson git_dirty_entries "${GIT_DIRTY_ENTRIES:-0}" \
        --arg git_diff_sha256 "$GIT_DIFF_SHA256" \
        --arg buddy_sha256 "$(file_sha256 "$BUDDY_BIN")" \
        --arg mcp_sha256 "$(file_sha256 "$MCP_BIN")" \
        --arg factorioctl_sha256 "$(file_sha256 "$CLI_BIN")" \
        --argjson rcon_port "$RCON_PORT" \
        --argjson game_port "$GAME_PORT" \
        --arg agent "$AGENT_ID" \
        --argjson sample_radius "$SAMPLE_RADIUS" \
        --arg started_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
        --arg fixture_case "$FIXTURE_CASE" \
        --arg layout "$LAYOUT" \
        --argjson fixture_version "$FIXTURE_VERSION" \
        --argjson fixture_only "$([[ "$FIXTURE_ONLY" == 1 ]] && echo true || echo false)" \
        '{
            arm:$arm, scenario:$scenario, seed:$seed, repeat:$repeat, trial_dir:$trial_dir,
            started_at:$started_at, lab_only:$lab_only, fixture_only:$fixture_only,
            fixture_case:(if $fixture_case == "" then null else $fixture_case end),
            layout:(if $layout == "" then null else $layout end),
            fixture_version:(if $fixture_case == "" then null else $fixture_version end),
            planner:{model:$model, effort:$effort, heartbeat_seconds:$heartbeat,
                     turn_timeout_seconds:$turn_timeout},
            decision:{mode:$decision_mode,
                      model:(if ($decision_mode == "jev" or $decision_mode == "jev-shadow") then $jev_model else null end)},
            budget:{max_autonomous_turns:$max_turns, autonomy_deadline_seconds:$deadline,
                    hard_wall_seconds:$hard_wall},
            holdout:{windows:2, window_ticks:$window_ticks},
            game:{speed_expected:$speed, map_gen_settings:$map_gen, peaceful_mode:$peaceful,
                  server_settings:$server_settings, factorio_bin:$factorio_bin,
                  factorio_version:null, loaded_mods:null, observed_game_speed:null},
            mod:{version:$mod_version, tree_sha256:$mod_tree_sha256},
            source:{git_rev:$git_rev, git_status_porcelain_sha256:$git_porcelain_sha256,
                    git_dirty_entries:$git_dirty_entries, git_diff_head_sha256:$git_diff_sha256,
                    buddy_sha256:$buddy_sha256, mcp_sha256:$mcp_sha256,
                    factorioctl_sha256:$factorioctl_sha256},
            runtime:{agent:$agent, rcon_port:$rcon_port, game_port:$game_port,
                     sample_radius:$sample_radius, connected_players_expected:0},
            fixture:null
        }' > "$TRIAL/manifest.json"
}

update_manifest() {
    local filter="$1"
    shift
    local tmp="$TRIAL/.manifest.tmp"
    jq "$@" "$filter" "$TRIAL/manifest.json" > "$tmp" && mv -f -- "$tmp" "$TRIAL/manifest.json"
}

prepare_trial_home() {
    # Buddy installs the mod into HOME/.factorio/mods for a graphical client;
    # an identical private copy keeps that install away from the real HOME.
    cp -a "$MOD_SOURCE" "$TRIAL/home/.factorio/mods/"
    cp -a "$MOD_SOURCE" "$WRITE_DATA/mods/"
}

prepare_tracker() {
    # A separate git repository stops bd from discovering any enclosing
    # repository; BEADS_*/BD_* routing variables are excluded by env -i.
    git -C "$TRACKER" init -q
    (cd "$TRACKER" && env -i "PATH=$PATH" "HOME=$HOME" \
        bd init --non-interactive --skip-agents --skip-hooks -q -p evaltrial) \
        > "$TRIAL/tracker-init.log" 2>&1 \
        || fail_trial tracker_init_failed "bd init failed for the disposable tracker (see tracker-init.log)"
    [[ -d "$TRACKER/.beads" ]] || fail_trial tracker_init_failed "bd init did not create $TRACKER/.beads"
}

write_claude_wrapper() {
    local wrapper="$TRIAL/bin/claude"
    if [[ "$PLANNER" == "noop" ]]; then
        # Component replay: a planner that never touches the world, so the
        # prepared fuel defect survives until Buddy's maintenance check.
        {
            printf '#!/usr/bin/env bash\n'
            printf 'capture=%q\n' "$TRIAL/claude-stream.jsonl"
            cat <<'STUB'
session="noop-${RANDOM}${RANDOM}"
{
    printf '{"type":"system","subtype":"init","model":"noop-stub","session_id":"%s"}\n' "$session"
    printf '{"type":"result","subtype":"success","is_error":false,"result":"noop planner turn","session_id":"%s","total_cost_usd":0}\n' "$session"
} | tee -a "$capture"
STUB
        } > "$wrapper"
        chmod 700 "$wrapper"
        : > "$TRIAL/claude-stream.jsonl"
        return
    fi
    {
        printf '#!/usr/bin/env bash\n'
        printf '# Generated by tests/autonomous_eval.sh: restore the real HOME only for\n'
        printf '# Claude authentication and capture stream-JSON stdout unmodified.\n'
        printf 'export HOME=%q\n' "$HOME"
        if [[ -n "${CLAUDE_CONFIG_DIR:-}" ]]; then
            printf 'export CLAUDE_CONFIG_DIR=%q\n' "$CLAUDE_CONFIG_DIR"
        fi
        printf 'unset TYPESAFE_API_KEY FACTORIOCTL_ALLOW_RAW_LUA FACTORIO_RCON_PASSWORD\n'
        # exec keeps the PID Buddy spawned, so signals, cancellation and the
        # exit status belong to the real Claude process; tee only duplicates
        # the bytes into the trial capture.
        printf 'exec %q "$@" > >(exec tee -a %q)\n' "$REAL_CLAUDE" "$TRIAL/claude-stream.jsonl"
    } > "$wrapper"
    chmod 700 "$wrapper"
    : > "$TRIAL/claude-stream.jsonl"
}

# Raw Lua for operator fixtures and read-only assertions. The opt-in and the
# RCON credential are scoped to this single factorioctl process.
raw_lua() {
    FACTORIO_RCON_PASSWORD="$RCON_SECRET" FACTORIOCTL_ALLOW_RAW_LUA=1 \
        timeout --signal=KILL 60s \
        "$CLI_BIN" --host 127.0.0.1 --port "$RCON_PORT" exec "$1"
}

# A fresh save refuses the first /c command until it is repeated; probe twice
# with an idempotent command (same pattern as live_regressions.sh).
enable_raw_lua() {
    local marker="factorio-buddy-eval-raw-lua-ready"
    local output
    raw_lua "rcon.print('$marker')" >/dev/null 2>&1 || true
    output="$(raw_lua "rcon.print('$marker')" 2>/dev/null)" || return 1
    [[ "$output" == *"$marker"* ]]
}

take_sample() {
    local name="$1"
    local output
    output="$(raw_lua "rcon.print(remote.call('claude_interface', 'evaluation_sample', '$AGENT_ID', $SAMPLE_RADIUS))" 2>&1)" || {
        printf '%s\n' "$output" > "$SAMPLES/$name.error.txt"
        return 1
    }
    if ! jq -e '.success == true and (.tick | type) == "number"' <<< "$output" >/dev/null 2>&1; then
        printf '%s\n' "$output" > "$SAMPLES/$name.error.txt"
        return 1
    fi
    printf '%s\n' "$output" > "$SAMPLES/$name.json"
}

game_tick() {
    local output
    output="$(raw_lua "rcon.print(game.tick)" 2>/dev/null)" || return 1
    [[ "$output" =~ ^[0-9]+$ ]] || return 1
    printf '%s\n' "$output"
}

poll_players() {
    local output
    local count
    if output="$(raw_lua "rcon.print(remote.call('claude_interface', 'connected_player_count_result'))" 2>/dev/null)" \
        && count="$(jq -er '.count | numbers' <<< "$output" 2>/dev/null)"; then
        jq -cn --argjson unix_ms "$(unix_ms)" --argjson count "$count" \
            '{unix_ms:$unix_ms, count:$count}' >> "$SAMPLES/players.jsonl"
        if (( count > 0 )); then
            invariant_failure contamination_connected_players "observed $count connected player(s) during the evaluation"
        fi
    else
        MONITOR_ERRORS=$((MONITOR_ERRORS + 1))
    fi
}

stop_fixture_server() {
    [[ -n "$FIXTURE_PID" ]] || return 0
    local signal
    for signal in INT TERM; do
        if process_active "$FIXTURE_PID"; then
            kill "-$signal" "$FIXTURE_PID" 2>/dev/null || true
            if wait_for_process_stop "$FIXTURE_PID" 30; then
                printf '%s\n' "$signal" > "$TRIAL/fixture-stop-signal"
                break
            fi
        fi
    done
    if process_active "$FIXTURE_PID"; then
        kill -KILL "$FIXTURE_PID" 2>/dev/null || true
        wait "$FIXTURE_PID" 2>/dev/null || true
        FIXTURE_PID=""
        return 1
    fi
    wait "$FIXTURE_PID" 2>/dev/null || true
    FIXTURE_PID=""
}

# ---------------------------------------------------------------------------
# Lab-only durable-fuel fixtures (fuel-repair scenario)
#
# Each case is built by trusted raw Lua on a disposable server before Buddy's
# first turn, checked against its stated premise through the shipped
# diagnose_fuel_sustainability remote and one repair_fuel_sustainability
# dry-run through the MCP binary, then saved. Cases:
#   missing-feed             cold coal drill + ore drill feeding a furnace, no
#                            durable fuel anywhere; materials for the repair
#   already-sustainable      closed coal loop already feeding an ore drill;
#                            correct behaviour is no repair and no teardown
#   insufficient-materials   missing-feed layout, NPC carries only coal
#   blocked-route            live coal trunk and an unfed ore drill across a
#                            water channel; the feed must detour
#   mixed-source-belt        the trunk tiles next to an unfed ore drill carry
#                            iron ore on one lane; only coal may be tapped
#   stale-consumer           unfed ore drill that the runner replaces (new
#                            unit_number) after the first repair preview
#   failed-unchanged-repair  idle furnace (no ore): the repair dry-runs ready
#                            but execution fails and rolls back every time
#   competing-consumers      two unfed ore drills, inserters for one feed
# The held-out layout rotates the same case a quarter turn, moves it and
# changes its obstacle geometry.
# ---------------------------------------------------------------------------

fixture_description() {
    case "$FIXTURE_CASE" in
        missing-feed) printf 'cold burner coal drill on a 4x4 coal patch; burner iron-ore drill feeding a stone furnace, both on a finite manual coal buffer; NPC carries 20 coal, 32 belts, 4 burner inserters' ;;
        already-sustainable) printf 'self-fueling burner coal loop whose live trunk already feeds a burner iron-ore drill through a burner inserter; NPC carries spare belts, inserters and coal' ;;
        insufficient-materials) printf 'missing-feed layout; the NPC carries only 20 coal (no belts, no inserters)' ;;
        blocked-route) printf 'self-fueling coal loop trunk and an unfed burner iron-ore drill separated by a water channel with one open end' ;;
        mixed-source-belt) printf 'self-fueling coal loop trunk whose tiles next to an unfed burner iron-ore drill carry iron ore on one lane' ;;
        stale-consumer) printf 'self-fueling coal loop trunk and an unfed burner iron-ore drill that was already rebuilt once (stale unit_number recorded) and is replaced again after the first observed repair preview' ;;
        failed-unchanged-repair) printf 'self-fueling coal loop trunk and an idle stone furnace with no ore and no fuel' ;;
        competing-consumers) printf 'self-fueling coal loop trunk and two unfed burner iron-ore drills; the NPC carries burner inserters for exactly one feed' ;;
    esac
}

fixture_build_lua() {
    printf "local FIXTURE_AGENT, FIXTURE_CASE, FIXTURE_LAYOUT = '%s', '%s', '%s'\n" "$AGENT_ID" "$FIXTURE_CASE" "$LAYOUT"
    cat <<'LUA'
-- Lab-only durable-fuel fixtures for tests/autonomous_eval.sh. Trusted raw Lua
-- (operator fixture setup only). Every case is built in a local frame that the
-- held-out layout rotates a quarter turn and shifts, so the same case is tested
-- with different positions, orientation and obstacle geometry.
local agent, case, layout = FIXTURE_AGENT, FIXTURE_CASE, FIXTURE_LAYOUT
remote.call('claude_interface', 'register_agent', agent, agent)
local placed = helpers.json_to_table(remote.call('claude_interface', 'pre_place_character_result', agent, 'nauvis', 0))
local c = remote.call('claude_interface', 'get_character', agent)
if not (c and c.valid) then
    rcon.print(helpers.table_to_json({error = 'agent character missing', placed = placed}))
    return
end
local s, force = c.surface, c.force
local heldout = layout == 'heldout'
local rot = heldout and 1 or 0
local ox = math.floor(c.position.x) + (heldout and -8 or 6)
local oy = math.floor(c.position.y) + 6
local order = {'north', 'east', 'south', 'west'}
local function P(x, y)
    for _ = 1, rot do x, y = -y, x end
    return {x = ox + x, y = oy + y}
end
local function D(name)
    for i, v in ipairs(order) do
        if v == name then return defines.direction[order[((i - 1 + rot) % 4) + 1]] end
    end
end
local function box(x1, y1, x2, y2)
    local a, b = P(x1, y1), P(x2, y2)
    return {{math.min(a.x, b.x), math.min(a.y, b.y)}, {math.max(a.x, b.x), math.max(a.y, b.y)}}
end
local failure = nil
local function fail(message) failure = failure or message end
local function make(name, x, y, dir)
    local spec = {name = name, position = P(x, y), force = force}
    if dir then spec.direction = D(dir) end
    local e = s.create_entity(spec)
    if not e then fail(name .. ' creation failed at local ' .. x .. ',' .. y) end
    return e
end
local function resource(name, x1, y1, x2, y2, amount)
    for x = x1, x2 do
        for y = y1, y2 do
            s.create_entity{name = name, position = P(x + 0.5, y + 0.5), amount = amount or 100000}
        end
    end
    return {item = name, area = box(x1, y1, x2 + 1, y2 + 1)}
end
local function set_tiles(name, x1, y1, x2, y2)
    local list = {}
    for x = x1, x2 do
        for y = y1, y2 do
            local p = P(x + 0.5, y + 0.5)
            list[#list + 1] = {name = name, position = {math.floor(p.x), math.floor(p.y)}}
        end
    end
    s.set_tiles(list)
end
local function status_name(e)
    if not (e and e.valid) then return nil end
    for name, value in pairs(defines.entity_status) do
        if value == e.status then return name end
    end
    return nil
end

-- missing-feed keeps the original fixture's cleared extent exactly.
local legacy = case == 'missing-feed' or case == 'insufficient-materials'
local area = legacy and box(0, 0, 24, 14) or box(-2, -8, 30, 24)
s.request_to_generate_chunks({(area[1][1] + area[2][1]) / 2, (area[1][2] + area[2][2]) / 2}, 3)
s.force_generate_chunk_requests()
for _, e in pairs(s.find_entities_filtered{area = area}) do
    if e.valid and e.type ~= 'character' then e.destroy() end
end
local tiles = {}
for x = area[1][1], area[2][1] - 1 do
    for y = area[1][2], area[2][2] - 1 do
        tiles[#tiles + 1] = {name = 'grass-1', position = {x, y}}
    end
end
s.set_tiles(tiles)

local targets, units, live_units = {}, {}, {}
local function keep(role, e)
    if e and e.valid then
        units[#units + 1] = {role = role, unit_number = e.unit_number, name = e.name, position = e.position}
        live_units[e.unit_number] = e
    end
end
-- drop_target is only resolved once the source first outputs, so compare the
-- drop position with the destination's selection box (what the game uses).
local function drops_into(src, dst)
    if not (src and src.valid and dst and dst.valid) then return false end
    local p, bb = src.drop_position, dst.selection_box
    return p.x >= bb.left_top.x and p.x <= bb.right_bottom.x and p.y >= bb.left_top.y and p.y <= bb.right_bottom.y
end
local function target(role, e, extra)
    if not (e and e.valid) then return end
    local t = {role = role, unit_number = e.unit_number, name = e.name, type = e.type,
        position = e.position, primary = false}
    for k, v in pairs(extra or {}) do t[k] = v end
    targets[#targets + 1] = t
    keep(role, e)
end

-- Fill one belt's lanes (false leaves a lane untouched).
local function stock_belt(b, lane1_item, lane2_item)
    if not (b and b.valid) then return 0 end
    local n = 0
    for lane, item in ipairs({lane1_item or false, lane2_item or false}) do
        if item then
            local line = b.get_transport_line(lane)
            for p = 0, line.line_length, 0.25 do
                if line.can_insert_at(p) and line.insert_at(p, {name = item, count = 1}) then n = n + 1 end
            end
        end
    end
    return n
end

-- A closed, self-fueling burner coal loop: drill -> belt, burner inserter from
-- the belt back into the drill, and the belt continuing south as a live trunk
-- that is already backed up with coal.
local function coal_loop(trunk_length, mixed_from)
    local patch = resource('coal', 2, 2, 5, 5)
    local drill = make('burner-mining-drill', 4, 4, 'east')
    local belts = {make('transport-belt', 5.5, 3.5, 'east'), make('transport-belt', 6.5, 3.5, 'south')}
    for i = 1, trunk_length do
        belts[#belts + 1] = make('transport-belt', 6.5, 3.5 + i, 'south')
    end
    local feeder = make('burner-inserter', 5.5, 4.5, 'east')
    if drill then drill.get_fuel_inventory().insert{name = 'coal', count = 5} end
    if feeder then feeder.get_fuel_inventory().insert{name = 'coal', count = 1} end
    for i, b in ipairs(belts) do
        if mixed_from and i >= mixed_from then stock_belt(b, 'coal', 'iron-ore') else stock_belt(b, 'coal', 'coal') end
    end
    target('loop_drill', drill, {patch = patch})
    target('loop_inserter', feeder)
    for i, b in ipairs(belts) do keep('loop_belt_' .. i, b) end
    return {drill = drill, belts = belts, feeder = feeder, terminal_y = 3.5 + trunk_length}
end

local checks, expected, extra = {}, {}, {}

local function stone_furnace(x, y, ore, fuel)
    local f = make('stone-furnace', x, y)
    if f then
        if ore and ore > 0 then f.get_inventory(defines.inventory.furnace_source).insert{name = 'iron-ore', count = ore} end
        if fuel and fuel > 0 then f.get_fuel_inventory().insert{name = 'coal', count = fuel} end
    end
    return f
end

-- A burner drill on its own 2x2 iron-ore patch emptying into an iron chest.
-- It burns fuel continuously for far longer than a trial plus holdout, so its
-- mined amount measures real fuel delivery in every holdout window.
local function ore_drill_target(role, x, y, dir, fuel)
    local patch = resource('iron-ore', x - 1, y - 1, x, y)
    local drill = make('burner-mining-drill', x, y, dir)
    local chest
    if drill then
        drill.get_fuel_inventory().clear()
        if fuel and fuel > 0 then drill.get_fuel_inventory().insert{name = 'coal', count = fuel} end
        chest = s.create_entity{name = 'iron-chest', position = drill.drop_position, force = force}
        if not chest then fail(role .. ' output chest creation failed') end
    end
    target(role, drill, {primary = true, patch = patch, output_chest = chest and chest.unit_number or nil})
    keep(role .. '_chest', chest)
    checks[role .. '_outputs_to_chest'] = drops_into(drill, chest)
    return drill
end

local inventory = {}
local function give(items)
    local inv = c.get_main_inventory()
    inv.clear()
    for name, count in pairs(items) do
        inventory[name] = inv.insert{name = name, count = count}
    end
end

if case == 'missing-feed' or case == 'insufficient-materials' then
    local coal_patch = resource('coal', 2, 2, 5, 5)
    local coal_drill = make('burner-mining-drill', 4, 4, 'east')
    if coal_drill then coal_drill.get_fuel_inventory().clear() end
    local ore_patch = resource('iron-ore', 16, 2, 17, 3)
    local ore_drill = make('burner-mining-drill', 17, 3, 'south')
    local furnace = stone_furnace(18, 5, 0, 2)
    if ore_drill then ore_drill.get_fuel_inventory().insert{name = 'coal', count = 2} end
    -- The cold coal drill is the source to establish; the ore drill and the
    -- furnace it feeds are the production consumers whose fuel is measured.
    target('cold_coal_drill', coal_drill, {patch = coal_patch})
    target('ore_drill', ore_drill, {primary = true, patch = ore_patch})
    target('furnace', furnace, {primary = true})
    checks.cold_drill_empty = coal_drill ~= nil and coal_drill.get_fuel_inventory().get_item_count() == 0
    checks.ore_drill_feeds_furnace = drops_into(ore_drill, furnace)
    checks.coal_tiles = s.count_entities_filtered{name = 'coal', area = area} == 16
    checks.ore_tiles = s.count_entities_filtered{name = 'iron-ore', area = area} == 4
    if case == 'missing-feed' then
        give({coal = 20, ['transport-belt'] = 32, ['burner-inserter'] = 4})
        expected = {
            repair_expected = true,
            dry_run = {success = true, selected_role = 'cold_coal_drill'},
            success = 'the ore drill or the furnace it feeds gains a durable coal feed and produces in both holdout windows',
        }
    else
        give({coal = 20})
        expected = {
            repair_expected = false,
            dry_run = {success = false, preflight_ready = false},
            success = 'no repair success is claimed unless a durable feed is actually verified; the NPC may only succeed after acquiring materials',
        }
    end
elseif case == 'already-sustainable' then
    local loop = coal_loop(6 + (heldout and 2 or 0))
    local fy = heldout and 9 or 8
    local feeder = make('burner-inserter', 7.5, fy - 0.5, 'west')
    if feeder then feeder.get_fuel_inventory().insert{name = 'coal', count = 1} end
    local drill = ore_drill_target('fed_drill', 9, fy, 'east', 5)
    target('fed_drill_inserter', feeder)
    give({coal = 10, ['transport-belt'] = 20, ['burner-inserter'] = 2})
    checks.drill_fed_by_trunk = drops_into(feeder, drill)
    expected = {
        repair_expected = false,
        dry_run = {success = false, error_kind = 'no_ready_fuel_transaction'},
        success = 'every original fixture entity survives in place, every burner target stays durably fed, and the fed drill mines in both holdout windows',
    }
elseif case == 'blocked-route' then
    local loop = coal_loop(4)
    -- A water channel between the trunk and the consumer. Primary leaves the
    -- detour open at the south end; held-out leaves it open at the north end.
    if heldout then set_tiles('water', 12, -3, 13, 23) else set_tiles('water', 13, -8, 14, 15) end
    local tx, ty = heldout and 23 or 22, heldout and 9 or 6
    ore_drill_target('target_drill', tx, ty, 'east', 0)
    give({coal = 20, ['transport-belt'] = 100, ['burner-inserter'] = 4})
    local blocked = 0
    for x = 7, tx - 2 do
        local p = P(x + 0.5, ty - 0.5)
        if not s.can_place_entity{name = 'transport-belt', position = p, force = force} then blocked = blocked + 1 end
    end
    checks.direct_line_blocked = blocked > 0
    extra.direct_line_blocked_tiles = blocked
    expected = {
        repair_expected = true,
        dry_run = {success = true, selected_role = 'target_drill'},
        success = 'the target drill gains a durable feed routed around the water channel and mines in both holdout windows',
    }
elseif case == 'mixed-source-belt' then
    local trunk = 6 + (heldout and 2 or 0)
    -- Iron ore on lane 2 of the trunk tiles downstream of the loop feeder's
    -- pickup. Nothing removes it, so the belt stays mixed next to the target.
    local loop = coal_loop(trunk, 4)
    local dy = trunk + 1
    local drill = heldout and ore_drill_target('target_drill', 4, dy, 'west', 0)
        or ore_drill_target('target_drill', 9, dy, 'east', 0)
    give({coal = 20, ['transport-belt'] = 40, ['burner-inserter'] = 4})
    local mixed_tiles, ore_on_belt = 0, 0
    for i = 4, #loop.belts do
        local b = loop.belts[i]
        local coal, ore = 0, 0
        for lane = 1, 2 do
            coal = coal + b.get_transport_line(lane).get_item_count('coal')
            ore = ore + b.get_transport_line(lane).get_item_count('iron-ore')
        end
        ore_on_belt = ore_on_belt + ore
        if coal > 0 and ore > 0 then mixed_tiles = mixed_tiles + 1 end
    end
    checks.mixed_tiles_near_target = mixed_tiles >= 2
    extra.mixed_tiles = mixed_tiles
    extra.ore_on_belt = ore_on_belt
    expected = {
        repair_expected = true,
        dry_run = {success = true, selected_role = 'target_drill'},
        success = 'the target drill is fed only coal from the mixed belt (no inserter jammed holding ore) and mines in both holdout windows',
    }
elseif case == 'stale-consumer' then
    local loop = coal_loop(4)
    local tx, ty, dir = heldout and 3 or 11, heldout and 10 or 8, heldout and 'west' or 'east'
    local first = make('burner-mining-drill', tx, ty, dir)
    local stale_unit = first and first.unit_number or nil
    if first then first.destroy() end
    local drill = ore_drill_target('target_drill', tx, ty, dir, 0)
    for _, t in ipairs(targets) do if t.role == 'target_drill' then t.replaceable = true end end
    give({coal = 20, ['transport-belt'] = 40, ['burner-inserter'] = 4})
    checks.replaced_identity = stale_unit ~= nil and drill ~= nil and drill.unit_number ~= stale_unit
    extra.stale_unit_number = stale_unit
    expected = {
        repair_expected = true,
        dry_run = {success = true, selected_role = 'target_drill'},
        stale_unit_number = stale_unit,
        runtime_perturbation = 'after the first observed repair preview the runner replaces the target drill once (same position, direction and fuel; new unit_number)',
        success = 'no repair success is claimed for a replaced unit_number and the live replacement drill ends durably fed and mining in both holdout windows',
    }
elseif case == 'failed-unchanged-repair' then
    local loop = coal_loop(4)
    local tx, ty = heldout and 3 or 11, heldout and 10 or 7
    -- Idle consumer: no ore and no fuel. A durable feed can be built, but the
    -- controller cannot verify production, so execution fails and rolls back
    -- until the world changes (for example the NPC supplies ore).
    local furnace = stone_furnace(tx, ty, 0, 0)
    target('idle_furnace', furnace, {primary = true})
    give({coal = 20, ['transport-belt'] = 40, ['burner-inserter'] = 4})
    checks.furnace_idle = furnace ~= nil
        and furnace.get_inventory(defines.inventory.furnace_source).is_empty()
        and furnace.get_fuel_inventory().is_empty()
    expected = {
        repair_expected = 'fails_until_world_changes',
        dry_run = {success = true, selected_role = 'idle_furnace'},
        execution = {success = false, error_kind = 'target_production_not_verified'},
        success = 'the unchanged failing repair is executed at most once; the furnace may only succeed after the NPC changes the world (supplies ore)',
    }
elseif case == 'competing-consumers' then
    local loop = coal_loop(6)
    ore_drill_target('drill_a', 10, 6, 'east', 0)
    if heldout then
        ore_drill_target('drill_b', 3, 10, 'west', 0)
    else
        ore_drill_target('drill_b', 10, 11, 'east', 0)
    end
    -- Each feed here needs a filtered source tap plus a terminal inserter, so
    -- two burner inserters (with ample belts and coal) fund exactly one feed.
    give({coal = 20, ['transport-belt'] = 30, ['burner-inserter'] = 2})
    expected = {
        repair_expected = 'exactly_one',
        dry_run = {success = true},
        success = 'one drill gains a durable feed and mines in both windows; no success is claimed for the second without materials',
    }
else
    rcon.print(helpers.table_to_json({error = 'unknown fixture case ' .. tostring(case)}))
    return
end

for _, t in ipairs(targets) do
    local e = live_units[t.unit_number]
    t.status = status_name(e)
    t.fuel = e and e.get_fuel_inventory() and e.get_fuel_inventory().get_item_count() or nil
end
for name, ok in pairs(checks) do
    if not ok then fail('fixture check failed: ' .. name) end
end
local a1, a2 = area[1], area[2]
rcon.print(helpers.table_to_json({
    error = failure,
    case = case,
    layout = layout,
    rotation_quarter_turns = rot,
    origin = {x = ox, y = oy},
    placed = placed,
    character_position = c.position,
    area = {x1 = a1[1], y1 = a1[2], x2 = a2[1], y2 = a2[2]},
    targets = targets,
    units = units,
    inventory = inventory,
    checks = checks,
    expected = expected,
    details = extra,
    lab_only = true,
}))
LUA
}

# $1: JSON with area, targets (current identities) and units.
fixture_readback_lua() {
    printf "local READBACK_FIXTURE, READBACK_AGENT = [==[%s]==], '%s'\n" "$1" "$AGENT_ID"
    cat <<'LUA'
-- Read-only fixture readback: per-target state, per-drill mined amount from its
-- own isolated resource patch, and survival of every original fixture entity.
local fx, agent = helpers.json_to_table(READBACK_FIXTURE), READBACK_AGENT
local s = game.surfaces.nauvis
local a = fx.area
local margin = 16
local by_unit = {}
for _, e in pairs(s.find_entities_filtered{area = {{a.x1 - margin, a.y1 - margin}, {a.x2 + margin, a.y2 + margin}}}) do
    if e.unit_number then by_unit[e.unit_number] = e end
end
local function status_name(e)
    for name, value in pairs(defines.entity_status) do
        if value == e.status then return name end
    end
    return nil
end
local function inside(p, bb)
    return p.x >= bb.left_top.x and p.x <= bb.right_bottom.x and p.y >= bb.left_top.y and p.y <= bb.right_bottom.y
end
local targets = {}
for _, t in ipairs(fx.targets or {}) do
    local e = by_unit[t.unit_number]
    local r = {role = t.role, unit_number = t.unit_number, valid = e ~= nil}
    if e then
        r.position = e.position
        r.status = status_name(e)
        local fuel_inv = e.get_fuel_inventory()
        r.fuel_count = fuel_inv and fuel_inv.get_item_count() or nil
        r.remaining_burning_fuel = e.burner and e.burner.remaining_burning_fuel or nil
        if e.type == 'furnace' or e.type == 'assembling-machine' then r.products_finished = e.products_finished end
        local feeders = {}
        for _, ins in pairs(s.find_entities_filtered{type = 'inserter', area = {{e.position.x - 3, e.position.y - 3}, {e.position.x + 3, e.position.y + 3}}}) do
            if inside(ins.drop_position, e.selection_box) then
                feeders[#feeders + 1] = {
                    unit_number = ins.unit_number,
                    status = status_name(ins),
                    held_item = ins.held_stack.valid_for_read and ins.held_stack.name or nil,
                }
            end
        end
        r.feeding_inserters = feeders
    end
    if t.patch then
        local pa = t.patch.area
        local amount, drills = 0, 0
        for _, res in pairs(s.find_entities_filtered{area = pa, name = t.patch.item}) do amount = amount + res.amount end
        for _, d in pairs(s.find_entities_filtered{type = 'mining-drill', area = {{pa[1][1] - 1.5, pa[1][2] - 1.5}, {pa[2][1] + 1.5, pa[2][2] + 1.5}}}) do
            if d.unit_number ~= t.unit_number then drills = drills + 1 end
        end
        r.patch_amount = amount
        r.other_drills_on_patch = drills
    end
    if t.output_chest then
        local chest = by_unit[t.output_chest]
        r.output_count = chest and chest.get_item_count() or nil
    end
    targets[#targets + 1] = r
end
local missing, moved = {}, {}
for _, u in ipairs(fx.units or {}) do
    local e = by_unit[u.unit_number]
    if not e then
        missing[#missing + 1] = {role = u.role, unit_number = u.unit_number}
    elseif math.abs(e.position.x - u.position.x) > 0.01 or math.abs(e.position.y - u.position.y) > 0.01 then
        moved[#moved + 1] = {role = u.role, unit_number = u.unit_number}
    end
end
local c = remote.call('claude_interface', 'get_character', agent)
local inventory = {}
if c and c.valid then
    for _, item in pairs(c.get_main_inventory().get_contents()) do
        inventory[item.name] = (inventory[item.name] or 0) + item.count
    end
end
rcon.print(helpers.table_to_json({
    success = true,
    tick = game.tick,
    targets = targets,
    units_missing = missing,
    units_moved = moved,
    character_inventory = inventory,
    area_counts = {
        belts = s.count_entities_filtered{type = 'transport-belt', area = {{a.x1, a.y1}, {a.x2, a.y2}}},
        inserters = s.count_entities_filtered{type = 'inserter', area = {{a.x1, a.y1}, {a.x2, a.y2}}},
        ground_items = s.count_entities_filtered{type = 'item-entity', area = {{a.x1, a.y1}, {a.x2, a.y2}}},
    },
}))
LUA
}

# $1: area JSON, $2: unit_number to replace.
fixture_replace_lua() {
    printf "local REPLACE_AREA, REPLACE_UNIT = helpers.json_to_table([==[%s]==]), %d\n" "$1" "$2"
    cat <<'LUA'
-- Stale-consumer perturbation (lab-only): replace one burner consumer with an
-- identical entity at the same position and direction, carrying over its
-- inventories, so only its unit_number changes.
local a, old_unit = REPLACE_AREA, REPLACE_UNIT
local s = game.surfaces.nauvis
local old
for _, e in pairs(s.find_entities_filtered{area = {{a.x1 - 8, a.y1 - 8}, {a.x2 + 8, a.y2 + 8}}, type = {'furnace', 'mining-drill'}}) do
    if e.unit_number == old_unit then old = e break end
end
if not old then
    rcon.print(helpers.table_to_json({success = false, error = 'target not found', old_unit_number = old_unit}))
    return
end
local name, position, direction, force = old.name, old.position, old.direction, old.force
local saved = {}
local kinds = {defines.inventory.fuel}
if old.type == 'furnace' then
    kinds[#kinds + 1] = defines.inventory.furnace_source
    kinds[#kinds + 1] = defines.inventory.furnace_result
end
for _, kind in ipairs(kinds) do
    local inv = old.get_inventory(kind)
    saved[kind] = inv and inv.get_contents() or {}
end
old.destroy()
local new = s.create_entity{name = name, position = position, direction = direction, force = force}
if not new then
    rcon.print(helpers.table_to_json({success = false, error = 'replacement creation failed', old_unit_number = old_unit}))
    return
end
for kind, contents in pairs(saved) do
    local inv = new.get_inventory(kind)
    for _, item in pairs(contents) do inv.insert{name = item.name, count = item.count, quality = item.quality} end
end
rcon.print(helpers.table_to_json({
    success = true,
    tick = game.tick,
    old_unit_number = old_unit,
    new_unit_number = new.unit_number,
    name = name,
    position = new.position,
    fuel = new.get_fuel_inventory().get_item_count(),
}))
LUA
}

fixture_readback() {
    local out="$1"
    local spec
    spec="$(jq -c --slurpfile current "$TRIAL/fixture-targets.json" \
        '{area, targets: $current[0], units}' "$TRIAL/fixture.json")"
    raw_lua "$(fixture_readback_lua "$spec")" > "$out" 2>&1 \
        && jq -e '.success == true' "$out" >/dev/null 2>&1
}

fixture_diagnose() {
    local out="$1"
    local x1 y1 x2 y2
    read -r x1 y1 x2 y2 < <(jq -r '.area | "\(.x1 - 16) \(.y1 - 16) \(.x2 + 16) \(.y2 + 16)"' "$TRIAL/fixture.json")
    raw_lua "rcon.print(remote.call('claude_interface', 'diagnose_fuel_sustainability', $x1, $y1, $x2, $y2, 30, '$AGENT_ID'))" \
        > "$out" 2>&1 && jq -e '.consumers | type == "array"' "$out" >/dev/null 2>&1
}

# Replace the case's replaceable target (stale-consumer) and record the new
# identity in fixture-targets.json. Prints the Lua result.
replace_fixture_target() {
    local role unit area result new_unit
    role="$(jq -r 'map(select(.replaceable == true)) | first | .role // empty' "$TRIAL/fixture-targets.json")"
    unit="$(jq -r 'map(select(.replaceable == true)) | first | .unit_number // empty' "$TRIAL/fixture-targets.json")"
    [[ -n "$role" && -n "$unit" ]] || { printf '{"success":false,"error":"no replaceable target"}\n'; return 1; }
    area="$(jq -c '.area' "$TRIAL/fixture.json")"
    result="$(raw_lua "$(fixture_replace_lua "$area" "$unit")" 2>&1)" || true
    if ! new_unit="$(jq -er 'select(.success == true) | .new_unit_number' <<< "$result" 2>/dev/null)"; then
        printf '%s\n' "$result"
        return 1
    fi
    jq --arg role "$role" --argjson unit "$new_unit" \
        'map(if .role == $role then .unit_number = $unit else . end)' \
        "$TRIAL/fixture-targets.json" > "$TRIAL/.fixture-targets.tmp" \
        && mv -f -- "$TRIAL/.fixture-targets.tmp" "$TRIAL/fixture-targets.json"
    jq -c --arg role "$role" '. + {role: $role}' <<< "$result"
}

# Minimal MCP stdio client for the fixture server (same JSON-RPC exchange as
# tests/live_regressions.sh). The MCP process gets only the fixture RCON
# credential, the agent id and the trial tracker root.
mcp_start() {
    coproc EVAL_MCP {
        exec env -i "PATH=$PATH" "HOME=$TRIAL/home" \
            "FACTORIO_RCON_HOST=127.0.0.1" "FACTORIO_RCON_PORT=$RCON_PORT" \
            "FACTORIO_RCON_PASSWORD=$RCON_SECRET" "FACTORIO_AGENT_ID=$AGENT_ID" \
            "FACTORIO_BUDDY_PROJECT_ROOT=$TRACKER" \
            "$MCP_BIN" 2>> "$TRIAL/fixture-mcp.stderr"
    }
    MCP_PID="$EVAL_MCP_PID"
    MCP_OUT_FD="${EVAL_MCP[0]}"
    MCP_IN_FD="${EVAL_MCP[1]}"
    MCP_NEXT_ID=1
    mcp_request initialize \
        '{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"factorio-buddy-autonomous-eval","version":"1"}}' \
        30 "$TRIAL/.mcp-response" || return 1
    jq -e '.result.serverInfo != null' "$TRIAL/.mcp-response" >/dev/null 2>&1 || return 1
    jq -cn '{jsonrpc:"2.0", method:"notifications/initialized", params:{}}' >&"$MCP_IN_FD"
}

# Send one JSON-RPC request and write its response line to $4. Runs in the
# calling shell (never in a command substitution) so request ids stay unique.
mcp_request() {
    local method="$1"
    local params="$2"
    local timeout_seconds="$3"
    local out="$4"
    local id="$MCP_NEXT_ID"
    local line
    local deadline=$((SECONDS + timeout_seconds))
    MCP_NEXT_ID=$((MCP_NEXT_ID + 1))
    : > "$out"
    jq -cn --argjson id "$id" --arg method "$method" --argjson params "$params" \
        '{jsonrpc:"2.0", id:$id, method:$method, params:$params}' >&"$MCP_IN_FD" || return 1
    while (( SECONDS < deadline )); do
        IFS= read -r -t "$((deadline - SECONDS + 1))" -u "$MCP_OUT_FD" line || return 1
        if jq -e --argjson id "$id" '.id == $id' >/dev/null 2>&1 <<< "$line"; then
            printf '%s\n' "$line" > "$out"
            return 0
        fi
    done
    return 1
}

mcp_stop() {
    if [[ -n "${MCP_IN_FD:-}" ]]; then
        { exec {MCP_IN_FD}>&-; } 2>/dev/null || true
        MCP_IN_FD=""
    fi
    if [[ -n "${MCP_OUT_FD:-}" ]]; then
        { exec {MCP_OUT_FD}<&-; } 2>/dev/null || true
        MCP_OUT_FD=""
    fi
    if [[ -n "${MCP_PID:-}" ]]; then
        kill "$MCP_PID" 2>/dev/null || true
        wait "$MCP_PID" 2>/dev/null || true
        MCP_PID=""
    fi
}

# Call repair_fuel_sustainability exactly as Buddy's maintenance loop does and
# write the tool's JSON payload to $2.
mcp_repair() {
    local dry_run="$1"
    local out="$2"
    local timeout_seconds="$3"
    local response="$TRIAL/.mcp-response"
    mcp_request tools/call \
        "$(jq -cn --argjson dry "$dry_run" '{name:"repair_fuel_sustainability", arguments:{radius:64, limit:20, dry_run:$dry}}')" \
        "$timeout_seconds" "$response" \
        || { printf '{"runner_error":"no MCP response within %s s"}\n' "$timeout_seconds" > "$out"; return 1; }
    jq -r '.result.content[0].text // empty' "$response" > "$out"
    if ! jq -e . "$out" >/dev/null 2>&1; then
        jq -c '{runner_error:"non-JSON tool payload", response:.}' "$response" > "$out"
        return 1
    fi
    jq -c --argjson is_error "$(jq '.result.isError // false' "$response")" '. + {mcp_is_error:$is_error}' "$out" \
        > "$out.tmp" && mv -f -- "$out.tmp" "$out"
}

PROBES_FILE=""
record_probe() {
    local name="$1"
    local ok="$2"
    local detail="$3"
    [[ "$ok" == true ]] || ok=false
    jq -e . >/dev/null 2>&1 <<< "$detail" || detail="$(jq -cn --arg raw "$detail" '{unparsed:$raw}')"
    jq -cn --arg name "$name" --argjson ok "$ok" --argjson detail "$detail" \
        '{name:$name, ok:$ok, detail:$detail}' >> "$PROBES_FILE"
    [[ "$ok" == true ]] || log "fixture probe failed: $name"
}

# jq: summary of one repair payload with the selected consumer's fixture role.
repair_view() {
    jq -c --slurpfile current "$TRIAL/fixture-targets.json" --slurpfile fx "$TRIAL/fixture.json" '
        (($current[0] + $fx[0].targets) | map({key: (.unit_number | tostring), value: .role}) | from_entries) as $roles
        | {success, dry_run, error_kind: (.error_kind // null), runner_error: (.runner_error // null),
           selected_unit: (.selected_transaction.consumer_unit_number // null),
           selected_role: ($roles[(.selected_transaction.consumer_unit_number // 0) | tostring] // null),
           repair_error_kind: (.repair.error_kind // null), preflight_ready: .repair.preflight.ready,
           materials: (.repair.preflight.routes.materials // null), bootstrap_fuel: (.repair.preflight.bootstrap_fuel // null),
           route_new_belts: (.repair.route.new_belt_count // null),
           tap_allowed_items: (.repair.source_tap.allowed_items // null),
           terminal_filters: ([.repair.inserter.filter.filters[]?.name]),
           rollback_success: .repair.rollback.success,
           next_action: (.next_action.type // null)}' "$1" 2>/dev/null || printf 'null\n'
}

structure_view() {
    jq -c '{area_counts, character_inventory, units_missing: (.units_missing | length), units_moved: (.units_moved | length)}' \
        "$1" 2>/dev/null || printf 'null\n'
}

area_counts_view() {
    jq -c '.area_counts // null' "$1" 2>/dev/null || printf 'null\n'
}

# Wait for Factorio to mine at least one item from a target's own patch.
target_mined_after() {
    local role="$1"
    local before="$2"
    local after="$3"
    jq -n --arg role "$role" --slurpfile a "$before" --slurpfile b "$after" '
        ($a[0].targets | map(select(.role == $role)) | first) as $x
        | ($b[0].targets | map(select(.role == $role)) | first) as $y
        | {role: $role, mined: (if ($x.patch_amount | type) == "number" and ($y.patch_amount | type) == "number"
                then $x.patch_amount - $y.patch_amount else null end),
           status: $y.status, fuel_count: $y.fuel_count,
           feeders_holding_other_items: [($y.feeding_inserters // [])[] | select(.held_item != null and .held_item != "coal") | .unit_number]}'
}

# Fixture-only case probes: exercise the shipped controller through MCP on the
# saved fixture to show that each case's stated expectation is reachable.
run_fixture_probes() {
    local p="$TRIAL/fixture-probes"
    mkdir -p "$p"
    PROBES_FILE="$TRIAL/.fixture-probes.jsonl"
    : > "$PROBES_FILE"
    local view before after mined replaced
    case "$FIXTURE_CASE" in
        missing-feed)
            mcp_repair false "$p/exec.json" "$MCP_EXEC_TIMEOUT_SECONDS" || true
            view="$(repair_view "$p/exec.json")"
            record_probe "repair executes the provisional cold-coal loop" \
                "$(jq '.success == true and .selected_role == "cold_coal_drill"' <<< "$view")" "$view"
            ;;
        insufficient-materials|already-sustainable)
            fixture_readback "$p/before.json" || true
            mcp_repair false "$p/exec.json" "$MCP_EXEC_TIMEOUT_SECONDS" || true
            fixture_readback "$p/after.json" || true
            view="$(repair_view "$p/exec.json")"
            before="$(structure_view "$p/before.json" 2>/dev/null || echo null)"
            after="$(structure_view "$p/after.json" 2>/dev/null || echo null)"
            record_probe "execution refuses without mutating the world or the NPC inventory" \
                "$(jq -n --argjson v "$view" --argjson b "$before" --argjson a "$after" --arg case "$FIXTURE_CASE" \
                    '$v.success == false and $b != null and $b == $a
                     and ($case != "already-sustainable" or $v.error_kind == "no_ready_fuel_transaction")')" \
                "$(jq -cn --argjson v "$view" --argjson b "$before" --argjson a "$after" '{repair:$v, before:$b, after:$a}')"
            ;;
        blocked-route|mixed-source-belt)
            fixture_readback "$p/before.json" || true
            mcp_repair false "$p/exec.json" "$MCP_EXEC_TIMEOUT_SECONDS" || true
            view="$(repair_view "$p/exec.json")"
            sleep 20
            fixture_readback "$p/after.json" || true
            mined="$(target_mined_after target_drill "$p/before.json" "$p/after.json" 2>/dev/null || echo null)"
            record_probe "repair executes and the target drill then mines on durable coal" \
                "$(jq -n --argjson v "$view" --argjson m "$mined" --arg case "$FIXTURE_CASE" \
                    '$v.success == true and $v.selected_role == "target_drill" and ($m.mined // 0) > 0
                     and ($m.feeders_holding_other_items | length) == 0
                     and ($case != "mixed-source-belt" or ($v.tap_allowed_items == ["coal"] and $v.terminal_filters == ["coal"]))')" \
                "$(jq -cn --argjson v "$view" --argjson m "$mined" '{repair:$v, target:$m}')"
            ;;
        stale-consumer)
            replaced="$(replace_fixture_target)" || true
            printf '%s\n' "$replaced" > "$p/replacement.json"
            mcp_repair true "$p/dry-after-replacement.json" 120 || true
            view="$(repair_view "$p/dry-after-replacement.json")"
            record_probe "after replacement the preview selects the live unit, not a stale one" \
                "$(jq -n --argjson v "$view" --argjson r "$replaced" --slurpfile fx "$TRIAL/fixture.json" \
                    '$r.success == true and $v.success == true and $v.selected_unit == $r.new_unit_number
                     and $v.selected_unit != $r.old_unit_number and $v.selected_unit != $fx[0].expected.stale_unit_number')" \
                "$(jq -cn --argjson v "$view" --argjson r "$replaced" '{preview:$v, replacement:$r}')"
            fixture_readback "$p/before.json" || true
            mcp_repair false "$p/exec.json" "$MCP_EXEC_TIMEOUT_SECONDS" || true
            view="$(repair_view "$p/exec.json")"
            sleep 20
            fixture_readback "$p/after.json" || true
            mined="$(target_mined_after target_drill "$p/before.json" "$p/after.json" 2>/dev/null || echo null)"
            record_probe "repair of the replacement drill executes and it then mines" \
                "$(jq -n --argjson v "$view" --argjson m "$mined" --argjson r "$replaced" \
                    '$v.success == true and $v.selected_unit == $r.new_unit_number and ($m.mined // 0) > 0')" \
                "$(jq -cn --argjson v "$view" --argjson m "$mined" '{repair:$v, target:$m}')"
            ;;
        failed-unchanged-repair)
            local attempt views=() structures=()
            fixture_readback "$p/before.json" || true
            structures+=("$(area_counts_view "$p/before.json")")
            for attempt in 1 2; do
                mcp_repair false "$p/exec-$attempt.json" "$MCP_EXEC_TIMEOUT_SECONDS" || true
                views+=("$(repair_view "$p/exec-$attempt.json")")
                fixture_readback "$p/after-$attempt.json" || true
                structures+=("$(area_counts_view "$p/after-$attempt.json")")
            done
            record_probe "the ready repair fails the same way twice and rolls back each time" \
                "$(jq -n --argjson a "${views[0]}" --argjson b "${views[1]}" \
                    --argjson s0 "${structures[0]}" --argjson s1 "${structures[1]}" --argjson s2 "${structures[2]}" \
                    '[$a, $b] | all(.success == false and .selected_role == "idle_furnace"
                        and .repair_error_kind == "target_production_not_verified" and .rollback_success == true)
                     and $s0 != null and $s0.belts == $s1.belts and $s1.belts == $s2.belts
                     and $s0.inserters == $s1.inserters and $s1.inserters == $s2.inserters')" \
                "$(jq -cn --argjson a "${views[0]}" --argjson b "${views[1]}" \
                    --argjson s0 "${structures[0]}" --argjson s1 "${structures[1]}" --argjson s2 "${structures[2]}" \
                    '{attempts:[$a, $b], area_counts:[$s0, $s1, $s2]}')"
            ;;
        competing-consumers)
            mcp_repair false "$p/exec.json" "$MCP_EXEC_TIMEOUT_SECONDS" || true
            view="$(repair_view "$p/exec.json")"
            mcp_repair true "$p/dry-second.json" 120 || true
            local second
            second="$(repair_view "$p/dry-second.json")"
            record_probe "one feed is built; the second consumer is then refused for lack of inserters" \
                "$(jq -n --argjson v "$view" --argjson s "$second" \
                    '$v.success == true and ($v.selected_role == "drill_a" or $v.selected_role == "drill_b")
                     and $s.success == false and $s.preflight_ready == false
                     and ($s.selected_role == "drill_a" or $s.selected_role == "drill_b") and $s.selected_role != $v.selected_role
                     and ($s.materials["burner-inserter"].sufficient == false)')" \
                "$(jq -cn --argjson v "$view" --argjson s "$second" '{first:$v, second_preview:$s}')"
            ;;
    esac
}

prepare_fuel_fixture() {
    local run="$TRIAL/fixture-run"
    local server_log="$run/factorio-current.log"
    local created_sha
    local fixture
    local saves_before
    local saves_after
    local deadline
    local live=false
    local premise_view
    local premise_ok
    local world_before
    local world_after
    local validation_ok=true

    mkdir -p "$run/saves"
    printf '[path]\nread-data=%s\nwrite-data=%s\n\n[other]\ncheck-updates=false\n' \
        "$FACTORIO_DATA_ROOT/data" "$run" > "$run/config.ini"

    log "creating fixture save ($FIXTURE_CASE/$LAYOUT, seed $SEED, $MAP_GEN)"
    env -i "PATH=$PATH" "HOME=$TRIAL/home" \
        "$FACTORIO_BIN_RESOLVED" \
            --config "$run/config.ini" \
            --mod-directory "$WRITE_DATA/mods" \
            --create "$SAVE" \
            --map-gen-settings "$MAP_GEN" \
            --map-gen-seed "$SEED" \
            < /dev/null > "$TRIAL/fixture-create.log" 2>&1 || true
    [[ -f "$SAVE" ]] || fail_trial fixture_failed "Factorio did not create the fixture save (see fixture-create.log)"
    created_sha="$(file_sha256 "$SAVE")"

    FIXTURE_SECRET="$(od -An -tx1 -N32 /dev/urandom | tr -d ' \n')"
    RCON_SECRET="$FIXTURE_SECRET"
    env -i "PATH=$PATH" "HOME=$TRIAL/home" \
        "$FACTORIO_BIN_RESOLVED" \
            --config "$run/config.ini" \
            --mod-directory "$WRITE_DATA/mods" \
            --start-server "$SAVE" \
            --rcon-bind "127.0.0.1:$RCON_PORT" \
            --rcon-password "$RCON_SECRET" \
            --port "$GAME_PORT" \
            --server-settings "$SERVER_SETTINGS" \
            < /dev/null > "$TRIAL/fixture-server.stdout" 2>&1 &
    FIXTURE_PID=$!

    deadline=$((SECONDS + 120))
    until raw_lua "rcon.print('ready')" >/dev/null 2>&1; do
        process_active "$FIXTURE_PID" || fail_trial fixture_failed "fixture Factorio server exited during startup"
        (( SECONDS < deadline )) || fail_trial fixture_failed "fixture Factorio server did not open RCON within 120 s"
        sleep 1
    done
    enable_raw_lua || fail_trial fixture_failed "fixture server refused trusted raw-Lua setup"

    fixture="$(raw_lua "$(fixture_build_lua)" 2>&1)" \
        || fail_trial fixture_failed "fixture construction command failed: $fixture"
    printf '%s\n' "$fixture" > "$TRIAL/fixture.json"
    jq -e --arg case "$FIXTURE_CASE" --arg layout "$LAYOUT" \
        '.error == null and .case == $case and .layout == $layout
         and (.targets | length) > 0 and all(.targets[]; (.unit_number | type) == "number")
         and ([.checks[]] | all)' \
        "$TRIAL/fixture.json" >/dev/null 2>&1 \
        || fail_trial fixture_failed "fixture build checks failed (see fixture.json)"
    jq '.targets' "$TRIAL/fixture.json" > "$TRIAL/fixture-targets.json"

    # Let the prepared loops run until their durable topology is live.
    deadline=$((SECONDS + 45))
    while (( SECONDS < deadline )); do
        if fixture_diagnose "$SAMPLES/fixture-diagnose.json" \
            && jq -e --slurpfile fx "$TRIAL/fixture.json" '
                .consumers as $consumers
                | ($fx[0].case) as $case
                | [$fx[0].targets[] | select(.role == "loop_drill" or .role == "loop_inserter"
                    or ($case == "already-sustainable")) | .unit_number] as $need
                | all($need[]; . as $u | any($consumers[]; .unit_number == $u and .automated == true and .issue == null))' \
                "$SAMPLES/fixture-diagnose.json" >/dev/null 2>&1; then
            live=true
            break
        fi
        sleep 2
    done
    [[ "$live" == true ]] || fail_trial fixture_failed "prepared durable topology did not become live within 45 s (see samples/fixture-diagnose.json)"

    # Premise: the shipped controller's dry-run must see the case as stated,
    # and must not change the world.
    fixture_readback "$SAMPLES/fixture-readback.json" \
        || fail_trial fixture_failed "fixture readback failed (see samples/fixture-readback.json)"
    mcp_start || fail_trial fixture_failed "MCP binary did not initialise against the fixture server"
    mcp_repair true "$TRIAL/fixture-dryrun.json" 120 \
        || fail_trial fixture_failed "repair_fuel_sustainability dry-run returned no payload (see fixture-dryrun.json)"
    fixture_readback "$SAMPLES/fixture-readback-after-dryrun.json" \
        || fail_trial fixture_failed "fixture readback after the dry-run failed"
    premise_view="$(repair_view "$TRIAL/fixture-dryrun.json")"
    premise_ok="$(jq -n --argjson v "$premise_view" --slurpfile fx "$TRIAL/fixture.json" '
        $fx[0].expected.dry_run as $d
        | ($v.success == $d.success)
          and (($d.selected_role // null) == null or $v.selected_role == $d.selected_role)
          and (($d.error_kind // null) == null or $v.error_kind == $d.error_kind)
          and (if ($d | has("preflight_ready")) then $v.preflight_ready == $d.preflight_ready else true end)')"
    world_before="$(structure_view "$SAMPLES/fixture-readback.json")"
    world_after="$(structure_view "$SAMPLES/fixture-readback-after-dryrun.json")"
    PROBES_FILE="$TRIAL/.fixture-probes.jsonl"
    : > "$PROBES_FILE"
    record_probe "controller dry-run matches the case premise" "$premise_ok" "$premise_view"
    record_probe "controller dry-run leaves the world and the NPC inventory unchanged" \
        "$(jq -n --argjson b "$world_before" --argjson a "$world_after" '$b == $a')" \
        "$(jq -cn --argjson b "$world_before" --argjson a "$world_after" '{before:$b, after:$a}')"

    take_sample fixture || fail_trial fixture_failed "evaluation_sample failed on the fixture server"

    saves_before="$(grep -c 'Saving finished' "$server_log" 2>/dev/null || true)"
    raw_lua "game.server_save()" >/dev/null 2>&1 \
        || fail_trial fixture_failed "game.server_save() was rejected"
    deadline=$((SECONDS + 60))
    saves_after="$saves_before"
    while (( SECONDS < deadline )); do
        saves_after="$(grep -c 'Saving finished' "$server_log" 2>/dev/null || true)"
        (( saves_after > saves_before )) && break
        sleep 0.5
    done
    (( saves_after > saves_before )) || fail_trial fixture_failed "fixture save did not complete within 60 s"
    cp -f -- "$SAVE" "$TRIAL/.fixture-pristine.zip"

    if (( FIXTURE_ONLY )); then
        local premise_probes
        premise_probes="$(< "$PROBES_FILE")"
        run_fixture_probes
        printf '%s\n' "$premise_probes" | cat - "$PROBES_FILE" > "$TRIAL/.fixture-probes.all" \
            && mv -f -- "$TRIAL/.fixture-probes.all" "$PROBES_FILE"
    fi
    mcp_stop
    RCON_SECRET=""
    stop_fixture_server || fail_trial fixture_failed "fixture server ignored SIGINT/SIGTERM and was killed"
    # The server saves again on exit; keep exactly the validated pre-probe save.
    mv -f -- "$TRIAL/.fixture-pristine.zip" "$SAVE"
    [[ "$(file_sha256 "$SAVE")" != "$created_sha" ]] \
        || fail_trial fixture_failed "fixture save was not persisted (save digest unchanged)"
    wait_for_ports_free 30 || fail_trial port_conflict "fixture server ports were not released"
    cp -f -- "$server_log" "$TRIAL/fixture-server.log" 2>/dev/null || true

    jq -s --slurpfile fx "$TRIAL/fixture.json" --argjson fixture_only "$([[ "$FIXTURE_ONLY" == 1 ]] && echo true || echo false)" \
        '{case: $fx[0].case, layout: $fx[0].layout, expected: $fx[0].expected, fixture_only: $fixture_only,
          probes: ., ok: (map(.ok) | all)}' "$PROBES_FILE" > "$TRIAL/fixture-validation.json"
    rm -f -- "$PROBES_FILE"
    jq -e '.ok == true' "$TRIAL/fixture-validation.json" >/dev/null 2>&1 || validation_ok=false

    update_manifest '.fixture = {
            lab_only: true,
            case: $fx[0].case,
            layout: $fx[0].layout,
            version: $version,
            description: $description,
            expected: $fx[0].expected,
            targets: $fx[0].targets,
            inventory: $fx[0].inventory,
            area: $fx[0].area,
            rotation_quarter_turns: $fx[0].rotation_quarter_turns,
            validation_ok: $validation_ok,
            explicit_save_observed: true,
            stop_signal: $signal,
            buddy_resume: "Buddy starts without --fresh on this save; no ownership sidecar exists yet, so Buddy records a new owned run"
        }' \
        --slurpfile fx "$TRIAL/fixture.json" \
        --argjson version "$FIXTURE_VERSION" \
        --arg description "$(fixture_description)" \
        --argjson validation_ok "$validation_ok" \
        --arg signal "$(cat "$TRIAL/fixture-stop-signal" 2>/dev/null || echo unknown)"
    [[ "$validation_ok" == true ]] \
        || fail_trial fixture_failed "fixture validation failed (see fixture-validation.json)"
    log "fixture $FIXTURE_CASE/$LAYOUT prepared, validated and saved"
}

start_buddy() {
    local fresh_args=()
    local buddy_env=(
        "PATH=$TRIAL/bin:$PATH"
        "HOME=$TRIAL/home"
        "USER=${USER:-$(id -un)}"
        "LOGNAME=${LOGNAME:-${USER:-$(id -un)}}"
        "LANG=${LANG:-C.UTF-8}"
        "RUST_LOG=info"
    )
    if [[ "$SCENARIO" == "open-play" && -z "$CONTINUE_FROM" ]]; then
        fresh_args=(--fresh)
    fi
    if [[ "$ARM" == jev-shadow || "$ARM" == jev ]]; then
        buddy_env+=("TYPESAFE_API_KEY=$TYPESAFE_API_KEY")
    fi

    BUDDY_START_MS="$(unix_ms)"
    BUDDY_START_SECONDS=$SECONDS
    STOP_REASON="startup"
    # env -i drops FACTORIO_*, BUDDY_*, MODEL, BEADS_*, ANTHROPIC_* and every
    # other operator setting so only the explicit arguments below apply.
    (
        cd "$TRIAL/work" && exec env -i "${buddy_env[@]}" \
            "$BUDDY_BIN" \
                --start-server \
                "${fresh_args[@]}" \
                --map-seed "$SEED" \
                --model "$PLANNER_MODEL" \
                --effort "$EFFORT" \
                --heartbeat-seconds "$HEARTBEAT_SECONDS" \
                --turn-timeout-seconds "$TURN_TIMEOUT_SECONDS" \
                --agent "$AGENT_ID" \
                --rcon-host 127.0.0.1 \
                --rcon-port "$RCON_PORT" \
                --game-port "$GAME_PORT" \
                --factorio-bin "$FACTORIO_BIN_RESOLVED" \
                --write-data "$WRITE_DATA" \
                --save "$SAVE" \
                --mcp-bin "$MCP_BIN" \
                --issue-project-root "$TRACKER" \
                --decision-mode "$DECISION_MODE" \
                --max-autonomous-turns "$MAX_AUTONOMOUS_TURNS" \
                --autonomy-deadline-seconds "$AUTONOMY_DEADLINE_SECONDS" \
                --evidence-log "$EVIDENCE"
    ) < /dev/null > "$BUDDY_LOG" 2>&1 &
    BUDDY_PID=$!
    log "Buddy started (pid $BUDDY_PID, decision mode $DECISION_MODE)"

    wait_for_log "$BUDDY_PID" "$BUDDY_LOG" "Factorio buddy online" "$ONLINE_TIMEOUT_SECONDS" \
        || { STOP_REASON="startup_failed"; fail_trial buddy_start_failed "Buddy did not reach the online state (see buddy.log)"; }
    [[ -s "$WRITE_DATA/rcon-password" ]] \
        || { STOP_REASON="startup_failed"; fail_trial buddy_start_failed "Buddy did not write its managed RCON password file"; }
    RCON_SECRET="$(tr -d '\r\n' < "$WRITE_DATA/rcon-password")"
    STOP_REASON="running"
}

take_before_sample() {
    enable_raw_lua || invariant_failure sample_failed "Buddy's server refused trusted raw-Lua sampling"
    if take_sample before; then
        BEFORE_TICK="$(jq '.tick' "$SAMPLES/before.json")"
    else
        invariant_failure sample_failed "before-sample failed"
    fi
    OBSERVED_GAME_SPEED="$(raw_lua "rcon.print(game.speed)" 2>/dev/null || echo null)"
    [[ "$OBSERVED_GAME_SPEED" =~ ^[0-9.]+$ ]] || OBSERVED_GAME_SPEED="null"
    if [[ "$OBSERVED_GAME_SPEED" == "null" ]] \
        || ! jq -en --argjson speed "$OBSERVED_GAME_SPEED" --argjson expected "$GAME_SPEED_EXPECTED" \
            '$speed == $expected' >/dev/null; then
        invariant_failure game_speed "observed game speed $OBSERVED_GAME_SPEED, expected $GAME_SPEED_EXPECTED"
    fi
    poll_players
}

verify_issue_root() {
    local config="$WRITE_DATA/mcp-$AGENT_ID.json"
    local configured
    local expected
    expected="$(readlink -f -- "$TRACKER")"
    configured="$(jq -r '.mcpServers.factorio.env.FACTORIO_BUDDY_PROJECT_ROOT // empty' "$config" 2>/dev/null || true)"
    printf '%s\n' "$configured" > "$TRIAL/mcp-issue-project-root"
    if [[ -z "$configured" || "$(readlink -f -- "$configured" 2>/dev/null)" != "$expected" ]]; then
        invariant_failure issue_root_not_isolated "MCP FACTORIO_BUDDY_PROJECT_ROOT is '$configured', expected $expected"
    fi
}

budget_exhausted_event() {
    [[ -f "$EVIDENCE" ]] || return 1
    jq -R -c -n 'first(inputs | fromjson? | select(type == "object" and .event == "autonomy_budget_exhausted"))' \
        "$EVIDENCE" 2>/dev/null
}

# stale-consumer: the first repair preview observed in Buddy's evidence (a
# maintenance decision with a preview, or a model dry-run of
# repair_fuel_sustainability) triggers one replacement of the target consumer.
stale_perturbation_check() {
    (( ! PERTURBATION_DONE )) || return 0
    [[ "$FIXTURE_CASE" == "stale-consumer" && -f "$EVIDENCE" ]] || return 0
    local trigger result
    trigger="$(jq -R -c -n 'first(inputs | fromjson? | select(type == "object")
        | select((.event == "decision" and .preview != null)
            or (.event == "tool_outcome" and ((.tool // "") | endswith("repair_fuel_sustainability"))
                and ((.arguments.dry_run // false) == true))))' "$EVIDENCE" 2>/dev/null || true)"
    [[ -n "$trigger" ]] || return 0
    PERTURBATION_DONE=1
    result="$(replace_fixture_target)" || true
    jq -n --argjson trigger "$trigger" --arg result "$result" --argjson unix_ms "$(unix_ms)" \
        '($result | fromjson? // {success:false, error:$result}) + {trigger:$trigger, unix_ms:$unix_ms}' \
        > "$SAMPLES/perturbation.json"
    if jq -e '.success == true' "$SAMPLES/perturbation.json" >/dev/null 2>&1; then
        log "stale-consumer perturbation: replaced unit $(jq -r '.old_unit_number' "$SAMPLES/perturbation.json") with $(jq -r '.new_unit_number' "$SAMPLES/perturbation.json")"
    else
        invariant_failure perturbation_failed "stale-consumer replacement failed (see samples/perturbation.json)"
    fi
}

monitor_until_budget() {
    local event
    local next_poll=0
    while :; do
        if ! process_active "$BUDDY_PID"; then
            STOP_REASON="buddy_exited"
            HOLDOUT_REASON="Buddy exited before its autonomy budget was exhausted"
            invariant_failure buddy_exited "Buddy exited before its autonomy budget was exhausted"
            return
        fi
        stale_perturbation_check
        event="$(budget_exhausted_event || true)"
        if [[ -n "$event" ]]; then
            STOP_REASON="autonomy_budget_exhausted:$(jq -r '.reason // "unknown"' <<< "$event")"
            log "autonomy budget exhausted ($STOP_REASON)"
            return
        fi
        if (( SECONDS - BUDDY_START_SECONDS >= HARD_WALL_SECONDS )); then
            STOP_REASON="hard_wall_limit"
            HOLDOUT_REASON="autonomy budget exhaustion was never observed, so a mutation-free holdout could not be guaranteed"
            invariant_failure hard_wall_limit "Buddy did not report autonomy_budget_exhausted within $HARD_WALL_SECONDS s"
            return
        fi
        if (( SECONDS >= next_poll )); then
            poll_players
            next_poll=$((SECONDS + MONITOR_INTERVAL_SECONDS))
        fi
        sleep 2
    done
}

wait_for_tick() {
    local target="$1"
    local tick
    local last_tick=-1
    local last_change=$SECONDS
    local next_poll=$((SECONDS + MONITOR_INTERVAL_SECONDS))
    while :; do
        process_active "$BUDDY_PID" || { HOLDOUT_REASON="Buddy exited during holdout"; return 1; }
        if tick="$(game_tick)"; then
            (( tick >= target )) && return 0
            if (( tick != last_tick )); then
                last_tick=$tick
                last_change=$SECONDS
            fi
        fi
        if (( SECONDS - last_change >= TICK_STALL_SECONDS )); then
            HOLDOUT_REASON="game tick did not advance for $TICK_STALL_SECONDS s"
            return 1
        fi
        if (( SECONDS >= next_poll )); then
            poll_players
            next_poll=$((SECONDS + MONITOR_INTERVAL_SECONDS))
        fi
        sleep 2
    done
}

# For fuel-repair trials, take the fixture readback right after each sample.
holdout_sample() {
    local index="$1"
    take_sample "holdout-$index" || return 1
    if [[ "$SCENARIO" == "fuel-repair" ]]; then
        fixture_readback "$SAMPLES/fixture-holdout-$index.json" \
            || invariant_failure sample_failed "fixture readback at holdout-$index failed"
    fi
}

run_holdout() {
    local start_tick
    local mid_tick
    local diagnosis_file="$SAMPLES/holdout-diagnose.json"
    log "holdout: two windows of $HOLDOUT_WINDOW_TICKS ticks with model mutations stopped"
    holdout_sample 0 || { HOLDOUT_REASON="holdout-0 sample failed"; invariant_failure holdout_failed "$HOLDOUT_REASON"; return; }
    start_tick="$(jq '.tick' "$SAMPLES/holdout-0.json")"
    STOP_TICK="$start_tick"
    wait_for_tick $((start_tick + HOLDOUT_WINDOW_TICKS)) || { invariant_failure holdout_failed "$HOLDOUT_REASON"; return; }
    holdout_sample 1 || { HOLDOUT_REASON="holdout-1 sample failed"; invariant_failure holdout_failed "$HOLDOUT_REASON"; return; }
    mid_tick="$(jq '.tick' "$SAMPLES/holdout-1.json")"
    wait_for_tick $((mid_tick + HOLDOUT_WINDOW_TICKS)) || { invariant_failure holdout_failed "$HOLDOUT_REASON"; return; }
    holdout_sample 2 || { HOLDOUT_REASON="holdout-2 sample failed"; invariant_failure holdout_failed "$HOLDOUT_REASON"; return; }

    jq -n --slurpfile a "$SAMPLES/holdout-0.json" --slurpfile b "$SAMPLES/holdout-1.json" \
        "$(jq_window_program)" > "$SAMPLES/window-1.json"
    jq -n --slurpfile a "$SAMPLES/holdout-1.json" --slurpfile b "$SAMPLES/holdout-2.json" \
        "$(jq_window_program)" > "$SAMPLES/window-2.json"

    printf 'null\n' > "$diagnosis_file"
    if [[ "$SCENARIO" == "fuel-repair" ]]; then
        fixture_diagnose "$diagnosis_file" \
            || { invariant_failure sample_failed "holdout-end fuel diagnosis failed"; printf 'null\n' > "$diagnosis_file"; }
    fi

    jq -n \
        --slurpfile w1 "$SAMPLES/window-1.json" \
        --slurpfile w2 "$SAMPLES/window-2.json" \
        --argjson window_ticks "$HOLDOUT_WINDOW_TICKS" \
        '{evaluated:true, window_ticks:$window_ticks, windows:[$w1[0], $w2[0]],
          character_inventory_changed:($w1[0].character_inventory_changed or $w2[0].character_inventory_changed),
          character_moved:($w1[0].character_moved or $w2[0].character_moved)}' \
        > "$TRIAL/holdout.json"
    jq -n \
        --slurpfile holdout "$TRIAL/holdout.json" \
        "\$holdout[0].windows as \$w | $(jq_milestones_program)" \
        > "$TRIAL/milestones.json"
    HOLDOUT_REASON=""
    log "holdout complete"
}

shutdown_buddy() {
    (( SHUTDOWN_DONE )) && return 0
    SHUTDOWN_DONE=1
    [[ -n "$BUDDY_PID" ]] || return 0
    if process_active "$BUDDY_PID"; then
        log "stopping Buddy with SIGTERM (up to $SHUTDOWN_GRACE_SECONDS s)"
        kill -TERM "$BUDDY_PID" 2>/dev/null || true
        if ! wait_for_process_stop "$BUDDY_PID" "$SHUTDOWN_GRACE_SECONDS"; then
            SHUTDOWN_FORCED=true
            kill -KILL "$BUDDY_PID" 2>/dev/null || true
            invariant_failure forced_kill "Buddy did not exit within $SHUTDOWN_GRACE_SECONDS s of SIGTERM and was killed"
        fi
    fi
    if wait "$BUDDY_PID" 2>/dev/null; then
        BUDDY_EXIT_STATUS=0
    else
        BUDDY_EXIT_STATUS=$?
    fi
    BUDDY_PID=""
    BUDDY_STOP_MS="$(unix_ms)"
    (( BUDDY_EXIT_STATUS == 0 )) \
        || invariant_failure unclean_shutdown "Buddy exited with status $BUDDY_EXIT_STATUS"
    local pid
    if ! no_owned_server "$TRIAL"; then
        invariant_failure owned_server_remaining "the trial's Factorio server or its listeners survived Buddy shutdown"
        if pid="$(find_owned_server_pid "$TRIAL")"; then
            kill -KILL "$pid" 2>/dev/null || true
        fi
    fi
    if ! jq -e '.version == 2 and .clean_shutdown == true' "$SAVE.buddy-owner.json" >/dev/null 2>&1; then
        invariant_failure unclean_shutdown "save ownership sidecar does not record clean_shutdown=true"
    fi
}

collect_server_logs() {
    local run_dir
    local latest=""
    mkdir -p "$TRIAL/server-logs"
    for run_dir in "$WRITE_DATA"/managed-runs/*/; do
        [[ -d "$run_dir" ]] || continue
        local run_id
        run_id="$(basename -- "$run_dir")"
        if [[ -f "$run_dir/factorio-current.log" ]]; then
            cp -f -- "$run_dir/factorio-current.log" "$TRIAL/server-logs/$run_id.log"
            latest="$run_dir/factorio-current.log"
        fi
        if [[ -f "$run_dir/factorio-previous.log" ]]; then
            cp -f -- "$run_dir/factorio-previous.log" "$TRIAL/server-logs/$run_id.previous.log"
        fi
    done
    if [[ -n "$latest" ]]; then
        cp -f -- "$latest" "$TRIAL/server.log"
    fi
}

# Remove the only password-bearing Buddy files after the server is gone and
# prove that no retained artifact contains a credential. Text artifacts that
# echo a credential (for example a log that records a command line) are
# redacted in place without passing the secret through any argv; a credential
# in a binary artifact cannot be redacted and fails the trial.
scrub_secrets() {
    local secrets=()
    local leaked
    local file
    local secret
    local content
    [[ -n "$RCON_SECRET" ]] && secrets+=("$RCON_SECRET")
    [[ -n "$FIXTURE_SECRET" ]] && secrets+=("$FIXTURE_SECRET")
    if [[ -s "$WRITE_DATA/rcon-password" ]]; then
        secrets+=("$(tr -d '\r\n' < "$WRITE_DATA/rcon-password")")
    fi
    if [[ "$ARM" == jev-shadow || "$ARM" == jev ]] && [[ -n "${TYPESAFE_API_KEY:-}" ]]; then
        secrets+=("$TYPESAFE_API_KEY")
    fi
    rm -f -- "$WRITE_DATA/rcon-password" "$WRITE_DATA/mcp-$AGENT_ID.json"
    RCON_SECRET=""
    FIXTURE_SECRET=""
    (( ${#secrets[@]} > 0 )) || return 0
    leaked="$(grep -rlF -f <(printf '%s\n' "${secrets[@]}") -- "$TRIAL" 2>/dev/null || true)"
    [[ -n "$leaked" ]] || return 0
    while IFS= read -r file; do
        [[ -f "$file" ]] || continue
        if grep -Iq . "$file" 2>/dev/null; then
            content="$(< "$file")"
            for secret in "${secrets[@]}"; do
                content="${content//"$secret"/[REDACTED]}"
            done
            printf '%s\n' "$content" > "$file"
            printf '%s\n' "${file#"$TRIAL"/}" >> "$TRIAL/credential-redactions.txt"
        else
            invariant_failure credential_leak "binary artifact ${file#"$TRIAL"/} contains a credential"
        fi
    done <<< "$leaked"
}

finalize_trial() {
    local status=$?
    trap - EXIT TERM INT
    set +e
    [[ "$STOP_REASON" == "running" ]] && STOP_REASON="aborted"
    if [[ -n "$BUDDY_PID" ]]; then
        SHUTDOWN_DONE=0
        shutdown_buddy
    fi
    mcp_stop
    if [[ -n "$FIXTURE_PID" ]]; then
        stop_fixture_server || invariant_failure fixture_failed "fixture server had to be killed during cleanup"
    fi
    local pid
    if pid="$(find_owned_server_pid "$TRIAL")"; then
        invariant_failure owned_server_remaining "a Factorio server for this trial was still running at cleanup"
        kill -KILL "$pid" 2>/dev/null
    fi
    collect_server_logs
    if [[ ! -f "$TRIAL/fixture-server.log" && -f "$TRIAL/fixture-run/factorio-current.log" ]]; then
        cp -f -- "$TRIAL/fixture-run/factorio-current.log" "$TRIAL/fixture-server.log"
    fi
    local version_log="$TRIAL/server.log"
    [[ -f "$version_log" ]] || version_log="$TRIAL/fixture-server.log"
    update_manifest '.game.factorio_version = $version | .game.loaded_mods = $mods | .game.observed_game_speed = $speed' \
        --arg version "$(grep -m1 -o 'Factorio [0-9][0-9.]* ([^)]*)' "$version_log" 2>/dev/null || true)" \
        --argjson mods "$(grep -o 'Loading mod [^ ]* [0-9][0-9.]*' "$version_log" 2>/dev/null \
            | awk '{ print $3 " " $4 }' | sort -u | jq -R . | jq -sc .)" \
        --argjson speed "${OBSERVED_GAME_SPEED:-null}"
    if [[ -d "$WRITE_DATA/mods/claude-interface" ]] \
        && [[ "$(tree_sha256 "$WRITE_DATA/mods/claude-interface")" != "$MOD_TREE_SHA256" ]]; then
        invariant_failure mod_mismatch "the server ran a claude-interface mod that differs from $MOD_SOURCE (Buddy built from another tree?)"
    fi
    local joins
    joins="$(grep -c 'PlayerJoinGame' "$TRIAL/server.log" 2>/dev/null || true)"
    if (( ${joins:-0} > 0 )); then
        invariant_failure contamination_player_join "server log records $joins player join(s)"
    fi
    if [[ -s "$TRIAL/fixture-server.log" ]] && grep -q 'PlayerJoinGame' "$TRIAL/fixture-server.log"; then
        invariant_failure contamination_player_join "a player joined the fixture server"
    fi
    if [[ -f "$SAMPLES/before.json" && ! -f "$SAMPLES/after.json" ]]; then
        : # after-sample failure is already recorded
    fi
    scrub_secrets
    if (( FIXTURE_ONLY )); then
        write_fixture_only_summary "$status"
    else
        write_summary "$status"
    fi
    local failures
    failures="$(grep -c . "$FAILURES" 2>/dev/null || true)"
    if (( ${failures:-0} > 0 || status != 0 )); then
        exit 1
    fi
    exit 0
}

write_fixture_only_summary() {
    local shell_status="$1"
    jq -n \
        --arg scenario "$SCENARIO" \
        --arg fixture_case "$FIXTURE_CASE" \
        --arg layout "$LAYOUT" \
        --argjson seed "$SEED" \
        --argjson repeat "$REPETITION" \
        --arg trial_dir "$TRIAL" \
        --arg stop_reason "$STOP_REASON" \
        --argjson validation "$(json_or_null "$TRIAL/fixture-validation.json")" \
        --argjson dry_run "$(if [[ -s "$TRIAL/fixture-dryrun.json" ]]; then repair_view "$TRIAL/fixture-dryrun.json"; else printf 'null'; fi)" \
        --argjson shell_status "$shell_status" \
        --slurpfile failures "$FAILURES" \
        --argjson redactions "$(if [[ -s "$TRIAL/credential-redactions.txt" ]]; then
            jq -R -s -c 'split("\n") | map(select(length > 0))' "$TRIAL/credential-redactions.txt"
            else printf '[]'; fi)" \
        '{arm: null, scenario: $scenario, fixture_case: $fixture_case, layout: $layout,
          seed: $seed, repeat: $repeat, lab_only: true, fixture_only: true,
          fixture_validation_ok: ($validation.ok // false),
          fixture_expected: ($validation.expected // null),
          fixture_dry_run: $dry_run,
          fixture_probes: [($validation.probes // [])[] | {name, ok}],
          stop_reason: $stop_reason,
          invariant_failures: $failures,
          trial_failed: (($failures | length) > 0 or $shell_status != 0 or ($validation.ok // false) != true),
          credential_redactions: $redactions,
          trial_dir: $trial_dir}' > "$TRIAL/summary.json"
}

write_summary() {
    local shell_status="$1"
    local evidence_json='[]'
    local evidence_stats
    local usage_claude
    local tracker_issues="null"
    local tracker_list
    local contamination
    local elapsed_ms="null"
    local elapsed_ticks="null"
    local holdout_json
    local milestones_json

    if [[ -f "$EVIDENCE" ]]; then
        evidence_json="$(jq -R -s -c '[split("\n")[] | fromjson? | select(type == "object")]' "$EVIDENCE")"
    fi
    evidence_stats="$(jq -c "$(jq_evidence_program)" <<< "$evidence_json")"
    if (( $(jq '.post_exhaustion_tool_outcomes // 0' <<< "$evidence_stats") > 0 )); then
        invariant_failure model_activity_after_budget "tool calls were recorded after autonomy_budget_exhausted"
    fi
    usage_claude="$(jq -R -s -c "$(jq_claude_usage_program)" "$TRIAL/claude-stream.jsonl" 2>/dev/null || echo null)"
    [[ -n "$usage_claude" ]] || usage_claude="null"
    # A trial that ran into the subscription limit measured the quota, not
    # the agent; keep its evidence but never count it as a valid sample.
    if (( $(jq '(.claude.provider_limited_results // 0)' <<< "$usage_claude") > 0 \
        || $(jq '.provider_limited_turns // 0' <<< "$evidence_stats") > 0 )); then
        invariant_failure provider_limited "Claude reported a subscription/provider limit during the trial"
    fi

    if tracker_list="$(cd "$TRACKER" 2>/dev/null && env -i "PATH=$PATH" "HOME=$HOME" bd list --json --all 2>/dev/null)"; then
        tracker_issues="$(jq 'length' <<< "$tracker_list" 2>/dev/null || echo null)"
    fi

    contamination="$(jq -s -c '{polls:length, max_connected_players:(map(.count) | max // null),
        nonzero_polls:(map(select(.count > 0)) | length)}' "$SAMPLES/players.jsonl" 2>/dev/null || echo null)"

    local exhausted_ms
    exhausted_ms="$(jq '.exhausted.unix_ms // null' <<< "$evidence_stats")"
    if [[ -n "$BUDDY_START_MS" ]]; then
        if [[ "$exhausted_ms" != "null" ]]; then
            elapsed_ms=$((exhausted_ms - BUDDY_START_MS))
        elif [[ -n "$BUDDY_STOP_MS" ]]; then
            elapsed_ms=$((BUDDY_STOP_MS - BUDDY_START_MS))
        fi
    fi
    if [[ "$BEFORE_TICK" != "null" && "$STOP_TICK" != "null" ]]; then
        elapsed_ticks=$((STOP_TICK - BEFORE_TICK))
    fi

    holdout_json="$(json_or_null "$TRIAL/holdout.json")"
    if [[ "$holdout_json" == "null" ]]; then
        holdout_json="$(jq -cn --arg reason "$HOLDOUT_REASON" '{evaluated:false, reason:$reason}')"
    fi
    milestones_json="$(json_or_null "$TRIAL/milestones.json")"
    if [[ "$milestones_json" == "null" ]]; then
        milestones_json="$(jq -cn --arg reason "no completed holdout: $HOLDOUT_REASON" '{evaluated:false, reason:$reason}')"
    fi
    if [[ "$SCENARIO" == "fuel-repair" ]]; then
        local fuel_json
        # The holdout diagnosis can exceed Linux's 128 KiB single-argument
        # limit (observed 595 KB), so it must be read from a file, never via
        # --argjson.
        local diagnosis_input="$SAMPLES/holdout-diagnose.json"
        if ! jq -e . "$diagnosis_input" >/dev/null 2>&1; then
            diagnosis_input="$TRIAL/.diagnosis-null.json"
            printf 'null\n' > "$diagnosis_input"
        fi
        if [[ -s "$SAMPLES/fixture-holdout-0.json" && -s "$SAMPLES/fixture-holdout-1.json" && -s "$SAMPLES/fixture-holdout-2.json" ]] \
            && fuel_json="$(jq -c -n \
                --slurpfile fixture "$TRIAL/fixture.json" \
                --slurpfile current "$TRIAL/fixture-targets.json" \
                --slurpfile r0 "$SAMPLES/fixture-holdout-0.json" \
                --slurpfile r1 "$SAMPLES/fixture-holdout-1.json" \
                --slurpfile r2 "$SAMPLES/fixture-holdout-2.json" \
                --slurpfile diagnosis_in "$diagnosis_input" \
                --argjson ev "$evidence_stats" \
                --argjson perturbation "$(json_or_null "$SAMPLES/perturbation.json")" \
                --argjson buddy_start_ms "${BUDDY_START_MS:-null}" \
                "\$fixture[0] as \$fixture | \$current[0] as \$current | \$diagnosis_in[0] as \$diagnosis | [\$r0[0], \$r1[0], \$r2[0]] as \$r | $(jq_fuel_program)" 2>/dev/null)" \
            && [[ -n "$fuel_json" ]]; then
            :
        else
            fuel_json="$(jq -cn --arg reason "no complete holdout fixture readbacks: ${HOLDOUT_REASON:-readback failed}" \
                --arg case "$FIXTURE_CASE" --arg layout "$LAYOUT" \
                '{evaluated:false, achieved:null, case:$case, layout:$layout, reason:$reason, lab_only:true}')"
        fi
        milestones_json="$(jq -c --argjson fuel "$fuel_json" '. + {durable_fuel_recovery: $fuel}' <<< "$milestones_json")"
    fi

    local clean_shutdown=false
    if [[ "$BUDDY_EXIT_STATUS" == "0" && "$SHUTDOWN_FORCED" == false ]] \
        && ! grep -Fq '"code":"owned_server_remaining"' "$FAILURES" \
        && jq -e '.clean_shutdown == true' "$SAVE.buddy-owner.json" >/dev/null 2>&1; then
        clean_shutdown=true
    fi

    jq -n \
        --arg arm "$ARM" \
        --arg scenario "$SCENARIO" \
        --argjson seed "$SEED" \
        --argjson repeat "$REPETITION" \
        --arg trial_dir "$TRIAL" \
        --argjson lab_only "$LAB_ONLY" \
        --arg planner "$PLANNER_MODEL" \
        --arg decision_mode "$DECISION_MODE" \
        --arg jev_model "$JEV_MODEL" \
        --argjson jev_price "$JEV_INPUT_USD_PER_MILLION" \
        --argjson evidence "$evidence_stats" \
        --argjson usage_claude "$usage_claude" \
        --argjson holdout "$holdout_json" \
        --argjson milestones "$milestones_json" \
        --argjson elapsed_ms "$elapsed_ms" \
        --argjson elapsed_ticks "$elapsed_ticks" \
        --argjson buddy_start_ms "${BUDDY_START_MS:-null}" \
        --arg stop_reason "$STOP_REASON" \
        --argjson clean_shutdown "$clean_shutdown" \
        --argjson buddy_exit_status "$BUDDY_EXIT_STATUS" \
        --argjson shutdown_forced "$SHUTDOWN_FORCED" \
        --argjson contamination "$contamination" \
        --argjson tracker_issues "$tracker_issues" \
        --arg tracker_root "$TRACKER" \
        --arg mcp_issue_root "$(cat "$TRIAL/mcp-issue-project-root" 2>/dev/null || true)" \
        --argjson monitor_errors "$MONITOR_ERRORS" \
        --argjson shell_status "$shell_status" \
        --slurpfile failures "$FAILURES" \
        --argjson redactions "$(if [[ -s "$TRIAL/credential-redactions.txt" ]]; then
            jq -R -s -c 'split("\n") | map(select(length > 0))' "$TRIAL/credential-redactions.txt"
            else printf '[]'; fi)" \
        --arg fixture_case "$FIXTURE_CASE" \
        --arg layout "$LAYOUT" \
        --argjson fixture_validation_ok "$(jq '.ok // false' "$TRIAL/fixture-validation.json" 2>/dev/null || echo null)" \
        --argjson space "$(jq -c '.space // null' "$SAMPLES/after.json" 2>/dev/null || echo null)" \
        '{
            arm: $arm,
            scenario: $scenario,
            fixture_case: (if $fixture_case == "" then null else $fixture_case end),
            layout: (if $layout == "" then null else $layout end),
            fixture_validation_ok: (if $scenario == "fuel-repair" then $fixture_validation_ok else null end),
            seed: $seed,
            repeat: $repeat,
            lab_only: $lab_only,
            planner_model: {requested: $planner, observed: ($usage_claude.observed_models // [])},
            decision_model: {
                mode: $decision_mode,
                requested: (if ($decision_mode == "jev" or $decision_mode == "jev-shadow") then $jev_model
                    elif $decision_mode == "deterministic" then "deterministic-rule" else null end),
                observed: $evidence.decisions.models
            },
            elapsed_ms: $elapsed_ms,
            elapsed_ticks: $elapsed_ticks,
            completed_turns: $evidence.completed_turns,
            tool_errors: $evidence.tool_errors,
            repeated_failures: $evidence.repeated_failures,
            milestones: $milestones,
            space: $space,
            holdout: $holdout,
            usage: {
                claude: $usage_claude.claude,
                claude_explanation: $usage_claude.claude_explanation,
                claude_api_key_sources: ($usage_claude.api_key_sources // []),
                jev: (if ($decision_mode == "jev" or $decision_mode == "jev-shadow") then {
                    requests: $evidence.decisions.count,
                    input_tokens: $evidence.decisions.input_tokens,
                    requests_without_usage: $evidence.decisions.requests_without_usage,
                    estimated_input_cost_usd: (if $evidence.decisions.input_tokens == null then null
                        else ($evidence.decisions.input_tokens * $jev_price / 1000000) end),
                    explanation: "input tokens reported in Buddy decision evidence; cost is an estimate at the documented input price, output is free"
                } else null end)
            },
            stop_reason: $stop_reason,
            clean_shutdown: $clean_shutdown,
            invariant_failures: $failures,
            trial_failed: (($failures | length) > 0 or $shell_status != 0),
            contamination: $contamination,
            evidence: ($evidence | del(.repair_executions)),
            shutdown: {buddy_exit_status: $buddy_exit_status, forced_kill: $shutdown_forced},
            tracker: {root: $tracker_root, mcp_issue_project_root: $mcp_issue_root, issues: $tracker_issues},
            monitor_rcon_errors: $monitor_errors,
            credential_redactions: $redactions,
            trial_dir: $trial_dir
        }' > "$TRIAL/summary.json"
}

# ---------------------------------------------------------------------------
# Matrix
# ---------------------------------------------------------------------------

MATRIX="$OUTPUT/matrix.jsonl"
: > "$MATRIX"
OVERALL=0
INTERRUPTED=0
TRIAL_PID=""
trap 'INTERRUPTED=1; [[ -n "$TRIAL_PID" ]] && kill -TERM "$TRIAL_PID" 2>/dev/null; true' INT TERM

for ((REPETITION = 1; REPETITION <= REPEAT; REPETITION++)); do
    (( INTERRUPTED )) && break
    trial_dir="$OUTPUT/$TRIAL_PREFIX-$TRIAL_STEM-r$REPETITION"
    ( run_trial "$REPETITION" ) &
    TRIAL_PID=$!
    set +e
    wait "$TRIAL_PID"
    trial_status=$?
    while process_active "$TRIAL_PID"; do
        wait "$TRIAL_PID"
        trial_status=$?
    done
    set -e
    TRIAL_PID=""
    if [[ -s "$trial_dir/summary.json" ]] && jq -e . "$trial_dir/summary.json" >/dev/null 2>&1; then
        jq -c . "$trial_dir/summary.json" >> "$MATRIX"
        if jq -e '.trial_failed == true' "$trial_dir/summary.json" >/dev/null; then
            OVERALL=1
        fi
    else
        jq -cn --arg arm "$ARM" --arg scenario "$SCENARIO" --argjson seed "$SEED" \
            --arg fixture_case "$FIXTURE_CASE" --arg layout "$LAYOUT" \
            --argjson repeat "$REPETITION" --argjson status "$trial_status" \
            '{arm:$arm, scenario:$scenario, seed:$seed, repeat:$repeat, trial_failed:true,
              fixture_case:(if $fixture_case == "" then null else $fixture_case end),
              layout:(if $layout == "" then null else $layout end),
              stop_reason:"runner_error", invariant_failures:[{code:"summary_missing",
              detail:("trial exited with status " + ($status | tostring) + " without a summary")}]}' \
            >> "$MATRIX"
        OVERALL=1
    fi
    (( trial_status == 0 )) || OVERALL=1
    log "trial $REPETITION/$REPEAT finished (status $trial_status)"
done

if (( INTERRUPTED )); then
    printf 'Evaluation interrupted; completed trial summaries are in %s\n' "$MATRIX" >&2
    exit 1
fi
printf 'Evaluation complete: %s\n' "$MATRIX"
exit "$OVERALL"
