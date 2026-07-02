"""Standalone Factorio agent eval harness.

The CI-testable seam is pure: score snapshots and milestone predicates without
touching a live game. The live harness reads Factorio force production
statistics over RCON and feeds the same pure evaluator.

Scoring policy: production_score is computed from one-minute production rates
when evaluate() receives rate_per_min data. If no rates are present, evaluate()
falls back to produced totals. Basic milestones use produced totals, while
milestones ending in _pm use rate_per_min and count values at the threshold as
reached.
"""

from __future__ import annotations

import argparse
import json
import re
import time
from typing import Any, Callable

from models import (
    BridgeLogRecordCollection,
    BridgeLogToolResultLine,
    EvalMilestoneSpec,
    EvalProductionSnapshot,
    EvalResult,
    RconRemoteCall,
)
from rcon import RCONClient, lua_long_string


Milestone = tuple[str, Callable[[Any], bool]]

_DONE_COST_RE = re.compile(r"\bdone:\s*\$([0-9]+(?:\.[0-9]+)?)")
_LEDGER_OBJECTIVE_RE = re.compile(r"^\s*objective:\s*(.+?)\s*$", re.IGNORECASE | re.MULTILINE)
_TOOL_CALL_RE = re.compile(r"\btool:\s*([A-Za-z0-9_]+)\s*\(")
_TOOL_CALL_WITH_ARGS_RE = re.compile(r"\btool:\s*([A-Za-z0-9_]+)\s*\((.*)\)\s*$")

AUTOMATION_TOOL_NAMES = frozenset({
    "build_assembler_feed",
    "build_assembler_output",
    "build_automation_science",
    "build_recipe_assembler_cell",
    "build_fuel_supply",
    "build_lab_feed",
    "execute_direct_smelter",
    "execute_edge_miner",
    "execute_entity_placement_near",
    "plan_automation_science",
    "plan_recipe_assembler_cell",
    "route_belt",
})
MANUAL_TRANSFER_TOOL_NAMES = frozenset({
    "craft",
    "extract_items",
    "feed_lab_from_inventory",
    "hand_feed_furnace",
    "insert_items",
})
FUEL_AUTOMATION_TOOL_NAMES = frozenset({
    "build_fuel_supply",
})
SCIENCE_AUTOMATION_TOOL_NAMES = frozenset({
    "build_automation_science",
    "build_lab_feed",
    "plan_automation_science",
})
MATERIAL_FLOW_AUTOMATION_TOOL_NAMES = frozenset({
    "build_assembler_feed",
    "build_assembler_output",
    "execute_direct_smelter",
    "execute_edge_miner",
    "route_belt",
})
COMPONENT_AUTOMATION_TOOL_NAMES = frozenset({
    "build_assembler_feed",
    "build_assembler_output",
    "build_automation_science",
    "plan_automation_science",
    "build_recipe_assembler_cell",
    "plan_recipe_assembler_cell",
})
FUEL_ITEMS = frozenset({
    "coal",
    "wood",
    "solid-fuel",
    "rocket-fuel",
    "nuclear-fuel",
})


VALUES: dict[str, float] = {
    "iron-ore": 0.25,
    "copper-ore": 0.25,
    "coal": 0.3,
    "stone": 0.2,
    "iron-plate": 1.0,
    "copper-plate": 1.0,
    "iron-gear-wheel": 2.2,
    "copper-cable": 0.6,
    "electronic-circuit": 4.5,
    "automation-science-pack": 8.0,
    "steel-plate": 6.0,
    "plastic-bar": 3.0,
}


def _eval_result(value: Any) -> EvalResult:
    return EvalResult.coerce(
        value,
        milestone_names=tuple(spec.name for spec in MILESTONE_SPECS),
    )


def production_score(produced: dict[str, float] | EvalProductionSnapshot) -> float:
    """Return the weighted value of known early-game items; unknowns are ignored."""
    snapshot = (
        produced
        if isinstance(produced, EvalProductionSnapshot)
        else EvalProductionSnapshot(produced=EvalProductionSnapshot.as_float_map(produced))
    )
    return snapshot.production_score(VALUES)


