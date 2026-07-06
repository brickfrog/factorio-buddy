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
from pathlib import Path
from typing import Any

from models import (
    BridgeLogRecord,
    BridgeLogRecordCollection,
    PromptEvalScenario,
    PromptEvalScenarioResult,
    PromptEvalSuiteResult,
    PromptEvalTranscript,
)


_TOOL_CALL_RE = re.compile(r"\btool:\s*([A-Za-z0-9_]+)\s*\(")
_PLAIN_LOG_RE = re.compile(
    r"^(?P<time>\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}(?:\.\d+)?)"
    r"\s+\|\s+(?P<level>[A-Z]+)\s+\|\s+(?P<agent>[^|]+?)\s+\|\s+(?P<message>.*)$"
)


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


def load_log_records_model(
    paths: list[str | Path],
    *,
    since: str = "",
    until: str = "",
) -> tuple[BridgeLogRecord, ...]:
    """Load bridge log records from loguru JSONL or plain bridge .log files."""
    records: list[BridgeLogRecord] = []
    for path_value in paths:
        path = Path(path_value)
        if not path.exists() or not path.is_file():
            continue
        file_records: list[BridgeLogRecord] = []
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            if not _is_structured_log_line(line) and file_records:
                previous = file_records[-1]
                file_records[-1] = previous.model_copy(
                    update={"message": previous.message + "\n" + line}
                )
                continue
            record = _record_from_log_line(line)
            if record:
                file_records.append(record)
        records.extend(
            record
            for record in file_records
            if _record_in_time_range(record, since=since, until=until)
        )
    return tuple(records)


def load_log_records(
    paths: list[str | Path],
    *,
    since: str = "",
    until: str = "",
) -> list[dict[str, Any]]:
    """Legacy dict wrapper for load_log_records_model."""
    return [
        record.model_dump()
        for record in load_log_records_model(
            paths,
            since=since,
            until=until,
        )
    ]


def mine_prompt_scenarios_model(records: Any) -> tuple[PromptEvalScenario, ...]:
    """Mine candidate prompt-regression scenarios from bridge log records."""
    typed_records = _coerce_records(records)
    text = "\n".join(record.message for record in typed_records)
    lower = text.lower()
    scenarios: list[PromptEvalScenario] = []

    if _count_any(lower, (
        "plan reaffirmed",
        "plan re-affirmed",
        "awaiting execution",
        "confirmed stable state",
        "confirmed static state",
        "no state drift",
    )) >= 3:
        scenarios.append(PromptEvalScenario(
            name="candidate_plan_ready_stall_from_logs",
            prompt_surface="autonomy",
            input_text=_snippet_for_terms(
                typed_records,
                ("plan reaffirmed", "awaiting execution", "no state drift"),
            ),
            expected_tools=("walk_to",),
            forbidden_tools=("situation_report",),
            forbidden_text=("plan reaffirmed", "awaiting execution"),
            notes=(
                "Mined from repeated stable/ready planner turns. Review the "
                "expected first tool against the concrete ledger before adding "
                "to the durable scenario corpus."
            ),
        ))

    if (
        "deadlock" in lower
        and (
            "burner-inserter" in lower
            or "no inserter" in lower
            or "insert_items" in lower
            or "extract_items" in lower
        )
        and "bootstrap_smelting_once" not in lower
    ):
        scenarios.append(PromptEvalScenario(
            name="candidate_first_inserter_deadlock_from_logs",
            prompt_surface="execution",
            input_text=_snippet_for_terms(
                typed_records,
                ("deadlock", "burner-inserter", "insert_items", "extract_items"),
            ),
            expected_tool_prefix=("bootstrap_smelting_once",),
            expected_tools=("bootstrap_smelting_once",),
            forbidden_tools=("insert_items", "extract_items", "hand_feed_furnace"),
            required_text=("exactly once", "durable automation"),
            notes=(
                "Mined from a first-inserter or first-plate deadlock. "
                "bootstrap_smelting_once should be the bounded escape hatch, "
                "not a production loop."
            ),
        ))

    if (
        "build_fuel_supply" in lower
        and (
            "insufficient materials" in lower
            or "materials_sufficient" in lower
            or "materials are missing" in lower
            or "missing inserter" in lower
            or "burner-inserter inventory: 0" in lower
        )
    ):
        scenarios.append(PromptEvalScenario(
            name="candidate_build_fuel_supply_missing_materials_from_logs",
            prompt_surface="execution",
            input_text=_snippet_for_terms(
                typed_records,
                (
                    "build_fuel_supply",
                    "insufficient materials",
                    "materials_sufficient",
                    "burner-inserter",
                ),
            ),
            expected_tool_prefix=("bootstrap_smelting_once",),
            expected_tools=("bootstrap_smelting_once",),
            forbidden_tools=("build_fuel_supply",),
            required_text=("materials", "missing"),
            notes=(
                "Mined from build_fuel_supply planning/execution when the "
                "route or dry-run already exposed missing materials."
            ),
        ))

    manual_fuel_transfers = sum(
        1
        for record in typed_records
        if _looks_like_manual_fuel_transfer(record.message)
    )
    durable_fuel_calls = sum(
        1
        for record in typed_records
        if any(tool in record.message for tool in (
            "repair_fuel_sustainability",
            "build_fuel_supply",
        ))
    )
    if manual_fuel_transfers >= 3 and durable_fuel_calls == 0:
        scenarios.append(PromptEvalScenario(
            name="candidate_manual_coal_babysitting_from_logs",
            prompt_surface="planner",
            input_text=_snippet_for_terms(
                typed_records,
                ("insert_items", "hand_feed_furnace", "coal", "fuel"),
            ),
            expected_tools=("repair_fuel_sustainability", "build_fuel_supply"),
            forbidden_tools=("insert_items", "hand_feed_furnace"),
            required_text=("fuel", "durable"),
            notes=(
                "Mined from repeated manual coal/fuel transfer without durable "
                "fuel controllers."
            ),
        ))

    repeated_rejection = _repeated_game_rejection(typed_records)
    if repeated_rejection:
        scenarios.append(PromptEvalScenario(
            name="candidate_repeated_game_rejection_from_logs",
            prompt_surface="execution",
            input_text=repeated_rejection,
            expected_tools=("check_placement",),
            forbidden_tools=("place_entity",),
            required_text=("diagnose",),
            notes=(
                "Mined from repeated same game rejection. Expected tool is a "
                "review hint; adjust it to the specific failed controller before "
                "promoting this candidate."
            ),
        ))

    return tuple(_dedupe_scenarios(scenarios))


