"""Summarize a bridge run from structured loguru JSONL logs."""

from __future__ import annotations

import argparse
import math
import os
from collections import Counter
from pathlib import Path
from typing import Any, Iterable

from eval import wasted_turn_metrics
from models import (
    BridgeLogMessage,
    BridgeLogRecord,
    BridgeLogRecordCollection,
    BridgeRunReport,
    BridgeRunVerdict,
    BridgeTextLines,
    LiveState,
    PowerStatus,
    RconConnectionSettings,
    RconJsonResponse,
    SteamPowerDiagnostic,
)
from paths import find_factorioctl_mcp
from transport import McpLifecycleClient


COMPANION_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_LOG_DIR = COMPANION_ROOT / "logs"

def latest_log(log_dir: Path = DEFAULT_LOG_DIR) -> Path | None:
    """Return the newest structured bridge JSONL log, if one exists."""
    try:
        logs = sorted(log_dir.glob("bridge-*.jsonl"), key=lambda path: path.stat().st_mtime)
    except OSError:
        return None
    return logs[-1] if logs else None


def iter_records(path: Path) -> Iterable[BridgeLogRecord]:
    """Yield compact records from loguru JSONL, skipping corrupt lines."""
    try:
        lines = BridgeTextLines.from_text(
            path.read_text(encoding="utf-8"),
            keep_blank=False,
        ).lines
    except OSError:
        return
    for line in lines:
        record = BridgeLogRecord.from_json_line(line)
        if not record:
            continue
        yield record


def analyze_log(
    path: Path,
    *,
    recent_progress_window_s: float = 1800.0,
) -> BridgeRunReport:
    return analyze_records(
        iter_records(path),
        log_path=str(path),
        recent_progress_window_s=recent_progress_window_s,
    )


