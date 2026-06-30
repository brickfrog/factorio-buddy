"""Append-only per-agent journal and reflected lessons for bridge autonomy."""

import json
import os
import re
from datetime import datetime
from pathlib import Path

from models import JOURNAL_EVENT_KINDS, JournalEvent


REFLECTION_RE = re.compile(r"<reflection>(.*?)</reflection>", re.DOTALL | re.IGNORECASE)
EVENT_KINDS = JOURNAL_EVENT_KINDS
MAX_REFLECTION_ITEMS = 12
TRANSIENT_FAILURE_PATTERNS = (
    "usage limit reached",
    "request rejected (429)",
    "reached maximum number of turns",
    "expected value at line 1 column 1",
    "stream idle timeout",
    "no electric poles found in area",
    "missing field `success`",
    "missing field success",
    "packet too large",
    "bad argument #1 of 2 to 'pairs' (table expected, got nil)",
    "failed to queue research - check if another research is in progress",
)
LOW_VALUE_PROGRESS_PATTERNS = (
    "no infrastructure yet deployed",
)


def _journal_file(agent_name: str) -> Path:
    return Path(__file__).resolve().parent / f".journal-{agent_name}.jsonl"


def _reflection_file(agent_name: str) -> Path:
    return Path(__file__).resolve().parent / f".reflection-{agent_name}.json"


def default_reflection() -> dict:
    return {
        "structures": [],
        "error_tips": [],
        "updated_at": "",
    }


def _str_list(value) -> list:
    """Coerce an on-disk value into a bounded list of non-empty strings."""
    if not isinstance(value, list):
        return []
    return [str(item) for item in value if isinstance(item, str)][:MAX_REFLECTION_ITEMS]


def _normalize(data: dict) -> dict:
    if not isinstance(data, dict):
        return default_reflection()
    updated_at = data.get("updated_at", "")
    return {
        "structures": _str_list(data.get("structures", [])),
        "error_tips": _str_list(data.get("error_tips", [])),
        "updated_at": updated_at if isinstance(updated_at, str) else "",
    }


def _is_transient_failure_text(text: str) -> bool:
    normalized = str(text).lower()
    return any(pattern in normalized for pattern in TRANSIENT_FAILURE_PATTERNS)


def _is_transient_failure_event(kind: str, text: str) -> bool:
    return kind == "failure" and _is_transient_failure_text(text)


def _is_low_value_progress_event(kind: str, text: str) -> bool:
    if kind != "progress":
        return False
    normalized = str(text).lower()
    return any(pattern in normalized for pattern in LOW_VALUE_PROGRESS_PATTERNS)


def _should_drop_event(kind: str, text: str) -> bool:
    return (
        _is_transient_failure_event(kind, text)
        or _is_low_value_progress_event(kind, text)
    )


def append_event(agent_name: str, kind: str, text: str) -> None:
    event = JournalEvent.create(
        ts=datetime.now().isoformat(),
        kind=kind,
        text=text,
    )
    if _should_drop_event(event.kind, event.text):
        return None
    path = _journal_file(agent_name)
    try:
        with path.open("a", encoding="utf-8") as f:
            f.write(json.dumps(event.to_dict()) + "\n")
    except OSError as e:
        print(f"[journal] WARNING: failed to append journal event for {agent_name}: {e}")
    return None


def load_events(agent_name: str, limit: int = 20) -> list[dict]:
    try:
        raw_lines = _journal_file(agent_name).read_text().splitlines()
    except (ValueError, OSError):
        return []

    events = []
    for line in raw_lines:
        try:
            data = json.loads(line)
        except (ValueError, TypeError):
            continue
        if not isinstance(data, dict):
            continue
        event = JournalEvent.from_mapping(data)
        if not event:
            continue
        if _should_drop_event(event.kind, event.text):
            continue
        events.append(event.to_dict())

    try:
        limit = int(limit)
    except (TypeError, ValueError):
        limit = 20
    if limit <= 0:
        return []
    return events[-limit:]


def count_events(agent_name: str) -> int:
    try:
        raw_lines = _journal_file(agent_name).read_text().splitlines()
    except (ValueError, OSError):
        return 0
    count = 0
    for line in raw_lines:
        try:
            data = json.loads(line)
        except (ValueError, TypeError):
            continue
        if isinstance(data, dict):
            count += 1
    return count


