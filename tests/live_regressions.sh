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
require_json "fuel repair reuses an existing drill seed instead of demanding five more coal" "$BUFFERED_DRILL_REPORT" \
    --argjson drill "$BUFFERED_DRILL_UNIT" \
    '.report.consumers[]
     | select(.unit_number == $drill)
     | .remaining_burning_fuel > 0
       and .ready_to_call.transaction_args.provisional_source_unit_number == $drill
       and .ready_to_call.transaction_args.bootstrap_consumer_fuel_count == 0'

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

# A cold burner coal drill is the one safe provisional-source case: the same
# drill may seed itself briefly while one transaction closes its output back
# into its fuel inventory. Reuse the isolated negative-fixture area, but remove
# every old entity/resource first so no stocked belt or second producer can
# satisfy the proof accidentally.
COLD_COAL_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
local area = {{-64, 12}, {-46, 31}}
for _, entity in pairs(s.find_entities_filtered{area = area}) do
    if entity.type ~= 'character' then entity.destroy() end
end
c.teleport({-60, 18}, s)
local inv = c.get_main_inventory()
inv.clear()
local seeded_coal = inv.insert{name = 'coal', count = 12}
local seeded_belts = inv.insert{name = 'transport-belt', count = 16}
local seeded_inserters = inv.insert{name = 'burner-inserter', count = 1}
for x = -58, -55 do
    for y = 21, 24 do
        s.create_entity{name = 'coal', position = {x + 0.5, y + 0.5}, amount = 100000}
    end
end
local drill = s.create_entity{
    name = 'burner-mining-drill',
    position = {-56, 23},
    direction = defines.direction.east,
    force = c.force
}
if not drill then
    rcon.print(helpers.table_to_json({error = 'cold burner coal drill creation failed'}))
    return
end
drill.get_fuel_inventory().clear()
local status = nil
for name, value in pairs(defines.entity_status) do
    if value == drill.status then status = name break end
end
rcon.print(helpers.table_to_json({
    drill_unit = drill.unit_number,
    drill_position = drill.position,
    drill_drop_position = drill.drop_position,
    mining_target = drill.mining_target and drill.mining_target.name or nil,
    status = status,
    drill_fuel = drill.get_fuel_inventory().get_item_count(),
    seeded_coal = seeded_coal,
    seeded_belts = seeded_belts,
    seeded_inserters = seeded_inserters,
    resource_tiles = s.count_entities_filtered{name = 'coal', area = area}
}))
")"
require_json "cold burner coal fixture has exactly one empty provisional producer" "$COLD_COAL_FIXTURE" \
    '.error == null
     and (.drill_unit | type) == "number"
     and .drill_position == {x:-56.0, y:23.0}
     and .status == "no_fuel"
     and .drill_fuel == 0
     and .seeded_coal == 12
     and .seeded_belts == 16
     and .seeded_inserters == 1
     and .resource_tiles == 16'
COLD_COAL_DRILL_UNIT="$(jq -r '.drill_unit' <<<"$COLD_COAL_FIXTURE")"
COLD_COAL_ARGS="$(jq -cn \
    --argjson x -56 \
    --argjson y 23 \
    '{x:$x, y:$y, radius:8, limit:10, search_radius:8, dry_run:true,
      respect_zones:false, allow_underground:false, extend_existing:true,
      verify_radius:8}')"

start_mcp

# Generic placement must use the same collision legality as an ordinary player
# build. Direct creation is fixture setup only; every attempted overlap goes
# through the shipped placement remote or the model-facing safe-near executor.
COLLISION_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
c.teleport({22.5, 0.5})
local inv = c.get_main_inventory()
local inserters_original = inv.get_item_count('inserter')
local furnaces_original = inv.get_item_count('stone-furnace')
inv.insert{name = 'inserter', count = 3}
inv.insert{name = 'stone-furnace', count = 3}
for _, entity in pairs(s.find_entities_filtered{area = {{20, -3}, {45, 6}}}) do
    if entity.type ~= 'resource' and entity.type ~= 'character' then entity.destroy() end
end
local inserter = s.create_entity{
    name = 'inserter', position = {24.5, 0.5}, direction = defines.direction.east, force = c.force,
}
local belt = s.create_entity{
    name = 'transport-belt', position = {28.5, 0.5}, direction = defines.direction.east, force = c.force,
}
local furnace = s.create_entity{
    name = 'stone-furnace', position = {33, 1}, force = c.force,
}
local planned_belt = s.create_entity{
    name = 'transport-belt', position = {39.5, 0.5}, direction = defines.direction.east, force = c.force,
}
rcon.print(helpers.table_to_json({
    inserter_unit = inserter and inserter.unit_number or nil,
    belt_unit = belt and belt.unit_number or nil,
    furnace_unit = furnace and furnace.unit_number or nil,
    planned_belt_unit = planned_belt and planned_belt.unit_number or nil,
    inserters_original = inserters_original,
    furnaces_original = furnaces_original,
    inserters_before = inv.get_item_count('inserter'),
    furnaces_before = inv.get_item_count('stone-furnace'),
}))
")"
require_json "collision fixture contains all reported blocker shapes" "$COLLISION_FIXTURE" \
    '(.inserter_unit | type) == "number"
     and (.belt_unit | type) == "number"
     and (.furnace_unit | type) == "number"
     and (.planned_belt_unit | type) == "number"
     and .inserters_before == (.inserters_original + 3)
     and .furnaces_before == (.furnaces_original + 3)'

STACKED_INSERTER="$(raw_lua "rcon.print(remote.call('claude_interface', 'place_entity', '$AGENT_ID', 'inserter', 24.5, 0.5, defines.direction.east))")"
assert_json "place_entity rejects inserter-on-inserter collision with exact blocker" "$STACKED_INSERTER" \
    --argjson blocker "$(jq '.inserter_unit' <<<"$COLLISION_FIXTURE")" \
    '.success == false
     and .can_place == false
     and .occupied_by.unit_number == $blocker
     and any(.blockers[]?; .unit_number == $blocker)'

raw_lua "local c = remote.call('claude_interface', 'get_character', '$AGENT_ID'); c.teleport({26.5, 3.5})" >/dev/null
FURNACE_ON_BELT="$(raw_lua "rcon.print(remote.call('claude_interface', 'place_entity', '$AGENT_ID', 'stone-furnace', 28, 1, defines.direction.north))")"
assert_json "place_entity rejects furnace-on-belt collision with exact blocker" "$FURNACE_ON_BELT" \
    --argjson blocker "$(jq '.belt_unit' <<<"$COLLISION_FIXTURE")" \
    '.success == false
     and .can_place == false
     and any(.blockers[]?; .unit_number == $blocker)'

raw_lua "local c = remote.call('claude_interface', 'get_character', '$AGENT_ID'); c.teleport({30.5, 4.5})" >/dev/null
INSERTER_ON_FURNACE="$(raw_lua "rcon.print(remote.call('claude_interface', 'place_entity', '$AGENT_ID', 'inserter', 32.5, 0.5, defines.direction.north))")"
assert_json "place_entity rejects inserter-on-furnace collision with exact blocker" "$INSERTER_ON_FURNACE" \
    --argjson blocker "$(jq '.furnace_unit' <<<"$COLLISION_FIXTURE")" \
    '.success == false
     and .can_place == false
     and any(.blockers[]?; .unit_number == $blocker)'

raw_lua "local c = remote.call('claude_interface', 'get_character', '$AGENT_ID'); c.teleport({36.5, 4.5})" >/dev/null
BLOCKED_NEAR_EXECUTION="$(mcp_tool execute_entity_placement_near '{
    "entity_name":"stone-furnace",
    "x":39,
    "y":1,
    "radius":1,
    "limit":10,
    "dry_run":false
}')"
BLOCKED_NEAR_PAYLOAD="$(tool_payload "$BLOCKED_NEAR_EXECUTION")"
assert_json "safe-near executor skips the occupied target and selects a valid neighbor" "$BLOCKED_NEAR_PAYLOAD" \
    '.success == true
     and .placement_success == true
     and .plan.success == true
     and .selected.distance > 0
     and .selected.position != {x:39, y:1}'

COLLISION_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
local inv = c.get_main_inventory()
rcon.print(helpers.table_to_json({
    stacked_inserters = s.count_entities_filtered{name = 'inserter', position = {24.5, 0.5}, radius = 0.1},
    furnaces_on_belt = s.count_entities_filtered{name = 'stone-furnace', position = {28, 1}, radius = 0.1},
    inserters_on_furnace = s.count_entities_filtered{name = 'inserter', position = {32.5, 0.5}, radius = 0.1},
    planned_target_furnaces = s.count_entities_filtered{name = 'stone-furnace', position = {39, 1}, radius = 0.1},
    planned_neighbor_furnaces = s.count_entities_filtered{name = 'stone-furnace', area = {{37.2, -0.8}, {40.8, 2.8}}},
    inserters_after = inv.get_item_count('inserter'),
    furnaces_after = inv.get_item_count('stone-furnace'),
}))
")"
assert_json "collision rejections preserve state and safe rerouting accounts for one furnace" "$COLLISION_WORLD" \
    --argjson inserters "$(jq '.inserters_before' <<<"$COLLISION_FIXTURE")" \
    --argjson furnaces "$(jq '.furnaces_before' <<<"$COLLISION_FIXTURE")" \
    '.stacked_inserters == 1
     and .furnaces_on_belt == 0
     and .inserters_on_furnace == 0
     and .planned_target_furnaces == 0
     and .planned_neighbor_furnaces == 1
     and .inserters_after == $inserters
     and .furnaces_after == ($furnaces - 1)'
raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
local inv = c.get_main_inventory()
local inserter_target = $(jq '.inserters_original' <<<"$COLLISION_FIXTURE")
local furnace_target = $(jq '.furnaces_original' <<<"$COLLISION_FIXTURE")
local inserter_delta = inv.get_item_count('inserter') - inserter_target
local furnace_delta = inv.get_item_count('stone-furnace') - furnace_target
if inserter_delta > 0 then
    inv.remove{name = 'inserter', count = inserter_delta}
elseif inserter_delta < 0 then
    inv.insert{name = 'inserter', count = -inserter_delta}
end
if furnace_delta > 0 then
    inv.remove{name = 'stone-furnace', count = furnace_delta}
elseif furnace_delta < 0 then
    inv.insert{name = 'stone-furnace', count = -furnace_delta}
end
for _, entity in pairs(s.find_entities_filtered{area = {{20, -3}, {45, 6}}}) do
    if entity.type ~= 'resource' and entity.type ~= 'character' then entity.destroy() end
end
" >/dev/null

COLD_COAL_DRY="$(mcp_tool repair_fuel_sustainability "$COLD_COAL_ARGS")"
COLD_COAL_DRY_PAYLOAD="$(tool_payload "$COLD_COAL_DRY")"
assert_json "cold coal repair dry-run succeeds through the model-visible seam" "$COLD_COAL_DRY" \
    '(.result.isError // false) == false'
require_json "cold coal dry-run selects only the exact provisional producer" "$COLD_COAL_DRY_PAYLOAD" \
    --argjson unit "$COLD_COAL_DRILL_UNIT" \
    '.success == true
     and .dry_run == true
     and .selected_transaction.consumer_unit_number == $unit
     and .selected_transaction.provisional_source_unit_number == $unit
     and .selected_transaction.bootstrap_consumer_fuel_count == 5
     and .selected_transaction.inserter_fuel_count == 5
     and .repair.success == true
     and .repair.dry_run == true
     and .repair.preflight.ready == true
     and .repair.preflight.bootstrap_fuel == {
         item:"coal",
         available:12,
         required:10,
         inserter_required:5,
         source_tap_required:0,
         consumer_required:5,
         ready:true
     }
     and any(.diagnosis.consumers[]?;
         .unit_number == $unit
         and .ready_to_call.transaction_args.consumer_unit_number == $unit
         and .ready_to_call.transaction_args.provisional_source_unit_number == $unit
         and .ready_to_call.transaction_args.bootstrap_consumer_fuel_count == 5)'
COLD_COAL_DRY_BYTES="$(printf '%s' "$COLD_COAL_DRY_PAYLOAD" | wc -c)"
if (( COLD_COAL_DRY_BYTES <= 65536 )); then
    pass "cold coal repair dry-run stays below the 64 KiB model payload bound"
else
    fail "cold coal repair dry-run stays below the 64 KiB model payload bound" \
        "observed $COLD_COAL_DRY_BYTES bytes"
fi

COLD_COAL_DRY_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
local drill
for _, entity in pairs(s.find_entities_filtered{area = {{-64, 12}, {-46, 31}}}) do
    if entity.unit_number == $COLD_COAL_DRILL_UNIT then drill = entity break end
end
rcon.print(helpers.table_to_json({
    same_drill = drill and drill.valid and drill.unit_number == $COLD_COAL_DRILL_UNIT or false,
    drill_fuel = drill and drill.get_fuel_inventory().get_item_count() or -1,
    belts = s.count_entities_filtered{type = 'transport-belt', area = {{-64, 12}, {-46, 31}}},
    inserters = s.count_entities_filtered{type = 'inserter', area = {{-64, 12}, {-46, 31}}},
    character_coal = c.get_main_inventory().get_item_count('coal'),
    character_belts = c.get_main_inventory().get_item_count('transport-belt'),
    character_inserters = c.get_main_inventory().get_item_count('burner-inserter')
}))
")"
require_json "cold coal dry-run leaves the disposable world untouched" "$COLD_COAL_DRY_WORLD" \
    '.same_drill == true
     and .drill_fuel == 0
     and .belts == 0
     and .inserters == 0
     and .character_coal == 12
     and .character_belts == 16
     and .character_inserters == 1'

COLD_COAL_EXEC_ARGS="$(jq -c '.dry_run = false' <<<"$COLD_COAL_ARGS")"
COLD_COAL_EXEC="$(mcp_tool repair_fuel_sustainability "$COLD_COAL_EXEC_ARGS")"
COLD_COAL_EXEC_PAYLOAD="$(tool_payload "$COLD_COAL_EXEC")"
assert_json "cold coal repair executes through one high-level controller call" "$COLD_COAL_EXEC" \
    '(.result.isError // false) == false'
require_json "cold coal repair bootstraps and verifies the exact closed loop" "$COLD_COAL_EXEC_PAYLOAD" \
    --argjson unit "$COLD_COAL_DRILL_UNIT" \
    '.success == true
     and .dry_run == false
     and .selected_transaction.consumer_unit_number == $unit
     and .selected_transaction.provisional_source_unit_number == $unit
     and .selected_transaction.bootstrap_consumer_fuel_count == 5
     and .repair.success == true
     and .repair.consumer.unit_number == $unit
     and .repair.route.success == true
     and .repair.inserter.success == true
     and .repair.bootstrap_fuel.success == true
     and .repair.bootstrap_fuel.count == 5
     and .repair.bootstrap_consumer_fuel.success == true
     and .repair.bootstrap_consumer_fuel.unit_number == $unit
     and .repair.bootstrap_consumer_fuel.count == 5
     and .repair.infrastructure_verified.success == true
     and .repair.infrastructure_verified.durable_fuel_topology.consumer_unit_number == $unit
     and .repair.infrastructure_verified.durable_fuel_topology.exact_connection_present == true
     and .repair.infrastructure_verified.durable_fuel_topology.durable_connection_verified == true
     and .repair.infrastructure_verified.durable_fuel_topology.structural_success == true
     and .repair.infrastructure_verified.delivery_observation.success == true
     and .repair.infrastructure_verified.delivery_observation.delivery_path_operational == true
     and (.repair.infrastructure_verified.delivery_observation.terminal_coal_observed == true
          or .repair.infrastructure_verified.delivery_observation.exact_feeder_transfer_observed == true)
     and (.repair.infrastructure_verified.durable_fuel_topology.proof_reasons
         | index("closed_self_sustaining_coal_cycle")) != null
     and .repair.automation_verified.infrastructure_success == true
     and .repair.automation_verified.fuel_supply_live == true'
COLD_COAL_EXEC_BYTES="$(printf '%s' "$COLD_COAL_EXEC_PAYLOAD" | wc -c)"
if (( COLD_COAL_EXEC_BYTES <= 65536 )); then
    pass "cold coal repair execution stays below the 64 KiB model payload bound"
else
    fail "cold coal repair execution stays below the 64 KiB model payload bound" \
        "observed $COLD_COAL_EXEC_BYTES bytes"
fi
COLD_COAL_INSERTER_UNIT="$(jq -r '.repair.inserter.unit_number' <<<"$COLD_COAL_EXEC_PAYLOAD")"

COLD_COAL_WORLD='{}'
for _ in $(seq 1 100); do
    COLD_COAL_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
local area = {{-64, 12}, {-46, 31}}
local drill, feeder
local belt_coal = 0
for _, entity in pairs(s.find_entities_filtered{area = area}) do
    if entity.unit_number == $COLD_COAL_DRILL_UNIT then drill = entity end
    if entity.unit_number == $COLD_COAL_INSERTER_UNIT then feeder = entity end
    if entity.type == 'transport-belt' then
        for line_index = 1, 2 do
            belt_coal = belt_coal + entity.get_transport_line(line_index).get_item_count('coal')
        end
    end
end
local resource_amount = 0
for _, resource in pairs(s.find_entities_filtered{name = 'coal', area = area}) do
    resource_amount = resource_amount + resource.amount
