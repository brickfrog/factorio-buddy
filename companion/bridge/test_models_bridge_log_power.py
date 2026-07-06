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



class ModelBridgeLogPowerTests(unittest.TestCase):

    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.base = Path(self.tempdir.name)

    def test_bridge_log_record_coerces_loguru_shape(self):
        record = BridgeLogRecord.from_loguru_entry({
            "record": {
                "message": ["coerced"],
                "time": {"timestamp": "12.5", "repr": "2026-06-30 12:00:00"},
                "level": {"name": "INFO"},
                "extra": {"agent": "doug-nauvis"},
            },
        })

        self.assertIsNotNone(record)
        self.assertEqual(record.message, "['coerced']")
        self.assertEqual(record.timestamp, 12.5)
        self.assertEqual(record.time, "2026-06-30 12:00:00")
        self.assertEqual(record.level, "INFO")
        self.assertEqual(record.agent, "doug-nauvis")
        self.assertIs(BridgeLogRecord.from_loguru_entry(record), record)
        self.assertIsNone(BridgeLogRecord.from_loguru_entry({"record": "bad"}))

    def test_bridge_log_record_parses_jsonl_line_at_model_boundary(self):
        raw_line = json.dumps({
            "record": {
                "message": "tool_result game_rejected: cannot place",
                "time": {"timestamp": "42", "repr": "2026-06-30 12:42:00"},
                "level": {"name": "WARNING"},
                "extra": {"agent": "doug-nauvis"},
            },
        })
        record = BridgeLogRecord.from_json_line(raw_line)
        collection = BridgeLogRecordCollection.from_value((record, raw_line, "{bad json}"))

        self.assertIsNotNone(record)
        self.assertEqual(record.message, "tool_result game_rejected: cannot place")
        self.assertEqual(record.timestamp, 42.0)
        self.assertEqual(record.level, "WARNING")
        self.assertEqual([item.message for item in collection.records], [
            "tool_result game_rejected: cannot place",
            "tool_result game_rejected: cannot place",
        ])
        self.assertIs(BridgeLogRecordCollection.from_value(collection), collection)
        self.assertIsNone(BridgeLogRecord.from_json_line("{not-json"))
        self.assertIsNone(BridgeLogRecord.from_json_line('{"record":"bad"}'))
        self.assertIsNone(BridgeLogRecord.from_json_line(None))
        self.assertIs(BridgeLogRecord.from_json_line(record), record)

    def test_telemetry_event_normalizes_and_serializes_event_shapes(self):
        chat = TelemetryEvent.chat(
            "agent",
            "hello",
            agent="doug",
            tick="4",
            sections={"STATUS": {"label": "STATUS"}},
        )
        odd = TelemetryEvent.coerce({
            "type": "status",
            "data": ["not", "a", "mapping"],
            "agent": 42,
            "tick": "bad",
            "timestamp": "",
            "future": "kept",
        })
        unknown = TelemetryEvent.coerce({
            "type": "surprise",
            "data": {"ok": True},
        })

        self.assertEqual(chat.type, TelemetryEventType.CHAT)
        self.assertEqual(chat.to_dict()["type"], "chat")
        self.assertEqual(chat.data["role"], "agent")
        self.assertEqual(chat.data["sections"], {"STATUS": {"label": "STATUS"}})
        self.assertEqual(chat.agent, "doug")
        self.assertEqual(chat.tick, 4)
        self.assertIn('"type":"chat"', chat.to_json_text())
        self.assertEqual(odd.type, TelemetryEventType.STATUS)
        self.assertEqual(odd.data, {})
        self.assertEqual(odd.agent, "42")
        self.assertIsNone(odd.tick)
        self.assertTrue(odd.timestamp)
        self.assertEqual(odd.to_dict()["future"], "kept")
        self.assertEqual(unknown.type, TelemetryEventType.EVENT)
        self.assertEqual(unknown.to_dict()["type"], "event")
        self.assertEqual(
            TelemetryEvent.tool_result("mine_at", "x" * 250).data["output"],
            "x" * 200,
        )
        self.assertEqual(
            TelemetryToolCallData(
                tool="place_entity",
                input=["not", "a", "mapping"],
            ).to_dict(),
            {"tool": "place_entity", "input": ["not", "a", "mapping"]},
        )
        self.assertEqual(
            TelemetryEvent.tool_call(
                "place_entity",
                ["not", "a", "mapping"],
            ).data,
            {"tool": "place_entity", "input": ["not", "a", "mapping"]},
        )
        self.assertEqual(
            TelemetryStatusData.coerce({"working": True}).to_dict(),
            {"working": True},
        )
        self.assertEqual(TelemetryStatusData.coerce(["bad"]).to_dict(), {})
        self.assertEqual(
            TelemetryEvent.compute_cost(
                {"cost_usd": 1.25},
                agent="doug",
            ).to_dict()["data"],
            {"cost_usd": 1.25},
        )
        self.assertEqual(
            TelemetryEvent.coerce("raw").to_dict()["data"],
            {"value": "raw"},
        )

    def test_telemetry_event_batch_normalizes_http_boundary(self):
        batch = TelemetryEventBatch.coerce([
            TelemetryEvent.status({"ok": True}, agent="doug", tick="7"),
            {"type": "tool_result", "data": {"tool": "mine_at"}, "agent": 42},
            "raw",
        ])
        sse = TelemetrySseMessage.coerce(batch.events[0])

        body = json.loads(batch.to_json_bytes())

        self.assertEqual([event.type for event in batch.events], [
            TelemetryEventType.STATUS,
            TelemetryEventType.TOOL_RESULT,
            TelemetryEventType.EVENT,
        ])
        self.assertEqual(body[0]["type"], "status")
        self.assertEqual(body[0]["tick"], 7)
        self.assertEqual(body[1]["agent"], "42")
        self.assertEqual(body[2]["data"], {"value": "raw"})
        self.assertEqual(json.loads(sse.data)["type"], "status")
        self.assertEqual(sse.to_bytes(), sse.frame.encode())

    def test_telemetry_health_status_serializes_endpoint_payload(self):
        status = TelemetryHealthStatus.ok("3")
        noisy = TelemetryHealthStatus(status="", clients="-5")

        self.assertEqual(
            json.loads(status.to_json_bytes()),
            {"status": "ok", "clients": 3},
        )
        self.assertEqual(noisy.status, "ok")
        self.assertEqual(noisy.clients, 0)

    def test_bridge_log_message_extracts_report_signals_at_model_boundary(self):
        message = BridgeLogMessage.from_text(
            "Live state: nauvis @ 1,2; player entities: "
            "stone-furnace=2, transport-belt=16, stone-furnace=1\n"
            "<ledger>\nobjective: Build automation\n"
            "progress: steam power online\n</ledger>\n"
            "Research count: 4 of 275."
        )

        self.assertEqual(message.entity_summary, "stone-furnace=3, transport-belt=16")
        self.assertEqual(message.objectives, ["Build automation"])
        self.assertEqual(message.progress_entries, ["steam power online"])
        self.assertTrue(message.progress_event)
        self.assertEqual(
            message.progress_evidence.kind,
            BridgeLogProgressKind.LEDGER_PROGRESS,
        )
        self.assertEqual(message.research_counts, [4])
        self.assertIs(BridgeLogMessage.from_text(message), message)
        self.assertIs(BridgeLogMessage.from_record(message), message)

    def test_bridge_log_message_preserves_typed_record_message_boundary(self):
        record = BridgeLogRecord(message="done: $0.0000 | 1 turns | 0.0s")

        message = BridgeLogMessage.from_record(record)

        self.assertEqual(message.text, record.message)
        self.assertTrue(message.sdk_done)

    def test_bridge_log_tool_result_line_extracts_typed_classification(self):
        line = BridgeLogToolResultLine.from_line(
            'tool_result game_rejected: {"error":"Cannot place entity here"}'
        )
        plain = BridgeLogToolResultLine.from_line(
            'tool_result: {"summary":{"issue_count":1}}'
        )
        unrelated = BridgeLogToolResultLine.from_line(
            "text: model said game rejected in prose"
        )

        self.assertTrue(line.is_game_rejected)
        self.assertEqual(line.classification, ToolResultClassification.GAME_REJECTED)
        self.assertEqual(line.suffix, '{"error":"Cannot place entity here"}')
        self.assertTrue(line.tool_result)
        self.assertTrue(plain.has_tool_result_payload)
        self.assertIsNone(plain.classification)
        self.assertEqual(plain.suffix, '{"summary":{"issue_count":1}}')
        self.assertFalse(unrelated.is_game_rejected)
        self.assertIsNone(unrelated.classification)
        self.assertEqual(unrelated.suffix, "")

    def test_bridge_log_tool_result_marker_extracts_suffix_once(self):
        match = BridgeLogToolResultMarker.from_line(
            'tool_result game_rejected: {"error":"a:b"}',
            "game_rejected:",
        )
        miss = BridgeLogToolResultMarker.from_line("tool_result: ok", "sdk_failure:")

        self.assertTrue(match.matched)
        self.assertEqual(match.marker, "game_rejected:")
        self.assertEqual(match.suffix, '{"error":"a:b"}')
        self.assertFalse(miss.matched)
        self.assertEqual(miss.suffix, "")

    def test_text_marker_split_owns_first_marker_protocol_split(self):
        split = TextMarkerSplit.from_text("prefix marker suffix:still suffix", "marker")
        miss = TextMarkerSplit.from_text("prefix only", "marker")
        empty_marker = TextMarkerSplit.from_text("prefix", "")

        self.assertTrue(split.matched)
        self.assertEqual(split.before, "prefix ")
        self.assertEqual(split.after, " suffix:still suffix")
        self.assertFalse(miss.matched)
        self.assertEqual(miss.before, "prefix only")
        self.assertFalse(empty_marker.matched)

    def test_bridge_log_message_filters_noise_and_summarizes_failures(self):
        provider = BridgeLogMessage.from_text(
            "provider usage limit active until 2026-06-30 03:19:43 CDT; "
            "pausing agent attempts"
        )
        prose_progress = BridgeLogMessage.from_text(
            "<ledger>\n"
            "progress: no change across planning ticks. "
            "Plan fully validated and awaiting execution.\n"
            "</ledger>"
        )
        rejection = BridgeLogMessage.from_text(
            'tool_result game_rejected: {"entity":"transport-belt",'
            '"error":"Cannot place entity here"}'
        )
        research_status = BridgeLogMessage.from_text(
            'tool_result game_rejected: [{"type":"text","text":"{'
            '\\"researched_count\\":6,\\"research_progress\\":0.36}"}]'
        )

        self.assertTrue(provider.provider_pause)
        self.assertEqual(provider.provider_reset_until, "2026-06-30 03:19:43 CDT")
        self.assertTrue(provider.runtime_evidence.has(BridgeLogRuntimeKind.PROVIDER_PAUSE))
        self.assertEqual(
            provider.runtime_evidence.provider_reset_until,
            "2026-06-30 03:19:43 CDT",
        )
        self.assertEqual(
            provider.runtime_evidence.reasons,
            ("provider usage limit pause",),
        )
        self.assertEqual(
            prose_progress.progress_entries,
            ["no change across planning ticks. Plan fully validated and awaiting execution."],
        )
        self.assertFalse(prose_progress.progress_event)
        self.assertEqual(
            prose_progress.progress_evidence.kind,
            BridgeLogProgressKind.PLAN_WAITING,
        )
        self.assertEqual(
            prose_progress.progress_evidence.reason,
            "ledger plan waiting for execution",
        )
        self.assertEqual(
            rejection.gameplay_rejection_signatures,
            ["Cannot place entity here | entity=transport-belt"],
        )
        self.assertEqual(
            rejection.gameplay_evidence.signatures,
            ["Cannot place entity here | entity=transport-belt"],
        )
        self.assertEqual(
            rejection.gameplay_rejection_lines,
            ['tool_result game_rejected: {"entity":"transport-belt","error":"Cannot place entity here"}'],
        )
        self.assertEqual(
            rejection.gameplay_evidence.lines,
            ['tool_result game_rejected: {"entity":"transport-belt","error":"Cannot place entity here"}'],
        )
        self.assertEqual(
            BridgeLogMessage.first_gameplay_rejection_signature_from_text(
                rejection.text,
            ),
            "Cannot place entity here | entity=transport-belt",
        )
        self.assertEqual(research_status.gameplay_rejection_signatures, [])
        self.assertEqual(research_status.gameplay_evidence.signatures, [])
        self.assertEqual(
            research_status.gameplay_evidence.lines,
            [research_status.text],
        )
        self.assertEqual(
            BridgeLogMessage.first_gameplay_rejection_signature_from_text(
                research_status.text,
            ),
            "",
        )

    def test_bridge_log_message_exposes_typed_runtime_evidence(self):
        message = BridgeLogMessage.from_text(
            "spawning claude sdk [model=haiku]\n"
            "done: $0.0000 | 1 turns | 0.0s\n"
            "sdk context window limit; cleared session for doug\n"
            "watchdog_abort: repeated same game rejection\n"
            "automation research completed",
        )

        self.assertTrue(message.sdk_spawn)
        self.assertFalse(message.sdk_done)
        self.assertTrue(message.context_reset)
        self.assertTrue(message.watchdog_abort)
        self.assertTrue(message.research_completed)
        self.assertEqual(
            message.runtime_evidence.kinds,
            frozenset({
                BridgeLogRuntimeKind.SDK_SPAWN,
                BridgeLogRuntimeKind.CONTEXT_RESET,
                BridgeLogRuntimeKind.WATCHDOG_ABORT,
                BridgeLogRuntimeKind.RESEARCH_COMPLETED,
            }),
        )
        self.assertEqual(
            message.runtime_evidence.reasons,
            (
                "sdk spawn log",
                "context window session reset",
                "watchdog abort log",
                "research completed log",
            ),
        )

        done = BridgeLogMessage.from_text("done: $0.0000 | 1 turns | 0.0s")
        self.assertTrue(done.sdk_done)
        self.assertEqual(
            done.runtime_evidence.kinds,
            frozenset({BridgeLogRuntimeKind.SDK_DONE}),
        )
        self.assertEqual(done.runtime_evidence.reasons, ("sdk done log",))

    def test_bridge_run_verdict_uses_typed_operator_states(self):
        provider = BridgeRunReport(
            provider_pauses=1,
            provider_reset_until="2026-06-30 03:19:43 CDT",
        )
        progress = BridgeRunReport(recent_progress_events=1)
        unverified = BridgeRunReport(
            recent_progress_events=2,
            automation_verified_successes=1,
            automation_verified_failures=2,
        )
        manual_heavy = BridgeRunReport(
            recent_progress_events=2,
            automation_tool_calls=1,
            manual_transfer_tool_calls=4,
        )
        component_manual = BridgeRunReport(
            recent_progress_events=2,
            component_automation_tool_calls=1,
            manual_component_craft_tool_calls=3,
        )
        repeated = BridgeRunReport(
            top_gameplay_rejections=[("Cannot place entity here", 3)],
        )
        context = BridgeRunReport(context_resets=1)

        provider_verdict = BridgeRunVerdict.from_report_state(
            provider,
            last_provider_pause_ts=20.0,
            progress_timestamps=(10.0,),
        )

        self.assertEqual(BridgeProgressTimestamps.from_value(("1.5", 3)).latest, 3.0)
        self.assertEqual(provider_verdict.kind, BridgeRunVerdictKind.PROVIDER_PAUSED)
        self.assertIn("2026-06-30 03:19:43 CDT", provider_verdict.message)
        self.assertEqual(
            BridgeRunVerdict.from_report_state(progress).kind,
            BridgeRunVerdictKind.RECENT_PROGRESS,
        )
        self.assertEqual(
            BridgeRunVerdict.from_report_state(unverified).kind,
            BridgeRunVerdictKind.AUTOMATION_UNVERIFIED,
        )
        self.assertIn(
            "automation controllers are failing verification",
            BridgeRunVerdict.from_report_state(unverified).message,
        )
        self.assertEqual(
            BridgeRunVerdict.from_report_state(manual_heavy).kind,
            BridgeRunVerdictKind.MANUAL_HEAVY,
        )
        self.assertIn(
            "manual transfer calls exceed automation controller calls",
            BridgeRunVerdict.from_report_state(manual_heavy).message,
        )
        self.assertEqual(
            BridgeRunVerdict.from_report_state(component_manual).kind,
            BridgeRunVerdictKind.COMPONENT_MANUAL_HEAVY,
        )
        self.assertIn(
            "science ingredients are being hand-crafted",
            BridgeRunVerdict.from_report_state(component_manual).message,
        )
        self.assertEqual(
            BridgeRunVerdict.from_report_state(repeated).kind,
            BridgeRunVerdictKind.REPEATED_FAILURES,
        )
        self.assertEqual(
            BridgeRunVerdict.from_report_state(context).kind,
            BridgeRunVerdictKind.CONTEXT_RESETS,
        )
        self.assertEqual(
            BridgeRunVerdict.no_records().message,
            "operator attention needed: no bridge records found",
        )

    def test_bridge_log_message_extracts_embedded_game_rejection_lines(self):
        message = BridgeLogMessage.from_text(
            "Recent events:\n"
            "- failure: game_rejected: one\n"
            "- progress: stable\n"
            "- failure: game_rejected: two\n"
        )

        self.assertEqual(
            message.gameplay_rejection_lines,
            ["- failure: game_rejected: one", "- failure: game_rejected: two"],
        )

    def test_bridge_log_message_compacts_power_payloads(self):
        diagnose_payload = json.dumps([{
            "type": "text",
            "text": json.dumps({
                "summary": {"issue_count": 1, "critical_issues": 1},
                "issues": [{"type": "boiler_no_fuel", "severity": "critical"}],
                "status": "critical",
                "next_action": "repair_existing_steam_power",
            }),
        }])

        message = BridgeLogMessage.from_text(f"tool_result: {diagnose_payload}")

        self.assertEqual(
            message.power_evidence.kind,
            BridgeLogPowerKind.DIAGNOSTIC_PAYLOAD,
        )
        self.assertIn("steam_power status=critical", message.power_summary)
        self.assertIn("types=boiler_no_fuel", message.power_summary)

        status = BridgeLogMessage.from_text("text: Power grid operational.")
        self.assertEqual(
            status.power_evidence.kind,
            BridgeLogPowerKind.CONCISE_STATUS_TEXT,
        )
        self.assertEqual(status.power_summary, "text: Power grid operational.")

        narrative = BridgeLogMessage.from_text(
            "text: Power layout: steam engine sits north of the boiler.",
        )
        self.assertEqual(
            narrative.power_evidence.kind,
            BridgeLogPowerKind.POWER_RELATED_TEXT,
        )
        self.assertEqual(narrative.power_summary, "")

    def test_game_rejection_payload_unwraps_mcp_text_and_signs_gameplay_failure(self):
        payload = GameRejectionPayload.from_payload(json.dumps([{
            "type": "text",
            "text": json.dumps({
                "success": False,
                "error": "Crafting did not start",
                "recipe": "transport-belt",
            }),
        }]))

        self.assertFalse(payload.is_research_status())
        self.assertFalse(payload.is_invalid_request())
        self.assertEqual(
            payload.evidence().kind,
            GameRejectionEvidenceKind.GAMEPLAY_FAILURE,
        )
        self.assertEqual(
            payload.evidence().reason,
            "gameplay rejection payload",
        )
        self.assertEqual(
            payload.signature(),
            "Crafting did not start | recipe=transport-belt",
        )
        self.assertIs(GameRejectionPayload.from_payload(payload), payload)

    def test_game_rejection_payload_hides_research_and_invalid_request_noise(self):
        research = GameRejectionPayload.from_payload(
            '{"researched_count":6,"research_progress":0.36}'
        )
        invalid = GameRejectionPayload.from_payload(
            '{"success":false,"error":"value for required field category is missing",'
            '"action_needed":"fix_get_power_status"}'
        )
        truncated = GameRejectionPayload.from_payload(
            '[{"type":"text","text":"{\\"success\\":false,'
            '\\"error\\":\\"invalid type: map'
        )

        self.assertTrue(research.is_research_status())
        self.assertEqual(research.evidence().kind, GameRejectionEvidenceKind.RESEARCH_STATUS)
        self.assertEqual(research.evidence().reason, "research-status payload")
        self.assertEqual(research.signature(), "")
        self.assertTrue(invalid.is_invalid_request())
        self.assertEqual(invalid.evidence().kind, GameRejectionEvidenceKind.INVALID_REQUEST)
        self.assertEqual(invalid.evidence().reason, "invalid-request payload")
        self.assertEqual(invalid.signature(), "")
        self.assertTrue(truncated.is_invalid_request())
        self.assertEqual(truncated.evidence().kind, GameRejectionEvidenceKind.INVALID_REQUEST)
        self.assertEqual(truncated.signature(), "")

    def test_power_status_coerces_remote_payload_shape(self):
        status = PowerStatus.from_payload({
            "network_id": 7,
            "pole_count": "15",
            "generators": [
                {"name": "steam-engine", "count": 1, "extra": "ignored"},
                {"name": None, "count": 2},
                "bad generator",
            ],
            "consumers": {
                "working": "3",
                "low_power": 1,
                "no_power": 0,
                "total": 4,
            },
            "production_kw": 900,
            "consumption_kw": 120,
            "satisfaction": "ok",
        })

        self.assertIsNotNone(status)
        self.assertEqual(
            status.compact(),
            "network=7; poles=15; generators=steam-engine=1, unknown=2; "
            "consumers=3 working/1 low/0 none/4 total; "
            "production_kw=900; consumption_kw=120; satisfaction=ok",
        )
        self.assertIsNone(PowerStatus.from_payload(["not", "a", "payload"]))

    def test_power_status_preserves_typed_payloads_and_nested_summaries(self):
        generator_blocks = (
            {"name": "steam-engine", "count": 1},
            PowerGeneratorSummary(name="solar-panel", count=2),
            "bad generator",
        )
        tuple_status = PowerStatus(
            network_id=10,
            pole_count=3,
            generators=generator_blocks,
        )
        status = PowerStatus(
            network_id=9,
            pole_count=2,
            generators=[PowerGeneratorSummary(name="steam-engine", count=1)],
            consumers=PowerConsumerSummary(working=1, low_power=0, no_power=0, total=1),
            production_kw=900,
            consumption_kw=120,
            satisfaction=1,
        )

        self.assertIs(PowerStatus.from_payload(status), status)
        self.assertEqual(
            PowerGeneratorSummaryCollection.from_value(generator_blocks).to_list(),
            list(generator_blocks[:2]),
        )
        self.assertEqual(
            [generator.compact() for generator in tuple_status.generators],
            ["steam-engine=1", "solar-panel=2"],
        )
        self.assertEqual(
            PowerStatus.compact_from_payload(status),
            "network=9; poles=2; generators=steam-engine=1; "
            "consumers=1 working/0 low/0 none/1 total; "
            "production_kw=900; consumption_kw=120; satisfaction=1",
        )

    def test_steam_power_diagnostic_coerces_nested_existing_plant_payload(self):
        diagnostic = SteamPowerDiagnostic.from_payload({
            "status": "top-level fallback",
            "next_action": "fallback action",
            "existing_plant": {
                "summary": {
                    "issue_count": "2",
                    "critical_issues": 1,
                },
                "issues": [
                    {"type": "boiler_no_fuel"},
                    {"type": "steam_engine_no_steam", "extra": True},
                    {"type": None},
                    "bad issue",
                ],
                "status": "critical",
            },
        })

        self.assertIsNotNone(diagnostic)
        self.assertEqual(
            diagnostic.compact(),
            "steam_power status=critical; issues=2; critical=1; "
            "types=boiler_no_fuel, steam_engine_no_steam, unknown; "
            "next=fallback action",
        )
        self.assertIsNone(SteamPowerDiagnostic.from_payload({"unrelated": True}))

    def test_steam_power_diagnostic_preserves_typed_payloads_and_nested_summaries(self):
        issue_blocks = (
            {"type": "boiler_no_fuel"},
            SteamPowerIssue(type="steam_engine_no_steam"),
            "bad issue",
        )
        tuple_diagnostic = SteamPowerDiagnostic(
            summary=SteamPowerSummary(issue_count=2, critical_issues=1),
            issues=issue_blocks,
            status="critical",
            next_action="fuel boiler",
        )
        diagnostic = SteamPowerDiagnostic(
            summary=SteamPowerSummary(issue_count=1, critical_issues=1),
            issues=[SteamPowerIssue(type="boiler_no_fuel")],
            status="critical",
            next_action="fuel boiler",
        )

        self.assertIs(SteamPowerDiagnostic.from_payload(diagnostic), diagnostic)
        self.assertEqual(
            SteamPowerIssueCollection.from_value(issue_blocks).to_list(),
            list(issue_blocks[:2]),
        )
        self.assertEqual(
            [issue.type for issue in tuple_diagnostic.issues],
            ["boiler_no_fuel", "steam_engine_no_steam"],
        )
        self.assertEqual(
            diagnostic.compact(),
            "steam_power status=critical; issues=1; critical=1; "
            "types=boiler_no_fuel; next=fuel boiler",
        )