def should_reflect(event_count: int, interval: int = 16) -> bool:
    try:
        event_count = int(event_count)
        interval = int(interval)
    except (TypeError, ValueError):
        return False
    return event_count > 0 and interval > 0 and event_count % interval == 0


def load_reflection(agent_name: str) -> dict:
    try:
        data = json.loads(_reflection_file(agent_name).read_text())
    except (ValueError, OSError):
        return default_reflection()
    if isinstance(data, dict):
        return _normalize(data)
    return default_reflection()


def save_reflection(agent_name: str, reflection: dict) -> None:
    path = _reflection_file(agent_name)
    tmp = path.with_name(path.name + ".tmp")
    try:
        payload = json.dumps(_normalize(reflection)) + "\n"
    except TypeError as e:
        print(f"[journal] WARNING: refusing to save unserializable reflection for "
              f"{agent_name}: {e}")
        return None
    try:
        tmp.write_text(payload)
        os.replace(tmp, path)
    except OSError as e:
        print(f"[journal] WARNING: failed to persist reflection for {agent_name}: {e}")
        try:
            tmp.unlink(missing_ok=True)
        except OSError:
            pass
    return None


def parse_reflection(text: str) -> dict | None:
    if not isinstance(text, str):
        return None
    match = REFLECTION_RE.search(text)
    if not match:
        return None

    parsed = {}
    active_key = None
    for raw_line in match.group(1).splitlines():
        line = raw_line.strip()
        if not line:
            continue
        key, sep, _value = line.partition(":")
        key_lower = key.strip().lower()
        if sep and key_lower == "structures":
            active_key = "structures"
            parsed.setdefault(active_key, [])
        elif sep and key_lower == "error_tips":
            active_key = "error_tips"
            parsed.setdefault(active_key, [])
        elif active_key and line.startswith("- "):
            item = line[2:].strip()
            if item:
                parsed[active_key].append(item)

    for key in list(parsed.keys()):
        parsed[key] = parsed[key][:MAX_REFLECTION_ITEMS]
    return parsed


def apply_reflection_update(agent_name: str, text: str) -> dict:
    parsed = parse_reflection(text)
    current = load_reflection(agent_name)
    if parsed is None:
        return current

    reflection = {
        "structures": list(current.get("structures", [])),
        "error_tips": list(current.get("error_tips", [])),
        "updated_at": str(current.get("updated_at", "")),
    }
    if "structures" in parsed:
        reflection["structures"] = list(parsed["structures"])[:MAX_REFLECTION_ITEMS]
    if "error_tips" in parsed:
        reflection["error_tips"] = list(parsed["error_tips"])[:MAX_REFLECTION_ITEMS]
    reflection["updated_at"] = datetime.now().isoformat()
    save_reflection(agent_name, reflection)
    return reflection


def strip_reflection_trailer(text: str) -> str:
    if not isinstance(text, str):
        return ""
    if not REFLECTION_RE.search(text):
        return text
    stripped = REFLECTION_RE.sub("", text)
    stripped = re.sub(r"\n{3,}", "\n\n", stripped)
    return stripped.strip()


def render_memory(events: list[dict], reflection: dict) -> str:
    events = events if isinstance(events, list) else []
    reflection = reflection if isinstance(reflection, dict) else {}
    recent_events = [e for e in events[-5:] if isinstance(e, dict)]
    recent_events = [
        e for e in recent_events
        if not _is_transient_failure_event(
            e.get("kind") if e.get("kind") in EVENT_KINDS else "progress",
            str(e.get("text", "")),
        )
    ]
    structures = _str_list(reflection.get("structures", []))
    error_tips = _str_list(reflection.get("error_tips", []))
    if not recent_events and not structures and not error_tips:
        return ""

    lines = []
    if recent_events:
        lines.append("Recent events:")
        for event in recent_events:
            kind = event.get("kind") if event.get("kind") in EVENT_KINDS else "progress"
            lines.append(f"- {kind}: {str(event.get('text', '')).strip()}")
    if structures or error_tips:
        if lines:
            lines.append("")
        lines.append("Lessons (EXISTING STRUCTURES / ERROR TIPS):")
        if structures:
            lines.append("EXISTING STRUCTURES:")
            for item in structures:
                lines.append(f"- {item}")
        if error_tips:
            lines.append("ERROR TIPS:")
            for item in error_tips:
                lines.append(f"- {item}")
    return "\n".join(lines)
