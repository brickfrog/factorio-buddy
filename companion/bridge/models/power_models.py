from __future__ import annotations

from ._compat import import_namespace as _import_namespace

_import_namespace(globals(), 'base', 'rcon_models')

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
