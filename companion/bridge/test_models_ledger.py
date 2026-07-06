import json
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest import mock

import journal
from models import (
    AgentProfile,
    AgentNameSelection,
    AgentClaudeOptionsSpec,
    AgentInvocationConfig,
    AgentInvocationExceptionSignal,
    AgentMessageResult,
    AgentRunTranscript,
    AgentProfileSdkSkills,
    AgentResponseFormat,
    AgentRuntimeConfig,
    AnomalyEvidenceKind,
    BridgeInputBatch,
    BridgeInputFileDelta,
    BridgeInputMessageCollection,
    AgentSessionIndex,
    AgentSessionState,
    AutonomyTickMessage,
    BridgeInputMessage,
    BridgeLogMessage,
    BridgeLogPowerKind,
    BridgeLogProgressKind,
    BridgeLogRecord,
    BridgeLogRecordCollection,
    BridgeLogRuntimeKind,
    BridgeLogToolResultMarker,
    BridgeLogToolResultLine,
    BridgeProgressTimestamps,
    BridgeRuntimeEnvField,
    BridgeRuntimeSettings,
    BridgeRunReport,
    BridgeRunVerdict,
    BridgeRunVerdictKind,
    BridgeTextLines,
    BridgeValidationError,
    CommaSeparatedItems,
    ConnectedPlayerCountResult,
    DotEnvAssignmentLine,
    DotEnvFile,
    EvalProductionSnapshot,
    EvalResult,
    FactorioPathSettings,
    FactorioMcpServerConfig,
    FactorioModInfo,
    GameRejectionPayload,
    GameRejectionEvidenceKind,
    HiddenTrailerBlock,
    HiddenTrailerBodyLine,
    JournalEvent,
    JournalFailureEvidence,
    JournalFailureClassification,
    JournalFailureKind,
    JournalPromptEvent,
    JournalEventCollection,
    JournalWindow,
    KeyValueTextSplit,
    LearningProposal,
    LearningProposalCollection,
    LearningProposalDraft,
    LearningProposalDraftBodyBuilder,
    LearningRuntimeSettings,
    LedgerRuntimeSettings,
    LedgerStalenessKind,
    LedgerNextRequiredMode,
    LedgerStatus,
    LedgerUpdateDraft,
    LedgerUpdate,
    LedgerState,
    ObjectiveCompletionKind,
    LiveState,
    McpTextPayload,
    PlayerMessageSplit,
    ParsedAgentResponse,
    ParsedResponseSection,
    PowerConsumerSummary,
    PowerGeneratorSummaryCollection,
    PowerGeneratorSummary,
    PowerStatus,
    PreToolUseDecision,
    PreToolUseGuardBlock,
    PreToolUseHookResponse,
    PreToolUseGuardKind,
    ProgressSignal,
    PromptTextSanitizer,
    ProviderUsageLimit,
    ProviderUsageLimitSettings,
    RawLuaPolicy,
    ReflectionDropEvidence,
    ReflectionDropKind,
    ReflectionDraft,
    ReflectionMemory,
    ResponseFormatSection,
    ResponseFormatSectionCollection,
    RconConnectionSettings,
    RconJsonResponse,
    RconTextResponse,
    SdkAssistantMessage,
    SdkContentBlocks,
    SdkAssistantTextObservation,
    SdkErrorKind,
    SdkErrorSignal,
    SdkResultMessage,
    SdkResultObservation,
    SdkMetadataItems,
    SdkSkillConfig,
    SdkStderrKind,
    SdkStderrSignal,
    SdkSystemMessage,
    SdkToolResultObservation,
    SdkToolUse,
    SdkToolResultEvent,
    SdkUserToolResultMessage,
    SkillDefinitionDraft,
    SkillDefinition,
    SkillDefinitionCollection,
    SkillLibrary,
    SteamPowerIssue,
    SteamPowerIssueCollection,
    SteamPowerDiagnostic,
    SteamPowerSummary,
    TelemetryEvent,
    TelemetryEventBatch,
    TelemetryEventType,
    TelemetryHealthStatus,
    TelemetrySseMessage,
    TelemetrySerializableValue,
    TelemetryStatusData,
    TelemetryToolCallData,
    TelemetryRelaySettings,
    TextMarkerSplit,
    WatchdogToolObservation,
    TOOL_PARAM_INTEGER,
    TOOL_PARAM_NUMBER,
    TOOL_PARAM_STRING,
    ToolParamSchema,
    ToolParamSchemaRegistry,
    ToolResultClassification,
    ToolResultContent,
    ToolResultLogLevel,
    ToolResultLogRecord,
    ToolResultPayload,
    ToolResultPayloadCollection,
    ToolResultTextEvidence,
    ToolResultTextKind,
    ToolResultOutcome,
    ToolCallRequest,
)



