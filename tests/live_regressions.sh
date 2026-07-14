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

raw_lua() {
    FACTORIOCTL_ALLOW_RAW_LUA=1 "${CLI[@]}" exec "$1"
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

# Build a clean test surface and an independent NPC. Raw Lua is fixture setup;
# the lifecycle behavior under test goes through the shipped mod remote.
SETUP="$(raw_lua "
local name = 'buddy-live-regression'
local old = remote.call('claude_interface', 'get_character', '$AGENT_ID')
if old and old.valid then old.destroy() end
local surface = game.surfaces[name]
if surface then game.delete_surface(surface) end
surface = game.create_surface(name, {peaceful_mode = true})
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
rcon.print(remote.call('claude_interface', 'pre_place_character_result', '$AGENT_ID', name, 0))
")"
assert_json "NPC is independently created on the requested surface" "$SETUP" \
    '.status == "created" and .planet == "buddy-live-regression"'

# An established NPC must remain distinct from a human character and must not
# be moved when Buddy repeats its idempotent startup lifecycle call.
IDENTITY="$(raw_lua "
local player = game.get_player('live-regression-human')
if not player then player = game.create_player{name = 'live-regression-human'} end
if not player.character then
    local nauvis = game.surfaces['nauvis']
    local position = nauvis.find_non_colliding_position('character', {0, 0}, 64, 0.5)
    local human = nauvis.create_entity{name = 'character', position = position, force = game.forces.player}
    player.set_controller{type = defines.controllers.character, character = human}
end
local agent = remote.call('claude_interface', 'get_character', '$AGENT_ID')
local agent_unit = agent.unit_number
local agent_surface = agent.surface.name
local agent_x = agent.position.x
local human_unit = player.character and player.character.unit_number or nil
local status = remote.call('claude_interface', 'pre_place_character_result', '$AGENT_ID', 'nauvis', 100)
local after = remote.call('claude_interface', 'get_character', '$AGENT_ID')
rcon.print(helpers.table_to_json({
    lifecycle = helpers.json_to_table(status),
    distinct = human_unit ~= nil and human_unit ~= agent_unit,
    same_unit = after.unit_number == agent_unit,
    same_surface = after.surface.name == agent_surface,
    same_x = after.position.x == agent_x
}))
")"
assert_json "startup never adopts or relocates the human character" "$IDENTITY" \
    '.lifecycle.status == "already_placed"
     and .distinct == true
     and .same_unit == true
     and .same_surface == true
     and .same_x == true'

# Ordinary Factorio chat must append the same inbox that wakes Buddy; it must
# not be limited to the custom GUI send button.
CHAT_TOKEN="live-console-chat-$BASHPID"
raw_lua "local p = game.get_player('live-regression-human'); script.raise_event(defines.events.on_console_chat, {player_index = p.index, message = '$CHAT_TOKEN'})" >/dev/null
CHAT_RECORD="$(grep -F "\"message\":\"$CHAT_TOKEN\"" "$ROOT/.factorio-test-data/script-output/claude-chat/input.jsonl" | tail -n 1 || true)"
assert_json "normal Factorio chat reaches the Buddy inbox" "$CHAT_RECORD" \
    --arg token "$CHAT_TOKEN" '.message == $token and .target_agent != null'

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

# A full inventory may cause placement to fail, but must never delete an item
# from the ground while trying to clear the tile.
raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
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
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
c.teleport({8.5, 0.5})
local s = c.surface
for _, e in pairs(s.find_entities_filtered{position = {10.5, 0.5}, radius = 0.2}) do
    if e.type ~= 'resource' and e.type ~= 'character' then e.destroy() end
end
s.create_entity{name = 'transport-belt', position = {10.5, 0.5}, direction = defines.direction.east, force = c.force}
s.create_entity{name = 'item-on-ground', position = {10.5, 0.5}, stack = {name = 'iron-plate', count = 1}}
" >/dev/null
AMBIGUOUS_REMOVE="$(raw_lua "rcon.print(remote.call('claude_interface', 'remove_entity_at', '$AGENT_ID', 10.5, 0.5))")"
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

# Native unit lookup must work beyond the former +/-500 scan boundary.
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

# Install a wrong-facing belt in an otherwise eastbound three-tile corridor.
# The MCP router may reject it or route around it, but may not count it as a
# connected, reusable segment.
raw_lua "
local c = remote.call('claude_interface', 'get_character', '$AGENT_ID')
c.teleport({28.5, 10.5})
local inv = c.get_main_inventory()
inv.clear()
inv.insert{name = 'transport-belt', count = 100}
local s = c.surface
for _, e in pairs(s.find_entities_filtered{area = {{29, 8}, {34, 13}}}) do
    if e.type ~= 'resource' and e.type ~= 'character' then e.destroy() end
end
s.create_entity{name = 'transport-belt', position = {31.5, 10.5}, direction = defines.direction.north, force = c.force}
" >/dev/null

RCON_BEFORE="$(rcon_connection_count)"
start_mcp

TOOLS="$(mcp_send tools/list '{}')"
assert_json "model tool surface hides lifecycle and hand-transfer shortcuts" "$TOOLS" \
    '[.result.tools[].name] as $names
     | (["execute_lua", "insert_items", "extract_items", "hand_feed_furnace",
         "feed_lab_from_inventory", "place_character", "register_agent",
         "broadcast_thought"] | all(. as $name | ($names | index($name) | not)))'

INVALID_PLACE="$(mcp_tool place_entity '{"entity_name":"not-a-real-entity","x":28,"y":10,"direction":"north"}')"
assert_json "semantic MCP failures set isError" "$INVALID_PLACE" '.result.isError == true'

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

# Multiple tools in one MCP process must share its RCON transport.
mcp_tool get_tick '{}' >/dev/null
mcp_tool get_tick '{}' >/dev/null
RCON_AFTER="$(rcon_connection_count)"
RCON_DELTA=$((RCON_AFTER - RCON_BEFORE))
if (( RCON_DELTA <= 1 )); then
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
