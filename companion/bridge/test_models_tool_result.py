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



class ModelToolResultTests(unittest.TestCase):

    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.base = Path(self.tempdir.name)

    def test_tool_result_outcome_prefers_structured_success(self):
        outcome = ToolResultOutcome.from_payload({
            "success": True,
            "queued": 3,
            "error": "legacy stale error text",
        })

        self.assertIsNotNone(outcome)
        self.assertEqual(outcome.classification, ToolResultClassification.OK)
        self.assertEqual(outcome.source, "success")

    def test_tool_result_outcome_accepts_typed_payload(self):
        success = ToolResultPayload(
            success=True,
            error="legacy stale error text",
        )
        expected_miss = ToolResultPayload(
            type="text",
            text='{"success":false,"expected_miss":true}',
        )
        hidden_failure = ToolResultPayload.model_validate({
            "nested": {"success": False, "error": "Cannot place entity here"},
        })

        self.assertEqual(
            ToolResultOutcome.from_payload(success).classification,
            ToolResultClassification.OK,
        )
        self.assertEqual(
            ToolResultOutcome.from_payload(expected_miss).classification,
            ToolResultClassification.EXPECTED_MISS,
        )
        self.assertFalse(
            ToolResultOutcome.payload_indicates_progress(ToolResultPayload(
                success=False,
                error="Cannot place entity here",
            )),
        )
        self.assertEqual(
            ToolResultOutcome.from_payload(hidden_failure).classification,
            ToolResultClassification.GAME_REJECTED,
        )
        ok = ToolResultOutcome.from_payload(success)
        self.assertIs(ToolResultOutcome.from_payload(ok), ok)

    def test_tool_result_outcome_classifies_expected_empty_mining(self):
        outcome = ToolResultOutcome.from_payload({
            "success": False,
            "mined_count": 0,
            "error": None,
            "inventory": [],
        })

        self.assertIsNotNone(outcome)
        self.assertEqual(outcome.classification, ToolResultClassification.EXPECTED_MISS)
        self.assertEqual(outcome.source, "empty_mining")

    def test_tool_result_outcome_classifies_structured_placement_rejection(self):
        outcome = ToolResultOutcome.from_payload({
            "success": False,
            "can_place": False,
            "entity": "stone-furnace",
            "position": {"x": 76, "y": -19},
            "error": "totally opaque runtime refusal",
        })

        self.assertIsNotNone(outcome)
        self.assertEqual(outcome.classification, ToolResultClassification.GAME_REJECTED)
        self.assertEqual(outcome.source, "placement_rejected")

    def test_tool_result_outcome_keeps_read_only_placement_diagnostics_ok(self):
        outcome = ToolResultOutcome.from_payload({
            "allowed": False,
            "policy_allowed": True,
            "factorio_allowed": False,
            "entity": "burner-mining-drill",
            "position": {"x": 45, "y": -35},
            "factorio": {"error": "Factorio cannot place entity here"},
        })

        self.assertIsNotNone(outcome)
        self.assertEqual(outcome.classification, ToolResultClassification.OK)
        self.assertEqual(outcome.source, "placement_diagnostic")

    def test_tool_result_outcome_classifies_automation_verification_failures_as_game_rejected(self):
        outcome = ToolResultOutcome.from_payload({
            "success": False,
            "placement_success": True,
            "automation_verified": {
                "success": False,
                "placed_unit_statuses": [
                    {"unit_number": 77, "name": "inserter", "status": "no_power"},
                ],
            },
        })

        self.assertIsNotNone(outcome)
        self.assertEqual(outcome.classification, ToolResultClassification.GAME_REJECTED)
        self.assertEqual(outcome.source, "automation_unverified")
        self.assertTrue(outcome.should_journal_failure)
        self.assertEqual(outcome.log_level, "info")

    def test_tool_result_outcome_recurses_through_text_block_json(self):
        blocks = ({
            "type": "text",
            "text": (
                '{"success":false,"expected_miss":true,'
                '"error":"Cannot place entity here"}'
            ),
        },)
        collection = ToolResultPayloadCollection.from_value(blocks)
        outcome = ToolResultOutcome.from_payload(collection)

        self.assertEqual(collection.items, blocks)
        self.assertIsNotNone(outcome)
        self.assertEqual(outcome.classification, ToolResultClassification.EXPECTED_MISS)

    def test_tool_result_outcome_accepts_text_classifier_for_json_failures(self):
        def classify(text: str):
            lowered = text.lower()
            if "invalid type" in lowered:
                return ToolResultClassification.INVALID_REQUEST
            if "blocked" in lowered:
                return ToolResultClassification.GAME_REJECTED
            return None

        message_outcome = ToolResultOutcome.from_payload(
            {"success": False, "message": "blocked by terrain"},
            text_classifier=classify,
        )
        text_block_outcome = ToolResultOutcome.from_payload(
            [{"type": "text", "text": "Error: invalid type: map"}],
            text_classifier=classify,
        )
        research_status = ToolResultOutcome.from_payload(
            {
                "researched_count": 6,
                "message": "Labs have no power! Connect labs to the power grid.",
            },
            text_classifier=classify,
        )

        self.assertIsNotNone(message_outcome)
        self.assertEqual(
            message_outcome.classification,
            ToolResultClassification.GAME_REJECTED,
        )
        self.assertIsNotNone(text_block_outcome)
        self.assertEqual(
            text_block_outcome.classification,
            ToolResultClassification.INVALID_REQUEST,
        )
        self.assertIsNone(research_status)

    def test_tool_result_outcome_from_text_uses_structured_success_before_heuristics(self):
        def classify(text: str):
            lowered = text.lower()
            if "cannot place" in lowered:
                return ToolResultClassification.GAME_REJECTED
            if "no items of that type" in lowered:
                return ToolResultClassification.EXPECTED_MISS
            if "invalid type" in lowered:
                return ToolResultClassification.INVALID_REQUEST
            return None

        success_with_stale_error = ToolResultOutcome.from_text(
            '{"success":true,"queued":1,"error":"Cannot place entity here"}',
            sdk_is_error=True,
            text_classifier=classify,
        )
        expected_miss = ToolResultOutcome.from_text(
            "Error: No items of that type in inventory",
            sdk_is_error=True,
            text_classifier=classify,
        )
        inventory_shape_miss = ToolResultOutcome.from_text(
            "Error: Entity has no such inventory",
            sdk_is_error=True,
            text_classifier=classify,
        )
        game_rejected = ToolResultOutcome.from_text(
            '{"success":false,"can_place":false,"entity":"transport-belt",'
            '"error":"Cannot place entity here","position":{"x":56,"y":-25}}',
            text_classifier=classify,
        )
        invalid_request = ToolResultOutcome.from_text(
            '[{"type":"text","text":"Error: invalid type: map, expected a sequence"}]',
            text_classifier=classify,
        )

        self.assertEqual(
            success_with_stale_error.classification,
            ToolResultClassification.OK,
        )
        self.assertEqual(success_with_stale_error.log_level, "debug")
        self.assertFalse(success_with_stale_error.should_journal_failure)

        self.assertEqual(
            expected_miss.classification,
            ToolResultClassification.EXPECTED_MISS,
        )
        self.assertEqual(expected_miss.log_level, "debug")
        self.assertFalse(expected_miss.should_journal_failure)

        self.assertEqual(
            inventory_shape_miss.classification,
            ToolResultClassification.EXPECTED_MISS,
        )
        self.assertEqual(inventory_shape_miss.log_level, "debug")
        self.assertFalse(inventory_shape_miss.should_journal_failure)

        self.assertEqual(
            game_rejected.classification,
            ToolResultClassification.GAME_REJECTED,
        )
        self.assertEqual(game_rejected.log_level, "info")
        self.assertTrue(game_rejected.should_journal_failure)

        self.assertEqual(
            invalid_request.classification,
            ToolResultClassification.INVALID_REQUEST,
        )
        self.assertEqual(invalid_request.log_level, "warning")
        self.assertTrue(invalid_request.should_journal_failure)

    def test_tool_result_log_record_formats_logging_and_journal_decision(self):
        ok = ToolResultLogRecord.from_outcome(
            ToolResultOutcome.from_text("ok scan complete"),
            text="ok scan complete",
        )
        expected_miss = ToolResultLogRecord.from_outcome(
            ToolResultOutcome.from_text("Error: No items of that type in inventory"),
            text="Error: No items of that type in inventory",
        )
        game_rejected = ToolResultLogRecord.from_outcome(
            ToolResultOutcome.from_text(
                '{"success":false,"can_place":false,"entity":"transport-belt",'
                '"error":"Cannot place entity here","position":{"x":56,"y":-25}}'
            ),
            text="Cannot place entity here",
        )
        invalid_request = ToolResultLogRecord.from_outcome(
            ToolResultOutcome.from_text(
                '[{"type":"text","text":"Error: invalid type: map, expected a sequence"}]'
            ),
            text="Error: invalid type: map, expected a sequence",
        )
        huge_dry_run = ToolResultLogRecord.from_outcome(
            ToolResultOutcome.from_text(
                '[{"type":"text","text":"{\\"success\\":false,\\"candidates\\":[]}"}]'
            ),
            text='{"success":false,"candidates":[' + ('{"x":1,"y":2},' * 80) + "]}",
        )

        self.assertEqual(ok.log_level, ToolResultLogLevel.DEBUG)
        self.assertEqual(ok.log_label, "tool_result")
        self.assertTrue(ok.should_emit_log)
        self.assertFalse(ok.should_journal_failure)

        self.assertEqual(expected_miss.log_level, ToolResultLogLevel.DEBUG)
        self.assertEqual(expected_miss.log_label, "tool_result expected_miss")
        self.assertFalse(expected_miss.should_journal_failure)

        self.assertEqual(game_rejected.log_level, ToolResultLogLevel.INFO)
        self.assertEqual(game_rejected.log_label, "tool_result game_rejected")
        self.assertEqual(
            game_rejected.journal_failure_text,
            "game_rejected: Cannot place entity here",
        )

        self.assertEqual(invalid_request.log_level, ToolResultLogLevel.WARNING)
        self.assertEqual(invalid_request.log_label, "tool_result invalid_request")
        self.assertEqual(
            invalid_request.journal_failure_text,
            "invalid_request: Error: invalid type: map, expected a sequence",
        )
        self.assertLessEqual(len(huge_dry_run.text), 300)
        self.assertNotIn("\n", huge_dry_run.text)

    def test_tool_result_outcome_default_text_classifier_handles_bridge_shapes(self):
        cases = {
            "Error: No items of that type in inventory": (
                ToolResultClassification.EXPECTED_MISS,
                ToolResultTextKind.EXPECTED_MISS,
            ),
            "Cannot place entity at target": (
                ToolResultClassification.GAME_REJECTED,
                ToolResultTextKind.GAME_REJECTED,
            ),
            "Error: invalid type: map, expected a sequence": (
                ToolResultClassification.INVALID_REQUEST,
                ToolResultTextKind.INVALID_REQUEST,
            ),
            "Error: expected value at line 1 column 1": (
                ToolResultClassification.INFRASTRUCTURE_FAILURE,
                ToolResultTextKind.INFRASTRUCTURE_FAILURE,
            ),
            (
                "Factorioctl bridge blocked invalid Factorio tool parameters: "
                "tool_input.x: expected number"
            ): (
                ToolResultClassification.OK,
                ToolResultTextKind.OPERATOR_ONLY,
            ),
        }

        for text, (expected, expected_kind) in cases.items():
            with self.subTest(text=text):
                evidence = ToolResultTextEvidence.from_text(text)
                outcome = ToolResultOutcome.from_text(text)
                self.assertEqual(evidence.classification, expected)
                self.assertEqual(evidence.kind, expected_kind)
                self.assertTrue(evidence.reason)
                self.assertEqual(outcome.classification, expected)
                self.assertEqual(outcome.text_evidence.kind, expected_kind)

        classified_failures = {
            '{"error":"cannot place stone furnace"}': (
                ToolResultClassification.GAME_REJECTED
            ),
            '{"success":false,"message":"blocked"}': (
                ToolResultClassification.GAME_REJECTED
            ),
            '[{"type":"text","text":"Error: invalid type: map, expected a sequence"}]': (
                ToolResultClassification.INVALID_REQUEST
            ),
            "Error: entity not found": ToolResultClassification.GAME_REJECTED,
            "Cannot place entity at target": ToolResultClassification.GAME_REJECTED,
            "Could not route belt to target": ToolResultClassification.GAME_REJECTED,
            "not in inventory": ToolResultClassification.GAME_REJECTED,
            "no power": ToolResultClassification.GAME_REJECTED,
            "route failed": ToolResultClassification.GAME_REJECTED,
            (
                '{"success":false,"error":"No labs found! Build a lab first '
                '(requires: 10 iron-gear-wheel, 10 electronic-circuit, 4 transport-belt)",'
                '"action_needed":"build_lab"}'
            ): ToolResultClassification.GAME_REJECTED,
            "Error: missing field `success` at line 1 column 135": (
                ToolResultClassification.INVALID_REQUEST
            ),
            "Error: value for required field 'category' is missing": (
                ToolResultClassification.INVALID_REQUEST
            ),
            "Error: Packet too large: 1553350 bytes": (
                ToolResultClassification.INVALID_REQUEST
            ),
            (
                '{"success":false,"can_place":false,"entity":"stone-furnace",'
                '"error":"Cannot place entity here","inventory_count":1,'
                '"position":{"x":76,"y":-19}}'
            ): ToolResultClassification.GAME_REJECTED,
            (
                '[{"type":"text","text":"Error: expected value at line 1 column 1"}]'
            ): ToolResultClassification.INFRASTRUCTURE_FAILURE,
        }

        non_failure_cases = {
            '{"success":true}': ToolResultClassification.OK,
            '{"success":true,"queued":3,"error":"legacy stale error text"}': (
                ToolResultClassification.OK
            ),
            (
                '{"success":false,"expected_miss":true,'
                '"error":"Cannot place entity here"}'
            ): ToolResultClassification.EXPECTED_MISS,
            '{"success":false,"mined_count":0,"error":null,"inventory":[]}': (
                ToolResultClassification.EXPECTED_MISS
            ),
            (
                '{"success":false,"expected_miss":true,'
                '"blockers":[{"type":"missing_science_pack"}]}'
            ): ToolResultClassification.EXPECTED_MISS,
            '[{"type":"text","text":"Error: No items of that type in inventory"}]': (
                ToolResultClassification.EXPECTED_MISS
            ),
            "Error: No items of that type in inventory": (
                ToolResultClassification.EXPECTED_MISS
            ),
            (
                '[{"type":"text","text":"{\\"success\\":false,'
                '\\"mined_count\\":0,\\"error\\":\\"No minable entity at position\\",'
                '\\"inventory\\":[]}"}]'
            ): ToolResultClassification.EXPECTED_MISS,
            (
                '[{"type":"text","text":"{\\"error\\": '
                '\\"No electric poles found in area\\"}\\n"}]'
            ): ToolResultClassification.EXPECTED_MISS,
            (
                '{"allowed":false,"policy_allowed":true,"factorio_allowed":false,'
                '"entity":"burner-mining-drill","position":{"x":45,"y":-35},'
                '"factorio":{"error":"Factorio cannot place entity here"}}'
            ): ToolResultClassification.OK,
            (
                '[{"type":"text","text":"{\\"success\\":true,'
                '\\"error\\":\\"Cannot place entity here\\"}"}]'
            ): ToolResultClassification.OK,
            (
                "Error: execute_lua is disabled. Raw Lua execution is an "
                "arbitrary-code-execution surface and is off by default."
            ): ToolResultClassification.OK,
            "nominal scan complete": ToolResultClassification.OK,
            (
                '[{"type":"text","text":"{\\"technologies\\":[{\\"ready\\":'
                '\\"blocked\\",\\"blockers\\":[\\"labs have no power\\"]}]}"}]'
            ): ToolResultClassification.OK,
            (
                '[{"type":"text","text":"{\\"researched_count\\":6,'
                '\\"total_count\\":275,\\"research_progress\\":0.36,'
                '\\"research_queue\\":[{\\"name\\":\\"steel-processing\\"}],'
                '\\"labs\\":{\\"count\\":1,\\"powered\\":0,\\"working\\":0},'
                '\\"message\\":\\"Labs have no power! Connect labs to the power grid.\\"}"}]'
            ): ToolResultClassification.OK,
        }

        for text, expected in classified_failures.items():
            with self.subTest(text=text):
                outcome = ToolResultOutcome.from_text(text, sdk_is_error=True)
                self.assertEqual(outcome.classification, expected)
                self.assertTrue(outcome.indicates_failure)

        for text, expected in non_failure_cases.items():
            with self.subTest(text=text):
                outcome = ToolResultOutcome.from_text(text)
                self.assertEqual(outcome.classification, expected)
                self.assertFalse(outcome.indicates_failure)

        text_block = ToolResultOutcome.from_text(
            '[{"type":"text","text":"Error: invalid type: map, expected a sequence"}]'
        )
        stale_success = ToolResultOutcome.from_text(
            '{"success":true,"queued":1,"error":"Cannot place entity here"}',
            sdk_is_error=True,
        )
        research_status = ToolResultOutcome.from_text(
            '[{"type":"text","text":"{\\"researched_count\\":6,'
            '\\"message\\":\\"Labs have no power! Connect labs to the power grid.\\"}"}]'
        )

        self.assertEqual(
            text_block.classification,
            ToolResultClassification.INVALID_REQUEST,
        )
        self.assertEqual(
            text_block.text_evidence.kind,
            ToolResultTextKind.INVALID_REQUEST,
        )
        self.assertEqual(stale_success.classification, ToolResultClassification.OK)
        self.assertEqual(research_status.classification, ToolResultClassification.OK)

    def test_tool_result_content_strips_player_messages_from_sdk_shapes(self):
        raw = ToolResultContent.from_sdk_content(
            "Error: invalid JSON\n\n--- Player Messages ---\n[TestPlayer]: hi"
        )
        text_block = ToolResultContent.from_sdk_content(({
            "type": "text",
            "text": "Error: invalid type\n\n--- Player Messages ---\n[TestPlayer]: oops",
        },))
        nested = ToolResultContent.from_sdk_content({
            "outer": [{
                "type": "text",
                "text": "ok\n\n--- Player Messages ---\n[TestPlayer]: nested",
            }],
        })

        self.assertEqual(raw.text, "Error: invalid JSON")
        self.assertEqual(raw.player_message_text, "[TestPlayer]: hi")
        self.assertEqual(
            text_block.text,
            '[{"type":"text","text":"Error: invalid type"}]',
        )
        self.assertEqual(
            text_block.value,
            [{"type": "text", "text": "Error: invalid type"}],
        )
        self.assertEqual(text_block.player_message_text, "[TestPlayer]: oops")
        self.assertEqual(
            nested.text,
            '{"outer":[{"type":"text","text":"ok"}]}',
        )
        self.assertEqual(
            nested.value,
            {"outer": [{"type": "text", "text": "ok"}]},
        )
        self.assertEqual(nested.player_message_text, "[TestPlayer]: nested")

    def test_player_message_split_typed_model_is_total(self):
        marker = "\n\n--- Player Messages ---\n"
        split = PlayerMessageSplit.from_text(
            f"tool result{marker}[TestPlayer]: hi",
            player_marker=marker,
        )
        plain = PlayerMessageSplit.from_text("tool result", player_marker=marker)
        non_string = PlayerMessageSplit.from_text(42, player_marker=marker)

        self.assertTrue(split.has_player_message)
        self.assertEqual(split.tool_text, "tool result")
        self.assertEqual(split.player_text, "[TestPlayer]: hi")
        self.assertIs(PlayerMessageSplit.from_text(split, player_marker=marker), split)
        self.assertEqual(split.legacy_tuple(), ("tool result", "[TestPlayer]: hi"))
        self.assertFalse(plain.has_player_message)
        self.assertEqual(plain.legacy_tuple(), ("tool result", ""))
        self.assertEqual(non_string.legacy_tuple(), ("42", ""))

    def test_key_value_text_split_owns_colon_protocol_split(self):
        split = KeyValueTextSplit.from_text("Name: value:with colon")
        miss = KeyValueTextSplit.from_text("plain text")

        self.assertTrue(split.matched)
        self.assertEqual(split.key, "name")
        self.assertEqual(split.value, "value:with colon")
        self.assertFalse(miss.matched)

    def test_tool_result_content_preserves_json_string_without_player_messages(self):
        content = '{"success":true,"queued":1}'
        result = ToolResultContent.from_sdk_content(content)

        self.assertEqual(result.text, content)
        self.assertEqual(result.value, {"success": True, "queued": 1})
        self.assertEqual(result.player_messages, [])

    def test_mcp_text_payload_unwraps_raw_and_sdk_text_shapes(self):
        raw = McpTextPayload.from_text('{"success":true,"queued":1}')
        wrapped = McpTextPayload.from_text(json.dumps([{
            "type": "text",
            "text": json.dumps({"success": False, "error": "Cannot place"}),
        }]))
        malformed_wrapped = McpTextPayload.from_text(json.dumps([{
            "type": "text",
            "text": '{"success":false,"error":"truncated',
        }]))
        plain = McpTextPayload.from_text("Cannot place entity here")

        self.assertEqual(raw.value, {"success": True, "queued": 1})
        self.assertEqual(wrapped.value, {"success": False, "error": "Cannot place"})
        self.assertEqual(
            malformed_wrapped.value,
            '{"success":false,"error":"truncated',
        )
        self.assertEqual(plain.value, "Cannot place entity here")
        self.assertEqual(plain.text, "Cannot place entity here")
        self.assertIs(McpTextPayload.from_text(wrapped), wrapped)

    def test_tool_result_payload_collection_normalizes_content_blocks(self):
        blocks = (
            ToolResultPayload(type="text", text='{"success":true}'),
            {"success": False, "expected_miss": True},
        )
        collection = ToolResultPayloadCollection.from_value(blocks)

        self.assertEqual(collection.items, blocks)
        self.assertTrue(collection.has_items)
        self.assertEqual(collection.first_text_block_text, '{"success":true}')
        self.assertEqual(ToolResultPayloadCollection.from_value({"success": True}).items, ())
        self.assertEqual(ToolResultPayloadCollection.from_value("not blocks").items, ())
        self.assertIs(ToolResultPayloadCollection.from_value(collection), collection)

    def test_tool_result_outcome_reports_payload_progress_structurally(self):
        self.assertTrue(ToolResultOutcome.payload_indicates_progress({
            "success": True,
            "queued": 1,
            "error": "legacy stale error text",
        }))
        self.assertFalse(ToolResultOutcome.payload_indicates_progress({
            "success": False,
            "error": "Cannot place entity here",
        }))
        self.assertFalse(ToolResultOutcome.payload_indicates_progress([{
            "type": "text",
            "text": "Cannot place entity here",
        }], text_is_error=lambda text: "cannot place" in text.lower()))
        self.assertFalse(ToolResultOutcome.payload_indicates_progress([{
            "type": "text",
            "text": "Error: No items of that type in inventory",
        }]))
        self.assertFalse(ToolResultOutcome.payload_indicates_progress(({
            "type": "text",
            "text": "Error: No items of that type in inventory",
        },)))

    def test_tool_result_outcome_reports_text_progress_structurally(self):
        self.assertTrue(ToolResultOutcome.text_indicates_progress(
            '{"success":true,"queued":1,"error":"Crafting did not start"}',
        ))
        self.assertFalse(ToolResultOutcome.text_indicates_progress(
            '[{"type":"text","text":"Error: No items of that type in inventory"}]',
            text_is_error=lambda text: "no items" in text.lower(),
        ))
        self.assertFalse(ToolResultOutcome.text_indicates_progress(
            '[{"type":"text","text":"Error: No items of that type in inventory"}]',
        ))
        self.assertTrue(ToolResultOutcome.text_indicates_failure(
            "Error: invalid type: map, expected a sequence",
        ))
        self.assertTrue(ToolResultOutcome.text_indicates_progress("Inserted 5 coal"))
        self.assertFalse(ToolResultOutcome.text_indicates_progress(""))

    def test_watchdog_tool_observation_normalizes_repeated_failure_evidence(self):
        observation = WatchdogToolObservation.from_result(
            tool_use_id=123,
            tool_name="mcp__factorioctl__place_entity",
            classification="game_rejected",
            text=" Error: Cannot place entity here \n",
        )

        self.assertEqual(observation.tool_use_id, "123")
        self.assertEqual(observation.short_tool_name, "place_entity")
        self.assertTrue(observation.is_game_rejected)
        self.assertTrue(observation.is_mutating_tool)
        self.assertEqual(
            observation.failure_signature(),
            "place_entity|game_rejected|Error: Cannot place entity here",
        )
        self.assertEqual(
            WatchdogToolObservation.from_result(
                classification="unknown",
                text="boom",
            ).classification,
            ToolResultClassification.SDK_FAILURE,
        )

    def test_watchdog_tool_observation_reports_progress_structurally(self):
        progress = WatchdogToolObservation.from_result(
            tool_name="mcp__factorioctl__insert_items",
            classification=ToolResultClassification.OK,
            text="Inserted 5 coal into entity",
        )
        forced_no_progress = WatchdogToolObservation.from_result(
            tool_name="mcp__factorioctl__insert_items",
            classification=ToolResultClassification.OK,
            text="Inserted 5 coal into entity",
            indicates_progress=False,
        )

        self.assertTrue(progress.is_ok)
        self.assertTrue(progress.is_mutating_tool)
        self.assertTrue(progress.indicates_mutating_progress(
            text_is_error=lambda _text: False,
        ))
        self.assertFalse(forced_no_progress.indicates_mutating_progress(
            text_is_error=lambda _text: False,
        ))

    def test_tool_result_content_reports_outcome_and_progress_from_value(self):
        content = ToolResultContent.from_sdk_content({
            "success": True,
            "queued": 1,
            "error": "legacy stale error text",
        })
        rejected = ToolResultContent.from_sdk_content([{
            "type": "text",
            "text": json.dumps({
                "success": False,
                "can_place": False,
                "entity": "transport-belt",
                "position": {"x": 1, "y": 2},
                "error": "Cannot place entity here",
            }),
        }])

        self.assertEqual(content.outcome().classification, ToolResultClassification.OK)
        self.assertTrue(content.indicates_progress())
        self.assertEqual(
            rejected.outcome().classification,
            ToolResultClassification.GAME_REJECTED,
        )
        self.assertFalse(rejected.indicates_progress(
            text_is_error=lambda text: "cannot place" in text.lower(),
        ))

    def test_sdk_tool_result_event_observation_groups_runtime_decisions(self):
        event = SdkToolResultEvent.from_sdk_block(type("ToolResult", (), {
            "tool_use_id": "tool-1",
            "content": [{
                "type": "text",
                "text": (
                    "Inserted 5 coal into entity"
                    "\n\n--- Player Messages ---\n"
                    "[Player]: hi"
                ),
            }],
            "is_error": False,
        })())

        observation = event.observation()

        self.assertIsInstance(observation, SdkToolResultObservation)
        self.assertEqual(observation.tool_use_id, "tool-1")
        self.assertEqual(
            observation.text,
            '[{"type":"text","text":"Inserted 5 coal into entity"}]',
        )
        self.assertEqual(observation.player_message_text, "[Player]: hi")
        self.assertEqual(observation.classification, ToolResultClassification.OK)
        self.assertEqual(observation.log_record.log_label, "tool_result")
        self.assertTrue(observation.indicates_progress)
