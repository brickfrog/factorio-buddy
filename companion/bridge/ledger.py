"""Persistent per-agent objective ledger for bridge autonomy continuity."""

from __future__ import annotations

import json
import os
import re
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Callable, Iterable

from pydantic import Field, field_validator

from runtime_paths import read_candidates, state_file
from models.base import (
    AutonomyMode,
    BridgeModel,
    BridgeTextLines,
    BridgeValidationError,
    KeyValueTextSplit,
    LedgerNextRequiredMode,
    LedgerStalenessKind,
    LedgerStatus,
    ProgressSignal,
    autonomy_mode,
    ledger_next_required_mode,
    ledger_status,
    progress_signal,
    _json_object_from_text,
)
from models.bridge_log import BridgeLogMessage
from models.live import (
    LedgerObjectiveIntent,
    LedgerProgressSignals,
    LedgerReadinessEvidence,
    LiveCompletionEvidence,
    LiveState,
)
from models.rcon_models import HiddenTrailerBlock
from models.settings_models import LedgerRuntimeSettings
from models.tool_schema import (
    _coerce_str_list,
    _coerce_str_or_list,
    _mapping,
    _optional_str,
    _required_any_str,
    _required_str_list,
)


class LedgerState(BridgeModel):
    objective: str
    plan_steps: list[str] = Field(default_factory=list)
    progress_notes: list[str] = Field(default_factory=list)
    updated_at: str = ""
    signal: ProgressSignal = ProgressSignal.NONE
    status: LedgerStatus = LedgerStatus.NONE
    next_required_mode: LedgerNextRequiredMode = LedgerNextRequiredMode.NONE
    blocker: str = ""

    @classmethod
    def default(cls) -> "LedgerState":
        return cls(
            objective="",
            plan_steps=[],
            progress_notes=[],
            updated_at="",
            signal=ProgressSignal.NONE,
            status=LedgerStatus.NONE,
            next_required_mode=LedgerNextRequiredMode.NONE,
            blocker="",
        )

    @classmethod
    def from_mapping(cls, value: Any) -> "LedgerState":
        if isinstance(value, cls):
            return value
        data = _mapping(value, "ledger")
        return cls(
            objective=_required_any_str(data, "objective"),
            plan_steps=_required_str_list(data, "plan_steps"),
            progress_notes=_required_str_list(data, "progress_notes"),
            updated_at=_optional_str(data, "updated_at") or "",
            signal=progress_signal(data.get("signal")),
            status=ledger_status(data.get("status")),
            next_required_mode=ledger_next_required_mode(data.get("next_required_mode")),
            blocker=_optional_str(data, "blocker") or "",
        )

    @classmethod
    def coerce(cls, value: Any) -> "LedgerState":
        if isinstance(value, cls):
            return value
        if not isinstance(value, dict):
            return cls.default()
        objective = value.get("objective", "")
        updated_at = value.get("updated_at", "")
        blocker = value.get("blocker", "")
        return cls(
            objective=objective if isinstance(objective, str) else "",
            plan_steps=_coerce_str_list(value.get("plan_steps", [])),
            progress_notes=_coerce_str_list(value.get("progress_notes", [])),
            updated_at=updated_at if isinstance(updated_at, str) else "",
            signal=progress_signal(value.get("signal")),
            status=ledger_status(value.get("status")),
            next_required_mode=ledger_next_required_mode(value.get("next_required_mode")),
            blocker=blocker.strip() if isinstance(blocker, str) else "",
        )

    @classmethod
    def normalized(
        cls,
        value: Any,
        *,
        progress_should_drop: Callable[[str], bool] | None = None,
    ) -> "LedgerState":
        return cls.coerce(value).cleaned(progress_should_drop=progress_should_drop)

    @classmethod
    def from_file_text(
        cls,
        value: str,
        *,
        progress_should_drop: Callable[[str], bool] | None = None,
    ) -> "LedgerState":
        if isinstance(value, cls):
            return cls.normalized(
                value,
                progress_should_drop=progress_should_drop,
            )
        data = _json_object_from_text(value, "ledger")
        return cls.normalized(data, progress_should_drop=progress_should_drop)

    def cleaned(
        self,
        *,
        progress_should_drop: Callable[[str], bool] | None = None,
    ) -> "LedgerState":
        notes: list[str] = []
        for note in self.progress_notes:
            try:
                should_drop = bool(progress_should_drop(note)) if progress_should_drop else False
            except Exception:
                should_drop = False
            if not should_drop:
                notes.append(note)
        return LedgerState(
            objective=self.objective,
            plan_steps=list(self.plan_steps),
            progress_notes=notes,
            updated_at=self.updated_at,
            signal=self.signal,
            status=self.status,
            next_required_mode=self.next_required_mode,
            blocker=self.blocker,
        )

    def merged_with(
        self,
        update: Any,
        *,
        updated_at: str = "",
        max_progress_notes: int = 10,
        progress_should_drop: Callable[[str], bool] | None = None,
    ) -> "LedgerState":
        ledger_update = update if isinstance(update, LedgerUpdate) else LedgerUpdate.coerce(update)
        objective = self.objective
        plan_steps = list(self.plan_steps)
        progress_notes = list(self.progress_notes)
        signal = self.signal
        status = self.status
        next_required_mode = self.next_required_mode
        blocker = self.blocker

        if ledger_update.objective:
            objective = ledger_update.objective
            plan_steps = list(ledger_update.plan_steps)
            status = ledger_update.status
            next_required_mode = ledger_update.next_required_mode
            blocker = ledger_update.blocker
        elif ledger_update.plan_steps:
            plan_steps = list(ledger_update.plan_steps)
        if ledger_update.signal != ProgressSignal.NONE:
            signal = ledger_update.signal
        if ledger_update.status != LedgerStatus.NONE:
            status = ledger_update.status
        if ledger_update.next_required_mode != LedgerNextRequiredMode.NONE:
            next_required_mode = ledger_update.next_required_mode
        if ledger_update.blocker:
            blocker = ledger_update.blocker

        try:
            drop_progress = (
                bool(progress_should_drop(ledger_update.progress))
                if progress_should_drop and ledger_update.progress
                else False
            )
        except Exception:
            drop_progress = False
        if ledger_update.progress and not drop_progress:
            progress_notes.append(ledger_update.progress)

        try:
            limit = int(max_progress_notes)
        except (TypeError, ValueError):
            limit = 10
        limit = max(0, limit)
        if limit:
            progress_notes = progress_notes[-limit:]
        else:
            progress_notes = []

        return LedgerState(
            objective=objective,
            plan_steps=plan_steps,
            progress_notes=progress_notes,
            updated_at=updated_at.strip() if isinstance(updated_at, str) and updated_at.strip() else self.updated_at,
            signal=signal,
            status=status,
            next_required_mode=next_required_mode,
            blocker=blocker,
        ).cleaned(progress_should_drop=progress_should_drop)

    def to_dict(self) -> dict[str, Any]:
        result = {
            "objective": self.objective,
            "plan_steps": list(self.plan_steps),
            "progress_notes": list(self.progress_notes),
            "updated_at": self.updated_at,
        }
        if self.signal != ProgressSignal.NONE:
            result["signal"] = self.signal.value
        if self.status != LedgerStatus.NONE:
            result["status"] = self.status.value
        if self.next_required_mode != LedgerNextRequiredMode.NONE:
            result["next_required_mode"] = self.next_required_mode.value
        if self.blocker:
            result["blocker"] = self.blocker
        return result

    def to_json_line(self) -> str:
        return json.dumps(self.to_dict()) + "\n"

    def readiness_evidence(self) -> LedgerReadinessEvidence:
        return LedgerReadinessEvidence.from_ledger(self)

    def has_execution_ready_plan(self) -> bool:
        return self.readiness_evidence().is_ready

    def active_text(self) -> str:
        parts = [self.objective]
        parts.extend(self.plan_steps)
        return "\n".join(parts).lower()

    def progress_text(self) -> str:
        return "\n".join(self.progress_notes).lower()

    def has_automation_enabling_setup_context(self) -> bool:
        """Return true for bounded setup steps that create automation parts."""
        text = self.active_text()
        if not text:
            return False
        return any(
            marker in text
            for marker in (
                "bootstrap",
                "build_recipe_assembler_cell",
                "durable automation",
                "furnace output",
                "inserter",
                "one-time",
                "initial",
                "plan_recipe_assembler_cell",
                "plate output",
                "recipe assembler",
            )
        )

    def has_durable_recovery_context(self) -> bool:
        """Return true when manual recovery is paired with durable logistics."""
        text = self.active_text()
        if not text:
            return False
        return any(
            marker in text
            for marker in (
                "repair_fuel_sustainability",
                "route_belt",
                "build_fuel_supply",
            )
        )

    def has_stale_manual_automation_plan(
        self,
        live_state: LiveState | None = None,
    ) -> bool:
        """Return true when a plan repeats manual transfers instead of automation.

        Once automation infrastructure exists, plans that only craft/extract/feed
        by hand preserve the wrong end state. This predicate is intentionally
        narrow: science/research manual plans are always stale; fuel/feed plans
        are stale as soon as live state has a real logistics/power footprint;
        output transfer plans need repeated-loop evidence in progress text.
        It yields to durable automation controllers when they are present.
        """
        text = self.active_text()
        if not text:
            return False
        durable_markers = (
            "execute_direct_smelter",
            "execute_edge_miner",
            "repair_fuel_sustainability",
            "diagnose_fuel_sustainability",
            "build_fuel_supply",
            "route_belt",
            "plan_automation_science",
            "build_automation_science",
            "plan_recipe_assembler_cell",
            "build_recipe_assembler_cell",
            "build_assembler_feed",
            "plan_machine_output",
            "build_assembler_output",
            "build_lab_feed",
        )
        if any(marker in text for marker in durable_markers):
            return False
        manual_markers = (
            "feed_lab_from_inventory",
            "hand_feed_furnace",
            "insert_items",
            "extract_items",
            "craft ",
            "craft(",
            "craft_",
        )
        if not any(marker in text for marker in manual_markers):
            return False
        mentions_science = any(
            marker in text
            for marker in (
                "automation-science-pack",
                "science pack",
                "science-pack",
                "research",
                "lab",
            )
        )
        mentions_manual_science_component = any(
            marker in text
            for marker in (
                "iron-gear-wheel",
                "copper-cable",
                "electronic-circuit",
            )
        ) and any(
            marker in text
            for marker in (
                "science",
                "research",
                "lab",
                "automation-science",
            )
        )
        if mentions_science or mentions_manual_science_component:
            return True

        automation_capable = bool(
            live_state and live_state.has_automation_capable_footprint()
        )
        mentions_manual_feed = (
            any(marker in text for marker in ("insert_items", "hand_feed_furnace"))
            and any(
                marker in text
                for marker in (
                    "coal",
                    "fuel",
                    "boiler",
                    "burner",
                    "furnace",
                    "furnace_source",
                    "iron-ore",
                    "copper-ore",
                )
            )
        )
        if automation_capable and mentions_manual_feed:
            return not self.has_durable_recovery_context()

        progress = self.progress_text()
        repeated_loop_text = "\n".join([text, progress])
        repeated_loop_markers = (
            "manual cycle",
            "manual extraction",
            "manual transfer",
            "manual feeding",
            "again",
            "repeated",
            "recurring",
            "continues",
            "still requires manual",
            "runs out",
            "ran out",
            "exhausted",
            "jammed",
            "full_output",
            "reloaded",
            "refueled",
        )

        def has_loop_marker(marker: str) -> bool:
            return re.search(
                rf"(?<![a-z0-9_-]){re.escape(marker)}(?![a-z0-9_-])",
                repeated_loop_text,
            ) is not None

        if not any(has_loop_marker(marker) for marker in repeated_loop_markers):
            return False

        mentions_fuel_loop = (
            any(
                marker in text
                for marker in ("coal", "fuel", "boiler", "burner", "furnace")
            )
            and any(marker in text for marker in ("insert_items", "hand_feed_furnace"))
        )
        mentions_output_loop = (
            "extract_items" in text
            and any(
                marker in repeated_loop_text
                for marker in ("plate", "output", "full_output", "chest", "jam")
            )
        )
        return mentions_fuel_loop or mentions_output_loop

    def age_seconds(self, *, now: datetime | None = None) -> float | None:
        if not self.updated_at:
            return None
        try:
            updated = datetime.fromisoformat(self.updated_at)
        except ValueError:
            return None

        reference = now
        if reference is None:
            reference = datetime.now(updated.tzinfo) if updated.tzinfo else datetime.now()
        elif updated.tzinfo and reference.tzinfo:
            reference = reference.astimezone(updated.tzinfo)
        elif updated.tzinfo and reference.tzinfo is None:
            reference = reference.replace(tzinfo=updated.tzinfo)
        elif updated.tzinfo is None and reference.tzinfo:
            reference = reference.replace(tzinfo=None)
        return max(0.0, (reference - updated).total_seconds())

    def bootstrap_staleness_evidence(
        self,
        *,
        max_age_s: float,
        now: datetime | None = None,
    ) -> "LedgerStalenessEvidence":
        return LedgerStalenessEvidence.from_bootstrap_policy(
            self,
            max_age_s=max_age_s,
            now=now,
        )

    def live_state_completion_evidence(self, live_state: Any) -> LiveCompletionEvidence:
        """Return typed evidence when live state proves this ledger is stale.

        The rules are intentionally conservative: only well-known early-game
        objectives with direct world evidence trigger an automatic planner tick.
        """
        return LiveCompletionEvidence.from_ledger_and_live_state(self, live_state)

    def render(self, *, recent_progress_count: int = 3) -> str:
        objective = BridgeLogMessage.single_line(self.objective, limit=160)
        if not objective:
            return ""

        lines = [
            f"Continuity ledger: continue the committed objective, do not restart it: {objective}",
        ]
        if self.status != LedgerStatus.NONE:
            lines.append(f"Status: {self.status.value}")
        if self.next_required_mode != LedgerNextRequiredMode.NONE:
            lines.append(f"Next required mode: {self.next_required_mode.value}")
        if self.blocker:
            blocker = BridgeLogMessage.single_line(self.blocker, limit=160)
            lines.append(f"Blocker: {blocker}")
        if self.plan_steps:
            lines.append("Plan:")
            for index, step in enumerate(self.plan_steps, start=1):
                rendered_step = BridgeLogMessage.single_line(step, limit=160)
                lines.append(f"{index}. {rendered_step}")
        progress_notes = self.progress_notes[-recent_progress_count:]
        if progress_notes:
            lines.append("Recent progress:")
            for note in progress_notes:
                rendered_note = BridgeLogMessage.single_line(note, limit=180)
                lines.append(f"- {rendered_note}")
        return "\n".join(lines)


