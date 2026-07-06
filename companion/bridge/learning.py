"""Bridge-side evolving memory proposals.

Agents may emit hidden proposal trailers when they discover reusable
procedures or tooling gaps. The bridge persists those trailers as inert local
artifacts. Pending artifacts are not injected back into prompts; only accepted
artifacts are rendered as compact procedural memory.
"""

from __future__ import annotations

import hashlib
import json
import os
import re
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, ClassVar, Iterable

from pydantic import Field, field_validator

from models.base import (
    BridgeModel,
    BridgeValidationError,
    CommaSeparatedItems,
    _json_object_from_text,
)
from models.rcon_models import HiddenTrailerBlock
from models.settings_models import LearningRuntimeSettings
from models.tool_schema import (
    _coerce_str_or_list,
    _mapping,
    _optional_str,
    _optional_str_list,
)


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
        from ledger import HiddenTrailerBodyLine

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


LEARNING_TAGS = LEARNING_PROPOSAL_KINDS
MAX_RENDERED_ACCEPTED = 8
MAX_RENDERED_STEPS = 3
MAX_RENDERED_ANTI_STEPS = 2


def _project_root() -> Path:
    return Path(__file__).resolve().parents[2]


def _learning_dir() -> Path:
    return LearningRuntimeSettings.from_env(os.environ).resolved_learning_dir(
        _project_root(),
    )


def _utc_now() -> datetime:
    return datetime.now(timezone.utc)


def _iso_utc(now: datetime | None = None) -> str:
    if now is None:
        now = _utc_now()
    if now.tzinfo is None:
        now = now.replace(tzinfo=timezone.utc)
    return now.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def parse_learning_trailer_models(
    source: object,
) -> list[LearningProposal]:
    if not isinstance(source, (str, bytes)):
        proposals = LearningProposalCollection.from_value(source).to_list()
        if proposals:
            return proposals
    return LearningProposal.all_from_trailer_text(source, tags=LEARNING_TAGS)


def _proposal_with_hash(proposal: LearningProposal) -> LearningProposal:
    return proposal.model_copy(update={"content_hash": _candidate_hash(proposal)})


def _candidate_hash(candidate: dict | LearningProposal) -> str:
    proposal = (
        candidate
        if isinstance(candidate, LearningProposal)
        else LearningProposal.coerce(candidate)
    )
    return proposal.stable_content_hash()


def strip_learning_trailers(text: str) -> str:
    return LearningProposal.strip_trailer_text(text, tags=LEARNING_TAGS)


def _status_dir(status: str) -> Path:
    return _learning_dir() / status


