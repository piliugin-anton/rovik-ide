"""Tests for preserving a runner's existing FSKit preferences."""

from __future__ import annotations

from pathlib import Path
import plistlib
import tempfile
import subprocess
import unittest
from unittest.mock import patch

import enable_ci_fskit as setup


class EnableFsKit(unittest.TestCase):
    def test_existing_modules_are_preserved_and_repeating_setup_does_not_write(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "enabledModules.plist"
            original = plistlib.dumps(["com.apple.fskit.exfat", setup.MODULES[0]], fmt=plistlib.FMT_BINARY)
            path.write_bytes(original)
            changed, backup = setup.enable(path)
            self.assertTrue(changed)
            self.assertEqual(backup, original)
            self.assertEqual(plistlib.loads(path.read_bytes()), ["com.apple.fskit.exfat", *setup.MODULES])
            saved = path.read_bytes()
            with patch.object(setup, "write_atomic", side_effect=AssertionError("unexpected write")):
                self.assertFalse(setup.enable(path)[0])
            self.assertEqual(path.read_bytes(), saved)

    def test_a_fresh_runner_gets_a_module_list(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "settings/enabledModules.plist"
            self.assertEqual(setup.enable(path), (True, None))
            self.assertEqual(plistlib.loads(path.read_bytes()), list(setup.MODULES))

    def test_an_unknown_preference_format_is_left_untouched(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "enabledModules.plist"
            for invalid in ({"enabled": []}, [123], "not an array"):
                original = plistlib.dumps(invalid)
                path.write_bytes(original)
                with self.assertRaises(ValueError):
                    setup.enable(path)
                self.assertEqual(path.read_bytes(), original)

    def test_a_failed_service_restart_restores_the_original_preference(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "Library/Group Containers/group.com.apple.fskit.settings/enabledModules.plist"
            path.parent.mkdir(parents=True)
            original = plistlib.dumps(["com.apple.fskit.exfat"])
            path.write_bytes(original)
            restart = subprocess.CompletedProcess(["pkill"], 2)
            with (
                patch.dict(setup.os.environ, {
                    "GITHUB_ACTIONS": "true", "RUNNER_ENVIRONMENT": "github-hosted"
                }),
                patch.object(setup.sys, "platform", "darwin"),
                patch.object(setup.Path, "home", return_value=Path(directory)),
                patch.object(setup.subprocess, "run", return_value=restart),
            ):
                with self.assertRaises(subprocess.CalledProcessError):
                    setup.main()
            self.assertEqual(path.read_bytes(), original)

    def test_regular_machines_are_refused_before_preferences_or_services_are_changed(self):
        with patch.dict(setup.os.environ, {}, clear=True), patch.object(setup, "enable") as enable:
            with self.assertRaises(RuntimeError):
                setup.main()
            enable.assert_not_called()


if __name__ == "__main__":
    unittest.main()