class AutonomyPromptInput(BridgeModel):
    """Typed source context for assembling one autonomy tick prompt."""

    mode: AutonomyMode = AutonomyMode.PLAN
    ledger: LedgerState = Field(default_factory=LedgerState.default)
    live_state: LiveState = Field(default_factory=LiveState)
    memory_text: str = ""
    learned_text: str = ""
    live_completion_reason: str = ""
    planner_advisory: str = ""

    @field_validator("mode", mode="before")
    @classmethod
    def _coerce_mode(cls, value: Any) -> AutonomyMode:
        return autonomy_mode(value)

    @field_validator("ledger", mode="before")
    @classmethod
    def _coerce_ledger(cls, value: Any) -> LedgerState:
        return LedgerState.normalized(value)

    @field_validator("live_state", mode="before")
    @classmethod
    def _coerce_live_state(cls, value: Any) -> LiveState:
        if isinstance(value, LiveState):
            return value
        if isinstance(value, str):
            return LiveState.from_line(value)
        try:
            return LiveState.from_payload(value)
        except BridgeValidationError:
            return LiveState()

    @field_validator(
        "memory_text",
        "learned_text",
        "live_completion_reason",
        "planner_advisory",
        mode="before",
    )
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return value.strip() if isinstance(value, str) else ""

    @property
    def live_state_line(self) -> str:
        line = self.live_state.to_line()
        if not self.live_completion_reason:
            return line
        return "\n".join([
            line,
            f"Live-state completion signal: {self.live_completion_reason}",
        ]).strip()

    def render(
        self,
        *,
        planner_prompt: str,
        execution_prompt: str,
    ) -> str:
        tick_prompt = (
            planner_prompt
            if self.mode == AutonomyMode.PLAN
            else execution_prompt
        )
        parts = [
            self.memory_text,
            self.ledger.render(recent_progress_count=3),
            self.learned_text,
            self.live_state_line,
            self.planner_advisory,
            tick_prompt,
        ]
        return "\n\n".join(part for part in parts if part)


