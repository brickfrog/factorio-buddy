import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import ledger
import journal
import pipe
import planner
from models import (
    AgentProfile,
    AutonomyDecisionReason,
    AutonomyMode,
    AutonomyPromptInput,
    AutonomyTickMessage,
    LedgerState,
    LiveCompletionEvidence,
    LiveState,
    ObjectiveCompletionKind,
)


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
        self.journal_patch = mock.patch(
            "journal._journal_file",
            side_effect=lambda agent_name: self.base / f".journal-{agent_name}.jsonl",
        )
        self.journal_patch.start()
        self.addCleanup(self.journal_patch.stop)

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

    def test_choose_autonomy_decision_executes_actionable_plan_at_interval(self):
        decision = planner.choose_autonomy_decision(
            {
                "objective": "activate second stone-furnace unit 15",
                "plan_steps": ["walk_to (42, -21)"],
                "progress_notes": [],
                "updated_at": "",
            },
            5,
            5,
            journal_window=[{
                "kind": "progress",
                "text": "plan validated, awaiting execution",
                "signal": "plan_ready",
            }],
        )

        self.assertEqual(decision.mode, AutonomyMode.EXECUTE)
        self.assertEqual(decision.reason, "actionable_plan")
        self.assertIs(decision.reason, AutonomyDecisionReason.ACTIONABLE_PLAN)
        self.assertTrue(decision.actionable_plan)
        self.assertFalse(decision.read_only_tools)
        self.assertEqual(decision.next_exec_ticks_since_plan("5"), 6)

    def test_choose_autonomy_decision_uses_execution_ready_ledger_at_interval(self):
        decision = planner.choose_autonomy_decision(
            {
                "objective": "activate second stone-furnace unit 15",
                "plan_steps": ["walk_to (42, -21)"],
                "progress_notes": [
                    "Plan concrete and executable. Awaiting execution turn.",
                ],
                "updated_at": "",
                "signal": "plan_ready",
            },
            5,
            5,
        )

        self.assertEqual(decision.mode, AutonomyMode.EXECUTE)
        self.assertEqual(decision.reason, "actionable_plan")
        self.assertTrue(decision.actionable_plan)

    def test_choose_autonomy_decision_does_not_execute_from_ready_prose_only(self):
        decision = planner.choose_autonomy_decision(
            {
                "objective": "activate second stone-furnace unit 15",
                "plan_steps": ["walk_to (42, -21)"],
                "progress_notes": [
                    "Plan concrete and executable. Awaiting execution turn.",
                    "Plan validated, queued for execution.",
                ],
                "updated_at": "",
            },
            5,
            5,
        )

        self.assertEqual(decision.mode, AutonomyMode.PLAN)
        self.assertEqual(decision.reason, "planner_interval")
        self.assertFalse(decision.actionable_plan)
        self.assertTrue(decision.read_only_tools)
        self.assertEqual(decision.next_exec_ticks_since_plan("5"), 0)

    def test_choose_autonomy_decision_executes_after_repeated_ready_ledger_notes(self):
        decision = planner.choose_autonomy_decision(
            {
                "objective": "activate second stone-furnace unit 15",
                "plan_steps": ["walk_to (42, -21)", "insert_items coal"],
                "progress_notes": [
                    "Inventory intact. Plan validated against live state, ready for execution.",
                    "No state drift. Plan validated, queued for execution.",
                    "Fourth planning cycle. Plan validated, awaiting execution.",
                ],
                "updated_at": "",
            },
            5,
            5,
        )

        self.assertEqual(decision.mode, AutonomyMode.EXECUTE)
        self.assertEqual(decision.reason, "actionable_plan")
        self.assertTrue(decision.actionable_plan)
        self.assertFalse(decision.read_only_tools)

    def test_choose_autonomy_decision_executes_after_repeated_unsignaled_progress(self):
        repeated = {
            "kind": "progress",
            "text": "situation_report confirmed stable. Plan validated, awaiting execution.",
        }
        decision = planner.choose_autonomy_decision(
            {
                "objective": "activate second stone-furnace unit 15",
                "plan_steps": ["walk_to (42, -21)"],
                "progress_notes": [],
                "updated_at": "",
            },
            5,
            5,
            journal_window=[repeated, repeated, repeated],
        )

        self.assertEqual(decision.mode, AutonomyMode.EXECUTE)
        self.assertEqual(decision.reason, "repeated_plan_progress")
        self.assertIs(
            decision.reason,
            AutonomyDecisionReason.REPEATED_PLAN_PROGRESS,
        )
        self.assertTrue(decision.actionable_plan)

    def test_choose_autonomy_decision_executes_after_stable_progress_with_new_ticks(self):
        decision = planner.choose_autonomy_decision(
            {
                "objective": "activate second stone-furnace unit 15",
                "plan_steps": ["walk_to (42, -21)"],
                "progress_notes": [],
                "updated_at": "",
            },
            5,
            5,
            journal_window=[
                {
                    "kind": "progress",
                    "text": (
                        "situation_report confirmed stable at tick 4105905. "
                        "Plan validated, ready for execution."
                    ),
                },
                {
                    "kind": "progress",
                    "text": (
                        "situation_report confirmed stable at tick 4107121. "
                        "Plan validated, ready for execution."
                    ),
                },
                {
                    "kind": "progress",
                    "text": (
                        "situation_report confirmed stable at tick 4108864. "
                        "Plan validated, ready for execution."
                    ),
                },
            ],
        )

        self.assertEqual(decision.mode, AutonomyMode.EXECUTE)
        self.assertEqual(decision.reason, "repeated_plan_progress")
        self.assertTrue(decision.actionable_plan)

    def test_choose_autonomy_decision_uses_typed_live_completion_evidence(self):
        decision = planner.choose_autonomy_decision(
            {
                "objective": "complete steam power deployment",
                "plan_steps": ["place boiler"],
                "progress_notes": [],
                "updated_at": "",
                "signal": "plan_ready",
            },
            5,
            5,
            live_completion_evidence=LiveCompletionEvidence(
                kind=ObjectiveCompletionKind.STEAM_POWER,
                reason="live state already has offshore-pump, boiler, and steam-engine",
            ),
        )

        self.assertEqual(decision.mode, AutonomyMode.PLAN)
        self.assertEqual(decision.reason, "live_state_completion")
        self.assertIs(
            decision.reason,
            AutonomyDecisionReason.LIVE_STATE_COMPLETION,
        )
        self.assertTrue(decision.actionable_plan)

    def test_choose_autonomy_decision_ignores_empty_live_completion_evidence(self):
        decision = planner.choose_autonomy_decision(
            {
                "objective": "activate second stone-furnace unit 15",
                "plan_steps": ["walk_to (42, -21)"],
                "progress_notes": [],
                "updated_at": "",
                "signal": "plan_ready",
            },
            5,
            5,
            live_completion_evidence=LiveCompletionEvidence.none(),
        )

        self.assertEqual(decision.mode, AutonomyMode.EXECUTE)
        self.assertEqual(decision.reason, "actionable_plan")
        self.assertTrue(decision.actionable_plan)

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

    def test_live_state_entity_counts_accepts_typed_model(self):
        state = LiveState(
            found=True,
            surface="nauvis",
            x=46.7,
            y=-15.6,
            entity_counts={
                "offshore-pump": "1",
                "boiler": 1,
                "lab": 1,
            },
        )

        self.assertEqual(
            planner.live_state_entity_counts(state),
            {
                "offshore-pump": 1,
                "boiler": 1,
                "lab": 1,
            },
        )

    def test_objective_completion_evidence_detects_completed_power_lab(self):
        evidence = planner.objective_completion_evidence(
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

        self.assertTrue(evidence.is_completion)
        self.assertEqual(evidence.kind, ObjectiveCompletionKind.POWERED_LAB)
        self.assertIn("steam power", evidence.reason)

    def test_objective_completion_evidence_accepts_typed_live_state(self):
        live_state = LiveState(
            found=True,
            surface="nauvis",
            x=46.7,
            y=-15.6,
            entity_counts={
                "offshore-pump": 1,
                "boiler": 1,
                "steam-engine": 1,
                "lab": 1,
                "small-electric-pole": 15,
            },
        )

        evidence = planner.objective_completion_evidence(
            {
                "objective": (
                    "complete steam power deployment, power the lab, start "
                    "automation research"
                ),
                "plan_steps": ["place steam engine adjacent to boiler"],
                "progress_notes": [],
                "updated_at": "now",
            },
            live_state,
        )

        self.assertTrue(evidence.is_completion)
        self.assertEqual(evidence.kind, ObjectiveCompletionKind.POWERED_LAB)

    def test_objective_completion_evidence_ignores_unrelated_objective(self):
        evidence = planner.objective_completion_evidence(
            {
                "objective": "build automated iron smelting array",
                "plan_steps": ["route belt", "place inserter"],
                "progress_notes": [],
                "updated_at": "now",
            },
            "Live state: nauvis @ 46.7,-15.6; player entities: "
            "offshore-pump=1, boiler=1, steam-engine=1, lab=1",
        )

        self.assertFalse(evidence.is_completion)
        self.assertEqual(evidence.kind, ObjectiveCompletionKind.NONE)

    def test_objective_completion_evidence_ignores_existing_plant_repair(self):
        evidence = planner.objective_completion_evidence(
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

        self.assertFalse(evidence.is_completion)
        self.assertEqual(evidence.kind, ObjectiveCompletionKind.NONE)

    def test_objective_completion_evidence_ignores_stale_power_progress(self):
        evidence = planner.objective_completion_evidence(
            {
                "objective": (
                    "establish iron ore flow from electric drill at "
                    "(53.5, -21.5) through belt line to inserter at "
                    "(59.5, -22.5) so furnaces begin producing iron plates"
                ),
                "plan_steps": [
                    "analyze_belt_reach at (53, -20) radius 15",
                    "analyze_belt_gaps at (57, -21) radius 12",
                    "route_belt or place_entity to bridge gaps",
                    "verify_production at (56, -22) radius 25",
                ],
                "progress_notes": [
                    "objective complete. Root cause was unfueled boiler only; "
                    "pole gap was already bridged. Boiler fueled with 5 coal.",
                    "power-energization objective complete. New objective "
                    "assigned: repair belt logistics so ore reaches furnaces.",
                ],
                "updated_at": "now",
            },
            "Live state: nauvis @ 54.1,-21.0; player entities: "
            "burner-mining-drill=1, electric-mining-drill=1, "
            "stone-furnace=2, transport-belt=16, inserter=1, "
            "small-electric-pole=21, offshore-pump=1, boiler=1, "
            "steam-engine=1, lab=1",
        )

        self.assertFalse(evidence.is_completion)
        self.assertEqual(evidence.kind, ObjectiveCompletionKind.NONE)

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
        self.assertIn("signal:", planner.PLANNER_PROMPT)

        self.assertIn("do not re-plan", planner.EXECUTION_PROMPT.lower())
        self.assertIn("committed objective and plan", planner.EXECUTION_PROMPT.lower())
        self.assertIn("stale", planner.EXECUTION_PROMPT.lower())
        self.assertIn("stop before", planner.EXECUTION_PROMPT.lower())
        self.assertIn("mutating tool call", planner.EXECUTION_PROMPT.lower())
        self.assertIn("replacement ledger", planner.EXECUTION_PROMPT.lower())
        self.assertIn("objective: <updated goal>", planner.EXECUTION_PROMPT)
        self.assertIn("signal:", planner.EXECUTION_PROMPT)
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

    def test_build_autonomy_prompt_accepts_typed_live_state(self):
        prompt = planner.build_autonomy_prompt(
            AutonomyMode.EXECUTE,
            "Continuity ledger: continue the committed objective",
            LiveState(
                found=True,
                surface="nauvis",
                x=5,
                y=0,
                entity_counts={"stone-furnace": 2},
            ),
        )

        self.assertIn(
            "Live state: nauvis @ 5.0,0.0; player entities: stone-furnace=2",
            prompt,
        )
        self.assertIn(planner.EXECUTION_PROMPT, prompt)

    def test_build_autonomy_prompt_model_accepts_typed_context(self):
        prompt = planner.build_autonomy_prompt_model(
            AutonomyPromptInput(
                mode=AutonomyMode.EXECUTE,
                ledger=LedgerState(
                    objective="Repair belt logistics",
                    plan_steps=["analyze_belt_gaps at (57, -21) radius 12"],
                    progress_notes=["belt unit 75 was rotated"],
                    updated_at="now",
                ),
                live_state=LiveState(
                    found=True,
                    surface="nauvis",
                    x=54.1,
                    y=-21.0,
                    entity_counts={"electric-mining-drill": 1},
                ),
                memory_text="Lessons: fuel the boiler",
                learned_text="Accepted learning: inspect belts first",
                live_completion_reason="lab already powered",
            )
        )

        self.assertIn("Lessons: fuel the boiler", prompt)
        self.assertIn(
            "Continuity ledger: continue the committed objective, do not restart it: "
            "Repair belt logistics",
            prompt,
        )
        self.assertIn("1. analyze_belt_gaps at (57, -21) radius 12", prompt)
        self.assertIn("- belt unit 75 was rotated", prompt)
        self.assertIn("Accepted learning: inspect belts first", prompt)
        self.assertIn(
            "Live state: nauvis @ 54.1,-21.0; player entities: "
            "electric-mining-drill=1",
            prompt,
        )
        self.assertIn("Live-state completion signal: lab already powered", prompt)
        self.assertIn(planner.EXECUTION_PROMPT, prompt)

    def test_build_autonomy_prompt_model_coerces_dict_context(self):
        prompt = planner.build_autonomy_prompt_model({
            "mode": "plan",
            "ledger": {
                "objective": "Build starter power",
                "plan_steps": ["place boiler"],
                "progress_notes": [],
                "updated_at": "now",
            },
            "live_state": (
                "Live state: nauvis @ 5.0,0.0; player entities: boiler=1"
            ),
        })

        self.assertIn("Build starter power", prompt)
        self.assertIn("Live state: nauvis @ 5.0,0.0; player entities: boiler=1", prompt)
        self.assertIn(planner.PLANNER_PROMPT, prompt)

    def test_agent_thread_executes_until_interval_then_plans(self):
        ledger.save_ledger("doug", {
            "objective": "Build a smelting column",
            "plan_steps": ["Place furnaces", "Lay the belt"],
            "progress_notes": ["Cleared the build site"],
            "updated_at": "now",
        })

        thread = self._thread()

        tick1 = thread._autonomy_tick()
        self.assertIsInstance(tick1, AutonomyTickMessage)
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

    def test_agent_thread_replans_when_json_live_state_satisfies_objective(self):
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

        thread = self._thread(live_state=json.dumps({
            "found": True,
            "surface": "nauvis",
            "x": 46.7,
            "y": -15.6,
            "entity_counts": {
                "offshore-pump": 1,
                "boiler": 1,
                "steam-engine": 1,
                "lab": 1,
                "small-electric-pole": 15,
            },
        }))

        tick = thread._autonomy_tick()

        self.assertTrue(tick["read_only_tools"])
        self.assertIn(planner.PLANNER_PROMPT, tick["message"])
        self.assertIn("Live-state completion signal", tick["message"])
        self.assertIn("Live state: nauvis @ 46.7,-15.6", tick["message"])
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
            "signal": "plan_ready",
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

    def test_agent_thread_executes_belt_plan_despite_old_power_progress(self):
        ledger.save_ledger("doug", {
            "objective": (
                "establish iron ore flow from electric drill at (53.5, -21.5) "
                "through belt line to inserter at (59.5, -22.5) so furnaces "
                "begin producing iron plates"
            ),
            "plan_steps": [
                "analyze_belt_reach at (53, -20) radius 15",
                "analyze_belt_gaps at (57, -21) radius 12",
                "route_belt or place_entity to bridge gaps",
                "verify_production at (56, -22) radius 25",
            ],
            "progress_notes": [
                "objective complete. Root cause was unfueled boiler only; "
                "pole gap was already bridged. Boiler fueled with 5 coal.",
                "situation_report confirmed stable state. No diagnostic step "
                "executed yet; plan continues from step 1.",
            ],
            "updated_at": "now",
            "signal": "plan_ready",
        })

        live_state = (
            "Live state: nauvis @ 54.1,-21.0; player entities: "
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

    def test_agent_thread_executes_new_objective_after_old_complete_event(self):
        ledger.save_ledger("doug", {
            "objective": (
                "activate second stone-furnace unit 15 at (42, -22) with "
                "hand-fed fuel and ore"
            ),
            "plan_steps": [
                "walk_to (42, -21) to approach furnace unit 15",
                "insert_items coal count=5 into fuel inventory of unit 15",
                "insert_items iron-ore count=20 into furnace_source inventory of unit 15",
                "verify_production at (42,-22) radius 4",
            ],
            "progress_notes": [
                "previous objective complete; second furnace identified as next bottleneck",
                "plan validated, awaiting execution",
            ],
            "updated_at": "now",
        })
        journal.append_event(
            "doug",
            "progress",
            "COMPLETE - belt rotated, all entities working, furnace producing plates",
        )
        journal.append_event(
            "doug",
            "progress",
            "previous objective complete; second furnace selected",
            signal="new_objective",
        )

        thread = self._thread(live_state=(
            "Live state: nauvis @ 54.1,-21.0; player entities: "
            "burner-mining-drill=1, electric-mining-drill=1, "
            "stone-furnace=2, transport-belt=16, inserter=1, "
            "small-electric-pole=21, offshore-pump=1, boiler=1, "
            "steam-engine=1, lab=1"
        ))

        tick = thread._autonomy_tick()

        self.assertIn(planner.EXECUTION_PROMPT, tick["message"])
        self.assertNotIn("read_only_tools", tick)
        self.assertIn("walk_to (42, -21)", tick["message"])
        self.assertEqual(thread._exec_ticks_since_plan, 1)

    def test_agent_thread_executes_plan_ready_signal_even_when_planner_due(self):
        ledger.save_ledger("doug", {
            "objective": "activate second stone-furnace unit 15",
            "plan_steps": [
                "walk_to (42, -21)",
                "insert_items coal count=5 into fuel inventory of unit 15",
            ],
            "progress_notes": ["plan validated, awaiting execution"],
            "updated_at": "now",
        })
        journal.append_event(
            "doug",
            "progress",
            "plan validated, awaiting execution",
            signal="plan_ready",
        )
        thread = self._thread()
        thread._exec_ticks_since_plan = thread._planner_interval

        tick = thread._autonomy_tick()

        self.assertIn(planner.EXECUTION_PROMPT, tick["message"])
        self.assertNotIn("read_only_tools", tick)
        self.assertIn("walk_to (42, -21)", tick["message"])
        self.assertEqual(thread._exec_ticks_since_plan, thread._planner_interval + 1)

    def test_agent_thread_executes_plan_ready_ledger_without_journal_signal(self):
        ledger.save_ledger("doug", {
            "objective": "activate second stone-furnace unit 15",
            "plan_steps": [
                "walk_to (42, -21)",
                "insert_items coal count=5 into fuel inventory of unit 15",
            ],
            "progress_notes": [
                "situation_report confirmed stable at tick 4107121",
                "Plan concrete and executable. Awaiting execution turn.",
                "Fourth consecutive planning cycle with zero state drift. "
                "Plan validated, queued for execution.",
            ],
            "updated_at": "now",
            "signal": "plan_ready",
        })
        thread = self._thread()
        thread._exec_ticks_since_plan = thread._planner_interval

        tick = thread._autonomy_tick()

        self.assertIn(planner.EXECUTION_PROMPT, tick["message"])
        self.assertNotIn("read_only_tools", tick)
        self.assertIn("walk_to (42, -21)", tick["message"])
        self.assertEqual(thread._exec_ticks_since_plan, thread._planner_interval + 1)

    def test_agent_thread_executes_repeated_ready_ledger_without_signal(self):
        ledger.save_ledger("doug", {
            "objective": "activate second stone-furnace unit 15",
            "plan_steps": [
                "walk_to (42, -21)",
                "insert_items coal count=5 into fuel inventory of unit 15",
            ],
            "progress_notes": [
                "Inventory intact. Plan validated against live state, ready for execution.",
                "No state drift. Plan validated, queued for execution.",
                "Fourth planning cycle. Plan validated, awaiting execution.",
            ],
            "updated_at": "now",
        })
        thread = self._thread()
        thread._exec_ticks_since_plan = thread._planner_interval

        tick = thread._autonomy_tick()

        self.assertIn(planner.EXECUTION_PROMPT, tick["message"])
        self.assertNotIn("read_only_tools", tick)
        self.assertIn("walk_to (42, -21)", tick["message"])
        self.assertEqual(thread._exec_ticks_since_plan, thread._planner_interval + 1)

    def test_agent_thread_executes_plan_ready_ledger_signal_without_progress_noise(self):
        ledger.save_ledger("doug", {
            "objective": "activate second stone-furnace unit 15",
            "plan_steps": [
                "walk_to (42, -21)",
                "insert_items coal count=5 into fuel inventory of unit 15",
            ],
            "progress_notes": [],
            "updated_at": "now",
            "signal": "plan_ready",
        })
        thread = self._thread()
        thread._exec_ticks_since_plan = thread._planner_interval

        tick = thread._autonomy_tick()

        self.assertIn(planner.EXECUTION_PROMPT, tick["message"])
        self.assertNotIn("read_only_tools", tick)
        self.assertIn("walk_to (42, -21)", tick["message"])
        self.assertEqual(thread._exec_ticks_since_plan, thread._planner_interval + 1)

    def test_execution_ready_ledger_overrides_stale_complete_journal_event(self):
        ledger.save_ledger("doug", {
            "objective": "activate second stone-furnace unit 15",
            "plan_steps": [
                "walk_to (42, -21)",
                "insert_items coal count=5 into fuel inventory of unit 15",
            ],
            "progress_notes": [
                "Plan concrete and executable. Awaiting execution turn.",
                "Fourth consecutive planning cycle with zero state drift. "
                "Plan validated, queued for execution.",
            ],
            "updated_at": "now",
            "signal": "plan_ready",
        })
        journal.append_event(
            "doug",
            "progress",
            "COMPLETE - previous belt objective fulfilled",
        )
        thread = self._thread()
        thread._exec_ticks_since_plan = thread._planner_interval

        tick = thread._autonomy_tick()

        self.assertIn(planner.EXECUTION_PROMPT, tick["message"])
        self.assertNotIn("read_only_tools", tick)
        self.assertIn("walk_to (42, -21)", tick["message"])
        self.assertEqual(thread._exec_ticks_since_plan, thread._planner_interval + 1)

    def test_plan_ready_signal_defers_reflection_to_execute_next_step(self):
        ledger.save_ledger("doug", {
            "objective": "activate second stone-furnace unit 15",
            "plan_steps": [
                "walk_to (42, -21)",
                "insert_items coal count=5 into fuel inventory of unit 15",
            ],
            "progress_notes": ["plan validated, awaiting execution"],
            "updated_at": "now",
        })
        journal.append_event(
            "doug",
            "progress",
            "plan validated, awaiting execution",
            signal="plan_ready",
        )
        thread = self._thread()
        thread._exec_ticks_since_plan = thread._planner_interval
        thread._reflect_interval = 1

        tick = thread._autonomy_tick()

        self.assertIn(planner.EXECUTION_PROMPT, tick["message"])
        self.assertNotIn("read_only_tools", tick)
        self.assertNotIn("<reflection>", tick["message"])

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

    def test_agent_thread_consumes_typed_profile_fields(self):
        class StubRCON:
            def execute(self, _cmd):
                return ""

        thread = pipe.AgentThread(
            {
                "name": "doug",
                "system_prompt": "system",
                "model": "profile-model",
                "max_turns": 99,
                "telemetry_name": "DOUG",
                "heartbeat_interval": 7,
                "planner_interval": 4,
                "reflect_interval": 8,
                "planner_model": "planner-model",
                "autonomy_requires_player": False,
                "sdk_skills": "factorio-control,verify",
            },
            mcp_config={},
            rcon=StubRCON(),
            model="cli-model",
            telemetry=None,
        )

        self.assertEqual(thread.agent_name, "doug")
        self.assertIsInstance(thread.profile, AgentProfile)
        self.assertIs(thread.agent, thread.profile)
        self.assertIs(thread.runtime.profile, thread.profile)
        self.assertEqual(thread.model, "cli-model")
        self.assertEqual(thread.max_turns, 99)
        self.assertEqual(thread.telemetry_name, "DOUG")
        self.assertEqual(thread.heartbeat_interval, 7.0)
        self.assertEqual(thread._planner_interval, 4)
        self.assertEqual(thread._exec_ticks_since_plan, 4)
        self.assertEqual(thread._reflect_interval, 8)
        self.assertEqual(thread._planner_model, "planner-model")
        self.assertFalse(thread.autonomy_requires_player)
        self.assertEqual(thread.sdk_skills, ["factorio-control", "verify"])

    def test_agent_thread_human_connected_uses_validated_remote_call(self):
        class StubRCON:
            def __init__(self):
                self.commands = []

            def execute(self, command):
                self.commands.append(command)
                return '{"count":1}\n'

        thread = pipe.AgentThread.__new__(pipe.AgentThread)
        thread.rcon = StubRCON()

        self.assertTrue(thread._human_connected())
        self.assertEqual(thread.rcon.commands, [
            '/silent-command rcon.print(remote.call("claude_interface", '
            '"connected_player_count_result"))',
        ])

    def test_agent_thread_human_connected_false_for_empty_or_malformed_payload(self):
        class StubRCON:
            def __init__(self, response):
                self.response = response

            def execute(self, _command):
                return self.response

        thread = pipe.AgentThread.__new__(pipe.AgentThread)
        thread.log = pipe.logger.bind(agent="test")

        thread.rcon = StubRCON('{"count":0}\n')
        self.assertFalse(thread._human_connected())

        thread.rcon = StubRCON("not json\n")
        self.assertFalse(thread._human_connected())

    def test_agent_thread_live_state_uses_validated_remote_call(self):
        class StubRCON:
            def __init__(self):
                self.commands = []

            def execute(self, command):
                self.commands.append(command)
                return json.dumps({
                    "found": True,
                    "surface": "nauvis",
                    "x": 5,
                    "y": 0,
                    "entity_counts": {"stone-furnace": 2},
                })

        thread = pipe.AgentThread.__new__(pipe.AgentThread)
        thread.agent_name = 'doug"]]'
        thread.rcon = StubRCON()

        self.assertEqual(
            thread._live_state_line(),
            "Live state: nauvis @ 5.0,0.0; player entities: stone-furnace=2",
        )
        self.assertEqual(thread.rcon.commands, [
            '/silent-command rcon.print(remote.call("claude_interface", '
            '"live_state_result", [=[doug"]]]=]))',
        ])

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