MILESTONE_SPECS: list[EvalMilestoneSpec] = [
    EvalMilestoneSpec.any_produced(
        "burner_mining",
        ("iron-ore", "copper-ore", "coal"),
    ),
    EvalMilestoneSpec.any_produced(
        "automated_smelting",
        ("iron-plate", "copper-plate"),
    ),
    EvalMilestoneSpec.any_produced(
        "green_circuits",
        ("electronic-circuit",),
    ),
    EvalMilestoneSpec.any_produced(
        "red_science",
        ("automation-science-pack",),
    ),
    EvalMilestoneSpec.rate_at_least(
        "iron_plate_16_pm",
        "iron-plate",
        16.0,
    ),
    EvalMilestoneSpec.rate_at_least(
        "red_science_16_pm",
        "automation-science-pack",
        16.0,
    ),
]
MILESTONES: list[Milestone] = [
    (spec.name, spec.reached)
    for spec in MILESTONE_SPECS
]


def wasted_turn_metrics(records: Any) -> dict[str, Any]:
    """Return cheap log-derived waste metrics for autonomy runs.

    This deliberately works from bridge log records instead of live game state so
    overnight logs can be scored after the fact.
    """
    typed_records = BridgeLogRecordCollection.from_value(records).to_list()
    metrics: dict[str, Any] = {
        "planner_ticks": 0,
        "execution_ticks": 0,
        "planner_turns_with_no_state_change": 0,
        "repeated_objective_restatements": 0,
        "rejected_placements": 0,
        "tool_calls_before_first_milestone": 0,
        "expected_misses": 0,
        "real_failures": 0,
        "automation_tool_calls": 0,
        "manual_transfer_tool_calls": 0,
        "automation_to_manual_ratio": None,
        "fuel_automation_tool_calls": 0,
        "manual_fuel_transfer_tool_calls": 0,
        "fuel_automation_to_manual_ratio": None,
        "science_automation_tool_calls": 0,
        "manual_science_transfer_tool_calls": 0,
        "science_automation_to_manual_ratio": None,
        "material_flow_automation_tool_calls": 0,
        "manual_material_transfer_tool_calls": 0,
        "material_flow_automation_to_manual_ratio": None,
        "component_automation_tool_calls": 0,
        "manual_component_craft_tool_calls": 0,
        "component_automation_to_manual_ratio": None,
        "automation_verified_successes": 0,
        "automation_verified_failures": 0,
        "cost_to_first_milestone_usd": 0.0,
        "total_cost_usd": 0.0,
    }
    seen_objectives: set[str] = set()
    first_milestone_seen = False

    for record in typed_records:
        text = record.message
        lower = text.lower()

        if "(planner tick)" in lower:
            metrics["planner_ticks"] += 1
        if "(execution tick)" in lower:
            metrics["execution_ticks"] += 1
        if "(planner tick)" in lower and any(
            phrase in lower
            for phrase in (
                "no state drift",
                "state unchanged",
                "confirmed stable state",
                "confirmed static state",
                "plan re-affirmed",
                "plan reaffirmed",
            )
        ):
            metrics["planner_turns_with_no_state_change"] += 1

        for objective in _LEDGER_OBJECTIVE_RE.findall(text):
            normalized = " ".join(objective.lower().split())
            if normalized in seen_objectives:
                metrics["repeated_objective_restatements"] += 1
            elif normalized:
                seen_objectives.add(normalized)

        if "expected_miss" in lower:
            metrics["expected_misses"] += 1
        if any(marker in lower for marker in ("sdk_failure", "invalid_request", "game_rejected")):
            if "expected_miss" not in lower:
                metrics["real_failures"] += 1
        if "game_rejected" in lower and (
            "cannot place entity here" in lower
            or '"can_place": false' in lower
            or '"can_place":false' in lower
        ):
            metrics["rejected_placements"] += 1
        verification = _automation_verification_from_tool_result(text)
        if verification is True:
            metrics["automation_verified_successes"] += 1
        elif verification is False:
            metrics["automation_verified_failures"] += 1

        if text.startswith("tool:"):
            tool_name = _tool_name_from_log_message(text)
            if tool_name in AUTOMATION_TOOL_NAMES:
                metrics["automation_tool_calls"] += 1
            if tool_name in MANUAL_TRANSFER_TOOL_NAMES:
                metrics["manual_transfer_tool_calls"] += 1
            if tool_name in FUEL_AUTOMATION_TOOL_NAMES:
                metrics["fuel_automation_tool_calls"] += 1
            if _is_manual_fuel_transfer_tool_call(text, tool_name=tool_name):
                metrics["manual_fuel_transfer_tool_calls"] += 1
            if tool_name in SCIENCE_AUTOMATION_TOOL_NAMES:
                metrics["science_automation_tool_calls"] += 1
            if _is_manual_science_transfer_tool_call(text, tool_name=tool_name):
                metrics["manual_science_transfer_tool_calls"] += 1
            if tool_name in MATERIAL_FLOW_AUTOMATION_TOOL_NAMES:
                metrics["material_flow_automation_tool_calls"] += 1
            if _is_manual_material_transfer_tool_call(text, tool_name=tool_name):
                metrics["manual_material_transfer_tool_calls"] += 1
            if tool_name in COMPONENT_AUTOMATION_TOOL_NAMES:
                metrics["component_automation_tool_calls"] += 1
            if _is_manual_component_craft_tool_call(text, tool_name=tool_name):
                metrics["manual_component_craft_tool_calls"] += 1
            if not first_milestone_seen:
                metrics["tool_calls_before_first_milestone"] += 1

        for match in _DONE_COST_RE.finditer(text):
            cost = float(match.group(1))
            metrics["total_cost_usd"] += cost
            if not first_milestone_seen:
                metrics["cost_to_first_milestone_usd"] += cost

        if _is_eval_milestone_text(lower):
            first_milestone_seen = True

    metrics["cost_to_first_milestone_usd"] = round(
        metrics["cost_to_first_milestone_usd"],
        6,
    )
    metrics["total_cost_usd"] = round(metrics["total_cost_usd"], 6)
    manual_count = metrics["manual_transfer_tool_calls"]
    if manual_count:
        metrics["automation_to_manual_ratio"] = round(
            metrics["automation_tool_calls"] / manual_count,
            6,
        )
    elif metrics["automation_tool_calls"]:
        metrics["automation_to_manual_ratio"] = float("inf")
    manual_fuel_count = metrics["manual_fuel_transfer_tool_calls"]
    if manual_fuel_count:
        metrics["fuel_automation_to_manual_ratio"] = round(
            metrics["fuel_automation_tool_calls"] / manual_fuel_count,
            6,
        )
    elif metrics["fuel_automation_tool_calls"]:
        metrics["fuel_automation_to_manual_ratio"] = float("inf")
    manual_science_count = metrics["manual_science_transfer_tool_calls"]
    if manual_science_count:
        metrics["science_automation_to_manual_ratio"] = round(
            metrics["science_automation_tool_calls"] / manual_science_count,
            6,
        )
    elif metrics["science_automation_tool_calls"]:
        metrics["science_automation_to_manual_ratio"] = float("inf")
    manual_material_count = metrics["manual_material_transfer_tool_calls"]
    if manual_material_count:
        metrics["material_flow_automation_to_manual_ratio"] = round(
            metrics["material_flow_automation_tool_calls"] / manual_material_count,
            6,
        )
    elif metrics["material_flow_automation_tool_calls"]:
        metrics["material_flow_automation_to_manual_ratio"] = float("inf")
    manual_component_count = metrics["manual_component_craft_tool_calls"]
    if manual_component_count:
        metrics["component_automation_to_manual_ratio"] = round(
            metrics["component_automation_tool_calls"] / manual_component_count,
            6,
        )
    elif metrics["component_automation_tool_calls"]:
        metrics["component_automation_to_manual_ratio"] = float("inf")
    return metrics


