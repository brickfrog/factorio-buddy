"""Append-only per-agent journal and reflected lessons for bridge autonomy."""

from __future__ import annotations

import json
import os
import re
from datetime import datetime
from pathlib import Path
from typing import Any, Callable, ClassVar, Iterable

from pydantic import BaseModel, Field, ValidationError, field_validator

from models.base import (
    BridgeModel,
    BridgeTextLines,
    JournalFailureKind,
    ProgressSignal,
    ReflectionDropKind,
    ToolResultClassification,
    ToolResultTextKind,
    _JSON_MISSING,
    _json_object_from_text,
    _json_value_or_missing,
    progress_signal,
)
from models.live import LedgerReadinessEvidence, ProviderUsageLimit
from models.rcon_models import HiddenTrailerBlock
from models.tool_result import SdkErrorSignal, ToolResultOutcome
from models.tool_schema import _coerce_str_or_list


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
        from ledger import HiddenTrailerBodyLine

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


MAX_REFLECTION_ITEMS = 12
MAX_REFLECTION_ITEM_TEXT = 180
MAX_RENDERED_EVENTS = 5
MAX_RENDERED_EVENT_TEXT = 500
USEFUL_EVENT_KINDS = {"progress", "discovery", "milestone"}


def _journal_file(agent_name: str) -> Path:
    return Path(__file__).resolve().parent / f".journal-{agent_name}.jsonl"


def _reflection_file(agent_name: str) -> Path:
    return Path(__file__).resolve().parent / f".reflection-{agent_name}.json"


def default_reflection_model() -> ReflectionMemory:
    return ReflectionMemory()


def coalesce_events_model(
    events: JournalWindow | list[dict | JournalEvent],
    max_items: int = MAX_RENDERED_EVENTS,
) -> list[JournalPromptEvent]:
    """Return prompt-ready events with adjacent identical entries collapsed.

    The journal stays append-only and raw on disk; this compaction is only for
    prompt injection so repeated failures don't crowd out useful context.
    """
    window = events if isinstance(events, JournalWindow) else JournalWindow.coerce(events)
    return window.prompt_events(
        max_items=max_items,
        text_limit=MAX_RENDERED_EVENT_TEXT,
        useful_kinds=USEFUL_EVENT_KINDS,
    )


def append_event(
    agent_name: str,
    kind: str,
    text: str,
    *,
    signal: ProgressSignal | str | None = None,
) -> None:
    event = JournalEvent.create(
        ts=datetime.now().isoformat(),
        kind=kind,
        text=text,
        signal=signal,
    )
    if event.should_drop():
        return None
    path = _journal_file(agent_name)
    try:
        with path.open("a", encoding="utf-8") as f:
            f.write(event.to_json_line())
    except OSError as e:
        print(f"[journal] WARNING: failed to append journal event for {agent_name}: {e}")
    return None


def load_events_model(agent_name: str, limit: int = 20) -> JournalWindow:
    try:
        raw_lines = BridgeTextLines.from_text(
            _journal_file(agent_name).read_text(),
            keep_blank=False,
        ).lines
    except (ValueError, OSError):
        return JournalWindow()

    events = []
    for line in raw_lines:
        event = JournalEvent.from_json_line(line)
        if not event:
            continue
        if event.should_drop():
            continue
        events.append(event)

    try:
        limit = int(limit)
    except (TypeError, ValueError):
        limit = 20
    if limit <= 0:
        return JournalWindow()
    return JournalWindow(events=events[-limit:])


def count_events(agent_name: str) -> int:
    try:
        raw_lines = BridgeTextLines.from_text(
            _journal_file(agent_name).read_text(),
            keep_blank=False,
        ).lines
    except (ValueError, OSError):
        return 0
    count = 0
    for line in raw_lines:
        if JournalEvent.from_json_line(line):
            count += 1
    return count


def should_reflect(event_count: int, interval: int = 16) -> bool:
    try:
        event_count = int(event_count)
        interval = int(interval)
    except (TypeError, ValueError):
        return False
    return event_count > 0 and interval > 0 and event_count % interval == 0


def load_reflection_model(agent_name: str) -> ReflectionMemory:
    try:
        memory = ReflectionMemory.from_file_text(
            _reflection_file(agent_name).read_text(),
            max_items=MAX_REFLECTION_ITEMS,
            max_len=MAX_REFLECTION_ITEM_TEXT,
        )
    except (ValueError, OSError):
        return default_reflection_model()
    return memory


def save_reflection_model(agent_name: str, reflection: ReflectionMemory | dict) -> None:
    path = _reflection_file(agent_name)
    tmp = path.with_name(path.name + ".tmp")
    try:
        payload = ReflectionMemory.coerce(
            reflection,
            max_items=MAX_REFLECTION_ITEMS,
            max_len=MAX_REFLECTION_ITEM_TEXT,
        ).to_json_line()
    except TypeError as e:
        print(f"[journal] WARNING: refusing to save unserializable reflection for "
              f"{agent_name}: {e}")
        return None
    try:
        tmp.write_text(payload)
        os.replace(tmp, path)
    except OSError as e:
        print(f"[journal] WARNING: failed to persist reflection for {agent_name}: {e}")
        try:
            tmp.unlink(missing_ok=True)
        except OSError:
            pass
    return None


def parse_reflection_model(source: str | ReflectionDraft) -> ReflectionDraft | None:
    return ReflectionDraft.from_trailer_text(
        source,
        max_items=MAX_REFLECTION_ITEMS,
        max_len=MAX_REFLECTION_ITEM_TEXT,
    )


def apply_reflection_update_model(
    agent_name: str,
    source: str | ReflectionDraft,
) -> ReflectionMemory:
    parsed = parse_reflection_model(source)
    current = load_reflection_model(agent_name)
    if parsed is None:
        return current

    reflection = current.merged_with(
        parsed,
        updated_at=datetime.now().isoformat(),
        max_items=MAX_REFLECTION_ITEMS,
        max_len=MAX_REFLECTION_ITEM_TEXT,
    )
    save_reflection_model(agent_name, reflection)
    return reflection


def strip_reflection_trailer(text: str) -> str:
    return ReflectionDraft.strip_trailer_text(text)


def render_memory(
    events: JournalWindow | list[dict | JournalEvent],
    reflection: ReflectionMemory | dict,
) -> str:
    journal_window = events if isinstance(events, JournalWindow) else JournalWindow.coerce(events)
    reflection_memory = (
        reflection
        if isinstance(reflection, ReflectionMemory)
        else ReflectionMemory.coerce(
            reflection,
            max_items=MAX_REFLECTION_ITEMS,
            max_len=MAX_REFLECTION_ITEM_TEXT,
        )
    )
    recent_events = journal_window.prompt_events(
        max_items=MAX_RENDERED_EVENTS,
        text_limit=MAX_RENDERED_EVENT_TEXT,
        useful_kinds=USEFUL_EVENT_KINDS,
    )
    structures = reflection_memory.structures
    error_tips = reflection_memory.error_tips
    if not recent_events and not structures and not error_tips:
        return ""

    lines = []
    if recent_events:
        lines.append("Recent events:")
        for event in recent_events:
            lines.append(event.render_line())
    if structures or error_tips:
        if lines:
            lines.append("")
        lines.append("Lessons (EXISTING STRUCTURES / ERROR TIPS):")
        if structures:
            lines.append("EXISTING STRUCTURES:")
            for item in structures:
                lines.append(f"- {item}")
        if error_tips:
            lines.append("ERROR TIPS:")
            for item in error_tips:
                lines.append(f"- {item}")
    return "\n".join(lines)
