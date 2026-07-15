#!/usr/bin/env bash
# High-risk runtime regressions against an isolated, disposable Factorio save.
#
# Setup is intentionally performed through the trusted operator-only raw-Lua
# path. Every behavior under test is exercised through the shipped mod remote
# interface or the model-facing MCP server.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

RCON_HOST="${RCON_HOST:-127.0.0.1}"
RCON_PORT="${RCON_PORT:-27016}"
RCON_PASSWORD="${RCON_PASSWORD:-test_password}"
AGENT_ID="${FACTORIO_AGENT_ID:-live-regression}"
CLI_BIN="${FACTORIOCTL_BIN:-$ROOT/target/release/factorioctl}"
MCP_BIN="${FACTORIO_MCP_BIN:-$ROOT/target/release/mcp}"
SERVER_LOG="${FACTORIO_TEST_SERVER_LOG:-$ROOT/logs/test-server.log}"
SCRIPT_OUTPUT="${FACTORIO_TEST_SCRIPT_OUTPUT:-${SERVER_DATA_DIR:-$ROOT/.factorio-test-data}/script-output}"

CLI=(
    "$CLI_BIN"
    --host "$RCON_HOST"
    --port "$RCON_PORT"
    --password "$RCON_PASSWORD"
)

PASSED=0
FAILED=0
MCP_PID=""
MCP_IN_FD=""
MCP_OUT_FD=""
MCP_NEXT_ID=1

pass() {
    printf '  PASS: %s\n' "$1"
    PASSED=$((PASSED + 1))
}

fail() {
    printf '  FAIL: %s\n' "$1" >&2
    if [[ -n "${2:-}" ]]; then
        printf '        %s\n' "$2" >&2
    fi
    FAILED=$((FAILED + 1))
}

assert_json() {
    local description="$1"
    local payload="$2"
    shift 2
    if jq -e "$@" >/dev/null 2>&1 <<<"$payload"; then
        pass "$description"
    else
        fail "$description" "$payload"
    fi
}

require_json() {
    local description="$1"
    local payload="$2"
    shift 2
    if jq -e "$@" >/dev/null 2>&1 <<<"$payload"; then
        pass "$description"
    else
        fail "$description" "$payload"
        return 1
    fi
}

raw_lua() {
    FACTORIOCTL_ALLOW_RAW_LUA=1 "${CLI[@]}" exec "$1"
}

enable_raw_lua_for_fixtures() {
    local marker="factorio-buddy-raw-lua-ready"
    local command="rcon.print('$marker')"
    local output

    # A fresh Factorio save refuses the first /c command and asks the operator
    # to repeat it before achievements are disabled. The refusal is not
    # reliably returned in the RCON response, so use an idempotent probe twice
    # and require the second response before executing any fixture mutation.
    FACTORIOCTL_ALLOW_RAW_LUA=1 "${CLI[@]}" exec "$command" >/dev/null 2>&1 || true
    output="$(FACTORIOCTL_ALLOW_RAW_LUA=1 "${CLI[@]}" exec "$command")"
    if [[ "$output" != *"$marker"* ]]; then
        printf 'ERROR: Factorio did not enable trusted raw-Lua fixture setup: %s\n' "$output" >&2
        return 1
    fi
}

rcon_i32_le() {
    local value="$1"
    printf "\\$(printf '%03o' $((value & 255)))"
    printf "\\$(printf '%03o' $(((value >> 8) & 255)))"
    printf "\\$(printf '%03o' $(((value >> 16) & 255)))"
    printf "\\$(printf '%03o' $(((value >> 24) & 255)))"
}

rcon_packet() {
    local request_id="$1"
    local packet_type="$2"
    local body="$3"
    rcon_i32_le "$((10 + ${#body}))"
    rcon_i32_le "$request_id"
    rcon_i32_le "$packet_type"
    printf '%s\0\0' "$body"
}

# Send actual server-console chat rather than raising an event from the level
# script. Factorio's RCON protocol is little-endian length-prefixed Source RCON;
# accepted chat is itself the acknowledgement we assert through the mod inbox.
send_console_chat() {
    local message="$1"
    local socket
    exec {socket}<>"/dev/tcp/$RCON_HOST/$RCON_PORT"
    rcon_packet 1 3 "$RCON_PASSWORD" >&"$socket"
    sleep 0.2
    rcon_packet 2 2 "$message" >&"$socket"
    sleep 0.2
    exec {socket}>&-
    exec {socket}<&-
}

rcon_connection_count() {
    if [[ ! -f "$SERVER_LOG" ]]; then
        printf '0\n'
        return
    fi
    grep -c 'RCON connection from' "$SERVER_LOG" 2>/dev/null || true
}

stop_mcp() {
    if [[ -n "$MCP_IN_FD" ]]; then
        exec {MCP_IN_FD}>&- 2>/dev/null || true
        MCP_IN_FD=""
    fi
    if [[ -n "$MCP_OUT_FD" ]]; then
        exec {MCP_OUT_FD}<&- 2>/dev/null || true
        MCP_OUT_FD=""
    fi
    if [[ -n "$MCP_PID" ]]; then
        kill "$MCP_PID" 2>/dev/null || true
        wait "$MCP_PID" 2>/dev/null || true
        MCP_PID=""
    fi
}
trap stop_mcp EXIT

mcp_read_id() {
    local wanted="$1"
    local line
    while IFS= read -r -t 20 -u "$MCP_OUT_FD" line; do
        if jq -e --argjson wanted "$wanted" '.id == $wanted' >/dev/null 2>&1 <<<"$line"; then
            printf '%s\n' "$line"
            return 0
        fi
    done
    return 1
}

mcp_send() {
    local method="$1"
    local params="$2"
    local id="$MCP_NEXT_ID"
    MCP_NEXT_ID=$((MCP_NEXT_ID + 1))
    jq -cn \
        --argjson id "$id" \
        --arg method "$method" \
        --argjson params "$params" \
        '{jsonrpc:"2.0", id:$id, method:$method, params:$params}' \
        >&"$MCP_IN_FD"
    mcp_read_id "$id"
}

mcp_notify() {
    local method="$1"
    local params="$2"
    jq -cn \
        --arg method "$method" \
        --argjson params "$params" \
        '{jsonrpc:"2.0", method:$method, params:$params}' \
        >&"$MCP_IN_FD"
}

mcp_tool() {
    local tool="$1"
    local arguments="$2"
    mcp_send tools/call "$(jq -cn --arg name "$tool" --argjson arguments "$arguments" \
        '{name:$name, arguments:$arguments}')"
}

tool_payload() {
    jq -r '.result.content[0].text | split("\n\n--- Player Messages ---")[0]' <<<"$1"
}

beads_issue_snapshot() (
    local name
    while IFS= read -r name; do
        case "$name" in
            BEADS_* | BD_*) unset "$name" ;;
        esac
    done < <(compgen -e)
    bd --json --actor=factorio-buddy list --all --limit=0 \
        | jq -cS 'map({id,title,status,issue_type,priority}) | sort_by(.id)'
)

start_mcp() {
    coproc LIVE_MCP {
        FACTORIO_RCON_HOST="$RCON_HOST" \
        FACTORIO_RCON_PORT="$RCON_PORT" \
        FACTORIO_RCON_PASSWORD="$RCON_PASSWORD" \
        FACTORIO_AGENT_ID="$AGENT_ID" \
            "$MCP_BIN" 2>"$ROOT/logs/live-mcp.log"
    }
    MCP_PID="$LIVE_MCP_PID"
    MCP_OUT_FD="${LIVE_MCP[0]}"
    MCP_IN_FD="${LIVE_MCP[1]}"

    local initialized
    initialized="$(mcp_send initialize '{
        "protocolVersion":"2025-03-26",
        "capabilities":{},
        "clientInfo":{"name":"factorio-buddy-live-regressions","version":"1"}
    }')"
    assert_json "MCP initializes" "$initialized" '.result.serverInfo.name == "factorio-mcp"'
    mcp_notify notifications/initialized '{}'
}

printf '=== live Factorio safety regressions ===\n'

if [[ ! -x "$CLI_BIN" || ! -x "$MCP_BIN" ]]; then
    printf 'ERROR: release binaries are missing; run cargo build --release --all-targets\n' >&2
    exit 1
fi
if ! command -v jq >/dev/null 2>&1; then
    printf 'ERROR: jq is required\n' >&2
    exit 1
fi
if ! command -v bd >/dev/null 2>&1; then
    printf 'ERROR: bd is required for the file_issue live regression\n' >&2
    exit 1
fi
if ! "${CLI[@]}" get tick >/dev/null 2>&1; then
    printf 'ERROR: isolated Factorio server is not reachable at %s:%s\n' "$RCON_HOST" "$RCON_PORT" >&2
    exit 1
fi
enable_raw_lua_for_fixtures

