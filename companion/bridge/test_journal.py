import asyncio
import json
import tempfile
import unittest
from datetime import datetime, timedelta, timezone
from pathlib import Path
from unittest import mock

import journal
from models import (
    BridgeInputMessage,
    JournalEvent,
    JournalWindow,
    LiveState,
    ParsedAgentResponse,
    ReflectionDraft,
    ReflectionMemory,
    SdkSystemMessage,
    ToolParamSchemaRegistry,
    ToolResultClassification,
    ToolResultOutcome,
)


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

    def test_model_event_loader_preserves_typed_window_and_legacy_wrapper(self):
        journal.append_event("doug", "discovery", "Found copper east of spawn")
        journal.append_event("doug", "unknown", "Fallback kind is progress")
        journal.append_event("doug", "milestone", "Starter power is online")

        window = journal.load_events_model("doug", limit=2)
        legacy = journal.load_events("doug", limit=2)

        self.assertIsInstance(window, JournalWindow)
        self.assertEqual([event.kind for event in window.events], ["progress", "milestone"])
        self.assertEqual([event.to_dict() for event in window.events], legacy)

    def test_load_events_and_reflection_are_total_for_missing_and_corrupt_files(self):
        self.assertEqual(journal.load_events("doug"), [])
        self.assertEqual(journal.load_reflection("doug"), journal.default_reflection())
        self.assertEqual(journal.load_events_model("doug"), JournalWindow())
        self.assertEqual(journal.load_reflection_model("doug"), journal.default_reflection_model())

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
            '{"ts": "t4b", "kind": "failure", "text": "tick timeout after 2400s"}\n'
            '{"ts": "t4c", "kind": "failure", "text": "sdk_failure: API Error: The model has reached its context window limit."}\n'
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
        journal.append_event("doug", "failure", "tick timeout after 2400s")
        journal.append_event("doug", "failure", "API Error: The model has reached its context window limit.")
        journal.append_event("doug", "failure", '{"error": "No electric poles found in area"}')
        journal.append_event("doug", "progress", "situation assessed; no infrastructure yet deployed")
        events = journal.load_events("doug")
        rendered = journal.render_memory(events, journal.default_reflection())

        self.assertEqual([event["text"] for event in events], [
            "situation assessed; no infrastructure yet deployed",
            "Inserter faced the wrong way",
            "situation assessed; no infrastructure yet deployed",
        ])
        self.assertIn("Inserter faced the wrong way", rendered)
        self.assertIn("situation assessed", rendered)
        self.assertNotIn("Usage limit", rendered)
        self.assertNotIn("maximum number of turns", rendered)
        self.assertNotIn("stream idle timeout", rendered)
        self.assertNotIn("tick timeout", rendered)
        self.assertNotIn("context window", rendered)
        self.assertNotIn("No electric poles", rendered)
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

    def test_reflection_model_api_round_trips_and_render_accepts_models(self):
        draft = journal.parse_reflection_model(
            "<reflection>\n"
            "structures:\n"
            "- Iron smelter at spawn\n"
            "error_tips:\n"
            "- Use rotate_entity after placement\n"
            "</reflection>"
        )
        self.assertIsInstance(draft, ReflectionDraft)
        self.assertEqual(journal.parse_reflection_model(draft), draft)

        updated = journal.apply_reflection_update_model(
            "doug",
            draft,
        )
        loaded = journal.load_reflection_model("doug")
        window = journal.load_events_model("doug")
        rendered = journal.render_memory(window, loaded)

        self.assertIsInstance(updated, ReflectionMemory)
        self.assertEqual(loaded, updated)
        self.assertEqual(updated.structures, ["Iron smelter at spawn"])
        self.assertIn("Iron smelter at spawn", rendered)
        self.assertEqual(journal.load_reflection("doug"), updated.to_dict())

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

    def test_reflection_update_filters_noise_dedupes_and_caps_items(self):
        long_tip = "Use route_belt diagnostics before rebuilding " + ("again " * 80)

        updated = journal.apply_reflection_update(
            "doug",
            "\n".join([
                "<reflection>",
                "structures:",
                "- Steam power at (-40, 26) feeds the lab bus",
                "- API Error: The model has reached its context-window limit.",
                "- no infrastructure yet deployed",
                "- Steam power at (-40, 26) feeds the lab bus",
                "error_tips:",
                "- Provider usage limit active until later",
                "- Check inserter direction with rotate_entity before rebuilding",
                f"- {long_tip}",
                "</reflection>",
            ]),
        )

        self.assertEqual(
            updated["structures"],
            ["Steam power at (-40, 26) feeds the lab bus"],
        )
        self.assertEqual(len(updated["error_tips"]), 2)
        self.assertIn(
            "Check inserter direction with rotate_entity before rebuilding",
            updated["error_tips"],
        )
        self.assertTrue(updated["error_tips"][1].endswith("..."))
        self.assertLessEqual(
            len(updated["error_tips"][1]),
            journal.MAX_REFLECTION_ITEM_TEXT + 3,
        )
        rendered = journal.render_memory([], journal.load_reflection("doug"))
        self.assertNotIn("context-window", rendered)
        self.assertNotIn("Provider usage limit", rendered)
        self.assertEqual(rendered.count("Steam power at"), 1)

    def test_load_reflection_normalizes_existing_noisy_reflection_file(self):
        (self.base / ".reflection-doug.json").write_text(json.dumps({
            "structures": [
                "Lab and steam power near the west lake",
                "Lab and steam power near the west lake",
                "fresh deployment assessment",
                "API Error: context window limit",
            ],
            "error_tips": [
                "No electric poles found in area",
                "Use find_entity_placements before placing furnaces on ore",
            ],
            "updated_at": "old",
        }))

        loaded = journal.load_reflection("doug")

        self.assertEqual(
            loaded["structures"],
            ["Lab and steam power near the west lake"],
        )
        self.assertEqual(
            loaded["error_tips"],
            ["Use find_entity_placements before placing furnaces on ore"],
        )

    def test_render_memory_empty_and_populated(self):
        self.assertEqual(journal.render_memory([], journal.default_reflection()), "")
        from models import ReflectionMemory

        rendered = journal.render_memory(
            [{"kind": "failure", "text": "Inserter faced the wrong way"}],
            ReflectionMemory(
                structures=["Iron smelter at spawn"],
                error_tips=["Use rotate_entity after placement"],
                updated_at="now",
            ),
        )

        self.assertIn("Recent events:", rendered)
        self.assertIn("failure: Inserter faced the wrong way", rendered)
        self.assertIn("EXISTING STRUCTURES", rendered)
        self.assertIn("Iron smelter at spawn", rendered)
        self.assertIn("ERROR TIPS", rendered)
        self.assertIn("Use rotate_entity after placement", rendered)

    def test_render_memory_coalesces_repeated_events_before_prompt_limit(self):
        events = [
            {"kind": "progress", "text": "Coal drill built"},
            {"kind": "failure", "text": "game_rejected: Cannot place belt at 56,-24"},
            {"kind": "failure", "text": "game_rejected: Cannot place belt at 56,-24"},
            {"kind": "failure", "text": "game_rejected: Cannot place belt at 56,-24"},
            {"kind": "failure", "text": "watchdog_abort: repeated same game rejection"},
            {"kind": "progress", "text": "Queued logistics research"},
        ]

        rendered = journal.render_memory(events, journal.default_reflection())

        self.assertIn("progress: Coal drill built", rendered)
        self.assertIn("failure (x3): game_rejected: Cannot place belt at 56,-24", rendered)
        self.assertIn("failure: watchdog_abort", rendered)
        self.assertIn("progress: Queued logistics research", rendered)
        self.assertEqual(rendered.count("Cannot place belt"), 1)

    def test_render_memory_accepts_typed_journal_events(self):
        events = [
            JournalEvent.create(ts="1", kind="progress", text="Plan ready"),
            JournalEvent.create(ts="2", kind="progress", text="Plan ready"),
            JournalEvent.create(ts="3", kind="milestone", text="Furnace working"),
        ]

        window = JournalWindow.coerce(events)
        rendered = journal.render_memory(window, journal.default_reflection())

        self.assertEqual([event.text for event in window.events], [
            "Plan ready",
            "Plan ready",
            "Furnace working",
        ])
        self.assertIn("progress (x2): Plan ready", rendered)
        self.assertIn("milestone: Furnace working", rendered)

    def test_render_memory_redacts_player_names_from_prompt_events(self):
        rendered = journal.render_memory(
            [
                {
                    "kind": "progress",
                    "text": (
                        "player_messages: [TestPlayer]: hello. "
                        "Keep [item=iron-ore]."
                    ),
                },
            ],
            journal.default_reflection(),
        )

        self.assertNotIn("TestPlayer", rendered)
        self.assertIn("[player]: hello", rendered)
        self.assertIn("[item=iron-ore]", rendered)

    def test_render_memory_preserves_progress_when_distinct_failures_fill_window(self):
        events = [{"kind": "progress", "text": "Lab powered"}]
        events.extend(
            {"kind": "failure", "text": f"game_rejected: Cannot place belt {i}"}
            for i in range(journal.MAX_RENDERED_EVENTS + 1)
        )

        rendered = journal.render_memory(events, journal.default_reflection())

        self.assertIn("progress: Lab powered", rendered)
        self.assertIn("failure: game_rejected: Cannot place belt 5", rendered)
        self.assertNotIn("Cannot place belt 0", rendered)
        self.assertEqual(
            len([line for line in rendered.splitlines() if line.startswith("- ")]),
            journal.MAX_RENDERED_EVENTS,
        )

    def test_render_memory_truncates_large_events(self):
        rendered = journal.render_memory(
            [{"kind": "failure", "text": "x" * (journal.MAX_RENDERED_EVENT_TEXT + 50)}],
            journal.default_reflection(),
        )

        self.assertLess(len(rendered), journal.MAX_RENDERED_EVENT_TEXT + 80)
        self.assertIn("...", rendered)

    def test_parse_response_uses_typed_shape_but_keeps_telemetry_dict(self):
        import pipe

        parsed = pipe.parse_response(
            "[color=1,0.6,0.2]CLASSIFICATION:[/color] Done\n\n"
            "Body text.\n\n"
            "[color=0.6,0.8,1]ACTIONS TAKEN:[/color]\n"
            "- placed belt\n"
            "- verified furnace\n\n"
            "[color=1,0.4,0.4]ANOMALY:[/color] belt blocked\n\n"
            "[color=0.4,0.6,0.4]FILED:[/color] complete"
        )

        self.assertEqual(parsed["header"]["label"], "CLASSIFICATION")
        self.assertEqual(parsed["body"], "Body text.")
        self.assertEqual(parsed["actions"], ["placed belt", "verified furnace"])
        self.assertEqual(parsed["data"]["ANOMALY"]["text"], "belt blocked")
        self.assertEqual(parsed["footer"]["text"], "complete")

    def test_parse_response_model_exposes_anomaly_text_without_dict_spelunking(self):
        import pipe

        parsed = pipe.parse_response_model(
            "[color=1,0.6,0.2]CLASSIFICATION:[/color] Reviewed\n\n"
            "[color=1,0.4,0.4]ANOMALY:[/color] route crosses water"
        )

        self.assertEqual(parsed.anomaly_text(), "route crosses water")

    def test_parse_response_preserves_body_shape_for_empty_text(self):
        import pipe

        self.assertEqual(pipe.parse_response(""), {"body": ""})

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

    def test_finalize_reply_persists_structured_ledger_signal(self):
        import pipe

        pipe._finalize_reply(
            """New plan.
<ledger>
objective: Activate second furnace
plan:
- walk_to (42, -21)
progress: previous objective complete; second furnace selected
signal: new_objective
</ledger>
""",
            "doug",
        )

        event = journal.load_events("doug")[-1]
        self.assertEqual(event["text"], "previous objective complete; second furnace selected")
        self.assertEqual(event["signal"], "new_objective")

    def test_finalize_reply_infers_plan_ready_when_signal_omitted(self):
        import pipe

        pipe._finalize_reply(
            """Plan confirmed.
<ledger>
objective: Activate second furnace
plan:
- walk_to (42, -21)
- insert_items coal count=5 into fuel inventory of unit 15
progress: plan confirmed; awaiting execution
</ledger>
""",
            "doug",
        )

        event = journal.load_events("doug")[-1]
        self.assertEqual(event["text"], "plan confirmed; awaiting execution")
        self.assertEqual(event["signal"], "plan_ready")

    def test_signal_bearing_progress_survives_low_value_filter(self):
        journal.append_event(
            "doug",
            "progress",
            "plan validated and ready for execution",
            signal="plan_ready",
        )

        events = journal.load_events("doug")

        self.assertEqual(events[-1]["text"], "plan validated and ready for execution")
        self.assertEqual(events[-1]["signal"], "plan_ready")

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

        self.assertTrue(ToolResultOutcome.from_text(
            '{"success":false,"can_place":false,"entity":"transport-belt",'
            '"error":"Cannot place entity here","position":{"x":56,"y":-25}}'
        ).indicates_failure)
        self.assertFalse(ToolResultOutcome.from_text('{"success":true}').indicates_failure)
        self.assertTrue(ToolResultOutcome.text_indicates_progress(
            '{"success":true,"queued":3,"error":"legacy stale error text"}'
        ))
        self.assertFalse(ToolResultOutcome.from_text(
            '[{"type":"text","text":"{\\"researched_count\\":6,'
            '\\"total_count\\":275,\\"research_progress\\":0.36,'
            '\\"research_queue\\":[{\\"name\\":\\"steel-processing\\"}],'
            '\\"labs\\":{\\"count\\":1,\\"powered\\":0,\\"working\\":0},'
            '\\"message\\":\\"Labs have no power! Connect labs to the power grid.\\"}"}]'
        ).indicates_failure)

    def test_player_message_trailer_is_split_from_tool_result_text(self):
        import pipe

        text, player_messages = pipe._result_text_and_player_messages([{
            "type": "text",
            "text": "Error: expected value at line 1 column 1"
            "\n\n--- Player Messages ---\n[TestPlayer]: uncraftable?",
        }])

        self.assertTrue(ToolResultOutcome.from_text(text).indicates_failure)
        self.assertIn("expected value", text)
        self.assertNotIn("TestPlayer", text)
        self.assertEqual(player_messages, "[TestPlayer]: uncraftable?")

    def test_execute_lua_is_disallowed_unless_operator_enables_raw_lua(self):
        import pipe

        self.assertEqual(
            pipe._disallowed_tools_for_env({}),
            ["mcp__factorioctl__execute_lua"],
        )
        self.assertEqual(
            pipe._disallowed_tools_for_env({"FACTORIOCTL_ALLOW_RAW_LUA": " TRUE "}),
            [],
        )
        self.assertEqual(
            pipe._disallowed_tools_for_env({"FACTORIOCTL_ALLOW_RAW_LUA": "false"}),
            ["mcp__factorioctl__execute_lua"],
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
        self.assertFalse(ToolResultOutcome.from_text(second["reason"]).indicates_failure)

    def test_mutating_tool_batch_gate_ignores_read_only_tools(self):
        import pipe

        gate = pipe.MutatingToolBatchGate(
            pipe.logger.bind(agent="test"), window_s=60
        )

        read_only = asyncio.run(gate.hook(
            {"tool_name": "mcp__factorioctl__check_placement"}, "tool-1", {}
        ))
        mutating = asyncio.run(gate.hook(
            {"tool_name": "mcp__factorioctl__rotate_entity"}, "tool-2", {}
        ))

        self.assertEqual(read_only, {})
        self.assertEqual(
            mutating["hookSpecificOutput"]["permissionDecision"], "allow"
        )

    def test_manual_automation_gate_blocks_manual_transfer_when_forced(self):
        import pipe

        gate = pipe.ManualAutomationDriftGate(
            pipe.logger.bind(agent="test"),
            agent_name="doug",
            block_all_manual_transfers=True,
        )

        blocked = asyncio.run(gate.hook({
            "tool_name": "mcp__factorioctl__extract_items",
            "tool_input": {
                "unit_number": 14,
                "item": "iron-plate",
                "count": 5,
                "inventory_type": "furnace_result",
            },
        }, "tool-1", {}))
        read_only = asyncio.run(gate.hook({
            "tool_name": "mcp__factorioctl__situation_report",
            "tool_input": {"radius": 10},
        }, "tool-2", {}))

        self.assertEqual(blocked["decision"], "block")
        self.assertIn("blocked stale manual automation tool", blocked["reason"])
        self.assertEqual(read_only, {})

    def test_hook_gates_accept_sdk_object_hook_input(self):
        import pipe

        class HookInput:
            def __init__(self, tool_name, tool_input=None):
                self.tool_name = tool_name
                self.tool_input = tool_input or {}

        mutating_gate = pipe.MutatingToolBatchGate(
            pipe.logger.bind(agent="test"), window_s=60
        )
        read_only_gate = pipe.PlannerReadOnlyToolGate(
            pipe.logger.bind(agent="test"), enabled=True
        )
        skill_gate = pipe.FactorioSkillGate(pipe.logger.bind(agent="test"))

        first = asyncio.run(mutating_gate.hook(
            HookInput("mcp__factorioctl__place_entity"), "tool-1", {}
        ))
        second = asyncio.run(mutating_gate.hook(
            HookInput("mcp__factorioctl__place_entity"), "tool-2", {}
        ))
        lab_feed_dry_run = asyncio.run(read_only_gate.hook(
            HookInput("mcp__factorioctl__feed_lab_from_inventory", {
                "lab_unit_number": 42,
                "science_pack": "automation-science-pack",
                "count": 5,
            }),
            "tool-3",
            {},
        ))
        blocked_before_skill = asyncio.run(skill_gate.hook(
            HookInput("mcp__factorioctl__situation_report"), "tool-4", {}
        ))

        self.assertEqual(
            first["hookSpecificOutput"]["permissionDecision"],
            "allow",
        )
        self.assertEqual(second["decision"], "block")
        self.assertEqual(lab_feed_dry_run, {})
        self.assertEqual(blocked_before_skill["decision"], "block")

    def test_planner_read_only_gate_blocks_mutations_and_allows_diagnostics(self):
        import pipe

        gate = pipe.PlannerReadOnlyToolGate(
            pipe.logger.bind(agent="test"), enabled=True
        )

        blocked = asyncio.run(gate.hook(
            {"tool_name": "mcp__factorioctl__rotate_entity"}, "tool-1", {}
        ))
        unknown = asyncio.run(gate.hook(
            {"tool_name": "mcp__factorioctl__future_write_tool"}, "tool-2", {}
        ))
        diagnostic = asyncio.run(gate.hook(
            {"tool_name": "mcp__factorioctl__check_placement"}, "tool-3", {}
        ))
        edge_miner_plan = asyncio.run(gate.hook(
            {"tool_name": "mcp__factorioctl__build_edge_miner"}, "tool-8", {}
        ))
        direct_smelter_plan = asyncio.run(gate.hook(
            {"tool_name": "mcp__factorioctl__build_direct_smelter"}, "tool-9", {}
        ))
        lab_feed_plan = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__feed_lab_from_inventory",
                "tool_input": {
                    "lab_unit_number": 42,
                    "science_pack": "automation-science-pack",
                    "count": 5,
                },
            },
            "tool-10",
            {},
        ))
        lab_feed_execute = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__feed_lab_from_inventory",
                "tool_input": {
                    "lab_unit_number": 42,
                    "science_pack": "automation-science-pack",
                    "count": 5,
                    "dry_run": False,
                },
            },
            "tool-11",
            {},
        ))
        automation_science_plan = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__build_automation_science",
                "tool_input": {"dry_run": True},
            },
            "tool-12",
            {},
        ))
        automation_science_execute = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__build_automation_science",
                "tool_input": {"dry_run": False},
            },
            "tool-13",
            {},
        ))
        repair_plan = asyncio.run(gate.hook(
            {"tool_name": "mcp__factorioctl__repair_steam_power"}, "tool-6", {}
        ))
        extend_plan = asyncio.run(gate.hook(
            {"tool_name": "mcp__factorioctl__extend_power_to"}, "tool-7", {}
        ))
        skill = asyncio.run(gate.hook({"tool_name": "Skill"}, "tool-4", {}))
        disabled = asyncio.run(pipe.PlannerReadOnlyToolGate(
            pipe.logger.bind(agent="test"), enabled=False
        ).hook(
            {"tool_name": "mcp__factorioctl__place_entity"}, "tool-5", {}
        ))

        self.assertEqual(blocked["decision"], "block")
        self.assertIn("planner/reflection turn", blocked["reason"])
        self.assertIn("ledger-only plan", blocked["reason"])
        self.assertFalse(ToolResultOutcome.from_text(blocked["reason"]).indicates_failure)
        self.assertEqual(unknown["decision"], "block")
        self.assertEqual(diagnostic, {})
        self.assertEqual(edge_miner_plan, {})
        self.assertEqual(direct_smelter_plan, {})
        self.assertEqual(lab_feed_plan, {})
        self.assertEqual(lab_feed_execute["decision"], "block")
        self.assertEqual(automation_science_plan, {})
        self.assertEqual(automation_science_execute["decision"], "block")
        self.assertEqual(repair_plan, {})
        self.assertEqual(extend_plan, {})
        self.assertEqual(skill, {})
        self.assertEqual(disabled, {})

    def test_manual_automation_drift_gate_blocks_manual_transfer_tools_for_stale_ledger(self):
        import pipe
        from models import LedgerState

        stale = LedgerState(
            objective="deliver more automation science by hand",
            plan_steps=[
                "craft automation-science-pack count=12",
                "feed_lab_from_inventory lab_unit_number=69 science_pack=automation-science-pack count=12 dry_run=false",
            ],
        )
        fresh = LedgerState(
            objective="bootstrap first smelting",
            plan_steps=["insert_items coal count=1 into first furnace"],
        )
        gate = pipe.ManualAutomationDriftGate(
            pipe.logger.bind(agent="test"),
            agent_name="doug",
            ledger_loader=lambda agent_name: stale,
        )
        fresh_gate = pipe.ManualAutomationDriftGate(
            pipe.logger.bind(agent="test"),
            agent_name="doug",
            ledger_loader=lambda agent_name: fresh,
        )

        blocked = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__feed_lab_from_inventory",
                "tool_input": {
                    "lab_unit_number": 69,
                    "science_pack": "automation-science-pack",
                    "count": 12,
                    "dry_run": False,
                },
            },
            "tool-1",
            {},
        ))
        allowed_controller = asyncio.run(gate.hook(
            {"tool_name": "mcp__factorioctl__build_automation_science"},
            "tool-2",
            {},
        ))
        allowed_bootstrap = asyncio.run(fresh_gate.hook(
            {"tool_name": "mcp__factorioctl__insert_items"},
            "tool-3",
            {},
        ))

        self.assertEqual(blocked["decision"], "block")
        self.assertIn("stale manual automation tool", blocked["reason"])
        self.assertIn("build_automation_science", blocked["reason"])
        self.assertFalse(ToolResultOutcome.from_text(blocked["reason"]).indicates_failure)
        self.assertEqual(allowed_controller, {})
        self.assertEqual(allowed_bootstrap, {})

    def test_manual_automation_drift_gate_uses_live_state_to_block_fuel_refills(self):
        import pipe
        from models import LedgerState

        manual_fuel = LedgerState(
            objective="keep the furnace running",
            plan_steps=[
                "walk_to (42, -21)",
                "insert_items coal count=5 into furnace fuel inventory",
            ],
        )
        automation_capable = LiveState(
            found=True,
            surface="nauvis",
            x=54.1,
            y=-21.0,
            entity_counts={
                "transport-belt": 16,
                "electric-mining-drill": 1,
                "small-electric-pole": 21,
            },
        )
        gate = pipe.ManualAutomationDriftGate(
            pipe.logger.bind(agent="test"),
            agent_name="doug",
            ledger_loader=lambda agent_name: manual_fuel,
            live_state_loader=lambda agent_name: automation_capable,
        )

        blocked = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__insert_items",
                "tool_input": {
                    "unit_number": 15,
                    "item": "coal",
                    "count": 5,
                    "inventory_type": "fuel",
                },
            },
            "tool-1",
            {},
        ))

        self.assertEqual(blocked["decision"], "block")
        self.assertIn("stale manual automation tool", blocked["reason"])
        self.assertIn("build_fuel_supply", blocked["reason"])

    def test_manual_automation_drift_gate_allows_bootstrap_transfers_with_durable_plan(self):
        import pipe
        from models import LedgerState

        durable_fuel_plan = LedgerState(
            objective="build durable fuel supply",
            plan_steps=[
                "diagnose_fuel_sustainability near boiler",
                "build_fuel_supply with returned args",
            ],
        )
        automation_capable = LiveState(
            found=True,
            surface="nauvis",
            x=54.1,
            y=-21.0,
            entity_counts={
                "transport-belt": 16,
                "electric-mining-drill": 1,
                "small-electric-pole": 21,
            },
        )
        gate = pipe.ManualAutomationDriftGate(
            pipe.logger.bind(agent="test"),
            agent_name="doug",
            ledger_loader=lambda agent_name: durable_fuel_plan,
            live_state_loader=lambda agent_name: automation_capable,
        )

        blocked = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__insert_items",
                "tool_input": {
                    "unit_number": 49,
                    "item": "coal",
                    "count": 5,
                    "inventory_type": "fuel",
                },
            },
            "tool-1",
            {},
        ))
        blocked_ore_bootstrap = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__insert_items",
                "tool_input": {
                    "unit_number": 15,
                    "item": "iron-ore",
                    "count": 5,
                    "inventory_type": "furnace_source",
                },
            },
            "tool-2",
            {},
        ))

        self.assertEqual(blocked, {})
        self.assertEqual(blocked_ore_bootstrap, {})

    def test_manual_automation_drift_gate_allows_bootstrap_for_incremental_fuel_route(self):
        import pipe
        from models import LedgerState

        incremental_route_plan = LedgerState(
            objective="build incremental coal belt route to boiler",
            plan_steps=[
                "insert_items unit=49 item=coal count=4 inventory_type=fuel",
                "insert_items unit=73 item=coal count=2 inventory_type=fuel",
                "craft transport-belt count=12",
                "route_belt from_x=78 from_y=-17 to_x=60 to_y=-17 extend_existing=true",
            ],
        )
        automation_capable = LiveState(
            found=True,
            surface="nauvis",
            x=-39.3,
            y=24.7,
            entity_counts={
                "transport-belt": 40,
                "electric-mining-drill": 1,
                "small-electric-pole": 21,
                "boiler": 1,
            },
        )
        gate = pipe.ManualAutomationDriftGate(
            pipe.logger.bind(agent="test"),
            agent_name="doug",
            ledger_loader=lambda agent_name: incremental_route_plan,
            live_state_loader=lambda agent_name: automation_capable,
        )

        fuel_bootstrap = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__insert_items",
                "tool_input": {
                    "unit_number": 49,
                    "item": "coal",
                    "count": 4,
                    "inventory_type": "fuel",
                },
            },
            "tool-1",
            {},
        ))
        ore_bootstrap = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__hand_feed_furnace",
                "tool_input": {
                    "furnace_unit_number": 73,
                    "source_item": "iron-ore",
                    "source_count": 20,
                    "fuel_item": "coal",
                    "fuel_count": 2,
                },
            },
            "tool-2",
            {},
        ))

        self.assertEqual(fuel_bootstrap, {})
        self.assertEqual(ore_bootstrap, {})

    def test_manual_automation_drift_gate_blocks_manual_science_after_assembler_exists(self):
        import pipe
        from models import LedgerState

        durable_science_plan = LedgerState(
            objective="build automated science",
            plan_steps=[
                "plan_automation_science for assembler and lab",
                "build_automation_science with ready_to_call args",
            ],
        )
        automation_capable = LiveState(
            found=True,
            surface="nauvis",
            x=54.1,
            y=-21.0,
            entity_counts={
                "assembling-machine-1": 1,
                "transport-belt": 16,
                "lab": 1,
            },
        )
        gate = pipe.ManualAutomationDriftGate(
            pipe.logger.bind(agent="test"),
            agent_name="doug",
            ledger_loader=lambda agent_name: durable_science_plan,
            live_state_loader=lambda agent_name: automation_capable,
        )

        blocked_feed = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__feed_lab_from_inventory",
                "tool_input": {
                    "lab_unit_number": 69,
                    "science_pack": "automation-science-pack",
                    "count": 12,
                    "dry_run": False,
                },
            },
            "tool-1",
            {},
        ))
        blocked_craft = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__craft",
                "tool_input": {
                    "recipe": "automation-science-pack",
                    "count": 12,
                },
            },
            "tool-2",
            {},
        ))
        blocked_component = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__craft",
                "tool_input": {
                    "recipe": "iron-gear-wheel",
                    "count": 12,
                },
            },
            "tool-3",
            {},
        ))
        allowed_dry_run = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__feed_lab_from_inventory",
                "tool_input": {
                    "lab_unit_number": 69,
                    "science_pack": "automation-science-pack",
                    "count": 12,
                    "dry_run": True,
                },
            },
            "tool-4",
            {},
        ))
        allowed_belt_craft = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__craft",
                "tool_input": {
                    "recipe": "transport-belt",
                    "count": 12,
                },
            },
            "tool-5",
            {},
        ))

        self.assertEqual(blocked_feed["decision"], "block")
        self.assertEqual(blocked_craft["decision"], "block")
        self.assertEqual(blocked_component["decision"], "block")
        self.assertIn("build_automation_science", blocked_feed["reason"])
        self.assertEqual(allowed_dry_run, {})
        self.assertEqual(allowed_belt_craft, {})

    def test_manual_automation_drift_gate_allows_component_craft_outside_science_context(self):
        import pipe
        from models import LedgerState

        build_more_power_plan = LedgerState(
            objective="extend power and place another assembler",
            plan_steps=[
                "craft iron-gear-wheel count=3 for assembling-machine-1",
                "place_entity assembling-machine-1 near base",
            ],
        )
        automation_capable = LiveState(
            found=True,
            surface="nauvis",
            x=54.1,
            y=-21.0,
            entity_counts={
                "assembling-machine-1": 1,
                "transport-belt": 16,
            },
        )
        gate = pipe.ManualAutomationDriftGate(
            pipe.logger.bind(agent="test"),
            agent_name="doug",
            ledger_loader=lambda agent_name: build_more_power_plan,
            live_state_loader=lambda agent_name: automation_capable,
        )

        allowed = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__craft",
                "tool_input": {
                    "recipe": "iron-gear-wheel",
                    "count": 3,
                },
            },
            "tool-1",
            {},
        ))

        self.assertEqual(allowed, {})

    def test_manual_automation_drift_gate_allows_setup_craft_but_blocks_transfers_after_belts(self):
        import pipe
        from models import LedgerState

        bootstrap_plan = LedgerState(
            objective="Bootstrap inserter and assembler crafting to enable durable automation pipeline",
            plan_steps=[
                "craft burner-inserter count=4 (bootstrap for furnace output connections)",
                "craft assembling-machine-1 count=1 (bootstrap for recipe assembler cell)",
                "plan_recipe_assembler_cell assembler=339 recipe=iron-gear-wheel",
                "build_recipe_assembler_cell",
                "build_automation_science",
            ],
        )
        automation_capable = LiveState(
            found=True,
            surface="nauvis",
            x=54.1,
            y=-21.0,
            entity_counts={
                "transport-belt": 16,
            },
        )
        gate = pipe.ManualAutomationDriftGate(
            pipe.logger.bind(agent="test"),
            agent_name="doug",
            ledger_loader=lambda agent_name: bootstrap_plan,
            live_state_loader=lambda agent_name: automation_capable,
        )

        allowed_inserter = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__craft",
                "tool_input": {
                    "recipe": "burner-inserter",
                    "count": 4,
                },
            },
            "tool-1",
            {},
        ))
        allowed_gear = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__craft",
                "tool_input": {
                    "recipe": "iron-gear-wheel",
                    "count": 5,
                },
            },
            "tool-2",
            {},
        ))
        blocked_science = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__craft",
                "tool_input": {
                    "recipe": "automation-science-pack",
                    "count": 12,
                },
            },
            "tool-3",
            {},
        ))
        blocked_furnace_feed = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__hand_feed_furnace",
                "tool_input": {
                    "furnace_unit_number": 267,
                    "source_item": "copper-ore",
                    "source_count": 5,
                    "fuel_item": "coal",
                    "fuel_count": 5,
                },
            },
            "tool-4",
            {},
        ))
        blocked_plate_extract = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__extract_items",
                "tool_input": {
                    "unit_number": 267,
                    "item": "copper-plate",
                    "count": 15,
                    "inventory_type": "furnace_result",
                },
            },
            "tool-5",
            {},
        ))
        blocked_boiler_refuel = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__insert_items",
                "tool_input": {
                    "unit_number": 49,
                    "item": "coal",
                    "count": 4,
                    "inventory_type": "fuel",
                },
            },
            "tool-6",
            {},
        ))

        self.assertEqual(allowed_inserter, {})
        self.assertEqual(allowed_gear, {})
        self.assertEqual(blocked_furnace_feed["decision"], "block")
        self.assertEqual(blocked_plate_extract["decision"], "block")
        self.assertEqual(blocked_science["decision"], "block")
        self.assertEqual(blocked_boiler_refuel["decision"], "block")

    def test_manual_automation_drift_gate_blocks_bootstrap_manual_flow_after_assembler(self):
        import pipe
        from models import LedgerState

        bootstrap_plan = LedgerState(
            objective="Bootstrap inserter and assembler crafting to enable durable automation pipeline",
            plan_steps=[
                "craft burner-inserter count=4",
                "craft assembling-machine-1 count=1",
                "plan_recipe_assembler_cell assembler=339 recipe=iron-gear-wheel",
                "build_recipe_assembler_cell",
                "build_automation_science",
            ],
        )
        automation_capable = LiveState(
            found=True,
            surface="nauvis",
            x=54.1,
            y=-21.0,
            entity_counts={
                "assembling-machine-1": 1,
                "transport-belt": 16,
                "lab": 1,
            },
        )
        gate = pipe.ManualAutomationDriftGate(
            pipe.logger.bind(agent="test"),
            agent_name="doug",
            ledger_loader=lambda agent_name: bootstrap_plan,
            live_state_loader=lambda agent_name: automation_capable,
        )

        allowed_craft = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__craft",
                "tool_input": {
                    "recipe": "iron-gear-wheel",
                    "count": 5,
                },
            },
            "tool-1",
            {},
        ))
        blocked_feed = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__insert_items",
                "tool_input": {
                    "unit_number": 15,
                    "item": "iron-ore",
                    "count": 20,
                    "inventory_type": "furnace_source",
                },
            },
            "tool-2",
            {},
        ))
        blocked_extract = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__extract_items",
                "tool_input": {
                    "unit_number": 15,
                    "item": "iron-plate",
                    "count": 10,
                    "inventory_type": "furnace_result",
                },
            },
            "tool-3",
            {},
        ))

        self.assertEqual(allowed_craft, {})
        self.assertEqual(blocked_feed["decision"], "block")
        self.assertEqual(blocked_extract["decision"], "block")
        self.assertIn("build_automation_science", blocked_feed["reason"])

    def test_manual_automation_drift_gate_blocks_manual_material_flow_after_belts_exist(self):
        import pipe
        from models import LedgerState

        durable_flow_plan = LedgerState(
            objective="automate smelting flow",
            plan_steps=[
                "execute_direct_smelter with dry_run=true",
                "execute_direct_smelter with dry_run=false",
            ],
        )
        automation_capable = LiveState(
            found=True,
            surface="nauvis",
            x=54.1,
            y=-21.0,
            entity_counts={
                "transport-belt": 16,
                "inserter": 2,
                "electric-mining-drill": 1,
            },
        )
        gate = pipe.ManualAutomationDriftGate(
            pipe.logger.bind(agent="test"),
            agent_name="doug",
            ledger_loader=lambda agent_name: durable_flow_plan,
            live_state_loader=lambda agent_name: automation_capable,
        )

        blocked_insert = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__insert_items",
                "tool_input": {
                    "unit_number": 15,
                    "item": "iron-ore",
                    "count": 20,
                    "inventory_type": "furnace_source",
                },
            },
            "tool-1",
            {},
        ))
        blocked_extract = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__extract_items",
                "tool_input": {
                    "unit_number": 15,
                    "item": "iron-plate",
                    "count": 20,
                    "inventory_type": "furnace_result",
                },
            },
            "tool-2",
            {},
        ))
        allowed_chest_extract = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__extract_items",
                "tool_input": {
                    "unit_number": 99,
                    "item": "iron-plate",
                    "count": 20,
                    "inventory_type": "chest",
                },
            },
            "tool-3",
            {},
        ))

        self.assertEqual(blocked_insert["decision"], "block")
        self.assertEqual(blocked_extract["decision"], "block")
        self.assertIn("execute_direct_smelter", blocked_insert["reason"])
        self.assertEqual(allowed_chest_extract, {})

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
        self.assertFalse(ToolResultOutcome.from_text(blocked["reason"]).indicates_failure)
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
        self.assertIsInstance(gate.schema_registry, ToolParamSchemaRegistry)

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
        self.assertFalse(ToolResultOutcome.from_text(blocked["reason"]).indicates_failure)

        blocked_fuel = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__build_fuel_supply",
                "tool_input": {
                    "consumer_unit_number": 49,
                    "from_x": 78,
                    "from_y": -20,
                    "pickup_x": 50,
                    "pickup_y": -24,
                    "inserter_x": 50.5,
                    "inserter_y": -23.5,
                    "inserter_direction": "north",
                    "inserter_name": {"bad": True},
                },
            },
            "tool-2",
            {},
        ))

        self.assertEqual(blocked_fuel["decision"], "block")
        self.assertIn(
            "build_fuel_supply: tool_input.inserter_name: expected string",
            blocked_fuel["reason"],
        )

        blocked_inserter_analysis = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__analyze_inserters",
                "tool_input": {"x": 54.5, "y": 9, "radius": 12},
            },
            "tool-3",
            {},
        ))

        self.assertEqual(blocked_inserter_analysis["decision"], "block")
        self.assertIn(
            "analyze_inserters: tool_input.x: expected integer",
            blocked_inserter_analysis["reason"],
        )

        allowed_repair = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__repair_fuel_sustainability",
                "tool_input": {
                    "x": 45.5,
                    "y": -12.5,
                    "radius": 64,
                    "dry_run": True,
                    "allow_underground": False,
                },
            },
            "tool-3",
            {},
        ))
        self.assertEqual(allowed_repair, {})

        allowed_output_plan = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__plan_machine_output",
                "tool_input": {
                    "source_unit_number": 73,
                    "item_name": "iron-plate",
                    "to_x": 60,
                    "to_y": -24,
                    "output_side": "north",
                },
            },
            "tool-4",
            {},
        ))
        self.assertEqual(allowed_output_plan, {})

        allowed_bootstrap = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__bootstrap_smelting_once",
                "tool_input": {
                    "furnace_unit_number": 15,
                    "fuel_item": "coal",
                    "fuel_count": 5,
                    "source_item": "iron-ore",
                    "source_count": 20,
                    "output_item": "iron-plate",
                    "output_count": 5,
                    "craft_recipe": "burner-inserter",
                    "craft_count": 1,
                    "wait_ticks": 1200,
                    "dry_run": True,
                },
            },
            "tool-5",
            {},
        ))
        self.assertEqual(allowed_bootstrap, {})

    def test_factorio_tool_schema_gate_blocks_bad_schema_declarations(self):
        import pipe

        gate = pipe.FactorioToolSchemaGate(
            pipe.logger.bind(agent="test"),
            schema_registry={"place_entity": {"required": {"x": "coordinate"}}},
        )
        self.assertIsInstance(gate.schema_registry, ToolParamSchemaRegistry)
        self.assertIsNotNone(gate.schema_registry_error)
        blocked = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__place_entity",
                "tool_input": {"x": 0},
            },
            "tool-1",
            {},
        ))

        self.assertEqual(blocked["decision"], "block")
        self.assertIn("place_entity: required.x: unknown parameter type", blocked["reason"])
        self.assertFalse(ToolResultOutcome.from_text(blocked["reason"]).indicates_failure)

    def test_factorio_tool_schema_gate_allows_valid_and_unknown_tools(self):
        import pipe

        gate = pipe.FactorioToolSchemaGate(pipe.logger.bind(agent="test"))

        valid = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__rotate_entity",
                "tool_input": {
                    "direction": "south",
                    "unit_number": 42,
                },
            },
            "tool-1",
            {},
        ))
        valid_repair = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__repair_steam_power",
                "tool_input": {
                    "x": 0,
                    "y": 0,
                    "radius": 50,
                    "target_x": 10.5,
                    "target_y": -2,
                },
            },
            "tool-repair",
            {},
        ))
        valid_extend = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__extend_power_to",
                "tool_input": {
                    "x": 0,
                    "y": 0,
                    "radius": 20,
                    "target_x": 2,
                    "target_y": 0,
                },
            },
            "tool-extend",
            {},
        ))
        valid_edge_miner = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__build_edge_miner",
                "tool_input": {
                    "resource_type": "iron-ore",
                    "x": 57,
                    "y": -22,
                    "radius": 25,
                    "drill_name": "burner-mining-drill",
                    "limit": 5,
                },
            },
            "tool-edge-miner",
            {},
        ))
        valid_execute_edge_miner = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__execute_edge_miner",
                "tool_input": {
                    "resource_type": "coal",
                    "x": 78,
                    "y": -19,
                    "radius": 25,
                    "drill_name": "burner-mining-drill",
                    "limit": 5,
                    "dry_run": True,
                    "fuel_item": "coal",
                    "fuel_count": 5,
                    "verify_radius": 10,
                },
            },
            "tool-execute-edge-miner",
            {},
        ))
        valid_execute_placement = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__execute_entity_placement_near",
                "tool_input": {
                    "entity_name": "assembling-machine-1",
                    "x": 45,
                    "y": -12,
                    "radius": 10,
                    "limit": 5,
                    "dry_run": True,
                },
            },
            "tool-execute-placement",
            {},
        ))
        valid_direct_smelter = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__build_direct_smelter",
                "tool_input": {
                    "output_x": 56,
                    "output_y": -18,
                    "output_direction": "south",
                    "furnace_name": "stone-furnace",
                    "inserter_name": "burner-inserter",
                    "belt_name": "transport-belt",
                    "radius": 6,
                },
            },
            "tool-direct-smelter",
            {},
        ))
        valid_lab_feed = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__feed_lab_from_inventory",
                "tool_input": {
                    "lab_unit_number": 42,
                    "science_pack": "automation-science-pack",
                    "count": 5,
                    "dry_run": True,
                },
            },
            "tool-lab-feed",
            {},
        ))
        automation_science_input = {
            "assembler_unit_number": 80,
            "lab_unit_number": 69,
            "gear_from_x": 10,
            "gear_from_y": 11,
            "gear_pickup_x": 12,
            "gear_pickup_y": 13,
            "gear_inserter_x": 12.5,
            "gear_inserter_y": 13.5,
            "gear_inserter_direction": "north",
            "copper_from_x": 20,
            "copper_from_y": 21,
            "copper_pickup_x": 22,
            "copper_pickup_y": 23,
            "copper_inserter_x": 22.5,
            "copper_inserter_y": 23.5,
            "copper_inserter_direction": "east",
            "science_drop_x": 30,
            "science_drop_y": 31,
            "science_to_x": 32,
            "science_to_y": 33,
            "output_inserter_x": 30.5,
            "output_inserter_y": 31.5,
            "output_inserter_direction": "south",
            "lab_from_x": 32,
            "lab_from_y": 33,
            "lab_pickup_x": 34,
            "lab_pickup_y": 35,
            "lab_inserter_x": 34.5,
            "lab_inserter_y": 35.5,
            "lab_inserter_direction": "west",
            "dry_run": True,
        }
        valid_automation_science = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__build_automation_science",
                "tool_input": automation_science_input,
            },
            "tool-automation-science",
            {},
        ))
        valid_automation_science_plan = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__plan_automation_science",
                "tool_input": {
                    "assembler_unit_number": 80,
                    "lab_unit_number": 69,
                    "gear_from_x": 10,
                    "gear_from_y": 11,
                    "copper_from_x": 20,
                    "copper_from_y": 21,
                    "gear_side": "north",
                    "copper_side": "south",
                    "output_side": "east",
                    "lab_side": "west",
                },
            },
            "tool-automation-science-plan",
            {},
        ))
        recipe_cell_input = {
            "assembler_unit_number": 81,
            "recipe": "iron-gear-wheel",
            "input_item_name": "iron-plate",
            "output_item_name": "iron-gear-wheel",
            "input_from_x": 10,
            "input_from_y": 11,
            "input_pickup_x": 12,
            "input_pickup_y": 13,
            "input_inserter_x": 12.5,
            "input_inserter_y": 13.5,
            "input_inserter_direction": "west",
            "output_drop_x": 20,
            "output_drop_y": 21,
            "output_to_x": 22,
            "output_to_y": 23,
            "output_inserter_x": 20.5,
            "output_inserter_y": 21.5,
            "output_inserter_direction": "east",
            "dry_run": True,
        }
        valid_recipe_cell = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__build_recipe_assembler_cell",
                "tool_input": recipe_cell_input,
            },
            "tool-recipe-cell",
            {},
        ))
        valid_recipe_cell_plan = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__plan_recipe_assembler_cell",
                "tool_input": {
                    "assembler_unit_number": 81,
                    "recipe": "iron-gear-wheel",
                    "input_item_name": "iron-plate",
                    "output_item_name": "iron-gear-wheel",
                    "input_from_x": 10,
                    "input_from_y": 11,
                    "output_to_x": 22,
                    "output_to_y": 23,
                },
            },
            "tool-recipe-cell-plan",
            {},
        ))
        bad_automation_science = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__build_automation_science",
                "tool_input": {
                    **automation_science_input,
                    "gear_inserter_x": {"bad": True},
                },
            },
            "tool-automation-science-bad",
            {},
        ))
        bad_automation_science_plan = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__plan_automation_science",
                "tool_input": {
                    "assembler_unit_number": 80,
                    "lab_unit_number": 69,
                    "gear_from_x": {"bad": True},
                    "gear_from_y": 11,
                    "copper_from_x": 20,
                    "copper_from_y": 21,
                },
            },
            "tool-automation-science-plan-bad",
            {},
        ))
        bad_recipe_cell = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__build_recipe_assembler_cell",
                "tool_input": {
                    **recipe_cell_input,
                    "output_inserter_x": {"bad": True},
                },
            },
            "tool-recipe-cell-bad",
            {},
        ))
        bad_recipe_cell_plan = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__plan_recipe_assembler_cell",
                "tool_input": {
                    "assembler_unit_number": 81,
                    "recipe": "iron-gear-wheel",
                    "input_item_name": "iron-plate",
                    "output_item_name": "iron-gear-wheel",
                    "input_from_x": ["bad"],
                    "input_from_y": 11,
                    "output_to_x": 22,
                    "output_to_y": 23,
                },
            },
            "tool-recipe-cell-plan-bad",
            {},
        ))
        bad_execute_edge_miner = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__execute_edge_miner",
                "tool_input": {
                    "resource_type": "coal",
                    "x": {"bad": True},
                    "y": -19,
                },
            },
            "tool-execute-edge-miner-bad",
            {},
        ))
        bad_execute_placement = asyncio.run(gate.hook(
            {
                "tool_name": "mcp__factorioctl__execute_entity_placement_near",
                "tool_input": {
                    "entity_name": "lab",
                    "x": 45,
                    "y": ["bad"],
                },
            },
            "tool-execute-placement-bad",
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
        self.assertEqual(valid_repair, {})
        self.assertEqual(valid_extend, {})
        self.assertEqual(valid_edge_miner, {})
        self.assertEqual(valid_execute_edge_miner, {})
        self.assertEqual(valid_execute_placement, {})
        self.assertEqual(valid_direct_smelter, {})
        self.assertEqual(valid_lab_feed, {})
        self.assertEqual(valid_automation_science, {})
        self.assertEqual(valid_automation_science_plan, {})
        self.assertEqual(valid_recipe_cell, {})
        self.assertEqual(valid_recipe_cell_plan, {})
        self.assertEqual(bad_automation_science["decision"], "block")
        self.assertIn(
            "build_automation_science: tool_input.gear_inserter_x: expected number",
            bad_automation_science["reason"],
        )
        self.assertEqual(bad_automation_science_plan["decision"], "block")
        self.assertIn(
            "plan_automation_science: tool_input.gear_from_x: expected integer",
            bad_automation_science_plan["reason"],
        )
        self.assertEqual(bad_recipe_cell["decision"], "block")
        self.assertIn(
            "build_recipe_assembler_cell: tool_input.output_inserter_x: expected number",
            bad_recipe_cell["reason"],
        )
        self.assertEqual(bad_recipe_cell_plan["decision"], "block")
        self.assertIn(
            "plan_recipe_assembler_cell: tool_input.input_from_x: expected integer",
            bad_recipe_cell_plan["reason"],
        )
        self.assertEqual(bad_execute_edge_miner["decision"], "block")
        self.assertIn(
            "execute_edge_miner: tool_input.x: expected number",
            bad_execute_edge_miner["reason"],
        )
        self.assertEqual(bad_execute_placement["decision"], "block")
        self.assertIn(
            "execute_entity_placement_near: tool_input.y: expected number",
            bad_execute_placement["reason"],
        )
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

        with mock.patch.dict("os.environ", {
            "BRIDGE_CONTEXT_WINDOW_BACKOFF_S": "12.5",
            "BRIDGE_MUTATING_TOOL_BATCH_WINDOW_S": "0.25",
        }):
            self.assertEqual(pipe._context_window_backoff_s(), 12.5)
            gate = pipe.MutatingToolBatchGate(log=mock.Mock())
            self.assertEqual(gate.window_s, 0.25)

        with mock.patch.dict("os.environ", {
            "BRIDGE_CONTEXT_WINDOW_BACKOFF_S": "oops",
            "BRIDGE_MUTATING_TOOL_BATCH_WINDOW_S": "oops",
        }):
            self.assertEqual(pipe._context_window_backoff_s(), 900.0)
            gate = pipe.MutatingToolBatchGate(log=mock.Mock())
            self.assertEqual(gate.window_s, 1.0)

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
        self.assertEqual(pipe._resolve_sdk_skills(["factorio-control", {"bad": True}]), [
            "factorio-control",
        ])
        self.assertEqual(pipe._claude_tools_for_sdk_skills(["factorio-control"]), ["Skill"])
        self.assertEqual(pipe._claude_tools_for_sdk_skills("all"), ["Skill"])
        self.assertEqual(pipe._claude_tools_for_sdk_skills([]), [])
        self.assertEqual(
            pipe._setting_sources_for_sdk_skills(["factorio-control"]),
            ["project", "local"],
        )
        self.assertEqual(pipe._setting_sources_for_sdk_skills("all"), ["project", "local"])
        self.assertEqual(pipe._setting_sources_for_sdk_skills([]), ["local"])

    def test_project_factorio_control_skill_enforces_durable_automation(self):
        skill_path = Path(__file__).resolve().parents[2] / ".claude" / "skills" / "factorio-control" / "SKILL.md"

        text = skill_path.read_text()

        self.assertIn("Build durable automation instead of repeating manual cycles", text)
        self.assertIn("build_automation_science", text)
        self.assertIn("build_assembler_feed", text)
        self.assertIn("build_assembler_output", text)
        self.assertIn("build_lab_feed", text)
        self.assertIn("A plan that only hand-crafts and hand-delivers more science packs is stale", text)

    def test_build_mcp_servers_uses_typed_factorio_stdio_config_shape(self):
        import pipe

        self.assertEqual(
            pipe.build_mcp_servers(
                "/tmp/mcp",
                "localhost",
                27015,
                "factorio",
                agent_id="doug",
            ),
            {
                "factorioctl": {
                    "type": "stdio",
                    "command": "/tmp/mcp",
                    "args": [],
                    "env": {
                        "FACTORIO_RCON_HOST": "localhost",
                        "FACTORIO_RCON_PORT": "27015",
                        "FACTORIO_RCON_PASSWORD": "factorio",
                        "FACTORIO_AGENT_ID": "doug",
                    },
                }
            },
        )

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

    def test_handle_message_installs_read_only_gate_when_requested(self):
        import pipe

        captured = {}

        def scripted_query(*, prompt, options):
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
                read_only_tools=True,
            )

        matcher = captured["options"].hooks["PreToolUse"][0]
        owners = [getattr(hook, "__self__", None) for hook in matcher.hooks]
        gates = [
            owner for owner in owners
            if isinstance(owner, pipe.PlannerReadOnlyToolGate)
        ]
        self.assertEqual(len(gates), 1)
        self.assertTrue(gates[0].enabled)

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
            SdkSystemMessage.from_sdk_message(SystemMessage(
                subtype="init",
                data={
                    "cwd": str(pipe._PROJECT_ROOT),
                    "tools": ["Skill", "mcp__factorioctl__walk_to"],
                    "skills": ["factorio-control"],
                },
            )),
            options,
            log,
        )

        self.assertTrue(logged)
        self.assertIn("sdk init", log.messages[0][0])
        self.assertFalse(SdkSystemMessage.from_sdk_message(SystemMessage(
            subtype="thinking_tokens",
            data={"estimated_tokens": 1},
        )).should_log)
        self.assertTrue(SdkSystemMessage.from_sdk_message(SystemMessage(
            subtype="error",
            data={"message": "visible diagnostic"},
        )).should_log)
        self.assertTrue(pipe._is_skill_tool(ToolUseBlock(
            id="s1",
            name="Skill",
            input={"skill": "factorio-control"},
        )))

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
        thread.enqueue({
            "message": "hi",
            "player_index": 1,
            "player_name": "TestPlayer",
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
            transcript = asyncio.run(pipe._run_agent(
                "go", object(), "doug", None, "doug",
                StubRCON(), 0, pipe.logger.bind(agent="doug"),
            ))

        self.assertEqual(journal.load_events("doug"), [])
        self.assertIsNotNone(pipe._get_usage_limit_cooldown("doug"))
        self.assertTrue(transcript.usage_limit_seen)
        self.assertFalse(transcript.context_window_limit)
        self.assertEqual(transcript.session_id, "s")
        self.assertEqual(transcript.text_parts, [limit_text])

    def test_handle_message_exception_uses_typed_invocation_signal(self):
        import pipe

        provider_now = datetime.now(timezone.utc) + timedelta(hours=8)
        provider_reset = provider_now + timedelta(hours=1)
        error_text = (
            "Claude Code returned an error result: API Error: Request rejected "
            "(429) · [1308][Usage limit reached for 5 hour. Your limit will "
            f"reset at {provider_reset:%Y-%m-%d %H:%M:%S}]"
            f"[{provider_now:%Y%m%d%H%M%S}abcdef]"
        )

        def raising_query(*, prompt, options):
            raise RuntimeError(error_text)

        class StubRCON:
            def execute(self, _cmd):
                return ""

        pipe._USAGE_LIMIT_COOLDOWNS.clear()
        self.addCleanup(pipe._USAGE_LIMIT_COOLDOWNS.clear)

        with mock.patch("pipe.query", raising_query):
            new_session = pipe.handle_message(
                "go", {}, "system", "old-session", StubRCON(), 0, None,
                agent_name="doug", sdk_skills=["factorio-control"],
            )

        self.assertEqual(new_session, "old-session")
        self.assertIsNotNone(pipe._get_usage_limit_cooldown("doug"))
        self.assertEqual(journal.load_events("doug"), [])

    def test_handle_message_clears_session_on_context_window_limit(self):
        import pipe
        from claude_agent_sdk import ResultMessage

        pipe._CONTEXT_WINDOW_COOLDOWNS.clear()
        self.addCleanup(pipe._CONTEXT_WINDOW_COOLDOWNS.clear)
        session_patch = mock.patch(
            "pipe._session_file",
            side_effect=lambda agent_name: self.base / f".session-{agent_name}.json",
        )
        sessions_patch = mock.patch.object(pipe, "SESSIONS_FILE", self.base / ".sessions.json")
        session_patch.start()
        sessions_patch.start()
        self.addCleanup(session_patch.stop)
        self.addCleanup(sessions_patch.stop)

        pipe.save_session("doug", "old-session")
        pipe.SESSIONS_FILE.write_text(json.dumps({
            "doug": "old-legacy-session",
            "other": "keep-me",
        }) + "\n")

        def scripted_query(*, prompt, options):
            async def gen():
                yield ResultMessage(
                    subtype="error",
                    duration_ms=1,
                    duration_api_ms=1,
                    is_error=True,
                    num_turns=1,
                    session_id="old-session",
                    result="API Error: The model has reached its context window limit.",
                    total_cost_usd=0.0,
                )
            return gen()

        class StubRCON:
            def execute(self, _cmd):
                return ""

        with mock.patch("pipe.query", scripted_query):
            new_session = pipe.handle_message(
                "go", {}, "system", "old-session", StubRCON(), 0, None,
                agent_name="doug", sdk_skills=["factorio-control"],
            )

        self.assertEqual(new_session, pipe.SESSION_RESET)
        self.assertIsNone(pipe.load_session("doug"))
        self.assertIsNone(pipe._get_context_window_cooldown("doug"))
        self.assertEqual(json.loads(pipe.SESSIONS_FILE.read_text()), {"other": "keep-me"})
        self.assertEqual(journal.load_events("doug"), [])

    def test_handle_message_clears_session_when_context_result_stream_raises(self):
        import pipe
        from claude_agent_sdk import ResultMessage

        pipe._CONTEXT_WINDOW_COOLDOWNS.clear()
        self.addCleanup(pipe._CONTEXT_WINDOW_COOLDOWNS.clear)
        session_patch = mock.patch(
            "pipe._session_file",
            side_effect=lambda agent_name: self.base / f".session-{agent_name}.json",
        )
        sessions_patch = mock.patch.object(pipe, "SESSIONS_FILE", self.base / ".sessions.json")
        session_patch.start()
        sessions_patch.start()
        self.addCleanup(session_patch.stop)
        self.addCleanup(sessions_patch.stop)

        pipe.save_session("doug", "old-session")

        def scripted_query(*, prompt, options):
            async def gen():
                yield ResultMessage(
                    subtype="error",
                    duration_ms=1,
                    duration_api_ms=1,
                    is_error=True,
                    num_turns=1,
                    session_id="old-session",
                    result="API Error: The model has reached its context window limit.",
                    total_cost_usd=0.0,
                )
                raise RuntimeError("Claude Code returned an error result: success")
            return gen()

        class StubRCON:
            def execute(self, _cmd):
                return ""

        with mock.patch("pipe.query", scripted_query):
            new_session = pipe.handle_message(
                "go", {}, "system", "old-session", StubRCON(), 0, None,
                agent_name="doug", sdk_skills=["factorio-control"],
            )

        self.assertEqual(new_session, pipe.SESSION_RESET)
        self.assertIsNone(pipe.load_session("doug"))
        self.assertIsNone(pipe._get_context_window_cooldown("doug"))
        self.assertEqual(journal.load_events("doug"), [])

    def test_session_persistence_uses_typed_current_and_legacy_shapes(self):
        import pipe

        session_patch = mock.patch(
            "pipe._session_file",
            side_effect=lambda agent_name: self.base / f".session-{agent_name}.json",
        )
        sessions_patch = mock.patch.object(pipe, "SESSIONS_FILE", self.base / ".sessions.json")
        session_patch.start()
        sessions_patch.start()
        self.addCleanup(session_patch.stop)
        self.addCleanup(sessions_patch.stop)

        pipe.save_session("doug", "new-session")
        self.assertEqual(pipe.load_session("doug"), "new-session")
        self.assertEqual(
            json.loads((self.base / ".session-doug.json").read_text()),
            {"session_id": "new-session"},
        )

        (self.base / ".session-doug.json").write_text('{"session_id": ""}\n')
        pipe.SESSIONS_FILE.write_text(json.dumps({
            "doug": "legacy-session",
            "other": "keep-me",
            "empty": "",
            "bad": 3,
        }) + "\n")
        self.assertIsNone(pipe.load_session("doug"))
        (self.base / ".session-doug.json").unlink()
        self.assertEqual(pipe.load_session("doug"), "legacy-session")

        pipe.clear_session("doug")
        self.assertFalse((self.base / ".session-doug.json").exists())
        self.assertEqual(json.loads(pipe.SESSIONS_FILE.read_text()), {"other": "keep-me"})

    def test_handle_message_backs_off_context_window_after_session_reset(self):
        import pipe
        from claude_agent_sdk import ResultMessage

        pipe._CONTEXT_WINDOW_COOLDOWNS.clear()
        self.addCleanup(pipe._CONTEXT_WINDOW_COOLDOWNS.clear)
        session_patch = mock.patch(
            "pipe._session_file",
            side_effect=lambda agent_name: self.base / f".session-{agent_name}.json",
        )
        sessions_patch = mock.patch.object(pipe, "SESSIONS_FILE", self.base / ".sessions.json")
        session_patch.start()
        sessions_patch.start()
        self.addCleanup(session_patch.stop)
        self.addCleanup(sessions_patch.stop)

        def scripted_query(*, prompt, options):
            async def gen():
                yield ResultMessage(
                    subtype="error",
                    duration_ms=1,
                    duration_api_ms=1,
                    is_error=True,
                    num_turns=1,
                    session_id=None,
                    result="API Error: The model has reached its context window limit.",
                    total_cost_usd=0.0,
                )
            return gen()

        class StubRCON:
            def execute(self, _cmd):
                return ""

        with mock.patch("pipe.query", scripted_query):
            new_session = pipe.handle_message(
                "go", {}, "system", None, StubRCON(), 0, None,
                agent_name="doug", sdk_skills=["factorio-control"],
            )

        self.assertEqual(new_session, pipe.SESSION_RESET)
        self.assertIsNotNone(pipe._get_context_window_cooldown("doug"))
        self.assertEqual(journal.load_events("doug"), [])

    def test_context_window_cooldown_blocks_human_message_without_model_call(self):
        import queue as std_queue

        import pipe

        class StubRCON:
            def __init__(self):
                self.commands = []

            def execute(self, cmd):
                self.commands.append(cmd)
                return ""

        reset = datetime.now(timezone.utc) + timedelta(minutes=15)
        pipe._CONTEXT_WINDOW_COOLDOWNS.clear()
        pipe._CONTEXT_WINDOW_COOLDOWNS["doug"] = reset
        self.addCleanup(pipe._CONTEXT_WINDOW_COOLDOWNS.clear)

        thread = pipe.AgentThread.__new__(pipe.AgentThread)
        thread.agent_name = "doug"
        thread.telemetry_name = "doug"
        thread.telemetry = None
        thread.rcon = StubRCON()
        thread.log = pipe.logger.bind(agent="doug")
        thread.heartbeat_interval = 0
        thread.inbox = std_queue.Queue()
        thread.enqueue({
            "message": "hi",
            "player_index": 1,
            "player_name": "TestPlayer",
        })

        with mock.patch("pipe.handle_message", side_effect=AssertionError("called model")):
            thread._run_once()

        joined = "\n".join(thread.rcon.commands)
        self.assertIn("SDK context-window limit repeated", joined)
        self.assertIn("Ready", joined)

    def test_watchdog_aborts_repeated_same_game_rejection_without_clearing_session(self):
        import pipe
        from claude_agent_sdk import AssistantMessage, ToolResultBlock, ToolUseBlock, UserMessage

        messages = []
        for i in range(3):
            tool_id = f"tool-{i}"
            messages.append(AssistantMessage(
                content=[ToolUseBlock(
                    id=tool_id,
                    name="mcp__factorioctl__place_entity",
                    input={"entity_name": "transport-belt", "x": 0, "y": 0},
                )],
                model="test",
            ))
            messages.append(UserMessage(content=[ToolResultBlock(
                tool_use_id=tool_id,
                content="Error: Cannot place entity here",
                is_error=False,
            )]))

        def scripted_query(*, prompt, options):
            async def gen():
                for msg in messages:
                    yield msg
            return gen()

        class StubRCON:
            def execute(self, _cmd):
                return ""

        with mock.patch("pipe.query", scripted_query):
            new_session = pipe.handle_message(
                "go", {}, "system", "old-session", StubRCON(), 0, None,
                agent_name="doug", sdk_skills=["factorio-control"],
            )

        self.assertEqual(new_session, "old-session")
        events = journal.load_events("doug")
        self.assertTrue(any(
            "watchdog_abort: repeated same game rejection" in event["text"]
            for event in events
        ))
        self.assertFalse(any("context window" in event["text"].lower() for event in events))

    def test_watchdog_no_progress_timeout_reason_reaches_next_autonomy_prompt(self):
        import ledger
        import pipe
        from claude_agent_sdk import AssistantMessage, TextBlock

        class ImmediateNoProgressWatchdog:
            def __init__(self):
                pass

            def observe_text(self):
                raise pipe.AgentTickWatchdogAbort(
                    "no successful mutating progress for 12s during active tick"
                )

            def record_tool_use(self, *_args):
                pass

            def observe_tool_result(self, *_args):
                pass

        def scripted_query(*, prompt, options):
            async def gen():
                yield AssistantMessage(
                    content=[TextBlock("still thinking")],
                    model="test",
                )
            return gen()

        class StubRCON:
            def execute(self, _cmd):
                return ""

        with (
            mock.patch("pipe.AgentTickWatchdog", ImmediateNoProgressWatchdog),
            mock.patch("pipe.query", scripted_query),
        ):
            new_session = pipe.handle_message(
                "go", {}, "system", "kept-session", StubRCON(), 0, None,
                agent_name="doug", sdk_skills=["factorio-control"],
            )

        self.assertEqual(new_session, "kept-session")
        ledger.save_ledger("doug", {
            "objective": "Fix stuck smelter",
            "plan_steps": ["place_entity transport-belt"],
            "progress_notes": [],
            "updated_at": "now",
        })

        thread = pipe.AgentThread.__new__(pipe.AgentThread)
        thread.agent_name = "doug"
        thread.rcon = StubRCON()
        thread._exec_ticks_since_plan = 0
        thread._planner_interval = 5
        thread._planner_model = None
        thread._reflect_interval = 99

        prompt = thread._autonomy_tick()["message"]

        self.assertIn("watchdog_abort", prompt)
        self.assertIn("no successful mutating progress", prompt)

    def test_tick_watchdog_no_progress_timeout_is_tunable(self):
        import pipe

        now = [0.0]

        def clock():
            return now[0]

        watchdog = pipe.AgentTickWatchdog(no_progress_timeout_s=10, clock=clock)
        now[0] = 9.9
        watchdog.observe_text()
        now[0] = 10.0

        with self.assertRaises(pipe.AgentTickWatchdogAbort) as raised:
            watchdog.observe_text()

        self.assertIn("no successful mutating progress", str(raised.exception))

    def test_tick_watchdog_does_not_count_payload_error_as_progress(self):
        import pipe

        now = [0.0]

        def clock():
            return now[0]

        watchdog = pipe.AgentTickWatchdog(no_progress_timeout_s=10, clock=clock)
        watchdog.record_tool_use("craft-1", "mcp__factorioctl__craft")
        watchdog.observe_tool_result(
            "craft-1",
            ToolResultClassification.OK,
            '{"success":true,"queued":1,"error":"Crafting did not start"}',
        )
        now[0] = 10.0

        with self.assertRaises(pipe.AgentTickWatchdogAbort):
            watchdog.observe_text()

    def test_tick_watchdog_coerces_legacy_string_classifications(self):
        import pipe

        watchdog = pipe.AgentTickWatchdog(
            same_failure_limit=2,
            no_progress_timeout_s=0,
        )
        watchdog.record_tool_use("belt-1", "mcp__factorioctl__place_entity")
        watchdog.observe_tool_result(
            "belt-1",
            "game_rejected",
            "Error: Cannot place entity here",
        )

        with self.assertRaises(pipe.AgentTickWatchdogAbort) as raised:
            watchdog.observe_tool_result(
                "belt-1",
                "game_rejected",
                "Error: Cannot place entity here",
            )

        self.assertIn("repeated same game rejection", str(raised.exception))

    def test_agent_thread_drops_in_memory_session_on_context_reset(self):
        import queue as std_queue

        import pipe

        class StubRCON:
            def execute(self, _cmd):
                return ""

        thread = pipe.AgentThread.__new__(pipe.AgentThread)
        thread.agent_name = "doug"
        thread.telemetry_name = "doug"
        thread.telemetry = None
        thread.rcon = StubRCON()
        thread.log = pipe.logger.bind(agent="doug")
        thread.heartbeat_interval = 0
        thread.inbox = std_queue.Queue()
        thread.enqueue({
            "message": "go",
            "player_index": 0,
            "player_name": "autonomy",
        })
        thread.mcp_config = {"factorioctl": {}}
        thread.system_prompt = "system"
        thread.session_id = "old-session"
        thread.model = "haiku"
        thread.max_turns = 200
        thread.sdk_skills = ["factorio-control"]

        with (
            mock.patch(
                "pipe.handle_message_model",
                return_value=pipe.AgentMessageResult.reset(),
            ),
            mock.patch("pipe.save_session") as save_session,
        ):
            thread._run_once()

        self.assertIsNone(thread.session_id)
        save_session.assert_not_called()

    def test_agent_thread_summarizes_autonomy_prompt_in_operator_log(self):
        import queue as std_queue

        import pipe
        from models import AutonomyTickMessage

        class StubRCON:
            def execute(self, _cmd):
                return ""

        class CapturingLog:
            def __init__(self):
                self.info_messages = []
                self.debug_messages = []

            def info(self, template, *args):
                self.info_messages.append(template.format(*args))

            def debug(self, template, *args):
                self.debug_messages.append(template.format(*args))

        prompt = (
            "(execution tick) Do the next unfinished step in your plan now: "
            "call the tool, do not describe it.\n<ledger>\nprogress: x\n</ledger>"
        )
        thread = pipe.AgentThread.__new__(pipe.AgentThread)
        thread.agent_name = "doug"
        thread.telemetry_name = "doug"
        thread.telemetry = object()
        thread.rcon = StubRCON()
        thread.log = CapturingLog()
        thread.heartbeat_interval = 0
        thread.inbox = std_queue.Queue()
        thread.inbox.put(AutonomyTickMessage.create(prompt))
        thread.mcp_config = {"factorioctl": {}}
        thread.system_prompt = "system"
        thread.session_id = None
        thread.model = "haiku"
        thread.max_turns = 200
        thread.sdk_skills = ["factorio-control"]

        with (
            mock.patch(
                "pipe.handle_message_model",
                return_value=pipe.AgentMessageResult.keep_session("new-session"),
            ) as handle_message,
            mock.patch("pipe.save_session"),
            mock.patch("pipe.emit_chat") as emit_chat,
        ):
            thread._run_once()

        args, kwargs = handle_message.call_args
        self.assertEqual(args[0], prompt)
        info_text = "\n".join(thread.log.info_messages)
        debug_text = "\n".join(thread.log.debug_messages)
        self.assertIn("autonomy -> doug: mode=execute", info_text)
        self.assertIn("prompt_chars=", info_text)
        self.assertNotIn("Do the next unfinished step", info_text)
        self.assertIn("Do the next unfinished step", debug_text)
        emit_chat.assert_not_called()

    def test_agent_thread_caps_autonomy_execute_turn_budget(self):
        import queue as std_queue

        import pipe
        from models import AutonomyTickMessage

        class StubRCON:
            def execute(self, _cmd):
                return ""

        thread = pipe.AgentThread.__new__(pipe.AgentThread)
        thread.agent_name = "doug"
        thread.telemetry_name = "doug"
        thread.telemetry = None
        thread.rcon = StubRCON()
        thread.log = pipe.logger.bind(agent="doug")
        thread.heartbeat_interval = 0
        thread.inbox = std_queue.Queue()
        thread.inbox.put(AutonomyTickMessage.create("execute next step"))
        thread.mcp_config = {"factorioctl": {}}
        thread.system_prompt = "system"
        thread.session_id = None
        thread.model = "haiku"
        thread.max_turns = 200
        thread.sdk_skills = ["factorio-control"]

        with (
            mock.patch(
                "pipe.handle_message_model",
                return_value=pipe.AgentMessageResult.keep_session("new-session"),
            ) as handle_message,
            mock.patch("pipe.save_session"),
        ):
            thread._run_once()

        self.assertEqual(
            handle_message.call_args.kwargs["max_turns"],
            pipe.DEFAULT_AUTONOMY_EXEC_MAX_TURNS,
        )
        self.assertTrue(handle_message.call_args.kwargs["stop_after_factorio_result"])
        self.assertEqual(
            handle_message.call_args.kwargs["tick_timeout_s"],
            pipe.DEFAULT_AUTONOMY_EXEC_TIMEOUT_S,
        )

    def test_agent_thread_keeps_planner_turn_budget(self):
        import queue as std_queue

        import pipe
        from models import AutonomyTickMessage

        class StubRCON:
            def execute(self, _cmd):
                return ""

        thread = pipe.AgentThread.__new__(pipe.AgentThread)
        thread.agent_name = "doug"
        thread.telemetry_name = "doug"
        thread.telemetry = None
        thread.rcon = StubRCON()
        thread.log = pipe.logger.bind(agent="doug")
        thread.heartbeat_interval = 0
        thread.inbox = std_queue.Queue()
        thread.inbox.put(AutonomyTickMessage.create(
            "plan next steps",
            read_only_tools=True,
            model="sonnet",
        ))
        thread.mcp_config = {"factorioctl": {}}
        thread.system_prompt = "system"
        thread.session_id = None
        thread.model = "haiku"
        thread.max_turns = 200
        thread.sdk_skills = ["factorio-control"]

        with (
            mock.patch(
                "pipe.handle_message_model",
                return_value=pipe.AgentMessageResult.keep_session("new-session"),
            ) as handle_message,
            mock.patch("pipe.save_session"),
        ):
            thread._run_once()

        self.assertEqual(handle_message.call_args.kwargs["max_turns"], 200)
        self.assertFalse(handle_message.call_args.kwargs["stop_after_factorio_result"])
        self.assertIsNone(handle_message.call_args.kwargs["tick_timeout_s"])

    def test_autonomy_execute_stops_after_first_factorio_tool_result(self):
        import ledger
        import pipe
        from claude_agent_sdk import AssistantMessage, ResultMessage, TextBlock, ToolResultBlock, ToolUseBlock, UserMessage

        consumed_after_factorio_result = []
        ledger.save_ledger("doug", {
            "objective": "Route coal",
            "plan_steps": ["route_belt from coal to furnace"],
            "progress_notes": [],
            "status": "executing",
            "next_required_mode": "execute",
        })

        class StubClaudeSDKClient:
            instances = []

            def __init__(self, *, options):
                self.options = options
                self.prompt = None
                self.interrupted = False
                self.disconnected = False
                self.__class__.instances.append(self)

            async def connect(self, prompt):
                self.prompt = prompt

            def receive_messages(self):
                async def gen():
                    yield AssistantMessage(
                        content=[ToolUseBlock(
                            id="skill-1",
                            name="Skill",
                            input={"skill": "factorio-control"},
                        )],
                        model="test",
                    )
                    yield UserMessage(content=[ToolResultBlock(
                        tool_use_id="skill-1",
                        content="Launching skill: factorio-control",
                        is_error=False,
                    )])
                    yield AssistantMessage(
                        content=[ToolUseBlock(
                            id="tool-1",
                            name="mcp__factorioctl__situation_report",
                            input={"radius": 10},
                        )],
                        model="test",
                    )
                    yield UserMessage(content=[ToolResultBlock(
                        tool_use_id="tool-1",
                        content='{"position":{"x":0,"y":0}}',
                        is_error=False,
                    )])
                    consumed_after_factorio_result.append(True)
                    yield AssistantMessage(
                        content=[TextBlock("should not be consumed")],
                        model="test",
                    )
                    yield ResultMessage(
                        subtype="error",
                        duration_ms=1,
                        duration_api_ms=1,
                        is_error=True,
                        num_turns=4,
                        session_id="old-session",
                        result="Reached maximum number of turns (4)",
                        total_cost_usd=0.0,
                    )
                return gen()

            async def interrupt(self):
                self.interrupted = True

            async def disconnect(self):
                self.disconnected = True

        class StubRCON:
            def execute(self, _cmd):
                return ""

        with (
            mock.patch("pipe.ClaudeSDKClient", StubClaudeSDKClient),
            mock.patch("pipe.query", side_effect=AssertionError("query should not be used")),
            mock.patch("pipe.clear_session") as clear_session,
        ):
            result = pipe.handle_message_model(
                "go",
                {},
                "system",
                "old-session",
                StubRCON(),
                0,
                None,
                agent_name="doug",
                sdk_skills=["factorio-control"],
                stop_after_factorio_result=True,
            )

        self.assertTrue(result.reset_session)
        clear_session.assert_called_once_with("doug")
        self.assertEqual(consumed_after_factorio_result, [])
        self.assertEqual(len(StubClaudeSDKClient.instances), 1)
        self.assertTrue(StubClaudeSDKClient.instances[0].interrupted)
        self.assertTrue(StubClaudeSDKClient.instances[0].disconnected)
        events = journal.load_events("doug")
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0]["kind"], "progress")
        self.assertIn("autonomy_step_complete: situation_report ok", events[0]["text"])
        updated = ledger.load_ledger("doug")
        self.assertEqual(updated["next_required_mode"], "plan")
        self.assertEqual(updated["status"], "ready")
        self.assertIn("situation_report ok", updated["progress_notes"][-1])

    def test_agent_thread_coerces_inbound_message_before_model_call(self):
        import queue as std_queue

        import pipe

        class StubRCON:
            def execute(self, _cmd):
                return ""

        thread = pipe.AgentThread.__new__(pipe.AgentThread)
        thread.agent_name = "doug"
        thread.telemetry_name = "doug"
        thread.telemetry = None
        thread.rcon = StubRCON()
        thread.log = pipe.logger.bind(agent="doug")
        thread.heartbeat_interval = 0
        thread.inbox = std_queue.Queue()
        thread.enqueue({
            "message": 99,
            "player_index": "0",
            "player_name": "",
            "response_to": "all",
            "model": "planner",
            "read_only_tools": "true",
        })
        thread.mcp_config = {"factorioctl": {}}
        thread.system_prompt = "system"
        thread.session_id = "old-session"
        thread.model = "haiku"
        thread.max_turns = 200
        thread.sdk_skills = ["factorio-control"]

        with (
            mock.patch(
                "pipe.handle_message_model",
                return_value=pipe.AgentMessageResult.keep_session("new-session"),
            ) as handle_message,
            mock.patch("pipe.save_session") as save_session,
        ):
            thread._run_once()

        args, kwargs = handle_message.call_args
        self.assertEqual(args[0], "99")
        self.assertEqual(args[5], 0)
        self.assertEqual(kwargs["response_to"], "all")
        self.assertEqual(kwargs["model"], "planner")
        self.assertTrue(kwargs["read_only_tools"])
        self.assertEqual(thread.session_id, "new-session")
        save_session.assert_called_once_with("doug", "new-session")

    def test_agent_thread_accepts_typed_inbound_message_without_recoercion(self):
        import queue as std_queue

        import pipe

        class StubRCON:
            def execute(self, _cmd):
                return ""

        thread = pipe.AgentThread.__new__(pipe.AgentThread)
        thread.agent_name = "doug"
        thread.telemetry_name = "doug"
        thread.telemetry = None
        thread.rcon = StubRCON()
        thread.log = pipe.logger.bind(agent="doug")
        thread.heartbeat_interval = 0
        thread.inbox = std_queue.Queue()
        thread.inbox.put(BridgeInputMessage(
            message="typed hi",
            player_index=7,
            player_name="TestPlayer",
            response_to="all",
            model="planner",
            read_only_tools=True,
        ))
        thread.mcp_config = {"factorioctl": {}}
        thread.system_prompt = "system"
        thread.session_id = None
        thread.model = "haiku"
        thread.max_turns = 200
        thread.sdk_skills = ["factorio-control"]

        with (
            mock.patch(
                "pipe.handle_message_model",
                return_value=pipe.AgentMessageResult.keep_session("typed-session"),
            ) as handle_message,
            mock.patch("pipe.save_session") as save_session,
        ):
            thread._run_once()

        args, kwargs = handle_message.call_args
        self.assertEqual(args[0], "typed hi")
        self.assertEqual(args[5], 7)
        self.assertEqual(kwargs["response_to"], "all")
        self.assertEqual(kwargs["model"], "planner")
        self.assertTrue(kwargs["read_only_tools"])
        self.assertEqual(thread.session_id, "typed-session")
        save_session.assert_called_once_with("doug", "typed-session")

    def test_agent_thread_enqueue_is_the_dict_coercion_boundary(self):
        import queue as std_queue

        import pipe

        thread = pipe.AgentThread.__new__(pipe.AgentThread)
        thread.inbox = std_queue.Queue()

        thread.enqueue({"message": "hi"})
        thread.enqueue({"message": ""})

        queued = thread.inbox.get_nowait()
        self.assertIsInstance(queued, BridgeInputMessage)
        self.assertEqual(queued.message, "hi")
        self.assertTrue(thread.inbox.empty())

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
                    "\n\n--- Player Messages ---\n[TestPlayer]: I put wood in a chest",
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
        self.assertFalse(any("TestPlayer" in t for t in texts))
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
                self.assertFalse(ParsedAgentResponse.is_meaningful_anomaly_text(text))

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

        tick = thread._autonomy_tick()
        prompt = tick["message"]

        self.assertIn("failure: failure 1", prompt)
        self.assertIn("Boiler area near water", prompt)
        self.assertIn("ERROR TIPS", prompt)
        self.assertIn("(execution tick)", prompt)
        self.assertNotIn("<reflection>", prompt)
        self.assertNotIn("durable built structures", prompt)
        self.assertNotIn("short gameplay mistake-avoidance tips", prompt)
        self.assertNotIn("Do not include provider limits", prompt)
        self.assertNotIn("what durable structure is built where", prompt)
        self.assertNotIn("read_only_tools", tick)

    def test_autonomy_tick_coalesces_burst_failures_before_injection(self):
        import ledger
        import pipe

        ledger_patch = mock.patch(
            "ledger._ledger_file",
            side_effect=lambda agent_name: self.base / f".ledger-{agent_name}.json",
        )
        ledger_patch.start()
        self.addCleanup(ledger_patch.stop)
        ledger.save_ledger("doug", {
            "objective": "Fix iron smelting",
            "plan_steps": ["place_entity transport-belt"],
            "progress_notes": [],
            "updated_at": "now",
        })
        journal.append_event("doug", "progress", "Lab powered")
        for _ in range(8):
            journal.append_event(
                "doug",
                "failure",
                "game_rejected: Cannot place belt at 56,-24",
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
        thread._reflect_interval = 99

        prompt = thread._autonomy_tick()["message"]

        self.assertIn("progress: Lab powered", prompt)
        self.assertIn("failure (x8): game_rejected: Cannot place belt", prompt)
        self.assertEqual(prompt.count("Cannot place belt"), 1)

    def test_autonomy_tick_replans_when_plan_done_signal_is_followed_by_failures(self):
        import ledger
        import pipe

        ledger.save_ledger("doug", {
            "objective": "Power the lab",
            "plan_steps": ["insert science packs", "start research"],
            "progress_notes": [],
            "updated_at": "now",
        })
        journal.append_event(
            "doug",
            "progress",
            "objective completed: lab is powered",
            signal="plan_done",
        )
        for i in range(3):
            journal.append_event("doug", "failure", f"game_rejected: stale action {i}")

        class StubRCON:
            def execute(self, _cmd):
                return ""

        thread = pipe.AgentThread.__new__(pipe.AgentThread)
        thread.agent_name = "doug"
        thread.rcon = StubRCON()
        thread._exec_ticks_since_plan = 0
        thread._planner_interval = 5
        thread._planner_model = None
        thread._reflect_interval = 99

        tick = thread._autonomy_tick()

        self.assertTrue(tick["read_only_tools"])
        self.assertIn("(planner tick)", tick["message"])
        self.assertIn("read-only planning turn", tick["message"])
        self.assertIn("objective completed: lab is powered", tick["message"])

    def test_autonomy_tick_does_not_replan_from_plan_done_prose_without_signal(self):
        import ledger
        import pipe

        ledger.save_ledger("doug", {
            "objective": "Power the lab",
            "plan_steps": ["insert science packs", "start research"],
            "progress_notes": [],
            "updated_at": "now",
        })
        journal.append_event("doug", "progress", "objective completed: lab is powered")
        for i in range(3):
            journal.append_event("doug", "failure", f"game_rejected: stale action {i}")

        class StubRCON:
            def execute(self, _cmd):
                return ""

        thread = pipe.AgentThread.__new__(pipe.AgentThread)
        thread.agent_name = "doug"
        thread.rcon = StubRCON()
        thread._exec_ticks_since_plan = 0
        thread._planner_interval = 5
        thread._planner_model = None
        thread._reflect_interval = 99

        tick = thread._autonomy_tick()

        self.assertNotIn("read_only_tools", tick)
        self.assertIn("(execution tick)", tick["message"])

    def test_autonomy_tick_does_not_treat_awaiting_execution_as_plan_done(self):
        import ledger
        import pipe

        ledger.save_ledger("doug", {
            "objective": "Energize the existing smelting array",
            "plan_steps": [
                "walk_to(-41, 26)",
                "insert_items(unit 49, 'coal', 5, 'fuel')",
            ],
            "progress_notes": [],
            "updated_at": "now",
        })
        journal.append_event(
            "doug",
            "progress",
            "no changes across planning ticks; plan validated and ready for execution",
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
        thread._reflect_interval = 99

        tick = thread._autonomy_tick()

        self.assertNotIn("read_only_tools", tick)
        self.assertIn("(execution tick)", tick["message"])
        self.assertIn("no changes across planning ticks", tick["message"])


if __name__ == "__main__":
    unittest.main()
