use factorioctl::client::lua::LuaCommand;
use factorioctl::client::AgentId;
use factorioctl::world::{Area, Direction, Position};

struct LuaCase {
    name: &'static str,
    lua: String,
}

impl LuaCase {
    fn new(name: &'static str, lua: String) -> Self {
        Self { name, lua }
    }

    fn assert_snapshot(&self, expected: &str) {
        assert_eq!(self.lua, expected, "Lua snapshot changed for {}", self.name);
    }

    fn assert_invariants(&self) {
        assert_no_same_line_trailing_comments(self.name, &self.lua);
        assert_balanced_double_quotes(self.name, &self.lua);
    }
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
        LuaCase::new("walk_to_driver", LuaCommand::walk_driver_lua().to_string()),
        LuaCase::new(
            "character_status",
            LuaCommand::character_status(&legacy_agent()),
        ),
        LuaCase::new(
            "character_inventory",
            LuaCommand::character_inventory(&legacy_agent()),
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
            "remove_entity_at",
            LuaCommand::remove_entity_at(pos(24.0, 25.0)),
        ),
        LuaCase::new("remove_entity", LuaCommand::remove_entity(43)),
        LuaCase::new("rotate_entity", LuaCommand::rotate_entity(44, 4)),
        LuaCase::new(
            "insert_items",
            LuaCommand::insert_items(45, "coal", 5, "fuel"),
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
            LuaCommand::create_native_blueprint(area()),
        ),
        LuaCase::new(
            "save_blueprint",
            LuaCommand::save_blueprint("starter", area()),
        ),
        LuaCase::new("list_blueprints", LuaCommand::list_blueprints()),
        LuaCase::new("get_blueprint", LuaCommand::get_blueprint("starter")),
        LuaCase::new(
            "place_blueprint",
            LuaCommand::place_blueprint("starter", pos(26.0, 27.0), 4),
        ),
        LuaCase::new(
            "import_blueprint",
            LuaCommand::import_blueprint("0eNq-test", pos(28.0, 29.0), 8),
        ),
        LuaCase::new("delete_blueprint", LuaCommand::delete_blueprint("starter")),
        LuaCase::new("register_chat_handler", LuaCommand::register_chat_handler()),
        LuaCase::new(
            "get_and_clear_chat_messages",
            LuaCommand::get_and_clear_chat_messages(),
        ),
        LuaCase::new("get_research_status", LuaCommand::get_research_status()),
        LuaCase::new(
            "get_available_research",
            LuaCommand::get_available_research(&legacy_agent()),
        ),
        LuaCase::new("start_research", LuaCommand::start_research("automation")),
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
            "get_power_coverage",
            LuaCommand::get_power_coverage(36, 37, 13),
        ),
        LuaCase::new("get_alerts", LuaCommand::get_alerts(38, 39, 14)),
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
fn corrected_inventory_readers_document_factorio_2_get_contents_shape() {
    for case in [
        LuaCase::new(
            "character_inventory",
            LuaCommand::character_inventory(&legacy_agent()),
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
            "get_available_research",
            LuaCommand::get_available_research(&legacy_agent()),
        ),
        LuaCase::new(
            "clear_area",
            LuaCommand::clear_area(&legacy_agent(), area(), true, true, false),
        ),
    ] {
        assert_uses_factorio_2_get_contents_shape(case.name, &case.lua);
    }
}