# Build a clean test surface and an independent NPC. Raw Lua is fixture setup;
# the lifecycle behavior under test goes through the shipped mod remote.
SETUP="$(raw_lua "
local name = 'buddy-live-regression'
local old = remote.call('claude_interface', 'get_character', '$AGENT_ID')
if old and old.valid then old.destroy() end
local surface = game.surfaces[name]
if not surface then surface = game.create_surface(name, {peaceful_mode = true}) end
surface.request_to_generate_chunks({0, 0}, 6)
surface.request_to_generate_chunks({600, 600}, 1)
surface.force_generate_chunk_requests()
local tiles = {}
for x = -64, 64 do
    for y = -32, 32 do
        tiles[#tiles + 1] = {name = 'landfill', position = {x, y}}
    end
end
for x = 595, 605 do
    for y = 595, 605 do
        tiles[#tiles + 1] = {name = 'landfill', position = {x, y}}
    end
end
surface.set_tiles(tiles, true)
for _, entity in pairs(surface.find_entities_filtered{area = {{-64, -32}, {65, 33}}}) do
    if entity.type ~= 'resource' then entity.destroy() end
end
for _, entity in pairs(surface.find_entities_filtered{area = {{595, 595}, {606, 606}}}) do
    if entity.type ~= 'resource' then entity.destroy() end
end
rcon.print(remote.call('claude_interface', 'pre_place_character_result', '$AGENT_ID', name, 0))
")"
require_json "NPC is independently created on the requested surface" "$SETUP" \
    '.status == "created" and .planet == "buddy-live-regression"'

# An established NPC must remain distinct from other character entities and
# must not be moved when Buddy repeats its idempotent startup lifecycle call.
# A dedicated headless server has no LuaPlayer until a real client joins, and
# the runtime API intentionally provides no synthetic-player constructor.
IDENTITY="$(raw_lua "
local nauvis = game.surfaces['nauvis']
local position = nauvis.find_non_colliding_position('character', {20, 20}, 64, 0.5)
local other = nauvis.create_entity{name = 'character', position = position, force = game.forces.player}
local agent = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local agent_unit = agent.unit_number
local agent_surface = agent.surface.name
local agent_x = agent.position.x
local other_unit = other and other.unit_number or nil
local status = remote.call('claude_interface', 'pre_place_character_result', '$AGENT_ID', 'nauvis', 100)
local after = remote.call('claude_interface', 'get_character', '$AGENT_ID')
rcon.print(helpers.table_to_json({
    lifecycle = helpers.json_to_table(status),
    distinct = other_unit ~= nil and other_unit ~= agent_unit,
    same_unit = after.unit_number == agent_unit,
    same_surface = after.surface.name == agent_surface,
    same_x = after.position.x == agent_x
}))
")"
assert_json "startup preserves an established independent NPC" "$IDENTITY" \
    '.lifecycle.status == "already_placed"
     and .distinct == true
     and .same_unit == true
     and .same_surface == true
     and .same_x == true'

# Ordinary Factorio chat must append the same inbox that wakes Buddy; it must
# not be limited to the custom GUI send button.
CHAT_TOKEN="live-console-chat-$BASHPID"
send_console_chat "$CHAT_TOKEN"
CHAT_RECORD=""
for _ in $(seq 1 20); do
    CHAT_RECORD="$(grep -F "\"message\":\"$CHAT_TOKEN\"" "$SCRIPT_OUTPUT/claude-chat/input.jsonl" 2>/dev/null | tail -n 1 || true)"
    [[ -z "$CHAT_RECORD" ]] || break
    sleep 0.1
done
assert_json "normal Factorio chat reaches the Buddy inbox" "$CHAT_RECORD" \
    --arg token "$CHAT_TOKEN" \
    '.message == $token and .player_index == 0 and .player_name == "console" and .target_agent == "all"'

# Trigger technologies must be observed, never assigned researched=true.
raw_lua "local force = game.forces.player; if force.current_research then force.cancel_current_research() end; force.technologies['steam-power'].researched = false" >/dev/null
RESEARCH="$(raw_lua "rcon.print(remote.call('claude_interface', 'start_research', 'steam-power', '$AGENT_ID'))")"
assert_json "trigger research is not force-completed" "$RESEARCH" \
    '.success == false and .error_kind == "research_trigger_required"'
RESEARCH_STATE="$(raw_lua "rcon.print(helpers.table_to_json({researched = game.forces.player.technologies['steam-power'].researched}))")"
assert_json "Factorio still owns trigger completion" "$RESEARCH_STATE" '.researched == false'

# Direct mod mutation must reject telekinetic placement. The normal Rust tool
# is allowed to walk first; this probes the safety boundary underneath it.
raw_lua "local c = remote.call('claude_interface', 'get_character', '$AGENT_ID'); c.teleport({0.5, 0.5}, game.surfaces['buddy-live-regression']); local inv = c.get_main_inventory(); inv.clear(); inv.insert{name = 'wooden-chest', count = 2}" >/dev/null
FAR_PLACE="$(raw_lua "rcon.print(remote.call('claude_interface', 'place_entity', '$AGENT_ID', 'wooden-chest', 50.5, 0.5, defines.direction.north))")"
assert_json "out-of-reach placement is rejected structurally" "$FAR_PLACE" \
    '.success == false and .error_kind == "out_of_reach" and .action_needed == "walk_to"'
FAR_COUNT="$(raw_lua "local s = game.surfaces['buddy-live-regression']; rcon.print(helpers.table_to_json({count = s.count_entities_filtered{name = 'wooden-chest', position = {50.5, 0.5}, radius = 0.1}}))")"
assert_json "out-of-reach placement leaves the world unchanged" "$FAR_COUNT" '.count == 0'

# The production walk driver must move through Factorio's ordinary character
# walking state. Observe an intermediate position before arrival so a hidden
# endpoint teleport cannot satisfy this regression.
raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
c.teleport({-20.5, 20.5}, game.surfaces['buddy-live-regression'])
remote.call('claude_interface', 'clear_walk_target', '$AGENT_ID')
rcon.print(remote.call('claude_interface', 'set_walk_target', '$AGENT_ID', -10.5, 20.5))
" >/dev/null
sleep 0.5
WALKING_MIDPOINT="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
rcon.print(helpers.table_to_json({
    x = c.position.x,
    y = c.position.y,
    walking = c.walking_state.walking,
    target_active = remote.call('claude_interface', 'has_walk_target', '$AGENT_ID')
}))
")"
assert_json "NPC traverses an intermediate position using engine walking" "$WALKING_MIDPOINT" \
    '.walking == true and .target_active == true and .x > -20.4 and .x < -10.7'
sleep 6
WALKING_ARRIVAL="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
rcon.print(helpers.table_to_json({
    x = c.position.x,
    y = c.position.y,
    walking = c.walking_state.walking,
    target_active = remote.call('claude_interface', 'has_walk_target', '$AGENT_ID')
}))
")"
assert_json "NPC arrives and stops ordinary walking" "$WALKING_ARRIVAL" \
    '.walking == false and .target_active == false and ((.x + 10.5) | fabs) < 0.4'

# A full inventory may cause placement to fail, but must never delete an item
# from the ground while trying to clear the tile.
raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
remote.call('claude_interface', 'clear_walk_target', '$AGENT_ID')
c.teleport({3.5, 0.5}, game.surfaces['buddy-live-regression'])
local inv = c.get_main_inventory()
inv.clear()
inv.insert{name = 'wooden-chest', count = 1}
for i = 1, #inv do
    if not inv[i].valid_for_read then inv[i].set_stack{name = 'stone', count = 50} end
end
local s = c.surface
for _, e in pairs(s.find_entities_filtered{position = {5.5, 0.5}, radius = 0.2}) do
    if e.type ~= 'character' and e.type ~= 'resource' then e.destroy() end
end
s.create_entity{name = 'item-on-ground', position = {5.5, 0.5}, stack = {name = 'copper-plate', count = 1}}
" >/dev/null
raw_lua "rcon.print(remote.call('claude_interface', 'place_entity', '$AGENT_ID', 'wooden-chest', 5.5, 0.5, defines.direction.north))" >/dev/null
GROUND_ITEM="$(raw_lua "local s = game.surfaces['buddy-live-regression']; local count = 0; for _, e in pairs(s.find_entities_filtered{type = 'item-entity', position = {5.5, 0.5}, radius = 0.2}) do if e.stack and e.stack.valid_for_read and e.stack.name == 'copper-plate' then count = count + e.stack.count end end; rcon.print(helpers.table_to_json({count = count}))")"
assert_json "placement never destroys blocked ground items" "$GROUND_ITEM" '.count == 1'

# Coordinate-only removal is discovery, never mutation. Even one candidate
# must be returned for an exact-unit follow-up, and exact removal must remain
# available for deliberate changes.
SOLE_REMOVE_FIXTURE="$(raw_lua "
game.tick_paused = true
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
c.teleport({8.5, 0.5})
local inv = c.get_main_inventory()
if inv then inv.clear() end
local s = c.surface
for _, e in pairs(s.find_entities_filtered{position = {10.5, 0.5}, radius = 0.2}) do
    if e.type ~= 'resource' and e.type ~= 'character' then e.destroy() end
end
local belt = s.create_entity{name = 'transport-belt', position = {10.5, 0.5}, direction = defines.direction.east, force = c.force}
rcon.print(helpers.table_to_json({unit_number = belt and belt.unit_number or nil}))
")"
SOLE_REMOVE_UNIT="$(jq -r '.unit_number' <<<"$SOLE_REMOVE_FIXTURE")"
COORDINATE_REMOVE="$(raw_lua "rcon.print(remote.call('claude_interface', 'remove_entity_at', '$AGENT_ID', 10.5, 0.5))")"
assert_json "coordinate-only removal requires exact identity for one candidate" "$COORDINATE_REMOVE" \
    --argjson unit "$SOLE_REMOVE_UNIT" \
    '.success == false
     and .error_kind == "exact_identity_required"
     and .action_needed == "remove_entity_by_unit_number"
     and (.candidates | length) == 1
     and .candidates[0].unit_number == $unit'
SOLE_BELT_REMAINS="$(raw_lua "local s = game.surfaces['buddy-live-regression']; rcon.print(helpers.table_to_json({count = s.count_entities_filtered{name = 'transport-belt', position = {10.5, 0.5}, radius = 0.2}}))")"
assert_json "coordinate-only removal leaves the sole candidate unchanged" "$SOLE_BELT_REMAINS" '.count == 1'
EXACT_REMOVE="$(raw_lua "rcon.print(remote.call('claude_interface', 'remove_entity', '$AGENT_ID', $SOLE_REMOVE_UNIT))")"
assert_json "exact-unit removal still removes the selected entity" "$EXACT_REMOVE" \
    --argjson unit "$SOLE_REMOVE_UNIT" \
    '.success == true and .removed == true and .unit_number == $unit'
SOLE_BELT_GONE="$(raw_lua "local s = game.surfaces['buddy-live-regression']; rcon.print(helpers.table_to_json({count = s.count_entities_filtered{name = 'transport-belt', position = {10.5, 0.5}, radius = 0.2}}))")"
assert_json "exact-unit removal changes only the selected entity" "$SOLE_BELT_GONE" '.count == 0'

# The same coordinate seam remains non-mutating when several entities overlap.
raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
s.create_entity{name = 'transport-belt', position = {10.5, 0.5}, direction = defines.direction.east, force = c.force}
s.create_entity{name = 'entity-ghost', inner_name = 'small-electric-pole', position = {10.5, 0.5}, force = c.force}
" >/dev/null
AMBIGUOUS_REMOVE="$(raw_lua "rcon.print(remote.call('claude_interface', 'remove_entity_at', '$AGENT_ID', 10.5, 0.5))")"
raw_lua "game.tick_paused = false" >/dev/null
assert_json "coordinate removal fails closed on overlap" "$AMBIGUOUS_REMOVE" \
    '.success == false and .error_kind == "exact_identity_required" and (.candidates | length) >= 2'
BELT_REMAINS="$(raw_lua "local s = game.surfaces['buddy-live-regression']; rcon.print(helpers.table_to_json({count = s.count_entities_filtered{name = 'transport-belt', position = {10.5, 0.5}, radius = 0.2}}))")"
assert_json "coordinate removal does not mine nearby infrastructure" "$BELT_REMAINS" '.count == 1'

# Surface-scoped snapshots and diagnostics must describe the same world.
SURFACE_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
local furnace = s.create_entity{name = 'stone-furnace', position = {15.5, 0.5}, force = c.force}
s.create_entity{name = 'transport-belt', position = {18.5, 0.5}, direction = defines.direction.east, force = c.force}
rcon.print(helpers.table_to_json({furnace_unit = furnace.unit_number}))
")"
FURNACE_UNIT="$(jq -r '.furnace_unit' <<<"$SURFACE_FIXTURE")"
SNAPSHOT="$(raw_lua "rcon.print(remote.call('claude_interface', 'autonomy_snapshot', '$AGENT_ID'))")"
assert_json "snapshot and blocker scan use the NPC surface" "$SNAPSHOT" \
    --argjson unit "$FURNACE_UNIT" \
    '.surface == "buddy-live-regression"
     and .factory.entity_count > 0
     and .factory.blockers.scanned_entities > 0
     and any(.factory.blockers.blockers[]?; .unit_number == $unit)'

# Unit lookup must work beyond the former +/-500 scan boundary, including for
# prototypes that Factorio does not expose through get_entity_by_unit_number.
FAR_ENTITY="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local entity = s.create_entity{name = 'stone-furnace', position = {600.5, 600.5}, force = game.forces.player}
rcon.print(helpers.table_to_json({unit_number = entity.unit_number}))
")"
FAR_UNIT="$(jq -r '.unit_number' <<<"$FAR_ENTITY")"
FAR_LOOKUP="$(raw_lua "rcon.print(remote.call('claude_interface', 'get_entity', $FAR_UNIT))")"
assert_json "unit-number lookup works beyond 500 tiles" "$FAR_LOOKUP" \
    --argjson unit "$FAR_UNIT" \
    '.unit_number == $unit and .name == "stone-furnace" and .position.x > 500 and .position.y > 500'

# A belt or chest stocked once by the character is not durable fuel
# automation. The topology proof must trace beyond the adjacent source to an
# operational coal producer before certifying the consumer as automated.
SEEDED_FUEL_FIXTURE="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
for _, e in pairs(s.find_entities_filtered{area = {{-48, -8}, {-32, 8}}}) do
    if e.type ~= 'resource' and e.type ~= 'character' then e.destroy() end
end
local furnace = s.create_entity{name = 'stone-furnace', position = {-40, 0}, force = game.forces.player}
local inserter_position = {x = furnace.position.x, y = furnace.bounding_box.left_top.y - 0.5}
local belt_position = {x = furnace.position.x, y = furnace.bounding_box.left_top.y - 1.5}
s.create_entity{name = 'inserter', position = inserter_position, direction = defines.direction.north, force = game.forces.player}
local belt = s.create_entity{name = 'transport-belt', position = belt_position, direction = defines.direction.east, force = game.forces.player}
belt.get_transport_line(1).insert_at_back({name = 'coal', count = 1})
local report = helpers.json_to_table(remote.call(
    'claude_interface',
    'diagnose_fuel_sustainability',
    -48,
    -8,
    -32,
    8,
    20,
    '$AGENT_ID'
))
rcon.print(helpers.table_to_json({consumer_unit = furnace.unit_number, report = report}))
")"
assert_json "manually stocked fuel source is not certified as durable automation" "$SEEDED_FUEL_FIXTURE" \
    '.consumer_unit as $unit
     | (.report.consumers[] | select(.unit_number == $unit))
     | .fuel_topology_present == true
       and .automated == false
       and any(.fuel_connections[]?;
           .source.coal_count > 0
           and .source_durable == false
           and .durable == false
           and .source.upstream_proof.reason == "stocked_without_proven_upstream")'

# A currently working burner coal drill is not a durable terminal producer just
# because the character put one coal in its burner. Prove the false-positive
# window while that manual fuel is still burning, then exhaust it and prove the
# downstream furnace remains uncertified.
BUFFERED_DRILL_FIXTURE="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local area = {{-64, 12}, {-46, 31}}
for _, entity in pairs(s.find_entities_filtered{area = area}) do
    if entity.type ~= 'character' then entity.destroy() end
end
for x = -58, -55 do
    for y = 21, 24 do
        s.create_entity{name = 'coal', position = {x + 0.5, y + 0.5}, amount = 100000}
    end
end
local drill = s.create_entity{
    name = 'burner-mining-drill',
    position = {-56, 23},
    direction = defines.direction.north,
    force = game.forces.player
}
if not drill then
    rcon.print(helpers.table_to_json({error = 'burner drill creation failed'}))
    return
end
local drill_fuel = drill.get_fuel_inventory()
drill_fuel.insert{name = 'coal', count = 1}
local belt = s.create_entity{
    name = 'transport-belt',
    position = drill.drop_position,
    direction = defines.direction.north,
    force = game.forces.player
}
if not belt then
    rcon.print(helpers.table_to_json({error = 'drill output belt creation failed'}))
    return
end
local inserter = s.create_entity{
    name = 'burner-inserter',
    position = {belt.position.x, belt.position.y - 1},
    direction = defines.direction.south,
    force = game.forces.player
}
local furnace = s.create_entity{
    name = 'stone-furnace',
    position = {belt.position.x, belt.position.y - 2.5},
    force = game.forces.player
}
if not (inserter and furnace) then
    rcon.print(helpers.table_to_json({error = 'downstream fuel consumer creation failed'}))
    return
end
inserter.active = false
inserter.get_fuel_inventory().insert{name = 'coal', count = 1}
rcon.print(helpers.table_to_json({
    drill_unit = drill.unit_number,
    belt_unit = belt.unit_number,
    inserter_unit = inserter.unit_number,
    furnace_unit = furnace.unit_number,
    mining_target = drill.mining_target and drill.mining_target.name or nil,
    drill_drop_position = drill.drop_position,
    belt_position = belt.position,
    inserter_position = inserter.position,
    furnace_position = furnace.position
}))
")"
require_json "manual-buffer burner drill fixture is physically constructed" "$BUFFERED_DRILL_FIXTURE" \
    '.error == null
     and (.drill_unit | type) == "number"
     and (.belt_unit | type) == "number"
     and (.inserter_unit | type) == "number"
     and (.furnace_unit | type) == "number"'
BUFFERED_DRILL_UNIT="$(jq -r '.drill_unit' <<<"$BUFFERED_DRILL_FIXTURE")"
BUFFERED_BELT_UNIT="$(jq -r '.belt_unit' <<<"$BUFFERED_DRILL_FIXTURE")"
BUFFERED_INSERTER_UNIT="$(jq -r '.inserter_unit' <<<"$BUFFERED_DRILL_FIXTURE")"
BUFFERED_FURNACE_UNIT="$(jq -r '.furnace_unit' <<<"$BUFFERED_DRILL_FIXTURE")"

BUFFERED_DRILL_STATE='{}'
for _ in $(seq 1 100); do
    BUFFERED_DRILL_STATE="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local drill, belt
for _, entity in pairs(s.find_entities_filtered{area = {{-64, 12}, {-46, 31}}}) do
    if entity.unit_number == $BUFFERED_DRILL_UNIT then drill = entity end
    if entity.unit_number == $BUFFERED_BELT_UNIT then belt = entity end
end
local coal = 0
if belt then
    for line_index = 1, 2 do
        coal = coal + belt.get_transport_line(line_index).get_item_count('coal')
    end
end
local status = nil
if drill then
    for name, value in pairs(defines.entity_status) do
        if value == drill.status then status = name break end
    end
end
rcon.print(helpers.table_to_json({
    belt_coal = coal,
    status = status,
    remaining_burning_fuel = drill and drill.burner and drill.burner.remaining_burning_fuel or 0
}))
")"
    if jq -e '.belt_coal > 0
        and .remaining_burning_fuel > 0
        and (.status == "working" or .status == "waiting_for_space_in_destination")' \
        >/dev/null 2>&1 <<<"$BUFFERED_DRILL_STATE"; then
        break
    fi
    sleep 0.1
done
require_json "manually fueled burner drill emits coal while its finite buffer is burning" "$BUFFERED_DRILL_STATE" \
    '.belt_coal > 0
     and .remaining_burning_fuel > 0
     and (.status == "working" or .status == "waiting_for_space_in_destination")'

BUFFERED_DRILL_REPORT="$(raw_lua "
local report = helpers.json_to_table(remote.call(
    'claude_interface',
    'diagnose_fuel_sustainability',
    -64,
    12,
    -46,
    31,
    30,
    '$AGENT_ID'
))
rcon.print(helpers.table_to_json({report = report}))
")"
require_json "manual-buffer burner drill is not a durable downstream coal source while running" "$BUFFERED_DRILL_REPORT" \
    --argjson furnace "$BUFFERED_FURNACE_UNIT" \
    --argjson belt "$BUFFERED_BELT_UNIT" \
    '.report.consumers[]
     | select(.unit_number == $furnace)
     | .fuel_topology_present == true
       and .automated == false
       and any(.fuel_connections[]?;
           .source.unit_number == $belt
           and .source.coal_count > 0
           and .source_durable == false
           and .durable == false
           and .source.upstream_proof.reason == "coal_drill_upstream_not_durable"
           and .source.upstream_proof.upstream_proof.reason == "burner_coal_drill_fuel_not_durable"
           and .source.upstream_proof.upstream_proof.fuel_proof.reason == "manual_burner_fuel_buffer")'

# Shorten only the already-burning test buffer after observing real output, then
# let the game advance into no_fuel. This keeps the regression fast without
# fabricating coal or a durable feed.
raw_lua "
local s = game.surfaces['buddy-live-regression']
for _, entity in pairs(s.find_entities_filtered{area = {{-64, 12}, {-46, 31}}}) do
    if entity.unit_number == $BUFFERED_DRILL_UNIT and entity.burner then
        entity.get_fuel_inventory().clear()
        entity.burner.remaining_burning_fuel = math.min(entity.burner.remaining_burning_fuel, 1)
    elseif entity.unit_number == $BUFFERED_INSERTER_UNIT then
        entity.active = true
    end
end
" >/dev/null

EXHAUSTED_DRILL_STATE='{}'
for _ in $(seq 1 50); do
    EXHAUSTED_DRILL_STATE="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local drill
for _, entity in pairs(s.find_entities_filtered{area = {{-64, 12}, {-46, 31}}}) do
    if entity.unit_number == $BUFFERED_DRILL_UNIT then drill = entity break end
end
local status = nil
if drill then
    for name, value in pairs(defines.entity_status) do
        if value == drill.status then status = name break end
    end
end
rcon.print(helpers.table_to_json({
    status = status,
    remaining_burning_fuel = drill and drill.burner and drill.burner.remaining_burning_fuel or 0,
    fuel_count = drill and drill.get_fuel_inventory().get_item_count() or 0
}))
")"
    if jq -e '.status == "no_fuel"
        and .remaining_burning_fuel <= 0
        and .fuel_count == 0' >/dev/null 2>&1 <<<"$EXHAUSTED_DRILL_STATE"; then
        break
    fi
    sleep 0.1
done
require_json "the manually fueled burner drill exhausts its finite buffer" "$EXHAUSTED_DRILL_STATE" \
    '.status == "no_fuel" and .remaining_burning_fuel <= 0 and .fuel_count == 0'

EXHAUSTED_DRILL_REPORT="$(raw_lua "
local report = helpers.json_to_table(remote.call(
    'claude_interface',
    'diagnose_fuel_sustainability',
    -64,
    12,
    -46,
    31,
    30,
    '$AGENT_ID'
))
rcon.print(helpers.table_to_json({report = report}))
")"
require_json "exhausted manual burner drill remains non-durable downstream" "$EXHAUSTED_DRILL_REPORT" \
    --argjson furnace "$BUFFERED_FURNACE_UNIT" \
    --argjson belt "$BUFFERED_BELT_UNIT" \
    '.report.consumers[]
     | select(.unit_number == $furnace)
     | .automated == false
       and any(.fuel_connections[]?;
           .source.unit_number == $belt
           and .source_durable == false
           and .durable == false
           and .source.upstream_proof.reason == "coal_drill_upstream_not_durable"
           and .source.upstream_proof.upstream_proof.reason == "burner_coal_drill_fuel_not_durable")'

# The fail-closed producer proof must still recognize genuine automated coal
# production. Exercise a powered electric drill and its real output belt so the
# negative burner checks cannot be satisfied by rejecting every producer.
ELECTRIC_COAL_FIXTURE="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local area = {{-31, -31}, {-8, -10}}
for _, entity in pairs(s.find_entities_filtered{area = area}) do
    if entity.type ~= 'character' then entity.destroy() end
end
for x = -24, -21 do
    for y = -24, -21 do
        s.create_entity{name = 'coal', position = {x + 0.5, y + 0.5}, amount = 100000}
    end
end
local interface = s.create_entity{
    name = 'electric-energy-interface',
    position = {-13, -22},
    force = game.forces.player
}
local substation = s.create_entity{name = 'substation', position = {-17, -22}, force = game.forces.player}
local drill = s.create_entity{
    name = 'electric-mining-drill',
    position = {-22.5, -22.5},
    direction = defines.direction.north,
    force = game.forces.player
}
local belt = drill and s.create_entity{
    name = 'transport-belt',
    position = drill.drop_position,
    direction = defines.direction.north,
    force = game.forces.player
} or nil
rcon.print(helpers.table_to_json({
    interface_unit = interface and interface.unit_number or nil,
    substation_unit = substation and substation.unit_number or nil,
    drill_unit = drill and drill.unit_number or nil,
    belt_unit = belt and belt.unit_number or nil
}))
")"
require_json "powered electric coal fixture is physically constructed" "$ELECTRIC_COAL_FIXTURE" \
    '(.interface_unit | type) == "number"
     and (.substation_unit | type) == "number"
     and (.drill_unit | type) == "number"
     and (.belt_unit | type) == "number"'
ELECTRIC_DRILL_UNIT="$(jq -r '.drill_unit' <<<"$ELECTRIC_COAL_FIXTURE")"
ELECTRIC_BELT_UNIT="$(jq -r '.belt_unit' <<<"$ELECTRIC_COAL_FIXTURE")"

ELECTRIC_COAL_STATE='{}'
for _ in $(seq 1 100); do
    ELECTRIC_COAL_STATE="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local drill, belt
for _, entity in pairs(s.find_entities_filtered{area = {{-31, -31}, {-8, -10}}}) do
    if entity.unit_number == $ELECTRIC_DRILL_UNIT then drill = entity end
    if entity.unit_number == $ELECTRIC_BELT_UNIT then belt = entity end
end
local coal = 0
if belt then
    for line_index = 1, 2 do
        coal = coal + belt.get_transport_line(line_index).get_item_count('coal')
    end
end
local status = nil
if drill then
    for name, value in pairs(defines.entity_status) do
        if value == drill.status then status = name break end
    end
end
rcon.print(helpers.table_to_json({
    belt_coal = coal,
    status = status,
    connected = drill and drill.is_connected_to_electric_network() or false,
    energy = drill and drill.energy or 0
}))
")"
    if jq -e '.belt_coal > 0 and .connected == true and .energy > 0
        and (.status == "working" or .status == "waiting_for_space_in_destination")' \
        >/dev/null 2>&1 <<<"$ELECTRIC_COAL_STATE"; then
        break
    fi
    sleep 0.1
done
require_json "powered electric drill produces real coal onto its belt" "$ELECTRIC_COAL_STATE" \
    '.belt_coal > 0 and .connected == true and .energy > 0
     and (.status == "working" or .status == "waiting_for_space_in_destination")'

ELECTRIC_COAL_REPORT="$(raw_lua "
local report = helpers.json_to_table(remote.call(
    'claude_interface',
    'diagnose_fuel_sustainability',
    -31,
    -31,
    -8,
    -10,
    30,
    '$AGENT_ID'
))
rcon.print(helpers.table_to_json({report = report}))
")"
require_json "powered electric coal production is certified durable and live" "$ELECTRIC_COAL_REPORT" \
    --argjson drill "$ELECTRIC_DRILL_UNIT" \
    --argjson belt "$ELECTRIC_BELT_UNIT" \
    'any(.report.coal_sources.mining_drills[]?;
         .unit_number == $drill
         and .durable == true
         and .operational == true
         and .upstream_proof.reason == "powered_operational_electric_coal_drill")
     and any(.report.coal_sources.belts[]?;
         .unit_number == $belt
         and .coal_count > 0
         and .durable == true
         and .operational == true
         and .upstream_proof.reason == "direct_durable_coal_drill")'

