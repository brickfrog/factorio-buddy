import json
import os
import tempfile
import unittest
from pathlib import Path

import report as bridge_report


class FakeRCON:
    def __init__(self):
        self.commands = []

    def execute(self, command):
        self.commands.append(command)
        if "live_state_line" in command:
            return (
                "Live state: nauvis @ 46.7,-15.6; player entities: "
                "electric-mining-drill=1, stone-furnace=2, lab=1"
            )
        if "get_power_status" in command:
            return json.dumps({
                "network_id": 7,
                "pole_count": 15,
                "generators": [{"name": "steam-engine", "count": 1}],
                "consumers": {
                    "working": 3,
                    "low_power": 0,
                    "no_power": 0,
                    "total": 3,
                },
                "production_kw": 900,
                "consumption_kw": 120,
                "satisfaction": "ok",
            })
        raise AssertionError(command)


class BrokenRCON:
    def execute(self, command):
        raise ConnectionError("server down")


def log_line(message, ts, *, agent="DOUG-NAUVIS", level="INFO"):
    return json.dumps({
        "record": {
            "message": message,
            "extra": {"agent": agent},
            "level": {"name": level},
            "time": {
                "repr": f"2026-06-30 00:{int(ts) % 60:02d}:00.000000-05:00",
                "timestamp": float(ts),
            },
        },
    })


