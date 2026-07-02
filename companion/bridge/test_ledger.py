import importlib
import tempfile
import unittest
from datetime import datetime, timedelta
from pathlib import Path
from unittest import mock

import journal
import ledger
from models import LedgerState, LedgerUpdate, ProgressSignal


class LedgerTests(unittest.TestCase):
    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.base = Path(self.tempdir.name)
        self.file_patch = mock.patch(
            "ledger._ledger_file",
            side_effect=lambda agent_name: self.base / f".ledger-{agent_name}.json",
        )
        self.file_patch.start()
        self.addCleanup(self.file_patch.stop)
        self.journal_patch = mock.patch(
            "journal._journal_file",
            side_effect=lambda agent_name: self.base / f".journal-{agent_name}.jsonl",
        )
        self.reflection_patch = mock.patch(
            "journal._reflection_file",
            side_effect=lambda agent_name: self.base / f".reflection-{agent_name}.json",
        )
        self.journal_patch.start()
        self.reflection_patch.start()
        self.addCleanup(self.journal_patch.stop)
        self.addCleanup(self.reflection_patch.stop)

    def test_parse_well_formed_ledger_trailer(self):
        text = """Done.
<ledger>
objective: Build iron automation
plan:
- Mine iron ore
- Feed a furnace
progress: Placed the first drill
</ledger>
"""

        parsed = ledger.parse_ledger_trailer(text)

        self.assertEqual(parsed["objective"], "Build iron automation")
        self.assertEqual(parsed["plan_steps"], ["Mine iron ore", "Feed a furnace"])
        self.assertEqual(parsed["progress"], "Placed the first drill")

    def test_parse_ledger_trailer_model_returns_typed_update(self):
        parsed = ledger.parse_ledger_trailer_model(
            "<ledger>\n"
            "objective: Activate second furnace\n"
            "plan:\n"
            "- walk_to (42, -21)\n"
            "progress: previous objective complete; new objective selected\n"
            "signal: new_objective\n"
            "</ledger>"
        )

        self.assertIsInstance(parsed, LedgerUpdate)
        self.assertEqual(parsed.objective, "Activate second furnace")
        self.assertEqual(parsed.plan_steps, ["walk_to (42, -21)"])
        self.assertEqual(parsed.progress, "previous objective complete; new objective selected")
        self.assertEqual(parsed.signal, ProgressSignal.NEW_OBJECTIVE)

        typed = LedgerUpdate(
            objective="Typed objective",
            plan_steps=["typed step"],
            progress="typed progress",
            signal=ProgressSignal.PLAN_DONE,
        )
        self.assertIs(ledger.parse_ledger_trailer_model(typed), typed)

    def test_parse_returns_none_without_ledger_block(self):
        self.assertIsNone(ledger.parse_ledger_trailer("Plain narration only."))
        self.assertIsNone(ledger.parse_ledger_trailer_model("Plain narration only."))

    def test_parse_tolerates_missing_keys(self):
        parsed = ledger.parse_ledger_trailer("<ledger>\nprogress: Checked the belt.\n</ledger>")

        self.assertEqual(parsed, {"progress": "Checked the belt."})

    def test_parse_ledger_trailer_carries_structured_signal(self):
        parsed = ledger.parse_ledger_trailer(
            "<ledger>\n"
            "objective: Activate second furnace\n"
            "plan:\n"
            "- walk_to (42, -21)\n"
            "progress: previous objective complete; new objective selected\n"
            "signal: new_objective\n"
            "</ledger>"
        )

        self.assertEqual(parsed["objective"], "Activate second furnace")
        self.assertEqual(parsed["signal"], "new_objective")

    def test_parse_ledger_trailer_infers_plan_ready_for_omitted_signal(self):
        parsed = ledger.parse_ledger_trailer(
            "<ledger>\n"
            "objective: Activate second furnace\n"
            "plan:\n"
            "- walk_to (42, -21)\n"
            "- insert_items coal count=5 into fuel inventory of unit 15\n"
            "progress: plan confirmed; awaiting execution\n"
            "</ledger>"
        )

        self.assertEqual(parsed["signal"], "plan_ready")

    def test_load_returns_default_for_missing_and_corrupt_file(self):
        self.assertEqual(ledger.load_ledger("doug"), ledger.default_ledger())
        self.assertIsInstance(ledger.load_ledger_model("doug"), LedgerState)

        (self.base / ".ledger-doug.json").write_text("{not json")

        self.assertEqual(ledger.load_ledger("doug"), ledger.default_ledger())
        self.assertEqual(ledger.load_ledger_model("doug"), ledger.default_ledger_model())

    def test_save_then_load_round_trips(self):
        saved = {
            "objective": "Automate copper",
            "plan_steps": ["Place miner", "Place furnace"],
            "progress_notes": ["Found copper patch"],
            "updated_at": "2026-06-27T12:00:00",
        }

        ledger.save_ledger("doug", saved)

        self.assertEqual(ledger.load_ledger("doug"), saved)
        self.assertEqual(ledger.load_ledger_model("doug").to_dict(), saved)
        self.assertTrue((self.base / ".ledger-doug.json").read_text().endswith("\n"))

    def test_model_ledger_api_round_trips_without_dict_churn(self):
        saved = LedgerState(
            objective="Automate iron",
            plan_steps=["Place drill", "Route belt"],
            progress_notes=["Found patch"],
            updated_at="2026-06-30T04:22:28",
        )

        ledger.save_ledger_model("doug", saved)
        loaded = ledger.load_ledger_model("doug")

        self.assertIsInstance(loaded, LedgerState)
        self.assertEqual(loaded, saved)

        updated = ledger.apply_ledger_update_model(
            "doug",
            LedgerUpdate(progress="belt fixed", signal=ProgressSignal.PLAN_DONE),
        )

        self.assertIsInstance(updated, LedgerState)
        self.assertEqual(updated.objective, "Automate iron")
        self.assertEqual(updated.plan_steps, ["Place drill", "Route belt"])
        self.assertEqual(updated.progress_notes, ["Found patch", "belt fixed"])
        self.assertEqual(ledger.load_ledger("doug"), updated.to_dict())

    def test_load_and_apply_preserve_planning_progress_without_signal(self):
        saved = {
            "objective": "Repair existing power",
            "plan_steps": ["fuel boiler"],
            "progress_notes": [
                "Found boiler unit 49 with no fuel",
                "no change across fifty-four planning ticks. State stable. "
                "Plan fully validated and awaiting execution turns.",
            ],
            "updated_at": "2026-06-30T04:22:28",
        }

        ledger.save_ledger("doug", saved)

        loaded = ledger.load_ledger("doug")
        self.assertEqual(loaded["progress_notes"], saved["progress_notes"])

        updated = ledger.apply_ledger_update(
            "doug",
            "<ledger>\n"
            "progress: no changes across planning ticks; plan validated and ready for execution\n"
            "</ledger>",
        )

        self.assertEqual(updated["progress_notes"], [
            "Found boiler unit 49 with no fuel",
            "no change across fifty-four planning ticks. State stable. "
            "Plan fully validated and awaiting execution turns.",
            "no changes across planning ticks; plan validated and ready for execution",
        ])

    def test_stale_bootstrap_ledger_returns_default(self):
        stale = {
            "objective": "Establish initial extraction infrastructure on iron patch",
            "plan_steps": ["place_entity burner-mining-drill on iron ore"],
            "progress_notes": ["situation assessed; no infrastructure yet deployed"],
            "updated_at": (datetime.now() - timedelta(hours=2)).isoformat(),
        }
        recent = {
            **stale,
            "updated_at": datetime.now().isoformat(),
        }

        with mock.patch.dict(
            "os.environ",
            {"BRIDGE_STALE_BOOTSTRAP_LEDGER_MAX_AGE_S": "60"},
        ):
            ledger.save_ledger("doug", stale)
            self.assertEqual(ledger.load_ledger("doug"), ledger.default_ledger())

            ledger.save_ledger("doug", recent)
            self.assertEqual(ledger.load_ledger("doug"), recent)

    def test_stale_bootstrap_ledger_bad_env_uses_typed_default(self):
        stale = {
            "objective": "Establish initial extraction infrastructure on iron patch",
            "plan_steps": ["place_entity burner-mining-drill on iron ore"],
            "progress_notes": ["situation assessed; no infrastructure yet deployed"],
            "updated_at": (datetime.now() - timedelta(hours=2)).isoformat(),
        }

        with mock.patch.dict(
            "os.environ",
            {"BRIDGE_STALE_BOOTSTRAP_LEDGER_MAX_AGE_S": "not-a-number"},
        ):
            ledger.save_ledger("doug", stale)
            self.assertEqual(ledger.load_ledger("doug"), ledger.default_ledger())

    def test_apply_update_replaces_plan_for_new_objective_and_caps_progress(self):
        for i in range(11):
            ledger.apply_ledger_update(
                "doug",
                f"<ledger>\nprogress: progress {i}\n</ledger>",
            )

        updated = ledger.apply_ledger_update(
            "doug",
            """<ledger>
objective: Build starter science
plan:
- Feed labs
- Craft red science
progress: Started the science plan
</ledger>""",
        )

        self.assertEqual(updated["objective"], "Build starter science")
        self.assertEqual(updated["plan_steps"], ["Feed labs", "Craft red science"])
        self.assertEqual(len(updated["progress_notes"]), 10)
        self.assertEqual(updated["progress_notes"][0], "progress 2")
        self.assertEqual(updated["progress_notes"][-1], "Started the science plan")
        self.assertTrue(updated["updated_at"])

    def test_strip_ledger_trailer_removes_block_and_keeps_narration(self):
        text = "Before.\n\n<ledger>\nprogress: Hidden\n</ledger>\n\nAfter."

        stripped = ledger.strip_ledger_trailer(text)

        self.assertEqual(stripped, "Before.\n\nAfter.")
        self.assertEqual(ledger.strip_ledger_trailer("No trailer."), "No trailer.")

    def test_render_ledger_empty_and_with_objective(self):
        self.assertEqual(ledger.render_ledger(ledger.default_ledger()), "")

        rendered = ledger.render_ledger({
            "objective": "Repair power",
            "plan_steps": ["Find break", "Replace pole"],
            "progress_notes": ["Reached the blackout area"],
            "updated_at": "now",
        })

        self.assertIn("Repair power", rendered)
        self.assertIn("1. Find break", rendered)
        self.assertIn("Reached the blackout area", rendered)

    def test_autonomy_prompts_keep_continuity_and_ledger_protocol(self):
        planner = importlib.import_module("planner")

        # Invariants that BOTH prompts must preserve (hfb continuity + ledger
        # protocol, u42 situation_report-over-three-scans guidance).
        for prompt in (planner.PLANNER_PROMPT, planner.EXECUTION_PROMPT):
            self.assertNotIn("check your current situation", prompt)
            self.assertIn("continuity", prompt.lower())
            self.assertIn("<ledger>", prompt)
            self.assertIn("situation_report", prompt)

        # The planner deliberates (sets objective/plan); execution does not
        # re-plan.
        self.assertIn("unless it is finished or impossible", planner.PLANNER_PROMPT)
        self.assertIn("do not re-plan", planner.EXECUTION_PROMPT.lower())

    def test_load_normalizes_null_fields_and_apply_does_not_raise(self):
        # A ledger persisted with null lists must not crash the next update.
        (self.base / ".ledger-doug.json").write_text(
            '{"objective": null, "plan_steps": null, "progress_notes": null}'
        )

        loaded = ledger.load_ledger("doug")
        self.assertEqual(loaded["plan_steps"], [])
        self.assertEqual(loaded["progress_notes"], [])
        self.assertEqual(loaded["objective"], "")

        # Would raise TypeError (list(None)) before the normalize fix.
        updated = ledger.apply_ledger_update(
            "doug", "<ledger>\nprogress: still fine\n</ledger>"
        )
        self.assertEqual(updated["progress_notes"], ["still fine"])

    def test_load_returns_default_for_non_utf8_file(self):
        # UnicodeDecodeError is a ValueError subclass and must be swallowed.
        (self.base / ".ledger-doug.json").write_bytes(b"\xff\xfe\x00\x01")

        self.assertEqual(ledger.load_ledger("doug"), ledger.default_ledger())

    def test_finalize_reply_guards_ledger_only_and_persists(self):
        # The real F2 seam: a ledger-only reply must finalize to a non-empty
        # placeholder AND the ledger must still be persisted. Deleting the guard
        # in _finalize_reply makes this return "" and fail.
        pipe = importlib.import_module("pipe")

        finalized = pipe._finalize_reply(
            "<ledger>\nobjective: Smelt iron\nprogress: placed a furnace\n</ledger>",
            "doug",
        )

        self.assertEqual(finalized, "(action complete)")
        self.assertEqual(ledger.load_ledger("doug")["objective"], "Smelt iron")

    def test_finalize_reply_keeps_narration_and_strips_block(self):
        pipe = importlib.import_module("pipe")

        finalized = pipe._finalize_reply(
            "Heading to the iron patch.\n\n<ledger>\nprogress: walking\n</ledger>",
            "doug",
        )

        self.assertEqual(finalized, "Heading to the iron patch.")

    def test_save_is_atomic_and_leaves_no_tmp(self):
        ledger.save_ledger("doug", ledger.default_ledger())

        self.assertFalse((self.base / ".ledger-doug.json.tmp").exists())
        self.assertTrue((self.base / ".ledger-doug.json").exists())

    def test_compose_autonomy_prompt_injects_ledger(self):
        # Prove the persisted objective is actually injected on autonomy ticks,
        # not just that the prompt copy contains <ledger>.
        pipe = importlib.import_module("pipe")
        ledger.save_ledger("doug", {
            "objective": "Build a smelting column",
            "plan_steps": ["Place furnaces", "Lay the belt"],
            "progress_notes": ["Cleared the build site"],
            "updated_at": "now",
        })

        class StubRCON:
            def execute(self, _cmd):
                return ""

        thread = pipe.AgentThread.__new__(pipe.AgentThread)
        thread.agent_name = "doug"
        thread.rcon = StubRCON()
        thread._exec_ticks_since_plan = 0
        thread._planner_interval = 5
        thread._planner_model = None

        prompt = thread._compose_autonomy_prompt()

        self.assertIn("Build a smelting column", prompt)
        self.assertIn("Do the next unfinished step", prompt)


if __name__ == "__main__":
    unittest.main()
