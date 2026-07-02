"""Append-only per-agent journal and reflected lessons for bridge autonomy."""

import os
from datetime import datetime
from pathlib import Path

from models import (
    BridgeTextLines,
    JournalEvent,
    JournalPromptEvent,
    JournalWindow,
    ProgressSignal,
    ReflectionDraft,
    ReflectionMemory,
)


MAX_REFLECTION_ITEMS = 12
MAX_REFLECTION_ITEM_TEXT = 180
MAX_RENDERED_EVENTS = 5
MAX_RENDERED_EVENT_TEXT = 500
USEFUL_EVENT_KINDS = {"progress", "discovery", "milestone"}


def _journal_file(agent_name: str) -> Path:
    return Path(__file__).resolve().parent / f".journal-{agent_name}.jsonl"


def _reflection_file(agent_name: str) -> Path:
    return Path(__file__).resolve().parent / f".reflection-{agent_name}.json"


def default_reflection_model() -> ReflectionMemory:
    return ReflectionMemory()


def default_reflection() -> dict:
    return default_reflection_model().to_dict()


def coalesce_events_model(
    events: JournalWindow | list[dict | JournalEvent],
    max_items: int = MAX_RENDERED_EVENTS,
) -> list[JournalPromptEvent]:
    """Return prompt-ready events with adjacent identical entries collapsed.

    The journal stays append-only and raw on disk; this compaction is only for
    prompt injection so repeated failures don't crowd out useful context.
    """
    window = events if isinstance(events, JournalWindow) else JournalWindow.coerce(events)
    return window.prompt_events(
        max_items=max_items,
        text_limit=MAX_RENDERED_EVENT_TEXT,
        useful_kinds=USEFUL_EVENT_KINDS,
    )


def coalesce_events(
    events: JournalWindow | list[dict | JournalEvent],
    max_items: int = MAX_RENDERED_EVENTS,
) -> list[dict]:
    return [event.to_dict() for event in coalesce_events_model(events, max_items)]


def append_event(
    agent_name: str,
    kind: str,
    text: str,
    *,
    signal: ProgressSignal | str | None = None,
) -> None:
    event = JournalEvent.create(
        ts=datetime.now().isoformat(),
        kind=kind,
        text=text,
        signal=signal,
    )
    if event.should_drop():
        return None
    path = _journal_file(agent_name)
    try:
        with path.open("a", encoding="utf-8") as f:
            f.write(event.to_json_line())
    except OSError as e:
        print(f"[journal] WARNING: failed to append journal event for {agent_name}: {e}")
    return None


def load_events_model(agent_name: str, limit: int = 20) -> JournalWindow:
    try:
        raw_lines = BridgeTextLines.from_text(
            _journal_file(agent_name).read_text(),
            keep_blank=False,
        ).lines
    except (ValueError, OSError):
        return JournalWindow()

    events = []
    for line in raw_lines:
        event = JournalEvent.from_json_line(line)
        if not event:
            continue
        if event.should_drop():
            continue
        events.append(event)

    try:
        limit = int(limit)
    except (TypeError, ValueError):
        limit = 20
    if limit <= 0:
        return JournalWindow()
    return JournalWindow(events=events[-limit:])


def load_events(agent_name: str, limit: int = 20) -> list[dict]:
    return [event.to_dict() for event in load_events_model(agent_name, limit).events]


def count_events(agent_name: str) -> int:
    try:
        raw_lines = BridgeTextLines.from_text(
            _journal_file(agent_name).read_text(),
            keep_blank=False,
        ).lines
    except (ValueError, OSError):
        return 0
    count = 0
    for line in raw_lines:
        if JournalEvent.from_json_line(line):
            count += 1
    return count


def should_reflect(event_count: int, interval: int = 16) -> bool:
    try:
        event_count = int(event_count)
        interval = int(interval)
    except (TypeError, ValueError):
        return False
    return event_count > 0 and interval > 0 and event_count % interval == 0


def load_reflection_model(agent_name: str) -> ReflectionMemory:
    try:
        memory = ReflectionMemory.from_file_text(
            _reflection_file(agent_name).read_text(),
            max_items=MAX_REFLECTION_ITEMS,
            max_len=MAX_REFLECTION_ITEM_TEXT,
        )
    except (ValueError, OSError):
        return default_reflection_model()
    return memory


def load_reflection(agent_name: str) -> dict:
    return load_reflection_model(agent_name).to_dict()


def save_reflection_model(agent_name: str, reflection: ReflectionMemory | dict) -> None:
    path = _reflection_file(agent_name)
    tmp = path.with_name(path.name + ".tmp")
    try:
        payload = ReflectionMemory.coerce(
            reflection,
            max_items=MAX_REFLECTION_ITEMS,
            max_len=MAX_REFLECTION_ITEM_TEXT,
        ).to_json_line()
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


def save_reflection(agent_name: str, reflection: dict) -> None:
    return save_reflection_model(agent_name, reflection)


def parse_reflection_model(source: str | ReflectionDraft) -> ReflectionDraft | None:
    return ReflectionDraft.from_trailer_text(
        source,
        max_items=MAX_REFLECTION_ITEMS,
        max_len=MAX_REFLECTION_ITEM_TEXT,
    )


def parse_reflection(source: str | ReflectionDraft) -> dict | None:
    draft = parse_reflection_model(source)
    return draft.to_sparse_dict() if draft is not None else None


def apply_reflection_update_model(
    agent_name: str,
    source: str | ReflectionDraft,
) -> ReflectionMemory:
    parsed = parse_reflection_model(source)
    current = load_reflection_model(agent_name)
    if parsed is None:
        return current

    reflection = current.merged_with(
        parsed,
        updated_at=datetime.now().isoformat(),
        max_items=MAX_REFLECTION_ITEMS,
        max_len=MAX_REFLECTION_ITEM_TEXT,
    )
    save_reflection_model(agent_name, reflection)
    return reflection


def apply_reflection_update(agent_name: str, source: str | ReflectionDraft) -> dict:
    return apply_reflection_update_model(agent_name, source).to_dict()


def strip_reflection_trailer(text: str) -> str:
    return ReflectionDraft.strip_trailer_text(text)


def render_memory(
    events: JournalWindow | list[dict | JournalEvent],
    reflection: ReflectionMemory | dict,
) -> str:
    journal_window = events if isinstance(events, JournalWindow) else JournalWindow.coerce(events)
    reflection_memory = (
        reflection
        if isinstance(reflection, ReflectionMemory)
        else ReflectionMemory.coerce(
            reflection,
            max_items=MAX_REFLECTION_ITEMS,
            max_len=MAX_REFLECTION_ITEM_TEXT,
        )
    )
    recent_events = journal_window.prompt_events(
        max_items=MAX_RENDERED_EVENTS,
        text_limit=MAX_RENDERED_EVENT_TEXT,
        useful_kinds=USEFUL_EVENT_KINDS,
    )
    structures = reflection_memory.structures
    error_tips = reflection_memory.error_tips
    if not recent_events and not structures and not error_tips:
        return ""

    lines = []
    if recent_events:
        lines.append("Recent events:")
        for event in recent_events:
            lines.append(event.render_line())
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
