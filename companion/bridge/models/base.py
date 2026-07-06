"""Typed bridge boundary models.

These models keep the existing JSON file formats stable while making shape
validation explicit at the Python bridge edges. They are intentionally small:
the bridge is still a flat-script app, so callers can convert back to plain
dicts where the older code expects them.
"""

from __future__ import annotations

from datetime import datetime, timedelta, timezone
from enum import Enum
import hashlib
import json
from pathlib import Path
import re
from typing import Any, Callable, ClassVar, Iterable, Mapping, Sequence

from pydantic import BaseModel, ConfigDict, Field, TypeAdapter, ValidationError, field_validator


TOOL_PARAM_STRING = "string"
TOOL_PARAM_NUMBER = "number"
TOOL_PARAM_INTEGER = "integer"
TOOL_PARAM_BOOLEAN = "boolean"
TOOL_PARAM_OBJECT = "object"
TOOL_PARAM_LIST = "list"
TOOL_PARAM_TYPES = {
    TOOL_PARAM_STRING,
    TOOL_PARAM_NUMBER,
    TOOL_PARAM_INTEGER,
    TOOL_PARAM_BOOLEAN,
    TOOL_PARAM_OBJECT,
    TOOL_PARAM_LIST,
}
FACTORIO_MCP_TOOL_PREFIX = "mcp__factorioctl__"
FACTORIO_MUTATING_TOOLS = frozenset({
    "bootstrap_smelting_once",
    "build_assembler_feed",
    "build_assembler_output",
    "build_automation_science",
    "build_recipe_assembler_cell",
    "clear_area",
    "build_lab_feed",
    "craft",
    "create_zone",
    "build_fuel_supply",
    "delete_zone",
    "extract_items",
    "execute_direct_smelter",
    "execute_edge_miner",
    "execute_entity_placement_near",
    "feed_lab_from_inventory",
    "hand_feed_furnace",
    "insert_items",
    "mine_at",
    "place_entity",
    "remove_entity",
    "repair_fuel_sustainability",
    "route_belt",
    "rotate_entity",
    "set_recipe",
    "start_research",
    "update_zone",
    "walk_to",
})
FACTORIO_READ_ONLY_TOOLS = frozenset({
    "analyze_belt_gaps",
    "analyze_item_flow",
    "analyze_belt_networks",
    "analyze_belt_reach",
    "analyze_inserters",
    "build_direct_smelter",
    "build_edge_miner",
    "check_placement",
    "detect_sushi_belts",
    "diagnose_factory_blockers",
    "diagnose_fuel_sustainability",
    "diagnose_steam_power",
    "extend_power_to",
    "find_build_area",
    "find_entity_placements",
    "find_nearest_resource",
    "get_alerts",
    "get_available_research",
    "get_belt_lane_contents",
    "get_blank_slate",
    "get_character",
    "get_entities",
    "get_inventory",
    "get_machine_belt_positions",
    "get_power_coverage",
    "get_power_networks",
    "get_power_status",
    "get_protected_resources",
    "get_recipe",
    "get_recipes_by_category",
    "get_recipes_for_item",
    "get_research_status",
    "get_resources",
    "get_tick",
    "get_zone",
    "list_zones",
    "plan_steam_power",
    "plan_machine_output",
    "plan_automation_science",
    "plan_recipe_assembler_cell",
    "repair_steam_power",
    "render_map",
    "scan_resources",
    "situation_report",
    "trace_belt_sources",
    "verify_production",
})
FACTORIO_DRY_RUN_SAFE_MUTATING_TOOLS = frozenset({
    "bootstrap_smelting_once",
    "build_assembler_feed",
    "build_assembler_output",
    "build_automation_science",
    "build_recipe_assembler_cell",
    "build_fuel_supply",
    "repair_fuel_sustainability",
    "execute_edge_miner",
    "execute_entity_placement_near",
    "build_lab_feed",
    "clear_area",
    "route_belt",
})

