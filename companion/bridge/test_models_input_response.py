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



class ModelInputResponseTests(unittest.TestCase):

    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.base = Path(self.tempdir.name)

    def test_agent_response_format_filters_bad_sections_and_preserves_shape(self):
        sections = (
            {"label": "INVENTORY", "color": "", "description": "current stock"},
            {"color": "1,0,0", "description": "missing label"},
            "bad section",
        )
        response_format = AgentResponseFormat.coerce({
            "header_label": "",
            "sections": sections,
            "future_field": "kept",
        })

        self.assertIsNotNone(response_format)
        self.assertEqual(response_format.header_label, "STATUS")
        self.assertEqual(len(response_format.sections), 1)
        self.assertEqual(
            [section.label for section in ResponseFormatSectionCollection.from_value(sections).items],
            ["INVENTORY"],
        )
        self.assertEqual(response_format.to_dict(), {
            "future_field": "kept",
            "header_label": "STATUS",
            "header_color": "1,0.8,0.2",
            "action_label": "ACTIONS",
            "action_color": "0.6,0.8,1",
            "footer_color": "0.4,0.6,0.4",
            "sections": [{
                "label": "INVENTORY",
                "color": "0.5,0.7,0.5",
                "description": "current stock",
            }],
        })

    def test_bridge_input_message_coerces_ingress_shape_and_preserves_extras(self):
        message = BridgeInputMessage.from_mapping({
            "message": 42,
            "player_index": "2",
            "player_name": "",
            "target_agent": "",
            "autonomy": "yes",
            "read_only_tools": "true",
            "response_to": "all",
            "model": "haiku",
            "future_field": "kept",
        })

        self.assertIsNotNone(message)
        self.assertEqual(message.message, "42")
        self.assertEqual(message.player_index, 2)
        self.assertEqual(message.player_name, "Player")
        self.assertEqual(message.target_agent, "default")
        self.assertTrue(message.autonomy)
        self.assertTrue(message.read_only_tools)
        self.assertEqual(message.response_to, "all")
        self.assertEqual(message.model, "haiku")
        self.assertEqual(message.to_dict()["future_field"], "kept")

    def test_bridge_input_message_preserves_typed_ingress_message(self):
        typed = BridgeInputMessage(
            message="go",
            player_index=2,
            player_name="TestPlayer",
            target_agent="doug-nauvis",
        )

        self.assertIs(BridgeInputMessage.from_mapping(typed), typed)
        self.assertIsNone(BridgeInputMessage.from_mapping(typed.model_copy(update={"message": ""})))

    def test_bridge_input_message_drops_empty_or_invalid_messages(self):
        self.assertIsNone(BridgeInputMessage.from_mapping({"message": ""}))
        self.assertIsNone(BridgeInputMessage.from_mapping({"message": None}))
        self.assertIsNone(BridgeInputMessage.from_mapping(["not", "a", "mapping"]))

    def test_bridge_input_message_parses_jsonl_line_at_model_boundary(self):
        message = BridgeInputMessage.from_json_line(json.dumps({
            "message": 42,
            "player_index": "3",
            "future_field": "kept",
        }))

        self.assertIsNotNone(message)
        self.assertEqual(message.to_dict(), {
            "future_field": "kept",
            "message": "42",
            "player_index": 3,
            "player_name": "Player",
            "target_agent": "default",
        })
        self.assertIsNone(BridgeInputMessage.from_json_line("{not-json"))
        self.assertIsNone(BridgeInputMessage.from_json_line('["not", "a", "mapping"]'))
        self.assertIsNone(BridgeInputMessage.from_json_line('{"message": ""}'))
        self.assertIsNone(BridgeInputMessage.from_json_line(None))
        self.assertIs(BridgeInputMessage.from_json_line(message), message)

    def test_bridge_input_batch_parses_jsonl_text_at_model_boundary(self):
        batch = BridgeInputBatch.from_jsonl_text("\n".join([
            "{not-json",
            json.dumps(["not", "a", "mapping"]),
            json.dumps({"message": ""}),
            json.dumps({
                "message": 42,
                "player_index": "3",
                "future_field": "kept",
            }),
            "",
            json.dumps({"message": "go", "target_agent": "doug"}),
        ]))

        self.assertEqual(batch.to_dicts(), [
            {
                "future_field": "kept",
                "message": "42",
                "player_index": 3,
                "player_name": "Player",
                "target_agent": "default",
            },
            {
                "message": "go",
                "player_index": 1,
                "player_name": "Player",
                "target_agent": "doug",
            },
        ])
        self.assertEqual(BridgeInputBatch.from_jsonl_text(None).to_dicts(), [])

    def test_bridge_input_batch_preserves_typed_batch_and_message_list(self):
        typed = BridgeInputMessage(
            message="go",
            player_name="TestPlayer",
            target_agent="doug",
        )
        batch = BridgeInputBatch(messages=[typed])
        mixed_items = (
            typed,
            {"message": "raw", "future_field": "kept"},
            {"message": ""},
            ["bad"],
        )
        mixed = BridgeInputBatch.from_jsonl_text(mixed_items)

        self.assertIs(BridgeInputBatch.from_jsonl_text(batch), batch)
        self.assertEqual(len(BridgeInputMessageCollection.from_value(mixed_items).items), 2)
        self.assertEqual(mixed.messages[0], typed)
        self.assertEqual(mixed.messages[1].message, "raw")
        self.assertEqual(mixed.messages[1].to_dict()["future_field"], "kept")
        self.assertEqual(len(mixed.messages), 2)

    def test_bridge_input_file_delta_models_cursor_and_parsed_batch(self):
        delta = BridgeInputFileDelta.from_chunk(
            previous_size="5",
            current_size="30",
            text=json.dumps({"message": "go", "target_agent": "doug"}) + "\n",
        )
        empty = BridgeInputFileDelta.empty(previous_size=30, current_size=20)

        self.assertTrue(delta.advanced)
        self.assertEqual(delta.next_size, 30)
        self.assertEqual(delta.messages[0].message, "go")
        self.assertEqual(delta.to_dicts()[0]["target_agent"], "doug")
        self.assertFalse(empty.advanced)
        self.assertEqual(empty.next_size, 30)
        self.assertEqual(empty.messages, [])

    def test_bridge_input_message_falls_back_on_bad_optional_fields(self):
        message = BridgeInputMessage.from_mapping({
            "message": "go",
            "player_index": {"bad": True},
            "autonomy": {"bad": True},
            "read_only_tools": {"bad": True},
        })

        self.assertIsNotNone(message)
        self.assertEqual(message.player_index, 1)
        self.assertFalse(message.autonomy)
        self.assertFalse(message.read_only_tools)

    def test_autonomy_tick_message_omits_planner_only_fields_for_execution(self):
        tick = AutonomyTickMessage.create(
            "Do the next step",
            read_only_tools=False,
            model="strong-planner",
        )

        self.assertEqual(
            tick.to_dict(),
            {
                "message": "Do the next step",
                "player_index": 0,
                "player_name": "autonomy",
                "autonomy": True,
            },
        )
        self.assertEqual(tick["message"], "Do the next step")
        self.assertNotIn("read_only_tools", tick)
        self.assertIsNone(tick.get("read_only_tools"))

    def test_autonomy_tick_message_keeps_read_only_planner_model(self):
        tick = AutonomyTickMessage.create(
            "Plan next",
            read_only_tools=True,
            model="strong-planner",
        )

        self.assertTrue(tick.to_dict()["read_only_tools"])
        self.assertEqual(tick.to_dict()["model"], "strong-planner")
        self.assertTrue(tick["read_only_tools"])
        self.assertEqual(tick.get("model"), "strong-planner")
        bridge_input = tick.to_bridge_input()
        self.assertEqual(bridge_input.message, "Plan next")
        self.assertEqual(bridge_input.player_index, 0)
        self.assertEqual(bridge_input.player_name, "autonomy")
        self.assertTrue(bridge_input.autonomy)
        self.assertTrue(bridge_input.read_only_tools)
        self.assertEqual(bridge_input.model, "strong-planner")

        with self.assertRaises(ValueError):
            AutonomyTickMessage.create("")

    def test_load_agent_uses_typed_validation_before_runtime(self):
        import pipe

        agents_dir = self.base / "agents"
        agents_dir.mkdir()
        (agents_dir / "bad.json").write_text(json.dumps({
            "name": "bad",
            "system_prompt": "Build.",
            "max_turns": {"oops": True},
        }))

        with mock.patch("pipe._BRIDGE_DIR", self.base):
            with self.assertRaisesRegex(BridgeValidationError, "max_turns: expected integer"):
                pipe.load_agent("bad")

    def test_load_agent_returns_profile_with_format_instructions(self):
        import pipe

        agents_dir = self.base / "agents"
        agents_dir.mkdir()
        (agents_dir / "doug.json").write_text(json.dumps({
            "name": "doug",
            "system_prompt": "Build.",
            "response_format": {"header_label": "STATUS"},
        }))

        with mock.patch("pipe._BRIDGE_DIR", self.base):
            profile = pipe.load_agent("doug")

        self.assertIsInstance(profile, AgentProfile)
        self.assertEqual(profile.name, "doug")
        self.assertIn("[color=1,0.8,0.2]STATUS:[/color]", profile.system_prompt)

    def test_build_format_instructions_uses_typed_response_format(self):
        import pipe

        instructions = pipe.build_format_instructions({
            "header_label": "CLASSIFICATION",
            "sections": [
                {"label": "ANOMALY", "description": "current anomaly"},
                {"description": "missing label"},
            ],
        })

        self.assertIn("[color=1,0.8,0.2]CLASSIFICATION:[/color]", instructions)
        self.assertIn("[color=0.5,0.7,0.5]ANOMALY:[/color] <current anomaly>", instructions)
        self.assertNotIn("missing label", instructions)

        typed = AgentResponseFormat(
            header_label="REPORT",
            sections=[
                ResponseFormatSection(
                    label="ROOT CAUSE",
                    color="1,0.4,0.4",
                    description="why it happened",
                ),
            ],
        )
        typed_instructions = pipe.build_format_instructions(typed)

        self.assertIn("[color=1,0.8,0.2]REPORT:[/color]", typed_instructions)
        self.assertIn(
            "[color=1,0.4,0.4]ROOT CAUSE:[/color] <why it happened>",
            typed_instructions,
        )

    def test_parsed_agent_response_coerces_nested_sections(self):
        typed = ParsedAgentResponse(
            header=ParsedResponseSection(label="STATUS", color="1,1,1", text="ok"),
            data={
                "ANOMALY": ParsedResponseSection(
                    label="ANOMALY",
                    color="1,0,0",
                    text="belt blocked",
                ),
            },
        )
        response = ParsedAgentResponse.from_mapping({
            "header": {"label": "STATUS", "color": "1,1,1", "text": 42},
            "body": ["body"],
            "actions": ["one", None, "two"],
            "footer": "bad footer",
            "data": {
                "ANOMALY": {"color": "1,0,0", "text": "belt blocked"},
                "bad": "ignored",
            },
        })

        self.assertEqual(response.header.text, "42")
        self.assertEqual(response.body, "['body']")
        self.assertEqual(response.actions, ["one", "two"])
        self.assertIsNone(response.footer)
        self.assertEqual(response.anomaly_text(), "belt blocked")
        self.assertEqual(response.to_dict()["data"]["ANOMALY"]["label"], "ANOMALY")
        self.assertIs(ParsedAgentResponse.from_mapping(typed), typed)
        self.assertEqual(typed.anomaly_text(), "belt blocked")

    def test_parsed_agent_response_parses_rich_text_sections(self):
        response = ParsedAgentResponse.from_text(
            "[color=1,0.6,0.2]CLASSIFICATION:[/color] Done\n\n"
            "Body text.\n\n"
            "[color=0.6,0.8,1]ACTIONS TAKEN:[/color]\n"
            "- placed belt\n"
            "- verified furnace\n\n"
            "[color=1,0.4,0.4]ANOMALY:[/color] belt blocked\n\n"
            "[color=0.4,0.6,0.4]FILED:[/color] complete"
        )

        self.assertEqual(response.header.label, "CLASSIFICATION")
        self.assertEqual(response.body, "Body text.")
        self.assertEqual(response.actions, ["placed belt", "verified furnace"])
        self.assertEqual(response.anomaly_text(), "belt blocked")
        self.assertEqual(response.footer.text, "complete")
        self.assertEqual(ParsedAgentResponse.from_text("").to_dict(), {"body": ""})

    def test_parsed_agent_response_filters_nominal_anomaly_text(self):
        response = ParsedAgentResponse.from_text(
            "[color=1,0.6,0.2]CLASSIFICATION:[/color] Done\n\n"
            "[color=1,0.4,0.4]ANOMALY:[/color] route crosses water"
        )
        nominal = ParsedAgentResponse.from_text(
            "[color=1,0.6,0.2]CLASSIFICATION:[/color] Done\n\n"
            "[color=1,0.4,0.4]ANOMALY:[/color] no anomalies found"
        )

        self.assertTrue(ParsedAgentResponse.is_meaningful_anomaly_text("route blocked"))
        self.assertFalse(ParsedAgentResponse.is_meaningful_anomaly_text("None"))
        self.assertFalse(ParsedAgentResponse.is_meaningful_anomaly_text("nominal"))
        self.assertFalse(ParsedAgentResponse.is_meaningful_anomaly_text("no anomalies found"))
        self.assertEqual(
            response.anomaly_evidence().kind,
            AnomalyEvidenceKind.MEANINGFUL,
        )
        self.assertEqual(
            response.anomaly_evidence().normalized_text,
            "route crosses water",
        )
        self.assertEqual(nominal.anomaly_evidence().kind, AnomalyEvidenceKind.NOMINAL)
        self.assertEqual(response.meaningful_anomaly_text(), "route crosses water")
        self.assertEqual(nominal.meaningful_anomaly_text(), "")

    def test_telemetry_chat_serializes_parsed_agent_response_sections(self):
        response = ParsedAgentResponse.from_text(
            "[color=1,0.6,0.2]CLASSIFICATION:[/color] Done\n\n"
            "Body text.\n\n"
            "[color=0.6,0.8,1]ACTIONS TAKEN:[/color]\n"
            "- placed belt\n\n"
            "[color=1,0.4,0.4]ANOMALY:[/color] route crosses water"
        )

        event = TelemetryEvent.chat("agent", "raw reply", sections=response)

        sections = event.to_dict()["data"]["sections"]
        self.assertEqual(sections["header"]["label"], "CLASSIFICATION")
        self.assertEqual(sections["body"], "Body text.")
        self.assertEqual(sections["actions"], ["placed belt"])
        self.assertEqual(sections["data"]["ANOMALY"]["text"], "route crosses water")

    def test_telemetry_chat_normalizes_mixed_section_payloads(self):
        class Weird:
            def __repr__(self):
                return "<Weird>"

        class SectionLike:
            def to_dict(self):
                return {"label": "SECTION", "items": {("bad", "key"): Weird()}}

        event = TelemetryEvent.chat(
            "agent",
            "raw reply",
            sections={
                "custom": SectionLike(),
                "items": {1, "two"},
                "plain": Weird(),
            },
        )

        payload = event.to_dict()
        json_text = event.to_json_text()
        sections = payload["data"]["sections"]

        self.assertEqual(sections["custom"]["label"], "SECTION")
        self.assertEqual(sections["custom"]["items"]["('bad', 'key')"], "<Weird>")
        self.assertCountEqual(sections["items"], [1, "two"])
        self.assertEqual(sections["plain"], "<Weird>")
        self.assertIn('"sections"', json_text)

    def test_telemetry_serializable_value_normalizes_nested_objects(self):
        class Weird:
            def __repr__(self):
                return "<Weird>"

        self.assertEqual(
            TelemetrySerializableValue.normalize({"bad": [Weird()]}),
            {"bad": ["<Weird>"]},
        )

    def test_parsed_agent_response_sanitizes_markdown_artifacts(self):
        self.assertEqual(
            ParsedAgentResponse.sanitize_text(
                "## Heading\n"
                "[color=1,0.6,0.2]CLASSIFICATION:[/color] **Done**\n"
                "```text\n"
                "body"
            ),
            "Heading\n"
            "[color=1,0.6,0.2]CLASSIFICATION:[/color] Done\n"
            "body",
        )
