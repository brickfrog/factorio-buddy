-- Claude Interface - In-game chat with Claude AI (multi-agent)
-- Communication: write_file -> bridge daemon -> Claude CLI -> RCON -> remote interface

local GUI_FRAME = "claude_interface_frame"
local MAX_MESSAGES = 100
local INPUT_FILE = "claude-chat/input.jsonl"
local autonomy = require("autonomy")
local characters = require("characters")
local diagnostics = require("diagnostics")
local entities = require("entities")
local json_response = require("json_response")
local json_remote_call = json_response.remote_call
local inventory = require("inventory")
local inventory_contents = inventory.contents
local inventory_define_for = inventory.define_for
local placement = require("placement")
local power = require("power")
local recipes = require("recipes")
local research = require("research")
local transport = require("transport")
local world = require("world")
local find_factorioctl_character = characters.find
local remember_factorioctl_character = characters.remember

-- ============================================================
-- Storage
-- ============================================================

local function init_storage()
    storage.messages = storage.messages or {}
    storage.msg_counter = storage.msg_counter or 0
    storage.agents = storage.agents or {"default"}
    storage.agent_labels = storage.agent_labels or {}
    storage.active_agent = storage.active_agent or {}
    storage._rcon_queue = storage._rcon_queue or {}
    storage.spectator_mode = storage.spectator_mode or false
    storage.spectator_previous = storage.spectator_previous or {}
    -- Agent character entities and walk targets (for deterministic on_tick processing)
    storage.characters = storage.characters or {}
    storage.walk_state = storage.walk_state or {}
    storage.walk_targets = storage.walk_targets or {}
    storage.blueprints = storage.blueprints or {}
    -- Map markers for agent characters (chart tag references)
    storage.agent_tags = storage.agent_tags or {}
    -- In-game chat captured for the bridge. Registered in the MOD (not the
    -- level script) so every peer has an identical handler set and clients can
    -- join — runtime-injected level-script handlers break MP ("not multiplayer safe").
    storage.chat_messages = storage.chat_messages or {}
end

-- Ensure per-agent message tables exist for a player
local function ensure_agent_messages(player_index)
    if not storage.messages[player_index] then
        storage.messages[player_index] = {}
    end
    for _, agent_name in ipairs(storage.agents) do
        if not storage.messages[player_index][agent_name] then
            storage.messages[player_index][agent_name] = {}
        end
    end
end

-- Get the active agent for a player (defaults to first registered)
local function get_active_agent(player_index)
    local agent = storage.active_agent[player_index]
    if agent then
        -- Verify agent still exists
        for _, a in ipairs(storage.agents) do
            if a == agent then return agent end
        end
    end
    return storage.agents[1] or "default"
end

-- ============================================================
-- Shortcut Bar State
-- ============================================================

local function update_shortcut_state(player)
    local is_open = player.gui.screen[GUI_FRAME] ~= nil
    player.set_shortcut_toggled("claude-interface-toggle", is_open)
end

-- ============================================================
-- GUI Construction
-- ============================================================

local function get_agent_display_name(player)
    return settings.get_player_settings(player)["claude-interface-agent-label"].value or "Claude"
end

local function add_message_label(chat_flow, role, text, player)
    local caption
    if role == "user" then
        caption = "[color=1,0.85,0.4]You:[/color] " .. text
    elseif role == "claude" then
        local name = player and get_agent_display_name(player) or "Claude"
        caption = "[color=0.6,0.8,1]" .. name .. ":[/color] " .. text
    else
        caption = "[color=0.6,0.6,0.6]" .. text .. "[/color]"
    end

    local label = chat_flow.add{
        type = "label",
        caption = caption
    }
    label.style.single_line = false
    label.style.horizontally_stretchable = true
    return label
end

local function restore_chat(player, chat_flow, agent_name)
    ensure_agent_messages(player.index)
    local msgs = storage.messages[player.index][agent_name]
    if not msgs then return end
    for _, msg in ipairs(msgs) do
        add_message_label(chat_flow, msg.role, msg.text, player)
    end
end

-- Get the chat_flow for a specific agent tab
local function get_agent_chat_flow(frame, agent_name)
    if not frame or not frame.valid then return nil end
    local tabbed = frame["ci_agent_tabs"]
    if not tabbed then return nil end
    local scroll = tabbed["ci_scroll_" .. agent_name]
    if not scroll then return nil end
    return scroll["ci_chat_" .. agent_name]
end

-- Get the scroll-pane for a specific agent tab
local function get_agent_scroll(frame, agent_name)
    if not frame or not frame.valid then return nil end
    local tabbed = frame["ci_agent_tabs"]
    if not tabbed then return nil end
    return tabbed["ci_scroll_" .. agent_name]
end

-- Find the tab index for a given agent name
local function find_tab_index(tabbed, agent_name)
    for i, tab_and_content in ipairs(tabbed.tabs) do
        if tab_and_content.tab.name == "ci_tab_" .. agent_name then
            return i
        end
    end
    return nil
end

-- Get display label for a tab (short name or agent_name)
local function get_agent_label(agent_name)
    return storage.agent_labels[agent_name] or agent_name
end

-- Create a single agent tab + scroll-pane + chat_flow inside a tabbed-pane
local function create_agent_tab(tabbed, player, agent_name)
    local tab = tabbed.add{
        type = "tab",
        name = "ci_tab_" .. agent_name,
        caption = get_agent_label(agent_name),
    }

    local scroll = tabbed.add{
        type = "scroll-pane",
        name = "ci_scroll_" .. agent_name,
        direction = "vertical",
    }
    scroll.style.vertically_stretchable = true
    scroll.style.horizontally_stretchable = true

    local chat_flow = scroll.add{
        type = "flow",
        name = "ci_chat_" .. agent_name,
        direction = "vertical",
    }
    chat_flow.style.vertical_spacing = 6
    chat_flow.style.horizontally_stretchable = true

    tabbed.add_tab(tab, scroll)

    -- Restore history for this agent
    restore_chat(player, chat_flow, agent_name)
    scroll.scroll_to_bottom()

    return tab, scroll
end

local function create_gui(player)
    if player.gui.screen[GUI_FRAME] then return end

    ensure_agent_messages(player.index)

    -- Main frame
    local frame = player.gui.screen.add{
        type = "frame",
        name = GUI_FRAME,
        direction = "vertical"
    }
    frame.auto_center = true
    frame.style.width = 700
    frame.style.height = 650

    -- Titlebar: drag + close
    local titlebar = frame.add{
        type = "flow",
        name = "ci_titlebar",
        direction = "horizontal"
    }
    titlebar.drag_target = frame
    titlebar.style.vertical_align = "center"

    local title_text = settings.get_player_settings(player)["claude-interface-title"].value or "Claude AI"
    titlebar.add{
        type = "label",
        name = "ci_title",
        caption = title_text,
        style = "frame_title"
    }

    local spacer = titlebar.add{
        type = "empty-widget",
        name = "ci_spacer",
        style = "draggable_space"
    }
    spacer.style.horizontally_stretchable = true
    spacer.style.height = 24
    spacer.drag_target = frame

    titlebar.add{
        type = "sprite-button",
        name = "ci_close",
        sprite = "utility/close",
        style = "close_button",
        tooltip = "Close [Ctrl+Shift+C]"
    }

    -- Tabbed pane for agents
    local tabbed = frame.add{
        type = "tabbed-pane",
        name = "ci_agent_tabs",
    }
    tabbed.style.vertically_stretchable = true
    tabbed.style.horizontally_stretchable = true

    -- Create a tab per registered agent
    local active_agent = get_active_agent(player.index)
    local active_idx = 1
    for i, agent_name in ipairs(storage.agents) do
        create_agent_tab(tabbed, player, agent_name)
        if agent_name == active_agent then
            active_idx = i
        end
    end

    -- Select the active tab
    tabbed.selected_tab_index = active_idx
    storage.active_agent[player.index] = storage.agents[active_idx]

    -- Status indicator
    frame.add{
        type = "label",
        name = "ci_status",
        caption = "[color=0.4,0.8,0.4]Ready[/color]"
    }

    -- Input area: textfield + send button
    local input_flow = frame.add{
        type = "flow",
        name = "ci_input_flow",
        direction = "horizontal"
    }
    input_flow.style.vertical_align = "center"
    input_flow.style.horizontally_stretchable = true

    local input = input_flow.add{
        type = "textfield",
        name = "ci_input",
        tooltip = "Type a message and press Enter"
    }
    input.style.horizontally_stretchable = true
    input.style.minimal_width = 0
    input.style.maximal_width = 0

    input_flow.add{
        type = "sprite-button",
        name = "ci_send",
        sprite = "utility/enter",
        style = "tool_button",
        tooltip = "Send"
    }

    -- Focus input and register for Escape-close
    input.focus()
    player.opened = frame
end

local function destroy_gui(player)
    local frame = player.gui.screen[GUI_FRAME]
    if frame and frame.valid then
        frame.destroy()
    end
end

local function toggle_gui(player)
    if player.gui.screen[GUI_FRAME] then
        destroy_gui(player)
    else
        create_gui(player)
    end
    update_shortcut_state(player)
end

-- ============================================================
-- Chat Logic
-- ============================================================

local function save_message(player_index, agent_name, role, text)
    ensure_agent_messages(player_index)
    local msgs = storage.messages[player_index][agent_name]
    table.insert(msgs, {
        role = role,
        text = text,
        tick = game.tick,
    })
    while #msgs > MAX_MESSAGES do
        table.remove(msgs, 1)
    end
end

local function add_chat_message(player, agent_name, role, text)
    save_message(player.index, agent_name, role, text)

    if role == "claude" then
        player.print("[" .. get_agent_label(agent_name) .. "] " .. text)
    end

    local frame = player.gui.screen[GUI_FRAME]
    if not frame or not frame.valid then return end

    local chat_flow = get_agent_chat_flow(frame, agent_name)
    if not chat_flow then return end
    add_message_label(chat_flow, role, text, player)

    while #chat_flow.children > MAX_MESSAGES do
        chat_flow.children[1].destroy()
    end

    local scroll = get_agent_scroll(frame, agent_name)
    if scroll then scroll.scroll_to_bottom() end

    -- Badge for non-active tabs
    local active = get_active_agent(player.index)
    if agent_name ~= active and role ~= "user" then
        local tabbed = frame["ci_agent_tabs"]
        if tabbed then
            local tab_idx = find_tab_index(tabbed, agent_name)
            if tab_idx then
                local tab_obj = tabbed.tabs[tab_idx].tab
                local current = tab_obj.badge_text
                local count = 0
                if current and current ~= "" then
                    count = tonumber(current) or 0
                end
                tab_obj.badge_text = tostring(count + 1)
            end
        end
    end