def load_scenarios_model(path: str | Path) -> tuple[PromptEvalScenario, ...]:
    """Load a prompt scenario corpus from a JSON list."""
    payload = json.loads(Path(path).read_text(encoding="utf-8"))
    if not isinstance(payload, list):
        return ()
    return tuple(PromptEvalScenario.coerce(item) for item in payload)


def save_scenarios(
    path: str | Path,
    scenarios: list[PromptEvalScenario] | tuple[PromptEvalScenario, ...],
) -> None:
    """Write a prompt scenario corpus as stable pretty JSON."""
    Path(path).write_text(
        json.dumps(
            [scenario.to_dict() for scenario in scenarios],
            indent=2,
            sort_keys=True,
        )
        + "\n",
        encoding="utf-8",
    )


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


def _record_from_log_line(line: str) -> BridgeLogRecord | None:
    if not line.strip():
        return None
    json_record = BridgeLogRecord.from_json_line(line)
    if json_record:
        return json_record
    match = _PLAIN_LOG_RE.match(line)
    if match:
        return BridgeLogRecord(
            message=match.group("message"),
            time=match.group("time"),
            level=match.group("level"),
            agent=match.group("agent").strip(),
        )
    return BridgeLogRecord(message=line)


def _is_structured_log_line(line: str) -> bool:
    stripped = line.lstrip()
    return stripped.startswith("{") or bool(_PLAIN_LOG_RE.match(line))


def _record_in_time_range(record: BridgeLogRecord, *, since: str, until: str) -> bool:
    record_time = (record.time or "").strip()
    if not record_time:
        return True
    comparable = record_time[:max(len(since), len(until), 19)]
    if since and comparable < since:
        return False
    if until and comparable > until:
        return False
    return True


def _coerce_records(records: Any) -> tuple[BridgeLogRecord, ...]:
    if (
        isinstance(records, tuple)
        and all(isinstance(item, BridgeLogRecord) for item in records)
    ):
        return records
    if (
        isinstance(records, list)
        and all(isinstance(item, BridgeLogRecord) for item in records)
    ):
        return tuple(records)
    if isinstance(records, list) and all(isinstance(item, dict) for item in records):
        result: list[BridgeLogRecord] = []
        for item in records:
            try:
                result.append(BridgeLogRecord.model_validate(item))
            except Exception:
                record = BridgeLogRecord.from_loguru_entry(item)
                if record:
                    result.append(record)
        return tuple(result)
    return tuple(BridgeLogRecordCollection.from_value(records).records)


def _count_any(text: str, terms: tuple[str, ...]) -> int:
    return sum(text.count(term) for term in terms)