def _tool_name_from_log_message(text: str) -> str:
    match = _TOOL_CALL_RE.search(text)
    return match.group(1) if match else ""


def _tool_args_from_log_message(text: str) -> Any:
    match = _TOOL_CALL_WITH_ARGS_RE.search(text)
    if not match:
        return None
    return _json_value_from_tool_result_suffix(match.group(2).strip())


def _is_manual_fuel_transfer_tool_call(text: str, *, tool_name: str) -> bool:
    if tool_name not in {"hand_feed_furnace", "insert_items"}:
        return False
    args = _tool_args_from_log_message(text)
    if isinstance(args, dict):
        item = str(args.get("item") or "").strip().lower()
        inventory_type = str(args.get("inventory_type") or "").strip().lower()
        if item in FUEL_ITEMS or inventory_type == "fuel":
            return True
    lower = text.lower()
    return any(
        marker in lower
        for marker in (
            '"inventory_type":"fuel"',
            '"inventory_type": "fuel"',
            '"item":"coal"',
            '"item": "coal"',
            '"item":"wood"',
            '"item": "wood"',
            '"item":"solid-fuel"',
            '"item": "solid-fuel"',
        )
    )


def _is_manual_science_transfer_tool_call(text: str, *, tool_name: str) -> bool:
    if tool_name not in {
        "craft",
        "extract_items",
        "feed_lab_from_inventory",
        "insert_items",
    }:
        return False
    args = _tool_args_from_log_message(text)
    if isinstance(args, dict):
        recipe = str(args.get("recipe") or "").strip().lower()
        item = str(args.get("item") or "").strip().lower()
        science_pack = str(args.get("science_pack") or "").strip().lower()
        if (
            recipe.endswith("-science-pack")
            or item.endswith("-science-pack")
            or science_pack.endswith("-science-pack")
        ):
            return True
    lower = text.lower()
    return "automation-science-pack" in lower or "-science-pack" in lower


