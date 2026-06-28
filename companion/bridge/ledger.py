"""Persistent per-agent objective ledger for bridge autonomy continuity."""

import json
import os
import re
from datetime import datetime
from pathlib import Path


LEDGER_RE = re.compile(r"<ledger>(.*?)</ledger>", re.DOTALL | re.IGNORECASE)


def _ledger_file(agent_name: str) -> Path:
    return Path(__file__).resolve().parent / f".ledger-{agent_name}.json"


def default_ledger() -> dict:
    return {
        "objective": "",
        "plan_steps": [],
        "progress_notes": [],
        "updated_at": "",
    }


def _str_list(value) -> list:
    """Coerce an on-disk value into a list of non-empty strings."""
    if not isinstance(value, list):
        return []
    return [str(item) for item in value if isinstance(item, str)]


def _normalize(data: dict) -> dict:
    """Coerce a loaded ledger dict to the canonical schema/types so callers
    never trip over null or wrong-typed fields (e.g. {"plan_steps": null})."""
    objective = data.get("objective", "")
    updated_at = data.get("updated_at", "")
    return {
        "objective": objective if isinstance(objective, str) else "",
        "plan_steps": _str_list(data.get("plan_steps", [])),
        "progress_notes": _str_list(data.get("progress_notes", [])),
        "updated_at": updated_at if isinstance(updated_at, str) else "",
    }


def load_ledger(agent_name: str) -> dict:
    # json.JSONDecodeError and UnicodeDecodeError are both ValueError subclasses,
    # so (ValueError, OSError) covers corrupt JSON and non-UTF8/unreadable files.
    try:
        data = json.loads(_ledger_file(agent_name).read_text())
    except (ValueError, OSError):
        return default_ledger()
    if isinstance(data, dict):
        return _normalize(data)
    return default_ledger()


def save_ledger(agent_name: str, ledger: dict) -> None:
    # Atomic write: serialize first, write to a temp file, then os.replace onto
    # the target so an interrupted/failed write can never truncate the real
    # ledger. Persistence failures are surfaced (printed), not silently swallowed.
    path = _ledger_file(agent_name)
    tmp = path.with_name(path.name + ".tmp")
    try:
        payload = json.dumps(ledger) + "\n"
    except TypeError as e:
        print(f"[ledger] WARNING: refusing to save unserializable ledger for "
              f"{agent_name}: {e}")
        return None
    try:
        tmp.write_text(payload)
        os.replace(tmp, path)
    except OSError as e:
        print(f"[ledger] WARNING: failed to persist ledger for {agent_name}: {e}")
        try:
            tmp.unlink(missing_ok=True)
        except OSError:
            pass
    return None


def parse_ledger_trailer(text: str) -> dict | None:
    match = LEDGER_RE.search(text)
    if not match:
        return None

    parsed = {}
    plan_steps = []
    saw_plan = False
    in_plan = False

    for raw_line in match.group(1).splitlines():
        line = raw_line.strip()
        if not line:
            continue
        key, sep, value = line.partition(":")
        key_lower = key.strip().lower()
        if sep and key_lower == "objective":
            parsed["objective"] = value.strip()
            in_plan = False
        elif sep and key_lower == "plan":
            saw_plan = True
            in_plan = True
        elif sep and key_lower == "progress":
            parsed["progress"] = value.strip()
            in_plan = False
        elif in_plan and line.startswith("- "):
            plan_steps.append(line[2:].strip())

    if saw_plan:
        parsed["plan_steps"] = [step for step in plan_steps if step]

    return parsed


def apply_ledger_update(agent_name: str, text: str) -> dict:
    parsed = parse_ledger_trailer(text)
    current = load_ledger(agent_name)
    if parsed is None:
        return current

    ledger = {
        "objective": str(current.get("objective", "")),
        "plan_steps": list(current.get("plan_steps", [])),
        "progress_notes": list(current.get("progress_notes", [])),
        "updated_at": str(current.get("updated_at", "")),
    }

    objective = parsed.get("objective", "")
    if objective:
        ledger["objective"] = objective
        ledger["plan_steps"] = list(parsed.get("plan_steps", []))
    elif "plan_steps" in parsed:
        ledger["plan_steps"] = list(parsed["plan_steps"])

    progress = parsed.get("progress", "")
    if progress:
        ledger["progress_notes"].append(progress)
        ledger["progress_notes"] = ledger["progress_notes"][-10:]

    ledger["updated_at"] = datetime.now().isoformat()
    save_ledger(agent_name, ledger)
    return ledger


def strip_ledger_trailer(text: str) -> str:
    if not LEDGER_RE.search(text):
        return text
    stripped = LEDGER_RE.sub("", text)
    stripped = re.sub(r"\n{3,}", "\n\n", stripped)
    return stripped.strip()


def render_ledger(ledger: dict) -> str:
    objective = str(ledger.get("objective", "")).strip()
    if not objective:
        return ""

    lines = [
        f"Continuity ledger: continue the committed objective, do not restart it: {objective}",
    ]
    plan_steps = ledger.get("plan_steps", [])
    if plan_steps:
        lines.append("Plan:")
        for index, step in enumerate(plan_steps, start=1):
            lines.append(f"{index}. {step}")
    progress_notes = ledger.get("progress_notes", [])[-3:]
    if progress_notes:
        lines.append("Recent progress:")
        for note in progress_notes:
            lines.append(f"- {note}")
    return "\n".join(lines)
