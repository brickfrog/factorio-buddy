from __future__ import annotations

from ._compat import import_namespace as _import_namespace

_import_namespace(globals(), 'base', 'rcon_models')

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