# Install a wrong-facing belt in an otherwise eastbound three-tile corridor.
# The MCP router may reject it or route around it, but may not count it as a
# connected, reusable segment.
raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
c.teleport({28.5, 10.5})
local inv = c.get_main_inventory()
inv.clear()
inv.insert{name = 'transport-belt', count = 100}
inv.insert{name = 'inserter', count = 20}
local s = c.surface
for _, e in pairs(s.find_entities_filtered{area = {{29, 8}, {34, 13}}}) do
    if e.type ~= 'resource' and e.type ~= 'character' then e.destroy() end
end
for _, e in pairs(s.find_entities_filtered{area = {{28, 18}, {41, 23}}}) do
    if e.type ~= 'resource' and e.type ~= 'character' then e.destroy() end
end
s.create_entity{name = 'transport-belt', position = {31.5, 10.5}, direction = defines.direction.north, force = c.force}
" >/dev/null

# A controller may not retain a geometrically complete belt plus an inserter
# that touches neither the route endpoint nor its named machine. This is the
# exact pointless-infrastructure failure reproduced by the final audit.
DISCONNECTED_LAB_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
for _, entity in pairs(s.find_entities_filtered{area = {{0, 24}, {21, 33}}}) do
    if entity.type ~= 'resource' and entity.type ~= 'character' then entity.destroy() end
end
local lab = s.create_entity{name = 'lab', position = {5.5, 28.5}, force = c.force}
rcon.print(helpers.table_to_json({lab_unit = lab and lab.unit_number or nil}))
")"
require_json "disconnected controller fixture has a real target lab" "$DISCONNECTED_LAB_FIXTURE" \
    '(.lab_unit | type) == "number"'
DISCONNECTED_LAB_UNIT="$(jq -r '.lab_unit' <<<"$DISCONNECTED_LAB_FIXTURE")"

start_mcp

TOOLS="$(mcp_send tools/list '{}')"
EXPECTED_TOOLS="$(printf '%s\n' \
    analyze_inserters analyze_item_flow bootstrap_burner_once bootstrap_smelting_once \
    build_assembler_feed build_assembler_output build_automation_science \
    build_lab_feed build_recipe_assembler_cell collect_from_chest configure_inserter craft diagnose_factory_blockers \
    diagnose_steam_power execute_direct_smelter execute_edge_miner \
    execute_entity_placement_near extend_power_to feed_lab_from_inventory file_issue \
    find_nearest_resource \
    get_available_research get_belt_lane_contents get_entities get_entity_inventory \
    get_machine_belt_positions get_power_status get_recipe get_recipes_for_item \
    get_research_status mine_at place_entity plan_automation_science \
    plan_machine_output plan_recipe_assembler_cell plan_steam_power \
    production_statistics remove_entity render_map repair_fuel_sustainability \
    rotate_entity route_belt set_recipe situation_report start_research unstuck \
    verify_production wait_for_crafting walk_to | jq -Rsc 'split("\n")[:-1] | sort')"
assert_json "model receives the exact 49-tool gameplay surface" "$TOOLS" \
    --argjson expected "$EXPECTED_TOOLS" \
    '([.result.tools[].name] | sort) == $expected and (.result.tools | length) == 49'
TOOLS_SCHEMA_BYTES="$(jq -c '.result.tools' <<<"$TOOLS" | wc -c)"
if (( TOOLS_SCHEMA_BYTES <= 61440 )); then
    pass "model tool schema stays below 60 KiB"
else
    fail "model tool schema stays below 60 KiB" \
        "observed $TOOLS_SCHEMA_BYTES bytes"
fi

# A two-tile walk is real movement, not an already-arrived near no-op. The
# response and the authoritative character position must agree.
SHORT_WALK_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
remote.call('claude_interface', 'clear_walk_target', '$AGENT_ID')
for _, entity in pairs(s.find_entities_filtered{area = {{-46, -26}, {-32, -14}}}) do
    if entity.type ~= 'resource' and entity.type ~= 'character' then entity.destroy() end
end
c.teleport({-44.5, -20.5}, s)
rcon.print(helpers.table_to_json({x = c.position.x, y = c.position.y}))
")"
require_json "short-walk fixture starts at the exact disposable position" "$SHORT_WALK_FIXTURE" \
    '.x == -44.5 and .y == -20.5'

SHORT_WALK="$(mcp_tool walk_to '{"x":-42.5,"y":-20.5}')"
SHORT_WALK_PAYLOAD="$(tool_payload "$SHORT_WALK")"
assert_json "two-tile MCP walk reports real movement and arrival near the target" "$SHORT_WALK_PAYLOAD" \
    '.arrived == true
     and .distance_walked > 1.5
     and ((.final_position.x + 42.5) | fabs) < 0.5
     and ((.final_position.y + 20.5) | fabs) < 0.5'
SHORT_WALK_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local dx = c.position.x - (-44.5)
local dy = c.position.y - (-20.5)
rcon.print(helpers.table_to_json({
    x = c.position.x,
    y = c.position.y,
    displacement = math.sqrt(dx * dx + dy * dy),
    target_active = remote.call('claude_interface', 'has_walk_target', '$AGENT_ID')
}))
")"
assert_json "two-tile MCP walk actually moves the NPC and clears its target" "$SHORT_WALK_WORLD" \
    '.displacement > 1.5
     and ((.x + 42.5) | fabs) < 0.5
     and ((.y + 20.5) | fabs) < 0.5
     and .target_active == false'

# Walk IDs make completion receipts race-safe. Superseding A with B must leave
# A's receipt readable, and cancelling A must never cancel active B.
WALK_RECEIPT_LIFECYCLE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
remote.call('claude_interface', 'clear_walk_target', '$AGENT_ID')
c.teleport({-44.5, -20.5}, s)
local first = helpers.json_to_table(remote.call(
    'claude_interface', 'set_walk_target', '$AGENT_ID', -30.5, -20.5
))
local second = helpers.json_to_table(remote.call(
    'claude_interface', 'set_walk_target', '$AGENT_ID', -44.5, -6.5
))
local first_status = helpers.json_to_table(remote.call(
    'claude_interface', 'get_walk_status', '$AGENT_ID', first.walk_id
))
local stale_clear = helpers.json_to_table(remote.call(
    'claude_interface', 'clear_walk_target', '$AGENT_ID', first.walk_id
))
local second_active = helpers.json_to_table(remote.call(
    'claude_interface', 'get_walk_status', '$AGENT_ID', second.walk_id
))
local target_active = remote.call('claude_interface', 'has_walk_target', '$AGENT_ID')
local second_cancelled = helpers.json_to_table(remote.call(
    'claude_interface', 'clear_walk_target', '$AGENT_ID', second.walk_id
))
rcon.print(helpers.table_to_json({
    first = first,
    second = second,
    first_status = first_status,
    stale_clear = stale_clear,
    second_active = second_active,
    target_active_after_stale_clear = target_active,
    second_cancelled = second_cancelled,
}))
")"
assert_json "walk receipts preserve supersession and stale cancellation safety" "$WALK_RECEIPT_LIFECYCLE" \
    '.first.active == true
     and .second.active == true
     and .first.walk_id != .second.walk_id
     and .first_status.active == false
     and .first_status.arrived == false
     and .first_status.reason == "superseded"
     and .stale_clear.walk_id == .first.walk_id
     and .stale_clear.reason == "superseded"
     and .second_active.walk_id == .second.walk_id
     and .second_active.active == true
     and .second_active.reason == "walking"
     and .target_active_after_stale_clear == true
     and .second_cancelled.walk_id == .second.walk_id
     and .second_cancelled.active == false
     and .second_cancelled.reason == "cancelled"'

# Factorio's authoritative reach check owns whether an exact-unit mutation
# needs movement. At the vanilla six-tile reach boundary, removal must not
# displace an NPC that can already reach the pole.
REACHABLE_POLE_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
remote.call('claude_interface', 'clear_walk_target', '$AGENT_ID')
for _, entity in pairs(s.find_entities_filtered{area = {{-46, -26}, {-32, -14}}}) do
    if entity.type ~= 'resource' and entity.type ~= 'character' then entity.destroy() end
end
c.teleport({-44.5, -20.5}, s)
c.get_main_inventory().clear()
local pole = s.create_entity{
    name = 'small-electric-pole',
    position = {-38.5, -20.5},
    force = c.force,
}
if not pole then error('failed to create reachable-pole fixture') end
local dx = pole.position.x - c.position.x
local dy = pole.position.y - c.position.y
rcon.print(helpers.table_to_json({
    unit_number = pole.unit_number,
    character_position = {x = c.position.x, y = c.position.y},
    pole_position = {x = pole.position.x, y = pole.position.y},
    distance = math.sqrt(dx * dx + dy * dy),
    can_reach = c.can_reach_entity(pole)
}))
")"
require_json "six-tile pole is authoritatively reachable before MCP removal" "$REACHABLE_POLE_FIXTURE" \
    '(.unit_number | type) == "number"
     and .can_reach == true
     and ((.distance - 6) | fabs) < 0.01'
REACHABLE_POLE_UNIT="$(jq -r '.unit_number' <<<"$REACHABLE_POLE_FIXTURE")"
REACHABLE_POLE_X="$(jq -r '.character_position.x' <<<"$REACHABLE_POLE_FIXTURE")"
REACHABLE_POLE_Y="$(jq -r '.character_position.y' <<<"$REACHABLE_POLE_FIXTURE")"

REACHABLE_POLE_REMOVE="$(mcp_tool remove_entity "$(jq -cn \
    --argjson unit "$REACHABLE_POLE_UNIT" \
    '{unit_number:$unit}')")"
assert_json "MCP removes the already-reachable pole" "$REACHABLE_POLE_REMOVE" \
    '(.result.isError // false) == false
     and .result.content[0].text == "Entity removed successfully"'
REACHABLE_POLE_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
rcon.print(helpers.table_to_json({
    pole_count = s.count_entities_filtered{
        name = 'small-electric-pole',
        position = {-38.5, -20.5},
        radius = 0.2,
    },
    character_position = {x = c.position.x, y = c.position.y},
    target_active = remote.call('claude_interface', 'has_walk_target', '$AGENT_ID')
}))
")"
assert_json "already-reachable pole removal does not displace the NPC" "$REACHABLE_POLE_WORLD" \
    --argjson before_x "$REACHABLE_POLE_X" \
    --argjson before_y "$REACHABLE_POLE_Y" \
    '.pole_count == 0
     and ((.character_position.x - $before_x) | fabs) < 0.01
     and ((.character_position.y - $before_y) | fabs) < 0.01
     and .target_active == false'

# Build reach is also Factorio-owned. A temporary bonus creates a target that
# is beyond the old Rust constant but natively reachable; placement must not
# start a walk.
BUILD_REACH_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
c.teleport({-44.5, -20.5}, s)
c.character_build_distance_bonus = 6
c.get_main_inventory().insert{name = 'iron-chest', count = 1}
rcon.print(helpers.table_to_json({
    x = c.position.x,
    y = c.position.y,
    build_distance = c.build_distance,
    target_distance = 12,
}))
")"
require_json "build-reach fixture exceeds the retired constant but is natively reachable" "$BUILD_REACH_FIXTURE" \
    '.build_distance >= 16 and .target_distance > 10 and .target_distance < .build_distance'
BUILD_REACH_PLACE="$(mcp_tool place_entity '{"entity_name":"iron-chest","x":-32.5,"y":-20.5,"direction":"north"}')"
BUILD_REACH_PLACE_PAYLOAD="$(tool_payload "$BUILD_REACH_PLACE")"
assert_json "MCP places at native build reach without a needless walk" "$BUILD_REACH_PLACE_PAYLOAD" \
    '.name == "iron-chest" and (.unit_number | type) == "number"'
BUILD_REACH_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
local chests = s.count_entities_filtered{name = 'iron-chest', position = {-32.5, -20.5}, radius = 0.2}
c.character_build_distance_bonus = 0
rcon.print(helpers.table_to_json({
    chests = chests,
    x = c.position.x,
    y = c.position.y,
    target_active = remote.call('claude_interface', 'has_walk_target', '$AGENT_ID'),
}))
")"
assert_json "native-reachable placement preserves the NPC position" "$BUILD_REACH_WORLD" \
    '.chests == 1
     and ((.x + 44.5) | fabs) < 0.01
     and ((.y + 20.5) | fabs) < 0.01
     and .target_active == false'