end

local function set_status(player, status_text)
    local frame = player.gui.screen[GUI_FRAME]
    if not frame or not frame.valid then return end
    frame["ci_status"].caption = status_text
end

local function write_bridge_message(player_index, player_name, target_agent, message, tick)
    storage.msg_counter = storage.msg_counter + 1
    local payload = {
        id = storage.msg_counter,
        player_index = player_index,
        player_name = player_name,
        message = message,
        target_agent = target_agent,
        tick = tick or game.tick,
    }
    helpers.write_file(INPUT_FILE, helpers.table_to_json(payload) .. "\n", true, 0)
end

local function send_to_bridge(player, message)
    write_bridge_message(
        player.index,
        player.name,
        get_active_agent(player.index),
        message,
        game.tick
    )
end

local function handle_send(player)
    local frame = player.gui.screen[GUI_FRAME]
    if not frame or not frame.valid then return end

    local input = frame["ci_input_flow"]["ci_input"]
    local text = input.text
    if text == "" or text == nil then return end

    input.text = ""
    input.focus()

    local agent_name = get_active_agent(player.index)
    add_chat_message(player, agent_name, "user", text)
    set_status(player, "[color=1,0.8,0.2]Thinking...[/color]")
    send_to_bridge(player, text)
end

-- ============================================================
-- Agent Management
-- ============================================================

local function agent_exists(name)
    for _, a in ipairs(storage.agents) do
        if a == name then return true end
    end
    return false
end

local function register_agent(agent_name, label)
    if label then
        storage.agent_labels[agent_name] = label
    end
    if agent_exists(agent_name) then return end
    table.insert(storage.agents, agent_name)

    -- Create message tables for all players
    for _, player in pairs(game.players) do
        ensure_agent_messages(player.index)
    end

    -- Add tab to all open GUIs
    for _, player in pairs(game.players) do
        local frame = player.gui.screen[GUI_FRAME]
        if frame and frame.valid then
            local tabbed = frame["ci_agent_tabs"]
            if tabbed then
                create_agent_tab(tabbed, player, agent_name)
            end
        end
    end
end

local function unregister_agent(agent_name)
    -- Allow removing "default" only if other agents exist
    if agent_name == "default" and #storage.agents <= 1 then return end
    local idx = nil
    for i, a in ipairs(storage.agents) do
        if a == agent_name then idx = i; break end
    end
    if not idx then return end

    table.remove(storage.agents, idx)

    -- Remove tab from all open GUIs
    for _, player in pairs(game.players) do
        local frame = player.gui.screen[GUI_FRAME]
        if frame and frame.valid then
            local tabbed = frame["ci_agent_tabs"]
            if tabbed then
                local tab_idx = find_tab_index(tabbed, agent_name)
                if tab_idx then
                    tabbed.remove_tab(tabbed.tabs[tab_idx].tab)
                    -- Clean up the scroll pane element
                    local scroll = tabbed["ci_scroll_" .. agent_name]
                    if scroll then scroll.destroy() end
                    local tab_el = tabbed["ci_tab_" .. agent_name]
                    if tab_el then tab_el.destroy() end
                end
            end
        end

        -- Reset active agent if it was the removed one
        if storage.active_agent[player.index] == agent_name then
            storage.active_agent[player.index] = storage.agents[1] or "default"
        end
    end
end

-- ============================================================
-- Queue Processing (on_tick)
-- ============================================================

-- Apply walk states to agent characters each tick.
-- Processed in on_tick for deterministic multiplayer behavior.
local function process_walk_states()
    if not storage.walk_state then return end
    local to_remove = {}
    for agent_id, ws in pairs(storage.walk_state) do
        local c = find_factorioctl_character(agent_id)
        if c and c.valid then
            c.walking_state = ws
        end
        -- Clean up stopped entries (applied once, then removed)
        if not ws.walking then
            table.insert(to_remove, agent_id)
        end
    end
    for _, agent_id in ipairs(to_remove) do
        storage.walk_state[agent_id] = nil
    end
end

local WALK_DIRECTION_THRESHOLD = 0.41421356237 -- tan(22.5 degrees)

local function walk_direction_toward(dx, dy)
    local abs_dx = math.abs(dx)
    local abs_dy = math.abs(dy)

    if abs_dy <= abs_dx * WALK_DIRECTION_THRESHOLD then
        return dx >= 0 and defines.direction.east or defines.direction.west
    end
    if abs_dx <= abs_dy * WALK_DIRECTION_THRESHOLD then
        return dy >= 0 and defines.direction.south or defines.direction.north
    end
    if dx >= 0 then
        return dy >= 0 and defines.direction.southeast or defines.direction.northeast
    end
    return dy >= 0 and defines.direction.southwest or defines.direction.northwest
end

local function stop_target_walk(agent_id, character)
    storage.walk_targets[agent_id] = nil
    if storage.walk_state then storage.walk_state[agent_id] = nil end
    if character and character.valid then
        character.walking_state = {walking = false}
    end
end

-- Drive queued targets through the character's ordinary Factorio walking state.
-- Factorio applies movement and collision; this tick handler only chooses a
-- direction, detects arrival/stalls, and reliably stops cancelled/expired walks.
local function process_walk_targets()
    if not storage.walk_targets then return end
    for agent_id, tgt in pairs(storage.walk_targets) do
        local c = find_factorioctl_character(agent_id)
        if not (c and c.valid) then
            stop_target_walk(agent_id, nil)
            goto continue
        end
        if tgt.expires_tick and game.tick >= tgt.expires_tick then
            stop_target_walk(agent_id, c)
            goto continue
        end

        local dx = tgt.x - c.position.x
        local dy = tgt.y - c.position.y
        local dist = math.sqrt(dx * dx + dy * dy)
        local sp = c.character_running_speed or 0.15
        local arrival_distance = math.max(0.2, sp * 1.5)

        if dist <= arrival_distance then
            stop_target_walk(agent_id, c)
        else
            local last_x = tgt.last_x or c.position.x
            local last_y = tgt.last_y or c.position.y
            local moved = math.sqrt(
                (c.position.x - last_x) * (c.position.x - last_x)
                    + (c.position.y - last_y) * (c.position.y - last_y)
            )
            if moved < 0.001 then
                tgt.stuck_ticks = (tgt.stuck_ticks or 0) + 1
            else
                tgt.stuck_ticks = 0
            end
            tgt.last_x = c.position.x
            tgt.last_y = c.position.y
            if tgt.stuck_ticks >= 120 then
                stop_target_walk(agent_id, c)
            else
                c.walking_state = {
                    walking = true,
                    direction = walk_direction_toward(dx, dy),
                }
            end
        end

        ::continue::
    end
end

-- Update map markers for agent characters (every 60 ticks = 1 second)
local function update_agent_markers()
    if not storage.characters then return end
    if not storage.agent_tags then storage.agent_tags = {} end
    for agent_id, _ in pairs(storage.characters) do
        local c = find_factorioctl_character(agent_id)
        if c and c.valid then
            local tag = storage.agent_tags[agent_id]
            if tag and tag.valid then
                -- Update position if moved
                local tp = tag.position
                local cp = c.position
                if tp.x ~= cp.x or tp.y ~= cp.y then
                    tag.position = cp
                end
            else
                -- Create new chart tag
                local label = storage.agent_labels[agent_id] or agent_id
                local new_tag = c.force.add_chart_tag(c.surface, {
                    position = c.position,
                    text = label,
                })
                if new_tag then
                    storage.agent_tags[agent_id] = new_tag
                end
            end
        else
            -- Character gone — remove tag
            local tag = storage.agent_tags[agent_id]
            if tag and tag.valid then tag.destroy() end
            storage.agent_tags[agent_id] = nil
        end
    end
end

-- Process queued RCON commands deterministically in on_tick.
-- This prevents desync in multiplayer: RCON pushes to queue,
-- on_tick processes it identically on server and all clients.
local function enable_spectator(player)
    storage.spectator_previous = storage.spectator_previous or {}
    if player.controller_type == defines.controllers.spectator then return true end
    storage.spectator_previous[player.index] = {
        controller_type = player.controller_type,
        character = player.character,
    }
    local ok = pcall(function()
        player.set_controller{type = defines.controllers.spectator}
    end)
    return ok
end

local function restore_spectator(player)
    storage.spectator_previous = storage.spectator_previous or {}
    local previous = storage.spectator_previous[player.index]
    if not previous then return false end
    local ok = pcall(function()
        if previous.controller_type == defines.controllers.character
            and previous.character
            and previous.character.valid
        then
            player.set_controller{
                type = defines.controllers.character,
                character = previous.character,
            }
        else
            player.set_controller{type = previous.controller_type}
        end
    end)
    if ok then storage.spectator_previous[player.index] = nil end
    return ok
end