class ModelLedgerTests(unittest.TestCase):

    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.base = Path(self.tempdir.name)

    def test_ledger_state_validates_and_round_trips_existing_shape(self):
        state = LedgerState.from_mapping({
            "objective": "",
            "plan_steps": ["Place boiler"],
            "progress_notes": ["Found water"],
            "updated_at": "now",
        })

        self.assertEqual(state.to_dict(), {
            "objective": "",
            "plan_steps": ["Place boiler"],
            "progress_notes": ["Found water"],
            "updated_at": "now",
        })
        self.assertIs(LedgerState.from_mapping(state), state)

        with self.assertRaisesRegex(BridgeValidationError, "plan_steps: expected list of strings"):
            LedgerState.from_mapping({
                "objective": "Power lab",
                "plan_steps": {"bad": True},
                "progress_notes": [],
                "updated_at": "now",
            })

        with self.assertRaisesRegex(BridgeValidationError, r"progress_notes\[0\]: expected string"):
            LedgerState.from_mapping({
                "objective": "Power lab",
                "plan_steps": [],
                "progress_notes": [{"bad": True}],
                "updated_at": "now",
            })

    def test_ledger_state_coerce_preserves_total_legacy_load_behavior(self):
        state = LedgerState.coerce({
            "objective": None,
            "plan_steps": ["Keep", None],
            "progress_notes": None,
            "updated_at": 123,
        })

        self.assertEqual(state.to_dict(), {
            "objective": "",
            "plan_steps": ["Keep"],
            "progress_notes": [],
            "updated_at": "",
        })
        self.assertIs(LedgerState.coerce(state), state)

        normalized = LedgerState.normalized({
            "objective": "Build power",
            "plan_steps": ["fuel boiler"],
            "progress_notes": ["drop me", "keep me"],
            "updated_at": "now",
        }, progress_should_drop=lambda note: note == "drop me")
        self.assertEqual(normalized.to_dict(), {
            "objective": "Build power",
            "plan_steps": ["fuel boiler"],
            "progress_notes": ["keep me"],
            "updated_at": "now",
        })

    def test_ledger_state_returns_stale_bootstrap_evidence(self):
        now = datetime(2026, 6, 30, 12, 0, 0)
        stale = LedgerState(
            objective="Establish initial extraction infrastructure on iron patch",
            plan_steps=["place_entity burner-mining-drill on iron ore"],
            progress_notes=["situation assessed; no infrastructure yet deployed"],
            updated_at=(now - timedelta(hours=2)).isoformat(),
        )
        recent = stale.model_copy(update={
            "updated_at": (now - timedelta(seconds=10)).isoformat(),
        })
        unrelated = stale.model_copy(update={
            "objective": "repair belt logistics",
        })

        self.assertEqual(stale.age_seconds(now=now), 7200.0)
        evidence = stale.bootstrap_staleness_evidence(
            max_age_s=60.0,
            now=now,
        )
        self.assertTrue(evidence.is_stale)
        self.assertEqual(evidence.kind, LedgerStalenessKind.STALE_BOOTSTRAP)
        self.assertEqual(evidence.age_seconds, 7200.0)
        self.assertEqual(evidence.max_age_s, 60.0)
        self.assertTrue(evidence.mentions_initial_extraction)
        self.assertTrue(evidence.reports_no_infrastructure)
        self.assertIn("no infrastructure", evidence.reason)

        recent_evidence = recent.bootstrap_staleness_evidence(
            max_age_s=60.0,
            now=now,
        )
        self.assertFalse(recent_evidence.is_stale)
        self.assertEqual(recent_evidence.kind, LedgerStalenessKind.NONE)
        self.assertTrue(recent_evidence.mentions_initial_extraction)
        self.assertTrue(recent_evidence.reports_no_infrastructure)

        unrelated_evidence = unrelated.bootstrap_staleness_evidence(
            max_age_s=60.0,
            now=now,
        )
        self.assertFalse(unrelated_evidence.is_stale)
        self.assertEqual(unrelated_evidence.kind, LedgerStalenessKind.NONE)
        self.assertFalse(unrelated_evidence.mentions_initial_extraction)
        self.assertTrue(unrelated_evidence.reports_no_infrastructure)
        self.assertIsNone(stale.model_copy(update={"updated_at": "wat"}).age_seconds(now=now))

    def test_ledger_state_parses_file_text_and_rejects_non_objects(self):
        state = LedgerState.from_file_text(json.dumps({
            "objective": "Power lab",
            "plan_steps": ["fuel boiler"],
            "progress_notes": ["drop this", "boiler fueled"],
            "updated_at": "then",
        }), progress_should_drop=lambda note: note == "drop this")

        self.assertEqual(state.to_dict(), {
            "objective": "Power lab",
            "plan_steps": ["fuel boiler"],
            "progress_notes": ["boiler fueled"],
            "updated_at": "then",
        })
        self.assertTrue(state.to_json_line().endswith("\n"))

        with self.assertRaisesRegex(BridgeValidationError, "ledger: expected object"):
            LedgerState.from_file_text("[]")
        with self.assertRaisesRegex(BridgeValidationError, "ledger: expected JSON object"):
            LedgerState.from_file_text("{not-json")

    def test_ledger_state_merges_updates_caps_progress_and_renders_prompt(self):
        state = LedgerState(
            objective="Repair power",
            plan_steps=["inspect plant"],
            progress_notes=["old 1", "old 2"],
            updated_at="old",
        )

        progress_update = LedgerUpdate.coerce({
            "progress": "plan validated and ready for execution",
        })
        unchanged = state.merged_with(
            progress_update,
            updated_at="new",
            progress_should_drop=lambda note: "plan validated" in note,
        )
        self.assertEqual(unchanged.progress_notes, ["old 1", "old 2"])
        self.assertEqual(unchanged.updated_at, "new")

        replaced = unchanged.merged_with(
            {
                "objective": "Activate second furnace",
                "plan_steps": ["walk_to (42, -21)"],
                "progress": "selected second furnace",
            },
            updated_at="later",
            max_progress_notes=2,
        )

        self.assertEqual(replaced.objective, "Activate second furnace")
        self.assertEqual(replaced.plan_steps, ["walk_to (42, -21)"])
        self.assertEqual(replaced.progress_notes, ["old 2", "selected second furnace"])
        self.assertIn("Activate second furnace", replaced.render())
        self.assertIn("1. walk_to (42, -21)", replaced.render())
        self.assertEqual(LedgerState.from_file_text(replaced), replaced)

    def test_ledger_state_uses_signal_or_repeated_evidence_for_execution_ready_plan(self):
        ready = LedgerState.coerce({
            "objective": "activate second stone-furnace unit 15",
            "plan_steps": ["walk_to (42, -21)", "insert_items coal"],
            "progress_notes": [
                "situation_report confirmed stable at tick 4107121",
                "Plan concrete and executable. Awaiting execution turn.",
                "Plan validated, queued for execution.",
            ],
            "updated_at": "now",
            "signal": "plan_ready",
        })
        prose_only = LedgerState.coerce({
            "objective": "activate second stone-furnace unit 15",
            "plan_steps": ["walk_to (42, -21)", "insert_items coal"],
            "progress_notes": [
                "Plan concrete and executable. Awaiting execution turn.",
                "Plan validated, queued for execution.",
            ],
            "updated_at": "now",
        })
        repeated_ready = LedgerState.coerce({
            "objective": "activate second stone-furnace unit 15",
            "plan_steps": ["walk_to (42, -21)", "insert_items coal"],
            "progress_notes": [
                "Inventory intact. Plan validated against live state, ready for execution.",
                "No state drift. Plan validated, queued for execution.",
                "Fourth consecutive planning cycle. Plan validated, awaiting execution.",
            ],
            "updated_at": "now",
        })
        unready = LedgerState.coerce({
            "objective": "activate second stone-furnace unit 15",
            "plan_steps": ["walk_to (42, -21)", "insert_items coal"],
            "progress_notes": ["situation_report confirmed stable state"],
            "updated_at": "now",
        })

        self.assertTrue(ready.has_execution_ready_plan())
        self.assertFalse(prose_only.has_execution_ready_plan())
        self.assertTrue(repeated_ready.has_execution_ready_plan())
        self.assertEqual(repeated_ready.readiness_evidence().ready_note_count, 3)
        self.assertFalse(unready.has_execution_ready_plan())
        self.assertFalse(LedgerState.default().has_execution_ready_plan())

    def test_ledger_state_keeps_plan_ready_signal_with_progress_note(self):
        update = LedgerUpdate.coerce({
            "objective": "activate second stone-furnace unit 15",
            "plan_steps": ["walk_to (42, -21)", "insert_items coal"],
            "progress": "Plan fully validated and awaiting execution turns.",
            "signal": "plan_ready",
        })

        state = LedgerState.default().merged_with(
            update,
            updated_at="now",
        )

        self.assertEqual(
            state.progress_notes,
            ["Plan fully validated and awaiting execution turns."],
        )
        self.assertEqual(state.signal, ProgressSignal.PLAN_READY)
        self.assertEqual(state.to_dict()["signal"], "plan_ready")
        self.assertTrue(state.has_execution_ready_plan())

        done = state.merged_with(
            {"progress": "furnace fed", "signal": "plan_done"},
            updated_at="later",
        )
        self.assertEqual(done.signal, ProgressSignal.PLAN_DONE)
        self.assertFalse(done.has_execution_ready_plan())

    def test_ledger_state_detects_live_state_completion_evidence(self):
        state = LedgerState.coerce({
            "objective": (
                "complete steam power deployment, power the lab, start "
                "automation research"
            ),
            "plan_steps": [
                "place steam engine adjacent to boiler",
                "place lab near power endpoint",
                "craft automation science packs",
            ],
            "progress_notes": [],
            "updated_at": "now",
        })
        live = LiveState.from_line(
            "Live state: nauvis @ 46.7,-15.6; player entities: "
            "offshore-pump=1, boiler=1, steam-engine=1, lab=1, "
            "small-electric-pole=15",
        )
        evidence = state.live_state_completion_evidence(live)

        self.assertTrue(evidence.is_completion)
        self.assertEqual(evidence.kind, ObjectiveCompletionKind.POWERED_LAB)
        self.assertEqual(evidence.entity_counts["boiler"], 1)
        self.assertEqual(
            evidence.reason,
            "live state already has steam power and a powered-lab footprint",
        )

    def test_ledger_state_ignores_live_state_for_unrelated_objective(self):
        state = LedgerState.coerce({
            "objective": "build automated iron smelting array",
            "plan_steps": ["route belt", "place inserter"],
            "progress_notes": [],
            "updated_at": "now",
        })
        live = LiveState.from_line(
            "Live state: nauvis @ 46.7,-15.6; player entities: "
            "offshore-pump=1, boiler=1, steam-engine=1, lab=1",
        )
        evidence = state.live_state_completion_evidence(live)

        self.assertFalse(evidence.is_completion)
        self.assertEqual(evidence.kind, ObjectiveCompletionKind.NONE)
        self.assertEqual(evidence.reason, "")

    def test_ledger_update_uses_typed_signal_and_infers_new_objective(self):
        self.assertEqual(
            LedgerUpdate.coerce({
                "objective": "activate second furnace",
                "progress": "previous objective complete; new objective selected",
            }).to_dict(),
            {
                "objective": "activate second furnace",
                "progress": "previous objective complete; new objective selected",
                "signal": "new_objective",
            },
        )
        self.assertEqual(
            LedgerUpdate.coerce({"progress": "done", "signal": "plan-done"}).signal,
            ProgressSignal.PLAN_DONE,
        )
        done = LedgerUpdate.coerce({
            "progress": "objective complete",
            "status": "done",
            "next_required_mode": "plan",
            "blocker": "none",
        })
        self.assertEqual(done.status, LedgerStatus.DONE)
        self.assertEqual(done.signal, ProgressSignal.PLAN_DONE)
        self.assertEqual(done.next_required_mode, LedgerNextRequiredMode.PLAN)
        self.assertEqual(done.blocker, "none")

    def test_ledger_update_infers_plan_ready_from_structured_plan(self):
        update = LedgerUpdate.coerce({
            "objective": "activate second furnace",
            "plan_steps": ["walk_to (42, -21)", "insert_items coal"],
            "progress": "plan confirmed",
        })

        self.assertEqual(update.signal, ProgressSignal.PLAN_READY)
        self.assertEqual(update.to_dict()["signal"], "plan_ready")
