"""Offline prompt regression scenarios for Doug's tool-choice policy.

This is deliberately not a live gameplay loop and does not depend on DSPy at
runtime. It captures the useful part of DSPy-style prompt optimization: frozen
scenarios, real bridge prompt surfaces, trajectory scoring, and exportable
examples that an optimizer can consume later.
"""

from __future__ import annotations

import argparse
import json
import re
from typing import Any

from models import (
    BridgeLogRecordCollection,
    PromptEvalScenario,
    PromptEvalScenarioResult,
    PromptEvalSuiteResult,
    PromptEvalTranscript,
)


_TOOL_CALL_RE = re.compile(r"\btool:\s*([A-Za-z0-9_]+)\s*\(")


DEFAULT_SCENARIOS: tuple[PromptEvalScenario, ...] = (
    PromptEvalScenario(
        name="first_inserter_deadlock_uses_bounded_bootstrap",
        prompt_surface="execution",
        input_text=(
            "No inserter exists. Raw insert_items/extract_items are blocked. "
            "A furnace exists and has recoverable ore/fuel nearby. The current "
            "plan is stuck trying to build fuel delivery that requires a "
            "burner-inserter."
        ),
        expected_tool_prefix=("bootstrap_smelting_once",),
        expected_tools=("bootstrap_smelting_once",),
        forbidden_tools=(
            "insert_items",
            "extract_items",
            "hand_feed_furnace",
        ),
        required_text=("exactly once", "durable automation"),
        notes=(
            "The first sanctioned recovery should create first plates or one "
            "burner-inserter, then stop being used as production."
        ),
    ),
    PromptEvalScenario(
        name="fuel_supply_missing_materials_does_not_execute_build",
        prompt_surface="execution",
        input_text=(
            "repair_fuel_sustainability dry_run found a viable route, but "
            "route.materials_sufficient is false because the burner-inserter "
            "and belts are missing."
        ),
        expected_tool_prefix=("bootstrap_smelting_once",),
        expected_tools=("bootstrap_smelting_once",),
        forbidden_tools=("build_fuel_supply",),
        required_text=("materials", "missing"),
        notes=(
            "Do not test-place build_fuel_supply when its dry-run says "
            "materials are missing."
        ),
    ),
    PromptEvalScenario(
        name="plan_ready_stall_executes_instead_of_replanning",
        prompt_surface="autonomy",
        input_text=(
            "The last four ticks all said plan ready / execution pending with "
            "no state drift. The ledger has a concrete walk_to/insert/verify "
            "plan and next_required_mode=execute."
        ),
        expected_tools=("walk_to",),
        forbidden_tools=("situation_report",),
        forbidden_text=("plan reaffirmed", "awaiting execution"),
        notes=(
            "Repeated ready-state planning should force execution rather than "
            "another read-only reaffirmation."
        ),
    ),
    PromptEvalScenario(
        name="coal_babysitting_prefers_durable_fuel_controller",
        prompt_surface="planner",
        input_text=(
            "Coal drill exists but output belt is full. Furnace and drill fuel "
            "run out because the agent manually inserts 5 coal at a time. "
            "Automation-capable factory has belts and inserters."
        ),
        expected_tools=("repair_fuel_sustainability", "build_fuel_supply"),
        forbidden_tools=("insert_items", "hand_feed_furnace"),
        required_text=("fuel", "durable"),
        notes=(
            "The planner should produce a durable fuel objective, not another "
            "manual refuel loop."
        ),
    ),
)


def evaluate_prompt_scenario_model(
    scenario: PromptEvalScenario | dict[str, Any],
    transcript: (
        PromptEvalTranscript
        | dict[str, Any]
        | list[str]
        | tuple[str, ...]
        | str
    ),
) -> PromptEvalScenarioResult:
    """Score one frozen scenario against an observed tool/text trajectory."""
    typed_scenario = PromptEvalScenario.coerce(scenario)
    typed_transcript = PromptEvalTranscript.coerce(transcript)
    tool_calls = tuple(
        _normalize_tool_name(tool)
        for tool in typed_transcript.tool_calls
        if _normalize_tool_name(tool)
    )
    tool_call_set = set(tool_calls)
    lower_text = typed_transcript.text.lower()

    checks: dict[str, bool] = {}
    missing_expected_tools: list[str] = []
    prefix_mismatches: list[str] = []
    forbidden_tools_seen: list[str] = []
    missing_required_text: list[str] = []
    forbidden_text_seen: list[str] = []

    for tool in typed_scenario.expected_tools:
        normalized = _normalize_tool_name(tool)
        passed = normalized in tool_call_set
        checks[f"expected_tool:{normalized}"] = passed
        if not passed:
            missing_expected_tools.append(normalized)

    for index, tool in enumerate(typed_scenario.expected_tool_prefix):
        normalized = _normalize_tool_name(tool)
        actual = tool_calls[index] if index < len(tool_calls) else ""
        passed = actual == normalized
        checks[f"expected_prefix:{index}:{normalized}"] = passed
        if not passed:
            prefix_mismatches.append(f"{index}:{normalized}!={actual or '<missing>'}")

    for tool in typed_scenario.forbidden_tools:
        normalized = _normalize_tool_name(tool)
        passed = normalized not in tool_call_set
        checks[f"forbidden_tool:{normalized}"] = passed
        if not passed:
            forbidden_tools_seen.append(normalized)

    for fragment in typed_scenario.required_text:
        normalized = fragment.lower()
        passed = normalized in lower_text
        checks[f"required_text:{normalized}"] = passed
        if not passed:
            missing_required_text.append(fragment)

    for fragment in typed_scenario.forbidden_text:
        normalized = fragment.lower()
        passed = normalized not in lower_text
        checks[f"forbidden_text:{normalized}"] = passed
        if not passed:
            forbidden_text_seen.append(fragment)

    return PromptEvalScenarioResult.create(
        scenario_name=typed_scenario.name,
        checks=checks,
        missing_expected_tools=missing_expected_tools,
        prefix_mismatches=prefix_mismatches,
        forbidden_tools_seen=forbidden_tools_seen,
        missing_required_text=missing_required_text,
        forbidden_text_seen=forbidden_text_seen,
    )