_BRIDGE_LOG_OBJECTIVE_RE = re.compile(
    r"Continuity ledger: continue the committed objective, do not restart it:\s*([^\n]+)"
    r"|^\s*objective:\s*(.+)$",
    re.IGNORECASE | re.MULTILINE,
)
_BRIDGE_LOG_PROGRESS_RE = re.compile(r"^\s*progress:\s*(.+)$", re.IGNORECASE | re.MULTILINE)
_BRIDGE_LOG_ENTITY_COUNTS_RE = re.compile(r"player entities:\s*([^\n]+)", re.IGNORECASE)
_BRIDGE_LOG_RESEARCH_COUNT_RE = re.compile(r"research count:\s*(\d+)", re.IGNORECASE)
_BRIDGE_LOG_RESEARCHED_COUNT_JSON_RE = re.compile(
    r'\\?"researched_count\\?"\s*:\s*(\d+)',
    re.IGNORECASE,
)
_BRIDGE_LOG_RESET_UNTIL_RE = re.compile(
    r"until\s+([0-9-]+\s+[0-9:]+\s+[A-Z]+)",
    re.IGNORECASE,
)
_PROVIDER_USAGE_LIMIT_RESET_RE = re.compile(
    r"Usage limit reached.*?reset at "
    r"(?P<reset>\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2})",
    re.IGNORECASE,
)
_PROVIDER_TIMESTAMP_RE = re.compile(r"\[(?P<stamp>\d{14})[^\]]*\]")
class BridgeValidationError(ValueError):
    """Validation error with a stable field path for operator-facing messages."""

    def __init__(self, field_path: str, message: str):
        self.field_path = str(field_path or "<root>")
        self.message = str(message)
        super().__init__(f"{self.field_path}: {self.message}")


_JSON_MISSING = object()
_JSON_VALUE_ADAPTER = TypeAdapter(Any)


def _json_text(value: Any) -> str:
    return str(value if value is not None else "")


def _json_value_or_missing(value: Any) -> Any:
    try:
        return _JSON_VALUE_ADAPTER.validate_json(_json_text(value))
    except (TypeError, ValueError, ValidationError):
        return _JSON_MISSING


def _json_object_from_text(value: Any, field_path: str) -> dict[str, Any]:
    try:
        parsed = _JSON_VALUE_ADAPTER.validate_json(_json_text(value))
    except (TypeError, ValueError, ValidationError) as exc:
        raise BridgeValidationError(field_path, "expected JSON object") from exc
    if not isinstance(parsed, dict):
        raise BridgeValidationError(field_path, "expected object")
    return parsed


class BridgeModel(BaseModel):
    """Common Pydantic base for bridge-owned JSON boundary models."""

    model_config = ConfigDict(frozen=True, extra="forbid")


class BridgeTextLines(BridgeModel):
    """Typed line view for textual ingress such as logs, stderr, RCON, and JSONL."""

    raw_text: str = ""
    lines: tuple[str, ...] = ()

    @field_validator("raw_text", mode="before")
    @classmethod
    def _coerce_raw_text(cls, value: Any) -> str:
        return str(value if value is not None else "")

    @field_validator("lines", mode="before")
    @classmethod
    def _coerce_lines(cls, value: Any) -> tuple[str, ...]:
        if isinstance(value, str):
            values = [value]
        elif isinstance(value, Iterable):
            values = list(value)
        else:
            values = []
        return tuple(str(item) for item in values)

    @classmethod
    def from_text(
        cls,
        value: Any,
        *,
        strip: bool = True,
        keep_blank: bool = True,
    ) -> "BridgeTextLines":
        if isinstance(value, cls):
            return value
        raw_text = str(value if value is not None else "")
        lines: list[str] = []
        for raw_line in raw_text.splitlines():
            line = raw_line.strip() if strip else raw_line
            if not keep_blank and not line.strip():
                continue
            lines.append(line)
        return cls(raw_text=raw_text, lines=tuple(lines))

    @property
    def non_empty(self) -> tuple[str, ...]:
        return tuple(line.strip() for line in self.lines if line.strip())

    @property
    def reversed_non_empty(self) -> tuple[str, ...]:
        return tuple(reversed(self.non_empty))


