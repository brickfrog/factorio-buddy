import unittest

import eval as eval_harness
from models import EvalMilestoneSpec, EvalProductionSnapshot, EvalResult


class FakeRcon:
    def __init__(self, response):
        self.response = response
        self.commands = []

    def execute(self, command):
        self.commands.append(command)
        return self.response


class ProductionScoreTest(unittest.TestCase):
    def test_empty_score_is_zero(self):
        self.assertEqual(eval_harness.production_score({}), 0.0)

    def test_known_items_are_weighted(self):
        produced = {
            "iron-plate": 10,
            "copper-plate": 4,
            "electronic-circuit": 2,
        }
        expected = (
            10 * eval_harness.VALUES["iron-plate"]
            + 4 * eval_harness.VALUES["copper-plate"]
            + 2 * eval_harness.VALUES["electronic-circuit"]
        )
        self.assertEqual(eval_harness.production_score(produced), expected)

    def test_production_score_accepts_typed_snapshot(self):
        snapshot = EvalProductionSnapshot(
            produced={"iron-plate": 1000},
            rate_per_min={"iron-plate": 2},
        )

        self.assertEqual(
            eval_harness.production_score(snapshot),
            2 * eval_harness.VALUES["iron-plate"],
        )

    def test_unknown_items_are_ignored(self):
        produced = {
            "iron-plate": 5,
            "space-age-widget": 999999,
        }
        self.assertEqual(
            eval_harness.production_score(produced),
            5 * eval_harness.VALUES["iron-plate"],
        )


class EvaluateTest(unittest.TestCase):
    def test_milestone_specs_are_typed_source_of_truth(self):
        self.assertTrue(eval_harness.MILESTONE_SPECS)
        self.assertTrue(all(
            isinstance(spec, EvalMilestoneSpec)
            for spec in eval_harness.MILESTONE_SPECS
        ))
        self.assertEqual(
            [spec.name for spec in eval_harness.MILESTONE_SPECS],
            [name for name, _ in eval_harness.MILESTONES],
        )

    def test_eval_milestone_spec_evaluates_typed_snapshot(self):
        produced = EvalMilestoneSpec.any_produced(
            "starter_ore",
            ("iron-ore", "coal"),
        )
        throughput = EvalMilestoneSpec.rate_at_least(
            "iron_plate_fast",
            "iron-plate",
            16,
        )
        snapshot = EvalProductionSnapshot(
            produced={"coal": 1},
            rate_per_min={"iron-plate": 16},
        )

        self.assertTrue(produced.reached(snapshot))
        self.assertTrue(throughput.reached(snapshot))

    def test_evaluate_model_returns_typed_result_and_accepts_typed_snapshot(self):
        snapshot = EvalProductionSnapshot(
            produced={"iron-plate": 1},
            rate_per_min={"iron-plate": 16},
        )

        result = eval_harness.evaluate_model(snapshot)

        self.assertIsInstance(result, EvalResult)
        self.assertTrue(result.milestones["automated_smelting"])
        self.assertTrue(result.milestones["iron_plate_16_pm"])
        self.assertEqual(result.milestones_reached, 2)

        legacy = eval_harness.evaluate(snapshot)
        self.assertEqual(legacy, result.to_dict())

    def test_basic_milestones_use_totals(self):
        snapshot = {
            "produced": {
                "iron-ore": 3,
                "iron-plate": 1,
            },
            "rate_per_min": {},
        }

        result = eval_harness.evaluate(snapshot)

        self.assertTrue(result["milestones"]["burner_mining"])
        self.assertTrue(result["milestones"]["automated_smelting"])
        self.assertFalse(result["milestones"]["green_circuits"])
        self.assertFalse(result["milestones"]["red_science"])
        self.assertFalse(result["milestones"]["iron_plate_16_pm"])
        self.assertFalse(result["milestones"]["red_science_16_pm"])
        self.assertEqual(result["milestones_reached"], 2)

    def test_red_science_throughput_uses_rate_boundary(self):
        at_threshold = {
            "produced": {"automation-science-pack": 1},
            "rate_per_min": {"automation-science-pack": 16},
        }
        below_threshold = {
            "produced": {"automation-science-pack": 1},
            "rate_per_min": {"automation-science-pack": 15.999},
        }

        self.assertTrue(
            eval_harness.evaluate(at_threshold)["milestones"]["red_science_16_pm"]
        )
        self.assertFalse(
            eval_harness.evaluate(below_threshold)["milestones"]["red_science_16_pm"]
        )

    def test_individual_milestone_boundaries(self):
        predicates = dict(eval_harness.MILESTONES)
        snapshot = {
            "produced": {
                "copper-ore": 1,
                "copper-plate": 1,
                "electronic-circuit": 1,
                "automation-science-pack": 1,
            },
            "rate_per_min": {
                "iron-plate": 16,
                "automation-science-pack": 16,
            },
        }

        self.assertTrue(predicates["burner_mining"](snapshot))
        self.assertTrue(predicates["automated_smelting"](snapshot))
        self.assertTrue(predicates["green_circuits"](snapshot))
        self.assertTrue(predicates["red_science"](snapshot))
        self.assertTrue(predicates["iron_plate_16_pm"](snapshot))
        self.assertTrue(predicates["red_science_16_pm"](snapshot))

    def test_score_prefers_rate_per_min(self):
        snapshot = {
            "produced": {"iron-plate": 1000},
            "rate_per_min": {"iron-plate": 2},
        }

        result = eval_harness.evaluate(snapshot)

        self.assertEqual(
            result["production_score"],
            2 * eval_harness.VALUES["iron-plate"],
        )

    def test_missing_keys_never_raise(self):
        result = eval_harness.evaluate({})

        self.assertEqual(result["production_score"], 0.0)
        self.assertEqual(result["milestones_reached"], 0)
        self.assertEqual(
            result["milestones"],
            {name: False for name, _ in eval_harness.MILESTONES},
        )