end
local report = helpers.json_to_table(remote.call(
    'claude_interface',
    'diagnose_fuel_sustainability',
    -64,
    12,
    -46,
    31,
    20,
    '$AGENT_ID'
))
local consumer, exact_connection, feeder_consumer, feeder_self_connection
for _, candidate in ipairs(report.consumers or {}) do
    if candidate.unit_number == $COLD_COAL_DRILL_UNIT then
        consumer = candidate
        for _, connection in ipairs(candidate.proven_fuel_connections or {}) do
            if connection.inserter_unit_number == $COLD_COAL_INSERTER_UNIT then
                exact_connection = connection
                break
            end
        end
    end
    if candidate.unit_number == $COLD_COAL_INSERTER_UNIT then
        feeder_consumer = candidate
        for _, connection in ipairs(candidate.proven_fuel_connections or {}) do
            if connection.connection_kind == 'self_fueling_coal_pickup'
                and connection.inserter_unit_number == $COLD_COAL_INSERTER_UNIT
            then
                feeder_self_connection = connection
                break
            end
        end
    end
end
local function contains_reason(value, wanted, seen)
    if type(value) ~= 'table' then return false end
    seen = seen or {}
    if seen[value] then return false end
    seen[value] = true
    if value.reason == wanted then return true end
    for _, child in pairs(value) do
        if contains_reason(child, wanted, seen) then return true end
    end
    return false
end
local drill_status = nil
if drill then
    for name, value in pairs(defines.entity_status) do
        if value == drill.status then drill_status = name break end
    end
end
rcon.print(helpers.table_to_json({
    same_drill = drill and drill.valid and drill.unit_number == $COLD_COAL_DRILL_UNIT or false,
    same_feeder = feeder and feeder.valid and feeder.unit_number == $COLD_COAL_INSERTER_UNIT or false,
    drill_status = drill_status,
    drill_fuel = drill and drill.get_fuel_inventory().get_item_count('coal') or -1,
    drill_remaining = drill and drill.burner and drill.burner.remaining_burning_fuel or 0,
    feeder_fuel = feeder and feeder.get_fuel_inventory().get_item_count('coal') or -1,
    feeder_remaining = feeder and feeder.burner and feeder.burner.remaining_burning_fuel or 0,
    belts = s.count_entities_filtered{type = 'transport-belt', area = area},
    belt_coal = belt_coal,
    resource_tiles = s.count_entities_filtered{name = 'coal', area = area},
    resource_amount = resource_amount,
    character_coal = c.get_main_inventory().get_item_count('coal'),
    character_inserters = c.get_main_inventory().get_item_count('burner-inserter'),
    automated = consumer and consumer.automated == true or false,
    exact_connection_durable = exact_connection and exact_connection.durable == true or false,
    exact_connection_live = exact_connection and exact_connection.live == true or false,
    closed_cycle = contains_reason(exact_connection, 'closed_self_sustaining_coal_cycle'),
    feeder_automated = feeder_consumer and feeder_consumer.automated == true or false,
    feeder_self_connection_present = feeder_self_connection ~= nil,
    feeder_self_connection_durable = feeder_self_connection and feeder_self_connection.durable == true or false,
    feeder_self_connection_live = feeder_self_connection and feeder_self_connection.live == true or false,
    feeder_self_source_operational = feeder_self_connection and feeder_self_connection.source_operational == true or false
}))
")"
    if jq -e '.same_drill == true
        and .same_feeder == true
        and .belts >= 3
        and .belt_coal > 0
        and .resource_amount < 1600000
        and .automated == true
        and .exact_connection_durable == true
        and .exact_connection_live == true
        and .closed_cycle == true
        and .feeder_automated == true
        and .feeder_self_connection_present == true
        and .feeder_self_connection_durable == true
        and .feeder_self_connection_live == true
        and .feeder_self_source_operational == true' >/dev/null 2>&1 <<<"$COLD_COAL_WORLD"; then
        break
    fi
    sleep 0.1
done
require_json "cold coal repair persists a live self-sustaining factory loop" "$COLD_COAL_WORLD" \
    '.same_drill == true
     and .same_feeder == true
     and (.drill_status == "working" or .drill_status == "waiting_for_space_in_destination")
     and (.drill_fuel > 0 or .drill_remaining > 0)
     and (.feeder_fuel > 0 or .feeder_remaining > 0)
     and .belts >= 3
     and .belt_coal > 0
     and .resource_tiles == 16
     and .resource_amount < 1600000
     and .character_coal == 2
     and .character_inserters == 0
     and .automated == true
     and .exact_connection_durable == true
     and .exact_connection_live == true
     and .closed_cycle == true
     and .feeder_automated == true
     and .feeder_self_connection_present == true
     and .feeder_self_connection_durable == true
     and .feeder_self_connection_live == true
     and .feeder_self_source_operational == true'

# If Factorio stalls after startup fuel is transferred, the controller must
# clear that transaction fuel before removing its new loop. This prevents a
# failed retry from leaving the pre-existing drill manually powered.
ROLLBACK_COAL_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
local area = {{-64, 12}, {-46, 31}}
for _, entity in pairs(s.find_entities_filtered{area = area}) do
    if entity.type ~= 'character' then entity.destroy() end
end
c.teleport({-58, 23}, s)
local inv = c.get_main_inventory()
inv.clear()
inv.insert{name = 'coal', count = 12}
inv.insert{name = 'transport-belt', count = 16}
inv.insert{name = 'burner-inserter', count = 1}
for x = -58, -55 do
    for y = 21, 24 do
        s.create_entity{name = 'coal', position = {x + 0.5, y + 0.5}, amount = 100000}
    end
end
local drill = s.create_entity{
    name = 'burner-mining-drill',
    position = {-56, 23},
    direction = defines.direction.east,
    force = c.force
}
drill.get_fuel_inventory().clear()
drill.burner.currently_burning = nil
drill.burner.heat = 0
rcon.print(helpers.table_to_json({drill_unit = drill and drill.unit_number or nil}))
")"
require_json "failed-bootstrap fixture creates one exact empty burner drill" "$ROLLBACK_COAL_FIXTURE" \
    '(.drill_unit | type) == "number"'
ROLLBACK_COAL_DRILL_UNIT="$(jq -r '.drill_unit' <<<"$ROLLBACK_COAL_FIXTURE")"

rollback_coal_state() {
    raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
local area = {{-64, 12}, {-46, 31}}
local drill
for _, entity in pairs(s.find_entities_filtered{area = area}) do
    if entity.unit_number == $ROLLBACK_COAL_DRILL_UNIT then drill = entity break end
end
local burning = drill and drill.burner and drill.burner.currently_burning or nil
local burning_name = burning and burning.name and burning.name.name or nil
local resource_amount = 0
for _, resource in pairs(s.find_entities_filtered{name = 'coal', area = area}) do
    resource_amount = resource_amount + resource.amount
end
rcon.print(helpers.table_to_json({
    tick = game.tick,
    same_drill = drill and drill.valid and drill.unit_number == $ROLLBACK_COAL_DRILL_UNIT or false,
    drill_fuel = drill and drill.get_fuel_inventory().get_item_count('coal') or -1,
    drill_remaining = drill and drill.burner and drill.burner.remaining_burning_fuel or -1,
    drill_heat = drill and drill.burner and drill.burner.heat or -1,
    drill_burning = burning_name,
    belts = s.count_entities_filtered{type = 'transport-belt', area = area},
    inserters = s.count_entities_filtered{type = 'inserter', area = area},
    character_coal = c.get_main_inventory().get_item_count('coal'),
    character_belts = c.get_main_inventory().get_item_count('transport-belt'),
    character_inserters = c.get_main_inventory().get_item_count('burner-inserter'),
    resource_amount = resource_amount
}))
"
}

# Pause before starting the transaction. RCON mutations can still lay the route
# and insert bounded bootstrap fuel, but no coal can traverse it. The first
# delivery wait must therefore fail and atomically restore the cold drill.
raw_lua "game.tick_paused = true" >/dev/null
ROLLBACK_COAL_REQUEST_ID="$MCP_NEXT_ID"
MCP_NEXT_ID=$((MCP_NEXT_ID + 1))
jq -cn \
    --argjson id "$ROLLBACK_COAL_REQUEST_ID" \
    --argjson arguments "$COLD_COAL_EXEC_ARGS" \
    '{jsonrpc:"2.0", id:$id, method:"tools/call",
      params:{name:"repair_fuel_sustainability", arguments:$arguments}}' \
    >&"$MCP_IN_FD"
pass "cold-coal rollback fixture stalls before route delivery"

ROLLBACK_COAL_CALL_STATUS=0
ROLLBACK_COAL_EXEC="$(mcp_read_id "$ROLLBACK_COAL_REQUEST_ID")" || ROLLBACK_COAL_CALL_STATUS=$?
raw_lua "game.tick_paused = false" >/dev/null
if (( ROLLBACK_COAL_CALL_STATUS != 0 )); then
    fail "interrupted cold-coal controller returns an MCP response" \
        "mcp_read_id exited $ROLLBACK_COAL_CALL_STATUS"
    exit 1
fi
pass "interrupted cold-coal controller returns an MCP response"

ROLLBACK_COAL_PAYLOAD="$(tool_payload "$ROLLBACK_COAL_EXEC")"
assert_json "stalled cold-coal repair is a semantic controller failure" "$ROLLBACK_COAL_EXEC" \
    '(.result.isError // false) == true'
require_json "failed cold-coal transaction clears bootstrap fuel before rollback" "$ROLLBACK_COAL_PAYLOAD" \
    --argjson unit "$ROLLBACK_COAL_DRILL_UNIT" \
    '.success == false
     and .selected_transaction.consumer_unit_number == $unit
     and .repair.error_kind == "self_bootstrap_observation_failed"
     and .repair.rollback.success == true
     and .repair.rollback.transaction_fuel_cleared == true
     and .repair.rollback.consumer_state.success == true
     and .repair.rollback.consumer_state.identity_valid == true
     and .repair.rollback.consumer_state.feeder_quiesced == true
     and .repair.rollback.consumer_state.consumer_state_restored == true
     and .repair.rollback.consumer_state.transaction_fuel_cleared == true
     and .repair.rollback.consumer_state.before.cold == false
     and .repair.rollback.consumer_state.expected.cold == true
     and .repair.rollback.consumer_state.expected.fuel_total == 0
     and .repair.rollback.consumer_state.expected.currently_burning == null
     and .repair.rollback.consumer_state.expected.remaining_burning_fuel == 0
     and .repair.rollback.consumer_state.expected.heat == 0
     and .repair.rollback.consumer_state.after.cold == true
     and .repair.rollback.consumer_state.after.fuel_total == 0
     and .repair.rollback.consumer_state.after.currently_burning == null
     and .repair.rollback.consumer_state.after.remaining_burning_fuel == 0
     and .repair.rollback.consumer_state.after.heat == 0
     and .repair.rollback.infrastructure.success == true'

ROLLBACK_COAL_WORLD="$(rollback_coal_state)"
require_json "failed cold-coal transaction leaves no powered drill or pointless infrastructure" "$ROLLBACK_COAL_WORLD" \
    '.same_drill == true
     and .drill_fuel == 0
     and .drill_remaining == 0
     and .drill_heat == 0
     and .drill_burning == null
     and .belts == 0
     and .inserters == 0
     and .character_coal == 12
     and .character_belts == 16
     and .character_inserters == 1
     and .resource_amount == 1600000'

ROLLBACK_COAL_TARGET_TICK="$(jq -r '.tick + 120' <<<"$ROLLBACK_COAL_WORLD")"
ROLLBACK_COAL_RESOURCE_AFTER="$(jq -r '.resource_amount' <<<"$ROLLBACK_COAL_WORLD")"
ROLLBACK_COAL_DELAYED_WORLD='{}'
for _ in $(seq 1 100); do
    ROLLBACK_COAL_DELAYED_WORLD="$(rollback_coal_state)"
    if jq -e --argjson target "$ROLLBACK_COAL_TARGET_TICK" \
        '.tick >= $target' >/dev/null 2>&1 <<<"$ROLLBACK_COAL_DELAYED_WORLD"; then
        break
    fi
    sleep 0.05
done
require_json "failed cold-coal rollback remains cold after 120 running ticks" \
    "$ROLLBACK_COAL_DELAYED_WORLD" \
    --argjson target "$ROLLBACK_COAL_TARGET_TICK" \
    --argjson resource_after "$ROLLBACK_COAL_RESOURCE_AFTER" \
    '.tick >= $target
     and .same_drill == true
     and .drill_fuel == 0
     and .drill_remaining == 0
     and .drill_heat == 0
     and .drill_burning == null
     and .belts == 0
     and .inserters == 0
     and .resource_amount == $resource_after'

# A rollback must conserve queued transaction fuel even when the character has
# no inventory capacity. The exact burner snapshot is restored and the excess
# is spilled at that consumer rather than silently deleted.
FULL_INVENTORY_ROLLBACK="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
local drill
for _, entity in pairs(s.find_entities_filtered{area = {{-64, 12}, {-46, 31}}}) do
    if entity.unit_number == $ROLLBACK_COAL_DRILL_UNIT then drill = entity break end
end
local snapshot = helpers.json_to_table(remote.call(
    'claude_interface',
    'snapshot_burner_state',
    $ROLLBACK_COAL_DRILL_UNIT
))
local inv = c.get_main_inventory()
inv.clear()
for i = 1, #inv do
    inv[i].set_stack{name = 'iron-plate', count = 100}
end
drill.get_fuel_inventory().insert{name = 'coal', count = 5}
drill.burner.currently_burning = {name = 'coal', quality = 'normal'}
drill.burner.remaining_burning_fuel = 1000
local report = helpers.json_to_table(remote.call(
    'claude_interface',
    'rollback_burner_bootstrap',
    '$AGENT_ID',
    snapshot,
    nil
))
local ground_coal = 0
for _, entity in pairs(s.find_entities_filtered{
    type = 'item-entity',
    area = {{drill.position.x - 4, drill.position.y - 4},
            {drill.position.x + 4, drill.position.y + 4}}
}) do
    if entity.stack and entity.stack.valid_for_read and entity.stack.name == 'coal' then
        ground_coal = ground_coal + entity.stack.count
    end
end
local burning = drill.burner.currently_burning
rcon.print(helpers.table_to_json({
    report = report,
    drill_fuel = drill.get_fuel_inventory().get_item_count('coal'),
    drill_remaining = drill.burner.remaining_burning_fuel,
    drill_heat = drill.burner.heat,
    drill_burning = burning and burning.name and burning.name.name or nil,
    ground_coal = ground_coal,
    character_full = inv.is_full()
}))
")"
require_json "full-inventory burner rollback spills rather than deletes excess fuel" \
    "$FULL_INVENTORY_ROLLBACK" \
    '.report.success == true
     and .report.consumer_state_restored == true
     and .report.transaction_fuel_cleared == true
     and .report.returned_excess == 0
     and .report.spilled_excess == 5
     and .report.unrecovered_excess == 0
     and .drill_fuel == 0
     and .drill_remaining == 0
     and .drill_heat == 0
     and .drill_burning == null
     and .ground_coal == 5
     and .character_full == true'
raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
c.get_main_inventory().clear()
for _, entity in pairs(s.find_entities_filtered{type = 'item-entity', area = {{-64, 12}, {-46, 31}}}) do
    entity.destroy()
end
" >/dev/null

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

# Producer capability and endpoint delivery are different facts. Build a long
# branch from the proven electric source and inspect it in the same game tick,
# before any coal can traverse the new belts. The empty terminal must retain a
# durable upstream proof without being reported as live fuel delivery.
EMPTY_TRANSIT_FIXTURE="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local source
for _, entity in pairs(s.find_entities_filtered{area = {{-31, -31}, {-8, -10}}}) do
    if entity.unit_number == $ELECTRIC_BELT_UNIT then source = entity break end
end
if not source then
    rcon.print(helpers.table_to_json({error = 'electric source belt missing'}))
    return
end
local created = {}
local endpoint
for offset = 1, 14 do
    local belt = s.create_entity{
        name = 'transport-belt',
        position = {source.position.x, source.position.y - offset},
        direction = defines.direction.north,
        force = game.forces.player
    }
    if belt then
        endpoint = belt
        table.insert(created, belt.unit_number)
    end
end
local terminal = endpoint and s.create_entity{
    name = 'burner-inserter',
    position = {endpoint.position.x, endpoint.position.y - 1},
    direction = defines.direction.south,
    force = game.forces.player
} or nil
if terminal then
    terminal.get_fuel_inventory().insert{name = 'coal', count = 5}
    table.insert(created, terminal.unit_number)
end
local endpoint_coal = 0
if endpoint then
    for line_index = 1, 2 do
        endpoint_coal = endpoint_coal + endpoint.get_transport_line(line_index).get_item_count('coal')
    end
end
local report = helpers.json_to_table(remote.call(
    'claude_interface',
    'diagnose_fuel_sustainability',
    source.position.x - 4,
    source.position.y - 18,
    source.position.x + 4,
    source.position.y + 4,
    50,
    '$AGENT_ID'
))
if endpoint then
    endpoint.get_transport_line(1).insert_at_back({name = 'coal', count = 1})
end
local endpoint_coal_after = 0
if endpoint then
    for line_index = 1, 2 do
        endpoint_coal_after = endpoint_coal_after
            + endpoint.get_transport_line(line_index).get_item_count('coal')
    end
end
local sparse_report = helpers.json_to_table(remote.call(
    'claude_interface',
    'diagnose_fuel_sustainability',
    source.position.x - 4,
    source.position.y - 18,
    source.position.x + 4,
    source.position.y + 4,
    50,
    '$AGENT_ID'
))
local endpoint_unit = endpoint and endpoint.unit_number or nil
local terminal_unit = terminal and terminal.unit_number or nil
local wanted = {}
for _, unit_number in ipairs(created) do wanted[unit_number] = true end
for _, entity in pairs(s.find_entities_filtered{area = {{-27, -43}, {-18, -10}}}) do
    if entity.unit_number and wanted[entity.unit_number] then entity.destroy() end
end
local remaining_created = 0
for _, entity in pairs(s.find_entities_filtered{area = {{-27, -43}, {-18, -10}}}) do
    if entity.unit_number and wanted[entity.unit_number] then
        remaining_created = remaining_created + 1
    end
