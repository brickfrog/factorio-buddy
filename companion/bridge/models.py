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


class LiveState(BridgeModel):
    raw: str = ""
    found: bool = False
    surface: str = ""
    x: float | None = None
    y: float | None = None
    entity_counts: dict[str, int] = Field(default_factory=dict)

    @field_validator("raw", "surface", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "").strip()

    @field_validator("found", mode="before")
    @classmethod
    def _coerce_found(cls, value: Any) -> bool:
        return bool(value)

    @field_validator("x", "y", mode="before")
    @classmethod
    def _coerce_coordinate(cls, value: Any) -> float | None:
        if value is None or value == "":
            return None
        try:
            return float(value)
        except (TypeError, ValueError) as exc:
            raise ValueError("expected numeric coordinate") from exc

    @field_validator("entity_counts", mode="before")
    @classmethod
    def _coerce_entity_counts(cls, value: Any) -> dict[str, int]:
        if not isinstance(value, dict):
            return {}
        counts: dict[str, int] = {}
        for key, raw_count in value.items():
            try:
                count = int(raw_count)
            except (TypeError, ValueError):
                continue
            if count > 0:
                normalized = str(key).strip().lower()
                counts[normalized] = counts.get(normalized, 0) + count
        return counts

    @classmethod
    def from_line(cls, value: Any) -> "LiveState":
        raw = value if isinstance(value, str) else ""
        counts: dict[str, int] = {}
        for name, raw_count in _LIVE_ENTITY_RE.findall(raw):
            try:
                count = int(raw_count)
            except ValueError:
                continue
            key = name.lower()
            counts[key] = counts.get(key, 0) + count
        match = _LIVE_STATE_LINE_RE.search(raw)
        surface = match.group("surface").strip() if match else ""
        x = match.group("x") if match else None
        y = match.group("y") if match else None
        return cls(
            raw=raw,
            found=bool(raw),
            surface=surface,
            x=x,
            y=y,
            entity_counts=counts,
        )

    @classmethod
    def from_payload(cls, value: Any) -> "LiveState":
        if isinstance(value, cls):
            return value
        if isinstance(value, str):
            return cls.from_line(value)
        if not isinstance(value, dict):
            raise BridgeValidationError("live_state", "expected object")
        try:
            return cls.model_validate(value)
        except ValidationError as exc:
            raise BridgeValidationError(
                "live_state",
                f"unexpected live-state payload: {value!r}",
            ) from exc

    @classmethod
    def from_rcon_response(cls, value: Any) -> "LiveState":
        return cls.from_payload(RconJsonResponse.parse_value(value))

    def has(self, name: str) -> bool:
        return self.entity_counts.get(str(name).lower(), 0) > 0

    def has_all(self, names: Iterable[str]) -> bool:
        return all(self.has(name) for name in names)

    def has_any(self, names: Iterable[str]) -> bool:
        return any(self.has(name) for name in names)

    def has_automation_capable_footprint(self) -> bool:
        """Return true once the factory can build durable logistics.

        A single hand-fed furnace is still bootstrap. Belts, inserters, power,
        labs, assemblers, or mining drills mean the agent should stop treating
        repeated manual transfers as a valid objective and build routes instead.
        """
        return self.has_any((
            "transport-belt",
            "underground-belt",
            "splitter",
            "inserter",
            "burner-inserter",
            "small-electric-pole",
            "medium-electric-pole",
            "big-electric-pole",
            "substation",
            "offshore-pump",
            "boiler",
            "steam-engine",
            "burner-mining-drill",
            "electric-mining-drill",
            "assembling-machine-1",
            "assembling-machine-2",
            "assembling-machine-3",
            "lab",
        ))

    @property
    def entity_summary(self) -> str:
        ordered = [
            name
            for name in _LIVE_ENTITY_ORDER
            if self.entity_counts.get(name, 0) > 0
        ]
        extras = sorted(name for name in self.entity_counts if name not in _LIVE_ENTITY_ORDER)
        return ", ".join(
            f"{name}={self.entity_counts[name]}"
            for name in ordered + extras
        )

    def to_line(self) -> str:
        if (
            self.raw
            and self.raw.lower().startswith("live state:")
            and not (self.surface and self.x is not None and self.y is not None)
        ):
            return self.raw
        if not self.found or not self.surface or self.x is None or self.y is None:
            return ""
        summary = self.entity_summary
        suffix = f"; player entities: {summary}" if summary else ""
        return f"Live state: {self.surface} @ {self.x:.1f},{self.y:.1f}{suffix}"


class LedgerObjectiveIntent(BridgeModel):
    """Typed early-game objective intent derived from objective and plan only."""

    mentions_initial_extraction: bool = False
    mentions_steam_power: bool = False
    mentions_powered_lab: bool = False
    steam_build_intent: bool = False
    mentions_automation_research: bool = False

    @classmethod
    def from_text(cls, value: Any) -> "LedgerObjectiveIntent":
        text = str(value or "").lower()
        return cls(
            mentions_initial_extraction=any(
                phrase in text
                for phrase in (
                    "establish initial extraction infrastructure",
                    "initial extraction infrastructure",
                    "foundational resource extraction",
                )
            ),
            mentions_steam_power=any(
                phrase in text
                for phrase in (
                    "steam power",
                    "steam-power",
                    "steam engine",
                    "steam-engine",
                    "boiler",
                    "offshore pump",
                    "offshore-pump",
                )
            ),
            mentions_powered_lab=any(
                phrase in text
                for phrase in (
                    "power the lab",
                    "powered lab",
                    "lab near power endpoint",
                )
            ),
            steam_build_intent=_STEAM_BUILD_INTENT_RE.search(text) is not None,
            mentions_automation_research=any(
                phrase in text
                for phrase in (
                    "automation research",
                    "start automation",
                    "automation-science-pack",
                )
            ),
        )


class LedgerProgressSignals(BridgeModel):
    """Typed signals extracted from ledger progress notes."""

    reports_no_infrastructure: bool = False

    @classmethod
    def from_text(cls, value: Any) -> "LedgerProgressSignals":
        text = str(value or "").lower()
        return cls(
            reports_no_infrastructure=any(
                phrase in text
                for phrase in (
                    "no infrastructure yet deployed",
                    "infrastructure is nonexistent",
                    "zero-state deployment confirmed",
                )
            ),
        )


class LedgerReadinessEvidence(BridgeModel):
    """Typed evidence that a persisted ledger plan is awaiting execution."""

    has_plan: bool = False
    explicit_signal: ProgressSignal = ProgressSignal.NONE
    status: LedgerStatus = LedgerStatus.NONE
    next_required_mode: LedgerNextRequiredMode = LedgerNextRequiredMode.NONE
    ready_note_count: int = 0
    min_ready_notes: int = 3

    @field_validator("explicit_signal", mode="before")
    @classmethod
    def _coerce_signal(cls, value: Any) -> ProgressSignal:
        return progress_signal(value)

    @field_validator("status", mode="before")
    @classmethod
    def _coerce_status(cls, value: Any) -> LedgerStatus:
        return ledger_status(value)

    @field_validator("next_required_mode", mode="before")
    @classmethod
    def _coerce_next_required_mode(cls, value: Any) -> LedgerNextRequiredMode:
        return ledger_next_required_mode(value)

    @property
    def explicit_ready(self) -> bool:
        return self.explicit_signal in {
            ProgressSignal.NEW_OBJECTIVE,
            ProgressSignal.PLAN_READY,
        } or self.status in {
            LedgerStatus.READY,
            LedgerStatus.EXECUTING,
        } or self.next_required_mode == LedgerNextRequiredMode.EXECUTE

    @property
    def repeated_ready(self) -> bool:
        return self.ready_note_count >= max(1, self.min_ready_notes)

    @property
    def is_ready(self) -> bool:
        return self.has_plan and (self.explicit_ready or self.repeated_ready)

    @staticmethod
    def note_indicates_ready(value: Any) -> bool:
        text = str(value or "").lower()
        return any(
            phrase in text
            for phrase in (
                "ready for execution",
                "ready for execution turn",
                "awaiting execution",
                "queued for execution",
                "pending mutation tick",
                "execution pending",
            )
        )

    @classmethod
    def from_ledger(
        cls,
        ledger: "LedgerState",
        *,
        min_ready_notes: int = 3,
    ) -> "LedgerReadinessEvidence":
        has_plan = bool(ledger.objective.strip()) and bool(ledger.plan_steps)
        ready_note_count = sum(
            1 for note in ledger.progress_notes if cls.note_indicates_ready(note)
        )
        return cls(
            has_plan=has_plan,
            explicit_signal=ledger.signal,
            status=ledger.status,
            next_required_mode=ledger.next_required_mode,
            ready_note_count=ready_note_count,
            min_ready_notes=min_ready_notes,
        )