def _is_manual_material_transfer_tool_call(text: str, *, tool_name: str) -> bool:
    if tool_name == "hand_feed_furnace":
        return True
    if tool_name not in {"extract_items", "insert_items"}:
        return False
    args = _tool_args_from_log_message(text)
    if isinstance(args, dict):
        item = str(args.get("item") or "").strip().lower()
        inventory_type = str(args.get("inventory_type") or "").strip().lower()
        if tool_name == "insert_items":
            return inventory_type == "furnace_source" and item in {
                "iron-ore",
                "copper-ore",
                "stone",
            }
        return inventory_type == "furnace_result" and item in {
            "iron-plate",
            "copper-plate",
            "steel-plate",
        }
    lower = text.lower()
    return (
        "furnace_source" in lower
        or (
            "furnace_result" in lower
            and any(item in lower for item in ("iron-plate", "copper-plate", "steel-plate"))
        )
    )


def _is_manual_component_craft_tool_call(text: str, *, tool_name: str) -> bool:
    if tool_name != "craft":
        return False
    args = _tool_args_from_log_message(text)
    if isinstance(args, dict):
        recipe = str(args.get("recipe") or "").strip().lower()
        return recipe in {
            "iron-gear-wheel",
            "copper-cable",
            "electronic-circuit",
        }
    lower = text.lower()
    return any(
        recipe in lower
        for recipe in (
            "iron-gear-wheel",
            "copper-cable",
            "electronic-circuit",
        )
    )


def _automation_verification_from_tool_result(text: str) -> bool | None:
    line = BridgeLogToolResultLine.from_line(text)
    if not line.has_tool_result_payload:
        return None
    payload = _json_value_from_tool_result_suffix(line.suffix)
    return _automation_verification_from_payload(payload)


def _json_value_from_tool_result_suffix(suffix: str) -> Any:
    try:
        return json.loads(suffix)
    except (TypeError, ValueError):
        return None


def _automation_verification_from_payload(value: Any) -> bool | None:
    if isinstance(value, list):
        for item in value:
            result = _automation_verification_from_payload(item)
            if result is not None:
                return result
        return None
    if not isinstance(value, dict):
        return None
    if value.get("type") == "text" and isinstance(value.get("text"), str):
        nested = _json_value_from_tool_result_suffix(value["text"])
        return _automation_verification_from_payload(nested)
    verification = value.get("automation_verified") or value.get("verification")
    if isinstance(verification, dict) and isinstance(verification.get("success"), bool):
        return bool(verification["success"])
    for item in value.values():
        result = _automation_verification_from_payload(item)
        if result is not None:
            return result
    return None


def _is_eval_milestone_text(lower_text: str) -> bool:
    return any(
        phrase in lower_text
        for phrase in (
            "milestone",
            "research completed",
            "verified working",
            "automation research completed",
            "furnace producing",
            "production verified",
        )
    )


def evaluate_model(snapshot: EvalProductionSnapshot | dict[str, Any]) -> EvalResult:
    """Evaluate a production snapshot and return the typed result.

    production_score uses rate_per_min when present, because that mirrors FLE's
    throughput/open-play yardstick better than cumulative totals. If no rate
    table is available, it falls back to produced totals so partial/offline
    snapshots remain useful.
    """
    typed_snapshot = EvalProductionSnapshot.coerce(snapshot)

    milestones: dict[str, bool] = {}
    for spec in MILESTONE_SPECS:
        try:
            milestones[spec.name] = spec.reached(typed_snapshot)
        except Exception:
            milestones[spec.name] = False

    return EvalResult.create(
        production_score=typed_snapshot.production_score(VALUES),
        milestones=milestones,
    )