class CommaSeparatedItems(BridgeModel):
    """Typed normalization for bridge fields that intentionally accept CSV text."""

    items: tuple[str, ...] = ()

    @field_validator("items", mode="before")
    @classmethod
    def _coerce_items(cls, value: Any) -> tuple[str, ...]:
        if value is None:
            values: list[Any] = []
        elif isinstance(value, str):
            values = value.split(",")
        elif isinstance(value, Iterable) and not isinstance(value, (str, bytes, dict)):
            values = list(value)
        else:
            values = [value]
        return tuple(str(item).strip() for item in values if str(item).strip())

    @classmethod
    def from_value(cls, value: Any) -> "CommaSeparatedItems":
        if isinstance(value, cls):
            return value
        return cls(items=value)

    def to_list(self, *, max_items: int | None = None) -> list[str]:
        items = list(self.items)
        if max_items is not None:
            return items[:max_items]
        return items


class TextMarkerSplit(BridgeModel):
    """Typed split of text around the first occurrence of an explicit marker."""

    before: str = ""
    marker: str = ""
    after: str = ""
    matched: bool = False

    @field_validator("before", "marker", "after", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "")

    @classmethod
    def from_text(
        cls,
        value: Any,
        marker: Any,
        *,
        strip_outer: bool = False,
    ) -> "TextMarkerSplit":
        text = str(value if value is not None else "")
        marker_text = str(marker if marker is not None else "")
        if strip_outer:
            text = text.strip()
            marker_text = marker_text.strip()
        if not marker_text or marker_text not in text:
            return cls(before=text, marker=marker_text)
        before, after = text.split(marker_text, 1)
        return cls(before=before, marker=marker_text, after=after, matched=True)


class KeyValueTextSplit(BridgeModel):
    """Typed split for one-line `key: value` protocol text."""

    text: str = ""
    key: str = ""
    value: str = ""
    matched: bool = False

    @field_validator("text", "key", "value", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "")

    @classmethod
    def from_text(
        cls,
        value: Any,
        *,
        separator: str = ":",
        strip: bool = True,
        lower_key: bool = True,
    ) -> "KeyValueTextSplit":
        text = str(value if value is not None else "")
        if strip:
            text = text.strip()
        if not separator or separator not in text:
            return cls(text=text)
        key, raw_value = text.split(separator, 1)
        key = key.strip()
        if lower_key:
            key = key.lower()
        return cls(
            text=text,
            key=key,
            value=raw_value.strip() if strip else raw_value,
            matched=True,
        )


class MutableBridgeModel(BaseModel):
    """Mutable Pydantic base for report/state accumulators."""

    model_config = ConfigDict(frozen=False, extra="forbid")


class RemotePayloadModel(BaseModel):
    """Pydantic base for remote Factorio payloads with forward-compatible extras."""

    model_config = ConfigDict(frozen=True, extra="allow")


class ProgressSignal(str, Enum):
    NONE = "none"
    PLAN_DONE = "plan_done"
    PLAN_READY = "plan_ready"
    NEW_OBJECTIVE = "new_objective"


class LedgerStatus(str, Enum):
    NONE = "none"
    READY = "ready"
    EXECUTING = "executing"
    DONE = "done"
    BLOCKED = "blocked"


class LedgerNextRequiredMode(str, Enum):
    NONE = "none"
    PLAN = "plan"
    EXECUTE = "execute"
    WAIT = "wait"


class JournalFailureKind(str, Enum):
    NONE = "none"
    PROVIDER_LIMIT = "provider_limit"
    TURN_LIMIT = "turn_limit"
    TIMEOUT = "timeout"
    CONTEXT_WINDOW = "context_window"
    EXPECTED_MISS = "expected_miss"
    INVALID_REQUEST = "invalid_request"
    INFRASTRUCTURE_FAILURE = "infrastructure_failure"
    ENGINE_TRANSIENT = "engine_transient"
    RESEARCH_BUSY = "research_busy"


