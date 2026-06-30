"""Summarize a bridge run from structured loguru JSONL logs."""

from __future__ import annotations

import argparse
import json
import math
import os
import re
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Iterable

from rcon import RCONClient, lua_long_string


COMPANION_ROOT = Path(__file__).resolve().parents[1]
DEFAULT_LOG_DIR = COMPANION_ROOT / "logs"

_OBJECTIVE_RE = re.compile(
    r"Continuity ledger: continue the committed objective, do not restart it:\s*([^\n]+)"
    r"|^\s*objective:\s*(.+)$",
    re.IGNORECASE | re.MULTILINE,
)
_LEDGER_PROGRESS_RE = re.compile(r"^\s*progress:\s*(.+)$", re.IGNORECASE | re.MULTILINE)
_ENTITY_COUNTS_RE = re.compile(r"player entities:\s*([^\n]+)", re.IGNORECASE)
_RESEARCH_COUNT_RE = re.compile(r"research count:\s*(\d+)", re.IGNORECASE)
_RESEARCHED_COUNT_JSON_RE = re.compile(
    r'\\?"researched_count\\?"\s*:\s*(\d+)',
    re.IGNORECASE,
)
_RESET_UNTIL_RE = re.compile(
    r"until\s+([0-9-]+\s+[0-9:]+\s+[A-Z]+)",
    re.IGNORECASE,
)
LOW_VALUE_PROGRESS_PATTERNS = (
    "plan fully validated and awaiting execution",
    "plan validated and ready for execution",
    "plan unchanged and ready for execution",
)


@dataclass
class BridgeRunReport:
    log_path: str = ""
    started_at: str = ""
    ended_at: str = ""
    duration_s: float = 0.0
    sdk_attempts: int = 0
    sdk_done: int = 0
    provider_pauses: int = 0
    provider_reset_until: str = ""
    context_resets: int = 0
    watchdog_aborts: int = 0
    research_completed_events: int = 0
    max_research_count: int = 0
    latest_entities: str = ""
    latest_objective: str = ""
    latest_progress: str = ""
    latest_power: str = ""
    live_attempted: bool = False
    live_connected: bool = False
    live_state: str = ""
    live_entities: str = ""
    live_power: str = ""
    live_error: str = ""
    recent_progress_events: int = 0
    recent_progress_window_s: float = 1800.0
    top_gameplay_rejections: list[tuple[str, int]] = field(default_factory=list)
    verdict: str = "operator attention needed: no bridge records found"

    def to_dict(self) -> dict[str, Any]:
        return {
            "log_path": self.log_path,
            "started_at": self.started_at,
            "ended_at": self.ended_at,
            "duration_s": self.duration_s,
            "sdk_attempts": self.sdk_attempts,
            "sdk_done": self.sdk_done,
            "provider_pauses": self.provider_pauses,
            "provider_reset_until": self.provider_reset_until,
            "context_resets": self.context_resets,
            "watchdog_aborts": self.watchdog_aborts,
            "research_completed_events": self.research_completed_events,
            "max_research_count": self.max_research_count,
            "latest_entities": self.latest_entities,
            "latest_objective": self.latest_objective,
            "latest_progress": self.latest_progress,
            "latest_power": self.latest_power,
            "live_attempted": self.live_attempted,
            "live_connected": self.live_connected,
            "live_state": self.live_state,
            "live_entities": self.live_entities,
            "live_power": self.live_power,
            "live_error": self.live_error,
            "recent_progress_events": self.recent_progress_events,
            "recent_progress_window_s": self.recent_progress_window_s,
            "top_gameplay_rejections": [
                {"count": count, "signature": signature}
                for signature, count in self.top_gameplay_rejections
            ],
            "verdict": self.verdict,
        }


def latest_log(log_dir: Path = DEFAULT_LOG_DIR) -> Path | None:
    """Return the newest structured bridge JSONL log, if one exists."""
    try:
        logs = sorted(log_dir.glob("bridge-*.jsonl"), key=lambda path: path.stat().st_mtime)
    except OSError:
        return None
    return logs[-1] if logs else None


def iter_records(path: Path) -> Iterable[dict[str, Any]]:
    """Yield compact records from loguru JSONL, skipping corrupt lines."""
    try:
        lines = path.read_text(encoding="utf-8").splitlines()
    except OSError:
        return
    for line in lines:
        try:
            entry = json.loads(line)
        except (TypeError, json.JSONDecodeError):
            continue
        record = entry.get("record") if isinstance(entry, dict) else None
        if not isinstance(record, dict):
            continue
        time = record.get("time") if isinstance(record.get("time"), dict) else {}
        level = record.get("level") if isinstance(record.get("level"), dict) else {}
        extra = record.get("extra") if isinstance(record.get("extra"), dict) else {}
        yield {
            "message": str(record.get("message", "")),
            "timestamp": _number(time.get("timestamp")),
            "time": str(time.get("repr", "")),
            "level": str(level.get("name", "")),
            "agent": str(extra.get("agent", "")),
        }


