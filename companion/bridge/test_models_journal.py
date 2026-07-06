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



class ModelJournalTests(unittest.TestCase):

    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.base = Path(self.tempdir.name)

    def test_journal_event_model_preserves_jsonl_shape(self):
        event = JournalEvent.create(ts="now", kind="unknown", text=["coerced"])

        self.assertEqual(event.to_dict(), {
            "ts": "now",
            "kind": "progress",
            "text": "['coerced']",
        })

        with mock.patch(
            "journal._journal_file",
            side_effect=lambda agent_name: self.base / f".journal-{agent_name}.jsonl",
        ):
            journal.append_event("doug", "failure", "classified failure")
            loaded = journal.load_events_model("doug")

        self.assertEqual(len(loaded.events), 1)
        self.assertEqual(loaded.events[0].kind, "failure")
        self.assertEqual(loaded.events[0].text, "classified failure")

    def test_journal_event_model_round_trips_structured_signal(self):
        event = JournalEvent.create(
            ts="now",
            kind="progress",
            text="new plan ready",
            signal="new-objective",
        )

        data = event.to_dict()

        self.assertEqual(data["signal"], "new_objective")
        self.assertEqual(JournalEvent.from_mapping(data).signal, ProgressSignal.NEW_OBJECTIVE)

    def test_journal_window_reads_newest_actionable_signal(self):
        events = (
            {"ts": "1", "kind": "progress", "text": "old plan done", "signal": "plan_done"},
            {"ts": "2", "kind": "failure", "text": "game rejected"},
            {"ts": "3", "kind": "progress", "text": "new plan ready", "signal": "plan_ready"},
        )
        window = JournalWindow.coerce(events)

        self.assertEqual(len(JournalEventCollection.from_value(events).items), 3)
        self.assertEqual(window.newest_autonomy_signal(), ProgressSignal.PLAN_READY)
        self.assertTrue(window.has_actionable_plan_signal())
        self.assertFalse(window.newest_event_indicates_plan_done())

    def test_journal_window_detects_plan_done_from_signal_only(self):
        by_signal = JournalWindow.coerce([
            {"ts": "1", "kind": "progress", "text": "all sorted", "signal": "plan_done"},
        ])
        prose_only = JournalWindow.coerce([
            {"ts": "1", "kind": "failure", "text": "old tool miss"},
            {"ts": "2", "kind": "discovery", "text": "objective complete; choose next bottleneck"},
        ])
        actionable_new_plan = JournalWindow.coerce([
            {"ts": "1", "kind": "progress", "text": "objective complete"},
            {"ts": "2", "kind": "progress", "text": "replacement plan ready", "signal": "new_objective"},
        ])

        self.assertTrue(by_signal.newest_event_indicates_plan_done())
        self.assertFalse(prose_only.newest_event_indicates_plan_done())
        self.assertFalse(actionable_new_plan.newest_event_indicates_plan_done())

    def test_prose_progress_is_preserved_without_low_value_filtering(self):
        prose_only = JournalEvent.create(
            ts="now",
            kind="progress",
            text="Plan fully validated and awaiting execution turns.",
        )
        bootstrap = JournalEvent.create(
            ts="now",
            kind="progress",
            text="situation assessed; no infrastructure yet deployed",
        )

        self.assertFalse(prose_only.should_drop())
        self.assertFalse(bootstrap.should_drop())

    def test_hidden_trailer_body_line_parses_keys_bullets_and_blank_lines(self):
        key_value = HiddenTrailerBodyLine.from_line(" progress: belts fixed ")
        colon_value = HiddenTrailerBodyLine.from_line(
            "progress: blocked by Error: no path"
        )
        bullet = HiddenTrailerBodyLine.from_line(" - place furnace ")
        blank = HiddenTrailerBodyLine.from_line("   ")

        self.assertTrue(key_value.has_text)
        self.assertTrue(key_value.has_key_value)
        self.assertTrue(key_value.key_is("progress"))
        self.assertEqual(key_value.value, "belts fixed")
        self.assertEqual(colon_value.key, "progress")
        self.assertEqual(colon_value.value, "blocked by Error: no path")
        self.assertTrue(bullet.is_bullet)
        self.assertEqual(bullet.bullet, "place furnace")
        self.assertFalse(blank.has_text)
        self.assertEqual(
            [line.text for line in HiddenTrailerBodyLine.iter_body("a: b\n\n- c")],
            ["a: b", "- c"],
        )

    def test_ledger_update_draft_parses_hidden_trailer_body(self):
        text = """Before.
<ledger>
objective: Activate second furnace
plan:
- walk_to (42, -21)
- insert_items coal count=5 into fuel inventory of unit 15
progress: plan confirmed; awaiting execution
</ledger>
After.
"""
        draft = LedgerUpdateDraft.from_body(
            """objective: Activate second furnace
plan:
- walk_to (42, -21)
- insert_items coal count=5 into fuel inventory of unit 15
progress: plan confirmed; awaiting execution
"""
        )
        update = draft.to_update()
        trailer_update = LedgerUpdate.from_trailer_text(text)

        self.assertEqual(update.objective, "Activate second furnace")
        self.assertEqual(
            update.plan_steps,
            [
                "walk_to (42, -21)",
                "insert_items coal count=5 into fuel inventory of unit 15",
            ],
        )
        self.assertEqual(update.progress, "plan confirmed; awaiting execution")
        self.assertEqual(update.signal, ProgressSignal.PLAN_READY)
        self.assertIs(LedgerUpdateDraft.from_body(draft), draft)
        self.assertEqual(LedgerUpdateDraft.from_body(update).to_update(), update)
        self.assertIs(LedgerUpdate.coerce(update), update)
        self.assertEqual(LedgerUpdate.coerce(draft), update)
        self.assertIs(LedgerUpdate.from_trailer_text(update), update)
        self.assertEqual(LedgerUpdate.from_trailer_text(draft), update)
        self.assertIsNotNone(trailer_update)
        self.assertEqual(trailer_update.objective, update.objective)
        self.assertEqual(trailer_update.plan_steps, update.plan_steps)
        self.assertEqual(LedgerUpdate.strip_trailer_text(text), "Before.\n\nAfter.")

    def test_reflection_draft_parses_sparse_hidden_trailer_body(self):
        text = """Before.
<reflection>
error_tips:
- Check boiler fuel before rebuilding power
- Verify inserter direction after placement
</reflection>
After.
"""
        draft = ReflectionDraft.from_body(
            """error_tips:
- Check boiler fuel before rebuilding power
- Verify inserter direction after placement
"""
        )
        trailer_draft = ReflectionDraft.from_trailer_text(text)

        self.assertEqual(draft.structures, [])
        self.assertEqual(
            draft.error_tips,
            [
                "Check boiler fuel before rebuilding power",
                "Verify inserter direction after placement",
            ],
        )
        self.assertEqual(
            draft.to_sparse_dict(),
            {
                "error_tips": [
                    "Check boiler fuel before rebuilding power",
                    "Verify inserter direction after placement",
                ],
            },
        )
        self.assertIsNotNone(trailer_draft)
        self.assertEqual(trailer_draft.to_sparse_dict(), draft.to_sparse_dict())
        self.assertIs(ReflectionDraft.from_body(draft), draft)
        self.assertEqual(ReflectionDraft.from_trailer_text(draft), draft)
        self.assertEqual(ReflectionDraft.strip_trailer_text(text), "Before.\n\nAfter.")

    def test_reflection_memory_coerces_cleans_and_merges_sparse_updates(self):
        memory = ReflectionMemory.coerce(
            {
                "structures": [" Iron base ", "drop me", "Iron base", "Copper base"],
                "error_tips": "Check belt direction",
                "updated_at": 42,
                "ignored_future_field": "safe",
            },
            max_items=2,
            item_normalizer=lambda value: " ".join(value.split()),
            item_should_drop=lambda value: value == "drop me",
        )

        self.assertEqual(memory.structures, ["Iron base", "Copper base"])
        self.assertEqual(memory.error_tips, ["Check belt direction"])
        self.assertEqual(memory.updated_at, "")
        default_cleaned = ReflectionMemory.coerce(
            {
                "structures": [
                    "  Iron bus  ",
                    "fresh deployment assessment",
                    "Iron bus",
                    "x " * 200,
                ],
                "error_tips": [
                    "API Error: The model has reached its context-window limit.",
                    "verify belts first",
                ],
            },
            max_items=3,
            max_len=24,
        )
        self.assertEqual(default_cleaned.structures[0], "Iron bus")
        self.assertNotIn("fresh deployment assessment", default_cleaned.structures)
        self.assertTrue(default_cleaned.structures[1].endswith("..."))
        self.assertEqual(default_cleaned.error_tips, ["verify belts first"])
        transient_drop = ReflectionDropEvidence.from_text(
            "API Error: The model has reached its context-window limit."
        )
        startup_drop = ReflectionDropEvidence.from_text("fresh deployment assessment")
        self.assertTrue(transient_drop.should_drop)
        self.assertEqual(transient_drop.kind, ReflectionDropKind.TRANSIENT_FAILURE)
        self.assertTrue(ReflectionMemory.should_drop_item(
            "API Error: The model has reached its context-window limit."
        ))
        self.assertTrue(ReflectionMemory.should_drop_item("fresh deployment assessment"))
        self.assertEqual(startup_drop.kind, ReflectionDropKind.LOW_VALUE_STARTUP)
        self.assertFalse(
            ReflectionDropEvidence.from_text("verify belts first").should_drop,
        )
        self.assertTrue(ReflectionMemory.compact_item("x " * 200).endswith("..."))

        merged = memory.merged_with(
            {"error_tips": ["New tip", "New tip"]},
            updated_at="now",
            max_items=2,
        )

        self.assertEqual(merged.structures, ["Iron base", "Copper base"])
        self.assertEqual(merged.error_tips, ["New tip"])
        self.assertEqual(merged.updated_at, "now")
        self.assertEqual(
            merged.to_dict(),
            {
                "structures": ["Iron base", "Copper base"],
                "error_tips": ["New tip"],
                "updated_at": "now",
            },
        )

    def test_reflection_memory_reads_file_text_and_rejects_non_objects(self):
        memory = ReflectionMemory.from_file_text(json.dumps({
            "structures": ["Lab at spawn"],
            "error_tips": ["Fuel boiler first"],
            "updated_at": "then",
        }))

        self.assertEqual(memory.structures, ["Lab at spawn"])
        self.assertEqual(memory.to_json_line(), json.dumps(memory.to_dict()) + "\n")
        self.assertEqual(ReflectionMemory.from_file_text(memory), memory)

        with self.assertRaisesRegex(BridgeValidationError, "reflection: expected object"):
            ReflectionMemory.from_file_text("[]")

    def test_journal_event_parses_jsonl_line_at_model_boundary(self):
        event = JournalEvent.from_json_line(json.dumps({
            "ts": "2026-06-30T00:00:00",
            "kind": "discovery",
            "text": "belt was backwards",
            "signal": "plan_ready",
        }))

        self.assertIsNotNone(event)
        self.assertEqual(event.kind, "discovery")
        self.assertEqual(event.signal, ProgressSignal.PLAN_READY)
        self.assertEqual(event.to_dict()["signal"], "plan_ready")
        self.assertEqual(
            event.to_json_line(),
            (
                '{"ts":"2026-06-30T00:00:00","kind":"discovery",'
                '"text":"belt was backwards","signal":"plan_ready"}\n'
            ),
        )
        self.assertIsNone(JournalEvent.from_json_line("{not-json"))
        self.assertIsNone(JournalEvent.from_json_line('["not", "a", "mapping"]'))
        self.assertIsNone(JournalEvent.from_json_line(None))
        self.assertIs(JournalEvent.from_json_line(event), event)
        self.assertIs(JournalEvent.from_mapping(event), event)

    def test_journal_event_owns_prompt_drop_and_compaction_policy(self):
        transient = JournalEvent.create(
            ts="now",
            kind="failure",
            text="API Error: Request rejected (429) - Usage limit reached",
        )
        placement_rejection = JournalEvent.create(
            ts="now",
            kind="failure",
            text='game_rejected: {"error": "Cannot place entity here"}',
        )
        prose_only = JournalEvent.create(
            ts="now",
            kind="progress",
            text="Plan fully validated and awaiting execution.",
        )
        actionable = JournalEvent.create(
            ts="now",
            kind="progress",
            text="Plan fully validated and awaiting execution.",
            signal="plan_ready",
        )
        noisy_text = "line one\n\n" + ("again " * 80)

        self.assertEqual(
            JournalFailureClassification.from_text(transient.text).kind,
            JournalFailureKind.PROVIDER_LIMIT,
        )
        self.assertEqual(
            JournalFailureClassification.from_text(
                '[{"type":"text","text":"Error: expected value at line 1 column 1"}]',
            ).kind,
            JournalFailureKind.INFRASTRUCTURE_FAILURE,
        )
        expected_miss = JournalFailureEvidence.from_text(
            '[{"type":"text","text":"Error: No items of that type in inventory"}]',
        )
        invalid_request = JournalFailureEvidence.from_text(
            "Error: invalid JSON in tool response",
        )
        invalid_envelope = JournalFailureEvidence.from_text(
            "Error: missing field `success` at line 1 column 135",
        )
        invalid_event = JournalEvent.create(
            ts="now",
            kind="failure",
            text="invalid_request: Error: invalid JSON",
        )
        gameplay = JournalFailureEvidence.from_text(
            'game_rejected: {"error": "Cannot place entity here"}',
        )

        self.assertEqual(expected_miss.kind, JournalFailureKind.EXPECTED_MISS)
        self.assertTrue(expected_miss.drop_from_journal)
        self.assertEqual(
            expected_miss.tool_classification,
            ToolResultClassification.EXPECTED_MISS,
        )
        self.assertEqual(expected_miss.tool_text_kind, ToolResultTextKind.EXPECTED_MISS)
        self.assertEqual(invalid_request.kind, JournalFailureKind.INVALID_REQUEST)
        self.assertFalse(invalid_request.drop_from_journal)
        self.assertTrue(invalid_request.drop_from_memory)
        self.assertEqual(invalid_request.tool_text_kind, ToolResultTextKind.INVALID_REQUEST)
        self.assertEqual(invalid_envelope.kind, JournalFailureKind.INVALID_REQUEST)
        self.assertTrue(invalid_envelope.drop_from_journal)
        self.assertTrue(invalid_envelope.journal_noise)
        self.assertFalse(invalid_event.should_drop())
        self.assertEqual(gameplay.kind, JournalFailureKind.NONE)
        self.assertFalse(gameplay.is_transient)
        self.assertTrue(transient.should_drop())
        self.assertFalse(placement_rejection.should_drop())
        self.assertFalse(prose_only.should_drop())
        self.assertFalse(actionable.should_drop())
        self.assertEqual(JournalEvent.event_kind("weird"), "progress")
        self.assertTrue(JournalEvent.compact_text(noisy_text, limit=40).endswith("..."))
        self.assertEqual(
            actionable.prompt_dict(text_limit=20)["signal"],
            ProgressSignal.PLAN_READY.value,
        )

    def test_journal_prompt_event_compacts_merges_and_renders_window(self):
        prompt = JournalPromptEvent(
            kind="odd",
            text="belt blocked",
            signal="plan-ready",
            count="2",
            ts="now",
        )
        same = JournalPromptEvent(
            kind="progress",
            text="belt blocked",
            signal="plan_ready",
            count=3,
            ts="later",
        )
        window = JournalWindow.coerce([
            {"kind": "progress", "text": "Lab powered"},
            {"kind": "failure", "text": "game_rejected: belt blocked 0"},
            {"kind": "failure", "text": "game_rejected: belt blocked 1"},
            {"kind": "failure", "text": "game_rejected: belt blocked 2"},
            {"kind": "failure", "text": "game_rejected: belt blocked 3"},
        ])

        merged = prompt.merged_with(same)
        rendered = window.prompt_events(max_items=3, useful_kinds={"progress"})

        self.assertEqual(prompt.kind, "progress")
        self.assertEqual(prompt.signal, ProgressSignal.PLAN_READY)
        self.assertEqual(
            prompt.render_line(),
            "- progress (x2) [signal=plan_ready]: belt blocked",
        )
        self.assertEqual(merged.count, 5)
        self.assertEqual(merged.ts, "later")
        self.assertEqual(rendered[0].render_line(), "- progress: Lab powered")
        self.assertEqual(rendered[-1].text, "game_rejected: belt blocked 3")
        self.assertEqual(len(rendered), 3)

    def test_prompt_text_sanitizer_redacts_player_message_prefixes(self):
        sanitized = PromptTextSanitizer.sanitize(
            "player_messages: [TestPlayer]: hi\n"
            "Keep [item=iron-ore] and [entity=lab] rich text."
        )

        self.assertEqual(
            sanitized,
            "player_messages: [player]: hi\n"
            "Keep [item=iron-ore] and [entity=lab] rich text.",
        )

    def test_journal_prompt_event_sanitizes_player_names_for_memory(self):
        event = JournalEvent.create(
            ts="now",
            kind="progress",
            text=(
                "player_messages: [TestPlayer]: hi Doug. "
                "Plan ready near [item=iron-ore]."
            ),
        )

        prompt_event = event.prompt_event()

        self.assertNotIn("TestPlayer", prompt_event.text)
        self.assertIn("[player]: hi Doug", prompt_event.text)
        self.assertIn("[item=iron-ore]", prompt_event.text)