class BridgeReportTest(unittest.TestCase):
    def test_analyze_log_summarizes_attempts_progress_failures_and_provider_pause(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bridge-test.jsonl"
            path.write_text("\n".join([
                log_line("Logging to logs/bridge-test.log", 1000, agent="system"),
                log_line("spawning claude sdk [model=haiku] (new session)", 1010),
                log_line(
                    "autonomy -> doug-nauvis: <ledger>\nobjective: <goal>\n"
                    "progress: <what changed>\n</ledger>",
                    1015,
                ),
                log_line(
                    "autonomy -> doug-nauvis: Live state: nauvis @ 1,2; "
                    "player entities: burner-mining-drill=2, lab=1\n"
                    "<ledger>\nobjective: Build automation\n"
                    "progress: steam power online\n</ledger>",
                    1020,
                ),
                log_line(
                    'tool_result game_rejected: {"entity":"transport-belt",'
                    '"error":"Cannot place entity here"}',
                    1030,
                ),
                log_line(
                    'tool_result game_rejected: {"entity":"transport-belt",'
                    '"error":"Cannot place entity here"}',
                    1040,
                ),
                log_line(
                    'tool_result game_rejected: {"entity":"transport-belt",'
                    '"error":"Cannot place entity here"}',
                    1050,
                ),
                log_line(
                    "text: automation research completed. Research count: 4 of 275. "
                    "Power grid operational.",
                    1060,
                ),
                log_line(
                    "provider usage limit active until 2026-06-30 03:19:43 CDT; "
                    "pausing agent attempts",
                    1070,
                ),
                log_line("done: $1.0000 | 10 turns | 60.0s", 1080),
            ]) + "\n")

            report = bridge_report.analyze_log(path, recent_progress_window_s=300)

        self.assertEqual(report.sdk_attempts, 1)
        self.assertEqual(report.sdk_done, 1)
        self.assertEqual(report.provider_pauses, 1)
        self.assertEqual(report.provider_reset_until, "2026-06-30 03:19:43 CDT")
        self.assertEqual(report.research_completed_events, 1)
        self.assertEqual(report.max_research_count, 4)
        self.assertEqual(report.latest_entities, "burner-mining-drill=2, lab=1")
        self.assertEqual(report.latest_objective, "Build automation")
        self.assertEqual(report.latest_progress, "steam power online")
        self.assertEqual(
            report.top_gameplay_rejections,
            [("Cannot place entity here | entity=transport-belt", 3)],
        )
        self.assertIn("provider paused", report.verdict)

    def test_game_rejection_signature_unwraps_nested_mcp_text_payload(self):
        signature = bridge_report._game_rejection_signature(
            'tool_result game_rejected: [{"type":"text","text":"{'
            '\\"success\\": false, \\"queued\\": 0, '
            '\\"error\\": \\"Crafting did not start\\", '
            '\\"recipe\\": \\"transport-belt\\"}"}]'
        )

        self.assertEqual(signature, "Crafting did not start | recipe=transport-belt")

    def test_research_status_payload_is_not_a_gameplay_rejection_signature(self):
        signature = bridge_report._game_rejection_signature(
            'tool_result game_rejected: [{"type":"text","text":"{'
            '\\"researched_count\\":6,\\"total_count\\":275,'
            '\\"research_progress\\":0.36,'
            '\\"research_queue\\":[{\\"name\\":\\"steel-processing\\"}],'
            '\\"labs\\":{\\"count\\":1,\\"powered\\":0,\\"working\\":0},'
            '\\"message\\":\\"Labs have no power! Connect labs to the power grid.\\"}"}]'
        )

        self.assertEqual(signature, "")

    def test_truncated_research_status_text_is_not_a_rejection_signature(self):
        signature = bridge_report._game_rejection_signature(
            '- failure: game_rejected: [{"type":"text","text":"{'
            '\\"researched_count\\":6,\\"total_count\\":275,'
            '\\"research_progress\\":0.36,\\"research_queue\\":[{\\"name\\":\\"stee'
        )

        self.assertEqual(signature, "")

    def test_invalid_request_payload_is_not_a_gameplay_rejection_signature(self):
        payload = json.dumps([{
            "type": "text",
            "text": json.dumps({
                "success": False,
                "error": "value for required field 'category' is missing",
                "action_needed": "fix_get_power_status",
            }),
        }])

        signature = bridge_report._game_rejection_signature(
            f"tool_result game_rejected: {payload}"
        )
        truncated_signature = bridge_report._game_rejection_signature(
            "- failure: game_rejected: "
            "[{\"type\":\"text\",\"text\":\"{\\\"success\\\":false,"
            "\\\"error\\\":\\\"value for required field 'category' is missing"
        )

        self.assertEqual(signature, "")
        self.assertEqual(truncated_signature, "")

    def test_analyze_log_counts_research_status_without_rejection_noise(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bridge-test.jsonl"
            path.write_text("\n".join([
                log_line("Logging to logs/bridge-test.log", 1000, agent="system"),
                log_line(
                    'tool_result game_rejected: [{"type":"text","text":"{'
                    '\\"researched_count\\":6,\\"total_count\\":275,'
                    '\\"research_progress\\":0.36,'
                    '\\"research_queue\\":[{\\"name\\":\\"steel-processing\\"}],'
                    '\\"labs\\":{\\"count\\":1,\\"powered\\":0,\\"working\\":0},'
                    '\\"message\\":\\"Labs have no power! Connect labs to the power grid.\\"}"}]',
                    1010,
                ),
                log_line(
                    'tool_result game_rejected: {"entity":"transport-belt",'
                    '"error":"Cannot place entity here"}',
                    1020,
                ),
            ]) + "\n")

            report = bridge_report.analyze_log(path)

        self.assertEqual(report.max_research_count, 6)
        self.assertEqual(
            report.top_gameplay_rejections,
            [("Cannot place entity here | entity=transport-belt", 1)],
        )

    def test_analyze_log_ignores_low_value_planning_progress(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bridge-test.jsonl"
            path.write_text("\n".join([
                log_line("Logging to logs/bridge-test.log", 1000, agent="system"),
                log_line(
                    "<ledger>\n"
                    "objective: Repair power\n"
                    "progress: diagnosed boiler_no_fuel and pole gap\n"
                    "</ledger>",
                    1010,
                ),
                log_line(
                    "<ledger>\n"
                    "progress: no change across fifty-four planning ticks. "
                    "State stable. Plan fully validated and awaiting execution turns.\n"
                    "</ledger>",
                    1300,
                ),
            ]) + "\n")

            report = bridge_report.analyze_log(path, recent_progress_window_s=60)

        self.assertEqual(report.latest_progress, "diagnosed boiler_no_fuel and pole gap")
        self.assertEqual(report.recent_progress_events, 0)

    def test_analyze_log_compacts_power_tool_results_and_ignores_prompt_noise(self):
        diagnose_payload = json.dumps([{
            "type": "text",
            "text": json.dumps({
                "summary": {
                    "issue_count": 2,
                    "critical_issues": 2,
                    "offshore_pumps": 1,
                    "boilers": 1,
                    "steam_engines": 1,
                },
                "issues": [
                    {"type": "steam_engine_no_steam", "severity": "critical"},
                    {"type": "boiler_no_fuel", "severity": "critical"},
                ],
                "status": "critical",
                "next_action": "repair_existing_steam_power",
            }),
        }])
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bridge-test.jsonl"
            path.write_text("\n".join([
                log_line("Logging to logs/bridge-test.log", 1000, agent="system"),
                log_line(
                    "autonomy -> doug-nauvis: Recent events:\n"
                    "- progress: no change across fifty planning ticks\n\n"
                    "Continuity ledger: continue the committed objective: "
                    "energize the power grid\n"
                    "Plan:\n"
                    "1. insert coal into boiler\n"
                    "Recent progress:\n"
                    "- no change; power repair plan awaiting execution",
                    1010,
                ),
                log_line(f"tool_result: {diagnose_payload}", 1015),
                log_line(
                    "reply: State is unchanged. The power repair plan is "
                    "validated and ready for execution.",
                    1020,
                ),
                log_line(
                    "tool_result: Factorioctl bridge blocked non-read-only "
                    "tool during planner/reflection turn: repair_steam_power. "
                    "This turn may only use read-only diagnostics.",
                    1030,
                ),
                log_line(
                    'tool: repair_steam_power({"target_x":-41,"target_y":23})',
                    1040,
                ),
                log_line(
                    "text: The map reveals the layout. Steam engine sits north "
                    "of the boiler near the offshore pump.",
                    1050,
                ),
            ]) + "\n")

            report = bridge_report.analyze_log(path)

        self.assertIn("steam_power status=critical", report.latest_power)
        self.assertIn("issues=2", report.latest_power)
        self.assertIn("types=steam_engine_no_steam, boiler_no_fuel", report.latest_power)
        self.assertIn("next=repair_existing_steam_power", report.latest_power)

    def test_analyze_log_accepts_concise_text_power_status(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bridge-test.jsonl"
            path.write_text("\n".join([
                log_line("Logging to logs/bridge-test.log", 1000, agent="system"),
                log_line("text: Power grid operational.", 1010),
            ]) + "\n")

            report = bridge_report.analyze_log(path)

        self.assertEqual(report.latest_power, "text: Power grid operational.")

    def test_game_rejection_lines_extracts_prompt_embedded_entries(self):
        lines = bridge_report._game_rejection_lines(
            "autonomy -> doug:\n"
            "- failure: game_rejected: one\n"
            "- progress: ok\n"
            "- failure: game_rejected: two\n"
        )

        self.assertEqual(lines, [
            "- failure: game_rejected: one",
            "- failure: game_rejected: two",
        ])

    def test_format_report_includes_operator_verdict_and_rejection_signatures(self):
        report = bridge_report.BridgeRunReport(
            log_path="bridge.jsonl",
            started_at="start",
            ended_at="end",
            duration_s=3661,
            sdk_attempts=2,
            sdk_done=1,
            recent_progress_events=1,
            latest_objective="Build smelting",
            top_gameplay_rejections=[("Cannot place entity here", 4)],
            verdict="safe to keep running: recent progress detected",
        )

        text = bridge_report.format_report(report)

        self.assertIn("Bridge Run Report", text)
        self.assertIn("1h01m01s", text)
        self.assertIn("attempts=2", text)
        self.assertIn("4x Cannot place entity here", text)
        self.assertIn("verdict: safe to keep running", text)

    def test_enrich_live_state_uses_compact_mod_remotes(self):
        report = bridge_report.BridgeRunReport()
        fake = FakeRCON()

        bridge_report.enrich_live_state(
            report,
            fake,
            agent_id="doug-nauvis",
            power_x=1,
            power_y=-2,
            power_radius=300,
        )

        self.assertTrue(report.live_attempted)
        self.assertTrue(report.live_connected)
        self.assertEqual(
            report.live_entities,
            "electric-mining-drill=1, stone-furnace=2, lab=1",
        )
        self.assertIn("steam-engine=1", report.live_power)
        self.assertIn("satisfaction=ok", report.live_power)
        command_text = "\n".join(fake.commands)
        self.assertIn('remote.call("claude_interface", "live_state_line"', command_text)
        self.assertIn('remote.call("claude_interface", "get_power_status"', command_text)
        self.assertNotIn("game.surfaces", command_text)

        formatted = bridge_report.format_report(report)
        self.assertIn("live_state: Live state: nauvis", formatted)
        self.assertIn("live_power: network=7", formatted)

    def test_enrich_live_state_records_nonfatal_rcon_errors(self):
        report = bridge_report.BridgeRunReport()

        bridge_report.enrich_live_state(report, BrokenRCON())

        self.assertTrue(report.live_attempted)
        self.assertFalse(report.live_connected)
        self.assertIn("server down", report.live_error)
        self.assertIn("live: unavailable", bridge_report.format_report(report))

    def test_latest_log_returns_newest_jsonl_log(self):
        with tempfile.TemporaryDirectory() as tmp:
            log_dir = Path(tmp)
            older = log_dir / "bridge-older.jsonl"
            newer = log_dir / "bridge-newer.jsonl"
            older.write_text("")
            newer.write_text("")
            os.utime(older, (1, 1))
            os.utime(newer, (2, 2))

            self.assertEqual(bridge_report.latest_log(log_dir), newer)


if __name__ == "__main__":
    unittest.main()
