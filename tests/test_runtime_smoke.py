#!/usr/bin/env python3

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("runtime_smoke.py")
SPEC = importlib.util.spec_from_file_location("runtime_smoke", MODULE_PATH)
runtime_smoke = importlib.util.module_from_spec(SPEC)
assert SPEC and SPEC.loader
sys.modules["runtime_smoke"] = runtime_smoke
SPEC.loader.exec_module(runtime_smoke)


class RuntimeSmokeTests(unittest.TestCase):
    def test_refuses_main_port_by_default(self):
        with self.assertRaises(runtime_smoke.SmokeError) as raised:
            runtime_smoke.ensure_disposable_port(27015, False)
        self.assertEqual(raised.exception.classification, "smoke-runner")

    def test_allows_non_main_port(self):
        runtime_smoke.ensure_disposable_port(27016, False)

    def test_classifies_common_failures(self):
        self.assertEqual(
            runtime_smoke.classify_failure("Error: connection refused"),
            "rcon",
        )
        self.assertEqual(
            runtime_smoke.classify_failure("MCP error -32602: failed to deserialize parameters"),
            "bridge",
        )
        self.assertEqual(
            runtime_smoke.classify_failure("Error: Cannot place entity here"),
            "factorio-game-rejection",
        )
        self.assertEqual(
            runtime_smoke.classify_failure(
                "create_entity returned nil after can_place_entity succeeded"
            ),
            "factorio-game-rejection",
        )
        self.assertEqual(
            runtime_smoke.classify_failure("action_needed\":\"fix_get_power_status"),
            "mod-lua",
        )

    def test_verify_synced_mod_compares_all_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            source = root / "source"
            synced = root / "synced"
            (source / "nested").mkdir(parents=True)
            (synced / "nested").mkdir(parents=True)
            (source / "control.lua").write_text("return 1\n")
            (synced / "control.lua").write_text("return 1\n")
            (source / "nested" / "power.lua").write_text("return 2\n")
            (synced / "nested" / "power.lua").write_text("return 2\n")

            result = runtime_smoke.verify_synced_mod(source, synced)
            self.assertEqual(result.classification, "ok")
            self.assertIn("2 files", result.result)

            (synced / "control.lua").write_text("return 3\n")
            with self.assertRaises(runtime_smoke.SmokeError) as raised:
                runtime_smoke.verify_synced_mod(source, synced)
            self.assertEqual(raised.exception.classification, "mod-sync")


if __name__ == "__main__":
    unittest.main()
