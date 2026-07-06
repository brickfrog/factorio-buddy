from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Iterable

_METADATA_FILE = (
    Path(__file__).resolve().parent
    / "generated"
    / "factorio_tool_metadata.json"
)


@dataclass(frozen=True)
class FactorioToolMetadata:
    name: str
    mutating: bool = False
    read_only: bool = False
    dry_run_safe: bool = False

    @classmethod
    def from_mapping(cls, value: Any) -> "FactorioToolMetadata":
        if not isinstance(value, dict):
            raise ValueError("tool metadata entry must be an object")
        name = str(value.get("name") or "").strip()
        if not name:
            raise ValueError("tool metadata entry missing name")
        mutating = bool(value.get("mutating", False))
        read_only = bool(value.get("read_only", False))
        if mutating and read_only:
            raise ValueError(f"tool metadata entry {name!r} cannot be both mutating and read_only")
        return cls(
            name=name,
            mutating=mutating,
            read_only=read_only,
            dry_run_safe=bool(value.get("dry_run_safe", False)),
        )


@dataclass(frozen=True)
class FactorioToolMetadataRegistry:
    tools: dict[str, FactorioToolMetadata]

    @classmethod
    def from_mapping(cls, value: Any) -> "FactorioToolMetadataRegistry":
        if not isinstance(value, dict):
            raise ValueError("tool metadata must be an object")
        if value.get("schema_version") != 1:
            raise ValueError("unsupported tool metadata schema_version")
        raw_tools = value.get("tools")
        if not isinstance(raw_tools, list):
            raise ValueError("tool metadata tools must be a list")
        tools: dict[str, FactorioToolMetadata] = {}
        for raw_tool in raw_tools:
            tool = FactorioToolMetadata.from_mapping(raw_tool)
            if tool.name in tools:
                raise ValueError(f"duplicate tool metadata entry {tool.name!r}")
            tools[tool.name] = tool
        return cls(tools=tools)

    @classmethod
    def from_json_text(cls, value: str) -> "FactorioToolMetadataRegistry":
        return cls.from_mapping(json.loads(value))

    @classmethod
    def from_file(cls, path: Path = _METADATA_FILE) -> "FactorioToolMetadataRegistry":
        return cls.from_json_text(path.read_text())

    @property
    def mutating_tools(self) -> frozenset[str]:
        return frozenset(tool.name for tool in self.tools.values() if tool.mutating)

    @property
    def read_only_tools(self) -> frozenset[str]:
        return frozenset(tool.name for tool in self.tools.values() if tool.read_only)

    @property
    def dry_run_safe_mutating_tools(self) -> frozenset[str]:
        return frozenset(
            tool.name
            for tool in self.tools.values()
            if tool.mutating and tool.dry_run_safe
        )

    @property
    def names(self) -> frozenset[str]:
        return frozenset(self.tools)

    def missing(self, names: Iterable[str]) -> frozenset[str]:
        return frozenset(str(name) for name in names if str(name) not in self.tools)


FACTORIO_TOOL_METADATA = FactorioToolMetadataRegistry.from_file()
FACTORIO_MUTATING_TOOLS = FACTORIO_TOOL_METADATA.mutating_tools
FACTORIO_READ_ONLY_TOOLS = FACTORIO_TOOL_METADATA.read_only_tools
FACTORIO_DRY_RUN_SAFE_MUTATING_TOOLS = FACTORIO_TOOL_METADATA.dry_run_safe_mutating_tools