# A genuinely out-of-reach entity behind a wall requires the range-aware A*
# perimeter path; a straight walk toward its occupied center gets stuck.
OBSTACLE_REACH_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
remote.call('claude_interface', 'clear_walk_target', '$AGENT_ID')
for _, entity in pairs(s.find_entities_filtered{area = {{-15, -45}, {9, -36}}}) do
    if entity.type ~= 'resource' and entity.type ~= 'character' then entity.destroy() end
end
c.teleport({-12.5, -40.5}, s)
c.get_main_inventory().clear()
local pole = s.create_entity{name = 'small-electric-pole', position = {5.5, -40.5}, force = c.force}
for y = -42, -38 do
    if not s.create_entity{name = 'stone-wall', position = {-7.5, y + 0.5}, force = c.force} then
        error('failed to create obstacle wall')
    end
end
if not pole then error('failed to create out-of-reach pole') end
rcon.print(helpers.table_to_json({
    pole_unit = pole.unit_number,
    can_reach = c.can_reach_entity(pole),
    reach_distance = c.reach_distance,
    start = {x = c.position.x, y = c.position.y},
    wall_count = s.count_entities_filtered{name = 'stone-wall', area = {{-8, -43}, {-7, -37}}},
}))
")"
require_json "out-of-reach fixture puts a real wall across the direct route" "$OBSTACLE_REACH_FIXTURE" \
    '(.pole_unit | type) == "number"
     and .can_reach == false
     and .wall_count == 5
     and .reach_distance > 0'
OBSTACLE_REACH_UNIT="$(jq -r '.pole_unit' <<<"$OBSTACLE_REACH_FIXTURE")"
OBSTACLE_REACH_REMOVE="$(mcp_tool remove_entity "$(jq -cn --argjson unit "$OBSTACLE_REACH_UNIT" '{unit_number:$unit}')")"
assert_json "range-aware A* reaches and removes the pole behind the wall" "$OBSTACLE_REACH_REMOVE" \
    '(.result.isError // false) == false
     and .result.content[0].text == "Entity removed successfully"'
OBSTACLE_REACH_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
local dx = c.position.x - (-12.5)
local dy = c.position.y - (-40.5)
rcon.print(helpers.table_to_json({
    pole_count = s.count_entities_filtered{name = 'small-electric-pole', position = {5.5, -40.5}, radius = 0.2},
    displacement = math.sqrt(dx * dx + dy * dy),
    wall_count = s.count_entities_filtered{name = 'stone-wall', area = {{-8, -43}, {-7, -37}}},
    target_active = remote.call('claude_interface', 'has_walk_target', '$AGENT_ID'),
}))
")"
assert_json "obstacle approach moves around intact walls and leaves no walk active" "$OBSTACLE_REACH_WORLD" \
    '.pole_count == 0
     and .displacement > 3
     and .wall_count == 5
     and .target_active == false'

# Standing diagnostics must use Factorio collision masks. Surface belts are
# walkable infrastructure, so neither diagnostic may report one as a blocker.
BELT_STANDING_DIAGNOSTICS="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
remote.call('claude_interface', 'clear_walk_target', '$AGENT_ID')
for _, entity in pairs(s.find_entities_filtered{area = {{-46, -26}, {-32, -14}}}) do
    if entity.type ~= 'resource' and entity.type ~= 'character' then entity.destroy() end
end
local belt = s.create_entity{
    name = 'transport-belt',
    position = {-38.5, -20.5},
    direction = defines.direction.east,
    force = c.force,
}
if not belt then error('failed to create belt-standing fixture') end
if not c.teleport(belt.position, s) then error('failed to stand character on fixture belt') end
local can_stand = helpers.json_to_table(remote.call(
    'claude_interface',
    'can_stand_at',
    '$AGENT_ID',
    c.position.x,
    c.position.y,
    4
))
local blocked = helpers.json_to_table(remote.call(
    'claude_interface',
    'is_player_blocked',
    '$AGENT_ID',
    4
))
rcon.print(helpers.table_to_json({
    belt_unit = belt.unit_number,
    can_stand = can_stand,
    blocked = blocked,
}))
")"
assert_json "belt under the NPC remains a clear standing position" "$BELT_STANDING_DIAGNOSTICS" \
    '(.belt_unit | type) == "number"
     and .can_stand.success == true
     and .can_stand.can_stand == true
     and .can_stand.blocker_count == 0
     and .blocked.success == true
     and .blocked.blocked == false
     and .blocked.can_stand_at_current_position == true
     and .blocked.blocker_count == 0'

COLLIDING_STANDING_CONTROL="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
local chest = s.create_entity{name = 'iron-chest', position = {-35.5, -20.5}, force = c.force}
if not chest then error('failed to create standing-blocker control') end
local report = helpers.json_to_table(remote.call(
    'claude_interface', 'can_stand_at', '$AGENT_ID', -35.5, -20.5, 4
))
rcon.print(helpers.table_to_json({chest_unit = chest.unit_number, report = report}))
")"
assert_json "generic collision diagnostics still identify a real chest blocker" "$COLLIDING_STANDING_CONTROL" \
    --argjson chest_unit "$(jq '.chest_unit' <<<"$COLLIDING_STANDING_CONTROL")" \
    '.report.success == true
     and .report.can_stand == false
     and .report.blocker_count > 0
     and any(.report.blockers[]; .unit_number == $chest_unit)'

INVALID_PLACE="$(mcp_tool place_entity '{"entity_name":"not-a-real-entity","x":28,"y":10,"direction":"north"}')"
assert_json "semantic MCP failures set isError" "$INVALID_PLACE" '.result.isError == true'
INVALID_ROTATION="$(mcp_tool rotate_entity '{"unit_number":1,"direction":"sideways"}')"
assert_json "invalid rotation validation sets isError" "$INVALID_ROTATION" \
    '.result.isError == true
     and (.result.content[0].text | fromjson
          | .success == false and .error_kind == "invalid_direction")'

# Inserter filtering is an exact-unit, identity-preserving mutation. Exercise
# both electric and burner prototypes, then prove a burner inserter takes only
# copper from a mixed source chest.
FILTER_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
c.teleport({19.5, -5.5})
local inv = c.get_main_inventory()
inv.insert{name = 'assembling-machine-1', count = 2}
inv.insert{name = 'burner-mining-drill', count = 2}
inv.insert{name = 'transport-belt', count = 200}
for _, entity in pairs(s.find_entities_filtered{area = {{17, -11}, {29, -2}}}) do
    if entity.type ~= 'resource' and entity.type ~= 'character' then entity.destroy() end
end
local electric = s.create_entity{
    name = 'inserter',
    position = {20.5, -5.5},
    direction = defines.direction.north,
    force = c.force,
}
electric.active = false
local burner = s.create_entity{
    name = 'burner-inserter',
    position = {25.5, -6.5},
    direction = defines.direction.north,
    force = c.force,
}
burner.active = false
local source = s.create_entity{name = 'iron-chest', position = burner.pickup_position, force = c.force}
local destination = s.create_entity{name = 'iron-chest', position = burner.drop_position, force = c.force}
source.insert{name = 'iron-plate', count = 20}
source.insert{name = 'copper-plate', count = 20}
local fuel = burner.get_fuel_inventory()
if fuel then fuel.insert{name = 'coal', count = 5} end
rcon.print(helpers.table_to_json({
    electric_unit = electric.unit_number,
    burner_unit = burner.unit_number,
    source_unit = source.unit_number,
    destination_unit = destination.unit_number
}))
")"
require_json "filter fixture creates exact standard and burner inserters" "$FILTER_FIXTURE" \
    '(.electric_unit | type) == "number"
     and (.burner_unit | type) == "number"
     and (.destination_unit | type) == "number"'
ELECTRIC_FILTER_UNIT="$(jq -r '.electric_unit' <<<"$FILTER_FIXTURE")"
BURNER_FILTER_UNIT="$(jq -r '.burner_unit' <<<"$FILTER_FIXTURE")"
FILTER_DESTINATION_UNIT="$(jq -r '.destination_unit' <<<"$FILTER_FIXTURE")"

ELECTRIC_FILTER="$(mcp_tool configure_inserter "$(jq -cn \
    --argjson unit "$ELECTRIC_FILTER_UNIT" \
    '{unit_number:$unit,allowed_items:["iron-plate","copper-plate"]}')")"
ELECTRIC_FILTER_PAYLOAD="$(tool_payload "$ELECTRIC_FILTER")"
assert_json "standard inserter whitelist is exact and read back" "$ELECTRIC_FILTER" \
    '.result.isError != true'
assert_json "standard inserter keeps identity and complete ordered filters" "$ELECTRIC_FILTER_PAYLOAD" \
    --argjson unit "$ELECTRIC_FILTER_UNIT" \
    '.readback_verified == true
     and .entity_identity_preserved == true
     and .unit_number == $unit
     and .filter_slot_count >= 2
     and .filtering_enabled == true
     and [.filters[].name] == ["iron-plate","copper-plate"]'

FILTER_BEFORE_INVALID="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local e = s.find_entities_filtered{type = 'inserter'}
local target = nil
for _, candidate in pairs(e) do if candidate.unit_number == $ELECTRIC_FILTER_UNIT then target = candidate end end
rcon.print(helpers.table_to_json({
    enabled = target and target.use_filters or false,
    first = target and target.get_filter(1) or nil,
    second = target and target.get_filter(2) or nil
}))
")"
INVALID_FILTER="$(mcp_tool configure_inserter "$(jq -cn \
    --argjson unit "$ELECTRIC_FILTER_UNIT" \
    '{unit_number:$unit,allowed_items:["copper-plate","copper-plate"]}')")"
assert_json "duplicate whitelist is rejected as a semantic tool error" "$INVALID_FILTER" \
    '.result.isError == true'
INVALID_FILTER_PAYLOAD="$(tool_payload "$INVALID_FILTER")"
assert_json "filter failure preserves the precise Lua semantic payload" "$INVALID_FILTER_PAYLOAD" \
    '.success == false
     and .error_kind == "invalid_allowed_items"
     and (.error | contains("duplicate item"))'
FILTER_AFTER_INVALID="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local target = nil
for _, candidate in pairs(s.find_entities_filtered{type = 'inserter'}) do
    if candidate.unit_number == $ELECTRIC_FILTER_UNIT then target = candidate end
end
rcon.print(helpers.table_to_json({
    enabled = target and target.use_filters or false,
    first = target and target.get_filter(1) or nil,
    second = target and target.get_filter(2) or nil
}))
")"
assert_json "rejected whitelist leaves every prior filter unchanged" "$FILTER_AFTER_INVALID" \
    --argjson before "$FILTER_BEFORE_INVALID" \
    '. == $before'

BURNER_FILTER="$(mcp_tool configure_inserter "$(jq -cn \
    --argjson unit "$BURNER_FILTER_UNIT" \
    '{unit_number:$unit,allowed_items:["copper-plate"]}')")"
BURNER_FILTER_PAYLOAD="$(tool_payload "$BURNER_FILTER")"
assert_json "burner inserter accepts the same exact filter contract" "$BURNER_FILTER_PAYLOAD" \
    --argjson unit "$BURNER_FILTER_UNIT" \
    '.readback_verified == true
     and .entity_identity_preserved == true
     and .unit_number == $unit
     and [.filters[].name] == ["copper-plate"]'
raw_lua "local s = game.surfaces['buddy-live-regression']; for _, e in pairs(s.find_entities_filtered{type = 'inserter'}) do if e.unit_number == $BURNER_FILTER_UNIT then e.active = true end end" >/dev/null
sleep 4
FILTERED_TRANSFER="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local destination = nil
for _, e in pairs(s.find_entities_filtered{name = 'iron-chest'}) do
    if e.unit_number == $FILTER_DESTINATION_UNIT then destination = e end
end
rcon.print(helpers.table_to_json({
    copper = destination and destination.get_item_count('copper-plate') or 0,
    iron = destination and destination.get_item_count('iron-plate') or 0
}))
")"
assert_json "filtered burner inserter takes only copper from mixed input" "$FILTERED_TRANSFER" \
    '.copper > 0 and .iron == 0'

CLEAR_FILTER="$(mcp_tool configure_inserter "$(jq -cn \
    --argjson unit "$ELECTRIC_FILTER_UNIT" \
    '{unit_number:$unit,allowed_items:[]}')")"
CLEAR_FILTER_PAYLOAD="$(tool_payload "$CLEAR_FILTER")"
assert_json "empty whitelist clears all slots and disables filtering" "$CLEAR_FILTER_PAYLOAD" \
    '.readback_verified == true
     and .filtering_enabled == false
     and (.filters | length) == 0'

# Resource patches are extraction capacity, not forbidden terrain. Ordinary
# Factorio-valid infrastructure may overlap them with an advisory, compact belt
# routes may cross them, and the ore must remain intact. Mining drills retain
# the one hard semantic rule: their prototype must support the resource below.
RESOURCE_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
c.teleport({37.5, -7.5})
local inv = c.get_main_inventory()
inv.insert{name = 'assembling-machine-1', count = 2}
inv.insert{name = 'burner-mining-drill', count = 3}
inv.insert{name = 'transport-belt', count = 200}
for _, entity in pairs(s.find_entities_filtered{area = {{37, -25}, {63, -3}}}) do
    if entity.type ~= 'character' then entity.destroy() end
end
-- Only the eastern edge of this assembler footprint contains ore; its center
-- tile is deliberately clear.
local assembler_ore = s.create_entity{name = 'iron-ore', position = {41.5, -7.5}, amount = 1000}
for x = 46, 51 do
    for y = -23, -18 do
        s.create_entity{name = 'copper-ore', position = {x + 0.5, y + 0.5}, amount = 1000}
    end
end
s.create_entity{name = 'iron-ore', position = {55.5, -7.5}, amount = 1000}
local incompatible_resource = s.create_entity{name = 'crude-oil', position = {60.5, -7.5}, amount = 100000}
rcon.print(helpers.table_to_json({
    assembler_before = inv.get_item_count('assembling-machine-1'),
    drill_before = inv.get_item_count('burner-mining-drill'),
    center_resources = s.count_entities_filtered{type = 'resource', position = {40.5, -7.5}, radius = 0.1},
    assembler_ore_before = assembler_ore and assembler_ore.amount or 0,
    incompatible_resource_before = incompatible_resource and incompatible_resource.amount or 0
}))
")"
require_json "resource fixture leaves assembler center clear" "$RESOURCE_FIXTURE" \
    '.assembler_before >= 1
     and .drill_before >= 3
     and .center_resources == 0
     and .assembler_ore_before == 1000
     and .incompatible_resource_before == 100000'

RESOURCE_OVERLAP_PLACE="$(mcp_tool place_entity '{"entity_name":"assembling-machine-1","x":40.5,"y":-7.5,"direction":"north"}')"
RESOURCE_OVERLAP_PAYLOAD="$(tool_payload "$RESOURCE_OVERLAP_PLACE")"
assert_json "ordinary building footprint on ore is allowed with exact advisory data" "$RESOURCE_OVERLAP_PAYLOAD" \
    '.name == "assembling-machine-1"
     and .policy_allowed == true
     and .resource_overlap == true
     and .resource_overlap_tile_count >= 1
     and .resource_advisory.kind == "resource_overlap"'
RESOURCE_OVERLAP_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
local inv = c.get_main_inventory()
local ore = s.find_entity('iron-ore', {41.5, -7.5})
rcon.print(helpers.table_to_json({
    assemblers = s.count_entities_filtered{name = 'assembling-machine-1', position = {40.5, -7.5}, radius = 0.2},
    inventory = inv.get_item_count('assembling-machine-1'),
    ore_amount = ore and ore.amount or 0
}))
")"
assert_json "ordinary overlap places the entity without consuming ore" "$RESOURCE_OVERLAP_WORLD" \
    --argjson before "$(jq '.assembler_before' <<<"$RESOURCE_FIXTURE")" \
    --argjson ore_before "$(jq '.assembler_ore_before' <<<"$RESOURCE_FIXTURE")" \
    '.assemblers == 1 and .inventory == ($before - 1) and .ore_amount == $ore_before'

raw_lua "local c = remote.call('claude_interface', 'get_character', '$AGENT_ID'); c.teleport({52.5, -7.5})" >/dev/null
EXTRACTOR_PLACE="$(mcp_tool place_entity '{"entity_name":"burner-mining-drill","x":55,"y":-8,"direction":"east"}')"
EXTRACTOR_PLACE_PAYLOAD="$(tool_payload "$EXTRACTOR_PLACE")"
assert_json "compatible mining drill keeps the resource extractor exception" "$EXTRACTOR_PLACE" \
    '.result.isError != true'