def _snippet_for_terms(
    records: tuple[BridgeLogRecord, ...],
    terms: tuple[str, ...],
    *,
    max_lines: int = 12,
) -> str:
    selected: list[str] = []
    normalized_terms = tuple(term.lower() for term in terms)
    for record in records:
        message = record.message.strip()
        if not message:
            continue
        lower = message.lower()
        if any(term in lower for term in normalized_terms):
            selected.append(message)
        if len(selected) >= max_lines:
            break
    if not selected:
        selected = [record.message.strip() for record in records[:max_lines] if record.message.strip()]
    return "\n".join(selected)


def _looks_like_manual_fuel_transfer(message: str) -> bool:
    lower = message.lower()
    return (
        (
            "tool: insert_items" in lower
            and (
                '"inventory_type":"fuel"' in lower
                or '"inventory_type": "fuel"' in lower
                or '"item":"coal"' in lower
                or '"item": "coal"' in lower
                or '"item":"wood"' in lower
                or '"item": "wood"' in lower
            )
        )
        or ("tool: hand_feed_furnace" in lower and "coal" in lower)
    )


def _repeated_game_rejection(records: tuple[BridgeLogRecord, ...]) -> str:
    seen: dict[str, int] = {}
    exemplar: dict[str, str] = {}
    for record in records:
        lower = record.message.lower()
        if "game_rejected" not in lower:
            continue
        key = _normalize_rejection_key(lower)
        seen[key] = seen.get(key, 0) + 1
        exemplar.setdefault(key, record.message.strip())
    for key, count in seen.items():
        if count >= 3:
            return exemplar.get(key, key)
    return ""


def _normalize_rejection_key(text: str) -> str:
    for marker in (
        "cannot place entity here",
        "insufficient materials",
        "create_entity returned nil",
        "no such inventory",
        "crafting did not start",
    ):
        if marker in text:
            return marker
    return text[:160]


def _dedupe_scenarios(scenarios: list[PromptEvalScenario]) -> list[PromptEvalScenario]:
    result: list[PromptEvalScenario] = []
    seen: set[str] = set()
    for scenario in scenarios:
        if scenario.name in seen:
            continue
        seen.add(scenario.name)
        result.append(scenario)
    return result


def _main() -> int:
    parser = argparse.ArgumentParser(
        description="Export or score offline prompt-regression scenarios.",
    )
    parser.add_argument(
        "--examples",
        action="store_true",
        help="Print DSPy-ready examples for the built-in scenario set.",
    )
    subparsers = parser.add_subparsers(dest="command")

    examples_parser = subparsers.add_parser("examples")
    examples_parser.add_argument("--scenarios", default="")

    mine_parser = subparsers.add_parser("mine-logs")
    mine_parser.add_argument("paths", nargs="+")
    mine_parser.add_argument("--since", default="")
    mine_parser.add_argument("--until", default="")
    mine_parser.add_argument("--output", default="")

    transcript_parser = subparsers.add_parser("extract-transcript")
    transcript_parser.add_argument("paths", nargs="+")
    transcript_parser.add_argument("--since", default="")
    transcript_parser.add_argument("--until", default="")
    transcript_parser.add_argument("--output", default="")

    score_parser = subparsers.add_parser("score-transcript")
    score_parser.add_argument("--scenarios", required=True)
    score_parser.add_argument("--transcript", required=True)

    args = parser.parse_args()
    if args.command == "examples":
        scenarios = (
            load_scenarios_model(args.scenarios)
            if args.scenarios
            else DEFAULT_SCENARIOS
        )
        payload = dspy_examples(tuple(scenarios))
    elif args.command == "mine-logs":
        records = load_log_records_model(args.paths, since=args.since, until=args.until)
        scenarios = mine_prompt_scenarios_model(records)
        payload = [scenario.to_dict() for scenario in scenarios]
        if args.output:
            save_scenarios(args.output, scenarios)
    elif args.command == "extract-transcript":
        records = load_log_records_model(args.paths, since=args.since, until=args.until)
        payload = transcript_from_log_records_model(records).to_dict()
        if args.output:
            _write_json(args.output, payload)
    elif args.command == "score-transcript":
        scenarios = load_scenarios_model(args.scenarios)
        transcript_payload = json.loads(Path(args.transcript).read_text(encoding="utf-8"))
        transcript = PromptEvalTranscript.coerce(transcript_payload)
        payload = evaluate_prompt_suite_model(
            {scenario.name: transcript for scenario in scenarios},
            tuple(scenarios),
        ).to_dict()
    else:
        payload = (
            dspy_examples()
            if args.examples
            else [scenario.to_dict() for scenario in DEFAULT_SCENARIOS]
        )
    print(json.dumps(payload, indent=2, sort_keys=True))
    return 0


def _write_json(path: str | Path, payload: Any) -> None:
    Path(path).write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )


if __name__ == "__main__":
    raise SystemExit(_main())
