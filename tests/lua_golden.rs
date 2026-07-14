use factorioctl::client::lua::LuaCommand;
use factorioctl::client::AgentId;
use factorioctl::world::{Area, Direction, Position};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

struct LuaCase {
    name: &'static str,
    lua: String,
}

impl LuaCase {
    fn new(name: &'static str, lua: String) -> Self {
        Self { name, lua }
    }

    fn assert_invariants(&self) {
        assert_no_same_line_trailing_comments(self.name, &self.lua);
        assert_balanced_double_quotes(self.name, &self.lua);
    }
}

fn remote_request(command: &str) -> Value {
    let request = command
        .strip_prefix("/claude ")
        .unwrap_or_else(|| panic!("expected /claude command envelope, got:\n{command}"));
    serde_json::from_str(request).expect("remote request envelope should be JSON")
}

fn remote_args(command: &str) -> Vec<Value> {
    remote_request(command)["args"]
        .as_array()
        .expect("request args should be an array")
        .clone()
}

fn assert_remote_request(name: &str, command: &str, method: &str) {
    let request = remote_request(command);
    assert_eq!(
        request["fn"].as_str(),
        Some(method),
        "{name} should target remote {method:?}:\n{command}"
    );
    let args = request["args"]
        .as_array()
        .expect("request args should be an array");
    assert_eq!(
        request["n"].as_u64(),
        Some(args.len() as u64),
        "{name} should include explicit n matching args length:\n{command}"
    );
}

fn pos(x: f64, y: f64) -> Position {
    Position::new(x, y)
}

fn area() -> Area {
    Area::new(-1.0, -2.0, 3.0, 4.0)
}

fn legacy_agent() -> AgentId {
    AgentId::new(None).expect("legacy agent id")
}

fn named_agent() -> AgentId {
    AgentId::new(Some("doug")).expect("named agent id")
}

fn manifest_remotes() -> BTreeMap<String, Vec<String>> {
    let manifest: Value =
        serde_json::from_str(include_str!("../mod/claude-interface/remote_api.json"))
            .expect("remote_api.json should be valid JSON");
    let remotes = manifest["remotes"]
        .as_object()
        .expect("remote_api.json remotes should be an object");
    remotes
        .iter()
        .map(|(name, spec)| {
            let args = spec["args"]
                .as_array()
                .expect("remote args should be an array")
                .iter()
                .map(|arg| {
                    arg.as_str()
                        .expect("remote arg names should be strings")
                        .to_string()
                })
                .collect();
            (name.to_string(), args)
        })
        .collect()
}

fn literal_call_remote_arguments(source: &str) -> Vec<(String, Vec<String>)> {
    let mut calls = Vec::new();
    let mut rest = source;
    let needle = ".call_remote(";
    while let Some(call_index) = rest.find(needle) {
        let after_call = &rest[call_index + needle.len()..];
        let trimmed = after_call.trim_start();
        let Some(after_quote) = trimmed.strip_prefix('"') else {
            rest = after_call;
            continue;
        };
        let Some(name_end) = after_quote.find('"') else {
            break;
        };
        let name = &after_quote[..name_end];
        let call_tail = &after_quote[name_end + 1..];
        let await_index = call_tail.find(".await").unwrap_or(call_tail.len());
        let call_expression = &call_tail[..await_index];
        let Some(array_index) = call_expression.find("&[") else {
            rest = after_call;
            continue;
        };
        let array = &call_expression[array_index + 1..];
        let Some(array_end) = matching_square_bracket(array) else {
            rest = after_call;
            continue;
        };
        calls.push((
            name.to_string(),
            split_top_level_arguments(&array[1..array_end]),
        ));
        rest = after_call;
    }
    calls
}

fn matching_square_bracket(value: &str) -> Option<usize> {
    let mut depth = 0_i32;
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in value.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '"' {
            in_string = true;
        } else if ch == '[' {
            depth += 1;
        } else if ch == ']' {
            depth -= 1;
            if depth == 0 {
                return Some(index);
            }
        }
    }
    None
}

fn split_top_level_arguments(value: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut start = 0;
    let mut parens = 0_i32;
    let mut squares = 0_i32;
    let mut braces = 0_i32;
    let mut in_string = false;
    let mut escaped = false;
    for (index, ch) in value.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '(' => parens += 1,
            ')' => parens -= 1,
            '[' => squares += 1,
            ']' => squares -= 1,
            '{' => braces += 1,
            '}' => braces -= 1,
            ',' if parens == 0 && squares == 0 && braces == 0 => {
                let argument = value[start..index].trim();
                if !argument.is_empty() {
                    arguments.push(argument.to_string());
                }
                start = index + 1;
            }
            _ => {}
        }
    }
    let argument = value[start..].trim();
    if !argument.is_empty() {
        arguments.push(argument.to_string());
    }
    arguments
}

fn rust_wrapper_remote_names() -> BTreeSet<String> {
    let source = include_str!("../src/client/lua.rs");
    let needle = "claude_interface_json_call(";
    let mut names = BTreeSet::new();
    let mut rest = source;
    while let Some(index) = rest.find(needle) {
        rest = &rest[index + needle.len()..];
        let trimmed = rest.trim_start();
        if let Some(after_quote) = trimmed.strip_prefix('"') {
            if let Some(end) = after_quote.find('"') {
                names.insert(after_quote[..end].to_string());
            }
        }
    }
    names
}

fn control_lua_remote_signatures() -> BTreeMap<String, Vec<String>> {
    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let mut signatures = BTreeMap::new();
    let mut in_api = false;
    for line in control_lua.lines() {
        let trimmed = line.trim();
        if trimmed == "local api = {" {
            in_api = true;
            continue;
        }
        if !in_api {
            continue;
        }
        if trimmed == "}" {
            break;
        }
        let Some((name, rest)) = trimmed.split_once(" = function(") else {
            continue;
        };
        let Some((args, _)) = rest.split_once(')') else {
            continue;
        };
        let args = args
            .split(',')
            .map(str::trim)
            .filter(|arg| !arg.is_empty())
            .map(str::to_string)
            .collect();
        signatures.insert(name.to_string(), args);
    }
    signatures
}

fn lifecycle_remote_names() -> BTreeSet<String> {
    [
        "autonomy_snapshot",
        "clear_chat",
        "connected_player_count",
        "connected_player_count_result",
        "ensure_surface",
        "ensure_surface_result",
        "production_statistics",
        "get_character",
        "has_walk_target",
        "inject_message",
        "list_characters",
        "live_state_line",
        "live_state_result",
        "ping",
        "pre_place_character",
        "pre_place_character_result",
        "receive_response",
        "register_agent",
        "register_character",
        "set_spectator_mode",
        "set_status",
        "set_walk",
        "stop_walk",
        "tool_status",
        "unregister_agent",
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn all_lua_cases() -> Vec<LuaCase> {
    vec![
        LuaCase::new("get_surfaces", LuaCommand::get_surfaces()),
        LuaCase::new(
            "find_entities",
            LuaCommand::find_entities(
                area(),
                Some("assembling-machine"),
                Some("assembling-machine-1"),
            ),
        ),
        LuaCase::new("get_entity", LuaCommand::get_entity(42)),
        LuaCase::new(
            "get_entity_drop_position",
            LuaCommand::get_entity_drop_position(42),
        ),
        LuaCase::new("get_entity_inventory", LuaCommand::get_entity_inventory(42)),
        LuaCase::new(
            "find_resources",
            LuaCommand::find_resources(area(), Some("iron-ore")),
        ),
        LuaCase::new(
            "find_nearest_resource",
            LuaCommand::find_nearest_resource("coal", pos(1.5, 2.5)),
        ),
        LuaCase::new("get_tiles", LuaCommand::get_tiles(area())),
        LuaCase::new("get_tile", LuaCommand::get_tile(pos(7.0, 8.0))),
        LuaCase::new(
            "init_character",
            LuaCommand::init_character(&legacy_agent(), 0.0, 0.0),
        ),
        LuaCase::new(
            "teleport_character",
            LuaCommand::teleport_character(&legacy_agent(), pos(10.0, 11.0)),
        ),
        LuaCase::new(
            "walk_character",
            LuaCommand::walk_character(&legacy_agent(), pos(12.0, 13.0)),
        ),
        LuaCase::new(
            "walk_character_named",
            LuaCommand::walk_character(&named_agent(), pos(12.0, 13.0)),
        ),
        LuaCase::new(
            "walk_to_named_target",
            LuaCommand::set_walk_target(&named_agent(), pos(12.0, 13.0)),
        ),
        LuaCase::new(
            "character_status",
            LuaCommand::character_status(&legacy_agent()),
        ),
        LuaCase::new(
            "character_inventory",
            LuaCommand::character_inventory(&legacy_agent()),
        ),
        LuaCase::new(
            "get_character_position",
            LuaCommand::get_character_position(&legacy_agent()),
        ),
        LuaCase::new(
            "start_mining",
            LuaCommand::start_mining(&legacy_agent(), pos(14.0, 15.0)),
        ),
        LuaCase::new("stop_mining", LuaCommand::stop_mining(&legacy_agent())),
        LuaCase::new(
            "get_mining_status",
            LuaCommand::get_mining_status(&legacy_agent()),
        ),
        LuaCase::new(
            "mine_at",
            LuaCommand::mine_at(&legacy_agent(), pos(16.0, 17.0), 2),
        ),
        LuaCase::new(
            "mine_nearest",
            LuaCommand::mine_nearest(&legacy_agent(), "iron-ore", 3),
        ),
        LuaCase::new(
            "find_nearest_minable",
            LuaCommand::find_nearest_minable(&legacy_agent(), "iron-ore", 100),
        ),
        LuaCase::new(
            "craft",
            LuaCommand::craft(&legacy_agent(), "iron-gear-wheel", 4),
        ),
        LuaCase::new(
            "wait_for_crafting",
            LuaCommand::wait_for_crafting(&legacy_agent()),
        ),
        LuaCase::new(
            "place_entity",
            LuaCommand::place_entity(
                &legacy_agent(),
                "burner-mining-drill",
                pos(18.0, 19.0),
                Direction::East,
            ),
        ),
        LuaCase::new(
            "check_entity_placement",
            LuaCommand::check_entity_placement(
                &legacy_agent(),
                "offshore-pump",
                pos(18.0, 19.0),
                Direction::West,
            ),
        ),
        LuaCase::new(
            "find_entity_placements",
            LuaCommand::find_entity_placements(
                &legacy_agent(),
                "offshore-pump",
                pos(18.0, 19.0),
                10,
                20,
            ),
        ),
        LuaCase::new(
            "build_edge_miner",
            LuaCommand::build_edge_miner(
                &legacy_agent(),
                "iron-ore",
                pos(57.0, -22.0),
                25,
                "burner-mining-drill",
                10,
            ),
        ),
        LuaCase::new(
            "build_direct_smelter",
            LuaCommand::build_direct_smelter(
                &legacy_agent(),
                Some(42),
                None,
                "stone-furnace",
                "burner-inserter",
                "transport-belt",
                6,
            ),
        ),
        LuaCase::new(
            "plan_steam_power",
            LuaCommand::plan_steam_power(
                &named_agent(),
                Area::new(-40.0, 37.0, -30.0, 57.0),
                pos(55.0, -2.0),
            ),
        ),
        LuaCase::new(
            "place_underground_belt",
            LuaCommand::place_underground_belt(
                &legacy_agent(),
                "underground-belt",
                pos(20.0, 21.0),
                Direction::South,
                "output",
            ),
        ),
        LuaCase::new(
            "place_ghost",
            LuaCommand::place_ghost(
                &legacy_agent(),
                "stone-furnace",
                pos(22.0, 23.0),
                Direction::West,
            ),
        ),
        LuaCase::new(
            "build_drill_array",
            LuaCommand::build_drill_array(
                &legacy_agent(),
                2,
                "iron-ore",
                Some((20.0, 21.0)),
                "burner-mining-drill",
                "south",
            ),
        ),
        LuaCase::new(
            "build_smelter_line",
            LuaCommand::build_smelter_line(
                &legacy_agent(),
                3,
                (22.0, 23.0),
                "stone-furnace",
                "east",
                3,
            ),
        ),
        LuaCase::new(
            "remove_entity_at",
            LuaCommand::remove_entity_at(&legacy_agent(), pos(24.0, 25.0)),
        ),
        LuaCase::new(
            "remove_entity",
            LuaCommand::remove_entity(&legacy_agent(), 43),
        ),
        LuaCase::new("rotate_entity", LuaCommand::rotate_entity(44, 4)),
        LuaCase::new(
            "insert_items",
            LuaCommand::insert_items(&legacy_agent(), 45, "coal", 5, "fuel"),
        ),
        LuaCase::new(
            "extract_items",
            LuaCommand::extract_items(&legacy_agent(), 46, "iron-ore", 6, "chest"),
        ),
        LuaCase::new("set_recipe", LuaCommand::set_recipe(47, "copper-cable")),
        LuaCase::new("get_recipe", LuaCommand::get_recipe("iron-plate")),
        LuaCase::new(
            "get_recipes_by_category",
            LuaCommand::get_recipes_by_category("crafting"),
        ),
        LuaCase::new(
            "get_recipes_for_item",
            LuaCommand::get_recipes_for_item("transport-belt"),
        ),
        LuaCase::new(
            "get_prototype",
            LuaCommand::get_prototype("assembling-machine-1"),
        ),
        LuaCase::new(
            "create_native_blueprint",
            LuaCommand::create_native_blueprint(&legacy_agent(), area()),
        ),
        LuaCase::new(
            "save_blueprint",
            LuaCommand::save_blueprint(&legacy_agent(), "starter", area()),
        ),
        LuaCase::new("list_blueprints", LuaCommand::list_blueprints()),
        LuaCase::new("get_blueprint", LuaCommand::get_blueprint("starter")),
        LuaCase::new(
            "place_blueprint",
            LuaCommand::place_blueprint(&legacy_agent(), "starter", pos(26.0, 27.0), 4),
        ),
        LuaCase::new(
            "import_blueprint",
            LuaCommand::import_blueprint(&legacy_agent(), "0eNq-test", pos(28.0, 29.0), 8),
        ),
        LuaCase::new("delete_blueprint", LuaCommand::delete_blueprint("starter")),
        LuaCase::new("register_chat_handler", LuaCommand::register_chat_handler()),
        LuaCase::new(
            "get_and_clear_chat_messages",
            LuaCommand::get_and_clear_chat_messages(),
        ),
        LuaCase::new(
            "broadcast_console",
            LuaCommand::broadcast_console("hello from test"),
        ),
        LuaCase::new(
            "broadcast_flying_text",
            LuaCommand::broadcast_flying_text("hello from test"),
        ),
        LuaCase::new("get_tick", LuaCommand::get_tick()),
        LuaCase::new("set_tick_paused", LuaCommand::set_tick_paused(true)),
        LuaCase::new("set_game_speed", LuaCommand::set_game_speed(1.25)),
        LuaCase::new("get_research_status", LuaCommand::get_research_status()),
        LuaCase::new(
            "get_available_research",
            LuaCommand::get_available_research(&legacy_agent()),
        ),
        LuaCase::new(
            "feed_lab_from_inventory",
            LuaCommand::feed_lab_from_inventory(
                &legacy_agent(),
                42,
                "automation-science-pack",
                5,
                true,
            ),
        ),
        LuaCase::new("start_research", LuaCommand::start_research("automation")),
        LuaCase::new(
            "is_tech_researched",
            LuaCommand::is_tech_researched("automation"),
        ),
        LuaCase::new("get_power_status", LuaCommand::get_power_status(30, 31, 10)),
        LuaCase::new(
            "get_power_networks",
            LuaCommand::get_power_networks(32, 33, 11),
        ),
        LuaCase::new(
            "find_power_issues",
            LuaCommand::find_power_issues(34, 35, 12),
        ),
        LuaCase::new(
            "diagnose_steam_power",
            LuaCommand::diagnose_steam_power(35, 36, 12),
        ),
        LuaCase::new(
            "get_power_coverage",
            LuaCommand::get_power_coverage(36, 37, 13),
        ),
        LuaCase::new("get_alerts", LuaCommand::get_alerts(38, 39, 14)),
        LuaCase::new("get_belt_contents", LuaCommand::get_belt_contents(area())),
        LuaCase::new(
            "get_belt_lane_contents",
            LuaCommand::get_belt_lane_contents(area()),
        ),
        LuaCase::new(
            "clear_area",
            LuaCommand::clear_area(&legacy_agent(), area(), true, true, false),
        ),
    ]
}

fn assert_no_same_line_trailing_comments(case_name: &str, lua: &str) {
    for (idx, line) in lua.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("--") {
            continue;
        }

        if let Some(comment_index) = comment_start_outside_string(line) {
            let before_comment = &line[..comment_index];
            assert!(
                before_comment.trim().is_empty(),
                "{} has a same-line trailing Lua comment on line {}: {}",
                case_name,
                idx + 1,
                line
            );
        }
    }
}

fn comment_start_outside_string(line: &str) -> Option<usize> {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;
    let bytes = line.as_bytes();

    for (idx, ch) in line.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && (in_single || in_double) {
            escaped = true;
            continue;
        }
        if ch == '\'' && !in_double {
            in_single = !in_single;
            continue;
        }
        if ch == '"' && !in_single {
            in_double = !in_double;
            continue;
        }
        if ch == '-' && !in_single && !in_double && bytes.get(idx + 1) == Some(&b'-') {
            return Some(idx);
        }
    }

    None
}