class QuerySnapshotTest(unittest.TestCase):
    def test_query_snapshot_model_returns_typed_snapshot(self):
        rcon = FakeRcon(
            '{"produced":{"iron-plate":12},"rate_per_min":{"iron-plate":16}}\n'
        )

        snapshot = eval_harness.query_snapshot_model(rcon)

        self.assertIsInstance(snapshot, EvalProductionSnapshot)
        self.assertEqual(snapshot.produced, {"iron-plate": 12.0})
        self.assertEqual(snapshot.rate_per_min, {"iron-plate": 16.0})

    def test_query_snapshot_uses_validated_mod_remote_not_inline_world_lua(self):
        rcon = FakeRcon(
            '{"produced":{"iron-plate":12},"rate_per_min":{"iron-plate":16}}\n'
        )

        snapshot = eval_harness.query_snapshot(
            rcon,
            surface='nauvis") game.print("oops ]]',
        )

        self.assertEqual(snapshot["produced"], {"iron-plate": 12.0})
        self.assertEqual(snapshot["rate_per_min"], {"iron-plate": 16.0})
        self.assertEqual(len(rcon.commands), 1)
        command = rcon.commands[0]
        self.assertEqual(
            command,
            '/silent-command rcon.print(remote.call("claude_interface", '
            '"eval_production_snapshot", [=[nauvis") game.print("oops ]]]=]))',
        )
        for forbidden in [
            "game.surfaces",
            "game.forces.player",
            "get_item_production_statistics",
            "get_flow_count",
            "defines.flow_precision_index",
        ]:
            self.assertNotIn(forbidden, command)

    def test_query_snapshot_treats_empty_object_buckets_as_empty_maps(self):
        rcon = FakeRcon('{"produced":{},"rate_per_min":{}}\n')

        snapshot = eval_harness.query_snapshot(rcon)

        self.assertEqual(snapshot, {"produced": {}, "rate_per_min": {}})

    def test_query_snapshot_errors_return_empty_snapshot(self):
        class BrokenRcon:
            def execute(self, command):
                raise RuntimeError("rcon down")

        snapshot = eval_harness.query_snapshot(BrokenRcon())

        self.assertEqual(snapshot, {"produced": {}, "rate_per_min": {}})


class RunTest(unittest.TestCase):
    def test_run_model_returns_typed_result_and_legacy_run_returns_dict(self):
        rcon = FakeRcon(
            '{"produced":{"iron-plate":1},"rate_per_min":{"iron-plate":16}}\n'
        )

        typed = eval_harness.run_model(rcon, duration_s=0, interval_s=1)

        self.assertIsInstance(typed, EvalResult)
        self.assertTrue(typed.milestones["automated_smelting"])
        self.assertTrue(typed.milestones["iron_plate_16_pm"])

        legacy_rcon = FakeRcon(
            '{"produced":{"iron-plate":1},"rate_per_min":{"iron-plate":16}}\n'
        )
        self.assertEqual(
            eval_harness.run(legacy_rcon, duration_s=0, interval_s=1),
            typed.to_dict(),
        )


if __name__ == "__main__":
    unittest.main()