end
rcon.print(helpers.table_to_json({
    source_unit = source.unit_number,
    endpoint_unit = endpoint_unit,
    terminal_unit = terminal_unit,
    endpoint_coal = endpoint_coal,
    endpoint_coal_after = endpoint_coal_after,
    created_units = created,
    remaining_created = remaining_created,
    report = report,
    sparse_report = sparse_report
}))
")"
require_json "empty downstream coal line is durable topology but not live delivery" \
    "$EMPTY_TRANSIT_FIXTURE" \
    '.terminal_unit as $terminal
     | .endpoint_unit as $endpoint
     | .error == null
       and .endpoint_coal == 0
       and .remaining_created == 0
       and ($endpoint | type) == "number"
       and ($terminal | type) == "number"
       and ((.report.consumers[] | select(.unit_number == $terminal))
           | .automated == true
             and .issue == "automated_supply_starved"
             and any(.fuel_connections[]?;
                 .inserter_unit_number == $terminal
                 and .source.unit_number == $endpoint
                 and .source.coal_count == 0
                 and .source_durable == true
                 and .source_operational == false
                 and .durable == true
                 and .live == false
                 and .source.upstream_proof.reason == "upstream_ready_but_source_empty"))'
require_json "coal on the exact endpoint remains live across empty intermediate belt tiles" \
    "$EMPTY_TRANSIT_FIXTURE" \
    '.terminal_unit as $terminal
     | .endpoint_unit as $endpoint
     | .endpoint_coal_after > 0
       and ((.sparse_report.consumers[] | select(.unit_number == $terminal))
           | .automated == true
             and .issue == null
             and any(.fuel_connections[]?;
                 .inserter_unit_number == $terminal
                 and .source.unit_number == $endpoint
                 and .source.coal_count > 0
                 and .source.producer_operational == true
                 and .source_operational == true
                 and .durable == true
                 and .live == true
                 and .source.upstream_proof.reason == "connected_surface_belt"))'

# An operational through-belt is a source to tap, never an endpoint to rotate.
# Build one mixed-item coal trunk, fail one complete fuel transaction on an idle
# furnace to prove exact rollback, then reuse the same preserved trunk for a
# successful filtered branch.
SOURCE_TAP_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
c.teleport({-30, -26}, s)
local inv = c.get_main_inventory()
inv.clear()
inv.insert{name = 'transport-belt', count = 64}
inv.insert{name = 'burner-inserter', count = 2}
inv.insert{name = 'coal', count = 10}
local belts = {}
for index, y in ipairs({-25.5, -26.5, -27.5, -28.5}) do
    belts[index] = s.create_entity{
        name = 'transport-belt',
        position = {-22.5, y},
        direction = defines.direction.north,
        force = c.force
    }
end
local sink = s.create_entity{
    name = 'inserter',
    position = {-22.5, -29.5},
    direction = defines.direction.south,
    force = c.force
}
local chest = s.create_entity{name = 'iron-chest', position = {-22.5, -30.5}, force = c.force}
local furnace = s.create_entity{name = 'stone-furnace', position = {-30, -26}, force = c.force}
local north_position = {
    x = furnace.position.x,
    y = furnace.bounding_box.left_top.y - 0.5
}
local east_position = {
    x = furnace.bounding_box.right_bottom.x + 0.5,
    y = furnace.position.y
}
local wall = s.create_entity{name = 'stone-wall', position = north_position, force = c.force}
local source = belts[1]
for _, belt in ipairs(belts) do
    for _ = 1, 3 do belt.get_transport_line(1).insert_at_back({name = 'coal', count = 1}) end
    belt.get_transport_line(2).insert_at_back({name = 'copper-plate', count = 1})
end
furnace.get_fuel_inventory().clear()
furnace.get_inventory(defines.inventory.furnace_source).clear()
furnace.burner.currently_burning = nil
furnace.burner.remaining_burning_fuel = 0
furnace.burner.heat = 0
local neighbours = source.belt_neighbours
rcon.print(helpers.table_to_json({
    source_unit = source and source.unit_number or nil,
    source_direction = source and source.direction or nil,
    source_inputs = source and #(neighbours.inputs or {}) or 0,
    source_outputs = source and #(neighbours.outputs or {}) or 0,
    consumer_unit = furnace and furnace.unit_number or nil,
    sink_unit = sink and sink.unit_number or nil,
    chest_unit = chest and chest.unit_number or nil,
    wall_unit = wall and wall.unit_number or nil,
    north_blocked = not s.can_place_entity{
        name = 'burner-inserter', position = north_position, direction = defines.direction.north,
        force = c.force, build_check_type = defines.build_check_type.manual
    },
    east_placeable = s.can_place_entity{
        name = 'burner-inserter', position = east_position, direction = defines.direction.east,
        force = c.force, build_check_type = defines.build_check_type.manual
    },
    baseline_belts = s.count_entities_filtered{type = 'transport-belt', area = {{-33, -33}, {-19, -20}}},
    baseline_inserters = s.count_entities_filtered{type = 'inserter', area = {{-33, -33}, {-19, -20}}},
    source_tile = {x = math.floor(source.position.x), y = math.floor(source.position.y)},
    source_coal = source.get_transport_line(1).get_item_count('coal')
        + source.get_transport_line(2).get_item_count('coal'),
    source_copper = source.get_transport_line(1).get_item_count('copper-plate')
        + source.get_transport_line(2).get_item_count('copper-plate')
}))
")"
require_json "source-tap fixture is a live mixed through-belt and one cold consumer" \
    "$SOURCE_TAP_FIXTURE" \
    '.source_unit > 0
     and .consumer_unit > 0
     and .sink_unit > 0
     and .chest_unit > 0
     and .wall_unit > 0
     and .source_direction == 0
     and .source_inputs > 0
     and .source_outputs > 0
     and .north_blocked == true
     and .east_placeable == true
     and .source_tile == {x:-23,y:-26}
     and .source_coal > 0
     and .source_copper > 0'
SOURCE_TAP_SOURCE_UNIT="$(jq -r '.source_unit' <<<"$SOURCE_TAP_FIXTURE")"
SOURCE_TAP_SOURCE_DIRECTION="$(jq -r '.source_direction' <<<"$SOURCE_TAP_FIXTURE")"
SOURCE_TAP_CONSUMER_UNIT="$(jq -r '.consumer_unit' <<<"$SOURCE_TAP_FIXTURE")"
SOURCE_TAP_CHEST_UNIT="$(jq -r '.chest_unit' <<<"$SOURCE_TAP_FIXTURE")"
SOURCE_TAP_BASELINE_BELTS="$(jq -r '.baseline_belts' <<<"$SOURCE_TAP_FIXTURE")"
SOURCE_TAP_BASELINE_INSERTERS="$(jq -r '.baseline_inserters' <<<"$SOURCE_TAP_FIXTURE")"

restock_source_tap_fixture() {
    raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
local inv = c.get_main_inventory()
inv.clear()
inv.insert{name = 'transport-belt', count = 64}
inv.insert{name = 'burner-inserter', count = 2}
inv.insert{name = 'coal', count = 10}
local source
for _, entity in pairs(s.find_entities_filtered{type = 'transport-belt', area = {{-24, -27}, {-22, -25}}}) do
    if entity.unit_number == $SOURCE_TAP_SOURCE_UNIT then source = entity break end
end
if source then
    for _ = 1, 4 do source.get_transport_line(1).insert_at_back({name = 'coal', count = 1}) end
    for _ = 1, 2 do source.get_transport_line(2).insert_at_back({name = 'copper-plate', count = 1}) end
end
rcon.print(helpers.table_to_json({
    source_present = source ~= nil,
    character_belts = inv.get_item_count('transport-belt'),
    character_inserters = inv.get_item_count('burner-inserter'),
    character_coal = inv.get_item_count('coal')
}))
"
}

SOURCE_TAP_ARGS="$(jq -cn \
    --argjson x -30 \
    --argjson y -26 \
    '{x:$x,y:$y,radius:18,limit:10,search_radius:12,dry_run:true,
      respect_zones:false,allow_underground:false,extend_existing:true,
      verify_radius:18}')"
restock_source_tap_fixture >/dev/null
SOURCE_TAP_DRY="$(mcp_tool repair_fuel_sustainability "$SOURCE_TAP_ARGS")"
SOURCE_TAP_DRY_PAYLOAD="$(tool_payload "$SOURCE_TAP_DRY")"
assert_json "through-belt source-tap dry-run is a normal successful tool result" \
    "$SOURCE_TAP_DRY" '.result.isError != true'
