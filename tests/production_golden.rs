use factorioctl::world::{build_production_report, EntityProduction, Position};
use serde_json::json;

fn production(
    name: &str,
    x: f64,
    y: f64,
    status: &str,
    products_finished: Option<u64>,
    working: bool,
) -> EntityProduction {
    EntityProduction {
        name: name.to_string(),
        unit_number: None,
        position: Position::new(x, y),
        status: status.to_string(),
        products_finished,
        working,
    }
}

#[test]
fn production_report_is_compact_and_aggregated() {
    let report = build_production_report(vec![
        production("burner-mining-drill", 10.5, -3.5, "working", Some(42), true),
        production("stone-furnace", 12.5, -3.5, "no_fuel", Some(7), false),
        production(
            "assembling-machine-1",
            14.0,
            -2.0,
            "full_output",
            Some(3),
            false,
        ),
        production("electric-mining-drill", 8.5, -4.5, "no_power", None, false),
        production("lab", 16.0, -2.5, "working", None, true),
    ]);

    assert_eq!(
        serde_json::to_value(&report).expect("production report JSON"),
        json!({
            "entities": [
                {
                    "name": "burner-mining-drill",
                    "unit_number": null,
                    "position": { "x": 10.5, "y": -3.5 },
                    "status": "working",
                    "products_finished": 42,
                    "working": true
                },
                {
                    "name": "stone-furnace",
                    "unit_number": null,
                    "position": { "x": 12.5, "y": -3.5 },
                    "status": "no_fuel",
                    "products_finished": 7,
                    "working": false
                },
                {
                    "name": "assembling-machine-1",
                    "unit_number": null,
                    "position": { "x": 14.0, "y": -2.0 },
                    "status": "full_output",
                    "products_finished": 3,
                    "working": false
                },
                {
                    "name": "electric-mining-drill",
                    "unit_number": null,
                    "position": { "x": 8.5, "y": -4.5 },
                    "status": "no_power",
                    "products_finished": null,
                    "working": false
                },
                {
                    "name": "lab",
                    "unit_number": null,
                    "position": { "x": 16.0, "y": -2.5 },
                    "status": "working",
                    "products_finished": null,
                    "working": true
                }
            ],
            "status_counts": {
                "full_output": 1,
                "no_fuel": 1,
                "no_power": 1,
                "working": 2
            },
            "working_count": 2,
            "total": 5
        })
    );
}