def analyze_log(
    path: Path,
    *,
    recent_progress_window_s: float = 1800.0,
) -> BridgeRunReport:
    records = list(iter_records(path))
    report = BridgeRunReport(
        log_path=str(path),
        recent_progress_window_s=max(1.0, float(recent_progress_window_s)),
    )
    if not records:
        return report

    first = records[0]
    last = records[-1]
    report.started_at = first["time"]
    report.ended_at = last["time"]
    first_ts = first["timestamp"]
    last_ts = last["timestamp"]
    if first_ts and last_ts:
        report.duration_s = max(0.0, last_ts - first_ts)

    rejection_counts: Counter[str] = Counter()
    progress_timestamps: list[float] = []
    last_provider_pause_ts = 0.0

    for record in records:
        msg = record["message"]
        lower = msg.lower()
        ts = record["timestamp"]

        if "spawning claude sdk" in lower:
            report.sdk_attempts += 1
        if msg.startswith("done: "):
            report.sdk_done += 1
        if "provider usage limit active" in lower or "paused by provider usage limit" in lower:
            report.provider_pauses += 1
            last_provider_pause_ts = ts
            reset = _RESET_UNTIL_RE.search(msg)
            if reset:
                report.provider_reset_until = reset.group(1)
        if "context window" in lower and "cleared session" in lower:
            report.context_resets += 1
        if "watchdog aborted stuck tick" in lower or "watchdog_abort:" in lower:
            report.watchdog_aborts += 1
        if "research completed" in lower:
            report.research_completed_events += 1
        for match in _RESEARCH_COUNT_RE.finditer(msg):
            report.max_research_count = max(report.max_research_count, int(match.group(1)))
        for match in _RESEARCHED_COUNT_JSON_RE.finditer(msg):
            report.max_research_count = max(report.max_research_count, int(match.group(1)))

        entities = _ENTITY_COUNTS_RE.search(msg)
        if entities:
            report.latest_entities = entities.group(1).strip()

        for match in _OBJECTIVE_RE.finditer(msg):
            objective = (match.group(1) or match.group(2) or "").strip()
            if objective and not _is_placeholder(objective):
                report.latest_objective = objective
        for match in _LEDGER_PROGRESS_RE.finditer(msg):
            progress = match.group(1).strip()
            if (
                progress
                and not _is_placeholder(progress)
                and not _is_low_value_progress_text(progress)
            ):
                report.latest_progress = progress
                if ts:
                    progress_timestamps.append(ts)

        if _is_progress_message(lower) and not _is_low_value_progress_text(msg) and ts:
            progress_timestamps.append(ts)
        if _is_power_message(lower):
            power_summary = _compact_power_log_message(msg)
            if power_summary:
                report.latest_power = power_summary
        for rejection_line in _game_rejection_lines(msg):
            signature = _game_rejection_signature(rejection_line)
            if signature:
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
        state = _remote_text(
            rcon,
            "live_state_line",
            lua_long_string(agent_id),
        )
        report.live_state = _single_line(state)
        entities = _ENTITY_COUNTS_RE.search(state)
        if entities:
            report.live_entities = entities.group(1).strip()
    except Exception as exc:  # pragma: no cover - exact socket failures vary
        errors.append(f"live_state_line: {_error_message(exc)}")

    try:
        power = _remote_json(
            rcon,
            "get_power_status",
            _lua_number(power_x),
            _lua_number(power_y),
            _lua_number(power_radius),
        )
        report.live_power = _compact_power_summary(power)
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


def _remote_text(rcon: Any, remote_name: str, *args: str) -> str:
    response = rcon.execute(_remote_call_command(remote_name, *args))
    return _last_nonempty_line(response)


def _remote_json(rcon: Any, remote_name: str, *args: str) -> Any:
    response = rcon.execute(_remote_call_command(remote_name, *args))
    return _parse_json_response(response)


def _remote_call_command(remote_name: str, *args: str) -> str:
    if not re.fullmatch(r"[A-Za-z0-9_]+", remote_name):
        raise ValueError(f"invalid remote name: {remote_name}")
    suffix = "".join(f", {arg}" for arg in args)
    return f'/silent-command rcon.print(remote.call("claude_interface", "{remote_name}"{suffix}))'