require_json "through-belt source-tap plan preserves the source and creates an independent branch" \
    "$SOURCE_TAP_DRY_PAYLOAD" \
    --argjson source "$SOURCE_TAP_SOURCE_UNIT" \
    --argjson consumer "$SOURCE_TAP_CONSUMER_UNIT" \
    '.success == true
     and .selected_transaction.consumer_unit_number == $consumer
     and .selected_transaction.source_unit_number == $source
     and .repair.success == true
     and .repair.dry_run == true
     and .repair.source_tap.source_unit_number == $source
     and .repair.source_tap.source_direction_preserved == true
     and .repair.source_tap.layout.source_tile == {x:-23,y:-26}
     and .repair.source_tap.layout.pickup_tile == {x:-23,y:-26}
     and .repair.source_tap.layout.inserter_tile == {x:-24,y:-26}
     and .repair.source_tap.layout.drop_tile == {x:-25,y:-26}
     and .repair.source_tap.layout.outward == "west"
     and .repair.source_tap.layout.inserter_direction == "east"
     and .repair.source_tap.route_start_matches_drop == true
     and .repair.source_tap.branch_extend_existing == false
     and .repair.route.from == {x:-25,y:-26}
     and .repair.route.topology.connected == true
     and .repair.preflight.ready == true
     and .repair.preflight.routes.materials["transport-belt"].needed > 0
     and .repair.preflight.routes.materials["transport-belt"].needed == .repair.route.new_belt_count
     and .repair.preflight.routes.materials["burner-inserter"].needed == 2
     and .repair.preflight.bootstrap_fuel.required == 10
     and .repair.preflight.bootstrap_fuel.inserter_required == 5
     and .repair.preflight.bootstrap_fuel.source_tap_required == 5
     and .repair.preflight.bootstrap_fuel.consumer_required == 0
     and (.repair.route.next_action.tool // null) != "rotate_entity"'

SOURCE_TAP_DRY_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = game.surfaces['buddy-live-regression']
local source, furnace
for _, entity in pairs(s.find_entities_filtered{area = {{-33, -33}, {-19, -20}}}) do
    if entity.unit_number == $SOURCE_TAP_SOURCE_UNIT then source = entity end
    if entity.unit_number == $SOURCE_TAP_CONSUMER_UNIT then furnace = entity end
end
rcon.print(helpers.table_to_json({
    same_source = source and source.valid and source.unit_number == $SOURCE_TAP_SOURCE_UNIT or false,
    source_direction = source and source.direction or nil,
    belts = s.count_entities_filtered{type = 'transport-belt', area = {{-33, -33}, {-19, -20}}},
    inserters = s.count_entities_filtered{type = 'inserter', area = {{-33, -33}, {-19, -20}}},
    burner_inserters = s.count_entities_filtered{name = 'burner-inserter', area = {{-33, -33}, {-19, -20}}},
    consumer_fuel = furnace and furnace.get_fuel_inventory().get_item_count() or -1,
    character_belts = c.get_main_inventory().get_item_count('transport-belt'),
    character_inserters = c.get_main_inventory().get_item_count('burner-inserter'),
    character_coal = c.get_main_inventory().get_item_count('coal')
}))
")"
require_json "source-tap dry-run mutates no world or inventory state" "$SOURCE_TAP_DRY_WORLD" \
    --argjson direction "$SOURCE_TAP_SOURCE_DIRECTION" \
    --argjson belts "$SOURCE_TAP_BASELINE_BELTS" \
    --argjson inserters "$SOURCE_TAP_BASELINE_INSERTERS" \
    '.same_source == true
     and .source_direction == $direction
     and .belts == $belts
     and .inserters == $inserters
     and .burner_inserters == 0
     and .consumer_fuel == 0
     and .character_belts == 64
     and .character_inserters == 2
     and .character_coal == 10'

SOURCE_TAP_FAIL_ARGS="$(jq -c '.dry_run = false' <<<"$SOURCE_TAP_ARGS")"
restock_source_tap_fixture >/dev/null
SOURCE_TAP_FAIL="$(mcp_tool repair_fuel_sustainability "$SOURCE_TAP_FAIL_ARGS")"
SOURCE_TAP_FAIL_PAYLOAD="$(tool_payload "$SOURCE_TAP_FAIL")"
require_json "idle source-tap target fails honestly and rolls back the complete new branch" \
    "$SOURCE_TAP_FAIL_PAYLOAD" \
    --argjson source "$SOURCE_TAP_SOURCE_UNIT" \
    --argjson consumer "$SOURCE_TAP_CONSUMER_UNIT" \
    '.success == false
     and .selected_transaction.consumer_unit_number == $consumer
     and .selected_transaction.source_unit_number == $source
     and .repair.error_kind == "target_production_not_verified"
     and .repair.automation_verified.production_success == false
     and .repair.source_tap.source_unit_number == $source
     and .repair.rollback.success == true
     and .repair.rollback.consumer_state.success == true
     and .repair.rollback.consumer_state.identity_valid == true
     and .repair.rollback.consumer_state.consumer_state_restored == true
     and .repair.rollback.consumer_state.expected.cold == true
     and .repair.rollback.consumer_state.after.cold == true
     and .repair.rollback.consumer_state.after.fuel_total == 0
     and .repair.rollback.infrastructure.success == true'

SOURCE_TAP_ROLLBACK_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local source, furnace, chest
for _, entity in pairs(s.find_entities_filtered{area = {{-33, -33}, {-19, -20}}}) do
    if entity.unit_number == $SOURCE_TAP_SOURCE_UNIT then source = entity end
    if entity.unit_number == $SOURCE_TAP_CONSUMER_UNIT then furnace = entity end
    if entity.unit_number == $SOURCE_TAP_CHEST_UNIT then chest = entity end
end
local burning = furnace and furnace.burner.currently_burning or nil
local neighbours = source and source.belt_neighbours or nil
rcon.print(helpers.table_to_json({
    same_source = source and source.valid and source.unit_number == $SOURCE_TAP_SOURCE_UNIT or false,
    source_direction = source and source.direction or nil,
    source_inputs = neighbours and #(neighbours.inputs or {}) or 0,
    source_outputs = neighbours and #(neighbours.outputs or {}) or 0,
    belts = s.count_entities_filtered{type = 'transport-belt', area = {{-33, -33}, {-19, -20}}},
    inserters = s.count_entities_filtered{type = 'inserter', area = {{-33, -33}, {-19, -20}}},
    burner_inserters = s.count_entities_filtered{name = 'burner-inserter', area = {{-33, -33}, {-19, -20}}},
    branch_belts = s.count_entities_filtered{type = 'transport-belt', area = {{-29, -27}, {-24, -25}}},
    consumer_fuel = furnace and furnace.get_fuel_inventory().get_item_count() or -1,
    consumer_remaining = furnace and furnace.burner.remaining_burning_fuel or -1,
    consumer_heat = furnace and furnace.burner.heat or -1,
    consumer_burning = burning and burning.name and burning.name.name or nil,
    downstream_chest_coal = chest and chest.get_item_count('coal') or 0
}))
")"
require_json "source-tap rollback preserves the exact live trunk and restores the cold consumer" \
    "$SOURCE_TAP_ROLLBACK_WORLD" \
    --argjson direction "$SOURCE_TAP_SOURCE_DIRECTION" \
    --argjson belts "$SOURCE_TAP_BASELINE_BELTS" \
    --argjson inserters "$SOURCE_TAP_BASELINE_INSERTERS" \
    '.same_source == true
     and .source_direction == $direction
     and .source_inputs > 0
     and .source_outputs > 0
     and .belts == $belts
     and .inserters == $inserters
     and .burner_inserters == 0
     and .branch_belts == 0
     and .consumer_fuel == 0
     and .consumer_remaining == 0
     and .consumer_heat == 0
     and .consumer_burning == null
     and .downstream_chest_coal > 0'
sleep 3
SOURCE_TAP_ROLLBACK_LATE="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local furnace
for _, entity in pairs(s.find_entities_filtered{type = 'furnace', area = {{-33, -33}, {-19, -20}}}) do
    if entity.unit_number == $SOURCE_TAP_CONSUMER_UNIT then furnace = entity break end
end
local burning = furnace and furnace.burner.currently_burning or nil
rcon.print(helpers.table_to_json({
    consumer_fuel = furnace and furnace.get_fuel_inventory().get_item_count() or -1,
    consumer_remaining = furnace and furnace.burner.remaining_burning_fuel or -1,
    consumer_heat = furnace and furnace.burner.heat or -1,
    consumer_burning = burning and burning.name and burning.name.name or nil,
    burner_inserters = s.count_entities_filtered{name = 'burner-inserter', area = {{-33, -33}, {-19, -20}}},
    branch_belts = s.count_entities_filtered{type = 'transport-belt', area = {{-29, -27}, {-24, -25}}}
}))
")"
require_json "rolled-back source tap cannot race another delivery after teardown" \
    "$SOURCE_TAP_ROLLBACK_LATE" \
    '.consumer_fuel == 0
     and .consumer_remaining == 0
     and .consumer_heat == 0
     and .consumer_burning == null
     and .burner_inserters == 0
     and .branch_belts == 0'

restock_source_tap_fixture >/dev/null
SOURCE_TAP_SUCCESS_PREP="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local furnace
for _, entity in pairs(s.find_entities_filtered{type = 'furnace', area = {{-33, -33}, {-19, -20}}}) do
    if entity.unit_number == $SOURCE_TAP_CONSUMER_UNIT then furnace = entity break end
end
local inserted = furnace and furnace.get_inventory(defines.inventory.furnace_source).insert{
    name = 'iron-ore', count = 20
} or 0
rcon.print(helpers.table_to_json({inserted = inserted}))
")"
require_json "source-tap success retry has real furnace input" "$SOURCE_TAP_SUCCESS_PREP" \
    '.inserted == 20'
SOURCE_TAP_SUCCESS="$(mcp_tool repair_fuel_sustainability "$SOURCE_TAP_FAIL_ARGS")"
SOURCE_TAP_SUCCESS_PAYLOAD="$(tool_payload "$SOURCE_TAP_SUCCESS")"
assert_json "source-tap success is a normal successful tool result" "$SOURCE_TAP_SUCCESS" \
    '.result.isError != true'
require_json "source-tap retry proves preserved source, filtered self-fueling tap, and target production" \
    "$SOURCE_TAP_SUCCESS_PAYLOAD" \
    --argjson source "$SOURCE_TAP_SOURCE_UNIT" \
    --argjson consumer "$SOURCE_TAP_CONSUMER_UNIT" \
    '.success == true
     and .selected_transaction.consumer_unit_number == $consumer
     and .selected_transaction.source_unit_number == $source
     and .repair.success == true
     and .repair.route.success == true
     and .repair.route.complete_route == true
     and .repair.source_tap.success == true
     and .repair.source_tap.source_unit_number == $source
     and .repair.source_tap.source_direction_preserved == true
     and .repair.source_tap.route_start_matches_drop == true
     and .repair.source_tap.branch_extend_existing == false
     and .repair.source_tap.filter_readback_verified == true
     and .repair.source_tap.filter_atomic_with_placement == true
     and .repair.source_tap.allowed_items == ["coal"]
     and .repair.source_tap.self_fueling_live == true
     and .repair.source_tap.source_preservation.unit_matches == true
     and .repair.source_tap.source_preservation.tile_matches == true
     and .repair.source_tap.source_preservation.direction_matches == true
     and .repair.bootstrap_source_tap_fuel.success == true
     and .repair.inserter.atomic_filter_configuration == true
     and .repair.inserter.filter.atomic_with_placement == true
     and .repair.inserter.filter.readback_verified == true
     and [.repair.inserter.filter.filters[].name] == ["coal"]
     and .repair.infrastructure_verified.success == true
     and .repair.infrastructure_verified.durable_fuel_topology.exact_connection_present == true
     and .repair.infrastructure_verified.durable_fuel_topology.durable_connection_verified == true
     and .repair.infrastructure_verified.durable_fuel_topology.structural_success == true
     and .repair.infrastructure_verified.delivery_observation.success == true
     and .repair.infrastructure_verified.delivery_observation.delivery_path_operational == true
     and (.repair.infrastructure_verified.delivery_observation.terminal_coal_observed == true
          or .repair.infrastructure_verified.delivery_observation.exact_feeder_transfer_observed == true)
     and .repair.production_verified.target_unit_number == $consumer
     and .repair.production_verified.target_working_or_progressed == true
     and .repair.automation_verified.success == true
     and .repair.automation_verified.infrastructure_success == true
     and .repair.automation_verified.production_success == true
     and .repair.automation_verified.fuel_supply_live == true
     and .repair.automation_verified.source_tap_success == true'
SOURCE_TAP_UNIT="$(jq -r '.repair.source_tap.unit_number' <<<"$SOURCE_TAP_SUCCESS_PAYLOAD")"
SOURCE_TAP_TERMINAL_UNIT="$(jq -r '.repair.inserter.unit_number' <<<"$SOURCE_TAP_SUCCESS_PAYLOAD")"

SOURCE_TAP_SUCCESS_WORLD='{}'
for _ in $(seq 1 100); do
    SOURCE_TAP_SUCCESS_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local source, tap, terminal, furnace, chest
for _, entity in pairs(s.find_entities_filtered{area = {{-33, -33}, {-19, -20}}}) do
    if entity.unit_number == $SOURCE_TAP_SOURCE_UNIT then source = entity end
    if entity.unit_number == $SOURCE_TAP_UNIT then tap = entity end
    if entity.unit_number == $SOURCE_TAP_TERMINAL_UNIT then terminal = entity end
    if entity.unit_number == $SOURCE_TAP_CONSUMER_UNIT then furnace = entity end
    if entity.unit_number == $SOURCE_TAP_CHEST_UNIT then chest = entity end
end
local branch_coal, branch_copper = 0, 0
for _, belt in pairs(s.find_entities_filtered{type = 'transport-belt', area = {{-29, -27}, {-24, -25}}}) do
    for line_index = 1, 2 do
        local line = belt.get_transport_line(line_index)
        branch_coal = branch_coal + line.get_item_count('coal')
        branch_copper = branch_copper + line.get_item_count('copper-plate')
    end
end
local report = helpers.json_to_table(remote.call(
    'claude_interface', 'diagnose_fuel_sustainability', -33, -33, -19, -20, 100, '$AGENT_ID'
))
local tap_connection, terminal_connection
for _, candidate in ipairs(report.consumers or {}) do
    if candidate.unit_number == $SOURCE_TAP_UNIT then
        for _, connection in ipairs(candidate.proven_fuel_connections or {}) do
            if connection.connection_kind == 'self_fueling_coal_pickup'
                and connection.inserter_unit_number == $SOURCE_TAP_UNIT
                and connection.source and connection.source.unit_number == $SOURCE_TAP_SOURCE_UNIT
            then tap_connection = connection break end
        end
    end
    if candidate.unit_number == $SOURCE_TAP_CONSUMER_UNIT then
        for _, connection in ipairs(candidate.proven_fuel_connections or {}) do
            if connection.inserter_unit_number == $SOURCE_TAP_TERMINAL_UNIT then
                terminal_connection = connection break
            end
        end
    end
end
local result_inv = furnace and furnace.get_inventory(defines.inventory.furnace_result) or nil
local source_neighbours = source and source.belt_neighbours or nil
local tap_filter = tap and tap.get_filter(1) or nil
local terminal_filter = terminal and terminal.get_filter(1) or nil
rcon.print(helpers.table_to_json({
    same_source = source and source.valid and source.unit_number == $SOURCE_TAP_SOURCE_UNIT or false,
    source_direction = source and source.direction or nil,
    source_inputs = source_neighbours and #(source_neighbours.inputs or {}) or 0,
    source_outputs = source_neighbours and #(source_neighbours.outputs or {}) or 0,
    tap_pickup_tile = tap and {x = math.floor(tap.pickup_position.x), y = math.floor(tap.pickup_position.y)} or nil,
    tap_drop_tile = tap and {x = math.floor(tap.drop_position.x), y = math.floor(tap.drop_position.y)} or nil,
    tap_filter = tap_filter and tap_filter.name or nil,
    terminal_filter = terminal_filter and terminal_filter.name or nil,
    branch_coal = branch_coal,
    branch_copper = branch_copper,
    iron_plates = result_inv and result_inv.get_item_count('iron-plate') or 0,
    downstream_chest_coal = chest and chest.get_item_count('coal') or 0,
    tap_connection_durable = tap_connection and tap_connection.durable == true or false,
    tap_connection_live = tap_connection and tap_connection.live == true or false,
    terminal_connection_durable = terminal_connection and terminal_connection.durable == true or false,
    terminal_connection_live = terminal_connection and terminal_connection.live == true or false
}))
")"
    if jq -e '.same_source == true
        and .source_inputs > 0
        and .source_outputs > 0
        and .tap_filter == "coal"
        and .terminal_filter == "coal"
        and .branch_copper == 0
        and .iron_plates > 0
        and .tap_connection_durable == true
        and .tap_connection_live == true
        and .terminal_connection_durable == true
        and .terminal_connection_live == true' >/dev/null 2>&1 <<<"$SOURCE_TAP_SUCCESS_WORLD"; then
        break
    fi
    sleep 0.1
done
require_json "source-tap world has one pure independent coal branch and the original through-line still works" \
    "$SOURCE_TAP_SUCCESS_WORLD" \
    --argjson direction "$SOURCE_TAP_SOURCE_DIRECTION" \
    '.same_source == true
     and .source_direction == $direction
     and .source_inputs > 0
     and .source_outputs > 0
     and .tap_pickup_tile == {x:-23,y:-26}
     and .tap_drop_tile == {x:-25,y:-26}
     and .tap_filter == "coal"
     and .terminal_filter == "coal"
     and .branch_copper == 0
     and .iron_plates > 0
     and .downstream_chest_coal > 0
     and .tap_connection_durable == true
     and .tap_connection_live == true
     and .terminal_connection_durable == true
     and .terminal_connection_live == true'

# A disabled source tap is durable infrastructure, but it is not an operating
# coal path. Factorio 2 exposes distinct script/circuit/frozen statuses; the
# diagnosis must normalize and reject them instead of treating unknown numeric
# enum values as healthy.
SOURCE_TAP_DISABLED_PROOF="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local tap
for _, entity in pairs(s.find_entities_filtered{type = 'inserter', area = {{-33, -33}, {-19, -20}}}) do
    if entity.unit_number == $SOURCE_TAP_UNIT then tap = entity break end
end
if not tap then
    rcon.print(helpers.table_to_json({error = 'source tap missing'}))
    return
end
local was_disabled = tap.disabled_by_script
tap.disabled_by_script = true
local report = helpers.json_to_table(remote.call(
    'claude_interface', 'diagnose_fuel_sustainability', -33, -33, -19, -20, 100, '$AGENT_ID'
))
local connection
for _, consumer in ipairs(report.consumers or {}) do
    if consumer.unit_number == $SOURCE_TAP_UNIT then
        for _, candidate in ipairs(consumer.fuel_connections or {}) do
            if candidate.connection_kind == 'self_fueling_coal_pickup'
                and candidate.inserter_unit_number == $SOURCE_TAP_UNIT
            then connection = candidate break end
        end
    end
end
tap.disabled_by_script = was_disabled
rcon.print(helpers.table_to_json({
    status = connection and connection.inserter_status or nil,
    operational = connection and connection.inserter_operational == true or false,
    durable = connection and connection.durable == true or false,
    live = connection and connection.live == true or false
}))
")"
require_json "script-disabled self-fueling tap fails closed without losing durable topology" \
    "$SOURCE_TAP_DISABLED_PROOF" \
    '.error == null
     and .status == "disabled_by_script"
     and .operational == false
     and .durable == true
     and .live == false'

# Burner inserters have a much narrower collision box than their engine-owned
# interaction tile. A real adjacent feeder's drop point sits just outside that
# collision box, so topology must trust Factorio's exact drop_target identity.
SMALL_FUEL_TARGET_FIXTURE="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local source = s.find_entity('transport-belt', {-22.5, -27.5})
local consumer = s.create_entity{
    name = 'burner-inserter',
    position = {-20.5, -27.5},
    direction = defines.direction.north,
    force = game.forces.player
}
if not (source and consumer) then
    rcon.print(helpers.table_to_json({error = 'small fuel target fixture creation failed'}))
    return
end
consumer.get_fuel_inventory().clear()
local pre = helpers.json_to_table(remote.call(
    'claude_interface',
    'diagnose_fuel_sustainability',
    -25,
    -31,
    -17,
    -24,
    30,
    '$AGENT_ID'
))
local planned = nil
for _, record in ipairs(pre.consumers or {}) do
    if record.unit_number == consumer.unit_number then
        for _, candidate in ipairs(record.fuel_inserter_candidates or {}) do
            if candidate.side == 'west' then planned = candidate break end
        end
    end
end
local feeder = s.create_entity{
    name = 'burner-inserter',
    position = {-21.5, -27.5},
    direction = defines.direction.west,
    force = game.forces.player
}
if not feeder then
    rcon.print(helpers.table_to_json({error = 'small fuel feeder creation failed'}))
    return
end
feeder.inserter_filter_mode = 'whitelist'
feeder.set_filter(1, 'coal')
feeder.use_filters = true
feeder.get_fuel_inventory().insert{name = 'coal', count = 5}
source.get_transport_line(1).insert_at_back({name = 'coal', count = 3})
local drop = feeder.drop_position
local box = consumer.bounding_box
local drop_outside_collision_box = not (
    drop.x >= box.left_top.x and drop.x <= box.right_bottom.x
    and drop.y >= box.left_top.y and drop.y <= box.right_bottom.y
)
rcon.print(helpers.table_to_json({
    consumer_unit = consumer.unit_number,
    feeder_unit = feeder.unit_number,
    source_unit = source.unit_number,
    planned_inserter_position = planned and planned.inserter_position or nil,
    planned_pickup_tile = planned and planned.pickup_tile or nil,
    actual_inserter_position = feeder.position,
    actual_pickup_position = feeder.pickup_position,
    actual_drop_position = drop,
    drop_target_unit = feeder.drop_target and feeder.drop_target.unit_number or nil,
    drop_outside_collision_box = drop_outside_collision_box
}))
")"
require_json "small burner target fixture exercises exact engine interaction geometry" \
    "$SMALL_FUEL_TARGET_FIXTURE" \
    '.error == null
     and (.consumer_unit | type) == "number"
     and (.feeder_unit | type) == "number"
     and .drop_outside_collision_box == true
     and .planned_inserter_position == {x:-21.5,y:-27.5}
     and .planned_pickup_tile == {x:-22.5,y:-27.5}
     and .actual_inserter_position == .planned_inserter_position
     and ((.actual_pickup_position.x | floor) == (.planned_pickup_tile.x | floor))
     and ((.actual_pickup_position.y | floor) == (.planned_pickup_tile.y | floor))'
SMALL_FUEL_CONSUMER_UNIT="$(jq -r '.consumer_unit' <<<"$SMALL_FUEL_TARGET_FIXTURE")"
SMALL_FUEL_FEEDER_UNIT="$(jq -r '.feeder_unit' <<<"$SMALL_FUEL_TARGET_FIXTURE")"

SMALL_FUEL_TARGET_PROOF='{}'
for _ in $(seq 1 100); do
    SMALL_FUEL_TARGET_PROOF="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local feeder = nil
for _, entity in pairs(s.find_entities_filtered{type = 'inserter', area = {{-23, -29}, {-19, -26}}}) do
    if entity.unit_number == $SMALL_FUEL_FEEDER_UNIT then
        feeder = entity
        break
    end
end
local report = helpers.json_to_table(remote.call(
    'claude_interface',
    'diagnose_fuel_sustainability',
    -25,
    -31,
    -17,
    -24,
    30,
    '$AGENT_ID'
))
local result = {
    consumer_found = false,
    connection = nil,
    engine_drop_target_unit = feeder and feeder.drop_target and feeder.drop_target.unit_number or nil
}
for _, consumer in ipairs(report.consumers or {}) do
    if consumer.unit_number == $SMALL_FUEL_CONSUMER_UNIT then
        result.consumer_found = true
        result.automated = consumer.automated == true
        result.fuel_count = consumer.fuel_count or 0
        result.remaining_burning_fuel = consumer.remaining_burning_fuel or 0
        for _, connection in ipairs(consumer.proven_fuel_connections or {}) do
            if connection.inserter_unit_number == $SMALL_FUEL_FEEDER_UNIT then
                result.connection = connection
                break
            end
        end
    end
end
rcon.print(helpers.table_to_json(result))
")"
    if jq -e --argjson consumer "$SMALL_FUEL_CONSUMER_UNIT" '.consumer_found == true
        and .engine_drop_target_unit == $consumer
        and .automated == true
        and .connection.durable == true
        and .connection.live == true' >/dev/null 2>&1 <<<"$SMALL_FUEL_TARGET_PROOF"; then
        break
    fi
    sleep 0.1
done
require_json "small burner target receives exact durable live fuel topology proof" \
    "$SMALL_FUEL_TARGET_PROOF" \
    --argjson consumer "$SMALL_FUEL_CONSUMER_UNIT" \
    --argjson feeder "$SMALL_FUEL_FEEDER_UNIT" \
    '.consumer_found == true
     and .engine_drop_target_unit == $consumer
     and .automated == true
     and ((.fuel_count > 0) or (.remaining_burning_fuel > 0))
     and .connection.inserter_unit_number == $feeder
     and .connection.durable == true
     and .connection.live == true
     and .connection.source.durable == true
     and .connection.source.operational == true'

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

# A route that joins a live belt must distinguish physical connectivity from
# item compatibility. The occupied lane and its consumer are one belt beyond
# the reused endpoint, proving the advisory follows the existing downstream
# graph instead of inspecting only the exact target tile.
LANE_CONTAMINATION_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
for _, entity in pairs(s.find_entities_filtered{area = {{-64, -32}, {-48, -22}}}) do
    if entity.type ~= 'resource' and entity.type ~= 'character' then entity.destroy() end
end
c.teleport({-60.5, -27.5})
local inv = c.get_main_inventory()
inv.clear()
inv.insert{name = 'transport-belt', count = 20}
local endpoint = s.create_entity{
    name = 'transport-belt', position = {-55.5, -27.5},
    direction = defines.direction.east, force = c.force
}
local downstream = s.create_entity{
    name = 'transport-belt', position = {-54.5, -27.5},
    direction = defines.direction.east, force = c.force
}
local assembler = s.create_entity{
    name = 'assembling-machine-1', position = {-54.5, -30.5}, force = c.force
}
local consumer = s.create_entity{
    name = 'inserter', position = {-54.5, -28.5},
    direction = defines.direction.south, force = c.force
}
if not (endpoint and downstream and assembler and consumer) then
    error('failed to create lane contamination fixture')
end
assembler.set_recipe('iron-gear-wheel')
local line = downstream.get_transport_line(1)
for slot = 1, 4 do
    line.force_insert_at((slot - 0.5) / 4, {name = 'iron-plate', count = 1})
end
rcon.print(helpers.table_to_json({
    endpoint_unit = endpoint.unit_number,
    downstream_unit = downstream.unit_number,
    assembler_unit = assembler.unit_number,
    consumer_unit = consumer.unit_number,
    downstream_item_count = line.get_item_count(),
}))
")"
require_json "lane-contamination fixture has an occupied downstream lane and real consumer" \
    "$LANE_CONTAMINATION_FIXTURE" \
    '(.endpoint_unit | type) == "number"
     and (.downstream_unit | type) == "number"
     and (.assembler_unit | type) == "number"
     and (.consumer_unit | type) == "number"
     and .downstream_item_count == 4'
LANE_CONTAMINATION_ASSEMBLER_UNIT="$(jq -r '.assembler_unit' <<<"$LANE_CONTAMINATION_FIXTURE")"
LANE_CONTAMINATION_CONSUMER_UNIT="$(jq -r '.consumer_unit' <<<"$LANE_CONTAMINATION_FIXTURE")"

LANE_CONTAMINATION_PLAN="$(mcp_tool route_belt '{
    "from_x":-59,
    "from_y":-28,
    "to_x":-56,
    "to_y":-28,
    "belt_type":"transport-belt",
    "item_name":"iron-ore",
    "search_radius":5,
    "dry_run":true,
    "extend_existing":true,
    "allow_underground":false,
    "respect_zones":false
}')"
LANE_CONTAMINATION_PAYLOAD="$(tool_payload "$LANE_CONTAMINATION_PLAN")"
assert_json "route dry-run warns that a connected endpoint would contaminate its downstream lane" \
    "$LANE_CONTAMINATION_PAYLOAD" \
    --argjson assembler "$LANE_CONTAMINATION_ASSEMBLER_UNIT" \
    --argjson consumer "$LANE_CONTAMINATION_CONSUMER_UNIT" \
    '.success == true
     and .dry_run == true
     and .ready_to_execute == true
     and .topology.connected == true
     and .lane_contamination_advisory.kind == "lane_contamination_risk"
     and .lane_contamination_advisory.checked == true
     and .lane_contamination_advisory.risk == true
     and .lane_contamination_advisory.risk_unknown == false
     and .lane_contamination_advisory.item_name == "iron-ore"
     and .lane_contamination_advisory.existing_route_belt_count >= 1
     and .lane_contamination_advisory.downstream_belt_count >= 1
     and .lane_contamination_advisory.incompatible_lane_count >= 1
     and .lane_contamination_advisory.rejecting_consumer_count >= 1
     and any(.lane_contamination_advisory.belts[];
         .position.x == -55 and .position.y == -28
         and .left_lane.would_mix == true
         and any(.left_lane.items[]; .name == "iron-plate" and .count == 4))
     and any(.lane_contamination_advisory.consumers[];
         .inserter_unit_number == $consumer
         and .dropoff_target.unit_number == $assembler
         and .item_acceptance.classification == "rejected"
         and .item_acceptance.recipe == "iron-gear-wheel"
         and any(.item_acceptance.accepted_items[]; . == "iron-plate"))
     and (.warning | type) == "string"
     and .ready_to_call.execute_args.item_name == "iron-ore"'

