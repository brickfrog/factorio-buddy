from __future__ import annotations

import asyncio
import inspect
import os
import time
from typing import Any

from ledger import load_ledger_model
from models import (
    BridgeLogMessage,
    BridgeRuntimeSettings,
    BridgeValidationError,
    PreToolUseGuardBlock,
    PreToolUseHookResponse,
    ToolCallRequest,
    ToolParamSchemaRegistry,
    ToolResultClassification,
    WatchdogToolObservation,
)
from tool_schema_registry import FACTORIO_TOOL_PARAM_SCHEMA_REGISTRY


def _runtime_settings(env: Any = None) -> BridgeRuntimeSettings:
    return BridgeRuntimeSettings.from_env(os.environ if env is None else env)


def _tool_request_from_hook_input(hook_input: Any) -> ToolCallRequest | None:
    try:
        return ToolCallRequest.from_hook_input(hook_input)
    except BridgeValidationError:
        return None


class MutatingToolBatchGate:
    """Block same-message mutating MCP batches before they race inventory state."""

    def __init__(self, log, window_s: float | None = None):
        self.log = log
        runtime = _runtime_settings()
        self.window_s = BridgeRuntimeSettings(
            mutating_tool_batch_window_s=(
                window_s
                if window_s is not None
                else runtime.mutating_tool_batch_window_s
            )
        ).mutating_tool_batch_window_s
        self._lock = asyncio.Lock()
        self._last_at = 0.0
        self._last_tool_use_id: str | None = None
        self._last_tool_name: str | None = None

    async def hook(
        self,
        hook_input: Any,
        tool_use_id: str | None,
        context: Any,
    ) -> dict[str, Any]:
        request = _tool_request_from_hook_input(hook_input)
        if request is None or not request.is_mutating_factorio_tool:
            return PreToolUseHookResponse.noop().to_dict()

        now = time.monotonic()
        short_name = request.short_name
        async with self._lock:
            if (
                self._last_tool_use_id
                and tool_use_id != self._last_tool_use_id
                and now - self._last_at < self.window_s
            ):
                previous = ToolCallRequest.short_factorio_tool_name(
                    self._last_tool_name or "",
                )
                block = PreToolUseGuardBlock.parallel_mutation(
                    tool_name=short_name,
                    previous_tool_name=previous,
                    elapsed_s=now - self._last_at,
                )
                self.log.debug(block.debug_message)
                return PreToolUseHookResponse.block(block).to_dict()

            self._last_at = now
            self._last_tool_use_id = tool_use_id
            self._last_tool_name = request.tool_name
        return PreToolUseHookResponse.allow().to_dict()


class PlannerReadOnlyToolGate:
    """Block Factorio MCP mutations while the bridge is running a planning turn."""

    def __init__(self, log, enabled: bool = False):
        self.log = log
        self.enabled = enabled

    async def hook(
        self,
        hook_input: Any,
        tool_use_id: str | None,
        context: Any,
    ) -> dict[str, Any]:
        if not self.enabled:
            return PreToolUseHookResponse.noop().to_dict()

        request = _tool_request_from_hook_input(hook_input)
        if request is None or not request.is_factorio_mcp_tool:
            return PreToolUseHookResponse.noop().to_dict()
        if request.is_read_only_factorio_tool:
            return PreToolUseHookResponse.noop().to_dict()
        if request.is_read_only_dry_run:
            return PreToolUseHookResponse.noop().to_dict()

        block = PreToolUseGuardBlock.read_only_turn(tool_name=request.short_name)
        self.log.debug(block.debug_message)
        return PreToolUseHookResponse.block(block).to_dict()


