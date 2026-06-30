import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import journal
from models import (
    AgentProfile,
    BridgeValidationError,
    JournalEvent,
    LearningProposal,
    LedgerState,
    TOOL_PARAM_INTEGER,
    TOOL_PARAM_NUMBER,
    TOOL_PARAM_STRING,
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
        profile = AgentProfile.from_mapping({
            "name": "doug",
            "system_prompt": "Build.",
            "model": "haiku",
            "max_turns": 200,
            "planet": "nauvis",
            "sdk_skills": ["factorio-control"],
            "response_format": {"header_label": "STATUS"},
            "future_field": "kept for compatibility",
        })

        data = profile.to_dict()

        self.assertEqual(data["name"], "doug")
        self.assertEqual(data["system_prompt"], "Build.")
        self.assertEqual(data["max_turns"], 200)
        self.assertEqual(data["sdk_skills"], ["factorio-control"])
        self.assertEqual(data["future_field"], "kept for compatibility")

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


if __name__ == "__main__":
    unittest.main()
