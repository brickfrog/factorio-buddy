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
    RconRemoteCall,
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



class ModelLiveEvalRconTests(unittest.TestCase):

    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.base = Path(self.tempdir.name)

    def test_live_state_parses_entity_counts_into_typed_model(self):
        state = LiveState.from_line(
            "Live state: nauvis @ 54.1,-21.0; player entities: "
            "stone-furnace=2, transport-belt=16, stone-furnace=1"
        )

        self.assertTrue(state.found)
        self.assertEqual(state.surface, "nauvis")
        self.assertEqual(state.x, 54.1)
        self.assertEqual(state.y, -21.0)
        self.assertEqual(state.entity_counts["stone-furnace"], 3)
        self.assertEqual(
            state.entity_summary,
            "stone-furnace=3, transport-belt=16",
        )
        self.assertTrue(state.has("transport-belt"))
        self.assertFalse(state.has("lab"))
        self.assertTrue(state.has_automation_capable_footprint())

    def test_live_state_treats_lone_furnace_as_bootstrap_footprint(self):
        state = LiveState(
            found=True,
            surface="nauvis",
            x=5,
            y=0,
            entity_counts={"stone-furnace": 1},
        )

        self.assertFalse(state.has_automation_capable_footprint())

    def test_live_state_validates_json_remote_payload(self):
        state = LiveState.from_rcon_response(json.dumps({
            "found": True,
            "surface": "nauvis",
            "x": "46.7",
            "y": -15.6,
            "entity_counts": {
                "lab": "1",
                "electric-mining-drill": 1,
                "stone-furnace": 2,
                "bad-count": "wat",
            },
        }))

        self.assertEqual(state.surface, "nauvis")
        self.assertEqual(state.entity_counts, {
            "lab": 1,
            "electric-mining-drill": 1,
            "stone-furnace": 2,
        })
        self.assertEqual(
            state.entity_summary,
            "electric-mining-drill=1, stone-furnace=2, lab=1",
        )
        self.assertEqual(
            state.to_line(),
            "Live state: nauvis @ 46.7,-15.6; player entities: "
            "electric-mining-drill=1, stone-furnace=2, lab=1",
        )

    def test_provider_usage_limit_infers_provider_timezone_from_request_id(self):
        text = (
            "API Error: Request rejected (429) · [1308][Usage limit reached "
            "for 5 hour. Your limit will reset at 2026-06-29 08:35:15]"
            "[202606290714523c923559680c406d]"
        )
        now = datetime(2026, 6, 28, 23, 14, 52, tzinfo=timezone.utc)

        limit = ProviderUsageLimit.from_text(text, now=now)

        self.assertIsNotNone(limit)
        self.assertEqual(limit.reset_at.utcoffset(), timedelta(hours=8))
        self.assertEqual(
            limit.reset_at.astimezone(timezone.utc),
            datetime(2026, 6, 29, 0, 35, 15, tzinfo=timezone.utc),
        )

    def test_provider_usage_limit_uses_default_offset_without_request_id(self):
        text = (
            "API Error: Request rejected (429) · [1308][Usage limit reached "
            "for 5 hour. Your limit will reset at 2026-06-29 08:35:15]"
        )
        settings = ProviderUsageLimitSettings.from_env({
            "BRIDGE_USAGE_LIMIT_RESET_UTC_OFFSET": " -05:00 ",
        })

        limit = ProviderUsageLimit.from_text(
            text,
            default_utc_offset=settings.usage_limit_reset_utc_offset,
        )

        self.assertIsNotNone(limit)
        self.assertEqual(settings.usage_limit_reset_utc_offset, "-05:00")
        self.assertIs(ProviderUsageLimitSettings.from_env(settings), settings)
        self.assertIsNone(
            ProviderUsageLimitSettings.from_env({
                "BRIDGE_USAGE_LIMIT_RESET_UTC_OFFSET": " ",
            }).usage_limit_reset_utc_offset,
        )
        self.assertEqual(limit.reset_at.utcoffset(), timedelta(hours=-5))
        self.assertEqual(
            limit.reset_at.astimezone(timezone.utc),
            datetime(2026, 6, 29, 13, 35, 15, tzinfo=timezone.utc),
        )

    def test_provider_usage_limit_settings_uses_typed_env_bindings(self):
        fields = ProviderUsageLimitSettings.env_fields()

        self.assertTrue(all(isinstance(field, BridgeRuntimeEnvField) for field in fields))
        self.assertEqual(
            fields[0].env_name,
            "BRIDGE_USAGE_LIMIT_RESET_UTC_OFFSET",
        )
        with mock.patch.object(
            ProviderUsageLimitSettings,
            "ENV_FIELDS",
            (
                BridgeRuntimeEnvField(env_name="A", field_name="usage_limit_reset_utc_offset"),
                BridgeRuntimeEnvField(env_name="B", field_name="usage_limit_reset_utc_offset"),
            ),
        ):
            with self.assertRaisesRegex(BridgeValidationError, "duplicate field_name"):
                ProviderUsageLimitSettings.env_fields()

    def test_provider_usage_limit_ignores_non_limit_text(self):
        self.assertIsNone(ProviderUsageLimit.from_text("ordinary SDK text"))
        self.assertIsNone(ProviderUsageLimit.from_text(None))

    def test_eval_production_snapshot_coerces_maps_and_scores_rate_first(self):
        snapshot = EvalProductionSnapshot.coerce({
            "produced": {"iron-plate": "100", "junk": "bad"},
            "rate_per_min": {"iron-plate": 16, "zero": 0},
        })

        self.assertEqual(snapshot.produced, {"iron-plate": 100.0})
        self.assertEqual(snapshot.rate_per_min, {"iron-plate": 16.0})
        self.assertTrue(snapshot.any_produced(("iron-plate",)))
        self.assertTrue(snapshot.rate_at_least("iron-plate", 16))
        self.assertEqual(snapshot.score_source(), {"iron-plate": 16.0})
        self.assertEqual(snapshot.production_score({"iron-plate": 2.0}), 32.0)
        self.assertEqual(EvalProductionSnapshot.coerce("bad").to_dict(), {
            "produced": {},
            "rate_per_min": {},
        })

    def test_eval_production_snapshot_parses_noisy_rcon_text(self):
        snapshot = EvalProductionSnapshot.from_rcon_text(
            "noise\n"
            '{"ignored": true}\n'
            '{"produced":{"iron-plate":"12"},"rate_per_min":{"iron-plate":16}}\n'
        )

        self.assertEqual(snapshot.to_dict(), {
            "produced": {"iron-plate": 12.0},
            "rate_per_min": {"iron-plate": 16.0},
        })
        self.assertEqual(
            EvalProductionSnapshot.from_rcon_text("not json").to_dict(),
            {"produced": {}, "rate_per_min": {}},
        )
        self.assertEqual(
            EvalProductionSnapshot.from_rcon_text("[1, 2, 3]").to_dict(),
            {"produced": {}, "rate_per_min": {}},
        )

    def test_eval_result_normalizes_milestones_and_comparison(self):
        result = EvalResult.coerce(
            {
                "production_score": "12.5",
                "milestones": {"burner_mining": 1},
            },
            milestone_names=("burner_mining", "red_science"),
        )
        lower = EvalResult.create(
            production_score=3,
            milestones={"burner_mining": False},
        )

        self.assertEqual(result.production_score, 12.5)
        self.assertEqual(result.milestones_reached, 1)
        self.assertEqual(result.milestones["red_science"], False)
        self.assertTrue(result.is_better_than(lower))
        self.assertEqual(result.to_dict()["milestones_reached"], 1)

    def test_sdk_error_signal_detects_context_window_variants(self):
        signal = SdkErrorSignal.from_text(
            "API Error: The model has reached its context window limit."
        )
        self.assertTrue(signal.has(SdkErrorKind.CONTEXT_WINDOW))
        self.assertTrue(signal.context_window_limit)
        self.assertFalse(signal.terminal_result_echo)
        self.assertEqual(signal.reasons, ("context window limit",))
        self.assertTrue(SdkErrorSignal.is_context_window_limit(signal.raw_text))
        self.assertTrue(SdkErrorSignal.is_context_window_limit(
            "API Error: The model has reached its context-window limit."
        ))
        self.assertTrue(SdkErrorSignal.is_context_window_limit(
            "Request failed: maximum context exceeded"
        ))
        self.assertTrue(SdkErrorSignal.is_context_window_limit(
            "Request failed due to context length"
        ))
        self.assertFalse(SdkErrorSignal.is_context_window_limit(
            "API Error: Request rejected (429)"
        ))
        empty = SdkErrorSignal.from_text(None)
        self.assertEqual(empty.raw_text, "")
        self.assertEqual(empty.kinds, frozenset())
        self.assertEqual(empty.reasons, ())
        self.assertFalse(SdkErrorSignal.is_context_window_limit(None))
        terminal_echo = SdkErrorSignal.from_text(
            "Claude Code returned an error result: Reached maximum number of turns (15)"
        )
        self.assertTrue(terminal_echo.has(SdkErrorKind.TERMINAL_RESULT_ECHO))
        self.assertEqual(terminal_echo.reasons, ("sdk terminal result echo",))
        self.assertTrue(SdkErrorSignal.is_terminal_result_echo(terminal_echo.raw_text))
        self.assertFalse(SdkErrorSignal.is_terminal_result_echo("RCON connection dropped"))

    def test_sdk_stderr_signal_classifies_known_cli_noise(self):
        lines = BridgeTextLines.from_text("  first \n\n second\n", keep_blank=False)
        raw_lines = BridgeTextLines.from_text("  raw \n", strip=False)

        self.assertEqual(lines.lines, ("first", "second"))
        self.assertEqual(lines.non_empty, ("first", "second"))
        self.assertEqual(lines.reversed_non_empty, ("second", "first"))
        self.assertEqual(raw_lines.lines, ("  raw ",))

        csv = CommaSeparatedItems.from_value(" alpha, beta ,, gamma ")
        iterable = CommaSeparatedItems.from_value([" alpha ", "", 42])
        none = CommaSeparatedItems.from_value(None)

        self.assertEqual(csv.to_list(), ["alpha", "beta", "gamma"])
        self.assertEqual(csv.to_list(max_items=2), ["alpha", "beta"])
        self.assertEqual(iterable.to_list(), ["alpha", "42"])
        self.assertEqual(none.to_list(), [])
        self.assertIs(CommaSeparatedItems.from_value(csv), csv)

        empty = SdkStderrSignal.from_text("")
        benign = SdkStderrSignal.from_text(
            "claude.ai connectors are disabled\n"
            "ANTHROPIC_API_KEY or another auth source is set"
        )
        mixed = SdkStderrSignal.from_text(
            "claude.ai connectors are disabled\n"
            "panic: transport exploded"
        )

        self.assertEqual(empty.kind, SdkStderrKind.EMPTY)
        self.assertTrue(empty.benign)
        self.assertEqual(empty.lines, ())
        self.assertTrue(SdkStderrSignal.is_benign(None))
        self.assertEqual(benign.kind, SdkStderrKind.BENIGN_CONNECTOR_NOISE)
        self.assertTrue(benign.benign)
        self.assertEqual(len(benign.lines), 2)
        self.assertEqual(
            benign.reasons,
            ("known SDK connector/auth stderr noise",),
        )
        self.assertEqual(mixed.kind, SdkStderrKind.UNKNOWN)
        self.assertFalse(mixed.benign)
        self.assertFalse(SdkStderrSignal.is_benign(mixed.raw_text))

    def test_agent_invocation_exception_signal_combines_sdk_and_usage_limit(self):
        provider_now = datetime(2026, 6, 30, 8, 51, 39, tzinfo=timezone.utc)
        raw = (
            "Claude Code returned an error result: API Error: Request rejected "
            "(429) [1308][Usage limit reached for 5 hour. Your limit will reset "
            "at 2026-06-30 16:19:43][202606301651398dd15706b5184e94]"
        )

        signal = AgentInvocationExceptionSignal.from_exception(
            RuntimeError(raw),
            now=provider_now,
        )
        context = AgentInvocationExceptionSignal.from_exception(
            RuntimeError("API Error: The model has reached its context window limit."),
        )

        self.assertEqual(signal.raw_text, raw)
        self.assertTrue(signal.terminal_result_echo)
        self.assertFalse(signal.context_window_limit)
        self.assertTrue(signal.usage_limit_seen)
        self.assertIsNotNone(signal.usage_limit)
        self.assertIn("Request rejected", signal.error_message)
        self.assertIn("Claude Code returned", signal.short_text)
        self.assertTrue(context.context_window_limit)
        self.assertFalse(context.terminal_result_echo)
        self.assertFalse(context.usage_limit_seen)

    def test_bridge_run_report_coerces_window_and_serializes_rejections(self):
        report = BridgeRunReport(
            recent_progress_window_s=-5,
            top_gameplay_rejections=[("Cannot place entity here", 4)],
        )

        self.assertEqual(report.recent_progress_window_s, 1.0)
        self.assertEqual(report.to_dict()["top_gameplay_rejections"], [{
            "count": 4,
            "signature": "Cannot place entity here",
        }])
        rendered = report.to_json_text(indent=2, sort_keys=True)
        self.assertTrue(rendered.startswith("{\n"))
        self.assertEqual(
            json.loads(rendered)["top_gameplay_rejections"],
            [{"count": 4, "signature": "Cannot place entity here"}],
        )

    def test_rcon_json_response_parses_last_json_line(self):
        response = RconJsonResponse.from_text(
            "noise from server\n"
            "not json\n"
            '{"network_id": 7, "pole_count": 15}\n'
        )

        self.assertEqual(
            response.value,
            {"network_id": 7, "pole_count": 15},
        )
        self.assertIn("noise from server", response.raw_text)
        self.assertIs(RconJsonResponse.from_text(response), response)
        self.assertEqual(RconJsonResponse.parse_value(response), response.value)

    def test_rcon_json_response_falls_back_to_whole_response_and_errors_cleanly(self):
        self.assertEqual(RconJsonResponse.parse_value("[1, 2, 3]"), [1, 2, 3])

        with self.assertRaisesRegex(
            BridgeValidationError,
            "rcon_response: did not contain JSON: nope",
        ):
            RconJsonResponse.parse_value("nope")

    def test_rcon_text_response_extracts_final_nonempty_line(self):
        response = RconTextResponse.from_text("noise\n\n  final state  \n")

        self.assertEqual(response.raw_text, "noise\n\n  final state  \n")
        self.assertEqual(response.text, "final state")
        self.assertEqual(RconTextResponse.final_line("\n\n"), "")

    def test_rcon_remote_call_validates_name_and_renders_command(self):
        command = RconRemoteCall.command("get_power_status", "-41", "26", "500")

        self.assertEqual(
            command,
            '/silent-command rcon.print(remote.call("claude_interface", '
            '"get_power_status", -41, 26, 500))',
        )
        with self.assertRaisesRegex(ValueError, "invalid remote name"):
            RconRemoteCall.command('bad") game.print("oops')

    def test_rcon_remote_call_renders_side_effect_command_without_printing_result(self):
        command = RconRemoteCall.side_effect_command(
            "receive_response",
            1,
            "[=[doug]=]",
            "[=[ok]=]",
        )

        self.assertEqual(
            command,
            '/silent-command remote.call("claude_interface", '
            '"receive_response", 1, [=[doug]=], [=[ok]=])',
        )
        self.assertNotIn("rcon.print", command)

    def test_rcon_remote_call_can_stringify_printed_result(self):
        command = RconRemoteCall.string_command("connected_player_count")

        self.assertEqual(
            command,
            '/silent-command rcon.print(tostring(remote.call("claude_interface", '
            '"connected_player_count")))',
        )

    def test_connected_player_count_result_validates_json_payload(self):
        result = ConnectedPlayerCountResult.from_rcon_response('noise\n{"count":"2"}\n')

        self.assertEqual(result.count, 2)
        self.assertTrue(result.has_connected_players)
        self.assertIs(ConnectedPlayerCountResult.from_rcon_response(result), result)
        self.assertFalse(
            ConnectedPlayerCountResult.from_rcon_response('{"count":0}').has_connected_players,
        )
