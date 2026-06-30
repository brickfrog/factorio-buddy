"""Persistent per-agent objective ledger for bridge autonomy continuity."""

import json
import os
import re
from datetime import datetime
from pathlib import Path

from models import LedgerState


LEDGER_RE = re.compile(r"<ledger>(.*?)</ledger>", re.DOTALL | re.IGNORECASE)
STALE_BOOTSTRAP_PROGRESS_PATTERNS = (
    "no infrastructure yet deployed",
)
LOW_VALUE_PROGRESS_PATTERNS = (
    "plan fully validated and awaiting execution",
    "plan validated and ready for execution",
    "plan unchanged and ready for execution",
)
STALE_BOOTSTRAP_OBJECTIVE_PATTERNS = (
    "establish initial extraction infrastructure",
)
DEFAULT_STALE_BOOTSTRAP_LEDGER_MAX_AGE_S = 1800.0


def _ledger_file(agent_name: str) -> Path:
    return Path(__file__).resolve().parent / f".ledger-{agent_name}.json"


def default_ledger() -> dict:
    return LedgerState.default().to_dict()


def _normalize(data: dict) -> dict:
    """Coerce a loaded ledger dict to the canonical schema/types so callers
    never trip over null or wrong-typed fields (e.g. {"plan_steps": null})."""
    ledger = LedgerState.coerce(data).to_dict()
    ledger["progress_notes"] = [
        note for note in ledger.get("progress_notes", [])
        if not _is_low_value_progress_note(note)
    ]
    return ledger


def _is_low_value_progress_note(text: str) -> bool:
    normalized = str(text).lower()
    if any(pattern in normalized for pattern in LOW_VALUE_PROGRESS_PATTERNS):
        return True
    return "planning tick" in normalized and (
        "no change" in normalized or "state unchanged" in normalized
    )


def _stale_bootstrap_max_age_s() -> float:
    try:
        return float(os.environ.get(
            "BRIDGE_STALE_BOOTSTRAP_LEDGER_MAX_AGE_S",
            str(DEFAULT_STALE_BOOTSTRAP_LEDGER_MAX_AGE_S),
        ))
    except (TypeError, ValueError):
        return DEFAULT_STALE_BOOTSTRAP_LEDGER_MAX_AGE_S


def _ledger_age_s(updated_at: str) -> float | None:
    if not updated_at:
        return None
    try:
        updated = datetime.fromisoformat(updated_at)
    except ValueError:
        return None
    now = datetime.now(updated.tzinfo) if updated.tzinfo else datetime.now()
    return max(0.0, (now - updated).total_seconds())


def _is_stale_bootstrap_ledger(ledger: dict) -> bool:
    objective = str(ledger.get("objective", "")).lower()
    progress = "\n".join(str(note) for note in ledger.get("progress_notes", [])).lower()
    if not any(pattern in objective for pattern in STALE_BOOTSTRAP_OBJECTIVE_PATTERNS):
        return False
    if not any(pattern in progress for pattern in STALE_BOOTSTRAP_PROGRESS_PATTERNS):
        return False
    age = _ledger_age_s(str(ledger.get("updated_at", "")))
    return age is not None and age > _stale_bootstrap_max_age_s()


def load_ledger(agent_name: str) -> dict:
    # json.JSONDecodeError and UnicodeDecodeError are both ValueError subclasses,
    # so (ValueError, OSError) covers corrupt JSON and non-UTF8/unreadable files.
    try:
        data = json.loads(_ledger_file(agent_name).read_text())
    except (ValueError, OSError):
        return default_ledger()
    if isinstance(data, dict):
        ledger = _normalize(data)
        if _is_stale_bootstrap_ledger(ledger):
            return default_ledger()
        return ledger
    return default_ledger()


def save_ledger(agent_name: str, ledger: dict) -> None:
    # Atomic write: serialize first, write to a temp file, then os.replace onto
    # the target so an interrupted/failed write can never truncate the real
    # ledger. Persistence failures are surfaced (printed), not silently swallowed.
    path = _ledger_file(agent_name)
    tmp = path.with_name(path.name + ".tmp")
    try:
        payload = json.dumps(LedgerState.coerce(ledger).to_dict()) + "\n"
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
    if progress and not _is_low_value_progress_note(progress):
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