class LiveCompletionEvidence(BridgeModel):
    """Typed evidence that live Factorio state has completed a ledger objective."""

    kind: ObjectiveCompletionKind = ObjectiveCompletionKind.NONE
    reason: str = ""
    entity_counts: dict[str, int] = Field(default_factory=dict)

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> ObjectiveCompletionKind:
        if isinstance(value, ObjectiveCompletionKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in ObjectiveCompletionKind:
                if normalized == kind.value:
                    return kind
        return ObjectiveCompletionKind.NONE

    @field_validator("reason", mode="before")
    @classmethod
    def _coerce_reason(cls, value: Any) -> str:
        return str(value or "").strip()

    @field_validator("entity_counts", mode="before")
    @classmethod
    def _coerce_entity_counts(cls, value: Any) -> dict[str, int]:
        if not isinstance(value, dict):
            return {}
        counts: dict[str, int] = {}
        for key, raw_count in value.items():
            try:
                count = int(raw_count)
            except (TypeError, ValueError):
                continue
            counts[str(key).lower()] = count
        return counts

    @property
    def is_completion(self) -> bool:
        return self.kind != ObjectiveCompletionKind.NONE and bool(self.reason)

    @classmethod
    def none(cls, *, live_state: LiveState | None = None) -> "LiveCompletionEvidence":
        return cls(
            kind=ObjectiveCompletionKind.NONE,
            reason="",
            entity_counts=dict(live_state.entity_counts) if live_state else {},
        )

    @classmethod
    def from_ledger_and_live_state(
        cls,
        ledger: "LedgerState",
        live_state: Any,
    ) -> "LiveCompletionEvidence":
        live = live_state if isinstance(live_state, LiveState) else LiveState.from_line(live_state)
        active_text = ledger.active_text()
        if not active_text or not live.entity_counts:
            return cls.none(live_state=live)

        intent = LedgerObjectiveIntent.from_text(active_text)
        has_steam_chain = live.has_all(("offshore-pump", "boiler", "steam-engine"))
        has_lab_power_evidence = live.has("lab") and (
            has_steam_chain or live.has("small-electric-pole")
        )

        progress_text = ledger.progress_text()
        progress_says_automation_done = any(
            phrase in progress_text
            for phrase in (
                "automation research completed",
                "automation+electric-mining-drill research",
            )
        )

        if (
            intent.steam_build_intent
            and intent.mentions_steam_power
            and intent.mentions_powered_lab
            and has_steam_chain
            and has_lab_power_evidence
        ):
            return cls(
                kind=ObjectiveCompletionKind.POWERED_LAB,
                reason="live state already has steam power and a powered-lab footprint",
                entity_counts=live.entity_counts,
            )
        if intent.steam_build_intent and intent.mentions_steam_power and has_steam_chain:
            return cls(
                kind=ObjectiveCompletionKind.STEAM_POWER,
                reason="live state already has offshore-pump, boiler, and steam-engine",
                entity_counts=live.entity_counts,
            )
        if intent.mentions_powered_lab and has_lab_power_evidence:
            return cls(
                kind=ObjectiveCompletionKind.POWERED_LAB,
                reason="live state already has lab plus power-grid evidence",
                entity_counts=live.entity_counts,
            )
        if intent.mentions_automation_research and progress_says_automation_done and live.has("lab"):
            return cls(
                kind=ObjectiveCompletionKind.AUTOMATION_RESEARCH,
                reason="ledger progress says automation research completed and live state has a lab",
                entity_counts=live.entity_counts,
            )
        return cls.none(live_state=live)


class ProviderUsageLimit(BridgeModel):
    """Typed provider 429 usage-limit reset parsed from SDK error text."""

    raw_text: str = ""
    reset_at: datetime

    @classmethod
    def from_text(
        cls,
        value: Any,
        *,
        now: datetime | None = None,
        default_utc_offset: str | None = None,
    ) -> "ProviderUsageLimit | None":
        text = str(value or "")
        match = _PROVIDER_USAGE_LIMIT_RESET_RE.search(text)
        if not match:
            return None
        try:
            reset_naive = datetime.strptime(
                match.group("reset"),
                "%Y-%m-%d %H:%M:%S",
            )
        except ValueError:
            return None
        provider_tz = (
            cls._infer_provider_timezone(text, now)
            or cls.parse_utc_offset(default_utc_offset)
        )
        return cls(raw_text=text, reset_at=reset_naive.replace(tzinfo=provider_tz))

    @staticmethod
    def parse_utc_offset(value: Any) -> timezone:
        raw = str(value or "+08:00").strip()
        match = re.fullmatch(r"([+-])(\d{1,2})(?::?(\d{2}))?", raw)
        if not match:
            return timezone(timedelta(hours=8))
        sign, hours_raw, minutes_raw = match.groups()
        hours = int(hours_raw)
        minutes = int(minutes_raw or "0")
        if hours > 23 or minutes > 59:
            return timezone(timedelta(hours=8))
        delta = timedelta(hours=hours, minutes=minutes)
        if sign == "-":
            delta = -delta
        return timezone(delta)

    @staticmethod
    def _infer_provider_timezone(
        text: Any,
        now: datetime | None = None,
    ) -> timezone | None:
        if now is None:
            now = datetime.now(timezone.utc)
        if now.tzinfo is None:
            now = now.astimezone()
        now_utc_naive = now.astimezone(timezone.utc).replace(tzinfo=None)
        for stamp in reversed(_PROVIDER_TIMESTAMP_RE.findall(str(text or ""))):
            try:
                provider_naive = datetime.strptime(stamp, "%Y%m%d%H%M%S")
            except ValueError:
                continue
            delta_minutes = round(
                (provider_naive - now_utc_naive).total_seconds() / 60,
            )
            rounded_minutes = int(round(delta_minutes / 15) * 15)
            if -12 * 60 <= rounded_minutes <= 14 * 60:
                return timezone(timedelta(minutes=rounded_minutes))
        return None


class ProviderUsageLimitSettings(BridgeModel):
    """Typed provider usage-limit parsing settings resolved from environment."""

    usage_limit_reset_utc_offset: str | None = None

    ENV_FIELDS: ClassVar[tuple["BridgeRuntimeEnvField", ...]] = ()

    @field_validator("usage_limit_reset_utc_offset", mode="before")
    @classmethod
    def _coerce_reset_offset(cls, value: Any) -> str | None:
        text = str(value).strip() if value is not None else ""
        return text or None

    @classmethod
    def from_env(cls, env: Any) -> "ProviderUsageLimitSettings":
        if isinstance(env, cls):
            return env
        return cls(**BridgeRuntimeEnvField.read_source(env, cls.env_fields()))

    @classmethod
    def env_fields(cls) -> tuple["BridgeRuntimeEnvField", ...]:
        # Initialized after BridgeRuntimeEnvField is defined.
        return BridgeRuntimeEnvField.validate_unique(
            cls.ENV_FIELDS,
            field_path="provider_usage_limit_env_fields",
        )


class SdkStderrSignal(BridgeModel):
    """Typed SDK stderr signal for deciding whether stderr should be a warning."""

    _BENIGN_MARKERS: ClassVar[tuple[str, ...]] = (
        "claude.ai connectors are disabled",
        "ANTHROPIC_API_KEY or another auth source is set",
    )

    raw_text: str = ""
    lines: tuple[str, ...] = ()
    kind: SdkStderrKind = SdkStderrKind.UNKNOWN
    reasons: tuple[str, ...] = ()
    benign: bool = False

    @field_validator("raw_text", mode="before")
    @classmethod
    def _coerce_raw_text(cls, value: Any) -> str:
        return str(value if value is not None else "")

    @field_validator("lines", "reasons", mode="before")
    @classmethod
    def _coerce_text_tuple(cls, value: Any) -> tuple[str, ...]:
        if isinstance(value, str):
            values = [value]
        elif isinstance(value, Iterable):
            values = list(value)
        else:
            values = []
        return tuple(str(item) for item in values if str(item))

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> SdkStderrKind:
        if isinstance(value, SdkStderrKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in SdkStderrKind:
                if normalized == kind.value:
                    return kind
        return SdkStderrKind.UNKNOWN

    @classmethod
    def from_text(cls, value: Any) -> "SdkStderrSignal":
        raw_text = str(value if value is not None else "")
        lines = BridgeTextLines.from_text(raw_text, keep_blank=False).lines
        if not lines:
            return cls(
                raw_text=raw_text,
                lines=(),
                kind=SdkStderrKind.EMPTY,
                reasons=("empty stderr",),
                benign=True,
            )
        if all(any(marker in line for marker in cls._BENIGN_MARKERS) for line in lines):
            return cls(
                raw_text=raw_text,
                lines=lines,
                kind=SdkStderrKind.BENIGN_CONNECTOR_NOISE,
                reasons=("known SDK connector/auth stderr noise",),
                benign=True,
            )
        return cls(
            raw_text=raw_text,
            lines=lines,
            kind=SdkStderrKind.UNKNOWN,
            reasons=("unrecognized SDK stderr",),
            benign=False,
        )

    @classmethod
    def is_benign(cls, value: Any) -> bool:
        return cls.from_text(value).benign


class SdkErrorSignal(BridgeModel):
    """Typed SDK error text signals that affect bridge retry/session policy."""

    raw_text: str = ""
    kinds: frozenset[SdkErrorKind] = Field(default_factory=frozenset)
    reasons: tuple[str, ...] = ()
    context_window_limit: bool = False
    terminal_result_echo: bool = False

    @field_validator("kinds", mode="before")
    @classmethod
    def _coerce_kinds(cls, value: Any) -> frozenset[SdkErrorKind]:
        if isinstance(value, SdkErrorKind):
            return frozenset({value})
        if isinstance(value, str):
            values = [value]
        elif isinstance(value, Iterable):
            values = list(value)
        else:
            values = []
        result: set[SdkErrorKind] = set()
        for item in values:
            if isinstance(item, SdkErrorKind):
                result.add(item)
                continue
            if isinstance(item, str):
                normalized = item.strip().lower().replace("-", "_")
                for kind in SdkErrorKind:
                    if normalized == kind.value:
                        result.add(kind)
                        break
        return frozenset(result)

    @field_validator("reasons", mode="before")
    @classmethod
    def _coerce_reasons(cls, value: Any) -> tuple[str, ...]:
        if isinstance(value, str):
            values = [value]
        elif isinstance(value, Iterable):
            values = list(value)
        else:
            values = []
        return tuple(str(item) for item in values if isinstance(item, str))

    @classmethod
    def from_text(cls, value: Any) -> "SdkErrorSignal":
        text = str(value if value is not None else "")
        normalized = re.sub(r"[-_]+", " ", text.lower())
        kinds: set[SdkErrorKind] = set()
        reasons: list[str] = []
        if (
            "context window limit" in normalized
            or "context length" in normalized
            or "maximum context" in normalized
        ):
            kinds.add(SdkErrorKind.CONTEXT_WINDOW)
            reasons.append("context window limit")
        if "claude code returned an error result:" in normalized:
            kinds.add(SdkErrorKind.TERMINAL_RESULT_ECHO)
            reasons.append("sdk terminal result echo")
        return cls(
            raw_text=text,
            kinds=frozenset(kinds),
            reasons=tuple(reasons),
            context_window_limit=SdkErrorKind.CONTEXT_WINDOW in kinds,
            terminal_result_echo=SdkErrorKind.TERMINAL_RESULT_ECHO in kinds,
        )

    def has(self, kind: SdkErrorKind) -> bool:
        return kind in self.kinds

    @classmethod
    def is_context_window_limit(cls, value: Any) -> bool:
        return cls.from_text(value).has(SdkErrorKind.CONTEXT_WINDOW)

    @classmethod
    def is_terminal_result_echo(cls, value: Any) -> bool:
        return cls.from_text(value).has(SdkErrorKind.TERMINAL_RESULT_ECHO)


class ToolResultPayload(BaseModel):
    """Typed view of common Factorio MCP JSON result shapes."""

    model_config = ConfigDict(frozen=True, extra="allow")

    type: str | None = None
    text: str | None = None
    success: bool | None = None
    expected_miss: bool | None = None
    mined_count: int | None = None
    error: Any = None
    message: Any = None
    reason: Any = None
    action_needed: Any = None
    status: Any = None
    state: Any = None
    result: Any = None
    can_place: bool | None = None
    allowed: bool | None = None
    policy_allowed: bool | None = None
    factorio_allowed: bool | None = None
    entity: str | None = None
    position: dict[str, Any] | None = None


class ToolResultPayloadCollection(BridgeModel):
    """Typed view of SDK/MCP payload collections.

    Tool results often arrive as a sequence of content blocks, most commonly
    `[{"type":"text","text":"..."}]`. Keeping sequence detection here avoids
    scattering exact `list` checks through classification and progress logic.
    """

    items: tuple[Any, ...] = ()

    @field_validator("items", mode="before")
    @classmethod
    def _coerce_items(cls, value: Any) -> tuple[Any, ...]:
        if value is None or isinstance(value, (str, bytes, dict, BaseModel)):
            return ()
        if isinstance(value, Iterable):
            return tuple(value)
        return ()

    @classmethod
    def from_value(cls, value: Any) -> "ToolResultPayloadCollection":
        if isinstance(value, cls):
            return value
        return cls(items=value)

    @property
    def has_items(self) -> bool:
        return bool(self.items)

    @property
    def first_text_block_text(self) -> str | None:
        if not self.items:
            return None
        first = self.items[0]
        if isinstance(first, ToolResultPayload):
            return first.text if first.type == "text" else None
        if not isinstance(first, dict):
            return None
        try:
            payload = ToolResultPayload.model_validate(first)
        except ValidationError:
            return None
        return payload.text if payload.type == "text" else None


class PlayerMessageSplit(BridgeModel):
    """Typed split of tool text and appended player-message trailers."""

    tool_text: str = ""
    player_text: str = ""

    @field_validator("tool_text", "player_text", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "")

    @property
    def has_player_message(self) -> bool:
        return bool(self.player_text)

    @classmethod
    def from_text(
        cls,
        value: Any,
        *,
        player_marker: str,
    ) -> "PlayerMessageSplit":
        if isinstance(value, cls):
            return value
        if not isinstance(value, str):
            return cls(tool_text=str(value), player_text="")
        split = TextMarkerSplit.from_text(value, player_marker)
        if not split.matched:
            return cls(tool_text=value, player_text="")
        return cls(tool_text=split.before.rstrip(), player_text=split.after.strip())

    def legacy_tuple(self) -> tuple[str, str]:
        return self.tool_text, self.player_text


class ToolResultContent(BridgeModel):
    """Normalized SDK tool-result content with player-message trailers removed."""

    text: str = ""
    value: Any = None
    player_messages: list[str] = Field(default_factory=list)

    @classmethod
    def from_sdk_content(
        cls,
        content: Any,
        *,
        player_marker: str = "\n\n--- Player Messages ---\n",
    ) -> "ToolResultContent":
        if content is None:
            return cls()

        if isinstance(content, str):
            tool_text, player_text = cls._split_player_messages(
                content,
                player_marker=player_marker,
            )
            if player_text:
                return cls(text=tool_text, value=tool_text, player_messages=[player_text])
            parsed = _json_value_or_missing(content)
            if parsed is _JSON_MISSING:
                return cls(text=content, value=content)
            stripped, player_messages = cls._strip_player_messages_from_value(
                parsed,
                player_marker=player_marker,
            )
            if player_messages:
                return cls(
                    text=cls._json_for_log(stripped),
                    value=stripped,
                    player_messages=player_messages,
                )
            return cls(text=content, value=parsed)

        stripped, player_messages = cls._strip_player_messages_from_value(
            content,
            player_marker=player_marker,
        )
        return cls(
            text=cls._json_for_log(stripped),
            value=stripped,
            player_messages=player_messages,
        )

    @property
    def player_message_text(self) -> str:
        return "\n".join(self.player_messages)

    def outcome(self, *, sdk_is_error: bool = False) -> "ToolResultOutcome":
        if self.value is not None and not isinstance(self.value, str):
            outcome = ToolResultOutcome.from_payload(self.value)
            if outcome:
                return outcome
        return ToolResultOutcome.from_text(self.text, sdk_is_error=sdk_is_error)

    def indicates_progress(
        self,
        *,
        text_is_error: Callable[[str], bool] | None = None,
    ) -> bool:
        if self.value is not None and not isinstance(self.value, str):
            return ToolResultOutcome.payload_indicates_progress(
                self.value,
                text_is_error=text_is_error,
            )
        return ToolResultOutcome.text_indicates_progress(
            self.text,
            text_is_error=text_is_error,
        )

    @staticmethod
    def _json_for_log(value: Any) -> str:
        try:
            return json.dumps(value, ensure_ascii=False, separators=(",", ":"))
        except (TypeError, ValueError):
            return str(value)

    @staticmethod
    def _split_player_messages(
        text: Any,
        *,
        player_marker: str,
    ) -> tuple[str, str]:
        return PlayerMessageSplit.from_text(
            text,
            player_marker=player_marker,
        ).legacy_tuple()

    @classmethod
    def _strip_player_messages_from_value(
        cls,
        value: Any,
        *,
        player_marker: str,
    ) -> tuple[Any, list[str]]:
        if isinstance(value, dict):
            if value.get("type") == "text":
                stripped, player_text = cls._split_player_messages(
                    str(value.get("text", "")),
                    player_marker=player_marker,
                )
                updated = dict(value)
                updated["text"] = stripped
                return updated, [player_text] if player_text else []
            updated: dict[Any, Any] = {}
            player_messages: list[str] = []
            for key, item in value.items():
                updated_item, item_messages = cls._strip_player_messages_from_value(
                    item,
                    player_marker=player_marker,
                )
                updated[key] = updated_item
                player_messages.extend(item_messages)
            return updated, player_messages
        collection = ToolResultPayloadCollection.from_value(value)
        if collection.has_items:
            updated_items = []
            player_messages: list[str] = []
            for item in collection.items:
                updated_item, item_messages = cls._strip_player_messages_from_value(
                    item,
                    player_marker=player_marker,
                )
                updated_items.append(updated_item)
                player_messages.extend(item_messages)
            return updated_items, player_messages
        return value, []


class McpTextPayload(BridgeModel):
    """Parsed MCP text wrapper payload.

    Many Factorio MCP results are logged as either raw JSON or the SDK-style
    list wrapper `[{"type":"text","text":"..."}]`. This model keeps that
    unwrapping in one place so report, journal, and classification code do not
    each grow their own half-parser.
    """

    raw_text: str = ""
    value: Any = None
    text: str = ""

    @classmethod
    def from_text(cls, value: Any) -> "McpTextPayload":
        if isinstance(value, cls):
            return value
        raw_text = str(value if value is not None else "").strip()
        if not raw_text:
            return cls(raw_text="", value="", text="")
        parsed = _json_value_or_missing(raw_text)
        if parsed is _JSON_MISSING:
            return cls(raw_text=raw_text, value=raw_text, text=raw_text)
        block_text = ToolResultPayloadCollection.from_value(parsed).first_text_block_text
        if block_text is not None:
            nested = _json_value_or_missing(block_text)
            if nested is _JSON_MISSING:
                return cls(raw_text=raw_text, value=block_text, text=block_text)
            return cls(raw_text=raw_text, value=nested, text=block_text)
        return cls(raw_text=raw_text, value=parsed, text=raw_text)


class GameRejectionPayload(RemotePayloadModel):
    """Typed view of payloads reported as gameplay rejections in bridge logs."""

    raw_text: str = ""
    success: bool | None = None
    entity: Any = None
    recipe: Any = None
    error: Any = None
    classification: Any = None
    action_needed: Any = None
    researched_count: Any = None
    research_progress: Any = None
    research_queue: Any = None
    current_research: Any = None
    automation_verified: Any = None
    verification: Any = None

    @field_validator("raw_text", mode="before")
    @classmethod
    def _coerce_raw_text(cls, value: Any) -> str:
        return str(value) if value is not None else ""

    @classmethod
    def from_payload(cls, value: Any) -> "GameRejectionPayload":
        if isinstance(value, cls):
            return value
        parsed, raw_text = _unwrap_mcp_text_payload(value)
        if isinstance(parsed, dict):
            try:
                return cls.model_validate({**parsed, "raw_text": raw_text})
            except ValidationError:
                return cls(raw_text=raw_text or str(value))
        return cls(raw_text=str(parsed if parsed is not None else raw_text))

    @property
    def has_research_status_fields(self) -> bool:
        if any(
            getattr(self, field) is not None
            for field in (
                "researched_count",
                "research_progress",
                "research_queue",
                "current_research",
            )
        ):
            return True
        normalized = self.raw_text.lower()
        return any(
            marker in normalized
            for marker in (
                "researched_count",
                "research_progress",
                "research_queue",
                "current_research",
            )
        )

    @property
    def has_invalid_request_fields(self) -> bool:
        action_needed = str(self.action_needed or "").lower()
        error = str(self.error or "").lower()
        classification = str(self.classification or "").lower()
        raw = self.raw_text.lower()
        return (
            "invalid_request" in classification
            or action_needed.startswith("fix_")
            or _contains_invalid_request_marker(error)
            or _contains_invalid_request_marker(raw)
        )

    @property
    def automation_verification_failed(self) -> bool:
        for value in (self.automation_verified, self.verification):
            if isinstance(value, dict) and value.get("success") is False:
                return True
        return False

    def evidence(self, *, limit: int = 180) -> "GameRejectionEvidence":
        return GameRejectionEvidence.from_payload(self, limit=limit)

    def is_research_status(self) -> bool:
        return self.evidence().is_research_status

    def is_invalid_request(self) -> bool:
        return self.evidence().is_invalid_request

    def signature(self, *, limit: int = 180) -> str:
        return self.evidence(limit=limit).signature


def _unwrap_mcp_text_payload(value: Any) -> tuple[Any, str]:
    if not isinstance(value, str):
        return value, ""
    payload = McpTextPayload.from_text(value)
    return payload.value, payload.text


def _contains_invalid_request_marker(value: str) -> bool:
    return (
        "value for required field" in value
        or "failed to deserialize" in value
        or "invalid type:" in value
        or "missing field" in value
    )


class GameRejectionEvidence(BridgeModel):
    """Typed evidence extracted from a game-rejected payload."""

    kind: GameRejectionEvidenceKind = GameRejectionEvidenceKind.EMPTY
    signature: str = ""
    reason: str = ""

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> GameRejectionEvidenceKind:
        if isinstance(value, GameRejectionEvidenceKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in GameRejectionEvidenceKind:
                if normalized == kind.value:
                    return kind
        return GameRejectionEvidenceKind.EMPTY

    @field_validator("signature", "reason", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "")

    @property
    def is_research_status(self) -> bool:
        return self.kind == GameRejectionEvidenceKind.RESEARCH_STATUS

    @property
    def is_invalid_request(self) -> bool:
        return self.kind == GameRejectionEvidenceKind.INVALID_REQUEST

    @property
    def is_gameplay_failure(self) -> bool:
        return self.kind == GameRejectionEvidenceKind.GAMEPLAY_FAILURE

    @classmethod
    def from_payload(
        cls,
        payload: "GameRejectionPayload",
        *,
        limit: int = 180,
    ) -> "GameRejectionEvidence":
        if payload.has_research_status_fields:
            return cls(
                kind=GameRejectionEvidenceKind.RESEARCH_STATUS,
                reason="research-status payload",
            )
        if payload.has_invalid_request_fields:
            return cls(
                kind=GameRejectionEvidenceKind.INVALID_REQUEST,
                reason="invalid-request payload",
            )
        if payload.automation_verification_failed:
            return cls(
                kind=GameRejectionEvidenceKind.GAMEPLAY_FAILURE,
                signature="automation_unverified",
                reason="automation verification payload",
            )
        parts: list[str] = []
        if payload.error:
            parts.append(str(payload.error))
        if payload.entity:
            parts.append(f"entity={payload.entity}")
        if payload.recipe:
            parts.append(f"recipe={payload.recipe}")
        signature = " | ".join(parts) if parts else " ".join(payload.raw_text.split())
        signature = signature[:limit]
        if signature:
            return cls(
                kind=GameRejectionEvidenceKind.GAMEPLAY_FAILURE,
                signature=signature,
                reason="gameplay rejection payload",
            )
        return cls()


class ToolResultTextEvidence(BridgeModel):
    """Typed evidence extracted from unstructured tool-result text."""

    raw_text: str = ""
    normalized_text: str = ""
    kind: ToolResultTextKind = ToolResultTextKind.NONE
    classification: ToolResultClassification | None = None
    reason: str = ""

    @field_validator("raw_text", "normalized_text", "reason", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "")

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> ToolResultTextKind:
        if isinstance(value, ToolResultTextKind):
            return value
        try:
            return ToolResultTextKind(str(value))
        except ValueError:
            return ToolResultTextKind.NONE

    @field_validator("classification", mode="before")
    @classmethod
    def _coerce_classification(
        cls,
        value: Any,
    ) -> ToolResultClassification | None:
        if value is None or value == "":
            return None
        if isinstance(value, ToolResultClassification):
            return value
        try:
            return ToolResultClassification(str(value))
        except ValueError:
            return None

    @property
    def classified(self) -> bool:
        return self.classification is not None

    @classmethod
    def from_classification(
        cls,
        value: Any,
        classification: ToolResultClassification | str,
        *,
        kind: ToolResultTextKind = ToolResultTextKind.CUSTOM,
        reason: str = "custom text classifier",
    ) -> "ToolResultTextEvidence":
        text = str(value or "").strip()
        return cls(
            raw_text=text,
            normalized_text=text.lower(),
            kind=kind,
            classification=classification,
            reason=reason,
        )

    @classmethod
    def from_text(cls, value: Any) -> "ToolResultTextEvidence":
        text = str(value or "").strip()
        lowered = text.lower()
        if not text:
            return cls(raw_text="", normalized_text="")
        if any(text.startswith(prefix) for prefix in _TOOL_OPERATOR_ONLY_PREFIXES):
            return cls(
                raw_text=text,
                normalized_text=lowered,
                kind=ToolResultTextKind.OPERATOR_ONLY,
                classification=ToolResultClassification.OK,
                reason="operator-only guard or policy refusal",
            )
        if lowered in _TOOL_BENIGN_MISSES:
            return cls(
                raw_text=text,
                normalized_text=lowered,
                kind=ToolResultTextKind.EXPECTED_MISS,
                classification=ToolResultClassification.EXPECTED_MISS,
                reason="benign expected miss",
            )
        if _TOOL_INVALID_REQUEST_RE.search(lowered):
            return cls(
                raw_text=text,
                normalized_text=lowered,
                kind=ToolResultTextKind.INVALID_REQUEST,
                classification=ToolResultClassification.INVALID_REQUEST,
                reason="invalid tool request text",
            )
        if _TOOL_INFRASTRUCTURE_FAILURE_RE.search(lowered):
            return cls(
                raw_text=text,
                normalized_text=lowered,
                kind=ToolResultTextKind.INFRASTRUCTURE_FAILURE,
                classification=ToolResultClassification.INFRASTRUCTURE_FAILURE,
                reason="bridge or infrastructure failure text",
            )
        if _TOOL_GAME_REJECTED_RE.search(lowered):
            return cls(
                raw_text=text,
                normalized_text=lowered,
                kind=ToolResultTextKind.GAME_REJECTED,
                classification=ToolResultClassification.GAME_REJECTED,
                reason="gameplay rejection text",
            )
        if lowered.startswith("error:") or lowered.startswith("error "):
            return cls(
                raw_text=text,
                normalized_text=lowered,
                kind=ToolResultTextKind.SDK_FAILURE,
                classification=ToolResultClassification.SDK_FAILURE,
                reason="generic sdk error text",
            )
        return cls(raw_text=text, normalized_text=lowered)


class ToolResultOutcome(BridgeModel):
    """Structured classification for tool results before text fallback."""

    _FAILURE_CLASSIFICATIONS: ClassVar[frozenset[ToolResultClassification]] = frozenset({
        ToolResultClassification.INVALID_REQUEST,
        ToolResultClassification.GAME_REJECTED,
        ToolResultClassification.SDK_FAILURE,
        ToolResultClassification.INFRASTRUCTURE_FAILURE,
    })

    classification: ToolResultClassification
    source: str = "structured"
    message: str = ""
    text_evidence: ToolResultTextEvidence | None = None

    @classmethod
    def from_text(
        cls,
        value: str,
        *,
        sdk_is_error: bool = False,
        text_classifier: Callable[[str], ToolResultClassification | str | None] | None = None,
    ) -> "ToolResultOutcome":
        stripped = str(value or "").strip()
        if not stripped:
            return cls(classification=ToolResultClassification.OK, source="empty")

        parsed = _json_value_or_missing(stripped)
        if parsed is _JSON_MISSING:
            parsed = None

        if parsed is not None:
            parsed_outcome = cls.from_payload(
                parsed,
                text_classifier=text_classifier,
            )
            if parsed_outcome:
                return parsed_outcome
            if sdk_is_error:
                return cls(
                    classification=ToolResultClassification.SDK_FAILURE,
                    source="sdk_error_json",
                    message=stripped,
                )
            return cls(
                classification=ToolResultClassification.OK,
                source="json",
                message=stripped,
            )

        text_evidence = cls._classify_text_evidence(stripped, text_classifier)
        if text_evidence.classification:
            return cls(
                classification=text_evidence.classification,
                source="text",
                message=stripped,
                text_evidence=text_evidence,
            )

        if sdk_is_error:
            return cls(
                classification=ToolResultClassification.SDK_FAILURE,
                source="sdk_error_text",
                message=stripped,
                text_evidence=text_evidence,
            )
        return cls(
            classification=ToolResultClassification.OK,
            source="text",
            message=stripped,
            text_evidence=text_evidence,
        )

    @property
    def value(self) -> str:
        return self.classification.value

    @property
    def indicates_failure(self) -> bool:
        return self.classification in self._FAILURE_CLASSIFICATIONS

    @property
    def should_journal_failure(self) -> bool:
        return self.indicates_failure

    @classmethod
    def text_indicates_failure(cls, value: Any) -> bool:
        return cls.from_text(str(value or "")).indicates_failure

    @property
    def log_level(self) -> str:
        if self.classification in {
            ToolResultClassification.OK,
            ToolResultClassification.EXPECTED_MISS,
        }:
            return "debug"
        if self.classification == ToolResultClassification.GAME_REJECTED:
            return "info"
        if self.indicates_failure:
            return "warning"
        return "debug"

    @staticmethod
    def _mapping_from_value(value: Any) -> Mapping[str, Any] | None:
        if isinstance(value, BaseModel):
            return value.model_dump()
        if isinstance(value, Mapping):
            return value
        return None

    @classmethod
    def _automation_verification_failed(
        cls,
        *,
        success_false: bool,
        extra_items: Iterable[tuple[str, Any]],
    ) -> bool:
        if not success_false:
            return False
        items = dict(extra_items)
        for key in ("automation_verified", "verification"):
            diagnostic = cls._mapping_from_value(items.get(key))
            if not diagnostic:
                continue
            if diagnostic.get("success") is False:
                return True
            statuses = diagnostic.get("placed_unit_statuses")
            if isinstance(statuses, Sequence) and not isinstance(statuses, (str, bytes)):
                for status in statuses:
                    status_map = cls._mapping_from_value(status)
                    if not status_map:
                        continue
                    status_text = str(status_map.get("status") or "").strip().lower()
                    if status_text and status_text not in {"working", "ok"}:
                        return True
        return "automation_verified" in items and items.get("success") is False

    @classmethod
    def from_payload(
        cls,
        value: Any,
        *,
        text_classifier: Callable[[str], ToolResultClassification | str | None] | None = None,
    ) -> "ToolResultOutcome | None":
        if isinstance(value, cls):
            return value
        collection = ToolResultPayloadCollection.from_value(value)
        if collection.has_items:
            saw_ok = False
            for item in collection.items:
                outcome = cls.from_payload(item, text_classifier=text_classifier)
                if outcome and outcome.classification != ToolResultClassification.OK:
                    return outcome
                if outcome and outcome.classification == ToolResultClassification.OK:
                    saw_ok = True
            if saw_ok:
                return cls(classification=ToolResultClassification.OK, source="list")
            return None

        extra_items: Iterable[tuple[str, Any]] = ()
        if isinstance(value, ToolResultPayload):
            payload = value
            extra_items = (value.model_extra or {}).items()
        else:
            if not isinstance(value, dict):
                return None
            try:
                payload = ToolResultPayload.model_validate(value)
            except ValidationError:
                return None
            extra_items = value.items()

        if payload.type == "text":
            text = payload.text or ""
            nested = _json_value_or_missing(text)
            if nested is _JSON_MISSING:
                text_evidence = cls._classify_text_evidence(text, text_classifier)
                if text_evidence.classification:
                    return cls(
                        classification=text_evidence.classification,
                        source="text",
                        text_evidence=text_evidence,
                    )
                return None
            return cls.from_payload(nested, text_classifier=text_classifier)

        if payload.success is True:
            return cls(classification=ToolResultClassification.OK, source="success")

        success_false = payload.success is False

        if payload.success is False and payload.expected_miss is True:
            return cls(
                classification=ToolResultClassification.EXPECTED_MISS,
                source="expected_miss",
            )

        if (
            payload.success is False
            and payload.mined_count == 0
            and payload.error is None
        ):
            return cls(
                classification=ToolResultClassification.EXPECTED_MISS,
                source="empty_mining",
            )

        if (
            payload.success is False
            and payload.can_place is False
            and payload.entity
            and payload.position is not None
        ):
            return cls(
                classification=ToolResultClassification.GAME_REJECTED,
                source="placement_rejected",
            )

        if (
            payload.allowed is not None
            and payload.policy_allowed is not None
            and payload.factorio_allowed is not None
            and payload.entity
            and payload.position is not None
        ):
            return cls(
                classification=ToolResultClassification.OK,
                source="placement_diagnostic",
            )

        if cls._automation_verification_failed(
            success_false=success_false,
            extra_items=extra_items,
        ):
            return cls(
                classification=ToolResultClassification.GAME_REJECTED,
                source="automation_unverified",
            )

        for key in ("error", "message", "reason", "action_needed"):
            item = getattr(payload, key)
            if not item:
                continue
            if key != "error" and not success_false:
                continue
            text_evidence = cls._classify_text_evidence(str(item), text_classifier)
            if text_evidence.classification:
                return cls(
                    classification=text_evidence.classification,
                    source=key,
                    text_evidence=text_evidence,
                )
            if key == "error":
                if success_false:
                    return cls(
                        classification=ToolResultClassification.GAME_REJECTED,
                        source="error",
                    )
                return cls(
                    classification=ToolResultClassification.SDK_FAILURE,
                    source="error",
                )
            if success_false:
                break

        if success_false and any(
            getattr(payload, key) for key in ("message", "reason", "action_needed")
        ):
            return cls(
                classification=ToolResultClassification.GAME_REJECTED,
                source="failure_reason",
            )

        for key in ("status", "state", "result"):
            item_text = str(getattr(payload, key) or "").strip().lower()
            if item_text in {"error", "failed", "failure", "fail"}:
                return cls(
                    classification=ToolResultClassification.SDK_FAILURE,
                    source=key,
                )

        for key, item in extra_items:
            if key in ToolResultPayload.model_fields:
                continue
            child = cls.from_payload(item, text_classifier=text_classifier)
            if child and child.classification != ToolResultClassification.OK:
                return child

        return None

    @classmethod
    def _classify_text(
        cls,
        value: str,
        text_classifier: Callable[[str], ToolResultClassification | str | None] | None,
    ) -> ToolResultClassification | None:
        return cls._classify_text_evidence(value, text_classifier).classification

    @classmethod
    def _classify_text_evidence(
        cls,
        value: str,
        text_classifier: Callable[[str], ToolResultClassification | str | None] | None,
    ) -> ToolResultTextEvidence:
        if text_classifier:
            classified = text_classifier(value)
            if isinstance(classified, ToolResultClassification):
                return ToolResultTextEvidence.from_classification(value, classified)
            if isinstance(classified, str):
                try:
                    return ToolResultTextEvidence.from_classification(
                        value,
                        ToolResultClassification(classified),
                    )
                except ValueError:
                    pass
        return ToolResultTextEvidence.from_text(value)

    @staticmethod
    def _default_text_classification(value: Any) -> ToolResultClassification | None:
        return ToolResultTextEvidence.from_text(value).classification

    @classmethod
    def payload_indicates_progress(
        cls,
        value: Any,
        *,
        text_is_error: Callable[[str], bool] | None = None,
    ) -> bool:
        outcome = cls.from_payload(value)
        if outcome:
            return outcome.classification == ToolResultClassification.OK

        collection = ToolResultPayloadCollection.from_value(value)
        if collection.has_items:
            return any(
                cls.payload_indicates_progress(item, text_is_error=text_is_error)
                for item in collection.items
            )
        if isinstance(value, ToolResultPayload):
            payload = value
        elif not isinstance(value, dict):
            return True
        else:
            try:
                payload = ToolResultPayload.model_validate(value)
            except ValidationError:
                return True

        if payload.success is False:
            return False
        if payload.error and payload.success is not True:
            return False
        if payload.type == "text":
            text = str(payload.text or "").strip()
            if not text:
                return False
            nested = _json_value_or_missing(text)
            if nested is _JSON_MISSING:
                return not (
                    text_is_error(text)
                    if text_is_error
                    else cls.text_indicates_failure(text)
                )
            return cls.payload_indicates_progress(nested, text_is_error=text_is_error)
        return True

    @classmethod
    def text_indicates_progress(
        cls,
        value: Any,
        *,
        text_is_error: Callable[[str], bool] | None = None,
    ) -> bool:
        stripped = str(value or "").strip()
        if not stripped:
            return False
        parsed = _json_value_or_missing(stripped)
        if parsed is _JSON_MISSING:
            return True
        return cls.payload_indicates_progress(parsed, text_is_error=text_is_error)


class ToolResultLogRecord(BridgeModel):
    """Typed logging/journaling decision for one tool result."""

    classification: ToolResultClassification = ToolResultClassification.OK
    log_level: ToolResultLogLevel = ToolResultLogLevel.DEBUG
    log_label: str = "tool_result"
    text: str = ""
    journal_failure_text: str = ""

    @field_validator("classification", mode="before")
    @classmethod
    def _coerce_classification(cls, value: Any) -> ToolResultClassification:
        if isinstance(value, ToolResultClassification):
            return value
        if isinstance(value, str):
            try:
                return ToolResultClassification(value.strip().lower())
            except ValueError:
                return ToolResultClassification.SDK_FAILURE
        return ToolResultClassification.SDK_FAILURE

    @field_validator("log_level", mode="before")
    @classmethod
    def _coerce_log_level(cls, value: Any) -> ToolResultLogLevel:
        if isinstance(value, ToolResultLogLevel):
            return value
        if isinstance(value, str):
            try:
                return ToolResultLogLevel(value.strip().lower())
            except ValueError:
                return ToolResultLogLevel.WARNING
        return ToolResultLogLevel.WARNING

    @field_validator("log_label", "text", "journal_failure_text", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "")

    @classmethod
    def from_outcome(
        cls,
        outcome: ToolResultOutcome,
        *,
        text: Any = "",
        text_limit: int = 500,
    ) -> "ToolResultLogRecord":
        raw_text = str(text or "")
        classification = outcome.classification
        if classification == ToolResultClassification.OK:
            label = "tool_result"
        else:
            label = f"tool_result {classification.value}"
        journal_text = ""
        if outcome.should_journal_failure:
            journal_text = (
                f"{classification.value}: "
                f"{_single_line_text(raw_text, limit=text_limit)}"
            )
        return cls(
            classification=classification,
            log_level=outcome.log_level,
            log_label=label,
            text=raw_text,
            journal_failure_text=journal_text,
        )

    @property
    def should_emit_log(self) -> bool:
        return self.classification != ToolResultClassification.OK or bool(self.text.strip())

    @property
    def should_journal_failure(self) -> bool:
        return bool(self.journal_failure_text)


class BridgeRunReport(MutableBridgeModel):
    log_path: str = ""
    started_at: str = ""
    ended_at: str = ""
    duration_s: float = 0.0
    sdk_attempts: int = 0
    sdk_done: int = 0
    provider_pauses: int = 0
    provider_reset_until: str = ""
    context_resets: int = 0
    watchdog_aborts: int = 0
    research_completed_events: int = 0
    max_research_count: int = 0
    latest_entities: str = ""
    latest_objective: str = ""
    latest_progress: str = ""
    latest_power: str = ""
    live_attempted: bool = False
    live_connected: bool = False
    live_state: str = ""
    live_entities: str = ""
    live_power: str = ""
    live_error: str = ""
    recent_progress_events: int = 0
    recent_progress_window_s: float = 1800.0
    automation_tool_calls: int = 0
    manual_transfer_tool_calls: int = 0
    automation_to_manual_ratio: float | None = None
    fuel_automation_tool_calls: int = 0
    manual_fuel_transfer_tool_calls: int = 0
    fuel_automation_to_manual_ratio: float | None = None
    science_automation_tool_calls: int = 0
    manual_science_transfer_tool_calls: int = 0
    science_automation_to_manual_ratio: float | None = None
    material_flow_automation_tool_calls: int = 0
    manual_material_transfer_tool_calls: int = 0
    material_flow_automation_to_manual_ratio: float | None = None
    component_automation_tool_calls: int = 0
    manual_component_craft_tool_calls: int = 0
    component_automation_to_manual_ratio: float | None = None
    automation_verified_successes: int = 0
    automation_verified_failures: int = 0
    top_gameplay_rejections: list[tuple[str, int]] = Field(default_factory=list)
    verdict: str = "operator attention needed: no bridge records found"

    @field_validator("recent_progress_window_s", mode="before")
    @classmethod
    def _positive_recent_window(cls, value: Any) -> float:
        try:
            window = float(value)
        except (TypeError, ValueError):
            return 1800.0
        return max(1.0, window)

    def to_dict(self) -> dict[str, Any]:
        return {
            "log_path": self.log_path,
            "started_at": self.started_at,
            "ended_at": self.ended_at,
            "duration_s": self.duration_s,
            "sdk_attempts": self.sdk_attempts,
            "sdk_done": self.sdk_done,
            "provider_pauses": self.provider_pauses,
            "provider_reset_until": self.provider_reset_until,
            "context_resets": self.context_resets,
            "watchdog_aborts": self.watchdog_aborts,
            "research_completed_events": self.research_completed_events,
            "max_research_count": self.max_research_count,
            "latest_entities": self.latest_entities,
            "latest_objective": self.latest_objective,
            "latest_progress": self.latest_progress,
            "latest_power": self.latest_power,
            "live_attempted": self.live_attempted,
            "live_connected": self.live_connected,
            "live_state": self.live_state,
            "live_entities": self.live_entities,
            "live_power": self.live_power,
            "live_error": self.live_error,
            "recent_progress_events": self.recent_progress_events,
            "recent_progress_window_s": self.recent_progress_window_s,
            "automation_tool_calls": self.automation_tool_calls,
            "manual_transfer_tool_calls": self.manual_transfer_tool_calls,
            "automation_to_manual_ratio": self.automation_to_manual_ratio,
            "fuel_automation_tool_calls": self.fuel_automation_tool_calls,
            "manual_fuel_transfer_tool_calls": self.manual_fuel_transfer_tool_calls,
            "fuel_automation_to_manual_ratio": self.fuel_automation_to_manual_ratio,
            "science_automation_tool_calls": self.science_automation_tool_calls,
            "manual_science_transfer_tool_calls": self.manual_science_transfer_tool_calls,
            "science_automation_to_manual_ratio": self.science_automation_to_manual_ratio,
            "material_flow_automation_tool_calls": self.material_flow_automation_tool_calls,
            "manual_material_transfer_tool_calls": self.manual_material_transfer_tool_calls,
            "material_flow_automation_to_manual_ratio": self.material_flow_automation_to_manual_ratio,
            "component_automation_tool_calls": self.component_automation_tool_calls,
            "manual_component_craft_tool_calls": self.manual_component_craft_tool_calls,
            "component_automation_to_manual_ratio": self.component_automation_to_manual_ratio,
            "automation_verified_successes": self.automation_verified_successes,
            "automation_verified_failures": self.automation_verified_failures,
            "top_gameplay_rejections": [
                {"count": count, "signature": signature}
                for signature, count in self.top_gameplay_rejections
            ],
            "verdict": self.verdict,
        }

    def to_json_text(self, *, indent: int | None = None, sort_keys: bool = False) -> str:
        return json.dumps(self.to_dict(), indent=indent, sort_keys=sort_keys)


class RconJsonResponse(BridgeModel):
    """Typed RCON response that contains a JSON value on its final output lines."""

    raw_text: str = ""
    value: Any = None

    @classmethod
    def from_text(cls, value: Any) -> "RconJsonResponse":
        if isinstance(value, cls):
            return value
        raw_text = str(value if value is not None else "")
        for candidate in BridgeTextLines.from_text(raw_text, keep_blank=False).reversed_non_empty:
            parsed = _json_value_or_missing(candidate)
            if parsed is _JSON_MISSING:
                continue
            return cls(raw_text=raw_text, value=parsed)
        parsed = _json_value_or_missing(raw_text)
        if parsed is _JSON_MISSING:
            raise BridgeValidationError(
                "rcon_response",
                f"did not contain JSON: {_single_line_text(raw_text)}",
            )
        return cls(raw_text=raw_text, value=parsed)

    @classmethod
    def parse_value(cls, value: Any) -> Any:
        if isinstance(value, cls):
            return value.value
        return cls.from_text(value).value


class RconTextResponse(BridgeModel):
    """Typed RCON response where the useful value is the final non-empty line."""

    raw_text: str = ""
    text: str = ""

    @field_validator("raw_text", "text", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "")

    @classmethod
    def from_text(cls, value: Any) -> "RconTextResponse":
        raw_text = str(value if value is not None else "")
        for candidate in BridgeTextLines.from_text(raw_text, keep_blank=False).reversed_non_empty:
            return cls(raw_text=raw_text, text=candidate)
        return cls(raw_text=raw_text, text="")

    @classmethod
    def final_line(cls, value: Any) -> str:
        return cls.from_text(value).text


class ModInterfaceStatus(BridgeModel):
    """Typed status for the claude-interface remote interface probe."""

    loaded: bool = False

    @classmethod
    def from_rcon_response(cls, value: Any) -> "ModInterfaceStatus":
        if isinstance(value, cls):
            return value
        payload = RconJsonResponse.parse_value(value)
        try:
            return cls.model_validate(payload)
        except ValidationError as exc:
            raise BridgeValidationError(
                "mod_interface_status",
                f"unexpected mod interface status payload: {payload!r}",
            ) from exc


class RconRemoteCall(BridgeModel):
    """Validated claude_interface remote call rendered for Factorio RCON."""

    remote_name: str
    args: list[str] = Field(default_factory=list)
    print_result: bool = True
    stringify_result: bool = False

    @field_validator("remote_name", mode="before")
    @classmethod
    def _coerce_remote_name(cls, value: Any) -> str:
        remote_name = str(value or "").strip()
        if not re.fullmatch(r"[A-Za-z0-9_]+", remote_name):
            raise ValueError(f"invalid remote name: {value}")
        return remote_name

    @field_validator("args", mode="before")
    @classmethod
    def _coerce_args(cls, value: Any) -> list[str]:
        if value is None:
            return []
        if isinstance(value, (list, tuple)):
            return [str(item) for item in value]
        return [str(value)]

    @classmethod
    def command(cls, remote_name: Any, *args: Any) -> str:
        return cls(remote_name=remote_name, args=list(args)).to_command()

    @classmethod
    def side_effect_command(cls, remote_name: Any, *args: Any) -> str:
        return cls(
            remote_name=remote_name,
            args=list(args),
            print_result=False,
        ).to_command()

    @classmethod
    def string_command(cls, remote_name: Any, *args: Any) -> str:
        return cls(
            remote_name=remote_name,
            args=list(args),
            stringify_result=True,
        ).to_command()

    def to_command(self) -> str:
        suffix = "".join(f", {arg}" for arg in self.args)
        call = f'remote.call("claude_interface", "{self.remote_name}"{suffix})'
        if self.print_result and self.stringify_result:
            body = f"rcon.print(tostring({call}))"
        elif self.print_result:
            body = f"rcon.print({call})"
        else:
            body = call
        return f"/silent-command {body}"


def _expected_payload_text(value: Any, field_path: str) -> str:
    text = str(value or "").strip()
    if not text:
        raise BridgeValidationError(field_path, "expected non-empty text")
    return text


def _payload_with_expected_text_field(
    payload: Any,
    *,
    key: str,
    expected: Any,
    field_path: str,
) -> dict[str, Any]:
    if not isinstance(payload, dict):
        raise BridgeValidationError(field_path, "expected object")
    expected_text = _expected_payload_text(expected, key)
    merged = dict(payload)
    observed = str(merged.get(key) or "").strip()
    if observed and observed != expected_text:
        raise BridgeValidationError(
            f"{field_path}.{key}",
            f"expected {expected_text!r}, got {observed!r}",
        )
    merged[key] = expected_text
    return merged


class SurfaceSetupResult(BridgeModel):
    """One remote ensure-surface result from the Factorio mod."""

    planet: str
    status: str

    @field_validator("planet", "status", mode="before")
    @classmethod
    def _coerce_non_empty_text(cls, value: Any) -> str:
        text = str(value or "").strip()
        if not text:
            raise ValueError("expected non-empty text")
        return text

    @classmethod
    def from_rcon_response(cls, value: Any, *, planet: Any) -> "SurfaceSetupResult":
        if isinstance(value, cls):
            _payload_with_expected_text_field(
                value.model_dump(),
                key="planet",
                expected=planet,
                field_path="surface_setup_result",
            )
            return value
        payload = RconJsonResponse.parse_value(value)
        try:
            payload = _payload_with_expected_text_field(
                payload,
                key="planet",
                expected=planet,
                field_path="surface_setup_result",
            )
        except BridgeValidationError:
            raise
        try:
            return cls.model_validate(payload)
        except ValidationError as exc:
            raise BridgeValidationError(
                "surface_setup_result",
                f"unexpected surface setup payload for {planet!r}: {payload!r}",
            ) from exc


class SurfaceSetupResults(BridgeModel):
    """Typed batch result for multi-surface setup."""

    results: tuple[SurfaceSetupResult, ...] = Field(default_factory=tuple)

    @field_validator("results", mode="before")
    @classmethod
    def _coerce_results(cls, value: Any) -> tuple[SurfaceSetupResult, ...]:
        if value is None:
            return ()
        if isinstance(value, dict):
            return tuple(
                SurfaceSetupResult(planet=planet, status=status)
                for planet, status in value.items()
            )
        if isinstance(value, SurfaceSetupResult):
            return (value,)
        if isinstance(value, Iterable) and not isinstance(value, (str, bytes)):
            results: list[SurfaceSetupResult] = []
            for item in value:
                if isinstance(item, SurfaceSetupResult):
                    results.append(item)
                elif isinstance(item, dict):
                    results.append(SurfaceSetupResult(**item))
                elif isinstance(item, (list, tuple)) and len(item) == 2:
                    planet, status = item
                    results.append(SurfaceSetupResult(planet=planet, status=status))
            return tuple(results)
        return ()

    @classmethod
    def from_mapping(cls, value: Any) -> "SurfaceSetupResults":
        if isinstance(value, cls):
            return value
        return cls(results=value)

    def items(self) -> tuple[tuple[str, str], ...]:
        return tuple((result.planet, result.status) for result in self.results)

    def to_dict(self) -> dict[str, str]:
        return dict(self.items())


class CharacterPlacementResult(BridgeModel):
    """Typed result for pre-placing or teleporting an agent character."""

    agent_name: str
    planet: str
    status: str

    @field_validator("agent_name", "planet", "status", mode="before")
    @classmethod
    def _coerce_non_empty_text(cls, value: Any) -> str:
        text = str(value or "").strip()
        if not text:
            raise ValueError("expected non-empty text")
        return text

    def to_status(self) -> str:
        return self.status

    @classmethod
    def from_rcon_response(
        cls,
        value: Any,
        *,
        agent_name: Any,
        planet: Any,
    ) -> "CharacterPlacementResult":
        if isinstance(value, cls):
            payload = value.model_dump()
            payload = _payload_with_expected_text_field(
                payload,
                key="agent_name",
                expected=agent_name,
                field_path="character_placement_result",
            )
            _payload_with_expected_text_field(
                payload,
                key="planet",
                expected=planet,
                field_path="character_placement_result",
            )
            return value
        payload = RconJsonResponse.parse_value(value)
        try:
            payload = _payload_with_expected_text_field(
                payload,
                key="agent_name",
                expected=agent_name,
                field_path="character_placement_result",
            )
            payload = _payload_with_expected_text_field(
                payload,
                key="planet",
                expected=planet,
                field_path="character_placement_result",
            )
        except BridgeValidationError:
            raise
        try:
            return cls.model_validate(payload)
        except ValidationError as exc:
            raise BridgeValidationError(
                "character_placement_result",
                f"unexpected character placement payload for {agent_name!r} on {planet!r}: {payload!r}",
            ) from exc


class ConnectedPlayerCountResult(BridgeModel):
    """Typed result for the connected real-player count probe."""

    count: int = 0

    @field_validator("count", mode="before")
    @classmethod
    def _coerce_count(cls, value: Any) -> int:
        try:
            count = int(value)
        except (TypeError, ValueError) as exc:
            raise ValueError("expected connected player count integer") from exc
        return max(0, count)

    @property
    def has_connected_players(self) -> bool:
        return self.count > 0

    @classmethod
    def from_rcon_response(cls, value: Any) -> "ConnectedPlayerCountResult":
        if isinstance(value, cls):
            return value
        payload = RconJsonResponse.parse_value(value)
        try:
            return cls.model_validate(payload)
        except ValidationError as exc:
            raise BridgeValidationError(
                "connected_player_count",
                f"unexpected connected-player payload: {payload!r}",
            ) from exc


class BridgeLogRecord(BridgeModel):
    message: str = ""
    timestamp: float = 0.0
    time: str = ""
    level: str = ""
    agent: str = ""

    @classmethod
    def from_loguru_entry(cls, value: Any) -> "BridgeLogRecord | None":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return None
        record = value.get("record")
        if not isinstance(record, dict):
            return None
        time = record.get("time") if isinstance(record.get("time"), dict) else {}
        level = record.get("level") if isinstance(record.get("level"), dict) else {}
        extra = record.get("extra") if isinstance(record.get("extra"), dict) else {}
        return cls(
            message=str(record.get("message", "")),
            timestamp=_coerce_float(time.get("timestamp")),
            time=str(time.get("repr", "")),
            level=str(level.get("name", "")),
            agent=str(extra.get("agent", "")),
        )

    @classmethod
    def from_json_line(cls, value: Any) -> "BridgeLogRecord | None":
        if isinstance(value, cls):
            return value
        if not isinstance(value, str):
            return None
        line = value.strip()
        if not line:
            return None
        entry = _json_value_or_missing(line)
        if entry is _JSON_MISSING:
            return None
        return cls.from_loguru_entry(entry)


class BridgeLogRecordCollection(BridgeModel):
    """Typed collection boundary for report log records."""

    records: tuple[BridgeLogRecord, ...] = ()

    @field_validator("records", mode="before")
    @classmethod
    def _coerce_records(cls, value: Any) -> tuple[BridgeLogRecord, ...]:
        if value is None or isinstance(value, (str, bytes, dict, BaseModel)):
            return ()
        if not isinstance(value, Iterable):
            return ()
        records: list[BridgeLogRecord] = []
        for item in value:
            record = (
                BridgeLogRecord.from_json_line(item)
                if isinstance(item, str)
                else BridgeLogRecord.from_loguru_entry(item)
            )
            if record:
                records.append(record)
        return tuple(records)

    @classmethod
    def from_value(cls, value: Any) -> "BridgeLogRecordCollection":
        if isinstance(value, cls):
            return value
        return cls(records=value)

    def to_list(self) -> list[BridgeLogRecord]:
        return list(self.records)


class BridgeRunVerdictKind(str, Enum):
    NO_RECORDS = "no_records"
    PROVIDER_PAUSED = "provider_paused"
    AUTOMATION_UNVERIFIED = "automation_unverified"
    FUEL_ROUTE_IN_PROGRESS = "fuel_route_in_progress"
    FUEL_MANUAL_HEAVY = "fuel_manual_heavy"
    SCIENCE_MANUAL_HEAVY = "science_manual_heavy"
    MATERIAL_MANUAL_HEAVY = "material_manual_heavy"
    COMPONENT_MANUAL_HEAVY = "component_manual_heavy"
    MANUAL_HEAVY = "manual_heavy"
    RECENT_PROGRESS = "recent_progress"
    REPEATED_FAILURES = "repeated_failures"
    CONTEXT_RESETS = "context_resets"
    NO_RECENT_PROGRESS = "no_recent_progress"


class BridgeProgressTimestamps(BridgeModel):
    """Typed float timestamp collection for bridge run verdicts."""

    values: tuple[float, ...] = ()

    @field_validator("values", mode="before")
    @classmethod
    def _coerce_values(cls, value: Any) -> tuple[float, ...]:
        if value is None or isinstance(value, (str, bytes, dict, BaseModel)):
            return ()
        if not isinstance(value, Iterable):
            return ()
        return tuple(_coerce_float(timestamp) for timestamp in value)

    @classmethod
    def from_value(cls, value: Any) -> "BridgeProgressTimestamps":
        if isinstance(value, cls):
            return value
        return cls(values=value)

    @property
    def latest(self) -> float:
        return max(self.values) if self.values else 0.0


class BridgeRunVerdict(BridgeModel):
    """Typed operator-facing verdict for a bridge run report."""

    kind: BridgeRunVerdictKind = BridgeRunVerdictKind.NO_RECENT_PROGRESS
    provider_reset_until: str = ""

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> BridgeRunVerdictKind:
        if isinstance(value, BridgeRunVerdictKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in BridgeRunVerdictKind:
                if normalized == kind.value:
                    return kind
        return BridgeRunVerdictKind.NO_RECENT_PROGRESS

    @field_validator("provider_reset_until", mode="before")
    @classmethod
    def _coerce_reset(cls, value: Any) -> str:
        return str(value or "").strip()

    @classmethod
    def no_records(cls) -> "BridgeRunVerdict":
        return cls(kind=BridgeRunVerdictKind.NO_RECORDS)

    @classmethod
    def from_report_state(
        cls,
        report: Any,
        *,
        last_provider_pause_ts: Any = 0.0,
        progress_timestamps: Any = None,
    ) -> "BridgeRunVerdict":
        last_progress_ts = BridgeProgressTimestamps.from_value(progress_timestamps).latest
        provider_pause_ts = _coerce_float(last_provider_pause_ts)

        if (
            getattr(report, "provider_pauses", 0)
            and provider_pause_ts >= last_progress_ts
        ):
            return cls(
                kind=BridgeRunVerdictKind.PROVIDER_PAUSED,
                provider_reset_until=getattr(report, "provider_reset_until", ""),
            )
        automation_failures = int(
            _coerce_float(getattr(report, "automation_verified_failures", 0))
        )
        automation_successes = int(
            _coerce_float(getattr(report, "automation_verified_successes", 0))
        )
        if automation_failures and automation_failures >= automation_successes:
            return cls(kind=BridgeRunVerdictKind.AUTOMATION_UNVERIFIED)
        manual_fuel_calls = int(
            _coerce_float(getattr(report, "manual_fuel_transfer_tool_calls", 0))
        )
        fuel_automation_calls = int(
            _coerce_float(getattr(report, "fuel_automation_tool_calls", 0))
        )
        if manual_fuel_calls >= 2 and manual_fuel_calls > fuel_automation_calls:
            material_automation_calls = int(
                _coerce_float(getattr(report, "material_flow_automation_tool_calls", 0))
            )
            latest_objective = str(getattr(report, "latest_objective", "") or "").lower()
            if material_automation_calls and any(
                marker in latest_objective
                for marker in ("boiler", "coal", "fuel", "power", "steam")
            ):
                return cls(kind=BridgeRunVerdictKind.FUEL_ROUTE_IN_PROGRESS)
            return cls(kind=BridgeRunVerdictKind.FUEL_MANUAL_HEAVY)
        manual_science_calls = int(
            _coerce_float(getattr(report, "manual_science_transfer_tool_calls", 0))
        )
        science_automation_calls = int(
            _coerce_float(getattr(report, "science_automation_tool_calls", 0))
        )
        if manual_science_calls >= 2 and manual_science_calls > science_automation_calls:
            return cls(kind=BridgeRunVerdictKind.SCIENCE_MANUAL_HEAVY)
        manual_material_calls = int(
            _coerce_float(getattr(report, "manual_material_transfer_tool_calls", 0))
        )
        material_automation_calls = int(
            _coerce_float(getattr(report, "material_flow_automation_tool_calls", 0))
        )
        if manual_material_calls >= 2 and manual_material_calls > material_automation_calls:
            return cls(kind=BridgeRunVerdictKind.MATERIAL_MANUAL_HEAVY)
        manual_component_calls = int(
            _coerce_float(getattr(report, "manual_component_craft_tool_calls", 0))
        )
        component_automation_calls = int(
            _coerce_float(getattr(report, "component_automation_tool_calls", 0))
        )
        if manual_component_calls >= 2 and manual_component_calls > component_automation_calls:
            return cls(kind=BridgeRunVerdictKind.COMPONENT_MANUAL_HEAVY)
        manual_calls = int(
            _coerce_float(getattr(report, "manual_transfer_tool_calls", 0))
        )
        automation_calls = int(
            _coerce_float(getattr(report, "automation_tool_calls", 0))
        )
        if manual_calls >= 3 and manual_calls > automation_calls:
            return cls(kind=BridgeRunVerdictKind.MANUAL_HEAVY)
        if getattr(report, "recent_progress_events", 0):
            return cls(kind=BridgeRunVerdictKind.RECENT_PROGRESS)
        rejections = getattr(report, "top_gameplay_rejections", []) or []
        if getattr(report, "watchdog_aborts", 0) or any(
            _coerce_float(count) >= 3 for _, count in rejections
        ):
            return cls(kind=BridgeRunVerdictKind.REPEATED_FAILURES)
        if getattr(report, "context_resets", 0):
            return cls(kind=BridgeRunVerdictKind.CONTEXT_RESETS)
        return cls(kind=BridgeRunVerdictKind.NO_RECENT_PROGRESS)

    @property
    def message(self) -> str:
        if self.kind == BridgeRunVerdictKind.NO_RECORDS:
            return "operator attention needed: no bridge records found"
        if self.kind == BridgeRunVerdictKind.PROVIDER_PAUSED:
            reset = f" until {self.provider_reset_until}" if self.provider_reset_until else ""
            return f"provider paused{reset}; safe to leave running"
        if self.kind == BridgeRunVerdictKind.AUTOMATION_UNVERIFIED:
            return "operator attention needed: automation controllers are failing verification"
        if self.kind == BridgeRunVerdictKind.FUEL_ROUTE_IN_PROGRESS:
            return "safe to keep running: fuel route automation is in progress but not connected yet"
        if self.kind == BridgeRunVerdictKind.FUEL_MANUAL_HEAVY:
            return "operator attention useful: fuel is being babysat manually instead of routed with build_fuel_supply"
        if self.kind == BridgeRunVerdictKind.SCIENCE_MANUAL_HEAVY:
            return "operator attention useful: science is being hand-crafted or hand-fed instead of routed through automation controllers"
        if self.kind == BridgeRunVerdictKind.MATERIAL_MANUAL_HEAVY:
            return "operator attention useful: ore or plates are being hand-carried instead of routed through smelting/material-flow controllers"
        if self.kind == BridgeRunVerdictKind.COMPONENT_MANUAL_HEAVY:
            return "operator attention useful: science ingredients are being hand-crafted instead of produced by assembler cells"
        if self.kind == BridgeRunVerdictKind.MANUAL_HEAVY:
            return "operator attention useful: manual transfer calls exceed automation controller calls"
        if self.kind == BridgeRunVerdictKind.RECENT_PROGRESS:
            return "safe to keep running: recent progress detected"
        if self.kind == BridgeRunVerdictKind.REPEATED_FAILURES:
            return "operator attention needed: repeated gameplay failures without recent progress"
        if self.kind == BridgeRunVerdictKind.CONTEXT_RESETS:
            return "operator attention useful: context resets occurred and no recent progress was detected"
        return "operator attention useful: no recent progress detected"


class AgentSessionState(BridgeModel):
    """Persisted Claude SDK session id for one bridge agent."""

    session_id: str

    @field_validator("session_id", mode="before")
    @classmethod
    def _coerce_session_id(cls, value: Any) -> str:
        if not isinstance(value, str) or not value.strip():
            raise ValueError("expected non-empty string")
        return value.strip()

    @classmethod
    def from_file_text(cls, value: str) -> "AgentSessionState":
        if isinstance(value, cls):
            return value
        data = _json_object_from_text(value, "session")
        try:
            return cls.model_validate(data)
        except ValidationError as exc:
            raise BridgeValidationError("session_id", "expected non-empty string") from exc

    def to_json_line(self) -> str:
        return self.model_dump_json() + "\n"


class AgentSessionIndex(BridgeModel):
    """Backward-compatible shared session file used by older bridge builds."""

    sessions: dict[str, str] = Field(default_factory=dict)

    @classmethod
    def from_file_text(cls, value: str) -> "AgentSessionIndex":
        data = _json_object_from_text(value, "sessions")
        sessions = {
            str(agent): session.strip()
            for agent, session in data.items()
            if isinstance(agent, str)
            and isinstance(session, str)
            and session.strip()
        }
        return cls(sessions=sessions)

    def get(self, agent_name: str) -> str | None:
        return self.sessions.get(str(agent_name))

    def without(self, agent_name: str) -> "AgentSessionIndex":
        sessions = dict(self.sessions)
        sessions.pop(str(agent_name), None)
        return AgentSessionIndex(sessions=sessions)

    def to_legacy_json_line(self) -> str:
        return json.dumps(self.sessions) + "\n"


class HiddenTrailerBlock(BridgeModel):
    """Typed wrapper for hidden self-emitted trailer blocks."""

    tag: str
    body: str = ""

    @field_validator("tag", mode="before")
    @classmethod
    def _coerce_tag(cls, value: Any) -> str:
        tag = str(value or "").strip().lower()
        if not re.fullmatch(r"[a-z][a-z0-9_-]*", tag):
            raise ValueError("expected trailer tag")
        return tag

    @field_validator("body", mode="before")
    @classmethod
    def _coerce_body(cls, value: Any) -> str:
        return str(value or "")

    @classmethod
    def first_from_text(cls, text: Any, tag: str) -> "HiddenTrailerBlock | None":
        blocks = cls.all_from_text(text, [tag])
        return blocks[0] if blocks else None

    @classmethod
    def all_from_text(
        cls,
        text: Any,
        tags: list[str] | tuple[str, ...] | set[str],
    ) -> list["HiddenTrailerBlock"]:
        if not isinstance(text, str):
            return []
        pattern = cls._pattern(tags)
        if pattern is None:
            return []
        blocks: list[HiddenTrailerBlock] = []
        for match in pattern.finditer(text):
            try:
                blocks.append(cls(tag=match.group("tag"), body=match.group("body")))
            except ValidationError:
                continue
        return blocks

    @classmethod
    def strip_from_text(
        cls,
        text: Any,
        tags: list[str] | tuple[str, ...] | set[str],
    ) -> str:
        if not isinstance(text, str):
            return ""
        pattern = cls._pattern(tags)
        if pattern is None or not pattern.search(text):
            return text
        stripped = pattern.sub("", text)
        stripped = re.sub(r"\n{3,}", "\n\n", stripped)
        return stripped.strip()

    @staticmethod
    def _pattern(
        tags: list[str] | tuple[str, ...] | set[str],
    ) -> re.Pattern[str] | None:
        normalized: list[str] = []
        for tag in tags:
            tag_text = str(tag or "").strip().lower()
            if re.fullmatch(r"[a-z][a-z0-9_-]*", tag_text):
                normalized.append(tag_text)
        if not normalized:
            return None
        alternates = "|".join(re.escape(tag) for tag in sorted(set(normalized)))
        return re.compile(
            rf"<(?P<tag>{alternates})>(?P<body>.*?)</(?P=tag)>",
            re.DOTALL | re.IGNORECASE,
        )


def _coerce_float(value: Any) -> float:
    try:
        return float(value)
    except (TypeError, ValueError):
        return 0.0


def _utc_timestamp_s() -> str:
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%S")


class TelemetryEventType(str, Enum):
    EVENT = "event"
    CHAT = "chat"
    TOOL_CALL = "tool_call"
    TOOL_RESULT = "tool_result"
    ERROR = "error"
    STATUS = "status"
    COMPUTE_COST = "compute_cost"


def telemetry_event_type(value: Any) -> TelemetryEventType:
    if isinstance(value, TelemetryEventType):
        return value
    if isinstance(value, str):
        normalized = value.strip().lower().replace("-", "_")
        for event_type in TelemetryEventType:
            if normalized == event_type.value:
                return event_type
    return TelemetryEventType.EVENT


class TelemetryToolCallData(BridgeModel):
    """Typed telemetry payload for a tool-call event."""

    tool: str = ""
    input: Any = Field(default_factory=dict)

    @field_validator("tool", mode="before")
    @classmethod
    def _coerce_tool(cls, value: Any) -> str:
        return str(value or "")

    def to_dict(self) -> dict[str, Any]:
        return {"tool": self.tool, "input": self.input}


class TelemetryStatusData(RemotePayloadModel):
    """Typed telemetry status payload with forward-compatible fields."""

    @classmethod
    def coerce(cls, value: Any) -> "TelemetryStatusData":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return cls()
        return cls.model_validate(value)

    def to_dict(self) -> dict[str, Any]:
        return dict(self.model_extra or {})


class TelemetrySerializableValue(BridgeModel):
    """JSON-safe value normalizer for telemetry payload fragments."""

    value: Any = None

    @field_validator("value", mode="before")
    @classmethod
    def _coerce_value(cls, value: Any) -> Any:
        if hasattr(value, "to_dict") and callable(value.to_dict):
            try:
                value = value.to_dict()
            except (TypeError, ValueError):
                value = str(value)
        if isinstance(value, dict):
            return {
                str(key): cls(value=item).value
                for key, item in value.items()
            }
        if isinstance(value, (list, tuple, set)):
            return [cls(value=item).value for item in value]
        if value is None or isinstance(value, (str, int, float, bool)):
            return value
        return str(value)

    @classmethod
    def normalize(cls, value: Any) -> Any:
        return cls(value=value).value


class TelemetryEvent(RemotePayloadModel):
    """Typed event emitted to local SSE clients and the optional relay."""

    type: TelemetryEventType = TelemetryEventType.EVENT
    data: dict[str, Any] = Field(default_factory=dict)
    agent: str = ""
    tick: int | None = None
    timestamp: str = Field(default_factory=_utc_timestamp_s)

    @field_validator("type", mode="before")
    @classmethod
    def _coerce_type(cls, value: Any) -> TelemetryEventType:
        return telemetry_event_type(value)

    @field_validator("agent", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value).strip() if value is not None else ""

    @field_validator("timestamp", mode="before")
    @classmethod
    def _coerce_timestamp(cls, value: Any) -> str:
        text = str(value).strip() if value is not None else ""
        return text or _utc_timestamp_s()

    @field_validator("data", mode="before")
    @classmethod
    def _coerce_data(cls, value: Any) -> dict[str, Any]:
        return dict(value) if isinstance(value, dict) else {}

    @field_validator("tick", mode="before")
    @classmethod
    def _coerce_tick(cls, value: Any) -> int | None:
        if value is None or value == "":
            return None
        try:
            return int(value)
        except (TypeError, ValueError):
            return None

    @classmethod
    def coerce(cls, value: Any) -> "TelemetryEvent":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return cls(type=TelemetryEventType.EVENT, data={"value": value})
        return cls.model_validate(value)

    @classmethod
    def chat(
        cls,
        role: Any,
        message: Any,
        *,
        agent: Any = "BORE-01",
        tick: Any = None,
        sections: Any = None,
    ) -> "TelemetryEvent":
        data: dict[str, Any] = {"role": str(role), "message": str(message)}
        if sections:
            data["sections"] = TelemetrySerializableValue.normalize(sections)
        return cls(type=TelemetryEventType.CHAT, data=data, agent=agent, tick=tick)

    @classmethod
    def tool_call(
        cls,
        tool: Any,
        input_data: Any,
        *,
        agent: Any = "BORE-01",
        tick: Any = None,
    ) -> "TelemetryEvent":
        data = TelemetryToolCallData(tool=tool, input=input_data)
        return cls(
            type=TelemetryEventType.TOOL_CALL,
            data=data.to_dict(),
            agent=agent,
            tick=tick,
        )

    @classmethod
    def tool_result(
        cls,
        tool: Any,
        output: Any,
        *,
        agent: Any = "BORE-01",
        tick: Any = None,
        output_limit: int = 200,
    ) -> "TelemetryEvent":
        return cls(
            type=TelemetryEventType.TOOL_RESULT,
            data={"tool": str(tool), "output": str(output)[:output_limit]},
            agent=agent,
            tick=tick,
        )

    @classmethod
    def error(
        cls,
        message: Any,
        *,
        agent: Any = "BORE-01",
        tick: Any = None,
    ) -> "TelemetryEvent":
        return cls(
            type=TelemetryEventType.ERROR,
            data={"message": str(message)},
            agent=agent,
            tick=tick,
        )

    @classmethod
    def status(
        cls,
        data: Any,
        *,
        agent: Any = "BORE-01",
        tick: Any = None,
    ) -> "TelemetryEvent":
        return cls(
            type=TelemetryEventType.STATUS,
            data=TelemetryStatusData.coerce(data).to_dict(),
            agent=agent,
            tick=tick,
        )

    @classmethod
    def compute_cost(
        cls,
        data: Any,
        *,
        agent: Any = "BORE-01",
        tick: Any = None,
    ) -> "TelemetryEvent":
        return cls(
            type=TelemetryEventType.COMPUTE_COST,
            data=data if isinstance(data, dict) else {},
            agent=agent,
            tick=tick,
        )

    def to_dict(self) -> dict[str, Any]:
        result = dict(self.model_extra or {})
        result.update({
            "type": self.type.value,
            "data": dict(self.data),
            "agent": self.agent,
            "tick": self.tick,
            "timestamp": self.timestamp,
        })
        return result

    def to_json_text(self) -> str:
        return json.dumps(self.to_dict(), separators=(",", ":"))


class TelemetryHealthStatus(BridgeModel):
    """Typed JSON payload for the local telemetry health endpoint."""

    status: str = "ok"
    clients: int = 0

    @field_validator("status", mode="before")
    @classmethod
    def _coerce_status(cls, value: Any) -> str:
        return str(value or "ok").strip() or "ok"

    @field_validator("clients", mode="before")
    @classmethod
    def _coerce_clients(cls, value: Any) -> int:
        try:
            return max(0, int(value))
        except (TypeError, ValueError):
            return 0

    @classmethod
    def ok(cls, clients: Any = 0) -> "TelemetryHealthStatus":
        return cls(status="ok", clients=clients)

    def to_json_bytes(self) -> bytes:
        return json.dumps(self.model_dump(), separators=(",", ":")).encode()


class TelemetrySseMessage(BridgeModel):
    """Typed SSE frame for a single telemetry event."""

    event: TelemetryEvent = Field(default_factory=TelemetryEvent)

    @field_validator("event", mode="before")
    @classmethod
    def _coerce_event(cls, value: Any) -> TelemetryEvent:
        return TelemetryEvent.coerce(value)

    @classmethod
    def coerce(cls, value: Any) -> "TelemetrySseMessage":
        if isinstance(value, cls):
            return value
        return cls(event=value)

    @property
    def data(self) -> str:
        return self.event.to_json_text()

    @property
    def frame(self) -> str:
        return f"data: {self.data}\n\n"

    def to_bytes(self) -> bytes:
        return self.frame.encode()


class TelemetryEventBatch(BridgeModel):
    """Typed HTTP relay batch payload for telemetry events."""

    events: list[TelemetryEvent] = Field(default_factory=list)

    @field_validator("events", mode="before")
    @classmethod
    def _coerce_events(cls, value: Any) -> list[TelemetryEvent]:
        if value is None:
            return []
        if isinstance(value, TelemetryEventBatch):
            return list(value.events)
        if isinstance(value, TelemetryEvent):
            return [value]
        if isinstance(value, Iterable) and not isinstance(value, (str, bytes, dict)):
            return [TelemetryEvent.coerce(item) for item in value]
        return [TelemetryEvent.coerce(value)]

    @classmethod
    def coerce(cls, value: Any) -> "TelemetryEventBatch":
        if isinstance(value, cls):
            return value
        return cls(events=value)

    def to_list(self) -> list[dict[str, Any]]:
        return [event.to_dict() for event in self.events]

    def to_json_bytes(self) -> bytes:
        return json.dumps(self.to_list(), separators=(",", ":")).encode()


class EvalProductionSnapshot(BridgeModel):
    produced: dict[str, float] = Field(default_factory=dict)
    rate_per_min: dict[str, float] = Field(default_factory=dict)

    @staticmethod
    def as_float_map(value: Any) -> dict[str, float]:
        if not isinstance(value, dict):
            return {}
        result: dict[str, float] = {}
        for key, raw in value.items():
            amount = _coerce_float(raw)
            if amount:
                result[str(key)] = amount
        return result

    @classmethod
    def coerce(cls, value: Any) -> "EvalProductionSnapshot":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return cls()
        return cls(
            produced=cls.as_float_map(value.get("produced")),
            rate_per_min=cls.as_float_map(value.get("rate_per_min")),
        )

    @classmethod
    def from_rcon_text(cls, value: Any) -> "EvalProductionSnapshot":
        try:
            payload = RconJsonResponse.parse_value(value)
        except BridgeValidationError:
            return cls()
        return cls.coerce(payload)

    def to_dict(self) -> dict[str, dict[str, float]]:
        return {
            "produced": dict(self.produced),
            "rate_per_min": dict(self.rate_per_min),
        }

    def any_produced(self, items: tuple[str, ...]) -> bool:
        return any(_coerce_float(self.produced.get(item)) > 0 for item in items)

    def rate_at_least(self, item: str, threshold: float) -> bool:
        return _coerce_float(self.rate_per_min.get(item)) >= _coerce_float(threshold)

    def score_source(self) -> dict[str, float]:
        return dict(self.rate_per_min or self.produced)

    def production_score(self, values: dict[str, float]) -> float:
        source = self.score_source()
        return sum(_coerce_float(source.get(item)) * value for item, value in values.items())


class EvalMilestoneKind(str, Enum):
    ANY_PRODUCED = "any_produced"
    RATE_AT_LEAST = "rate_at_least"


class EvalMilestoneSpec(BridgeModel):
    """Typed milestone rule for the production eval harness."""

    name: str
    kind: EvalMilestoneKind
    items: tuple[str, ...] = ()
    item: str = ""
    threshold: float = 0.0

    @field_validator("name", "item", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "").strip()

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> EvalMilestoneKind:
        if isinstance(value, EvalMilestoneKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in EvalMilestoneKind:
                if normalized == kind.value:
                    return kind
        return EvalMilestoneKind.ANY_PRODUCED

    @field_validator("items", mode="before")
    @classmethod
    def _coerce_items(cls, value: Any) -> tuple[str, ...]:
        if isinstance(value, str):
            items = [value]
        elif isinstance(value, (list, tuple)):
            items = list(value)
        else:
            items = []
        return tuple(str(item).strip() for item in items if str(item).strip())

    @field_validator("threshold", mode="before")
    @classmethod
    def _coerce_threshold(cls, value: Any) -> float:
        return _coerce_float(value)

    @classmethod
    def any_produced(cls, name: str, items: tuple[str, ...]) -> "EvalMilestoneSpec":
        return cls(name=name, kind=EvalMilestoneKind.ANY_PRODUCED, items=items)

    @classmethod
    def rate_at_least(cls, name: str, item: str, threshold: float) -> "EvalMilestoneSpec":
        return cls(
            name=name,
            kind=EvalMilestoneKind.RATE_AT_LEAST,
            item=item,
            threshold=threshold,
        )

    def reached(self, snapshot: Any) -> bool:
        typed_snapshot = EvalProductionSnapshot.coerce(snapshot)
        if self.kind == EvalMilestoneKind.RATE_AT_LEAST:
            return typed_snapshot.rate_at_least(self.item, self.threshold)
        return typed_snapshot.any_produced(self.items)


class EvalResult(BridgeModel):
    production_score: float = 0.0
    milestones: dict[str, bool] = Field(default_factory=dict)
    milestones_reached: int = 0

    @classmethod
    def create(cls, *, production_score: Any, milestones: dict[str, Any]) -> "EvalResult":
        normalized = {
            str(name): bool(reached)
            for name, reached in (milestones if isinstance(milestones, dict) else {}).items()
        }
        return cls(
            production_score=_coerce_float(production_score),
            milestones=normalized,
            milestones_reached=sum(1 for reached in normalized.values() if reached),
        )

    @classmethod
    def coerce(
        cls,
        value: Any,
        *,
        milestone_names: list[str] | tuple[str, ...] = (),
    ) -> "EvalResult":
        if isinstance(value, cls):
            return value
        data = value if isinstance(value, dict) else {}
        raw_milestones = data.get("milestones", {})
        milestones = {
            str(name): bool(reached)
            for name, reached in (
                raw_milestones if isinstance(raw_milestones, dict) else {}
            ).items()
        }
        for name in milestone_names:
            milestones.setdefault(str(name), False)
        return cls.create(
            production_score=data.get("production_score", 0.0),
            milestones=milestones,
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "production_score": self.production_score,
            "milestones": dict(self.milestones),
            "milestones_reached": self.milestones_reached,
        }

    def is_better_than(self, other: "EvalResult | None") -> bool:
        return other is None or self.production_score > other.production_score


class PowerGeneratorSummary(RemotePayloadModel):
    name: str = "unknown"
    count: Any = "?"

    @field_validator("name", mode="before")
    @classmethod
    def _coerce_name(cls, value: Any) -> str:
        return str(value).strip() if value is not None and str(value).strip() else "unknown"

    def compact(self) -> str:
        return f"{self.name}={self.count}"


class PowerConsumerSummary(RemotePayloadModel):
    working: int = 0
    low_power: int = 0
    no_power: int = 0
    total: int = 0

    def compact(self) -> str:
        return (
            f"{self.working} working/"
            f"{self.low_power} low/"
            f"{self.no_power} none/"
            f"{self.total} total"
        )


class PowerGeneratorSummaryCollection(BridgeModel):
    """Typed collection of generator summaries from remote power payloads."""

    items: tuple[Any, ...] = ()

    @field_validator("items", mode="before")
    @classmethod
    def _coerce_items(cls, value: Any) -> tuple[Any, ...]:
        if value is None or isinstance(value, (str, bytes, dict, BaseModel)):
            return ()
        if not isinstance(value, Iterable):
            return ()
        return tuple(
            item
            for item in value
            if isinstance(item, dict | PowerGeneratorSummary)
        )

    @classmethod
    def from_value(cls, value: Any) -> "PowerGeneratorSummaryCollection":
        if isinstance(value, cls):
            return value
        return cls(items=value)

    def to_list(self) -> list[Any]:
        return list(self.items)


class PowerStatus(RemotePayloadModel):
    error: Any = None
    network_id: Any = "unknown"
    pole_count: Any = "unknown"
    generators: list[PowerGeneratorSummary] = Field(default_factory=list)
    consumers: PowerConsumerSummary = Field(default_factory=PowerConsumerSummary)
    production_kw: Any = "unknown"
    consumption_kw: Any = "unknown"
    satisfaction: Any = "unknown"

    @field_validator("generators", mode="before")
    @classmethod
    def _coerce_generators(cls, value: Any) -> list[Any]:
        return PowerGeneratorSummaryCollection.from_value(value).to_list()

    @field_validator("consumers", mode="before")
    @classmethod
    def _coerce_consumers(cls, value: Any) -> Any:
        return value if isinstance(value, dict | PowerConsumerSummary) else {}

    @classmethod
    def from_payload(cls, value: Any) -> "PowerStatus | None":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return None
        try:
            return cls.model_validate(value)
        except ValidationError:
            return None

    @classmethod
    def compact_from_payload(cls, value: Any, *, fallback_to_text: bool = False) -> str:
        status = cls.from_payload(value)
        if status:
            return _single_line_text(status.compact())
        return _single_line_text(str(value)) if fallback_to_text else ""

    def compact(self) -> str:
        if self.error:
            return f"unavailable: {self.error}"
        generator_summary = ", ".join(
            generator.compact() for generator in self.generators
        ) or "none"
        return "; ".join([
            f"network={self.network_id}",
            f"poles={self.pole_count}",
            f"generators={generator_summary}",
            f"consumers={self.consumers.compact()}",
            f"production_kw={self.production_kw}",
            f"consumption_kw={self.consumption_kw}",
            f"satisfaction={self.satisfaction}",
        ])


class SteamPowerIssue(RemotePayloadModel):
    type: str = "unknown"

    @field_validator("type", mode="before")
    @classmethod
    def _coerce_type(cls, value: Any) -> str:
        return str(value).strip() if value is not None and str(value).strip() else "unknown"


class SteamPowerIssueCollection(BridgeModel):
    """Typed collection of steam diagnostic issues from remote payloads."""

    items: tuple[Any, ...] = ()

    @field_validator("items", mode="before")
    @classmethod
    def _coerce_items(cls, value: Any) -> tuple[Any, ...]:
        if value is None or isinstance(value, (str, bytes, dict, BaseModel)):
            return ()
        if not isinstance(value, Iterable):
            return ()
        return tuple(
            item
            for item in value
            if isinstance(item, dict | SteamPowerIssue)
        )

    @classmethod
    def from_value(cls, value: Any) -> "SteamPowerIssueCollection":
        if isinstance(value, cls):
            return value
        return cls(items=value)

    def to_list(self) -> list[Any]:
        return list(self.items)


class SteamPowerSummary(RemotePayloadModel):
    issue_count: int | None = None
    critical_issues: Any = None


class SteamPowerDiagnostic(RemotePayloadModel):
    summary: SteamPowerSummary = Field(default_factory=SteamPowerSummary)
    issues: list[SteamPowerIssue] = Field(default_factory=list)
    status: str = "unknown"
    next_action: str = ""

    @field_validator("summary", mode="before")
    @classmethod
    def _coerce_summary(cls, value: Any) -> Any:
        return value if isinstance(value, dict | SteamPowerSummary) else {}

    @field_validator("issues", mode="before")
    @classmethod
    def _coerce_issues(cls, value: Any) -> list[Any]:
        return SteamPowerIssueCollection.from_value(value).to_list()

    @field_validator("next_action", "status", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value).strip() if value is not None else ""

    @classmethod
    def from_payload(cls, value: Any) -> "SteamPowerDiagnostic | None":
        if isinstance(value, cls):
            return value if value.is_meaningful() else None
        if not isinstance(value, dict):
            return None
        source = value.get("existing_plant")
        if not isinstance(source, dict):
            source = value
        merged = dict(source)
        if not merged.get("status") and value.get("status"):
            merged["status"] = value.get("status")
        if not merged.get("next_action") and value.get("next_action"):
            merged["next_action"] = value.get("next_action")
        try:
            diagnostic = cls.model_validate(merged)
        except ValidationError:
            return None
        if not diagnostic.is_meaningful():
            return None
        return diagnostic

    def is_meaningful(self) -> bool:
        return bool(
            self.summary.issue_count is not None
            or self.summary.critical_issues is not None
            or self.issues
            or self.next_action
            or self.status != "unknown"
        )

    def compact(self) -> str:
        issue_types = [issue.type for issue in self.issues]
        issue_count = self.summary.issue_count
        if issue_count is None:
            issue_count = len(issue_types)
        critical_issues = self.summary.critical_issues
        if critical_issues is None:
            critical_issues = "unknown"
        parts = [
            f"steam_power status={self.status or 'unknown'}",
            f"issues={issue_count}",
            f"critical={critical_issues}",
        ]
        if issue_types:
            parts.append(f"types={', '.join(issue_types[:3])}")
        if self.next_action:
            parts.append(f"next={self.next_action}")
        return "; ".join(parts)


class BridgeLogProgressEvidence(BridgeModel):
    """Typed progress signal extracted from an operator log line."""

    kind: BridgeLogProgressKind = BridgeLogProgressKind.NONE
    reason: str = ""

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> BridgeLogProgressKind:
        if isinstance(value, BridgeLogProgressKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in BridgeLogProgressKind:
                if normalized == kind.value:
                    return kind
        return BridgeLogProgressKind.NONE

    @field_validator("reason", mode="before")
    @classmethod
    def _coerce_reason(cls, value: Any) -> str:
        return str(value or "").strip()

    @property
    def is_progress(self) -> bool:
        return self.kind not in {
            BridgeLogProgressKind.NONE,
            BridgeLogProgressKind.PLAN_WAITING,
        }

    @classmethod
    def none(cls) -> "BridgeLogProgressEvidence":
        return cls(kind=BridgeLogProgressKind.NONE, reason="")

    @classmethod
    def from_text(
        cls,
        value: Any,
        *,
        progress_entries: list[str] | None = None,
    ) -> "BridgeLogProgressEvidence":
        text = str(value or "")
        lower = text.lower()
        if progress_entries:
            if cls._entries_are_waiting_for_execution(progress_entries):
                return cls(
                    kind=BridgeLogProgressKind.PLAN_WAITING,
                    reason="ledger plan waiting for execution",
                )
            return cls(
                kind=BridgeLogProgressKind.LEDGER_PROGRESS,
                reason="ledger progress entry",
            )
        if "research completed" in lower:
            return cls(
                kind=BridgeLogProgressKind.RESEARCH_COMPLETED,
                reason="research completion text",
            )
        if "verified working" in lower:
            return cls(
                kind=BridgeLogProgressKind.VERIFIED_WORKING,
                reason="production verification text",
            )
        if "automation milestone" in lower:
            return cls(
                kind=BridgeLogProgressKind.AUTOMATION_MILESTONE,
                reason="automation milestone text",
            )
        if "power grid operational" in lower:
            return cls(
                kind=BridgeLogProgressKind.POWER_GRID,
                reason="power grid operational text",
            )
        return cls.none()

    @staticmethod
    def _entries_are_waiting_for_execution(entries: list[str]) -> bool:
        normalized_entries = [
            re.sub(r"\s+", " ", str(entry or "").lower()).strip()
            for entry in entries
        ]
        normalized_entries = [entry for entry in normalized_entries if entry]
        if not normalized_entries:
            return False
        return all(
            BridgeLogProgressEvidence._entry_is_waiting_for_execution(entry)
            for entry in normalized_entries
        )

    @staticmethod
    def _entry_is_waiting_for_execution(entry: str) -> bool:
        waiting_markers = (
            "awaiting execution",
            "ready for execution",
            "queued for execution",
            "pending mutation tick",
            "execution pending",
            "pending mutation",
        )
        no_change_markers = (
            "no change",
            "no state drift",
            "state unchanged",
            "state stable",
            "plan validated",
            "plan fully validated",
            "situation_report confirmed stable",
        )
        return (
            any(marker in entry for marker in waiting_markers)
            and any(marker in entry for marker in no_change_markers)
        )


class BridgeLogPowerEvidence(BridgeModel):
    """Typed power-related signal extracted from an operator log line."""

    kind: BridgeLogPowerKind = BridgeLogPowerKind.NONE
    reason: str = ""
    summary: str = ""

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> BridgeLogPowerKind:
        if isinstance(value, BridgeLogPowerKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in BridgeLogPowerKind:
                if normalized == kind.value:
                    return kind
        return BridgeLogPowerKind.NONE

    @field_validator("reason", "summary", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "").strip()

    @property
    def is_power(self) -> bool:
        return self.kind != BridgeLogPowerKind.NONE

    @classmethod
    def none(cls) -> "BridgeLogPowerEvidence":
        return cls(kind=BridgeLogPowerKind.NONE, reason="", summary="")

    @classmethod
    def from_text(cls, value: Any) -> "BridgeLogPowerEvidence":
        text = str(value or "")
        lower = text.lower()
        if cls._is_ignored_source(lower) or cls._is_context_noise(lower):
            return cls.none()
        if not cls._mentions_power_context(lower):
            return cls.none()
        tool_result = BridgeLogToolResultLine.from_line(text)
        if tool_result.has_tool_result_payload:
            payload = McpTextPayload.from_text(tool_result.suffix).value
            summary = cls.summary_from_payload(payload)
            if summary:
                return cls(
                    kind=BridgeLogPowerKind.DIAGNOSTIC_PAYLOAD,
                    reason="tool_result power diagnostic payload",
                    summary=summary,
                )
        if text.startswith("text:") and cls._is_concise_status_text(lower):
            return cls(
                kind=BridgeLogPowerKind.CONCISE_STATUS_TEXT,
                reason="concise power status text",
                summary=_single_line_text(text),
            )
        return cls(
            kind=BridgeLogPowerKind.POWER_RELATED_TEXT,
            reason="power-related bridge text",
            summary="",
        )

    @classmethod
    def summary_from_payload(cls, payload: Any) -> str:
        diagnostic = SteamPowerDiagnostic.from_payload(payload)
        if diagnostic:
            return _single_line_text(diagnostic.compact())
        return PowerStatus.compact_from_payload(payload)

    @staticmethod
    def _is_ignored_source(lower: str) -> bool:
        return (
            lower.startswith("thinking:")
            or lower.startswith("autonomy ->")
            or lower.startswith("reply:")
            or lower.startswith("tool:")
        )

    @staticmethod
    def _is_context_noise(lower: str) -> bool:
        return (
            "continuity ledger:" in lower
            or "\nplan:" in lower
            or "\nprogress:" in lower
            or "blocked non-read-only tool" in lower
            or "planner/reflection turn" in lower
            or "this turn may only use read-only diagnostics" in lower
        )

    @staticmethod
    def _mentions_power_context(lower: str) -> bool:
        return (
            "power" in lower
            and (
                "operational" in lower
                or "no_power" in lower
                or "steam" in lower
                or "boiler" in lower
                or "electric pole" in lower
            )
        )

    @staticmethod
    def _is_concise_status_text(lower: str) -> bool:
        return (
            "power grid operational" in lower
            or "no_power" in lower
            or " no power" in lower
            or "boiler no-fuel" in lower
            or "boiler_no_fuel" in lower
        )


class BridgeLogRuntimeEvidence(BridgeModel):
    """Typed runtime/reporting signals extracted from an operator log line."""

    kinds: frozenset[BridgeLogRuntimeKind] = Field(default_factory=frozenset)
    reasons: tuple[str, ...] = ()
    provider_reset_until: str = ""

    @field_validator("kinds", mode="before")
    @classmethod
    def _coerce_kinds(cls, value: Any) -> frozenset[BridgeLogRuntimeKind]:
        if isinstance(value, BridgeLogRuntimeKind):
            return frozenset({value})
        if isinstance(value, str):
            values = [value]
        elif isinstance(value, Iterable):
            values = list(value)
        else:
            values = []
        result: set[BridgeLogRuntimeKind] = set()
        for item in values:
            if isinstance(item, BridgeLogRuntimeKind):
                result.add(item)
                continue
            if isinstance(item, str):
                normalized = item.strip().lower().replace("-", "_")
                for kind in BridgeLogRuntimeKind:
                    if normalized == kind.value:
                        result.add(kind)
                        break
        return frozenset(result)

    @field_validator("provider_reset_until", mode="before")
    @classmethod
    def _coerce_provider_reset_until(cls, value: Any) -> str:
        return str(value or "").strip()

    @field_validator("reasons", mode="before")
    @classmethod
    def _coerce_reasons(cls, value: Any) -> tuple[str, ...]:
        if isinstance(value, str):
            values = [value]
        elif isinstance(value, Iterable):
            values = list(value)
        else:
            values = []
        return tuple(str(item) for item in values if isinstance(item, str))

    @classmethod
    def from_text(cls, value: Any) -> "BridgeLogRuntimeEvidence":
        text = str(value or "")
        lower = text.lower()
        kinds: set[BridgeLogRuntimeKind] = set()
        reasons: list[str] = []
        if "spawning claude sdk" in lower:
            kinds.add(BridgeLogRuntimeKind.SDK_SPAWN)
            reasons.append("sdk spawn log")
        if text.startswith("done: "):
            kinds.add(BridgeLogRuntimeKind.SDK_DONE)
            reasons.append("sdk done log")
        if (
            "provider usage limit active" in lower
            or "paused by provider usage limit" in lower
        ):
            kinds.add(BridgeLogRuntimeKind.PROVIDER_PAUSE)
            reasons.append("provider usage limit pause")
        if "context window" in lower and "cleared session" in lower:
            kinds.add(BridgeLogRuntimeKind.CONTEXT_RESET)
            reasons.append("context window session reset")
        if (
            "watchdog aborted stuck tick" in lower
            or "watchdog_abort:" in lower
        ):
            kinds.add(BridgeLogRuntimeKind.WATCHDOG_ABORT)
            reasons.append("watchdog abort log")
        if "research completed" in lower:
            kinds.add(BridgeLogRuntimeKind.RESEARCH_COMPLETED)
            reasons.append("research completed log")
        return cls(
            kinds=frozenset(kinds),
            reasons=tuple(reasons),
            provider_reset_until=cls.provider_reset_until_from_text(text),
        )

    def has(self, kind: BridgeLogRuntimeKind) -> bool:
        return kind in self.kinds

    @staticmethod
    def provider_reset_until_from_text(text: Any) -> str:
        reset = _BRIDGE_LOG_RESET_UNTIL_RE.search(str(text or ""))
        return reset.group(1) if reset else ""


class BridgeLogToolResultMarker(BridgeModel):
    """Typed marker match inside a bridge tool-result log line."""

    line: str = ""
    marker: str = ""
    suffix: str = ""
    matched: bool = False

    @field_validator("line", "marker", "suffix", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "").strip()

    @classmethod
    def from_line(cls, value: Any, marker: Any) -> "BridgeLogToolResultMarker":
        line = str(value or "").strip()
        marker_text = str(marker or "").strip()
        split = TextMarkerSplit.from_text(line, marker_text, strip_outer=True)
        if not split.matched:
            return cls(line=line, marker=marker_text)
        return cls(line=line, marker=marker_text, suffix=split.after.strip(), matched=True)


class BridgeLogToolResultLine(BridgeModel):
    """Typed view of one log line containing a tool-result classification."""

    line: str = ""
    classification: ToolResultClassification | None = None
    suffix: str = ""
    tool_result: bool = False

    @field_validator("line", "suffix", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "").strip()

    @field_validator("classification", mode="before")
    @classmethod
    def _coerce_classification(
        cls,
        value: Any,
    ) -> ToolResultClassification | None:
        if value is None or isinstance(value, ToolResultClassification):
            return value
        try:
            return ToolResultClassification(str(value).strip().lower())
        except ValueError:
            return None

    @property
    def is_game_rejected(self) -> bool:
        return self.classification == ToolResultClassification.GAME_REJECTED

    @property
    def has_tool_result_payload(self) -> bool:
        return self.tool_result and bool(self.suffix)

    @classmethod
    def from_line(cls, value: Any) -> "BridgeLogToolResultLine":
        line = str(value or "").strip()
        plain = BridgeLogToolResultMarker.from_line(line, "tool_result:")
        if plain.matched and line.startswith("tool_result:"):
            return cls(line=line, suffix=plain.suffix, tool_result=True)
        for classification in ToolResultClassification:
            marker = f"{classification.value}:"
            match = BridgeLogToolResultMarker.from_line(line, marker)
            if not match.matched:
                continue
            return cls(
                line=line,
                classification=classification,
                suffix=match.suffix,
                tool_result="tool_result" in line,
            )
        return cls(line=line)


class BridgeLogGameplayEvidence(BridgeModel):
    """Typed gameplay rejection evidence extracted from an operator log line."""

    lines: list[str] = Field(default_factory=list)
    signatures: list[str] = Field(default_factory=list)

    @field_validator("lines", "signatures", mode="before")
    @classmethod
    def _coerce_items(cls, value: Any) -> list[str]:
        return _coerce_str_list(value)

    @classmethod
    def from_text(cls, value: Any) -> "BridgeLogGameplayEvidence":
        lines = cls.rejection_lines_from_text(value)
        return cls(
            lines=lines,
            signatures=cls.signatures_from_lines(lines),
        )

    @staticmethod
    def rejection_lines_from_text(value: Any) -> list[str]:
        lines: list[str] = []
        for raw_line in BridgeTextLines.from_text(value).non_empty:
            tool_result = BridgeLogToolResultLine.from_line(raw_line)
            if tool_result.is_game_rejected:
                lines.append(tool_result.line)
        return lines

    @staticmethod
    def signatures_from_lines(lines: list[str]) -> list[str]:
        signatures: list[str] = []
        for line in _coerce_str_list(lines):
            tool_result = BridgeLogToolResultLine.from_line(line)
            signature = GameRejectionPayload.from_payload(
                tool_result.suffix or tool_result.line,
            ).signature()
            if signature:
                signatures.append(signature)
        return signatures

    @classmethod
    def first_signature_from_text(cls, value: Any) -> str:
        evidence = cls.from_text(value)
        if evidence.signatures:
            return evidence.signatures[0]
        if evidence.lines:
            return ""
        return GameRejectionPayload.from_payload(value).signature()


class BridgeLogMessage(BridgeModel):
    """Typed summary of one bridge log message for report aggregation."""

    text: str = ""
    runtime_evidence: BridgeLogRuntimeEvidence = Field(
        default_factory=BridgeLogRuntimeEvidence,
    )
    sdk_spawn: bool = False
    sdk_done: bool = False
    provider_pause: bool = False
    provider_reset_until: str = ""
    context_reset: bool = False
    watchdog_abort: bool = False
    research_completed: bool = False
    research_counts: list[int] = Field(default_factory=list)
    entity_summary: str = ""
    objectives: list[str] = Field(default_factory=list)
    progress_entries: list[str] = Field(default_factory=list)
    progress_evidence: BridgeLogProgressEvidence = Field(
        default_factory=BridgeLogProgressEvidence.none,
    )
    progress_event: bool = False
    power_evidence: BridgeLogPowerEvidence = Field(
        default_factory=BridgeLogPowerEvidence.none,
    )
    power_summary: str = ""
    gameplay_evidence: BridgeLogGameplayEvidence = Field(
        default_factory=BridgeLogGameplayEvidence,
    )
    gameplay_rejection_signatures: list[str] = Field(default_factory=list)
    gameplay_rejection_lines: list[str] = Field(default_factory=list)

    @classmethod
    def from_record(cls, record: BridgeLogRecord | Any) -> "BridgeLogMessage":
        if isinstance(record, cls):
            return record
        if isinstance(record, BridgeLogRecord):
            text = record.message
        elif hasattr(record, "message"):
            text = str(getattr(record, "message", ""))
        else:
            text = str(record or "")
        return cls.from_text(text)

    @classmethod
    def from_text(cls, value: Any) -> "BridgeLogMessage":
        if isinstance(value, cls):
            return value
        text = str(value or "")
        runtime_evidence = BridgeLogRuntimeEvidence.from_text(text)
        progress_entries = cls._progress_entries(text)
        progress_evidence = BridgeLogProgressEvidence.from_text(
            text,
            progress_entries=progress_entries,
        )
        power_evidence = BridgeLogPowerEvidence.from_text(text)
        gameplay_evidence = BridgeLogGameplayEvidence.from_text(text)
        return cls(
            text=text,
            runtime_evidence=runtime_evidence,
            sdk_spawn=runtime_evidence.has(BridgeLogRuntimeKind.SDK_SPAWN),
            sdk_done=runtime_evidence.has(BridgeLogRuntimeKind.SDK_DONE),
            provider_pause=runtime_evidence.has(BridgeLogRuntimeKind.PROVIDER_PAUSE),
            provider_reset_until=runtime_evidence.provider_reset_until,
            context_reset=runtime_evidence.has(BridgeLogRuntimeKind.CONTEXT_RESET),
            watchdog_abort=runtime_evidence.has(BridgeLogRuntimeKind.WATCHDOG_ABORT),
            research_completed=runtime_evidence.has(
                BridgeLogRuntimeKind.RESEARCH_COMPLETED,
            ),
            research_counts=cls._research_counts(text),
            entity_summary=cls._entity_summary(text),
            objectives=cls._objectives(text),
            progress_entries=progress_entries,
            progress_evidence=progress_evidence,
            progress_event=progress_evidence.is_progress,
            power_evidence=power_evidence,
            power_summary=power_evidence.summary,
            gameplay_evidence=gameplay_evidence,
            gameplay_rejection_signatures=gameplay_evidence.signatures,
            gameplay_rejection_lines=gameplay_evidence.lines,
        )

    @staticmethod
    def _single_line(value: Any, *, limit: int = 240) -> str:
        return BridgeLogMessage.single_line(value, limit=limit)

    @staticmethod
    def single_line(value: Any, *, limit: int = 240) -> str:
        return _single_line_text(value, limit=limit)

    @staticmethod
    def _is_placeholder(value: str) -> bool:
        return value.strip().lower() in {
            "<goal>",
            "<updated goal>",
            "<what changed>",
            "<why the old plan was stale or complete>",
        }

    @classmethod
    def _objectives(cls, text: str) -> list[str]:
        objectives: list[str] = []
        for match in _BRIDGE_LOG_OBJECTIVE_RE.finditer(text):
            objective = (match.group(1) or match.group(2) or "").strip()
            if objective and not cls._is_placeholder(objective):
                objectives.append(objective)
        return objectives

    @classmethod
    def _progress_entries(cls, text: str) -> list[str]:
        entries: list[str] = []
        for match in _BRIDGE_LOG_PROGRESS_RE.finditer(text):
            progress = match.group(1).strip()
            if (
                progress
                and not cls._is_placeholder(progress)
            ):
                entries.append(progress)
        return entries

    @staticmethod
    def _research_counts(text: str) -> list[int]:
        counts: list[int] = []
        for match in _BRIDGE_LOG_RESEARCH_COUNT_RE.finditer(text):
            counts.append(int(match.group(1)))
        for match in _BRIDGE_LOG_RESEARCHED_COUNT_JSON_RE.finditer(text):
            counts.append(int(match.group(1)))
        return counts

    @staticmethod
    def _provider_reset_until(text: str) -> str:
        return BridgeLogRuntimeEvidence.provider_reset_until_from_text(text)

    @staticmethod
    def _entity_summary(text: str) -> str:
        live = LiveState.from_line(text)
        if live.entity_counts:
            return ", ".join(
                f"{name}={count}"
                for name, count in live.entity_counts.items()
            )
        entities = _BRIDGE_LOG_ENTITY_COUNTS_RE.search(text)
        return entities.group(1).strip() if entities else ""

    @classmethod
    def power_summary_from_payload(cls, payload: Any) -> str:
        return BridgeLogPowerEvidence.summary_from_payload(payload)

    @classmethod
    def first_gameplay_rejection_signature_from_text(cls, value: Any) -> str:
        return BridgeLogGameplayEvidence.first_signature_from_text(value)


def _mapping(value: Any, field_path: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise BridgeValidationError(field_path, "expected object")
    return value


def _required_str(data: dict[str, Any], key: str) -> str:
    value = data.get(key)
    if not isinstance(value, str) or not value:
        raise BridgeValidationError(key, "expected non-empty string")
    return value


def _optional_str(data: dict[str, Any], key: str) -> str | None:
    value = data.get(key)
    if value is None:
        return None
    if not isinstance(value, str):
        raise BridgeValidationError(key, "expected string")
    return value


def _required_any_str(data: dict[str, Any], key: str) -> str:
    value = data.get(key)
    if not isinstance(value, str):
        raise BridgeValidationError(key, "expected string")
    return value


def _optional_int(data: dict[str, Any], key: str) -> int | None:
    value = data.get(key)
    if value is None:
        return None
    if isinstance(value, bool) or not isinstance(value, int):
        raise BridgeValidationError(key, "expected integer")
    if value <= 0:
        raise BridgeValidationError(key, "expected positive integer")
    return value


def _optional_bool(data: dict[str, Any], key: str) -> bool | None:
    value = data.get(key)
    if value is None:
        return None
    if not isinstance(value, bool):
        raise BridgeValidationError(key, "expected boolean")
    return value


def _required_str_list(data: dict[str, Any], key: str) -> list[str]:
    value = data.get(key)
    if not isinstance(value, list):
        raise BridgeValidationError(key, "expected list of strings")
    result = []
    for index, item in enumerate(value):
        if not isinstance(item, str):
            raise BridgeValidationError(f"{key}[{index}]", "expected string")
        if item:
            result.append(item)
    return result


def _coerce_str_list(value: Any) -> list[str]:
    if not isinstance(value, list):
        return []
    return [str(item) for item in value if isinstance(item, str)]


def _coerce_str_or_list(value: Any, *, max_items: int | None = None) -> list[str]:
    if isinstance(value, str):
        result = [value.strip()] if value.strip() else []
    elif isinstance(value, Iterable) and not isinstance(value, (str, bytes, dict)):
        result = [item.strip() for item in value if isinstance(item, str) and item.strip()]
    else:
        result = []
    if max_items is not None:
        return result[:max_items]
    return result


def _optional_str_list(data: dict[str, Any], key: str) -> list[str]:
    value = data.get(key, [])
    if not isinstance(value, list):
        raise BridgeValidationError(key, "expected list of strings")
    result = []
    for index, item in enumerate(value):
        if not isinstance(item, str):
            raise BridgeValidationError(f"{key}[{index}]", "expected string")
        if item:
            result.append(item)
    return result


def _optional_response_format(data: dict[str, Any]) -> "AgentResponseFormat | None":
    value = data.get("response_format")
    if value is None:
        return None
    try:
        return AgentResponseFormat.coerce(value)
    except BridgeValidationError:
        raise
    except (TypeError, ValueError, ValidationError) as exc:
        raise BridgeValidationError("response_format", "expected object") from exc


def _optional_sdk_skills(data: dict[str, Any]) -> str | list[str] | None:
    value = data.get("sdk_skills")
    if value is None:
        return None
    if isinstance(value, str):
        return value
    if isinstance(value, list) and all(isinstance(item, str) for item in value):
        return list(value)
    raise BridgeValidationError("sdk_skills", "expected string or list of strings")


def _matches_tool_param_type(value: Any, expected_type: str) -> bool:
    if expected_type == TOOL_PARAM_STRING:
        return isinstance(value, str)
    if expected_type == TOOL_PARAM_NUMBER:
        return (isinstance(value, int | float) and not isinstance(value, bool))
    if expected_type == TOOL_PARAM_INTEGER:
        return isinstance(value, int) and not isinstance(value, bool)
    if expected_type == TOOL_PARAM_BOOLEAN:
        return isinstance(value, bool)
    if expected_type == TOOL_PARAM_OBJECT:
        return isinstance(value, dict)
    if expected_type == TOOL_PARAM_LIST:
        return isinstance(value, list)
    raise BridgeValidationError("<schema>", f"unknown parameter type {expected_type!r}")


def _coerce_tool_param_type_map(value: Any, field_path: str) -> dict[str, str]:
    if value is None:
        return {}
    if not isinstance(value, dict):
        raise BridgeValidationError(field_path, "expected object")
    result: dict[str, str] = {}
    for raw_key, raw_type in value.items():
        if not isinstance(raw_key, str) or not raw_key.strip():
            raise BridgeValidationError(field_path, "expected non-empty string keys")
        key = raw_key.strip()
        if not isinstance(raw_type, str):
            raise BridgeValidationError(f"{field_path}.{key}", "expected parameter type string")
        expected_type = raw_type.strip()
        if expected_type not in TOOL_PARAM_TYPES:
            raise BridgeValidationError(
                f"{field_path}.{key}",
                f"unknown parameter type {expected_type!r}",
            )
        result[key] = expected_type
    return result


class ToolParamSchema(BridgeModel):
    required: dict[str, str] = Field(default_factory=dict)
    optional: dict[str, str] = Field(default_factory=dict)

    @classmethod
    def from_mapping(cls, value: Any) -> "ToolParamSchema":
        if value is None:
            return cls()
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            raise BridgeValidationError("tool_param_schema", "expected object")
        return cls(
            required=_coerce_tool_param_type_map(value.get("required"), "required"),
            optional=_coerce_tool_param_type_map(value.get("optional"), "optional"),
        )

    def validate_request(self, request: "ToolCallRequest") -> None:
        request.validate_params(required=self.required, optional=self.optional)


class ToolParamSchemaRegistry(BridgeModel):
    """Typed registry of Factorio MCP parameter schemas."""

    schemas: dict[str, ToolParamSchema] = Field(default_factory=dict)

    @classmethod
    def from_mapping(cls, value: Any) -> "ToolParamSchemaRegistry":
        if isinstance(value, cls):
            return value
        if value is None:
            return cls()
        if not isinstance(value, dict):
            raise BridgeValidationError("tool_param_schema_registry", "expected object")
        schemas: dict[str, ToolParamSchema] = {}
        for raw_name, raw_schema in value.items():
            if not isinstance(raw_name, str) or not raw_name.strip():
                raise BridgeValidationError(
                    "tool_param_schema_registry",
                    "expected non-empty string keys",
                )
            name = raw_name.strip()
            try:
                schemas[name] = ToolParamSchema.from_mapping(raw_schema)
            except BridgeValidationError as exc:
                raise BridgeValidationError(f"{name}: {exc.field_path}", exc.message) from exc
        return cls(schemas=schemas)

    def get(self, tool_name: Any) -> ToolParamSchema | None:
        return self.schemas.get(str(tool_name or ""))

    def validate_request(self, request: "ToolCallRequest") -> None:
        if not request.is_factorio_mcp_tool:
            return
        schema = self.get(request.short_name)
        if schema:
            schema.validate_request(request)


class ToolCallRequest(BridgeModel):
    tool_name: str
    tool_input: dict[str, Any] = Field(default_factory=dict)

    @field_validator("tool_name", mode="before")
    @classmethod
    def _coerce_tool_name(cls, value: Any) -> str:
        if not isinstance(value, str) or not value.strip():
            raise ValueError("expected non-empty string")
        return value.strip()

    @field_validator("tool_input", mode="before")
    @classmethod
    def _coerce_tool_input(cls, value: Any) -> dict[str, Any]:
        if value is None:
            return {}
        if not isinstance(value, dict):
            raise ValueError("expected object")
        return dict(value)

    @classmethod
    def from_hook_input(cls, value: Any) -> "ToolCallRequest":
        if isinstance(value, cls):
            return value
        if isinstance(value, dict):
            data = value
        elif hasattr(value, "tool_name"):
            data = {
                "tool_name": getattr(value, "tool_name", None),
                "tool_input": getattr(value, "tool_input", {}),
            }
        else:
            raise BridgeValidationError("tool_call", "expected object")
        tool_input = data.get("tool_input", {})
        if tool_input is not None and not isinstance(tool_input, dict):
            raise BridgeValidationError("tool_input", "expected object")
        try:
            return cls(tool_name=data.get("tool_name"), tool_input=tool_input)
        except ValidationError as exc:
            for error in exc.errors():
                location = error.get("loc", ())
                if location == ("tool_name",):
                    raise BridgeValidationError(
                        "tool_name",
                        "expected non-empty string",
                    ) from exc
                if location == ("tool_input",):
                    raise BridgeValidationError("tool_input", "expected object") from exc
            raise BridgeValidationError("tool_call", "expected object") from exc

    @staticmethod
    def short_factorio_tool_name(tool_name: Any) -> str:
        name = str(tool_name or "")
        if name.startswith(FACTORIO_MCP_TOOL_PREFIX):
            return name[len(FACTORIO_MCP_TOOL_PREFIX):]
        return name

    @staticmethod
    def is_factorio_mcp_tool_name(tool_name: Any) -> bool:
        return str(tool_name or "").startswith(FACTORIO_MCP_TOOL_PREFIX)

    @staticmethod
    def is_mutating_factorio_tool_name(tool_name: Any) -> bool:
        return (
            ToolCallRequest.short_factorio_tool_name(tool_name)
            in FACTORIO_MUTATING_TOOLS
        )

    @staticmethod
    def is_read_only_factorio_tool_name(tool_name: Any) -> bool:
        return (
            ToolCallRequest.short_factorio_tool_name(tool_name)
            in FACTORIO_READ_ONLY_TOOLS
        )

    @property
    def short_name(self) -> str:
        return self.short_factorio_tool_name(self.tool_name)

    @property
    def is_factorio_mcp_tool(self) -> bool:
        return self.is_factorio_mcp_tool_name(self.tool_name)

    @property
    def is_mutating_factorio_tool(self) -> bool:
        return self.is_mutating_factorio_tool_name(self.tool_name)

    @property
    def is_read_only_factorio_tool(self) -> bool:
        return self.is_read_only_factorio_tool_name(self.tool_name)

    @property
    def is_read_only_dry_run(self) -> bool:
        if self.short_name != "feed_lab_from_inventory":
            return (
                self.short_name in FACTORIO_DRY_RUN_SAFE_MUTATING_TOOLS
                and self.tool_input.get("dry_run") is True
            )
        return self.tool_input.get("dry_run", True) is not False

    @property
    def is_manual_fuel_transfer(self) -> bool:
        if self.short_name not in {"hand_feed_furnace", "insert_items"}:
            return False
        item = str(self.tool_input.get("item") or "").strip().lower()
        inventory_type = str(self.tool_input.get("inventory_type") or "").strip().lower()
        return inventory_type == "fuel" or item in {
            "coal",
            "wood",
            "solid-fuel",
            "rocket-fuel",
            "nuclear-fuel",
        }

    @property
    def is_manual_science_transfer(self) -> bool:
        item = str(self.tool_input.get("item") or "").strip().lower()
        recipe = str(self.tool_input.get("recipe") or "").strip().lower()
        science_pack = str(self.tool_input.get("science_pack") or "").strip().lower()
        if self.short_name == "feed_lab_from_inventory":
            return self.tool_input.get("dry_run", True) is False
        if self.short_name == "craft":
            return recipe.endswith("-science-pack") or recipe == "automation-science-pack"
        if self.short_name in {"extract_items", "insert_items"}:
            return (
                item.endswith("-science-pack")
                or science_pack.endswith("-science-pack")
                or item == "automation-science-pack"
                or science_pack == "automation-science-pack"
            )
        return False

    @property
    def is_manual_material_transfer(self) -> bool:
        item = str(self.tool_input.get("item") or "").strip().lower()
        inventory_type = str(self.tool_input.get("inventory_type") or "").strip().lower()
        if self.short_name == "hand_feed_furnace":
            return True
        if self.short_name == "insert_items":
            return inventory_type == "furnace_source" and item in {
                "iron-ore",
                "copper-ore",
                "stone",
            }
        if self.short_name == "extract_items":
            return inventory_type == "furnace_result" and item in {
                "iron-plate",
                "copper-plate",
                "steel-plate",
            }
        return False

    @property
    def is_manual_component_craft(self) -> bool:
        if self.short_name != "craft":
            return False
        recipe = str(self.tool_input.get("recipe") or "").strip().lower()
        return recipe in {
            "iron-gear-wheel",
            "copper-cable",
            "electronic-circuit",
        }

    @property
    def is_bootstrap_infrastructure_craft(self) -> bool:
        if self.short_name != "craft":
            return False
        recipe = str(self.tool_input.get("recipe") or "").strip().lower()
        return recipe in {
            "assembling-machine-1",
            "burner-inserter",
            "copper-cable",
            "electronic-circuit",
            "inserter",
            "iron-gear-wheel",
            "small-electric-pole",
            "transport-belt",
        }

    def validate_params(
        self,
        *,
        schema: ToolParamSchema | dict[str, Any] | None = None,
        required: dict[str, str] | None = None,
        optional: dict[str, str] | None = None,
    ) -> None:
        if schema is not None:
            param_schema = ToolParamSchema.from_mapping(schema)
            required = param_schema.required
            optional = param_schema.optional
        required = required or {}
        optional = optional or {}
        for key, expected_type in required.items():
            if key not in self.tool_input:
                raise BridgeValidationError(f"tool_input.{key}", "missing required field")
            value = self.tool_input.get(key)
            if not _matches_tool_param_type(value, expected_type):
                raise BridgeValidationError(f"tool_input.{key}", f"expected {expected_type}")
        for key, expected_type in optional.items():
            if key not in self.tool_input or self.tool_input.get(key) is None:
                continue
            value = self.tool_input.get(key)
            if not _matches_tool_param_type(value, expected_type):
                raise BridgeValidationError(f"tool_input.{key}", f"expected {expected_type}")

    def to_dict(self) -> dict[str, Any]:
        return {
            "tool_name": self.tool_name,
            "tool_input": dict(self.tool_input),
        }


class PreToolUsePermissionDecision(str, Enum):
    ALLOW = "allow"
    DENY = "deny"


class PreToolUseGuardKind(str, Enum):
    PARALLEL_MUTATION = "parallel_mutation"
    READ_ONLY_TURN = "read_only_turn"
    PARAM_SCHEMA = "param_schema"
    SKILL_REQUIRED = "skill_required"
    MANUAL_AUTOMATION = "manual_automation"


class PreToolUseDecision(BridgeModel):
    """Typed Claude SDK PreToolUse hook response payload."""

    permission_decision: PreToolUsePermissionDecision
    reason: str = ""
    hook_event_name: str = "PreToolUse"

    @field_validator("permission_decision", mode="before")
    @classmethod
    def _coerce_permission_decision(
        cls,
        value: Any,
    ) -> PreToolUsePermissionDecision:
        if isinstance(value, PreToolUsePermissionDecision):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower()
            for decision in PreToolUsePermissionDecision:
                if normalized == decision.value:
                    return decision
        return PreToolUsePermissionDecision.DENY

    @field_validator("reason", "hook_event_name", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "").strip()

    @classmethod
    def allow(cls) -> "PreToolUseDecision":
        return cls(permission_decision=PreToolUsePermissionDecision.ALLOW)

    @classmethod
    def deny(cls, reason: Any) -> "PreToolUseDecision":
        return cls(
            permission_decision=PreToolUsePermissionDecision.DENY,
            reason=reason,
        )

    @property
    def is_denied(self) -> bool:
        return self.permission_decision == PreToolUsePermissionDecision.DENY

    def to_dict(self) -> dict[str, Any]:
        hook_output: dict[str, Any] = {
            "hookEventName": self.hook_event_name or "PreToolUse",
            "permissionDecision": self.permission_decision.value,
        }
        if self.reason:
            hook_output["permissionDecisionReason"] = self.reason
        result = {"hookSpecificOutput": hook_output}
        if self.is_denied:
            result["decision"] = "block"
            result["reason"] = self.reason
        return result


class PreToolUseHookResponse(BridgeModel):
    """Typed SDK PreToolUse hook response, including no-op pass-through."""

    decision: PreToolUseDecision | None = None

    @field_validator("decision", mode="before")
    @classmethod
    def _coerce_decision(cls, value: Any) -> PreToolUseDecision | None:
        if value is None or isinstance(value, PreToolUseDecision):
            return value
        if isinstance(value, PreToolUseGuardBlock):
            return value.to_decision()
        if isinstance(value, dict):
            hook_output = value.get("hookSpecificOutput")
            if isinstance(hook_output, dict):
                return PreToolUseDecision(
                    permission_decision=hook_output.get("permissionDecision"),
                    reason=(
                        value.get("reason")
                        or hook_output.get("permissionDecisionReason")
                        or ""
                    ),
                    hook_event_name=hook_output.get("hookEventName", "PreToolUse"),
                )
        return None

    @classmethod
    def noop(cls) -> "PreToolUseHookResponse":
        return cls()

    @classmethod
    def allow(cls) -> "PreToolUseHookResponse":
        return cls(decision=PreToolUseDecision.allow())

    @classmethod
    def block(cls, block: "PreToolUseGuardBlock") -> "PreToolUseHookResponse":
        return cls(decision=block.to_decision())

    @property
    def is_noop(self) -> bool:
        return self.decision is None

    def to_dict(self) -> dict[str, Any]:
        if self.decision is None:
            return {}
        return self.decision.to_dict()


class PreToolUseGuardBlock(BridgeModel):
    """Typed pre-tool-use guard block with operator-safe reason text."""

    kind: PreToolUseGuardKind
    tool_name: str = ""
    previous_tool_name: str = ""
    detail: str = ""
    elapsed_s: float = 0.0

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> PreToolUseGuardKind:
        if isinstance(value, PreToolUseGuardKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in PreToolUseGuardKind:
                if normalized == kind.value:
                    return kind
        return PreToolUseGuardKind.PARAM_SCHEMA

    @field_validator("tool_name", "previous_tool_name", "detail", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "").strip()

    @field_validator("elapsed_s", mode="before")
    @classmethod
    def _coerce_elapsed(cls, value: Any) -> float:
        try:
            return max(0.0, float(value))
        except (TypeError, ValueError):
            return 0.0

    @classmethod
    def parallel_mutation(
        cls,
        *,
        tool_name: Any,
        previous_tool_name: Any = "",
        elapsed_s: Any = 0.0,
    ) -> "PreToolUseGuardBlock":
        return cls(
            kind=PreToolUseGuardKind.PARALLEL_MUTATION,
            tool_name=ToolCallRequest.short_factorio_tool_name(tool_name),
            previous_tool_name=ToolCallRequest.short_factorio_tool_name(previous_tool_name),
            elapsed_s=elapsed_s,
        )

    @classmethod
    def read_only_turn(cls, *, tool_name: Any) -> "PreToolUseGuardBlock":
        return cls(
            kind=PreToolUseGuardKind.READ_ONLY_TURN,
            tool_name=ToolCallRequest.short_factorio_tool_name(tool_name),
        )

    @classmethod
    def param_schema(
        cls,
        *,
        detail: Any,
        tool_name: Any = "",
    ) -> "PreToolUseGuardBlock":
        short_name = ToolCallRequest.short_factorio_tool_name(tool_name)
        normalized_detail = str(detail or "").strip()
        if short_name and not normalized_detail.startswith(f"{short_name}:"):
            normalized_detail = f"{short_name}: {normalized_detail}"
        return cls(
            kind=PreToolUseGuardKind.PARAM_SCHEMA,
            tool_name=short_name,
            detail=normalized_detail,
        )

    @classmethod
    def skill_required(cls, *, tool_name: Any) -> "PreToolUseGuardBlock":
        return cls(
            kind=PreToolUseGuardKind.SKILL_REQUIRED,
            tool_name=ToolCallRequest.short_factorio_tool_name(tool_name),
        )

    @classmethod
    def manual_automation(cls, *, tool_name: Any) -> "PreToolUseGuardBlock":
        return cls(
            kind=PreToolUseGuardKind.MANUAL_AUTOMATION,
            tool_name=ToolCallRequest.short_factorio_tool_name(tool_name),
        )

    @property
    def reason(self) -> str:
        if self.kind == PreToolUseGuardKind.PARALLEL_MUTATION:
            return (
                f"{BRIDGE_PARALLEL_MUTATION_GUARD_PREFIX} {self.tool_name}. "
                "Wait for the previous mutating tool result before issuing "
                "another world/inventory-changing command."
            )
        if self.kind == PreToolUseGuardKind.READ_ONLY_TURN:
            return (
                f"{BRIDGE_READ_ONLY_TURN_GUARD_PREFIX} {self.tool_name}. "
                "This turn may only use read-only diagnostics; emit a ledger-only "
                "plan or reflection and stop."
            )
        if self.kind == PreToolUseGuardKind.SKILL_REQUIRED:
            return (
                f"{BRIDGE_SKILL_REQUIRED_GUARD_PREFIX} {self.tool_name}. "
                "Call Skill(factorio-control) before using Factorio MCP tools."
            )
        if self.kind == PreToolUseGuardKind.MANUAL_AUTOMATION:
            return (
                f"{BRIDGE_MANUAL_AUTOMATION_GUARD_PREFIX} {self.tool_name}. "
                "The active ledger plan is stale because it relies on manual "
                "transfer loops. Replace it with durable automation controllers "
                "such as repair_fuel_sustainability, build_fuel_supply, execute_direct_smelter, "
                "plan_recipe_assembler_cell, build_recipe_assembler_cell, "
                "build_automation_science, build_assembler_feed, "
                "plan_machine_output, build_assembler_output for machine/furnace output belts, "
                "or build_lab_feed."
            )
        return f"{BRIDGE_PARAM_SCHEMA_GUARD_PREFIX} {self.detail}"

    @property
    def debug_message(self) -> str:
        if self.kind == PreToolUseGuardKind.PARALLEL_MUTATION:
            return (
                "blocked parallel mutating tool: "
                f"{self.tool_name} after {self.previous_tool_name} "
                f"in {self.elapsed_s:.3f}s"
            )
        if self.kind == PreToolUseGuardKind.READ_ONLY_TURN:
            return (
                "blocked non-read-only tool during planner/reflection turn: "
                f"{self.tool_name}"
            )
        if self.kind == PreToolUseGuardKind.SKILL_REQUIRED:
            return f"blocked Factorio MCP tool before skill: {self.tool_name}"
        if self.kind == PreToolUseGuardKind.MANUAL_AUTOMATION:
            return f"blocked stale manual automation tool: {self.tool_name}"
        if self.tool_name:
            return f"blocked invalid {self.tool_name} params: {self.detail}"
        return f"blocked malformed tool call hook input: {self.detail}"

    def to_decision(self) -> PreToolUseDecision:
        return PreToolUseDecision.deny(self.reason)

    def to_dict(self) -> dict[str, Any]:
        return self.to_decision().to_dict()


class WatchdogToolObservation(BridgeModel):
    """Typed view of one tool result as consumed by the tick watchdog."""

    tool_use_id: str = ""
    tool_name: str = ""
    classification: ToolResultClassification = ToolResultClassification.SDK_FAILURE
    text: str = ""
    indicates_progress: bool | None = None

    @field_validator("tool_use_id", "tool_name", "text", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "")

    @field_validator("classification", mode="before")
    @classmethod
    def _coerce_classification(cls, value: Any) -> ToolResultClassification:
        if isinstance(value, ToolResultClassification):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower()
            for classification in ToolResultClassification:
                if normalized == classification.value:
                    return classification
        return ToolResultClassification.SDK_FAILURE

    @classmethod
    def from_result(
        cls,
        *,
        tool_use_id: Any = None,
        tool_name: Any = None,
        classification: ToolResultClassification | str,
        text: Any = "",
        indicates_progress: bool | None = None,
    ) -> "WatchdogToolObservation":
        return cls(
            tool_use_id=tool_use_id,
            tool_name=tool_name,
            classification=classification,
            text=text,
            indicates_progress=indicates_progress,
        )

    @property
    def short_tool_name(self) -> str:
        return ToolCallRequest.short_factorio_tool_name(self.tool_name)

    @property
    def is_ok(self) -> bool:
        return self.classification == ToolResultClassification.OK

    @property
    def is_expected_miss(self) -> bool:
        return self.classification == ToolResultClassification.EXPECTED_MISS

    @property
    def is_game_rejected(self) -> bool:
        return self.classification == ToolResultClassification.GAME_REJECTED

    @property
    def is_mutating_tool(self) -> bool:
        return ToolCallRequest.is_mutating_factorio_tool_name(self.tool_name)

    def indicates_mutating_progress(
        self,
        *,
        text_is_error: Callable[[str], bool] | None = None,
    ) -> bool:
        if self.indicates_progress is not None:
            return self.indicates_progress
        return ToolResultOutcome.text_indicates_progress(
            self.text,
            text_is_error=text_is_error,
        )

    def failure_signature(self, *, limit: int = 300) -> str:
        return "|".join([
            self.short_tool_name,
            self.classification.value,
            " ".join(self.text.split())[:limit],
        ])


class SdkMetadataItems(BridgeModel):
    """Typed sequence view for Claude SDK init metadata lists."""

    items: tuple[Any, ...] = ()

    @field_validator("items", mode="before")
    @classmethod
    def _coerce_items(cls, value: Any) -> tuple[Any, ...]:
        if value is None or isinstance(value, (str, bytes, dict, BaseModel)):
            return ()
        if isinstance(value, Iterable):
            return tuple(value)
        return ()

    @classmethod
    def from_value(cls, value: Any) -> "SdkMetadataItems":
        if isinstance(value, cls):
            return value
        return cls(items=value)

    def bounded_strings(self, *, limit: int = 12) -> list[str]:
        items = [str(item) for item in self.items[:limit]]
        if len(self.items) > limit:
            items.append(f"...+{len(self.items) - limit}")
        return items

    def contains(self, value: Any) -> bool:
        return value in self.items


class SdkSystemMessage(BridgeModel):
    """Typed view of a Claude SDK system message."""

    subtype: str = ""
    data: dict[str, Any] = Field(default_factory=dict)
    data_is_mapping: bool = True

    @classmethod
    def from_sdk_message(cls, value: Any) -> "SdkSystemMessage":
        if isinstance(value, cls):
            return value
        raw_data = getattr(value, "data", None)
        return cls(
            subtype=str(getattr(value, "subtype", "") or ""),
            data=dict(raw_data) if isinstance(raw_data, dict) else {},
            data_is_mapping=isinstance(raw_data, dict),
        )

    @staticmethod
    def bounded_list(value: Any, limit: int = 12) -> list[str]:
        return SdkMetadataItems.from_value(value).bounded_strings(limit=limit)

    @property
    def is_init(self) -> bool:
        return self.subtype == "init"

    @property
    def is_loggable_init(self) -> bool:
        return self.is_init and self.data_is_mapping

    @property
    def should_log(self) -> bool:
        return self.subtype not in {"thinking_tokens"}

    @property
    def cwd(self) -> Any:
        return self.data.get("cwd")

    @property
    def has_skill_tool(self) -> bool:
        return SdkMetadataItems.from_value(self.data.get("tools", [])).contains("Skill")

    @property
    def skill_tool_label(self) -> str:
        return "yes" if self.has_skill_tool else "no"

    def bounded_visible_skills(self, limit: int = 12) -> list[str]:
        return self.bounded_list(self.data.get("skills", []), limit=limit)


class SdkToolUse(BridgeModel):
    """Typed view of a Claude SDK tool-use block.

    The SDK object itself is intentionally not imported here; this model owns
    bridge policy derived from tool names so the stream loop does not need to
    repeat ad hoc string predicates.
    """

    name: str
    tool_input: Any = Field(default_factory=dict)

    @classmethod
    def from_sdk_block(cls, value: Any) -> "SdkToolUse":
        if isinstance(value, cls):
            return value
        return cls(
            name=str(getattr(value, "name", "") or ""),
            tool_input=getattr(value, "input", {}),
        )

    @staticmethod
    def display_name_for(tool_name: Any) -> str:
        return ToolCallRequest.short_factorio_tool_name(tool_name)

    @property
    def display_name(self) -> str:
        return self.display_name_for(self.name)

    @staticmethod
    def json_for_log(value: Any) -> str:
        try:
            return json.dumps(value, ensure_ascii=False, separators=(",", ":"))
        except (TypeError, ValueError):
            return str(value)

    @property
    def log_input_text(self) -> str:
        return self.json_for_log(self.tool_input)

    @property
    def input_mapping(self) -> dict[str, Any]:
        if isinstance(self.tool_input, dict):
            return self.tool_input
        return {}

    @property
    def is_skill_tool(self) -> bool:
        return self.name == "Skill" or self.name.endswith("__Skill")

    @property
    def is_broadcast_thought(self) -> bool:
        return self.display_name.endswith("broadcast_thought")

    @property
    def thought_message(self) -> str:
        message = self.input_mapping.get("message", "")
        if message is None:
            return ""
        return str(message)

    @property
    def should_send_tool_status(self) -> bool:
        return (
            not self.name.startswith("mcp__")
            or ToolCallRequest.is_factorio_mcp_tool_name(self.name)
        )


class SdkAssistantEventKind(str, Enum):
    TEXT = "text"
    TOOL_USE = "tool_use"
    THINKING = "thinking"


class SdkAssistantEvent(BridgeModel):
    """Ordered assistant content event from the Claude SDK stream."""

    kind: SdkAssistantEventKind
    text: str = ""
    tool_use_id: str | None = None
    tool_use: SdkToolUse | None = None

    @classmethod
    def text_event(cls, value: Any) -> "SdkAssistantEvent":
        return cls(kind=SdkAssistantEventKind.TEXT, text=str(value or ""))

    @classmethod
    def thinking_event(cls, value: Any) -> "SdkAssistantEvent":
        return cls(kind=SdkAssistantEventKind.THINKING, text=str(value or ""))

    @classmethod
    def tool_use_event(
        cls,
        value: Any,
        *,
        tool_use_id: Any = None,
    ) -> "SdkAssistantEvent":
        return cls(
            kind=SdkAssistantEventKind.TOOL_USE,
            tool_use_id=str(tool_use_id) if tool_use_id is not None else None,
            tool_use=SdkToolUse.from_sdk_block(value),
        )

    @property
    def is_text(self) -> bool:
        return self.kind == SdkAssistantEventKind.TEXT

    @property
    def is_tool_use(self) -> bool:
        return self.kind == SdkAssistantEventKind.TOOL_USE

    @property
    def is_thinking(self) -> bool:
        return self.kind == SdkAssistantEventKind.THINKING


class SdkContentBlocks(BridgeModel):
    """Typed sequence boundary for Claude SDK message content blocks."""

    blocks: tuple[Any, ...] = ()

    @field_validator("blocks", mode="before")
    @classmethod
    def _coerce_blocks(cls, value: Any) -> tuple[Any, ...]:
        if value is None or isinstance(value, (str, bytes, dict)):
            return ()
        if isinstance(value, Iterable):
            return tuple(value)
        return ()

    @classmethod
    def from_value(cls, value: Any) -> "SdkContentBlocks":
        if isinstance(value, cls):
            return value
        return cls(blocks=value)


class SdkAssistantMessage(BridgeModel):
    """Typed ordered view of SDK assistant-message content."""

    session_id: str | None = None
    events: list[SdkAssistantEvent] = Field(default_factory=list)

    @classmethod
    def from_sdk_message(
        cls,
        value: Any,
        *,
        text_block_type: Any = None,
        tool_use_block_type: Any = None,
        thinking_block_type: Any = None,
    ) -> "SdkAssistantMessage":
        if isinstance(value, cls):
            return value
        raw_session_id = getattr(value, "session_id", None)
        content = SdkContentBlocks.from_value(getattr(value, "content", None))

        events: list[SdkAssistantEvent] = []
        for block in content.blocks:
            if cls._matches(block, text_block_type, "text"):
                events.append(SdkAssistantEvent.text_event(
                    getattr(block, "text", ""),
                ))
            elif cls._matches(block, tool_use_block_type, "name"):
                events.append(SdkAssistantEvent.tool_use_event(
                    block,
                    tool_use_id=getattr(block, "id", None),
                ))
            elif cls._matches(block, thinking_block_type, "thinking"):
                events.append(SdkAssistantEvent.thinking_event(
                    getattr(block, "thinking", ""),
                ))
        return cls(
            session_id=str(raw_session_id) if raw_session_id else None,
            events=events,
        )

    @staticmethod
    def _matches(block: Any, expected_type: Any, fallback_attr: str) -> bool:
        if expected_type is not None:
            return isinstance(block, expected_type)
        return hasattr(block, fallback_attr)


class SdkToolResultEvent(BridgeModel):
    """Normalized tool-result event from an SDK user message."""

    tool_use_id: str | None = None
    content: ToolResultContent = Field(default_factory=ToolResultContent)
    is_error: bool = False

    @classmethod
    def from_string(
        cls,
        value: Any,
        *,
        player_marker: str = "\n\n--- Player Messages ---\n",
    ) -> "SdkToolResultEvent":
        if isinstance(value, cls):
            return value
        return cls(
            content=ToolResultContent.from_sdk_content(
                value,
                player_marker=player_marker,
            ),
            is_error=False,
        )

    @classmethod
    def from_sdk_block(
        cls,
        value: Any,
        *,
        player_marker: str = "\n\n--- Player Messages ---\n",
    ) -> "SdkToolResultEvent":
        if isinstance(value, cls):
            return value
        raw_tool_use_id = getattr(value, "tool_use_id", None)
        return cls(
            tool_use_id=str(raw_tool_use_id) if raw_tool_use_id is not None else None,
            content=ToolResultContent.from_sdk_content(
                getattr(value, "content", None),
                player_marker=player_marker,
            ),
            is_error=bool(getattr(value, "is_error", False)),
        )

    @property
    def text(self) -> str:
        return self.content.text

    @property
    def player_message_text(self) -> str:
        return self.content.player_message_text

    @property
    def outcome(self) -> ToolResultOutcome:
        return self.content.outcome(sdk_is_error=self.is_error)

    def indicates_progress(
        self,
        *,
        text_is_error: Callable[[str], bool] | None = None,
    ) -> bool:
        return self.content.indicates_progress(text_is_error=text_is_error)

    def observation(
        self,
        *,
        text_is_error: Callable[[str], bool] | None = None,
    ) -> "SdkToolResultObservation":
        return SdkToolResultObservation.from_event(
            self,
            text_is_error=text_is_error,
        )


class SdkToolResultObservation(BridgeModel):
    """Typed runtime observation derived from one SDK tool result."""

    tool_use_id: str | None = None
    text: str = ""
    player_message_text: str = ""
    outcome: ToolResultOutcome
    log_record: ToolResultLogRecord
    indicates_progress: bool = False

    @field_validator("tool_use_id", mode="before")
    @classmethod
    def _coerce_optional_text(cls, value: Any) -> str | None:
        text = str(value or "").strip()
        return text or None

    @field_validator("text", "player_message_text", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "")

    @property
    def classification(self) -> ToolResultClassification:
        return self.outcome.classification

    @classmethod
    def from_event(
        cls,
        event: SdkToolResultEvent,
        *,
        text_is_error: Callable[[str], bool] | None = None,
    ) -> "SdkToolResultObservation":
        outcome = event.outcome
        return cls(
            tool_use_id=event.tool_use_id,
            text=event.text,
            player_message_text=event.player_message_text,
            outcome=outcome,
            log_record=ToolResultLogRecord.from_outcome(
                outcome,
                text=event.text,
            ),
            indicates_progress=event.indicates_progress(
                text_is_error=text_is_error,
            ),
        )


class SdkUserToolResultMessage(BridgeModel):
    """Typed view of SDK user-message content that may carry tool results."""

    results: list[SdkToolResultEvent] = Field(default_factory=list)

    @classmethod
    def from_sdk_message(
        cls,
        value: Any,
        *,
        tool_result_block_type: Any = None,
        player_marker: str = "\n\n--- Player Messages ---\n",
    ) -> "SdkUserToolResultMessage":
        if isinstance(value, cls):
            return value
        content = getattr(value, "content", None)
        if isinstance(content, str):
            return cls(results=[
                SdkToolResultEvent.from_string(
                    content,
                    player_marker=player_marker,
                )
            ])

        results: list[SdkToolResultEvent] = []
        for block in SdkContentBlocks.from_value(content).blocks:
            if tool_result_block_type is not None:
                if not isinstance(block, tool_result_block_type):
                    continue
            elif not hasattr(block, "content"):
                continue
            results.append(SdkToolResultEvent.from_sdk_block(
                block,
                player_marker=player_marker,
            ))
        return cls(results=results)


class SdkResultMessage(BridgeModel):
    """Typed view of a Claude SDK terminal result message."""

    session_id: str | None = None
    result_text: str = ""
    errors: list[str] = Field(default_factory=list)
    is_error: bool = False
    total_cost_usd: float | None = None
    num_turns: int | None = None
    duration_ms: float | None = None

    @classmethod
    def from_sdk_message(cls, value: Any) -> "SdkResultMessage":
        if isinstance(value, cls):
            return value
        raw_session_id = getattr(value, "session_id", None)
        raw_result = getattr(value, "result", "")
        raw_cost = getattr(value, "total_cost_usd", None)
        raw_num_turns = getattr(value, "num_turns", None)
        raw_duration_ms = getattr(value, "duration_ms", None)
        return cls(
            session_id=str(raw_session_id) if raw_session_id else None,
            result_text=str(raw_result) if raw_result else "",
            errors=_coerce_str_list(getattr(value, "errors", [])),
            is_error=bool(getattr(value, "is_error", False)),
            total_cost_usd=(
                None if raw_cost is None else _coerce_float(raw_cost)
            ),
            num_turns=cls._optional_int(raw_num_turns),
            duration_ms=(
                None if raw_duration_ms is None else _coerce_float(raw_duration_ms)
            ),
        )

    @staticmethod
    def _optional_int(value: Any) -> int | None:
        if value is None:
            return None
        try:
            return int(value)
        except (TypeError, ValueError):
            return None

    @property
    def has_result_text(self) -> bool:
        return bool(self.result_text)

    @property
    def error_detail(self) -> str:
        if self.result_text:
            return self.result_text
        if self.errors:
            return "; ".join(self.errors)
        return "agent result marked as error"

    @property
    def is_context_window_limit(self) -> bool:
        return SdkErrorSignal.is_context_window_limit(self.error_detail)

    @property
    def has_cost(self) -> bool:
        return self.total_cost_usd is not None

    @property
    def duration_s(self) -> float:
        return (self.duration_ms or 0.0) / 1000

    @property
    def compute_cost_payload(self) -> dict[str, Any]:
        return {
            "cost_usd": self.total_cost_usd,
            "turns": self.num_turns,
            "duration_ms": self.duration_ms,
        }

    def observation(
        self,
        *,
        default_utc_offset: str | None = None,
        failure_text_limit: int = 300,
    ) -> "SdkResultObservation":
        return SdkResultObservation.from_message(
            self,
            default_utc_offset=default_utc_offset,
            failure_text_limit=failure_text_limit,
        )


class SdkResultObservation(BridgeModel):
    """Typed runtime observation derived from one SDK terminal result."""

    session_id: str | None = None
    transcript_text: str = ""
    is_error: bool = False
    error_detail: str = ""
    context_window_limit: bool = False
    usage_limit: ProviderUsageLimit | None = None
    failure_classification: ToolResultClassification | None = None
    failure_journal_text: str = ""
    total_cost_usd: float | None = None
    num_turns: int | None = None
    duration_s: float = 0.0
    compute_cost_payload: dict[str, Any] = Field(default_factory=dict)

    @field_validator("session_id", mode="before")
    @classmethod
    def _coerce_optional_text(cls, value: Any) -> str | None:
        text = str(value or "").strip()
        return text or None

    @field_validator("transcript_text", "error_detail", "failure_journal_text", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "")

    @field_validator("failure_classification", mode="before")
    @classmethod
    def _coerce_failure_classification(
        cls,
        value: Any,
    ) -> ToolResultClassification | None:
        if value is None or isinstance(value, ToolResultClassification):
            return value
        try:
            return ToolResultClassification(str(value).strip().lower())
        except ValueError:
            return ToolResultClassification.SDK_FAILURE

    @property
    def has_transcript_text(self) -> bool:
        return bool(self.transcript_text)

    @property
    def usage_limit_seen(self) -> bool:
        return self.usage_limit is not None

    @property
    def has_cost(self) -> bool:
        return self.total_cost_usd is not None

    @classmethod
    def from_message(
        cls,
        message: SdkResultMessage,
        *,
        default_utc_offset: str | None = None,
        failure_text_limit: int = 300,
    ) -> "SdkResultObservation":
        error_detail = message.error_detail if message.is_error else ""
        usage_limit = (
            ProviderUsageLimit.from_text(
                error_detail,
                default_utc_offset=default_utc_offset,
            )
            if message.is_error and not message.is_context_window_limit
            else None
        )
        failure_classification: ToolResultClassification | None = None
        failure_journal_text = ""
        if message.is_error and not message.is_context_window_limit and not usage_limit:
            failure_classification = ToolResultClassification.SDK_FAILURE
            failure_journal_text = (
                f"{failure_classification.value}: "
                f"{_single_line_text(error_detail, limit=failure_text_limit)}"
            )
        return cls(
            session_id=message.session_id,
            transcript_text=message.result_text,
            is_error=message.is_error,
            error_detail=error_detail,
            context_window_limit=message.is_context_window_limit,
            usage_limit=usage_limit,
            failure_classification=failure_classification,
            failure_journal_text=failure_journal_text,
            total_cost_usd=message.total_cost_usd,
            num_turns=message.num_turns,
            duration_s=message.duration_s,
            compute_cost_payload=message.compute_cost_payload if message.has_cost else {},
        )


class AgentRunTranscript(BridgeModel):
    """Typed transcript emitted by one SDK run."""

    text_parts: list[str] = Field(default_factory=list)
    session_id: str | None = None
    context_window_limit: bool = False
    usage_limit_seen: bool = False

    @field_validator("text_parts", mode="before")
    @classmethod
    def _coerce_text_parts(cls, value: Any) -> list[str]:
        return [part for part in _coerce_str_list(value) if part]

    @field_validator("session_id", mode="before")
    @classmethod
    def _coerce_session_id(cls, value: Any) -> str | None:
        text = str(value or "").strip()
        return text or None

    @field_validator("context_window_limit", "usage_limit_seen", mode="before")
    @classmethod
    def _coerce_bool(cls, value: Any) -> bool:
        if isinstance(value, bool):
            return value
        return str(value).strip().lower() in {"1", "true", "yes", "on"}

    @classmethod
    def from_parts(
        cls,
        *,
        text_parts: Any = None,
        session_id: Any = None,
        context_window_limit: Any = False,
        usage_limit_seen: Any = False,
    ) -> "AgentRunTranscript":
        return cls(
            text_parts=text_parts or [],
            session_id=session_id,
            context_window_limit=context_window_limit,
            usage_limit_seen=usage_limit_seen,
        )

    def with_text_parts(self, text_parts: Any) -> "AgentRunTranscript":
        return self.model_copy(update={
            "text_parts": [part for part in _coerce_str_list(text_parts) if part],
        })

    def session_or(self, fallback: Any = None) -> str | None:
        return self.session_id or (str(fallback).strip() if fallback else None)

    @property
    def reply_text(self) -> str:
        return "\n\n".join(self.text_parts) if self.text_parts else "(action complete)"


class AgentInvocationExceptionSignal(BridgeModel):
    """Typed bridge policy signals extracted from an invocation exception."""

    raw_text: str = ""
    sdk_error: SdkErrorSignal = Field(default_factory=SdkErrorSignal)
    usage_limit: ProviderUsageLimit | None = None

    @field_validator("raw_text", mode="before")
    @classmethod
    def _coerce_raw_text(cls, value: Any) -> str:
        return str(value or "")

    @classmethod
    def from_exception(
        cls,
        exc: Any,
        *,
        now: datetime | None = None,
        default_utc_offset: str | None = None,
    ) -> "AgentInvocationExceptionSignal":
        text = str(exc or "")
        return cls(
            raw_text=text,
            sdk_error=SdkErrorSignal.from_text(text),
            usage_limit=ProviderUsageLimit.from_text(
                text,
                now=now,
                default_utc_offset=default_utc_offset,
            ),
        )

    @property
    def context_window_limit(self) -> bool:
        return self.sdk_error.context_window_limit

    @property
    def terminal_result_echo(self) -> bool:
        return self.sdk_error.terminal_result_echo

    @property
    def usage_limit_seen(self) -> bool:
        return self.usage_limit is not None

    @property
    def error_message(self) -> str:
        return f"Error: {self.raw_text}"

    @property
    def short_text(self) -> str:
        return _single_line_text(self.raw_text, limit=300)


class SdkAssistantTextObservation(BridgeModel):
    """Typed runtime observation derived from one assistant text event."""

    text: str = ""
    usage_limit: ProviderUsageLimit | None = None

    @field_validator("text", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "")

    @classmethod
    def from_event(
        cls,
        event: SdkAssistantEvent,
        *,
        now: datetime | None = None,
        default_utc_offset: str | None = None,
    ) -> "SdkAssistantTextObservation":
        text = event.text if event.is_text else ""
        return cls(
            text=text,
            usage_limit=ProviderUsageLimit.from_text(
                text,
                now=now,
                default_utc_offset=default_utc_offset,
            ),
        )

    @property
    def usage_limit_seen(self) -> bool:
        return self.usage_limit is not None

    @property
    def counts_as_watchdog_progress(self) -> bool:
        return bool(self.text) and self.usage_limit is None


def _input_text(value: Any, default: str = "") -> str:
    if value is None:
        return default
    text = str(value).strip()
    return text if text else default


def _input_bool(value: Any) -> bool:
    if isinstance(value, bool):
        return value
    if isinstance(value, str):
        normalized = value.strip().lower()
        if normalized in {"1", "true", "yes", "y", "on"}:
            return True
        if normalized in {"0", "false", "no", "n", "off", ""}:
            return False
    return False


def _input_player_index(value: Any) -> int:
    if isinstance(value, bool):
        return 1
    try:
        index = int(value)
    except (TypeError, ValueError):
        return 1
    return index if index >= 0 else 1


class BridgeInputMessage(RemotePayloadModel):
    """Typed inbound game/autonomy message while preserving JSONL dict shape."""

    message: str
    player_index: int = 1
    player_name: str = "Player"
    target_agent: str = "default"
    response_to: str | None = None
    model: str | None = None
    autonomy: bool = False
    read_only_tools: bool = False

    @field_validator("message", mode="before")
    @classmethod
    def _coerce_message(cls, value: Any) -> str:
        return str(value).strip() if value is not None else ""

    @field_validator("player_index", mode="before")
    @classmethod
    def _coerce_player_index(cls, value: Any) -> int:
        return _input_player_index(value)

    @field_validator("player_name", mode="before")
    @classmethod
    def _coerce_player_name(cls, value: Any) -> str:
        return _input_text(value, "Player")

    @field_validator("target_agent", mode="before")
    @classmethod
    def _coerce_target_agent(cls, value: Any) -> str:
        return _input_text(value, "default")

    @field_validator("response_to", "model", mode="before")
    @classmethod
    def _coerce_optional_text(cls, value: Any) -> str | None:
        text = _input_text(value)
        return text or None

    @field_validator("autonomy", "read_only_tools", mode="before")
    @classmethod
    def _coerce_flag(cls, value: Any) -> bool:
        return _input_bool(value)

    @classmethod
    def from_mapping(cls, value: Any) -> "BridgeInputMessage | None":
        if isinstance(value, cls):
            return value if value.message else None
        if not isinstance(value, dict):
            return None
        try:
            message = cls.model_validate(value)
        except ValidationError:
            return None
        return message if message.message else None

    @classmethod
    def from_json_line(cls, value: Any) -> "BridgeInputMessage | None":
        if isinstance(value, cls):
            return cls.from_mapping(value)
        if not isinstance(value, str):
            return None
        line = value.strip()
        if not line:
            return None
        data = _json_value_or_missing(line)
        if data is _JSON_MISSING:
            return None
        return cls.from_mapping(data)

    def to_dict(self) -> dict[str, Any]:
        result = dict(self.model_extra or {})
        result.update({
            "message": self.message,
            "player_index": self.player_index,
            "player_name": self.player_name,
            "target_agent": self.target_agent,
        })
        if self.response_to is not None:
            result["response_to"] = self.response_to
        if self.model is not None:
            result["model"] = self.model
        if self.autonomy:
            result["autonomy"] = True
        if self.read_only_tools:
            result["read_only_tools"] = True
        return result


class BridgeInputMessageCollection(BridgeModel):
    """Typed sequence of bridge input messages from parsed JSONL payloads."""

    items: tuple[BridgeInputMessage, ...] = ()

    @field_validator("items", mode="before")
    @classmethod
    def _coerce_items(cls, value: Any) -> tuple[BridgeInputMessage, ...]:
        if value is None or isinstance(value, (str, bytes, dict, BaseModel)):
            return ()
        if not isinstance(value, Iterable):
            return ()
        messages: list[BridgeInputMessage] = []
        for item in value:
            message = BridgeInputMessage.from_mapping(item)
            if message:
                messages.append(message)
        return tuple(messages)

    @classmethod
    def from_value(cls, value: Any) -> "BridgeInputMessageCollection":
        if isinstance(value, cls):
            return value
        return cls(items=value)

    def to_list(self) -> list[BridgeInputMessage]:
        return list(self.items)


class BridgeInputBatch(BridgeModel):
    """Typed JSONL ingress batch from the Factorio chat input file."""

    messages: list[BridgeInputMessage] = Field(default_factory=list)

    @classmethod
    def from_jsonl_text(cls, value: Any) -> "BridgeInputBatch":
        if isinstance(value, cls):
            return value
        collection = BridgeInputMessageCollection.from_value(value)
        if collection.items:
            return cls(messages=collection.to_list())
        if not isinstance(value, str):
            return cls()
        messages = []
        for line in BridgeTextLines.from_text(value, strip=False).lines:
            message = BridgeInputMessage.from_json_line(line)
            if message:
                messages.append(message)
        return cls(messages=messages)

    def to_dicts(self) -> list[dict[str, Any]]:
        return [message.to_dict() for message in self.messages]


class BridgeInputFileDelta(BridgeModel):
    """Typed view of a newly-read slice from the Factorio chat input JSONL file."""

    previous_size: int = 0
    current_size: int = 0
    text: str = ""
    batch: BridgeInputBatch = Field(default_factory=BridgeInputBatch)

    @field_validator("previous_size", "current_size", mode="before")
    @classmethod
    def _coerce_size(cls, value: Any) -> int:
        try:
            size = int(value)
        except (TypeError, ValueError):
            return 0
        return max(0, size)

    @field_validator("text", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value if value is not None else "")

    @field_validator("batch", mode="before")
    @classmethod
    def _coerce_batch(cls, value: Any) -> BridgeInputBatch:
        return BridgeInputBatch.from_jsonl_text(value)

    @classmethod
    def from_chunk(
        cls,
        *,
        previous_size: Any,
        current_size: Any,
        text: Any,
    ) -> "BridgeInputFileDelta":
        return cls(
            previous_size=previous_size,
            current_size=current_size,
            text=text,
            batch=BridgeInputBatch.from_jsonl_text(text),
        )

    @classmethod
    def empty(cls, *, previous_size: Any = 0, current_size: Any = 0) -> "BridgeInputFileDelta":
        return cls(previous_size=previous_size, current_size=current_size)

    @property
    def advanced(self) -> bool:
        return self.current_size > self.previous_size

    @property
    def next_size(self) -> int:
        return self.current_size if self.advanced else self.previous_size

    @property
    def messages(self) -> list[BridgeInputMessage]:
        return list(self.batch.messages)

    def to_dicts(self) -> list[dict[str, Any]]:
        return self.batch.to_dicts()


class AutonomyTickMessage(BridgeModel):
    """Typed synthetic input message generated by the bridge autonomy loop."""

    message: str
    player_index: int = 0
    player_name: str = "autonomy"
    autonomy: bool = True
    read_only_tools: bool = False
    model: str | None = None

    @field_validator("message", mode="before")
    @classmethod
    def _coerce_message(cls, value: Any) -> str:
        text = str(value).strip() if value is not None else ""
        if not text:
            raise ValueError("message is required")
        return text

    @field_validator("player_index", mode="before")
    @classmethod
    def _coerce_player_index(cls, value: Any) -> int:
        try:
            index = int(value)
        except (TypeError, ValueError):
            return 0
        return index if index >= 0 else 0

    @field_validator("player_name", mode="before")
    @classmethod
    def _coerce_player_name(cls, value: Any) -> str:
        return _input_text(value, "autonomy")

    @field_validator("autonomy", "read_only_tools", mode="before")
    @classmethod
    def _coerce_flag(cls, value: Any) -> bool:
        return _input_bool(value)

    @field_validator("model", mode="before")
    @classmethod
    def _coerce_optional_model(cls, value: Any) -> str | None:
        text = _input_text(value)
        return text or None

    @classmethod
    def create(
        cls,
        message: Any,
        *,
        read_only_tools: bool = False,
        model: str | None = None,
    ) -> "AutonomyTickMessage":
        return cls(
            message=message,
            read_only_tools=read_only_tools,
            model=model if read_only_tools else None,
        )

    def __getitem__(self, key: str) -> Any:
        return self.to_dict()[key]

    def __contains__(self, key: object) -> bool:
        return isinstance(key, str) and key in self.to_dict()

    def get(self, key: str, default: Any = None) -> Any:
        return self.to_dict().get(key, default)

    def to_bridge_input(self) -> BridgeInputMessage:
        message = BridgeInputMessage.from_mapping(self.to_dict())
        if message is None:
            raise BridgeValidationError("autonomy_tick", "expected bridge input message")
        return message

    def to_dict(self) -> dict[str, Any]:
        result: dict[str, Any] = {
            "message": self.message,
            "player_index": self.player_index,
            "player_name": self.player_name,
            "autonomy": True,
        }
        if self.read_only_tools:
            result["read_only_tools"] = True
        if self.model is not None:
            result["model"] = self.model
        return result


class SdkSkillConfig(BridgeModel):
    """Normalized Claude SDK skill configuration used by bridge launch code."""

    skills: list[str] = Field(default_factory=list)
    all_skills: bool = False

    ENV_FIELDS: ClassVar[tuple["BridgeRuntimeEnvField", ...]] = ()

    @field_validator("skills", mode="before")
    @classmethod
    def _coerce_skills(cls, value: Any) -> list[str]:
        return _coerce_str_or_list(value)

    @classmethod
    def resolve(
        cls,
        value: Any = None,
        *,
        default: Any = None,
    ) -> "SdkSkillConfig":
        if isinstance(value, cls):
            return value
        if value is None:
            value = default
        if isinstance(value, cls):
            return value
        if isinstance(value, Iterable) and not isinstance(value, (str, bytes, dict)):
            return cls(skills=value)

        raw = str(value or "").strip()
        if not raw:
            return cls()
        lowered = raw.lower()
        if lowered in {"0", "false", "no", "none", "off", "disabled"}:
            return cls()
        if lowered == "all":
            return cls(all_skills=True)
        return cls(skills=CommaSeparatedItems.from_value(raw).to_list())

    @classmethod
    def from_env(
        cls,
        env: Any,
        *,
        value: Any = None,
        default: Any = None,
    ) -> "SdkSkillConfig":
        if isinstance(env, cls) and value is None:
            return env
        if value is None:
            data = BridgeRuntimeEnvField.read_source(env, cls.env_fields())
            value = data.get("skills")
        return cls.resolve(value, default=default)

    @classmethod
    def env_fields(cls) -> tuple["BridgeRuntimeEnvField", ...]:
        # Initialized after BridgeRuntimeEnvField is defined.
        return BridgeRuntimeEnvField.validate_unique(
            cls.ENV_FIELDS,
            field_path="sdk_skill_env_fields",
        )

    @property
    def enabled(self) -> bool:
        return self.all_skills or bool(self.skills)

    @property
    def sdk_value(self) -> list[str] | str:
        return "all" if self.all_skills else list(self.skills)

    @property
    def claude_tools(self) -> list[str]:
        # The SDK documents `skills=` as auto-configuring the Skill tool, but
        # the Claude Code init stream used by this bridge still reports
        # `skill_tool=no` without this explicit entry. Keep the explicit tool
        # until the live init payload proves the native path works here.
        return ["Skill"] if self.enabled else []

    @property
    def setting_sources(self) -> list[str]:
        return ["project", "local"] if self.enabled else ["local"]

    @property
    def requires_factorio_control(self) -> bool:
        return self.all_skills or "factorio-control" in self.skills


class AgentProfileSdkSkills(BridgeModel):
    """Strict profile-file SDK skill value."""

    value: str | list[str] | None = None

    @field_validator("value", mode="before")
    @classmethod
    def _coerce_value(cls, value: Any) -> str | list[str] | None:
        if value is None:
            return None
        if isinstance(value, str):
            return value
        if isinstance(value, Iterable) and not isinstance(value, (str, bytes, dict)):
            items = list(value)
            if all(isinstance(item, str) for item in items):
                return CommaSeparatedItems.from_value(items).to_list()
        raise ValueError("expected string or list of strings")

    @classmethod
    def from_value(cls, value: Any) -> "AgentProfileSdkSkills":
        if isinstance(value, cls):
            return value
        return cls(value=value)

    def to_profile_value(self) -> str | list[str] | None:
        return self.value


class RawLuaPolicy(BridgeModel):
    """Typed bridge policy for exposing raw Lua execution to the SDK."""

    allow_raw_lua: bool = False

    ENV_FIELDS: ClassVar[tuple["BridgeRuntimeEnvField", ...]] = ()
    TRUE_VALUES: ClassVar[frozenset[str]] = frozenset({"1", "true", "yes", "on"})
    EXECUTE_LUA_TOOL: ClassVar[str] = "mcp__factorioctl__execute_lua"

    @field_validator("allow_raw_lua", mode="before")
    @classmethod
    def _coerce_allow_raw_lua(cls, value: Any) -> bool:
        if isinstance(value, bool):
            return value
        return str(value or "").strip().lower() in cls.TRUE_VALUES

    @classmethod
    def from_env(cls, env: Any) -> "RawLuaPolicy":
        if isinstance(env, cls):
            return env
        return cls(**BridgeRuntimeEnvField.read_source(env, cls.env_fields()))

    @classmethod
    def env_fields(cls) -> tuple["BridgeRuntimeEnvField", ...]:
        # Initialized after BridgeRuntimeEnvField is defined.
        return BridgeRuntimeEnvField.validate_unique(
            cls.ENV_FIELDS,
            field_path="raw_lua_env_fields",
        )

    @property
    def disallowed_tools(self) -> list[str]:
        return [] if self.allow_raw_lua else [self.EXECUTE_LUA_TOOL]


class BridgeRuntimeEnvField(BridgeModel):
    """Typed binding from an environment variable to a runtime settings field."""

    env_name: str
    field_name: str

    @field_validator("env_name", "field_name", mode="before")
    @classmethod
    def _coerce_non_empty_text(cls, value: Any) -> str:
        text = str(value or "").strip()
        if not text:
            raise ValueError("expected non-empty string")
        return text

    @classmethod
    def validate_unique(
        cls,
        fields: Iterable["BridgeRuntimeEnvField"],
        *,
        field_path: str,
    ) -> tuple["BridgeRuntimeEnvField", ...]:
        env_names: set[str] = set()
        field_names: set[str] = set()
        result: list[BridgeRuntimeEnvField] = []
        for field in fields:
            binding = cls.model_validate(field)
            if binding.env_name in env_names:
                raise BridgeValidationError(field_path, "duplicate env_name")
            if binding.field_name in field_names:
                raise BridgeValidationError(field_path, "duplicate field_name")
            env_names.add(binding.env_name)
            field_names.add(binding.field_name)
            result.append(binding)
        return tuple(result)

    @classmethod
    def read_source(
        cls,
        source: Any,
        fields: Iterable["BridgeRuntimeEnvField"],
    ) -> dict[str, Any]:
        env = source if isinstance(source, dict) or hasattr(source, "__contains__") else {}
        data: dict[str, Any] = {}
        for binding in fields:
            try:
                if binding.env_name in env:
                    data[binding.field_name] = env[binding.env_name]
            except (TypeError, KeyError):
                continue
        return data


ProviderUsageLimitSettings.ENV_FIELDS = (
    BridgeRuntimeEnvField(
        env_name="BRIDGE_USAGE_LIMIT_RESET_UTC_OFFSET",
        field_name="usage_limit_reset_utc_offset",
    ),
)

RawLuaPolicy.ENV_FIELDS = (
    BridgeRuntimeEnvField(
        env_name="FACTORIOCTL_ALLOW_RAW_LUA",
        field_name="allow_raw_lua",
    ),
)

SdkSkillConfig.ENV_FIELDS = (
    BridgeRuntimeEnvField(env_name="BRIDGE_SDK_SKILLS", field_name="skills"),
)


class TelemetryRelaySettings(BridgeModel):
    """Typed remote telemetry relay settings resolved from CLI and environment."""

    relay_url: str | None = None
    relay_token: str | None = None

    ENV_FIELDS: ClassVar[tuple[BridgeRuntimeEnvField, ...]] = (
        BridgeRuntimeEnvField(env_name="RELAY_URL", field_name="relay_url"),
        BridgeRuntimeEnvField(env_name="RELAY_TOKEN", field_name="relay_token"),
    )

    @staticmethod
    def _optional_text(value: Any) -> str | None:
        text = str(value).strip() if value is not None else ""
        return text or None

    @field_validator("relay_url", "relay_token", mode="before")
    @classmethod
    def _coerce_optional_text(cls, value: Any) -> str | None:
        return cls._optional_text(value)

    @classmethod
    def from_sources(
        cls,
        *,
        cli_url: Any = None,
        cli_token: Any = None,
        env: Any = None,
    ) -> "TelemetryRelaySettings":
        env_data = BridgeRuntimeEnvField.read_source(env, cls.env_fields())

        return cls(
            relay_url=cls._optional_text(cli_url) or env_data.get("relay_url"),
            relay_token=cls._optional_text(cli_token) or env_data.get("relay_token"),
        )

    @classmethod
    def env_fields(cls) -> tuple[BridgeRuntimeEnvField, ...]:
        return BridgeRuntimeEnvField.validate_unique(
            cls.ENV_FIELDS,
            field_path="telemetry_relay_env_fields",
        )

    @property
    def enabled(self) -> bool:
        return self.relay_url is not None

    @property
    def ready(self) -> bool:
        return self.relay_url is not None and self.relay_token is not None


class BridgeRuntimeSettings(BridgeModel):
    """Typed runtime settings resolved from bridge environment variables."""

    max_turns: int = 200
    context_window_backoff_s: float = 900.0
    tick_timeout_s: float = 2400.0
    stream_idle_timeout_s: float = 300.0
    watchdog_same_failure_limit: int = 3
    watchdog_no_progress_timeout_s: float = 900.0
    mutating_tool_batch_window_s: float = 1.0

    ENV_FIELDS: ClassVar[tuple[BridgeRuntimeEnvField, ...]] = (
        BridgeRuntimeEnvField(env_name="BRIDGE_MAX_TURNS", field_name="max_turns"),
        BridgeRuntimeEnvField(
            env_name="BRIDGE_CONTEXT_WINDOW_BACKOFF_S",
            field_name="context_window_backoff_s",
        ),
        BridgeRuntimeEnvField(env_name="BRIDGE_TICK_TIMEOUT_S", field_name="tick_timeout_s"),
        BridgeRuntimeEnvField(
            env_name="BRIDGE_STREAM_IDLE_TIMEOUT_S",
            field_name="stream_idle_timeout_s",
        ),
        BridgeRuntimeEnvField(
            env_name="BRIDGE_WATCHDOG_SAME_FAILURE_LIMIT",
            field_name="watchdog_same_failure_limit",
        ),
        BridgeRuntimeEnvField(
            env_name="BRIDGE_WATCHDOG_NO_PROGRESS_TIMEOUT_S",
            field_name="watchdog_no_progress_timeout_s",
        ),
        BridgeRuntimeEnvField(
            env_name="BRIDGE_MUTATING_TOOL_BATCH_WINDOW_S",
            field_name="mutating_tool_batch_window_s",
        ),
    )
    INT_DEFAULTS: ClassVar[dict[str, tuple[int, int]]] = {
        "max_turns": (200, 1),
        "watchdog_same_failure_limit": (3, 0),
    }
    FLOAT_DEFAULTS: ClassVar[dict[str, tuple[float, float]]] = {
        "context_window_backoff_s": (900.0, 1.0),
        "tick_timeout_s": (2400.0, 1.0),
        "stream_idle_timeout_s": (300.0, 1.0),
        "watchdog_no_progress_timeout_s": (900.0, 0.0),
        "mutating_tool_batch_window_s": (1.0, 0.0),
    }

    @field_validator("max_turns", "watchdog_same_failure_limit", mode="before")
    @classmethod
    def _coerce_int_setting(cls, value: Any, info) -> int:
        default, minimum = cls.INT_DEFAULTS[info.field_name]
        try:
            parsed = int(value)
        except (TypeError, ValueError):
            return default
        return parsed if parsed >= minimum else default

    @field_validator(
        "context_window_backoff_s",
        "tick_timeout_s",
        "stream_idle_timeout_s",
        "watchdog_no_progress_timeout_s",
        "mutating_tool_batch_window_s",
        mode="before",
    )
    @classmethod
    def _coerce_float_setting(cls, value: Any, info) -> float:
        default, minimum = cls.FLOAT_DEFAULTS[info.field_name]
        try:
            parsed = float(value)
        except (TypeError, ValueError):
            return default
        return parsed if parsed >= minimum else default

    @classmethod
    def from_env(cls, env: Any) -> "BridgeRuntimeSettings":
        if isinstance(env, cls):
            return env
        return cls(**BridgeRuntimeEnvField.read_source(env, cls.env_fields()))

    @classmethod
    def env_fields(cls) -> tuple[BridgeRuntimeEnvField, ...]:
        return BridgeRuntimeEnvField.validate_unique(
            cls.ENV_FIELDS,
            field_path="runtime_env_fields",
        )


class LedgerRuntimeSettings(BridgeModel):
    """Typed ledger persistence settings resolved from bridge environment."""

    stale_bootstrap_ledger_max_age_s: float = 1800.0

    ENV_FIELDS: ClassVar[tuple[BridgeRuntimeEnvField, ...]] = (
        BridgeRuntimeEnvField(
            env_name="BRIDGE_STALE_BOOTSTRAP_LEDGER_MAX_AGE_S",
            field_name="stale_bootstrap_ledger_max_age_s",
        ),
    )

    @field_validator("stale_bootstrap_ledger_max_age_s", mode="before")
    @classmethod
    def _coerce_stale_bootstrap_max_age(cls, value: Any) -> float:
        try:
            parsed = float(value)
        except (TypeError, ValueError):
            return 1800.0
        return parsed if parsed >= 0.0 else 1800.0

    @classmethod
    def from_env(cls, env: Any) -> "LedgerRuntimeSettings":
        if isinstance(env, cls):
            return env
        return cls(**BridgeRuntimeEnvField.read_source(env, cls.env_fields()))

    @classmethod
    def env_fields(cls) -> tuple[BridgeRuntimeEnvField, ...]:
        return BridgeRuntimeEnvField.validate_unique(
            cls.ENV_FIELDS,
            field_path="ledger_env_fields",
        )


class LearningRuntimeSettings(BridgeModel):
    """Typed learning-memory settings resolved from bridge environment."""

    learning_dir: str | None = None

    ENV_FIELDS: ClassVar[tuple[BridgeRuntimeEnvField, ...]] = (
        BridgeRuntimeEnvField(env_name="BRIDGE_LEARNING_DIR", field_name="learning_dir"),
    )

    @field_validator("learning_dir", mode="before")
    @classmethod
    def _coerce_learning_dir(cls, value: Any) -> str | None:
        text = str(value).strip() if value is not None else ""
        return text or None

    @classmethod
    def from_env(cls, env: Any) -> "LearningRuntimeSettings":
        if isinstance(env, cls):
            return env
        return cls(**BridgeRuntimeEnvField.read_source(env, cls.env_fields()))

    @classmethod
    def env_fields(cls) -> tuple[BridgeRuntimeEnvField, ...]:
        return BridgeRuntimeEnvField.validate_unique(
            cls.ENV_FIELDS,
            field_path="learning_env_fields",
        )

    def resolved_learning_dir(self, project_root: Any) -> Path:
        if self.learning_dir:
            return Path(self.learning_dir)
        return Path(project_root) / ".factorioctl" / "learned"


class DotEnvAssignmentLine(BridgeModel):
    """Typed parse result for one bridge-local .env line."""

    line: str = ""
    key: str = ""
    value: str = ""
    valid: bool = False

    @field_validator("line", "key", "value", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "").strip()

    @classmethod
    def from_line(cls, value: Any) -> "DotEnvAssignmentLine":
        line = str(value or "").strip()
        if not line or line.startswith("#") or "=" not in line:
            return cls(line=line)
        key, raw_value = line.split("=", 1)
        key = key.strip()
        if not key:
            return cls(line=line, value=raw_value.strip())
        return cls(line=line, key=key, value=raw_value.strip(), valid=True)


class DotEnvFile(BridgeModel):
    """Typed parse result for the bridge-local .env file."""

    assignments: dict[str, str] = Field(default_factory=dict)

    @field_validator("assignments", mode="before")
    @classmethod
    def _coerce_assignments(cls, value: Any) -> dict[str, str]:
        if not isinstance(value, dict):
            return {}
        result: dict[str, str] = {}
        for raw_key, raw_value in value.items():
            key = str(raw_key).strip()
            value_text = str(raw_value).strip() if raw_value is not None else ""
            if key:
                result[key] = value_text
        return result

    @classmethod
    def from_text(cls, value: Any) -> "DotEnvFile":
        assignments: dict[str, str] = {}
        for line in BridgeTextLines.from_text(value, keep_blank=False).lines:
            assignment = DotEnvAssignmentLine.from_line(line)
            if assignment.valid:
                assignments[assignment.key] = assignment.value
        return cls(assignments=assignments)

    def apply_to_environ(self, environ: Any) -> None:
        if not hasattr(environ, "__contains__") or not hasattr(environ, "__setitem__"):
            return
        for key, value in self.assignments.items():
            if value and key not in environ:
                environ[key] = value


class FactorioPathSettings(BridgeModel):
    """Typed path-related bridge settings resolved from environment variables."""

    server_data: str | None = None
    mods_dir: str | None = None
    mcp_bin: str | None = None

    ENV_FIELDS: ClassVar[tuple[BridgeRuntimeEnvField, ...]] = (
        BridgeRuntimeEnvField(env_name="FACTORIO_SERVER_DATA", field_name="server_data"),
        BridgeRuntimeEnvField(env_name="FACTORIO_MODS_DIR", field_name="mods_dir"),
        BridgeRuntimeEnvField(env_name="FACTORIOCTL_MCP_BIN", field_name="mcp_bin"),
    )

    @field_validator("server_data", "mods_dir", "mcp_bin", mode="before")
    @classmethod
    def _coerce_optional_path(cls, value: Any) -> str | None:
        text = str(value).strip() if value is not None else ""
        return text or None

    @classmethod
    def from_env(cls, env: Any) -> "FactorioPathSettings":
        if isinstance(env, cls):
            return env
        return cls(**BridgeRuntimeEnvField.read_source(env, cls.env_fields()))

    @classmethod
    def env_fields(cls) -> tuple[BridgeRuntimeEnvField, ...]:
        return BridgeRuntimeEnvField.validate_unique(
            cls.ENV_FIELDS,
            field_path="factorio_path_env_fields",
        )

    @property
    def script_output_dir(self) -> Path | None:
        return Path(self.server_data) / "script-output" if self.server_data else None

    @property
    def mods_dir_path(self) -> Path | None:
        return Path(self.mods_dir) if self.mods_dir else None

    @property
    def mcp_bin_path(self) -> Path | None:
        return Path(self.mcp_bin) if self.mcp_bin else None


class FactorioModInfo(BridgeModel):
    """Typed view of a Factorio mod info.json file."""

    model_config = ConfigDict(frozen=True, extra="ignore")

    name: str = ""
    version: str = ""
    title: str = ""

    @field_validator("name", "version", "title", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value).strip() if value is not None else ""

    @classmethod
    def from_file_text(cls, value: Any) -> "FactorioModInfo":
        if isinstance(value, cls):
            return value
        data = _json_object_from_text(value, "mod_info")
        return cls.model_validate(data)

    @property
    def version_label(self) -> str:
        return self.version or "?"


class RconConnectionSettings(BridgeModel):
    """Typed RCON connection settings shared by bridge tools."""

    host: str = "localhost"
    port: int = 27015
    password: str = "factorio"

    ENV_FIELDS: ClassVar[tuple[BridgeRuntimeEnvField, ...]] = (
        BridgeRuntimeEnvField(env_name="FACTORIO_RCON_HOST", field_name="host"),
        BridgeRuntimeEnvField(env_name="FACTORIO_RCON_PORT", field_name="port"),
        BridgeRuntimeEnvField(env_name="FACTORIO_RCON_PASSWORD", field_name="password"),
    )

    @field_validator("host", mode="before")
    @classmethod
    def _coerce_host(cls, value: Any) -> str:
        return _input_text(value, "localhost")

    @field_validator("password", mode="before")
    @classmethod
    def _coerce_password(cls, value: Any) -> str:
        return _input_text(value, "factorio")

    @field_validator("port", mode="before")
    @classmethod
    def _coerce_port(cls, value: Any) -> int:
        try:
            port = int(value)
        except (TypeError, ValueError):
            return 27015
        return port if 1 <= port <= 65535 else 27015

    @classmethod
    def from_env(cls, env: Any) -> "RconConnectionSettings":
        if isinstance(env, cls):
            return env
        return cls(**BridgeRuntimeEnvField.read_source(env, cls.env_fields()))

    @classmethod
    def env_fields(cls) -> tuple[BridgeRuntimeEnvField, ...]:
        return BridgeRuntimeEnvField.validate_unique(
            cls.ENV_FIELDS,
            field_path="rcon_env_fields",
        )

    def to_env(self, *, agent_id: Any = None) -> dict[str, str]:
        result = {
            "FACTORIO_RCON_HOST": self.host,
            "FACTORIO_RCON_PORT": str(self.port),
            "FACTORIO_RCON_PASSWORD": self.password,
        }
        if agent_id is not None:
            result["FACTORIO_AGENT_ID"] = _input_text(agent_id, "default")
        return result


class FactorioMcpServerConfig(BridgeModel):
    """Typed Claude SDK stdio MCP config for the factorioctl server."""

    server_name: str = "factorioctl"
    command: str
    args: list[str] = Field(default_factory=list)
    rcon_host: str = "localhost"
    rcon_port: int = 27015
    rcon_password: str = "factorio"
    agent_id: str = "default"

    @field_validator("server_name", "rcon_host", "rcon_password", "agent_id", mode="before")
    @classmethod
    def _coerce_text_setting(cls, value: Any, info) -> str:
        defaults = {
            "server_name": "factorioctl",
            "rcon_host": "localhost",
            "rcon_password": "factorio",
            "agent_id": "default",
        }
        return _input_text(value, defaults[info.field_name])

    @field_validator("command", mode="before")
    @classmethod
    def _coerce_command(cls, value: Any) -> str:
        text = _input_text(value)
        if not text:
            raise ValueError("command is required")
        return text

    @field_validator("args", mode="before")
    @classmethod
    def _coerce_args(cls, value: Any) -> list[str]:
        return _coerce_str_or_list(value)

    @field_validator("rcon_port", mode="before")
    @classmethod
    def _coerce_port(cls, value: Any) -> int:
        return RconConnectionSettings(port=value).port

    def to_sdk_config(self) -> dict[str, dict[str, Any]]:
        rcon = RconConnectionSettings(
            host=self.rcon_host,
            port=self.rcon_port,
            password=self.rcon_password,
        )
        return {
            self.server_name: {
                "type": "stdio",
                "command": self.command,
                "args": list(self.args),
                "env": rcon.to_env(agent_id=self.agent_id),
            }
        }


class ResponseFormatSection(RemotePayloadModel):
    label: str
    color: str = "0.5,0.7,0.5"
    description: str = ""

    @field_validator("label", mode="before")
    @classmethod
    def _coerce_label(cls, value: Any) -> str:
        text = _input_text(value)
        if not text:
            raise ValueError("section label is required")
        return text

    @field_validator("color", mode="before")
    @classmethod
    def _coerce_color(cls, value: Any) -> str:
        return _input_text(value, "0.5,0.7,0.5")

    @field_validator("description", mode="before")
    @classmethod
    def _coerce_description(cls, value: Any) -> str:
        return _input_text(value)

    @classmethod
    def coerce(cls, value: Any) -> "ResponseFormatSection | None":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return None
        try:
            return cls.model_validate(value)
        except ValidationError:
            return None

    def to_dict(self) -> dict[str, Any]:
        result = dict(self.model_extra or {})
        result["label"] = self.label
        result["color"] = self.color
        if self.description:
            result["description"] = self.description
        return result


class ResponseFormatSectionCollection(BridgeModel):
    """Typed collection of response-format sections."""

    items: tuple[ResponseFormatSection, ...] = ()

    @field_validator("items", mode="before")
    @classmethod
    def _coerce_items(cls, value: Any) -> tuple[ResponseFormatSection, ...]:
        if value is None or isinstance(value, (str, bytes, dict, BaseModel)):
            return ()
        if not isinstance(value, Iterable):
            return ()
        sections: list[ResponseFormatSection] = []
        for item in value:
            section = (
                item
                if isinstance(item, ResponseFormatSection)
                else ResponseFormatSection.coerce(item)
            )
            if section:
                sections.append(section)
        return tuple(sections)

    @classmethod
    def from_value(cls, value: Any) -> "ResponseFormatSectionCollection":
        if isinstance(value, cls):
            return value
        return cls(items=value)

    def to_list(self) -> list[ResponseFormatSection]:
        return list(self.items)


class AgentResponseFormat(RemotePayloadModel):
    header_label: str = "STATUS"
    header_color: str = "1,0.8,0.2"
    action_label: str = "ACTIONS"
    action_color: str = "0.6,0.8,1"
    footer_label: str | None = None
    footer_color: str = "0.4,0.6,0.4"
    sections: list[ResponseFormatSection] = Field(default_factory=list)

    @field_validator(
        "header_label",
        "header_color",
        "action_label",
        "action_color",
        "footer_color",
        mode="before",
    )
    @classmethod
    def _coerce_required_text(cls, value: Any, info) -> str:
        defaults = {
            "header_label": "STATUS",
            "header_color": "1,0.8,0.2",
            "action_label": "ACTIONS",
            "action_color": "0.6,0.8,1",
            "footer_color": "0.4,0.6,0.4",
        }
        return _input_text(value, defaults[info.field_name])

    @field_validator("footer_label", mode="before")
    @classmethod
    def _coerce_footer_label(cls, value: Any) -> str | None:
        text = _input_text(value)
        return text or None

    @field_validator("sections", mode="before")
    @classmethod
    def _coerce_sections(cls, value: Any) -> list[ResponseFormatSection]:
        return ResponseFormatSectionCollection.from_value(value).to_list()

    @classmethod
    def coerce(cls, value: Any) -> "AgentResponseFormat | None":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return None
        try:
            return cls.model_validate(value)
        except ValidationError:
            return None

    def to_dict(self) -> dict[str, Any]:
        result = dict(self.model_extra or {})
        result.update({
            "header_label": self.header_label,
            "header_color": self.header_color,
            "action_label": self.action_label,
            "action_color": self.action_color,
        })
        if self.footer_label is not None:
            result["footer_label"] = self.footer_label
        result["footer_color"] = self.footer_color
        if self.sections:
            result["sections"] = [section.to_dict() for section in self.sections]
        return result


class ParsedResponseSection(RemotePayloadModel):
    label: str = ""
    color: str = ""
    text: str = ""

    @field_validator("label", "color", "text", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return _input_text(value)

    @classmethod
    def coerce(cls, value: Any) -> "ParsedResponseSection | None":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return None
        try:
            return cls.model_validate(value)
        except ValidationError:
            return None

    def to_dict(self) -> dict[str, Any]:
        result = dict(self.model_extra or {})
        result.update({
            "label": self.label,
            "color": self.color,
            "text": self.text,
        })
        return result


class AnomalyEvidence(BridgeModel):
    """Typed interpretation of an agent ANOMALY section."""

    kind: AnomalyEvidenceKind = AnomalyEvidenceKind.EMPTY
    raw_text: str = ""
    normalized_text: str = ""

    NOMINAL_VALUES: ClassVar[frozenset[str]] = frozenset({
        "none",
        "nominal",
        "na",
        "n a",
        "not applicable",
        "none detected",
        "none noted",
        "none observed",
        "no anomaly",
        "no anomalies",
        "no anomalies observed",
    })

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> AnomalyEvidenceKind:
        if isinstance(value, AnomalyEvidenceKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in AnomalyEvidenceKind:
                if normalized == kind.value:
                    return kind
        return AnomalyEvidenceKind.EMPTY

    @field_validator("raw_text", "normalized_text", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "").strip()

    @property
    def is_meaningful(self) -> bool:
        return self.kind == AnomalyEvidenceKind.MEANINGFUL

    @classmethod
    def from_text(cls, value: Any) -> "AnomalyEvidence":
        raw = str(value or "").strip()
        normalized = cls.normalize_text(raw)
        if not normalized:
            kind = AnomalyEvidenceKind.EMPTY
        elif cls.is_nominal_normalized_text(normalized):
            kind = AnomalyEvidenceKind.NOMINAL
        else:
            kind = AnomalyEvidenceKind.MEANINGFUL
        return cls(kind=kind, raw_text=raw, normalized_text=normalized)

    @staticmethod
    def normalize_text(value: Any) -> str:
        normalized = re.sub(r"[^a-z0-9 ]+", "", str(value or "").strip().lower())
        return re.sub(r"\s+", " ", normalized)

    @classmethod
    def is_nominal_normalized_text(cls, normalized: str) -> bool:
        return (
            normalized in cls.NOMINAL_VALUES
            or normalized.startswith(("no anomaly", "no anomalies", "none ", "nominal"))
        )


class ParsedAgentResponse(BridgeModel):
    header: ParsedResponseSection | None = None
    body: str = ""
    actions: list[str] = Field(default_factory=list)
    footer: ParsedResponseSection | None = None
    data: dict[str, ParsedResponseSection] = Field(default_factory=dict)

    @field_validator("body", mode="before")
    @classmethod
    def _coerce_body(cls, value: Any) -> str:
        return _input_text(value)

    @field_validator("actions", mode="before")
    @classmethod
    def _coerce_actions(cls, value: Any) -> list[str]:
        return _coerce_str_list(value)

    @field_validator("header", "footer", mode="before")
    @classmethod
    def _coerce_section(cls, value: Any) -> ParsedResponseSection | None:
        return ParsedResponseSection.coerce(value)

    @field_validator("data", mode="before")
    @classmethod
    def _coerce_data(cls, value: Any) -> dict[str, ParsedResponseSection]:
        if not isinstance(value, dict):
            return {}
        result: dict[str, ParsedResponseSection] = {}
        for key, item in value.items():
            section = ParsedResponseSection.coerce(item)
            if section:
                label = _input_text(key, section.label)
                if label:
                    if not section.label:
                        section = section.model_copy(update={"label": label})
                    result[label] = section
        return result

    @classmethod
    def body_only(cls, value: Any) -> "ParsedAgentResponse":
        return cls(body=str(value) if value is not None else "")

    @classmethod
    def from_text(cls, value: Any) -> "ParsedAgentResponse":
        text = str(value if value is not None else "")
        matches = list(_AGENT_RESPONSE_SECTION_RE.finditer(text))
        if not matches:
            return cls.body_only(text)

        header: ParsedResponseSection | None = None
        body = ""
        actions: list[str] = []
        footer: ParsedResponseSection | None = None
        data: dict[str, ParsedResponseSection] = {}

        for index, match in enumerate(matches):
            color = match.group(1)
            label = match.group(2).strip()
            content_start = match.end()
            content_end = matches[index + 1].start() if index + 1 < len(matches) else len(text)
            content = text[content_start:content_end].strip()

            if index == 0:
                header_body = TextMarkerSplit.from_text(content, "\n\n")
                header = ParsedResponseSection(
                    label=label,
                    color=color,
                    text=header_body.before.strip(),
                )
                if header_body.matched and header_body.after.strip():
                    body = header_body.after.strip()
            elif "ACTION" in label.upper():
                for line in BridgeTextLines.from_text(content, keep_blank=False).lines:
                    item = line.strip().lstrip("- ").strip()
                    if item:
                        actions.append(item)
            elif label.upper() in {"FILED", "CLASSIFIED", "END"}:
                footer = ParsedResponseSection(label=label, color=color, text=content)
            else:
                data[label] = ParsedResponseSection(label=label, color=color, text=content)

        if not body:
            body = header.text if header else text

        return cls(
            header=header,
            body=body,
            actions=actions,
            footer=footer,
            data=data,
        )

    @staticmethod
    def sanitize_text(value: Any) -> str:
        """Remove markdown artifacts while preserving Factorio rich text tags."""
        text = str(value if value is not None else "")
        text = re.sub(r"\*\*(.+?)\*\*", r"\1", text)
        text = re.sub(r"^#{1,3}\s+", "", text, flags=re.MULTILINE)
        text = re.sub(r"```\w*\n?", "", text)
        return text.strip()

    @classmethod
    def from_mapping(cls, value: Any) -> "ParsedAgentResponse":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return cls.body_only(value)
        try:
            return cls.model_validate(value)
        except ValidationError:
            return cls.body_only(value)

    def anomaly_text(self) -> str:
        section = self.data.get("ANOMALY")
        return section.text if section else ""

    def anomaly_evidence(self) -> AnomalyEvidence:
        return AnomalyEvidence.from_text(self.anomaly_text())

    @staticmethod
    def normalized_anomaly_text(value: Any) -> str:
        return AnomalyEvidence.normalize_text(value)

    @classmethod
    def is_meaningful_anomaly_text(cls, value: Any) -> bool:
        return AnomalyEvidence.from_text(value).is_meaningful

    def meaningful_anomaly_text(self) -> str:
        evidence = self.anomaly_evidence()
        return evidence.raw_text if evidence.is_meaningful else ""

    def to_dict(self) -> dict[str, Any]:
        result: dict[str, Any] = {}
        if self.header:
            result["header"] = self.header.to_dict()
        if self.body or not any((self.header, self.actions, self.footer, self.data)):
            result["body"] = self.body
        if self.actions:
            result["actions"] = list(self.actions)
        if self.footer:
            result["footer"] = self.footer.to_dict()
        if self.data:
            result["data"] = {
                label: section.to_dict()
                for label, section in self.data.items()
            }
        return result


class AgentProfile(BridgeModel):
    name: str
    system_prompt: str
    model: str | None = None
    planner_model: str | None = None
    max_turns: int | None = None
    telemetry_name: str | None = None
    planet: str | None = None
    group: str | None = None
    heartbeat_interval: int | None = None
    planner_interval: int | None = None
    reflect_interval: int | None = None
    autonomy_requires_player: bool | None = None
    sdk_skills: str | list[str] | None = None
    response_format: AgentResponseFormat | None = None
    extra: dict[str, Any] = Field(default_factory=dict)

    @field_validator("sdk_skills", mode="before")
    @classmethod
    def _coerce_sdk_skills(cls, value: Any) -> str | list[str] | None:
        return AgentProfileSdkSkills.from_value(value).to_profile_value()

    @classmethod
    def from_mapping(cls, value: Any) -> "AgentProfile":
        if isinstance(value, cls):
            return value
        data = _mapping(value, "agent")
        known = {
            "name",
            "system_prompt",
            "model",
            "planner_model",
            "max_turns",
            "telemetry_name",
            "planet",
            "group",
            "heartbeat_interval",
            "planner_interval",
            "reflect_interval",
            "autonomy_requires_player",
            "sdk_skills",
            "response_format",
        }
        extra = {key: item for key, item in data.items() if key not in known}
        try:
            return cls(
                name=_required_str(data, "name"),
                system_prompt=_required_str(data, "system_prompt"),
                model=_optional_str(data, "model"),
                planner_model=_optional_str(data, "planner_model"),
                max_turns=_optional_int(data, "max_turns"),
                telemetry_name=_optional_str(data, "telemetry_name"),
                planet=_optional_str(data, "planet"),
                group=_optional_str(data, "group"),
                heartbeat_interval=_optional_int(data, "heartbeat_interval"),
                planner_interval=_optional_int(data, "planner_interval"),
                reflect_interval=_optional_int(data, "reflect_interval"),
                autonomy_requires_player=_optional_bool(data, "autonomy_requires_player"),
                sdk_skills=data.get("sdk_skills"),
                response_format=_optional_response_format(data),
                extra=extra,
            )
        except ValidationError as exc:
            for error in exc.errors():
                loc = error.get("loc", ())
                if loc and loc[0] == "sdk_skills":
                    raise BridgeValidationError(
                        "sdk_skills",
                        "expected string or list of strings",
                    ) from exc
            raise

    @classmethod
    def coerce(cls, value: Any) -> "AgentProfile":
        if isinstance(value, cls):
            return value
        return cls.from_mapping(value)

    @classmethod
    def from_file_text(cls, value: str) -> "AgentProfile":
        if isinstance(value, cls):
            return value
        data = _json_object_from_text(value, "agent")
        return cls.from_mapping(data)

    @property
    def planet_name(self) -> str:
        return self.planet or "nauvis"

    @property
    def registration_label(self) -> str:
        return (self.planet or self.name).capitalize()

    def sort_key(self, planet_order: dict[str, int]) -> tuple[int, str]:
        return (planet_order.get(self.planet_name, 99), self.name)

    def with_system_prompt(self, system_prompt: str) -> "AgentProfile":
        return AgentProfile(
            name=self.name,
            system_prompt=system_prompt,
            model=self.model,
            planner_model=self.planner_model,
            max_turns=self.max_turns,
            telemetry_name=self.telemetry_name,
            planet=self.planet,
            group=self.group,
            heartbeat_interval=self.heartbeat_interval,
            planner_interval=self.planner_interval,
            reflect_interval=self.reflect_interval,
            autonomy_requires_player=self.autonomy_requires_player,
            sdk_skills=self.sdk_skills,
            response_format=self.response_format,
            extra=dict(self.extra),
        )

    def to_dict(self) -> dict[str, Any]:
        result = dict(self.extra)
        result["name"] = self.name
        result["system_prompt"] = self.system_prompt
        optional = {
            "model": self.model,
            "planner_model": self.planner_model,
            "max_turns": self.max_turns,
            "telemetry_name": self.telemetry_name,
            "planet": self.planet,
            "group": self.group,
            "heartbeat_interval": self.heartbeat_interval,
            "planner_interval": self.planner_interval,
            "reflect_interval": self.reflect_interval,
            "autonomy_requires_player": self.autonomy_requires_player,
            "sdk_skills": self.sdk_skills,
            "response_format": self.response_format,
        }
        for key, value in optional.items():
            if value is not None:
                if key == "response_format":
                    result[key] = value.to_dict()
                else:
                    result[key] = value
        return result


class AgentNameSelection(BridgeModel):
    """Typed comma/list selector for explicit multi-agent startup."""

    names: list[str] = Field(default_factory=list)

    @field_validator("names", mode="before")
    @classmethod
    def _coerce_names(cls, value: Any) -> list[str]:
        return CommaSeparatedItems.from_value(value).to_list()

    @classmethod
    def from_cli_arg(cls, value: Any) -> "AgentNameSelection":
        return cls(names=value)

    @property
    def filter_or_none(self) -> list[str] | None:
        return list(self.names) if self.names else None


class AgentRuntimeConfig(BridgeModel):
    """Resolved per-agent runtime config after profile, CLI, and env overlays."""

    profile: AgentProfile
    model: str = "haiku"
    planner_model: str = "sonnet"
    max_turns: int = 200
    skill_config: SdkSkillConfig = Field(default_factory=SdkSkillConfig)
    telemetry_name: str
    heartbeat_interval: float = 0.0
    planner_interval: int = 5
    reflect_interval: int = 16
    autonomy_requires_player: bool = True

    @field_validator("model", "planner_model", "telemetry_name", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "").strip()

    @field_validator("max_turns", "planner_interval", "reflect_interval", mode="before")
    @classmethod
    def _coerce_positive_int(cls, value: Any, info) -> int:
        defaults = {
            "max_turns": 200,
            "planner_interval": 5,
            "reflect_interval": 16,
        }
        minimums = {
            "max_turns": 1,
            "planner_interval": 1,
            "reflect_interval": 1,
        }
        default = defaults[info.field_name]
        minimum = minimums[info.field_name]
        try:
            parsed = int(value)
        except (TypeError, ValueError):
            return default
        return parsed if parsed >= minimum else default

    @field_validator("heartbeat_interval", mode="before")
    @classmethod
    def _coerce_heartbeat_interval(cls, value: Any) -> float:
        try:
            parsed = float(value)
        except (TypeError, ValueError):
            return 0.0
        return parsed if parsed >= 0.0 else 0.0

    @field_validator("autonomy_requires_player", mode="before")
    @classmethod
    def _coerce_bool(cls, value: Any) -> bool:
        if isinstance(value, bool):
            return value
        text = str(value).strip().lower()
        if text in {"1", "true", "yes", "on"}:
            return True
        if text in {"0", "false", "no", "off"}:
            return False
        return True

    @classmethod
    def from_sources(
        cls,
        profile: AgentProfile | dict[str, Any],
        *,
        cli_model: Any = None,
        cli_max_turns: Any = None,
        cli_sdk_skills: Any = None,
        default_sdk_skills: Any = None,
        heartbeat_interval: Any = 0.0,
        planner_interval: Any = 5,
        autonomy_requires_player: Any = True,
        runtime_settings: BridgeRuntimeSettings | None = None,
        env: Any = None,
    ) -> "AgentRuntimeConfig":
        resolved_profile = AgentProfile.coerce(profile)
        settings = runtime_settings or BridgeRuntimeSettings.from_env(env or {})
        max_turns_source = (
            cli_max_turns
            if cli_max_turns is not None
            else resolved_profile.max_turns
        )
        max_turns = (
            BridgeRuntimeSettings(max_turns=max_turns_source).max_turns
            if max_turns_source is not None
            else settings.max_turns
        )
        sdk_skill_value = (
            cli_sdk_skills
            if cli_sdk_skills is not None
            else resolved_profile.sdk_skills
        )
        return cls(
            profile=resolved_profile,
            model=cli_model or resolved_profile.model or "haiku",
            planner_model=resolved_profile.planner_model or "sonnet",
            max_turns=max_turns,
            skill_config=SdkSkillConfig.from_env(
                env or {},
                value=sdk_skill_value,
                default=default_sdk_skills,
            ),
            telemetry_name=resolved_profile.telemetry_name or resolved_profile.name,
            heartbeat_interval=(
                resolved_profile.heartbeat_interval
                if resolved_profile.heartbeat_interval is not None
                else heartbeat_interval
            ),
            planner_interval=(
                resolved_profile.planner_interval
                if resolved_profile.planner_interval is not None
                else planner_interval
            ),
            reflect_interval=(
                resolved_profile.reflect_interval
                if resolved_profile.reflect_interval is not None
                else 16
            ),
            autonomy_requires_player=(
                resolved_profile.autonomy_requires_player
                if resolved_profile.autonomy_requires_player is not None
                else autonomy_requires_player
            ),
        )

    @property
    def agent_name(self) -> str:
        return self.profile.name

    @property
    def system_prompt(self) -> str:
        return self.profile.system_prompt

    @property
    def planet_name(self) -> str:
        return self.profile.planet_name

    @property
    def sdk_skills(self) -> list[str] | str:
        return self.skill_config.sdk_value


class AgentInvocationConfig(BridgeModel):
    """Resolved config for one Claude SDK invocation."""

    agent_name: str = "default"
    telemetry_name: str | None = None
    response_to: str | None = None
    system_prompt: str
    session_id: str | None = None
    model: str | None = None
    max_turns: int = 200
    skill_config: SdkSkillConfig = Field(default_factory=SdkSkillConfig)
    read_only_tools: bool = False

    @field_validator(
        "agent_name",
        "telemetry_name",
        "response_to",
        "system_prompt",
        "session_id",
        "model",
        mode="before",
    )
    @classmethod
    def _coerce_optional_text(cls, value: Any, info) -> str | None:
        text = str(value or "").strip()
        if info.field_name in {"telemetry_name", "response_to", "session_id", "model"}:
            return text or None
        return text or "default"

    @field_validator("max_turns", mode="before")
    @classmethod
    def _coerce_max_turns(cls, value: Any) -> int:
        return BridgeRuntimeSettings(max_turns=value).max_turns

    @field_validator("read_only_tools", mode="before")
    @classmethod
    def _coerce_read_only(cls, value: Any) -> bool:
        if isinstance(value, bool):
            return value
        text = str(value).strip().lower()
        if text in {"1", "true", "yes", "on"}:
            return True
        if text in {"0", "false", "no", "off"}:
            return False
        return False

    @classmethod
    def from_sources(
        cls,
        *,
        system_prompt: Any,
        agent_name: Any = "default",
        telemetry_name: Any = None,
        response_to: Any = None,
        session_id: Any = None,
        model: Any = None,
        max_turns: Any = None,
        sdk_skills: Any = None,
        read_only_tools: Any = False,
        default_sdk_skills: Any = None,
        runtime_settings: BridgeRuntimeSettings | None = None,
        env: Any = None,
    ) -> "AgentInvocationConfig":
        settings = runtime_settings or BridgeRuntimeSettings.from_env(env or {})
        resolved_max_turns = (
            BridgeRuntimeSettings(max_turns=max_turns).max_turns
            if max_turns is not None
            else settings.max_turns
        )
        return cls(
            agent_name=agent_name,
            telemetry_name=telemetry_name,
            response_to=response_to,
            system_prompt=system_prompt,
            session_id=session_id,
            model=model,
            max_turns=resolved_max_turns,
            skill_config=SdkSkillConfig.from_env(
                env or {},
                value=sdk_skills,
                default=default_sdk_skills,
            ),
            read_only_tools=read_only_tools,
        )

    @property
    def telemetry_label(self) -> str:
        return self.telemetry_name or self.agent_name

    @property
    def rcon_target(self) -> str:
        return self.response_to or self.agent_name

    @property
    def sdk_skills(self) -> list[str] | str:
        return self.skill_config.sdk_value

    @property
    def resume_tag(self) -> str:
        return (
            f" (resume {self.session_id[:8]}...)"
            if self.session_id
            else " (new session)"
        )

    def to_sdk_options_spec(
        self,
        *,
        mcp_servers: Any,
        env: Any,
        project_root: Any,
    ) -> "AgentClaudeOptionsSpec":
        return AgentClaudeOptionsSpec(
            system_prompt=self.system_prompt,
            model=self.model,
            max_turns=self.max_turns,
            mcp_servers=mcp_servers,
            tools=self.skill_config.claude_tools,
            disallowed_tools=RawLuaPolicy.from_env(env).disallowed_tools,
            permission_mode="bypassPermissions",
            resume=self.session_id,
            setting_sources=self.skill_config.setting_sources,
            cwd=project_root,
            skills=self.sdk_skills,
        )


class AgentClaudeOptionsSpec(BridgeModel):
    """Typed bridge-owned spec for constructing ClaudeAgentOptions."""

    system_prompt: str
    model: str | None = None
    max_turns: int = 200
    mcp_servers: Any = Field(default_factory=dict)
    strict_mcp_config: bool = True
    tools: list[str] = Field(default_factory=list)
    disallowed_tools: list[str] = Field(default_factory=list)
    permission_mode: str = "bypassPermissions"
    resume: str | None = None
    setting_sources: list[str] = Field(default_factory=list)
    cwd: str
    skills: list[str] | str | None = None

    @field_validator("system_prompt", "permission_mode", mode="before")
    @classmethod
    def _coerce_required_text(cls, value: Any, info) -> str:
        text = str(value or "").strip()
        if text:
            return text
        return "default" if info.field_name == "system_prompt" else "bypassPermissions"

    @field_validator("model", "resume", mode="before")
    @classmethod
    def _coerce_optional_text(cls, value: Any) -> str | None:
        text = str(value or "").strip()
        return text or None

    @field_validator("max_turns", mode="before")
    @classmethod
    def _coerce_max_turns(cls, value: Any) -> int:
        return BridgeRuntimeSettings(max_turns=value).max_turns

    @field_validator("strict_mcp_config", mode="before")
    @classmethod
    def _coerce_strict_mcp_config(cls, value: Any) -> bool:
        if isinstance(value, bool):
            return value
        text = str(value).strip().lower()
        if text in {"0", "false", "no", "off"}:
            return False
        return True

    @field_validator("tools", "disallowed_tools", "setting_sources", mode="before")
    @classmethod
    def _coerce_string_list(cls, value: Any) -> list[str]:
        return _coerce_str_or_list(value)

    @field_validator("cwd", mode="before")
    @classmethod
    def _coerce_cwd(cls, value: Any) -> str:
        text = str(value or "").strip()
        return text or "."


class AgentMessageResult(BridgeModel):
    """Typed result for a handled agent message."""

    session_id: str | None = None
    reset_session: bool = False

    @field_validator("session_id", mode="before")
    @classmethod
    def _coerce_session_id(cls, value: Any) -> str | None:
        text = str(value or "").strip()
        return text or None

    @field_validator("reset_session", mode="before")
    @classmethod
    def _coerce_reset_session(cls, value: Any) -> bool:
        if isinstance(value, bool):
            return value
        text = str(value).strip().lower()
        if text in {"1", "true", "yes", "on"}:
            return True
        if text in {"0", "false", "no", "off"}:
            return False
        return False

    @classmethod
    def keep_session(cls, session_id: Any = None) -> "AgentMessageResult":
        return cls(session_id=session_id, reset_session=False)

    @classmethod
    def reset(cls) -> "AgentMessageResult":
        return cls(session_id=None, reset_session=True)

    def to_legacy_session_value(self, reset_token: str) -> str | None:
        return reset_token if self.reset_session else self.session_id


class LedgerState(BridgeModel):
    objective: str
    plan_steps: list[str] = Field(default_factory=list)
    progress_notes: list[str] = Field(default_factory=list)
    updated_at: str = ""
    signal: ProgressSignal = ProgressSignal.NONE
    status: LedgerStatus = LedgerStatus.NONE
    next_required_mode: LedgerNextRequiredMode = LedgerNextRequiredMode.NONE
    blocker: str = ""

    @classmethod
    def default(cls) -> "LedgerState":
        return cls(
            objective="",
            plan_steps=[],
            progress_notes=[],
            updated_at="",
            signal=ProgressSignal.NONE,
            status=LedgerStatus.NONE,
            next_required_mode=LedgerNextRequiredMode.NONE,
            blocker="",
        )

    @classmethod
    def from_mapping(cls, value: Any) -> "LedgerState":
        if isinstance(value, cls):
            return value
        data = _mapping(value, "ledger")
        return cls(
            objective=_required_any_str(data, "objective"),
            plan_steps=_required_str_list(data, "plan_steps"),
            progress_notes=_required_str_list(data, "progress_notes"),
            updated_at=_optional_str(data, "updated_at") or "",
            signal=progress_signal(data.get("signal")),
            status=ledger_status(data.get("status")),
            next_required_mode=ledger_next_required_mode(data.get("next_required_mode")),
            blocker=_optional_str(data, "blocker") or "",
        )

    @classmethod
    def coerce(cls, value: Any) -> "LedgerState":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return cls.default()
        objective = value.get("objective", "")
        updated_at = value.get("updated_at", "")
        blocker = value.get("blocker", "")
        return cls(
            objective=objective if isinstance(objective, str) else "",
            plan_steps=_coerce_str_list(value.get("plan_steps", [])),
            progress_notes=_coerce_str_list(value.get("progress_notes", [])),
            updated_at=updated_at if isinstance(updated_at, str) else "",
            signal=progress_signal(value.get("signal")),
            status=ledger_status(value.get("status")),
            next_required_mode=ledger_next_required_mode(value.get("next_required_mode")),
            blocker=blocker.strip() if isinstance(blocker, str) else "",
        )

    @classmethod
    def normalized(
        cls,
        value: Any,
        *,
        progress_should_drop: Callable[[str], bool] | None = None,
    ) -> "LedgerState":
        return cls.coerce(value).cleaned(progress_should_drop=progress_should_drop)

    @classmethod
    def from_file_text(
        cls,
        value: str,
        *,
        progress_should_drop: Callable[[str], bool] | None = None,
    ) -> "LedgerState":
        if isinstance(value, cls):
            return cls.normalized(
                value,
                progress_should_drop=progress_should_drop,
            )
        data = _json_object_from_text(value, "ledger")
        return cls.normalized(data, progress_should_drop=progress_should_drop)

    def cleaned(
        self,
        *,
        progress_should_drop: Callable[[str], bool] | None = None,
    ) -> "LedgerState":
        notes: list[str] = []
        for note in self.progress_notes:
            try:
                should_drop = bool(progress_should_drop(note)) if progress_should_drop else False
            except Exception:
                should_drop = False
            if not should_drop:
                notes.append(note)
        return LedgerState(
            objective=self.objective,
            plan_steps=list(self.plan_steps),
            progress_notes=notes,
            updated_at=self.updated_at,
            signal=self.signal,
            status=self.status,
            next_required_mode=self.next_required_mode,
            blocker=self.blocker,
        )

    def merged_with(
        self,
        update: Any,
        *,
        updated_at: str = "",
        max_progress_notes: int = 10,
        progress_should_drop: Callable[[str], bool] | None = None,
    ) -> "LedgerState":
        ledger_update = update if isinstance(update, LedgerUpdate) else LedgerUpdate.coerce(update)
        objective = self.objective
        plan_steps = list(self.plan_steps)
        progress_notes = list(self.progress_notes)
        signal = self.signal
        status = self.status
        next_required_mode = self.next_required_mode
        blocker = self.blocker

        if ledger_update.objective:
            objective = ledger_update.objective
            plan_steps = list(ledger_update.plan_steps)
            status = ledger_update.status
            next_required_mode = ledger_update.next_required_mode
            blocker = ledger_update.blocker
        elif ledger_update.plan_steps:
            plan_steps = list(ledger_update.plan_steps)
        if ledger_update.signal != ProgressSignal.NONE:
            signal = ledger_update.signal
        if ledger_update.status != LedgerStatus.NONE:
            status = ledger_update.status
        if ledger_update.next_required_mode != LedgerNextRequiredMode.NONE:
            next_required_mode = ledger_update.next_required_mode
        if ledger_update.blocker:
            blocker = ledger_update.blocker

        try:
            drop_progress = (
                bool(progress_should_drop(ledger_update.progress))
                if progress_should_drop and ledger_update.progress
                else False
            )
        except Exception:
            drop_progress = False
        if ledger_update.progress and not drop_progress:
            progress_notes.append(ledger_update.progress)

        try:
            limit = int(max_progress_notes)
        except (TypeError, ValueError):
            limit = 10
        limit = max(0, limit)
        if limit:
            progress_notes = progress_notes[-limit:]
        else:
            progress_notes = []

        return LedgerState(
            objective=objective,
            plan_steps=plan_steps,
            progress_notes=progress_notes,
            updated_at=updated_at.strip() if isinstance(updated_at, str) and updated_at.strip() else self.updated_at,
            signal=signal,
            status=status,
            next_required_mode=next_required_mode,
            blocker=blocker,
        ).cleaned(progress_should_drop=progress_should_drop)

    def to_dict(self) -> dict[str, Any]:
        result = {
            "objective": self.objective,
            "plan_steps": list(self.plan_steps),
            "progress_notes": list(self.progress_notes),
            "updated_at": self.updated_at,
        }
        if self.signal != ProgressSignal.NONE:
            result["signal"] = self.signal.value
        if self.status != LedgerStatus.NONE:
            result["status"] = self.status.value
        if self.next_required_mode != LedgerNextRequiredMode.NONE:
            result["next_required_mode"] = self.next_required_mode.value
        if self.blocker:
            result["blocker"] = self.blocker
        return result

    def to_json_line(self) -> str:
        return json.dumps(self.to_dict()) + "\n"

    def readiness_evidence(self) -> LedgerReadinessEvidence:
        return LedgerReadinessEvidence.from_ledger(self)

    def has_execution_ready_plan(self) -> bool:
        return self.readiness_evidence().is_ready

    def active_text(self) -> str:
        parts = [self.objective]
        parts.extend(self.plan_steps)
        return "\n".join(parts).lower()

    def progress_text(self) -> str:
        return "\n".join(self.progress_notes).lower()

    def has_automation_enabling_setup_context(self) -> bool:
        """Return true for bounded setup steps that create automation parts."""
        text = self.active_text()
        if not text:
            return False
        return any(
            marker in text
            for marker in (
                "bootstrap",
                "build_recipe_assembler_cell",
                "durable automation",
                "furnace output",
                "inserter",
                "one-time",
                "initial",
                "plan_recipe_assembler_cell",
                "plate output",
                "recipe assembler",
            )
        )

    def has_durable_recovery_context(self) -> bool:
        """Return true when manual recovery is paired with durable logistics."""
        text = self.active_text()
        if not text:
            return False
        return any(
            marker in text
            for marker in (
                "repair_fuel_sustainability",
                "route_belt",
                "build_fuel_supply",
            )
        )

    def has_stale_manual_automation_plan(
        self,
        live_state: LiveState | None = None,
    ) -> bool:
        """Return true when a plan repeats manual transfers instead of automation.

        Once automation infrastructure exists, plans that only craft/extract/feed
        by hand preserve the wrong end state. This predicate is intentionally
        narrow: science/research manual plans are always stale; fuel/feed plans
        are stale as soon as live state has a real logistics/power footprint;
        output transfer plans need repeated-loop evidence in progress text.
        It yields to durable automation controllers when they are present.
        """
        text = self.active_text()
        if not text:
            return False
        durable_markers = (
            "execute_direct_smelter",
            "execute_edge_miner",
            "repair_fuel_sustainability",
            "diagnose_fuel_sustainability",
            "build_fuel_supply",
            "route_belt",
            "plan_automation_science",
            "build_automation_science",
            "plan_recipe_assembler_cell",
            "build_recipe_assembler_cell",
            "build_assembler_feed",
            "plan_machine_output",
            "build_assembler_output",
            "build_lab_feed",
        )
        if any(marker in text for marker in durable_markers):
            return False
        manual_markers = (
            "feed_lab_from_inventory",
            "hand_feed_furnace",
            "insert_items",
            "extract_items",
            "craft ",
            "craft(",
            "craft_",
        )
        if not any(marker in text for marker in manual_markers):
            return False
        mentions_science = any(
            marker in text
            for marker in (
                "automation-science-pack",
                "science pack",
                "science-pack",
                "research",
                "lab",
            )
        )
        mentions_manual_science_component = any(
            marker in text
            for marker in (
                "iron-gear-wheel",
                "copper-cable",
                "electronic-circuit",
            )
        ) and any(
            marker in text
            for marker in (
                "science",
                "research",
                "lab",
                "automation-science",
            )
        )
        if mentions_science or mentions_manual_science_component:
            return True

        automation_capable = bool(
            live_state and live_state.has_automation_capable_footprint()
        )
        mentions_manual_feed = (
            any(marker in text for marker in ("insert_items", "hand_feed_furnace"))
            and any(
                marker in text
                for marker in (
                    "coal",
                    "fuel",
                    "boiler",
                    "burner",
                    "furnace",
                    "furnace_source",
                    "iron-ore",
                    "copper-ore",
                )
            )
        )
        if automation_capable and mentions_manual_feed:
            return not self.has_durable_recovery_context()

        progress = self.progress_text()
        repeated_loop_text = "\n".join([text, progress])
        repeated_loop_markers = (
            "manual cycle",
            "manual extraction",
            "manual transfer",
            "manual feeding",
            "again",
            "repeated",
            "recurring",
            "continues",
            "still requires manual",
            "runs out",
            "ran out",
            "exhausted",
            "jammed",
            "full_output",
            "reloaded",
            "refueled",
        )

        def has_loop_marker(marker: str) -> bool:
            return re.search(
                rf"(?<![a-z0-9_-]){re.escape(marker)}(?![a-z0-9_-])",
                repeated_loop_text,
            ) is not None

        if not any(has_loop_marker(marker) for marker in repeated_loop_markers):
            return False

        mentions_fuel_loop = (
            any(
                marker in text
                for marker in ("coal", "fuel", "boiler", "burner", "furnace")
            )
            and any(marker in text for marker in ("insert_items", "hand_feed_furnace"))
        )
        mentions_output_loop = (
            "extract_items" in text
            and any(
                marker in repeated_loop_text
                for marker in ("plate", "output", "full_output", "chest", "jam")
            )
        )
        return mentions_fuel_loop or mentions_output_loop

    def age_seconds(self, *, now: datetime | None = None) -> float | None:
        if not self.updated_at:
            return None
        try:
            updated = datetime.fromisoformat(self.updated_at)
        except ValueError:
            return None

        reference = now
        if reference is None:
            reference = datetime.now(updated.tzinfo) if updated.tzinfo else datetime.now()
        elif updated.tzinfo and reference.tzinfo:
            reference = reference.astimezone(updated.tzinfo)
        elif updated.tzinfo and reference.tzinfo is None:
            reference = reference.replace(tzinfo=updated.tzinfo)
        elif updated.tzinfo is None and reference.tzinfo:
            reference = reference.replace(tzinfo=None)
        return max(0.0, (reference - updated).total_seconds())

    def bootstrap_staleness_evidence(
        self,
        *,
        max_age_s: float,
        now: datetime | None = None,
    ) -> "LedgerStalenessEvidence":
        return LedgerStalenessEvidence.from_bootstrap_policy(
            self,
            max_age_s=max_age_s,
            now=now,
        )

    def live_state_completion_evidence(self, live_state: Any) -> LiveCompletionEvidence:
        """Return typed evidence when live state proves this ledger is stale.

        The rules are intentionally conservative: only well-known early-game
        objectives with direct world evidence trigger an automatic planner tick.
        """
        return LiveCompletionEvidence.from_ledger_and_live_state(self, live_state)

    def render(self, *, recent_progress_count: int = 3) -> str:
        objective = self.objective.strip()
        if not objective:
            return ""

        lines = [
            f"Continuity ledger: continue the committed objective, do not restart it: {objective}",
        ]
        if self.status != LedgerStatus.NONE:
            lines.append(f"Status: {self.status.value}")
        if self.next_required_mode != LedgerNextRequiredMode.NONE:
            lines.append(f"Next required mode: {self.next_required_mode.value}")
        if self.blocker:
            lines.append(f"Blocker: {self.blocker}")
        if self.plan_steps:
            lines.append("Plan:")
            for index, step in enumerate(self.plan_steps, start=1):
                lines.append(f"{index}. {step}")
        progress_notes = self.progress_notes[-recent_progress_count:]
        if progress_notes:
            lines.append("Recent progress:")
            for note in progress_notes:
                lines.append(f"- {note}")
        return "\n".join(lines)


class AutonomyPromptInput(BridgeModel):
    """Typed source context for assembling one autonomy tick prompt."""

    mode: AutonomyMode = AutonomyMode.PLAN
    ledger: LedgerState = Field(default_factory=LedgerState.default)
    live_state: LiveState = Field(default_factory=LiveState)
    memory_text: str = ""
    learned_text: str = ""
    live_completion_reason: str = ""
    planner_advisory: str = ""

    @field_validator("mode", mode="before")
    @classmethod
    def _coerce_mode(cls, value: Any) -> AutonomyMode:
        return autonomy_mode(value)

    @field_validator("ledger", mode="before")
    @classmethod
    def _coerce_ledger(cls, value: Any) -> LedgerState:
        return LedgerState.normalized(value)

    @field_validator("live_state", mode="before")
    @classmethod
    def _coerce_live_state(cls, value: Any) -> LiveState:
        if isinstance(value, LiveState):
            return value
        if isinstance(value, str):
            return LiveState.from_line(value)
        try:
            return LiveState.from_payload(value)
        except BridgeValidationError:
            return LiveState()

    @field_validator(
        "memory_text",
        "learned_text",
        "live_completion_reason",
        "planner_advisory",
        mode="before",
    )
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return value.strip() if isinstance(value, str) else ""

    @property
    def live_state_line(self) -> str:
        line = self.live_state.to_line()
        if not self.live_completion_reason:
            return line
        return "\n".join([
            line,
            f"Live-state completion signal: {self.live_completion_reason}",
        ]).strip()

    def render(
        self,
        *,
        planner_prompt: str,
        execution_prompt: str,
    ) -> str:
        tick_prompt = (
            planner_prompt
            if self.mode == AutonomyMode.PLAN
            else execution_prompt
        )
        parts = [
            self.memory_text,
            self.ledger.render(recent_progress_count=3),
            self.learned_text,
            self.live_state_line,
            self.planner_advisory,
            tick_prompt,
        ]
        return "\n\n".join(part for part in parts if part)


class LedgerStalenessEvidence(BridgeModel):
    """Typed evidence for discarding a stale persisted ledger."""

    kind: LedgerStalenessKind = LedgerStalenessKind.NONE
    reason: str = ""
    age_seconds: float | None = None
    max_age_s: float = 0.0
    mentions_initial_extraction: bool = False
    reports_no_infrastructure: bool = False

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> LedgerStalenessKind:
        if isinstance(value, LedgerStalenessKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in LedgerStalenessKind:
                if normalized == kind.value:
                    return kind
        return LedgerStalenessKind.NONE

    @field_validator("reason", mode="before")
    @classmethod
    def _coerce_reason(cls, value: Any) -> str:
        return str(value or "").strip()

    @property
    def is_stale(self) -> bool:
        return self.kind != LedgerStalenessKind.NONE and bool(self.reason)

    @classmethod
    def none(
        cls,
        *,
        age_seconds: float | None = None,
        max_age_s: float = 0.0,
        mentions_initial_extraction: bool = False,
        reports_no_infrastructure: bool = False,
    ) -> "LedgerStalenessEvidence":
        return cls(
            kind=LedgerStalenessKind.NONE,
            reason="",
            age_seconds=age_seconds,
            max_age_s=max_age_s,
            mentions_initial_extraction=mentions_initial_extraction,
            reports_no_infrastructure=reports_no_infrastructure,
        )

    @classmethod
    def from_bootstrap_policy(
        cls,
        ledger: LedgerState | dict,
        *,
        max_age_s: float,
        now: datetime | None = None,
    ) -> "LedgerStalenessEvidence":
        state = LedgerState.coerce(ledger)
        active_intent = LedgerObjectiveIntent.from_text(state.objective)
        progress_signals = LedgerProgressSignals.from_text(state.progress_text())
        age = state.age_seconds(now=now)
        try:
            max_age = float(max_age_s)
        except (TypeError, ValueError):
            max_age = 0.0

        common = {
            "age_seconds": age,
            "max_age_s": max_age,
            "mentions_initial_extraction": active_intent.mentions_initial_extraction,
            "reports_no_infrastructure": progress_signals.reports_no_infrastructure,
        }
        if not active_intent.mentions_initial_extraction:
            return cls.none(**common)
        if not progress_signals.reports_no_infrastructure:
            return cls.none(**common)
        if age is None or age <= max_age:
            return cls.none(**common)
        return cls(
            kind=LedgerStalenessKind.STALE_BOOTSTRAP,
            reason="initial-extraction ledger is older than bootstrap stale threshold and still reports no infrastructure",
            **common,
        )


class HiddenTrailerBodyLine(BridgeModel):
    """Typed view of one normalized line inside a hidden trailer body."""

    text: str = ""
    key: str = ""
    value: str = ""
    bullet: str = ""

    @field_validator("text", "key", "value", "bullet", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "").strip()

    @property
    def has_text(self) -> bool:
        return bool(self.text)

    @property
    def has_key_value(self) -> bool:
        return bool(self.key)

    @property
    def is_bullet(self) -> bool:
        return bool(self.bullet)

    def key_is(self, value: str) -> bool:
        return self.key == str(value or "").strip().lower()

    @classmethod
    def from_line(cls, value: Any) -> "HiddenTrailerBodyLine":
        text = str(value or "").strip()
        if not text:
            return cls()
        if text.startswith("- "):
            return cls(text=text, bullet=text[2:].strip())
        split = KeyValueTextSplit.from_text(text)
        if split.matched:
            return cls(
                text=text,
                key=split.key,
                value=split.value,
            )
        return cls(text=text)

    @classmethod
    def iter_body(cls, body: Any) -> Iterable["HiddenTrailerBodyLine"]:
        for raw_line in BridgeTextLines.from_text(body).lines:
            line = cls.from_line(raw_line)
            if line.has_text:
                yield line


class LedgerUpdateDraft(BridgeModel):
    """Typed intermediate shape parsed from a hidden <ledger> trailer body."""

    objective: str = ""
    plan_steps: list[str] = Field(default_factory=list)
    progress: str = ""
    signal: ProgressSignal = ProgressSignal.NONE
    status: LedgerStatus = LedgerStatus.NONE
    next_required_mode: LedgerNextRequiredMode = LedgerNextRequiredMode.NONE
    blocker: str = ""

    @field_validator("objective", "progress", "blocker", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return value.strip() if isinstance(value, str) else ""

    @field_validator("plan_steps", mode="before")
    @classmethod
    def _coerce_steps(cls, value: Any) -> list[str]:
        return _coerce_str_or_list(value)

    @field_validator("signal", mode="before")
    @classmethod
    def _coerce_signal(cls, value: Any) -> ProgressSignal:
        return progress_signal(value)

    @field_validator("status", mode="before")
    @classmethod
    def _coerce_status(cls, value: Any) -> LedgerStatus:
        return ledger_status(value)

    @field_validator("next_required_mode", mode="before")
    @classmethod
    def _coerce_next_required_mode(cls, value: Any) -> LedgerNextRequiredMode:
        return ledger_next_required_mode(value)

    @classmethod
    def from_body(cls, body: Any) -> "LedgerUpdateDraft":
        if isinstance(body, cls):
            return body
        if isinstance(body, LedgerUpdate):
            return cls(
                objective=body.objective,
                plan_steps=list(body.plan_steps),
                progress=body.progress,
                signal=body.signal,
                status=body.status,
                next_required_mode=body.next_required_mode,
                blocker=body.blocker,
            )
        data: dict[str, Any] = {
            "objective": "",
            "plan_steps": [],
            "progress": "",
            "signal": ProgressSignal.NONE,
            "status": LedgerStatus.NONE,
            "next_required_mode": LedgerNextRequiredMode.NONE,
            "blocker": "",
        }
        in_plan = False
        for line in HiddenTrailerBodyLine.iter_body(body):
            if line.key_is("objective"):
                data["objective"] = line.value
                in_plan = False
            elif line.key_is("plan"):
                in_plan = True
            elif line.key_is("progress"):
                data["progress"] = line.value
                in_plan = False
            elif line.key_is("signal"):
                data["signal"] = line.value
                in_plan = False
            elif line.key_is("status"):
                data["status"] = line.value
                in_plan = False
            elif line.key_is("next_required_mode"):
                data["next_required_mode"] = line.value
                in_plan = False
            elif line.key_is("blocker"):
                data["blocker"] = line.value
                in_plan = False
            elif in_plan and line.is_bullet:
                data["plan_steps"].append(line.bullet)
        return cls.model_validate(data)

    def to_update(self) -> "LedgerUpdate":
        return LedgerUpdate.coerce({
            "objective": self.objective,
            "plan_steps": list(self.plan_steps),
            "progress": self.progress,
            "signal": self.signal,
            "status": self.status,
            "next_required_mode": self.next_required_mode,
            "blocker": self.blocker,
        })


class LedgerUpdate(BridgeModel):
    objective: str = ""
    plan_steps: list[str] = Field(default_factory=list)
    progress: str = ""
    signal: ProgressSignal = ProgressSignal.NONE
    status: LedgerStatus = LedgerStatus.NONE
    next_required_mode: LedgerNextRequiredMode = LedgerNextRequiredMode.NONE
    blocker: str = ""

    @classmethod
    def from_trailer_text(cls, text: Any) -> "LedgerUpdate | None":
        if isinstance(text, cls):
            return text
        if isinstance(text, LedgerUpdateDraft):
            return text.to_update()
        block = HiddenTrailerBlock.first_from_text(text, "ledger")
        if not block:
            return None
        return LedgerUpdateDraft.from_body(block.body).to_update()

    @classmethod
    def strip_trailer_text(cls, text: Any) -> str:
        return HiddenTrailerBlock.strip_from_text(text, ["ledger"])

    @classmethod
    def coerce(cls, value: Any) -> "LedgerUpdate":
        if isinstance(value, cls):
            return value
        if isinstance(value, LedgerUpdateDraft):
            return value.to_update()
        if not isinstance(value, dict):
            return cls()
        objective = value.get("objective", "")
        progress = value.get("progress", "")
        plan_steps = _coerce_str_list(value.get("plan_steps", []))
        signal = progress_signal(value.get("signal"))
        status = ledger_status(value.get("status"))
        next_required_mode = ledger_next_required_mode(value.get("next_required_mode"))
        blocker = value.get("blocker", "")
        if signal == ProgressSignal.NONE:
            if status == LedgerStatus.DONE:
                signal = ProgressSignal.PLAN_DONE
            elif status in {LedgerStatus.READY, LedgerStatus.EXECUTING}:
                signal = ProgressSignal.PLAN_READY
            elif next_required_mode == LedgerNextRequiredMode.EXECUTE:
                signal = ProgressSignal.PLAN_READY
            elif plan_steps:
                signal = ProgressSignal.PLAN_READY
            elif isinstance(objective, str) and objective.strip():
                signal = ProgressSignal.NEW_OBJECTIVE
        return cls(
            objective=objective.strip() if isinstance(objective, str) else "",
            plan_steps=plan_steps,
            progress=progress.strip() if isinstance(progress, str) else "",
            signal=signal,
            status=status,
            next_required_mode=next_required_mode,
            blocker=blocker.strip() if isinstance(blocker, str) else "",
        )

    def to_dict(self) -> dict[str, Any]:
        result: dict[str, Any] = {}
        if self.objective:
            result["objective"] = self.objective
        if self.plan_steps:
            result["plan_steps"] = list(self.plan_steps)
        if self.progress:
            result["progress"] = self.progress
        if self.signal != ProgressSignal.NONE:
            result["signal"] = self.signal.value
        if self.status != LedgerStatus.NONE:
            result["status"] = self.status.value
        if self.next_required_mode != LedgerNextRequiredMode.NONE:
            result["next_required_mode"] = self.next_required_mode.value
        if self.blocker:
            result["blocker"] = self.blocker
        return result


class ReflectionDraft(BridgeModel):
    """Typed intermediate shape parsed from a hidden <reflection> trailer body."""

    structures: list[str] = Field(default_factory=list)
    error_tips: list[str] = Field(default_factory=list)
    saw_structures: bool = False
    saw_error_tips: bool = False

    @field_validator("structures", "error_tips", mode="before")
    @classmethod
    def _coerce_items(cls, value: Any) -> list[str]:
        return _coerce_str_or_list(value)

    @classmethod
    def from_body(cls, body: Any) -> "ReflectionDraft":
        if isinstance(body, cls):
            return body
        data: dict[str, Any] = {
            "structures": [],
            "error_tips": [],
            "saw_structures": False,
            "saw_error_tips": False,
        }
        active_key: str | None = None
        for line in HiddenTrailerBodyLine.iter_body(body):
            if line.key_is("structures"):
                active_key = "structures"
                data["saw_structures"] = True
            elif line.key_is("error_tips"):
                active_key = "error_tips"
                data["saw_error_tips"] = True
            elif active_key and line.is_bullet:
                data[active_key].append(line.bullet)
        return cls.model_validate(data)

    @classmethod
    def from_trailer_text(
        cls,
        text: Any,
        *,
        max_items: int = 12,
        max_len: int = 180,
    ) -> "ReflectionDraft | None":
        if isinstance(text, cls):
            draft = text
            sparse = ReflectionMemory.clean_sparse_dict(
                draft.to_sparse_dict(),
                max_items=max_items,
                max_len=max_len,
            )
            return cls(
                structures=sparse.get("structures", []),
                error_tips=sparse.get("error_tips", []),
                saw_structures="structures" in sparse,
                saw_error_tips="error_tips" in sparse,
            )
        block = HiddenTrailerBlock.first_from_text(text, "reflection")
        if not block:
            return None
        draft = cls.from_body(block.body)
        sparse = ReflectionMemory.clean_sparse_dict(
            draft.to_sparse_dict(),
            max_items=max_items,
            max_len=max_len,
        )
        return cls(
            structures=sparse.get("structures", []),
            error_tips=sparse.get("error_tips", []),
            saw_structures="structures" in sparse,
            saw_error_tips="error_tips" in sparse,
        )

    @classmethod
    def strip_trailer_text(cls, text: Any) -> str:
        return HiddenTrailerBlock.strip_from_text(text, ["reflection"])

    def to_sparse_dict(self) -> dict[str, list[str]]:
        result: dict[str, list[str]] = {}
        if self.saw_structures:
            result["structures"] = list(self.structures)
        if self.saw_error_tips:
            result["error_tips"] = list(self.error_tips)
        return result


class ReflectionMemory(BridgeModel):
    """Persisted autonomy lessons distilled from hidden <reflection> trailers."""

    structures: list[str] = Field(default_factory=list)
    error_tips: list[str] = Field(default_factory=list)
    updated_at: str = ""

    @field_validator("structures", "error_tips", mode="before")
    @classmethod
    def _coerce_items(cls, value: Any) -> list[str]:
        return _coerce_str_or_list(value)

    @field_validator("updated_at", mode="before")
    @classmethod
    def _coerce_updated_at(cls, value: Any) -> str:
        return value.strip() if isinstance(value, str) else ""

    @staticmethod
    def compact_item(value: Any, *, max_len: int = 180) -> str:
        return _compact_prompt_text(value, limit=max_len)

    @staticmethod
    def should_drop_item(value: Any) -> bool:
        return ReflectionDropEvidence.from_text(value).should_drop

    @classmethod
    def clean_items(
        cls,
        value: Any,
        *,
        max_items: int | None = None,
        max_len: int = 180,
        item_normalizer: Callable[[str], str] | None = None,
        item_should_drop: Callable[[str], bool] | None = None,
    ) -> list[str]:
        try:
            limit = int(max_items) if max_items is not None else None
        except (TypeError, ValueError):
            limit = None
        if limit is not None and limit <= 0:
            return []

        result: list[str] = []
        seen: set[str] = set()
        for item in _coerce_str_or_list(value):
            try:
                compact = (
                    item_normalizer(item)
                    if item_normalizer
                    else cls.compact_item(item, max_len=max_len)
                )
            except Exception:
                compact = cls.compact_item(item, max_len=max_len)
            compact = str(compact).strip()
            if not compact:
                continue
            try:
                should_drop = (
                    bool(item_should_drop(compact))
                    if item_should_drop
                    else cls.should_drop_item(item) or cls.should_drop_item(compact)
                )
            except Exception:
                should_drop = False
            if should_drop or compact in seen:
                continue
            seen.add(compact)
            result.append(compact)
            if limit is not None and len(result) >= limit:
                break
        return result

    @classmethod
    def coerce(
        cls,
        value: Any,
        *,
        max_items: int | None = None,
        max_len: int = 180,
        item_normalizer: Callable[[str], str] | None = None,
        item_should_drop: Callable[[str], bool] | None = None,
    ) -> "ReflectionMemory":
        if isinstance(value, cls):
            memory = value
        elif isinstance(value, dict):
            data = {
                "structures": value.get("structures", []),
                "error_tips": value.get("error_tips", []),
                "updated_at": value.get("updated_at", ""),
            }
            try:
                memory = cls.model_validate(data)
            except ValidationError:
                memory = cls()
        else:
            memory = cls()
        return memory.cleaned(
            max_items=max_items,
            max_len=max_len,
            item_normalizer=item_normalizer,
            item_should_drop=item_should_drop,
        )

    @classmethod
    def from_file_text(
        cls,
        value: str,
        *,
        max_items: int | None = None,
        max_len: int = 180,
        item_normalizer: Callable[[str], str] | None = None,
        item_should_drop: Callable[[str], bool] | None = None,
    ) -> "ReflectionMemory":
        if isinstance(value, cls):
            return cls.coerce(
                value,
                max_items=max_items,
                max_len=max_len,
                item_normalizer=item_normalizer,
                item_should_drop=item_should_drop,
            )
        data = _json_object_from_text(value, "reflection")
        return cls.coerce(
            data,
            max_items=max_items,
            max_len=max_len,
            item_normalizer=item_normalizer,
            item_should_drop=item_should_drop,
        )

    @classmethod
    def clean_sparse_dict(
        cls,
        value: Any,
        *,
        max_items: int | None = None,
        max_len: int = 180,
        item_normalizer: Callable[[str], str] | None = None,
        item_should_drop: Callable[[str], bool] | None = None,
    ) -> dict[str, list[str]]:
        if not isinstance(value, dict):
            return {}
        result: dict[str, list[str]] = {}
        for key in ("structures", "error_tips"):
            if key in value:
                result[key] = cls.clean_items(
                    value.get(key, []),
                    max_items=max_items,
                    max_len=max_len,
                    item_normalizer=item_normalizer,
                    item_should_drop=item_should_drop,
                )
        return result

    def cleaned(
        self,
        *,
        max_items: int | None = None,
        max_len: int = 180,
        item_normalizer: Callable[[str], str] | None = None,
        item_should_drop: Callable[[str], bool] | None = None,
    ) -> "ReflectionMemory":
        return ReflectionMemory(
            structures=self.clean_items(
                self.structures,
                max_items=max_items,
                max_len=max_len,
                item_normalizer=item_normalizer,
                item_should_drop=item_should_drop,
            ),
            error_tips=self.clean_items(
                self.error_tips,
                max_items=max_items,
                max_len=max_len,
                item_normalizer=item_normalizer,
                item_should_drop=item_should_drop,
            ),
            updated_at=self.updated_at,
        )

    def merged_with(
        self,
        update: Any,
        *,
        updated_at: str = "",
        max_items: int | None = None,
        max_len: int = 180,
        item_normalizer: Callable[[str], str] | None = None,
        item_should_drop: Callable[[str], bool] | None = None,
    ) -> "ReflectionMemory":
        if isinstance(update, ReflectionDraft):
            sparse = update.to_sparse_dict()
        elif isinstance(update, ReflectionMemory):
            sparse = update.to_dict()
        elif isinstance(update, dict):
            sparse = update
        else:
            sparse = {}
        clean_update = self.clean_sparse_dict(
            sparse,
            max_items=max_items,
            max_len=max_len,
            item_normalizer=item_normalizer,
            item_should_drop=item_should_drop,
        )
        return ReflectionMemory(
            structures=clean_update.get("structures", list(self.structures)),
            error_tips=clean_update.get("error_tips", list(self.error_tips)),
            updated_at=updated_at.strip() if isinstance(updated_at, str) and updated_at.strip() else self.updated_at,
        ).cleaned(
            max_items=max_items,
            max_len=max_len,
            item_normalizer=item_normalizer,
            item_should_drop=item_should_drop,
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "structures": list(self.structures),
            "error_tips": list(self.error_tips),
            "updated_at": self.updated_at,
        }

    def to_json_line(self) -> str:
        return json.dumps(self.to_dict()) + "\n"


JOURNAL_EVENT_KINDS = {"progress", "failure", "discovery", "milestone"}


class ReflectionDropEvidence(BridgeModel):
    """Typed reason a reflection-memory item is too noisy to persist."""

    kind: ReflectionDropKind = ReflectionDropKind.NONE
    reason: str = ""
    failure_kind: JournalFailureKind = JournalFailureKind.NONE

    LOW_VALUE_STARTUP_PHRASES: ClassVar[tuple[str, ...]] = (
        "no prior progress",
        "no infrastructure yet deployed",
        "fresh deployment",
        "zero-state",
        "nothing built",
    )

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> ReflectionDropKind:
        if isinstance(value, ReflectionDropKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in ReflectionDropKind:
                if normalized == kind.value:
                    return kind
        return ReflectionDropKind.NONE

    @field_validator("failure_kind", mode="before")
    @classmethod
    def _coerce_failure_kind(cls, value: Any) -> JournalFailureKind:
        if isinstance(value, JournalFailureKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in JournalFailureKind:
                if normalized == kind.value:
                    return kind
        return JournalFailureKind.NONE

    @field_validator("reason", mode="before")
    @classmethod
    def _coerce_reason(cls, value: Any) -> str:
        return str(value or "").strip()

    @property
    def should_drop(self) -> bool:
        return self.kind != ReflectionDropKind.NONE

    @classmethod
    def none(cls) -> "ReflectionDropEvidence":
        return cls(kind=ReflectionDropKind.NONE, reason="")

    @classmethod
    def from_text(cls, value: Any) -> "ReflectionDropEvidence":
        text = str(value or "")
        failure = JournalFailureClassification.from_text(text)
        if failure.drop_from_memory:
            return cls(
                kind=ReflectionDropKind.TRANSIENT_FAILURE,
                reason="transient failure classification",
                failure_kind=failure.kind,
            )
        normalized = text.lower()
        if any(phrase in normalized for phrase in cls.LOW_VALUE_STARTUP_PHRASES):
            return cls(
                kind=ReflectionDropKind.LOW_VALUE_STARTUP,
                reason="startup/no-infrastructure reflection",
            )
        return cls.none()


def _compact_prompt_text(value: Any, *, limit: int) -> str:
    compact = re.sub(r"\s+", " ", str(value)).strip()
    if limit <= 0 or len(compact) <= limit:
        return compact
    return compact[:limit].rstrip() + "..."


class JournalFailureEvidence(BridgeModel):
    """Typed reason a journal failure line is transient/noisy."""

    _JOURNAL_DROP_KINDS: ClassVar[frozenset[JournalFailureKind]] = frozenset({
        JournalFailureKind.PROVIDER_LIMIT,
        JournalFailureKind.TURN_LIMIT,
        JournalFailureKind.TIMEOUT,
        JournalFailureKind.CONTEXT_WINDOW,
        JournalFailureKind.EXPECTED_MISS,
        JournalFailureKind.INFRASTRUCTURE_FAILURE,
        JournalFailureKind.ENGINE_TRANSIENT,
        JournalFailureKind.RESEARCH_BUSY,
    })

    kind: JournalFailureKind = JournalFailureKind.NONE
    raw_text: str = ""
    reason: str = ""
    tool_classification: ToolResultClassification | None = None
    tool_text_kind: ToolResultTextKind | None = None
    journal_noise: bool = False

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> JournalFailureKind:
        if isinstance(value, JournalFailureKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in JournalFailureKind:
                if normalized == kind.value:
                    return kind
        return JournalFailureKind.NONE

    @field_validator("raw_text", "reason", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "")

    @field_validator("tool_classification", mode="before")
    @classmethod
    def _coerce_tool_classification(
        cls,
        value: Any,
    ) -> ToolResultClassification | None:
        if value is None or isinstance(value, ToolResultClassification):
            return value
        if isinstance(value, str):
            try:
                return ToolResultClassification(value.strip().lower())
            except ValueError:
                return None
        return None

    @field_validator("tool_text_kind", mode="before")
    @classmethod
    def _coerce_tool_text_kind(cls, value: Any) -> ToolResultTextKind | None:
        if value is None or isinstance(value, ToolResultTextKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in ToolResultTextKind:
                if normalized == kind.value:
                    return kind
        return None

    @field_validator("journal_noise", mode="before")
    @classmethod
    def _coerce_journal_noise(cls, value: Any) -> bool:
        if isinstance(value, bool):
            return value
        return str(value).strip().lower() in {"1", "true", "yes", "on"}

    @property
    def is_transient(self) -> bool:
        return self.kind != JournalFailureKind.NONE

    @property
    def drop_from_journal(self) -> bool:
        return self.journal_noise or self.kind in self._JOURNAL_DROP_KINDS

    @property
    def drop_from_memory(self) -> bool:
        return self.kind != JournalFailureKind.NONE

    @classmethod
    def none(cls, *, raw_text: Any = "") -> "JournalFailureEvidence":
        return cls(kind=JournalFailureKind.NONE, raw_text=str(raw_text or ""))

    @classmethod
    def from_text(cls, value: Any) -> "JournalFailureEvidence":
        text = str(value or "")
        normalized = re.sub(r"\s+", " ", text.lower()).strip()
        if not normalized:
            return cls.none(raw_text=text)

        if (
            ProviderUsageLimit.from_text(text) is not None
            or "usage limit reached" in normalized
            or "request rejected (429)" in normalized
            or "provider usage limit" in normalized
        ):
            return cls(
                kind=JournalFailureKind.PROVIDER_LIMIT,
                raw_text=text,
                reason="provider usage limit",
            )

        if "reached maximum number of turns" in normalized:
            return cls(
                kind=JournalFailureKind.TURN_LIMIT,
                raw_text=text,
                reason="sdk turn limit",
            )

        if (
            "stream idle timeout" in normalized
            or "tick timeout" in normalized
            or "agent tick exceeded" in normalized
        ):
            return cls(
                kind=JournalFailureKind.TIMEOUT,
                raw_text=text,
                reason="agent or stream timeout",
            )

        if SdkErrorSignal.is_context_window_limit(text):
            return cls(
                kind=JournalFailureKind.CONTEXT_WINDOW,
                raw_text=text,
                reason="sdk context window limit",
            )

        tool_outcome = ToolResultOutcome.from_text(text)
        tool_evidence = tool_outcome.text_evidence
        if tool_outcome.classification == ToolResultClassification.EXPECTED_MISS:
            return cls(
                kind=JournalFailureKind.EXPECTED_MISS,
                raw_text=text,
                reason="expected tool miss",
                tool_classification=tool_outcome.classification,
                tool_text_kind=tool_evidence.kind if tool_evidence else None,
            )
        if tool_outcome.classification == ToolResultClassification.INVALID_REQUEST:
            journal_noise = (
                "missing field `success`" in normalized
                or "missing field success" in normalized
                or "packet too large" in normalized
            )
            return cls(
                kind=JournalFailureKind.INVALID_REQUEST,
                raw_text=text,
                reason=(
                    "invalid sdk result envelope"
                    if journal_noise
                    else "invalid tool request"
                ),
                tool_classification=tool_outcome.classification,
                tool_text_kind=tool_evidence.kind if tool_evidence else None,
                journal_noise=journal_noise,
            )
        if tool_outcome.classification == ToolResultClassification.INFRASTRUCTURE_FAILURE:
            return cls(
                kind=JournalFailureKind.INFRASTRUCTURE_FAILURE,
                raw_text=text,
                reason="bridge or infrastructure failure",
                tool_classification=tool_outcome.classification,
                tool_text_kind=tool_evidence.kind if tool_evidence else None,
            )

        if "bad argument #1" in normalized:
            return cls(
                kind=JournalFailureKind.ENGINE_TRANSIENT,
                raw_text=text,
                reason="factorio lua transient engine error",
            )

        if "failed to queue research" in normalized:
            return cls(
                kind=JournalFailureKind.RESEARCH_BUSY,
                raw_text=text,
                reason="research queue transient",
            )

        return cls.none(raw_text=text)


class JournalFailureClassification(BridgeModel):
    """Typed view of failure lines that are noisy rather than durable memory."""

    kind: JournalFailureKind = JournalFailureKind.NONE
    raw_text: str = ""
    tool_classification: ToolResultClassification | None = None
    reason: str = ""
    tool_text_kind: ToolResultTextKind | None = None
    journal_noise: bool = False

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> JournalFailureKind:
        if isinstance(value, JournalFailureKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in JournalFailureKind:
                if normalized == kind.value:
                    return kind
        return JournalFailureKind.NONE

    @field_validator("tool_classification", mode="before")
    @classmethod
    def _coerce_tool_classification(cls, value: Any) -> ToolResultClassification | None:
        if value is None or isinstance(value, ToolResultClassification):
            return value
        if isinstance(value, str):
            try:
                return ToolResultClassification(value.strip().lower())
            except ValueError:
                return None
        return None

    @field_validator("reason", mode="before")
    @classmethod
    def _coerce_reason(cls, value: Any) -> str:
        return str(value or "").strip()

    @field_validator("tool_text_kind", mode="before")
    @classmethod
    def _coerce_tool_text_kind(cls, value: Any) -> ToolResultTextKind | None:
        if value is None or isinstance(value, ToolResultTextKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in ToolResultTextKind:
                if normalized == kind.value:
                    return kind
        return None

    @field_validator("journal_noise", mode="before")
    @classmethod
    def _coerce_journal_noise(cls, value: Any) -> bool:
        if isinstance(value, bool):
            return value
        return str(value).strip().lower() in {"1", "true", "yes", "on"}

    @classmethod
    def none(cls, *, raw_text: Any = "") -> "JournalFailureClassification":
        return cls(kind=JournalFailureKind.NONE, raw_text=str(raw_text or ""))

    @classmethod
    def from_text(cls, value: Any) -> "JournalFailureClassification":
        evidence = JournalFailureEvidence.from_text(value)
        return cls(
            kind=evidence.kind,
            raw_text=evidence.raw_text,
            tool_classification=evidence.tool_classification,
            reason=evidence.reason,
            tool_text_kind=evidence.tool_text_kind,
            journal_noise=evidence.journal_noise,
        )

    @property
    def drop_from_memory(self) -> bool:
        return self.kind != JournalFailureKind.NONE

    @property
    def drop_from_journal(self) -> bool:
        return (
            self.journal_noise
            or self.kind in JournalFailureEvidence._JOURNAL_DROP_KINDS
        )


def _journal_transient_failure_text(value: Any) -> bool:
    return JournalFailureClassification.from_text(value).drop_from_journal


class PromptTextSanitizer(BridgeModel):
    """Typed prompt-facing sanitizer for volatile human/player identifiers."""

    text: str = ""
    player_label: str = "player"

    @field_validator("text", "player_label", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "")

    @property
    def sanitized(self) -> str:
        label = self.player_label.strip() or "player"

        def replace(match: re.Match[str]) -> str:
            prefix = match.group("prefix")
            return f"{prefix}[{label}]:"

        return re.sub(
            r"(?P<prefix>^|[\n\s])\[(?P<name>[^\]\n:=]{1,64})\]:",
            replace,
            self.text,
        )

    @classmethod
    def sanitize(cls, value: Any) -> str:
        return cls(text=value).sanitized


class JournalEvent(BridgeModel):
    ts: str
    kind: str
    text: str
    signal: ProgressSignal = ProgressSignal.NONE

    @staticmethod
    def event_kind(value: Any) -> str:
        return value if value in JOURNAL_EVENT_KINDS else "progress"

    @staticmethod
    def compact_text(value: Any, *, limit: int = 500) -> str:
        return _compact_prompt_text(value, limit=limit)

    @staticmethod
    def progress_fingerprint_text(value: Any) -> str:
        """Stable identity for repeated autonomy progress notes.

        Planner status lines often include volatile tick numbers or counts.
        Those should not prevent the autonomy scheduler from recognizing the
        same no-op planning loop repeating across ticks.
        """
        text = _compact_prompt_text(value, limit=1000).lower()
        text = re.sub(r"\btick\s+\d+\b", "tick <n>", text)
        text = re.sub(r"\b\d+(?:\.\d+)?\b", "<n>", text)
        return text

    @staticmethod
    def is_transient_failure_text(value: Any) -> bool:
        return _journal_transient_failure_text(value)

    @classmethod
    def should_drop_event(
        cls,
        *,
        kind: Any,
        text: Any,
        signal: ProgressSignal | str | None = None,
    ) -> bool:
        normalized_kind = cls.event_kind(kind)
        if progress_signal(signal) != ProgressSignal.NONE:
            return False
        if normalized_kind == "failure" and cls.is_transient_failure_text(text):
            return True
        return False

    @classmethod
    def create(
        cls,
        *,
        ts: str,
        kind: str,
        text: Any,
        signal: ProgressSignal | str | None = None,
    ) -> "JournalEvent":
        return cls(
            ts=str(ts),
            kind=cls.event_kind(kind),
            text=str(text),
            signal=progress_signal(signal),
        )

    @classmethod
    def from_mapping(cls, value: Any) -> "JournalEvent | None":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return None
        return cls.create(
            ts=value.get("ts", ""),
            kind=value.get("kind", "progress"),
            text=value.get("text", ""),
            signal=value.get("signal", ProgressSignal.NONE),
        )

    @classmethod
    def from_json_line(cls, value: Any) -> "JournalEvent | None":
        if isinstance(value, cls):
            return value
        if not isinstance(value, str):
            return None
        line = value.strip()
        if not line:
            return None
        data = _json_value_or_missing(line)
        if data is _JSON_MISSING:
            return None
        return cls.from_mapping(data)

    def to_dict(self) -> dict[str, str]:
        result = {
            "ts": self.ts,
            "kind": self.kind,
            "text": self.text,
        }
        if self.signal != ProgressSignal.NONE:
            result["signal"] = self.signal.value
        return result

    def to_json_line(self) -> str:
        return json.dumps(self.to_dict(), separators=(",", ":")) + "\n"

    def should_drop(self) -> bool:
        return self.should_drop_event(
            kind=self.kind,
            text=self.text,
            signal=self.signal,
        )

    def prompt_dict(self, *, text_limit: int = 500, count: int = 1) -> dict[str, Any]:
        return self.prompt_event(text_limit=text_limit, count=count).to_dict()

    def prompt_event(self, *, text_limit: int = 500, count: int = 1) -> "JournalPromptEvent":
        return JournalPromptEvent(
            kind=self.kind,
            text=PromptTextSanitizer.sanitize(
                self.compact_text(self.text, limit=text_limit),
            ),
            signal=self.signal,
            count=count,
            ts=self.ts,
        )

    @property
    def progress_fingerprint(self) -> str:
        return self.progress_fingerprint_text(self.text)


_AUTONOMY_EVENT_KINDS = {"progress", "discovery", "milestone"}


class JournalPromptEvent(BridgeModel):
    """Prompt-ready view of a journal event with compaction metadata."""

    kind: str = "progress"
    text: str = ""
    signal: ProgressSignal = ProgressSignal.NONE
    count: int = 1
    ts: str = ""

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> str:
        return JournalEvent.event_kind(value)

    @field_validator("text", "ts", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "")

    @field_validator("signal", mode="before")
    @classmethod
    def _coerce_signal(cls, value: Any) -> ProgressSignal:
        return progress_signal(value)

    @field_validator("count", mode="before")
    @classmethod
    def _coerce_count(cls, value: Any) -> int:
        try:
            count = int(value)
        except (TypeError, ValueError):
            return 1
        return max(1, count)

    @classmethod
    def from_event(
        cls,
        event: JournalEvent,
        *,
        text_limit: int = 500,
    ) -> "JournalPromptEvent | None":
        if event.should_drop():
            return None
        prompt_event = event.prompt_event(text_limit=text_limit)
        if not prompt_event.text:
            return None
        return prompt_event

    def has_same_prompt_identity(self, other: "JournalPromptEvent") -> bool:
        return (
            self.kind == other.kind
            and self.text == other.text
            and self.signal == other.signal
        )

    def merged_with(self, other: "JournalPromptEvent") -> "JournalPromptEvent":
        if not self.has_same_prompt_identity(other):
            return other
        return self.model_copy(update={
            "count": self.count + other.count,
            "ts": other.ts or self.ts,
        })

    def to_dict(self) -> dict[str, Any]:
        return {
            "kind": self.kind,
            "text": self.text,
            "signal": self.signal.value,
            "count": self.count,
            "ts": self.ts,
        }

    def render_line(self) -> str:
        repeat = f" (x{self.count})" if self.count > 1 else ""
        signal = (
            f" [signal={self.signal.value}]"
            if self.signal != ProgressSignal.NONE
            else ""
        )
        return f"- {self.kind}{repeat}{signal}: {self.text.strip()}"


class JournalEventCollection(BridgeModel):
    """Typed collection of journal events used by prompt windows."""

    items: tuple[JournalEvent, ...] = ()

    @field_validator("items", mode="before")
    @classmethod
    def _coerce_items(cls, value: Any) -> tuple[JournalEvent, ...]:
        if value is None or isinstance(value, (str, bytes, dict, BaseModel)):
            return ()
        if not isinstance(value, Iterable):
            return ()
        events: list[JournalEvent] = []
        for item in value:
            event = item if isinstance(item, JournalEvent) else JournalEvent.from_mapping(item)
            if event:
                events.append(event)
        return tuple(events)

    @classmethod
    def from_value(cls, value: Any) -> "JournalEventCollection":
        if isinstance(value, cls):
            return value
        return cls(items=value)

    def to_list(self) -> list[JournalEvent]:
        return list(self.items)


class JournalWindow(BridgeModel):
    events: list[JournalEvent] = Field(default_factory=list)

    @classmethod
    def coerce(cls, value: Any) -> "JournalWindow":
        if isinstance(value, cls):
            return value
        return cls(events=JournalEventCollection.from_value(value).to_list())

    def newest_autonomy_event(self) -> JournalEvent | None:
        for event in reversed(self.events):
            if event.kind in _AUTONOMY_EVENT_KINDS:
                return event
        return None

    def newest_autonomy_signal(self) -> ProgressSignal:
        event = self.newest_autonomy_event()
        if not event:
            return ProgressSignal.NONE
        return event.signal

    def has_actionable_plan_signal(self) -> bool:
        return self.newest_autonomy_signal() in {
            ProgressSignal.NEW_OBJECTIVE,
            ProgressSignal.PLAN_READY,
        }

    def repeated_unsignaled_progress_count(self) -> int:
        newest = self.newest_autonomy_event()
        if not newest or newest.kind != "progress" or newest.signal != ProgressSignal.NONE:
            return 0
        newest_fingerprint = newest.progress_fingerprint
        count = 0
        for event in reversed(self.events):
            if event.kind not in _AUTONOMY_EVENT_KINDS:
                continue
            if (
                event.kind != newest.kind
                or event.signal != newest.signal
                or event.progress_fingerprint != newest_fingerprint
            ):
                break
            count += 1
        return count

    def has_repeated_unsignaled_progress(self, *, min_count: int = 3) -> bool:
        try:
            threshold = int(min_count)
        except (TypeError, ValueError):
            threshold = 3
        threshold = max(1, threshold)
        return self.repeated_unsignaled_progress_count() >= threshold

    def repeated_ready_progress_count(self) -> int:
        newest = self.newest_autonomy_event()
        if (
            not newest
            or newest.kind != "progress"
            or newest.signal != ProgressSignal.NONE
            or not LedgerReadinessEvidence.note_indicates_ready(newest.text)
        ):
            return 0
        count = 0
        for event in reversed(self.events):
            if event.kind not in _AUTONOMY_EVENT_KINDS:
                continue
            if (
                event.kind != "progress"
                or event.signal != ProgressSignal.NONE
                or not LedgerReadinessEvidence.note_indicates_ready(event.text)
            ):
                break
            count += 1
        return count

    def has_repeated_ready_progress(self, *, min_count: int = 3) -> bool:
        try:
            threshold = int(min_count)
        except (TypeError, ValueError):
            threshold = 3
        threshold = max(1, threshold)
        return self.repeated_ready_progress_count() >= threshold

    def newest_event_indicates_plan_done(self) -> bool:
        event = self.newest_autonomy_event()
        if not event:
            return False
        return event.signal == ProgressSignal.PLAN_DONE

    def prompt_events(
        self,
        *,
        max_items: int = 5,
        text_limit: int = 500,
        useful_kinds: set[str] | frozenset[str] | None = None,
    ) -> list[JournalPromptEvent]:
        try:
            max_items = int(max_items)
        except (TypeError, ValueError):
            max_items = 5
        if max_items <= 0:
            return []

        useful = set(useful_kinds or _AUTONOMY_EVENT_KINDS)
        compacted: list[JournalPromptEvent] = []
        for event in self.events:
            prompt_event = JournalPromptEvent.from_event(event, text_limit=text_limit)
            if not prompt_event:
                continue
            if compacted and compacted[-1].has_same_prompt_identity(prompt_event):
                compacted[-1] = compacted[-1].merged_with(prompt_event)
                continue
            compacted.append(prompt_event)

        rendered = compacted[-max_items:]
        if any(event.kind in useful for event in rendered):
            return rendered
        if max_items <= 1:
            return rendered

        # Preserve one useful state transition when distinct failures fill the
        # final window; otherwise the next tick only sees symptoms.
        for event in reversed(compacted[:-max_items]):
            if event.kind in useful:
                return [event] + rendered[-(max_items - 1):]
        return rendered


class SkillDefinitionDraft(BridgeModel):
    """Typed intermediate shape parsed from a legacy <skill> trailer body."""

    name: str = ""
    params: list[str] = Field(default_factory=list)
    steps: list[str] = Field(default_factory=list)
    outcome: str = ""

    @field_validator("name", "outcome", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return value.strip() if isinstance(value, str) else ""

    @field_validator("params", "steps", mode="before")
    @classmethod
    def _coerce_string_list(cls, value: Any) -> list[str]:
        return _coerce_str_or_list(value)

    @classmethod
    def from_body(cls, body: Any) -> "SkillDefinitionDraft":
        if isinstance(body, SkillDefinition):
            return cls(
                name=body.name,
                params=list(body.params),
                steps=list(body.steps),
                outcome=body.outcome,
            )
        if isinstance(body, cls):
            return body
        data: dict[str, Any] = {
            "name": "",
            "params": [],
            "steps": [],
            "outcome": "",
        }
        active_key: str | None = None
        for line in HiddenTrailerBodyLine.iter_body(body):
            if line.key_is("name"):
                data["name"] = line.value
                active_key = None
            elif line.key_is("params"):
                active_key = "params"
                data["params"].extend(cls._parse_inline_items(line.value))
            elif line.key_is("steps"):
                active_key = "steps"
            elif line.key_is("outcome"):
                data["outcome"] = line.value
                active_key = None
            elif active_key in {"params", "steps"} and line.is_bullet:
                data[active_key].append(line.bullet)
        return cls.model_validate(data)

    @staticmethod
    def _parse_inline_items(value: Any) -> list[str]:
        return CommaSeparatedItems.from_value(value).to_list()

    def to_skill(self) -> "SkillDefinition | None":
        return SkillDefinition.coerce(self.to_dict())

    def to_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "params": list(self.params),
            "steps": list(self.steps),
            "outcome": self.outcome,
        }


class SkillDefinition(SkillDefinitionDraft):
    name: str

    @field_validator("name", mode="before")
    @classmethod
    def _validate_name(cls, value: Any) -> str:
        if not isinstance(value, str) or not value.strip():
            raise ValueError("skill name is required")
        return value.strip()

    @classmethod
    def from_trailer_text(cls, text: Any) -> "SkillDefinition | None":
        if isinstance(text, cls):
            return text
        if isinstance(text, SkillDefinitionDraft):
            return text.to_skill()
        block = HiddenTrailerBlock.first_from_text(text, "skill")
        if not block:
            return None
        return SkillDefinitionDraft.from_body(block.body).to_skill()

    @classmethod
    def strip_trailer_text(cls, text: Any) -> str:
        return HiddenTrailerBlock.strip_from_text(text, ["skill"])

    @classmethod
    def coerce(cls, value: Any) -> "SkillDefinition | None":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return None
        try:
            return cls.model_validate(value)
        except ValidationError:
            return None

    def to_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "params": list(self.params),
            "steps": list(self.steps),
            "outcome": self.outcome,
        }

    def to_sparse_dict(self) -> dict[str, Any]:
        return {
            key: value
            for key, value in self.to_dict().items()
            if key == "name" or value not in ("", [])
        }

    def signature(self) -> str:
        return f"{self.name}({', '.join(self.params)})"

    def prompt_summary_line(self) -> str:
        return f"- {self.signature()} — {self.outcome}"


class SkillDefinitionCollection(BridgeModel):
    """Typed collection boundary for reusable skill definitions."""

    skills: tuple[SkillDefinition, ...] = ()

    @field_validator("skills", mode="before")
    @classmethod
    def _coerce_skills(cls, value: Any) -> tuple[SkillDefinition, ...]:
        if value is None:
            items: list[Any] = []
        elif isinstance(value, SkillDefinition):
            items = [value]
        elif isinstance(value, SkillDefinitionDraft):
            items = [value]
        elif isinstance(value, dict):
            items = value.get("skills", [value])
        elif isinstance(getattr(value, "skills", None), Iterable) and not isinstance(
            getattr(value, "skills", None),
            (str, bytes, dict),
        ):
            items = list(value.skills)
        elif isinstance(value, Iterable) and not isinstance(value, (str, bytes, dict)):
            items = list(value)
        else:
            items = []

        skills: list[SkillDefinition] = []
        for item in items:
            if isinstance(item, SkillDefinition):
                skill = item
            elif isinstance(item, SkillDefinitionDraft):
                skill = item.to_skill()
            else:
                skill = SkillDefinition.coerce(item)
            if skill:
                skills.append(skill)
        return tuple(skills)

    @classmethod
    def from_value(cls, value: Any) -> "SkillDefinitionCollection":
        if isinstance(value, cls):
            return value
        return cls(skills=value)

    def to_list(self) -> list[SkillDefinition]:
        return list(self.skills)


class SkillLibrary(BridgeModel):
    skills: list[SkillDefinition] = Field(default_factory=list)

    @classmethod
    def from_file_text(cls, value: str, *, max_skills: int = 50) -> "SkillLibrary":
        if isinstance(value, cls):
            return cls.coerce(value, max_skills=max_skills)
        data = _json_object_from_text(value, "skills")
        return cls.coerce(data, max_skills=max_skills)

    @classmethod
    def coerce(cls, value: Any, *, max_skills: int = 50) -> "SkillLibrary":
        raw_skills = SkillDefinitionCollection.from_value(value).to_list()
        try:
            limit = int(max_skills)
        except (TypeError, ValueError):
            limit = 50
        limit = max(0, limit)
        skills: list[SkillDefinition] = []
        seen: set[str] = set()
        for skill in raw_skills:
            if skill.name in seen:
                skills = [existing for existing in skills if existing.name != skill.name]
            seen.add(skill.name)
            skills.append(skill)
            if limit and len(skills) >= limit:
                break
        return cls(skills=skills)

    @classmethod
    def normalized(cls, value: Any, *, max_skills: int = 50) -> "SkillLibrary":
        return cls.coerce(value, max_skills=max_skills)

    def merged_with(self, library: "SkillLibrary", *, max_skills: int = 50) -> "SkillLibrary":
        try:
            limit = int(max_skills)
        except (TypeError, ValueError):
            limit = 50
        limit = max(0, limit)
        merged = list(self.skills)
        positions = {skill.name: index for index, skill in enumerate(merged)}
        for skill in library.skills:
            if skill.name in positions:
                merged[positions[skill.name]] = skill
            else:
                positions[skill.name] = len(merged)
                merged.append(skill)
            if limit and len(merged) >= limit:
                break
        return SkillLibrary(skills=merged[:limit] if limit else [])

    def replace_or_append(
        self,
        skill: SkillDefinition,
        *,
        max_skills: int = 50,
        move_to_end: bool = True,
    ) -> "SkillLibrary":
        skills = [existing for existing in self.skills if existing.name != skill.name]
        if move_to_end:
            skills.append(skill)
        else:
            inserted = False
            for index, existing in enumerate(self.skills):
                if existing.name == skill.name:
                    skills.insert(index, skill)
                    inserted = True
                    break
            if not inserted:
                skills.append(skill)
        try:
            limit = int(max_skills)
        except (TypeError, ValueError):
            limit = 50
        limit = max(0, limit)
        return SkillLibrary(skills=skills[-limit:] if limit else [])

    def get(self, name: Any) -> SkillDefinition | None:
        if not isinstance(name, str):
            return None
        for skill in self.skills:
            if skill.name == name:
                return skill
        return None

    def render_prompt(self) -> str:
        if not self.skills:
            return ""
        lines = ["Available skills (reuse these recipes; follow the steps with your tools):"]
        lines.extend(skill.prompt_summary_line() for skill in self.skills)
        return "\n".join(lines)

    def to_dict(self) -> dict[str, Any]:
        return {"skills": [skill.to_dict() for skill in self.skills]}

    def to_json_line(self) -> str:
        return self.model_dump_json() + "\n"


LEARNING_PROPOSAL_KINDS = (
    "skill_proposal",
    "diagnostic_proposal",
    "script_proposal",
    "bug_report",
)
LEARNING_PROPOSAL_STATUSES = {"pending", "accepted", "rejected"}
MAX_LEARNING_PROPOSAL_FIELD_ITEMS = 20


def _proposal_kind(value: Any, *, strict: bool = False) -> str:
    if isinstance(value, str):
        normalized = value.strip().lower()
        if normalized in LEARNING_PROPOSAL_KINDS:
            return normalized
    if strict:
        allowed = ", ".join(LEARNING_PROPOSAL_KINDS)
        raise BridgeValidationError("kind", f"expected one of: {allowed}")
    return "skill_proposal"


def _proposal_status(value: Any, *, default: str, strict: bool = False) -> str:
    if isinstance(value, str):
        normalized = value.strip().lower()
        if normalized in LEARNING_PROPOSAL_STATUSES:
            return normalized
    if strict:
        allowed = ", ".join(sorted(LEARNING_PROPOSAL_STATUSES))
        raise BridgeValidationError("status", f"expected one of: {allowed}")
    return default if default in LEARNING_PROPOSAL_STATUSES else "pending"


def _optional_learning_timestamp(data: dict[str, Any], key: str) -> str | None:
    value = data.get(key)
    if value is None:
        return None
    if not isinstance(value, str):
        raise BridgeValidationError(key, "expected string")
    return value


class LearningProposalDraft(BridgeModel):
    """Typed intermediate shape parsed from a hidden learning trailer body."""

    kind: str = "skill_proposal"
    name: str = ""
    trigger: str = ""
    problem: str = ""
    preconditions: list[str] = Field(default_factory=list)
    steps: list[str] = Field(default_factory=list)
    anti_steps: list[str] = Field(default_factory=list)
    evidence: list[str] = Field(default_factory=list)
    acceptance_tests: list[str] = Field(default_factory=list)
    raw_body: str = ""

    LIST_FIELDS: ClassVar[set[str]] = {
        "preconditions",
        "steps",
        "anti_steps",
        "evidence",
        "acceptance_tests",
    }
    FIELD_ALIASES: ClassVar[dict[str, str]] = {
        "title": "name",
        "summary": "problem",
        "avoid": "anti_steps",
        "anti-step": "anti_steps",
        "anti-steps": "anti_steps",
        "anti_steps": "anti_steps",
        "acceptance": "acceptance_tests",
        "acceptance_test": "acceptance_tests",
        "acceptance_tests": "acceptance_tests",
        "test": "acceptance_tests",
        "tests": "acceptance_tests",
    }

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> str:
        return _proposal_kind(value)

    @field_validator("name", "trigger", "problem", "raw_body", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return value.strip() if isinstance(value, str) else ""

    @field_validator(
        "preconditions",
        "steps",
        "anti_steps",
        "evidence",
        "acceptance_tests",
        mode="before",
    )
    @classmethod
    def _coerce_items(cls, value: Any) -> list[str]:
        return _coerce_str_or_list(
            value,
            max_items=MAX_LEARNING_PROPOSAL_FIELD_ITEMS,
        )

    @classmethod
    def from_tag_body(cls, tag: Any, body: Any) -> "LearningProposalDraft":
        if isinstance(body, cls):
            return body if _proposal_kind(tag) == body.kind else body.model_copy(
                update={"kind": _proposal_kind(tag)},
            )
        if isinstance(body, LearningProposal):
            return cls(
                kind=_proposal_kind(tag),
                name=body.name,
                trigger=body.trigger,
                problem=body.problem,
                preconditions=list(body.preconditions),
                steps=list(body.steps),
                anti_steps=list(body.anti_steps),
                evidence=list(body.evidence),
                acceptance_tests=list(body.acceptance_tests),
                raw_body=body.raw_body,
            )
        return LearningProposalDraftBodyBuilder.from_body(tag, body).to_draft()

    @classmethod
    def _normalize_body_key(cls, value: Any) -> str:
        key = str(value).strip().lower().replace(" ", "_")
        return cls.FIELD_ALIASES.get(key, key)

    @staticmethod
    def _parse_inline_items(value: Any) -> list[str]:
        return CommaSeparatedItems.from_value(value).to_list()

    def to_dict(self) -> dict[str, Any]:
        return {
            "kind": self.kind,
            "name": self.name,
            "trigger": self.trigger,
            "problem": self.problem,
            "preconditions": list(self.preconditions),
            "steps": list(self.steps),
            "anti_steps": list(self.anti_steps),
            "evidence": list(self.evidence),
            "acceptance_tests": list(self.acceptance_tests),
            "raw_body": self.raw_body,
        }

    def to_proposal(self) -> "LearningProposal":
        return LearningProposal.coerce(self.to_dict())


class LearningProposalDraftBodyBuilder(BridgeModel):
    """Typed accumulator for hidden learning proposal trailer bodies."""

    kind: str = "skill_proposal"
    raw_body: str = ""
    data: dict[str, Any] = Field(default_factory=dict)
    active_key: str | None = None

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> str:
        return _proposal_kind(value)

    @field_validator("raw_body", mode="before")
    @classmethod
    def _coerce_raw_body(cls, value: Any) -> str:
        return str(value) if value is not None else ""

    @field_validator("data", mode="before")
    @classmethod
    def _coerce_data(cls, value: Any) -> dict[str, Any]:
        return dict(value) if isinstance(value, dict) else {}

    @field_validator("active_key", mode="before")
    @classmethod
    def _coerce_active_key(cls, value: Any) -> str | None:
        return value if isinstance(value, str) and value else None

    @classmethod
    def from_body(cls, tag: Any, body: Any) -> "LearningProposalDraftBodyBuilder":
        builder = cls(kind=tag, raw_body=body)
        for line in HiddenTrailerBodyLine.iter_body(builder.raw_body):
            builder = builder.ingest_line(line)
        return builder

    def ingest_line(self, line: HiddenTrailerBodyLine) -> "LearningProposalDraftBodyBuilder":
        normalized_key = LearningProposalDraft._normalize_body_key(line.key)
        if line.has_key_value and normalized_key in LearningProposalDraft.model_fields:
            if normalized_key in LearningProposalDraft.LIST_FIELDS:
                return self.with_list_items(
                    normalized_key,
                    LearningProposalDraft._parse_inline_items(line.value),
                )
            return self.with_scalar(normalized_key, line.value)
        if self.active_key and line.is_bullet:
            return self.with_list_items(self.active_key, [line.bullet])
        return self

    def with_scalar(self, key: str, value: Any) -> "LearningProposalDraftBodyBuilder":
        data = dict(self.data)
        data[key] = value
        return self.model_copy(update={"data": data, "active_key": None})

    def with_list_items(
        self,
        key: str,
        values: Iterable[str],
    ) -> "LearningProposalDraftBodyBuilder":
        data = dict(self.data)
        existing = CommaSeparatedItems.from_value(data.get(key, ())).to_list(
            max_items=MAX_LEARNING_PROPOSAL_FIELD_ITEMS,
        )
        incoming = CommaSeparatedItems.from_value(values).to_list(
            max_items=MAX_LEARNING_PROPOSAL_FIELD_ITEMS,
        )
        data[key] = [*existing, *incoming][:MAX_LEARNING_PROPOSAL_FIELD_ITEMS]
        return self.model_copy(update={"data": data, "active_key": key})

    def to_draft(self) -> LearningProposalDraft:
        return LearningProposalDraft.model_validate({
            **self.data,
            "kind": self.kind,
            "raw_body": self.raw_body,
        })


class LearningProposal(BridgeModel):
    schema_version: int = 1
    status: str = "pending"
    kind: str = "skill_proposal"
    agent: str = "unknown"
    name: str = ""
    trigger: str = ""
    problem: str = ""
    preconditions: list[str] = Field(default_factory=list)
    steps: list[str] = Field(default_factory=list)
    anti_steps: list[str] = Field(default_factory=list)
    evidence: list[str] = Field(default_factory=list)
    acceptance_tests: list[str] = Field(default_factory=list)
    raw_body: str = ""
    content_hash: str = ""
    created_at: str | None = None
    accepted_at: str | None = None
    rejected_at: str | None = None
    extra: dict[str, Any] = Field(default_factory=dict)

    @staticmethod
    def safe_slug(value: Any, *, fallback: str = "proposal", limit: int = 80) -> str:
        slug = re.sub(r"[^a-zA-Z0-9_-]+", "-", str(value or "").strip().lower())
        slug = re.sub(r"-{2,}", "-", slug).strip("-")
        try:
            max_len = int(limit)
        except (TypeError, ValueError):
            max_len = 80
        if max_len <= 0:
            max_len = 80
        return slug[:max_len] or fallback

    @staticmethod
    def short_items(items: Any, limit: int) -> str:
        try:
            max_items = int(limit)
        except (TypeError, ValueError):
            max_items = 0
        if max_items <= 0:
            return ""
        values = [
            item.strip()
            for item in _coerce_str_or_list(items)
            if item.strip()
        ][:max_items]
        return "; ".join(values)

    @classmethod
    def from_mapping(cls, value: Any) -> "LearningProposal":
        if isinstance(value, cls):
            return value
        data = _mapping(value, "learning_proposal")
        schema_version = data.get("schema_version", 1)
        if isinstance(schema_version, bool) or not isinstance(schema_version, int):
            raise BridgeValidationError("schema_version", "expected integer")
        if schema_version <= 0:
            raise BridgeValidationError("schema_version", "expected positive integer")

        known = {
            "schema_version",
            "status",
            "kind",
            "agent",
            "name",
            "trigger",
            "problem",
            "preconditions",
            "steps",
            "anti_steps",
            "evidence",
            "acceptance_tests",
            "raw_body",
            "content_hash",
            "created_at",
            "accepted_at",
            "rejected_at",
        }
        extra = {key: item for key, item in data.items() if key not in known}
        name = _optional_str(data, "name") or ""
        problem = _optional_str(data, "problem") or ""
        if not name:
            name = problem[:80].strip()
        return cls(
            schema_version=schema_version,
            status=_proposal_status(data.get("status", "pending"), default="pending", strict=True),
            kind=_proposal_kind(data.get("kind", "skill_proposal"), strict=True),
            agent=_optional_str(data, "agent") or "unknown",
            name=name,
            trigger=_optional_str(data, "trigger") or "",
            problem=problem,
            preconditions=_optional_str_list(data, "preconditions"),
            steps=_optional_str_list(data, "steps"),
            anti_steps=_optional_str_list(data, "anti_steps"),
            evidence=_optional_str_list(data, "evidence"),
            acceptance_tests=_optional_str_list(data, "acceptance_tests"),
            raw_body=_optional_str(data, "raw_body") or "",
            content_hash=_optional_str(data, "content_hash") or "",
            created_at=_optional_learning_timestamp(data, "created_at"),
            accepted_at=_optional_learning_timestamp(data, "accepted_at"),
            rejected_at=_optional_learning_timestamp(data, "rejected_at"),
            extra=extra,
        )

    @classmethod
    def from_file_text(
        cls,
        value: str,
        *,
        default_status: str = "accepted",
    ) -> "LearningProposal":
        if isinstance(value, cls):
            return value
        data = _json_object_from_text(value, "learning_proposal")
        return cls.coerce(
            data,
            agent_name=data.get("agent", "unknown"),
            status=data.get("status", default_status),
        )

    @classmethod
    def from_tag_body(cls, tag: Any, body: Any) -> "LearningProposal":
        return LearningProposalDraft.from_tag_body(tag, body).to_proposal()

    @classmethod
    def all_from_trailer_text(
        cls,
        text: Any,
        *,
        tags: Iterable[str] = LEARNING_PROPOSAL_KINDS,
    ) -> list["LearningProposal"]:
        proposals: list[LearningProposal] = []
        for block in HiddenTrailerBlock.all_from_text(text, tuple(tags)):
            proposal = cls.from_tag_body(block.tag, block.body)
            if proposal.is_meaningful():
                proposals.append(proposal)
        return proposals

    @classmethod
    def strip_trailer_text(
        cls,
        text: Any,
        *,
        tags: Iterable[str] = LEARNING_PROPOSAL_KINDS,
    ) -> str:
        return HiddenTrailerBlock.strip_from_text(text, tuple(tags))

    @classmethod
    def coerce(
        cls,
        value: Any,
        *,
        agent_name: str | None = None,
        status: str | None = None,
    ) -> "LearningProposal":
        if isinstance(value, cls):
            return value.with_overrides(agent_name=agent_name, status=status)
        data = value if isinstance(value, dict) else {}
        schema_version = data.get("schema_version", 1)
        if isinstance(schema_version, bool) or not isinstance(schema_version, int) or schema_version <= 0:
            schema_version = 1

        def clean_text(key: str) -> str:
            item = data.get(key, "")
            return item.strip() if isinstance(item, str) else ""

        known = {
            "schema_version",
            "status",
            "kind",
            "agent",
            "name",
            "trigger",
            "problem",
            "preconditions",
            "steps",
            "anti_steps",
            "evidence",
            "acceptance_tests",
            "raw_body",
            "content_hash",
            "created_at",
            "accepted_at",
            "rejected_at",
        }
        extra = {key: item for key, item in data.items() if key not in known}
        agent_value = agent_name if agent_name is not None else data.get("agent", "unknown")
        agent = str(agent_value or "unknown")
        problem = clean_text("problem")
        name = clean_text("name") or problem[:80].strip()
        return cls(
            schema_version=schema_version,
            status=_proposal_status(
                status if status is not None else data.get("status", "pending"),
                default="pending",
            ),
            kind=_proposal_kind(data.get("kind", "skill_proposal")),
            agent=agent,
            name=name,
            trigger=clean_text("trigger"),
            problem=problem,
            preconditions=_coerce_str_or_list(
                data.get("preconditions", []),
                max_items=MAX_LEARNING_PROPOSAL_FIELD_ITEMS,
            ),
            steps=_coerce_str_or_list(
                data.get("steps", []),
                max_items=MAX_LEARNING_PROPOSAL_FIELD_ITEMS,
            ),
            anti_steps=_coerce_str_or_list(
                data.get("anti_steps", []),
                max_items=MAX_LEARNING_PROPOSAL_FIELD_ITEMS,
            ),
            evidence=_coerce_str_or_list(
                data.get("evidence", []),
                max_items=MAX_LEARNING_PROPOSAL_FIELD_ITEMS,
            ),
            acceptance_tests=_coerce_str_or_list(
                data.get("acceptance_tests", []),
                max_items=MAX_LEARNING_PROPOSAL_FIELD_ITEMS,
            ),
            raw_body=clean_text("raw_body"),
            content_hash=clean_text("content_hash"),
            created_at=clean_text("created_at") or None,
            accepted_at=clean_text("accepted_at") or None,
            rejected_at=clean_text("rejected_at") or None,
            extra=extra,
        )

    @classmethod
    def candidate_model(
        cls,
        value: Any,
        *,
        agent_name: str | None = None,
        status: str | None = None,
        default_status: str = "pending",
    ) -> "LearningProposal | None":
        if isinstance(value, cls):
            return value.with_overrides(agent_name=agent_name, status=status)
        if not isinstance(value, dict):
            return None
        resolved_agent = agent_name if agent_name is not None else value.get("agent", "unknown")
        resolved_status = status if status is not None else value.get("status", default_status)
        return cls.coerce(
            value,
            agent_name=resolved_agent,
            status=resolved_status,
        )

    def with_overrides(
        self,
        *,
        agent_name: str | None = None,
        status: str | None = None,
    ) -> "LearningProposal":
        updates: dict[str, Any] = {}
        if agent_name is not None:
            updates["agent"] = str(agent_name or "unknown")
        if status is not None:
            updates["status"] = _proposal_status(status, default=self.status)
        return self.model_copy(update=updates) if updates else self

    def is_meaningful(self) -> bool:
        if not self.name and not self.problem:
            return False
        return bool(
            self.steps
            or self.anti_steps
            or self.evidence
            or self.acceptance_tests
            or self.trigger
            or self.problem
        )

    def hash_payload(self) -> dict[str, Any]:
        return {
            "kind": self.kind,
            "name": self.name,
            "trigger": self.trigger,
            "problem": self.problem,
            "preconditions": list(self.preconditions),
            "steps": list(self.steps),
            "anti_steps": list(self.anti_steps),
            "evidence": list(self.evidence),
            "acceptance_tests": list(self.acceptance_tests),
        }

    def hash_payload_json(self) -> str:
        return json.dumps(self.hash_payload(), sort_keys=True, separators=(",", ":"))

    def stable_content_hash(self, *, length: int = 16) -> str:
        try:
            digest_len = int(length)
        except (TypeError, ValueError):
            digest_len = 16
        digest_len = max(1, digest_len)
        return hashlib.sha256(
            self.hash_payload_json().encode("utf-8"),
        ).hexdigest()[:digest_len]

    def display_name(self) -> str:
        return self.name or self.problem or self.kind

    def accepted_memory_line(
        self,
        *,
        max_steps: int = 3,
        max_anti_steps: int = 2,
    ) -> str:
        parts = []
        trigger = self.trigger or self.problem
        if trigger:
            parts.append(f"when {trigger}")
        steps = self.short_items(self.steps, max_steps)
        if steps:
            parts.append(f"do {steps}")
        anti_steps = self.short_items(self.anti_steps, max_anti_steps)
        if anti_steps:
            parts.append(f"avoid {anti_steps}")
        if not parts:
            evidence = self.short_items(self.evidence, 1)
            if evidence:
                parts.append(evidence)
        if not parts:
            return ""
        return f"- {self.display_name()}: " + "; ".join(parts)

    def to_dict(self) -> dict[str, Any]:
        result = dict(self.extra)
        result.update({
            "schema_version": self.schema_version,
            "status": self.status,
            "kind": self.kind,
            "agent": self.agent,
            "name": self.name,
            "trigger": self.trigger,
            "problem": self.problem,
            "preconditions": list(self.preconditions),
            "steps": list(self.steps),
            "anti_steps": list(self.anti_steps),
            "evidence": list(self.evidence),
            "acceptance_tests": list(self.acceptance_tests),
            "raw_body": self.raw_body,
        })
        if self.content_hash:
            result["content_hash"] = self.content_hash
        if self.created_at:
            result["created_at"] = self.created_at
        if self.accepted_at:
            result["accepted_at"] = self.accepted_at
        if self.rejected_at:
            result["rejected_at"] = self.rejected_at
        return result

    def to_json_text(self) -> str:
        return json.dumps(self.to_dict(), indent=2, sort_keys=True) + "\n"


class LearningProposalCollection(BridgeModel):
    """Typed collection boundary for already-structured learning proposals."""

    proposals: tuple[LearningProposal, ...] = ()

    @field_validator("proposals", mode="before")
    @classmethod
    def _coerce_proposals(cls, value: Any) -> tuple[LearningProposal, ...]:
        if value is None:
            items: list[Any] = []
        elif isinstance(value, LearningProposal):
            items = [value]
        elif isinstance(value, LearningProposalDraft):
            items = [value]
        elif isinstance(value, dict):
            items = [value]
        elif isinstance(value, Iterable) and not isinstance(value, (str, bytes, dict)):
            items = list(value)
        else:
            items = []

        proposals: list[LearningProposal] = []
        for item in items:
            if isinstance(item, LearningProposal):
                proposal = item
            elif isinstance(item, LearningProposalDraft):
                proposal = item.to_proposal()
            else:
                proposal = LearningProposal.candidate_model(item)
            if proposal and proposal.is_meaningful():
                proposals.append(proposal)
        return tuple(proposals)

    @classmethod
    def from_value(cls, value: Any) -> "LearningProposalCollection":
        if isinstance(value, cls):
            return value
        return cls(proposals=value)

    def to_list(self) -> list[LearningProposal]:
        return list(self.proposals)

    def to_dicts(self) -> list[dict[str, Any]]:
        return [proposal.to_dict() for proposal in self.proposals]