class GameRejectionEvidenceKind(str, Enum):
    EMPTY = "empty"
    RESEARCH_STATUS = "research_status"
    INVALID_REQUEST = "invalid_request"
    GAMEPLAY_FAILURE = "gameplay_failure"


class ObjectiveCompletionKind(str, Enum):
    NONE = "none"
    STEAM_POWER = "steam_power"
    POWERED_LAB = "powered_lab"
    AUTOMATION_RESEARCH = "automation_research"


class LedgerStalenessKind(str, Enum):
    NONE = "none"
    STALE_BOOTSTRAP = "stale_bootstrap"


class BridgeLogProgressKind(str, Enum):
    NONE = "none"
    LEDGER_PROGRESS = "ledger_progress"
    PLAN_WAITING = "plan_waiting"
    RESEARCH_COMPLETED = "research_completed"
    VERIFIED_WORKING = "verified_working"
    AUTOMATION_MILESTONE = "automation_milestone"
    POWER_GRID = "power_grid"


class BridgeLogPowerKind(str, Enum):
    NONE = "none"
    DIAGNOSTIC_PAYLOAD = "diagnostic_payload"
    CONCISE_STATUS_TEXT = "concise_status_text"
    POWER_RELATED_TEXT = "power_related_text"


class BridgeLogRuntimeKind(str, Enum):
    SDK_SPAWN = "sdk_spawn"
    SDK_DONE = "sdk_done"
    PROVIDER_PAUSE = "provider_pause"
    CONTEXT_RESET = "context_reset"
    WATCHDOG_ABORT = "watchdog_abort"
    RESEARCH_COMPLETED = "research_completed"


class SdkErrorKind(str, Enum):
    CONTEXT_WINDOW = "context_window"
    TERMINAL_RESULT_ECHO = "terminal_result_echo"


class SdkStderrKind(str, Enum):
    EMPTY = "empty"
    BENIGN_CONNECTOR_NOISE = "benign_connector_noise"
    UNKNOWN = "unknown"


class AnomalyEvidenceKind(str, Enum):
    EMPTY = "empty"
    NOMINAL = "nominal"
    MEANINGFUL = "meaningful"


class ReflectionDropKind(str, Enum):
    NONE = "none"
    TRANSIENT_FAILURE = "transient_failure"
    LOW_VALUE_STARTUP = "low_value_startup"


class AutonomyMode(str, Enum):
    PLAN = "plan"
    EXECUTE = "execute"


class AutonomyDecisionReason(str, Enum):
    MISSING_PLAN = "missing_plan"
    PLANNER_INTERVAL = "planner_interval"
    WITHIN_INTERVAL = "within_interval"
    ACTIONABLE_PLAN = "actionable_plan"
    REPEATED_PLAN_PROGRESS = "repeated_plan_progress"
    PLAN_DONE = "plan_done"
    LIVE_STATE_COMPLETION = "live_state_completion"
    REFLECTION_DUE = "reflection_due"
    STALE_MANUAL_AUTOMATION = "stale_manual_automation"


def progress_signal(value: Any) -> ProgressSignal:
    if isinstance(value, ProgressSignal):
        return value
    if isinstance(value, str):
        normalized = value.strip().lower().replace("-", "_")
        for signal in ProgressSignal:
            if normalized == signal.value:
                return signal
    return ProgressSignal.NONE


def ledger_status(value: Any) -> LedgerStatus:
    if isinstance(value, LedgerStatus):
        return value
    if isinstance(value, str):
        normalized = value.strip().lower().replace("-", "_")
        for status in LedgerStatus:
            if normalized == status.value:
                return status
    return LedgerStatus.NONE


