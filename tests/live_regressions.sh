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

# Coordinate removal must fail closed when more than one entity overlaps the
# target. Exact unit-number removal remains available for deliberate changes.
raw_lua "
game.tick_paused = true
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
c.teleport({8.5, 0.5})
local s = c.surface
for _, e in pairs(s.find_entities_filtered{position = {10.5, 0.5}, radius = 0.2}) do
    if e.type ~= 'resource' and e.type ~= 'character' then e.destroy() end
end
s.create_entity{name = 'transport-belt', position = {10.5, 0.5}, direction = defines.direction.east, force = c.force}
s.create_entity{name = 'entity-ghost', inner_name = 'small-electric-pole', position = {10.5, 0.5}, force = c.force}
" >/dev/null
AMBIGUOUS_REMOVE="$(raw_lua "rcon.print(remote.call('claude_interface', 'remove_entity_at', '$AGENT_ID', 10.5, 0.5))")"
raw_lua "game.tick_paused = false" >/dev/null
assert_json "coordinate removal fails closed on overlap" "$AMBIGUOUS_REMOVE" \
    '.success == false and .error_kind == "ambiguous_entity" and (.candidates | length) >= 2'
BELT_REMAINS="$(raw_lua "local s = game.surfaces['buddy-live-regression']; rcon.print(helpers.table_to_json({count = s.count_entities_filtered{name = 'transport-belt', position = {10.5, 0.5}, radius = 0.2}}))")"
assert_json "ambiguous removal does not mine nearby infrastructure" "$BELT_REMAINS" '.count == 1'

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
    analyze_inserters analyze_item_flow bootstrap_smelting_once \
    build_assembler_feed build_assembler_output build_automation_science \
    build_lab_feed build_recipe_assembler_cell craft diagnose_factory_blockers \
    diagnose_steam_power execute_direct_smelter execute_edge_miner \
    execute_entity_placement_near extend_power_to find_nearest_resource \
    get_available_research get_belt_lane_contents get_entities \
    get_machine_belt_positions get_power_status get_recipe get_recipes_for_item \
    get_research_status mine_at place_entity plan_automation_science \
    plan_machine_output plan_recipe_assembler_cell plan_steam_power \
    production_statistics remove_entity render_map repair_fuel_sustainability \
    rotate_entity route_belt set_recipe situation_report start_research unstuck \
    verify_production walk_to | jq -Rsc 'split("\n")[:-1] | sort')"
assert_json "model receives the exact 42-tool gameplay surface" "$TOOLS" \
    --argjson expected "$EXPECTED_TOOLS" \
    '([.result.tools[].name] | sort) == $expected and (.result.tools | length) == 42'
TOOLS_SCHEMA_BYTES="$(jq -c '.result.tools' <<<"$TOOLS" | wc -c)"
if (( TOOLS_SCHEMA_BYTES <= 61440 )); then
    pass "model tool schema stays below 60 KiB"
else
    fail "model tool schema stays below 60 KiB" \
        "observed $TOOLS_SCHEMA_BYTES bytes"
fi

INVALID_PLACE="$(mcp_tool place_entity '{"entity_name":"not-a-real-entity","x":28,"y":10,"direction":"north"}')"
assert_json "semantic MCP failures set isError" "$INVALID_PLACE" '.result.isError == true'

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
