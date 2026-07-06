from __future__ import annotations

from ._compat import import_namespace as _import_namespace

_import_namespace(globals(), 'base', 'live')

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
        log_text_limit: int = 300,
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
            text=_single_line_text(raw_text, limit=log_text_limit),
            journal_failure_text=journal_text,
        )

    @property
    def should_emit_log(self) -> bool:
        return self.classification != ToolResultClassification.OK or bool(self.text.strip())

    @property
    def should_journal_failure(self) -> bool:
        return bool(self.journal_failure_text)
