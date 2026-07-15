//! Legacy remote command builders for Factorio interactions.
//!
//! The public facade is still named `LuaCommand` because many tests and older
//! callers refer to it, but these builders emit the mod's `/claude` JSON
//! command envelope instead of generated Lua.

use crate::client::AgentId;
use crate::world::{Area, Direction, Position};
use serde_json::{json, Value};

/// Builder for Lua commands
pub struct LuaCommand;

impl LuaCommand {
    fn claude_interface_json_call(function_name: &str, args: &[String], _guidance: &str) -> String {
        let args: Vec<Value> = args
            .iter()
            .map(|arg| match arg.as_str() {
                "nil" => Value::Null,
                other => serde_json::from_str(other).unwrap_or_else(|_| Value::String(arg.clone())),
            })
            .collect();
        let request = json!({
            "fn": function_name,
            "args": args,
            "n": args.len(),
        });
        format!("/claude {}", request)
    }

    fn lua_string_arg(value: &str) -> String {
        serde_json::to_string(value).expect("string JSON serialization cannot fail")
    }

    fn optional_lua_string_arg(value: Option<&str>) -> String {
        value
            .map(Self::lua_string_arg)
            .unwrap_or_else(|| "null".to_string())
    }

    fn character_storage_key(agent_id: &AgentId) -> &str {
        if agent_id.is_legacy() {
            "__player__"
        } else {
            agent_id.as_str()
        }
    }

    pub fn broadcast_console(message: &str) -> String {
        Self::claude_interface_json_call(
            "broadcast_console",
            &[Self::lua_string_arg(message)],
            "Run just sync/resume so the updated claude-interface mod is loaded before broadcasting messages.",
        )
    }

    pub fn broadcast_flying_text(message: &str) -> String {
        Self::claude_interface_json_call(
            "broadcast_flying_text",
            &[Self::lua_string_arg(message)],
            "Run just sync/resume so the updated claude-interface mod is loaded before broadcasting flying text.",
        )
    }

    pub fn get_tick() -> String {
        Self::claude_interface_json_call(
            "get_tick",
            &[],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading the game tick.",
        )
    }

    pub fn set_tick_paused(paused: bool) -> String {
        Self::claude_interface_json_call(
            "set_tick_paused",
            &[paused.to_string()],
            "Run just sync/resume so the updated claude-interface mod is loaded before pausing or resuming the game.",
        )
    }

    pub fn set_game_speed(speed: f64) -> String {
        Self::claude_interface_json_call(
            "set_game_speed",
            &[speed.to_string()],
            "Run just sync/resume so the updated claude-interface mod is loaded before changing game speed.",
        )
    }

    /// Get list of surfaces
    pub fn get_surfaces() -> String {
        Self::claude_interface_json_call(
            "get_surfaces",
            &[],
            "Run just sync/resume so the updated claude-interface mod is loaded before listing surfaces.",
        )
    }