def evaluate_prompt_scenario(
    scenario: PromptEvalScenario | dict[str, Any],
    transcript: (
        PromptEvalTranscript
        | dict[str, Any]
        | list[str]
        | tuple[str, ...]
        | str
    ),
) -> dict[str, Any]:
    """Legacy dict wrapper for evaluate_prompt_scenario_model."""
    return evaluate_prompt_scenario_model(scenario, transcript).to_dict()


def evaluate_prompt_suite_model(
    scenario_transcripts: dict[str, Any],
    scenarios: tuple[PromptEvalScenario, ...] | list[PromptEvalScenario] | None = None,
) -> PromptEvalSuiteResult:
    """Score a set of scenario transcripts keyed by scenario name."""
    selected = tuple(scenarios or DEFAULT_SCENARIOS)
    results = [
        evaluate_prompt_scenario_model(
            scenario,
            scenario_transcripts.get(scenario.name, {}),
        )
        for scenario in selected
    ]
    return PromptEvalSuiteResult.from_results(results)


def evaluate_prompt_suite(
    scenario_transcripts: dict[str, Any],
    scenarios: tuple[PromptEvalScenario, ...] | list[PromptEvalScenario] | None = None,
) -> dict[str, Any]:
    """Legacy dict wrapper for evaluate_prompt_suite_model."""
    return evaluate_prompt_suite_model(scenario_transcripts, scenarios).to_dict()


def transcript_from_log_records_model(records: Any) -> PromptEvalTranscript:
    """Extract tool-call sequence and text from bridge log records."""
    typed_records = BridgeLogRecordCollection.from_value(records).to_list()
    tool_calls: list[str] = []
    text_parts: list[str] = []
    for record in typed_records:
        message = str(
            record.get("message")
            if isinstance(record, dict)
            else getattr(record, "message", "")
        )
        match = _TOOL_CALL_RE.search(message)
        if match:
            tool_calls.append(match.group(1))
        text_parts.append(message)
    return PromptEvalTranscript(tool_calls=tool_calls, text="\n".join(text_parts))


def transcript_from_log_records(records: Any) -> dict[str, Any]:
    """Legacy dict wrapper for transcript_from_log_records_model."""
    return transcript_from_log_records_model(records).to_dict()


def dspy_examples(
    scenarios: tuple[PromptEvalScenario, ...] | None = None,
) -> list[dict[str, str]]:
    """Return optimizer-ready examples without importing DSPy.

    A DSPy harness can wrap each dict as dspy.Example(...).with_inputs("input_text")
    and use expected_behavior as the scoring target/feedback seed.
    """
    return [
        {
            "name": scenario.name,
            "prompt_surface": scenario.prompt_surface,
            "input_text": scenario.input_text,
            "expected_behavior": scenario.expected_behavior(),
            "notes": scenario.notes,
        }
        for scenario in (scenarios or DEFAULT_SCENARIOS)
    ]


def _normalize_tool_name(value: Any) -> str:
    text = str(value or "").strip()
    if text.startswith("mcp__factorioctl__"):
        text = text[len("mcp__factorioctl__"):]
    return text


def _main() -> int:
    parser = argparse.ArgumentParser(
        description="Export or score offline prompt-regression scenarios.",
    )
    parser.add_argument(
        "--examples",
        action="store_true",
        help="Print DSPy-ready examples for the built-in scenario set.",
    )
    args = parser.parse_args()
    payload = (
        dspy_examples()
        if args.examples
        else [scenario.to_dict() for scenario in DEFAULT_SCENARIOS]
    )
    print(json.dumps(payload, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(_main())