def evaluate(snapshot: EvalProductionSnapshot | dict[str, Any]) -> dict[str, Any]:
    """Evaluate a production snapshot and return the legacy dict shape."""
    return evaluate_model(snapshot).to_dict()


def query_snapshot_model(rcon: Any, surface: str = "nauvis") -> EvalProductionSnapshot:
    """Read force item production statistics over RCON.

    Errors return an empty snapshot so the benchmark can keep running and report
    a zero score instead of crashing on transient RCON or Lua issues.
    """
    surface_literal = lua_long_string(surface)
    try:
        response = rcon.execute(RconRemoteCall.command(
            "eval_production_snapshot",
            surface_literal,
        ))
        return EvalProductionSnapshot.from_rcon_text(response)
    except Exception:
        return EvalProductionSnapshot()


def query_snapshot(rcon: Any, surface: str = "nauvis") -> dict[str, dict[str, float]]:
    """Read production stats and return the legacy snapshot dict shape."""
    return query_snapshot_model(rcon, surface=surface).to_dict()


def _format_report(
    elapsed_s: float,
    result: EvalResult | dict[str, Any],
    first_reached: dict[str, float],
) -> str:
    evaluation = _eval_result(result)
    lines = [
        f"[eval] elapsed={elapsed_s:.0f}s score={evaluation.production_score:.2f} "
        f"milestones={evaluation.milestones_reached}/{len(MILESTONE_SPECS)}",
    ]
    for spec in MILESTONE_SPECS:
        name = spec.name
        reached = bool(evaluation.milestones.get(name))
        first = first_reached.get(name)
        suffix = f" at {first:.0f}s" if first is not None else ""
        lines.append(f"  [{'x' if reached else ' '}] {name}{suffix}")
    return "\n".join(lines)


def run_model(
    rcon: Any,
    duration_s: float,
    interval_s: float,
    surface: str = "nauvis",
) -> EvalResult:
    """Sample production stats for duration_s and return the typed result."""
    duration_s = max(0.0, float(duration_s))
    interval_s = max(1.0, float(interval_s))
    started = time.monotonic()
    deadline = started + duration_s
    best_result: EvalResult | None = None
    final_result = _eval_result(evaluate_model(EvalProductionSnapshot()))
    first_reached: dict[str, float] = {}

    while True:
        elapsed = time.monotonic() - started
        snapshot = query_snapshot_model(rcon, surface=surface)
        final_result = _eval_result(evaluate_model(snapshot))

        if final_result.is_better_than(best_result):
            best_result = final_result

        for name, reached in final_result.milestones.items():
            if reached and name not in first_reached:
                first_reached[name] = elapsed

        print(_format_report(elapsed, final_result, first_reached), flush=True)

        if time.monotonic() >= deadline:
            break
        time.sleep(min(interval_s, max(0.0, deadline - time.monotonic())))

    if best_result is not None:
        print(
            f"[eval] best_score={best_result.production_score:.2f} "
            f"final_score={final_result.production_score:.2f}",
            flush=True,
        )
    return final_result


def run(
    rcon: Any,
    duration_s: float,
    interval_s: float,
    surface: str = "nauvis",
) -> dict[str, Any]:
    """Sample production stats for duration_s and return the legacy dict shape."""
    return run_model(
        rcon,
        duration_s,
        interval_s,
        surface=surface,
    ).to_dict()


def main() -> int:
    parser = argparse.ArgumentParser(description="Factorio production eval harness")
    parser.add_argument("--duration", type=float, default=300.0, help="Run duration in seconds")
    parser.add_argument("--interval", type=float, default=30.0, help="Sample interval in seconds")
    parser.add_argument("--host", default="localhost", help="RCON host")
    parser.add_argument("--port", type=int, default=27015, help="RCON port")
    parser.add_argument("--password", default="", help="RCON password")
    parser.add_argument("--surface", default="nauvis", help="Surface to read")
    args = parser.parse_args()

    rcon = RCONClient(args.host, args.port, args.password)
    try:
        run(rcon, args.duration, args.interval, surface=args.surface)
    finally:
        rcon.close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