    /// Find entities in an area
    pub fn find_entities(area: Area, entity_type: Option<&str>, name: Option<&str>) -> String {
        Self::claude_interface_json_call(
            "find_entities",
            &[
                area.left_top.x.to_string(),
                area.left_top.y.to_string(),
                area.right_bottom.x.to_string(),
                area.right_bottom.y.to_string(),
                Self::optional_lua_string_arg(entity_type),
                Self::optional_lua_string_arg(name),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before finding entities.",
        )
    }

    /// Verify production status for producing entities in an area
    pub fn verify_production(area: Area) -> String {
        Self::claude_interface_json_call(
            "verify_production",
            &[
                area.left_top.x.to_string(),
                area.left_top.y.to_string(),
                area.right_bottom.x.to_string(),
                area.right_bottom.y.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before verifying production.",
        )
    }

    /// Diagnose ranked production blockers in an area.
    pub fn diagnose_factory_blockers(area: Area, limit: u32) -> String {
        Self::claude_interface_json_call(
            "diagnose_factory_blockers",
            &[
                area.left_top.x.to_string(),
                area.left_top.y.to_string(),
                area.right_bottom.x.to_string(),
                area.right_bottom.y.to_string(),
                limit.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before diagnosing factory blockers.",
        )
    }

    /// Diagnose whether burnable consumers have durable coal supply.
    pub fn diagnose_fuel_sustainability(area: Area, limit: u32) -> String {
        Self::claude_interface_json_call(
            "diagnose_fuel_sustainability",
            &[
                area.left_top.x.to_string(),
                area.left_top.y.to_string(),
                area.right_bottom.x.to_string(),
                area.right_bottom.y.to_string(),
                limit.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before diagnosing fuel sustainability.",
        )
    }

    /// Get a specific entity by unit number
    pub fn get_entity(unit_number: u32) -> String {
        Self::claude_interface_json_call(
            "get_entity",
            &[unit_number.to_string()],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading entities.",
        )
    }

    /// Get an entity's real drop position, if Factorio exposes one
    pub fn get_entity_drop_position(unit_number: u32) -> String {
        Self::claude_interface_json_call(
            "get_entity_drop_position",
            &[unit_number.to_string()],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading entity drop positions.",
        )
    }

    /// Get an entity's inventories
    pub fn get_entity_inventory(unit_number: u32) -> String {
        Self::claude_interface_json_call(
            "get_entity_inventory",
            &[unit_number.to_string()],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading entity inventories.",
        )
    }

    /// Get the recipe currently configured on a crafting machine.
    pub fn get_entity_recipe(unit_number: u32) -> String {
        Self::claude_interface_json_call(
            "get_entity_recipe",
            &[unit_number.to_string()],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading entity recipes.",
        )
    }

    /// Find resources in an area and aggregate by type
    pub fn find_resources(area: Area, resource_type: Option<&str>) -> String {
        Self::claude_interface_json_call(
            "find_resources",
            &[
                area.left_top.x.to_string(),
                area.left_top.y.to_string(),
                area.right_bottom.x.to_string(),
                area.right_bottom.y.to_string(),
                Self::optional_lua_string_arg(resource_type),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before finding resources.",
        )
    }

    /// Find nearest resource from a position
    pub fn find_nearest_resource(resource_name: &str, from: Position) -> String {
        Self::claude_interface_json_call(
            "find_nearest_resource",
            &[
                Self::lua_string_arg(resource_name),
                from.x.to_string(),
                from.y.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before finding resources.",
        )
    }

    /// Get tiles in an area
    pub fn get_tiles(area: Area) -> String {
        Self::claude_interface_json_call(
            "get_tiles",
            &[
                (area.left_top.x as i32).to_string(),
                (area.left_top.y as i32).to_string(),
                (area.right_bottom.x as i32).to_string(),
                (area.right_bottom.y as i32).to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading tiles.",
        )
    }

    /// Get a specific tile
    pub fn get_tile(position: Position) -> String {
        Self::claude_interface_json_call(
            "get_tile",
            &[
                (position.x as i32).to_string(),
                (position.y as i32).to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading tiles.",
        )
    }

    /// Initialize character entity
    pub fn init_character(agent_id: &AgentId, x: f64, y: f64) -> String {
        Self::claude_interface_json_call(
            "init_character",
            &[
                Self::lua_string_arg(Self::character_storage_key(agent_id)),
                x.to_string(),
                y.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before creating characters.",
        )
    }

    /// Teleport character to position
    pub fn teleport_character(agent_id: &AgentId, position: Position) -> String {
        Self::claude_interface_json_call(
            "teleport_character",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                position.x.to_string(),
                position.y.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before teleporting characters.",
        )
    }

    pub fn set_walk_target(agent_id: &AgentId, position: Position) -> String {
        Self::claude_interface_json_call(
            "set_walk_target",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                position.x.to_string(),
                position.y.to_string(),
                "nil".to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before walking.",
        )
    }

    pub fn clear_walk_target(agent_id: &AgentId) -> String {
        Self::claude_interface_json_call(
            "clear_walk_target",
            &[Self::lua_string_arg(agent_id.as_str()), "nil".to_string()],
            "Run just sync/resume so the updated claude-interface mod is loaded before clearing walk targets.",
        )
    }

    pub fn get_walk_status(agent_id: &AgentId, walk_id: u64) -> String {
        Self::claude_interface_json_call(
            "get_walk_status",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                walk_id.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading walk status.",
        )
    }

    pub fn get_entity_reach(agent_id: &AgentId, unit_number: u32) -> String {
        Self::claude_interface_json_call(
            "get_entity_reach",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                unit_number.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading entity reach.",
        )
    }

    pub fn get_position_reach(agent_id: &AgentId, position: Position, reach_kind: &str) -> String {
        Self::claude_interface_json_call(
            "get_position_reach",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                position.x.to_string(),
                position.y.to_string(),
                Self::lua_string_arg(reach_kind),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading position reach.",
        )
    }

    pub fn walk_target_active(agent_id: &AgentId) -> String {
        Self::claude_interface_json_call(
            "has_walk_target",
            &[Self::lua_string_arg(agent_id.as_str())],
            "Run just sync/resume so the updated claude-interface mod is loaded before checking walk targets.",
        )
    }

    /// Start walking character to position via the mod-owned deterministic target driver.
    pub fn walk_character(agent_id: &AgentId, position: Position) -> String {
        Self::set_walk_target(agent_id, position)
    }

    /// Get character status
    pub fn character_status(agent_id: &AgentId) -> String {
        Self::claude_interface_json_call(
            "character_status",
            &[Self::lua_string_arg(agent_id.as_str())],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading character status.",
        )
    }

    /// Get character inventory
    pub fn character_inventory(agent_id: &AgentId) -> String {
        Self::claude_interface_json_call(
            "character_inventory",
            &[Self::lua_string_arg(agent_id.as_str())],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading character inventories.",
        )
    }

    /// Check whether the agent character can stand at a world position.
    pub fn can_stand_at(agent_id: &AgentId, position: Position, radius: u32) -> String {
        Self::claude_interface_json_call(
            "can_stand_at",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                position.x.to_string(),
                position.y.to_string(),
                radius.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before checking standability.",
        )
    }

    /// Diagnose whether the agent character is currently blocked by placed entities.
    pub fn is_player_blocked(agent_id: &AgentId, radius: u32) -> String {
        Self::claude_interface_json_call(
            "is_player_blocked",
            &[Self::lua_string_arg(agent_id.as_str()), radius.to_string()],
            "Run just sync/resume so the updated claude-interface mod is loaded before checking character blockage.",
        )
    }

    /// Move a physically blocked agent character to the nearest verified clear standing position.
    pub fn unstuck(agent_id: &AgentId, radius: u32, dry_run: bool) -> String {
        Self::claude_interface_json_call(
            "unstuck",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                radius.to_string(),
                if dry_run { "true" } else { "false" }.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before unsticking characters.",
        )
    }

    /// Get character position as "x,y".
    pub fn get_character_position(agent_id: &AgentId) -> String {
        Self::claude_interface_json_call(
            "get_character_pos",
            &[Self::lua_string_arg(agent_id.as_str())],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading character positions.",
        )
    }

    /// Start mining at a position (uses mining_state for animations)
    pub fn start_mining(agent_id: &AgentId, position: Position) -> String {
        Self::claude_interface_json_call(
            "start_mining",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                position.x.to_string(),
                position.y.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before mining.",
        )
    }

    /// Stop mining
    pub fn stop_mining(agent_id: &AgentId) -> String {
        Self::claude_interface_json_call(
            "stop_mining",
            &[Self::lua_string_arg(agent_id.as_str())],
            "Run just sync/resume so the updated claude-interface mod is loaded before mining.",
        )
    }

    /// Get mining status
    pub fn get_mining_status(agent_id: &AgentId) -> String {
        Self::claude_interface_json_call(
            "get_mining_status",
            &[Self::lua_string_arg(agent_id.as_str())],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading mining status.",
        )
    }

    /// Mine entity at position (instant - for compatibility)
    pub fn mine_at(agent_id: &AgentId, position: Position, count: u32) -> String {
        Self::claude_interface_json_call(
            "mine_at",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                position.x.to_string(),
                position.y.to_string(),
                count.to_string(),
                "3".to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before mining.",
        )
    }

    /// Find nearest minable entity by prototype name.
    pub fn find_nearest_minable(agent_id: &AgentId, entity_name: &str, radius: u32) -> String {
        Self::claude_interface_json_call(
            "find_nearest_minable",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                Self::lua_string_arg(entity_name),
                radius.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before locating minable entities.",
        )
    }

    /// Mine nearest entity of type
    pub fn mine_nearest(agent_id: &AgentId, entity_type: &str, count: u32) -> String {
        Self::claude_interface_json_call(
            "mine_nearest",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                Self::lua_string_arg(entity_type),
                count.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before mining.",
        )
    }

    /// Start crafting a recipe
    pub fn craft(agent_id: &AgentId, recipe: &str, count: u32) -> String {
        Self::claude_interface_json_call(
            "craft",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                Self::lua_string_arg(recipe),
                count.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before crafting.",
        )
    }

    /// Wait for crafting to complete (poll-based, handled in client)
    pub fn wait_for_crafting(agent_id: &AgentId) -> String {
        Self::claude_interface_json_call(
            "wait_for_crafting",
            &[Self::lua_string_arg(agent_id.as_str())],
            "Run just sync/resume so the updated claude-interface mod is loaded before waiting for crafting.",
        )
    }

    /// Read the exact craft admission persisted in the Factorio save.
    pub fn get_craft_admission(agent_id: &AgentId) -> String {
        Self::claude_interface_json_call(
            "get_craft_admission",
            &[Self::lua_string_arg(agent_id.as_str())],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading craft admission evidence.",
        )
    }

    /// Acknowledge and clear one exact terminal craft admission.
    pub fn clear_craft_admission(
        agent_id: &AgentId,
        operation_id: &str,
        terminal_status: &str,
    ) -> String {
        Self::claude_interface_json_call(
            "clear_craft_admission",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                Self::lua_string_arg(operation_id),
                Self::lua_string_arg(terminal_status),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before clearing craft admission evidence.",
        )
    }

    /// Account inventory-verified standalone-character production and
    /// consumption flows so Factorio can evaluate craft triggers.
    pub fn record_verified_craft_flows(
        agent_id: &AgentId,
        operation_id: &str,
        flows: &Value,
    ) -> String {
        Self::claude_interface_json_call(
            "record_verified_craft_flows",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                Self::lua_string_arg(operation_id),
                flows.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before accounting verified character crafts.",
        )
    }

    /// Place an entity from inventory
    pub fn place_entity(
        agent_id: &AgentId,
        entity_name: &str,
        position: Position,
        direction: Direction,
    ) -> String {
        Self::claude_interface_json_call(
            "place_entity",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                Self::lua_string_arg(entity_name),
                position.x.to_string(),
                position.y.to_string(),
                direction.to_factorio().to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before placing entities.",
        )
    }

    /// Check whether Factorio itself can place an entity at a position
    pub fn check_entity_placement(
        agent_id: &AgentId,
        entity_name: &str,
        position: Position,
        direction: Direction,
    ) -> String {
        Self::claude_interface_json_call(
            "check_entity_placement",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                Self::lua_string_arg(entity_name),
                position.x.to_string(),
                position.y.to_string(),
                direction.to_factorio().to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before checking placements.",
        )
    }

    /// Find nearby Factorio-valid placements for an entity in any cardinal direction
    pub fn find_entity_placements(
        agent_id: &AgentId,
        entity_name: &str,
        center: Position,
        radius: u32,
        limit: u32,
    ) -> String {
        Self::claude_interface_json_call(
            "find_entity_placements",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                Self::lua_string_arg(entity_name),
                center.x.to_string(),
                center.y.to_string(),
                radius.to_string(),
                limit.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before searching placements.",
        )
    }

    /// Plan a Factorio-valid placement near a target while avoiding character overlap.
    pub fn plan_entity_placement_near(
        agent_id: &AgentId,
        entity_name: &str,
        target: Position,
        radius: u32,
        limit: u32,
    ) -> String {
        Self::claude_interface_json_call(
            "plan_entity_placement_near",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                Self::lua_string_arg(entity_name),
                target.x.to_string(),
                target.y.to_string(),
                radius.to_string(),
                limit.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before planning safe placements.",
        )
    }

    /// Plan an edge mining drill and output belt without mutating the game.
    pub fn build_edge_miner(
        agent_id: &AgentId,
        resource_name: &str,
        center: Position,
        radius: u32,
        drill_name: &str,
        limit: u32,
    ) -> String {
        Self::claude_interface_json_call(
            "build_edge_miner",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                Self::lua_string_arg(resource_name),
                center.x.to_string(),
                center.y.to_string(),
                radius.to_string(),
                Self::lua_string_arg(drill_name),
                limit.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before planning edge miners.",
        )
    }

    /// Plan a direct drill-output belt/inserter/furnace smelter without mutating the game.
    pub fn build_direct_smelter(
        agent_id: &AgentId,
        drill_unit_number: Option<u32>,
        output: Option<(Position, Direction)>,
        furnace_name: &str,
        inserter_name: &str,
        belt_name: &str,
        radius: u32,
    ) -> String {
        let (output_x, output_y, output_direction) = match output {
            Some((position, direction)) => (
                position.x.to_string(),
                position.y.to_string(),
                direction.to_factorio().to_string(),
            ),
            None => ("nil".to_string(), "nil".to_string(), "nil".to_string()),
        };
        Self::claude_interface_json_call(
            "build_direct_smelter",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                drill_unit_number
                    .map(|unit| unit.to_string())
                    .unwrap_or_else(|| "nil".to_string()),
                output_x,
                output_y,
                output_direction,
                Self::lua_string_arg(furnace_name),
                Self::lua_string_arg(inserter_name),
                Self::lua_string_arg(belt_name),
                radius.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before planning direct smelters.",
        )
    }

    /// Place an underground belt with specified type (input or output)
    pub fn place_underground_belt(
        agent_id: &AgentId,
        entity_name: &str,
        position: Position,
        direction: Direction,
        belt_type: &str,
    ) -> String {
        Self::claude_interface_json_call(
            "place_underground_belt",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                Self::lua_string_arg(entity_name),
                position.x.to_string(),
                position.y.to_string(),
                direction.to_factorio().to_string(),
                Self::lua_string_arg(belt_type),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before placing underground belts.",
        )
    }

    /// Place a ghost entity (for planning)
    pub fn place_ghost(
        agent_id: &AgentId,
        entity_name: &str,
        position: Position,
        direction: Direction,
    ) -> String {
        Self::claude_interface_json_call(
            "place_ghost",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                Self::lua_string_arg(entity_name),
                position.x.to_string(),
                position.y.to_string(),
                direction.to_factorio().to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before placing ghosts.",
        )
    }

    pub fn build_drill_array(
        agent_id: &AgentId,
        count: u32,
        resource: &str,
        near: Option<(f64, f64)>,
        drill_type: &str,
        direction: &str,
    ) -> String {
        let near_x = near
            .map(|pos| pos.0.to_string())
            .unwrap_or_else(|| "nil".to_string());
        let near_y = near
            .map(|pos| pos.1.to_string())
            .unwrap_or_else(|| "nil".to_string());
        Self::claude_interface_json_call(
            "build_drill_array",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                count.to_string(),
                Self::lua_string_arg(resource),
                near_x,
                near_y,
                Self::lua_string_arg(drill_type),
                Self::lua_string_arg(direction),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before building drill arrays.",
        )
    }

    pub fn build_smelter_line(
        agent_id: &AgentId,
        count: u32,
        start: (f64, f64),
        furnace_type: &str,
        line_direction: &str,
        spacing: u32,
    ) -> String {
        Self::claude_interface_json_call(
            "build_smelter_line",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                count.to_string(),
                start.0.to_string(),
                start.1.to_string(),
                Self::lua_string_arg(furnace_type),
                Self::lua_string_arg(line_direction),
                spacing.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before building smelter lines.",
        )
    }

    /// Remove entity at position
    pub fn remove_entity_at(agent_id: &AgentId, position: Position) -> String {
        Self::claude_interface_json_call(
            "remove_entity_at",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                position.x.to_string(),
                position.y.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before removing entities.",
        )
    }

    /// Remove entity by unit number
    pub fn remove_entity(agent_id: &AgentId, unit_number: u32) -> String {
        Self::claude_interface_json_call(
            "remove_entity",
            &[Self::lua_string_arg(agent_id.as_str()), unit_number.to_string()],
            "Run just sync/resume so the updated claude-interface mod is loaded before removing entities.",
        )
    }

    /// Rotate an entity to a new direction
    pub fn rotate_entity(unit_number: u32, direction: u8) -> String {
        Self::claude_interface_json_call(
            "rotate_entity",
            &[unit_number.to_string(), direction.to_string()],
            "Run just sync/resume so the updated claude-interface mod is loaded before rotating entities.",
        )
    }

    /// Replace an existing inserter's complete whitelist.
    pub fn configure_inserter(
        agent_id: &AgentId,
        unit_number: u32,
        allowed_items: &[String],
    ) -> String {
        Self::claude_interface_json_call(
            "configure_inserter",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                unit_number.to_string(),
                serde_json::to_string(allowed_items)
                    .expect("inserter whitelist JSON serialization cannot fail"),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before configuring inserter filters.",
        )
    }

    /// Insert items into an entity
    pub fn insert_items(
        agent_id: &AgentId,
        unit_number: u32,
        item: &str,
        count: u32,
        inventory_type: &str,
    ) -> String {
        Self::claude_interface_json_call(
            "insert_items",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                unit_number.to_string(),
                Self::lua_string_arg(item),
                count.to_string(),
                Self::lua_string_arg(inventory_type),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before inserting items.",
        )
    }

    /// Insert a small, bounded fuel buffer into an existing burner drill or
    /// burner inserter without replacing the entity.
    pub fn bootstrap_burner_once(
        agent_id: &AgentId,
        unit_number: u32,
        fuel_item: &str,
        count: u32,
    ) -> String {
        Self::claude_interface_json_call(
            "bootstrap_burner_once",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                unit_number.to_string(),
                Self::lua_string_arg(fuel_item),
                count.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before bootstrapping burner fuel.",
        )
    }

    /// Collect a bounded item count from an existing chest without mining or
    /// replacing the chest.
    pub fn collect_from_chest(
        agent_id: &AgentId,
        unit_number: u32,
        item: &str,
        count: u32,
    ) -> String {
        Self::claude_interface_json_call(
            "collect_from_chest",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                unit_number.to_string(),
                Self::lua_string_arg(item),
                count.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before collecting from a chest.",
        )
    }

    /// Extract items from an entity's inventory into the player's inventory
    pub fn extract_items(
        agent_id: &AgentId,
        unit_number: u32,
        item: &str,
        count: u32,
        inventory_type: &str,
    ) -> String {
        Self::claude_interface_json_call(
            "extract_items",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                unit_number.to_string(),
                Self::lua_string_arg(item),
                count.to_string(),
                Self::lua_string_arg(inventory_type),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before extracting items.",
        )
    }

    /// Set recipe on an assembling machine
    pub fn set_recipe(unit_number: u32, recipe: &str) -> String {
        Self::claude_interface_json_call(
            "set_recipe",
            &[unit_number.to_string(), Self::lua_string_arg(recipe)],
            "Run just sync/resume so the updated claude-interface mod is loaded before setting recipes.",
        )
    }

    // --- Prototype Queries ---

    /// Get a recipe by name
    pub fn get_recipe(name: &str) -> String {
        Self::claude_interface_json_call(
            "get_recipe",
            &[Self::lua_string_arg(name)],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading recipes.",
        )
    }

    /// Get all recipes in a category
    pub fn get_recipes_by_category(category: &str) -> String {
        Self::claude_interface_json_call(
            "get_recipes_by_category",
            &[Self::lua_string_arg(category)],
            "Run just sync/resume so the updated claude-interface mod is loaded before listing recipes.",
        )
    }

    /// Get all recipes that produce a specific item
    pub fn get_recipes_for_item(item: &str) -> String {
        Self::claude_interface_json_call(
            "get_recipes_for_item",
            &[Self::lua_string_arg(item)],
            "Run just sync/resume so the updated claude-interface mod is loaded before listing recipes.",
        )
    }

    /// Get an entity prototype by name
    pub fn get_prototype(name: &str) -> String {
        Self::claude_interface_json_call(
            "get_prototype",
            &[Self::lua_string_arg(name)],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading prototypes.",
        )
    }

    // --- Native Blueprint Commands ---

    /// Create a native Factorio blueprint string from entities in an area
    pub fn create_native_blueprint(agent_id: &AgentId, area: Area) -> String {
        Self::claude_interface_json_call(
            "create_native_blueprint",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                area.left_top.x.to_string(),
                area.left_top.y.to_string(),
                area.right_bottom.x.to_string(),
                area.right_bottom.y.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before creating blueprints.",
        )
    }

    /// Save a blueprint to storage with a name
    pub fn save_blueprint(agent_id: &AgentId, name: &str, area: Area) -> String {
        Self::claude_interface_json_call(
            "save_blueprint",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                Self::lua_string_arg(name),
                area.left_top.x.to_string(),
                area.left_top.y.to_string(),
                area.right_bottom.x.to_string(),
                area.right_bottom.y.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before saving blueprints.",
        )
    }

    /// List all saved blueprints
    pub fn list_blueprints() -> String {
        Self::claude_interface_json_call(
            "list_blueprints",
            &[],
            "Run just sync/resume so the updated claude-interface mod is loaded before listing blueprints.",
        )
    }

    /// Get a saved blueprint string by name
    pub fn get_blueprint(name: &str) -> String {
        Self::claude_interface_json_call(
            "get_blueprint",
            &[Self::lua_string_arg(name)],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading blueprints.",
        )
    }

    /// Place a saved blueprint at a position
    pub fn place_blueprint(
        agent_id: &AgentId,
        name: &str,
        position: Position,
        direction: u8,
    ) -> String {
        Self::claude_interface_json_call(
            "place_blueprint",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                Self::lua_string_arg(name),
                position.x.to_string(),
                position.y.to_string(),
                direction.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before placing blueprints.",
        )
    }

    /// Import and place a blueprint from a string
    pub fn import_blueprint(
        agent_id: &AgentId,
        bp_string: &str,
        position: Position,
        direction: u8,
    ) -> String {
        Self::claude_interface_json_call(
            "import_blueprint",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                Self::lua_string_arg(bp_string),
                position.x.to_string(),
                position.y.to_string(),
                direction.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before importing blueprints.",
        )
    }

    /// Delete a saved blueprint
    pub fn delete_blueprint(name: &str) -> String {
        Self::claude_interface_json_call(
            "delete_blueprint",
            &[Self::lua_string_arg(name)],
            "Run just sync/resume so the updated claude-interface mod is loaded before deleting blueprints.",
        )
    }

    /// Compatibility shim. Chat capture is registered by the claude-interface
    /// MOD's on_console_chat handler (control.lua), NOT by injecting a handler
    /// into the level script over RCON.
    pub fn register_chat_handler() -> String {
        Self::claude_interface_json_call(
            "chat_capture_status",
            &[],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading player messages.",
        )
    }

    /// Get and clear pending chat messages. Reads from the mod's chat buffer via
    /// the remote interface (MP-safe).
    pub fn get_and_clear_chat_messages() -> String {
        Self::claude_interface_json_call(
            "get_chat_messages",
            &[],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading player messages.",
        )
    }

    // --- Research Commands ---

    /// Get overall research status
    pub fn get_research_status() -> String {
        Self::claude_interface_json_call(
            "get_research_status",
            &[],
            "Run just sync/resume so the updated claude-interface mod is loaded before checking research.",
        )
    }

    /// Get available research (technologies that can be researched now)
    pub fn get_available_research(agent_id: &AgentId) -> String {
        Self::claude_interface_json_call(
            "get_available_research",
            &[Self::lua_string_arg(agent_id.as_str())],
            "Run just sync/resume so the updated claude-interface mod is loaded before checking research.",
        )
    }

    /// Feed science packs from the agent inventory into a lab, dry-run by default.
    pub fn feed_lab_from_inventory(
        agent_id: &AgentId,
        lab_unit_number: u32,
        science_pack: &str,
        count: u32,
        dry_run: bool,
    ) -> String {
        Self::claude_interface_json_call(
            "feed_lab_from_inventory",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                lab_unit_number.to_string(),
                Self::lua_string_arg(science_pack),
                count.to_string(),
                dry_run.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before feeding labs.",
        )
    }

    /// Start researching a technology (queues it properly)
    pub fn start_research(tech_name: &str) -> String {
        Self::claude_interface_json_call(
            "start_research",
            &[Self::lua_string_arg(tech_name)],
            "Run just sync/resume so the updated claude-interface mod is loaded before starting research.",
        )
    }

    /// Check whether a technology has already been researched.
    pub fn is_tech_researched(tech_name: &str) -> String {
        Self::claude_interface_json_call(
            "is_tech_researched",
            &[Self::lua_string_arg(tech_name)],
            "Run just sync/resume so the updated claude-interface mod is loaded before checking research state.",
        )
    }

    // --- Power Network Commands ---

    /// Get power status at a location (enhanced version with generator/consumer details)
    pub fn get_power_status(x: i32, y: i32, radius: u32) -> String {
        Self::claude_interface_json_call(
            "get_power_status",
            &[x.to_string(), y.to_string(), radius.to_string()],
            "Run just sync/resume so the updated claude-interface mod is loaded before using power diagnostics.",
        )
    }

    /// Get all power networks in an area
    pub fn get_power_networks(x: i32, y: i32, radius: u32) -> String {
        Self::claude_interface_json_call(
            "get_power_networks",
            &[x.to_string(), y.to_string(), radius.to_string()],
            "Run just sync/resume so the updated claude-interface mod is loaded before using power diagnostics.",
        )
    }

    /// Find power issues - entities without power or with low power
    pub fn find_power_issues(x: i32, y: i32, radius: u32) -> String {
        Self::claude_interface_json_call(
            "find_power_issues",
            &[x.to_string(), y.to_string(), radius.to_string()],
            "Run just sync/resume so the updated claude-interface mod is loaded before using power diagnostics.",
        )
    }

    /// Diagnose steam-power fluid and electric connectivity in an area.
    pub fn diagnose_steam_power(x: i32, y: i32, radius: u32) -> String {
        Self::claude_interface_json_call(
            "diagnose_steam_power",
            &[x.to_string(), y.to_string(), radius.to_string()],
            "Run just sync/resume so the updated claude-interface mod is loaded before using steam diagnostics.",
        )
    }

    /// Plan starter steam power before mutating pump/boiler/engine placement.
    pub fn plan_steam_power(agent_id: &AgentId, water_area: Area, target: Position) -> String {
        Self::claude_interface_json_call(
            "plan_steam_power",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                water_area.left_top.x.to_string(),
                water_area.left_top.y.to_string(),
                water_area.right_bottom.x.to_string(),
                water_area.right_bottom.y.to_string(),
                target.x.to_string(),
                target.y.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before planning steam power.",
        )
    }

    /// Plan dry-run repairs for an existing steam-power plant.
    pub fn repair_steam_power(
        agent_id: &AgentId,
        x: i32,
        y: i32,
        radius: u32,
        target: Position,
    ) -> String {
        Self::claude_interface_json_call(
            "repair_steam_power",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                x.to_string(),
                y.to_string(),
                radius.to_string(),
                target.x.to_string(),
                target.y.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before planning steam repairs.",
        )
    }

    /// Plan dry-run pole placement to extend power to a target.
    pub fn extend_power_to(
        agent_id: &AgentId,
        x: i32,
        y: i32,
        radius: u32,
        target: Position,
    ) -> String {
        Self::claude_interface_json_call(
            "extend_power_to",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                x.to_string(),
                y.to_string(),
                radius.to_string(),
                target.x.to_string(),
                target.y.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before planning power extensions.",
        )
    }

    /// Get power coverage data for map visualization
    pub fn get_power_coverage(x: i32, y: i32, radius: u32) -> String {
        Self::claude_interface_json_call(
            "get_power_coverage",
            &[x.to_string(), y.to_string(), radius.to_string()],
            "Run just sync/resume so the updated claude-interface mod is loaded before rendering power coverage.",
        )
    }

    // --- Alerts/Notifications Commands ---

    /// Get alerts for urgent conditions in an area
    pub fn get_alerts(x: i32, y: i32, radius: u32) -> String {
        Self::claude_interface_json_call(
            "get_alerts",
            &[x.to_string(), y.to_string(), radius.to_string()],
            "Run just sync/resume so the updated claude-interface mod is loaded before using alert diagnostics.",
        )
    }

    /// Get items on transport belts in an area
    pub fn get_belt_contents(area: Area) -> String {
        Self::claude_interface_json_call(
            "get_belt_contents",
            &[
                area.left_top.x.to_string(),
                area.left_top.y.to_string(),
                area.right_bottom.x.to_string(),
                area.right_bottom.y.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading belt contents.",
        )
    }

    /// Get items on transport belts with lane separation
    pub fn get_belt_lane_contents(area: Area) -> String {
        Self::claude_interface_json_call(
            "get_belt_lane_contents",
            &[
                area.left_top.x.to_string(),
                area.left_top.y.to_string(),
                area.right_bottom.x.to_string(),
                area.right_bottom.y.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading belt lane contents.",
        )
    }

    /// Clear trees and rocks in an area by mining them (player gets the items)
    /// Returns the count of cleared entities and items gained
    /// Requires player to be within proximity of the area
    pub fn clear_area(
        agent_id: &AgentId,
        area: Area,
        clear_trees: bool,
        clear_rocks: bool,
        dry_run: bool,
    ) -> String {
        Self::claude_interface_json_call(
            "clear_area",
            &[
                Self::lua_string_arg(agent_id.as_str()),
                area.left_top.x.to_string(),
                area.left_top.y.to_string(),
                area.right_bottom.x.to_string(),
                area.right_bottom.y.to_string(),
                clear_trees.to_string(),
                clear_rocks.to_string(),
                dry_run.to_string(),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before clearing areas.",
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::client::AgentId;
    use crate::world::{Area, Position};
    use serde_json::{json, Value};

    use super::LuaCommand;

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

    fn assert_remote_request(command: &str, method: &str) {
        let request = remote_request(command);
        assert_eq!(request["fn"].as_str(), Some(method));
        let args = request["args"]
            .as_array()
            .expect("request args should be an array");
        assert_eq!(request["n"].as_u64(), Some(args.len() as u64));
    }

    #[test]
    fn register_chat_handler_injects_no_level_script_event_handler() {
        // MP-safety: chat capture lives in the mod (control.lua on_console_chat),
        // NOT a runtime-injected level-script handler. register_chat_handler must
        // never emit script.on_event, or joining clients are refused with
        // "mod event handlers are not identical ... level".
        let lua = LuaCommand::register_chat_handler();
        assert_remote_request(&lua, "chat_capture_status");
        assert!(!lua.contains(r#"rcon.print("registered")"#));
        assert!(!lua.contains("script.on_event"));
        assert!(!lua.contains("on_console_chat"));
    }

    #[test]
    fn get_and_clear_chat_messages_reads_via_mod_remote() {
        let lua = LuaCommand::get_and_clear_chat_messages();
        assert_remote_request(&lua, "get_chat_messages");
        for line in lua.lines() {
            if let Some(idx) = line.find("--") {
                assert!(
                    line[..idx].trim().is_empty(),
                    "inline -- comment after code: {line}"
                );
            }
        }
    }

    #[test]
    fn diagnose_factory_blockers_routes_to_mod_remote_with_limit() {
        let area = Area {
            left_top: Position::new(1.0, 2.0),
            right_bottom: Position::new(11.0, 12.0),
        };
        let lua = LuaCommand::diagnose_factory_blockers(area, 7);

        assert_remote_request(&lua, "diagnose_factory_blockers");
        assert_eq!(
            remote_args(&lua),
            vec![json!(1), json!(2), json!(11), json!(12), json!(7)]
        );
        assert!(!lua.contains("execute_lua"));
    }

    #[test]
    fn diagnose_fuel_sustainability_routes_to_mod_remote_with_limit() {
        let area = Area {
            left_top: Position::new(1.0, 2.0),
            right_bottom: Position::new(11.0, 12.0),
        };
        let lua = LuaCommand::diagnose_fuel_sustainability(area, 9);

        assert_remote_request(&lua, "diagnose_fuel_sustainability");
        assert_eq!(
            remote_args(&lua),
            vec![json!(1), json!(2), json!(11), json!(12), json!(9)]
        );
        assert!(!lua.contains("execute_lua"));
    }

    #[test]
    fn named_set_walk_target_routes_to_mod_remote_without_fallback_driver() {
        let agent = AgentId::new(Some("doug")).expect("named agent id");
        let lua = LuaCommand::set_walk_target(&agent, Position::new(12.0, 13.0));

        assert_remote_request(&lua, "set_walk_target");
        assert_eq!(
            remote_args(&lua),
            vec![json!("doug"), json!(12), json!(13), Value::Null]
        );
        for forbidden in [
            "storage.factorioctl_walk_targets",
            "remote.call(\"claude_interface\", \"register_character\"",
            "script.on_event",
            "walking_state",
        ] {
            assert!(
                !lua.contains(forbidden),
                "set_walk_target should not retain host-side walk fallback {forbidden:?}"
            );
        }
    }

    #[test]
    fn named_walk_target_active_routes_to_mod_remote_and_fails_closed() {
        let agent = AgentId::new(Some("doug")).expect("named agent id");
        let lua = LuaCommand::walk_target_active(&agent);

        assert_remote_request(&lua, "has_walk_target");
        assert_eq!(remote_args(&lua), vec![json!("doug")]);
        assert!(!lua.contains("storage.factorioctl_walk_targets"));
    }

    #[test]
    fn legacy_walk_character_uses_mod_target_remote_too() {
        let agent = AgentId::new(None).expect("legacy agent id");
        let lua = LuaCommand::walk_character(&agent, Position::new(12.0, 13.0));

        assert_remote_request(&lua, "set_walk_target");
        assert_eq!(
            remote_args(&lua),
            vec![json!("__player__"), json!(12), json!(13), Value::Null]
        );
        assert!(!lua.contains("walking_state"));
        assert!(!lua.contains("storage.factorioctl_walk_targets"));
    }
}
