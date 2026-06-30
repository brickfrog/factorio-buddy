import asyncio
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest import mock

import journal


class JournalTests(unittest.TestCase):
    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.base = Path(self.tempdir.name)
        self.journal_patch = mock.patch(
            "journal._journal_file",
            side_effect=lambda agent_name: self.base / f".journal-{agent_name}.jsonl",
        )
        self.reflection_patch = mock.patch(
            "journal._reflection_file",
            side_effect=lambda agent_name: self.base / f".reflection-{agent_name}.json",
        )
        # _finalize_reply also persists the objective ledger; patch its path too
        # so tests never write .ledger-*.json into the repo working tree.
        self.ledger_patch = mock.patch(
            "ledger._ledger_file",
            side_effect=lambda agent_name: self.base / f".ledger-{agent_name}.json",
        )
        self.journal_patch.start()
        self.reflection_patch.start()
        self.ledger_patch.start()
        self.addCleanup(self.journal_patch.stop)
        self.addCleanup(self.reflection_patch.stop)
        self.addCleanup(self.ledger_patch.stop)

    def test_append_event_loads_recent_events_in_file_order_and_respects_limit(self):
        journal.append_event("doug", "discovery", "Found copper east of spawn")
        journal.append_event("doug", "unknown", "Fallback kind is progress")
        journal.append_event("doug", "milestone", "Starter power is online")

        events = journal.load_events("doug", limit=2)

        self.assertEqual([event["kind"] for event in events], ["progress", "milestone"])
        self.assertEqual(
            [event["text"] for event in events],
            ["Fallback kind is progress", "Starter power is online"],
        )
        self.assertTrue(all(event["ts"] for event in events))
        self.assertEqual(journal.count_events("doug"), 3)

    def test_load_events_and_reflection_are_total_for_missing_and_corrupt_files(self):
        self.assertEqual(journal.load_events("doug"), [])
        self.assertEqual(journal.load_reflection("doug"), journal.default_reflection())

        (self.base / ".journal-doug.jsonl").write_text(
            '{"ts": "ok", "kind": "progress", "text": "good"}\n'
            "{not json\n"
            '{"ts": null, "kind": 3, "text": ["coerced"]}\n'
        )
        (self.base / ".reflection-doug.json").write_text("{not json")

        self.assertEqual(len(journal.load_events("doug")), 2)
        self.assertEqual(journal.load_reflection("doug"), journal.default_reflection())

    def test_transient_provider_failures_do_not_pollute_memory(self):
        path = self.base / ".journal-doug.jsonl"
        path.write_text(
            '{"ts": "t1", "kind": "failure", "text": "Reached maximum number of turns (200)"}\n'
            '{"ts": "t2", "kind": "failure", "text": "API Error: Request rejected (429) - Usage limit reached"}\n'
            '{"ts": "t3", "kind": "failure", "text": "[{\\"type\\":\\"text\\",\\"text\\":\\"Error: expected value at line 1 column 1\\"}]"}\n'
            '{"ts": "t4", "kind": "failure", "text": "stream idle timeout after 300s"}\n'
            '{"ts": "t5", "kind": "failure", "text": "{\\"error\\": \\"No electric poles found in area\\"}"}\n'
            '{"ts": "t6", "kind": "progress", "text": "situation assessed; no infrastructure yet deployed"}\n'
            '{"ts": "t7", "kind": "failure", "text": "invalid_request: Error: missing field `success` at line 1 column 135"}\n'
            '{"ts": "t8", "kind": "failure", "text": "sdk_failure: Error: Packet too large: 1553350 bytes"}\n'
            '{"ts": "t9", "kind": "failure", "text": "game_rejected: __claude-interface__/control.lua:946: bad argument #1 of 2 to \'pairs\' (table expected, got nil)"}\n'
            '{"ts": "t10", "kind": "failure", "text": "game_rejected: Failed to queue research - check if another research is in progress"}\n'
            '{"ts": "t3", "kind": "failure", "text": "Inserter faced the wrong way"}\n'
        )

        journal.append_event("doug", "failure", "Usage limit reached for 5 hour")
        journal.append_event("doug", "failure", "stream idle timeout after 300s")
        journal.append_event("doug", "failure", '{"error": "No electric poles found in area"}')
        journal.append_event("doug", "progress", "situation assessed; no infrastructure yet deployed")
        events = journal.load_events("doug")
        rendered = journal.render_memory(events, journal.default_reflection())

        self.assertEqual([event["text"] for event in events], ["Inserter faced the wrong way"])
        self.assertIn("Inserter faced the wrong way", rendered)
        self.assertNotIn("Usage limit", rendered)
        self.assertNotIn("maximum number of turns", rendered)
        self.assertNotIn("stream idle timeout", rendered)
        self.assertNotIn("No electric poles", rendered)
        self.assertNotIn("no infrastructure", rendered)
        self.assertNotIn("missing field", rendered)
        self.assertNotIn("Packet too large", rendered)
        self.assertNotIn("bad argument #1", rendered)
        self.assertNotIn("Failed to queue research", rendered)

    def test_should_reflect_only_at_positive_interval_multiples(self):
        self.assertFalse(journal.should_reflect(0, interval=16))
        self.assertFalse(journal.should_reflect(15, interval=16))
        self.assertTrue(journal.should_reflect(16, interval=16))
        self.assertTrue(journal.should_reflect(32, interval=16))
        self.assertFalse(journal.should_reflect(16, interval=0))

    def test_parse_reflection_extracts_two_buckets_and_tolerates_partial_blocks(self):
        parsed = journal.parse_reflection(
            """Visible text.
<reflection>
structures:
- Smelting line west of spawn
- Labs near the bus
error_tips:
- Verify inserter direction after placement
- Check power before expanding
</reflection>
"""
        )

        self.assertEqual(
            parsed["structures"],
            ["Smelting line west of spawn", "Labs near the bus"],
        )
        self.assertEqual(
            parsed["error_tips"],
            ["Verify inserter direction after placement", "Check power before expanding"],
        )
        self.assertIsNone(journal.parse_reflection("No reflection here."))
        self.assertEqual(
            journal.parse_reflection("<reflection>\nerror_tips:\n- Avoid dead belts\n</reflection>"),
            {"error_tips": ["Avoid dead belts"]},
        )

    def test_apply_reflection_update_replaces_persists_and_strip_is_idempotent(self):
        journal.apply_reflection_update(
            "doug",
            "<reflection>\nstructures:\n- Old base\nerror_tips:\n- Old tip\n</reflection>",
        )

        updated = journal.apply_reflection_update(
            "doug",
            "Done.\n<reflection>\nstructures:\n- New mall\nerror_tips:\n- New tip\n</reflection>",
        )

        self.assertEqual(updated["structures"], ["New mall"])
        self.assertEqual(updated["error_tips"], ["New tip"])
        self.assertTrue(updated["updated_at"])
        self.assertEqual(journal.load_reflection("doug")["structures"], ["New mall"])

        text = "Before.\n\n<reflection>\nerror_tips:\n- Hidden\n</reflection>\n\nAfter."
        self.assertEqual(journal.strip_reflection_trailer(text), "Before.\n\nAfter.")
        self.assertEqual(
            journal.strip_reflection_trailer(journal.strip_reflection_trailer(text)),
            "Before.\n\nAfter.",
        )

    def test_render_memory_empty_and_populated(self):
        self.assertEqual(journal.render_memory([], journal.default_reflection()), "")

        rendered = journal.render_memory(
            [{"kind": "failure", "text": "Inserter faced the wrong way"}],
            {
                "structures": ["Iron smelter at spawn"],
                "error_tips": ["Use rotate_entity after placement"],
                "updated_at": "now",
            },
        )

        self.assertIn("Recent events:", rendered)
        self.assertIn("failure: Inserter faced the wrong way", rendered)
        self.assertIn("EXISTING STRUCTURES", rendered)
        self.assertIn("Iron smelter at spawn", rendered)
        self.assertIn("ERROR TIPS", rendered)
        self.assertIn("Use rotate_entity after placement", rendered)

    def test_journal_helpers_are_total_on_bad_input(self):
        # None/non-str/non-dict inputs must never raise (audit round 1).
        self.assertIsNone(journal.parse_reflection(None))
        self.assertEqual(journal.strip_reflection_trailer(None), "")
        journal.save_reflection("doug", None)  # must not raise
        self.assertEqual(journal.load_reflection("doug"), journal.default_reflection())
        self.assertEqual(journal.render_memory("oops", "nope"), "")
        self.assertEqual(journal.render_memory(["notadict", 42], None), "")
        self.assertIsInstance(journal.render_memory([{"bad": 1}], None), str)

    def test_count_events_ignores_corrupt_lines(self):
        path = self.base / ".journal-doug.jsonl"
        path.write_text('{not json\n{"ts":"t","kind":"progress","text":"ok"}\n')

        self.assertEqual(journal.count_events("doug"), 1)
        self.assertEqual(len(journal.load_events("doug")), 1)

    def test_finalize_reply_applies_reflection_and_journals_ledger_progress(self):
        import pipe

        finalized = pipe._finalize_reply(
            """I placed the first drill.
<ledger>
progress: Placed the first drill
</ledger>
<reflection>
structures:
- Drill on the northern iron patch
error_tips:
- Check fuel after placing burner drills
</reflection>
""",
            "doug",
        )

        self.assertEqual(finalized, "I placed the first drill.")
        self.assertEqual(
            journal.load_reflection("doug")["structures"],
            ["Drill on the northern iron patch"],
        )
        self.assertEqual(journal.load_events("doug")[-1]["text"], "Placed the first drill")

    def test_finalize_reply_journals_meaningful_anomaly_discovery(self):
        import pipe

        finalized = pipe._finalize_reply(
            """[color=1,0.6,0.2]CLASSIFICATION:[/color] Reviewed.

[color=0.6,0.8,1]ACTIONS TAKEN:[/color]
- inspected placement

[color=1,0.4,0.4]ANOMALY:[/color] belt route failed across water

[color=0.4,0.6,0.4]FILED:[/color] recorded
""",
            "doug",
        )

        self.assertIn("CLASSIFICATION", finalized)
        events = journal.load_events("doug")
        self.assertEqual(events[-1]["kind"], "discovery")
        self.assertEqual(events[-1]["text"], "belt route failed across water")

    def test_tool_error_detection_flags_factorio_failures(self):
        import pipe

        classified_failures = {
            '{"error":"cannot place stone furnace"}': pipe.TOOL_RESULT_GAME_REJECTED,
            '{"success":false,"message":"blocked"}': pipe.TOOL_RESULT_GAME_REJECTED,
            '[{"type":"text","text":"Error: invalid type: map, expected a sequence"}]': pipe.TOOL_RESULT_INVALID_REQUEST,
            "Error: entity not found": pipe.TOOL_RESULT_GAME_REJECTED,
            "Cannot place entity at target": pipe.TOOL_RESULT_GAME_REJECTED,
            "Could not route belt to target": pipe.TOOL_RESULT_GAME_REJECTED,
            "not in inventory": pipe.TOOL_RESULT_GAME_REJECTED,
            "no power": pipe.TOOL_RESULT_GAME_REJECTED,
            "route failed": pipe.TOOL_RESULT_GAME_REJECTED,
            (
                '{"success":false,"error":"No labs found! Build a lab first '
                '(requires: 10 iron-gear-wheel, 10 electronic-circuit, 4 transport-belt)",'
                '"action_needed":"build_lab"}'
            ): pipe.TOOL_RESULT_GAME_REJECTED,
            "Error: missing field `success` at line 1 column 135": pipe.TOOL_RESULT_INVALID_REQUEST,
            "Error: Packet too large: 1553350 bytes": pipe.TOOL_RESULT_INVALID_REQUEST,
            (
                '{"success":false,"can_place":false,"entity":"stone-furnace",'
                '"error":"Cannot place entity here","inventory_count":1,'
                '"position":{"x":76,"y":-19}}'
            ): pipe.TOOL_RESULT_GAME_REJECTED,
            (
                '[{"type":"text","text":"Error: expected value at line 1 column 1"}]'
            ): pipe.TOOL_RESULT_INFRASTRUCTURE_FAILURE,
        }

        for text, expected in classified_failures.items():
            with self.subTest(text=text):
                self.assertEqual(pipe._classify_tool_result(text), expected)
                self.assertTrue(pipe._looks_like_tool_error(text))

        self.assertFalse(pipe._looks_like_tool_error('{"success":true}'))
        self.assertEqual(
            pipe._classify_tool_result(
                '{"success":true,"queued":3,"error":"legacy stale error text"}',
                sdk_is_error=True,
            ),
            pipe.TOOL_RESULT_OK,
        )
        self.assertFalse(pipe._looks_like_tool_error(
            '{"success":true,"queued":3,"error":"legacy stale error text"}'
        ))
        self.assertEqual(
            pipe._classify_tool_result(
                '{"success":false,"mined_count":0,"error":null,"inventory":[]}',
                sdk_is_error=True,
            ),
            pipe.TOOL_RESULT_EXPECTED_MISS,
        )
        self.assertFalse(pipe._looks_like_tool_error(
            '{"success":false,"mined_count":0,"error":null,"inventory":[]}'
        ))
        self.assertEqual(
            pipe._classify_tool_result(
                '[{"type":"text","text":"Error: No items of that type in inventory"}]',
                sdk_is_error=True,
            ),
            pipe.TOOL_RESULT_EXPECTED_MISS,
        )
        self.assertFalse(pipe._looks_like_tool_error(
            '[{"type":"text","text":"Error: No items of that type in inventory"}]'
        ))
        self.assertFalse(pipe._looks_like_tool_error(
            "Error: No items of that type in inventory"
        ))
        self.assertEqual(
            pipe._classify_tool_result(
                '[{"type":"text","text":"{\\"success\\":false,'
                '\\"mined_count\\":0,\\"error\\":\\"No minable entity at position\\",'
                '\\"inventory\\":[]}"}]',
                sdk_is_error=True,
            ),
            pipe.TOOL_RESULT_EXPECTED_MISS,
        )
        self.assertFalse(pipe._looks_like_tool_error(
            '[{"type":"text","text":"{\\"success\\":false,'
            '\\"mined_count\\":0,\\"error\\":\\"No minable entity at position\\",'
            '\\"inventory\\":[]}"}]'
        ))
        self.assertFalse(pipe._looks_like_tool_error(
            '[{"type":"text","text":"{\\"error\\": '
            '\\"No electric poles found in area\\"}\\n"}]'
        ))
        self.assertFalse(pipe._looks_like_tool_error(
            '{"allowed":false,"policy_allowed":true,"factorio_allowed":false,'
            '"entity":"burner-mining-drill","position":{"x":45,"y":-35},'
            '"factorio":{"error":"Factorio cannot place entity here"}}'
        ))
        self.assertFalse(pipe._looks_like_tool_error(
            "Error: execute_lua is disabled. Raw Lua execution is an "
            "arbitrary-code-execution surface and is off by default."
        ))
        self.assertFalse(pipe._looks_like_tool_error("nominal scan complete"))
        self.assertFalse(pipe._looks_like_tool_error(
            '[{"type":"text","text":"{\\"technologies\\":[{\\"ready\\":'
            '\\"blocked\\",\\"blockers\\":[\\"labs have no power\\"]}]}"}]'
        ))

    def test_player_message_trailer_is_split_from_tool_result_text(self):
        import pipe

        text, player_messages = pipe._result_text_and_player_messages([{
            "type": "text",
            "text": "Error: expected value at line 1 column 1"
            "\n\n--- Player Messages ---\n[giga]: uncraftable?",
        }])

        self.assertTrue(pipe._looks_like_tool_error(text))
        self.assertIn("expected value", text)
        self.assertNotIn("giga", text)
        self.assertEqual(player_messages, "[giga]: uncraftable?")

    def test_sdk_terminal_error_echo_is_not_rejournaled(self):
        import pipe

        self.assertTrue(pipe._is_sdk_terminal_error_echo(
            "Claude Code returned an error result: Reached maximum number of turns (15)"
        ))
        self.assertFalse(pipe._is_sdk_terminal_error_echo("RCON connection dropped"))

    def test_execute_lua_is_disallowed_unless_operator_enables_raw_lua(self):
        import pipe

        self.assertEqual(
            pipe._disallowed_tools_for_env({}),
            ["mcp__factorioctl__execute_lua"],
        )
        self.assertEqual(
            pipe._disallowed_tools_for_env({"FACTORIOCTL_ALLOW_RAW_LUA": "1"}),
            [],
        )

    def test_mutating_tool_batch_gate_denies_fast_second_mutation(self):
        import pipe

        gate = pipe.MutatingToolBatchGate(
            pipe.logger.bind(agent="test"), window_s=60
        )

        first = asyncio.run(gate.hook(
            {"tool_name": "mcp__factorioctl__place_entity"}, "tool-1", {}
        ))
        second = asyncio.run(gate.hook(
            {"tool_name": "mcp__factorioctl__place_entity"}, "tool-2", {}
        ))

        self.assertEqual(
            first["hookSpecificOutput"]["permissionDecision"], "allow"
        )
        self.assertEqual(second["decision"], "block")
        self.assertEqual(
            second["hookSpecificOutput"]["permissionDecision"], "deny"
        )
        self.assertIn("blocked parallel mutating tool call", second["reason"])
        self.assertFalse(pipe._looks_like_tool_error(second["reason"]))

    def test_mutating_tool_batch_gate_ignores_read_only_tools(self):
        import pipe

        gate = pipe.MutatingToolBatchGate(
            pipe.logger.bind(agent="test"), window_s=60
        )

        read_only = asyncio.run(gate.hook(
            {"tool_name": "mcp__factorioctl__check_placement"}, "tool-1", {}
        ))
        mutating = asyncio.run(gate.hook(
            {"tool_name": "mcp__factorioctl__place_entity"}, "tool-2", {}
        ))

        self.assertEqual(read_only, {})
        self.assertEqual(
            mutating["hookSpecificOutput"]["permissionDecision"], "allow"
        )

    def test_factorio_skill_gate_requires_skill_before_mcp_tools(self):
        import pipe

        gate = pipe.FactorioSkillGate(pipe.logger.bind(agent="test"))

        blocked = asyncio.run(gate.hook(
            {"tool_name": "mcp__factorioctl__situation_report"}, "tool-1", {}
        ))
        allowed_skill = asyncio.run(gate.hook(
            {"tool_name": "Skill"}, "tool-2", {}
        ))
        allowed_mcp = asyncio.run(gate.hook(
            {"tool_name": "mcp__factorioctl__situation_report"}, "tool-3", {}
        ))

        self.assertEqual(blocked["decision"], "block")
        self.assertIn("Call Skill(factorio-control)", blocked["reason"])
        self.assertFalse(pipe._looks_like_tool_error(blocked["reason"]))
        self.assertEqual(
            allowed_skill["hookSpecificOutput"]["permissionDecision"],
            "allow",
        )
        self.assertEqual(allowed_mcp, {})

    def test_factorio_skill_gate_is_disableable(self):
        import pipe

        gate = pipe.FactorioSkillGate(pipe.logger.bind(agent="test"), required=False)

        self.assertEqual(
            asyncio.run(gate.hook(
                {"tool_name": "mcp__factorioctl__situation_report"},
                "tool-1",
                {},
            )),
            {},
        )

    def test_factorio_tool_schema_gate_blocks_bad_params_before_mcp(self):
        import pipe

        gate = pipe.FactorioToolSchemaGate(pipe.logger.bind(agent="test"))

        blocked = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__place_entity",
                "tool_input": {
                    "entity_name": "stone-furnace",
                    "x": {"bad": True},
                    "y": 0,
                },
            },
            "tool-1",
            {},
        ))

        self.assertEqual(blocked["decision"], "block")
        self.assertEqual(
            blocked["hookSpecificOutput"]["permissionDecision"],
            "deny",
        )
        self.assertIn("place_entity: tool_input.x: expected number", blocked["reason"])
        self.assertIn("invalid Factorio tool parameters", blocked["reason"])
        self.assertFalse(pipe._looks_like_tool_error(blocked["reason"]))

    def test_factorio_tool_schema_gate_allows_valid_and_unknown_tools(self):
        import pipe

        gate = pipe.FactorioToolSchemaGate(pipe.logger.bind(agent="test"))

        valid = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__place_entity",
                "tool_input": {
                    "entity_name": "stone-furnace",
                    "x": 42.0,
                    "y": -3,
                    "direction": "south",
                },
            },
            "tool-1",
            {},
        ))
        unknown = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__future_tool",
                "tool_input": {"whatever": {"shape": "kept"}},
            },
            "tool-2",
            {},
        ))
        non_factorio = asyncio.run(gate.hook(
            {
                "tool_name": "Skill",
                "tool_input": {"name": "factorio-control"},
            },
            "tool-3",
            {},
        ))

        self.assertEqual(valid, {})
        self.assertEqual(unknown, {})
        self.assertEqual(non_factorio, {})

    def test_max_turn_default_is_raised_and_env_tunable(self):
        import pipe

        self.assertEqual(pipe.DEFAULT_MAX_TURNS, 200)
        self.assertEqual(pipe._resolve_max_turns(None), 200)
        self.assertEqual(pipe._resolve_max_turns(25), 25)

        with mock.patch.dict("os.environ", {"BRIDGE_MAX_TURNS": "80"}):
            self.assertEqual(pipe._resolve_max_turns(None), 80)

        with mock.patch.dict("os.environ", {"BRIDGE_MAX_TURNS": "nope"}):
            self.assertEqual(pipe._resolve_max_turns(None), 200)

    def test_sdk_skills_default_to_project_skill_and_are_disableable(self):
        import pipe

        with mock.patch.dict("os.environ", {}, clear=True):
            self.assertEqual(pipe._resolve_sdk_skills(None), ["factorio-control"])
        self.assertEqual(pipe._resolve_sdk_skills("factorio-control,other"), [
            "factorio-control",
            "other",
        ])
        self.assertEqual(pipe._resolve_sdk_skills("all"), "all")
        self.assertEqual(pipe._resolve_sdk_skills("none"), [])
        self.assertEqual(pipe._claude_tools_for_sdk_skills(["factorio-control"]), ["Skill"])
        self.assertEqual(pipe._claude_tools_for_sdk_skills([]), [])
        self.assertEqual(
            pipe._setting_sources_for_sdk_skills(["factorio-control"]),
            ["project", "local"],
        )
        self.assertEqual(pipe._setting_sources_for_sdk_skills([]), ["local"])

    def test_handle_message_enables_sdk_skill_without_shell_tools(self):
        import pipe

        captured = {}

        def scripted_query(*, prompt, options):
            captured["prompt"] = prompt
            captured["options"] = options

            async def gen():
                if False:
                    yield None

            return gen()

        class StubRCON:
            def execute(self, _cmd):
                return ""

        with mock.patch("pipe.query", scripted_query):
            pipe.handle_message(
                "go",
                {},
                "system",
                None,
                StubRCON(),
                0,
                None,
                agent_name="doug",
                sdk_skills=["factorio-control"],
            )

        options = captured["options"]
        self.assertEqual(options.skills, ["factorio-control"])
        self.assertEqual(options.tools, ["Skill"])
        self.assertEqual(options.setting_sources, ["project", "local"])
        self.assertEqual(options.cwd, pipe._PROJECT_ROOT)

    def test_sdk_skill_init_and_tool_use_are_observable(self):
        import pipe
        from claude_agent_sdk import ClaudeAgentOptions, SystemMessage, ToolUseBlock

        class CapturingLog:
            def __init__(self):
                self.messages = []

            def info(self, template, *args):
                self.messages.append((template, args))

        log = CapturingLog()
        options = ClaudeAgentOptions(skills=["factorio-control"])

        logged = pipe._log_sdk_init(
            SystemMessage(
                subtype="init",
                data={
                    "cwd": str(pipe._PROJECT_ROOT),
                    "tools": ["Skill", "mcp__factorioctl__walk_to"],
                    "skills": ["factorio-control"],
                },
            ),
            options,
            log,
        )

        self.assertTrue(logged)
        self.assertIn("sdk init", log.messages[0][0])
        self.assertFalse(pipe._should_log_system_message(SystemMessage(
            subtype="thinking_tokens",
            data={"estimated_tokens": 1},
        )))
        self.assertTrue(pipe._should_log_system_message(SystemMessage(
            subtype="error",
            data={"message": "visible diagnostic"},
        )))
        self.assertTrue(pipe._is_skill_tool(ToolUseBlock(
            id="s1",
            name="Skill",
            input={"skill": "factorio-control"},
        )))

    def test_usage_limit_reset_infers_provider_timezone_from_request_id(self):
        import pipe

        text = (
            "API Error: Request rejected (429) · [1308][Usage limit reached "
            "for 5 hour. Your limit will reset at 2026-06-29 08:35:15]"
            "[202606290714523c923559680c406d]"
        )
        now = datetime(2026, 6, 28, 23, 14, 52, tzinfo=timezone.utc)

        reset = pipe._usage_limit_reset_at(text, now)

        self.assertIsNotNone(reset)
        self.assertEqual(reset.utcoffset(), timedelta(hours=8))
        self.assertEqual(
            reset.astimezone(timezone.utc),
            datetime(2026, 6, 29, 0, 35, 15, tzinfo=timezone.utc),
        )

    def test_usage_limit_cooldown_blocks_human_message_without_model_call(self):
        import queue as std_queue

        import pipe

        class StubRCON:
            def __init__(self):
                self.commands = []

            def execute(self, cmd):
                self.commands.append(cmd)
                return ""

        reset = datetime.now(timezone.utc) + timedelta(hours=1)
        pipe._USAGE_LIMIT_COOLDOWNS.clear()
        pipe._USAGE_LIMIT_COOLDOWNS["doug"] = reset
        self.addCleanup(pipe._USAGE_LIMIT_COOLDOWNS.clear)

        thread = pipe.AgentThread.__new__(pipe.AgentThread)
        thread.agent_name = "doug"
        thread.telemetry_name = "doug"
        thread.telemetry = None
        thread.rcon = StubRCON()
        thread.log = pipe.logger.bind(agent="doug")
        thread.heartbeat_interval = 0
        thread.inbox = std_queue.Queue()
        thread.inbox.put({
            "message": "hi",
            "player_index": 1,
            "player_name": "giga",
        })

        with mock.patch("pipe.handle_message", side_effect=AssertionError("called model")):
            thread._run_once()

        joined = "\n".join(thread.rcon.commands)
        self.assertIn("Provider usage limit is active", joined)
        self.assertIn("Ready", joined)

    def test_run_agent_records_usage_limit_cooldown_without_failure_event(self):
        import asyncio

        import pipe
        from claude_agent_sdk import ResultMessage

        provider_now = datetime.now(timezone.utc) + timedelta(hours=8)
        provider_reset = provider_now + timedelta(hours=1)
        limit_text = (
            "API Error: Request rejected (429) · [1308][Usage limit reached "
            f"for 5 hour. Your limit will reset at {provider_reset:%Y-%m-%d %H:%M:%S}]"
            f"[{provider_now:%Y%m%d%H%M%S}abcdef]"
        )
        messages = [
            ResultMessage(
                subtype="error",
                duration_ms=1,
                duration_api_ms=1,
                is_error=True,
                num_turns=1,
                session_id="s",
                result=limit_text,
                total_cost_usd=0.0,
            )
        ]

        def scripted_query(*, prompt, options):
            async def gen():
                for msg in messages:
                    yield msg
            return gen()

        class StubRCON:
            def execute(self, _cmd):
                return ""

        pipe._USAGE_LIMIT_COOLDOWNS.clear()
        self.addCleanup(pipe._USAGE_LIMIT_COOLDOWNS.clear)

        with mock.patch("pipe.query", scripted_query):
            asyncio.run(pipe._run_agent(
                "go", object(), "doug", None, "doug",
                StubRCON(), 0, pipe.logger.bind(agent="doug"),
            ))

        self.assertEqual(journal.load_events("doug"), [])
        self.assertIsNotNone(pipe._get_usage_limit_cooldown("doug"))

    def test_run_agent_journals_sdk_tool_result_failures(self):
        # The whole point of the SDK migration: tool failures arrive as
        # ToolResultBlocks inside UserMessage.content (list) AND, from some
        # GLM adapters, as a bare UserMessage.content string. Both must be
        # journaled; successes and plain narration must not.
        import asyncio

        import pipe
        from claude_agent_sdk import ToolResultBlock, UserMessage

        messages = [
            UserMessage(content=[ToolResultBlock(
                tool_use_id="t1", content="ok scan complete", is_error=False)]),
            UserMessage(content=[ToolResultBlock(
                tool_use_id="t2", content=[{"type": "text", "text": "boom"}], is_error=True)]),
            UserMessage(content=[ToolResultBlock(
                tool_use_id="t3", content="Error: cannot place stone furnace", is_error=False)]),
            UserMessage(content=[ToolResultBlock(
                tool_use_id="t4",
                content=[{
                    "type": "text",
                    "text": "Error: invalid JSON"
                    "\n\n--- Player Messages ---\n[giga]: I put wood in a chest",
                }],
                is_error=False,
            )]),
            UserMessage(content="Error: invalid type: map, expected a sequence"),
            UserMessage(content="just narrating, nothing wrong here"),
            UserMessage(content=[ToolResultBlock(
                tool_use_id="t5",
                content="Error: No items of that type in inventory",
                is_error=True,
            )]),
            UserMessage(content=[ToolResultBlock(
                tool_use_id="t6",
                content='{"success":false,"mined_count":0,"error":null,"inventory":[]}',
                is_error=True,
            )]),
            UserMessage(content=[ToolResultBlock(
                tool_use_id="t7",
                content='{"success":true,"queued":1,"error":"Crafting did not start"}',
                is_error=True,
            )]),
            UserMessage(content=[ToolResultBlock(
                tool_use_id="t8",
                content=(
                    "Factorioctl bridge blocked parallel mutating tool call: "
                    "insert_items. Wait for the previous mutating tool result "
                    "before issuing another world/inventory-changing command."
                ),
                is_error=True,
            )]),
        ]

        def scripted_query(*, prompt, options):
            async def gen():
                for m in messages:
                    yield m
            return gen()

        class StubRCON:
            def execute(self, _cmd):
                return ""

        with mock.patch("pipe.query", scripted_query):
            asyncio.run(pipe._run_agent(
                "go", object(), "doug", None, "doug",
                StubRCON(), 0, pipe.logger.bind(agent="doug"),
            ))

        texts = [event["text"] for event in journal.load_events("doug")]
        # is_error=True, error-text list-blocks, and string-wrapped error -> 4 failures
        self.assertEqual(len(texts), 4)
        self.assertTrue(any(t.startswith("sdk_failure:") and "boom" in t for t in texts))
        self.assertTrue(any(t.startswith("game_rejected:") and "cannot place stone furnace" in t for t in texts))
        self.assertTrue(any(t.startswith("invalid_request:") and "invalid JSON" in t for t in texts))
        self.assertTrue(any(t.startswith("invalid_request:") and "invalid type: map" in t for t in texts))
        self.assertFalse(any("giga" in t for t in texts))
        # success result, expected miss, and benign narration must NOT be journaled
        self.assertFalse(any("ok scan complete" in t for t in texts))
        self.assertFalse(any("narrating" in t for t in texts))
        self.assertFalse(any("No items of that type" in t for t in texts))
        self.assertFalse(any("mined_count" in t for t in texts))
        self.assertFalse(any("Crafting did not start" in t for t in texts))
        self.assertFalse(any("parallel mutating tool call" in t for t in texts))

    def test_anomaly_filter_ignores_nominal_variants(self):
        import pipe

        for text in ("None", "nominal", "no anomalies found", "none detected"):
            with self.subTest(text=text):
                self.assertFalse(pipe._is_meaningful_anomaly(text))

    def test_autonomy_tick_injects_memory_and_periodic_reflection_nudge(self):
        import ledger
        import pipe

        ledger_patch = mock.patch(
            "ledger._ledger_file",
            side_effect=lambda agent_name: self.base / f".ledger-{agent_name}.json",
        )
        ledger_patch.start()
        self.addCleanup(ledger_patch.stop)
        ledger.save_ledger("doug", {
            "objective": "Build starter power",
            "plan_steps": ["Place boiler"],
            "progress_notes": [],
            "updated_at": "now",
        })
        for i in range(2):
            journal.append_event("doug", "failure", f"failure {i}")
        journal.save_reflection("doug", {
            "structures": ["Boiler area near water"],
            "error_tips": ["Confirm offshore pump water connection"],
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
        thread._reflect_interval = 2

        prompt = thread._compose_autonomy_prompt()

        self.assertIn("failure: failure 1", prompt)
        self.assertIn("Boiler area near water", prompt)
        self.assertIn("ERROR TIPS", prompt)
        self.assertIn("<reflection>", prompt)
        self.assertIn("what is built where", prompt)


if __name__ == "__main__":
    unittest.main()