local function process_rcon_queue()
    if not storage._rcon_queue or #storage._rcon_queue == 0 then return end
    local queue = storage._rcon_queue
    storage._rcon_queue = {}
    for _, item in ipairs(queue) do
        local pi = item.pi or 0
        if item.type == "response" then
            if pi > 0 then
                local player = game.get_player(pi)
                if player then
                    add_chat_message(player, item.agent, "claude", item.text)
                end
            else
                -- Autonomous turns are not tied to one player. Deliver their
                -- chat to every connected player instead of silently dropping it.
                for _, player in pairs(game.connected_players) do
                    add_chat_message(player, item.agent, "claude", item.text)
                end
            end
        elseif item.type == "tool" then
            -- Tool calls only shown in status bar, not in chat log
            if pi > 0 then
                local player = game.get_player(pi)
                if player then
                    set_status(player, "[color=0.6,0.7,1]Using " .. item.tool .. "...[/color]")
                end
            end
        elseif item.type == "status" then
            if pi > 0 then
                local player = game.get_player(pi)
                if player then
                    set_status(player, item.text)
                end
            else
                for _, player in pairs(game.connected_players) do
                    set_status(player, item.text)
                end
            end
        elseif item.type == "register" then
            register_agent(item.agent, item.label)
        elseif item.type == "unregister" then
            unregister_agent(item.agent)
        elseif item.type == "clear" then
            if pi < 1 then goto continue end
            local player = game.get_player(pi)
            if player then
                if item.agent then
                    if storage.messages[item.pi] then
                        storage.messages[item.pi][item.agent] = {}
                    end
                    local frame = player.gui.screen[GUI_FRAME]
                    if frame and frame.valid then
                        local chat_flow = get_agent_chat_flow(frame, item.agent)
                        if chat_flow then chat_flow.clear() end
                    end
                else
                    storage.messages[item.pi] = {}
                    ensure_agent_messages(item.pi)
                    local frame = player.gui.screen[GUI_FRAME]
                    if frame and frame.valid then
                        for _, a in ipairs(storage.agents) do
                            local chat_flow = get_agent_chat_flow(frame, a)
                            if chat_flow then chat_flow.clear() end
                        end
                    end
                end
            end
        elseif item.type == "spectator" then
            storage.spectator_mode = item.enabled == true
            if storage.spectator_mode then
                for _, player in pairs(game.players) do
                    if player.connected and player.controller_type ~= defines.controllers.spectator then
                        enable_spectator(player)
                    end
                end
            else
                for _, player in pairs(game.players) do
                    restore_spectator(player)
                end
            end
        end
        ::continue::
    end
end

-- ============================================================
-- Remote Interface (called by bridge via RCON)
-- All state-modifying operations push to _rcon_queue for
-- deterministic processing in on_tick (prevents MP desync).
-- ============================================================

local function pos_table(pos)
    if not pos then return nil end
    return {x = pos.x, y = pos.y}
end

local function scoped_character(agent_id)
    return find_factorioctl_character(agent_id or "default")
end

local function plan_steam_power_impl(agent_id, water_x1, water_y1, water_x2, water_y2, target_x, target_y)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return {
            success = false,
            error = "no character for agent " .. tostring(agent_id) .. "; spawn first",
            blockers = {"no_character"},
        }
    end
    return power.plan_steam_power(character, water_x1, water_y1, water_x2, water_y2, target_x, target_y)
end

local function repair_steam_power_impl(agent_id, x, y, radius, target_x, target_y)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return {
            success = false,
            error = "no character for agent " .. tostring(agent_id) .. "; spawn first",
            blockers = {"no_character"},
        }
    end
    return power.repair_steam_power(character, x, y, radius, target_x, target_y)
end

local function extend_power_to_impl(agent_id, x, y, radius, target_x, target_y)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return {
            success = false,
            error = "no character for agent " .. tostring(agent_id) .. "; spawn first",
            blockers = {"no_character"},
        }
    end
    return power.extend_power_to(character, x, y, radius, target_x, target_y)
end

local function broadcast_console_impl(message)
    game.print("[Agent] " .. tostring(message or ""))
    return {success = true}
end

local function broadcast_flying_text_impl(message)
    local displayed = 0
    local text = tostring(message or "")
    for _, player in pairs(game.connected_players) do
        if player.character and player.character.valid then
            player.create_local_flying_text{
                text = text,
                position = {
                    player.character.position.x,
                    player.character.position.y - 2,
                },
                color = {r = 0.8, g = 0.8, b = 1.0},
                speed = 0.3,
                time_to_live = 300,
            }
            displayed = displayed + 1
        end
    end
    return {success = true, displayed = displayed}
end

local function get_tick_impl()
    return {tick = game.tick}
end

local function set_tick_paused_impl(paused)
    game.tick_paused = paused and true or false
    return {success = true, tick_paused = game.tick_paused}
end

local function set_game_speed_impl(speed)
    game.speed = tonumber(speed) or game.speed
    return {success = true, speed = game.speed}
end

local function blueprint_scratch_stack(inv)
    local scratch_temp_inventory = nil
    local slot = inv.find_empty_stack("blueprint")
    if not slot then
        scratch_temp_inventory = game.create_inventory(1)
        slot = scratch_temp_inventory[1]
    end
    slot.set_stack{name = "blueprint"}
    local function cleanup_scratch()
        slot.clear()
        if scratch_temp_inventory then scratch_temp_inventory.destroy() end
    end
    return slot, cleanup_scratch
end

local function blueprint_relative_position(spec)
    local position = spec and spec.position or nil
    if not position then return nil end
    return {
        x = position.x or position[1] or 0,
        y = position.y or position[2] or 0,
    }
end

local function blueprint_max_offset(slot)
    local max_offset = 0
    local function include(specs)
        for _, spec in pairs(specs or {}) do
            local position = blueprint_relative_position(spec)
            if position then
                max_offset = math.max(max_offset, math.sqrt(position.x * position.x + position.y * position.y))
            end
        end
    end
    local entities_ok, blueprint_entities = pcall(function() return slot.get_blueprint_entities() end)
    if entities_ok then include(blueprint_entities) end
    local tiles_ok, blueprint_tiles = pcall(function() return slot.get_blueprint_tiles() end)
    if tiles_ok then include(blueprint_tiles) end
    return max_offset
end

local function require_blueprint_reach(character, slot, x, y)
    local reach_error = characters.require_position_reach(character, x, y, "build")
    if reach_error then return reach_error end

    local dx = x - character.position.x
    local dy = y - character.position.y
    local anchor_distance = math.sqrt(dx * dx + dy * dy)
    local max_offset = blueprint_max_offset(slot)
    local build_distance = character.build_distance or character.reach_distance or 0
    if anchor_distance + max_offset <= build_distance then return nil end

    return {
        success = false,
        error = "blueprint footprint extends outside character build reach",
        error_kind = "out_of_reach",
        action_needed = "walk_to",
        surface = character.surface.name,
        character_position = pos_table(character.position),
        target_position = {x = x, y = y},
        distance = anchor_distance,
        max_distance = build_distance,
        blueprint_max_offset = max_offset,
    }
end

local function register_blueprint_ghosts(ghosts)
    storage.factorioctl_entities = storage.factorioctl_entities or {}
    for _, ghost in pairs(ghosts) do
        if ghost.unit_number then storage.factorioctl_entities[ghost.unit_number] = ghost end
    end
end

local function create_native_blueprint_impl(agent_id, x1, y1, x2, y2)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return {error = "no character for agent " .. tostring(agent_id) .. "; spawn first"}
    end

    local inv = character.get_main_inventory()
    if not inv then return {error = "No inventory"} end

    local slot, cleanup_scratch = blueprint_scratch_stack(inv)
    local entities = slot.create_blueprint{
        surface = character.surface,
        force = character.force,
        area = {{x1, y1}, {x2, y2}},
        include_entities = true,
        include_tiles = false,
    }
    local count = #entities

    if count == 0 then
        cleanup_scratch()
        return {error = "No entities in area"}
    end

    local bp_string = slot.export_stack()
    cleanup_scratch()
    return {
        blueprint_string = bp_string,
        entity_count = count,
    }
end

local function save_blueprint_impl(agent_id, name, x1, y1, x2, y2)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return {success = false, error = "no character for agent " .. tostring(agent_id) .. "; spawn first"}
    end

    local inv = character.get_main_inventory()
    if not inv then return {success = false, error = "No inventory"} end

    local slot, cleanup_scratch = blueprint_scratch_stack(inv)
    local entities = slot.create_blueprint{
        surface = character.surface,
        force = character.force,
        area = {{x1, y1}, {x2, y2}},
        include_entities = true,
    }
    local count = #entities

    if count == 0 then
        cleanup_scratch()
        return {success = false, error = "No entities in area"}
    end

    storage.blueprints = storage.blueprints or {}
    storage.blueprints[name] = {
        string = slot.export_stack(),
        entity_count = count,
    }
    cleanup_scratch()
    return {success = true, entity_count = count}
end

local function list_blueprints_impl()
    storage.blueprints = storage.blueprints or {}
    local result = {}
    for name, data in pairs(storage.blueprints) do
        table.insert(result, {
            name = name,
            entity_count = data.entity_count,
        })
    end
    return result
end

local function get_blueprint_impl(name)
    storage.blueprints = storage.blueprints or {}
    local data = storage.blueprints[name]
    if data then
        return {
            blueprint_string = data.string,
            entity_count = data.entity_count,
        }
    end
    return {error = "Blueprint not found"}
end