assert_json "extractor is placed with exact identity" "$EXTRACTOR_PLACE_PAYLOAD" \
    '.name == "burner-mining-drill"
     and (.unit_number | type) == "number"
     and .extractor_exception == true
     and .resource_overlap == true'

raw_lua "local c = remote.call('claude_interface', 'get_character', '$AGENT_ID'); c.teleport({57.5, -7.5})" >/dev/null
INCOMPATIBLE_EXTRACTOR="$(mcp_tool place_entity '{"entity_name":"burner-mining-drill","x":60,"y":-8,"direction":"east"}')"
assert_json "incompatible mining drill remains a semantic placement error" "$INCOMPATIBLE_EXTRACTOR" \
    '.result.isError == true'
INCOMPATIBLE_EXTRACTOR_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
local oil = s.find_entity('crude-oil', {60.5, -7.5})
rcon.print(helpers.table_to_json({
    drills = s.count_entities_filtered{name = 'burner-mining-drill', position = {60, -8}, radius = 0.2},
    inventory = c.get_main_inventory().get_item_count('burner-mining-drill'),
    oil_amount = oil and oil.amount or 0
}))
")"
assert_json "incompatible extractor rejection preserves inventory and resource" "$INCOMPATIBLE_EXTRACTOR_WORLD" \
    --argjson before "$(jq '.drill_before' <<<"$RESOURCE_FIXTURE")" \
    --argjson oil_before "$(jq '.incompatible_resource_before' <<<"$RESOURCE_FIXTURE")" \
    '.drills == 0 and .inventory == ($before - 1) and .oil_amount == $oil_before'

RESOURCE_ROUTE="$(mcp_tool route_belt '{
    "from_x":42,
    "from_y":-20,
    "to_x":55,
    "to_y":-20,
    "belt_type":"transport-belt",
    "search_radius":7,
    "dry_run":true,
    "extend_existing":true,
    "allow_underground":false,
    "respect_zones":false
}')"
RESOURCE_ROUTE_PAYLOAD="$(tool_payload "$RESOURCE_ROUTE")"
assert_json "belt planner may take the compact route across live resources" "$RESOURCE_ROUTE_PAYLOAD" \
    '.success == true
     and .resource_tiles_observed >= 36
     and .planned_surface_resource_tiles_crossed_count >= 6
     and any(.planned_new_belts[]?;
         ((.position.x | floor) >= 46
          and (.position.x | floor) <= 51
          and (.position.y | floor) >= -23
          and (.position.y | floor) <= -18))'

raw_lua "local c = remote.call('claude_interface', 'get_character', '$AGENT_ID'); c.teleport({48.5, -15.5})" >/dev/null
RESOURCE_ROUTE_EXECUTE="$(mcp_tool route_belt '{
    "from_x":42,
    "from_y":-20,
    "to_x":55,
    "to_y":-20,
    "belt_type":"transport-belt",
    "search_radius":7,
    "dry_run":false,
    "extend_existing":true,
    "allow_underground":false,
    "respect_zones":false
}')"
RESOURCE_ROUTE_EXECUTE_PAYLOAD="$(tool_payload "$RESOURCE_ROUTE_EXECUTE")"
assert_json "complete belt route executes across ore" "$RESOURCE_ROUTE_EXECUTE_PAYLOAD" \
    '.success == true
     and .complete_route == true
     and .planned_surface_resource_tiles_crossed_count >= 6
     and .placed > 0'
RESOURCE_ROUTE_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local amount = 0
for _, resource in pairs(s.find_entities_filtered{name = 'copper-ore', area = {{46, -23}, {52, -17}}}) do
    amount = amount + (resource.amount or 0)
end
rcon.print(helpers.table_to_json({
    belts_on_ore = s.count_entities_filtered{name = 'transport-belt', area = {{46, -23}, {52, -17}}},
    resource_tiles = s.count_entities_filtered{name = 'copper-ore', area = {{46, -23}, {52, -17}}},
    resource_amount = amount
}))
")"
assert_json "belt crossing leaves the live copper patch intact" "$RESOURCE_ROUTE_WORLD" \
    '.belts_on_ore >= 6 and .resource_tiles == 36 and .resource_amount == 36000'

# A drill embedded inside a dense patch has no resource-free output tile, but a
# belt is still Factorio-valid there. The edge planner must prefer a clear edge
# when one exists without turning that preference into another hard blocker.
DENSE_OUTPUT_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
for _, entity in pairs(s.find_entities_filtered{area = {{10, -42}, {24, -27}}}) do
    if entity.type ~= 'character' then entity.destroy() end
end
c.teleport({17.5, -29.5})
local inv = c.get_main_inventory()
inv.insert{name = 'burner-mining-drill', count = 1}
inv.insert{name = 'transport-belt', count = 1}
inv.insert{name = 'coal', count = 10}
for x = 10, 23 do
    for y = -42, -27 do
        s.create_entity{name = 'iron-ore', position = {x + 0.5, y + 0.5}, amount = 1000}
    end
end
rcon.print(helpers.table_to_json({
    resource_tiles = s.count_entities_filtered{name = 'iron-ore', area = {{10, -42}, {24, -26}}},
    drills = inv.get_item_count('burner-mining-drill'),
    belts = inv.get_item_count('transport-belt'),
    coal = inv.get_item_count('coal')
}))
")"
require_json "dense-output fixture surrounds the bounded drill search with ore" "$DENSE_OUTPUT_FIXTURE" \
    '.resource_tiles == 224 and .drills >= 1 and .belts >= 1 and .coal >= 10'
DENSE_OUTPUT_PLAN="$(mcp_tool execute_edge_miner '{
    "resource_type":"iron-ore",
    "x":17,
    "y":-34,
    "radius":2,
    "drill_name":"burner-mining-drill",
    "limit":10,
    "dry_run":true
}')"
DENSE_OUTPUT_PLAN_PAYLOAD="$(tool_payload "$DENSE_OUTPUT_PLAN")"
assert_json "dense-patch drill plan accepts a buildable output belt on ore" "$DENSE_OUTPUT_PLAN_PAYLOAD" \
    '.success == true
     and .dry_run == true
     and .preflight.ready == true
     and .plan.selected.output_buildable == true
     and .plan.selected.output_clear == false
     and (.plan.selected.output.overlapping_resources | length) > 0'
DENSE_OUTPUT_EXECUTE="$(mcp_tool execute_edge_miner '{
    "resource_type":"iron-ore",
    "x":17,
    "y":-34,
    "radius":2,
    "drill_name":"burner-mining-drill",
    "limit":10,
    "dry_run":false
}')"
DENSE_OUTPUT_EXECUTE_PAYLOAD="$(tool_payload "$DENSE_OUTPUT_EXECUTE")"
assert_json "dense-patch drill and its output belt execute without a clear-tile fiction" "$DENSE_OUTPUT_EXECUTE_PAYLOAD" \
    '.success == true
     and .automation_verified.success == true
     and (.placed_drill_unit_number | type) == "number"
     and (.placed_belt_unit_number | type) == "number"
     and .selected.output_buildable == true
     and .selected.output_clear == false'
DENSE_OUTPUT_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local amount = 0
for _, resource in pairs(s.find_entities_filtered{name = 'iron-ore', area = {{10, -42}, {24, -26}}}) do
    amount = amount + (resource.amount or 0)
end
rcon.print(helpers.table_to_json({
    drills = s.count_entities_filtered{name = 'burner-mining-drill', area = {{14, -37}, {21, -31}}},
    belts = s.count_entities_filtered{name = 'transport-belt', area = {{10, -42}, {24, -26}}},
    resource_tiles = s.count_entities_filtered{name = 'iron-ore', area = {{10, -42}, {24, -26}}},
    resource_amount = amount
}))
")"
assert_json "dense-patch output belt leaves the resource entities intact" "$DENSE_OUTPUT_WORLD" \
    '.drills == 1
     and .belts >= 1
     and .resource_tiles == 224
     and .resource_amount <= 224000
     and .resource_amount > 223000'

ROTATION_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
c.teleport({66.5, -7.5})
local rectangular = s.create_entity{
    name = 'steam-engine',
    position = {70.5, -7.5},
    direction = defines.direction.north,
    force = c.force,
    create_build_effect_smoke = false
}
local rectangular_ore = s.create_entity{name = 'iron-ore', position = {72.5, -7.5}, amount = 1000}
local legacy_rectangular = s.create_entity{
    name = 'steam-engine',
    position = {78.5, -7.5},
    direction = defines.direction.north,
    force = c.force,
    create_build_effect_smoke = false
}
local legacy_rectangular_ore = s.create_entity{name = 'iron-ore', position = {78.5, -7.5}, amount = 1000}
local belt_ore = s.create_entity{name = 'iron-ore', position = {70.5, -10.5}, amount = 1000}
local belt = s.create_entity{
    name = 'transport-belt',
    position = {70.5, -10.5},
    direction = defines.direction.north,
    force = c.force,
    create_build_effect_smoke = false
}
c.teleport({74.5, -7.5})
rcon.print(helpers.table_to_json({
    rectangular_unit = rectangular and rectangular.unit_number or nil,
    rectangular_direction = rectangular and rectangular.direction or nil,
    rectangular_ore_created = rectangular_ore ~= nil,
    legacy_rectangular_unit = legacy_rectangular and legacy_rectangular.unit_number or nil,
    legacy_rectangular_direction = legacy_rectangular and legacy_rectangular.direction or nil,
    legacy_rectangular_ore_created = legacy_rectangular_ore ~= nil,
    belt_unit = belt and belt.unit_number or nil,
    belt_ore_created = belt_ore ~= nil
}))
")"
require_json "rotation fixture creates rectangular and square legacy overlaps" "$ROTATION_FIXTURE" \
    '.rectangular_ore_created == true
     and .legacy_rectangular_ore_created == true
     and .belt_ore_created == true
     and (.rectangular_unit | type) == "number"
     and (.legacy_rectangular_unit | type) == "number"
     and (.belt_unit | type) == "number"'
ROTATION_RECTANGULAR_UNIT="$(jq -r '.rectangular_unit' <<<"$ROTATION_FIXTURE")"
ROTATION_LEGACY_RECTANGULAR_UNIT="$(jq -r '.legacy_rectangular_unit' <<<"$ROTATION_FIXTURE")"
ROTATION_BELT_UNIT="$(jq -r '.belt_unit' <<<"$ROTATION_FIXTURE")"

RESOURCE_ROTATION="$(mcp_tool rotate_entity "$(jq -cn \
    --argjson unit "$ROTATION_RECTANGULAR_UNIT" \
    '{unit_number:$unit,direction:"east"}')")"
assert_json "rectangular rotation that could destroy newly covered ore is rejected" "$RESOURCE_ROTATION" \
    '.result.isError == true'
RESOURCE_ROTATION_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local rectangular = nil
for _, candidate in pairs(s.find_entities_filtered{name = 'steam-engine'}) do
    if candidate.unit_number == $ROTATION_RECTANGULAR_UNIT then rectangular = candidate end
end
rcon.print(helpers.table_to_json({
    direction = rectangular and rectangular.direction or nil,
    ore = s.count_entities_filtered{name = 'iron-ore', position = {72.5, -7.5}, radius = 0.2}
}))
")"
assert_json "rejected rectangular rotation preserves entity direction and resource" "$RESOURCE_ROTATION_WORLD" \
    --argjson before "$(jq '.rectangular_direction' <<<"$ROTATION_FIXTURE")" \
    '.direction == $before and .ore == 1'

LEGACY_RESOURCE_ROTATION="$(mcp_tool rotate_entity "$(jq -cn \
    --argjson unit "$ROTATION_LEGACY_RECTANGULAR_UNIT" \
    '{unit_number:$unit,direction:"east"}')")"
assert_json "changed-footprint rotation cannot destructively revalidate existing ore" "$LEGACY_RESOURCE_ROTATION" \
    '.result.isError == true'
LEGACY_RESOURCE_ROTATION_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local rectangular = nil
for _, candidate in pairs(s.find_entities_filtered{name = 'steam-engine'}) do
    if candidate.unit_number == $ROTATION_LEGACY_RECTANGULAR_UNIT then rectangular = candidate end
end
rcon.print(helpers.table_to_json({
    direction = rectangular and rectangular.direction or nil,
    ore = s.count_entities_filtered{name = 'iron-ore', position = {78.5, -7.5}, radius = 0.2}
}))
")"
assert_json "destructive-rotation guard preserves the existing resource overlap" "$LEGACY_RESOURCE_ROTATION_WORLD" \
    --argjson before "$(jq '.legacy_rectangular_direction' <<<"$ROTATION_FIXTURE")" \
    '.direction == $before and .ore == 1'

SQUARE_ROTATION="$(mcp_tool rotate_entity "$(jq -cn \
    --argjson unit "$ROTATION_BELT_UNIT" \
    '{unit_number:$unit,direction:"east"}')")"
assert_json "square legacy overlap may rotate without expanding its resource footprint" "$SQUARE_ROTATION" \
    '.result.isError != true'
SQUARE_ROTATION_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local belt = nil
for _, candidate in pairs(s.find_entities_filtered{name = 'transport-belt'}) do
    if candidate.unit_number == $ROTATION_BELT_UNIT then belt = candidate end
end
rcon.print(helpers.table_to_json({
    direction = belt and belt.direction or nil,
    ore = s.count_entities_filtered{name = 'iron-ore', position = {70.5, -10.5}, radius = 0.2}
}))
")"
assert_json "allowed square rotation preserves the exact belt and resource" "$SQUARE_ROTATION_WORLD" \
    '.direction == 4 and .ore == 1'

DISCONNECTED_LAB="$(mcp_tool build_lab_feed "$(jq -cn \
    --argjson unit "$DISCONNECTED_LAB_UNIT" \
    '{
        lab_unit_number:$unit,
        from_x:12,
        from_y:28,
        pickup_x:14,
        pickup_y:28,
        inserter_x:17.5,
        inserter_y:28.5,
        inserter_direction:"north",
        belt_type:"transport-belt",
        search_radius:4,
        dry_run:false,
        respect_zones:false,
        allow_underground:false,
        extend_existing:true
    }')")"
DISCONNECTED_LAB_PAYLOAD="$(tool_payload "$DISCONNECTED_LAB")"
assert_json "disconnected controller geometry fails before mutation" "$DISCONNECTED_LAB" \
    '.result.isError == true'
assert_json "controller reports the failed route-inserter-machine proof" "$DISCONNECTED_LAB_PAYLOAD" \
    '.success == false
     and .error_kind == "compound_preflight_failed"
     and .preflight.ready == false
     and .preflight.endpoint_topology.success == false
     and .preflight.endpoint_topology.inserter.route_endpoint_matches == false
     and .preflight.endpoint_topology.machine.interaction_intersects_footprint == false'
DISCONNECTED_LAB_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
rcon.print(helpers.table_to_json({
    belts = s.count_entities_filtered{type = 'transport-belt', area = {{0, 24}, {21, 33}}},
    inserters = s.count_entities_filtered{type = 'inserter', area = {{0, 24}, {21, 33}}}
}))
")"
assert_json "disconnected controller leaves no pointless belts or inserters" "$DISCONNECTED_LAB_WORLD" \
    '.belts == 0 and .inserters == 0'

PRODUCTION="$(mcp_tool verify_production '{"x":16,"y":1,"radius":5}')"
PRODUCTION_PAYLOAD="$(tool_payload "$PRODUCTION")"
assert_json "production verifier rejects an idle furnace" "$PRODUCTION_PAYLOAD" \
    '.success == false and .working_count == 0 and any(.entities[]?; .name == "stone-furnace")'
assert_json "transport entities do not inflate production" "$PRODUCTION_PAYLOAD" \
    'all(.entities[]?; .name != "transport-belt" and .type != "transport-belt")'

ROUTE="$(mcp_tool route_belt '{
    "from_x":30,
    "from_y":10,
    "to_x":32,
    "to_y":10,
    "belt_type":"transport-belt",
    "search_radius":3,
    "dry_run":true,
    "extend_existing":true,
    "allow_underground":false,
    "respect_zones":false
}')"
ROUTE_PAYLOAD="$(tool_payload "$ROUTE")"
assert_json "wrong-facing existing belt is never reused as a connected segment" "$ROUTE_PAYLOAD" \
    'if .success == true then
         all(.planned_belts[]?; ((.position.x == 31.5 and .position.y == 10.5) | not))
     else
         .error_kind == "incompatible_existing_belt"
     end'

