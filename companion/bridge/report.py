"""Summarize a bridge run from structured loguru JSONL logs."""

from __future__ import annotations

import argparse
import math
import os
from collections import Counter
from pathlib import Path
from typing import Any, Iterable

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
    RconRemoteCall,
    SteamPowerDiagnostic,
)
from rcon import RCONClient, lua_long_string


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

    report.top_gameplay_rejections = rejection_counts.most_common(5)
    report.verdict = _verdict(report, last_provider_pause_ts, progress_timestamps)
    return report


def enrich_live_state(
    report: BridgeRunReport,
    rcon: Any,
    *,
    agent_id: str = "doug-nauvis",
    power_x: float = 0.0,
    power_y: float = 0.0,
    power_radius: float = 500.0,
) -> BridgeRunReport:
    """Add compact current-save state to a log-derived report.

    This is deliberately best-effort: report generation should remain useful
    when the headless server is down, RCON is unavailable, or one diagnostic
    remote fails.
    """
    report.live_attempted = True
    errors: list[str] = []

    try:
        state_payload = _remote_json(
            rcon,
            "live_state_result",
            lua_long_string(agent_id),
        )
        state = LiveState.from_payload(state_payload)
        report.live_state = _single_line(state.to_line())
        report.live_entities = state.entity_summary
    except Exception as exc:  # pragma: no cover - exact socket failures vary
        errors.append(f"live_state_result: {_error_message(exc)}")

    try:
        power_payload = _remote_json(
            rcon,
            "get_power_status",
            _lua_number(power_x),
            _lua_number(power_y),
            _lua_number(power_radius),
        )
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
    ])
    if report.top_gameplay_rejections:
        lines.append("top_gameplay_rejections:")
        for signature, count in report.top_gameplay_rejections:
            lines.append(f"- {count}x {signature}")
    else:
        lines.append("top_gameplay_rejections: none")
    lines.append(f"verdict: {report.verdict}")
    return "\n".join(lines)


def _remote_json(rcon: Any, remote_name: str, *args: str) -> Any:
    response = rcon.execute(_remote_call_command(remote_name, *args))
    return RconJsonResponse.parse_value(response)


def _remote_call_command(remote_name: str, *args: str) -> str:
    return RconRemoteCall.command(remote_name, *args)


def _lua_number(value: float) -> str:
    number = float(value)
    if not math.isfinite(number):
        raise ValueError(f"not a finite Lua number: {value!r}")
    if number.is_integer():
        return str(int(number))
    return repr(number)


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
        help="Best-effort enrichment from the currently running Factorio RCON server",
    )
    parser.add_argument(
        "--rcon-host",
        default=rcon_defaults.host,
        help="RCON host for --live",
    )
    parser.add_argument(
        "--rcon-port",
        type=int,
        default=rcon_defaults.port,
        help="RCON port for --live",
    )
    parser.add_argument(
        "--rcon-password",
        default=rcon_defaults.password,
        help="RCON password for --live",
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
        help="Seconds before giving up on live RCON report enrichment",
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
        rcon = None
        try:
            rcon = RCONClient(
                args.rcon_host,
                args.rcon_port,
                args.rcon_password,
                timeout=max(0.1, args.live_timeout),
                retry_forever=False,
                reconnect_initial_delay=0.0,
            )
            enrich_live_state(
                report,
                rcon,
                agent_id=args.live_agent,
                power_x=args.live_power_x,
                power_y=args.live_power_y,
                power_radius=args.live_power_radius,
            )
        except Exception as exc:  # pragma: no cover - depends on local server state
            report.live_connected = False
            report.live_error = _error_message(exc)
        finally:
            if rcon is not None:
                rcon.close()
    if args.json:
        print(report.to_json_text(indent=2, sort_keys=True))
    else:
        print(format_report(report))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
