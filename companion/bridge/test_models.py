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


class ModelTests(unittest.TestCase):
    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.base = Path(self.tempdir.name)

    def test_agent_profile_reports_field_specific_shape_errors(self):
        with self.assertRaisesRegex(BridgeValidationError, "system_prompt: expected non-empty string"):
            AgentProfile.from_mapping({"name": "doug", "system_prompt": ""})

        with self.assertRaisesRegex(BridgeValidationError, "max_turns: expected integer"):
            AgentProfile.from_mapping({
                "name": "doug",
                "system_prompt": "Build.",
                "max_turns": "many",
            })

        with self.assertRaisesRegex(BridgeValidationError, "sdk_skills: expected string or list of strings"):
            AgentProfile.from_mapping({
                "name": "doug",
                "system_prompt": "Build.",
                "sdk_skills": [{"name": "factorio-control"}],
            })

    def test_agent_profile_round_trips_existing_dict_shape(self):
        typed_format = AgentResponseFormat.coerce({"header_label": "STATUS"})
        profile = AgentProfile.from_mapping({
            "name": "doug",
            "system_prompt": "Build.",
            "model": "haiku",
            "max_turns": 200,
            "planet": "nauvis",
            "sdk_skills": ["factorio-control"],
            "response_format": typed_format,
            "future_field": "kept for compatibility",
        })

        data = profile.to_dict()

        self.assertIs(profile.response_format, typed_format)
        self.assertEqual(data["name"], "doug")
        self.assertEqual(data["system_prompt"], "Build.")
        self.assertEqual(data["max_turns"], 200)
        self.assertEqual(data["sdk_skills"], ["factorio-control"])
        self.assertEqual(data["response_format"]["header_label"], "STATUS")
        self.assertEqual(data["future_field"], "kept for compatibility")

    def test_agent_profile_from_mapping_preserves_typed_profile(self):
        profile = AgentProfile.from_mapping({
            "name": "doug",
            "system_prompt": "Build.",
        })

        self.assertIs(AgentProfile.from_mapping(profile), profile)

    def test_agent_profile_model_validates_sdk_skills_directly(self):
        profile = AgentProfile(
            name="doug",
            system_prompt="Build.",
            sdk_skills=["factorio-control"],
        )
        tuple_profile = AgentProfile(
            name="ada",
            system_prompt="Build.",
            sdk_skills=("factorio-control", " verify "),
        )

        self.assertEqual(profile.sdk_skills, ["factorio-control"])
        self.assertEqual(tuple_profile.sdk_skills, ["factorio-control", "verify"])
        self.assertEqual(
            AgentProfileSdkSkills.from_value(("factorio-control", " verify ")).to_profile_value(),
            ["factorio-control", "verify"],
        )
        self.assertEqual(
            AgentProfileSdkSkills.from_value("factorio-control, verify").to_profile_value(),
            "factorio-control, verify",
        )
        with self.assertRaisesRegex(ValueError, "sdk_skills"):
            AgentProfile(
                name="doug",
                system_prompt="Build.",
                sdk_skills=[{"name": "factorio-control"}],
            )
        with self.assertRaisesRegex(ValueError, "expected string or list of strings"):
            AgentProfileSdkSkills.from_value([{"name": "factorio-control"}])

    def test_agent_profile_exposes_runtime_convenience_fields(self):
        nauvis = AgentProfile.from_mapping({
            "name": "doug",
            "system_prompt": "Build.",
        })
        vulcanus = AgentProfile.from_mapping({
            "name": "vulcanus-builder",
            "system_prompt": "Build.",
            "planet": "vulcanus",
        })

        self.assertIs(AgentProfile.coerce(nauvis), nauvis)
        self.assertEqual(nauvis.planet_name, "nauvis")
        self.assertEqual(nauvis.registration_label, "Doug")
        self.assertEqual(vulcanus.registration_label, "Vulcanus")
        self.assertLess(
            nauvis.sort_key({"nauvis": 0, "vulcanus": 1}),
            vulcanus.sort_key({"nauvis": 0, "vulcanus": 1}),
        )

    def test_agent_name_selection_parses_cli_comma_list(self):
        selection = AgentNameSelection.from_cli_arg(" doug , ada,, bob ")
        from_list = AgentNameSelection.from_cli_arg([" doug ", "", "ada"])

        self.assertEqual(selection.names, ["doug", "ada", "bob"])
        self.assertEqual(selection.filter_or_none, ["doug", "ada", "bob"])
        self.assertEqual(from_list.names, ["doug", "ada"])
        self.assertIsNone(AgentNameSelection.from_cli_arg(None).filter_or_none)

    def test_agent_runtime_config_resolves_profile_cli_and_env_overlays(self):
        profile = AgentProfile.from_mapping({
            "name": "doug",
            "system_prompt": "Build.",
            "model": "profile-model",
            "planner_model": "planner-model",
            "max_turns": 99,
            "telemetry_name": "DOUG",
            "heartbeat_interval": 7,
            "planner_interval": 4,
            "reflect_interval": 8,
            "autonomy_requires_player": False,
            "sdk_skills": "profile-skill",
        })

        runtime = AgentRuntimeConfig.from_sources(
            profile,
            cli_model="cli-model",
            cli_max_turns=120,
            cli_sdk_skills="cli-skill, verify",
            default_sdk_skills="default-skill",
            heartbeat_interval=1,
            planner_interval=2,
            autonomy_requires_player=True,
            env={"BRIDGE_MAX_TURNS": "80", "BRIDGE_SDK_SKILLS": "env-skill"},
        )

        self.assertIs(runtime.profile, profile)
        self.assertEqual(runtime.agent_name, "doug")
        self.assertEqual(runtime.system_prompt, "Build.")
        self.assertEqual(runtime.model, "cli-model")
        self.assertEqual(runtime.planner_model, "planner-model")
        self.assertEqual(runtime.max_turns, 120)
        self.assertEqual(runtime.sdk_skills, ["cli-skill", "verify"])
        self.assertEqual(runtime.telemetry_name, "DOUG")
        self.assertEqual(runtime.heartbeat_interval, 7.0)
        self.assertEqual(runtime.planner_interval, 4)
        self.assertEqual(runtime.reflect_interval, 8)
        self.assertFalse(runtime.autonomy_requires_player)

    def test_agent_runtime_config_uses_profile_before_default_sdk_skills(self):
        profile = AgentProfile.from_mapping({
            "name": "doug",
            "system_prompt": "Build.",
            "sdk_skills": "profile-skill",
        })
        runtime = AgentRuntimeConfig.from_sources(
            profile,
            default_sdk_skills="default-skill",
            env={"BRIDGE_MAX_TURNS": "80"},
        )

        self.assertEqual(runtime.model, "haiku")
        self.assertEqual(runtime.max_turns, 80)
        self.assertEqual(runtime.sdk_skills, ["profile-skill"])
        self.assertEqual(runtime.telemetry_name, "doug")
        self.assertEqual(runtime.heartbeat_interval, 0.0)
        self.assertEqual(runtime.planner_interval, 5)
        self.assertEqual(runtime.reflect_interval, 16)
        self.assertTrue(runtime.autonomy_requires_player)

    def test_sdk_skill_config_does_not_stringify_invalid_list_items(self):
        config = SdkSkillConfig.resolve(("factorio-control", {"bad": True}, 7, " verify "))

        self.assertEqual(config.skills, ["factorio-control", "verify"])
        self.assertEqual(config.sdk_value, ["factorio-control", "verify"])
        self.assertTrue(config.requires_factorio_control)

    def test_agent_invocation_config_resolves_sdk_options_and_targets(self):
        invocation = AgentInvocationConfig.from_sources(
            system_prompt="system",
            agent_name="doug",
            telemetry_name="DOUG",
            response_to="all",
            session_id="session-123456789",
            model="haiku",
            max_turns="40",
            sdk_skills="factorio-control, verify",
            read_only_tools="true",
            default_sdk_skills="default-skill",
            env={"BRIDGE_MAX_TURNS": "80", "BRIDGE_SDK_SKILLS": "env-skill"},
        )

        self.assertEqual(invocation.agent_name, "doug")
        self.assertEqual(invocation.telemetry_label, "DOUG")
        self.assertEqual(invocation.rcon_target, "all")
        self.assertEqual(invocation.system_prompt, "system")
        self.assertEqual(invocation.session_id, "session-123456789")
        self.assertEqual(invocation.model, "haiku")
        self.assertEqual(invocation.max_turns, 40)
        self.assertEqual(invocation.sdk_skills, ["factorio-control", "verify"])
        self.assertTrue(invocation.skill_config.requires_factorio_control)
        self.assertTrue(invocation.read_only_tools)
        self.assertEqual(invocation.resume_tag, " (resume session-...)")

    def test_agent_invocation_config_uses_env_and_defaults_when_unset(self):
        invocation = AgentInvocationConfig.from_sources(
            system_prompt="system",
            agent_name="doug",
            sdk_skills=None,
            default_sdk_skills="default-skill",
            env={"BRIDGE_MAX_TURNS": "80"},
        )

        self.assertEqual(invocation.telemetry_label, "doug")
        self.assertEqual(invocation.rcon_target, "doug")
        self.assertIsNone(invocation.session_id)
        self.assertIsNone(invocation.model)
        self.assertEqual(invocation.max_turns, 80)
        self.assertEqual(invocation.sdk_skills, ["default-skill"])
        self.assertFalse(invocation.read_only_tools)
        self.assertEqual(invocation.resume_tag, " (new session)")

    def test_agent_invocation_config_builds_typed_sdk_options_spec(self):
        invocation = AgentInvocationConfig.from_sources(
            system_prompt="system",
            agent_name="doug",
            session_id="session-123",
            model="haiku",
            max_turns="40",
            sdk_skills="factorio-control, verify",
            env={"BRIDGE_SDK_SKILLS": "ignored-env-skill"},
        )

        spec = invocation.to_sdk_options_spec(
            mcp_servers={"factorioctl": {"command": "mcp"}},
            env={},
            project_root=self.base,
        )

        self.assertIsInstance(spec, AgentClaudeOptionsSpec)
        self.assertEqual(spec.system_prompt, "system")
        self.assertEqual(spec.model, "haiku")
        self.assertEqual(spec.max_turns, 40)
        self.assertEqual(spec.mcp_servers, {"factorioctl": {"command": "mcp"}})
        self.assertTrue(spec.strict_mcp_config)
        self.assertEqual(spec.tools, ["Skill"])
        self.assertEqual(spec.disallowed_tools, ["mcp__factorioctl__execute_lua"])
        self.assertEqual(spec.permission_mode, "bypassPermissions")
        self.assertEqual(spec.resume, "session-123")
        self.assertEqual(spec.setting_sources, ["project", "local"])
        self.assertEqual(spec.cwd, str(self.base))
        self.assertEqual(spec.skills, ["factorio-control", "verify"])

    def test_agent_invocation_options_spec_allows_raw_lua_from_policy(self):
        invocation = AgentInvocationConfig.from_sources(
            system_prompt="system",
            agent_name="doug",
            sdk_skills="factorio-control",
        )

        spec = invocation.to_sdk_options_spec(
            mcp_servers={},
            env={"FACTORIOCTL_ALLOW_RAW_LUA": "true"},
            project_root=self.base,
        )

        self.assertEqual(spec.disallowed_tools, [])

    def test_agent_message_result_models_session_keep_and_reset(self):
        kept = AgentMessageResult.keep_session("new-session")
        reset = AgentMessageResult.reset()

        self.assertEqual(kept.session_id, "new-session")
        self.assertFalse(kept.reset_session)
        self.assertEqual(kept.to_legacy_session_value("__reset__"), "new-session")
        self.assertIsNone(AgentMessageResult.keep_session("").session_id)
        self.assertIsNone(AgentMessageResult.keep_session(None).session_id)
        self.assertTrue(reset.reset_session)
        self.assertIsNone(reset.session_id)
        self.assertEqual(reset.to_legacy_session_value("__reset__"), "__reset__")

    def test_agent_run_transcript_carries_typed_run_signals(self):
        transcript = AgentRunTranscript.from_parts(
            text_parts=["first", "", "3"],
            session_id=" session-1 ",
            context_window_limit="yes",
            usage_limit_seen="true",
        )

        self.assertEqual(transcript.text_parts, ["first", "3"])
        self.assertEqual(transcript.session_id, "session-1")
        self.assertTrue(transcript.context_window_limit)
        self.assertTrue(transcript.usage_limit_seen)
        self.assertEqual(transcript.session_or("fallback"), "session-1")
        self.assertEqual(transcript.reply_text, "first\n\n3")

        replaced = transcript.with_text_parts(["Provider usage limit is active"])
        self.assertEqual(replaced.text_parts, ["Provider usage limit is active"])
        self.assertTrue(replaced.context_window_limit)
        self.assertTrue(replaced.usage_limit_seen)
        self.assertEqual(
            AgentRunTranscript.from_parts().reply_text,
            "(action complete)",
        )
        self.assertEqual(
            AgentRunTranscript.from_parts().session_or(" fallback "),
            "fallback",
        )

    def test_sdk_skill_config_owns_skill_parsing_and_sdk_launch_shape(self):
        defaulted = SdkSkillConfig.resolve(None, default="factorio-control, verify")
        disabled = SdkSkillConfig.resolve("none")
        all_skills = SdkSkillConfig.resolve("all")
        from_list = SdkSkillConfig.resolve(("factorio-control", " ", "debug"))
        env_override = SdkSkillConfig.from_env(
            {"BRIDGE_SDK_SKILLS": "none"},
            default="factorio-control",
        )
        cli_override = SdkSkillConfig.from_env(
            {"BRIDGE_SDK_SKILLS": "none"},
            value="all",
            default="factorio-control",
        )
        env_default = SdkSkillConfig.from_env({}, default="factorio-control")

        self.assertEqual(defaulted.skills, ["factorio-control", "verify"])
        self.assertEqual(defaulted.sdk_value, ["factorio-control", "verify"])
        self.assertEqual(defaulted.claude_tools, ["Skill"])
        self.assertEqual(defaulted.setting_sources, ["project", "local"])
        self.assertTrue(defaulted.requires_factorio_control)
        self.assertEqual(disabled.sdk_value, [])
        self.assertEqual(disabled.claude_tools, [])
        self.assertEqual(disabled.setting_sources, ["local"])
        self.assertFalse(disabled.requires_factorio_control)
        self.assertEqual(all_skills.sdk_value, "all")
        self.assertEqual(all_skills.claude_tools, ["Skill"])
        self.assertTrue(all_skills.requires_factorio_control)
        self.assertEqual(from_list.skills, ["factorio-control", "debug"])
        self.assertFalse(env_override.enabled)
        self.assertTrue(cli_override.all_skills)
        self.assertEqual(env_default.skills, ["factorio-control"])
        self.assertIs(SdkSkillConfig.from_env(all_skills), all_skills)
        self.assertTrue(SdkSkillConfig.from_env(disabled, value="all").all_skills)

    def test_sdk_skill_config_uses_typed_env_bindings(self):
        fields = SdkSkillConfig.env_fields()

        self.assertTrue(all(isinstance(field, BridgeRuntimeEnvField) for field in fields))
        self.assertEqual(fields[0].env_name, "BRIDGE_SDK_SKILLS")
        self.assertEqual(fields[0].field_name, "skills")
        with mock.patch.object(
            SdkSkillConfig,
            "ENV_FIELDS",
            (
                BridgeRuntimeEnvField(env_name="A", field_name="skills"),
                BridgeRuntimeEnvField(env_name="B", field_name="skills"),
            ),
        ):
            with self.assertRaisesRegex(BridgeValidationError, "duplicate field_name"):
                SdkSkillConfig.env_fields()

    def test_raw_lua_policy_matches_env_truth_table(self):
        tool = "mcp__factorioctl__execute_lua"

        for value in ("1", " TRUE ", "yes", "on", True):
            policy = RawLuaPolicy.from_env({"FACTORIOCTL_ALLOW_RAW_LUA": value})
            self.assertTrue(policy.allow_raw_lua, value)
            self.assertEqual(policy.disallowed_tools, [])

        for value in ("0", "false", "no", "off", "anything", "", None, False):
            policy = RawLuaPolicy.from_env({"FACTORIOCTL_ALLOW_RAW_LUA": value})
            self.assertFalse(policy.allow_raw_lua, value)
            self.assertEqual(policy.disallowed_tools, [tool])

        self.assertEqual(RawLuaPolicy.from_env({}).disallowed_tools, [tool])
        self.assertEqual(RawLuaPolicy.from_env(None).disallowed_tools, [tool])
        blocked = RawLuaPolicy()
        self.assertIs(RawLuaPolicy.from_env(blocked), blocked)

    def test_raw_lua_policy_uses_typed_env_bindings(self):
        fields = RawLuaPolicy.env_fields()

        self.assertTrue(all(isinstance(field, BridgeRuntimeEnvField) for field in fields))
        self.assertEqual(fields[0].env_name, "FACTORIOCTL_ALLOW_RAW_LUA")
        self.assertEqual(fields[0].field_name, "allow_raw_lua")
        with mock.patch.object(
            RawLuaPolicy,
            "ENV_FIELDS",
            (
                BridgeRuntimeEnvField(env_name="A", field_name="allow_raw_lua"),
                BridgeRuntimeEnvField(env_name="B", field_name="allow_raw_lua"),
            ),
        ):
            with self.assertRaisesRegex(BridgeValidationError, "duplicate field_name"):
                RawLuaPolicy.env_fields()

    def test_telemetry_relay_settings_normalizes_cli_and_env_sources(self):
        cli = TelemetryRelaySettings.from_sources(
            cli_url=" https://cli.example ",
            cli_token=" cli-token ",
            env={"RELAY_URL": "https://env.example", "RELAY_TOKEN": "env-token"},
        )
        fallback = TelemetryRelaySettings.from_sources(
            cli_url=" ",
            cli_token=None,
            env={"RELAY_URL": " https://env.example ", "RELAY_TOKEN": " env-token "},
        )
        missing_token = TelemetryRelaySettings.from_sources(
            env={"RELAY_URL": " https://env.example ", "RELAY_TOKEN": " "},
        )
        disabled = TelemetryRelaySettings.from_sources(
            cli_url=" ",
            env={"RELAY_URL": ""},
        )

        self.assertEqual(cli.relay_url, "https://cli.example")
        self.assertEqual(cli.relay_token, "cli-token")
        self.assertTrue(cli.ready)
        self.assertEqual(fallback.relay_url, "https://env.example")
        self.assertEqual(fallback.relay_token, "env-token")
        self.assertTrue(fallback.ready)
        self.assertTrue(missing_token.enabled)
        self.assertFalse(missing_token.ready)
        self.assertFalse(disabled.enabled)

    def test_telemetry_relay_settings_uses_typed_env_bindings(self):
        fields = TelemetryRelaySettings.env_fields()

        self.assertTrue(all(isinstance(field, BridgeRuntimeEnvField) for field in fields))
        self.assertIn("RELAY_TOKEN", {field.env_name for field in fields})
        with mock.patch.object(
            TelemetryRelaySettings,
            "ENV_FIELDS",
            (
                BridgeRuntimeEnvField(env_name="RELAY_DUP", field_name="relay_url"),
                BridgeRuntimeEnvField(env_name="RELAY_DUP", field_name="relay_token"),
            ),
        ):
            with self.assertRaisesRegex(BridgeValidationError, "duplicate env_name"):
                TelemetryRelaySettings.env_fields()

    def test_bridge_runtime_settings_normalizes_env_and_bad_values(self):
        settings = BridgeRuntimeSettings.from_env({
            "BRIDGE_MAX_TURNS": "80",
            "BRIDGE_CONTEXT_WINDOW_BACKOFF_S": "0.1",
            "BRIDGE_TICK_TIMEOUT_S": "bad",
            "BRIDGE_STREAM_IDLE_TIMEOUT_S": "45.5",
            "BRIDGE_WATCHDOG_SAME_FAILURE_LIMIT": "0",
            "BRIDGE_WATCHDOG_NO_PROGRESS_TIMEOUT_S": "-1",
            "BRIDGE_MUTATING_TOOL_BATCH_WINDOW_S": "0",
        })

        self.assertEqual(settings.max_turns, 80)
        self.assertEqual(settings.context_window_backoff_s, 900.0)
        self.assertEqual(settings.tick_timeout_s, 2400.0)
        self.assertEqual(settings.stream_idle_timeout_s, 45.5)
        self.assertEqual(settings.watchdog_same_failure_limit, 0)
        self.assertEqual(settings.watchdog_no_progress_timeout_s, 900.0)
        self.assertEqual(settings.mutating_tool_batch_window_s, 0.0)
        self.assertIs(BridgeRuntimeSettings.from_env(settings), settings)
        self.assertEqual(BridgeRuntimeSettings(max_turns="bad").max_turns, 200)
        self.assertEqual(
            BridgeRuntimeSettings(watchdog_no_progress_timeout_s="0").watchdog_no_progress_timeout_s,
            0.0,
        )

    def test_bridge_runtime_settings_uses_typed_env_bindings(self):
        fields = BridgeRuntimeSettings.env_fields()

        self.assertTrue(all(isinstance(field, BridgeRuntimeEnvField) for field in fields))
        self.assertIn(
            "BRIDGE_MAX_TURNS",
            {field.env_name for field in fields},
        )
        with mock.patch.object(
            BridgeRuntimeSettings,
            "ENV_FIELDS",
            (
                BridgeRuntimeEnvField(env_name="DUP", field_name="max_turns"),
                BridgeRuntimeEnvField(env_name="DUP", field_name="tick_timeout_s"),
            ),
        ):
            with self.assertRaisesRegex(BridgeValidationError, "duplicate env_name"):
                BridgeRuntimeSettings.env_fields()
        with self.assertRaisesRegex(ValueError, "env_name"):
            BridgeRuntimeEnvField(env_name="", field_name="max_turns")

    def test_bridge_runtime_env_field_validates_and_reads_sources(self):
        fields = BridgeRuntimeEnvField.validate_unique(
            (
                {"env_name": "ONE", "field_name": "one"},
                BridgeRuntimeEnvField(env_name="TWO", field_name="two"),
            ),
            field_path="test_env_fields",
        )

        self.assertEqual(
            BridgeRuntimeEnvField.read_source(
                {"ONE": "1", "TWO": "2", "THREE": "3"},
                fields,
            ),
            {"one": "1", "two": "2"},
        )
        self.assertEqual(BridgeRuntimeEnvField.read_source(None, fields), {})
        with self.assertRaisesRegex(BridgeValidationError, "duplicate env_name"):
            BridgeRuntimeEnvField.validate_unique(
                (
                    BridgeRuntimeEnvField(env_name="ONE", field_name="one"),
                    BridgeRuntimeEnvField(env_name="ONE", field_name="other"),
                ),
                field_path="test_env_fields",
            )
        with self.assertRaisesRegex(BridgeValidationError, "duplicate field_name"):
            BridgeRuntimeEnvField.validate_unique(
                (
                    BridgeRuntimeEnvField(env_name="ONE", field_name="one"),
                    BridgeRuntimeEnvField(env_name="TWO", field_name="one"),
                ),
                field_path="test_env_fields",
            )

    def test_ledger_runtime_settings_normalizes_env_and_bad_values(self):
        self.assertEqual(
            LedgerRuntimeSettings.from_env({
                "BRIDGE_STALE_BOOTSTRAP_LEDGER_MAX_AGE_S": "60",
            }).stale_bootstrap_ledger_max_age_s,
            60.0,
        )
        self.assertEqual(
            LedgerRuntimeSettings.from_env({
                "BRIDGE_STALE_BOOTSTRAP_LEDGER_MAX_AGE_S": "bad",
            }).stale_bootstrap_ledger_max_age_s,
            1800.0,
        )
        self.assertEqual(
            LedgerRuntimeSettings.from_env({
                "BRIDGE_STALE_BOOTSTRAP_LEDGER_MAX_AGE_S": "-1",
            }).stale_bootstrap_ledger_max_age_s,
            1800.0,
        )
        typed = LedgerRuntimeSettings(stale_bootstrap_ledger_max_age_s=12)
        self.assertIs(LedgerRuntimeSettings.from_env(typed), typed)

    def test_ledger_runtime_settings_uses_typed_env_bindings(self):
        fields = LedgerRuntimeSettings.env_fields()

        self.assertTrue(all(isinstance(field, BridgeRuntimeEnvField) for field in fields))
        self.assertEqual(fields[0].env_name, "BRIDGE_STALE_BOOTSTRAP_LEDGER_MAX_AGE_S")
        self.assertEqual(fields[0].field_name, "stale_bootstrap_ledger_max_age_s")
        with mock.patch.object(
            LedgerRuntimeSettings,
            "ENV_FIELDS",
            (
                BridgeRuntimeEnvField(
                    env_name="A",
                    field_name="stale_bootstrap_ledger_max_age_s",
                ),
                BridgeRuntimeEnvField(
                    env_name="B",
                    field_name="stale_bootstrap_ledger_max_age_s",
                ),
            ),
        ):
            with self.assertRaisesRegex(BridgeValidationError, "duplicate field_name"):
                LedgerRuntimeSettings.env_fields()

    def test_learning_runtime_settings_resolves_env_or_project_default(self):
        configured = LearningRuntimeSettings.from_env({
            "BRIDGE_LEARNING_DIR": " /tmp/factorio-learned ",
        })
        defaulted = LearningRuntimeSettings.from_env({
            "BRIDGE_LEARNING_DIR": "   ",
        })

        self.assertEqual(configured.learning_dir, "/tmp/factorio-learned")
        self.assertEqual(
            configured.resolved_learning_dir("/repo"),
            Path("/tmp/factorio-learned"),
        )
        self.assertIs(LearningRuntimeSettings.from_env(configured), configured)
        self.assertIsNone(defaulted.learning_dir)
        self.assertEqual(
            defaulted.resolved_learning_dir("/repo"),
            Path("/repo/.factorioctl/learned"),
        )

    def test_learning_runtime_settings_uses_typed_env_bindings(self):
        fields = LearningRuntimeSettings.env_fields()

        self.assertTrue(all(isinstance(field, BridgeRuntimeEnvField) for field in fields))
        self.assertEqual(fields[0].env_name, "BRIDGE_LEARNING_DIR")
        self.assertEqual(fields[0].field_name, "learning_dir")
        with mock.patch.object(
            LearningRuntimeSettings,
            "ENV_FIELDS",
            (
                BridgeRuntimeEnvField(env_name="BRIDGE_LEARNING_DIR", field_name="learning_dir"),
                BridgeRuntimeEnvField(env_name="BRIDGE_LEARNING_DIR", field_name="other_dir"),
            ),
        ):
            with self.assertRaisesRegex(BridgeValidationError, "duplicate env_name"):
                LearningRuntimeSettings.env_fields()

    def test_factorio_path_settings_normalizes_env_values(self):
        settings = FactorioPathSettings.from_env({
            "FACTORIO_SERVER_DATA": " /tmp/server-data ",
            "FACTORIO_MODS_DIR": " /tmp/mods ",
            "FACTORIOCTL_MCP_BIN": " /tmp/mcp ",
        })
        defaulted = FactorioPathSettings.from_env({
            "FACTORIO_SERVER_DATA": " ",
            "FACTORIO_MODS_DIR": "",
            "FACTORIOCTL_MCP_BIN": None,
        })

        self.assertEqual(settings.server_data, "/tmp/server-data")
        self.assertEqual(settings.script_output_dir, Path("/tmp/server-data/script-output"))
        self.assertEqual(settings.mods_dir_path, Path("/tmp/mods"))
        self.assertEqual(settings.mcp_bin_path, Path("/tmp/mcp"))
        self.assertIs(FactorioPathSettings.from_env(settings), settings)
        self.assertIsNone(defaulted.server_data)
        self.assertIsNone(defaulted.script_output_dir)
        self.assertIsNone(defaulted.mods_dir_path)
        self.assertIsNone(defaulted.mcp_bin_path)

    def test_factorio_path_settings_uses_typed_env_bindings(self):
        fields = FactorioPathSettings.env_fields()

        self.assertTrue(all(isinstance(field, BridgeRuntimeEnvField) for field in fields))
        self.assertIn("FACTORIOCTL_MCP_BIN", {field.env_name for field in fields})
        with mock.patch.object(
            FactorioPathSettings,
            "ENV_FIELDS",
            (
                BridgeRuntimeEnvField(env_name="A", field_name="server_data"),
                BridgeRuntimeEnvField(env_name="B", field_name="server_data"),
            ),
        ):
            with self.assertRaisesRegex(BridgeValidationError, "duplicate field_name"):
                FactorioPathSettings.env_fields()

    def test_factorio_mcp_server_config_renders_sdk_stdio_shape(self):
        config = FactorioMcpServerConfig(
            command="/tmp/mcp",
            args=["--debug", ""],
            rcon_host="127.0.0.1",
            rcon_port="27016",
            rcon_password="secret",
            agent_id="doug-nauvis",
        )
        bad_port = FactorioMcpServerConfig(command="/tmp/mcp", rcon_port="bad")

        self.assertEqual(config.to_sdk_config(), {
            "factorioctl": {
                "type": "stdio",
                "command": "/tmp/mcp",
                "args": ["--debug"],
                "env": {
                    "FACTORIO_RCON_HOST": "127.0.0.1",
                    "FACTORIO_RCON_PORT": "27016",
                    "FACTORIO_RCON_PASSWORD": "secret",
                    "FACTORIO_AGENT_ID": "doug-nauvis",
                },
            }
        })
        self.assertEqual(
            bad_port.to_sdk_config()["factorioctl"]["env"]["FACTORIO_RCON_PORT"],
            "27015",
        )
        with self.assertRaises(ValueError):
            FactorioMcpServerConfig(command="")

    def test_rcon_connection_settings_normalizes_env_and_env_payload(self):
        settings = RconConnectionSettings.from_env({
            "FACTORIO_RCON_HOST": "",
            "FACTORIO_RCON_PORT": "bad",
            "FACTORIO_RCON_PASSWORD": "",
        })
        configured = RconConnectionSettings.from_env({
            "FACTORIO_RCON_HOST": "127.0.0.1",
            "FACTORIO_RCON_PORT": "27016",
            "FACTORIO_RCON_PASSWORD": "secret",
        })

        self.assertEqual(settings.host, "localhost")
        self.assertEqual(settings.port, 27015)
        self.assertEqual(settings.password, "factorio")
        self.assertEqual(configured.port, 27016)
        self.assertEqual(configured.to_env(agent_id="doug"), {
            "FACTORIO_RCON_HOST": "127.0.0.1",
            "FACTORIO_RCON_PORT": "27016",
            "FACTORIO_RCON_PASSWORD": "secret",
            "FACTORIO_AGENT_ID": "doug",
        })
        self.assertIs(RconConnectionSettings.from_env(configured), configured)

    def test_rcon_connection_settings_uses_typed_env_bindings(self):
        fields = RconConnectionSettings.env_fields()

        self.assertTrue(all(isinstance(field, BridgeRuntimeEnvField) for field in fields))
        self.assertIn("FACTORIO_RCON_PASSWORD", {field.env_name for field in fields})
        with mock.patch.object(
            RconConnectionSettings,
            "ENV_FIELDS",
            (
                BridgeRuntimeEnvField(env_name="DUP", field_name="host"),
                BridgeRuntimeEnvField(env_name="DUP", field_name="password"),
            ),
        ):
            with self.assertRaisesRegex(BridgeValidationError, "duplicate env_name"):
                RconConnectionSettings.env_fields()

    def test_agent_profile_parses_file_text_at_model_boundary(self):
        profile = AgentProfile.from_file_text(json.dumps({
            "name": "doug",
            "system_prompt": "Build.",
            "group": "solo",
        }))

        self.assertEqual(profile.name, "doug")
        self.assertEqual(profile.group, "solo")
        self.assertIs(AgentProfile.from_file_text(profile), profile)

        with self.assertRaisesRegex(BridgeValidationError, "agent: expected object"):
            AgentProfile.from_file_text("[]")
        with self.assertRaisesRegex(BridgeValidationError, "agent: expected JSON object"):
            AgentProfile.from_file_text("{not-json")

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
            loaded = journal.load_events("doug")

        self.assertEqual(loaded, [{
            "ts": loaded[0]["ts"],
            "kind": "failure",
            "text": "classified failure",
        }])

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

    def test_ledger_update_infers_plan_ready_from_structured_plan(self):
        update = LedgerUpdate.coerce({
            "objective": "activate second furnace",
            "plan_steps": ["walk_to (42, -21)", "insert_items coal"],
            "progress": "plan confirmed",
        })

        self.assertEqual(update.signal, ProgressSignal.PLAN_READY)
        self.assertEqual(update.to_dict()["signal"], "plan_ready")

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

    def test_skill_definition_coerces_and_rejects_invalid_shapes(self):
        skill = SkillDefinition.coerce({
            "name": " feed_lab ",
            "params": [" lab_pos ", "", 42],
            "steps": [" place_entity lab ", None],
            "outcome": " lab consumes packs ",
        })

        self.assertIsNotNone(skill)
        self.assertEqual(skill.to_dict(), {
            "name": "feed_lab",
            "params": ["lab_pos"],
            "steps": ["place_entity lab"],
            "outcome": "lab consumes packs",
        })
        self.assertIsNone(SkillDefinition.coerce({"steps": ["missing name"]}))
        self.assertIsNone(SkillDefinition.coerce("oops"))

    def test_skill_definition_draft_parses_legacy_skill_body(self):
        text = """Before.
<skill>
name: feed_lab_fast
params: lab_pos, science_belt_pos
steps:
- place_entity lab at lab_pos
- route_belt science packs to science_belt_pos
outcome: lab consumes packs
</skill>
After.
"""
        draft = SkillDefinitionDraft.from_body(
            """name: feed_lab_fast
params: lab_pos, science_belt_pos
steps:
- place_entity lab at lab_pos
- route_belt science packs to science_belt_pos
outcome: lab consumes packs
"""
        )
        bullet_params = SkillDefinitionDraft.from_body(
            """name: feed_lab_fast
params:
- lab_pos
- science_belt_pos
steps:
- place_entity lab at lab_pos
outcome: lab consumes packs
"""
        )
        skill = SkillDefinition.from_trailer_text(text)

        self.assertEqual(draft.name, "feed_lab_fast")
        self.assertEqual(draft.params, ["lab_pos", "science_belt_pos"])
        self.assertEqual(bullet_params.params, ["lab_pos", "science_belt_pos"])
        self.assertEqual(
            draft.steps,
            [
                "place_entity lab at lab_pos",
                "route_belt science packs to science_belt_pos",
            ],
        )
        self.assertEqual(draft.outcome, "lab consumes packs")
        self.assertEqual(draft.to_skill().name, "feed_lab_fast")
        self.assertIsNotNone(skill)
        self.assertEqual(skill.name, "feed_lab_fast")
        self.assertEqual(skill.params, ["lab_pos", "science_belt_pos"])
        self.assertIs(SkillDefinitionDraft.from_body(draft), draft)
        self.assertEqual(SkillDefinitionDraft.from_body(skill), draft)
        self.assertIs(SkillDefinition.from_trailer_text(skill), skill)
        self.assertEqual(SkillDefinition.from_trailer_text(draft), skill)
        self.assertEqual(SkillDefinition.strip_trailer_text(text), "Before.\n\nAfter.")

    def test_skill_library_deduplicates_and_limits_typed_entries(self):
        typed_old = SkillDefinition(
            name="old",
            steps=["typed old"],
        )
        typed_replacement = SkillDefinition(
            name="old",
            steps=["typed replacement"],
        )
        library = SkillLibrary.coerce({
            "skills": [
                {"name": "old", "steps": ["a"]},
                {"name": "", "steps": ["bad"]},
                {"name": "old", "steps": ["replacement"]},
                {"name": "new", "params": ["x"]},
            ],
        }, max_skills=2)

        self.assertEqual(library.to_dict(), {
            "skills": [
                {"name": "old", "params": [], "steps": ["replacement"], "outcome": ""},
                {"name": "new", "params": ["x"], "steps": [], "outcome": ""},
            ],
        })
        self.assertEqual(
            SkillLibrary.normalized(library, max_skills=1).to_dict(),
            {"skills": [{"name": "old", "params": [], "steps": ["replacement"], "outcome": ""}]},
        )
        self.assertEqual(
            SkillLibrary.from_file_text(library, max_skills=1).to_dict(),
            {"skills": [{"name": "old", "params": [], "steps": ["replacement"], "outcome": ""}]},
        )
        typed_library = SkillLibrary.coerce(
            [typed_old, {"name": "new", "params": ["x"]}, typed_replacement],
        )
        self.assertEqual(
            typed_library.to_dict(),
            {
                "skills": [
                    {
                        "name": "new",
                        "params": ["x"],
                        "steps": [],
                        "outcome": "",
                    },
                    {
                        "name": "old",
                        "params": [],
                        "steps": ["typed replacement"],
                        "outcome": "",
                    },
                ],
            },
        )
        self.assertEqual(
            SkillLibrary.normalized(typed_library, max_skills=1).to_dict(),
            {
                "skills": [{
                    "name": "new",
                    "params": ["x"],
                    "steps": [],
                    "outcome": "",
                }],
            },
        )
        self.assertEqual(SkillLibrary.normalized("oops").to_dict(), {"skills": []})

    def test_skill_definition_collection_normalizes_structured_inputs(self):
        typed = SkillDefinition(name="typed", steps=["typed step"])
        draft = SkillDefinitionDraft(
            name="draft",
            params=["x"],
            steps=["draft step"],
        )
        collection = SkillDefinitionCollection.from_value((
            typed,
            draft,
            {"name": "mapped", "steps": ["mapped step"]},
            {"name": "", "steps": ["bad"]},
            "bad input",
        ))
        generator_library = SkillLibrary.coerce(
            item
            for item in [
                {"name": "gen", "steps": ["generator step"]},
                {"name": "gen", "steps": ["replacement"]},
                {"name": "tail", "steps": ["tail step"]},
            ]
        )

        class LibraryLike:
            skills = (
                {"name": "like", "steps": ["tuple skill"]},
            )

        self.assertEqual(
            [skill.name for skill in collection.to_list()],
            ["typed", "draft", "mapped"],
        )
        self.assertEqual(
            [skill.name for skill in SkillDefinitionCollection.from_value(LibraryLike()).skills],
            ["like"],
        )
        self.assertIs(SkillDefinitionCollection.from_value(collection), collection)
        self.assertEqual(
            generator_library.to_dict(),
            {
                "skills": [
                    {"name": "gen", "params": [], "steps": ["replacement"], "outcome": ""},
                    {"name": "tail", "params": [], "steps": ["tail step"], "outcome": ""},
                ],
            },
        )

    def test_skill_library_parses_file_merges_and_replaces_entries(self):
        starters = SkillLibrary.coerce({
            "skills": [
                {"name": "build", "steps": ["old"], "outcome": "old"},
                {"name": "feed", "params": ["lab"]},
            ],
        })
        saved = SkillLibrary.from_file_text(
            '{"skills": ['
            '{"name": "build", "steps": ["replacement"], "outcome": "new"},'
            '{"name": "smelt", "steps": ["place furnace"]}'
            ']}'
        )
        extra = SkillDefinition.coerce({
            "name": "power",
            "steps": ["place boiler"],
        })
        self.assertIsNotNone(extra)

        merged = starters.merged_with(saved, max_skills=3)
        replaced = merged.replace_or_append(extra, max_skills=3)

        self.assertEqual(
            [skill.name for skill in merged.skills],
            ["build", "feed", "smelt"],
        )
        self.assertEqual(merged.skills[0].steps, ["replacement"])
        self.assertEqual(
            [skill.name for skill in replaced.skills],
            ["feed", "smelt", "power"],
        )
        self.assertEqual(
            replaced.to_json_line(),
            (
                '{"skills":[{"name":"feed","params":["lab"],"steps":[],"outcome":""},'
                '{"name":"smelt","params":[],"steps":["place furnace"],"outcome":""},'
                '{"name":"power","params":[],"steps":["place boiler"],"outcome":""}]}\n'
            ),
        )

        with self.assertRaisesRegex(BridgeValidationError, "skills"):
            SkillLibrary.from_file_text("{not json")

    def test_skill_library_owns_lookup_rendering_and_sparse_serialization(self):
        skill = SkillDefinition.coerce({
            "name": "feed_lab",
            "params": ["lab_pos", "belt_pos"],
            "steps": ["place lab", "feed science"],
            "outcome": "lab researches",
        })
        library = SkillLibrary(skills=[skill])

        self.assertEqual(skill.signature(), "feed_lab(lab_pos, belt_pos)")
        self.assertEqual(
            skill.prompt_summary_line(),
            "- feed_lab(lab_pos, belt_pos) — lab researches",
        )
        self.assertEqual(
            SkillDefinition.coerce({"name": "scout"}).to_sparse_dict(),
            {"name": "scout"},
        )
        self.assertIs(library.get("feed_lab"), skill)
        self.assertIsNone(library.get(None))
        rendered = library.render_prompt()
        self.assertIn("Available skills", rendered)
        self.assertIn("feed_lab(lab_pos, belt_pos)", rendered)
        self.assertNotIn("place lab", rendered)

    def test_learning_proposal_validates_field_specific_shape_errors(self):
        base = {
            "kind": "skill_proposal",
            "status": "pending",
            "agent": "doug",
            "name": "repair_power",
            "steps": ["inspect poles"],
        }

        with self.assertRaisesRegex(BridgeValidationError, "kind: expected one of"):
            LearningProposal.from_mapping({**base, "kind": "note"})

        with self.assertRaisesRegex(BridgeValidationError, "steps: expected list of strings"):
            LearningProposal.from_mapping({**base, "steps": {"bad": True}})

        with self.assertRaisesRegex(BridgeValidationError, r"acceptance_tests\[0\]: expected string"):
            LearningProposal.from_mapping({
                **base,
                "acceptance_tests": [{"bad": True}],
            })

    def test_learning_proposal_coerce_preserves_file_shape_and_fallbacks(self):
        proposal = LearningProposal.coerce(
            {
                "kind": "unknown",
                "status": "maybe",
                "agent": "",
                "problem": "steam plant exists but lab is dark",
                "steps": "inspect pole coverage",
                "anti_steps": ["do not rebuild power", 123],
                "future_field": {"kept": True},
            },
            agent_name="doug",
            status="accepted",
        )

        data = proposal.to_dict()

        self.assertEqual(data["schema_version"], 1)
        self.assertEqual(data["status"], "accepted")
        self.assertEqual(data["kind"], "skill_proposal")
        self.assertEqual(data["agent"], "doug")
        self.assertEqual(data["name"], "steam plant exists but lab is dark")
        self.assertEqual(data["steps"], ["inspect pole coverage"])
        self.assertEqual(data["anti_steps"], ["do not rebuild power"])
        self.assertEqual(data["future_field"], {"kept": True})
        self.assertTrue(proposal.is_meaningful())

    def test_learning_proposal_preserves_typed_instances_and_input_status(self):
        proposal = LearningProposal.coerce({
            "kind": "bug_report",
            "status": "accepted",
            "agent": "doug",
            "name": "belt_gap",
            "steps": ["run analyze_belt_gaps"],
        })

        self.assertEqual(proposal.status, "accepted")
        self.assertIs(LearningProposal.from_mapping(proposal), proposal)
        self.assertIs(LearningProposal.coerce(proposal), proposal)
        self.assertEqual(
            LearningProposal.coerce(proposal, agent_name="ada", status="pending").to_dict(),
            {
                **proposal.to_dict(),
                "agent": "ada",
                "status": "pending",
            },
        )

    def test_learning_proposal_owns_slug_and_accepted_memory_rendering(self):
        proposal = LearningProposal.coerce(
            {
                "kind": "skill_proposal",
                "agent": "Doug Nauvis",
                "name": "Repair Steam Power!",
                "trigger": "lab has no power",
                "steps": ["inspect poles", "fuel boiler", "verify production", "extra"],
                "anti_steps": ["do not rebuild pump", "do not move engine", "extra"],
            },
            status="accepted",
        )
        evidence_only = LearningProposal.coerce(
            {
                "kind": "bug_report",
                "name": "belt_gap",
                "evidence": ["route_belt failed near drill", "extra"],
            },
            status="accepted",
        )

        self.assertEqual(
            LearningProposal.safe_slug(" Repair Steam Power! "),
            "repair-steam-power",
        )
        self.assertEqual(LearningProposal.safe_slug("", fallback="agent"), "agent")
        self.assertEqual(
            proposal.accepted_memory_line(max_steps=3, max_anti_steps=2),
            "- Repair Steam Power!: when lab has no power; "
            "do inspect poles; fuel boiler; verify production; "
            "avoid do not rebuild pump; do not move engine",
        )
        self.assertEqual(
            evidence_only.accepted_memory_line(),
            "- belt_gap: route_belt failed near drill",
        )

    def test_learning_proposal_candidate_model_applies_agent_and_status_defaults(self):
        raw = {
            "kind": "bug_report",
            "name": "belt_gap",
            "steps": ["run analyze_belt_gaps"],
        }
        proposal = LearningProposal.candidate_model(
            raw,
            agent_name="doug",
            status="accepted",
        )
        typed = LearningProposal.coerce(raw, agent_name="old", status="pending")

        self.assertIsNone(LearningProposal.candidate_model("bad input"))
        self.assertEqual(proposal.agent, "doug")
        self.assertEqual(proposal.status, "accepted")
        self.assertEqual(
            LearningProposal.candidate_model(typed, agent_name="new").agent,
            "new",
        )
        self.assertEqual(
            LearningProposal.candidate_model(typed, status="accepted").status,
            "accepted",
        )

    def test_learning_proposal_collection_normalizes_structured_inputs(self):
        typed = LearningProposal.coerce({
            "kind": "bug_report",
            "name": "belt_gap",
            "steps": ["run analyze_belt_gaps"],
        })
        draft = LearningProposalDraft(
            kind="diagnostic_proposal",
            name="inspect_power",
            problem="power status is ambiguous",
            steps=["call diagnose_steam_power"],
        )
        collection = LearningProposalCollection.from_value((
            typed,
            draft,
            {"name": "dict_gap", "steps": ["coerce mapping"]},
            "bad input",
            {"name": "not_meaningful"},
        ))

        self.assertEqual(
            [proposal.name for proposal in collection.to_list()],
            ["belt_gap", "inspect_power", "dict_gap"],
        )
        self.assertEqual(
            [proposal["name"] for proposal in collection.to_dicts()],
            ["belt_gap", "inspect_power", "dict_gap"],
        )
        self.assertEqual(LearningProposalCollection.from_value(None).to_list(), [])
        self.assertIs(LearningProposalCollection.from_value(collection), collection)

    def test_learning_proposal_parses_file_text_and_serializes_json_text(self):
        proposal = LearningProposal.from_file_text(
            '{"agent":"doug","kind":"bug_report","name":"belt_gap",'
            '"steps":["run analyze_belt_gaps"],"future_field":{"kept":true}}',
            default_status="accepted",
        )
        same_content = LearningProposal.coerce(
            {
                **proposal.to_dict(),
                "status": "pending",
                "content_hash": "ignored-for-hash",
            },
        )

        self.assertEqual(proposal.status, "accepted")
        self.assertEqual(proposal.agent, "doug")
        self.assertEqual(proposal.extra, {"future_field": {"kept": True}})
        self.assertEqual(
            json.loads(proposal.to_json_text())["future_field"],
            {"kept": True},
        )
        self.assertEqual(
            same_content.hash_payload_json(),
            proposal.hash_payload_json(),
        )
        self.assertEqual(
            same_content.stable_content_hash(),
            proposal.stable_content_hash(),
        )
        self.assertEqual(len(proposal.stable_content_hash()), 16)
        self.assertIs(LearningProposal.from_file_text(proposal), proposal)

        with self.assertRaisesRegex(BridgeValidationError, "learning_proposal"):
            LearningProposal.from_file_text("[1, 2, 3]")

    def test_learning_proposal_draft_parses_hidden_trailer_body(self):
        body = """title: belt_direction_mismatch
summary: belts point away from the inserter
preconditions: inspect belt reach, inspect inserters
steps:
- run analyze_belt_reach
- rotate only the wrong belt
avoid:
- do not rebuild the miner first
anti-steps:
- do not place duplicate belts
acceptance:
- verify_production reports furnace working
"""
        draft = LearningProposalDraft.from_tag_body(
            "bug_report",
            body,
        )
        builder = LearningProposalDraftBodyBuilder.from_body("bug_report", body)
        proposal = LearningProposal.from_tag_body("bug_report", body)
        text = (
            "Before.\n\n"
            f"<bug_report>\n{body}</bug_report>\n\n"
            "<script_proposal>\n"
            "name: inspect_power\n"
            "problem: verify current grid before rebuilding\n"
            "steps:\n"
            "- call diagnose_steam_power\n"
            "</script_proposal>\n\n"
            "After."
        )
        proposals = LearningProposal.all_from_trailer_text(text)

        self.assertEqual(draft.kind, "bug_report")
        self.assertEqual(builder.active_key, "acceptance_tests")
        self.assertEqual(builder.to_draft(), draft)
        self.assertEqual(draft.name, "belt_direction_mismatch")
        self.assertEqual(draft.problem, "belts point away from the inserter")
        self.assertEqual(
            draft.preconditions,
            ["inspect belt reach", "inspect inserters"],
        )
        self.assertEqual(
            draft.steps,
            ["run analyze_belt_reach", "rotate only the wrong belt"],
        )
        self.assertEqual(
            draft.anti_steps,
            ["do not rebuild the miner first", "do not place duplicate belts"],
        )
        self.assertEqual(
            draft.acceptance_tests,
            ["verify_production reports furnace working"],
        )
        self.assertIs(LearningProposalDraft.from_tag_body("bug_report", draft), draft)
        self.assertEqual(draft.to_proposal().kind, "bug_report")
        self.assertEqual(proposal.kind, "bug_report")
        self.assertEqual(proposal.name, "belt_direction_mismatch")
        self.assertEqual(
            LearningProposalDraft.from_tag_body("script_proposal", proposal).kind,
            "script_proposal",
        )
        self.assertEqual(
            LearningProposalDraft.from_tag_body("script_proposal", proposal).steps,
            proposal.steps,
        )
        self.assertEqual(
            proposal.steps,
            ["run analyze_belt_reach", "rotate only the wrong belt"],
        )
        self.assertTrue(proposal.is_meaningful())
        self.assertEqual(
            [(item.kind, item.name) for item in proposals],
            [
                ("bug_report", "belt_direction_mismatch"),
                ("script_proposal", "inspect_power"),
            ],
        )
        self.assertEqual(
            LearningProposal.strip_trailer_text(text),
            "Before.\n\nAfter.",
        )

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
        scan = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__situation_report",
        })
        diagnostic = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__diagnose_factory_blockers",
        })
        item_flow = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__analyze_item_flow",
        })
        dry_run_feed = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__feed_lab_from_inventory",
            "tool_input": {"dry_run": True},
        })
        active_feed = ToolCallRequest.from_hook_input({
            "tool_name": "mcp__factorioctl__feed_lab_from_inventory",
            "tool_input": {"dry_run": False},
        })
        non_factorio = ToolCallRequest.from_hook_input({"tool_name": "Skill"})

        self.assertEqual(placement.short_name, "place_entity")
        self.assertTrue(placement.is_factorio_mcp_tool)
        self.assertTrue(placement.is_mutating_factorio_tool)
        self.assertFalse(placement.is_read_only_factorio_tool)
        self.assertTrue(scan.is_read_only_factorio_tool)
        self.assertFalse(scan.is_mutating_factorio_tool)
        self.assertTrue(diagnostic.is_read_only_factorio_tool)
        self.assertFalse(diagnostic.is_mutating_factorio_tool)
        self.assertTrue(item_flow.is_read_only_factorio_tool)
        self.assertFalse(item_flow.is_mutating_factorio_tool)
        self.assertTrue(dry_run_feed.is_read_only_dry_run)
        self.assertFalse(active_feed.is_read_only_dry_run)
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


if __name__ == "__main__":
    unittest.main()