class LedgerStalenessEvidence(BridgeModel):
    """Typed evidence for discarding a stale persisted ledger."""

    kind: LedgerStalenessKind = LedgerStalenessKind.NONE
    reason: str = ""
    age_seconds: float | None = None
    max_age_s: float = 0.0
    mentions_initial_extraction: bool = False
    reports_no_infrastructure: bool = False

    @field_validator("kind", mode="before")
    @classmethod
    def _coerce_kind(cls, value: Any) -> LedgerStalenessKind:
        if isinstance(value, LedgerStalenessKind):
            return value
        if isinstance(value, str):
            normalized = value.strip().lower().replace("-", "_")
            for kind in LedgerStalenessKind:
                if normalized == kind.value:
                    return kind
        return LedgerStalenessKind.NONE

    @field_validator("reason", mode="before")
    @classmethod
    def _coerce_reason(cls, value: Any) -> str:
        return str(value or "").strip()

    @property
    def is_stale(self) -> bool:
        return self.kind != LedgerStalenessKind.NONE and bool(self.reason)

    @classmethod
    def none(
        cls,
        *,
        age_seconds: float | None = None,
        max_age_s: float = 0.0,
        mentions_initial_extraction: bool = False,
        reports_no_infrastructure: bool = False,
    ) -> "LedgerStalenessEvidence":
        return cls(
            kind=LedgerStalenessKind.NONE,
            reason="",
            age_seconds=age_seconds,
            max_age_s=max_age_s,
            mentions_initial_extraction=mentions_initial_extraction,
            reports_no_infrastructure=reports_no_infrastructure,
        )

    @classmethod
    def from_bootstrap_policy(
        cls,
        ledger: LedgerState | dict,
        *,
        max_age_s: float,
        now: datetime | None = None,
    ) -> "LedgerStalenessEvidence":
        state = LedgerState.coerce(ledger)
        active_intent = LedgerObjectiveIntent.from_text(state.objective)
        progress_signals = LedgerProgressSignals.from_text(state.progress_text())
        age = state.age_seconds(now=now)
        try:
            max_age = float(max_age_s)
        except (TypeError, ValueError):
            max_age = 0.0

        common = {
            "age_seconds": age,
            "max_age_s": max_age,
            "mentions_initial_extraction": active_intent.mentions_initial_extraction,
            "reports_no_infrastructure": progress_signals.reports_no_infrastructure,
        }
        if not active_intent.mentions_initial_extraction:
            return cls.none(**common)
        if not progress_signals.reports_no_infrastructure:
            return cls.none(**common)
        if age is None or age <= max_age:
            return cls.none(**common)
        return cls(
            kind=LedgerStalenessKind.STALE_BOOTSTRAP,
            reason="initial-extraction ledger is older than bootstrap stale threshold and still reports no infrastructure",
            **common,
        )


