"""Re-exported bridge model package.

The model definitions are split by domain, while this shim preserves the
legacy `from models import X` surface used by bridge modules and tests.
"""

from __future__ import annotations

from importlib import import_module as _import_module
import sys as _sys
from typing import Any as _Any

_MODULE_NAMES = (
    "base",
    "live",
    "tool_result",
    "rcon_models",
    "telemetry_models",
    "eval_models",
    "power_models",
    "bridge_log",
    "tool_schema",
    "sdk_models",
    "input_models",
    "settings_models",
    "response_models",
    "ledger_models",
    "journal_models",
    "skill_models",
    "learning_models",
)
_META_NAMES = {
    "__builtins__",
    "__cached__",
    "__doc__",
    "__file__",
    "__loader__",
    "__name__",
    "__package__",
    "__spec__",
    "annotations",
}
_OWNER_MODULES = {
    "ledger_models": ("ledger", "LedgerState"),
    "journal_models": ("journal", "JournalEvent"),
    "skill_models": ("skills", "SkillDefinition"),
    "learning_models": ("learning", "LearningProposal"),
}

_modules: list[_Any] = []
_exports: dict[str, _Any] = {}
_loaded_module_names: set[str] = set()
_deferred_module_names: set[str] = set()


def _owner_is_initializing(module_name: str) -> bool:
    owner = _OWNER_MODULES.get(module_name)
    if owner is None:
        return False
    owner_name, ready_attr = owner
    owner_module = _sys.modules.get(owner_name)
    return owner_module is not None and not hasattr(owner_module, ready_attr)


def _record_exports(module: _Any) -> None:
    _modules.append(module)
    for key, value in vars(module).items():
        if key not in _META_NAMES:
            _exports[key] = value


def _backfill_modules() -> None:
    for module in _modules:
        for key, value in _exports.items():
            if key not in _META_NAMES:
                vars(module).setdefault(key, value)


def _load_model_module(module_name: str) -> _Any:
    if module_name in _loaded_module_names:
        return _sys.modules[f"{__name__}.{module_name}"]
    module = _import_module(f"{__name__}.{module_name}")
    _loaded_module_names.add(module_name)
    _deferred_module_names.discard(module_name)
    _record_exports(module)
    _backfill_modules()
    globals().update(_exports)
    return module


for _module_name in _MODULE_NAMES:
    if _owner_is_initializing(_module_name):
        _deferred_module_names.add(_module_name)
        continue
    _module = _import_module(f"{__name__}.{_module_name}")
    _loaded_module_names.add(_module_name)
    _record_exports(_module)

# Some legacy methods reference symbols that used to live later in the flat
# module. After every split module is imported, backfill each namespace with
# the complete symbol table so those runtime lookups remain compatible.
_backfill_modules()


def __getattr__(name: str) -> _Any:
    for module_name in _MODULE_NAMES:
        if module_name not in _loaded_module_names:
            _load_model_module(module_name)
            if name in _exports:
                return _exports[name]
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")

globals().update(_exports)
__all__ = sorted(_key for _key in _exports if not _key.startswith("_"))
