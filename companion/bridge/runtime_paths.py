"""Runtime state paths for bridge files that should not live beside source."""

from __future__ import annotations

import os
from pathlib import Path
from typing import Iterable

from models.settings_models import FactorioPathSettings

BRIDGE_DIR = Path(__file__).resolve().parent
COMPANION_ROOT = BRIDGE_DIR.parent


def bridge_state_dir(
    *,
    env: object | None = None,
    cwd: Path | None = None,
    create: bool = True,
) -> Path:
    settings = FactorioPathSettings.from_env(os.environ if env is None else env)
    if settings.bridge_state_dir_path:
        return _ensure_dir(settings.bridge_state_dir_path, create=create)
    if settings.server_data:
        return _ensure_dir(Path(settings.server_data) / "bridge-state", create=create)

    starts = (cwd or Path.cwd(), COMPANION_ROOT)
    for start in starts:
        for candidate in _ancestor_state_dirs(start):
            if candidate.parent.exists():
                return _ensure_dir(candidate, create=create)

    return _ensure_dir(COMPANION_ROOT / ".factorio-server-data" / "bridge-state", create=create)


def state_file(name: str) -> Path:
    return bridge_state_dir() / name


def legacy_bridge_file(name: str) -> Path:
    return BRIDGE_DIR / name


def read_candidates(name: str) -> tuple[Path, ...]:
    primary = state_file(name)
    legacy = legacy_bridge_file(name)
    if legacy == primary or not legacy.exists():
        return (primary,)
    return (primary, legacy)


def _ancestor_state_dirs(start: Path) -> Iterable[Path]:
    search = start.resolve()
    while True:
        yield search / ".factorio-server-data" / "bridge-state"
        if search == search.parent:
            return
        search = search.parent


def _ensure_dir(path: Path, *, create: bool) -> Path:
    if create:
        path.mkdir(parents=True, exist_ok=True)
    return path
