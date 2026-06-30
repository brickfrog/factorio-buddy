import tempfile
import unittest
from pathlib import Path
from unittest import mock

import ledger
import pipe
import planner


class PlannerTests(unittest.TestCase):
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

    def test_choose_autonomy_mode_plans_without_objective(self):
        self.assertEqual(
            planner.choose_autonomy_mode(ledger.default_ledger(), 0, 5),
            "plan",
        )

    def test_choose_autonomy_mode_plans_without_plan_steps(self):
        self.assertEqual(
            planner.choose_autonomy_mode(
                {
                    "objective": "Build starter power",
                    "plan_steps": [],
                    "progress_notes": [],
                    "updated_at": "",
                },
                0,
                5,
            ),
            "plan",
        )

    def test_choose_autonomy_mode_executes_before_interval(self):
        self.assertEqual(
            planner.choose_autonomy_mode(
                {
                    "objective": "Build starter power",
                    "plan_steps": ["Place boiler"],
                    "progress_notes": [],
                    "updated_at": "",
                },
                4,
                5,
            ),
            "execute",
        )

    def test_choose_autonomy_mode_plans_at_interval(self):
        self.assertEqual(
            planner.choose_autonomy_mode(
                {
                    "objective": "Build starter power",
                    "plan_steps": ["Place boiler"],
                    "progress_notes": [],
                    "updated_at": "",
                },
                5,
                5,
            ),
            "plan",
        )

    def test_live_state_entity_counts_parses_player_entity_summary(self):
        self.assertEqual(
            planner.live_state_entity_counts(
                "Live state: nauvis @ 46.7,-15.6; player entities: "
                "offshore-pump=1, boiler=1, steam-engine=1, lab=1, "
                "small-electric-pole=15",
            ),
            {
                "offshore-pump": 1,
                "boiler": 1,
                "steam-engine": 1,
                "lab": 1,
                "small-electric-pole": 15,
            },
        )

    def test_objective_satisfied_by_live_state_detects_completed_power_lab(self):
        reason = planner.objective_satisfied_by_live_state(
            {
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
            },
            "Live state: nauvis @ 46.7,-15.6; player entities: "
            "offshore-pump=1, boiler=1, steam-engine=1, lab=1, "
            "small-electric-pole=15",
        )

        self.assertIn("steam power", reason)

    def test_objective_satisfied_by_live_state_ignores_unrelated_objective(self):
        reason = planner.objective_satisfied_by_live_state(
            {
                "objective": "build automated iron smelting array",
                "plan_steps": ["route belt", "place inserter"],
                "progress_notes": [],
                "updated_at": "now",
            },
            "Live state: nauvis @ 46.7,-15.6; player entities: "
            "offshore-pump=1, boiler=1, steam-engine=1, lab=1",
        )

        self.assertEqual(reason, "")

    def test_objective_satisfied_by_live_state_ignores_existing_plant_repair(self):
        reason = planner.objective_satisfied_by_live_state(
            {
                "objective": (
                    "energize the power grid to activate the existing automated "
                    "iron smelting array"
                ),
                "plan_steps": [
                    "walk_to(-41, 26) to reach the boiler at the steam plant",
                    "insert_items(unit 49, 'coal', 5, 'fuel')",
                    "place_entity('small-electric-pole', -8.5, 12.5)",
                    "verify_production at (56, -22) radius 25",
                ],
                "progress_notes": [
                    "state stable; plan validated and awaiting execution turns",
                ],
                "updated_at": "now",
            },
            "Live state: nauvis @ 55.4,-20.4; player entities: "
            "offshore-pump=1, boiler=1, steam-engine=1, lab=1, "
            "small-electric-pole=21",
        )

        self.assertEqual(reason, "")

    def test_prompt_constants_keep_planner_and_execution_contracts(self):
        self.assertIn("3-6 step plan", planner.PLANNER_PROMPT.lower())
        self.assertIn("plan", planner.PLANNER_PROMPT.lower())
        self.assertIn("<ledger>", planner.PLANNER_PROMPT)
        self.assertIn("situation_report", planner.PLANNER_PROMPT)
        self.assertIn("live state is authoritative", planner.PLANNER_PROMPT.lower())
        self.assertIn("local absence", planner.PLANNER_PROMPT.lower())
        self.assertIn("target site", planner.PLANNER_PROMPT.lower())
        self.assertIn("read-only planning turn", planner.PLANNER_PROMPT.lower())
        self.assertIn("do not call mutating tools", planner.PLANNER_PROMPT.lower())
        self.assertIn("do not execute the plan", planner.PLANNER_PROMPT.lower())
        self.assertIn("stop after the ledger block", planner.PLANNER_PROMPT.lower())

        self.assertIn("do not re-plan", planner.EXECUTION_PROMPT.lower())
        self.assertIn("committed objective and plan", planner.EXECUTION_PROMPT.lower())
        self.assertIn("stale", planner.EXECUTION_PROMPT.lower())
        self.assertIn("stop before", planner.EXECUTION_PROMPT.lower())
        self.assertIn("mutating tool call", planner.EXECUTION_PROMPT.lower())
        self.assertIn("replacement ledger", planner.EXECUTION_PROMPT.lower())
        self.assertIn("objective: <updated goal>", planner.EXECUTION_PROMPT)
        self.assertIn("plan:", planner.EXECUTION_PROMPT)
        self.assertIn("<ledger>", planner.EXECUTION_PROMPT)

    def test_build_autonomy_prompt_joins_non_empty_parts(self):
        prompt = planner.build_autonomy_prompt(
            "execute",
            "Continuity ledger: continue the committed objective",
            "",
        )

        self.assertIn("Continuity ledger", prompt)
        self.assertIn(planner.EXECUTION_PROMPT, prompt)
        self.assertNotIn("\n\n\n", prompt)

    def test_agent_thread_executes_until_interval_then_plans(self):
        ledger.save_ledger("doug", {
            "objective": "Build a smelting column",
            "plan_steps": ["Place furnaces", "Lay the belt"],
            "progress_notes": ["Cleared the build site"],
            "updated_at": "now",
        })

        thread = self._thread()

        tick1 = thread._autonomy_tick()
        self.assertIn(planner.EXECUTION_PROMPT, tick1["message"])
        self.assertNotIn("model", tick1)
        self.assertNotIn("read_only_tools", tick1)
        self.assertEqual(thread._exec_ticks_since_plan, 1)

        tick2 = thread._autonomy_tick()
        self.assertIn(planner.EXECUTION_PROMPT, tick2["message"])
        self.assertEqual(thread._exec_ticks_since_plan, 2)

        tick3 = thread._autonomy_tick()
        self.assertIn(planner.PLANNER_PROMPT, tick3["message"])
        self.assertTrue(tick3["read_only_tools"])
        self.assertEqual(thread._exec_ticks_since_plan, 0)

    def test_agent_thread_empty_ledger_plans_first_tick(self):
        thread = self._thread()

        tick = thread._autonomy_tick()

        self.assertIn(planner.PLANNER_PROMPT, tick["message"])
        self.assertTrue(tick["read_only_tools"])
        self.assertEqual(thread._exec_ticks_since_plan, 0)

    def test_agent_thread_planner_tick_can_override_model(self):
        thread = self._thread()
        thread._planner_model = "strong-planner"

        tick = thread._autonomy_tick()

        self.assertEqual(tick["model"], "strong-planner")

    def test_agent_thread_replans_when_live_state_satisfies_objective(self):
        ledger.save_ledger("doug", {
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

        live_state = (
            "Live state: nauvis @ 46.7,-15.6; player entities: "
            "offshore-pump=1, boiler=1, steam-engine=1, lab=1, "
            "small-electric-pole=15"
        )
        thread = self._thread(live_state=live_state)

        tick = thread._autonomy_tick()

        self.assertTrue(tick["read_only_tools"])
        self.assertIn(planner.PLANNER_PROMPT, tick["message"])
        self.assertIn("Live-state completion signal", tick["message"])
        self.assertEqual(thread._exec_ticks_since_plan, 0)

    def test_agent_thread_executes_existing_plant_repair_after_planner_tick(self):
        ledger.save_ledger("doug", {
            "objective": (
                "energize the power grid to activate the existing automated "
                "iron smelting array"
            ),
            "plan_steps": [
                "walk_to(-41, 26) to reach the boiler at the steam plant",
                "insert_items(unit 49, 'coal', 5, 'fuel')",
                "place_entity('small-electric-pole', -8.5, 12.5)",
                "verify_production at (56, -22) radius 25",
            ],
            "progress_notes": [
                "no change across fifty-four planning ticks. State stable. "
                "Plan fully validated and awaiting execution turns.",
            ],
            "updated_at": "now",
        })

        live_state = (
            "Live state: nauvis @ 55.4,-20.4; player entities: "
            "burner-mining-drill=1, electric-mining-drill=1, "
            "stone-furnace=2, transport-belt=16, inserter=1, "
            "small-electric-pole=21, offshore-pump=1, boiler=1, "
            "steam-engine=1, lab=1"
        )
        thread = self._thread(live_state=live_state)

        tick = thread._autonomy_tick()

        self.assertIn(planner.EXECUTION_PROMPT, tick["message"])
        self.assertNotIn("read_only_tools", tick)
        self.assertNotIn("Live-state completion signal", tick["message"])
        self.assertEqual(thread._exec_ticks_since_plan, 1)

    def test_agent_thread_initializes_to_plan_on_resume(self):
        class StubRCON:
            def execute(self, _cmd):
                return ""

        thread = pipe.AgentThread(
            {"name": "doug", "system_prompt": "system"},
            mcp_config={},
            rcon=StubRCON(),
            model=None,
            telemetry=None,
            planner_interval=3,
        )

        self.assertEqual(thread._exec_ticks_since_plan, 3)

    def _thread(self, live_state=""):
        class StubRCON:
            def __init__(self, response):
                self.response = response

            def execute(self, _cmd):
                return self.response

        thread = pipe.AgentThread.__new__(pipe.AgentThread)
        thread.agent_name = "doug"
        thread.rcon = StubRCON(live_state)
        thread._exec_ticks_since_plan = 0
        thread._planner_interval = 2
        thread._planner_model = None
        thread._reflect_interval = 99
        return thread


if __name__ == "__main__":
    unittest.main()
