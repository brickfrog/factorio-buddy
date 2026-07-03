import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest import mock

import report as bridge_report
from models import BridgeLogMessage, BridgeLogRecord, McpTextPayload, RconJsonResponse


class FakeRCON:
    def __init__(self):
        self.commands = []

    def execute(self, command):
        self.commands.append(command)
        if "live_state_result" in command:
            return json.dumps({
                "found": True,
                "surface": "nauvis",
                "x": 46.7,
                "y": -15.6,
                "entity_counts": {
                    "electric-mining-drill": 1,
                    "stone-furnace": 2,
                    "lab": 1,
                },
            })
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


class DiagnosticPowerRCON:
    def execute(self, command):
        if "live_state_result" in command:
            return json.dumps({
                "found": True,
                "surface": "nauvis",
                "x": 46.7,
                "y": -15.6,
                "entity_counts": {"boiler": 1},
            })
        if "get_power_status" in command:
            return json.dumps({
                "next_action": "repair_existing_steam_power",
                "existing_plant": {
                    "summary": {"issue_count": 1, "critical_issues": 1},
                    "issues": [{"type": "boiler_no_fuel"}],
                    "status": "critical",
                },
            })
        raise AssertionError(command)


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
    def test_iter_records_uses_typed_loguru_model_and_skips_bad_lines(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bridge-test.jsonl"
            path.write_text(
                "\n".join([
                    "not json",
                    json.dumps({"record": "bad"}),
                    json.dumps({
                        "record": {
                            "message": {"shape": "coerced"},
                            "extra": {"agent": "doug-nauvis"},
                            "level": {"name": "WARNING"},
                            "time": {
                                "repr": "2026-06-30 13:00:00.000000-05:00",
                                "timestamp": "12.5",
                            },
                        },
                    }),
                ]) + "\n"
            )

            records = list(bridge_report.iter_records(path))

        self.assertEqual(len(records), 1)
        self.assertEqual(records[0].message, "{'shape': 'coerced'}")
        self.assertEqual(records[0].timestamp, 12.5)
        self.assertEqual(records[0].time, "2026-06-30 13:00:00.000000-05:00")
        self.assertEqual(records[0].level, "WARNING")
        self.assertEqual(records[0].agent, "doug-nauvis")

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
                log_line('tool: insert_items({"item":"coal"})', 1025),
                log_line('tool: build_fuel_supply({"consumer_unit_number":49})', 1026),
                log_line('tool: build_automation_science({"assembler_unit_number":80})', 1027),
                log_line('tool: craft({"recipe":"iron-gear-wheel","count":4})', 1028),
                log_line(
                    'tool: feed_lab_from_inventory({"science_pack":"automation-science-pack",'
                    '"dry_run":false})',
                    1029,
                ),
                log_line(
                    'tool_result: {"automation_verified":{"success":true}}',
                    1030,
                ),
                log_line(
                    'tool_result game_rejected: {"success":false,'
                    '"automation_verified":{"success":false}}',
                    1031,
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
                    'tool_result game_rejected: {"entity":"transport-belt",'
                    '"error":"Cannot place entity here"}',
                    1060,
                ),
                log_line(
                    "text: automation research completed. Research count: 4 of 275. "
                    "Power grid operational.",
                    1070,
                ),
                log_line(
                    "provider usage limit active until 2026-06-30 03:19:43 CDT; "
                    "pausing agent attempts",
                    1080,
                ),
                log_line("done: $1.0000 | 10 turns | 60.0s", 1090),
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
        self.assertEqual(report.automation_tool_calls, 2)
        self.assertEqual(report.manual_transfer_tool_calls, 3)
        self.assertEqual(report.automation_to_manual_ratio, 0.666667)
        self.assertEqual(report.fuel_automation_tool_calls, 1)
        self.assertEqual(report.manual_fuel_transfer_tool_calls, 1)
        self.assertEqual(report.fuel_automation_to_manual_ratio, 1.0)
        self.assertEqual(report.science_automation_tool_calls, 1)
        self.assertEqual(report.manual_science_transfer_tool_calls, 1)
        self.assertEqual(report.science_automation_to_manual_ratio, 1.0)
        self.assertEqual(report.material_flow_automation_tool_calls, 0)
        self.assertEqual(report.manual_material_transfer_tool_calls, 0)
        self.assertEqual(report.component_automation_tool_calls, 1)
        self.assertEqual(report.manual_component_craft_tool_calls, 1)
        self.assertEqual(report.component_automation_to_manual_ratio, 1.0)
        self.assertEqual(report.automation_verified_successes, 1)
        self.assertEqual(report.automation_verified_failures, 1)
        self.assertEqual(
            report.top_gameplay_rejections,
            [
                ("Cannot place entity here | entity=transport-belt", 3),
                ("automation_unverified", 1),
            ],
        )
        self.assertIn("provider paused", report.verdict)

    def test_analyze_log_flags_unverified_automation_despite_recent_progress(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bridge-test.jsonl"
            path.write_text("\n".join([
                log_line("spawning claude sdk [model=haiku] (new session)", 1000),
                log_line(
                    "autonomy -> doug-nauvis: <ledger>\n"
                    "objective: Build automation\n"
                    "progress: attempted automation controller\n"
                    "</ledger>",
                    1010,
                ),
                log_line(
                    'tool_result game_rejected: {"success":false,'
                    '"automation_verified":{"success":false}}',
                    1020,
                ),
                log_line(
                    'tool_result game_rejected: {"success":false,'
                    '"automation_verified":{"success":false}}',
                    1030,
                ),
                log_line("done: $1.0000 | 10 turns | 60.0s", 1040),
            ]) + "\n")

            report = bridge_report.analyze_log(path, recent_progress_window_s=300)

        self.assertEqual(report.automation_verified_failures, 2)
        self.assertIn("automation controllers are failing verification", report.verdict)

    def test_analyze_log_flags_manual_fuel_babysitting_despite_recent_progress(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bridge-test.jsonl"
            path.write_text("\n".join([
                log_line("spawning claude sdk [model=haiku] (new session)", 1000),
                log_line(
                    "autonomy -> doug-nauvis: <ledger>\n"
                    "objective: Build automation\n"
                    "progress: plates are being produced\n"
                    "</ledger>",
                    1010,
                ),
                log_line('tool: insert_items({"item":"coal","inventory_type":"fuel"})', 1020),
                log_line('tool: insert_items({"item":"coal","inventory_type":"fuel"})', 1030),
                log_line("done: $0.2500 | 4 turns | 30.0s", 1040),
            ]) + "\n")

            report = bridge_report.analyze_log(path, recent_progress_window_s=300)

        self.assertEqual(report.manual_fuel_transfer_tool_calls, 2)
        self.assertEqual(report.fuel_automation_tool_calls, 0)
        self.assertIn("fuel is being babysat manually", report.verdict)

    def test_analyze_log_treats_belted_fuel_route_as_progress(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bridge-test.jsonl"
            path.write_text("\n".join([
                log_line("spawning claude sdk [model=haiku] (new session)", 1000),
                log_line(
                    "autonomy -> doug-nauvis: <ledger>\n"
                    "objective: build coal fuel route to boiler\n"
                    "progress: belt route started\n"
                    "</ledger>",
                    1010,
                ),
                log_line('tool: insert_items({"item":"coal","inventory_type":"fuel"})', 1020),
                log_line('tool: insert_items({"item":"coal","inventory_type":"fuel"})', 1030),
                log_line('tool: route_belt({"from":{"x":1,"y":1},"to":{"x":2,"y":1}})', 1040),
                log_line("done: $0.2500 | 4 turns | 30.0s", 1050),
            ]) + "\n")

            report = bridge_report.analyze_log(path, recent_progress_window_s=300)

        self.assertEqual(report.manual_fuel_transfer_tool_calls, 2)
        self.assertEqual(report.material_flow_automation_tool_calls, 1)
        self.assertIn("fuel route automation is in progress", report.verdict)

    def test_analyze_log_flags_manual_science_babysitting_despite_recent_progress(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bridge-test.jsonl"
            path.write_text("\n".join([
                log_line("spawning claude sdk [model=haiku] (new session)", 1000),
                log_line(
                    "autonomy -> doug-nauvis: <ledger>\n"
                    "objective: Build automation\n"
                    "progress: research is moving\n"
                    "</ledger>",
                    1010,
                ),
                log_line(
                    'tool: craft({"recipe":"automation-science-pack","count":12})',
                    1020,
                ),
                log_line(
                    'tool: feed_lab_from_inventory({"science_pack":"automation-science-pack",'
                    '"dry_run":false})',
                    1030,
                ),
                log_line("done: $0.2500 | 4 turns | 30.0s", 1040),
            ]) + "\n")

            report = bridge_report.analyze_log(path, recent_progress_window_s=300)

        self.assertEqual(report.manual_science_transfer_tool_calls, 2)
        self.assertEqual(report.science_automation_tool_calls, 0)
        self.assertIn("science is being hand-crafted or hand-fed", report.verdict)

    def test_analyze_log_flags_manual_material_babysitting_despite_recent_progress(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bridge-test.jsonl"
            path.write_text("\n".join([
                log_line("spawning claude sdk [model=haiku] (new session)", 1000),
                log_line(
                    "autonomy -> doug-nauvis: <ledger>\n"
                    "objective: Build automation\n"
                    "progress: plates are being produced\n"
                    "</ledger>",
                    1010,
                ),
                log_line(
                    'tool: insert_items({"item":"iron-ore","inventory_type":"furnace_source"})',
                    1020,
                ),
                log_line(
                    'tool: extract_items({"item":"iron-plate","inventory_type":"furnace_result"})',
                    1030,
                ),
                log_line("done: $0.2500 | 4 turns | 30.0s", 1040),
            ]) + "\n")

            report = bridge_report.analyze_log(path, recent_progress_window_s=300)

        self.assertEqual(report.manual_material_transfer_tool_calls, 2)
        self.assertEqual(report.material_flow_automation_tool_calls, 0)
        self.assertIn("ore or plates are being hand-carried", report.verdict)

    def test_analyze_log_flags_manual_component_babysitting_despite_recent_progress(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bridge-test.jsonl"
            path.write_text("\n".join([
                log_line("spawning claude sdk [model=haiku] (new session)", 1000),
                log_line(
                    "autonomy -> doug-nauvis: <ledger>\n"
                    "objective: Build automation science\n"
                    "progress: research is moving\n"
                    "</ledger>",
                    1010,
                ),
                log_line(
                    'tool: craft({"recipe":"iron-gear-wheel","count":12})',
                    1020,
                ),
                log_line(
                    'tool: craft({"recipe":"copper-cable","count":12})',
                    1030,
                ),
                log_line(
                    'tool: build_assembler_feed({"assembler_unit_number":80})',
                    1040,
                ),
                log_line("done: $0.2500 | 4 turns | 30.0s", 1050),
            ]) + "\n")

            report = bridge_report.analyze_log(path, recent_progress_window_s=300)

        self.assertEqual(report.manual_component_craft_tool_calls, 2)
        self.assertEqual(report.component_automation_tool_calls, 1)
        self.assertIn("science ingredients are being hand-crafted", report.verdict)

    def test_analyze_records_accepts_typed_records_without_file_roundtrip(self):
        done_line = json.dumps({
            "record": {
                "message": "done: $1.0000 | 10 turns | 60.0s",
                "time": {"timestamp": 1020, "repr": "end"},
                "level": {"name": "INFO"},
                "extra": {"agent": "DOUG-NAUVIS"},
            },
        })
        records = (
            BridgeLogRecord(
                message="spawning claude sdk [model=haiku] (new session)",
                timestamp=1000,
                time="start",
                agent="DOUG-NAUVIS",
            ),
            BridgeLogRecord(
                message=(
                    "autonomy -> doug-nauvis: <ledger>\n"
                    "objective: Build automation\n"
                    "progress: typed records analyzed\n"
                    "</ledger>"
                ),
                timestamp=1010,
                time="middle",
                agent="DOUG-NAUVIS",
            ),
            done_line,
            "{bad json}",
        )

        report = bridge_report.analyze_records(
            records,
            log_path="typed-memory",
            recent_progress_window_s=60,
        )

        self.assertEqual(report.log_path, "typed-memory")
        self.assertEqual(report.started_at, "start")
        self.assertEqual(report.ended_at, "end")
        self.assertEqual(report.duration_s, 20)
        self.assertEqual(report.sdk_attempts, 1)
        self.assertEqual(report.sdk_done, 1)
        self.assertEqual(report.latest_objective, "Build automation")
        self.assertEqual(report.latest_progress, "typed records analyzed")
        self.assertEqual(report.recent_progress_events, 1)

    def test_game_rejection_signature_unwraps_nested_mcp_text_payload(self):
        signature = BridgeLogMessage.first_gameplay_rejection_signature_from_text(
            'tool_result game_rejected: [{"type":"text","text":"{'
            '\\"success\\": false, \\"queued\\": 0, '
            '\\"error\\": \\"Crafting did not start\\", '
            '\\"recipe\\": \\"transport-belt\\"}"}]'
        )

        self.assertEqual(signature, "Crafting did not start | recipe=transport-belt")

    def test_report_mcp_payload_parser_uses_shared_unwrapper(self):
        payload = json.dumps([{
            "type": "text",
            "text": json.dumps({
                "success": False,
                "error": "Cannot place entity here",
            }),
        }])
        malformed = json.dumps([{
            "type": "text",
            "text": '{"success":false,"error":"truncated',
        }])

        self.assertEqual(
            McpTextPayload.from_text(payload).value,
            {"success": False, "error": "Cannot place entity here"},
        )
        self.assertEqual(
            McpTextPayload.from_text(malformed).value,
            '{"success":false,"error":"truncated',
        )

    def test_report_json_response_parser_uses_typed_rcon_response(self):
        self.assertEqual(
            RconJsonResponse.parse_value(
                "Factorio says hello\n"
                '{"network_id": 7, "pole_count": 15}\n'
            ),
            {"network_id": 7, "pole_count": 15},
        )
        with self.assertRaisesRegex(ValueError, "rcon_response: did not contain JSON"):
            RconJsonResponse.parse_value("nope")

    def test_research_status_payload_is_not_a_gameplay_rejection_signature(self):
        signature = BridgeLogMessage.first_gameplay_rejection_signature_from_text(
            'tool_result game_rejected: [{"type":"text","text":"{'
            '\\"researched_count\\":6,\\"total_count\\":275,'
            '\\"research_progress\\":0.36,'
            '\\"research_queue\\":[{\\"name\\":\\"steel-processing\\"}],'
            '\\"labs\\":{\\"count\\":1,\\"powered\\":0,\\"working\\":0},'
            '\\"message\\":\\"Labs have no power! Connect labs to the power grid.\\"}"}]'
        )

        self.assertEqual(signature, "")

    def test_truncated_research_status_text_is_not_a_rejection_signature(self):
        signature = BridgeLogMessage.first_gameplay_rejection_signature_from_text(
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

        signature = BridgeLogMessage.first_gameplay_rejection_signature_from_text(
            f"tool_result game_rejected: {payload}"
        )
        truncated_signature = BridgeLogMessage.first_gameplay_rejection_signature_from_text(
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

    def test_analyze_log_uses_typed_entity_summary_parser(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "bridge-test.jsonl"
            path.write_text("\n".join([
                log_line("Logging to logs/bridge-test.log", 1000, agent="system"),
                log_line(
                    "Live state: nauvis @ 1,2; player entities: "
                    "stone-furnace=2, transport-belt=16, stone-furnace=1",
                    1010,
                ),
            ]) + "\n")

            report = bridge_report.analyze_log(path)

        self.assertEqual(
            report.latest_entities,
            "stone-furnace=3, transport-belt=16",
        )

    def test_analyze_log_preserves_planning_progress(self):
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

        self.assertEqual(
            report.latest_progress,
            "no change across fifty-four planning ticks. "
            "State stable. Plan fully validated and awaiting execution turns.",
        )
        self.assertEqual(report.recent_progress_events, 0)
        self.assertIn("no recent progress", report.verdict)

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

    def test_power_compaction_uses_typed_remote_payload_models(self):
        self.assertEqual(
            BridgeLogMessage.power_summary_from_payload({
                "network_id": 7,
                "pole_count": 15,
                "generators": [
                    {"name": "steam-engine", "count": 1},
                    "not a generator",
                ],
                "consumers": {
                    "working": 3,
                    "low_power": 0,
                    "no_power": 0,
                    "total": 3,
                },
                "production_kw": 900,
                "consumption_kw": 120,
                "satisfaction": "ok",
            }),
            "network=7; poles=15; generators=steam-engine=1; "
            "consumers=3 working/0 low/0 none/3 total; "
            "production_kw=900; consumption_kw=120; satisfaction=ok",
        )
        self.assertEqual(
            BridgeLogMessage.power_summary_from_payload({"error": "server down"}),
            "unavailable: server down",
        )
        self.assertEqual(
            BridgeLogMessage.power_summary_from_payload({
                "next_action": "repair_existing_steam_power",
                "existing_plant": {
                    "summary": {"issue_count": 1, "critical_issues": 1},
                    "issues": [{"type": "boiler_no_fuel"}],
                    "status": "critical",
                },
            }),
            "steam_power status=critical; issues=1; critical=1; "
            "types=boiler_no_fuel; next=repair_existing_steam_power",
        )

    def test_message_predicate_helpers_use_typed_log_models(self):
        self.assertTrue(BridgeLogMessage.from_text(
            "<ledger>\nprogress: belts fixed\n</ledger>",
        ).progress_event)
        self.assertFalse(BridgeLogMessage.from_text("ordinary line").progress_event)

        self.assertTrue(BridgeLogMessage.from_text(
            "text: Power layout: steam engine sits north of the boiler.",
        ).power_evidence.is_power)
        self.assertFalse(BridgeLogMessage.from_text(
            "autonomy -> doug: Power layout: steam engine sits north of the boiler.",
        ).power_evidence.is_power)

    def test_game_rejection_lines_extracts_prompt_embedded_entries(self):
        lines = BridgeLogMessage.from_text(
            "autonomy -> doug:\n"
            "- failure: game_rejected: one\n"
            "- progress: ok\n"
            "- failure: game_rejected: two\n"
        ).gameplay_rejection_lines

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
            automation_tool_calls=3,
            manual_transfer_tool_calls=2,
            automation_to_manual_ratio=1.5,
            fuel_automation_tool_calls=1,
            manual_fuel_transfer_tool_calls=2,
            fuel_automation_to_manual_ratio=0.5,
            science_automation_tool_calls=2,
            manual_science_transfer_tool_calls=1,
            science_automation_to_manual_ratio=2.0,
            material_flow_automation_tool_calls=3,
            manual_material_transfer_tool_calls=1,
            material_flow_automation_to_manual_ratio=3.0,
            component_automation_tool_calls=4,
            manual_component_craft_tool_calls=2,
            component_automation_to_manual_ratio=2.0,
            automation_verified_successes=2,
            automation_verified_failures=1,
            latest_objective="Build smelting",
            top_gameplay_rejections=[("Cannot place entity here", 4)],
            verdict="safe to keep running: recent progress detected",
        )

        text = bridge_report.format_report(report)

        self.assertIn("Bridge Run Report", text)
        self.assertIn("1h01m01s", text)
        self.assertIn("attempts=2", text)
        self.assertIn("4x Cannot place entity here", text)
        self.assertIn(
            "automation_vs_manual: automation_tool_calls=3 "
            "manual_transfer_tool_calls=2 ratio=1.50",
            text,
        )
        self.assertIn(
            "fuel_automation: fuel_controller_calls=1 "
            "manual_fuel_transfer_calls=2 ratio=0.50",
            text,
        )
        self.assertIn(
            "science_automation: automation_science_controller_calls=2 "
            "manual_science_transfer_calls=1 ratio=2.00",
            text,
        )
        self.assertIn(
            "material_flow_automation: material_flow_controller_calls=3 "
            "manual_material_transfer_calls=1 ratio=3.00",
            text,
        )
        self.assertIn(
            "component_automation: component_controller_calls=4 "
            "manual_component_craft_calls=2 ratio=2.00",
            text,
        )
        self.assertIn("automation_verified: successes=2 failures=1", text)
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
        self.assertIn('remote.call("claude_interface", "live_state_result"', command_text)
        self.assertIn('remote.call("claude_interface", "get_power_status"', command_text)
        self.assertNotIn("game.surfaces", command_text)

        formatted = bridge_report.format_report(report)
        self.assertIn("live_state: Live state: nauvis", formatted)
        self.assertIn("live_power: network=7", formatted)

    def test_enrich_live_state_compacts_power_diagnostic_payload(self):
        report = bridge_report.BridgeRunReport()

        bridge_report.enrich_live_state(report, DiagnosticPowerRCON())

        self.assertTrue(report.live_connected)
        self.assertIn("steam_power status=critical", report.live_power)
        self.assertIn("types=boiler_no_fuel", report.live_power)
        self.assertIn("next=repair_existing_steam_power", report.live_power)

    def test_enrich_live_state_records_nonfatal_rcon_errors(self):
        report = bridge_report.BridgeRunReport()

        bridge_report.enrich_live_state(report, BrokenRCON())

        self.assertTrue(report.live_attempted)
        self.assertFalse(report.live_connected)
        self.assertIn("server down", report.live_error)
        self.assertIn("live: unavailable", bridge_report.format_report(report))

    def test_parse_args_uses_typed_rcon_env_defaults_without_bad_env_crash(self):
        with mock.patch.dict(os.environ, {
            "FACTORIO_RCON_HOST": "rcon.local",
            "FACTORIO_RCON_PORT": "bad",
            "FACTORIO_RCON_PASSWORD": "secret",
        }):
            args = bridge_report.parse_args(["--no-live"])

        self.assertEqual(args.rcon_host, "rcon.local")
        self.assertEqual(args.rcon_port, 27015)
        self.assertEqual(args.rcon_password, "secret")

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