def _proposal_filename(proposal: LearningProposal, now: datetime | None = None) -> str:
    if now is None:
        now = _utc_now()
    stamp = now.astimezone(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    name = LearningProposal.safe_slug(proposal.display_name())
    agent = LearningProposal.safe_slug(proposal.agent, fallback="agent")
    digest = proposal.content_hash or _candidate_hash(proposal)
    return f"{stamp}-{agent}-{name}-{digest}.json"


def save_candidate(
    candidate: dict | LearningProposal,
    status: str = "pending",
    now: datetime | None = None,
    agent_name: str | None = None,
) -> Path | None:
    proposal = LearningProposal.candidate_model(
        candidate,
        agent_name=agent_name,
        status=status,
        default_status="pending",
    )
    if not proposal or not proposal.is_meaningful():
        return None
    proposal = _proposal_with_hash(proposal).model_copy(update={"created_at": _iso_utc(now)})
    path = _status_dir(status) / _proposal_filename(proposal, now)
    tmp = path.with_name(path.name + ".tmp")
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp.write_text(proposal.to_json_text())
        os.replace(tmp, path)
    except OSError as e:
        print(f"[learning] WARNING: failed to persist learning candidate: {e}")
        try:
            tmp.unlink(missing_ok=True)
        except OSError:
            pass
        return None
    return path


def promote_candidate(path: str | Path, now: datetime | None = None) -> Path | None:
    source = Path(path)
    proposal = _load_candidate_model(source, default_status="pending")
    if not proposal:
        return None
    proposal = proposal.model_copy(update={
        "status": "accepted",
        "accepted_at": _iso_utc(now),
    })
    target = _status_dir("accepted") / source.name
    tmp = target.with_name(target.name + ".tmp")
    try:
        target.parent.mkdir(parents=True, exist_ok=True)
        tmp.write_text(proposal.to_json_text())
        os.replace(tmp, target)
        try:
            source.unlink()
        except OSError:
            pass
    except OSError as e:
        print(f"[learning] WARNING: failed to promote learning candidate: {e}")
        try:
            tmp.unlink(missing_ok=True)
        except OSError:
            pass
        return None
    return target


def reject_candidate(path: str | Path, now: datetime | None = None) -> Path | None:
    source = Path(path)
    proposal = _load_candidate_model(source, default_status="pending")
    if not proposal:
        return None
    proposal = proposal.model_copy(update={
        "status": "rejected",
        "rejected_at": _iso_utc(now),
    })
    target = _status_dir("rejected") / source.name
    tmp = target.with_name(target.name + ".tmp")
    try:
        target.parent.mkdir(parents=True, exist_ok=True)
        tmp.write_text(proposal.to_json_text())
        os.replace(tmp, target)
        try:
            source.unlink()
        except OSError:
            pass
    except OSError as e:
        print(f"[learning] WARNING: failed to reject learning candidate: {e}")
        try:
            tmp.unlink(missing_ok=True)
        except OSError:
            pass
        return None
    return target


def pending_candidates() -> list[Path]:
    try:
        return sorted(_status_dir("pending").glob("*.json"))
    except OSError:
        return []


def _candidate_title(proposal: LearningProposal) -> str:
    return proposal.name or proposal.problem or proposal.kind


def _candidate_preview(proposal: LearningProposal) -> str:
    for value in (proposal.problem, proposal.trigger):
        text = str(value or "").strip()
        if text:
            return text
    return LearningProposal.short_items(
        [
            *proposal.steps,
            *proposal.evidence,
            *proposal.acceptance_tests,
        ],
        1,
    )


def _relative_candidate_path(path: Path) -> str:
    try:
        return str(path.relative_to(_project_root()))
    except ValueError:
        return str(path)


def format_pending_triage(paths: Iterable[Path] | None = None) -> str:
    candidate_paths = list(paths) if paths is not None else pending_candidates()
    loaded: list[tuple[Path, LearningProposal]] = []
    for path in candidate_paths:
        proposal = _load_candidate_model(path, default_status="pending")
        if proposal:
            loaded.append((path, proposal))
    if not loaded:
        return "No pending learning candidates."

    counts: dict[str, int] = {}
    for _, proposal in loaded:
        counts[proposal.kind] = counts.get(proposal.kind, 0) + 1
    count_text = ", ".join(
        f"{kind}={count}" for kind, count in sorted(counts.items())
    )
    lines = [
        "Pending learning candidates",
        f"total={len(loaded)} {count_text}".strip(),
        "",
    ]
    for path, proposal in loaded:
        lines.append(f"- {proposal.kind}: {_candidate_title(proposal)}")
        lines.append(f"  path: {_relative_candidate_path(path)}")
        if proposal.agent:
            lines.append(f"  agent: {proposal.agent}")
        preview = _candidate_preview(proposal)
        if preview:
            lines.append(f"  why: {preview}")
        tests = LearningProposal.short_items(proposal.acceptance_tests, 2)
        if tests:
            lines.append(f"  acceptance: {tests}")
    return "\n".join(lines)


def apply_learning_update(
    agent_name: str,
    source: object,
) -> list[Path]:
    saved = []
    for proposal in parse_learning_trailer_models(source):
        path = save_candidate(proposal, status="pending", agent_name=agent_name)
        if path:
            saved.append(path)
    return saved


def _load_candidate_model(
    path: Path,
    *,
    default_status: str = "accepted",
) -> LearningProposal | None:
    try:
        proposal = LearningProposal.from_file_text(
            path.read_text(),
            default_status=default_status,
        )
    except (BridgeValidationError, OSError):
        return None
    if not proposal or not proposal.is_meaningful():
        return None
    return _proposal_with_hash(proposal)


def load_accepted_learning_model(
    limit: int = MAX_RENDERED_ACCEPTED,
) -> list[LearningProposal]:
    accepted_dir = _status_dir("accepted")
    try:
        paths = sorted(accepted_dir.glob("*.json"))
    except OSError:
        return []
    candidates: list[LearningProposal] = []
    for path in paths:
        candidate = _load_candidate_model(path, default_status="accepted")
        if candidate:
            candidates.append(candidate)
    candidates.sort(key=lambda item: item.created_at or "")
    try:
        limit = int(limit)
    except (TypeError, ValueError):
        limit = MAX_RENDERED_ACCEPTED
    if limit <= 0:
        return []
    return candidates[-limit:]


def render_accepted_learning(candidates: object) -> str:
    proposals = LearningProposalCollection.from_value(candidates).to_list()
    if not proposals:
        return ""

    lines = ["Accepted learned procedures (reuse when applicable):"]
    for proposal in proposals[-MAX_RENDERED_ACCEPTED:]:
        line = proposal.accepted_memory_line(
            max_steps=MAX_RENDERED_STEPS,
            max_anti_steps=MAX_RENDERED_ANTI_STEPS,
        )
        if line:
            lines.append(line)
    return "\n".join(lines) if len(lines) > 1 else ""


def learning_proposal_prompt() -> str:
    return (
        "If this run reveals a reusable procedure, repeated failure mode, or "
        "tooling gap that would help future runs, emit at most one hidden "
        "<skill_proposal>, <diagnostic_proposal>, <script_proposal>, or "
        "<bug_report> block. Use fields like name, trigger/problem, "
        "preconditions, steps, anti_steps, evidence, and acceptance_tests. "
        "These proposals are inert local artifacts; do not include secrets, "
        "raw credentials, or requests for unrestricted repo access."
    )


def _print_candidate(path: Path) -> None:
    candidate = _load_candidate_model(path, default_status="pending")
    if not candidate:
        print(f"{path}: invalid")
        return
    name = candidate.name or candidate.problem or candidate.kind
    print(f"{path}: {candidate.kind} {name}")


def main(argv: list[str] | None = None) -> int:
    if argv is None:
        argv = sys.argv[1:]
    command = argv[0] if argv else "list"
    if command == "list":
        for path in pending_candidates():
            _print_candidate(path)
        return 0
    if command in {"triage", "report"}:
        print(format_pending_triage())
        return 0
    if command in {"accept", "promote"} and len(argv) == 2:
        target = promote_candidate(argv[1])
        if not target:
            print("failed to promote candidate", file=sys.stderr)
            return 1
        print(target)
        return 0
    if command == "reject" and len(argv) == 2:
        target = reject_candidate(argv[1])
        if not target:
            print("failed to reject candidate", file=sys.stderr)
            return 1
        print(target)
        return 0
    print(
        "usage: learning.py [list|triage] | accept <pending.json> | reject <pending.json>",
        file=sys.stderr,
    )
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
