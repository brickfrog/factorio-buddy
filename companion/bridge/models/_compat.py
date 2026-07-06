"""Internal helpers for the split bridge model package."""

from __future__ import annotations

from importlib import import_module
from typing import Any


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


def import_namespace(target: dict[str, Any], *module_names: str) -> None:
    """Import prior split modules into a generated module namespace."""
    package = __package__ or "models"
    for module_name in module_names:
        module = import_module(f"{package}.{module_name}")
        for key, value in vars(module).items():
            if key not in _META_NAMES:
                target.setdefault(key, value)