LANE_CONTAMINATION_UNKNOWN="$(mcp_tool route_belt '{
    "from_x":-59,
    "from_y":-28,
    "to_x":-56,
    "to_y":-28,
    "belt_type":"transport-belt",
    "search_radius":5,
    "dry_run":true,
    "extend_existing":true,
    "allow_underground":false,
    "respect_zones":false
}')"
LANE_CONTAMINATION_UNKNOWN_PAYLOAD="$(tool_payload "$LANE_CONTAMINATION_UNKNOWN")"
assert_json "route dry-run reports unknown compatibility when an occupied reused lane has no item intent" \
    "$LANE_CONTAMINATION_UNKNOWN_PAYLOAD" \
    '.success == true
     and .ready_to_execute == true
     and .lane_contamination_advisory.risk == false
     and .lane_contamination_advisory.risk_unknown == true
     and (.warning | type) == "string"'

LANE_CONTAMINATION_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local units = {
    [$LANE_CONTAMINATION_ASSEMBLER_UNIT] = false,
    [$LANE_CONTAMINATION_CONSUMER_UNIT] = false,
}
local belts = 0
local item_count = 0
for _, entity in pairs(s.find_entities_filtered{area = {{-64, -32}, {-48, -22}}}) do
    if entity.type == 'transport-belt' then
        belts = belts + 1
        item_count = item_count
            + entity.get_transport_line(1).get_item_count()
            + entity.get_transport_line(2).get_item_count()
    end
    if units[entity.unit_number] ~= nil then units[entity.unit_number] = true end
end
rcon.print(helpers.table_to_json({
    belt_count = belts,
    item_count = item_count,
    assembler_exists = units[$LANE_CONTAMINATION_ASSEMBLER_UNIT],
    consumer_exists = units[$LANE_CONTAMINATION_CONSUMER_UNIT],
}))
")"
require_json "lane-contamination dry-runs do not mutate the live fixture" \
    "$LANE_CONTAMINATION_WORLD" \
    '.belt_count == 2
     and .item_count == 4
     and .assembler_exists == true
     and .consumer_exists == true'

# A full belt lane with no output is the physical root of upstream
# waiting-for-space symptoms. Diagnosis must name that terminus and stalled
# lane once, then group the directly traced inserter symptom under it.
DEAD_END_BELT_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
for _, entity in pairs(s.find_entities_filtered{area = {{-63, 23}, {-49, 32}}}) do
    if entity.type ~= 'resource' and entity.type ~= 'character' then entity.destroy() end
end
local belt = s.create_entity{
    name = 'transport-belt', position = {-55.5, 27.5},
    direction = defines.direction.east, force = c.force
}
local assembler = s.create_entity{
    name = 'assembling-machine-1', position = {-53.5, 27.5}, force = c.force
}
local chest = s.create_entity{
    name = 'wooden-chest', position = {-55.5, 25.5}, force = c.force
}
local feeder = s.create_entity{
    name = 'burner-inserter', position = {-55.5, 26.5},
    direction = defines.direction.north, force = c.force
}
if not (belt and assembler and chest and feeder) then
    error('failed to create dead-end belt diagnostic fixture')
end
chest.insert{name = 'copper-plate', count = 1000}
feeder.get_fuel_inventory().insert{name = 'coal', count = 5}
for line_index = 1, 2 do
    local line = belt.get_transport_line(line_index)
    local capacity = math.floor(line.line_length * 4 + 0.5)
    for slot = 1, capacity do
        local item = slot % 2 == 0 and 'iron-ore' or 'coal'
        line.force_insert_at((slot - 0.5) / 4, {name = item, count = 1})
    end
end
rcon.print(helpers.table_to_json({
    belt_unit = belt.unit_number,
    assembler_unit = assembler.unit_number,
    feeder_unit = feeder.unit_number,
    left_count = belt.get_transport_line(1).get_item_count(),
    right_count = belt.get_transport_line(2).get_item_count(),
    line_capacity = math.floor(belt.get_transport_line(1).line_length * 4 + 0.5),
}))
")"
require_json "dead-end diagnostic fixture has two physically full mixed lanes" \
    "$DEAD_END_BELT_FIXTURE" \
    '.left_count == .line_capacity
     and .right_count == .line_capacity
     and .line_capacity >= 4
     and (.belt_unit | type) == "number"
     and (.assembler_unit | type) == "number"
     and (.feeder_unit | type) == "number"'
DEAD_END_BELT_UNIT="$(jq -r '.belt_unit' <<<"$DEAD_END_BELT_FIXTURE")"
DEAD_END_ASSEMBLER_UNIT="$(jq -r '.assembler_unit' <<<"$DEAD_END_BELT_FIXTURE")"
DEAD_END_FEEDER_UNIT="$(jq -r '.feeder_unit' <<<"$DEAD_END_BELT_FIXTURE")"
DEAD_END_FEEDER_STATUS='{}'
for _ in $(seq 1 100); do
    DEAD_END_FEEDER_STATUS="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local feeder = nil
for _, entity in pairs(s.find_entities_filtered{type = 'inserter', area = {{-57, 25}, {-54, 28}}}) do
    if entity.unit_number == $DEAD_END_FEEDER_UNIT then feeder = entity break end
end
local status = feeder and feeder.status or nil
local status_name = nil
for name, value in pairs(defines.entity_status) do
    if value == status then status_name = name break end
end
rcon.print(helpers.table_to_json({status = status_name}))
")"
    if jq -e '.status == "waiting_for_space_in_destination"' \
        >/dev/null 2>&1 <<<"$DEAD_END_FEEDER_STATUS"; then
        break
    fi
    sleep 0.1
done
require_json "full terminal belt back-pressures its live feeder inserter" \
    "$DEAD_END_FEEDER_STATUS" '.status == "waiting_for_space_in_destination"'

DEAD_END_DIAGNOSIS="$(mcp_tool diagnose_factory_blockers \
    '{"x":-55.5,"y":27.5,"radius":6,"limit":20}')"
DEAD_END_DIAGNOSIS_PAYLOAD="$(tool_payload "$DEAD_END_DIAGNOSIS")"
assert_json "factory diagnosis promotes the dead-end belt lane and groups its symptom" \
    "$DEAD_END_DIAGNOSIS_PAYLOAD" \
    --argjson belt "$DEAD_END_BELT_UNIT" \
    --argjson assembler "$DEAD_END_ASSEMBLER_UNIT" \
    --argjson feeder "$DEAD_END_FEEDER_UNIT" \
    '.root_cause.type == "dead_end_belt_lane"
     and .root_cause.primary_unit_number == $belt
     and .root_cause.terminal_belt.blocked_by.unit_number == $assembler
     and .root_cause.belt_run.belt_count == 1
     and .root_cause.grouped_symptom_count >= 1
     and .grouped_symptom_count == .root_cause.grouped_symptom_count
     and any(.root_cause.grouped_symptoms[]; .unit_number == $feeder)
     and any(.root_cause.stalled_lanes[];
         .saturated == true
         and .item_count >= .capacity
         and any(.items[]; .name == "coal")
         and any(.items[]; .name == "iron-ore"))
     and all(.blockers[]; .unit_number != $feeder)
     and .suggested_actions[0].tool == "get_belt_lane_contents"'

# Resource discovery must see every already-generated chunk, report absence as
# normal structured data, and generate bounded nearby terrain only on explicit
# request. The distant chunk is part of this disposable test surface.
RESOURCE_DISCOVERY_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
for _, resource in pairs(s.find_entities_filtered{name = 'crude-oil'}) do
    resource.destroy()
end
for _, resource in pairs(s.find_entities_filtered{name = 'uranium-ore'}) do
    resource.destroy()
end
local oil = s.create_entity{
    name = 'crude-oil', position = {600.5, 600.5}, amount = 100000
}
local chunks = 0
for _ in s.get_chunks() do chunks = chunks + 1 end
rcon.print(helpers.table_to_json({
    oil_created = oil ~= nil,
    oil_x = oil and oil.position.x or nil,
    oil_y = oil and oil.position.y or nil,
    generated_chunks = chunks,
}))
")"
require_json "resource discovery fixture has one patch beyond the old 200-tile ceiling" \
    "$RESOURCE_DISCOVERY_FIXTURE" \
    '.oil_created == true and .oil_x == 600.5 and .oil_y == 600.5'

DISTANT_RESOURCE="$(mcp_tool find_nearest_resource \
    '{"resource_type":"crude-oil","x":0,"y":0}')"
DISTANT_RESOURCE_PAYLOAD="$(tool_payload "$DISTANT_RESOURCE")"
require_json "resource discovery searches all generated chunks without walking" \
    "$DISTANT_RESOURCE_PAYLOAD" \
    '.success == true
     and .found == true
     and .resource.name == "crude-oil"
     and .resource.center.x == 600.5
     and .resource.center.y == 600.5
     and .distance > 800
     and .search.scope == "all_generated_chunks"
     and .search.generated_chunks_added == 0'

MISSING_RESOURCE="$(mcp_tool find_nearest_resource \
    '{"resource_type":"uranium-ore","x":0,"y":0}')"
MISSING_RESOURCE_PAYLOAD="$(tool_payload "$MISSING_RESOURCE")"
assert_json "missing resource is a normal structured tool result" \
    "$MISSING_RESOURCE" '.result.isError != true'
require_json "missing resource distinguishes generated-surface absence from serde failure" \
    "$MISSING_RESOURCE_PAYLOAD" \
    '.success == true
     and .found == false
     and .resource_name == "uranium-ore"
     and .search.scope == "all_generated_chunks"
     and (.guidance | contains("Set explore_radius"))'

EXPLORED_RESOURCE="$(mcp_tool find_nearest_resource \
    '{"resource_type":"uranium-ore","x":900,"y":0,"explore_radius":32}')"
EXPLORED_RESOURCE_PAYLOAD="$(tool_payload "$EXPLORED_RESOURCE")"
require_json "explicit resource exploration generates and searches a bounded area" \
    "$EXPLORED_RESOURCE_PAYLOAD" \
    '.success == true
     and (.found | type) == "boolean"
     and .search.scope == "generated_area"
     and .search.explore_radius == 32
     and .search.generated_chunks_after > .search.generated_chunks_before
     and .search.generated_chunks_added > 0
     and .search.max_explore_radius == 512'

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
     and (.result.content[0].text | fromjson
          | .success == true and .removed == true)'
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
     and (.result.content[0].text | fromjson
          | .success == true and .removed == true)'
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

# A terminal belt can still be load-bearing when an inserter taps it from the
# side. Removal preflight must expose that interaction and its downstream
# machine before an exact-unit removal is allowed to proceed deliberately.
REMOVAL_DEPENDENCY_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
for _, entity in pairs(s.find_entities_filtered{area = {{68, -28}, {77, -19}}}) do
    if entity.type ~= 'character' then entity.destroy() end
end
local tiles = {}
for x = 68, 76 do
    for y = -28, -20 do
        table.insert(tiles, {name = 'landfill', position = {x, y}})
    end
end
s.set_tiles(tiles, true)
c.teleport({70.5, -22.5})
local furnace = s.create_entity{
    name = 'stone-furnace', position = {72, -22}, force = c.force
}
local first = s.create_entity{
    name = 'transport-belt', position = {70.5, -24.5},
    direction = defines.direction.east, force = c.force
}
local second = s.create_entity{
    name = 'transport-belt', position = {71.5, -24.5},
    direction = defines.direction.east, force = c.force
}
local tapped = s.create_entity{
    name = 'transport-belt', position = {72.5, -24.5},
    direction = defines.direction.east, force = c.force
}
local inserter = s.create_entity{
    name = 'inserter', position = {72.5, -23.5},
    direction = defines.direction.north, force = c.force
}
rcon.print(helpers.table_to_json({
    furnace_unit = furnace and furnace.unit_number or nil,
    inserter_unit = inserter and inserter.unit_number or nil,
    first_unit = first and first.unit_number or nil,
    second_unit = second and second.unit_number or nil,
    tapped_unit = tapped and tapped.unit_number or nil,
    pickup_target_unit = inserter and inserter.pickup_target
        and inserter.pickup_target.unit_number or nil,
    drop_target_unit = inserter and inserter.drop_target
        and inserter.drop_target.unit_number or nil,
}))
")"
require_json "removal fixture has a terminal belt side-tapped into one furnace" \
    "$REMOVAL_DEPENDENCY_FIXTURE" \
    '(.furnace_unit | type) == "number"
     and (.inserter_unit | type) == "number"
     and (.tapped_unit | type) == "number"
     and (.second_unit | type) == "number"'

REMOVAL_DEPENDENCY_DRY="$(mcp_tool remove_entity "$(jq -cn \
    --argjson unit "$(jq '.tapped_unit' <<<"$REMOVAL_DEPENDENCY_FIXTURE")" \
    '{unit_number:$unit,dry_run:true}')")"
REMOVAL_DEPENDENCY_DRY_PAYLOAD="$(tool_payload "$REMOVAL_DEPENDENCY_DRY")"
assert_json "removal dependency dry-run is a successful non-mutation" \
    "$REMOVAL_DEPENDENCY_DRY" '.result.isError != true'
require_json "removal preflight warns about the exact side-tapping inserter and furnace" \
    "$REMOVAL_DEPENDENCY_DRY_PAYLOAD" \
    --argjson fixture "$REMOVAL_DEPENDENCY_FIXTURE" \
    '.success == true
     and .dry_run == true
     and .removed == false
     and .would_remove.unit_number == $fixture.tapped_unit
     and .removal_advisory.severity == "warning"
     and .removal_advisory.has_dependents == true
     and any(.removal_advisory.dependencies[];
         .kind == "inserter_interaction"
         and .interaction == "pickup"
         and .inserter.unit_number == $fixture.inserter_unit
         and .downstream_target.unit_number == $fixture.furnace_unit
         and .downstream_input_inserter_count == 1
         and .only_observed_input_path == true)
     and any(.removal_advisory.dependencies[];
         .kind == "belt_connection"
         and .belt.unit_number == $fixture.second_unit)'

REMOVAL_DEPENDENCY_DRY_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
rcon.print(helpers.table_to_json({
    belts = s.count_entities_filtered{
        name = 'transport-belt', position = {72.5, -24.5}, radius = 0.1
    }
}))
")"
assert_json "removal dry-run leaves the tapped belt intact" \
    "$REMOVAL_DEPENDENCY_DRY_WORLD" '.belts == 1'

REMOVAL_DEPENDENCY_EXEC="$(mcp_tool remove_entity "$(jq -cn \
    --argjson unit "$(jq '.tapped_unit' <<<"$REMOVAL_DEPENDENCY_FIXTURE")" \
    '{unit_number:$unit,dry_run:false}')")"