def _parse_json_response(response: str) -> Any:
    for line in reversed(str(response).splitlines()):
        candidate = line.strip()
        if not candidate:
            continue
        try:
            return json.loads(candidate)
        except json.JSONDecodeError:
            continue
    try:
        return json.loads(str(response))
    except json.JSONDecodeError as exc:
        raise ValueError(f"RCON response did not contain JSON: {_single_line(str(response))}") from exc


def _last_nonempty_line(response: str) -> str:
    for line in reversed(str(response).splitlines()):
        candidate = line.strip()
        if candidate:
            return candidate
    return ""


def _lua_number(value: float) -> str:
    number = float(value)
    if not math.isfinite(number):
        raise ValueError(f"not a finite Lua number: {value!r}")
    if number.is_integer():
        return str(int(number))
    return repr(number)


def _compact_power_summary(power: Any) -> str:
    if not isinstance(power, dict):
        return _single_line(str(power))
    if power.get("error"):
        return _single_line(f"unavailable: {power['error']}")
    consumers = power.get("consumers") if isinstance(power.get("consumers"), dict) else {}
    generators = power.get("generators") if isinstance(power.get("generators"), list) else []
    generator_summary = ", ".join(
        f"{item.get('name', 'unknown')}={item.get('count', '?')}"
        for item in generators
        if isinstance(item, dict)
    ) or "none"
    parts = [
        f"network={power.get('network_id', 'unknown')}",
        f"poles={power.get('pole_count', 'unknown')}",
        f"generators={generator_summary}",
        (
            "consumers="
            f"{consumers.get('working', 0)} working/"
            f"{consumers.get('low_power', 0)} low/"
            f"{consumers.get('no_power', 0)} none/"
            f"{consumers.get('total', 0)} total"
        ),
        f"production_kw={power.get('production_kw', 'unknown')}",
        f"consumption_kw={power.get('consumption_kw', 'unknown')}",
        f"satisfaction={power.get('satisfaction', 'unknown')}",
    ]
    return _single_line("; ".join(parts))


def _compact_power_log_message(message: str) -> str:
    text = str(message)
    lower = text.lower()
    if text.startswith("tool_result:"):
        payload = _parse_mcp_text_payload(text.partition("tool_result:")[2].strip())
        compact = _compact_power_payload(payload)
        if compact:
            return compact
    if text.startswith("text:") and (
        "power grid operational" in lower
        or "no_power" in lower
        or " no power" in lower
        or "boiler no-fuel" in lower
        or "boiler_no_fuel" in lower
    ):
        return _single_line(text)
    return ""


def _compact_power_payload(payload: Any) -> str:
    if not isinstance(payload, dict):
        return ""
    source = payload.get("existing_plant") if isinstance(payload.get("existing_plant"), dict) else payload
    summary = source.get("summary") if isinstance(source.get("summary"), dict) else {}
    issues = source.get("issues") if isinstance(source.get("issues"), list) else []
    if not summary and not issues and "next_action" not in source and "status" not in source:
        return ""
    issue_types = [
        str(issue.get("type", "unknown"))
        for issue in issues
        if isinstance(issue, dict)
    ]
    parts = [
        f"steam_power status={source.get('status', payload.get('status', 'unknown'))}",
        f"issues={summary.get('issue_count', len(issue_types))}",
        f"critical={summary.get('critical_issues', 'unknown')}",
    ]
    if issue_types:
        parts.append(f"types={', '.join(issue_types[:3])}")
    next_action = source.get("next_action") or payload.get("next_action")
    if next_action:
        parts.append(f"next={next_action}")
    return _single_line("; ".join(parts))


def _error_message(exc: Exception) -> str:
    message = str(exc)
    if not message:
        message = exc.__class__.__name__
    return _single_line(message)


def _number(value: Any) -> float:
    try:
        return float(value)
    except (TypeError, ValueError):
        return 0.0


def _single_line(value: str) -> str:
    return re.sub(r"\s+", " ", value).strip()[:240]


def _is_progress_message(lower: str) -> bool:
    return (
        "research completed" in lower
        or "verified working" in lower
        or "automation milestone" in lower
        or " power grid operational" in lower
        or "progress:" in lower
    )


def _is_low_value_progress_text(value: str) -> bool:
    normalized = str(value).lower()
    if any(pattern in normalized for pattern in LOW_VALUE_PROGRESS_PATTERNS):
        return True
    return "planning tick" in normalized and (
        "no change" in normalized or "state unchanged" in normalized
    )


