use factorioctl::world::{
    build_situation_report, Area, Entity, InventoryItem, Position, ResourcePatch,
};
use serde_json::json;

fn entity(name: &str, x: f64, y: f64) -> Entity {
    Entity {
        unit_number: None,
        name: name.to_string(),
        entity_type: None,
        position: Position::new(x, y),
        direction: 0,
        health: None,
        force: None,
        bounding_box: None,
        pickup_position: None,
        drop_position: None,
        belt_to_ground_type: None,
        underground_belt_neighbour: None,
        belt_input_neighbours: Vec::new(),
        belt_output_neighbours: Vec::new(),
        belt_neighbours_observed: false,
    }
}

fn resource(
    name: &str,
    center_x: f64,
    center_y: f64,
    total_amount: u64,
    tile_count: u32,
) -> ResourcePatch {
    ResourcePatch {
        name: name.to_string(),
        total_amount,
        tile_count,
        center: Position::new(center_x, center_y),
        bounding_box: Area::new(
            center_x - 1.0,
            center_y - 1.0,
            center_x + 1.0,
            center_y + 1.0,
        ),
    }
}

#[test]
fn situation_report_is_compact_sorted_and_aggregated() {
    let report = build_situation_report(
        Position::new(12.5, -3.25),
        Some(92.0),
        Some(true),
        12_345,
        vec![
            InventoryItem {
                name: "stone".to_string(),
                count: 8,
            },
            InventoryItem {
                name: "iron-plate".to_string(),
                count: 24,
            },
            InventoryItem {
                name: "coal".to_string(),
                count: 24,
            },
        ],
        vec![
            entity("transport-belt", 11.0, -3.0),
            entity("stone-furnace", 10.0, -4.0),
            entity("transport-belt", 13.0, -2.0),
        ],
        vec![
            resource("iron-ore", 20.5, -1.5, 7_500, 12),
            resource("coal", 6.0, -8.0, 2_000, 4),
        ],
        32,
    );

    assert_eq!(
        serde_json::to_value(&report).expect("situation report JSON"),
        json!({
            "position": { "x": 12.5, "y": -3.25 },
            "health": 92.0,
            "walking": true,
            "tick": 12345,
            "inventory": [
                { "name": "coal", "count": 24 },
                { "name": "iron-plate", "count": 24 },
                { "name": "stone", "count": 8 }
            ],
            "nearby_entities": {
                "stone-furnace": 1,
                "transport-belt": 2
            },
            "nearby_resources": [
                {
                    "name": "iron-ore",
                    "center_x": 20.5,
                    "center_y": -1.5,
                    "total_amount": 7500,
                    "tile_count": 12
                },
                {
                    "name": "coal",
                    "center_x": 6.0,
                    "center_y": -8.0,
                    "total_amount": 2000,
                    "tile_count": 4
                }
            ],
            "radius": 32
        })
    );
}
