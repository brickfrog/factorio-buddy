"""Factorio MCP tool parameter schemas used by bridge PreToolUse guards."""

from models import (
    TOOL_PARAM_BOOLEAN,
    TOOL_PARAM_INTEGER,
    TOOL_PARAM_NUMBER,
    TOOL_PARAM_STRING,
    ToolParamSchemaRegistry,
)


FACTORIO_TOOL_PARAM_SCHEMA_REGISTRY = ToolParamSchemaRegistry.from_mapping({
    "send_chat_response": {
        "required": {
            "player_index": TOOL_PARAM_INTEGER,
            "agent_name": TOOL_PARAM_STRING,
            "text": TOOL_PARAM_STRING,
        },
    },
    "tool_status": {
        "required": {
            "player_index": TOOL_PARAM_INTEGER,
            "agent_name": TOOL_PARAM_STRING,
            "tool_name": TOOL_PARAM_STRING,
        },
    },
    "set_status": {
        "required": {
            "player_index": TOOL_PARAM_INTEGER,
            "status": TOOL_PARAM_STRING,
        },
    },
    "register_agent": {
        "required": {"agent_name": TOOL_PARAM_STRING},
        "optional": {"label": TOOL_PARAM_STRING},
    },
    "unregister_agent": {
        "required": {"agent_name": TOOL_PARAM_STRING},
    },
    "ensure_surface": {
        "required": {"planet": TOOL_PARAM_STRING},
    },
    "place_character": {
        "required": {
            "agent_name": TOOL_PARAM_STRING,
            "planet": TOOL_PARAM_STRING,
            "spawn_x": TOOL_PARAM_NUMBER,
        },
    },
    "set_spectator_mode": {
        "required": {"enabled": TOOL_PARAM_BOOLEAN},
    },
    "ping": {
        "required": {},
    },
    "live_state": {
        "required": {"agent_name": TOOL_PARAM_STRING},
    },
    "connected_player_count": {
        "required": {},
    },
    "eval_production_snapshot": {
        "required": {},
        "optional": {"surface_name": TOOL_PARAM_STRING},
    },
    "walk_to": {
        "required": {"x": TOOL_PARAM_NUMBER, "y": TOOL_PARAM_NUMBER},
    },
    "place_entity": {
        "required": {
            "entity_name": TOOL_PARAM_STRING,
            "x": TOOL_PARAM_NUMBER,
            "y": TOOL_PARAM_NUMBER,
        },
        "optional": {"direction": TOOL_PARAM_STRING},
    },
    "check_placement": {
        "required": {
            "entity_name": TOOL_PARAM_STRING,
            "x": TOOL_PARAM_NUMBER,
            "y": TOOL_PARAM_NUMBER,
        },
        "optional": {"direction": TOOL_PARAM_STRING},
    },
    "find_entity_placements": {
        "required": {
            "entity_name": TOOL_PARAM_STRING,
            "x": TOOL_PARAM_NUMBER,
            "y": TOOL_PARAM_NUMBER,
        },
        "optional": {
            "radius": TOOL_PARAM_INTEGER,
            "limit": TOOL_PARAM_INTEGER,
        },
    },
    "mine_at": {
        "required": {"x": TOOL_PARAM_NUMBER, "y": TOOL_PARAM_NUMBER},
        "optional": {"count": TOOL_PARAM_INTEGER},
    },
    "craft": {
        "required": {"recipe": TOOL_PARAM_STRING},
        "optional": {"count": TOOL_PARAM_INTEGER},
    },
    "insert_items": {
        "required": {
            "unit_number": TOOL_PARAM_INTEGER,
            "item": TOOL_PARAM_STRING,
            "count": TOOL_PARAM_INTEGER,
        },
        "optional": {"inventory_type": TOOL_PARAM_STRING},
    },
    "extract_items": {
        "required": {
            "unit_number": TOOL_PARAM_INTEGER,
            "item": TOOL_PARAM_STRING,
            "count": TOOL_PARAM_INTEGER,
        },
        "optional": {"inventory_type": TOOL_PARAM_STRING},
    },
    "bootstrap_smelting_once": {
        "required": {
            "furnace_unit_number": TOOL_PARAM_INTEGER,
        },
        "optional": {
            "fuel_item": TOOL_PARAM_STRING,
            "fuel_count": TOOL_PARAM_INTEGER,
            "source_item": TOOL_PARAM_STRING,
            "source_count": TOOL_PARAM_INTEGER,
            "output_item": TOOL_PARAM_STRING,
            "output_count": TOOL_PARAM_INTEGER,
            "craft_recipe": TOOL_PARAM_STRING,
            "craft_count": TOOL_PARAM_INTEGER,
            "wait_ticks": TOOL_PARAM_INTEGER,
            "verify_radius": TOOL_PARAM_INTEGER,
            "dry_run": TOOL_PARAM_BOOLEAN,
        },
    },
    "build_fuel_supply": {
        "required": {
            "consumer_unit_number": TOOL_PARAM_INTEGER,
            "from_x": TOOL_PARAM_NUMBER,
            "from_y": TOOL_PARAM_NUMBER,
            "pickup_x": TOOL_PARAM_NUMBER,
            "pickup_y": TOOL_PARAM_NUMBER,
            "inserter_x": TOOL_PARAM_NUMBER,
            "inserter_y": TOOL_PARAM_NUMBER,
            "inserter_direction": TOOL_PARAM_STRING,
        },
        "optional": {
            "inserter_name": TOOL_PARAM_STRING,
            "inserter_fuel_item": TOOL_PARAM_STRING,
            "inserter_fuel_count": TOOL_PARAM_INTEGER,
            "belt_type": TOOL_PARAM_STRING,
            "search_radius": TOOL_PARAM_INTEGER,
            "dry_run": TOOL_PARAM_BOOLEAN,
            "respect_zones": TOOL_PARAM_BOOLEAN,
            "allow_underground": TOOL_PARAM_BOOLEAN,
            "extend_existing": TOOL_PARAM_BOOLEAN,
            "verify_radius": TOOL_PARAM_INTEGER,
        },
    },
    "repair_fuel_sustainability": {
        "required": {},
        "optional": {
            "x": TOOL_PARAM_NUMBER,
            "y": TOOL_PARAM_NUMBER,
            "radius": TOOL_PARAM_INTEGER,
            "limit": TOOL_PARAM_INTEGER,
            "search_radius": TOOL_PARAM_INTEGER,
            "dry_run": TOOL_PARAM_BOOLEAN,
            "respect_zones": TOOL_PARAM_BOOLEAN,
            "allow_underground": TOOL_PARAM_BOOLEAN,
            "extend_existing": TOOL_PARAM_BOOLEAN,
            "verify_radius": TOOL_PARAM_INTEGER,
        },
    },
    "feed_lab_from_inventory": {
        "required": {
            "lab_unit_number": TOOL_PARAM_INTEGER,
            "science_pack": TOOL_PARAM_STRING,
            "count": TOOL_PARAM_INTEGER,
        },
        "optional": {"dry_run": TOOL_PARAM_BOOLEAN},
    },
    "plan_automation_science": {
        "required": {
            "assembler_unit_number": TOOL_PARAM_INTEGER,
            "lab_unit_number": TOOL_PARAM_INTEGER,
            "gear_from_x": TOOL_PARAM_NUMBER,
            "gear_from_y": TOOL_PARAM_NUMBER,
            "copper_from_x": TOOL_PARAM_NUMBER,
            "copper_from_y": TOOL_PARAM_NUMBER,
        },
        "optional": {
            "gear_side": TOOL_PARAM_STRING,
            "copper_side": TOOL_PARAM_STRING,
            "output_side": TOOL_PARAM_STRING,
            "lab_side": TOOL_PARAM_STRING,
            "belt_type": TOOL_PARAM_STRING,
            "search_radius": TOOL_PARAM_INTEGER,
            "respect_zones": TOOL_PARAM_BOOLEAN,
            "allow_underground": TOOL_PARAM_BOOLEAN,
            "extend_existing": TOOL_PARAM_BOOLEAN,
            "verify_radius": TOOL_PARAM_INTEGER,
        },
    },
    "build_automation_science": {
        "required": {
            "assembler_unit_number": TOOL_PARAM_INTEGER,
            "lab_unit_number": TOOL_PARAM_INTEGER,
            "gear_from_x": TOOL_PARAM_NUMBER,
            "gear_from_y": TOOL_PARAM_NUMBER,
            "gear_pickup_x": TOOL_PARAM_NUMBER,
            "gear_pickup_y": TOOL_PARAM_NUMBER,
            "gear_inserter_x": TOOL_PARAM_NUMBER,
            "gear_inserter_y": TOOL_PARAM_NUMBER,
            "gear_inserter_direction": TOOL_PARAM_STRING,
            "copper_from_x": TOOL_PARAM_NUMBER,
            "copper_from_y": TOOL_PARAM_NUMBER,
            "copper_pickup_x": TOOL_PARAM_NUMBER,
            "copper_pickup_y": TOOL_PARAM_NUMBER,
            "copper_inserter_x": TOOL_PARAM_NUMBER,
            "copper_inserter_y": TOOL_PARAM_NUMBER,
            "copper_inserter_direction": TOOL_PARAM_STRING,
            "science_drop_x": TOOL_PARAM_NUMBER,
            "science_drop_y": TOOL_PARAM_NUMBER,
            "science_to_x": TOOL_PARAM_NUMBER,
            "science_to_y": TOOL_PARAM_NUMBER,
            "output_inserter_x": TOOL_PARAM_NUMBER,
            "output_inserter_y": TOOL_PARAM_NUMBER,
            "output_inserter_direction": TOOL_PARAM_STRING,
            "lab_from_x": TOOL_PARAM_NUMBER,
            "lab_from_y": TOOL_PARAM_NUMBER,
            "lab_pickup_x": TOOL_PARAM_NUMBER,
            "lab_pickup_y": TOOL_PARAM_NUMBER,
            "lab_inserter_x": TOOL_PARAM_NUMBER,
            "lab_inserter_y": TOOL_PARAM_NUMBER,
            "lab_inserter_direction": TOOL_PARAM_STRING,
        },
        "optional": {
            "belt_type": TOOL_PARAM_STRING,
            "search_radius": TOOL_PARAM_INTEGER,
            "dry_run": TOOL_PARAM_BOOLEAN,
            "respect_zones": TOOL_PARAM_BOOLEAN,
            "allow_underground": TOOL_PARAM_BOOLEAN,
            "extend_existing": TOOL_PARAM_BOOLEAN,
            "verify_radius": TOOL_PARAM_INTEGER,
        },
    },
    "plan_machine_output": {
        "required": {
            "source_unit_number": TOOL_PARAM_INTEGER,
            "item_name": TOOL_PARAM_STRING,
            "to_x": TOOL_PARAM_NUMBER,
            "to_y": TOOL_PARAM_NUMBER,
        },
        "optional": {
            "output_side": TOOL_PARAM_STRING,
            "belt_type": TOOL_PARAM_STRING,
            "search_radius": TOOL_PARAM_INTEGER,
            "respect_zones": TOOL_PARAM_BOOLEAN,
            "allow_underground": TOOL_PARAM_BOOLEAN,
            "extend_existing": TOOL_PARAM_BOOLEAN,
            "verify_radius": TOOL_PARAM_INTEGER,
        },
    },
    "build_assembler_output": {
        "required": {
            "assembler_unit_number": TOOL_PARAM_INTEGER,
            "item_name": TOOL_PARAM_STRING,
            "drop_x": TOOL_PARAM_NUMBER,
            "drop_y": TOOL_PARAM_NUMBER,
            "to_x": TOOL_PARAM_NUMBER,
            "to_y": TOOL_PARAM_NUMBER,
            "inserter_x": TOOL_PARAM_NUMBER,
            "inserter_y": TOOL_PARAM_NUMBER,
            "inserter_direction": TOOL_PARAM_STRING,
        },
        "optional": {
            "belt_type": TOOL_PARAM_STRING,
            "search_radius": TOOL_PARAM_INTEGER,
            "dry_run": TOOL_PARAM_BOOLEAN,
            "respect_zones": TOOL_PARAM_BOOLEAN,
            "allow_underground": TOOL_PARAM_BOOLEAN,
            "extend_existing": TOOL_PARAM_BOOLEAN,
            "verify_radius": TOOL_PARAM_INTEGER,
        },
    },
    "plan_recipe_assembler_cell": {
        "required": {
            "assembler_unit_number": TOOL_PARAM_INTEGER,
            "recipe": TOOL_PARAM_STRING,
            "input_item_name": TOOL_PARAM_STRING,
            "output_item_name": TOOL_PARAM_STRING,
            "input_from_x": TOOL_PARAM_NUMBER,
            "input_from_y": TOOL_PARAM_NUMBER,
            "output_to_x": TOOL_PARAM_NUMBER,
            "output_to_y": TOOL_PARAM_NUMBER,
        },
        "optional": {
            "input_side": TOOL_PARAM_STRING,
            "output_side": TOOL_PARAM_STRING,
            "belt_type": TOOL_PARAM_STRING,
            "search_radius": TOOL_PARAM_INTEGER,
            "respect_zones": TOOL_PARAM_BOOLEAN,
            "allow_underground": TOOL_PARAM_BOOLEAN,
            "extend_existing": TOOL_PARAM_BOOLEAN,
            "verify_radius": TOOL_PARAM_INTEGER,
        },
    },
    "build_recipe_assembler_cell": {
        "required": {
            "assembler_unit_number": TOOL_PARAM_INTEGER,
            "recipe": TOOL_PARAM_STRING,
            "input_item_name": TOOL_PARAM_STRING,
            "output_item_name": TOOL_PARAM_STRING,
            "input_from_x": TOOL_PARAM_NUMBER,
            "input_from_y": TOOL_PARAM_NUMBER,
            "input_pickup_x": TOOL_PARAM_NUMBER,
            "input_pickup_y": TOOL_PARAM_NUMBER,
            "input_inserter_x": TOOL_PARAM_NUMBER,
            "input_inserter_y": TOOL_PARAM_NUMBER,
            "input_inserter_direction": TOOL_PARAM_STRING,
            "output_drop_x": TOOL_PARAM_NUMBER,
            "output_drop_y": TOOL_PARAM_NUMBER,
            "output_to_x": TOOL_PARAM_NUMBER,
            "output_to_y": TOOL_PARAM_NUMBER,
            "output_inserter_x": TOOL_PARAM_NUMBER,
            "output_inserter_y": TOOL_PARAM_NUMBER,
            "output_inserter_direction": TOOL_PARAM_STRING,
        },
        "optional": {
            "belt_type": TOOL_PARAM_STRING,
            "search_radius": TOOL_PARAM_INTEGER,
            "dry_run": TOOL_PARAM_BOOLEAN,
            "respect_zones": TOOL_PARAM_BOOLEAN,
            "allow_underground": TOOL_PARAM_BOOLEAN,
            "extend_existing": TOOL_PARAM_BOOLEAN,
            "verify_radius": TOOL_PARAM_INTEGER,
        },
    },
    "route_belt": {
        "required": {
            "from_x": TOOL_PARAM_NUMBER,
            "from_y": TOOL_PARAM_NUMBER,
            "to_x": TOOL_PARAM_NUMBER,
            "to_y": TOOL_PARAM_NUMBER,
        },
        "optional": {
            "belt_type": TOOL_PARAM_STRING,
            "search_radius": TOOL_PARAM_INTEGER,
            "dry_run": TOOL_PARAM_BOOLEAN,
            "respect_zones": TOOL_PARAM_BOOLEAN,
            "allow_underground": TOOL_PARAM_BOOLEAN,
            "extend_existing": TOOL_PARAM_BOOLEAN,
        },
    },
    "get_entities": {
        "required": {"x": TOOL_PARAM_INTEGER, "y": TOOL_PARAM_INTEGER},
        "optional": {
            "radius": TOOL_PARAM_INTEGER,
            "name": TOOL_PARAM_STRING,
            "entity_type": TOOL_PARAM_STRING,
            "limit": TOOL_PARAM_INTEGER,
        },
    },
    "get_resources": {
        "required": {"x": TOOL_PARAM_INTEGER, "y": TOOL_PARAM_INTEGER},
        "optional": {
            "radius": TOOL_PARAM_INTEGER,
            "resource_type": TOOL_PARAM_STRING,
        },
    },
    "find_nearest_resource": {
        "required": {"resource_type": TOOL_PARAM_STRING},
        "optional": {"x": TOOL_PARAM_NUMBER, "y": TOOL_PARAM_NUMBER},
    },
    "get_recipe": {
        "required": {"name": TOOL_PARAM_STRING},
    },
    "get_recipes_for_item": {
        "required": {"item": TOOL_PARAM_STRING},
    },
    "get_recipes_by_category": {
        "required": {"category": TOOL_PARAM_STRING},
    },
    "set_recipe": {
        "required": {
            "unit_number": TOOL_PARAM_INTEGER,
            "recipe": TOOL_PARAM_STRING,
        },
    },
    "remove_entity": {
        "required": {"unit_number": TOOL_PARAM_INTEGER},
    },
    "rotate_entity": {
        "required": {
            "unit_number": TOOL_PARAM_INTEGER,
            "direction": TOOL_PARAM_STRING,
        },
    },
    "get_machine_belt_positions": {
        "required": {"unit_number": TOOL_PARAM_INTEGER},
    },
    "execute_entity_placement_near": {
        "required": {
            "entity_name": TOOL_PARAM_STRING,
            "x": TOOL_PARAM_NUMBER,
            "y": TOOL_PARAM_NUMBER,
        },
        "optional": {
            "radius": TOOL_PARAM_INTEGER,
            "limit": TOOL_PARAM_INTEGER,
            "dry_run": TOOL_PARAM_BOOLEAN,
        },
    },
    "build_edge_miner": {
        "required": {
            "resource_type": TOOL_PARAM_STRING,
            "x": TOOL_PARAM_NUMBER,
            "y": TOOL_PARAM_NUMBER,
        },
        "optional": {
            "radius": TOOL_PARAM_INTEGER,
            "drill_name": TOOL_PARAM_STRING,
            "limit": TOOL_PARAM_INTEGER,
        },
    },
    "execute_edge_miner": {
        "required": {
            "resource_type": TOOL_PARAM_STRING,
            "x": TOOL_PARAM_NUMBER,
            "y": TOOL_PARAM_NUMBER,
        },
        "optional": {
            "radius": TOOL_PARAM_INTEGER,
            "drill_name": TOOL_PARAM_STRING,
            "limit": TOOL_PARAM_INTEGER,
            "dry_run": TOOL_PARAM_BOOLEAN,
            "fuel_item": TOOL_PARAM_STRING,
            "fuel_count": TOOL_PARAM_INTEGER,
            "verify_radius": TOOL_PARAM_INTEGER,
        },
    },
    "build_direct_smelter": {
        "optional": {
            "drill_unit_number": TOOL_PARAM_INTEGER,
            "output_x": TOOL_PARAM_NUMBER,
            "output_y": TOOL_PARAM_NUMBER,
            "output_direction": TOOL_PARAM_STRING,
            "furnace_name": TOOL_PARAM_STRING,
            "inserter_name": TOOL_PARAM_STRING,
            "belt_name": TOOL_PARAM_STRING,
            "radius": TOOL_PARAM_INTEGER,
        },
    },
    "plan_steam_power": {
        "required": {
            "water_x1": TOOL_PARAM_NUMBER,
            "water_y1": TOOL_PARAM_NUMBER,
            "water_x2": TOOL_PARAM_NUMBER,
            "water_y2": TOOL_PARAM_NUMBER,
            "target_x": TOOL_PARAM_NUMBER,
            "target_y": TOOL_PARAM_NUMBER,
        },
    },
    "repair_steam_power": {
        "required": {
            "x": TOOL_PARAM_INTEGER,
            "y": TOOL_PARAM_INTEGER,
            "target_x": TOOL_PARAM_NUMBER,
            "target_y": TOOL_PARAM_NUMBER,
        },
        "optional": {"radius": TOOL_PARAM_INTEGER},
    },
    "extend_power_to": {
        "required": {
            "x": TOOL_PARAM_INTEGER,
            "y": TOOL_PARAM_INTEGER,
            "target_x": TOOL_PARAM_NUMBER,
            "target_y": TOOL_PARAM_NUMBER,
        },
        "optional": {"radius": TOOL_PARAM_INTEGER},
    },
    "diagnose_steam_power": {
        "required": {"x": TOOL_PARAM_INTEGER, "y": TOOL_PARAM_INTEGER},
        "optional": {"radius": TOOL_PARAM_INTEGER},
    },
    "get_power_status": {
        "required": {"x": TOOL_PARAM_INTEGER, "y": TOOL_PARAM_INTEGER},
        "optional": {"radius": TOOL_PARAM_INTEGER},
    },
    "get_power_networks": {
        "required": {"x": TOOL_PARAM_INTEGER, "y": TOOL_PARAM_INTEGER},
        "optional": {"radius": TOOL_PARAM_INTEGER},
    },
    "find_power_issues": {
        "required": {"x": TOOL_PARAM_INTEGER, "y": TOOL_PARAM_INTEGER},
        "optional": {"radius": TOOL_PARAM_INTEGER},
    },
    "get_power_coverage": {
        "required": {"x": TOOL_PARAM_INTEGER, "y": TOOL_PARAM_INTEGER},
        "optional": {"radius": TOOL_PARAM_INTEGER},
    },
    "analyze_inserters": {
        "required": {"x": TOOL_PARAM_INTEGER, "y": TOOL_PARAM_INTEGER},
        "optional": {"radius": TOOL_PARAM_INTEGER},
    },
    "get_alerts": {
        "required": {"x": TOOL_PARAM_INTEGER, "y": TOOL_PARAM_INTEGER},
        "optional": {"radius": TOOL_PARAM_INTEGER},
    },
    "start_research": {
        "required": {"technology": TOOL_PARAM_STRING},
    },
})