#[test]
fn named_walk_uses_the_shared_driver_target_without_walking_state() {
    let agent = named_agent();
    let walk_character = LuaCommand::walk_character(&agent, pos(12.0, 13.0));
    let walk_target = LuaCommand::set_walk_target(&agent, pos(12.0, 13.0));

    for (name, lua) in [
        ("walk_character", walk_character.as_str()),
        ("walk_target", walk_target.as_str()),
    ] {
        assert!(
            lua.contains(r#"storage.factorioctl_walk_targets["doug"] = { x = 12, y = 13"#),
            "{name} should store the named agent target for the on_tick driver"
        );
        assert!(
            !lua.contains("walking = true") && !lua.contains("walking=true"),
            "{name} should not start named orphan agents by relying on walking_state"
        );
    }

    let driver = LuaCommand::walk_driver_lua();
    assert!(
        driver.contains("tgt.stuck_ticks") && driver.contains("tgt.expires_tick"),
        "walk driver should clear targets after bounded no-progress/expiry guards"
    );
}

#[test]
fn research_readiness_counts_resolved_character_science_in_totals() {
    let lua = LuaCommand::get_available_research(&named_agent());

    let inventory_fold = lua
        .find("science_totals[item.name] = (science_totals[item.name] or 0) + item.count")
        .expect("character science should be folded into science_totals");
    let readiness_read = lua
        .find("local have = science_totals[ing.name] or 0")
        .expect("research readiness should read science_totals");
    assert!(
        inventory_fold < readiness_read,
        "character science must be folded before readiness is calculated"
    );
}

#[test]
fn get_entity_inventory_remains_a_known_pre_factorio_2_reader_for_cjf_2() {
    let lua = LuaCommand::get_entity_inventory(42);

    assert!(
        lua.contains("local contents = inv.get_contents()")
            && lua.contains("for item, count in pairs(contents) do"),
        "get_entity_inventory should remain visibly covered as the cjf.2 known-gap until that bead fixes runtime Lua"
    );
}

#[test]
fn inventory_and_crafting_lua_snapshots_are_stable() {
    LuaCase::new("character_inventory", LuaCommand::character_inventory(&legacy_agent())).assert_snapshot(
        r#"storage.factorioctl_characters = storage.factorioctl_characters or {}
local c = nil
for _, p in pairs(game.connected_players) do if p.character and p.character.valid then c = p.character break end end
if not c then c = storage.factorioctl_characters["__player__"] end
if c and c.valid then
    local inv = c.get_main_inventory()
    local items = {}
    local free_slots = 0
    if inv then
        for _, item in pairs(inv.get_contents()) do
            table.insert(items, { name = item.name, count = item.count })
        end
        free_slots = inv.count_empty_stacks() or 0
    end
    if #items == 0 then
        rcon.print('{"items": [], "free_slots": ' .. tostring(free_slots) .. '}')
    else
        rcon.print(helpers.table_to_json({ items = items, free_slots = free_slots }))
    end
else
    rcon.print('{"items": [], "free_slots": 0}')
end"#,
    );

    LuaCase::new("craft", LuaCommand::craft(&legacy_agent(), "iron-gear-wheel", 4)).assert_snapshot(
        r#"storage.factorioctl_characters = storage.factorioctl_characters or {}
local c = nil
for _, p in pairs(game.connected_players) do if p.character and p.character.valid then c = p.character break end end
if not c then c = storage.factorioctl_characters["__player__"] end
if not (c and c.valid) then rcon.print('{"error":"no character for agent __player__; spawn first"}') return end

local crafted = c.begin_crafting{ recipe = "iron-gear-wheel", count = 4 }

-- Build queue info
local queue = {}
for i, item in pairs(c.crafting_queue) do
    table.insert(queue, { recipe = item.recipe, count = item.count })
end

rcon.print(helpers.table_to_json({
    success = crafted > 0,
    queued = crafted,
    queue_size = c.crafting_queue_size,
    queue = queue
}))"#,
    );
}

#[test]
fn placement_and_mining_lua_snapshots_are_stable() {
    LuaCase::new(
        "place_ghost",
        LuaCommand::place_ghost(&legacy_agent(), "stone-furnace", pos(22.0, 23.0), Direction::West),
    )
    .assert_snapshot(
        r#"storage.factorioctl_characters = storage.factorioctl_characters or {}
local c = nil
for _, p in pairs(game.connected_players) do if p.character and p.character.valid then c = p.character break end end
if not c then c = storage.factorioctl_characters["__player__"] end
if not (c and c.valid) then rcon.print('{"error":"no character for agent __player__; spawn first"}') return end

-- Create ghost entity (doesn't require items in inventory)
local e = game.surfaces[1].create_entity{
    name = "entity-ghost",
    inner_name = "stone-furnace",
    position = { 22, 23 },
    direction = 12,
    force = c.force
}

if e then
    storage.factorioctl_entities = storage.factorioctl_entities or {}
storage.factorioctl_entities[e.unit_number] = e
    rcon.print(helpers.table_to_json({
        unit_number = e.unit_number,
        name = e.ghost_name or "stone-furnace",
        entity_type = "entity-ghost",
        position = { x = e.position.x, y = e.position.y },
        direction = e.direction,
        health = e.health,
        force = e.force.name
    }))
else
    rcon.print('{"error": "Failed to create ghost"}')
end"#,
    );

    LuaCase::new("start_mining", LuaCommand::start_mining(&legacy_agent(), pos(14.0, 15.0))).assert_snapshot(
        r#"storage.factorioctl_characters = storage.factorioctl_characters or {}
local c = nil
for _, p in pairs(game.connected_players) do if p.character and p.character.valid then c = p.character break end end
if not c then c = storage.factorioctl_characters["__player__"] end
if not (c and c.valid) then rcon.print('{"error":"no character for agent __player__; spawn first"}') return end

-- Find a minable entity at the position
local target = nil
local resources = game.surfaces[1].find_entities_filtered{
    position = { 14, 15 },
    radius = 1,
    type = "resource"
}
if #resources > 0 then
    target = resources[1]
else
    local entities = game.surfaces[1].find_entities_filtered{
        position = { 14, 15 },
        radius = 1
    }
    for _, e in pairs(entities) do
        if e.minable and e ~= c then
            target = e
            break
        end
    end
end

if not target then
    rcon.print('{"success": false, "error": "No minable entity at position"}')
    return
end

-- Check if in range
local dx = target.position.x - c.position.x
local dy = target.position.y - c.position.y
local dist = math.sqrt(dx*dx + dy*dy)
if dist > c.resource_reach_distance + 0.5 then
    rcon.print('{"success": false, "error": "Too far", "distance": ' .. dist .. ', "reach": ' .. c.resource_reach_distance .. '}')
    return
end

-- Start mining
c.mining_state = { mining = true, position = target.position }
rcon.print('{"success": true, "target": "' .. target.name .. '", "position": {\"x\": ' .. target.position.x .. ', \"y\": ' .. target.position.y .. '}}')"#,
    );
}

#[test]
fn recipe_prototype_blueprint_and_research_snapshots_are_stable() {
    LuaCase::new("get_recipe", LuaCommand::get_recipe("iron-plate")).assert_snapshot(
        r#"local recipe = prototypes.recipe["iron-plate"]
if recipe then
    local ingredients = {}
    for _, ing in pairs(recipe.ingredients) do
        table.insert(ingredients, {
            type = ing.type,
            name = ing.name,
            amount = ing.amount
        })
    end
    local products = {}
    for _, prod in pairs(recipe.products) do
        table.insert(products, {
            type = prod.type,
            name = prod.name,
            amount = prod.amount,
            probability = prod.probability
        })
    end
    rcon.print(helpers.table_to_json({
        name = recipe.name,
        category = recipe.category,
        energy = recipe.energy,
        ingredients = ingredients,
        products = products
    }))
else
    rcon.print('{"error": "Recipe not found"}')
end"#,
    );

    LuaCase::new(
        "get_prototype",
        LuaCommand::get_prototype("assembling-machine-1"),
    )
    .assert_snapshot(
        r#"local proto = prototypes.entity["assembling-machine-1"]
if proto then
    local result = {
        name = proto.name,
        type = proto.type,
    }

    -- Helper to safely get property
    local function try_get(fn)
        local ok, val = pcall(fn)
        if ok then return val end
        return nil
    end

    -- Calculate size from collision box
    local cb = try_get(function() return proto.collision_box end)
    if cb then
        result.size = {
            cb.right_bottom.x - cb.left_top.x,
            cb.right_bottom.y - cb.left_top.y
        }
    end

    -- Crafting machine properties (use method for speed)
    local craft_speed = try_get(function() return proto.get_crafting_speed() end)
    if craft_speed then
        result.crafting_speed = craft_speed
    end
    local craft_cats = try_get(function() return proto.crafting_categories end)
    if craft_cats then
        result.crafting_categories = {}
        for cat, _ in pairs(craft_cats) do
            table.insert(result.crafting_categories, cat)
        end
    end

    -- Mining drill properties
    local mining_speed = try_get(function() return proto.mining_speed end)
    if mining_speed then
        result.mining_speed = mining_speed
    end
    local res_cats = try_get(function() return proto.resource_categories end)
    if res_cats then
        result.resource_categories = {}
        for cat, _ in pairs(res_cats) do
            table.insert(result.resource_categories, cat)
        end
    end

    -- Inserter properties
    local rot_speed = try_get(function() return proto.inserter_rotation_speed end)
    if rot_speed then
        result.rotation_speed = rot_speed
    end
    local ext_speed = try_get(function() return proto.inserter_extension_speed end)
    if ext_speed then
        result.extension_speed = ext_speed
    end

    -- Belt properties
    local belt_speed = try_get(function() return proto.belt_speed end)
    if belt_speed then
        result.belt_speed = belt_speed
    end

    -- Energy
    local energy = try_get(function() return proto.energy_usage end)
    if energy then
        result.energy_usage = energy
    end

    -- Energy source
    if try_get(function() return proto.burner_prototype end) then
        result.energy_source = "burner"
    elseif try_get(function() return proto.electric_energy_source_prototype end) then
        result.energy_source = "electric"
    elseif try_get(function() return proto.heat_energy_source_prototype end) then
        result.energy_source = "heat"
    elseif try_get(function() return proto.void_energy_source_prototype end) then
        result.energy_source = "void"
    end

    rcon.print(helpers.table_to_json(result))
else
    rcon.print('{"error": "Prototype not found"}')
end"#,
    );

    LuaCase::new("get_blueprint", LuaCommand::get_blueprint("starter")).assert_snapshot(
        r#"storage.blueprints = storage.blueprints or {}
local data = storage.blueprints["starter"]
if data then
    rcon.print(helpers.table_to_json({
        blueprint_string = data.string,
        entity_count = data.entity_count
    }))
else
    rcon.print('{"error": "Blueprint not found"}')
end"#,
    );

    LuaCase::new("start_research", LuaCommand::start_research("automation")).assert_snapshot(
        r#"local force = game.forces.player
local surface = game.surfaces[1]
local tech = force.technologies["automation"]

if not tech then
    rcon.print('{"success": false, "error": "Technology not found"}')
    return
end

if tech.researched then
    rcon.print('{"success": false, "error": "Already researched"}')
    return
end

if not tech.enabled then
    rcon.print('{"success": false, "error": "Technology not enabled"}')
    return
end

-- Check prerequisites
for _, prereq in pairs(tech.prerequisites) do
    if not prereq.researched then
        rcon.print('{"success": false, "error": "Prerequisites not met: ' .. prereq.name .. '"}')
        return
    end
end

-- Check for labs
local labs = surface.find_entities_filtered{type = "lab", force = force}
if #labs == 0 then
    rcon.print('{"success": false, "error": "No labs found! Build a lab first (requires: 10 iron-gear-wheel, 10 electronic-circuit, 4 transport-belt)", "action_needed": "build_lab"}')
    return
end

-- Check if any lab has power
local powered_labs = 0
for _, lab in pairs(labs) do
    local status = lab.status
    if status ~= defines.entity_status.no_power and status ~= defines.entity_status.low_power then
        powered_labs = powered_labs + 1
    end
end
if powered_labs == 0 then
    rcon.print('{"success": false, "error": "Labs have no power! Connect labs to power grid.", "action_needed": "power_labs"}')
    return
end

-- Check for required science packs
local ingredients = {}
local missing_packs = {}
local science_in_labs = {}

-- Count science packs in labs
for _, lab in pairs(labs) do
    local inv = lab.get_inventory(defines.inventory.lab_input)
    if inv then
        for i = 1, #inv do
            local stack = inv[i]
            if stack and stack.valid_for_read then
                science_in_labs[stack.name] = (science_in_labs[stack.name] or 0) + stack.count
            end
        end
    end
end

for _, ing in pairs(tech.research_unit_ingredients) do
    table.insert(ingredients, {name = ing.name, amount = ing.amount})
    local have = science_in_labs[ing.name] or 0
    if have < ing.amount then
        table.insert(missing_packs, ing.name .. " (need " .. ing.amount .. ", have " .. have .. " in labs)")
    end
end

if #missing_packs > 0 then
    rcon.print(helpers.table_to_json({
        success = false,
        error = "Missing science packs in labs: " .. table.concat(missing_packs, ", "),
        action_needed = "insert_science_packs",
        required_packs = ingredients,
        hint = "Craft the required science packs and insert them into your labs"
    }))
    return
end

-- Queue the research properly (not cheating)
local added = force.add_research(tech)
if added then
    rcon.print(helpers.table_to_json({
        success = true,
        name = tech.name,
        research_unit_count = tech.research_unit_count,
        ingredients = ingredients,
        message = "Research queued! Labs will now consume science packs to progress."
    }))
else
    rcon.print('{"success": false, "error": "Failed to queue research - check if another research is in progress"}')
end"#,
    );
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
fn resolve_helpers_match_spec_snapshots() {
    let named = AgentId::new(Some("doug")).unwrap();
    let legacy = AgentId::new(None).unwrap();

    LuaCase::new("resolve_required_named", LuaCommand::resolve_required(&named)).assert_snapshot(
        r#"storage.factorioctl_characters = storage.factorioctl_characters or {}
local c = storage.factorioctl_characters["doug"]
if not (c and c.valid) then rcon.print('{"error":"no character for agent doug; spawn first"}') return end"#,
    );

    LuaCase::new("resolve_required_legacy", LuaCommand::resolve_required(&legacy)).assert_snapshot(
        r#"storage.factorioctl_characters = storage.factorioctl_characters or {}
local c = nil
for _, p in pairs(game.connected_players) do if p.character and p.character.valid then c = p.character break end end
if not c then c = storage.factorioctl_characters["__player__"] end
if not (c and c.valid) then rcon.print('{"error":"no character for agent __player__; spawn first"}') return end"#,
    );

    LuaCase::new(
        "resolve_optional_named",
        LuaCommand::resolve_optional(&named),
    )
    .assert_snapshot(
        r#"storage.factorioctl_characters = storage.factorioctl_characters or {}
local c = storage.factorioctl_characters["doug"]"#,
    );

    LuaCase::new("resolve_optional_legacy", LuaCommand::resolve_optional(&legacy)).assert_snapshot(
        r#"storage.factorioctl_characters = storage.factorioctl_characters or {}
local c = nil
for _, p in pairs(game.connected_players) do if p.character and p.character.valid then c = p.character break end end
if not c then c = storage.factorioctl_characters["__player__"] end"#,
    );
}

#[test]
fn static_builder_tests_cover_named_legacy_extract_and_registry_contracts() {
    let named = named_agent();
    let legacy = legacy_agent();

    let named_lua = LuaCommand::walk_character(&named, pos(12.0, 13.0));
    assert!(named_lua.contains("storage.factorioctl_characters[\"doug\"]"));
    assert!(!named_lua.contains("connected_players"));
    assert!(!named_lua.contains("global."));

    let legacy_lua = LuaCommand::walk_character(&legacy, pos(12.0, 13.0));
    assert!(legacy_lua.contains("for _, p in pairs(game.connected_players) do"));
    assert!(legacy_lua.contains("storage.factorioctl_characters[\"__player__\"]"));

    let extract_lua = LuaCommand::extract_items(&named, 46, "iron-ore", 6, "chest");
    assert!(extract_lua.contains("local player_inv = c.get_main_inventory()"));
    assert!(!extract_lua.contains("game.players[1]"));

    for lua in [
        LuaCommand::get_entity_inventory(42),
        LuaCommand::extract_items(&named, 46, "iron-ore", 6, "chest"),
        LuaCommand::set_recipe(47, "copper-cable"),
    ] {
        assert!(lua.contains("storage.factorioctl_entities["));
    }
}
