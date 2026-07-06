from __future__ import annotations

from ._compat import import_namespace as _import_namespace

_import_namespace(globals(), 'base', 'live', 'tool_result')

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
