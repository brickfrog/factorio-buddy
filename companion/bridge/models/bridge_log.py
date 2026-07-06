from __future__ import annotations

from ._compat import import_namespace as _import_namespace

_import_namespace(globals(), 'base', 'live', 'tool_result', 'rcon_models', 'power_models')

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