class ManualAutomationDriftGate:
    """Block manual transfer tools when the committed plan is stale automation."""

    MANUAL_TRANSFER_TOOLS = frozenset({
        "craft",
        "extract_items",
        "feed_lab_from_inventory",
        "hand_feed_furnace",
        "insert_items",
    })

    def __init__(
        self,
        log,
        agent_name: str,
        ledger_loader: Any = load_ledger_model,
        live_state_loader: Any | None = None,
        block_all_manual_transfers: bool = False,
    ):
        self.log = log
        self.agent_name = str(agent_name or "")
        self.ledger_loader = ledger_loader
        self.live_state_loader = live_state_loader
        self.block_all_manual_transfers = bool(block_all_manual_transfers)

    async def hook(
        self,
        hook_input: Any,
        tool_use_id: str | None,
        context: Any,
    ) -> dict[str, Any]:
        request = _tool_request_from_hook_input(hook_input)
        if request is None or not request.is_factorio_mcp_tool:
            return PreToolUseHookResponse.noop().to_dict()
        if request.short_name not in self.MANUAL_TRANSFER_TOOLS:
            return PreToolUseHookResponse.noop().to_dict()

        try:
            ledger = self.ledger_loader(self.agent_name)
        except Exception as exc:
            self.log.debug("manual automation guard ledger lookup failed: {}", exc)
            return PreToolUseHookResponse.noop().to_dict()
        live_state = None
        if self.live_state_loader is not None:
            try:
                live_state = self.live_state_loader(self.agent_name)
                if inspect.isawaitable(live_state):
                    live_state = await live_state
            except Exception as exc:
                self.log.debug("manual automation guard live-state lookup failed: {}", exc)
        durable_recovery_context = self._ledger_has_durable_recovery_context(ledger)
        has_assembler_or_lab = live_state is not None and live_state.has_any((
            "assembling-machine-1",
            "assembling-machine-2",
            "assembling-machine-3",
            "lab",
        ))
        has_automation_footprint = (
            live_state is not None and live_state.has_automation_capable_footprint()
        )
        if durable_recovery_context and (
            request.is_manual_fuel_transfer or request.is_manual_material_transfer
        ):
            return PreToolUseHookResponse.noop().to_dict()
        automation_setup_context = self._ledger_has_automation_enabling_setup_context(ledger)
        if (
            request.is_bootstrap_infrastructure_craft
            and (
                automation_setup_context
                or not has_automation_footprint
            )
        ):
            return PreToolUseHookResponse.noop().to_dict()
        if self.block_all_manual_transfers:
            block = PreToolUseGuardBlock.manual_automation(tool_name=request.short_name)
            self.log.debug(block.debug_message)
            return PreToolUseHookResponse.block(block).to_dict()
        if (
            automation_setup_context
            and request.is_manual_material_transfer
            and not has_automation_footprint
        ):
            return PreToolUseHookResponse.noop().to_dict()
        if (
            request.is_manual_fuel_transfer
            and has_automation_footprint
        ):
            block = PreToolUseGuardBlock.manual_automation(tool_name=request.short_name)
            self.log.debug(block.debug_message)
            return PreToolUseHookResponse.block(block).to_dict()
        if (
            request.is_manual_science_transfer
            and (has_assembler_or_lab or self._ledger_is_science_automation_context(ledger))
        ):
            block = PreToolUseGuardBlock.manual_automation(tool_name=request.short_name)
            self.log.debug(block.debug_message)
            return PreToolUseHookResponse.block(block).to_dict()
        if (
            request.is_manual_material_transfer
            and has_automation_footprint
        ):
            block = PreToolUseGuardBlock.manual_automation(tool_name=request.short_name)
            self.log.debug(block.debug_message)
            return PreToolUseHookResponse.block(block).to_dict()
        if (
            request.is_manual_component_craft
            and live_state is not None
            and live_state.has_any((
                "assembling-machine-1",
                "assembling-machine-2",
                "assembling-machine-3",
            ))
            and self._ledger_is_science_automation_context(ledger)
        ):
            block = PreToolUseGuardBlock.manual_automation(tool_name=request.short_name)
            self.log.debug(block.debug_message)
            return PreToolUseHookResponse.block(block).to_dict()
        if not ledger.has_stale_manual_automation_plan(live_state):
            return PreToolUseHookResponse.noop().to_dict()

        block = PreToolUseGuardBlock.manual_automation(tool_name=request.short_name)
        self.log.debug(block.debug_message)
        return PreToolUseHookResponse.block(block).to_dict()

    @staticmethod
    def _ledger_is_science_automation_context(ledger: Any) -> bool:
        try:
            text = str(ledger.active_text() or "")
        except Exception:
            text = ""
        if not text:
            return False
        return any(
            marker in text
            for marker in (
                "automation-science",
                "science pack",
                "red science",
                "plan_automation_science",
                "build_automation_science",
                "build_lab_feed",
            )
        )

    @staticmethod
    def _ledger_has_durable_recovery_context(ledger: Any) -> bool:
        checker = getattr(ledger, "has_durable_recovery_context", None)
        if callable(checker):
            try:
                return bool(checker())
            except Exception:
                return False
        return False

    @staticmethod
    def _ledger_has_automation_enabling_setup_context(ledger: Any) -> bool:
        checker = getattr(ledger, "has_automation_enabling_setup_context", None)
        if callable(checker):
            try:
                return bool(checker())
            except Exception:
                return False
        return False


class FactorioToolSchemaGate:
    """Reject clearly malformed Factorio MCP parameters before Rust deserialization."""

    def __init__(self, log, schema_registry: Any = None):
        self.log = log
        try:
            self.schema_registry = ToolParamSchemaRegistry.from_mapping(
                FACTORIO_TOOL_PARAM_SCHEMA_REGISTRY
                if schema_registry is None
                else schema_registry
            )
            self.schema_registry_error: BridgeValidationError | None = None
        except BridgeValidationError as exc:
            self.schema_registry = ToolParamSchemaRegistry()
            self.schema_registry_error = exc

    async def hook(
        self,
        hook_input: Any,
        tool_use_id: str | None,
        context: Any,
    ) -> dict[str, Any]:
        try:
            request = ToolCallRequest.from_hook_input(hook_input)
        except BridgeValidationError as exc:
            block = PreToolUseGuardBlock.param_schema(detail=str(exc))
            self.log.debug(block.debug_message)
            return PreToolUseHookResponse.block(block).to_dict()

        if not request.is_factorio_mcp_tool:
            return PreToolUseHookResponse.noop().to_dict()

        if self.schema_registry_error:
            block = PreToolUseGuardBlock.param_schema(
                detail=str(self.schema_registry_error),
                tool_name=request.short_name,
            )
            self.log.debug(block.debug_message)
            return PreToolUseHookResponse.block(block).to_dict()

        try:
            self.schema_registry.validate_request(request)
        except BridgeValidationError as exc:
            block = PreToolUseGuardBlock.param_schema(
                detail=str(exc),
                tool_name=request.short_name,
            )
            self.log.debug(block.debug_message)
            return PreToolUseHookResponse.block(block).to_dict()

        return PreToolUseHookResponse.noop().to_dict()