def _is_power_message(lower: str) -> bool:
    if (
        lower.startswith("thinking:")
        or lower.startswith("autonomy ->")
        or lower.startswith("reply:")
        or lower.startswith("tool:")
    ):
        return False
    if "continuity ledger:" in lower or "\nplan:" in lower or "\nprogress:" in lower:
        return False
    if (
        "blocked non-read-only tool" in lower
        or "planner/reflection turn" in lower
        or "this turn may only use read-only diagnostics" in lower
    ):
        return False
    return (
        "power" in lower
        and (
            "operational" in lower
            or "no_power" in lower
            or "steam" in lower
            or "boiler" in lower
            or "electric pole" in lower
        )
    )


def _game_rejection_signature(message: str) -> str:
    _, _, suffix = message.partition("game_rejected:")
    text = (suffix or message).strip()
    if _looks_like_research_status_text(text) or _looks_like_invalid_request_text(text):
        return ""
    parsed = _parse_rejection_payload(text)
    if _is_research_status_payload(parsed) or _is_invalid_request_payload(parsed):
        return ""
    if isinstance(parsed, dict):
        parts = []
        entity = parsed.get("entity")
        recipe = parsed.get("recipe")
        error = parsed.get("error")
        if error:
            parts.append(str(error))
        if entity:
            parts.append(f"entity={entity}")
        if recipe:
            parts.append(f"recipe={recipe}")
        return " | ".join(parts)[:180] if parts else text[:180]
    if isinstance(parsed, str):
        return _single_line(parsed)[:180]
    return _single_line(text)[:180]


def _is_research_status_payload(value: Any) -> bool:
    return isinstance(value, dict) and (
        "researched_count" in value
        or "research_progress" in value
        or "research_queue" in value
        or "current_research" in value
    )


def _looks_like_research_status_text(text: str) -> bool:
    normalized = str(text).lower()
    return (
        "researched_count" in normalized
        or "research_progress" in normalized
        or "research_queue" in normalized
        or "current_research" in normalized
    )


def _is_invalid_request_payload(value: Any) -> bool:
    if not isinstance(value, dict):
        return False
    action_needed = str(value.get("action_needed", "")).lower()
    error = str(value.get("error", "")).lower()
    return (
        "invalid_request" in str(value.get("classification", "")).lower()
        or action_needed.startswith("fix_")
        or "value for required field" in error
        or "failed to deserialize" in error
        or "invalid type:" in error
        or "missing field" in error
    )


def _looks_like_invalid_request_text(text: str) -> bool:
    normalized = str(text).lower()
    return (
        "value for required field" in normalized
        or "failed to deserialize" in normalized
        or "invalid type:" in normalized
        or "missing field" in normalized
    )


def _game_rejection_lines(message: str) -> list[str]:
    return [
        line.strip()
        for line in str(message).splitlines()
        if "game_rejected:" in line
    ]


def _parse_rejection_payload(text: str):
    return _parse_mcp_text_payload(text)


def _parse_mcp_text_payload(text: str):
    try:
        parsed = json.loads(text)
    except json.JSONDecodeError:
        return text
    if isinstance(parsed, list) and parsed:
        first = parsed[0]
        if isinstance(first, dict) and isinstance(first.get("text"), str):
            try:
                return json.loads(first["text"])
            except json.JSONDecodeError:
                return first["text"]
    return parsed


def _is_placeholder(value: str) -> bool:
    normalized = value.strip().lower()
    return normalized in {
        "<goal>",
        "<updated goal>",
        "<what changed>",
        "<why the old plan was stale or complete>",
    }


def _verdict(
    report: BridgeRunReport,
    last_provider_pause_ts: float,
    progress_timestamps: list[float],
) -> str:
    last_progress_ts = max(progress_timestamps) if progress_timestamps else 0.0
    if report.provider_pauses and last_provider_pause_ts >= last_progress_ts:
        reset = f" until {report.provider_reset_until}" if report.provider_reset_until else ""
        return f"provider paused{reset}; safe to leave running"
    if report.recent_progress_events:
        return "safe to keep running: recent progress detected"
    if report.watchdog_aborts or any(count >= 3 for _, count in report.top_gameplay_rejections):
        return "operator attention needed: repeated gameplay failures without recent progress"
    if report.context_resets:
        return "operator attention useful: context resets occurred and no recent progress was detected"
    return "operator attention useful: no recent progress detected"


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
        default=os.environ.get("FACTORIO_RCON_HOST", "localhost"),
        help="RCON host for --live",
    )
    parser.add_argument(
        "--rcon-port",
        type=int,
        default=int(os.environ.get("FACTORIO_RCON_PORT", "27015")),
        help="RCON port for --live",
    )
    parser.add_argument(
        "--rcon-password",
        default=os.environ.get("FACTORIO_RCON_PASSWORD", "factorio"),
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
        print(json.dumps(report.to_dict(), indent=2, sort_keys=True))
    else:
        print(format_report(report))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