def analyze_records(
    records: Iterable[BridgeLogRecord],
    *,
    log_path: str = "",
    recent_progress_window_s: float = 1800.0,
) -> BridgeRunReport:
    records = BridgeLogRecordCollection.from_value(records).to_list()
    report = BridgeRunReport(
        log_path=log_path,
        recent_progress_window_s=recent_progress_window_s,
    )
    if not records:
        return report

    first = records[0]
    last = records[-1]
    report.started_at = first.time
    report.ended_at = last.time
    first_ts = first.timestamp
    last_ts = last.timestamp
    if first_ts and last_ts:
        report.duration_s = max(0.0, last_ts - first_ts)

    rejection_counts: Counter[str] = Counter()
    progress_timestamps: list[float] = []
    last_provider_pause_ts = 0.0

    for record in records:
        message = BridgeLogMessage.from_record(record)
        ts = record.timestamp

        if message.sdk_spawn:
            report.sdk_attempts += 1
        if message.sdk_done:
            report.sdk_done += 1
        if message.provider_pause:
            report.provider_pauses += 1
            last_provider_pause_ts = ts
            if message.provider_reset_until:
                report.provider_reset_until = message.provider_reset_until
        if message.context_reset:
            report.context_resets += 1
        if message.watchdog_abort:
            report.watchdog_aborts += 1
        if message.research_completed:
            report.research_completed_events += 1
        for count in message.research_counts:
            report.max_research_count = max(report.max_research_count, count)

        if message.entity_summary:
            report.latest_entities = message.entity_summary

        for objective in message.objectives:
            report.latest_objective = objective
        for progress in message.progress_entries:
            report.latest_progress = progress

        if message.progress_event and ts:
            progress_timestamps.append(ts)
        if message.power_summary:
            report.latest_power = message.power_summary
        for signature in message.gameplay_rejection_signatures:
            rejection_counts[signature] += 1

    if last_ts:
        cutoff = last_ts - report.recent_progress_window_s
        report.recent_progress_events = sum(1 for ts in progress_timestamps if ts >= cutoff)

    waste_metrics = wasted_turn_metrics(records)
    report.automation_tool_calls = int(waste_metrics.get("automation_tool_calls") or 0)
    report.manual_transfer_tool_calls = int(
        waste_metrics.get("manual_transfer_tool_calls") or 0
    )
    ratio = waste_metrics.get("automation_to_manual_ratio")
    report.automation_to_manual_ratio = (
        float(ratio) if isinstance(ratio, (int, float)) else None
    )
    report.fuel_automation_tool_calls = int(
        waste_metrics.get("fuel_automation_tool_calls") or 0
    )
    report.manual_fuel_transfer_tool_calls = int(
        waste_metrics.get("manual_fuel_transfer_tool_calls") or 0
    )
    fuel_ratio = waste_metrics.get("fuel_automation_to_manual_ratio")
    report.fuel_automation_to_manual_ratio = (
        float(fuel_ratio) if isinstance(fuel_ratio, (int, float)) else None
    )
    report.science_automation_tool_calls = int(
        waste_metrics.get("science_automation_tool_calls") or 0
    )
    report.manual_science_transfer_tool_calls = int(
        waste_metrics.get("manual_science_transfer_tool_calls") or 0
    )
    science_ratio = waste_metrics.get("science_automation_to_manual_ratio")
    report.science_automation_to_manual_ratio = (
        float(science_ratio) if isinstance(science_ratio, (int, float)) else None
    )
    report.material_flow_automation_tool_calls = int(
        waste_metrics.get("material_flow_automation_tool_calls") or 0
    )
    report.manual_material_transfer_tool_calls = int(
        waste_metrics.get("manual_material_transfer_tool_calls") or 0
    )
    material_ratio = waste_metrics.get("material_flow_automation_to_manual_ratio")
    report.material_flow_automation_to_manual_ratio = (
        float(material_ratio) if isinstance(material_ratio, (int, float)) else None
    )
    report.component_automation_tool_calls = int(
        waste_metrics.get("component_automation_tool_calls") or 0
    )
    report.manual_component_craft_tool_calls = int(
        waste_metrics.get("manual_component_craft_tool_calls") or 0
    )
    component_ratio = waste_metrics.get("component_automation_to_manual_ratio")
    report.component_automation_to_manual_ratio = (
        float(component_ratio) if isinstance(component_ratio, (int, float)) else None
    )
    report.automation_verified_successes = int(
        waste_metrics.get("automation_verified_successes") or 0
    )
    report.automation_verified_failures = int(
        waste_metrics.get("automation_verified_failures") or 0
    )

    report.top_gameplay_rejections = rejection_counts.most_common(5)
    report.verdict = _verdict(report, last_provider_pause_ts, progress_timestamps)
    return report


def enrich_live_state(
    report: BridgeRunReport,
    lifecycle: Any,
    *,
    agent_id: str = "doug-nauvis",
    power_x: float = 0.0,
    power_y: float = 0.0,
    power_radius: float = 500.0,
) -> BridgeRunReport:
    """Add compact current-save state to a log-derived report.

    This is deliberately best-effort: report generation should remain useful
    when the headless server is down, the MCP transport is unavailable, or one diagnostic
    remote fails.
    """
    report.live_attempted = True
    errors: list[str] = []

    try:
        state_payload = RconJsonResponse.parse_value(lifecycle.live_state(agent_id))
        state = LiveState.from_payload(state_payload)
        report.live_state = _single_line(state.to_line())
        report.live_entities = state.entity_summary
    except Exception as exc:  # pragma: no cover - exact socket failures vary
        errors.append(f"live_state: {_error_message(exc)}")

    try:
        power_payload = RconJsonResponse.parse_value(lifecycle.get_power_status(
            power_x,
            power_y,
            power_radius,
        ))
        report.live_power = (
            _live_power_summary(power_payload)
            or BridgeLogMessage.single_line(str(power_payload))
        )
    except Exception as exc:
        errors.append(f"get_power_status: {_error_message(exc)}")

    report.live_connected = bool(report.live_state or report.live_power)
    if errors:
        report.live_error = _single_line("; ".join(errors))
    return report


