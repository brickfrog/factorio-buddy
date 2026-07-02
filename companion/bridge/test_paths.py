import tempfile
import unittest
from pathlib import Path
from unittest import mock

import paths


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


if __name__ == "__main__":
    unittest.main()