# Build a separate clean route, put a real item on its first transport line,
# and require both Factorio delivery and the static analyzer to agree. This is
# the end-to-end proof that a geometrically complete route actually transports.
DELIVERY_ROUTE="$(mcp_tool route_belt '{
    "from_x":30,
    "from_y":20,
    "to_x":38,
    "to_y":20,
    "belt_type":"transport-belt",
    "search_radius":4,
    "dry_run":false,
    "extend_existing":true,
    "allow_underground":false,
    "respect_zones":false
}')"
DELIVERY_ROUTE_PAYLOAD="$(tool_payload "$DELIVERY_ROUTE")"
assert_json "route_belt builds one complete atomic route" "$DELIVERY_ROUTE_PAYLOAD" \
    '.success == true and .complete_route == true and .placed > 0'
ITEM_INSERTED="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local belt = s.find_entity('transport-belt', {30.5, 20.5})
local inserted = belt and belt.get_transport_line(1).insert_at_back({name = 'iron-plate', count = 1}) or false
rcon.print(helpers.table_to_json({inserted = inserted}))
")"
assert_json "delivery fixture inserts an item on the route source" "$ITEM_INSERTED" '.inserted == true'
sleep 6
DELIVERED="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local belt = s.find_entity('transport-belt', {38.5, 20.5})
local count = 0
if belt then
    for line_index = 1, 2 do
        local contents = belt.get_transport_line(line_index).get_contents()
        for _, stack in pairs(contents) do
            if stack.name == 'iron-plate' then count = count + stack.count end
        end
    end
end
rcon.print(helpers.table_to_json({count = count}))
")"
assert_json "Factorio delivers the item to the requested route endpoint" "$DELIVERED" '.count >= 1'
FLOW="$(mcp_tool analyze_item_flow '{
    "source_x":30,
    "source_y":20,
    "target_x":38,
    "target_y":20,
    "radius":12
}')"
FLOW_PAYLOAD="$(tool_payload "$FLOW")"
assert_json "static analyzer agrees with Factorio item delivery" "$FLOW_PAYLOAD" \
    '.connected == true
     and .connectivity_certified == true
     and .analysis_scope.connectivity_model_complete == true
     and .target_receives_item == true
     and .first_break == null'

# Force a late verification failure in a complete two-route assembler cell.
# Every belt/inserter placed by the controller must be removed and the previous
# recipe restored; this catches controller-wide partial-build debris.
ROLLBACK_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
for _, e in pairs(s.find_entities_filtered{area = {{42, 15}, {61, 26}}}) do
    if e.type ~= 'resource' and e.type ~= 'character' then e.destroy() end
end
remote.call('claude_interface', 'clear_walk_target', '$AGENT_ID')
c.teleport({48.5, 20.5})
local inv = c.get_main_inventory()
inv.clear()
inv.insert{name = 'assembling-machine-1', count = 1}
inv.insert{name = 'transport-belt', count = 100}
inv.insert{name = 'inserter', count = 10}
local placed = helpers.json_to_table(remote.call(
    'claude_interface',
    'place_entity',
    '$AGENT_ID',
    'assembling-machine-1',
    50.5,
    20.5,
    defines.direction.north
))
local assembler = placed.unit_number and s.find_entity('assembling-machine-1', {50.5, 20.5}) or nil
if not assembler then error('failed to create registered assembler fixture') end
assembler.set_recipe('copper-cable')
c.teleport({50.5, 16.5})
rcon.print(helpers.table_to_json({unit_number = assembler.unit_number}))
")"
ROLLBACK_ASSEMBLER_UNIT="$(jq -r '.unit_number' <<<"$ROLLBACK_FIXTURE")"
CELL_PLAN="$(mcp_tool plan_recipe_assembler_cell "$(jq -cn \
    --argjson unit "$ROLLBACK_ASSEMBLER_UNIT" \
    '{
        assembler_unit_number:$unit,
        recipe:"iron-gear-wheel",
        input_item_name:"iron-plate",
        output_item_name:"iron-gear-wheel",
        input_from_x:43,
        input_from_y:20,
        output_to_x:58,
        output_to_y:20,
        input_side:"west",
        output_side:"east",
        belt_type:"transport-belt",
        search_radius:4,
        respect_zones:false,
        allow_underground:false,
        extend_existing:true,
        verify_radius:5
    }')")"
CELL_PLAN_PAYLOAD="$(tool_payload "$CELL_PLAN")"
assert_json "compound assembler cell passes one shared preflight" "$CELL_PLAN_PAYLOAD" \
    '.success == true and .compound_preflight.ready == true'
CELL_EXEC_ARGS="$(jq -c '.ready_to_call.execute_args' <<<"$CELL_PLAN_PAYLOAD")"
CELL_RESULT="$(mcp_tool build_recipe_assembler_cell "$CELL_EXEC_ARGS")"
CELL_RESULT_PAYLOAD="$(tool_payload "$CELL_RESULT")"
assert_json "late compound verification failure rolls back exact units" "$CELL_RESULT_PAYLOAD" \
    '.success == false
     and .error_kind == "verification_failed"
     and .rollback.success == true
     and (.rollback.units.removed_units | length) >= 4
     and (.rollback.units.errors | length) == 0
     and .rollback.recipe.success == true'
ROLLBACK_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local assembler = s.find_entity('assembling-machine-1', {50.5, 20.5})
local recipe = assembler and assembler.get_recipe()
rcon.print(helpers.table_to_json({
    belts = s.count_entities_filtered{type = 'transport-belt', area = {{42, 15}, {61, 26}}},
    inserters = s.count_entities_filtered{type = 'inserter', area = {{42, 15}, {61, 26}}},
    recipe = recipe and recipe.name or nil
}))
")"
assert_json "compound rollback leaves no fragments and restores recipe" "$ROLLBACK_WORLD" \
    '.belts == 0 and .inserters == 0 and .recipe == "copper-cable"'

# The same rollback must restore an actually absent recipe. Factorio rejects
# set_recipe(""); the shipped seam must explicitly send nil and verify clear.
CLEAR_RECIPE="$(mcp_tool set_recipe "$(jq -cn \
    --argjson unit "$ROLLBACK_ASSEMBLER_UNIT" \
    '{unit_number:$unit, recipe:""}')")"
assert_json "recipe clear uses the shipped nullable recipe seam" "$CLEAR_RECIPE" \
    '(.result.isError // false) == false'
CLEARED_RECIPE_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local assembler = s.find_entity('assembling-machine-1', {50.5, 20.5})
local recipe = assembler and assembler.get_recipe()
rcon.print(helpers.table_to_json({recipe = recipe and recipe.name or nil}))
")"
assert_json "assembler really has no recipe before rollback test" "$CLEARED_RECIPE_WORLD" \
    '.recipe == null'

EMPTY_RECIPE_CELL_RESULT="$(mcp_tool build_recipe_assembler_cell "$CELL_EXEC_ARGS")"
EMPTY_RECIPE_CELL_PAYLOAD="$(tool_payload "$EMPTY_RECIPE_CELL_RESULT")"
assert_json "late rollback explicitly restores an absent recipe" "$EMPTY_RECIPE_CELL_PAYLOAD" \
    '.success == false
     and .error_kind == "verification_failed"
     and .rollback.success == true
     and .rollback.recipe.success == true
     and .rollback.recipe.operation == "clear"
     and .rollback.recipe.restored_recipe == null'
EMPTY_RECIPE_ROLLBACK_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local assembler = s.find_entity('assembling-machine-1', {50.5, 20.5})
local recipe = assembler and assembler.get_recipe()
rcon.print(helpers.table_to_json({
    belts = s.count_entities_filtered{type = 'transport-belt', area = {{42, 15}, {61, 26}}},
    inserters = s.count_entities_filtered{type = 'inserter', area = {{42, 15}, {61, 26}}},
    recipe = recipe and recipe.name or nil
}))
")"
assert_json "absent-recipe rollback leaves no fragments or recipe" "$EMPTY_RECIPE_ROLLBACK_WORLD" \
    '.belts == 0 and .inserters == 0 and .recipe == null'

# Bounded inventory recovery must operate on exact existing entities without
# the destructive remove-and-replace workaround. Freeze ticks so burner fuel
# cannot begin burning between the transfer response and the conservation
# observation.
INVENTORY_PRIMITIVES_FIXTURE="$(raw_lua "
game.tick_paused = true
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
remote.call('claude_interface', 'clear_walk_target', '$AGENT_ID')
c.force = game.forces.player
c.teleport({5, -20}, game.surfaces['buddy-live-regression'])
local s = c.surface
for _, entity in pairs(s.find_entities_filtered{area = {{0, -27}, {14, -13}}}) do
    if entity.type ~= 'resource' and entity.type ~= 'character' then entity.destroy() end
end
local inv = c.get_main_inventory()
inv.clear()
local seeded_coal = inv.insert{name = 'coal', count = 7}
local drill = s.create_entity{
    name = 'burner-mining-drill',
    position = {8, -20},
    direction = defines.direction.north,
    force = c.force
}
local inserter = s.create_entity{
    name = 'burner-inserter',
    position = {4, -24},
    direction = defines.direction.north,
    force = c.force
}
local chest = s.create_entity{name = 'iron-chest', position = {3, -17}, force = c.force}
if not (drill and inserter and chest) then error('failed to construct bounded inventory fixtures') end
drill.get_fuel_inventory().clear()
inserter.get_fuel_inventory().clear()
local chest_inventory = chest.get_inventory(defines.inventory.chest)
local seeded_plates = chest_inventory.insert{name = 'iron-plate', count = 37}
local seeded_magazines = chest_inventory.insert{name = 'firearm-magazine', count = 8}
rcon.print(helpers.table_to_json({
    drill_unit = drill.unit_number,
    inserter_unit = inserter.unit_number,
    chest_unit = chest.unit_number,
    seeded_coal = seeded_coal,
    seeded_plates = seeded_plates,
    seeded_magazines = seeded_magazines
}))
")"
require_json "bounded inventory fixtures use exact existing entities" "$INVENTORY_PRIMITIVES_FIXTURE" \
    '.seeded_coal == 7
     and .seeded_plates == 37
     and .seeded_magazines == 8
     and (.drill_unit | type) == "number"
     and (.inserter_unit | type) == "number"
     and (.chest_unit | type) == "number"'
BOOTSTRAP_DRILL_UNIT="$(jq -r '.drill_unit' <<<"$INVENTORY_PRIMITIVES_FIXTURE")"
BOOTSTRAP_INSERTER_UNIT="$(jq -r '.inserter_unit' <<<"$INVENTORY_PRIMITIVES_FIXTURE")"
COLLECTION_CHEST_UNIT="$(jq -r '.chest_unit' <<<"$INVENTORY_PRIMITIVES_FIXTURE")"

CHEST_INVENTORY="$(mcp_tool get_entity_inventory "$(jq -cn \
    --argjson unit "$COLLECTION_CHEST_UNIT" \
    '{unit_number:$unit}')")"
CHEST_INVENTORY_PAYLOAD="$(tool_payload "$CHEST_INVENTORY")"
assert_json "entity inventory inspection succeeds through the model-visible seam" "$CHEST_INVENTORY" \
    '(.result.isError // false) == false'
assert_json "entity inventory inspection discovers mixed contents without guessed item names" "$CHEST_INVENTORY_PAYLOAD" \
    --argjson unit "$COLLECTION_CHEST_UNIT" \
    '.unit_number == $unit
     and .name == "iron-chest"
     and any(.inventories.chest[]; .name == "iron-plate" and .count == 37)
     and any(.inventories.chest[]; .name == "firearm-magazine" and .count == 8)'

OVER_CAP_FUEL="$(mcp_tool bootstrap_burner_once "$(jq -cn \
    --argjson unit "$BOOTSTRAP_DRILL_UNIT" \
    '{unit_number:$unit, fuel_item:"coal", count:11}')")"
OVER_CAP_FUEL_PAYLOAD="$(tool_payload "$OVER_CAP_FUEL")"
assert_json "burner bootstrap rejects fuel requests above its hard cap" "$OVER_CAP_FUEL" \
    '.result.isError == true'
assert_json "over-cap burner rejection is structured" "$OVER_CAP_FUEL_PAYLOAD" \
    '.success == false
     and .error_kind == "count_exceeds_limit"
     and .maximum_count == 10'
OVER_CAP_FUEL_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local drill
for _, entity in pairs(s.find_entities_filtered{area = {{0, -27}, {14, -13}}}) do
    if entity.unit_number == $BOOTSTRAP_DRILL_UNIT then drill = entity break end
end
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
rcon.print(helpers.table_to_json({
    same_unit = drill and drill.valid and drill.unit_number == $BOOTSTRAP_DRILL_UNIT or false,
    drill_coal = drill and drill.get_fuel_inventory().get_item_count('coal') or -1,
    character_coal = c.get_main_inventory().get_item_count('coal')
}))
")"
assert_json "over-cap burner rejection leaves entity and coal untouched" "$OVER_CAP_FUEL_WORLD" \
    '.same_unit == true and .drill_coal == 0 and .character_coal == 7'

DRILL_BOOTSTRAP="$(mcp_tool bootstrap_burner_once "$(jq -cn \
    --argjson unit "$BOOTSTRAP_DRILL_UNIT" \
    '{unit_number:$unit, fuel_item:"coal", count:3}')")"
DRILL_BOOTSTRAP_PAYLOAD="$(tool_payload "$DRILL_BOOTSTRAP")"
assert_json "burner drill bootstrap succeeds through the model-visible seam" "$DRILL_BOOTSTRAP" \
    '(.result.isError // false) == false'
assert_json "burner drill bootstrap is bounded and explicitly temporary" "$DRILL_BOOTSTRAP_PAYLOAD" \
    --argjson unit "$BOOTSTRAP_DRILL_UNIT" \
    '.success == true
     and .target.unit_number == $unit
     and .target.name == "burner-mining-drill"
     and .entity_identity_preserved == true
     and .classification == "temporary_bootstrap"
     and .temporary_bootstrap == true
     and .automation_complete == false
     and .next_action == "repair_fuel_sustainability"
     and .requested == 3
     and .inserted == 3
     and .target_before == 0
     and .target_after == 3
     and .character_after == 4
     and .conservation.balanced == true
     and .conservation.measured_balanced == true
     and .conservation.target_increase == 3
     and .conservation.character_decrease == 3'

INSERTER_BOOTSTRAP="$(mcp_tool bootstrap_burner_once "$(jq -cn \
    --argjson unit "$BOOTSTRAP_INSERTER_UNIT" \
    '{unit_number:$unit, fuel_item:"coal", count:2}')")"
INSERTER_BOOTSTRAP_PAYLOAD="$(tool_payload "$INSERTER_BOOTSTRAP")"
assert_json "burner inserter bootstrap succeeds through the same bounded seam" "$INSERTER_BOOTSTRAP" \
    '(.result.isError // false) == false'
assert_json "burner inserter bootstrap preserves exact identity and durable follow-up" "$INSERTER_BOOTSTRAP_PAYLOAD" \
    --argjson unit "$BOOTSTRAP_INSERTER_UNIT" \
    '.success == true
     and .target.unit_number == $unit
     and .target.name == "burner-inserter"
     and .entity_identity_preserved == true
     and .classification == "temporary_bootstrap"
     and .automation_complete == false
     and .action_needed == "repair_fuel_sustainability"
     and .inserted == 2
     and .target_before == 0
     and .target_after == 2
     and .character_after == 2
     and .conservation.balanced == true
     and .conservation.measured_balanced == true'

CHEST_COLLECTION="$(mcp_tool collect_from_chest "$(jq -cn \
    --argjson unit "$COLLECTION_CHEST_UNIT" \
    '{unit_number:$unit, item:"iron-plate", count:12}')")"
CHEST_COLLECTION_PAYLOAD="$(tool_payload "$CHEST_COLLECTION")"
assert_json "bounded chest collection succeeds without mining the chest" "$CHEST_COLLECTION" \
    '(.result.isError // false) == false'
assert_json "chest collection reports exact identity and item conservation" "$CHEST_COLLECTION_PAYLOAD" \
    --argjson unit "$COLLECTION_CHEST_UNIT" \
    '.success == true
     and .target.unit_number == $unit
     and .target.name == "iron-chest"
     and .entity_identity_preserved == true
     and .bounded_collection == true
     and .automation_complete == false
     and .requested == 12
     and .available_before == 37
     and .transferred == 12
     and .chest_after == 25
     and .character_before == 0
     and .character_after == 12
     and .partial == false
     and .conservation.balanced == true
     and .conservation.measured_balanced == true
     and .conservation.chest_decrease == 12
     and .conservation.character_increase == 12'
INVENTORY_PRIMITIVES_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local drill, inserter, chest
for _, entity in pairs(s.find_entities_filtered{area = {{0, -27}, {14, -13}}}) do
    if entity.unit_number == $BOOTSTRAP_DRILL_UNIT then drill = entity end
    if entity.unit_number == $BOOTSTRAP_INSERTER_UNIT then inserter = entity end
    if entity.unit_number == $COLLECTION_CHEST_UNIT then chest = entity end
