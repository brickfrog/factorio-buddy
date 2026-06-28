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
import time
from typing import Any, Callable

from rcon import RCONClient, lua_long_string


Snapshot = dict[str, dict[str, float]]
Milestone = tuple[str, Callable[[dict[str, Any]], bool]]


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


def _number(value: Any) -> float:
    try:
        return float(value)
    except (TypeError, ValueError):
        return 0.0


def _bucket(snapshot: dict[str, Any], key: str) -> dict[str, Any]:
    value = snapshot.get(key)
    if isinstance(value, dict):
        return value
    return {}


def _any_positive(values: dict[str, Any], items: tuple[str, ...]) -> bool:
    return any(_number(values.get(item)) > 0 for item in items)


def _rate_at_least(snapshot: dict[str, Any], item: str, threshold: float) -> bool:
    return _number(_bucket(snapshot, "rate_per_min").get(item)) >= threshold


def production_score(produced: dict[str, float]) -> float:
    """Return the weighted value of known early-game items; unknowns are ignored."""
    if not isinstance(produced, dict):
        return 0.0
    return sum(_number(produced.get(item)) * value for item, value in VALUES.items())


MILESTONES: list[Milestone] = [
    (
        "burner_mining",
        lambda snapshot: _any_positive(
            _bucket(snapshot, "produced"),
            ("iron-ore", "copper-ore", "coal"),
        ),
    ),
    (
        "automated_smelting",
        lambda snapshot: _any_positive(
            _bucket(snapshot, "produced"),
            ("iron-plate", "copper-plate"),
        ),
    ),
    (
        "green_circuits",
        lambda snapshot: _any_positive(
            _bucket(snapshot, "produced"),
            ("electronic-circuit",),
        ),
    ),
    (
        "red_science",
        lambda snapshot: _any_positive(
            _bucket(snapshot, "produced"),
            ("automation-science-pack",),
        ),
    ),
    (
        "iron_plate_16_pm",
        lambda snapshot: _rate_at_least(snapshot, "iron-plate", 16.0),
    ),
    (
        "red_science_16_pm",
        lambda snapshot: _rate_at_least(snapshot, "automation-science-pack", 16.0),
    ),
]


def evaluate(snapshot: dict[str, Any]) -> dict[str, Any]:
    """Evaluate a production snapshot.

    production_score uses rate_per_min when present, because that mirrors FLE's
    throughput/open-play yardstick better than cumulative totals. If no rate
    table is available, it falls back to produced totals so partial/offline
    snapshots remain useful.
    """
    if not isinstance(snapshot, dict):
        snapshot = {}

    rate_per_min = _bucket(snapshot, "rate_per_min")
    produced = _bucket(snapshot, "produced")
    score_source = rate_per_min if rate_per_min else produced

    milestones: dict[str, bool] = {}
    for name, predicate in MILESTONES:
        try:
            milestones[name] = bool(predicate(snapshot))
        except Exception:
            milestones[name] = False

    return {
        "production_score": production_score(score_source),
        "milestones": milestones,
        "milestones_reached": sum(1 for reached in milestones.values() if reached),
    }


def _as_float_map(value: Any) -> dict[str, float]:
    if not isinstance(value, dict):
        return {}
    result: dict[str, float] = {}
    for key, raw in value.items():
        amount = _number(raw)
        if amount:
            result[str(key)] = amount
    return result


def _last_json_object(text: str) -> dict[str, Any] | None:
    for line in reversed(text.splitlines()):
        candidate = line.strip()
        if not candidate:
            continue
        try:
            parsed = json.loads(candidate)
        except json.JSONDecodeError:
            continue
        if isinstance(parsed, dict):
            return parsed
    return None


def query_snapshot(rcon: Any, surface: str = "nauvis") -> Snapshot:
    """Read force item production statistics over RCON.

    Errors return an empty snapshot so the benchmark can keep running and report
    a zero score instead of crashing on transient RCON or Lua issues.
    """
    empty: Snapshot = {"produced": {}, "rate_per_min": {}}
    surface_literal = lua_long_string(surface)
    lua = f"""
local surface = game.surfaces[{surface_literal}]
if not surface then
  rcon.print('{{"produced":{{}},"rate_per_min":{{}}}}')
  return
end
local force = game.forces.player
local stats = force.get_item_production_statistics(surface)
local produced = {{}}
local rate_per_min = {{}}
local precision = defines.flow_precision_index.one_minute
for item, count in pairs(stats.input_counts or {{}}) do
  local name = type(item) == "string" and item or item.name
  if name then
    produced[name] = count
    rate_per_min[name] = stats.get_flow_count{{
      name = name,
      category = "input",
      precision_index = precision,
    }}
  end
end
local payload = {{produced = produced, rate_per_min = rate_per_min}}
local ok, encoded = pcall(function() return helpers.table_to_json(payload) end)
if ok then
  rcon.print(encoded)
else
  rcon.print('{{"produced":{{}},"rate_per_min":{{}}}}')
end
"""
    try:
        response = rcon.execute("/silent-command " + " ".join(lua.split()))
        parsed = _last_json_object(response)
        if parsed is None:
            return empty
        return {
            "produced": _as_float_map(parsed.get("produced")),
            "rate_per_min": _as_float_map(parsed.get("rate_per_min")),
        }
    except Exception:
        return empty


def _format_report(
    elapsed_s: float,
    result: dict[str, Any],
    first_reached: dict[str, float],
) -> str:
    milestones = result.get("milestones", {})
    lines = [
        f"[eval] elapsed={elapsed_s:.0f}s score={result.get('production_score', 0.0):.2f} "
        f"milestones={result.get('milestones_reached', 0)}/{len(MILESTONES)}",
    ]
    for name, _ in MILESTONES:
        reached = bool(milestones.get(name))
        first = first_reached.get(name)
        suffix = f" at {first:.0f}s" if first is not None else ""
        lines.append(f"  [{'x' if reached else ' '}] {name}{suffix}")
    return "\n".join(lines)


def run(
    rcon: Any,
    duration_s: float,
    interval_s: float,
    surface: str = "nauvis",
) -> dict[str, Any]:
    """Sample production stats for duration_s and print a readable report."""
    duration_s = max(0.0, float(duration_s))
    interval_s = max(1.0, float(interval_s))
    started = time.monotonic()
    deadline = started + duration_s
    best_result: dict[str, Any] | None = None
    final_result: dict[str, Any] = evaluate({})
    first_reached: dict[str, float] = {}

    while True:
        elapsed = time.monotonic() - started
        snapshot = query_snapshot(rcon, surface=surface)
        final_result = evaluate(snapshot)

        if (
            best_result is None
            or final_result["production_score"] > best_result["production_score"]
        ):
            best_result = dict(final_result)

        for name, reached in final_result["milestones"].items():
            if reached and name not in first_reached:
                first_reached[name] = elapsed

        print(_format_report(elapsed, final_result, first_reached), flush=True)

        if time.monotonic() >= deadline:
            break
        time.sleep(min(interval_s, max(0.0, deadline - time.monotonic())))

    if best_result is not None:
        print(
            f"[eval] best_score={best_result['production_score']:.2f} "
            f"final_score={final_result['production_score']:.2f}",
            flush=True,
        )
    return final_result


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