def ledger_next_required_mode(value: Any) -> LedgerNextRequiredMode:
    if isinstance(value, LedgerNextRequiredMode):
        return value
    if isinstance(value, str):
        normalized = value.strip().lower().replace("-", "_")
        for mode in LedgerNextRequiredMode:
            if normalized == mode.value:
                return mode
    return LedgerNextRequiredMode.NONE


def autonomy_mode(value: Any) -> AutonomyMode:
    if isinstance(value, AutonomyMode):
        return value
    if isinstance(value, str):
        normalized = value.strip().lower().replace("-", "_")
        for mode in AutonomyMode:
            if normalized == mode.value:
                return mode
    return AutonomyMode.PLAN


def autonomy_decision_reason(value: Any) -> AutonomyDecisionReason:
    if isinstance(value, AutonomyDecisionReason):
        return value
    if isinstance(value, str):
        normalized = value.strip().lower().replace("-", "_")
        for reason in AutonomyDecisionReason:
            if normalized == reason.value:
                return reason
    return AutonomyDecisionReason.MISSING_PLAN


class AutonomyDecision(BridgeModel):
    """Typed plan/execute decision for autonomy ticks."""

    mode: AutonomyMode = AutonomyMode.PLAN
    reason: AutonomyDecisionReason = AutonomyDecisionReason.MISSING_PLAN
    actionable_plan: bool = False

    @field_validator("mode", mode="before")
    @classmethod
    def _coerce_mode(cls, value: Any) -> AutonomyMode:
        return autonomy_mode(value)

    @field_validator("reason", mode="before")
    @classmethod
    def _coerce_reason(cls, value: Any) -> AutonomyDecisionReason:
        return autonomy_decision_reason(value)

    @property
    def is_plan(self) -> bool:
        return self.mode == AutonomyMode.PLAN

    @property
    def is_execute(self) -> bool:
        return self.mode == AutonomyMode.EXECUTE

    @property
    def mode_value(self) -> str:
        return self.mode.value

    @property
    def reason_value(self) -> str:
        return self.reason.value

    @property
    def read_only_tools(self) -> bool:
        return self.is_plan

    def next_exec_ticks_since_plan(self, current: Any) -> int:
        if self.is_plan:
            return 0
        try:
            value = int(current)
        except (TypeError, ValueError):
            value = 0
        return max(0, value) + 1


def _single_line_text(value: Any, *, limit: int = 240) -> str:
    return re.sub(r"\s+", " ", str(value or "")).strip()[:limit]


class ToolResultClassification(str, Enum):
    OK = "ok"
    EXPECTED_MISS = "expected_miss"
    INVALID_REQUEST = "invalid_request"
    GAME_REJECTED = "game_rejected"
    SDK_FAILURE = "sdk_failure"
    INFRASTRUCTURE_FAILURE = "infrastructure_failure"


class ToolResultLogLevel(str, Enum):
    DEBUG = "debug"
    INFO = "info"
    WARNING = "warning"


class ToolResultTextKind(str, Enum):
    NONE = "none"
    CUSTOM = "custom"
    OPERATOR_ONLY = "operator_only"
    EXPECTED_MISS = "expected_miss"
    INVALID_REQUEST = "invalid_request"
    INFRASTRUCTURE_FAILURE = "infrastructure_failure"
    GAME_REJECTED = "game_rejected"
    SDK_FAILURE = "sdk_failure"


BRIDGE_PARALLEL_MUTATION_GUARD_PREFIX = (
    "Factorioctl bridge blocked parallel mutating tool call:"
)
BRIDGE_READ_ONLY_TURN_GUARD_PREFIX = (
    "Factorioctl bridge blocked non-read-only tool during planner/reflection turn:"
)
BRIDGE_SKILL_REQUIRED_GUARD_PREFIX = (
    "Factorioctl bridge blocked Factorio tool before control skill:"
)
BRIDGE_PARAM_SCHEMA_GUARD_PREFIX = (
    "Factorioctl bridge blocked invalid Factorio tool parameters:"
)
BRIDGE_MANUAL_AUTOMATION_GUARD_PREFIX = (
    "Factorioctl bridge blocked stale manual automation tool:"
)

