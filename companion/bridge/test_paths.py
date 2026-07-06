import tempfile
import unittest
from pathlib import Path
from unittest import mock

import paths
import runtime_paths


class PathDiscoveryTests(unittest.TestCase):
    def setUp(self):
        self.tempdir = tempfile.TemporaryDirectory()
        self.addCleanup(self.tempdir.cleanup)
        self.base = Path(self.tempdir.name)

    def test_find_script_output_uses_typed_server_data_env_and_creates_dir(self):
        server_data = self.base / "server-data"

        with mock.patch.dict("os.environ", {"FACTORIO_SERVER_DATA": f" {server_data} "}):
            result = paths.find_script_output()

        self.assertEqual(result, server_data / "script-output")
        self.assertTrue(result.is_dir())

    def test_find_mods_dir_uses_typed_mods_env_and_reports_missing(self):
        mods_dir = self.base / "mods"
        mods_dir.mkdir()

        with mock.patch.dict("os.environ", {"FACTORIO_MODS_DIR": f" {mods_dir} "}):
            self.assertEqual(paths.find_mods_dir(), mods_dir)

        missing = self.base / "missing-mods"
        with mock.patch.dict("os.environ", {"FACTORIO_MODS_DIR": str(missing)}):
            with self.assertRaisesRegex(FileNotFoundError, "FACTORIO_MODS_DIR="):
                paths.find_mods_dir()

    def test_find_factorioctl_mcp_uses_typed_mcp_bin_env(self):
        mcp = self.base / "mcp"
        mcp.write_text("#!/bin/sh\n")

        with mock.patch.dict("os.environ", {"FACTORIOCTL_MCP_BIN": f" {mcp} "}):
            self.assertEqual(paths.find_factorioctl_mcp(), str(mcp))

    def test_bridge_state_dir_uses_env_or_server_data(self):
        configured = self.base / "configured-state"
        server_data = self.base / "server-data"

        self.assertEqual(
            runtime_paths.bridge_state_dir(
                env={"FACTORIOCTL_BRIDGE_STATE_DIR": f" {configured} "},
            ),
            configured,
        )
        self.assertTrue(configured.is_dir())
        self.assertEqual(
            runtime_paths.bridge_state_dir(env={"FACTORIO_SERVER_DATA": str(server_data)}),
            server_data / "bridge-state",
        )
        self.assertTrue((server_data / "bridge-state").is_dir())

    def test_read_candidates_prefers_state_file_then_existing_legacy(self):
        state_dir = self.base / "state"
        legacy = runtime_paths.BRIDGE_DIR / ".unit-test-legacy-state"
        self.addCleanup(lambda: legacy.unlink(missing_ok=True))
        legacy.write_text("legacy")

        with mock.patch.dict(
            "os.environ",
            {"FACTORIOCTL_BRIDGE_STATE_DIR": str(state_dir)},
        ):
            candidates = runtime_paths.read_candidates(".unit-test-legacy-state")

        self.assertEqual(candidates[0], state_dir / ".unit-test-legacy-state")
        self.assertEqual(candidates[1], legacy)


if __name__ == "__main__":
    unittest.main()
