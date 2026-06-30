//! Lua command builders for Factorio interactions
//!
//! These builders generate Lua code that can be executed via RCON.
//! All commands use rcon.print() to return JSON-formatted results.

use crate::client::AgentId;
use crate::world::{Area, Direction, Position};

/// Builder for Lua commands
pub struct LuaCommand;

impl LuaCommand {
    /// Escape text for safe embedding inside a Lua string literal.
    ///
    /// Escapes both quote styles so the result is safe in either a double- or
    /// single-quoted Lua literal (`"..."` or `'...'`). `\'` and `\"` are valid
    /// escapes regardless of the surrounding quote, so over-escaping is harmless.
    pub fn lua_escape(s: &str) -> String {
        let mut escaped = String::with_capacity(s.len());
        for ch in s.chars() {
            match ch {
                '\\' => escaped.push_str("\\\\"),
                '"' => escaped.push_str("\\\""),
                '\'' => escaped.push_str("\\'"),
                '\n' => escaped.push_str("\\n"),
                '\r' => escaped.push_str("\\r"),
                '\t' => escaped.push_str("\\t"),
                c if c.is_control() => escaped.push_str(&format!("\\{:03}", c as u32)),
                c => escaped.push(c),
            }
        }
        escaped
    }

    fn claude_interface_json_call(function_name: &str, args: &[String], guidance: &str) -> String {
        let function_name = Self::lua_escape(function_name);
        let guidance = Self::lua_escape(guidance);
        let args = args.join(", ");
        let call_args = if args.is_empty() {
            String::new()
        } else {
            format!(", {}", args)
        };
        format!(
            r#"
if remote.interfaces["claude_interface"] and remote.interfaces["claude_interface"]["{}"] then
    rcon.print(remote.call("claude_interface", "{}"{}))
else
    rcon.print(helpers.table_to_json({{
        error = "claude-interface mod does not expose {}",
        action_needed = "sync_or_restart_mod",
        guidance = "{}"
    }}))
end
"#,
            function_name, function_name, call_args, function_name, guidance
        )
        .trim()
        .to_string()
    }

    fn lua_string_arg(value: &str) -> String {
        format!(r#""{}""#, Self::lua_escape(value))
    }

    fn optional_lua_string_arg(value: Option<&str>) -> String {
        value
            .map(Self::lua_string_arg)
            .unwrap_or_else(|| "nil".to_string())
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
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before walking.",
        )
    }

    pub fn clear_walk_target(agent_id: &AgentId) -> String {
        Self::claude_interface_json_call(
            "clear_walk_target",
            &[Self::lua_string_arg(agent_id.as_str())],
            "Run just sync/resume so the updated claude-interface mod is loaded before clearing walk targets.",
        )
    }

    pub fn walk_target_active(agent_id: &AgentId) -> String {
        format!(
            r#"
if remote.interfaces["claude_interface"] and remote.interfaces["claude_interface"]["has_walk_target"] then
    rcon.print(remote.call("claude_interface", "has_walk_target", "{}") and "true" or "false")
else
    rcon.print("false")
end
"#,
            agent_id.as_str()
        )
        .trim()
        .to_string()
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

    /// Insert items into an entity
    pub fn insert_items(unit_number: u32, item: &str, count: u32, inventory_type: &str) -> String {
        Self::claude_interface_json_call(
            "insert_items",
            &[
                unit_number.to_string(),
                Self::lua_string_arg(item),
                count.to_string(),
                Self::lua_string_arg(inventory_type),
            ],
            "Run just sync/resume so the updated claude-interface mod is loaded before inserting items.",
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
        let name = Self::lua_escape(name);
        Self::claude_interface_json_call(
            "get_recipe",
            &[format!(r#""{}""#, name)],
            "Run just sync/resume so the updated claude-interface mod is loaded before reading recipes.",
        )
    }

    /// Get all recipes in a category
    pub fn get_recipes_by_category(category: &str) -> String {
        let category = Self::lua_escape(category);
        Self::claude_interface_json_call(
            "get_recipes_by_category",
            &[format!(r#""{}""#, category)],
            "Run just sync/resume so the updated claude-interface mod is loaded before listing recipes.",
        )
    }

    /// Get all recipes that produce a specific item
    pub fn get_recipes_for_item(item: &str) -> String {
        let item = Self::lua_escape(item);
        Self::claude_interface_json_call(
            "get_recipes_for_item",
            &[format!(r#""{}""#, item)],
            "Run just sync/resume so the updated claude-interface mod is loaded before listing recipes.",
        )
    }

    /// Get an entity prototype by name
    pub fn get_prototype(name: &str) -> String {
        let name = Self::lua_escape(name);
        Self::claude_interface_json_call(
            "get_prototype",
            &[format!(r#""{}""#, name)],
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
    use crate::world::Position;

    use super::LuaCommand;

    #[test]
    fn register_chat_handler_injects_no_level_script_event_handler() {
        // MP-safety: chat capture lives in the mod (control.lua on_console_chat),
        // NOT a runtime-injected level-script handler. register_chat_handler must
        // never emit script.on_event, or joining clients are refused with
        // "mod event handlers are not identical ... level".
        let lua = LuaCommand::register_chat_handler();
        assert!(lua.contains(r#"remote.call("claude_interface", "chat_capture_status")"#));
        assert!(!lua.contains(r#"rcon.print("registered")"#));
        assert!(!lua.contains("script.on_event"));
        assert!(!lua.contains("on_console_chat"));
    }

    #[test]
    fn get_and_clear_chat_messages_reads_via_mod_remote() {
        let lua = LuaCommand::get_and_clear_chat_messages();
        assert!(lua.contains(r#"remote.call("claude_interface", "get_chat_messages")"#));
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
    fn named_set_walk_target_routes_to_mod_remote_without_fallback_driver() {
        let agent = AgentId::new(Some("doug")).expect("named agent id");
        let lua = LuaCommand::set_walk_target(&agent, Position::new(12.0, 13.0));

        assert!(
            lua.contains(r#"remote.interfaces["claude_interface"]"#),
            "set_walk_target should guard the claude-interface remote path"
        );
        assert!(
            lua.contains(r#"remote.call("claude_interface", "set_walk_target", "doug", 12, 13)"#),
            "set_walk_target should route targets through the mod"
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

        assert!(
            lua.contains(r#"remote.interfaces["claude_interface"]["has_walk_target"]"#),
            "walk_target_active should guard the mod active-target query"
        );
        assert!(
            lua.contains(r#"remote.call("claude_interface", "has_walk_target", "doug")"#),
            "walk_target_active should query the mod target state when available"
        );
        assert!(
            lua.contains(r#"rcon.print("false")"#),
            "walk_target_active should report inactive if the required mod backend is unavailable"
        );
        assert!(!lua.contains("storage.factorioctl_walk_targets"));
    }

    #[test]
    fn legacy_walk_character_uses_mod_target_remote_too() {
        let agent = AgentId::new(None).expect("legacy agent id");
        let lua = LuaCommand::walk_character(&agent, Position::new(12.0, 13.0));

        assert!(
            lua.contains(
                r#"remote.call("claude_interface", "set_walk_target", "__player__", 12, 13)"#
            ),
            "legacy/player walking should route through the same mod target backend"
        );
        assert!(!lua.contains("walking_state"));
        assert!(!lua.contains("storage.factorioctl_walk_targets"));
    }
}
