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
MAX_REFLECTION_ITEM_TEXT = 180
MAX_RENDERED_EVENTS = 5
MAX_RENDERED_EVENT_TEXT = 500
USEFUL_EVENT_KINDS = {"progress", "discovery", "milestone"}
TRANSIENT_FAILURE_PATTERNS = (
    "usage limit reached",
    "request rejected (429)",
    "provider usage limit",
    "reached maximum number of turns",
    "expected value at line 1 column 1",
    "stream idle timeout",
    "tick timeout",
    "agent tick exceeded",
    "context window limit",
    "context-window limit",
    "context length",
    "no electric poles found in area",
    "missing field `success`",
    "missing field success",
    "packet too large",
    "bad argument #1 of 2 to 'pairs' (table expected, got nil)",
    "failed to queue research - check if another research is in progress",
)
LOW_VALUE_PROGRESS_PATTERNS = (
    "no infrastructure yet deployed",
    "plan fully validated and awaiting execution",
    "plan validated and ready for execution",
    "plan unchanged and ready for execution",
)
LOW_VALUE_REFLECTION_PATTERNS = (
    "no prior progress",
    "no infrastructure yet deployed",
    "fresh deployment",
    "zero-state",
    "nothing built",
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


def _compact_reflection_item(text: str) -> str:
    compact = re.sub(r"\s+", " ", str(text)).strip()
    if len(compact) <= MAX_REFLECTION_ITEM_TEXT:
        return compact
    return compact[:MAX_REFLECTION_ITEM_TEXT].rstrip() + "..."


def _should_drop_reflection_item(text: str) -> bool:
    normalized = str(text).lower()
    return (
        _is_transient_failure_text(normalized)
        or any(pattern in normalized for pattern in LOW_VALUE_REFLECTION_PATTERNS)
    )


def _str_list(value) -> list:
    """Coerce an on-disk value into bounded, prompt-worthy lesson strings."""
    if not isinstance(value, list):
        return []
    items = []
    seen = set()
    for item in value:
        if not isinstance(item, str):
            continue
        compact = _compact_reflection_item(item)
        if not compact or _should_drop_reflection_item(compact):
            continue
        if compact in seen:
            continue
        seen.add(compact)
        items.append(compact)
        if len(items) >= MAX_REFLECTION_ITEMS:
            break
    return items


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
    if any(pattern in normalized for pattern in LOW_VALUE_PROGRESS_PATTERNS):
        return True
    return "planning tick" in normalized and (
        "no change" in normalized or "state unchanged" in normalized
    )


def _should_drop_event(kind: str, text: str) -> bool:
    return (
        _is_transient_failure_event(kind, text)
        or _is_low_value_progress_event(kind, text)
    )


def _event_kind(value) -> str:
    return value if value in EVENT_KINDS else "progress"


def _compact_event_text(text: str, limit: int = MAX_RENDERED_EVENT_TEXT) -> str:
    compact = re.sub(r"\s+", " ", str(text)).strip()
    if limit <= 0 or len(compact) <= limit:
        return compact
    return compact[:limit].rstrip() + "..."


def coalesce_events(events: list[dict], max_items: int = MAX_RENDERED_EVENTS) -> list[dict]:
    """Return prompt-ready events with adjacent identical entries collapsed.

    The journal stays append-only and raw on disk; this compaction is only for
    prompt injection so repeated failures don't crowd out useful context.
    """
    events = events if isinstance(events, list) else []
    try:
        max_items = int(max_items)
    except (TypeError, ValueError):
        max_items = MAX_RENDERED_EVENTS
    if max_items <= 0:
        return []

    compacted: list[dict] = []
    for raw in events:
        if not isinstance(raw, dict):
            continue
        kind = _event_kind(raw.get("kind"))
        text = _compact_event_text(raw.get("text", ""))
        if not text or _should_drop_event(kind, text):
            continue
        if compacted and compacted[-1]["kind"] == kind and compacted[-1]["text"] == text:
            compacted[-1]["count"] += 1
            compacted[-1]["ts"] = str(raw.get("ts", compacted[-1].get("ts", "")))
            continue
        compacted.append({
            "kind": kind,
            "text": text,
            "count": 1,
            "ts": str(raw.get("ts", "")),
        })

    rendered = compacted[-max_items:]
    if any(event["kind"] in USEFUL_EVENT_KINDS for event in rendered):
        return rendered
    if max_items <= 1:
        return rendered

    # A burst of distinct gameplay failures can otherwise hide the last useful
    # state transition, leaving the next prompt with only "what failed" and no
    # "what changed." Reserve one slot for the latest non-failure if the final
    # window would be all failures.
    for event in reversed(compacted[:-max_items]):
        if event["kind"] in USEFUL_EVENT_KINDS:
            return [event] + rendered[-(max_items - 1):]
    return rendered


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
        parsed[key] = _str_list(parsed[key])
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
        reflection["structures"] = _str_list(parsed["structures"])
    if "error_tips" in parsed:
        reflection["error_tips"] = _str_list(parsed["error_tips"])
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
    recent_events = coalesce_events(events, MAX_RENDERED_EVENTS)
    structures = _str_list(reflection.get("structures", []))
    error_tips = _str_list(reflection.get("error_tips", []))
    if not recent_events and not structures and not error_tips:
        return ""

    lines = []
    if recent_events:
        lines.append("Recent events:")
        for event in recent_events:
            kind = _event_kind(event.get("kind"))
            count = event.get("count", 1)
            repeat = f" (x{count})" if isinstance(count, int) and count > 1 else ""
            lines.append(f"- {kind}{repeat}: {str(event.get('text', '')).strip()}")
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
