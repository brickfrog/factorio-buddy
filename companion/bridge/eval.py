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
import time
from typing import Any, Callable

from models import EvalMilestoneSpec, EvalProductionSnapshot, EvalResult, RconRemoteCall
from rcon import RCONClient, lua_long_string


Milestone = tuple[str, Callable[[Any], bool]]


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
