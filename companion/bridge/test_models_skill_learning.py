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



class ModelSkillLearningTests(unittest.TestCase):

    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.base = Path(self.tempdir.name)

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