def format_report(report: BridgeRunReport) -> str:
    lines = [
        "Bridge Run Report",
        f"log: {report.log_path or 'N/A'}",
        f"window: {report.started_at or 'N/A'} -> {report.ended_at or 'N/A'} "
        f"({ _format_duration(report.duration_s) })",
        "sdk: "
        f"attempts={report.sdk_attempts} done={report.sdk_done} "
        f"provider_pauses={report.provider_pauses} "
        f"context_resets={report.context_resets} watchdog_aborts={report.watchdog_aborts}",
    ]
    if report.provider_reset_until:
        lines.append(f"provider_reset_until: {report.provider_reset_until}")
    lines.extend([
        "factory: "
        f"research_completed_events={report.research_completed_events} "
        f"max_research_count={report.max_research_count} "
        f"entities={report.latest_entities or 'unknown'}",
        f"objective: {report.latest_objective or 'unknown'}",
        f"progress: {report.latest_progress or 'unknown'}",
        f"power: {report.latest_power or 'unknown'}",
    ])
    if report.live_attempted:
        if report.live_connected:
            lines.extend([
                f"live_state: {report.live_state or 'unknown'}",
                f"live_entities: {report.live_entities or 'unknown'}",
                f"live_power: {report.live_power or 'unknown'}",
            ])
            if report.live_error:
                lines.append(f"live_warning: {report.live_error}")
        else:
            lines.append(f"live: unavailable ({report.live_error or 'no data returned'})")
    lines.extend([
        f"recent_progress: {report.recent_progress_events} events in "
        f"{_format_duration(report.recent_progress_window_s)}",
        "automation_vs_manual: "
        f"automation_tool_calls={report.automation_tool_calls} "
        f"manual_transfer_tool_calls={report.manual_transfer_tool_calls} "
        f"ratio={_format_ratio(report.automation_to_manual_ratio)}",
        "fuel_automation: "
        f"fuel_controller_calls={report.fuel_automation_tool_calls} "
        f"manual_fuel_transfer_calls={report.manual_fuel_transfer_tool_calls} "
        f"ratio={_format_ratio(report.fuel_automation_to_manual_ratio)}",
        "science_automation: "
        f"automation_science_controller_calls={report.science_automation_tool_calls} "
        f"manual_science_transfer_calls={report.manual_science_transfer_tool_calls} "
        f"ratio={_format_ratio(report.science_automation_to_manual_ratio)}",
        "material_flow_automation: "
        f"material_flow_controller_calls={report.material_flow_automation_tool_calls} "
        f"manual_material_transfer_calls={report.manual_material_transfer_tool_calls} "
        f"ratio={_format_ratio(report.material_flow_automation_to_manual_ratio)}",
        "component_automation: "
        f"component_controller_calls={report.component_automation_tool_calls} "
        f"manual_component_craft_calls={report.manual_component_craft_tool_calls} "
        f"ratio={_format_ratio(report.component_automation_to_manual_ratio)}",
        "automation_verified: "
        f"successes={report.automation_verified_successes} "
        f"failures={report.automation_verified_failures}",
    ])
    if report.top_gameplay_rejections:
        lines.append("top_gameplay_rejections:")
        for signature, count in report.top_gameplay_rejections:
            lines.append(f"- {count}x {signature}")
    else:
        lines.append("top_gameplay_rejections: none")
    lines.append(f"verdict: {report.verdict}")
    return "\n".join(lines)


def _error_message(exc: Exception) -> str:
    message = str(exc)
    if not message:
        message = exc.__class__.__name__
    return _single_line(message)


def _single_line(value: str) -> str:
    return BridgeLogMessage.single_line(value)


def _live_power_summary(payload: Any) -> str:
    diagnostic = SteamPowerDiagnostic.from_payload(payload)
    if diagnostic:
        return _single_line(diagnostic.compact())
    return PowerStatus.compact_from_payload(payload)


def _verdict(
    report: BridgeRunReport,
    last_provider_pause_ts: float,
    progress_timestamps: list[float],
) -> str:
    return BridgeRunVerdict.from_report_state(
        report,
        last_provider_pause_ts=last_provider_pause_ts,
        progress_timestamps=progress_timestamps,
    ).message