class HiddenTrailerBodyLine(BridgeModel):
    """Typed view of one normalized line inside a hidden trailer body."""

    text: str = ""
    key: str = ""
    value: str = ""
    bullet: str = ""

    @field_validator("text", "key", "value", "bullet", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return str(value or "").strip()

    @property
    def has_text(self) -> bool:
        return bool(self.text)

    @property
    def has_key_value(self) -> bool:
        return bool(self.key)

    @property
    def is_bullet(self) -> bool:
        return bool(self.bullet)

    def key_is(self, value: str) -> bool:
        return self.key == str(value or "").strip().lower()

    @classmethod
    def from_line(cls, value: Any) -> "HiddenTrailerBodyLine":
        text = str(value or "").strip()
        if not text:
            return cls()
        if text.startswith("- "):
            return cls(text=text, bullet=text[2:].strip())
        numbered = re.match(r"^\d+[.)]\s+(.+)$", text)
        if numbered:
            return cls(text=text, bullet=numbered.group(1).strip())
        split = KeyValueTextSplit.from_text(text)
        if split.matched:
            return cls(
                text=text,
                key=split.key,
                value=split.value,
            )
        return cls(text=text)

    @classmethod
    def iter_body(cls, body: Any) -> Iterable["HiddenTrailerBodyLine"]:
        for raw_line in BridgeTextLines.from_text(body).lines:
            line = cls.from_line(raw_line)
            if line.has_text:
                yield line


class LedgerUpdateDraft(BridgeModel):
    """Typed intermediate shape parsed from a hidden <ledger> trailer body."""

    objective: str = ""
    plan_steps: list[str] = Field(default_factory=list)
    progress: str = ""
    signal: ProgressSignal = ProgressSignal.NONE
    status: LedgerStatus = LedgerStatus.NONE
    next_required_mode: LedgerNextRequiredMode = LedgerNextRequiredMode.NONE
    blocker: str = ""

    @field_validator("objective", "progress", "blocker", mode="before")
    @classmethod
    def _coerce_text(cls, value: Any) -> str:
        return value.strip() if isinstance(value, str) else ""

    @field_validator("plan_steps", mode="before")
    @classmethod
    def _coerce_steps(cls, value: Any) -> list[str]:
        return _coerce_str_or_list(value)

    @field_validator("signal", mode="before")
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

    @classmethod
    def from_body(cls, body: Any) -> "LedgerUpdateDraft":
        if isinstance(body, cls):
            return body
        if isinstance(body, LedgerUpdate):
            return cls(
                objective=body.objective,
                plan_steps=list(body.plan_steps),
                progress=body.progress,
                signal=body.signal,
                status=body.status,
                next_required_mode=body.next_required_mode,
                blocker=body.blocker,
            )
        data: dict[str, Any] = {
            "objective": "",
            "plan_steps": [],
            "progress": "",
            "signal": ProgressSignal.NONE,
            "status": LedgerStatus.NONE,
            "next_required_mode": LedgerNextRequiredMode.NONE,
            "blocker": "",
        }
        in_plan = False
        for line in HiddenTrailerBodyLine.iter_body(body):
            if line.key_is("objective"):
                data["objective"] = line.value
                in_plan = False
            elif line.key_is("plan"):
                in_plan = True
            elif line.key_is("progress"):
                data["progress"] = line.value
                in_plan = False
            elif line.key_is("signal"):
                data["signal"] = line.value
                in_plan = False
            elif line.key_is("status"):
                data["status"] = line.value
                in_plan = False
            elif line.key_is("next_required_mode"):
                data["next_required_mode"] = line.value
                in_plan = False
            elif line.key_is("blocker"):
                data["blocker"] = line.value
                in_plan = False
            elif in_plan and line.is_bullet:
                data["plan_steps"].append(line.bullet)
        return cls.model_validate(data)

    def to_update(self) -> "LedgerUpdate":
        return LedgerUpdate.coerce({
            "objective": self.objective,
            "plan_steps": list(self.plan_steps),
            "progress": self.progress,
            "signal": self.signal,
            "status": self.status,
            "next_required_mode": self.next_required_mode,
            "blocker": self.blocker,
        })


class LedgerUpdate(BridgeModel):
    objective: str = ""
    plan_steps: list[str] = Field(default_factory=list)
    progress: str = ""
    signal: ProgressSignal = ProgressSignal.NONE
    status: LedgerStatus = LedgerStatus.NONE
    next_required_mode: LedgerNextRequiredMode = LedgerNextRequiredMode.NONE
    blocker: str = ""

    @classmethod
    def from_trailer_text(cls, text: Any) -> "LedgerUpdate | None":
        if isinstance(text, cls):
            return text
        if isinstance(text, LedgerUpdateDraft):
            return text.to_update()
        block = HiddenTrailerBlock.first_from_text(text, "ledger")
        if not block:
            return None
        return LedgerUpdateDraft.from_body(block.body).to_update()

    @classmethod
    def strip_trailer_text(cls, text: Any) -> str:
        return HiddenTrailerBlock.strip_from_text(text, ["ledger"])

    @classmethod
    def coerce(cls, value: Any) -> "LedgerUpdate":
        if isinstance(value, cls):
            return value
        if isinstance(value, LedgerUpdateDraft):
            return value.to_update()
        if not isinstance(value, dict):
            return cls()
        objective = value.get("objective", "")
        progress = value.get("progress", "")
        plan_steps = _coerce_str_list(value.get("plan_steps", []))
        signal = progress_signal(value.get("signal"))
        status = ledger_status(value.get("status"))
        next_required_mode = ledger_next_required_mode(value.get("next_required_mode"))
        blocker = value.get("blocker", "")
        if signal == ProgressSignal.NONE:
            if status == LedgerStatus.DONE:
                signal = ProgressSignal.PLAN_DONE
            elif status in {LedgerStatus.READY, LedgerStatus.EXECUTING}:
                signal = ProgressSignal.PLAN_READY
            elif next_required_mode == LedgerNextRequiredMode.EXECUTE:
                signal = ProgressSignal.PLAN_READY
            elif plan_steps:
                signal = ProgressSignal.PLAN_READY
            elif isinstance(objective, str) and objective.strip():
                signal = ProgressSignal.NEW_OBJECTIVE
        return cls(
            objective=objective.strip() if isinstance(objective, str) else "",
            plan_steps=plan_steps,
            progress=progress.strip() if isinstance(progress, str) else "",
            signal=signal,
            status=status,
            next_required_mode=next_required_mode,
            blocker=blocker.strip() if isinstance(blocker, str) else "",
        )

    def to_dict(self) -> dict[str, Any]:
        result: dict[str, Any] = {}
        if self.objective:
            result["objective"] = self.objective
        if self.plan_steps:
            result["plan_steps"] = list(self.plan_steps)
        if self.progress:
            result["progress"] = self.progress
        if self.signal != ProgressSignal.NONE:
            result["signal"] = self.signal.value
        if self.status != LedgerStatus.NONE:
            result["status"] = self.status.value
        if self.next_required_mode != LedgerNextRequiredMode.NONE:
            result["next_required_mode"] = self.next_required_mode.value
        if self.blocker:
            result["blocker"] = self.blocker
        return result


def _ledger_file(agent_name: str) -> Path:
    return state_file(f".ledger-{agent_name}.json")


def _ledger_read_files(agent_name: str) -> tuple[Path, ...]:
    primary = _ledger_file(agent_name)
    candidates = [primary]
    candidates.extend(
        path for path in read_candidates(f".ledger-{agent_name}.json")
        if path not in candidates
    )
    return tuple(candidates)


def default_ledger_model() -> LedgerState:
    return LedgerState.default()


def _stale_bootstrap_max_age_s() -> float:
    return LedgerRuntimeSettings.from_env(os.environ).stale_bootstrap_ledger_max_age_s


def _is_stale_bootstrap_ledger(ledger: LedgerState | dict) -> bool:
    return LedgerState.normalized(ledger).bootstrap_staleness_evidence(
        max_age_s=_stale_bootstrap_max_age_s(),
    ).is_stale


def load_ledger_model(agent_name: str) -> LedgerState:
    # json.JSONDecodeError and UnicodeDecodeError are both ValueError subclasses,
    # so (ValueError, OSError) covers corrupt JSON and non-UTF8/unreadable files.
    for path in _ledger_read_files(agent_name):
        try:
            ledger = LedgerState.from_file_text(path.read_text())
        except (ValueError, OSError):
            continue
        if _is_stale_bootstrap_ledger(ledger):
            return default_ledger_model()
        return ledger
    return default_ledger_model()


def save_ledger_model(agent_name: str, ledger: LedgerState | dict) -> None:
    # Atomic write: serialize first, write to a temp file, then os.replace onto
    # the target so an interrupted/failed write can never truncate the real
    # ledger. Persistence failures are surfaced (printed), not silently swallowed.
    path = _ledger_file(agent_name)
    tmp = path.with_name(path.name + ".tmp")
    try:
        payload = LedgerState.normalized(ledger).to_json_line()
    except TypeError as e:
        print(f"[ledger] WARNING: refusing to save unserializable ledger for "
              f"{agent_name}: {e}")
        return None
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp.write_text(payload)
        os.replace(tmp, path)
    except OSError as e:
        print(f"[ledger] WARNING: failed to persist ledger for {agent_name}: {e}")
        try:
            tmp.unlink(missing_ok=True)
        except OSError:
            pass
    return None


def parse_ledger_trailer_model(source: str | LedgerUpdate) -> LedgerUpdate | None:
    return LedgerUpdate.from_trailer_text(source)


def apply_ledger_update_model(agent_name: str, source: str | LedgerUpdate) -> LedgerState:
    parsed = parse_ledger_trailer_model(source)
    current = load_ledger_model(agent_name)
    if parsed is None:
        return current

    ledger = current.merged_with(
        parsed,
        updated_at=datetime.now(timezone.utc).isoformat(),
        max_progress_notes=10,
    )
    save_ledger_model(agent_name, ledger)
    return ledger


def strip_ledger_trailer(text: str) -> str:
    return LedgerUpdate.strip_trailer_text(text)


def render_ledger(ledger: LedgerState | dict) -> str:
    return LedgerState.normalized(ledger).render(recent_progress_count=3)
