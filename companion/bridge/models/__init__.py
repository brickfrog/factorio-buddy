"""Re-exported bridge model package.

The model definitions are split by domain, while this shim preserves the
legacy `from models import X` surface used by bridge modules and tests.
"""

from __future__ import annotations

from importlib import import_module as _import_module
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

_modules: list[_Any] = []
_exports: dict[str, _Any] = {}
for _module_name in _MODULE_NAMES:
    _module = _import_module(f"{__name__}.{_module_name}")
    _modules.append(_module)
    for _key, _value in vars(_module).items():
        if _key not in _META_NAMES:
            _exports[_key] = _value

# Some legacy methods reference symbols that used to live later in the flat
# module. After every split module is imported, backfill each namespace with
# the complete symbol table so those runtime lookups remain compatible.
for _module in _modules:
    for _key, _value in _exports.items():
        if _key not in _META_NAMES:
            vars(_module).setdefault(_key, _value)

globals().update(_exports)
__all__ = sorted(_key for _key in _exports if not _key.startswith("_"))