fn assert_balanced_double_quotes(case_name: &str, lua: &str) {
    let mut in_single = false;
    let mut in_double = false;
    let mut escaped = false;

    for line in lua.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("--") {
            continue;
        }

        let executable = comment_start_outside_string(line)
            .map(|idx| &line[..idx])
            .unwrap_or(line);

        for ch in executable.chars() {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' && (in_single || in_double) {
                escaped = true;
                continue;
            }
            if ch == '\'' && !in_double {
                in_single = !in_single;
                continue;
            }
            if ch == '"' && !in_single {
                in_double = !in_double;
            }
        }
    }

    assert!(
        !in_single && !in_double,
        "{} has an unbalanced quoted Lua string",
        case_name
    );
}

fn assert_uses_factorio_2_get_contents_shape(case_name: &str, lua: &str) {
    assert!(
        lua.contains("for _, item in pairs(inv.get_contents()) do"),
        "{} should iterate the Factorio 2.0 get_contents() object array",
        case_name
    );
    assert!(
        lua.contains("item.name") && lua.contains("item.count"),
        "{} should read item.name and item.count from get_contents() entries",
        case_name
    );
    assert!(
        !lua.contains("for item, count in pairs(inv.get_contents()) do")
            && !lua.contains("for name, count in pairs(inv.get_contents()) do"),
        "{} should not use the pre-Factorio-2.0 get_contents() dict shape",
        case_name
    );
}

#[test]
fn generated_lua_has_rcon_safe_syntax_invariants() {
    for case in all_lua_cases() {
        case.assert_invariants();
    }
}

#[test]
fn remote_api_manifest_matches_rust_wrappers_and_mod_exports() {
    let manifest = manifest_remotes();
    let manifest_names = manifest.keys().cloned().collect::<BTreeSet<_>>();
    let wrapper_names = rust_wrapper_remote_names();
    let lifecycle_names = lifecycle_remote_names();
    let control_signatures = control_lua_remote_signatures();
    let control_names = control_signatures.keys().cloned().collect::<BTreeSet<_>>();

    assert_eq!(
        wrapper_names
            .union(&lifecycle_names)
            .cloned()
            .collect::<BTreeSet<_>>(),
        manifest_names,
        "remote_api.json should cover Rust wrappers plus bridge lifecycle remotes"
    );

    for name in &manifest_names {
        assert!(
            control_names.contains(name),
            "remote_api.json entry {name:?} is not exposed by control.lua"
        );
        assert_eq!(
            control_signatures.get(name),
            manifest.get(name),
            "remote_api.json args for {name:?} should match the control.lua remote signature"
        );
    }
}