REMOVAL_DEPENDENCY_EXEC_PAYLOAD="$(tool_payload "$REMOVAL_DEPENDENCY_EXEC")"
require_json "deliberate removal succeeds but preserves the dependency warning" \
    "$REMOVAL_DEPENDENCY_EXEC_PAYLOAD" \
    --argjson fixture "$REMOVAL_DEPENDENCY_FIXTURE" \
    '.success == true
     and .dry_run == false
     and .removed == true
     and .unit_number == $fixture.tapped_unit
     and .removal_advisory.severity == "warning"
     and any(.removal_advisory.dependencies[];
         .kind == "inserter_interaction"
         and .inserter.unit_number == $fixture.inserter_unit)'

REMOVAL_DEPENDENCY_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local inserter = nil
for _, candidate in pairs(s.find_entities_filtered{type = 'inserter'}) do
    if candidate.unit_number == $(jq '.inserter_unit' <<<"$REMOVAL_DEPENDENCY_FIXTURE") then
        inserter = candidate
    end
end
rcon.print(helpers.table_to_json({
    tapped_belts = s.count_entities_filtered{
        name = 'transport-belt', position = {72.5, -24.5}, radius = 0.1
    },
    inserter_remains = inserter ~= nil,
    pickup_target_after = inserter and inserter.pickup_target
        and inserter.pickup_target.unit_number or nil,
    furnaces = s.count_entities_filtered{
        name = 'stone-furnace', position = {72, -22}, radius = 0.2
    },
}))
")"
require_json "removal leaves the exact downstream path visibly orphaned" \
    "$REMOVAL_DEPENDENCY_WORLD" \
    '.tapped_belts == 0
     and .inserter_remains == true
     and (.pickup_target_after | not)
     and .furnaces == 1'

INVALID_PLACE="$(mcp_tool place_entity '{"entity_name":"not-a-real-entity","x":28,"y":10,"direction":"north"}')"
assert_json "semantic MCP failures set isError" "$INVALID_PLACE" '.result.isError == true'
INVALID_ROTATION="$(mcp_tool rotate_entity '{"unit_number":1,"direction":"sideways"}')"
assert_json "invalid rotation validation sets isError" "$INVALID_ROTATION" \
    '.result.isError == true
     and (.result.content[0].text | fromjson
          | .success == false and .error_kind == "invalid_direction")'

# place_entity must surface Factorio's resolved inserter endpoints immediately.
# A resource or bare drop tile remains a successful placement, but receives an
# explicit warning because neither destination can accept inserted items.
INSERTER_PLACE_TARGET_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
c.teleport({18.5, -7})
local inv = c.get_main_inventory()
for _, entity in pairs(s.find_entities_filtered{area = {{16, -11}, {21, -2}}}) do
    if entity.type ~= 'character' then entity.destroy() end
end
inv.insert{name = 'inserter', count = 2}
local source = s.create_entity{
    name = 'iron-chest', position = {17.5, -9.5}, force = c.force
}
local resource = s.create_entity{
    name = 'iron-ore', position = {19.5, -9.5}, amount = 1000
}
rcon.print(helpers.table_to_json({
    inventory_before = inv.get_item_count('inserter'),
    source_unit = source and source.unit_number or nil,
    source_position = source and source.position or nil,
    resource_position = resource and resource.position or nil,
}))
")"
require_json "inserter placement target fixture has a chest source and resource destination" \
    "$INSERTER_PLACE_TARGET_FIXTURE" \
    '.inventory_before >= 2
     and (.source_unit | type) == "number"
     and .source_position == {x:17.5,y:-9.5}
     and .resource_position == {x:19.5,y:-9.5}'

RESOURCE_DROP_PLACE="$(mcp_tool place_entity \
    '{"entity_name":"inserter","x":18.5,"y":-9.5,"direction":"west"}')"
RESOURCE_DROP_PAYLOAD="$(tool_payload "$RESOURCE_DROP_PLACE")"
assert_json "resource-drop inserter placement remains successful" "$RESOURCE_DROP_PLACE" \
    '.result.isError != true'
require_json "place_entity exposes exact inserter targets and warns on a resource drop tile" \
    "$RESOURCE_DROP_PAYLOAD" \
    --argjson fixture "$INSERTER_PLACE_TARGET_FIXTURE" \
    '.name == "inserter"
     and (.unit_number | type) == "number"
     and .pickup_target_present == true
     and .pickup_target.unit_number == $fixture.source_unit
     and .pickup_target.position == $fixture.source_position
     and .dropoff_target_present == true
     and .dropoff_target.name == "iron-ore"
     and .dropoff_target.entity_type == "resource"
     and .dropoff_target.position == $fixture.resource_position
     and .dropoff_target_usable == false
     and .inserter_target_advisory.kind == "unusable_inserter_drop_target"
     and .inserter_target_advisory.severity == "warning"
     and .inserter_target_advisory.reason == "resource"
     and (.warning | contains("cannot accept inserted items"))'

BARE_DROP_PLACE="$(mcp_tool place_entity \
    '{"entity_name":"inserter","x":18.5,"y":-4.5,"direction":"west"}')"
BARE_DROP_PAYLOAD="$(tool_payload "$BARE_DROP_PLACE")"
assert_json "bare-drop inserter placement remains successful" "$BARE_DROP_PLACE" \
    '.result.isError != true'
require_json "place_entity reports a null target and warns on bare-ground dropoff" \
    "$BARE_DROP_PAYLOAD" \
    '.name == "inserter"
     and (.unit_number | type) == "number"
     and .dropoff_target_present == false
     and .dropoff_target_usable == false
     and (.dropoff_target | not)
     and .inserter_target_advisory.kind == "unusable_inserter_drop_target"
     and .inserter_target_advisory.severity == "warning"
     and .inserter_target_advisory.reason == "bare_ground"'

INSERTER_PLACE_TARGET_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
local inv = c.get_main_inventory()
rcon.print(helpers.table_to_json({
    resource_drop_inserters = s.count_entities_filtered{
        name = 'inserter', position = {18.5, -9.5}, radius = 0.1
    },
    bare_drop_inserters = s.count_entities_filtered{
        name = 'inserter', position = {18.5, -4.5}, radius = 0.1
    },
    inventory_after = inv.get_item_count('inserter'),
}))
")"
require_json "advisory inserter placements persist and consume both items" \
    "$INSERTER_PLACE_TARGET_WORLD" \
    --argjson fixture "$INSERTER_PLACE_TARGET_FIXTURE" \
    '.resource_drop_inserters == 1
     and .bare_drop_inserters == 1
     and .inventory_after == ($fixture.inventory_before - 2)'

# Inserter targets are resolved through occupied footprint tiles, but their
# reported position must remain the resolved entity's exact Factorio center.
# Two arms touching different tiles of one assembler must not make that same
# unit appear at two different probe-tile positions.
INSERTER_TARGET_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
for _, entity in pairs(s.find_entities_filtered{area = {{17, -11}, {29, -2}}}) do
    if entity.type ~= 'resource' and entity.type ~= 'character' then entity.destroy() end
end
local assembler = s.create_entity{
    name = 'assembling-machine-1', position = {23.5, -6.5}, force = c.force
}
local west = s.create_entity{
    name = 'inserter', position = {21.5, -6.5},
    direction = defines.direction.west, force = c.force
}
local east = s.create_entity{
    name = 'inserter', position = {25.5, -6.5},
    direction = defines.direction.east, force = c.force
}
west.active = false
east.active = false
rcon.print(helpers.table_to_json({
    assembler_unit = assembler.unit_number,
    assembler_position = assembler.position,
    west_unit = west.unit_number,
    west_drop = west.drop_position,
    east_unit = east.unit_number,
    east_drop = east.drop_position,
}))
")"
require_json "inserter-target fixture touches two distinct assembler footprint tiles" \
    "$INSERTER_TARGET_FIXTURE" \
    '.assembler_position == {x:23.5,y:-6.5}
     and (.assembler_unit | type) == "number"
     and (.west_unit | type) == "number"
     and (.east_unit | type) == "number"
     and {x:(.west_drop.x | floor),y:(.west_drop.y | floor)}
         != {x:(.east_drop.x | floor),y:(.east_drop.y | floor)}'
INSERTER_TARGETS="$(mcp_tool analyze_inserters '{"x":23,"y":-6,"radius":6}')"
INSERTER_TARGETS_PAYLOAD="$(tool_payload "$INSERTER_TARGETS")"
assert_json "analyze_inserters resolves both arms through the shared assembler footprint" \
    "$INSERTER_TARGETS" \
    '.result.isError != true'
require_json "one target unit has one exact entity-center position across distinct probe tiles" \
    "$INSERTER_TARGETS_PAYLOAD" \
    --argjson fixture "$INSERTER_TARGET_FIXTURE" \
    '[.[] | select(.unit_number == $fixture.west_unit or .unit_number == $fixture.east_unit)]
        | length == 2
          and all(.[].dropoff_target;
              .unit_number == $fixture.assembler_unit
              and .position == $fixture.assembler_position)'

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

JAMMED_FILTER_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
local target = nil
for _, candidate in pairs(s.find_entities_filtered{type = 'inserter'}) do
    if candidate.unit_number == $ELECTRIC_FILTER_UNIT then target = candidate end
end
local inv = c.get_main_inventory()
local coal_before = inv.get_item_count('coal')
local seeded = target and target.held_stack.set_stack{name = 'coal', count = 1} or false
rcon.print(helpers.table_to_json({
    seeded = seeded,
    unit_number = target and target.unit_number or nil,
    coal_before = coal_before,
    held = target and target.held_stack.valid_for_read and {
        name = target.held_stack.name,
        count = target.held_stack.count,
    } or nil,
}))
")"
require_json "jammed-filter fixture puts one excluded item in the disabled inserter hand" \
    "$JAMMED_FILTER_FIXTURE" \
    --argjson unit "$ELECTRIC_FILTER_UNIT" \
    '.seeded == true
     and .unit_number == $unit
     and .held == {name:"coal",count:1}'
JAMMED_COAL_BEFORE="$(jq -r '.coal_before' <<<"$JAMMED_FILTER_FIXTURE")"

UNJAM_FILTER="$(mcp_tool configure_inserter "$(jq -cn \
    --argjson unit "$ELECTRIC_FILTER_UNIT" \
    '{unit_number:$unit,allowed_items:["iron-plate"]}')")"
UNJAM_FILTER_PAYLOAD="$(tool_payload "$UNJAM_FILTER")"
assert_json "configure_inserter returns an excluded held item instead of reporting a false recovery" \
    "$UNJAM_FILTER" \
    '.result.isError != true'
assert_json "held-item recovery preserves identity and reports explicit before/after state" \
    "$UNJAM_FILTER_PAYLOAD" \
    --argjson unit "$ELECTRIC_FILTER_UNIT" \
    '.success == true
     and .unit_number == $unit
     and .entity_identity_preserved == true
     and .readback_verified == true
     and [.filters[].name] == ["iron-plate"]
     and .held_stack_present_before == true
     and .held_stack_before.name == "coal"
     and .held_stack_before.count == 1
     and .held_stack_present_after == false
     and .held_stack_violated_whitelist == true
     and .held_stack_evacuated == true
     and .held_stack_returned_count == 1'
UNJAM_FILTER_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
local target = nil
for _, candidate in pairs(s.find_entities_filtered{type = 'inserter'}) do
    if candidate.unit_number == $ELECTRIC_FILTER_UNIT then target = candidate end
end
local filter = target and target.get_filter(1) or nil
rcon.print(helpers.table_to_json({
    same_unit = target and target.unit_number == $ELECTRIC_FILTER_UNIT or false,
    held_present = target and target.held_stack.valid_for_read or false,
    coal = c.get_main_inventory().get_item_count('coal'),
    filter = type(filter) == 'table' and filter.name or filter,
}))
")"
require_json "live inserter is unjammed and the excluded item is conserved in character inventory" \
    "$UNJAM_FILTER_WORLD" \
    --argjson coal_before "$JAMMED_COAL_BEFORE" \
    '.same_unit == true
     and .held_present == false
     and .coal == ($coal_before + 1)
     and .filter == "iron-plate"'

FULL_INVENTORY_JAM="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
local target = nil
for _, candidate in pairs(s.find_entities_filtered{type = 'inserter'}) do
    if candidate.unit_number == $ELECTRIC_FILTER_UNIT then target = candidate end
end
local inv = c.get_main_inventory()
inv.clear()
for index = 1, #inv do inv[index].set_stack{name = 'stone', count = 50} end
local seeded = target and target.held_stack.set_stack{name = 'coal', count = 1} or false
rcon.print(helpers.table_to_json({
    seeded = seeded,
    coal_capacity = inv.get_insertable_count{name = 'coal'},
    filter = target and target.get_filter(1) or nil,
}))
")"
require_json "full-inventory fixture cannot accept the excluded held stack" \
    "$FULL_INVENTORY_JAM" \
    '.seeded == true
     and .coal_capacity == 0
     and ((.filter | if type == "object" then .name else . end) == "iron-plate")'
BLOCKED_UNJAM="$(mcp_tool configure_inserter "$(jq -cn \
    --argjson unit "$ELECTRIC_FILTER_UNIT" \
    '{unit_number:$unit,allowed_items:["copper-plate"]}')")"
BLOCKED_UNJAM_PAYLOAD="$(tool_payload "$BLOCKED_UNJAM")"
assert_json "configure_inserter fails rather than losing an excluded held item" \
    "$BLOCKED_UNJAM" \
    '.result.isError == true'
assert_json "failed held-item evacuation reports the jam and verifies filter rollback" \
    "$BLOCKED_UNJAM_PAYLOAD" \
    --argjson unit "$ELECTRIC_FILTER_UNIT" \
    '.success == false
     and .error_kind == "held_stack_evacuation_failed"
     and .unit_number == $unit
     and .held_stack_present_before == true
     and .held_stack_present_after == true
     and .held_stack_before.name == "coal"
     and .held_stack_after.name == "coal"
     and .held_stack_violated_whitelist == true
     and .held_stack_evacuation.required == true
     and .held_stack_evacuation.succeeded == false
     and .held_stack_evacuation.insertable_count == 0
     and .rollback_succeeded == true
     and .rollback_readback_verified == true'
BLOCKED_UNJAM_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
local target = nil
for _, candidate in pairs(s.find_entities_filtered{type = 'inserter'}) do
    if candidate.unit_number == $ELECTRIC_FILTER_UNIT then target = candidate end
end
local filter = target and target.get_filter(1) or nil
local report = {
    same_unit = target and target.unit_number == $ELECTRIC_FILTER_UNIT or false,
    held = target and target.held_stack.valid_for_read and {
        name = target.held_stack.name,
        count = target.held_stack.count,
    } or nil,
    filter = type(filter) == 'table' and filter.name or filter,
    coal = c.get_main_inventory().get_item_count('coal'),
}
if target then target.held_stack.clear() end
c.get_main_inventory().clear()
rcon.print(helpers.table_to_json(report))
")"
require_json "failed evacuation preserves the exact inserter, held item, and previous filter" \
    "$BLOCKED_UNJAM_WORLD" \
    '.same_unit == true
     and .held == {name:"coal",count:1}
     and .filter == "iron-plate"
     and .coal == 0'

ATOMIC_FILTER_FAILURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
local inv = c.get_main_inventory()
local seeded = inv.insert{name = 'burner-inserter', count = 1}
local inventory_before = inv.get_item_count('burner-inserter')
local entities_before = s.count_entities_filtered{type = 'inserter'}
local report = helpers.json_to_table(remote.call(
    'claude_interface',
    'place_filtered_inserter',
    '$AGENT_ID',
    'burner-inserter',
    22.5,
    -5.5,
    defines.direction.north,
    {'coal', 'coal'}
))
local placement_unit = report.placement and report.placement.unit_number or nil
local placement_present = false
for _, entity in pairs(s.find_entities_filtered{type = 'inserter'}) do
    if placement_unit and entity.unit_number == placement_unit then
        placement_present = true
        break
    end
end
rcon.print(helpers.table_to_json({
    seeded = seeded,
    inventory_before = inventory_before,
    inventory_after = inv.get_item_count('burner-inserter'),
    entities_before = entities_before,
    entities_after = s.count_entities_filtered{type = 'inserter'},
    placement_present = placement_present,
    report = report,
}))
")"
require_json "atomic duplicate-filter failure removes the exact inserter and returns its item" \
    "$ATOMIC_FILTER_FAILURE" \
    '.seeded == 1
     and .inventory_after == .inventory_before
     and .entities_after == .entities_before
     and .placement_present == false
     and .report.success == false
     and .report.error_kind == "atomic_filter_configuration_failed"
     and (.report.placement.unit_number | type) == "number"
     and .report.filter.success == false
     and .report.filter.error_kind == "invalid_allowed_items"
     and .report.filter.unit_number == .report.placement.unit_number
     and (.report.filter.error | contains("duplicate item"))
     and .report.rollback.success == true
     and .report.rollback.entity_removed == true
     and .report.rollback.item_returned == 1'

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

# Splitters are deliberate branch points, not isolated belt tiles. Replacing a
# compatible live belt must use Factorio's native fast-replace behavior so the
# displaced belts are conserved and both splitter outputs remain operational.
SPLITTER_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
for _, entity in pairs(s.find_entities_filtered{area = {{48, 18}, {60, 25}}}) do
    if entity.type ~= 'resource' and entity.type ~= 'character' then entity.destroy() end
end
c.teleport({50.5, 24})
local inv = c.get_main_inventory()
inv.clear()
inv.insert{name = 'splitter', count = 1}
local replaced = nil
local source = nil
for x = 49, 57 do
    local belt = s.create_entity{
        name = 'transport-belt', position = {x + 0.5, 20.5},
        direction = defines.direction.east, force = c.force
    }
    if x == 49 then source = belt end
    if x == 52 then replaced = belt end
end
for x = 49, 57 do
    s.create_entity{
        name = 'transport-belt', position = {x + 0.5, 21.5},
        direction = defines.direction.east, force = c.force
    }
