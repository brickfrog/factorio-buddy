from __future__ import annotations

from ._compat import import_namespace as _import_namespace

_import_namespace(globals(), 'base')

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
