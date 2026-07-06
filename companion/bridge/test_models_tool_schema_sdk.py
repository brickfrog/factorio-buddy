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



class ModelToolSchemaSdkTests(unittest.TestCase):

    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.base = Path(self.tempdir.name)

    def test_tool_call_request_validates_field_specific_parameter_shapes(self):
        request = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__place_entity",
            "tool_input": {
                "entity_name": "stone-furnace",
                "x": {"bad": True},
                "y": 4,
            },
        })

        with self.assertRaisesRegex(BridgeValidationError, "tool_input.x: expected number"):
            request.validate_params(
                required={
                    "entity_name": TOOL_PARAM_STRING,
                    "x": TOOL_PARAM_NUMBER,
                    "y": TOOL_PARAM_NUMBER,
                },
            )

        with self.assertRaisesRegex(BridgeValidationError, "tool_input.count: missing required field"):
            ToolCallRequest.from_hook_input({
                "tool_name": "mcp__factorioctl__craft",
                "tool_input": {"recipe": "pipe"},
            }).validate_params(
                required={
                    "recipe": TOOL_PARAM_STRING,
                    "count": TOOL_PARAM_INTEGER,
                },
            )

        with self.assertRaisesRegex(BridgeValidationError, "tool_input: expected object"):
            ToolCallRequest.from_hook_input({
                "tool_name": "mcp__factorioctl__place_entity",
                "tool_input": ["not", "a", "mapping"],
            })

    def test_tool_call_request_preserves_typed_hook_request(self):
        request = ToolCallRequest(
            tool_name="mcp__factorioctl__place_entity",
            tool_input={"entity_name": "stone-furnace", "x": 1, "y": 2},
        )

        self.assertIs(ToolCallRequest.from_hook_input(request), request)

    def test_tool_call_request_model_validates_direct_construction(self):
        request = ToolCallRequest(
            tool_name=" mcp__factorioctl__situation_report ",
            tool_input=None,
        )

        self.assertEqual(request.tool_name, "mcp__factorioctl__situation_report")
        self.assertEqual(request.tool_input, {})
        with self.assertRaisesRegex(ValueError, "tool_name"):
            ToolCallRequest(tool_name="", tool_input={})
        with self.assertRaisesRegex(ValueError, "tool_input"):
            ToolCallRequest(tool_name="mcp__factorioctl__mine_at", tool_input=[])

    def test_tool_call_request_exposes_factorio_tool_policy(self):
        placement = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__place_entity",
            "tool_input": {"entity_name": "transport-belt", "x": 1, "y": 2},
        })
        fuel_build = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__build_fuel_supply",
        })
        lab_feed_build = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__build_lab_feed",
        })
        assembler_feed_build = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__build_assembler_feed",
        })
        assembler_output_build = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__build_assembler_output",
        })
        automation_science_build = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__build_automation_science",
        })
        recipe_cell_build = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__build_recipe_assembler_cell",
        })
        edge_miner_build = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__execute_edge_miner",
        })
        placement_build = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__execute_entity_placement_near",
        })
        smelter_build = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__execute_direct_smelter",
        })
        scan = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__situation_report",
        })
        diagnostic = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__diagnose_factory_blockers",
        })
        fuel_diagnostic = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__diagnose_fuel_sustainability",
        })
        automation_science_plan = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__plan_automation_science",
        })
        recipe_cell_plan = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__plan_recipe_assembler_cell",
        })
        item_flow = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__analyze_item_flow",
        })
        dry_run_feed = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__feed_lab_from_inventory",
            "tool_input": {"dry_run": True},
        })
        dry_run_automation_science = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__build_automation_science",
            "tool_input": {"dry_run": True},
        })
        dry_run_recipe_cell = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__build_recipe_assembler_cell",
            "tool_input": {"dry_run": True},
        })
        dry_run_edge_miner = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__execute_edge_miner",
            "tool_input": {"dry_run": True},
        })
        dry_run_placement = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__execute_entity_placement_near",
            "tool_input": {"dry_run": True},
        })
        active_automation_science = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__build_automation_science",
            "tool_input": {"dry_run": False},
        })
        active_recipe_cell = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__build_recipe_assembler_cell",
            "tool_input": {"dry_run": False},
        })
        active_edge_miner = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__execute_edge_miner",
            "tool_input": {"dry_run": False},
        })
        active_placement = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__execute_entity_placement_near",
            "tool_input": {"dry_run": False},
        })
        active_feed = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__feed_lab_from_inventory",
            "tool_input": {"dry_run": False},
        })
        hand_feed = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__hand_feed_furnace",
        })
        non_factorio = ToolCallRequest.from_hook_input({"tool_name": "Skill"})

        self.assertEqual(placement.short_name, "place_entity")
        self.assertTrue(placement.is_factorio_mcp_tool)
        self.assertTrue(placement.is_mutating_factorio_tool)
        self.assertFalse(placement.is_read_only_factorio_tool)
        self.assertTrue(fuel_build.is_mutating_factorio_tool)
        self.assertFalse(fuel_build.is_read_only_factorio_tool)
        self.assertTrue(lab_feed_build.is_mutating_factorio_tool)
        self.assertFalse(lab_feed_build.is_read_only_factorio_tool)
        self.assertTrue(assembler_feed_build.is_mutating_factorio_tool)
        self.assertFalse(assembler_feed_build.is_read_only_factorio_tool)
        self.assertTrue(assembler_output_build.is_mutating_factorio_tool)
        self.assertFalse(assembler_output_build.is_read_only_factorio_tool)
        self.assertTrue(automation_science_build.is_mutating_factorio_tool)
        self.assertFalse(automation_science_build.is_read_only_factorio_tool)
        self.assertTrue(recipe_cell_build.is_mutating_factorio_tool)
        self.assertFalse(recipe_cell_build.is_read_only_factorio_tool)
        self.assertTrue(edge_miner_build.is_mutating_factorio_tool)
        self.assertFalse(edge_miner_build.is_read_only_factorio_tool)
        self.assertTrue(placement_build.is_mutating_factorio_tool)
        self.assertFalse(placement_build.is_read_only_factorio_tool)
        self.assertTrue(smelter_build.is_mutating_factorio_tool)
        self.assertFalse(smelter_build.is_read_only_factorio_tool)
        self.assertTrue(scan.is_read_only_factorio_tool)
        self.assertFalse(scan.is_mutating_factorio_tool)
        self.assertTrue(diagnostic.is_read_only_factorio_tool)
        self.assertFalse(diagnostic.is_mutating_factorio_tool)
        self.assertTrue(fuel_diagnostic.is_read_only_factorio_tool)
        self.assertFalse(fuel_diagnostic.is_mutating_factorio_tool)
        self.assertTrue(automation_science_plan.is_read_only_factorio_tool)
        self.assertFalse(automation_science_plan.is_mutating_factorio_tool)
        self.assertTrue(recipe_cell_plan.is_read_only_factorio_tool)
        self.assertFalse(recipe_cell_plan.is_mutating_factorio_tool)
        self.assertTrue(item_flow.is_read_only_factorio_tool)
        self.assertFalse(item_flow.is_mutating_factorio_tool)
        self.assertTrue(dry_run_feed.is_read_only_dry_run)
        self.assertTrue(dry_run_automation_science.is_read_only_dry_run)
        self.assertTrue(dry_run_recipe_cell.is_read_only_dry_run)
        self.assertTrue(dry_run_edge_miner.is_read_only_dry_run)
        self.assertTrue(dry_run_placement.is_read_only_dry_run)
        self.assertFalse(active_automation_science.is_read_only_dry_run)
        self.assertFalse(active_recipe_cell.is_read_only_dry_run)
        self.assertFalse(active_edge_miner.is_read_only_dry_run)
        self.assertFalse(active_placement.is_read_only_dry_run)
        self.assertFalse(active_feed.is_read_only_dry_run)
        self.assertTrue(hand_feed.is_mutating_factorio_tool)
        self.assertFalse(hand_feed.is_read_only_factorio_tool)
        self.assertFalse(non_factorio.is_factorio_mcp_tool)
        self.assertEqual(non_factorio.short_name, "Skill")

    def test_sdk_assistant_message_preserves_ordered_content_events(self):
        class Message:
            def __init__(self, content, session_id=None):
                self.content = content
                self.session_id = session_id

        class Text:
            def __init__(self, text):
                self.text = text

        class Tool:
            def __init__(self, tool_id, name, tool_input):
                self.id = tool_id
                self.name = name
                self.input = tool_input

        class Thinking:
            def __init__(self, thinking):
                self.thinking = thinking

        content = (
                Text("hello"),
                Tool("tool-1", "mcp__factorioctl__walk_to", {"x": 1, "y": 2}),
                Thinking("considering"),
        )
        message = SdkAssistantMessage.from_sdk_message(
            Message(content, session_id="session-1"),
            text_block_type=Text,
            tool_use_block_type=Tool,
            thinking_block_type=Thinking,
        )

        self.assertEqual(message.session_id, "session-1")
        self.assertEqual([event.kind.value for event in message.events], [
            "text",
            "tool_use",
            "thinking",
        ])
        self.assertEqual(message.events[0].text, "hello")
        self.assertEqual(message.events[1].tool_use_id, "tool-1")
        self.assertEqual(message.events[1].tool_use.display_name, "walk_to")
        self.assertEqual(message.events[2].text, "considering")
        self.assertEqual(
            SdkAssistantMessage.from_sdk_message(Message("not-list")).events,
            [],
        )
        self.assertEqual(SdkContentBlocks.from_value(content).blocks, content)
        self.assertEqual(SdkContentBlocks.from_value("not blocks").blocks, ())
        self.assertIs(SdkAssistantMessage.from_sdk_message(message), message)

    def test_sdk_assistant_text_observation_detects_usage_limit(self):
        normal = SdkAssistantTextObservation.from_event(
            SdkAssistantMessage.from_sdk_message(type("Message", (), {
                "content": [type("Text", (), {"text": "ordinary progress"})()],
                "session_id": None,
            })()).events[0],
        )
        provider_now = datetime.now(timezone.utc) + timedelta(hours=8)
        provider_reset = provider_now + timedelta(hours=1)
        limit_text = (
            "API Error: Request rejected (429) · [1308][Usage limit reached "
            f"for 5 hour. Your limit will reset at {provider_reset:%Y-%m-%d %H:%M:%S}]"
            f"[{provider_now:%Y%m%d%H%M%S}abcdef]"
        )
        limit = SdkAssistantTextObservation.from_event(
            SdkAssistantMessage.from_sdk_message(type("Message", (), {
                "content": [type("Text", (), {"text": limit_text})()],
                "session_id": None,
            })()).events[0],
        )

        self.assertEqual(normal.text, "ordinary progress")
        self.assertFalse(normal.usage_limit_seen)
        self.assertTrue(normal.counts_as_watchdog_progress)
        self.assertTrue(limit.usage_limit_seen)
        self.assertFalse(limit.counts_as_watchdog_progress)

    def test_sdk_system_message_exposes_init_logging_policy(self):
        class Message:
            def __init__(self, subtype, data):
                self.subtype = subtype
                self.data = data

        init = SdkSystemMessage.from_sdk_message(Message(
            "init",
            {
                "cwd": "/tmp/factorioctl",
                "tools": ("Skill", "mcp__factorioctl__walk_to"),
                "skills": tuple(f"skill-{index}" for index in range(14)),
            },
        ))
        malformed_init = SdkSystemMessage.from_sdk_message(Message("init", []))
        thinking = SdkSystemMessage.from_sdk_message(Message(
            "thinking_tokens",
            {"estimated_tokens": 12},
        ))

        self.assertTrue(init.is_loggable_init)
        self.assertEqual(init.cwd, "/tmp/factorioctl")
        self.assertTrue(init.has_skill_tool)
        self.assertEqual(init.skill_tool_label, "yes")
        self.assertEqual(init.bounded_visible_skills()[-1], "...+2")
        self.assertEqual(
            SdkMetadataItems.from_value(("Skill", "tool")).bounded_strings(limit=1),
            ["Skill", "...+1"],
        )
        self.assertFalse(malformed_init.is_loggable_init)
        self.assertEqual(malformed_init.bounded_visible_skills(), [])
        self.assertFalse(thinking.should_log)
        self.assertTrue(SdkSystemMessage.from_sdk_message(
            Message("error", {"message": "visible diagnostic"})
        ).should_log)
        self.assertIs(SdkSystemMessage.from_sdk_message(init), init)

    def test_sdk_tool_use_exposes_stream_policy(self):
        class Block:
            def __init__(self, name, tool_input):
                self.name = name
                self.input = tool_input

        factorio_tool = SdkToolUse.from_sdk_block(Block(
            "mcp__factorioctl__place_entity",
            {"entity_name": "transport-belt"},
        ))
        external_mcp_tool = SdkToolUse.from_sdk_block(Block(
            "mcp__github__search",
            {"query": "factorioctl"},
        ))
        direct_skill = SdkToolUse.from_sdk_block(Block(
            "Skill",
            {"skill": "factorio-control"},
        ))
        namespaced_skill = SdkToolUse.from_sdk_block(Block(
            "mcp__codex__Skill",
            {"skill": "debug"},
        ))
        thought = SdkToolUse.from_sdk_block(Block(
            "mcp__factorioctl__broadcast_thought",
            {"message": "belt turn found"},
        ))
        odd_input = SdkToolUse.from_sdk_block(Block(
            "mcp__factorioctl__broadcast_thought",
            ["not", "a", "mapping"],
        ))

        self.assertEqual(factorio_tool.display_name, "place_entity")
        self.assertTrue(factorio_tool.should_send_tool_status)
        self.assertEqual(
            factorio_tool.log_input_text,
            '{"entity_name":"transport-belt"}',
        )
        self.assertEqual(external_mcp_tool.display_name, "mcp__github__search")
        self.assertFalse(external_mcp_tool.should_send_tool_status)
        self.assertTrue(direct_skill.is_skill_tool)
        self.assertTrue(direct_skill.should_send_tool_status)
        self.assertTrue(namespaced_skill.is_skill_tool)
        self.assertFalse(namespaced_skill.should_send_tool_status)
        self.assertTrue(thought.is_broadcast_thought)
        self.assertEqual(thought.thought_message, "belt turn found")
        self.assertEqual(odd_input.input_mapping, {})
        self.assertEqual(odd_input.thought_message, "")
        self.assertEqual(odd_input.log_input_text, '["not","a","mapping"]')
        cyclic_input = []
        cyclic_input.append(cyclic_input)
        self.assertEqual(SdkToolUse.json_for_log(cyclic_input), "[[...]]")
        self.assertIs(SdkToolUse.from_sdk_block(factorio_tool), factorio_tool)

    def test_sdk_user_tool_result_message_normalizes_string_and_block_content(self):
        class Message:
            def __init__(self, content):
                self.content = content

        class ToolResult:
            def __init__(self, tool_use_id, content, is_error=False):
                self.tool_use_id = tool_use_id
                self.content = content
                self.is_error = is_error

        class TextBlock:
            content = "ignored"

        string_message = SdkUserToolResultMessage.from_sdk_message(Message(
            "Error: invalid type\n\n--- Player Messages ---\n[TestPlayer]: hi"
        ))
        block_message = SdkUserToolResultMessage.from_sdk_message(
            Message((
                TextBlock(),
                ToolResult("tool-1", [{"type": "text", "text": "boom"}], True),
            )),
            tool_result_block_type=ToolResult,
        )
        odd_message = SdkUserToolResultMessage.from_sdk_message(Message({"bad": True}))

        self.assertEqual(len(string_message.results), 1)
        self.assertEqual(string_message.results[0].text, "Error: invalid type")
        self.assertEqual(string_message.results[0].player_message_text, "[TestPlayer]: hi")
        self.assertFalse(string_message.results[0].is_error)
        self.assertEqual(len(block_message.results), 1)
        self.assertEqual(block_message.results[0].tool_use_id, "tool-1")
        self.assertTrue(block_message.results[0].is_error)
        self.assertIn("boom", block_message.results[0].text)
        self.assertEqual(
            block_message.results[0].outcome.classification,
            ToolResultClassification.SDK_FAILURE,
        )
        self.assertFalse(block_message.results[0].indicates_progress(
            text_is_error=lambda text: "boom" in text,
        ))
        self.assertEqual(odd_message.results, [])
        self.assertIs(
            SdkToolResultEvent.from_sdk_block(block_message.results[0]),
            block_message.results[0],
        )
        self.assertIs(
            SdkToolResultEvent.from_string(string_message.results[0]),
            string_message.results[0],
        )
        self.assertIs(
            SdkUserToolResultMessage.from_sdk_message(block_message),
            block_message,
        )

    def test_sdk_result_message_exposes_error_and_cost_policy(self):
        class Result:
            def __init__(
                self,
                *,
                session_id=None,
                result=None,
                errors=None,
                is_error=False,
                total_cost_usd=None,
                num_turns=None,
                duration_ms=None,
            ):
                self.session_id = session_id
                self.result = result
                self.errors = errors or []
                self.is_error = is_error
                self.total_cost_usd = total_cost_usd
                self.num_turns = num_turns
                self.duration_ms = duration_ms

        normal = SdkResultMessage.from_sdk_message(Result(
            session_id="sess-1",
            result="done",
            total_cost_usd="1.25",
            num_turns="3",
            duration_ms=2500,
        ))
        errors = SdkResultMessage.from_sdk_message(Result(
            errors=["first", "second"],
            is_error=True,
        ))
        fallback = SdkResultMessage.from_sdk_message(Result(is_error=True))
        context_window = SdkResultMessage.from_sdk_message(Result(
            result="API Error: The model has reached its context window limit.",
            is_error=True,
        ))

        self.assertEqual(normal.session_id, "sess-1")
        self.assertTrue(normal.has_result_text)
        self.assertEqual(normal.result_text, "done")
        self.assertTrue(normal.has_cost)
        self.assertEqual(normal.duration_s, 2.5)
        self.assertEqual(normal.compute_cost_payload, {
            "cost_usd": 1.25,
            "turns": 3,
            "duration_ms": 2500.0,
        })
        self.assertEqual(errors.error_detail, "first; second")
        self.assertEqual(fallback.error_detail, "agent result marked as error")
        self.assertTrue(context_window.is_context_window_limit)
        self.assertIs(SdkResultMessage.from_sdk_message(normal), normal)

    def test_sdk_result_observation_groups_terminal_runtime_decisions(self):
        provider_now = datetime.now(timezone.utc) + timedelta(hours=8)
        provider_reset = provider_now + timedelta(hours=1)
        usage_text = (
            "API Error: Request rejected (429) · [1308][Usage limit reached "
            f"for 5 hour. Your limit will reset at {provider_reset:%Y-%m-%d %H:%M:%S}]"
            f"[{provider_now:%Y%m%d%H%M%S}abcdef]"
        )

        normal = SdkResultMessage(
            session_id="sess-1",
            result_text="done",
            total_cost_usd=1.25,
            num_turns=3,
            duration_ms=2500,
        ).observation()
        context_window = SdkResultMessage(
            result_text="API Error: The model has reached its context window limit.",
            is_error=True,
        ).observation()
        usage_limit = SdkResultMessage(
            result_text=usage_text,
            is_error=True,
        ).observation()
        sdk_failure = SdkResultMessage(
            errors=["boom"],
            is_error=True,
        ).observation()

        self.assertIsInstance(normal, SdkResultObservation)
        self.assertEqual(normal.session_id, "sess-1")
        self.assertEqual(normal.transcript_text, "done")
        self.assertTrue(normal.has_cost)
        self.assertEqual(normal.duration_s, 2.5)
        self.assertEqual(normal.compute_cost_payload, {
            "cost_usd": 1.25,
            "turns": 3,
            "duration_ms": 2500.0,
        })
        self.assertTrue(context_window.context_window_limit)
        self.assertFalse(context_window.usage_limit_seen)
        self.assertTrue(usage_limit.usage_limit_seen)
        self.assertIsNone(usage_limit.failure_classification)
        self.assertEqual(
            sdk_failure.failure_classification,
            ToolResultClassification.SDK_FAILURE,
        )
        self.assertEqual(sdk_failure.failure_journal_text, "sdk_failure: boom")

    def test_tool_param_schema_validates_schema_shape_and_request(self):
        schema = ToolParamSchema.from_mapping({
            "required": {
                "entity_name": TOOL_PARAM_STRING,
                "x": TOOL_PARAM_NUMBER,
                "y": TOOL_PARAM_NUMBER,
            },
            "optional": {"direction": TOOL_PARAM_STRING},
        })
        request = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__place_entity",
            "tool_input": {
                "entity_name": "stone-furnace",
                "x": 42.5,
                "y": -21,
                "direction": "south",
            },
        })

        schema.validate_request(request)

        with self.assertRaisesRegex(
            BridgeValidationError,
            "required.x: unknown parameter type",
        ):
            ToolParamSchema.from_mapping({"required": {"x": "coordinate"}})
        with self.assertRaisesRegex(
            BridgeValidationError,
            "optional.direction: expected parameter type string",
        ):
            ToolParamSchema.from_mapping({"optional": {"direction": 8}})

    def test_tool_param_schema_registry_validates_and_dispatches_by_tool_name(self):
        registry = ToolParamSchemaRegistry.from_mapping({
            "place_entity": {
                "required": {"entity_name": TOOL_PARAM_STRING, "x": TOOL_PARAM_NUMBER},
                "optional": {"direction": TOOL_PARAM_STRING},
            },
        })
        request = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__place_entity",
            "tool_input": {"entity_name": "stone-furnace", "x": 1.5},
        })
        unknown = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__future_tool",
            "tool_input": {"bad": object()},
        })

        registry.validate_request(request)
        registry.validate_request(unknown)

        with self.assertRaisesRegex(
            BridgeValidationError,
            "place_entity: required.x: unknown parameter type",
        ):
            ToolParamSchemaRegistry.from_mapping({
                "place_entity": {"required": {"x": "coordinate"}},
            })
        with self.assertRaisesRegex(
            BridgeValidationError,
            "tool_param_schema_registry: expected non-empty string keys",
        ):
            ToolParamSchemaRegistry.from_mapping({" ": {}})

    def test_pre_tool_use_decision_serializes_sdk_hook_shape(self):
        allowed = PreToolUseDecision.allow()
        denied = PreToolUseDecision.deny("blocked because planner turn")

        self.assertEqual(
            allowed.to_dict(),
            {
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "allow",
                },
            },
        )
        self.assertEqual(
            denied.to_dict(),
            {
                "decision": "block",
                "reason": "blocked because planner turn",
                "hookSpecificOutput": {
                    "hookEventName": "PreToolUse",
                    "permissionDecision": "deny",
                    "permissionDecisionReason": "blocked because planner turn",
                },
            },
        )
        self.assertTrue(PreToolUseDecision(
            permission_decision="unexpected",
        ).is_denied)

    def test_pre_tool_use_hook_response_serializes_noop_allow_and_block(self):
        block = PreToolUseGuardBlock.read_only_turn(
            tool_name="mcp__factorioctl__mine_at",
        )
        denied_payload = PreToolUseHookResponse.block(block).to_dict()
        round_trip = PreToolUseHookResponse(decision=denied_payload)

        self.assertTrue(PreToolUseHookResponse.noop().is_noop)
        self.assertEqual(PreToolUseHookResponse.noop().to_dict(), {})
        self.assertEqual(
            PreToolUseHookResponse.allow().to_dict(),
            PreToolUseDecision.allow().to_dict(),
        )
        self.assertEqual(denied_payload, block.to_dict())
        self.assertFalse(round_trip.is_noop)
        self.assertEqual(round_trip.to_dict(), denied_payload)

    def test_pre_tool_use_guard_block_formats_operator_safe_reasons(self):
        cases = [
            (
                PreToolUseGuardBlock.parallel_mutation(
                    tool_name="mcp__factorioctl__place_entity",
                    previous_tool_name="mcp__factorioctl__walk_to",
                    elapsed_s="0.1259",
                ),
                (
                    "Factorioctl bridge blocked parallel mutating tool call: "
                    "place_entity. Wait for the previous mutating tool result "
                    "before issuing another world/inventory-changing command."
                ),
                "blocked parallel mutating tool: place_entity after walk_to in 0.126s",
            ),
            (
                PreToolUseGuardBlock.read_only_turn(
                    tool_name="mcp__factorioctl__mine_at",
                ),
                (
                    "Factorioctl bridge blocked non-read-only tool during "
                    "planner/reflection turn: mine_at. This turn may only use "
                    "read-only diagnostics; emit a ledger-only plan or reflection "
                    "and stop."
                ),
                "blocked non-read-only tool during planner/reflection turn: mine_at",
            ),
            (
                PreToolUseGuardBlock.skill_required(
                    tool_name="mcp__factorioctl__insert_items",
                ),
                (
                    "Factorioctl bridge blocked Factorio tool before control skill: "
                    "insert_items. Call Skill(factorio-control) before using "
                    "Factorio MCP tools."
                ),
                "blocked Factorio MCP tool before skill: insert_items",
            ),
            (
                PreToolUseGuardBlock.manual_automation(
                    tool_name="mcp__factorioctl__insert_items",
                ),
                (
                    "Factorioctl bridge blocked stale manual automation tool: "
                    "insert_items. The active ledger plan is stale because it "
                    "relies on manual transfer loops. Replace it with durable "
                    "automation controllers such as bootstrap_smelting_once for "
                    "first-inserter deadlocks, repair_fuel_sustainability, build_fuel_supply, "
                    "execute_direct_smelter, plan_recipe_assembler_cell, "
                    "build_recipe_assembler_cell, build_automation_science, "
                    "build_assembler_feed, plan_machine_output, build_assembler_output for "
                    "machine/furnace output belts, or build_lab_feed."
                ),
                "blocked stale manual automation tool: insert_items",
            ),
            (
                PreToolUseGuardBlock.param_schema(
                    tool_name="mcp__factorioctl__walk_to",
                    detail="walk_to: x: expected number",
                ),
                (
                    "Factorioctl bridge blocked invalid Factorio tool parameters: "
                    "walk_to: x: expected number"
                ),
                "blocked invalid walk_to params: walk_to: x: expected number",
            ),
            (
                PreToolUseGuardBlock.param_schema(
                    detail="tool_name: expected non-empty string",
                ),
                (
                    "Factorioctl bridge blocked invalid Factorio tool parameters: "
                    "tool_name: expected non-empty string"
                ),
                (
                    "blocked malformed tool call hook input: "
                    "tool_name: expected non-empty string"
                ),
            ),
        ]

        for block, expected_reason, expected_debug in cases:
            with self.subTest(kind=block.kind):
                payload = block.to_dict()

                self.assertEqual(block.reason, expected_reason)
                self.assertEqual(block.debug_message, expected_debug)
                self.assertEqual(payload["decision"], "block")
                self.assertEqual(payload["reason"], expected_reason)
                self.assertEqual(
                    payload["hookSpecificOutput"],
                    {
                        "hookEventName": "PreToolUse",
                        "permissionDecision": "deny",
                        "permissionDecisionReason": expected_reason,
                    },
                )

        coerced = PreToolUseGuardBlock(
            kind="read-only-turn",
            tool_name="mcp__factorioctl__place_entity",
        )
        self.assertEqual(coerced.kind, PreToolUseGuardKind.READ_ONLY_TURN)

    def test_hidden_trailer_block_parses_and_strips_multiple_tags(self):
        text = (
            "Before.\n\n"
            "<ledger>\nprogress: fixed belt\n</ledger>\n\n"
            "<bug_report>\nname: belt_gap\nproblem: missing belt\n</bug_report>\n\n"
            "<ledger>\nprogress: second ledger ignored by first_from_text\n</ledger>\n\n"
            "After."
        )

        first_ledger = HiddenTrailerBlock.first_from_text(text, "ledger")
        proposals = HiddenTrailerBlock.all_from_text(text, ["bug_report", "skill_proposal"])

        self.assertIsNotNone(first_ledger)
        self.assertEqual(first_ledger.tag, "ledger")
        self.assertIn("fixed belt", first_ledger.body)
        self.assertEqual([block.tag for block in proposals], ["bug_report"])
        self.assertIn("missing belt", proposals[0].body)
        self.assertEqual(
            HiddenTrailerBlock.strip_from_text(text, ["ledger", "bug_report"]),
            "Before.\n\nAfter.",
        )

    def test_hidden_trailer_block_is_total_on_bad_shapes_and_mismatched_tags(self):
        self.assertEqual(HiddenTrailerBlock.all_from_text(None, ["ledger"]), [])
        self.assertIsNone(HiddenTrailerBlock.first_from_text("<ledger>oops</skill>", "ledger"))
        self.assertEqual(HiddenTrailerBlock.strip_from_text(42, ["ledger"]), "")
        self.assertEqual(
            HiddenTrailerBlock.strip_from_text("No trailer", ["not a tag"]),
            "No trailer",
        )

    def test_tool_call_request_accepts_sdk_hook_object_shape(self):
        class HookInput:
            tool_name = "mcp__factorioctl__insert_items"
            tool_input = {"unit_number": 42, "item": "coal", "count": 5}

        request = ToolCallRequest.from_hook_input(HookInput())

        self.assertEqual(request.tool_name, "mcp__factorioctl__insert_items")
        self.assertEqual(request.tool_input["item"], "coal")

    def test_tool_call_request_identifies_manual_fuel_transfers(self):
        coal = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__insert_items",
            "tool_input": {"unit_number": 42, "item": "coal", "count": 5},
        })
        fuel_inventory = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__insert_items",
            "tool_input": {
                "unit_number": 42,
                "item": "not-a-fuel",
                "count": 5,
                "inventory_type": "fuel",
            },
        })
        ore = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__insert_items",
            "tool_input": {
                "unit_number": 42,
                "item": "iron-ore",
                "count": 5,
                "inventory_type": "furnace_source",
            },
        })
        durable_controller = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__build_fuel_supply",
            "tool_input": {"consumer_unit_number": 49},
        })

        self.assertTrue(coal.is_manual_fuel_transfer)
        self.assertTrue(fuel_inventory.is_manual_fuel_transfer)
        self.assertFalse(ore.is_manual_fuel_transfer)
        self.assertFalse(durable_controller.is_manual_fuel_transfer)

    def test_tool_call_request_identifies_manual_science_transfers(self):
        craft_science = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__craft",
            "tool_input": {"recipe": "automation-science-pack", "count": 12},
        })
        feed_lab = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__feed_lab_from_inventory",
            "tool_input": {
                "lab_unit_number": 69,
                "science_pack": "automation-science-pack",
                "count": 12,
                "dry_run": False,
            },
        })
        dry_run_feed = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__feed_lab_from_inventory",
            "tool_input": {
                "lab_unit_number": 69,
                "science_pack": "automation-science-pack",
                "count": 12,
                "dry_run": True,
            },
        })
        craft_belt = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__craft",
            "tool_input": {"recipe": "transport-belt", "count": 12},
        })
        durable_controller = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__build_automation_science",
            "tool_input": {"assembler_unit_number": 80},
        })

        self.assertTrue(craft_science.is_manual_science_transfer)
        self.assertTrue(feed_lab.is_manual_science_transfer)
        self.assertFalse(dry_run_feed.is_manual_science_transfer)
        self.assertFalse(craft_belt.is_manual_science_transfer)
        self.assertFalse(durable_controller.is_manual_science_transfer)

    def test_tool_call_request_identifies_manual_material_transfers(self):
        ore_input = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__insert_items",
            "tool_input": {
                "unit_number": 42,
                "item": "iron-ore",
                "count": 20,
                "inventory_type": "furnace_source",
            },
        })
        plate_output = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__extract_items",
            "tool_input": {
                "unit_number": 42,
                "item": "iron-plate",
                "count": 20,
                "inventory_type": "furnace_result",
            },
        })
        chest_extract = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__extract_items",
            "tool_input": {
                "unit_number": 42,
                "item": "iron-plate",
                "count": 20,
                "inventory_type": "chest",
            },
        })
        durable_controller = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__execute_direct_smelter",
            "tool_input": {"drill_unit_number": 80},
        })

        self.assertTrue(ore_input.is_manual_material_transfer)
        self.assertTrue(plate_output.is_manual_material_transfer)
        self.assertFalse(chest_extract.is_manual_material_transfer)
        self.assertFalse(durable_controller.is_manual_material_transfer)

    def test_tool_call_request_identifies_manual_component_crafting(self):
        craft_gears = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__craft",
            "tool_input": {"recipe": "iron-gear-wheel", "count": 12},
        })
        craft_cables = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__craft",
            "tool_input": {"recipe": "copper-cable", "count": 12},
        })
        craft_circuits = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__craft",
            "tool_input": {"recipe": "electronic-circuit", "count": 12},
        })
        craft_belts = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__craft",
            "tool_input": {"recipe": "transport-belt", "count": 12},
        })
        durable_controller = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__build_assembler_feed",
            "tool_input": {"assembler_unit_number": 80},
        })

        self.assertTrue(craft_gears.is_manual_component_craft)
        self.assertTrue(craft_cables.is_manual_component_craft)
        self.assertTrue(craft_circuits.is_manual_component_craft)
        self.assertFalse(craft_belts.is_manual_component_craft)
        self.assertFalse(durable_controller.is_manual_component_craft)

    def test_agent_session_state_models_current_and_legacy_files(self):
        current = AgentSessionState.from_file_text(
            '{"session_id": " abc123 "} \n'
        )
        legacy = AgentSessionIndex.from_file_text(
            '{"doug": " old-session ", "empty": "", "bad": 3}'
        )

        self.assertEqual(current.session_id, "abc123")
        self.assertEqual(current.to_json_line(), '{"session_id":"abc123"}\n')
        self.assertIs(AgentSessionState.from_file_text(current), current)
        self.assertEqual(legacy.get("doug"), "old-session")
        self.assertIsNone(legacy.get("empty"))
        self.assertEqual(
            legacy.without("doug").to_legacy_json_line(),
            "{}\n",
        )

        with self.assertRaisesRegex(BridgeValidationError, "session_id"):
            AgentSessionState.from_file_text('{"session_id": ""}')

    def test_factorio_mod_info_parses_info_json_version(self):
        info = FactorioModInfo.from_file_text(
            '{"name":"claude-interface","version":" 0.9.0 ","title":"Bridge",'
            '"author":"qry","factorio_version":"2.0","dependencies":["base >= 2.0"]}'
        )
        missing_version = FactorioModInfo.from_file_text('{"name":"claude-interface"}')

        self.assertEqual(info.name, "claude-interface")
        self.assertEqual(info.version_label, "0.9.0")
        self.assertEqual(missing_version.version_label, "?")
        self.assertIs(FactorioModInfo.from_file_text(info), info)
        with self.assertRaisesRegex(BridgeValidationError, "mod_info: expected object"):
            FactorioModInfo.from_file_text("[1, 2, 3]")

    def test_fuel_sustainability_mod_exposes_ready_to_call_repair_args(self):
        entities_lua = (
            Path(__file__).resolve().parents[1]
            / "mod"
            / "claude-interface"
            / "entities.lua"
        ).read_text()

        self.assertIn("consumer.ready_to_call = {", entities_lua)
        self.assertIn('tool = "build_fuel_supply"', entities_lua)
        self.assertIn("args = target.ready_to_call.args", entities_lua)
        self.assertIn("follow_up = target.ready_to_call.follow_up", entities_lua)

    def test_dotenv_file_parses_and_applies_without_overwriting_existing_env(self):
        dotenv = DotEnvFile.from_text(
            """
            # comment
            KEEP=from-file
            NEW = value
            EMPTY=
            TOKEN=a=b=c
            =missing-key
            BAD_LINE
            """
        )
        env = {"KEEP": "existing"}

        dotenv.apply_to_environ(env)

        self.assertEqual(dotenv.assignments, {
            "KEEP": "from-file",
            "NEW": "value",
            "EMPTY": "",
            "TOKEN": "a=b=c",
        })
        self.assertEqual(env, {"KEEP": "existing", "NEW": "value", "TOKEN": "a=b=c"})

    def test_dotenv_assignment_line_classifies_valid_assignments(self):
        self.assertEqual(
            DotEnvAssignmentLine.from_line("TOKEN = a=b=c").model_dump(),
            {
                "line": "TOKEN = a=b=c",
                "key": "TOKEN",
                "value": "a=b=c",
                "valid": True,
            },
        )
        self.assertFalse(DotEnvAssignmentLine.from_line("# comment").valid)
        self.assertFalse(DotEnvAssignmentLine.from_line("MISSING_EQUALS").valid)
        self.assertFalse(DotEnvAssignmentLine.from_line(" = nope").valid)