end
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
rcon.print(helpers.table_to_json({
    drill_same_unit = drill and drill.valid and drill.unit_number == $BOOTSTRAP_DRILL_UNIT or false,
    inserter_same_unit = inserter and inserter.valid and inserter.unit_number == $BOOTSTRAP_INSERTER_UNIT or false,
    chest_same_unit = chest and chest.valid and chest.unit_number == $COLLECTION_CHEST_UNIT or false,
    drill_coal = drill and drill.get_fuel_inventory().get_item_count('coal') or -1,
    inserter_coal = inserter and inserter.get_fuel_inventory().get_item_count('coal') or -1,
    chest_plates = chest and chest.get_inventory(defines.inventory.chest).get_item_count('iron-plate') or -1,
    chest_magazines = chest and chest.get_inventory(defines.inventory.chest).get_item_count('firearm-magazine') or -1,
    character_coal = c.get_main_inventory().get_item_count('coal'),
    character_plates = c.get_main_inventory().get_item_count('iron-plate')
}))
")"
assert_json "bounded inventory actions preserve all three entity identities and totals" "$INVENTORY_PRIMITIVES_WORLD" \
    '.drill_same_unit == true
     and .inserter_same_unit == true
     and .chest_same_unit == true
     and .drill_coal == 3
     and .inserter_coal == 2
     and .character_coal == 2
     and .chest_plates == 25
     and .chest_magazines == 8
     and .character_plates == 12'

# Character crafting is asynchronous admission. A dedicated disposable force
# isolates the native craft-item trigger: prerequisites are fixture setup, but
# automation-science-pack itself must remain false until Factorio observes the
# lab produced by the MCP craft request.
CRAFT_TRIGGER_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local force = game.forces['buddy-live-craft'] or game.create_force('buddy-live-craft')
c.force = force
local inv = c.get_main_inventory()
inv.clear()
force.technologies['steam-power'].researched = true
force.technologies['electronics'].researched = true
force.technologies['automation-science-pack'].researched = false
force.reset_technology_effects()
force.manual_crafting_speed_modifier = 100
local recipe = force.recipes['lab']
if not (recipe and recipe.enabled) then error('lab recipe was not enabled by trigger prerequisites') end
local seeded = 0
local recipe_ingredients = {}
local statistics = force.get_item_production_statistics(c.surface)
for _, ingredient in pairs(recipe.ingredients) do
    if ingredient.type ~= 'item' then error('live lab fixture only supports item ingredients') end
    local count = math.ceil(ingredient.amount)
    seeded = seeded + inv.insert{name = ingredient.name, count = count}
    table.insert(recipe_ingredients, {
        name = ingredient.name,
        count = count,
        consumption_before = statistics.get_output_count(ingredient.name)
    })
end
rcon.print(helpers.table_to_json({
    queue_size = c.crafting_queue_size,
    seeded_ingredients = seeded,
    recipe_ingredients = recipe_ingredients,
    lab_recipe_enabled = recipe.enabled,
    trigger_researched = force.technologies['automation-science-pack'].researched,
    science_recipe_enabled = force.recipes['automation-science-pack'].enabled,
    crafted_lab_produced = force.get_item_production_statistics(c.surface).get_input_count('lab'),
    lab_count = inv.get_item_count('lab')
}))
")"
require_json "craft-trigger fixture starts before the native lab trigger" "$CRAFT_TRIGGER_FIXTURE" \
    '.queue_size == 0
     and .seeded_ingredients > 0
     and .lab_recipe_enabled == true
     and .trigger_researched == false
     and .science_recipe_enabled == false
     and .crafted_lab_produced == 0
     and .lab_count == 0
     and (.recipe_ingredients | length) > 0
     and all(.recipe_ingredients[]; .count > 0 and .consumption_before == 0)'
LAB_RECIPE_INGREDIENTS="$(jq -c '.recipe_ingredients' <<<"$CRAFT_TRIGGER_FIXTURE")"

LAB_CRAFT="$(mcp_tool craft '{"recipe":"lab","count":1}')"
LAB_CRAFT_PAYLOAD="$(tool_payload "$LAB_CRAFT")"
assert_json "craft accepts the lab request without claiming production" "$LAB_CRAFT" \
    '(.result.isError // false) == false'
assert_json "craft response is admission evidence, never completion evidence" "$LAB_CRAFT_PAYLOAD" \
    '.success == true
     and .completed == false
     and .admission.status == "queued"
     and .admission.recipe == "lab"
     and .admission.accepted_count == 1
     and .admission.remaining_queue_size > 0
     and .craft_result.success == true
     and .craft_result.queue_size > 0
     and .admission_persisted_in_save == true
     and (.operation_id | type) == "string"
     and .operation_id == .craft_result.operation_id
     and (.next_action | contains("wait_for_crafting"))'
LAB_OPERATION_ID="$(jq -r '.operation_id // empty' <<<"$LAB_CRAFT_PAYLOAD")"
LAB_CRAFT_PENDING_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local force = c.force
local inv = c.get_main_inventory()
rcon.print(helpers.table_to_json({
    queue_size = c.crafting_queue_size,
    lab_count = inv.get_item_count('lab'),
    trigger_researched = force.technologies['automation-science-pack'].researched,
    science_recipe_enabled = force.recipes['automation-science-pack'].enabled
}))
")"
assert_json "paused admitted craft has not produced a lab or fired its trigger" "$LAB_CRAFT_PENDING_WORLD" \
    '.queue_size > 0
     and .lab_count == 0
     and .trigger_researched == false
     and .science_recipe_enabled == false'

# A fresh MCP process must observe the exact transaction stored in the save.
# It must reject a second admission without overwriting the original one.
stop_mcp
start_mcp
LAB_OVERLAPPING_CRAFT="$(mcp_tool craft '{"recipe":"lab","count":1}')"
LAB_OVERLAPPING_CRAFT_PAYLOAD="$(tool_payload "$LAB_OVERLAPPING_CRAFT")"
assert_json "save-persisted admission rejects an overlapping craft after MCP restart" "$LAB_OVERLAPPING_CRAFT" \
    '.result.isError == true'
assert_json "overlap rejection preserves the original save-persisted operation" "$LAB_OVERLAPPING_CRAFT_PAYLOAD" \
    --arg operation_id "$LAB_OPERATION_ID" \
    '.success == false
     and .completed == false
     and .operation_id == $operation_id
     and .craft_result.operation_id == $operation_id
     and .craft_result.error_kind == "craft_admission_pending"
     and .admission.status == "rejected"'

LAB_CRAFT_TIMEOUT="$(mcp_tool wait_for_crafting '{"timeout_seconds":1}')"
LAB_CRAFT_TIMEOUT_PAYLOAD="$(tool_payload "$LAB_CRAFT_TIMEOUT")"
assert_json "paused crafting timeout is a semantic MCP error" "$LAB_CRAFT_TIMEOUT" \
    '.result.isError == true'
assert_json "craft timeout reports the original save-persisted queue evidence" "$LAB_CRAFT_TIMEOUT_PAYLOAD" \
    --arg operation_id "$LAB_OPERATION_ID" \
    '.success == false
     and .completed == false
     and .queue_drained == false
     and .status == "timed_out"
     and .error_kind == "crafting_timeout"
     and .operation_id == $operation_id
     and .admission_persisted_in_save == true
     and .admission_cleared == false
     and .evidence.status == "timed_out"
     and .evidence.recipe == "lab"
     and .evidence.accepted_count == 1
     and .evidence.current_recipe == "lab"
     and (.evidence.remaining_queue | length) > 0
     and .evidence.remaining_queue[0].recipe == "lab"
     and .evidence.initial_queue_size > 0
     and .evidence.remaining_queue_size > 0
     and .evidence.polls >= 1
     and any(.product_evidence[];
         .name == "lab"
         and .expected_increase == 1
         and .observed_increase == 0
         and .satisfied == false)'
LAB_CRAFT_TIMEOUT_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local force = c.force
rcon.print(helpers.table_to_json({
    queue_size = c.crafting_queue_size,
    lab_count = c.get_main_inventory().get_item_count('lab'),
    trigger_researched = force.technologies['automation-science-pack'].researched
}))
")"
assert_json "timeout does not fabricate craft completion or trigger research" "$LAB_CRAFT_TIMEOUT_WORLD" \
    '.queue_size > 0 and .lab_count == 0 and .trigger_researched == false'

raw_lua "game.tick_paused = false" >/dev/null
LAB_CRAFT_COMPLETION="$(mcp_tool wait_for_crafting '{"timeout_seconds":10}')"
LAB_CRAFT_COMPLETION_PAYLOAD="$(tool_payload "$LAB_CRAFT_COMPLETION")"
assert_json "craft wait succeeds only after the queue reaches zero" "$LAB_CRAFT_COMPLETION" \
    '(.result.isError // false) == false'
assert_json "craft completion carries an observed empty-queue proof" "$LAB_CRAFT_COMPLETION_PAYLOAD" \
    --arg operation_id "$LAB_OPERATION_ID" \
    --argjson ingredients "$LAB_RECIPE_INGREDIENTS" \
    '. as $completion
     | .success == true
     and .completed == true
     and .queue_drained == true
     and .status == "completed"
     and .error_kind == null
     and .operation_id == $operation_id
     and .admission_persisted_in_save == true
     and .admission_cleared == true
     and .terminal_receipt_persisted == true
     and .receipt_replayed == false
     and .terminal_status == "completed"
     and .clear_result.completion_receipt == true
     and .evidence.status == "completed"
     and .evidence.recipe == "lab"
     and .evidence.accepted_count == 1
     and .evidence.current_recipe == null
     and (.evidence.remaining_queue | length) == 0
     and .evidence.remaining_queue_size == 0
     and .evidence.polls >= 1
     and any(.product_evidence[];
         .name == "lab"
         and .before_count == 0
         and .expected_increase == 1
         and .expected_after_minimum == 1
         and .observed_after == 1
         and .observed_increase == 1
         and .satisfied == true)
     and .accounting.result.success == true
     and .accounting.result.accounted == true
     and .accounting.result.duplicate == false
     and .accounting.result.technology_progression == "owned_by_factorio"
     and .accounting.trigger_evaluation_ticks == 61
     and .accounting.tick_advanced == true
     and .flow_accounting_complete == true
     and any(.flows[];
         .name == "lab"
         and .produced == 1
         and .consumed == 0)
     and all($ingredients[];
         . as $expected
         | any($completion.flows[];
             .name == $expected.name
             and .produced == 0
             and .consumed == $expected.count))
     and any(.accounting.result.flows[];
         .name == "lab"
         and .produced == 1
         and .consumed == 0
         and .production_injected == 1
         and .consumption_injected == 0
         and .production_increase == 1)
     and all($ingredients[];
         . as $expected
         | any($completion.accounting.result.flows[];
             .name == $expected.name
             and .produced == 0
             and .consumed == $expected.count
             and .consumption_increase == $expected.count))'
LAB_CRAFT_COMPLETE_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local force = c.force
local inv = c.get_main_inventory()
local statistics = force.get_item_production_statistics(c.surface)
local recipe = force.recipes['lab']
local consumed_ingredients = {}
for _, ingredient in pairs(recipe.ingredients) do
    table.insert(consumed_ingredients, {
        name = ingredient.name,
        expected = math.ceil(ingredient.amount),
        observed = statistics.get_output_count(ingredient.name)
    })
end
rcon.print(helpers.table_to_json({
    queue_size = c.crafting_queue_size,
    lab_count = inv.get_item_count('lab'),
    trigger_researched = force.technologies['automation-science-pack'].researched,
    science_recipe_enabled = force.recipes['automation-science-pack'].enabled,
    crafted_lab_produced = statistics.get_input_count('lab'),
    consumed_ingredients = consumed_ingredients
}))
")"
require_json "native Factorio lab crafting fires automation-science-pack trigger" "$LAB_CRAFT_COMPLETE_WORLD" \
    --argjson ingredients "$LAB_RECIPE_INGREDIENTS" \
    '.queue_size == 0
     and .lab_count == 1
     and .crafted_lab_produced == 1
     and .trigger_researched == true
     and .science_recipe_enabled == true
     and (.consumed_ingredients | length) == ($ingredients | length)
     and all(.consumed_ingredients[]; .observed == .expected)'

# A lost MCP/RCON reply after acknowledgement must not lose the result. The
# save-owned terminal receipt is replayed by a fresh MCP process without
# accounting the craft a second time.
stop_mcp
start_mcp
LAB_CRAFT_REPLAY="$(mcp_tool wait_for_crafting '{"timeout_seconds":1}')"
LAB_CRAFT_REPLAY_PAYLOAD="$(tool_payload "$LAB_CRAFT_REPLAY")"
assert_json "completed craft receipt replays successfully after MCP restart" "$LAB_CRAFT_REPLAY" \
    '(.result.isError // false) == false'
assert_json "completion replay preserves the exact terminal operation" "$LAB_CRAFT_REPLAY_PAYLOAD" \
    --arg operation_id "$LAB_OPERATION_ID" \
    '.success == true
     and .completed == true
     and .queue_drained == true
     and .status == "completed"
     and .operation_id == $operation_id
     and .admission_cleared == true
     and .terminal_receipt_persisted == true
     and .receipt_replayed == true
     and .terminal_status == "completed"
     and .error_kind == null'

# Exact queue evidence must distinguish the requested recipe from the
# auto-queued intermediate Factorio is currently crafting.
AUTO_QUEUE_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local inv = c.get_main_inventory()
inv.clear()
local recipe = c.force.recipes['electronic-circuit']
if not (recipe and recipe.enabled) then error('electronic-circuit recipe is unavailable') end
c.force.manual_crafting_speed_modifier = 100
local iron = inv.insert{name = 'iron-plate', count = 1}
local copper = inv.insert{name = 'copper-plate', count = 3}
game.tick_paused = true
rcon.print(helpers.table_to_json({iron = iron, copper = copper, queue_size = c.crafting_queue_size}))
")"
require_json "auto-intermediate fixture starts paused with raw ingredients" "$AUTO_QUEUE_FIXTURE" \
    '.iron == 1 and .copper == 3 and .queue_size == 0'
AUTO_QUEUE_CRAFT="$(mcp_tool craft '{"recipe":"electronic-circuit","count":1}')"
AUTO_QUEUE_CRAFT_PAYLOAD="$(tool_payload "$AUTO_QUEUE_CRAFT")"
assert_json "auto-intermediate craft admission exposes its complete initial queue" "$AUTO_QUEUE_CRAFT_PAYLOAD" \
    '.success == true
     and .completed == false
     and .admission.recipe == "electronic-circuit"
     and any(.craft_result.queue[]; .recipe == "copper-cable")
     and any(.craft_result.queue[]; .recipe == "electronic-circuit")'
AUTO_QUEUE_OPERATION_ID="$(jq -r '.operation_id // empty' <<<"$AUTO_QUEUE_CRAFT_PAYLOAD")"
AUTO_QUEUE_TIMEOUT="$(mcp_tool wait_for_crafting '{"timeout_seconds":1}')"
AUTO_QUEUE_TIMEOUT_PAYLOAD="$(tool_payload "$AUTO_QUEUE_TIMEOUT")"
assert_json "paused auto-intermediate craft times out semantically" "$AUTO_QUEUE_TIMEOUT" \
    '.result.isError == true'
assert_json "timeout reports requested recipe, current intermediate, and exact remaining queue" "$AUTO_QUEUE_TIMEOUT_PAYLOAD" \
    --arg operation_id "$AUTO_QUEUE_OPERATION_ID" \
    '.success == false
     and .status == "timed_out"
     and .operation_id == $operation_id
     and .evidence.recipe == "electronic-circuit"
     and .evidence.current_recipe == "copper-cable"
     and (.evidence.remaining_queue | length) >= 2
     and .evidence.remaining_queue[0].recipe == "copper-cable"
     and any(.evidence.remaining_queue[]; .recipe == "electronic-circuit")'
raw_lua "game.tick_paused = false" >/dev/null
AUTO_QUEUE_COMPLETION="$(mcp_tool wait_for_crafting '{"timeout_seconds":10}')"
AUTO_QUEUE_COMPLETION_PAYLOAD="$(tool_payload "$AUTO_QUEUE_COMPLETION")"
require_json "auto-intermediate transaction completes with an observed empty queue" "$AUTO_QUEUE_COMPLETION_PAYLOAD" \
    '.success == true
     and .completed == true
     and .evidence.recipe == "electronic-circuit"
     and .evidence.current_recipe == null
     and (.evidence.remaining_queue | length) == 0
     and any(.flows[]; .name == "copper-cable" and .produced == 4 and .consumed == 3)
     and any(.flows[]; .name == "electronic-circuit" and .produced == 1)'

