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



class ModelAgentConfigTests(unittest.TestCase):

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
            autonomy_step_progress=" autonomy_step_complete: route_belt ok ",
        )

        self.assertEqual(transcript.text_parts, ["first", "3"])
        self.assertEqual(transcript.session_id, "session-1")
        self.assertTrue(transcript.context_window_limit)
        self.assertTrue(transcript.usage_limit_seen)
        self.assertEqual(
            transcript.autonomy_step_progress,
            "autonomy_step_complete: route_belt ok",
        )
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
