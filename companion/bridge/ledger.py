"""Persistent per-agent objective ledger for bridge autonomy continuity."""

import os
from datetime import datetime
from pathlib import Path

from models import (
    LedgerRuntimeSettings,
    LedgerState,
    LedgerUpdate,
)


def _ledger_file(agent_name: str) -> Path:
    return Path(__file__).resolve().parent / f".ledger-{agent_name}.json"


def default_ledger_model() -> LedgerState:
    return LedgerState.default()


def default_ledger() -> dict:
    return default_ledger_model().to_dict()


def _stale_bootstrap_max_age_s() -> float:
    return LedgerRuntimeSettings.from_env(os.environ).stale_bootstrap_ledger_max_age_s


def _is_stale_bootstrap_ledger(ledger: LedgerState | dict) -> bool:
    return LedgerState.normalized(ledger).bootstrap_staleness_evidence(
        max_age_s=_stale_bootstrap_max_age_s(),
    ).is_stale


def load_ledger_model(agent_name: str) -> LedgerState:
    # json.JSONDecodeError and UnicodeDecodeError are both ValueError subclasses,
    # so (ValueError, OSError) covers corrupt JSON and non-UTF8/unreadable files.
    try:
        ledger = LedgerState.from_file_text(
            _ledger_file(agent_name).read_text(),
        )
    except (ValueError, OSError):
        return default_ledger_model()
    if _is_stale_bootstrap_ledger(ledger):
        return default_ledger_model()
    return ledger


def load_ledger(agent_name: str) -> dict:
    return load_ledger_model(agent_name).to_dict()


def save_ledger_model(agent_name: str, ledger: LedgerState | dict) -> None:
    # Atomic write: serialize first, write to a temp file, then os.replace onto
    # the target so an interrupted/failed write can never truncate the real
    # ledger. Persistence failures are surfaced (printed), not silently swallowed.
    path = _ledger_file(agent_name)
    tmp = path.with_name(path.name + ".tmp")
    try:
        payload = LedgerState.normalized(ledger).to_json_line()
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


def save_ledger(agent_name: str, ledger: dict) -> None:
    return save_ledger_model(agent_name, ledger)


def parse_ledger_trailer_model(source: str | LedgerUpdate) -> LedgerUpdate | None:
    return LedgerUpdate.from_trailer_text(source)


def parse_ledger_trailer(source: str | LedgerUpdate) -> dict | None:
    update = parse_ledger_trailer_model(source)
    return update.to_dict() if update is not None else None


def apply_ledger_update_model(agent_name: str, source: str | LedgerUpdate) -> LedgerState:
    parsed = parse_ledger_trailer_model(source)
    current = load_ledger_model(agent_name)
    if parsed is None:
        return current

    ledger = current.merged_with(
        parsed,
        updated_at=datetime.now().isoformat(),
        max_progress_notes=10,
    )
    save_ledger_model(agent_name, ledger)
    return ledger


def apply_ledger_update(agent_name: str, source: str | LedgerUpdate) -> dict:
    return apply_ledger_update_model(agent_name, source).to_dict()


def strip_ledger_trailer(text: str) -> str:
    return LedgerUpdate.strip_trailer_text(text)


def render_ledger(ledger: LedgerState | dict) -> str:
    return LedgerState.normalized(ledger).render(recent_progress_count=3)
