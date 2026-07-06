import unittest
from pathlib import Path
from tempfile import TemporaryDirectory

import prompt_eval
from models import (
    BridgeLogRecord,
    PromptEvalScenario,
    PromptEvalScenarioResult,
    PromptEvalSuiteResult,
    PromptEvalTranscript,
)


class PromptEvalScenarioTest(unittest.TestCase):
    def test_evaluate_prompt_scenario_model_passes_good_bootstrap_trajectory(self):
        scenario = prompt_eval.DEFAULT_SCENARIOS[0]
        transcript = PromptEvalTranscript(
            tool_calls=("bootstrap_smelting_once", "build_fuel_supply"),
            text=(
                "Used bootstrap_smelting_once exactly once, then moving to "
                "durable automation."
            ),
        )

        result = prompt_eval.evaluate_prompt_scenario_model(scenario, transcript)

        self.assertIsInstance(result, PromptEvalScenarioResult)
        self.assertTrue(result.passed)
        self.assertEqual(result.score, 1.0)
        self.assertEqual(result.missing_expected_tools, ())
        self.assertEqual(result.forbidden_tools_seen, ())

    def test_evaluate_prompt_scenario_model_catches_bad_bootstrap_trajectory(self):
        scenario = prompt_eval.DEFAULT_SCENARIOS[0]
        transcript = PromptEvalTranscript(
            tool_calls=("build_fuel_supply", "insert_items", "extract_items"),
            text="Deadlock persists; trying manual insert loop.",
        )

        result = prompt_eval.evaluate_prompt_scenario_model(scenario, transcript)

        self.assertFalse(result.passed)
        self.assertLess(result.score, 1.0)
        self.assertEqual(result.missing_expected_tools, ("bootstrap_smelting_once",))
        self.assertIn(
            "0:bootstrap_smelting_once!=build_fuel_supply",
            result.prefix_mismatches,
        )
        self.assertEqual(result.forbidden_tools_seen, ("insert_items", "extract_items"))
        self.assertEqual(
            result.missing_required_text,
            ("exactly once", "durable automation"),
        )

    def test_evaluate_prompt_scenario_normalizes_mcp_tool_names(self):
        scenario = PromptEvalScenario(
            name="normalized",
            expected_tools=("bootstrap_smelting_once",),
            expected_tool_prefix=("bootstrap_smelting_once",),
        )

        result = prompt_eval.evaluate_prompt_scenario_model(
            scenario,
            ["mcp__factorioctl__bootstrap_smelting_once"],
        )

        self.assertTrue(result.passed)

    def test_missing_materials_scenario_rejects_build_fuel_supply_first(self):
        scenario = next(
            item
            for item in prompt_eval.DEFAULT_SCENARIOS
            if item.name == "fuel_supply_missing_materials_does_not_execute_build"
        )

        result = prompt_eval.evaluate_prompt_scenario_model(
            scenario,
            PromptEvalTranscript(
                tool_calls=("build_fuel_supply",),
                text="Route materials are missing.",
            ),
        )

        self.assertFalse(result.passed)
        self.assertEqual(result.forbidden_tools_seen, ("build_fuel_supply",))
        self.assertEqual(result.missing_expected_tools, ("bootstrap_smelting_once",))

    def test_plan_ready_stall_scenario_rejects_another_situation_report(self):
        scenario = next(
            item
            for item in prompt_eval.DEFAULT_SCENARIOS
            if item.name == "plan_ready_stall_executes_instead_of_replanning"
        )

        result = prompt_eval.evaluate_prompt_scenario_model(
            scenario,
            PromptEvalTranscript(
                tool_calls=("situation_report",),
                text="Plan reaffirmed; awaiting execution.",
            ),
        )

        self.assertFalse(result.passed)
        self.assertEqual(result.forbidden_tools_seen, ("situation_report",))
        self.assertEqual(
            result.forbidden_text_seen,
            ("plan reaffirmed", "awaiting execution"),
        )

    def test_suite_scores_multiple_scenarios(self):
        transcripts = {
            "first_inserter_deadlock_uses_bounded_bootstrap": {
                "tool_calls": ["bootstrap_smelting_once"],
                "text": "exactly once; durable automation next",
            },
            "fuel_supply_missing_materials_does_not_execute_build": {
                "tool_calls": ["build_fuel_supply"],
                "text": "materials missing",
            },
        }
        scenarios = prompt_eval.DEFAULT_SCENARIOS[:2]

        result = prompt_eval.evaluate_prompt_suite_model(transcripts, scenarios)

        self.assertIsInstance(result, PromptEvalSuiteResult)
        self.assertFalse(result.passed)
        self.assertEqual(len(result.results), 2)
        self.assertGreater(result.score, 0.0)
        self.assertLess(result.score, 1.0)

    def test_transcript_from_log_records_extracts_tool_sequence_and_text(self):
        transcript = prompt_eval.transcript_from_log_records_model([
            BridgeLogRecord(message='tool: situation_report({})'),
            BridgeLogRecord(
                message='tool: bootstrap_smelting_once({"furnace_unit_number":15})',
            ),
            BridgeLogRecord(message="text: exactly once; durable automation next"),
        ])

        self.assertEqual(
            transcript.tool_calls,
            ("situation_report", "bootstrap_smelting_once"),
        )
        self.assertIn("durable automation", transcript.text)

    def test_dspy_examples_are_dependency_free_training_records(self):
        examples = prompt_eval.dspy_examples(prompt_eval.DEFAULT_SCENARIOS[:1])

        self.assertEqual(len(examples), 1)
        self.assertEqual(
            examples[0]["name"],
            "first_inserter_deadlock_uses_bounded_bootstrap",
        )
        self.assertIn("bootstrap_smelting_once", examples[0]["expected_behavior"])
        self.assertIn("input_text", examples[0])

    def test_prompt_scenario_model_accepts_mapping_inputs(self):
        scenario = prompt_eval.DEFAULT_SCENARIOS[0]

        result = prompt_eval.evaluate_prompt_scenario_model(
            scenario.to_dict(),
            {
                "tool_calls": ["bootstrap_smelting_once"],
                "text": "exactly once; durable automation next",
            },
        )

        self.assertEqual(result.scenario_name, scenario.name)
        self.assertTrue(result.passed)
        self.assertEqual(result.score, 1.0)

    def test_load_log_records_accepts_plain_bridge_log_and_time_filters(self):
        with TemporaryDirectory() as tmp:
            path = Path(tmp) / "bridge.log"
            path.write_text(
                "\n".join([
                    "2026-07-02 20:00:00.000 | INFO     | system | before",
                    "continuation before",
                    "2026-07-02 20:10:00.000 | DEBUG    | DOUG-NAUVIS | tool: walk_to({})",
                    "continuation kept with selected line",
                    "2026-07-02 20:20:00.000 | INFO     | DOUG-NAUVIS | after",
                    "continuation after",
                ]),
                encoding="utf-8",
            )

            records = prompt_eval.load_log_records_model(
                [path],
                since="2026-07-02 20:05:00",
                until="2026-07-02 20:15:00",
            )

        self.assertEqual(len(records), 1)
        self.assertEqual(
            records[0].message,
            "tool: walk_to({})\ncontinuation kept with selected line",
        )
        self.assertEqual(records[0].agent, "DOUG-NAUVIS")

    def test_mine_prompt_scenarios_detects_first_inserter_deadlock(self):
        records = [
            BridgeLogRecord(
                message=(
                    "Bridge permanently blocks insert_items/extract_items. "
                    "All durable fuel paths require burner-inserter. "
                    "Deadlock persists."
                ),
            ),
        ]

        scenarios = prompt_eval.mine_prompt_scenarios_model(records)

        self.assertEqual(
            [scenario.name for scenario in scenarios],
            ["candidate_first_inserter_deadlock_from_logs"],
        )
        self.assertEqual(scenarios[0].expected_tools, ("bootstrap_smelting_once",))
        self.assertEqual(scenarios[0].forbidden_tools, (
            "insert_items",
            "extract_items",
            "hand_feed_furnace",
        ))

    def test_mine_prompt_scenarios_detects_build_fuel_supply_missing_materials(self):
        records = [
            BridgeLogRecord(
                message=(
                    "build_fuel_supply dry_run route succeeded but "
                    "burner-inserter inventory: 0 and materials are missing."
                ),
            ),
            BridgeLogRecord(
                message='tool: build_fuel_supply({"consumer_unit_number":19})',
            ),
        ]

        scenarios = prompt_eval.mine_prompt_scenarios_model(records)

        self.assertIn(
            "candidate_build_fuel_supply_missing_materials_from_logs",
            [scenario.name for scenario in scenarios],
        )
        mined = next(
            scenario
            for scenario in scenarios
            if scenario.name == "candidate_build_fuel_supply_missing_materials_from_logs"
        )
        self.assertEqual(mined.expected_tool_prefix, ("bootstrap_smelting_once",))
        self.assertEqual(mined.forbidden_tools, ("build_fuel_supply",))

    def test_mine_prompt_scenarios_detects_manual_coal_babysitting(self):
        records = [
            BridgeLogRecord(
                message='tool: insert_items({"item":"coal","inventory_type":"fuel"})',
            ),
            BridgeLogRecord(
                message='tool: insert_items({"item":"coal","inventory_type":"fuel"})',
            ),
            BridgeLogRecord(message='tool: hand_feed_furnace({"fuel_item":"coal"})'),
        ]

        scenarios = prompt_eval.mine_prompt_scenarios_model(records)

        self.assertEqual(
            [scenario.name for scenario in scenarios],
            ["candidate_manual_coal_babysitting_from_logs"],
        )
        self.assertEqual(
            scenarios[0].expected_tools,
            ("repair_fuel_sustainability", "build_fuel_supply"),
        )

    def test_mine_prompt_scenarios_detects_repeated_plan_ready_stall(self):
        records = [
            BridgeLogRecord(message="PLAN RE-AFFIRMED — READY FOR EXECUTION"),
            BridgeLogRecord(message="awaiting execution tick"),
            BridgeLogRecord(message="No state drift. awaiting execution"),
        ]

        scenarios = prompt_eval.mine_prompt_scenarios_model(records)

        self.assertEqual(
            [scenario.name for scenario in scenarios],
            ["candidate_plan_ready_stall_from_logs"],
        )
        self.assertEqual(scenarios[0].forbidden_tools, ("situation_report",))

    def test_mine_prompt_scenarios_detects_repeated_game_rejections(self):
        records = [
            BridgeLogRecord(
                message="tool_result game_rejected: Error: Cannot place entity here",
            ),
            BridgeLogRecord(
                message="tool_result game_rejected: Error: Cannot place entity here",
            ),
            BridgeLogRecord(
                message="tool_result game_rejected: Error: Cannot place entity here",
            ),
        ]

        scenarios = prompt_eval.mine_prompt_scenarios_model(records)

        self.assertEqual(
            [scenario.name for scenario in scenarios],
            ["candidate_repeated_game_rejection_from_logs"],
        )
        self.assertEqual(scenarios[0].expected_tools, ("check_placement",))

    def test_scenario_file_round_trips(self):
        with TemporaryDirectory() as tmp:
            path = Path(tmp) / "prompt_scenarios.json"
            prompt_eval.save_scenarios(path, prompt_eval.DEFAULT_SCENARIOS[:1])
            loaded = prompt_eval.load_scenarios_model(path)

        self.assertEqual(len(loaded), 1)
        self.assertEqual(
            loaded[0].name,
            "first_inserter_deadlock_uses_bounded_bootstrap",
        )

    def test_cli_mine_logs_and_extract_transcript_emit_json(self):
        with TemporaryDirectory() as tmp:
            path = Path(tmp) / "bridge.log"
            path.write_text(
                "\n".join([
                    "2026-07-02 20:00:00.000 | INFO | DOUG-NAUVIS | PLAN RE-AFFIRMED",
                    "2026-07-02 20:00:01.000 | INFO | DOUG-NAUVIS | awaiting execution",
                    "2026-07-02 20:00:02.000 | INFO | DOUG-NAUVIS | no state drift",
                    "2026-07-02 20:00:03.000 | DEBUG | DOUG-NAUVIS | tool: situation_report({})",
                ]),
                encoding="utf-8",
            )
            records = prompt_eval.load_log_records_model([path])
            transcript = prompt_eval.transcript_from_log_records_model(records)
            scenarios = prompt_eval.mine_prompt_scenarios_model(records)

        self.assertEqual(transcript.tool_calls, ("situation_report",))
        self.assertEqual(
            [scenario.name for scenario in scenarios],
            ["candidate_plan_ready_stall_from_logs"],
        )


if __name__ == "__main__":
    unittest.main()
