import unittest

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

    def test_legacy_wrappers_return_dict_shapes(self):
        scenario = prompt_eval.DEFAULT_SCENARIOS[0]

        result = prompt_eval.evaluate_prompt_scenario(
            scenario.to_dict(),
            {
                "tool_calls": ["bootstrap_smelting_once"],
                "text": "exactly once; durable automation next",
            },
        )

        self.assertEqual(result["scenario_name"], scenario.name)
        self.assertTrue(result["passed"])
        self.assertEqual(result["score"], 1.0)


if __name__ == "__main__":
    unittest.main()