def _format_duration(seconds: float) -> str:
    seconds = max(0, int(round(seconds)))
    hours, rem = divmod(seconds, 3600)
    minutes, secs = divmod(rem, 60)
    if hours:
        return f"{hours}h{minutes:02d}m{secs:02d}s"
    if minutes:
        return f"{minutes}m{secs:02d}s"
    return f"{secs}s"


def _format_ratio(value: float | None) -> str:
    if value is None:
        return "unknown"
    if math.isinf(value):
        return "inf"
    return f"{value:.2f}"


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    rcon_defaults = RconConnectionSettings.from_env(os.environ)
    parser = argparse.ArgumentParser(description="Summarize a Factorio bridge run log")
    parser.add_argument(
        "--log",
        default="latest",
        help="Path to bridge JSONL log, or 'latest' for the newest companion log",
    )
    parser.add_argument(
        "--log-dir",
        type=Path,
        default=DEFAULT_LOG_DIR,
        help="Directory used when --log=latest",
    )
    parser.add_argument(
        "--recent-minutes",
        type=float,
        default=30.0,
        help="Window for recent progress verdict",
    )
    parser.add_argument("--json", action="store_true", help="Emit JSON instead of text")
    parser.add_argument(
        "--live",
        action=argparse.BooleanOptionalAction,
        default=True,
        help="Best-effort enrichment from the currently running Factorio server via MCP",
    )
    parser.add_argument(
        "--rcon-host",
        default=rcon_defaults.host,
        help="RCON host passed to factorioctl MCP for --live",
    )
    parser.add_argument(
        "--rcon-port",
        type=int,
        default=rcon_defaults.port,
        help="RCON port passed to factorioctl MCP for --live",
    )
    parser.add_argument(
        "--rcon-password",
        default=rcon_defaults.password,
        help="RCON password passed to factorioctl MCP for --live",
    )
    parser.add_argument(
        "--factorioctl-mcp",
        default=None,
        help="Path to factorioctl MCP binary used for --live",
    )
    parser.add_argument(
        "--live-agent",
        default="doug-nauvis",
        help="Agent id used for the compact live state line",
    )
    parser.add_argument(
        "--live-power-x",
        type=float,
        default=0.0,
        help="Center x for live power status scan",
    )
    parser.add_argument(
        "--live-power-y",
        type=float,
        default=0.0,
        help="Center y for live power status scan",
    )
    parser.add_argument(
        "--live-power-radius",
        type=float,
        default=500.0,
        help="Radius for live power status scan",
    )
    parser.add_argument(
        "--live-timeout",
        type=float,
        default=2.0,
        help="Seconds before giving up on live MCP report enrichment",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    path = latest_log(args.log_dir) if args.log == "latest" else Path(args.log)
    if path is None:
        print(f"No bridge JSONL logs found in {args.log_dir}")
        return 2
    report = analyze_log(path, recent_progress_window_s=args.recent_minutes * 60.0)
    if args.live:
        report.live_attempted = True
        lifecycle = None
        try:
            mcp_bin = args.factorioctl_mcp or find_factorioctl_mcp()
            if not mcp_bin:
                raise FileNotFoundError("factorioctl MCP not found")
            lifecycle = McpLifecycleClient(
                mcp_bin,
                rcon_host=args.rcon_host,
                rcon_port=args.rcon_port,
                rcon_password=args.rcon_password,
                agent_id="bridge-report",
                timeout_s=max(0.1, args.live_timeout),
            )
            enrich_live_state(
                report,
                lifecycle,
                agent_id=args.live_agent,
                power_x=args.live_power_x,
                power_y=args.live_power_y,
                power_radius=args.live_power_radius,
            )
        except Exception as exc:  # pragma: no cover - depends on local server state
            report.live_connected = False
            report.live_error = _error_message(exc)
        finally:
            if lifecycle is not None:
                lifecycle.close()
    if args.json:
        print(report.to_json_text(indent=2, sort_keys=True))
    else:
        print(format_report(report))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
