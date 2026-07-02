import tempfile
import unittest
from pathlib import Path
from unittest import mock

import skills


class SkillTests(unittest.TestCase):
    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.base = Path(self.tempdir.name)
        self.file_patch = mock.patch(
            "skills._skills_file",
            side_effect=lambda: self.base / ".skills.json",
        )
        self.ledger_patch = mock.patch(
            "ledger._ledger_file",
            side_effect=lambda agent_name: self.base / f".ledger-{agent_name}.json",
        )
        self.journal_patch = mock.patch(
            "journal._journal_file",
            side_effect=lambda agent_name: self.base / f".journal-{agent_name}.jsonl",
        )
        self.reflection_patch = mock.patch(
            "journal._reflection_file",
            side_effect=lambda agent_name: self.base / f".reflection-{agent_name}.json",
        )
        self.file_patch.start()
        self.ledger_patch.start()
        self.journal_patch.start()
        self.reflection_patch.start()
        self.addCleanup(self.file_patch.stop)
        self.addCleanup(self.ledger_patch.stop)
        self.addCleanup(self.journal_patch.stop)
        self.addCleanup(self.reflection_patch.stop)

    def test_load_returns_starters_for_missing_and_corrupt_file(self):
        missing = skills.load_library()

        self.assertGreaterEqual(len(missing["skills"]), 3)
        self.assertIn("build_burner_mining_setup", self._names(missing))
        self.assertIn("build_steam_power", self._names(missing))
        self.assertIn("build_automation_science", self._names(missing))
        mining_steps = " ".join(skills.get_skill(missing, "build_burner_mining_setup")["steps"])
        self.assertIn("execute_edge_miner", mining_steps)
        self.assertIn("execute_direct_smelter", mining_steps)
        self.assertIn("build_fuel_supply_args", mining_steps)
        smelting_steps = " ".join(skills.get_skill(missing, "lay_smelting_line")["steps"])
        self.assertIn("execute_direct_smelter", smelting_steps)
        self.assertIn("build_fuel_supply_args", smelting_steps)
        steam_steps = " ".join(skills.get_skill(missing, "build_steam_power")["steps"])
        self.assertIn("plan_steam_power", steam_steps)
        self.assertIn("place_args", steam_steps)
        self.assertIn("diagnose_steam_power", steam_steps)
        self.assertIn("repair_steam_power", steam_steps)
        self.assertIn("extend_power_to", steam_steps)
        self.assertIn("build_fuel_supply_args", steam_steps)
        lab_steps = " ".join(skills.get_skill(missing, "feed_lab")["steps"])
        self.assertIn("feed_lab_from_inventory", lab_steps)
        science_steps = " ".join(skills.get_skill(missing, "build_automation_science")["steps"])
        self.assertIn("build_automation_science", science_steps)
        self.assertIn("execute_entity_placement_near", science_steps)
        self.assertIn("placed_unit_number", science_steps)
        self.assertIn("plan_automation_science", science_steps)
        self.assertIn("ready_to_call.execute_args", science_steps)
        self.assertIn("build_assembler_feed", science_steps)
        self.assertIn("build_assembler_output", science_steps)
        self.assertIn("build_lab_feed", science_steps)

        (self.base / ".skills.json").write_text("{not json")

        corrupt = skills.load_library()
        self.assertIn("lay_smelting_line", self._names(corrupt))
        self.assertIn("build_steam_power", self._names(corrupt))

    def test_load_library_model_merges_saved_skills_and_helpers_accept_typed_library(self):
        (self.base / ".skills.json").write_text(
            '{"skills": ['
            '{"name": "feed_lab", "params": ["lab"], "outcome": "custom feed"},'
            '{"name": "repair_belt", "steps": ["rotate belt"], "outcome": "ore flows"}'
            ']}\n'
        )

        library = skills.load_library_model()
        rendered = skills.render_skills(library)

        self.assertIn("custom feed", skills.get_skill(library, "feed_lab")["outcome"])
        self.assertEqual(
            skills.get_skill(library, "repair_belt")["steps"],
            ["rotate belt"],
        )
        self.assertIn("feed_lab(lab)", rendered)
        self.assertIn("repair_belt()", rendered)

    def test_save_library_model_and_get_skill_model_keep_typed_boundary(self):
        library = skills.load_library_model().replace_or_append(
            skills.SkillDefinition(
                name="repair_belt",
                steps=["rotate belt"],
                outcome="ore flows",
            )
        )

        skills.save_library_model(library)

        loaded = skills.load_library_model()
        skill = skills.get_skill_model(loaded, "repair_belt")
        self.assertIsInstance(loaded, skills.SkillLibrary)
        self.assertIsInstance(skill, skills.SkillDefinition)
        self.assertEqual(skill.steps, ["rotate belt"])

    def test_parse_skill_trailer_extracts_well_formed_block(self):
        parsed = skills.parse_skill_trailer(
            """Visible text.
<skill>
name: feed_lab
params: lab_pos, science_belt_pos
steps:
- check_placement for the lab and belt-adjacent inserter
- place_entity lab at lab_pos
- route_belt science packs to science_belt_pos
- place inserters with correct facing from belt to lab
outcome: lab consumes automation science packs
</skill>
"""
        )

        self.assertEqual(parsed["name"], "feed_lab")
        self.assertEqual(parsed["params"], ["lab_pos", "science_belt_pos"])
        self.assertEqual(len(parsed["steps"]), 4)
        self.assertEqual(parsed["outcome"], "lab consumes automation science packs")

    def test_parse_skill_trailer_model_accepts_typed_sources(self):
        typed = skills.SkillDefinition(
            name="repair_belt",
            params=["belt_pos"],
            steps=["rotate belt"],
            outcome="ore flows",
        )
        draft = skills.SkillDefinitionDraft(
            name="feed_lab_fast",
            params=["lab_pos"],
            steps=["insert science"],
            outcome="research advances",
        )

        self.assertIs(skills.parse_skill_trailer_model(typed), typed)
        parsed_draft = skills.parse_skill_trailer_model(draft)
        self.assertIsInstance(parsed_draft, skills.SkillDefinition)
        self.assertEqual(parsed_draft.name, "feed_lab_fast")
        self.assertEqual(
            skills.parse_skill_trailer(typed),
            {
                "name": "repair_belt",
                "params": ["belt_pos"],
                "steps": ["rotate belt"],
                "outcome": "ore flows",
            },
        )

    def test_parse_skill_trailer_none_without_block_or_name_and_partial_ok(self):
        self.assertIsNone(skills.parse_skill_trailer("Plain narration only."))
        self.assertIsNone(skills.parse_skill_trailer("<skill>\nsteps:\n- place_entity\n</skill>"))

        parsed = skills.parse_skill_trailer("<skill>\nname: scout_coal\noutcome: coal located\n</skill>")

        self.assertEqual(parsed, {"name": "scout_coal", "outcome": "coal located"})

    def test_parse_params_accepts_comma_list_and_bullets(self):
        parsed = skills.parse_skill_trailer(
            """<skill>
name: two_param_recipe
params:
- start_pos
- end_pos
steps:
- route_belt from start_pos to end_pos
outcome: belt is routed
</skill>"""
        )

        self.assertEqual(parsed["params"], ["start_pos", "end_pos"])

    def test_apply_skill_update_adds_replaces_persists_and_round_trips(self):
        added = skills.apply_skill_update(
            """<skill>
name: build_power_pair
params: water_pos, coal_belt_pos
steps:
- place_entity offshore-pump at water_pos
- place_entity boiler next to the pump
- place_entity steam-engine after the boiler
outcome: starter steam power online
</skill>"""
        )

        self.assertEqual(skills.get_skill(added, "build_power_pair")["outcome"], "starter steam power online")
        self.assertEqual(
            skills.get_skill(skills.load_library(), "build_power_pair")["steps"][0],
            "place_entity offshore-pump at water_pos",
        )

        replaced = skills.apply_skill_update(
            """<skill>
name: build_power_pair
params: water_pos
steps:
- check_placement for pump, boiler, and steam engine
- place_entity offshore-pump at water_pos
outcome: verified starter steam power online
</skill>"""
        )

        skill = skills.get_skill(replaced, "build_power_pair")
        self.assertEqual(skill["params"], ["water_pos"])
        self.assertEqual(skill["outcome"], "verified starter steam power online")
        self.assertEqual(len([s for s in replaced["skills"] if s["name"] == "build_power_pair"]), 1)

    def test_apply_skill_update_model_adds_replaces_and_persists(self):
        library = skills.apply_skill_update_model(
            """<skill>
name: build_power_pair
params: water_pos, coal_belt_pos
steps:
- place_entity offshore-pump at water_pos
- place_entity boiler next to the pump
outcome: starter steam power online
</skill>"""
        )

        skill = skills.get_skill_model(library, "build_power_pair")
        persisted_skill = skills.get_skill_model(skills.load_library_model(), "build_power_pair")
        self.assertIsInstance(library, skills.SkillLibrary)
        self.assertEqual(skill.params, ["water_pos", "coal_belt_pos"])
        self.assertEqual(persisted_skill.outcome, "starter steam power online")

        replaced = skills.apply_skill_update_model(skills.SkillDefinition(
            name="build_power_pair",
            params=["typed_water_pos"],
            steps=["use typed placement plan"],
            outcome="typed starter steam power online",
        ))

        typed_skill = skills.get_skill_model(replaced, "build_power_pair")
        typed_persisted = skills.get_skill_model(skills.load_library_model(), "build_power_pair")
        self.assertEqual(typed_skill.params, ["typed_water_pos"])
        self.assertEqual(typed_persisted.outcome, "typed starter steam power online")

    def test_apply_skill_update_noops_without_valid_block(self):
        before = skills.load_library()

        after = skills.apply_skill_update("No skill here.")

        self.assertEqual(after, before)

    def test_strip_skill_trailer_removes_only_block_and_is_idempotent(self):
        text = "Before.\n\n<skill>\nname: hidden\n</skill>\n\nAfter."

        stripped = skills.strip_skill_trailer(text)

        self.assertEqual(stripped, "Before.\n\nAfter.")
        self.assertEqual(skills.strip_skill_trailer(stripped), "Before.\n\nAfter.")
        self.assertEqual(skills.strip_skill_trailer(None), "")

    def test_render_skills_signatures_only(self):
        self.assertEqual(skills.render_skills({}), "")
        self.assertEqual(skills.render_skills(None), "")
        library = {
            "skills": [{
                "name": "feed_lab",
                "params": ["lab_pos", "science_belt_pos"],
                "steps": ["place_entity lab", "route_belt science packs"],
                "outcome": "lab consumes science packs",
            }]
        }

        rendered = skills.render_skills(library)

        self.assertIn("Available skills", rendered)
        self.assertIn("feed_lab(lab_pos, science_belt_pos)", rendered)
        self.assertIn("lab consumes science packs", rendered)
        self.assertNotIn("place_entity lab", rendered)

    def test_helpers_are_total_on_bad_input(self):
        self.assertIsNone(skills.parse_skill_trailer(None))
        self.assertEqual(skills.strip_skill_trailer(42), "")
        self.assertEqual(skills.render_skills("oops"), "")
        self.assertIsNone(skills.get_skill(None, "feed_lab"))
        self.assertIsNone(skills.get_skill({"skills": "oops"}, None))
        skills.save_library(None)
        self.assertIn("build_burner_mining_setup", self._names(skills.load_library()))

    def test_finalize_reply_strips_legacy_skill_block_without_persisting(self):
        import pipe

        finalized = pipe._finalize_reply(
            """Done.
<skill>
name: feed_lab_fast
params: lab_pos
steps:
- place_entity lab at lab_pos
outcome: lab ready for science
</skill>
""",
            "doug",
        )

        self.assertEqual(finalized, "Done.")
        self.assertIsNone(skills.get_skill(skills.load_library(), "feed_lab_fast"))

    def test_autonomy_tick_does_not_inject_legacy_skill_library(self):
        import pipe

        skills.apply_skill_update(
            """<skill>
name: feed_lab_fast
params: lab_pos
steps:
- place_entity lab at lab_pos
outcome: lab ready for science
</skill>"""
        )

        class StubRCON:
            def execute(self, _cmd):
                return ""

        thread = pipe.AgentThread.__new__(pipe.AgentThread)
        thread.agent_name = "doug"
        thread.rcon = StubRCON()
        thread._exec_ticks_since_plan = 0
        thread._planner_interval = 5
        thread._planner_model = None
        thread._reflect_interval = 16

        prompt = thread._compose_autonomy_prompt()

        self.assertNotIn("Available skills", prompt)
        self.assertNotIn("feed_lab_fast(lab_pos)", prompt)
        self.assertNotIn("Prefer reusing an existing skill", prompt)
        self.assertNotIn("<skill>", prompt)

    def test_execution_tick_does_not_inject_legacy_skill_library(self):
        import ledger
        import pipe

        ledger.save_ledger("doug", {
            "objective": "Smelt iron",
            "plan_steps": ["place furnaces", "lay belt"],
            "progress_notes": [],
            "updated_at": "now",
        })
        skills.apply_skill_update(
            "<skill>\nname: feed_lab_fast\nparams: lab_pos\nsteps:\n- do x\n"
            "outcome: ready\n</skill>"
        )

        class StubRCON:
            def execute(self, _cmd):
                return ""

        thread = pipe.AgentThread.__new__(pipe.AgentThread)
        thread.agent_name = "doug"
        thread.rcon = StubRCON()
        thread._exec_ticks_since_plan = 0  # objective+plan set, 0 < interval -> execute
        thread._planner_interval = 5
        thread._planner_model = None
        thread._reflect_interval = 16

        prompt = thread._compose_autonomy_prompt()

        self.assertNotIn("Available skills", prompt)
        self.assertNotIn("feed_lab_fast(lab_pos)", prompt)
        self.assertNotIn("name: lay_smelting_line", prompt)
        self.assertNotIn("save it as a <skill> block", prompt)

    def _names(self, library):
        return {skill["name"] for skill in library["skills"]}


if __name__ == "__main__":
    unittest.main()