class FactorioSkillGate:
    """Require the SDK control skill before exposing Factorio MCP actions."""

    def __init__(self, log, required: bool = True):
        self.log = log
        self.required = required
        self._lock = asyncio.Lock()
        self._saw_skill = False

    async def hook(
        self,
        hook_input: Any,
        tool_use_id: str | None,
        context: Any,
    ) -> dict[str, Any]:
        request = _tool_request_from_hook_input(hook_input)
        if not self.required or request is None:
            return PreToolUseHookResponse.noop().to_dict()

        async with self._lock:
            if request.tool_name == "Skill":
                self._saw_skill = True
                return PreToolUseHookResponse.allow().to_dict()

            if request.is_factorio_mcp_tool and not self._saw_skill:
                block = PreToolUseGuardBlock.skill_required(
                    tool_name=request.short_name,
                )
                self.log.debug(block.debug_message)
                return PreToolUseHookResponse.block(block).to_dict()

        return PreToolUseHookResponse.noop().to_dict()


class AgentTickWatchdogAbort(TimeoutError):
    pass


class AgentTickWatchdog:
    """Abort a single SDK tick that is looping without useful game progress."""

    def __init__(
        self,
        *,
        same_failure_limit: int | None = None,
        no_progress_timeout_s: float | None = None,
        clock=time.monotonic,
    ):
        runtime = _runtime_settings()
        resolved = BridgeRuntimeSettings(
            watchdog_same_failure_limit=(
                same_failure_limit
                if same_failure_limit is not None
                else runtime.watchdog_same_failure_limit
            ),
            watchdog_no_progress_timeout_s=(
                no_progress_timeout_s
                if no_progress_timeout_s is not None
                else runtime.watchdog_no_progress_timeout_s
            ),
        )
        self.same_failure_limit = resolved.watchdog_same_failure_limit
        self.no_progress_timeout_s = resolved.watchdog_no_progress_timeout_s
        self.clock = clock
        self.started_at = self.clock()
        self.last_progress_at = self.started_at
        self._tool_names: dict[str, str] = {}
        self._last_failure_signature: str | None = None
        self._same_failure_count = 0

    def record_tool_use(self, tool_use_id: str | None, tool_name: str) -> None:
        if tool_use_id:
            self._tool_names[str(tool_use_id)] = str(tool_name)
        self.check_no_progress()

    def observe_text(self) -> None:
        self.check_no_progress()

    def observe_tool_result(
        self,
        tool_use_id: str | None,
        classification: ToolResultClassification | str,
        text: str,
        *,
        indicates_progress: bool | None = None,
    ) -> None:
        tool_name = self._tool_names.get(str(tool_use_id), "") if tool_use_id else ""
        observation = WatchdogToolObservation.from_result(
            tool_use_id=tool_use_id,
            tool_name=tool_name,
            classification=classification,
            text=text,
            indicates_progress=indicates_progress,
        )
        if observation.is_ok:
            if (
                observation.is_mutating_tool
                and observation.indicates_mutating_progress()
            ):
                self.mark_progress()
            self._last_failure_signature = None
            self._same_failure_count = 0
            self.check_no_progress()
            return

        if observation.is_expected_miss:
            self.check_no_progress()
            return

        if observation.is_game_rejected:
            signature = observation.failure_signature()
            if signature == self._last_failure_signature:
                self._same_failure_count += 1
            else:
                self._last_failure_signature = signature
                self._same_failure_count = 1
            if (
                self.same_failure_limit > 0
                and self._same_failure_count >= self.same_failure_limit
            ):
                short_tool = observation.short_tool_name or "tool"
                raise AgentTickWatchdogAbort(
                    "repeated same game rejection "
                    f"({self._same_failure_count}x) from {short_tool}: "
                    f"{BridgeLogMessage.single_line(observation.text, limit=300)}"
                )
            self.check_no_progress()
            return

        self.check_no_progress()

    def mark_progress(self) -> None:
        self.last_progress_at = self.clock()

    def check_no_progress(self) -> None:
        if self.no_progress_timeout_s <= 0:
            return
        elapsed = self.clock() - self.last_progress_at
        if elapsed >= self.no_progress_timeout_s:
            raise AgentTickWatchdogAbort(
                "no successful mutating progress for "
                f"{elapsed:.0f}s during active tick"
            )