#[test]
fn literal_rust_remote_calls_match_manifest_argument_contracts() {
    let manifest = manifest_remotes();
    let sources = [
        ("src/client/mod.rs", include_str!("../src/client/mod.rs")),
        ("src/bin/mcp.rs", include_str!("../src/bin/mcp.rs")),
        ("src/cli/map.rs", include_str!("../src/cli/map.rs")),
        ("src/cli/power.rs", include_str!("../src/cli/power.rs")),
        (
            "src/cli/research.rs",
            include_str!("../src/cli/research.rs"),
        ),
        ("src/cli/say.rs", include_str!("../src/cli/say.rs")),
    ];

    for (path, source) in sources {
        for (remote, arguments) in literal_call_remote_arguments(source) {
            let expected = manifest
                .get(&remote)
                .unwrap_or_else(|| panic!("{path} calls unmanifested remote {remote:?}"));
            assert_eq!(
                arguments.len(),
                expected.len(),
                "{path} remote {remote:?} argument count/order drifted: {arguments:?}"
            );
            for (index, name) in expected.iter().enumerate() {
                if name == "agent_id" {
                    assert!(
                        arguments[index].contains("agent_id")
                            || arguments[index].contains("character_storage_key"),
                        "{path} remote {remote:?} must pass agent_id at argument {index}: {arguments:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn mod_json_response_helper_lives_in_domain_module() {
    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let json_response_lua = include_str!("../mod/claude-interface/json_response.lua");

    assert!(
        control_lua.contains(r#"local json_response = require("json_response")"#)
            && control_lua.contains("local json_remote_call = json_response.remote_call"),
        "control.lua should import the shared JSON remote-call helper"
    );
    assert!(
        !control_lua.contains("local function json_remote_call"),
        "control.lua should not re-inline the JSON remote-call helper"
    );
    assert!(
        json_response_lua.contains("function M.remote_call(action_name, fn, ...)")
            && json_response_lua.contains("pcall(fn, ...)")
            && json_response_lua.contains("success = false")
            && json_response_lua.contains("helpers.table_to_json(result_or_error)"),
        "json_response.lua should own the remote JSON wrapper and keep pcall failures typed"
    );
}

#[test]
fn claude_command_dispatcher_uses_shared_api_and_structured_errors() {
    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let json_response_lua = include_str!("../mod/claude-interface/json_response.lua");

    assert!(
        control_lua.contains("local api = {")
            && control_lua.contains(r#"remote.add_interface("claude_interface", api)"#)
            && control_lua.contains(r#"commands.add_command("claude""#),
        "control.lua should expose the same api table via remote interface and /claude command"
    );
    assert!(
        control_lua.contains("pcall(helpers.json_to_table, cmd.parameter or \"\")")
            && control_lua.contains("local handler = api[request.fn]")
            && control_lua.contains("table.unpack(args, 1, n)"),
        "/claude dispatcher should parse one JSON envelope and preserve nil holes via explicit n"
    );
    assert!(
        control_lua.contains("if cmd.player_index ~= nil then")
            && control_lua.contains("restricted to the server RCON console"),
        "/claude must reject in-game callers and remain an authenticated server/RCON bridge"
    );
    assert!(
        control_lua.contains(r#"json_response.error("bad_request""#)
            && control_lua.contains(r#"json_response.error("unknown_function""#)
            && control_lua.contains(r#"json_response.error("lua_error""#)
            && control_lua.contains("action_needed = \"sync_or_restart_mod\""),
        "/claude dispatcher should return structured request/skew/lua errors"
    );
    assert!(
        json_response_lua.contains("function M.error(error_kind, message, extra)")
            && json_response_lua.contains("result.error_kind = error_kind")
            && json_response_lua.contains("result.success = false"),
        "json_response.error should produce the structured error_kind envelope"
    );
}

#[test]
fn critical_mod_safety_contracts_are_explicit() {
    let characters_lua = include_str!("../mod/claude-interface/characters.lua");
    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let entities_lua = include_str!("../mod/claude-interface/entities.lua");
    let placement_lua = include_str!("../mod/claude-interface/placement.lua");
    let power_lua = include_str!("../mod/claude-interface/power.lua");
    let research_lua = include_str!("../mod/claude-interface/research.lua");
    let transport_lua = include_str!("../mod/claude-interface/transport.lua");
    let world_lua = include_str!("../mod/claude-interface/world.lua");

    let pre_place = characters_lua
        .split("function M.pre_place(agent_id, planet_name, spawn_x)")
        .nth(1)
        .and_then(|tail| tail.split("function M.pre_place_result").next())
        .expect("pre_place should precede pre_place_result");
    let find_character = characters_lua
        .split("function M.find(agent_id)")
        .nth(1)
        .and_then(|tail| tail.split("function M.remember").next())
        .expect("find should precede remember");
    assert!(
        characters_lua.contains("local function is_player_character(character)")
            && find_character.contains("and not is_player_character(character)")
            && !find_character.contains("game.connected_players")
            && pre_place.contains("local character = M.find(agent_id)")
            && pre_place.contains("return \"already_placed\"")
            && pre_place.contains("target_surface.find_non_colliding_position(")
            && !pre_place.contains("teleport("),
        "agent identity must be exact, reject human characters, and preserve an existing NPC without relocating it"
    );
    assert!(
        control_lua.contains("local c = find_factorioctl_character(agent_id)")
            && !control_lua.contains("local c = storage.characters[agent_id]")
            && !control_lua.contains("queue_rotate = function")
            && !control_lua.contains("process_entity_queue"),
        "tick movement must revalidate NPC identity and no queued rotation may bypass reach"
    );

    assert!(
        characters_lua.contains("error_kind = \"out_of_reach\"")
            && characters_lua.contains("action_needed = \"walk_to\"")
            && placement_lua
                .matches("characters.require_position_reach")
                .count()
                >= 3
            && placement_lua.contains("characters.require_entity_reach(character, entity)")
            && control_lua
                .matches("characters.require_entity_reach")
                .count()
                >= 8
            && control_lua
                .matches("characters.require_position_reach")
                .count()
                >= 2
            && research_lua.contains("characters.require_entity_reach(character, lab)")
            && control_lua.contains("local function require_blueprint_reach"),
        "every direct world/entity mutation family must enforce structured character reach"
    );
    assert!(
        placement_lua.contains("function M.place_entity(agent_id")
            && !placement_lua.contains("clear_ground_items_for_placement")
            && !placement_lua.contains("item_entity.destroy"),
        "placement must fail on blocking ground items rather than deleting them"
    );
    assert!(
        control_lua.contains("error_kind = \"ambiguous_entity\"")
            && control_lua.contains("action_needed = \"remove_entity_by_unit_number\"")
            && control_lua.contains("#candidates > 1"),
        "coordinate removal must fail closed and require an exact unit number when ambiguous"
    );

    assert!(
        entities_lua.contains("game.get_entity_by_unit_number(unit_number)")
            && entities_lua.contains("for _, surface in pairs(game.surfaces) do")
            && entities_lua.contains("for _, candidate in pairs(surface.find_entities()) do")
            && entities_lua.contains("candidate.unit_number == unit_number")
            && !entities_lua.contains("radius = 500"),
        "entity lookup must use Factorio's native index plus an unbounded all-surface fallback for unsupported prototypes"
    );
    assert!(
        entities_lua.contains("local PRODUCTION_ENTITY_TYPES = {")
            && entities_lua.contains("[\"assembling-machine\"] = true")
            && entities_lua.contains("[\"furnace\"] = true")
            && entities_lua.contains("[\"mining-drill\"] = true")
            && entities_lua.contains("[\"lab\"] = true")
            && entities_lua.contains("[\"rocket-silo\"] = true")
            && entities_lua.contains("if is_production_entity(entity) then")
            && entities_lua.contains("type = entity.type")
            && !entities_lua
                .contains("if status_value ~= nil then\n            local products_finished"),
        "verify_production must report actual producers, not every status-bearing belt or inserter"
    );
    for (name, source) in [
        ("characters.lua", characters_lua),
        ("entities.lua", entities_lua),
        ("placement.lua", placement_lua),
        ("power.lua", power_lua),
        ("research.lua", research_lua),
        ("transport.lua", transport_lua),
        ("world.lua", world_lua),
    ] {
        assert!(
            !source.contains("game.surfaces[1]"),
            "{name} must not silently redirect agent work to surface 1"
        );
    }
    assert!(
        power_lua.contains("local surface = character.surface")
            && power_lua.contains("local force = character.force")
            && !power_lua.contains("surface.create_entity")
            && !power_lua.contains("entity.destroy"),
        "power diagnostics/planners must be character-scoped and observational"
    );

    assert!(
        research_lua.contains("error_kind = \"research_trigger_required\"")
            && research_lua.contains("local added = force.add_research(tech)")
            && research_lua.contains("result.steps = {}")
            && research_lua.contains("result.next_action = \"automate_science_delivery\"")
            && !research_lua.contains("tech.researched = true"),
        "research must use Factorio's queue/trigger mechanics and not advertise hand-feeding as automation"
    );
    assert!(
        entities_lua.contains("local function fuel_connections(surface, force, consumer)")
            && entities_lua.contains("inserter_operational = inserter_operational")
            && entities_lua.contains("source_durable = source_record.durable == true")
            && entities_lua.contains("source_operational = source_record.operational == true")
            && entities_lua.contains("and source_record.durable == true")
            && entities_lua.contains("stocked_without_proven_upstream")
            && entities_lua.contains("operational_coal_drill")
            && entities_lua.contains("if connection.durable then")
            && entities_lua.contains("consumer.fuel_topology_present")
            && entities_lua.contains("source_is_proposed = true")
            && entities_lua.contains("if a.operational ~= b.operational then")
            && entities_lua.contains("if a.distance_sq ~= b.distance_sq then"),
        "fuel diagnosis must prove adjacent inserter/source operation and rank unproven sources explicitly"
    );

    assert!(
        control_lua
            .contains("player.print(\"[\" .. get_agent_label(agent_name) .. \"] \" .. text)")
            && control_lua.contains("script.on_event(defines.events.on_console_chat")
            && control_lua.contains("write_bridge_message(")
            && transport_lua.contains("quality = inventory.quality_name(item)"),
        "normal chat must wake/display the bot and transport observations must retain item quality"
    );
}

#[test]
fn corrected_inventory_readers_document_factorio_2_get_contents_shape() {
    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let research_lua = include_str!("../mod/claude-interface/research.lua");
    assert_uses_factorio_2_get_contents_shape("research.lua get_available_research", research_lua);
    assert_uses_factorio_2_get_contents_shape("control.lua clear_area_impl", control_lua);
}

fn assert_uses_transport_line_contents_shape(case_name: &str, lua: &str) {
    assert!(
        lua.contains("for _, item in pairs(line1.get_contents()) do")
            || lua.contains("for _, item in pairs(line.get_contents()) do"),
        "{} should iterate LuaTransportLine::get_contents() as a Factorio 2.0 object array",
        case_name
    );
    assert!(
        lua.contains("item.name") && lua.contains("item.count"),
        "{} should read item.name and item.count from transport-line contents",
        case_name
    );
    assert!(
        !lua.contains("for name, count in pairs(line1.get_contents()) do")
            && !lua.contains("for item_name, item_count in pairs(line.get_contents()) do"),
        "{} should not use the pre-Factorio-2.0 transport-line contents map shape",
        case_name
    );
}

#[test]
fn transport_line_readers_document_factorio_2_object_array_shape() {
    for (name, lua, method) in [
        (
            "get_belt_contents",
            LuaCommand::get_belt_contents(area()),
            "get_belt_contents",
        ),
        (
            "get_belt_lane_contents",
            LuaCommand::get_belt_lane_contents(area()),
            "get_belt_lane_contents",
        ),
    ] {
        assert_remote_request(name, &lua, method);

        for forbidden in [
            "get_transport_line",
            "get_contents()",
            "surface.find_entities_filtered",
        ] {
            assert!(
                !lua.contains(forbidden),
                "{name} Rust wrapper should not embed heavy Lua {forbidden:?}:\n{lua}"
            );
        }
    }

    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let transport_lua = include_str!("../mod/claude-interface/transport.lua");
    assert!(
        control_lua.contains("local transport = require(\"transport\")")
            && control_lua.contains("get_belt_contents = function(x1, y1, x2, y2, agent_id)")
            && control_lua.contains("transport.get_belt_contents")
            && control_lua.contains("get_belt_lane_contents = function(x1, y1, x2, y2, agent_id)")
            && control_lua.contains("transport.get_belt_lane_contents")
            && control_lua.contains("character and character.surface or nil"),
        "control.lua should expose both belt contents remotes through transport.lua"
    );
    assert!(
        !control_lua.contains("local function get_belt_contents_impl")
            && !control_lua.contains("local function get_belt_lane_contents_impl"),
        "control.lua should not keep transport reader implementations after the domain split"
    );
    assert_uses_transport_line_contents_shape("transport.lua get_belt_contents", transport_lua);
    assert_uses_transport_line_contents_shape(
        "transport.lua get_belt_lane_contents",
        transport_lua,
    );
}

#[test]
fn named_walk_routes_to_mod_target_without_host_driver_state() {
    let agent = named_agent();
    let walk_character = LuaCommand::walk_character(&agent, pos(12.0, 13.0));
    let walk_target = LuaCommand::set_walk_target(&agent, pos(12.0, 13.0));

    for (name, lua) in [
        ("walk_character", walk_character.as_str()),
        ("walk_target", walk_target.as_str()),
    ] {
        assert_remote_request(name, lua, "set_walk_target");
        assert_eq!(remote_args(lua), vec![json!("doug"), json!(12), json!(13)]);
        for forbidden in [
            "storage.factorioctl_walk_targets",
            "walking_state",
            "script.on_event",
        ] {
            assert!(
                !lua.contains(forbidden),
                "{name} should not reintroduce host-side walk driver state {forbidden:?}"
            );
        }
    }
}

#[test]
fn research_readiness_counts_resolved_character_science_in_totals() {
    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let research_lua = include_str!("../mod/claude-interface/research.lua");

    assert!(
        control_lua.contains("local character = find_factorioctl_character(agent_id)")
            && control_lua.contains(
                "json_remote_call(\"get_available_research\", research.get_available_research, character)"
            ),
        "control.lua should resolve the agent character before calling research.lua"
    );

    let inventory_fold = research_lua
        .find("science_totals[item.name] = (science_totals[item.name] or 0) + item.count")
        .expect("character science should be folded into science_totals");
    let readiness_read = research_lua
        .find("local have = science_totals[ing.name] or 0")
        .expect("research readiness should read science_totals");
    assert!(
        inventory_fold < readiness_read,
        "character science must be folded before readiness is calculated"
    );
}

#[test]
fn get_entity_inventory_uses_factorio_2_object_array_for_cjf_2() {
    let lua = LuaCommand::get_entity_inventory(42);

    assert_remote_request("get_entity_inventory", &lua, "get_entity_inventory");
    assert_eq!(remote_args(&lua), vec![json!(42)]);
    for forbidden in [
        "inv.get_contents()",
        "storage.factorioctl_entities[",
        "surface.find_entities_filtered",
    ] {
        assert!(
            !lua.contains(forbidden),
            "get_entity_inventory Rust wrapper should not embed heavy Lua {forbidden:?}:\n{lua}"
        );
    }

    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let inventory_lua = include_str!("../mod/claude-interface/inventory.lua");
    assert!(
        control_lua.contains("local function get_entity_inventory_impl")
            && control_lua.contains("get_entity_inventory = function(unit_number)")
            && control_lua.contains("local items = inventory_contents(inv)"),
        "control.lua should expose the entity inventory remote and use the shared inventory reader"
    );
    assert!(
        control_lua.contains(r#"local inventory = require("inventory")"#)
            && control_lua.contains("local inventory_contents = inventory.contents"),
        "control.lua should import the inventory helper module"
    );
    assert_uses_factorio_2_get_contents_shape("inventory.lua contents", inventory_lua);
}

#[test]
fn world_observation_queries_live_in_the_mod_not_rust_strings() {
    for (name, lua, method) in [
        ("get_surfaces", LuaCommand::get_surfaces(), "get_surfaces"),
        (
            "find_entities",
            LuaCommand::find_entities(
                area(),
                Some("assembling-machine"),
                Some("assembling-machine-1"),
            ),
            "find_entities",
        ),
        (
            "verify_production",
            LuaCommand::verify_production(area()),
            "verify_production",
        ),
        ("get_entity", LuaCommand::get_entity(42), "get_entity"),
        (
            "get_entity_drop_position",
            LuaCommand::get_entity_drop_position(42),
            "get_entity_drop_position",
        ),
        (
            "find_resources",
            LuaCommand::find_resources(area(), Some("iron-ore")),
            "find_resources",
        ),
        (
            "find_nearest_resource",
            LuaCommand::find_nearest_resource("coal", pos(1.5, 2.5)),
            "find_nearest_resource",
        ),
        ("get_tiles", LuaCommand::get_tiles(area()), "get_tiles"),
        ("get_tile", LuaCommand::get_tile(pos(7.0, 8.0)), "get_tile"),
    ] {
        assert_remote_request(name, &lua, method);

        for forbidden in [
            "game.surfaces",
            "find_entities_filtered",
            "get_tile(",
            "defines.entity_status",
            "storage.factorioctl_entities",
            "entity.bounding_box",
            ".collides_with",
        ] {
            assert!(
                !lua.contains(forbidden),
                "{name} Rust wrapper should not embed heavy Lua {forbidden:?}:\n{lua}"
            );
        }
    }

    let client_mod = include_str!("../src/client/mod.rs");
    for forbidden in [
        "let lua = LuaCommand::get_surfaces();",
        "let lua = LuaCommand::find_entities(area, entity_type, name);",
        "let lua = LuaCommand::get_entity(unit_number);",
        "let lua = LuaCommand::get_entity_inventory(unit_number);",
        "let lua = LuaCommand::verify_production(area);",
        "let lua = LuaCommand::diagnose_factory_blockers(area, limit);",
        "let lua = LuaCommand::diagnose_fuel_sustainability(area, limit);",
        "let lua = LuaCommand::find_resources(area, resource_type);",
        "let lua = LuaCommand::find_nearest_resource(resource_name, from);",
        "let lua = LuaCommand::get_tiles(area);",
        "let lua = LuaCommand::get_tile(position);",
    ] {
        assert!(
            !client_mod.contains(forbidden),
            "FactorioClient observation queries should call /claude directly, not generated Lua {forbidden:?}"
        );
    }
    for required in [
        r#"self.call_remote("get_surfaces", &[])"#,
        r#"call_remote("#,
        r#""find_entities""#,
        r#""get_entity""#,
        r#""get_entity_inventory""#,
        r#""verify_production""#,
        r#""diagnose_factory_blockers""#,
        r#""diagnose_fuel_sustainability""#,
        r#""find_resources""#,
        r#""find_nearest_resource""#,
        r#""get_tiles""#,
        r#""get_tile""#,
    ] {
        assert!(
            client_mod.contains(required),
            "FactorioClient observation queries should retain direct /claude call marker {required:?}"
        );
    }

    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let entities_lua = include_str!("../mod/claude-interface/entities.lua");
    let world_lua = include_str!("../mod/claude-interface/world.lua");
    for required in [
        "local entities = require(\"entities\")",
        "entities.get_surfaces",
        "entities.find_entities",
        "entities.verify_production",
        "entities.get_entity",
        "entities.get_drop_position",
        "local world = require(\"world\")",
        "world.find_resources",
        "world.find_nearest_resource",
        "world.get_tiles",
        "world.get_tile",
        "get_surfaces = function()",
        "find_entities = function(x1, y1, x2, y2, entity_type, name, agent_id)",
        "verify_production = function(x1, y1, x2, y2, agent_id)",
        "get_entity = function(unit_number)",
        "get_entity_drop_position = function(unit_number)",
        "find_resources = function(x1, y1, x2, y2, resource_type, agent_id)",
        "find_nearest_resource = function(resource_name, from_x, from_y, agent_id)",
        "get_tiles = function(x1, y1, x2, y2, agent_id)",
        "get_tile = function(x, y, agent_id)",
    ] {
        assert!(
            control_lua.contains(required),
            "control.lua world-observation remotes should include {required:?}"
        );
    }
    for moved in [
        "local function entity_summary",
        "local function get_surfaces_impl",
        "local function find_entities_impl",
        "local function verify_production_impl",
        "local function get_entity_impl",
        "local function get_entity_drop_position_impl",
        "local function aggregate_resource_patches",
        "local function find_resources_impl",
        "local function find_nearest_resource_impl",
        "local function get_tiles_impl",
        "local function get_tile_impl",
    ] {
        assert!(
            !control_lua.contains(moved),
            "control.lua should not keep entity observation implementation {moved:?}"
        );
    }
    for required in [
        "function M.summary",
        "function M.find_by_unit_number",
        "function M.get_surfaces",
        "function M.find_entities",
        "function M.verify_production",
        "function M.get_entity",
        "function M.get_drop_position",
    ] {
        assert!(
            entities_lua.contains(required),
            "entities.lua should include {required:?}"
        );
    }
    for required in [
        "local function aggregate_resource_patches",
        "function M.find_resources",
        "function M.find_nearest_resource",
        "function M.get_tiles",
        "function M.get_tile",
    ] {
        assert!(
            world_lua.contains(required),
            "world.lua should include {required:?}"
        );
    }
}

#[test]
fn entity_lookup_and_drop_position_live_in_the_mod_not_rust_strings() {
    let lua_rs = include_str!("../src/client/lua.rs");
    assert!(
        !lua_rs.contains("pub fn entity_lookup")
            && !lua_rs.contains("fn register_entity")
            && !lua_rs.contains(
                "game.surfaces[1].find_entities_filtered{{area={{{{-500,-500}},{{500,500}}}}}}"
            ),
        "Rust Lua builders should not carry registry scan helpers"
    );

    let drop_lua = LuaCommand::get_entity_drop_position(42);
    assert_remote_request(
        "get_entity_drop_position",
        &drop_lua,
        "get_entity_drop_position",
    );
    assert_eq!(remote_args(&drop_lua), vec![json!(42)]);
    for forbidden in [
        "local dp",
        ".drop_position",
        "storage.factorioctl_entities",
        "find_entities_filtered",
        "game.table_to_json",
    ] {
        assert!(
            !drop_lua.contains(forbidden),
            "get_entity_drop_position Rust wrapper should not embed heavy Lua {forbidden:?}:\n{drop_lua}"
        );
    }

    let mcp_rs = include_str!("../src/bin/mcp.rs");
    assert!(
        mcp_rs.contains(r#"call_remote("#)
            && mcp_rs.contains(r#""get_entity_drop_position""#)
            && !mcp_rs.contains("fn drill_drop_position_lua")
            && !mcp_rs.contains("LuaCommand::entity_lookup"),
        "MCP drill belt-position helper should call /claude directly, not build Lua locally"
    );

    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let entities_lua = include_str!("../mod/claude-interface/entities.lua");
    for required in [
        "local entities = require(\"entities\")",
        "entities.get_drop_position",
        "get_entity_drop_position = function(unit_number)",
    ] {
        assert!(
            control_lua.contains(required),
            "control.lua entity lookup/drop-position remote should include {required:?}"
        );
    }
    for moved in [
        "local function find_entity_by_unit_number",
        "local function get_entity_drop_position_impl",
        "if not entity.drop_position then",
        "drop_x = drop_position.x",
        "belt_direction = direction",
    ] {
        assert!(
            !control_lua.contains(moved),
            "control.lua should not keep entity lookup/drop-position implementation {moved:?}"
        );
    }
    for required in [
        "function M.find_by_unit_number",
        "local registered = storage.factorioctl_entities[unit_number]",
        "function M.get_drop_position",
        "if not entity.drop_position then",
        "drop_x = drop_position.x",
        "belt_direction = direction",
    ] {
        assert!(
            entities_lua.contains(required),
            "entities.lua entity lookup/drop-position remote should include {required:?}"
        );
    }
}

#[test]
fn lifecycle_remotes_have_thin_rust_mcp_wrappers() {
    let mcp_rs = include_str!("../src/bin/mcp.rs");
    for (tool, remote) in [
        ("send_chat_response", "receive_response"),
        ("tool_status", "tool_status"),
        ("set_status", "set_status"),
        ("register_agent", "register_agent"),
        ("unregister_agent", "unregister_agent"),
        ("ensure_surface", "ensure_surface_result"),
        ("place_character", "pre_place_character_result"),
        ("set_spectator_mode", "set_spectator_mode"),
        ("ping", "ping"),
        ("live_state", "live_state_result"),
        ("connected_player_count", "connected_player_count_result"),
        ("production_statistics", "production_statistics"),
    ] {
        assert!(
            mcp_rs.contains(&format!("async fn {tool}"))
                && mcp_rs.contains(&format!("\"{remote}\"")),
            "MCP lifecycle tool {tool:?} should call mod remote {remote:?}"
        );
    }
}

#[test]
fn autonomous_agent_responses_are_broadcast_to_connected_players() {
    let control_lua = include_str!("../mod/claude-interface/control.lua");
    for required in [
        "if item.type == \"response\" then",
        "for _, player in pairs(game.connected_players) do",
        "add_chat_message(player, item.agent, \"claude\", item.text)",
    ] {
        assert!(
            control_lua.contains(required),
            "autonomous response delivery should include {required:?}"
        );
    }
    assert!(
        !control_lua.contains("Skip GUI updates for injected/synthetic messages (player_index=0)"),
        "autonomous responses must not be documented or implemented as discarded"
    );

    let response_start = control_lua
        .find("if item.type == \"response\" then")
        .expect("response queue branch");
    let tool_start = control_lua[response_start..]
        .find("elseif item.type == \"tool\" then")
        .map(|offset| response_start + offset)
        .expect("tool queue branch");
    assert!(
        !control_lua[response_start..tool_start].contains("set_status("),
        "streamed response chunks must not mark a still-running turn Ready"
    );

    let status_start = control_lua
        .find("elseif item.type == \"status\" then")
        .expect("status queue branch");
    let register_start = control_lua[status_start..]
        .find("elseif item.type == \"register\" then")
        .map(|offset| status_start + offset)
        .expect("register queue branch");
    let status_branch = &control_lua[status_start..register_start];
    assert!(status_branch.contains("for _, player in pairs(game.connected_players) do"));
    assert!(status_branch.contains("set_status(player, item.text)"));
}

#[test]
fn inserter_mutations_report_engine_geometry_and_direct_smelter_uses_pickup_direction() {
    let placement_lua = include_str!("../mod/claude-interface/placement.lua");
    for required in [
        "result.pickup_position = pos_table(entity.pickup_position)",
        "result.drop_position = pos_table(entity.drop_position)",
        "local inserter_pos = {belt_tile.x - vec.x, belt_tile.y - vec.y}",
        "local drop_tile = {x = belt_tile.x - (vec.x * 2), y = belt_tile.y - (vec.y * 2)}",
        "direction = pickup_dir",
    ] {
        assert!(
            placement_lua.contains(required),
            "inserter geometry should include {required:?}"
        );
    }
    for reversed in [
        "local inserter_pos = {belt_tile.x + vec.x, belt_tile.y + vec.y}",
        "local drop_tile = {x = belt_tile.x + (vec.x * 2), y = belt_tile.y + (vec.y * 2)}",
        "for _, drop_dir in pairs(directions) do",
    ] {
        assert!(
            !placement_lua.contains(reversed),
            "direct-smelter planning must use Factorio's pickup-direction geometry, not {reversed:?}"
        );
    }
}

#[test]
fn fuel_inserter_candidates_use_the_belt_as_factorio_pickup_side() {
    let entities_lua = include_str!("../mod/claude-interface/entities.lua");
    let function_start = entities_lua
        .find("local function inserter_fuel_candidates")
        .expect("fuel candidate function");
    let function_end = entities_lua[function_start..]
        .find("\nlocal function add_action")
        .map(|offset| function_start + offset)
        .expect("end of fuel candidate function");
    let function = &entities_lua[function_start..function_end];
    let expected = [
        ("north", "north"),
        ("east", "east"),
        ("south", "south"),
        ("west", "west"),
    ];
    for (index, (side, direction)) in expected.iter().enumerate() {
        let start = function
            .find(&format!("side = \"{side}\""))
            .unwrap_or_else(|| panic!("missing {side} fuel candidate"));
        let end = expected
            .get(index + 1)
            .and_then(|(next_side, _)| function.find(&format!("side = \"{next_side}\"")))
            .unwrap_or(function.len());
        assert!(
            function[start..end].contains(&format!("direction = defines.direction.{direction}")),
            "{side} fuel candidate must pick up from the {direction} belt side"
        );
    }
}

#[test]
fn broadcast_display_gameplay_lives_in_the_mod_not_cli_or_mcp_strings() {
    let console_lua = LuaCommand::broadcast_console("hello");
    let flying_text_lua = LuaCommand::broadcast_flying_text("hello");
    for (name, lua, method) in [
        (
            "broadcast_console",
            console_lua.as_str(),
            "broadcast_console",
        ),
        (
            "broadcast_flying_text",
            flying_text_lua.as_str(),
            "broadcast_flying_text",
        ),
    ] {
        assert_remote_request(name, lua, method);
        for forbidden in ["game.print", "game.players[1]", "create_local_flying_text"] {
            assert!(
                !lua.contains(forbidden),
                "{name} should not embed display gameplay Lua {forbidden:?}"
            );
        }
    }

    let say_rs = include_str!("../src/cli/say.rs");
    let mcp_rs = include_str!("../src/bin/mcp.rs");
    for required in ["broadcast_console", "broadcast_flying_text"] {
        assert!(
            say_rs.contains("call_remote(")
                && say_rs.contains(required)
                && mcp_rs.contains("call_remote(")
                && mcp_rs.contains(required),
            "CLI and MCP broadcast paths should call /claude directly {required:?}"
        );
    }
    for forbidden in [
        "game.print(\"[Agent]",
        "game.players[1]",
        "create_local_flying_text",
    ] {
        assert!(
            !say_rs.contains(forbidden) && !mcp_rs.contains(forbidden),
            "CLI/MCP broadcast paths should not embed display Lua {forbidden:?}"
        );
    }

    let control_lua = include_str!("../mod/claude-interface/control.lua");
    for required in [
        "local function broadcast_console_impl",
        "game.print(\"[Agent] \" .. tostring(message or \"\"))",
        "local function broadcast_flying_text_impl",
        "for _, player in pairs(game.connected_players) do",
        "player.create_local_flying_text{",
        "broadcast_console = function(message)",
        "broadcast_flying_text = function(message)",
    ] {
        assert!(
            control_lua.contains(required),
            "control.lua broadcast remotes should include {required:?}"
        );
    }
}

#[test]
fn tick_control_gameplay_lives_in_the_mod_not_rust_strings() {
    for (name, lua, method) in [
        ("get_tick", LuaCommand::get_tick(), "get_tick"),
        (
            "set_tick_paused",
            LuaCommand::set_tick_paused(true),
            "set_tick_paused",
        ),
        (
            "set_game_speed",
            LuaCommand::set_game_speed(1.25),
            "set_game_speed",
        ),
    ] {
        assert_remote_request(name, &lua, method);
        for forbidden in ["rcon.print(game.tick)", "game.tick_paused", "game.speed"] {
            assert!(
                !lua.contains(forbidden),
                "{name} should not embed tick-control gameplay Lua {forbidden:?}"
            );
        }
    }

    let client_mod = include_str!("../src/client/mod.rs");
    for forbidden in [
        "execute_lua(\"rcon.print(game.tick)\")",
        "execute_lua(\"game.tick_paused = true\")",
        "execute_lua(\"game.tick_paused = false\")",
        "format!(\"game.speed = {}\"",
    ] {
        assert!(
            !client_mod.contains(forbidden),
            "FactorioClient tick control should not embed direct Lua {forbidden:?}"
        );
    }
    for forbidden in [
        "let lua = LuaCommand::get_tick();",
        "let lua = LuaCommand::set_tick_paused(true);",
        "let lua = LuaCommand::set_tick_paused(false);",
        "let lua = LuaCommand::set_game_speed(speed);",
    ] {
        assert!(
            !client_mod.contains(forbidden),
            "FactorioClient tick control should use the /claude remote primitive, not generated Lua {forbidden:?}"
        );
    }
    for required in [
        r#"self.call_remote("get_tick", &[])"#,
        r#"self.call_remote("set_tick_paused", &[json!(true)])"#,
        r#"self.call_remote("set_tick_paused", &[json!(false)])"#,
        r#"self.call_remote("set_game_speed", &[json!(speed)])"#,
    ] {
        assert!(
            client_mod.contains(required),
            "FactorioClient tick control should call /claude directly with typed JSON args {required:?}"
        );
    }
    assert!(
        client_mod.contains("use serde_json::{json, Value};"),
        "FactorioClient get_tick should use the /claude remote primitive, not generated Lua"
    );

    let control_lua = include_str!("../mod/claude-interface/control.lua");
    for required in [
        "local function get_tick_impl",
        "return {tick = game.tick}",
        "local function set_tick_paused_impl",
        "game.tick_paused = paused and true or false",
        "local function set_game_speed_impl",
        "game.speed = tonumber(speed) or game.speed",
        "get_tick = function()",
        "set_tick_paused = function(paused)",
        "set_game_speed = function(speed)",
    ] {
        assert!(
            control_lua.contains(required),
            "control.lua tick-control remotes should include {required:?}"
        );
    }
}

#[test]
fn rust_production_paths_do_not_use_generated_lua_remotes() {
    for (path, source) in [
        ("src/client/mod.rs", include_str!("../src/client/mod.rs")),
        ("src/bin/mcp.rs", include_str!("../src/bin/mcp.rs")),
        ("src/cli/say.rs", include_str!("../src/cli/say.rs")),
        (
            "src/cli/research.rs",
            include_str!("../src/cli/research.rs"),
        ),
        ("src/cli/power.rs", include_str!("../src/cli/power.rs")),
        ("src/cli/map.rs", include_str!("../src/cli/map.rs")),
    ] {
        assert!(
            !source.contains("LuaCommand::"),
            "{path} normal remote path should not use generated Lua builders"
        );
        assert!(
            !source.contains("execute_lua(&lua)")
                && !source.contains("execute_lua(&register_lua)")
                && !source.contains("execute_lua(&fetch_lua)")
                && !source.contains("execute_lua(&research_lua)")
                && !source.contains("execute_lua(&find_lua)")
                && !source.contains("execute_lua(&target_lua)")
                && !source.contains("execute_lua(&active_lua)")
                && !source.contains("execute_lua(&clear_lua)"),
            "{path} normal remote path should use call_remote, not execute generated Lua"
        );
    }

    for (path, source, allowed) in [
        (
            "src/cli/exec.rs",
            include_str!("../src/cli/exec.rs"),
            "execute_lua(&cmd.lua)",
        ),
        (
            "src/bin/mcp.rs",
            include_str!("../src/bin/mcp.rs"),
            "execute_lua(&params.lua)",
        ),
    ] {
        assert!(
            source.contains(allowed),
            "{path} should keep only its explicit raw-Lua escape hatch"
        );
    }
}

#[test]
fn entity_mutation_queries_live_in_the_mod_not_rust_strings() {
    for (name, lua, method) in [
        (
            "remove_entity_at",
            LuaCommand::remove_entity_at(&legacy_agent(), pos(24.0, 25.0)),
            "remove_entity_at",
        ),
        (
            "remove_entity",
            LuaCommand::remove_entity(&legacy_agent(), 43),
            "remove_entity",
        ),
        (
            "rotate_entity",
            LuaCommand::rotate_entity(44, 4),
            "rotate_entity",
        ),
        (
            "insert_items",
            LuaCommand::insert_items(&legacy_agent(), 45, "coal", 5, "fuel"),
            "insert_items",
        ),
        (
            "extract_items",
            LuaCommand::extract_items(&named_agent(), 46, "iron-ore", 6, "chest"),
            "extract_items",
        ),
        (
            "set_recipe",
            LuaCommand::set_recipe(47, "copper-cable"),
            "set_recipe",
        ),
    ] {
        assert_remote_request(name, &lua, method);

        for forbidden in [
            "storage.factorioctl_entities[",
            "find_entities_filtered",
            "get_inventory(",
            "get_main_inventory()",
            "inv.insert",
            "inv.remove",
            "set_recipe(",
            "e.destroy()",
            "e.direction",
        ] {
            assert!(
                !lua.contains(forbidden),
                "{name} Rust wrapper should not embed heavy Lua {forbidden:?}:\n{lua}"
            );
        }
    }

    let client_mod = include_str!("../src/client/mod.rs");
    for forbidden in [
        "let lua = LuaCommand::init_character(&self.agent_id, x, y);",
        "let lua = LuaCommand::teleport_character(&self.agent_id, position);",
        "let lua = LuaCommand::walk_character(&self.agent_id, position);",
        "let lua = LuaCommand::character_status(&self.agent_id);",
        "let lua = LuaCommand::character_inventory(&self.agent_id);",
        "let lua = LuaCommand::can_stand_at(&self.agent_id, position, radius);",
        "let lua = LuaCommand::is_player_blocked(&self.agent_id, radius);",
        "let lua = LuaCommand::unstuck(&self.agent_id, radius, dry_run);",
        "let lua = LuaCommand::get_character_position(&self.agent_id);",
        "let lua = LuaCommand::craft(&self.agent_id, recipe, count);",
        "let lua = LuaCommand::wait_for_crafting(&self.agent_id);",
    ] {
        assert!(
            !client_mod.contains(forbidden),
            "FactorioClient character/crafting methods should call /claude directly, not generated Lua {forbidden:?}"
        );
    }
    for required in [
        r#""init_character""#,
        r#""teleport_character""#,
        r#""set_walk_target""#,
        r#""character_status""#,
        r#""character_inventory""#,
        r#""can_stand_at""#,
        r#""is_player_blocked""#,
        r#""unstuck""#,
        r#""get_character_pos""#,
        r#""craft""#,
        r#""wait_for_crafting""#,
    ] {
        assert!(
            client_mod.contains(required),
            "FactorioClient character/crafting methods should retain direct /claude marker {required:?}"
        );
    }

    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let characters_lua = include_str!("../mod/claude-interface/characters.lua");
    let json_response_lua = include_str!("../mod/claude-interface/json_response.lua");
    let inventory_lua = include_str!("../mod/claude-interface/inventory.lua");
    for required in [
        "local find_factorioctl_character = characters.find",
        "local function remove_entity_at_impl",
        "local function remove_entity_impl",
        "local function insert_items_impl",
        "local function extract_items_impl",
        "local function set_recipe_impl",
        "remove_entity_at = function(agent_id, x, y)",
        "remove_entity = function(agent_id, unit_number)",
        "rotate_entity = function(agent_id, unit_number, direction)",
        "insert_items = function(agent_id, unit_number, item, count, inventory_type)",
        "extract_items = function(agent_id, unit_number, item, count, inventory_type)",
        "set_recipe = function(agent_id, unit_number, recipe)",
    ] {
        assert!(
            control_lua.contains(required),
            "control.lua entity mutation remotes should include {required:?}"
        );
    }

    assert!(
        control_lua.contains("local player_inv = character.get_main_inventory()")
            && control_lua.contains("local character = find_factorioctl_character(agent_id)")
            && control_lua.contains("return {extracted = 0, available = available, item = item}")
            && control_lua.contains("local inventory_define_for = inventory.define_for")
            && characters_lua.contains("local character = storage.characters[agent_id]")
            && inventory_lua.contains("function M.define_for(inventory_type, default_type)")
            && json_response_lua
                .contains("if type(result_or_error) == \"string\" then return result_or_error end")
            && !control_lua.contains("\"error\": \"No items of that type in inventory\""),
        "control.lua extraction logic should preserve the named-agent/no-items contract"
    );

    for required in [
        "local function insert_items_impl(agent_id, unit_number, item, count, inventory_type)",
        "local character_inv = character.get_main_inventory()",
        "local available = character_inv.get_item_count(item)",
        "local removed = character_inv.remove{name = item, count = math.min(count, available)}",
        "local inserted = inv.insert{name = item, count = removed}",
        "returned = character_inv.insert{name = item, count = remainder}",
        "if returned ~= remainder then",
    ] {
        assert!(
            control_lua.contains(required),
            "control.lua insertion must conserve the named agent's real inventory via {required:?}"
        );
    }

    let init_lua = LuaCommand::init_character(&named_agent(), 0.0, 0.0);
    assert_remote_request("init_character", &init_lua, "init_character");
    assert_eq!(
        remote_args(&init_lua),
        vec![json!("doug"), json!(0), json!(0)]
    );
    assert!(
        control_lua.contains("local remember_factorioctl_character = characters.remember")
            && control_lua.contains("json_remote_call(\"init_character\", characters.init")
            && characters_lua.contains("function M.remember(agent_id, character)")
            && characters_lua.contains("storage.characters[agent_id] = character")
            && control_lua.contains("init_character = function(agent_id, x, y)"),
        "characters.lua init_character should populate mod character storage through control.lua remotes"
    );
}

#[test]
fn blueprint_commands_use_scratch_stack_without_name_only_restore() {
    let control_lua = include_str!("../mod/claude-interface/control.lua");
    assert!(
        control_lua.contains("local function blueprint_scratch_stack")
            && control_lua.contains("inv.find_empty_stack(\"blueprint\")")
            && control_lua.contains("game.create_inventory(1)")
            && control_lua.contains("slot.set_stack{name = \"blueprint\"}")
            && control_lua.contains("if scratch_temp_inventory then scratch_temp_inventory.destroy() end"),
        "control.lua should prefer an empty player stack and fall back to a temporary scratch inventory"
    );
    assert!(
        !control_lua.contains("local slot = inv[1]") && !control_lua.contains("saved_item"),
        "blueprint scratch handling should not overwrite slot 1 or restore an item by name only"
    );
}

#[test]
fn blueprint_commands_are_agent_scoped_for_cjf_11() {
    for (name, lua, method) in [
        (
            "create_native_blueprint",
            LuaCommand::create_native_blueprint(&named_agent(), area()),
            "create_native_blueprint",
        ),
        (
            "save_blueprint",
            LuaCommand::save_blueprint(&named_agent(), "starter", area()),
            "save_blueprint",
        ),
        (
            "list_blueprints",
            LuaCommand::list_blueprints(),
            "list_blueprints",
        ),
        (
            "get_blueprint",
            LuaCommand::get_blueprint("starter"),
            "get_blueprint",
        ),
        (
            "place_blueprint",
            LuaCommand::place_blueprint(&named_agent(), "starter", pos(26.0, 27.0), 4),
            "place_blueprint",
        ),
        (
            "import_blueprint",
            LuaCommand::import_blueprint(&named_agent(), "0eNq-test", pos(28.0, 29.0), 8),
            "import_blueprint",
        ),
        (
            "delete_blueprint",
            LuaCommand::delete_blueprint("starter"),
            "delete_blueprint",
        ),
    ] {
        assert_remote_request(name, &lua, method);

        for forbidden in [
            "storage.factorioctl_characters",
            "game.get_player(1)",
            "game.surfaces[1]",
            "find_empty_stack",
            "create_blueprint",
            "build_blueprint",
            "storage.blueprints",
        ] {
            assert!(
                !lua.contains(forbidden),
                "{name} Rust wrapper should not embed heavy Lua {forbidden:?}:\n{lua}"
            );
        }
    }

    let control_lua = include_str!("../mod/claude-interface/control.lua");
    for required in [
        "local function create_native_blueprint_impl",
        "local function save_blueprint_impl",
        "local function list_blueprints_impl",
        "local function get_blueprint_impl",
        "local function place_blueprint_impl",
        "local function import_blueprint_impl",
        "local function delete_blueprint_impl",
        "create_native_blueprint = function(agent_id, x1, y1, x2, y2)",
        "save_blueprint = function(agent_id, name, x1, y1, x2, y2)",
        "list_blueprints = function()",
        "get_blueprint = function(name)",
        "place_blueprint = function(agent_id, name, x, y, direction)",
        "import_blueprint = function(agent_id, bp_string, x, y, direction)",
        "delete_blueprint = function(name)",
        "local character = find_factorioctl_character(agent_id)",
        "local inv = character.get_main_inventory()",
        "surface = character.surface",
        "register_blueprint_ghosts(ghosts)",
        "return {success = false, error = \"Invalid or empty blueprint string\"}",
    ] {
        assert!(
            control_lua.contains(required),
            "control.lua blueprint remotes should include {required:?}"
        );
    }
    assert!(
        !control_lua.contains("game.get_player(1)"),
        "blueprint remotes must not hardcode player 1"
    );
}

#[test]
fn chat_fetch_uses_mod_remote_without_level_storage_fallback() {
    let register_lua = LuaCommand::register_chat_handler();
    let lua = LuaCommand::get_and_clear_chat_messages();

    assert_remote_request("chat_capture_status", &register_lua, "chat_capture_status");
    assert!(!register_lua.contains(r#"rcon.print("registered")"#));
    assert_remote_request("get_chat_messages", &lua, "get_chat_messages");
    assert!(!lua.contains("storage.factorioctl_chat"));
    assert!(!lua.contains("handler_registered"));

    let control_lua = include_str!("../mod/claude-interface/control.lua");
    assert!(control_lua.contains("chat_capture_status = function()"));
    assert!(control_lua.contains("registered = true"));
}

#[test]
fn named_walk_poll_loop_exits_when_driver_clears_target() {
    let client_mod = include_str!("../src/client/mod.rs");
    let active_lua = LuaCommand::walk_target_active(&named_agent());

    assert!(
        client_mod.contains(r#"call_remote("has_walk_target""#)
            && client_mod.contains(r#"call_remote("clear_walk_target""#)
            && client_mod.contains("Walk target cleared"),
        "named walk_to should poll the shared driver target and exit when it has been cleared"
    );
    assert_remote_request("walk_target_active", &active_lua, "has_walk_target");
    assert_eq!(remote_args(&active_lua), vec![json!("doug")]);
    assert!(
        !active_lua.contains("storage.factorioctl_walk_targets"),
        "Rust should not keep a fallback walk-target table after the mod backend is required"
    );
}

#[test]
fn mod_walk_target_uses_engine_walking_without_teleporting() {
    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let walk_driver = control_lua
        .split("local WALK_DIRECTION_THRESHOLD")
        .nth(1)
        .and_then(|tail| tail.split("local function update_agent_markers()").next())
        .expect("walk target subsystem should precede marker updates");

    assert!(
        control_lua.contains("local function walk_direction_toward(dx, dy)")
            && walk_driver.contains("c.walking_state = {")
            && walk_driver.contains("walking = true")
            && walk_driver.contains("direction = walk_direction_toward(dx, dy)")
            && walk_driver.contains("tgt.stuck_ticks >= 120")
            && walk_driver.contains("stop_target_walk(agent_id, c)"),
        "walk targets must use tick-driven Factorio walking with arrival, stuck, and stop handling"
    );
    assert!(
        !walk_driver.contains("teleport("),
        "the production walk-target driver must never teleport the NPC"
    );
}

#[test]
fn character_and_crafting_queries_live_in_the_mod_not_rust_strings() {
    for (name, lua, method) in [
        (
            "init_character",
            LuaCommand::init_character(&named_agent(), 0.0, 0.0),
            "init_character",
        ),
        (
            "teleport_character",
            LuaCommand::teleport_character(&named_agent(), pos(10.0, 11.0)),
            "teleport_character",
        ),
        (
            "character_status",
            LuaCommand::character_status(&named_agent()),
            "character_status",
        ),
        (
            "character_inventory",
            LuaCommand::character_inventory(&named_agent()),
            "character_inventory",
        ),
        (
            "can_stand_at",
            LuaCommand::can_stand_at(&named_agent(), pos(10.0, 11.0), 6),
            "can_stand_at",
        ),
        (
            "is_player_blocked",
            LuaCommand::is_player_blocked(&named_agent(), 6),
            "is_player_blocked",
        ),
        (
            "unstuck",
            LuaCommand::unstuck(&named_agent(), 8, false),
            "unstuck",
        ),
        (
            "get_character_position",
            LuaCommand::get_character_position(&named_agent()),
            "get_character_pos",
        ),
        (
            "craft",
            LuaCommand::craft(&named_agent(), "iron-gear-wheel", 4),
            "craft",
        ),
        (
            "wait_for_crafting",
            LuaCommand::wait_for_crafting(&named_agent()),
            "wait_for_crafting",
        ),
    ] {
        assert_remote_request(name, &lua, method);

        for forbidden in [
            "storage.factorioctl_characters",
            "game.connected_players",
            "get_main_inventory()",
            "begin_crafting",
            "prototypes.recipe",
            "crafting_queue",
            "create_entity",
            ".teleport(",
        ] {
            assert!(
                !lua.contains(forbidden),
                "{name} Rust wrapper should not embed heavy Lua {forbidden:?}:\n{lua}"
            );
        }
    }

    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let characters_lua = include_str!("../mod/claude-interface/characters.lua");
    let json_response_lua = include_str!("../mod/claude-interface/json_response.lua");
    for required in [
        "local characters = require(\"characters\")",
        "local remember_factorioctl_character = characters.remember",
        "characters.init",
        "characters.teleport",
        "characters.status",
        "characters.inventory",
        "local function crafting_queue_summary",
        "local function craft_impl",
        "local function wait_for_crafting_impl",
        "init_character = function(agent_id, x, y)",
        "teleport_character = function(agent_id, x, y)",
        "character_status = function(agent_id)",
        "character_inventory = function(agent_id)",
        "can_stand_at = function(agent_id, x, y, radius)",
        "is_player_blocked = function(agent_id, radius)",
        "unstuck = function(agent_id, radius, dry_run)",
        "get_character_pos = function(agent_id)",
        "craft = function(agent_id, recipe_name, count)",
        "wait_for_crafting = function(agent_id)",
    ] {
        assert!(
            control_lua.contains(required),
            "control.lua character/crafting remotes should include {required:?}"
        );
    }
    for moved in [
        "local function init_character_impl",
        "local function teleport_character_impl",
        "local function character_status_impl",
        "local function character_inventory_impl",
        "game.surfaces[1].create_entity{",
        "character.teleport({x, y})",
    ] {
        assert!(
            !control_lua.contains(moved),
            "control.lua should not retain character implementation {moved:?}"
        );
    }

    assert!(
        characters_lua.contains("storage.characters[agent_id] = character")
            && characters_lua.contains("local surface = game.get_surface(\"nauvis\")")
            && characters_lua.contains("character.teleport({x, y})")
            && characters_lua.contains("items = inventory.contents(inv)")
            && characters_lua.contains("return {items = {}, free_slots = 0}")
            && characters_lua.contains("function M.can_stand_at(agent_id, x, y, radius)")
            && characters_lua.contains("function M.is_player_blocked(agent_id, radius)")
            && characters_lua.contains("function M.unstuck(agent_id, radius, dry_run)")
            && characters_lua.contains("local function stand_blockers(character, position)")
            && characters_lua.contains("unstuck_candidates")
            && characters_lua.contains("walk_to_clear_position")
            && characters_lua.contains("local started = M.set_walk_target(agent_id, target.x, target.y)")
            && characters_lua.contains("walking to nearest verified clear standing position")
            && !characters_lua.contains("game.surfaces[1]")
            && control_lua.contains("local c = find_factorioctl_character(agent_id)")
            && control_lua.contains("return character.begin_crafting{recipe = recipe_name, count = count}")
            && control_lua.contains("pairs(character.crafting_queue or {})")
            && control_lua.contains("return tostring(character.crafting_queue_size or 0)")
            && control_lua.contains(
                "Crafting did not start; check ingredients, recipe category, or character craftability"
            )
            && json_response_lua
                .contains("if type(result_or_error) == \"string\" then return result_or_error end"),
        "control.lua should own character/crafting semantics and preserve return contracts"
    );
}

#[test]
fn wedged_debug_probe_combines_collision_diagnostics_and_visual_snapshot() {
    let mcp_rs = include_str!("../src/bin/mcp.rs");
    assert!(
        mcp_rs.contains("async fn debug_wedged_state")
            && mcp_rs.contains("client.is_player_blocked(radius.min(12))")
            && mcp_rs.contains(".can_stand_at(character_position, radius.min(12))")
            && mcp_rs.contains("client.unstuck(radius.min(12), true)")
            && mcp_rs.contains("render_ascii_map_snapshot(")
            && mcp_rs.contains("\"format\": \"ascii_map\"")
            && mcp_rs.contains("\"blocked\": blocked")
            && mcp_rs.contains("\"unstuck_preview\": unstuck_preview"),
        "debug_wedged_state should be a one-call save-state visual/collision probe"
    );
    assert!(
        mcp_rs.contains("Read-only save-state debug probe for stuck NPCs")
            && mcp_rs.contains("appears under an entity")
            && mcp_rs.contains("@ marking the character"),
        "debug_wedged_state tool description should steer agents toward wedged-state diagnosis"
    );
}

#[test]
fn placement_queries_live_in_the_mod_not_rust_strings() {
    for (name, lua, method) in [
        (
            "place_entity",
            LuaCommand::place_entity(
                &named_agent(),
                "steam-engine",
                pos(-37.0, 37.0),
                Direction::East,
            ),
            "place_entity",
        ),
        (
            "check_entity_placement",
            LuaCommand::check_entity_placement(
                &named_agent(),
                "offshore-pump",
                pos(-39.0, 37.0),
                Direction::West,
            ),
            "check_entity_placement",
        ),
        (
            "find_entity_placements",
            LuaCommand::find_entity_placements(
                &named_agent(),
                "offshore-pump",
                pos(-39.0, 37.0),
                10,
                20,
            ),
            "find_entity_placements",
        ),
        (
            "plan_entity_placement_near",
            LuaCommand::plan_entity_placement_near(
                &named_agent(),
                "steam-engine",
                pos(-37.0, 37.0),
                8,
                10,
            ),
            "plan_entity_placement_near",
        ),
        (
            "build_edge_miner",
            LuaCommand::build_edge_miner(
                &named_agent(),
                "iron-ore",
                pos(57.0, -22.0),
                25,
                "burner-mining-drill",
                10,
            ),
            "build_edge_miner",
        ),
        (
            "build_direct_smelter",
            LuaCommand::build_direct_smelter(
                &named_agent(),
                None,
                Some((pos(56.0, -18.0), Direction::South)),
                "stone-furnace",
                "burner-inserter",
                "transport-belt",
                6,
            ),
            "build_direct_smelter",
        ),
        (
            "place_ghost",
            LuaCommand::place_ghost(
                &named_agent(),
                "stone-furnace",
                pos(22.0, 23.0),
                Direction::West,
            ),
            "place_ghost",
        ),
        (
            "place_underground_belt",
            LuaCommand::place_underground_belt(
                &named_agent(),
                "underground-belt",
                pos(20.0, 21.0),
                Direction::South,
                "output",
            ),
            "place_underground_belt",
        ),
    ] {
        assert_remote_request(name, &lua, method);

        for forbidden in [
            "storage.factorioctl_characters",
            "game.connected_players",
            "get_main_inventory()",
            "find_entities_filtered",
            "can_place_entity",
            "create_entity",
            "create_entity returned nil after can_place_entity succeeded",
            "table.sort(placements",
            "build_check_type",
        ] {
            assert!(
                !lua.contains(forbidden),
                "{name} Rust wrapper should not embed heavy Lua {forbidden:?}:\n{lua}"
            );
        }
    }

    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let placement_lua = include_str!("../mod/claude-interface/placement.lua");
    for required in [
        "local placement = require(\"placement\")",
        "placement.place_entity",
        "placement.place_underground_belt",
        "placement.check_entity_placement",
        "placement.find_entity_placements",
        "placement.plan_entity_placement_near",
        "placement.build_edge_miner",
        "placement.build_direct_smelter",
        "placement.place_ghost",
        "placement.rotate_entity",
        "place_entity = function(agent_id, entity_name, x, y, direction)",
        "place_underground_belt = function(agent_id, entity_name, x, y, direction, belt_type)",
        "check_entity_placement = function(agent_id, entity_name, x, y, direction)",
        "find_entity_placements = function(agent_id, entity_name, center_x, center_y, radius, limit)",
        "plan_entity_placement_near = function(agent_id, entity_name, target_x, target_y, radius, limit)",
        "build_edge_miner = function(agent_id, resource_name, center_x, center_y, radius, drill_name, limit)",
        "build_direct_smelter = function(agent_id, drill_unit_number, output_x, output_y, output_direction, furnace_name, inserter_name, belt_name, radius)",
        "place_ghost = function(agent_id, entity_name, x, y, direction)",
        "rotate_entity = function(agent_id, unit_number, direction)",
    ] {
        assert!(
            control_lua.contains(required),
            "control.lua placement remotes should include {required:?}"
        );
    }
    for moved in [
        "local function placement_entity_result",
        "local function placement_failure",
        "local function clear_ground_items_for_placement",
        "local function place_entity_impl",
        "local function place_underground_belt_impl",
        "local function check_entity_placement_impl",
        "local function find_entity_placements_impl",
        "local function build_edge_miner_impl",
        "local function build_direct_smelter_impl",
        "local function place_ghost_impl",
        "local function rotate_entity_impl",
    ] {
        assert!(
            !control_lua.contains(moved),
            "control.lua should not retain placement implementation {moved:?}"
        );
    }

    assert!(
        placement_lua.contains("surface.can_place_entity{")
            && placement_lua.contains("surface.create_entity{")
            && placement_lua
                .contains("create_entity returned nil after can_place_entity succeeded")
            && placement_lua.contains("local function inspect_placement_blockers")
            && placement_lua.contains("occupied_by = blockers[1]")
            && placement_lua.contains("bounding_box = bounding_box_table(entity)")
            && placement_lua.contains("details.recommended_action = \"route_belt\"")
            && placement_lua.contains("do not place an unrelated nearby belt")
            && placement_lua.contains("local function character_placement_blocker")
            && placement_lua.contains("character_overlap = true")
            && placement_lua.contains("walk_to_clear_placement")
            && placement_lua.contains("local function character_standing_area")
            && placement_lua.contains("post_placement = {")
            && placement_lua.contains("has_clear_standing_position")
            && placement_lua.contains("would_trap_agent")
            && placement_lua.contains("nearest_clear_standing_position")
            && placement_lua.contains("local function placement_candidate")
            && placement_lua.contains("function M.plan_entity_placement_near(agent_id, entity_name, target_x, target_y, radius, limit)")
            && placement_lua.contains("avoids_character = character_blocker == nil")
            && placement_lua.contains("can_place_and_keep_working")
            && placement_lua.contains("selected.footprint")
            && placement_lua.contains("selected.post_placement.nearest_clear_standing_position")
            && placement_lua.contains("selected.can_place_and_keep_working")
            && placement_lua.contains("output_blocked = output.belt_can_place ~= true")
            && placement_lua.contains("output_usable = output.output_clear == true")
            && placement_lua.contains("expand_radius_or_use_edge_planner")
            && placement_lua.contains("execute_place_entity_step")
            && placement_lua.contains("recommended_action = \"rotate_entity\"")
            && placement_lua.contains("create_entity_nil_after_can_place = true")
            && placement_lua.contains("local function mining_drill_output_diagnostics")
            && placement_lua.contains("local function mining_drill_output_tile")
            && placement_lua.contains("output_buildable = output.belt_can_place")
            && placement_lua.contains("output_clear = output.output_clear")
            && placement_lua.contains("Drill output tile overlaps resource")
            && placement_lua.contains("function M.build_edge_miner(agent_id, resource_name, center_x, center_y, radius, drill_name, limit)")
            && placement_lua.contains("no_clear_output_tile")
            && placement_lua.contains("execute_edge_miner_steps")
            && placement_lua.contains("count_matching_resources(surface, resource_name, drill_area)")
            && placement_lua.contains("selected.output.belt_tile")
            && placement_lua.contains("function M.build_direct_smelter(agent_id, drill_unit_number, output_x, output_y, output_direction, furnace_name, inserter_name, belt_name, radius)")
            && placement_lua.contains("missing_output_reference")
            && placement_lua.contains("no_direct_smelter_layout")
            && placement_lua.contains("execute_direct_smelter_steps")
            && placement_lua.contains("selected.input_inserter")
            && placement_lua.contains("verify_step")
            && placement_lua.contains("inventory_count = inv.get_item_count(entity_name)")
            && placement_lua.contains("item_in_inventory = inventory_count > 0")
            && placement_lua.contains("type = belt_type")
            && placement_lua.contains("result.belt_to_ground_type = entity.belt_to_ground_type")
            && placement_lua.contains("function M.rotate_entity(agent_id, unit_number, direction)")
            && placement_lua.contains("result.previous_direction = previous_direction")
            && placement_lua.contains("requested_direction = direction")
            && placement_lua.contains("table.sort(placements")
            && !control_lua.contains("and nil or"),
        "placement.lua should own placement diagnostics, scans, and create_entity contracts"
    );
    assert!(
        !placement_lua.contains("belt_alternate_candidates")
            && !placement_lua.contains("alternate_belt_placements")
            && !placement_lua.contains("candidate_alternate_path"),
        "blocked belt diagnostics must not suggest disconnected one-tile detours"
    );
}

#[test]
fn build_helpers_live_in_the_mod_not_rust_strings() {
    for (name, lua, method) in [
        (
            "build_drill_array",
            LuaCommand::build_drill_array(
                &named_agent(),
                2,
                "iron-ore",
                Some((-37.0, 37.0)),
                "burner-mining-drill",
                "south",
            ),
            "build_drill_array",
        ),
        (
            "build_smelter_line",
            LuaCommand::build_smelter_line(
                &named_agent(),
                3,
                (-25.0, 50.0),
                "stone-furnace",
                "east",
                3,
            ),
            "build_smelter_line",
        ),
    ] {
        assert_remote_request(name, &lua, method);

        for forbidden in [
            "storage.factorioctl_characters",
            "game.connected_players",
            "get_main_inventory()",
            "find_entities_filtered",
            "can_place_entity",
            "create_entity",
            "storage.factorioctl_entities",
            "table.sort(resources",
        ] {
            assert!(
                !lua.contains(forbidden),
                "{name} Rust wrapper should not embed heavy Lua {forbidden:?}:\n{lua}"
            );
        }
    }

    let control_lua = include_str!("../mod/claude-interface/control.lua");
    for required in [
        "local function build_entity_result",
        "local function direction_from_name",
        "local function build_result",
        "local function build_drill_array_impl",
        "local function smelter_line_delta",
        "local function build_smelter_line_impl",
        "build_drill_array = function(agent_id, count, resource, near_x, near_y, drill_type, direction_name)",
        "build_smelter_line = function(agent_id, count, start_x, start_y, furnace_type, line_direction, spacing)",
        "surface.find_entities_filtered{",
        "surface.can_place_entity{",
        "surface.create_entity{",
        "inv.get_item_count(drill_type)",
        "inv.get_item_count(furnace_type)",
        "storage.factorioctl_entities[entity.unit_number] = entity",
        "smelter_line_delta(line_direction, spacing)",
        "direction_from_name(direction_name, defines.direction.south)",
    ] {
        assert!(
            control_lua.contains(required),
            "control.lua build-helper remotes should include {required:?}"
        );
    }
}

#[test]
fn steam_power_diagnostic_uses_mod_remote_not_inline_lua() {
    let lua = LuaCommand::diagnose_steam_power(-25, 50, 20);

    assert_remote_request("diagnose_steam_power", &lua, "diagnose_steam_power");
    assert_eq!(remote_args(&lua), vec![json!(-25), json!(50), json!(20)]);
    for forbidden in [
        "get_fluid_box_neighbours",
        "get_fluid_box_pipe_connections",
        "has_fluid_segment",
        "boiler_steam_output_blocked",
    ] {
        assert!(
            !lua.contains(forbidden),
            "diagnose_steam_power Rust wrapper should not embed heavy Lua {forbidden:?}:\n{lua}"
        );
    }
}

#[test]
fn steam_power_planner_uses_mod_remote_not_inline_lua() {
    let lua = LuaCommand::plan_steam_power(
        &named_agent(),
        Area::new(-40.0, 37.0, -30.0, 57.0),
        pos(55.0, -2.0),
    );

    assert_remote_request("plan_steam_power", &lua, "plan_steam_power");
    assert_eq!(
        remote_args(&lua),
        vec![
            json!("doug"),
            json!(-40),
            json!(37),
            json!(-30),
            json!(57),
            json!(55),
            json!(-2),
        ]
    );
    for forbidden in [
        "surface.can_place_entity",
        "offshore-pump",
        "steam-engine",
        "fuel_target",
    ] {
        assert!(
            !lua.contains(forbidden),
            "plan_steam_power Rust wrapper should not embed planner Lua {forbidden:?}:\n{lua}"
        );
    }
}

#[test]
fn steam_power_repair_uses_mod_remote_not_inline_lua() {
    let lua = LuaCommand::repair_steam_power(&named_agent(), -25, 50, 20, pos(55.0, -2.0));

    assert_remote_request("repair_steam_power", &lua, "repair_steam_power");
    assert_eq!(
        remote_args(&lua),
        vec![
            json!("doug"),
            json!(-25),
            json!(50),
            json!(20),
            json!(55),
            json!(-2)
        ]
    );
    for forbidden in [
        "surface.find_entities_filtered",
        "repair_steps",
        "boiler_no_fuel",
        "steam_engine_pole_route_incomplete",
    ] {
        assert!(
            !lua.contains(forbidden),
            "repair_steam_power Rust wrapper should not embed repair Lua {forbidden:?}:\n{lua}"
        );
    }
}

#[test]
fn power_extension_uses_mod_remote_not_inline_lua() {
    let lua = LuaCommand::extend_power_to(&named_agent(), 0, 0, 20, pos(2.0, 0.0));

    assert_remote_request("extend_power_to", &lua, "extend_power_to");
    assert_eq!(
        remote_args(&lua),
        vec![
            json!("doug"),
            json!(0),
            json!(0),
            json!(20),
            json!(2),
            json!(0)
        ]
    );
    for forbidden in [
        "surface.find_entities_filtered",
        "pole_repair_path",
        "no_power_grid_found",
        "place_power_pole",
    ] {
        assert!(
            !lua.contains(forbidden),
            "extend_power_to Rust wrapper should not embed planner Lua {forbidden:?}:\n{lua}"
        );
    }
}

#[test]
fn steam_power_planner_lives_in_power_module() {
    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let power_lua = include_str!("../mod/claude-interface/power.lua");

    assert!(
        control_lua.contains(r#"local power = require("power")"#)
            && control_lua.contains("local function plan_steam_power_impl")
            && control_lua.contains(
                r#"json_remote_call("plan_steam_power", plan_steam_power_impl, agent_id, water_x1, water_y1, water_x2, water_y2, target_x, target_y)"#
            ),
        "control.lua should expose the steam-power planner through the power module"
    );

    for required in [
        "function M.plan_steam_power(character, water_x1, water_y1, water_x2, water_y2, target_x, target_y)",
        "local BOILER_FLUID_LAYOUTS",
        "local pipe_center = pos(pipe_pos.x + 0.5, pipe_pos.y + 0.5)",
        "steam_target.x - fluid_layout.engine_input.x",
        "local function find_machine_connection_pole",
        "local first_pole = find_machine_connection_pole(surface, force, from_pos)",
        "local function opposite_direction",
        "local function validate_cumulative_placement",
        "local function planned_collision_box",
        "local function collision_boxes_overlap",
        "local land_dir = opposite_direction(pump.dir)",
        "candidate_layouts(pump, land_dir)",
        "surface.can_place_entity",
        "offshore-pump",
        "boiler",
        "steam-engine",
        "small-electric-pole",
        "fuel_target",
        "build_steps",
        "placement_success",
        "missing_items",
        "blockers",
    ] {
        assert!(
            power_lua.contains(required),
            "power.lua steam planner should include {required:?}"
        );
    }
    assert!(
        !power_lua.contains("surface.create_entity{")
            && !power_lua.contains("cleanup_simulated_entities"),
        "steam-power planning must validate geometry without temporarily mutating the live surface"
    );
}

#[test]
fn power_diagnostics_use_mod_remote_not_inline_lua() {
    for (name, lua, method) in [
        (
            "get_power_status",
            LuaCommand::get_power_status(30, 31, 10),
            "get_power_status",
        ),
        (
            "get_power_networks",
            LuaCommand::get_power_networks(32, 33, 11),
            "get_power_networks",
        ),
        (
            "find_power_issues",
            LuaCommand::find_power_issues(34, 35, 12),
            "find_power_issues",
        ),
        (
            "get_power_coverage",
            LuaCommand::get_power_coverage(36, 37, 13),
            "get_power_coverage",
        ),
        (
            "get_alerts",
            LuaCommand::get_alerts(38, 39, 14),
            "get_alerts",
        ),
    ] {
        assert_remote_request(name, &lua, method);
        for forbidden in [
            "surface.find_entities_filtered",
            "electric-pole",
            "POWER_CONSUMER_TYPES",
            "entity_status.no_power",
            "No electric poles found in area",
        ] {
            assert!(
                !lua.contains(forbidden),
                "{name} Rust wrapper should not embed heavy Lua {forbidden:?}:\n{lua}"
            );
        }
    }
}

#[test]
fn steam_power_repair_lives_in_power_module() {
    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let power_lua = include_str!("../mod/claude-interface/power.lua");

    assert!(
        control_lua.contains(r#"local power = require("power")"#)
            && control_lua.contains("local function repair_steam_power_impl")
            && control_lua.contains(
                r#"json_remote_call("repair_steam_power", repair_steam_power_impl, agent_id, x, y, radius, target_x, target_y)"#
            ),
        "control.lua should expose the steam-power repair planner through the power module"
    );

    for required in [
        "function M.repair_steam_power(character, x, y, radius, target_x, target_y)",
        "local diagnostic = M.diagnose_steam_power(character, x, y, r)",
        "dry_run = true",
        "repair_steps = {}",
        "missing_items = {}",
        "append_repair_step(",
        "durable_boiler_fuel_required",
        "\"place_entity\"",
        "steam_engine_no_steam_may_clear_after_fuel",
        "manual_inspection_required",
    ] {
        assert!(
            power_lua.contains(required),
            "power.lua steam repair planner should include {required:?}"
        );
    }
}

#[test]
fn power_extension_lives_in_power_module() {
    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let power_lua = include_str!("../mod/claude-interface/power.lua");

    assert!(
        control_lua.contains(r#"local power = require("power")"#)
            && control_lua.contains("local function extend_power_to_impl")
            && control_lua.contains(
                r#"json_remote_call("extend_power_to", extend_power_to_impl, agent_id, x, y, radius, target_x, target_y)"#
            ),
        "control.lua should expose the power-extension planner through the power module"
    );

    for required in [
        "function M.extend_power_to(character, x, y, radius, target_x, target_y)",
        "dry_run = true",
        "no_power_grid_found",
        "pole_supply_reaches(",
        "pole_repair_path(surface, force, nearest.position, target)",
        "execute_power_extension_steps",
        "small-electric-pole",
    ] {
        assert!(
            power_lua.contains(required),
            "power.lua power-extension planner should include {required:?}"
        );
    }
}

#[test]
fn steam_power_diagnostic_lives_in_mod_remote_interface() {
    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let power_lua = include_str!("../mod/claude-interface/power.lua");

    assert!(
        control_lua.contains(r#"local power = require("power")"#)
            && control_lua.contains("diagnose_steam_power = function(x, y, radius, agent_id)")
            && control_lua.contains(
                r#"json_remote_call("diagnose_steam_power", power.diagnose_steam_power, scoped_character(agent_id), x, y, radius)"#
            ),
        "claude-interface control.lua should expose the steam diagnostic remote"
    );
    assert!(
        !control_lua.contains("local function diagnose_steam_power_impl"),
        "control.lua should not own the steam diagnostic implementation"
    );

    for required in [
        "get_fluid_capacity",
        "get_fluid_filter",
        "get_fluid(",
        "has_fluid_segment",
        "get_fluid_segment_id",
        "get_fluid_segment_fluid",
        "get_fluid_segment_capacity",
        "get_fluid_segment_extent_bounding_box",
        "get_fluid_box_neighbours",
        "get_fluid_box_pipe_connections",
        "finish_steam_diagnostic",
        "fluidbox_has_neighbour",
        "existing_steam_power_found",
        "has_existing_plant",
        "boiler_steam_output_blocked",
        "boiler_water_alignment_mismatch",
        "steam_engine_no_steam",
        "steam_engine_alignment_mismatch",
        "steam_engine_not_on_grid",
        "steam_engine_pole_route_incomplete",
        "offshore-pump",
        "boiler",
        "steam-engine",
    ] {
        assert!(
            power_lua.contains(required),
            "power.lua steam diagnostic should include {required:?}"
        );
    }
}

#[test]
fn power_diagnostics_live_in_mod_remote_interface() {
    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let power_lua = include_str!("../mod/claude-interface/power.lua");

    for required in [
        "get_power_status = function(x, y, radius, agent_id)",
        "get_power_networks = function(x, y, radius, agent_id)",
        "find_power_issues = function(x, y, radius, agent_id)",
        "get_power_coverage = function(x, y, radius, agent_id)",
        "get_alerts = function(x, y, radius, agent_id)",
        "json_remote_call",
    ] {
        assert!(
            control_lua.contains(required),
            "control.lua power remotes should include {required:?}"
        );
    }

    for forbidden in [
        "local function get_power_status_impl",
        "local function get_power_networks_impl",
        "local function find_power_issues_impl",
        "local function get_power_coverage_impl",
        "local function get_alerts_impl",
        "POWER_CONSUMER_TYPES",
        "POLE_SUPPLY_AREAS",
    ] {
        assert!(
            !control_lua.contains(forbidden),
            "control.lua should not own power diagnostic implementation detail {forbidden:?}"
        );
    }

    for required in [
        "function M.get_power_status(character, x, y, radius)",
        "function M.get_power_networks(character, x, y, radius)",
        "function M.find_power_issues(character, x, y, radius)",
        "function M.get_power_coverage(character, x, y, radius)",
        "function M.get_alerts(character, x, y, radius)",
        "POWER_CONSUMER_TYPES",
        "POLE_SUPPLY_AREAS",
    ] {
        assert!(
            power_lua.contains(required),
            "power.lua power diagnostics should include {required:?}"
        );
    }
}

#[test]
fn mining_queries_live_in_the_mod_not_rust_strings() {
    for (name, lua, method) in [
        (
            "start_mining",
            LuaCommand::start_mining(&named_agent(), pos(14.0, 15.0)),
            "start_mining",
        ),
        (
            "stop_mining",
            LuaCommand::stop_mining(&named_agent()),
            "stop_mining",
        ),
        (
            "get_mining_status",
            LuaCommand::get_mining_status(&named_agent()),
            "get_mining_status",
        ),
        (
            "mine_at",
            LuaCommand::mine_at(&named_agent(), pos(16.0, 17.0), 2),
            "mine_at",
        ),
        (
            "find_nearest_minable",
            LuaCommand::find_nearest_minable(&named_agent(), "iron-ore", 100),
            "find_nearest_minable",
        ),
        (
            "mine_nearest",
            LuaCommand::mine_nearest(&named_agent(), "iron-ore", 3),
            "mine_nearest",
        ),
        (
            "clear_area",
            LuaCommand::clear_area(&named_agent(), area(), true, true, false),
            "clear_area",
        ),
    ] {
        assert_remote_request(name, &lua, method);

        for forbidden in [
            "storage.factorioctl_characters",
            "game.connected_players",
            "find_entities_filtered",
            "get_main_inventory()",
            "mine_entity",
            "mining_state",
            "resource_reach_distance",
        ] {
            assert!(
                !lua.contains(forbidden),
                "{name} Rust wrapper should not embed heavy Lua {forbidden:?}:\n{lua}"
            );
        }
    }

    let client_mod = include_str!("../src/client/mod.rs");
    for forbidden in [
        "let lua = LuaCommand::mine_at(&self.agent_id, position, count);",
        "let find_lua = LuaCommand::find_nearest_minable(&self.agent_id, entity_type, 100);",
    ] {
        assert!(
            !client_mod.contains(forbidden),
            "FactorioClient mining methods should call /claude directly, not generated Lua {forbidden:?}"
        );
    }
    for required in [r#""mine_at""#, r#""find_nearest_minable""#] {
        assert!(
            client_mod.contains(required),
            "FactorioClient mining methods should retain direct /claude marker {required:?}"
        );
    }
    let mine_at_client = client_mod
        .split("pub async fn mine_at")
        .nth(1)
        .and_then(|tail| tail.split("pub async fn mine_nearest").next())
        .expect("mine_at should exist before mine_nearest");
    assert!(
        mine_at_client.contains("json!(0.5)") && !mine_at_client.contains("json!(3)"),
        "FactorioClient::mine_at must point-target rather than search nearby infrastructure"
    );

    let control_lua = include_str!("../mod/claude-interface/control.lua");
    for required in [
        "local function inventory_item_total",
        "local function find_minable_at",
        "local function start_mining_impl",
        "local function stop_mining_impl",
        "local function get_mining_status_impl",
        "local function mine_at_impl",
        "local function find_nearest_minable_impl",
        "local function mine_nearest_impl",
        "local function clear_area_impl",
        "start_mining = function(agent_id, x, y)",
        "stop_mining = function(agent_id)",
        "get_mining_status = function(agent_id)",
        "mine_at = function(agent_id, x, y, count, radius)",
        "find_nearest_minable = function(agent_id, entity_name, radius)",
        "mine_nearest = function(agent_id, entity_name, count)",
        "clear_area = function(agent_id, x1, y1, x2, y2, clear_trees, clear_rocks, dry_run)",
    ] {
        assert!(
            control_lua.contains(required),
            "control.lua mining remotes should include {required:?}"
        );
    }

    assert!(
        control_lua.contains("character.mine_entity(target, true)")
            && control_lua
                .contains("character.mining_state = {mining = true, position = target.position}")
            && control_lua.contains("items = inventory_contents(inv)")
            && control_lua.contains("local iteration_before_count = inventory_item_total(inv)")
            && control_lua.contains("local inventory_progress = iteration_after_count > iteration_before_count")
            && control_lua.contains("local resource_progress = target.valid and target_amount_before")
            && control_lua.contains("picked_up = picked_up + pick_up_item_entity")
            && control_lua.contains("local trees = clear_trees and surface.find_entities_filtered{type = \"tree\", area = area} or {}")
            && control_lua.contains("surface.find_entities_filtered{type = \"simple-entity\", area = area}")
            && control_lua.contains("find_entities_filtered{"),
        "control.lua should own mining scans and measure mining progress from state changes"
    );
    let find_minable_at = control_lua
        .split("local function find_minable_at")
        .nth(1)
        .and_then(|tail| tail.split("local function mining_failure").next())
        .expect("find_minable_at should exist before mining_failure");
    assert!(
        find_minable_at.contains("type = \"resource\"")
            && find_minable_at.contains("type = {\"tree\", \"simple-entity\", \"fish\"}"),
        "find_minable_at must exclude placed infrastructure entity types"
    );
    let mine_at_impl = control_lua
        .split("local function mine_at_impl")
        .nth(1)
        .and_then(|tail| {
            tail.split("local function find_nearest_minable_impl")
                .next()
        })
        .expect("mine_at_impl should exist before find_nearest_minable_impl");
    assert!(
        !mine_at_impl.contains("if character.mine_entity(target, true) then")
            && mine_at_impl
                .contains("local search_radius = math.min(math.max(radius or 0.5, 0), 0.5)"),
        "mine_at_impl must point-target and measure actual mining progress"
    );
}

#[test]
fn gather_resource_reuses_mining_remotes_not_inline_resource_scans() {
    let client_mod = include_str!("../src/client/mod.rs");
    assert!(
        client_mod.contains(r#"call_remote("#)
            && client_mod.contains(r#""find_nearest_minable""#)
            && client_mod.contains("let mine_result = self.mine_at(target_pos, 1).await?")
            && client_mod.contains("let inv_result = self.character_inventory().await?"),
        "gather_resource should compose existing remote-backed mining and inventory helpers"
    );

    for forbidden in [
        "resource_name_lua",
        "rcon.print(\"mined\")",
        "rcon.print(\"none\")",
        "c.mine_entity(resources[1], true)",
        "local resources = game.surfaces[1].find_entities_filtered",
        "local inv = c.get_main_inventory()",
    ] {
        assert!(
            !client_mod.contains(forbidden),
            "gather_resource should not reintroduce inline Lua snippet {forbidden:?}"
        );
    }
}

#[test]
fn recipe_prototype_blueprint_and_research_snapshots_are_stable() {
    for (name, lua, method) in [
        (
            "get_recipe",
            LuaCommand::get_recipe("iron-plate"),
            "get_recipe",
        ),
        (
            "get_recipes_by_category",
            LuaCommand::get_recipes_by_category("crafting"),
            "get_recipes_by_category",
        ),
        (
            "get_recipes_for_item",
            LuaCommand::get_recipes_for_item("transport-belt"),
            "get_recipes_for_item",
        ),
        (
            "get_prototype",
            LuaCommand::get_prototype("assembling-machine-1"),
            "get_prototype",
        ),
    ] {
        assert_remote_request(name, &lua, method);
        for forbidden in [
            "prototypes.recipe",
            "prototypes.entity",
            "recipe_unlocks",
            "recipe.ingredients",
            "recipe.products",
            "try_get",
        ] {
            assert!(
                !lua.contains(forbidden),
                "{name} Rust wrapper should not embed heavy Lua {forbidden:?}:\n{lua}"
            );
        }
    }

    for (name, lua, method) in [
        (
            "get_research_status",
            LuaCommand::get_research_status(),
            "get_research_status",
        ),
        (
            "get_available_research",
            LuaCommand::get_available_research(&named_agent()),
            "get_available_research",
        ),
        (
            "feed_lab_from_inventory",
            LuaCommand::feed_lab_from_inventory(
                &named_agent(),
                42,
                "automation-science-pack",
                5,
                true,
            ),
            "feed_lab_from_inventory",
        ),
        (
            "start_research",
            LuaCommand::start_research("automation"),
            "start_research",
        ),
        (
            "is_tech_researched",
            LuaCommand::is_tech_researched("automation"),
            "is_tech_researched",
        ),
    ] {
        assert_remote_request(name, &lua, method);
        for forbidden in [
            "force.technologies",
            "find_entities_filtered",
            "research_unit_ingredients",
            "force.add_research",
            "lab_input",
        ] {
            assert!(
                !lua.contains(forbidden),
                "{name} Rust wrapper should not embed heavy Lua {forbidden:?}:\n{lua}"
            );
        }
    }

    let client_mod = include_str!("../src/client/mod.rs");
    for forbidden in [
        "let lua = LuaCommand::get_recipe(name);",
        "let lua = LuaCommand::get_recipes_by_category(category);",
        "let lua = LuaCommand::get_recipes_for_item(item);",
        "let lua = LuaCommand::get_prototype(name);",
    ] {
        assert!(
            !client_mod.contains(forbidden),
            "FactorioClient recipe/prototype queries should call /claude directly, not generated Lua {forbidden:?}"
        );
    }
    for required in [
        r#"self.call_remote("get_recipe", &[json!(name)])"#,
        r#"call_remote("get_recipes_by_category", &[json!(category)])"#,
        r#"call_remote("get_recipes_for_item", &[json!(item)])"#,
        r#"self.call_remote("get_prototype", &[json!(name)])"#,
    ] {
        assert!(
            client_mod.contains(required),
            "FactorioClient recipe/prototype queries should retain direct /claude call marker {required:?}"
        );
    }

    let control_lua = include_str!("../mod/claude-interface/control.lua");
    let recipes_lua = include_str!("../mod/claude-interface/recipes.lua");
    let research_lua = include_str!("../mod/claude-interface/research.lua");
    for required in [
        "local function recipe_unlocks",
        "local function recipe_ingredients",
        "local function recipe_products",
        "local function recipe_summary",
        "local function recipe_details",
        "function M.get_recipe(name)",
        "function M.get_recipes_by_category(category)",
        "function M.get_recipes_for_item(item)",
        "return M",
    ] {
        assert!(
            recipes_lua.contains(required),
            "recipes.lua should own recipe query helper {required:?}"
        );
    }
    for forbidden in [
        "local function recipe_unlocks",
        "local function recipe_ingredients",
        "local function recipe_products",
        "local function recipe_summary",
        "local function recipe_details",
        "function M.get_recipe(name)",
        "function M.get_recipes_by_category(category)",
        "function M.get_recipes_for_item(item)",
    ] {
        assert!(
            !control_lua.contains(forbidden),
            "control.lua should not retain recipe implementation helper {forbidden:?}"
        );
    }
    for required in [
        "local recipes = require(\"recipes\")",
        "local research = require(\"research\")",
        "local function get_prototype_impl",
        "get_recipe = function(name)",
        "json_remote_call(\"get_recipe\", recipes.get_recipe, name)",
        "get_recipes_by_category = function(category)",
        "json_remote_call(\"get_recipes_by_category\", recipes.get_recipes_by_category, category)",
        "get_recipes_for_item = function(item)",
        "json_remote_call(\"get_recipes_for_item\", recipes.get_recipes_for_item, item)",
        "get_prototype = function(name)",
        "get_research_status = function(agent_id)",
        "json_remote_call(\"get_research_status\", research.get_research_status, scoped_character(agent_id))",
        "get_available_research = function(agent_id)",
        "local character = find_factorioctl_character(agent_id)",
        "json_remote_call(\"get_available_research\", research.get_available_research, character)",
        "feed_lab_from_inventory = function(agent_id, lab_unit_number, science_pack, count, dry_run)",
        "json_remote_call(\"feed_lab_from_inventory\", research.feed_lab_from_inventory, character, lab_unit_number, science_pack, count, dry_run)",
        "start_research = function(tech_name, agent_id)",
        "json_remote_call(\"start_research\", research.start_research, scoped_character(agent_id), tech_name)",
        "is_tech_researched = function(tech_name, agent_id)",
        "json_remote_call(\"is_tech_researched\", research.is_tech_researched, scoped_character(agent_id), tech_name)",
    ] {
        assert!(
            control_lua.contains(required),
            "control.lua recipe/prototype/research remotes should include {required:?}"
        );
    }
    for required in [
        "local function research_ingredients",
        "local function research_needs_science",
        "local function research_effects",
        "local function science_totals_from_labs",
        "local function count_science_from_inventory",
        "function M.feed_lab_from_inventory(character, lab_unit_number, science_pack, count, dry_run)",
        "function M.get_research_status(character)",
        "function M.get_available_research(character)",
        "function M.start_research(character, tech_name)",
        "function M.is_tech_researched(character, tech_name)",
    ] {
        assert!(
            research_lua.contains(required),
            "research.lua should own research helper {required:?}"
        );
    }
    for forbidden in [
        "local function research_ingredients",
        "local function research_needs_science",
        "local function research_effects",
        "local function science_totals_from_labs",
        "local function count_science_from_inventory",
        "function M.feed_lab_from_inventory(character, lab_unit_number, science_pack, count, dry_run)",
        "function M.get_research_status(character)",
        "function M.get_available_research(character)",
        "function M.start_research(character, tech_name)",
        "function M.is_tech_researched(character, tech_name)",
    ] {
        assert!(
            !control_lua.contains(forbidden),
            "control.lua should not retain research implementation helper {forbidden:?}"
        );
    }

    assert!(
        research_lua.contains("force.add_research(tech)")
            && research_lua.contains(
                "local labs = surface.find_entities_filtered{type = \"lab\", force = force}"
            )
            && research_lua.contains("lab.get_inventory(defines.inventory.lab_input)")
            && research_lua.contains("entities.find_by_unit_number(tonumber(lab_unit_number))")
            && research_lua.contains("player_inv.remove{name = science_pack, count = count}")
            && research_lua.contains("lab_inv.insert{name = science_pack, count = removed}")
            && research_lua.contains("expected_miss")
            && research_lua.contains("local have = science_totals[ing.name] or 0")
            && research_lua.contains("requires_lab = needs_science")
            && research_lua.contains("if trigger then")
            && research_lua.contains("error_kind = \"research_trigger_required\"")
            && research_lua.contains("force.add_research(tech)")
            && !research_lua.contains("tech.researched = true")
            && research_lua.contains("return {success = false, error = \"Technology not found\"}"),
        "research.lua should own research lab scans, science accounting, and queueing"
    );
}

#[test]
fn research_cli_queries_use_mod_remotes_not_inline_lua() {
    let research_rs = include_str!("../src/cli/research.rs");
    let client_mod = include_str!("../src/client/mod.rs");

    for required in [
        "get_research_status",
        "get_available_research",
        "start_research",
    ] {
        assert!(
            (research_rs.contains("call_remote(") && research_rs.contains(required))
                || (client_mod.contains("call_remote(") && client_mod.contains(required)),
            "research path should call /claude directly {required:?}"
        );
    }
    for required in [r#""feed_lab_from_inventory""#, r#""is_tech_researched""#] {
        assert!(
            client_mod.contains(required),
            "FactorioClient research helpers should use /claude directly with remote marker {required:?}"
        );
    }

    for forbidden in [
        "force.technologies",
        "force.current_research",
        "research_unit_ingredients",
        "game.forces.player.technologies",
    ] {
        assert!(
            !research_rs.contains(forbidden) && !client_mod.contains(forbidden),
            "research CLI/client should not embed inline gameplay Lua {forbidden:?}"
        );
    }
}

#[test]
fn lua_plans_only_recommend_model_visible_tools() {
    let allowed = BTreeSet::from([
        "build_automation_science",
        "build_lab_feed",
        "execute_edge_miner",
        "execute_entity_placement_near",
        "get_power_status",
        "get_research_status",
        "place_entity",
        "plan_steam_power",
        "repair_fuel_sustainability",
        "rotate_entity",
        "start_research",
        "verify_production",
    ]);
    let sources = [
        (
            "entities.lua",
            include_str!("../mod/claude-interface/entities.lua"),
        ),
        (
            "placement.lua",
            include_str!("../mod/claude-interface/placement.lua"),
        ),
        (
            "power.lua",
            include_str!("../mod/claude-interface/power.lua"),
        ),
        (
            "research.lua",
            include_str!("../mod/claude-interface/research.lua"),
        ),
    ];

    for (path, source) in sources {
        for (line_number, line) in source.lines().enumerate() {
            let Some((_, suffix)) = line.split_once("tool = \"") else {
                continue;
            };
            let tool = suffix
                .split_once('"')
                .map(|(tool, _)| tool)
                .expect("literal Lua tool assignment should close its quote");
            assert!(
                allowed.contains(tool),
                "{path}:{} recommends hidden or nonexistent tool {tool:?}",
                line_number + 1
            );
        }
    }
}

#[test]
fn agent_id_accepts_and_rejects_spec_vectors() {
    for raw in [
        None,
        Some(""),
        Some("default"),
        Some("__player__"),
        Some("doug-nauvis"),
        Some("a.b:c"),
        Some("a--b"),
    ] {
        AgentId::new(raw).expect("accepted agent id");
    }

    for raw in [Some("\""), Some("\n"), Some("]"), Some("a\"b")] {
        assert!(AgentId::new(raw).is_err(), "expected {raw:?} to reject");
    }

    assert!(AgentId::new(Some(&"a".repeat(65))).is_err());
    assert!(AgentId::new(None).expect("default").is_legacy());
    assert!(AgentId::new(Some("default")).expect("default").is_legacy());
    assert!(!AgentId::new(Some("doug-nauvis"))
        .expect("named")
        .is_legacy());
}

#[test]
fn remote_command_preserves_hostile_string_arguments_as_json_values() {
    let hostile_inputs = [
        "a\"b",
        "a'b",
        "a\\b",
        "a\nb",
        "a\rb",
        "a]b",
        "\\\"); game.print(\"pwned",
    ];

    for raw in hostile_inputs {
        for (case_name, lua) in [
            (
                "find_entities_name",
                LuaCommand::find_entities(area(), None, Some(raw)),
            ),
            ("craft", LuaCommand::craft(&legacy_agent(), raw, 1)),
            (
                "place_entity",
                LuaCommand::place_entity(&legacy_agent(), raw, pos(1.0, 2.0), Direction::North),
            ),
            (
                "place_underground_belt",
                LuaCommand::place_underground_belt(
                    &legacy_agent(),
                    raw,
                    pos(1.0, 2.0),
                    Direction::North,
                    "input",
                ),
            ),
            (
                "insert_items",
                LuaCommand::insert_items(&legacy_agent(), 45, raw, 1, "chest"),
            ),
            ("set_recipe", LuaCommand::set_recipe(47, raw)),
            ("get_recipe", LuaCommand::get_recipe(raw)),
            (
                "save_blueprint",
                LuaCommand::save_blueprint(&legacy_agent(), raw, area()),
            ),
            (
                "import_blueprint",
                LuaCommand::import_blueprint(&legacy_agent(), raw, pos(1.0, 2.0), 0),
            ),
            (
                "find_nearest_minable",
                LuaCommand::find_nearest_minable(&legacy_agent(), raw, 100),
            ),
            (
                "mine_nearest",
                LuaCommand::mine_nearest(&legacy_agent(), raw, 1),
            ),
            (
                "build_drill_array_resource",
                LuaCommand::build_drill_array(
                    &legacy_agent(),
                    1,
                    raw,
                    Some((1.0, 2.0)),
                    "burner-mining-drill",
                    "south",
                ),
            ),
            (
                "build_drill_array_drill",
                LuaCommand::build_drill_array(
                    &legacy_agent(),
                    1,
                    "iron-ore",
                    Some((1.0, 2.0)),
                    raw,
                    "south",
                ),
            ),
            (
                "build_smelter_line_furnace",
                LuaCommand::build_smelter_line(&legacy_agent(), 1, (1.0, 2.0), raw, "east", 3),
            ),
            (
                "build_smelter_line_direction",
                LuaCommand::build_smelter_line(
                    &legacy_agent(),
                    1,
                    (1.0, 2.0),
                    "stone-furnace",
                    raw,
                    3,
                ),
            ),
            ("broadcast_console", LuaCommand::broadcast_console(raw)),
            (
                "broadcast_flying_text",
                LuaCommand::broadcast_flying_text(raw),
            ),
            ("start_research", LuaCommand::start_research(raw)),
        ] {
            let args = remote_args(&lua);
            assert!(
                args.iter().any(|arg| arg == &json!(raw)),
                "{} should preserve {raw:?} as one JSON argument:\n{}",
                case_name,
                lua
            );
            assert_balanced_double_quotes(case_name, &lua);
            assert!(
                !lua.contains("remote.call") && !lua.contains("rcon.print"),
                "{} should not expose hostile Lua as executable code",
                case_name
            );
        }
    }
}

#[test]
fn static_builder_tests_cover_named_legacy_extract_and_registry_contracts() {
    let named = named_agent();
    let legacy = legacy_agent();

    let named_lua = LuaCommand::walk_character(&named, pos(12.0, 13.0));
    assert_remote_request("named walk_character", &named_lua, "set_walk_target");
    assert_eq!(
        remote_args(&named_lua),
        vec![json!("doug"), json!(12), json!(13)]
    );
    assert!(!named_lua.contains("storage.factorioctl_characters"));
    assert!(!named_lua.contains("connected_players"));
    assert!(!named_lua.contains("global."));
    assert!(!named_lua.contains("walking_state"));

    let legacy_lua = LuaCommand::walk_character(&legacy, pos(12.0, 13.0));
    assert_remote_request("legacy walk_character", &legacy_lua, "set_walk_target");
    assert_eq!(
        remote_args(&legacy_lua),
        vec![json!("__player__"), json!(12), json!(13)]
    );
    assert!(!legacy_lua.contains("for _, p in pairs(game.connected_players) do"));
    assert!(!legacy_lua.contains("storage.factorioctl_characters"));
    assert!(!legacy_lua.contains("walking_state"));

    let extract_lua = LuaCommand::extract_items(&named, 46, "iron-ore", 6, "chest");
    assert_remote_request("extract_items", &extract_lua, "extract_items");
    assert_eq!(
        remote_args(&extract_lua),
        vec![
            json!("doug"),
            json!(46),
            json!("iron-ore"),
            json!(6),
            json!("chest")
        ]
    );
    assert!(!extract_lua.contains("get_main_inventory()"));
    assert!(!extract_lua.contains("game.players[1]"));

    let get_entity_inventory_lua = LuaCommand::get_entity_inventory(42);
    assert_remote_request(
        "get_entity_inventory",
        &get_entity_inventory_lua,
        "get_entity_inventory",
    );

    for lua in [
        LuaCommand::extract_items(&named, 46, "iron-ore", 6, "chest"),
        LuaCommand::set_recipe(47, "copper-cable"),
    ] {
        assert!(!lua.contains("storage.factorioctl_entities["));
    }
}