end
rcon.print(helpers.table_to_json({
    replaced_unit = replaced and replaced.unit_number or nil,
    source_unit = source and source.unit_number or nil,
    splitter_before = inv.get_item_count('splitter'),
    belt_before = inv.get_item_count('transport-belt'),
    surface_belts_before = s.count_entities_filtered{name = 'transport-belt', area = {{48, 18}, {60, 25}}},
    native_fast_replace = s.can_fast_replace{
        name = 'splitter', position = {52.5, 20.5},
        direction = defines.direction.east, force = c.force
    }
}))
")"
require_json "splitter fixture has two exact replaceable bus lanes and two outputs" \
    "$SPLITTER_FIXTURE" \
    '.splitter_before == 1
     and .belt_before == 0
     and .surface_belts_before == 18
     and .native_fast_replace == true
     and (.replaced_unit | type) == "number"
     and (.source_unit | type) == "number"'
SPLITTER_REPLACED_UNIT="$(jq -r '.replaced_unit' <<<"$SPLITTER_FIXTURE")"

SPLITTER_PLACE="$(mcp_tool place_entity '{
    "entity_name":"splitter",
    "x":52.5,
    "y":20.5,
    "direction":"east"
}')"
SPLITTER_PLACE_PAYLOAD="$(tool_payload "$SPLITTER_PLACE")"
assert_json "place_entity fast-replaces compatible bus lanes with a splitter" \
    "$SPLITTER_PLACE" \
    '.result.isError != true'
assert_json "splitter placement reports the native fast-replace result" \
    "$SPLITTER_PLACE_PAYLOAD" \
    '.name == "splitter"
     and .entity_type == "splitter"
     and .direction == 4
     and .fast_replaced == true
     and (.unit_number | type) == "number"'

SPLITTER_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
local inv = c.get_main_inventory()
local replaced_survived = false
for _, entity in pairs(s.find_entities_filtered{area = {{48, 18}, {60, 25}}}) do
    if entity.unit_number == $SPLITTER_REPLACED_UNIT then replaced_survived = true end
end
local inserted = 0
for _, belt in pairs(s.find_entities_filtered{name = 'transport-belt', area = {{49, 20}, {52, 21}}}) do
    for line_index = 1, 2 do
        if belt.get_transport_line(line_index).insert_at_back({name = 'iron-plate', count = 1}) then
            inserted = inserted + 1
        end
    end
end
rcon.print(helpers.table_to_json({
    replaced_survived = replaced_survived,
    inserted = inserted,
    splitter_inventory = inv.get_item_count('splitter'),
    returned_belts = inv.get_item_count('transport-belt'),
    surface_belts = s.count_entities_filtered{name = 'transport-belt', area = {{48, 18}, {60, 25}}},
    splitters = s.count_entities_filtered{name = 'splitter', area = {{48, 18}, {60, 25}}}
}))
")"
require_json "native splitter replacement conserves the displaced belt and exact entity counts" \
    "$SPLITTER_WORLD" \
    '.replaced_survived == false
     and .inserted > 1
     and .splitter_inventory == 0
     and .returned_belts == 2
     and .surface_belts == 16
     and .splitters == 1'
sleep 3
SPLITTER_FLOW="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local main = 0
local branch = 0
for _, belt in pairs(s.find_entities_filtered{name = 'transport-belt', area = {{53, 20}, {58, 22}}}) do
    local count = belt.get_transport_line(1).get_item_count('iron-plate')
        + belt.get_transport_line(2).get_item_count('iron-plate')
    if belt.position.y < 21 then main = main + count else branch = branch + count end
end
rcon.print(helpers.table_to_json({main = main, branch = branch}))
")"
assert_json "the native splitter keeps the original trunk and new branch carrying items" \
    "$SPLITTER_FLOW" \
    '.main > 0 and .branch > 0'

# Direct-smelter candidates must apply the same compound footprint invariant as
# execution. The planner used to mark an individually legal furnace and
# inserter ready even when their two footprints occupied the same tile.
DIRECT_SMELTER_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
for _, entity in pairs(s.find_entities_filtered{area = {{16, 23}, {36, 33}}}) do
    if entity.type ~= 'resource' and entity.type ~= 'character' then entity.destroy() end
end
c.teleport({30.5, 31.5})
local inv = c.get_main_inventory()
inv.clear()
inv.insert{name = 'stone-furnace', count = 1}
inv.insert{name = 'burner-inserter', count = 1}
inv.insert{name = 'transport-belt', count = 1}
inv.insert{name = 'coal', count = 50}
local belt = s.create_entity{
    name = 'transport-belt', position = {24.5, 27.5},
    direction = defines.direction.east, force = c.force
}
local inserted = 0
for line_index = 1, 2 do
    for _ = 1, 8 do
        if belt.get_transport_line(line_index).insert_at_back({name = 'iron-ore', count = 1}) then
            inserted = inserted + 1
        end
    end
end
rcon.print(helpers.table_to_json({
    belt_unit = belt and belt.unit_number or nil,
    inserted = inserted,
    furnaces = inv.get_item_count('stone-furnace'),
    inserters = inv.get_item_count('burner-inserter'),
    coal = inv.get_item_count('coal')
}))
")"
require_json "direct-smelter fixture has a live terminal ore belt and exact materials" \
    "$DIRECT_SMELTER_FIXTURE" \
    '.inserted == 2
     and (.belt_unit | type) == "number"
     and .furnaces == 1
     and .inserters == 1
     and .coal == 50'

DIRECT_SMELTER_PLAN="$(mcp_tool execute_direct_smelter '{
    "output_x":24.5,
    "output_y":27.5,
    "output_direction":"east",
    "furnace_name":"stone-furnace",
    "inserter_name":"burner-inserter",
    "belt_name":"transport-belt",
    "radius":6,
    "dry_run":true
}')"
DIRECT_SMELTER_PLAN_PAYLOAD="$(tool_payload "$DIRECT_SMELTER_PLAN")"
assert_json "direct-smelter dry-run selects disjoint furnace and inserter footprints" \
    "$DIRECT_SMELTER_PLAN_PAYLOAD" \
    '.success == true
     and .dry_run == true
     and .preflight.ready == true
     and .plan.success == true
     and .plan.selected.ready == true
     and .plan.selected.furnace.overlaps_input_inserter == false
     and (.plan.selected.furnace.position != .plan.selected.input_inserter.position)
     and (all(.preflight.errors[]?; .kind != "entity_footprint_overlap"))'

DIRECT_SMELTER_EXECUTE="$(mcp_tool execute_direct_smelter '{
    "output_x":24.5,
    "output_y":27.5,
    "output_direction":"east",
    "furnace_name":"stone-furnace",
    "inserter_name":"burner-inserter",
    "belt_name":"transport-belt",
    "radius":6,
    "dry_run":false
}')"
DIRECT_SMELTER_EXECUTE_PAYLOAD="$(tool_payload "$DIRECT_SMELTER_EXECUTE")"
assert_json "direct-smelter executes and verifies the selected non-overlapping cell" \
    "$DIRECT_SMELTER_EXECUTE_PAYLOAD" \
    '.success == true
     and .placement_success == true
     and .automation_verified.success == true
     and .automation_verified.furnace_working == true
     and .automation_verified.placed_units_exist == true
     and .preflight.ready == true'

DIRECT_SMELTER_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local furnace = s.find_entities_filtered{name = 'stone-furnace', area = {{16, 23}, {36, 33}}}[1]
local inserter = s.find_entities_filtered{name = 'burner-inserter', area = {{16, 23}, {36, 33}}}[1]
local overlap = nil
if furnace and inserter then
    local a = furnace.bounding_box
    local b = inserter.bounding_box
    overlap = a.left_top.x < b.right_bottom.x
        and a.right_bottom.x > b.left_top.x
        and a.left_top.y < b.right_bottom.y
        and a.right_bottom.y > b.left_top.y
end
rcon.print(helpers.table_to_json({
    furnace_unit = furnace and furnace.unit_number or nil,
    inserter_unit = inserter and inserter.unit_number or nil,
    furnace_position = furnace and furnace.position or nil,
    inserter_position = inserter and inserter.position or nil,
    bounding_boxes_overlap = overlap
}))
")"
require_json "executed direct-smelter entities have distinct identities and disjoint live bounding boxes" \
    "$DIRECT_SMELTER_WORLD" \
    '(.furnace_unit | type) == "number"
     and (.inserter_unit | type) == "number"
     and .furnace_unit != .inserter_unit
     and .furnace_position != .inserter_position
     and .bounding_boxes_overlap == false'

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

# Existing steam entities must not make additive capacity a planner dead end.
# The conservative default still diagnoses/redirects an unhealthy footprint;
# explicit additional_capacity must run the checked independent-plant search.
STEAM_EXPANSION_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
for _, entity in pairs(s.find_entities_filtered{area = {{112, -80}, {140, -60}}}) do
    if entity.type ~= 'character' then entity.destroy() end
end
local tiles = {}
for x = 112, 139 do
    for y = -80, -61 do
        table.insert(tiles, {
            name = x <= 116 and 'water' or 'landfill',
            position = {x, y},
        })
    end
end
s.set_tiles(tiles, true)
local existing = s.create_entity{
    name = 'steam-engine', position = {132.5, -70.5},
    direction = defines.direction.north, force = c.force
}
local inv = c.get_main_inventory()
inv.insert{name = 'offshore-pump', count = 2}
inv.insert{name = 'boiler', count = 2}
inv.insert{name = 'steam-engine', count = 2}
inv.insert{name = 'pipe', count = 20}
inv.insert{name = 'small-electric-pole', count = 20}
rcon.print(helpers.table_to_json({
    existing_unit = existing and existing.unit_number or nil,
    pumps = s.count_entities_filtered{name = 'offshore-pump', area = {{112, -80}, {140, -60}}},
    boilers = s.count_entities_filtered{name = 'boiler', area = {{112, -80}, {140, -60}}},
    engines = s.count_entities_filtered{name = 'steam-engine', area = {{112, -80}, {140, -60}}},
}))
")"
require_json "steam expansion fixture has an existing plant footprint and open shoreline" \
    "$STEAM_EXPANSION_FIXTURE" \
    '(.existing_unit | type) == "number"
     and .pumps == 0 and .boilers == 0 and .engines == 1'

STEAM_STARTER_REDIRECT="$(mcp_tool plan_steam_power '{
    "water_x1":112,"water_y1":-80,"water_x2":116,"water_y2":-61,
    "target_x":136,"target_y":-70
}')"
STEAM_STARTER_REDIRECT_PAYLOAD="$(tool_payload "$STEAM_STARTER_REDIRECT")"
require_json "starter steam planning names the exact additive-capacity escape hatch" \
    "$STEAM_STARTER_REDIRECT_PAYLOAD" \
    '.success == false
     and .checked == 0
     and .existing_plant.has_existing_plant == true
     and any(.blockers[]; .type == "existing_steam_power_found")
     and .suggested_next_tool.tool == "plan_steam_power"
     and .suggested_next_tool.tool_args.intent == "additional_capacity"'

STEAM_ADDITIVE_PLAN="$(mcp_tool plan_steam_power '{
    "water_x1":112,"water_y1":-80,"water_x2":116,"water_y2":-61,
    "target_x":136,"target_y":-70,"intent":"additional_capacity"
}')"
STEAM_ADDITIVE_PAYLOAD="$(tool_payload "$STEAM_ADDITIVE_PLAN")"
require_json "additive steam planning checks a complete independent capacity layout" \
    "$STEAM_ADDITIVE_PAYLOAD" \
    --argjson fixture "$STEAM_EXPANSION_FIXTURE" \
    '.success == true
     and .placement_success == true
     and .intent == "additional_capacity"
     and .auto_selected_intent == false
     and .existing_plant.has_existing_plant == true
     and any(.existing_plant.entities[]; .unit_number == $fixture.existing_unit)
     and .checked > 0
     and .pump_candidates > 0
     and (.missing_items | length) == 0
     and .plan.success == true
     and .plan.offshore_pump.allowed == true
     and .plan.boiler.allowed == true
     and .plan.steam_engine.allowed == true
     and (.guidance | contains("separate pump, boiler, and engine"))'

STEAM_ADDITIVE_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
rcon.print(helpers.table_to_json({
    pumps = s.count_entities_filtered{name = 'offshore-pump', area = {{112, -80}, {140, -60}}},
    boilers = s.count_entities_filtered{name = 'boiler', area = {{112, -80}, {140, -60}}},
    engines = s.count_entities_filtered{name = 'steam-engine', area = {{112, -80}, {140, -60}}},
}))
")"
assert_json "additive steam planning remains a non-mutating contract" \
    "$STEAM_ADDITIVE_WORLD" '.pumps == 0 and .boilers == 0 and .engines == 1'

DISCONNECTED_ENGINE_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
for _, entity in pairs(s.find_entities_filtered{area = {{92, -58}, {110, -42}}}) do
    if entity.type ~= 'resource' and entity.type ~= 'character' then entity.destroy() end
end
local engine = s.create_entity{
    name = 'steam-engine', position = {100.5, -50.5},
    direction = defines.direction.north, force = c.force
}
local pole = s.create_entity{
    name = 'small-electric-pole', position = {106.5, -50.5}, force = c.force
}
engine.fluidbox[1] = {name = 'steam', amount = 200, temperature = 165}
local fluids = engine.get_fluid_contents()
local status = nil
for name, value in pairs(defines.entity_status) do
    if value == engine.status then status = name break end
end
rcon.print(helpers.table_to_json({
    engine_unit = engine.unit_number,
    engine_position = engine.position,
    pole_unit = pole.unit_number,
    pole_position = pole.position,
    status = status,
    connected = engine.is_connected_to_electric_network(),
    steam = fluids.steam or 0,
}))
")"
require_json "steam fixture has a fueled engine and nearby pole but no electric connection" \
    "$DISCONNECTED_ENGINE_FIXTURE" \
    '.status == "not_plugged_in_electric_network"
     and .connected == false
     and .steam > 0
     and (.engine_unit | type) == "number"
     and (.pole_unit | type) == "number"'
DISCONNECTED_ENGINE_DIAG="$(mcp_tool diagnose_steam_power '{"x":101,"y":-50,"radius":10}')"
DISCONNECTED_ENGINE_PAYLOAD="$(tool_payload "$DISCONNECTED_ENGINE_DIAG")"
assert_json "disconnected steam engine diagnosis is a successful model-visible query" \
    "$DISCONNECTED_ENGINE_DIAG" \
    '.result.isError != true'
require_json "diagnosis promotes the authoritative disconnected status into an actionable warning" \
    "$DISCONNECTED_ENGINE_PAYLOAD" \
    --argjson fixture "$DISCONNECTED_ENGINE_FIXTURE" \
    '.status == "warning"
     and .next_action == "inspect_existing_steam_power"
     and .summary.warning_issues == 1
     and .summary.issue_count == 1
     and any(.entities[];
         .unit_number == $fixture.engine_unit
         and .status == "not_plugged_in_electric_network"
         and .connected_to_electric_network == false)
     and any(.issues[];
         .type == "steam_engine_pole_route_incomplete"
         and .severity == "warning"
         and .entity.unit_number == $fixture.engine_unit
         and .details.nearest_pole.unit_number == $fixture.pole_unit
         and (.action | contains($fixture.engine_unit | tostring)))'

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

# Reproduce the assembler-feed contradiction with canonical geometry: the
# unreserved shortest route enters the pickup goal through the future inserter
# tile, while a longer complete route exists around it. The controller must
# reserve that footprint during both dry-run and execution.
ASSEMBLER_FEED_ROUTE_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
for _, entity in pairs(s.find_entities_filtered{area = {{76, -56}, {86, -42}}}) do
    if entity.type ~= 'character' then entity.destroy() end
end
local tiles = {}
for x = 76, 85 do
    for y = -56, -43 do
        table.insert(tiles, {name = 'landfill', position = {x, y}})
    end
end
s.set_tiles(tiles, true)
c.teleport({84.5, -51.5})
local inv = c.get_main_inventory()
inv.insert{name = 'transport-belt', count = 100}
inv.insert{name = 'inserter', count = 1}
local assembler = s.create_entity{
    name = 'assembling-machine-1', position = {80.5, -47.5}, force = c.force
}
local west_wall = s.create_entity{
    name = 'stone-wall', position = {79.5, -44.5}, force = c.force
}
local east_wall = s.create_entity{
    name = 'stone-wall', position = {81.5, -44.5}, force = c.force
}
rcon.print(helpers.table_to_json({
    assembler_unit = assembler and assembler.unit_number or nil,
    assembler_position = assembler and assembler.position or nil,
    west_wall_unit = west_wall and west_wall.unit_number or nil,
    east_wall_unit = east_wall and east_wall.unit_number or nil,
    inserters_before = inv.get_item_count('inserter'),
}))
")"
require_json "assembler-feed fixture has one clear machine and two goal-side blockers" \
    "$ASSEMBLER_FEED_ROUTE_FIXTURE" \
    '.assembler_position == {x:80.5,y:-47.5}
     and (.assembler_unit | type) == "number"
     and (.west_wall_unit | type) == "number"
     and (.east_wall_unit | type) == "number"
     and .inserters_before >= 1'

ASSEMBLER_FEED_UNRESERVED="$(mcp_tool route_belt '{
    "from_x":80,
    "from_y":-54,
    "to_x":80,
    "to_y":-45,
    "belt_type":"transport-belt",
    "search_radius":5,
    "dry_run":true,
    "extend_existing":false,
    "allow_underground":false,
    "respect_zones":false
}')"
ASSEMBLER_FEED_UNRESERVED_PAYLOAD="$(tool_payload "$ASSEMBLER_FEED_UNRESERVED")"
require_json "ordinary shortest route reproduces the future inserter-footprint conflict" \
    "$ASSEMBLER_FEED_UNRESERVED_PAYLOAD" \
    '.success == true
     and any(.planned_belts[];
         (.position.x | floor) == 80 and (.position.y | floor) == -46)'

