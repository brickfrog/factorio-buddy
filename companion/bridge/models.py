"""Typed bridge boundary models.

These models keep the existing JSON file formats stable while making shape
validation explicit at the Python bridge edges. They are intentionally small:
the bridge is still a flat-script app, so callers can convert back to plain
dicts where the older code expects them.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from typing import Any


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


class BridgeValidationError(ValueError):
    """Validation error with a stable field path for operator-facing messages."""

    def __init__(self, field_path: str, message: str):
        self.field_path = str(field_path or "<root>")
        self.message = str(message)
        super().__init__(f"{self.field_path}: {self.message}")


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
    elif isinstance(value, list):
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


def _optional_response_format(data: dict[str, Any]) -> dict[str, Any] | None:
    value = data.get("response_format")
    if value is None:
        return None
    return _mapping(value, "response_format")


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


@dataclass(frozen=True)
class ToolCallRequest:
    tool_name: str
    tool_input: dict[str, Any] = field(default_factory=dict)

    @classmethod
    def from_hook_input(cls, value: Any) -> "ToolCallRequest":
        data = _mapping(value, "tool_call")
        tool_input = data.get("tool_input", {})
        if not isinstance(tool_input, dict):
            raise BridgeValidationError("tool_input", "expected object")
        return cls(
            tool_name=_required_str(data, "tool_name"),
            tool_input=dict(tool_input),
        )

    def validate_params(
        self,
        *,
        required: dict[str, str] | None = None,
        optional: dict[str, str] | None = None,
    ) -> None:
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


@dataclass(frozen=True)
class AgentProfile:
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
    response_format: dict[str, Any] | None = None
    extra: dict[str, Any] = field(default_factory=dict)

    @classmethod
    def from_mapping(cls, value: Any) -> "AgentProfile":
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
            sdk_skills=_optional_sdk_skills(data),
            response_format=_optional_response_format(data),
            extra=extra,
        )

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
                result[key] = value
        return result


@dataclass(frozen=True)
class LedgerState:
    objective: str
    plan_steps: list[str] = field(default_factory=list)
    progress_notes: list[str] = field(default_factory=list)
    updated_at: str = ""

    @classmethod
    def default(cls) -> "LedgerState":
        return cls(objective="", plan_steps=[], progress_notes=[], updated_at="")

    @classmethod
    def from_mapping(cls, value: Any) -> "LedgerState":
        data = _mapping(value, "ledger")
        return cls(
            objective=_required_any_str(data, "objective"),
            plan_steps=_required_str_list(data, "plan_steps"),
            progress_notes=_required_str_list(data, "progress_notes"),
            updated_at=_optional_str(data, "updated_at") or "",
        )

    @classmethod
    def coerce(cls, value: Any) -> "LedgerState":
        if not isinstance(value, dict):
            return cls.default()
        objective = value.get("objective", "")
        updated_at = value.get("updated_at", "")
        return cls(
            objective=objective if isinstance(objective, str) else "",
            plan_steps=_coerce_str_list(value.get("plan_steps", [])),
            progress_notes=_coerce_str_list(value.get("progress_notes", [])),
            updated_at=updated_at if isinstance(updated_at, str) else "",
        )

    def to_dict(self) -> dict[str, Any]:
        return {
            "objective": self.objective,
            "plan_steps": list(self.plan_steps),
            "progress_notes": list(self.progress_notes),
            "updated_at": self.updated_at,
        }


JOURNAL_EVENT_KINDS = {"progress", "failure", "discovery", "milestone"}


@dataclass(frozen=True)
class JournalEvent:
    ts: str
    kind: str
    text: str

    @classmethod
    def create(cls, *, ts: str, kind: str, text: Any) -> "JournalEvent":
        normalized_kind = kind if kind in JOURNAL_EVENT_KINDS else "progress"
        return cls(ts=str(ts), kind=normalized_kind, text=str(text))

    @classmethod
    def from_mapping(cls, value: Any) -> "JournalEvent | None":
        if not isinstance(value, dict):
            return None
        return cls.create(
            ts=value.get("ts", ""),
            kind=value.get("kind", "progress"),
            text=value.get("text", ""),
        )

    def to_dict(self) -> dict[str, str]:
        return {
            "ts": self.ts,
            "kind": self.kind,
            "text": self.text,
        }


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


@dataclass(frozen=True)
class LearningProposal:
    schema_version: int = 1
    status: str = "pending"
    kind: str = "skill_proposal"
    agent: str = "unknown"
    name: str = ""
    trigger: str = ""
    problem: str = ""
    preconditions: list[str] = field(default_factory=list)
    steps: list[str] = field(default_factory=list)
    anti_steps: list[str] = field(default_factory=list)
    evidence: list[str] = field(default_factory=list)
    acceptance_tests: list[str] = field(default_factory=list)
    raw_body: str = ""
    content_hash: str = ""
    created_at: str | None = None
    accepted_at: str | None = None
    rejected_at: str | None = None
    extra: dict[str, Any] = field(default_factory=dict)

    @classmethod
    def from_mapping(cls, value: Any) -> "LearningProposal":
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
    def coerce(
        cls,
        value: Any,
        *,
        agent_name: str | None = None,
        status: str = "pending",
    ) -> "LearningProposal":
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
            status=_proposal_status(status if status is not None else data.get("status"), default="pending"),
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