_LIVE_ENTITY_RE = re.compile(r"\b([a-z0-9][a-z0-9-]*)=(\d+)\b", re.IGNORECASE)
_LIVE_ENTITY_ORDER = (
    "burner-mining-drill",
    "electric-mining-drill",
    "stone-furnace",
    "assembling-machine-1",
    "transport-belt",
    "burner-inserter",
    "inserter",
    "small-electric-pole",
    "medium-electric-pole",
    "offshore-pump",
    "boiler",
    "steam-engine",
    "pipe",
    "lab",
)
_LIVE_STATE_LINE_RE = re.compile(
    r"\bLive state:\s*(?P<surface>[^@\n;]+?)\s*@\s*"
    r"(?P<x>-?\d+(?:\.\d+)?),(?P<y>-?\d+(?:\.\d+)?)",
    re.IGNORECASE,
)
_STEAM_BUILD_INTENT_RE = re.compile(
    r"\b(?:build|deploy|place|set up|setup|construct|create|craft|complete)\b"
    r".{0,80}\b(?:steam power|steam-power|offshore pump|offshore-pump|boiler|"
    r"steam engine|steam-engine)\b"
    r"|\b(?:steam power|steam-power)\b.{0,80}\b"
    r"(?:build|deployment|setup|set up|complete)\b",
    re.IGNORECASE | re.DOTALL,
)
_AGENT_RESPONSE_SECTION_RE = re.compile(
    r"\[color=([0-9.,]+)\]([A-Z][A-Z _]*?):\[/color\]\s*",
)
_TOOL_INVALID_REQUEST_RE = re.compile(
    r"invalid type|invalid json|failed to deserialize|expected .*sequence|"
    r"missing required|missing field|value for required field\b.{0,80}\bmissing|"
    r"unknown field|bad request|packet too large",
    re.IGNORECASE,
)
_TOOL_INFRASTRUCTURE_FAILURE_RE = re.compile(
    r"expected value at line \d+ column \d+|exceeds maximum allowed tokens|"
    r"rcon|connection|timed out|timeout|unavailable|sync_or_restart_mod|"
    r"mod does not expose|claude-interface mod",
    re.IGNORECASE,
)
_TOOL_GAME_REJECTED_RE = re.compile(
    r"cannot\b.{0,80}\b(?:place|build|craft|insert|mine|find|reach|connect|route|move|walk|teleport)|"
    r"could not\b.{0,80}\b(?:place|build|craft|insert|mine|find|reach|connect|route|move|walk|teleport)|"
    r"not in inventory|no power|not found|no labs found|no .*resource entity found|"
    r"failed|insufficient\b.{0,40}\b(?:items|resources|inventory|materials)|"
    r"placement\b.{0,40}\b(?:failed|blocked|invalid)|"
    r"entity\b.{0,40}\b(?:not found|invalid|missing)|"
    r"route failed|factorio cannot place|\bblocked\b",
    re.IGNORECASE,
)
_TOOL_BENIGN_MISSES = frozenset({
    "error: no items of that type in inventory",
    "no items of that type in inventory",
    "error: entity has no such inventory",
    "entity has no such inventory",
    "error: no electric poles found in area",
    "no electric poles found in area",
    "error: no minable entity at position",
    "no minable entity at position",
})
_TOOL_OPERATOR_ONLY_PREFIXES = (
    "Error: execute_lua is disabled.",
    BRIDGE_PARALLEL_MUTATION_GUARD_PREFIX,
    BRIDGE_READ_ONLY_TURN_GUARD_PREFIX,
    BRIDGE_SKILL_REQUIRED_GUARD_PREFIX,
    BRIDGE_PARAM_SCHEMA_GUARD_PREFIX,
    BRIDGE_MANUAL_AUTOMATION_GUARD_PREFIX,
)
