from __future__ import annotations

from ._compat import import_namespace as _import_namespace

_import_namespace(globals(), 'base', 'tool_result', 'tool_schema')

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
    autonomy_step_progress: str = ""

    @field_validator("text_parts", mode="before")
    @classmethod
    def _coerce_text_parts(cls, value: Any) -> list[str]:
        return [part for part in _coerce_str_list(value) if part]

    @field_validator("session_id", "autonomy_step_progress", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        text = str(value or "").strip()
        return text

    @field_validator("session_id", mode="after")
    @classmethod
    def _empty_session_to_none(cls, value: str) -> str | None:
        return value or None

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
        autonomy_step_progress: Any = "",
    ) -> "AgentRunTranscript":
        return cls(
            text_parts=text_parts or [],
            session_id=session_id,
            context_window_limit=context_window_limit,
            usage_limit_seen=usage_limit_seen,
            autonomy_step_progress=autonomy_step_progress,
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