ASSEMBLER_FEED_PLAN="$(mcp_tool build_assembler_feed "$(jq -cn \
    --argjson unit "$(jq '.assembler_unit' <<<"$ASSEMBLER_FEED_ROUTE_FIXTURE")" \
    '{
        assembler_unit_number:$unit,
        recipe:"",
        item_name:"iron-plate",
        from_x:80,
        from_y:-54,
        pickup_x:80,
        pickup_y:-45,
        inserter_x:80.5,
        inserter_y:-45.5,
        inserter_direction:"south",
        belt_type:"transport-belt",
        search_radius:5,
        dry_run:true,
        respect_zones:false,
        allow_underground:false,
        extend_existing:false,
        verify_radius:5
    }')")"
ASSEMBLER_FEED_PLAN_PAYLOAD="$(tool_payload "$ASSEMBLER_FEED_PLAN")"
assert_json "assembler-feed dry-run finds a complete route around its inserter" \
    "$ASSEMBLER_FEED_PLAN" '.result.isError != true'
require_json "assembler-feed preflight is internally consistent and executable" \
    "$ASSEMBLER_FEED_PLAN_PAYLOAD" \
    '.success == true
     and .preflight.ready == true
     and .preflight.placements.input_inserter.allowed == true
     and .preflight.endpoint_topology.success == true
     and .route.controller_reserved_tiles == [{x:80,y:-46}]
     and all(.route.planned_belts[];
         ((.position.x | floor) == 80 and (.position.y | floor) == -46) | not)
     and all(.preflight.routes.errors[]?; .kind != "route_entity_overlap")
     and .ready_to_call.tool == "build_assembler_feed"
     and .ready_to_call.execute_args.dry_run == false'

ASSEMBLER_FEED_EXEC_ARGS="$(jq -c '.ready_to_call.execute_args' \
    <<<"$ASSEMBLER_FEED_PLAN_PAYLOAD")"
ASSEMBLER_FEED_RESULT="$(mcp_tool build_assembler_feed "$ASSEMBLER_FEED_EXEC_ARGS")"
ASSEMBLER_FEED_RESULT_PAYLOAD="$(tool_payload "$ASSEMBLER_FEED_RESULT")"
require_json "assembler-feed executes the same disjoint route and exact inserter" \
    "$ASSEMBLER_FEED_RESULT_PAYLOAD" \
    '.success == true
     and .placement_success == true
     and .infrastructure_verified.success == true
     and .infrastructure_verified.endpoint_topology.success == true
     and all(.route.placed_entities[];
         ((.position.x | floor) == 80 and (.position.y | floor) == -46) | not)'

ASSEMBLER_FEED_ROUTE_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
local inv = c.get_main_inventory()
rcon.print(helpers.table_to_json({
    reserved_tile_belts = s.count_entities_filtered{
        type = 'transport-belt', position = {80.5, -45.5}, radius = 0.1
    },
    pickup_belts = s.count_entities_filtered{
        type = 'transport-belt', position = {80.5, -44.5}, radius = 0.1
    },
    input_inserters = s.count_entities_filtered{
        type = 'inserter', position = {80.5, -45.5}, radius = 0.1
    },
    inserters_after = inv.get_item_count('inserter'),
}))
")"
require_json "executed assembler feed keeps route and inserter footprints disjoint" \
    "$ASSEMBLER_FEED_ROUTE_WORLD" \
    --argjson fixture "$ASSEMBLER_FEED_ROUTE_FIXTURE" \
    '.reserved_tile_belts == 0
     and .pickup_belts == 1
     and .input_inserters == 1
     and .inserters_after == ($fixture.inserters_before - 1)'

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

# An independent route uses a character-walking collision map, where belts are
# intentionally walkable. It must still treat every existing belt as occupied
# build space rather than silently planning a fast replacement through it.
INDEPENDENT_ROUTE_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
local existing = s.find_entity('transport-belt', {31.5, 10.5})
c.teleport({28.5, 10.5})
c.get_main_inventory().insert{name = 'transport-belt', count = 32}
rcon.print(helpers.table_to_json({
    unit_number = existing and existing.unit_number or nil,
    direction = existing and existing.direction or nil
}))
")"
require_json "independent-route fixture retains the perpendicular live belt" \
    "$INDEPENDENT_ROUTE_FIXTURE" \
    '(.unit_number | type) == "number" and .direction == 0'
INDEPENDENT_ROUTE_UNIT="$(jq -r '.unit_number' <<<"$INDEPENDENT_ROUTE_FIXTURE")"

INDEPENDENT_ROUTE_PLAN="$(mcp_tool route_belt '{
    "from_x":30,
    "from_y":10,
    "to_x":32,
    "to_y":10,
    "belt_type":"transport-belt",
    "search_radius":3,
    "dry_run":true,
    "extend_existing":false,
    "allow_underground":false,
    "respect_zones":false
}')"
INDEPENDENT_ROUTE_PLAN_PAYLOAD="$(tool_payload "$INDEPENDENT_ROUTE_PLAN")"
assert_json "independent route dry-run detours around occupied belt tiles" \
    "$INDEPENDENT_ROUTE_PLAN_PAYLOAD" \
    '.success == true
     and .complete_route != false
     and .ready_to_execute == true
     and all(.planned_belts[]?; ((.position.x == 31.5 and .position.y == 10.5) | not))
     and all(.planned_new_belts[]?; ((.position.x == 31.5 and .position.y == 10.5) | not))'

INDEPENDENT_ROUTE_EXECUTE="$(mcp_tool route_belt '{
    "from_x":30,
    "from_y":10,
    "to_x":32,
    "to_y":10,
    "belt_type":"transport-belt",
    "search_radius":3,
    "dry_run":false,
    "extend_existing":false,
    "allow_underground":false,
    "respect_zones":false
}')"
INDEPENDENT_ROUTE_EXECUTE_PAYLOAD="$(tool_payload "$INDEPENDENT_ROUTE_EXECUTE")"
assert_json "independent route executes atomically without replacing the crossing belt" \
    "$INDEPENDENT_ROUTE_EXECUTE_PAYLOAD" \
    '.success == true
     and .complete_route == true
     and .placed >= 5
     and (.rollback // null) == null'
INDEPENDENT_ROUTE_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local existing = nil
for _, candidate in pairs(s.find_entities_filtered{name = 'transport-belt', area = {{29, 8}, {34, 13}}}) do
    if candidate.unit_number == $INDEPENDENT_ROUTE_UNIT then existing = candidate end
end
rcon.print(helpers.table_to_json({
    same_unit = existing and existing.valid and existing.unit_number == $INDEPENDENT_ROUTE_UNIT or false,
    direction = existing and existing.direction or nil,
    belt_count = s.count_entities_filtered{name = 'transport-belt', area = {{29, 8}, {34, 13}}}
}))
")"
assert_json "independent route preserves the exact crossing belt and its direction" \
    "$INDEPENDENT_ROUTE_WORLD" \
    '.same_unit == true and .direction == 0 and .belt_count >= 6'

# A new underground endpoint inside an existing pair's span can silently steal
# the old endpoint in Factorio. The router must preserve the live pair while
# still allowing harmless surface belts to cross the reserved span.
UNDERGROUND_PAIR_FIXTURE="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
for _, entity in pairs(s.find_entities_filtered{area = {{37, -27}, {49, -18}}}) do
    if entity.type ~= 'resource' and entity.type ~= 'character' then entity.destroy() end
end
local force = c.force
force.technologies['logistics'].researched = true
local route_start = s.create_entity{
    name = 'transport-belt', position = {38.5, -24.5},
    direction = defines.direction.east, force = force
}
local before = s.create_entity{
    name = 'transport-belt', position = {39.5, -24.5},
    direction = defines.direction.east, force = force
}
local input = s.create_entity{
    name = 'underground-belt', position = {40.5, -24.5},
    direction = defines.direction.east, type = 'input', force = force
}
local output = s.create_entity{
    name = 'underground-belt', position = {44.5, -24.5},
    direction = defines.direction.east, type = 'output', force = force
}
local after = s.create_entity{
    name = 'transport-belt', position = {45.5, -24.5},
    direction = defines.direction.east, force = force
}
if not (route_start and before and input and output and after and input.neighbours == output) then
    error('failed to create authoritative underground pair fixture')
end
c.teleport({38.5, -21.5})
local inv = c.get_main_inventory()
inv.clear()
inv.insert{name = 'transport-belt', count = 100}
inv.insert{name = 'underground-belt', count = 20}
rcon.print(helpers.table_to_json({
    route_start_unit = route_start.unit_number,
    input_unit = input.unit_number,
    output_unit = output.unit_number,
    input_direction = input.direction,
    output_direction = output.direction,
    input_type = input.belt_to_ground_type,
    output_type = output.belt_to_ground_type,
    paired = input.neighbours == output
}))
")"
require_json "underground-pair fixture exposes Factorio's authoritative pair state" \
    "$UNDERGROUND_PAIR_FIXTURE" \
    '.paired == true
     and .input_direction == 4
     and .output_direction == 4
     and .input_type == "input"
     and .output_type == "output"
     and (.route_start_unit | type) == "number"
     and (.input_unit | type) == "number"
     and (.output_unit | type) == "number"'
UNDERGROUND_PAIR_INPUT_UNIT="$(jq -r '.input_unit' <<<"$UNDERGROUND_PAIR_FIXTURE")"
UNDERGROUND_PAIR_OUTPUT_UNIT="$(jq -r '.output_unit' <<<"$UNDERGROUND_PAIR_FIXTURE")"
UNDERGROUND_ROUTE_START_UNIT="$(jq -r '.route_start_unit' <<<"$UNDERGROUND_PAIR_FIXTURE")"

UNDERGROUND_CROSSING_PLAN="$(mcp_tool route_belt '{
    "from_x":38,
    "from_y":-24,
    "to_x":42,
    "to_y":-24,
    "belt_type":"transport-belt",
    "search_radius":6,
    "dry_run":true,
    "extend_existing":true,
    "allow_underground":true,
    "respect_zones":false
}')"
UNDERGROUND_CROSSING_PLAN_PAYLOAD="$(tool_payload "$UNDERGROUND_CROSSING_PLAN")"
assert_json "route dry-run reserves every tile inside the existing underground pair" \
    "$UNDERGROUND_CROSSING_PLAN_PAYLOAD" \
    --argjson input "$UNDERGROUND_PAIR_INPUT_UNIT" \
    --argjson output "$UNDERGROUND_PAIR_OUTPUT_UNIT" \
    '.success == true
     and .ready_to_execute == true
     and .preserved_underground_pair_count >= 1
     and any(.preserved_underground_pairs[]?;
         .first.unit_number == $input and .second.unit_number == $output
         or .first.unit_number == $output and .second.unit_number == $input)
     and all(.planned_new_belts[]?;
         if .kind == "Surface" then true
         else ((.position.y | floor) != -25
               or (.position.x | floor) < 41
               or (.position.x | floor) > 43)
         end)'

UNDERGROUND_CROSSING_EXECUTE="$(mcp_tool route_belt '{
    "from_x":38,
    "from_y":-24,
    "to_x":42,
    "to_y":-24,
    "belt_type":"transport-belt",
    "search_radius":6,
    "dry_run":false,
    "extend_existing":true,
    "allow_underground":true,
    "respect_zones":false
}')"
UNDERGROUND_CROSSING_EXECUTE_PAYLOAD="$(tool_payload "$UNDERGROUND_CROSSING_EXECUTE")"
assert_json "route executes without re-pairing the existing underground line" \
    "$UNDERGROUND_CROSSING_EXECUTE_PAYLOAD" \
    '.success == true and .complete_route == true and .placed > 0'

UNDERGROUND_PAIR_WORLD="$(raw_lua "
local s = game.surfaces['buddy-live-regression']
local input = nil
local output = nil
local route_start = nil
for _, candidate in pairs(s.find_entities_filtered{name = 'transport-belt'}) do
    if candidate.unit_number == $UNDERGROUND_ROUTE_START_UNIT then route_start = candidate end
end
for _, candidate in pairs(s.find_entities_filtered{name = 'underground-belt'}) do
    if candidate.unit_number == $UNDERGROUND_PAIR_INPUT_UNIT then input = candidate end
    if candidate.unit_number == $UNDERGROUND_PAIR_OUTPUT_UNIT then output = candidate end
end
local internal_endpoints = 0
for _, candidate in pairs(s.find_entities_filtered{name = 'underground-belt', area = {{41, -25}, {44, -24}}}) do
    if candidate.unit_number ~= $UNDERGROUND_PAIR_INPUT_UNIT
        and candidate.unit_number ~= $UNDERGROUND_PAIR_OUTPUT_UNIT then
        internal_endpoints = internal_endpoints + 1
    end
end
rcon.print(helpers.table_to_json({
    same_route_start = route_start and route_start.valid
        and route_start.unit_number == $UNDERGROUND_ROUTE_START_UNIT
        and route_start.direction == defines.direction.east or false,
    same_input = input and input.valid and input.unit_number == $UNDERGROUND_PAIR_INPUT_UNIT or false,
    same_output = output and output.valid and output.unit_number == $UNDERGROUND_PAIR_OUTPUT_UNIT or false,
    still_paired = input and output and input.neighbours == output or false,
    internal_endpoints = internal_endpoints
}))
")"
assert_json "the exact pre-existing underground units remain paired after routing" \
    "$UNDERGROUND_PAIR_WORLD" \
    '.same_route_start == true
     and .same_input == true
     and .same_output == true
     and .still_paired == true
     and .internal_endpoints == 0'

UNDERGROUND_FLOW="$(mcp_tool analyze_item_flow '{
    "source_x":39,
    "source_y":-25,
    "target_x":45,
    "target_y":-25,
    "radius":10
}')"
UNDERGROUND_FLOW_PAYLOAD="$(tool_payload "$UNDERGROUND_FLOW")"
assert_json "item-flow analysis certifies the live path through the underground pair" \
    "$UNDERGROUND_FLOW_PAYLOAD" \
    '.status == "connected"
     and .connected == true
     and .connectivity_certified == true
     and .analysis_scope.modeled_underground_belts >= 2
     and .analysis_scope.unsupported_transports == null
     and any(.reachable_belts[]?; .x == 40 and .y == -25)
     and any(.reachable_belts[]?; .x == 44 and .y == -25)'

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

# Banked furnace output is existing recoverable stock, not a function of how
# much new source material this one-shot bootstrap inserts. A caller requesting
# 39 output with one source item must receive all 39 available plates.
BANKED_FURNACE_FIXTURE="$(raw_lua "
game.tick_paused = false
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
remote.call('claude_interface', 'clear_walk_target', '$AGENT_ID')
c.force = game.forces.player
c.teleport({23.5, -20.5}, game.surfaces['buddy-live-regression'])
local s = c.surface
for _, entity in pairs(s.find_entities_filtered{area = {{16, -27}, {28, -13}}}) do
    if entity.type ~= 'resource' and entity.type ~= 'character' then entity.destroy() end
end
local inv = c.get_main_inventory()
inv.clear()
local seeded_coal = inv.insert{name = 'coal', count = 1}
local seeded_source = inv.insert{name = 'copper-ore', count = 1}
local furnace = s.create_entity{name = 'stone-furnace', position = {20, -20}, force = c.force}
if not furnace then error('failed to construct banked furnace fixture') end
local result_inventory = furnace.get_inventory(defines.inventory.furnace_result)
local banked_output = result_inventory.insert{name = 'iron-plate', count = 39}
rcon.print(helpers.table_to_json({
    furnace_unit = furnace.unit_number,
    seeded_coal = seeded_coal,
    seeded_source = seeded_source,
    banked_output = banked_output
}))
")"
require_json "banked furnace fixture has one new source item and 39 finished plates" \
    "$BANKED_FURNACE_FIXTURE" \
    '.seeded_coal == 1
     and .seeded_source == 1
     and .banked_output == 39
     and (.furnace_unit | type) == "number"'
BANKED_FURNACE_UNIT="$(jq -r '.furnace_unit' <<<"$BANKED_FURNACE_FIXTURE")"

BANKED_COLLECTION="$(mcp_tool bootstrap_smelting_once "$(jq -cn \
    --argjson unit "$BANKED_FURNACE_UNIT" \
    '{
        furnace_unit_number:$unit,
        fuel_item:"coal",
        fuel_count:1,
        source_item:"copper-ore",
        source_count:1,
        output_item:"iron-plate",
        output_count:39,
        wait_ticks:1,
        verify_radius:4,
        dry_run:false
    }')")"
BANKED_COLLECTION_PAYLOAD="$(tool_payload "$BANKED_COLLECTION")"
assert_json "bootstrap collection honors output_count independently of source_count" \
    "$BANKED_COLLECTION_PAYLOAD" \
    --argjson unit "$BANKED_FURNACE_UNIT" \
    '.success == true
     and .furnace.unit_number == $unit
     and ((.actions | map(select(.operation == "collect_furnace_output")) | first) as $collect
        | $collect.requested_count == 39
          and $collect.result == 39
          and $collect.success == true)'

BANKED_FURNACE_WORLD="$(raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local s = c.surface
local furnace
for _, entity in pairs(s.find_entities_filtered{area = {{16, -27}, {28, -13}}}) do
    if entity.unit_number == $BANKED_FURNACE_UNIT then furnace = entity break end
end
rcon.print(helpers.table_to_json({
    same_unit = furnace and furnace.valid and furnace.unit_number == $BANKED_FURNACE_UNIT or false,
    furnace_result = furnace and furnace.get_inventory(defines.inventory.furnace_result).get_item_count('iron-plate') or -1,
    character_output = c.get_main_inventory().get_item_count('iron-plate')
}))
")"
assert_json "banked collection preserves furnace identity and transfers all finished output" \
    "$BANKED_FURNACE_WORLD" \
    '.same_unit == true and .furnace_result == 0 and .character_output == 39'

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