FAILED_CRAFT="$(mcp_tool craft '{"recipe":"definitely-not-a-recipe","count":1}')"
FAILED_CRAFT_PAYLOAD="$(tool_payload "$FAILED_CRAFT")"
assert_json "rejected craft is a semantic MCP error" "$FAILED_CRAFT" \
    '.result.isError == true'
assert_json "rejected craft never claims admission or completion" "$FAILED_CRAFT_PAYLOAD" \
    '.success == false
     and .completed == false
     and .operation_id == null
     and .craft_result.error_kind == "unknown_recipe"
     and .admission.status == "rejected"'

# Cancellation is operator-only fixture setup. The model-facing completion seam
# must observe the empty queue but reject completion because the admitted
# deterministic product never appeared and was never production-accounted.
CANCELLED_CRAFT_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local inv = c.get_main_inventory()
inv.clear()
local recipe = c.force.recipes['iron-gear-wheel']
if not (recipe and recipe.enabled) then error('gear recipe unavailable for cancellation fixture') end
local seeded = 0
for _, ingredient in pairs(recipe.ingredients) do
    if ingredient.type ~= 'item' then error('cancellation fixture only supports item ingredients') end
    seeded = seeded + inv.insert{name = ingredient.name, count = math.ceil(ingredient.amount)}
end
game.tick_paused = true
rcon.print(helpers.table_to_json({
    seeded_ingredients = seeded,
    queue_size = c.crafting_queue_size,
    gear_count = inv.get_item_count('iron-gear-wheel'),
    produced_gears = c.force.get_item_production_statistics(c.surface).get_input_count('iron-gear-wheel')
}))
")"
require_json "cancelled-craft fixture starts empty and paused" "$CANCELLED_CRAFT_FIXTURE" \
    '.seeded_ingredients > 0
     and .queue_size == 0
     and .gear_count == 0
     and .produced_gears == 0'

CANCELLED_CRAFT="$(mcp_tool craft '{"recipe":"iron-gear-wheel","count":1}')"
CANCELLED_CRAFT_PAYLOAD="$(tool_payload "$CANCELLED_CRAFT")"
assert_json "cancellation fixture admits one real craft" "$CANCELLED_CRAFT_PAYLOAD" \
    '.success == true
     and .completed == false
     and .admission.status == "queued"
     and .admission.accepted_count == 1
     and .admission_persisted_in_save == true
     and (.operation_id | type) == "string"'
CANCELLED_OPERATION_ID="$(jq -r '.operation_id // empty' <<<"$CANCELLED_CRAFT_PAYLOAD")"
raw_lua "local c = remote.call('claude_interface', 'get_character', '$AGENT_ID'); c.cancel_crafting{index = 1, count = 1}" >/dev/null
CANCELLED_COMPLETION="$(mcp_tool wait_for_crafting '{"timeout_seconds":2}')"
CANCELLED_COMPLETION_PAYLOAD="$(tool_payload "$CANCELLED_COMPLETION")"
assert_json "cancelled craft completion is a semantic MCP error" "$CANCELLED_COMPLETION" \
    '.result.isError == true'
assert_json "cancelled craft cannot fabricate output or production accounting" "$CANCELLED_COMPLETION_PAYLOAD" \
    --arg operation_id "$CANCELLED_OPERATION_ID" \
    '.success == false
     and .completed == false
     and .queue_drained == true
     and .status == "output_missing"
     and .error_kind == "craft_output_missing"
     and .operation_id == $operation_id
     and .admission_persisted_in_save == true
     and .admission_cleared == true
     and .accounting == null
     and any(.product_evidence[];
         .name == "iron-gear-wheel"
         and .expected_increase == 1
         and .observed_increase == 0
         and .satisfied == false)'
CANCELLED_CRAFT_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
game.tick_paused = false
rcon.print(helpers.table_to_json({
    queue_size = c.crafting_queue_size,
    gear_count = c.get_main_inventory().get_item_count('iron-gear-wheel'),
    produced_gears = c.force.get_item_production_statistics(c.surface).get_input_count('iron-gear-wheel')
}))
")"
require_json "cancelled craft leaves no output or production credit" "$CANCELLED_CRAFT_WORLD" \
    '.queue_size == 0 and .gear_count == 0 and .produced_gears == 0'
CANCELLED_ADMISSION_AFTER="$(raw_lua \
    "rcon.print(remote.call('claude_interface', 'get_craft_admission', '$AGENT_ID'))")"
assert_json "terminal cancellation persists the exact replayable failure receipt" "$CANCELLED_ADMISSION_AFTER" \
    --arg operation_id "$CANCELLED_OPERATION_ID" \
    '.operation_id == $operation_id
     and .completion_receipt == true
     and .terminal_status == "craft_output_missing"'
CANCELLED_REPLAY="$(mcp_tool wait_for_crafting '{"timeout_seconds":1}')"
CANCELLED_REPLAY_PAYLOAD="$(tool_payload "$CANCELLED_REPLAY")"
assert_json "cancelled craft receipt replays as the same semantic failure" "$CANCELLED_REPLAY" \
    '.result.isError == true'
assert_json "cancelled craft replay never fabricates completion" "$CANCELLED_REPLAY_PAYLOAD" \
    --arg operation_id "$CANCELLED_OPERATION_ID" \
    '.success == false
     and .completed == false
     and .status == "terminal_failure"
     and .operation_id == $operation_id
     and .receipt_replayed == true
     and .terminal_status == "craft_output_missing"
     and .error_kind == "craft_output_missing"'

# Produce the initial red packs by character crafting, then exercise the one
# mandatory hand-feed bootstrap. The lab itself is fixture setup; both dry-run
# validation and the transfer go through the model-visible MCP tool.
SCIENCE_CRAFT_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local inv = c.get_main_inventory()
inv.clear()
local recipe = c.force.recipes['automation-science-pack']
if not (recipe and recipe.enabled) then error('automation science recipe was not natively unlocked') end
local seeded = 0
for _, ingredient in pairs(recipe.ingredients) do
    if ingredient.type ~= 'item' then error('live science fixture only supports item ingredients') end
    local count = math.ceil(ingredient.amount * 3)
    seeded = seeded + inv.insert{name = ingredient.name, count = count}
end
rcon.print(helpers.table_to_json({seeded_ingredients = seeded, recipe_enabled = recipe.enabled}))
")"
require_json "initial science ingredients are seeded only after native unlock" "$SCIENCE_CRAFT_FIXTURE" \
    '.seeded_ingredients > 0 and .recipe_enabled == true'
SCIENCE_CRAFT="$(mcp_tool craft '{"recipe":"automation-science-pack","count":3}')"
SCIENCE_CRAFT_PAYLOAD="$(tool_payload "$SCIENCE_CRAFT")"
assert_json "initial automation science is admitted through character crafting" "$SCIENCE_CRAFT_PAYLOAD" \
    '.success == true
     and .completed == false
     and (.admission.status == "queued" or .admission.status == "accepted")
     and .admission.accepted_count == 3'
SCIENCE_CRAFT_COMPLETION="$(mcp_tool wait_for_crafting '{"timeout_seconds":10}')"
SCIENCE_CRAFT_COMPLETION_PAYLOAD="$(tool_payload "$SCIENCE_CRAFT_COMPLETION")"
require_json "initial automation science character craft completes" "$SCIENCE_CRAFT_COMPLETION_PAYLOAD" \
    '.success == true
     and .completed == true
     and .queue_drained == true
     and .status == "completed"
     and .evidence.status == "completed"
     and .evidence.recipe == "automation-science-pack"
     and .evidence.accepted_count == 3
     and .evidence.remaining_queue_size == 0
     and any(.product_evidence[];
         .name == "automation-science-pack"
         and .before_count == 0
         and .expected_increase == 3
         and .expected_after_minimum == 3
         and .observed_after == 3
         and .observed_increase == 3
         and .satisfied == true)
     and .accounting.result.accounted == true
     and .accounting.result.technology_progression == "owned_by_factorio"
     and .flow_accounting_complete == true
     and (.flows | length) > 1
     and .admission_cleared == true
     and .accounting.tick_advanced == true'

LAB_FEED_FIXTURE="$(raw_lua "
game.tick_paused = true
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
remote.call('claude_interface', 'clear_walk_target', '$AGENT_ID')
c.teleport({21, -20}, game.surfaces['buddy-live-regression'])
local s = c.surface
for _, entity in pairs(s.find_entities_filtered{area = {{18, -25}, {29, -15}}}) do
    if entity.type ~= 'resource' and entity.type ~= 'character' then entity.destroy() end
end
local lab = s.create_entity{name = 'lab', position = {25, -20}, force = c.force}
if not lab then error('failed to construct exact lab feed fixture') end
local lab_inv = lab.get_inventory(defines.inventory.lab_input)
rcon.print(helpers.table_to_json({
    lab_unit = lab.unit_number,
    character_packs = c.get_main_inventory().get_item_count('automation-science-pack'),
    lab_packs = lab_inv.get_item_count('automation-science-pack')
}))
")"
require_json "hand-feed fixture has three crafted packs and one exact empty lab" "$LAB_FEED_FIXTURE" \
    '.character_packs == 3
     and .lab_packs == 0
     and (.lab_unit | type) == "number"'
LAB_FEED_UNIT="$(jq -r '.lab_unit' <<<"$LAB_FEED_FIXTURE")"

LAB_FEED_DRY_RUN="$(mcp_tool feed_lab_from_inventory "$(jq -cn \
    --argjson unit "$LAB_FEED_UNIT" \
    '{lab_unit_number:$unit, science_pack:"automation-science-pack", count:2, dry_run:true}')")"
LAB_FEED_DRY_RUN_PAYLOAD="$(tool_payload "$LAB_FEED_DRY_RUN")"
assert_json "lab hand-feed dry-run is non-erroring and executable" "$LAB_FEED_DRY_RUN" \
    '(.result.isError // false) == false'
assert_json "lab hand-feed dry-run returns the exact guarded execution step" "$LAB_FEED_DRY_RUN_PAYLOAD" \
    --argjson unit "$LAB_FEED_UNIT" \
    '.success == true
     and .dry_run == true
     and .classification == "bootstrap_science_transfer"
     and .bootstrap == true
     and .automation_complete == false
     and .ready == true
     and .manual_bootstrap_available == true
     and .lab_unit_number == $unit
     and .available == 3
     and .lab_before == 0
     and .inserted == 0
     and .ready_to_call.tool == "feed_lab_from_inventory"
     and .ready_to_call.args.lab_unit_number == $unit
     and .ready_to_call.args.science_pack == "automation-science-pack"
     and .ready_to_call.args.count == 2
     and .ready_to_call.args.dry_run == false
     and .next_action == "feed_lab_from_inventory"
     and .follow_up_action == "automate_science_delivery"'
LAB_FEED_DRY_RUN_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
local lab
for _, entity in pairs(s.find_entities_filtered{area = {{18, -25}, {29, -15}}}) do
    if entity.unit_number == $LAB_FEED_UNIT then lab = entity break end
end
rcon.print(helpers.table_to_json({
    same_unit = lab and lab.valid and lab.unit_number == $LAB_FEED_UNIT or false,
    character_packs = c.get_main_inventory().get_item_count('automation-science-pack'),
    lab_packs = lab and lab.get_inventory(defines.inventory.lab_input).get_item_count('automation-science-pack') or -1
}))
")"
assert_json "lab hand-feed dry-run leaves both inventories unchanged" "$LAB_FEED_DRY_RUN_WORLD" \
    '.same_unit == true and .character_packs == 3 and .lab_packs == 0'

LAB_FEED_EXECUTION="$(mcp_tool feed_lab_from_inventory "$(jq -cn \
    --argjson unit "$LAB_FEED_UNIT" \
    '{lab_unit_number:$unit, science_pack:"automation-science-pack", count:2, dry_run:false}')")"
LAB_FEED_EXECUTION_PAYLOAD="$(tool_payload "$LAB_FEED_EXECUTION")"
assert_json "initial science hand-feed executes through the model-visible seam" "$LAB_FEED_EXECUTION" \
    '(.result.isError // false) == false'
assert_json "initial science hand-feed reports the exact bounded transfer" "$LAB_FEED_EXECUTION_PAYLOAD" \
    --argjson unit "$LAB_FEED_UNIT" \
    '.success == true
     and .dry_run == false
     and .classification == "bootstrap_science_transfer"
     and .bootstrap == true
     and .automation_complete == false
     and .lab_unit_number == $unit
     and .lab_identity_preserved == true
     and .science_pack == "automation-science-pack"
     and .requested_count == 2
     and .available == 3
     and .lab_before == 0
     and .inserted == 2
     and .returned_to_inventory == 0
     and .lab_after == 2
     and .inventory_after == 1
     and .conservation.removed == 2
     and .conservation.inserted == 2
     and .conservation.returned == 0
     and .conservation.balanced == true
     and .conservation.lab_increase == 2
     and .conservation.character_decrease == 2
     and .conservation.measured_balanced == true
     and .next_action == "get_research_status"
     and (.follow_up_actions | index("build_automation_science")) != null
     and (.follow_up_actions | index("build_lab_feed")) != null'
LAB_FEED_EXECUTION_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
local lab
for _, entity in pairs(s.find_entities_filtered{area = {{18, -25}, {29, -15}}}) do
    if entity.unit_number == $LAB_FEED_UNIT then lab = entity break end
end
rcon.print(helpers.table_to_json({
    same_unit = lab and lab.valid and lab.unit_number == $LAB_FEED_UNIT or false,
    character_packs = c.get_main_inventory().get_item_count('automation-science-pack'),
    lab_packs = lab and lab.get_inventory(defines.inventory.lab_input).get_item_count('automation-science-pack') or -1,
    conserved_total = c.get_main_inventory().get_item_count('automation-science-pack')
        + (lab and lab.get_inventory(defines.inventory.lab_input).get_item_count('automation-science-pack') or 0)
}))
")"
assert_json "initial science hand-feed preserves exact lab identity and pack total" "$LAB_FEED_EXECUTION_WORLD" \
    '.same_unit == true
     and .character_packs == 1
     and .lab_packs == 2
     and .conserved_total == 3'

# Restore ordinary runtime state before the remaining shared-transport probe.
raw_lua "
game.tick_paused = false
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
c.force = game.forces.player
c.teleport({28.5, 10.5}, game.surfaces['buddy-live-regression'])
" >/dev/null

# Invalid model-authored issue fields must fail validation before `bd create`.
# Snapshot the full issue identity/state projection around the MCP call so this
# live test cannot silently mutate the real repository tracker.
BEADS_BEFORE="$(beads_issue_snapshot)"
INVALID_ISSUE="$(mcp_tool file_issue '{
    "title":"",
    "observed_behavior":"This request must be rejected before issue creation.",
    "expected_behavior":"No issue is created for an empty title.",
    "evidence":["live-regression-invalid-input"],
    "labels":["mcp"],
    "priority":2
}')"
INVALID_ISSUE_PAYLOAD="$(tool_payload "$INVALID_ISSUE")"
assert_json "invalid file_issue input is a semantic MCP error" "$INVALID_ISSUE" \
    '.result.isError == true'
assert_json "invalid file_issue input reports bounded validation failure" "$INVALID_ISSUE_PAYLOAD" \
    '.success == false and .error_kind == "invalid_issue_report"'
BEADS_AFTER="$(beads_issue_snapshot)"
if [[ "$BEADS_AFTER" == "$BEADS_BEFORE" ]]; then
    pass "invalid file_issue cannot mutate the real Beads issue set"
else
    fail "invalid file_issue cannot mutate the real Beads issue set" \
        "Beads issue identity/state projection changed"
fi

# Multiple tools in one MCP process must share its RCON transport.
RCON_BEFORE="$(rcon_connection_count)"
mcp_tool get_power_status '{"x":0,"y":0,"radius":5}' >/dev/null
mcp_tool get_power_status '{"x":0,"y":0,"radius":5}' >/dev/null
RCON_AFTER="$(rcon_connection_count)"
RCON_DELTA=$((RCON_AFTER - RCON_BEFORE))
if (( RCON_DELTA == 0 )); then
    pass "one MCP process reuses one RCON connection"
else
    fail "one MCP process reuses one RCON connection" \
        "observed $RCON_DELTA new connections for one MCP session"
fi

stop_mcp

printf '\n=== live regression summary ===\n'
printf '  Passed: %d\n' "$PASSED"
printf '  Failed: %d\n' "$FAILED"

if (( FAILED > 0 )); then
    exit 1
fi