local function place_blueprint_impl(agent_id, name, x, y, direction)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return {success = false, error = "no character for agent " .. tostring(agent_id) .. "; spawn first"}
    end
    storage.blueprints = storage.blueprints or {}
    local data = storage.blueprints[name]
    if not data then return {success = false, error = "Blueprint not found"} end

    local inv = character.get_main_inventory()
    if not inv then return {success = false, error = "No inventory"} end

    local slot, cleanup_scratch = blueprint_scratch_stack(inv)
    local ok = slot.import_stack(data.string)
    if not ok then
        cleanup_scratch()
        return {success = false, error = "Invalid stored blueprint string"}
    end

    local reach_error = require_blueprint_reach(character, slot, x, y)
    if reach_error then
        cleanup_scratch()
        return reach_error
    end

    local ghosts = slot.build_blueprint{
        surface = character.surface,
        force = character.force,
        position = {x = x, y = y},
        direction = direction,
        force_build = true,
    }

    if #ghosts == 0 then
        cleanup_scratch()
        return {success = false, error = "Blueprint created no ghosts"}
    end

    register_blueprint_ghosts(ghosts)
    cleanup_scratch()
    return {success = true, ghosts_created = #ghosts}
end

local function import_blueprint_impl(agent_id, bp_string, x, y, direction)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return {success = false, error = "no character for agent " .. tostring(agent_id) .. "; spawn first"}
    end
    local inv = character.get_main_inventory()
    if not inv then return {success = false, error = "No inventory"} end

    local slot, cleanup_scratch = blueprint_scratch_stack(inv)
    local ok = slot.import_stack(bp_string)
    if not ok then
        cleanup_scratch()
        return {success = false, error = "Invalid blueprint string"}
    end

    local reach_error = require_blueprint_reach(character, slot, x, y)
    if reach_error then
        cleanup_scratch()
        return reach_error
    end

    local ghosts = slot.build_blueprint{
        surface = character.surface,
        force = character.force,
        position = {x = x, y = y},
        direction = direction,
        force_build = true,
    }

    if #ghosts == 0 then
        cleanup_scratch()
        return {success = false, error = "Invalid or empty blueprint string"}
    end

    register_blueprint_ghosts(ghosts)
    cleanup_scratch()
    return {success = true, ghosts_created = #ghosts}
end

local function delete_blueprint_impl(name)
    storage.blueprints = storage.blueprints or {}
    if storage.blueprints[name] then
        storage.blueprints[name] = nil
        return {success = true}
    end
    return {success = false, error = "Blueprint not found"}
end

local function crafting_queue_summary(character)
    local queue = {}
    if not character then return queue end
    for _, item in pairs(character.crafting_queue or {}) do
        table.insert(queue, {recipe = item.recipe, count = item.count})
    end
    return queue
end

local function craft_failure(character, recipe_name, error)
    return {
        success = false,
        queued = 0,
        queue_size = character and character.crafting_queue_size or 0,
        queue = crafting_queue_summary(character),
        recipe = recipe_name,
        error = error,
    }
end

local function craft_impl(agent_id, recipe_name, count)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return craft_failure(nil, recipe_name, "no character for agent " .. tostring(agent_id) .. "; spawn first")
    end

    if not prototypes.recipe[recipe_name] then
        return craft_failure(character, recipe_name, "Unknown recipe")
    end

    local force_recipe = character.force.recipes[recipe_name]
    if force_recipe and not force_recipe.enabled then
        return craft_failure(character, recipe_name, "Recipe is disabled")
    end

    local ok, crafted_or_error = pcall(function()
        return character.begin_crafting{recipe = recipe_name, count = count}
    end)
    if not ok then
        return craft_failure(character, recipe_name, tostring(crafted_or_error))
    end

    local crafted = crafted_or_error
    local result = {
        success = crafted > 0,
        queued = crafted,
        queue_size = character.crafting_queue_size,
        queue = crafting_queue_summary(character),
        recipe = recipe_name,
    }
    if crafted <= 0 then
        result.error = "Crafting did not start; check ingredients, recipe category, or character craftability"
    end
    return result
end

local function wait_for_crafting_impl(agent_id)
    local character = find_factorioctl_character(agent_id)
    if character and character.valid then
        return tostring(character.crafting_queue_size or 0)
    end
    return "0"
end

local function inventory_item_total(inv)
    local total = 0
    if not inv then return total end
    for _, item in pairs(inv.get_contents()) do
        total = total + item.count
    end
    return total
end

local function find_minable_at(surface, character, x, y, radius)
    local resources = surface.find_entities_filtered{
        position = {x, y},
        radius = radius,
        type = "resource",
    }
    if #resources > 0 then return resources[1] end

    local entities = surface.find_entities_filtered{
        position = {x, y},
        radius = radius,
        type = {"tree", "simple-entity", "fish"},
    }
    for _, entity in pairs(entities) do
        if entity.minable and entity ~= character then
            return entity
        end
    end
    return nil
end

local function mining_failure(character, error)
    local inv = character and character.valid and character.get_main_inventory() or nil
    return {
        success = false,
        mined_count = 0,
        picked_up = 0,
        inventory = inventory_contents(inv),
        error = error,
    }
end

local function start_mining_impl(agent_id, x, y)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return {success = false, error = "no character for agent " .. tostring(agent_id) .. "; spawn first"}
    end

    local target = find_minable_at(character.surface, character, x, y, 1)
    if not target then
        return {success = false, error = "No minable entity at position"}
    end

    local reach_error = characters.require_entity_reach(character, target)
    if reach_error then return reach_error end

    character.mining_state = {mining = true, position = target.position}
    return {
        success = true,
        target = target.name,
        position = pos_table(target.position),
    }
end

local function stop_mining_impl(agent_id)
    local character = find_factorioctl_character(agent_id)
    if character and character.valid then
        character.mining_state = {mining = false}
        return "ok"
    end
    return "error"
end

local function get_mining_status_impl(agent_id)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return {mining = false}
    end
    return {
        mining = character.mining_state.mining,
        position = pos_table(character.position),
    }
end

local function pick_up_item_entity(character, inv, item_entity)
    local stack = item_entity.stack
    if not (stack and stack.valid_for_read and inv) then return 0 end
    local stack_count = stack.count
    local inserted = inv.insert(stack)
    if inserted <= 0 then return 0 end
    if inserted >= stack_count then
        item_entity.destroy()
    else
        stack.count = stack_count - inserted
    end
    return inserted
end

local function mine_at_impl(agent_id, x, y, count, radius)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return mining_failure(nil, "no character for agent " .. tostring(agent_id) .. "; spawn first")
    end

    local inv = character.get_main_inventory()
    local before_count = inventory_item_total(inv)
    local mined = 0
    local picked_up = 0
    local surface = character.surface
    -- mine_at is deliberately point-targeted. Never let a caller turn it into
    -- an area deconstruction primitive that can catch nearby infrastructure.
    local search_radius = math.min(math.max(radius or 0.5, 0), 0.5)

    for _ = 1, count do
        local iteration_before_count = inventory_item_total(inv)
        local items_on_ground = surface.find_entities_filtered{
            position = {x, y},
            radius = search_radius,
            type = "item-entity",
        }

        if #items_on_ground > 0 then
            local reach_error = characters.require_entity_reach(character, items_on_ground[1])
            if reach_error then return reach_error end
            picked_up = picked_up + pick_up_item_entity(character, inv, items_on_ground[1])
        else
            local target = find_minable_at(surface, character, x, y, search_radius)
            if not target then break end
            local reach_error = characters.require_entity_reach(character, target)
            if reach_error then return reach_error end
            local target_amount_before = nil
            if target.type == "resource" then
                target_amount_before = target.amount
            end
            character.mine_entity(target, true)
            local iteration_after_count = inventory_item_total(inv)
            local inventory_progress = iteration_after_count > iteration_before_count
            local resource_progress = target.valid and target_amount_before and target.amount < target_amount_before
            if inventory_progress or resource_progress then
                mined = mined + 1
            else
                break
            end
        end
    end

    local after_count = inventory_item_total(inv)
    local items_gained = after_count - before_count
    local items = inventory_contents(inv)
    local success = items_gained > 0 or picked_up > 0
    local result = {
        success = success,
        mined_count = items_gained,
        mined_entities = mined,
        picked_up = picked_up,
        inventory = items,
    }
    if not success then
        result.error = "No loose item or natural minable entity at exact position; use remove_entity with a unit number for placed infrastructure"
    end
    return result
end

local function find_nearest_minable_impl(agent_id, entity_name, radius)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return {found = false, error = "no character for agent " .. tostring(agent_id) .. "; spawn first"}
    end

    local entities = character.surface.find_entities_filtered{
        name = entity_name,
        position = character.position,
        radius = radius or 100,
    }

    local nearest = nil
    local nearest_dist = math.huge
    for _, entity in pairs(entities) do
        if entity.minable then
            local dx = entity.position.x - character.position.x
            local dy = entity.position.y - character.position.y
            local dist = dx * dx + dy * dy
            if dist < nearest_dist then
                nearest = entity
                nearest_dist = dist
            end
        end
    end

    if not nearest then
        return {found = false}
    end
    return {
        found = true,
        name = nearest.name,
        position = pos_table(nearest.position),
        distance = math.sqrt(nearest_dist),
    }
end

local function mine_nearest_impl(agent_id, entity_name, count)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return mining_failure(nil, "no character for agent " .. tostring(agent_id) .. "; spawn first")
    end

    local mined = 0
    for _ = 1, count do
        local nearest = find_nearest_minable_impl(agent_id, entity_name, 100)
        if not nearest.found then break end
        local target = find_minable_at(character.surface, character, nearest.position.x, nearest.position.y, 0.5)
        if not target then break end
        local reach_error = characters.require_entity_reach(character, target)
        if reach_error then return reach_error end
        if character.mine_entity(target, true) then
            mined = mined + 1
        else
            break
        end
    end

    local inv = character.get_main_inventory()
    local result = {
        success = mined > 0,
        mined_count = mined,
        inventory = inventory_contents(inv),
    }
    if mined == 0 then
        result.error = "No minable entity found"
    end
    return result
end

local function clear_area_impl(agent_id, x1, y1, x2, y2, clear_trees, clear_rocks, dry_run)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return {error = "no character for agent " .. tostring(agent_id) .. "; spawn first"}
    end

    local surface = character.surface
    local area = {{x1, y1}, {x2, y2}}
    local result = {
        trees_found = 0,
        rocks_found = 0,
        trees_mined = 0,
        rocks_mined = 0,
        dry_run = dry_run,
        too_far = false,
        items_gained = {},
    }

    local inv = character.get_main_inventory()
    local before = {}
    if inv then
        for _, item in pairs(inv.get_contents()) do
            before[item.name] = item.count
        end
    end

    local trees = clear_trees and surface.find_entities_filtered{type = "tree", area = area} or {}
    local rocks = {}
    if clear_rocks then
        for _, entity in pairs(surface.find_entities_filtered{type = "simple-entity", area = area}) do
            if entity.name:find("rock") then table.insert(rocks, entity) end
        end
    end
    result.trees_found = #trees
    result.rocks_found = #rocks

    if not dry_run then
        for _, entity in ipairs(trees) do
            local reach_error = characters.require_entity_reach(character, entity)
            if reach_error then return reach_error end
        end
        for _, entity in ipairs(rocks) do
            local reach_error = characters.require_entity_reach(character, entity)
            if reach_error then return reach_error end
        end
        for _, tree in ipairs(trees) do
            if character.mine_entity(tree, true) then
                result.trees_mined = result.trees_mined + 1
            end
        end
        for _, rock in ipairs(rocks) do
            if character.mine_entity(rock, true) then
                result.rocks_mined = result.rocks_mined + 1
            end
        end
    end

    if not dry_run and inv then
        for _, item in pairs(inv.get_contents()) do
            local gained = item.count - (before[item.name] or 0)
            if gained > 0 then
                table.insert(result.items_gained, {name = item.name, count = gained})
            end
        end
    end

    return result
end

local function build_entity_result(entity)
    return {
        unit_number = entity.unit_number,
        name = entity.name,
        type = entity.type,
        position = pos_table(entity.position),
        direction = entity.direction,
        health = entity.health,
        force = entity.force and entity.force.name or nil,
    }
end

local function direction_from_name(direction_name, default_direction)
    local normalized = string.lower(tostring(direction_name or ""))
    if normalized == "north" or normalized == "n" then return defines.direction.north end
    if normalized == "east" or normalized == "e" then return defines.direction.east end
    if normalized == "south" or normalized == "s" then return defines.direction.south end
    if normalized == "west" or normalized == "w" then return defines.direction.west end
    if normalized == "northeast" or normalized == "ne" then return defines.direction.northeast end
    if normalized == "southeast" or normalized == "se" then return defines.direction.southeast end
    if normalized == "southwest" or normalized == "sw" then return defines.direction.southwest end
    if normalized == "northwest" or normalized == "nw" then return defines.direction.northwest end
    return default_direction
end

local function build_result(placed, total, entities, errors)
    return {
        placed = placed,
        total = total,
        entities = entities,
        errors = errors,
    }
end

local function build_drill_array_impl(agent_id, count, resource, near_x, near_y, drill_type, direction_name)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return build_result(0, count, {}, {"no character for agent " .. tostring(agent_id) .. "; spawn first"})
    end

    local inv = character.get_main_inventory()
    local drill_count = inv and inv.get_item_count(drill_type) or 0
    if drill_count < count then
        return build_result(0, count, {}, {"Not enough drills in inventory (have " .. drill_count .. ")"})
    end

    if count <= 0 then return build_result(0, count, {}, {}) end

    local surface = character.surface
    local origin_x = near_x or 0
    local origin_y = near_y or 0
    local resources = surface.find_entities_filtered{
        name = resource,
        position = {origin_x, origin_y},
        radius = 100,
    }

    if #resources == 0 then
        return build_result(0, count, {}, {"No " .. resource .. " found nearby"})
    end

    table.sort(resources, function(a, b)
        local da = (a.position.x - origin_x) ^ 2 + (a.position.y - origin_y) ^ 2
        local db = (b.position.x - origin_x) ^ 2 + (b.position.y - origin_y) ^ 2
        return da < db
    end)

    local direction = direction_from_name(direction_name, defines.direction.south)
    local placed = 0
    local entities = {}
    local errors = {}
    local used_positions = {}

    for _, resource_entity in pairs(resources) do
        if placed >= count then break end

        local px = math.floor(resource_entity.position.x)
        local py = math.floor(resource_entity.position.y)
        local key = px .. "," .. py
        if not used_positions[key] then
            local can_place = surface.can_place_entity{
                name = drill_type,
                position = {px, py},
                direction = direction,
                force = character.force,
            }

            local reach_error = characters.require_position_reach(character, px, py, "build")
            if reach_error then
                table.insert(errors, "Out of reach at " .. px .. "," .. py)
            elseif can_place then
                local entity = surface.create_entity{
                    name = drill_type,
                    position = {px, py},
                    direction = direction,
                    force = character.force,
                }
                if entity then
                    storage.factorioctl_entities = storage.factorioctl_entities or {}
                    storage.factorioctl_entities[entity.unit_number] = entity
                    inv.remove{name = drill_type, count = 1}
                    placed = placed + 1
                    used_positions[key] = true
                    table.insert(entities, build_entity_result(entity))
                end
            end
        end
    end

    return build_result(placed, count, entities, errors)
end

local function smelter_line_delta(line_direction, spacing)
    local normalized = string.lower(tostring(line_direction or ""))
    if normalized == "west" or normalized == "w" then return -spacing, 0 end
    if normalized == "south" or normalized == "s" then return 0, spacing end
    if normalized == "north" or normalized == "n" then return 0, -spacing end
    return spacing, 0
end

local function build_smelter_line_impl(agent_id, count, start_x, start_y, furnace_type, line_direction, spacing)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return build_result(0, count, {}, {"no character for agent " .. tostring(agent_id) .. "; spawn first"})
    end

    local inv = character.get_main_inventory()
    local furnace_count = inv and inv.get_item_count(furnace_type) or 0
    if furnace_count < count then
        return build_result(0, count, {}, {"Not enough furnaces in inventory (have " .. furnace_count .. ")"})
    end

    local dx, dy = smelter_line_delta(line_direction, spacing)
    local surface = character.surface
    local placed = 0
    local entities = {}
    local errors = {}

    for i = 0, count - 1 do
        local px = start_x + i * dx
        local py = start_y + i * dy
        local can_place = surface.can_place_entity{
            name = furnace_type,
            position = {px, py},
            force = character.force,
        }

        local reach_error = characters.require_position_reach(character, px, py, "build")
        if reach_error then
            table.insert(errors, "Out of reach at " .. px .. "," .. py)
        elseif can_place then
            local entity = surface.create_entity{
                name = furnace_type,
                position = {px, py},
                force = character.force,
            }
            if entity then
                storage.factorioctl_entities = storage.factorioctl_entities or {}
                storage.factorioctl_entities[entity.unit_number] = entity
                inv.remove{name = furnace_type, count = 1}
                placed = placed + 1
                table.insert(entities, build_entity_result(entity))
            end
        else
            table.insert(errors, "Cannot place at " .. px .. "," .. py)
        end
    end

    return build_result(placed, count, entities, errors)
end

local function mine_entity_for_agent(agent_id, entity)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return {success = false, error = "no character for agent " .. tostring(agent_id) .. "; spawn first"}
    end

    if not (entity and entity.valid) then
        return {success = false, error = "Entity not found"}
    end

    local reach_error = characters.require_entity_reach(character, entity)
    if reach_error then return reach_error end

    local inv = character.get_main_inventory()
    local before_count = inventory_item_total(inv)
    local name = entity.name
    local unit_number = entity.unit_number

    if not character.mine_entity(entity, true) then
        return {
            success = false,
            error = "Could not mine entity",
            name = name,
            unit_number = unit_number,
        }
    end

    if unit_number then storage.factorioctl_entities[unit_number] = nil end
    local after_count = inventory_item_total(inv)
    return {
        success = true,
        removed = true,
        name = name,
        unit_number = unit_number,
        items_gained = after_count - before_count,
        inventory = inventory_contents(inv),
    }
end

local function remove_entity_at_impl(agent_id, x, y)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return {success = false, error = "no character for agent " .. tostring(agent_id) .. "; spawn first"}
    end
    local found = character.surface.find_entities_filtered{
        position = {x, y},
        radius = 0.5,
    }

    local candidates = {}
    for _, entity in pairs(found) do
        if entity.type ~= "character" and entity.type ~= "resource" then
            table.insert(candidates, entity)
        end
    end

    table.sort(candidates, function(a, b)
        return (a.unit_number or math.huge) < (b.unit_number or math.huge)
    end)
    local summaries = {}
    for _, entity in ipairs(candidates) do
        local summary = entities.summary(entity, false)
        summary.exact_remove = {
            available = entity.unit_number ~= nil,
            tool = entity.unit_number and "remove_entity" or nil,
            args = entity.unit_number and {unit_number = entity.unit_number} or nil,
            guidance = entity.unit_number
                and ("Call remove_entity with unit_number=" .. tostring(entity.unit_number))
                or "This entity has no unit_number and cannot be removed through the exact-unit API",
        }
        table.insert(summaries, summary)
    end

    return {
        success = false,
        removed = false,
        error = #candidates == 0
            and "No removable entity found at this position; coordinate removal is non-mutating"
            or "Coordinate removal is non-mutating; choose a candidate and call remove_entity with its exact unit_number",
        error_kind = "exact_identity_required",
        action_needed = "remove_entity_by_unit_number",
        position = {x = x, y = y},
        candidate_count = #summaries,
        candidates = summaries,
        guidance = #candidates == 0
            and "Inspect the exact target again and use remove_entity only after obtaining its unit_number"
            or "Use candidates[].exact_remove.args with remove_entity; never retry this coordinate-only API as a mutation",
    }
end

local function remove_entity_impl(agent_id, unit_number)
    storage.factorioctl_entities = storage.factorioctl_entities or {}
    local entity = entities.find_by_unit_number(unit_number)
    if not entity then
        return {error = "Entity not found"}
    end

    return mine_entity_for_agent(agent_id, entity)
end

local function insert_items_impl(agent_id, unit_number, item, count, inventory_type)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return {error = "no character for agent " .. tostring(agent_id) .. "; spawn first"}
    end

    local character_inv = character.get_main_inventory()
    if not character_inv then
        return {error = "Character has no inventory"}
    end

    if type(count) ~= "number" or count <= 0 then
        return {error = "Count must be a positive number"}
    end

    local entity = entities.find_by_unit_number(unit_number)
    if not entity then
        return {error = "Entity not found"}
    end

    local reach_error = characters.require_entity_reach(character, entity)
    if reach_error then return reach_error end

    local inv = entity.get_inventory(inventory_define_for(inventory_type, "fuel"))
    if not inv then
        return {error = "Entity has no such inventory"}
    end

    local available = character_inv.get_item_count(item)
    local removed = character_inv.remove{name = item, count = math.min(count, available)}
    if removed == 0 then
        return {
            error = "Character has no items to insert",
            item = item,
            requested = count,
            available = available,
        }
    end

    local inserted = inv.insert{name = item, count = removed}
    local remainder = removed - inserted
    local returned = 0
    if remainder > 0 then
        returned = character_inv.insert{name = item, count = remainder}
    end

    if returned ~= remainder then
        return {
            error = "Item conservation failure while returning unaccepted items",
            item = item,
            requested = count,
            available = available,
            removed = removed,
            inserted = inserted,
            returned = returned,
        }
    end

    if inserted == 0 then
        return {
            error = "Inserted 0 items (inventory full or item not accepted)",
            item = item,
            requested = count,
            available = available,
            removed = removed,
            inserted = inserted,
            returned = returned,
        }
    end

    return {
        item = item,
        requested = count,
        available = available,
        removed = removed,
        inserted = inserted,
        returned = returned,
    }
end

local function extract_items_impl(agent_id, unit_number, item, count, inventory_type)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return {error = "no character for agent " .. tostring(agent_id) .. "; spawn first"}
    end

    local entity = entities.find_by_unit_number(unit_number)
    if not entity then
        return {error = "Entity not found"}
    end

    local reach_error = characters.require_entity_reach(character, entity)
    if reach_error then return reach_error end

    local inv = entity.get_inventory(inventory_define_for(inventory_type, "chest"))
    if not inv then
        return {error = "Entity has no such inventory"}
    end

    local player_inv = character.get_main_inventory()
    if not player_inv then
        return {error = "Character has no inventory"}
    end

    local available = inv.get_item_count(item)
    local to_extract = math.min(count, available)
    if to_extract == 0 then
        return {extracted = 0, available = available, item = item}
    end

    local removed = inv.remove{name = item, count = to_extract}
    local inserted = player_inv.insert{name = item, count = removed}
    if inserted < removed then
        inv.insert{name = item, count = removed - inserted}
    end

    return {extracted = inserted, available = available}
end

local function set_recipe_impl(agent_id, unit_number, recipe)
    local character = find_factorioctl_character(agent_id)
    if not (character and character.valid) then
        return {success = false, error = "no character for agent " .. tostring(agent_id) .. "; spawn first"}
    end
    local entity = entities.find_by_unit_number(unit_number)
    if not entity then
        return {error = "Entity not found"}
    end


    local reach_error = characters.require_entity_reach(character, entity)
    if reach_error then return reach_error end

    if not entity.set_recipe then
        return {error = "Entity cannot have recipes"}
    end

    -- JSON null arrives as Lua nil. Keep empty-string compatibility at the
    -- public tool boundary, but never pass an empty recipe name to Factorio:
    -- clearing a crafting machine is explicitly set_recipe(nil).
    local requested_recipe = recipe
    if requested_recipe == "" then requested_recipe = nil end
    local ok, set_error = pcall(function()
        entity.set_recipe(requested_recipe)
    end)
    if not ok then
        return {success = false, error = tostring(set_error)}
    end

    local current = entity.get_recipe and entity.get_recipe() or nil
    if requested_recipe == nil then
        if current ~= nil then
            return {
                success = false,
                error = "Could not clear recipe",
                current_recipe = current.name,
            }
        end
        return {success = true, cleared = true, recipe = nil}
    end
    if not current or current.name ~= requested_recipe then
        return {
            success = false,
            error = "Could not set recipe (unknown or incompatible recipe)",
            requested_recipe = requested_recipe,
            current_recipe = current and current.name or nil,
        }
    end

    return {success = true, cleared = false, recipe = current.name}
end

local function get_entity_recipe_impl(unit_number)
    local entity = entities.find_by_unit_number(unit_number)
    if not entity then
        return {success = false, error = "Entity not found"}
    end
    local ok, recipe = pcall(function()
        if not entity.get_recipe then return nil end
        return entity.get_recipe()
    end)
    if not ok then
        return {success = false, error = tostring(recipe)}
    end
    return {
        success = true,
        recipe = recipe and recipe.name or nil,
    }
end

local function get_entity_inventory_impl(unit_number)
    local entity = entities.find_by_unit_number(unit_number)
    if not entity then
        return {error = "Entity not found"}
    end

    local result = {
        unit_number = entity.unit_number,
        name = entity.name,
        inventories = {},
    }

    local inventory_types = {
        {name = "fuel", define = defines.inventory.fuel},
        {name = "chest", define = defines.inventory.chest},
        {name = "furnace_source", define = defines.inventory.furnace_source},
        {name = "furnace_result", define = defines.inventory.furnace_result},
        {name = "assembling_machine_input", define = defines.inventory.assembling_machine_input},
        {name = "assembling_machine_output", define = defines.inventory.assembling_machine_output},
        {name = "burnt_result", define = defines.inventory.burnt_result},
    }

    for _, inventory_type in pairs(inventory_types) do
        local ok, inv = pcall(function()
            return entity.get_inventory(inventory_type.define)
        end)
        if ok and inv then
            local items = inventory_contents(inv)
            if #items > 0 then
                result.inventories[inventory_type.name] = items
            end
        end
    end

    return result
end

local function try_get(fn)
    local ok, value = pcall(fn)
    if ok then return value end
    return nil
end

local function table_keys(map)
    local result = {}
    if not map then return result end
    for key, _ in pairs(map) do
        table.insert(result, key)
    end
    return result
end

local function get_prototype_impl(name)
    local proto = prototypes.entity[name]
    if not proto then
        return {error = "Prototype not found"}
    end

    local result = {
        name = proto.name,
        type = proto.type,
    }

    local collision_box = try_get(function() return proto.collision_box end)
    if collision_box then
        result.size = {
            collision_box.right_bottom.x - collision_box.left_top.x,
            collision_box.right_bottom.y - collision_box.left_top.y,
        }
    end

    local crafting_speed = try_get(function() return proto.get_crafting_speed() end)
    if crafting_speed then result.crafting_speed = crafting_speed end

    local crafting_categories = try_get(function() return proto.crafting_categories end)
    if crafting_categories then result.crafting_categories = table_keys(crafting_categories) end

    local mining_speed = try_get(function() return proto.mining_speed end)
    if mining_speed then result.mining_speed = mining_speed end

    local resource_categories = try_get(function() return proto.resource_categories end)
    if resource_categories then result.resource_categories = table_keys(resource_categories) end

    local rotation_speed = try_get(function() return proto.inserter_rotation_speed end)
    if rotation_speed then result.rotation_speed = rotation_speed end

    local extension_speed = try_get(function() return proto.inserter_extension_speed end)
    if extension_speed then result.extension_speed = extension_speed end

    local belt_speed = try_get(function() return proto.belt_speed end)
    if belt_speed then result.belt_speed = belt_speed end

    local energy_usage = try_get(function() return proto.energy_usage end)
    if energy_usage then result.energy_usage = energy_usage end

    if try_get(function() return proto.burner_prototype end) then
        result.energy_source = "burner"
    elseif try_get(function() return proto.electric_energy_source_prototype end) then
        result.energy_source = "electric"
    elseif try_get(function() return proto.heat_energy_source_prototype end) then
        result.energy_source = "heat"
    elseif try_get(function() return proto.void_energy_source_prototype end) then
        result.energy_source = "void"
    end

    return result
end

local api = {
    receive_response = function(player_index, agent_name, text)
        table.insert(storage._rcon_queue, {
            type = "response", pi = player_index,
            agent = agent_name or "default", text = text,
        })
    end,

    tool_status = function(player_index, agent_name, tool_name)
        table.insert(storage._rcon_queue, {
            type = "tool", pi = player_index,
            agent = agent_name or "default", tool = tool_name,
        })
    end,

    set_status = function(player_index, status_text)
        table.insert(storage._rcon_queue, {
            type = "status", pi = player_index, text = status_text,
        })
    end,

    clear_chat = function(player_index, agent_name)
        table.insert(storage._rcon_queue, {
            type = "clear", pi = player_index, agent = agent_name,
        })
    end,

    register_agent = function(agent_name, label)
        table.insert(storage._rcon_queue, {
            type = "register", agent = agent_name, label = label,
        })
    end,

    unregister_agent = function(agent_name)
        table.insert(storage._rcon_queue, {
            type = "unregister", agent = agent_name,
        })
    end,

    ensure_surface = function(planet_name)
        return characters.ensure_surface(planet_name)
    end,

    ensure_surface_result = function(planet_name)
        return characters.ensure_surface_result(planet_name)
    end,

    pre_place_character = function(agent_id, planet_name, spawn_x)
        return characters.pre_place(agent_id, planet_name, spawn_x)
    end,

    pre_place_character_result = function(agent_id, planet_name, spawn_x)
        return characters.pre_place_result(agent_id, planet_name, spawn_x)
    end,

    live_state_line = function(agent_id)
        return characters.live_state_line(agent_id)
    end,

    live_state_result = function(agent_id)
        return characters.live_state_result(agent_id)
    end,

    connected_player_count = function()
        return characters.connected_player_count()
    end,

    connected_player_count_result = function()
        return characters.connected_player_count_result()
    end,

    broadcast_console = function(message)
        return json_remote_call("broadcast_console", broadcast_console_impl, message)
    end,

    broadcast_flying_text = function(message)
        return json_remote_call("broadcast_flying_text", broadcast_flying_text_impl, message)
    end,

    get_tick = function()
        return json_remote_call("get_tick", get_tick_impl)
    end,

    set_tick_paused = function(paused)
        return json_remote_call("set_tick_paused", set_tick_paused_impl, paused)
    end,

    set_game_speed = function(speed)
        return json_remote_call("set_game_speed", set_game_speed_impl, speed)
    end,

    -- Register an agent character entity for on_tick walk processing
    register_character = function(agent_id, entity)
        return characters.register(agent_id, entity)
    end,

    -- Set walking direction for an agent (processed in on_tick)
    set_walk = function(agent_id, direction)
        if not storage.walk_state then storage.walk_state = {} end
        storage.walk_state[agent_id] = {walking = true, direction = direction}
    end,

    -- Stop walking for an agent (processed in on_tick)
    stop_walk = function(agent_id)
        if not storage.walk_state then storage.walk_state = {} end
        storage.walk_state[agent_id] = {walking = false}
    end,

    -- Set a target for ordinary character walking (processed in on_tick)
    set_walk_target = function(agent_id, x, y)
        return json_remote_call("set_walk_target", characters.set_walk_target, agent_id, x, y)
    end,

    -- Clear target position AND any leftover walk state for an agent. Must reset
    -- walking_state too, or a stale {walking=true} keeps the orphan character
    -- engine-walking with no target (audit F2 trapdoor).
    clear_walk_target = function(agent_id)
        return json_remote_call("clear_walk_target", characters.clear_walk_target, agent_id)
    end,

    -- Report whether an agent has an active deterministic walk target
    has_walk_target = function(agent_id)
        return storage.walk_targets ~= nil and storage.walk_targets[agent_id] ~= nil
    end,

    chat_capture_status = function()
        return helpers.table_to_json({success = true, registered = true})
    end,

    -- Return and clear captured chat messages as a JSON string (bridge polls this)
    get_chat_messages = function()
        local msgs = storage.chat_messages or {}
        storage.chat_messages = {}
        return helpers.table_to_json(msgs)
    end,

    -- Get character entity (safe from any context, uses synced mod storage)
    get_character = function(agent_id)
        return find_factorioctl_character(agent_id)
    end,

    -- List all agent characters as JSON string
    list_characters = function()
        if not storage.characters then return "[]" end
        local result = {}
        for agent_id, _ in pairs(storage.characters) do
            local c = find_factorioctl_character(agent_id)
            if c and c.valid then
                table.insert(result, {
                    agent_id = agent_id,
                    unit_number = c.unit_number,
                    position = { x = c.position.x, y = c.position.y },
                    health = c.health
                })
            end
        end
        return helpers.table_to_json(result)
    end,

    -- Diagnose steam-power fluid and electric connectivity near a position.
    diagnose_steam_power = function(x, y, radius, agent_id)
        return json_remote_call("diagnose_steam_power", power.diagnose_steam_power, scoped_character(agent_id), x, y, radius)
    end,

    -- Plan a checked starter steam-power layout before mutating the world.
    plan_steam_power = function(agent_id, water_x1, water_y1, water_x2, water_y2, target_x, target_y)
        return json_remote_call("plan_steam_power", plan_steam_power_impl, agent_id, water_x1, water_y1, water_x2, water_y2, target_x, target_y)
    end,

    -- Plan dry-run repairs for an existing steam-power plant.
    repair_steam_power = function(agent_id, x, y, radius, target_x, target_y)
        return json_remote_call("repair_steam_power", repair_steam_power_impl, agent_id, x, y, radius, target_x, target_y)
    end,

    -- Plan dry-run pole placement to extend an existing power grid to a target.
    extend_power_to = function(agent_id, x, y, radius, target_x, target_y)
        return json_remote_call("extend_power_to", extend_power_to_impl, agent_id, x, y, radius, target_x, target_y)
    end,

    -- Power diagnostics live in the mod so Rust only emits small remote calls.
    get_power_status = function(x, y, radius, agent_id)
        return json_remote_call("get_power_status", power.get_power_status, scoped_character(agent_id), x, y, radius)
    end,

    get_power_networks = function(x, y, radius, agent_id)
        return json_remote_call("get_power_networks", power.get_power_networks, scoped_character(agent_id), x, y, radius)
    end,

    find_power_issues = function(x, y, radius, agent_id)
        return json_remote_call("find_power_issues", power.find_power_issues, scoped_character(agent_id), x, y, radius)
    end,

    get_power_coverage = function(x, y, radius, agent_id)
        return json_remote_call("get_power_coverage", power.get_power_coverage, scoped_character(agent_id), x, y, radius)
    end,

    get_alerts = function(x, y, radius, agent_id)
        return json_remote_call("get_alerts", power.get_alerts, scoped_character(agent_id), x, y, radius)
    end,

    get_belt_contents = function(x1, y1, x2, y2, agent_id)
        local character = scoped_character(agent_id)
        return json_remote_call("get_belt_contents", transport.get_belt_contents, character and character.surface or nil, x1, y1, x2, y2)
    end,

    get_belt_lane_contents = function(x1, y1, x2, y2, agent_id)
        local character = scoped_character(agent_id)
        return json_remote_call("get_belt_lane_contents", transport.get_belt_lane_contents, character and character.surface or nil, x1, y1, x2, y2)
    end,

    get_surfaces = function()
        return json_remote_call("get_surfaces", entities.get_surfaces)
    end,

    find_entities = function(x1, y1, x2, y2, entity_type, name, agent_id)
        local character = scoped_character(agent_id)
        return json_remote_call("find_entities", entities.find_entities, character and character.surface or nil, x1, y1, x2, y2, entity_type, name)
    end,

    verify_production = function(x1, y1, x2, y2, agent_id)
        local character = scoped_character(agent_id)
        return json_remote_call("verify_production", entities.verify_production, character and character.surface or nil, character and character.force or nil, x1, y1, x2, y2)
    end,

    diagnose_factory_blockers = function(x1, y1, x2, y2, limit, agent_id)
        local character = scoped_character(agent_id)
        return json_remote_call("diagnose_factory_blockers", entities.diagnose_factory_blockers, character and character.surface or nil, character and character.force or nil, x1, y1, x2, y2, limit)
    end,

    diagnose_fuel_sustainability = function(x1, y1, x2, y2, limit, agent_id)
        local character = scoped_character(agent_id)
        return json_remote_call("diagnose_fuel_sustainability", entities.diagnose_fuel_sustainability, character and character.surface or nil, character and character.force or nil, x1, y1, x2, y2, limit)
    end,

    get_entity = function(unit_number)
        return json_remote_call("get_entity", entities.get_entity, unit_number)
    end,

    get_entity_drop_position = function(unit_number)
        return json_remote_call("get_entity_drop_position", entities.get_drop_position, unit_number)
    end,

    find_resources = function(x1, y1, x2, y2, resource_type, agent_id)
        local character = scoped_character(agent_id)
        return json_remote_call("find_resources", world.find_resources, character and character.surface or nil, x1, y1, x2, y2, resource_type)
    end,

    find_nearest_resource = function(resource_name, from_x, from_y, agent_id)
        local character = scoped_character(agent_id)
        return json_remote_call("find_nearest_resource", world.find_nearest_resource, character and character.surface or nil, resource_name, from_x, from_y)
    end,

    get_tiles = function(x1, y1, x2, y2, agent_id)
        local character = scoped_character(agent_id)
        return json_remote_call("get_tiles", world.get_tiles, character and character.surface or nil, x1, y1, x2, y2)
    end,

    get_tile = function(x, y, agent_id)
        local character = scoped_character(agent_id)
        return json_remote_call("get_tile", world.get_tile, character and character.surface or nil, x, y)
    end,

    init_character = function(agent_id, x, y)
        return json_remote_call("init_character", characters.init, agent_id, x, y)
    end,

    teleport_character = function(agent_id, x, y)
        return json_remote_call("teleport_character", characters.teleport, agent_id, x, y)
    end,

    character_status = function(agent_id)
        return json_remote_call("character_status", characters.status, agent_id)
    end,

    character_inventory = function(agent_id)
        return json_remote_call("character_inventory", characters.inventory, agent_id)
    end,

    can_stand_at = function(agent_id, x, y, radius)
        return json_remote_call("can_stand_at", characters.can_stand_at, agent_id, x, y, radius)
    end,

    is_player_blocked = function(agent_id, radius)
        return json_remote_call("is_player_blocked", characters.is_player_blocked, agent_id, radius)
    end,

    unstuck = function(agent_id, radius, dry_run)
        return json_remote_call("unstuck", characters.unstuck, agent_id, radius, dry_run)
    end,

    craft = function(agent_id, recipe_name, count)
        return json_remote_call("craft", craft_impl, agent_id, recipe_name, count)
    end,

    wait_for_crafting = function(agent_id)
        return json_remote_call("wait_for_crafting", wait_for_crafting_impl, agent_id)
    end,

    create_native_blueprint = function(agent_id, x1, y1, x2, y2)
        return json_remote_call("create_native_blueprint", create_native_blueprint_impl, agent_id, x1, y1, x2, y2)
    end,

    save_blueprint = function(agent_id, name, x1, y1, x2, y2)
        return json_remote_call("save_blueprint", save_blueprint_impl, agent_id, name, x1, y1, x2, y2)
    end,

    list_blueprints = function()
        return json_remote_call("list_blueprints", list_blueprints_impl)
    end,

    get_blueprint = function(name)
        return json_remote_call("get_blueprint", get_blueprint_impl, name)
    end,

    place_blueprint = function(agent_id, name, x, y, direction)
        return json_remote_call("place_blueprint", place_blueprint_impl, agent_id, name, x, y, direction)
    end,

    import_blueprint = function(agent_id, bp_string, x, y, direction)
        return json_remote_call("import_blueprint", import_blueprint_impl, agent_id, bp_string, x, y, direction)
    end,

    delete_blueprint = function(name)
        return json_remote_call("delete_blueprint", delete_blueprint_impl, name)
    end,

    start_mining = function(agent_id, x, y)
        return json_remote_call("start_mining", start_mining_impl, agent_id, x, y)
    end,

    stop_mining = function(agent_id)
        return json_remote_call("stop_mining", stop_mining_impl, agent_id)
    end,

    get_mining_status = function(agent_id)
        return json_remote_call("get_mining_status", get_mining_status_impl, agent_id)
    end,

    mine_at = function(agent_id, x, y, count, radius)
        return json_remote_call("mine_at", mine_at_impl, agent_id, x, y, count, radius)
    end,

    find_nearest_minable = function(agent_id, entity_name, radius)
        return json_remote_call("find_nearest_minable", find_nearest_minable_impl, agent_id, entity_name, radius)
    end,

    mine_nearest = function(agent_id, entity_name, count)
        return json_remote_call("mine_nearest", mine_nearest_impl, agent_id, entity_name, count)
    end,

    clear_area = function(agent_id, x1, y1, x2, y2, clear_trees, clear_rocks, dry_run)
        return json_remote_call("clear_area", clear_area_impl, agent_id, x1, y1, x2, y2, clear_trees, clear_rocks, dry_run)
    end,

    place_entity = function(agent_id, entity_name, x, y, direction)
        return json_remote_call("place_entity", placement.place_entity, agent_id, entity_name, x, y, direction)
    end,

    place_underground_belt = function(agent_id, entity_name, x, y, direction, belt_type)
        return json_remote_call("place_underground_belt", placement.place_underground_belt, agent_id, entity_name, x, y, direction, belt_type)
    end,

    check_entity_placement = function(agent_id, entity_name, x, y, direction)
        return json_remote_call("check_entity_placement", placement.check_entity_placement, agent_id, entity_name, x, y, direction)
    end,

    find_entity_placements = function(agent_id, entity_name, center_x, center_y, radius, limit)
        return json_remote_call("find_entity_placements", placement.find_entity_placements, agent_id, entity_name, center_x, center_y, radius, limit)
    end,

    plan_entity_placement_near = function(agent_id, entity_name, target_x, target_y, radius, limit)
        return json_remote_call("plan_entity_placement_near", placement.plan_entity_placement_near, agent_id, entity_name, target_x, target_y, radius, limit)
    end,

    build_edge_miner = function(agent_id, resource_name, center_x, center_y, radius, drill_name, limit)
        return json_remote_call("build_edge_miner", placement.build_edge_miner, agent_id, resource_name, center_x, center_y, radius, drill_name, limit)
    end,

    build_direct_smelter = function(agent_id, drill_unit_number, output_x, output_y, output_direction, furnace_name, inserter_name, belt_name, radius)
        return json_remote_call("build_direct_smelter", placement.build_direct_smelter, agent_id, drill_unit_number, output_x, output_y, output_direction, furnace_name, inserter_name, belt_name, radius)
    end,

    place_ghost = function(agent_id, entity_name, x, y, direction)
        return json_remote_call("place_ghost", placement.place_ghost, agent_id, entity_name, x, y, direction)
    end,

    build_drill_array = function(agent_id, count, resource, near_x, near_y, drill_type, direction_name)
        return json_remote_call("build_drill_array", build_drill_array_impl, agent_id, count, resource, near_x, near_y, drill_type, direction_name)
    end,

    build_smelter_line = function(agent_id, count, start_x, start_y, furnace_type, line_direction, spacing)
        return json_remote_call("build_smelter_line", build_smelter_line_impl, agent_id, count, start_x, start_y, furnace_type, line_direction, spacing)
    end,

    remove_entity_at = function(agent_id, x, y)
        return json_remote_call("remove_entity_at", remove_entity_at_impl, agent_id, x, y)
    end,

    remove_entity = function(agent_id, unit_number)
        return json_remote_call("remove_entity", remove_entity_impl, agent_id, unit_number)
    end,

    rotate_entity = function(agent_id, unit_number, direction)
        return json_remote_call("rotate_entity", placement.rotate_entity, agent_id, unit_number, direction)
    end,

    insert_items = function(agent_id, unit_number, item, count, inventory_type)
        return json_remote_call("insert_items", insert_items_impl, agent_id, unit_number, item, count, inventory_type)
    end,

    extract_items = function(agent_id, unit_number, item, count, inventory_type)
        return json_remote_call("extract_items", extract_items_impl, agent_id, unit_number, item, count, inventory_type)
    end,

    set_recipe = function(agent_id, unit_number, recipe)
        return json_remote_call("set_recipe", set_recipe_impl, agent_id, unit_number, recipe)
    end,

    get_entity_recipe = function(unit_number)
        return json_remote_call("get_entity_recipe", get_entity_recipe_impl, unit_number)
    end,

    get_entity_inventory = function(unit_number)
        return json_remote_call("get_entity_inventory", get_entity_inventory_impl, unit_number)
    end,

    get_recipe = function(name)
        return json_remote_call("get_recipe", recipes.get_recipe, name)
    end,

    get_recipes_by_category = function(category)
        return json_remote_call("get_recipes_by_category", recipes.get_recipes_by_category, category)
    end,

    get_recipes_for_item = function(item)
        return json_remote_call("get_recipes_for_item", recipes.get_recipes_for_item, item)
    end,

    get_prototype = function(name)
        return json_remote_call("get_prototype", get_prototype_impl, name)
    end,

    get_research_status = function(agent_id)
        return json_remote_call("get_research_status", research.get_research_status, scoped_character(agent_id))
    end,

    get_available_research = function(agent_id)
        local character = find_factorioctl_character(agent_id)
        return json_remote_call("get_available_research", research.get_available_research, character)
    end,

    feed_lab_from_inventory = function(agent_id, lab_unit_number, science_pack, count, dry_run)
        local character = find_factorioctl_character(agent_id)
        return json_remote_call("feed_lab_from_inventory", research.feed_lab_from_inventory, character, lab_unit_number, science_pack, count, dry_run)
    end,

    start_research = function(tech_name, agent_id)
        return json_remote_call("start_research", research.start_research, scoped_character(agent_id), tech_name)
    end,

    is_tech_researched = function(tech_name, agent_id)
        return json_remote_call("is_tech_researched", research.is_tech_researched, scoped_character(agent_id), tech_name)
    end,

    production_statistics = function(surface_name, agent_id)
        local character = scoped_character(agent_id)
        local scoped_surface = surface_name or (character and character.surface.name or nil)
        return json_remote_call("production_statistics", diagnostics.production_statistics, scoped_surface, character and character.force or nil)
    end,

    autonomy_snapshot = function(agent_id)
        local character = find_factorioctl_character(agent_id)
        return json_remote_call("autonomy_snapshot", autonomy.snapshot, character)
    end,

    -- Get character position (read-only, safe from any context)
    get_character_pos = function(agent_id)
        local c = find_factorioctl_character(agent_id)
        if c and c.valid then
            return c.position.x .. "," .. c.position.y
        end
        return nil
    end,

    -- Queue spectator mode change (processed in on_tick for MP determinism)
    set_spectator_mode = function(enabled)
        table.insert(storage._rcon_queue, {
            type = "spectator", enabled = enabled,
        })
    end,

    -- Inject a message into the bridge input as if from a player.
    -- Used by supervisor sessions to send tasks to agents.
    inject_message = function(from_name, target_agent, message)
        helpers.write_file(INPUT_FILE, helpers.table_to_json({
            player_index = 0,
            player_name = from_name or "Supervisor",
            target_agent = target_agent or "all",
            message = message,
        }) .. "\n", true, 0)
    end,

    ping = function()
        rcon.print("pong")
    end,
}

remote.add_interface("claude_interface", api)

commands.add_command("claude", "claude-interface dispatch", function(cmd)
    if cmd.player_index ~= nil then
        local player = game.get_player(cmd.player_index)
        if player then
            player.print("The /claude bridge command is restricted to the server RCON console.")
        end
        return
    end
    local ok, request = pcall(helpers.json_to_table, cmd.parameter or "")
    if not ok or type(request) ~= "table" or type(request.fn) ~= "string" then
        rcon.print(json_response.error("bad_request", "expected {fn, args, n}"))
        return
    end
    local handler = api[request.fn]
    if not handler then
        rcon.print(json_response.error("unknown_function", request.fn, {
            action_needed = "sync_or_restart_mod",
        }))
        return
    end
    local args = request.args or {}
    if type(args) ~= "table" then
        rcon.print(json_response.error("bad_request", "args must be an array"))
        return
    end
    local n = request.n or #args
    local results = { pcall(handler, table.unpack(args, 1, n)) }
    if not results[1] then
        rcon.print(json_response.error("lua_error", tostring(results[2])))
        return
    end
    if results[2] ~= nil then rcon.print(results[2]) end
end)

-- ============================================================
-- Event Handlers
-- ============================================================

script.on_init(init_storage)

-- Process RCON queue and walk states every tick
script.on_event(defines.events.on_tick, function(event)
    process_rcon_queue()
    process_walk_states()
    process_walk_targets()
    -- Update map markers every 60 ticks (~1 second)
    if event.tick % 60 == 0 then
        update_agent_markers()
    end
end)

script.on_configuration_changed(function(data)
    -- Migrate old flat messages to per-agent structure
    if storage.messages then
        for player_index, msgs in pairs(storage.messages) do
            -- Detect old format: flat array of {role, text, tick}
            if msgs[1] and msgs[1].role then
                storage.messages[player_index] = {default = msgs}
            end
        end
    end

    init_storage()

    -- Rebuild GUI for existing players after mod update
    for _, player in pairs(game.players) do
        local frame = player.gui.screen[GUI_FRAME]
        if frame and frame.valid then
            frame.destroy()
            create_gui(player)
        end
        update_shortcut_state(player)
    end
end)

-- Settings changed — rebuild GUI to pick up new title/label
script.on_event(defines.events.on_runtime_mod_setting_changed, function(event)
    if event.setting == "claude-interface-title" or event.setting == "claude-interface-agent-label" then
        local player = game.get_player(event.player_index)
        if player then
            local frame = player.gui.screen[GUI_FRAME]
            if frame and frame.valid then
                frame.destroy()
                create_gui(player)
            end
        end
    end
end)

-- Auto-spectator: when spectator_mode is enabled, new players join as spectators
script.on_event(defines.events.on_player_joined_game, function(event)
    if storage.spectator_mode then
        local player = game.get_player(event.player_index)
        if player and player.controller_type ~= defines.controllers.spectator then
            enable_spectator(player)
        end
    end
end)

-- Capture in-game chat for the bridge (registered in the mod -> MP-safe)
script.on_event(defines.events.on_console_chat, function(event)
    if not event.message then return end
    storage.chat_messages = storage.chat_messages or {}
    local player_name = "console"
    if event.player_index then
        local p = game.get_player(event.player_index)
        if p then player_name = p.name end
    end
    table.insert(storage.chat_messages, {
        player = player_name,
        message = event.message,
        tick = event.tick,
    })
    while #storage.chat_messages > MAX_MESSAGES do
        table.remove(storage.chat_messages, 1)
    end
    local target_agent = event.player_index and get_active_agent(event.player_index) or "all"
    write_bridge_message(
        event.player_index or 0,
        player_name,
        target_agent,
        event.message,
        event.tick
    )
end)

-- Hotkey toggle
script.on_event("claude-interface-toggle", function(event)
    local player = game.get_player(event.player_index)
    if player then toggle_gui(player) end
end)

-- Shortcut bar toggle
script.on_event(defines.events.on_lua_shortcut, function(event)
    if event.prototype_name ~= "claude-interface-toggle" then return end
    local player = game.get_player(event.player_index)
    if player then toggle_gui(player) end
end)

-- Tab switching
script.on_event(defines.events.on_gui_selected_tab_changed, function(event)
    if not event.element or not event.element.valid then return end
    if event.element.name ~= "ci_agent_tabs" then return end

    local player = game.get_player(event.player_index)
    if not player then return end

    local tabbed = event.element
    local idx = tabbed.selected_tab_index
    if not idx or not tabbed.tabs[idx] then return end

    local tab_obj = tabbed.tabs[idx].tab
    local tab_name = tab_obj.name  -- "ci_tab_<agent_name>"
    local agent_name = tab_name:sub(8)  -- strip "ci_tab_"

    storage.active_agent[player.index] = agent_name

    -- Clear badge on newly selected tab
    tab_obj.badge_text = ""
end)

-- Click handler
script.on_event(defines.events.on_gui_click, function(event)
    if not event.element or not event.element.valid then return end
    local name = event.element.name

    if name == "ci_send" then
        handle_send(game.get_player(event.player_index))
    elseif name == "ci_close" then
        local player = game.get_player(event.player_index)
        destroy_gui(player)
        update_shortcut_state(player)
    end
end)

-- Enter key submits
script.on_event(defines.events.on_gui_confirmed, function(event)
    if not event.element or not event.element.valid then return end
    if event.element.name == "ci_input" then
        handle_send(game.get_player(event.player_index))
    end
end)

-- Escape closes
script.on_event(defines.events.on_gui_closed, function(event)
    if event.element and event.element.valid and event.element.name == GUI_FRAME then
        local player = game.get_player(event.player_index)
        destroy_gui(player)
        update_shortcut_state(player)
    end
end)
