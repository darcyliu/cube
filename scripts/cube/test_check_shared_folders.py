"""Export fixtures must be reachable after the VMM drops sudo privileges."""
import os
from pathlib import Path
import shutil
import tempfile
import unittest
from unittest.mock import patch

import sys
sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_shared_folders as check


class ExportFixtureTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.artifacts = Path(self.temp.name) / "private-artifacts"
        self.artifacts.mkdir(mode=0o700)

    def test_regular_user_export_stays_in_artifacts(self):
        with patch.object(check.os, "geteuid", return_value=501):
            export = check.prepare_export(self.artifacts)
        self.assertEqual(export, self.artifacts / "export")
        self.assertEqual((export / "host.txt").read_text(), "host-original\n")
        self.assertEqual(os.readlink(export / "outside-link"), "/etc/passwd")
        self.assertEqual((self.artifacts / "export-path.txt").read_text().strip(), str(export))

    def test_sudo_export_bypasses_private_artifact_ancestors(self):
        # Simulate a root-owned TMPDIR. Mock chown only; verify the actual path,
        # permissions and exactly which objects would change ownership.
        with patch.dict(os.environ, SUDO_UID="501", SUDO_GID="20", TMPDIR=str(self.artifacts)), \
                patch.object(check.os, "geteuid", return_value=0), \
                patch.object(check.os, "chown") as chown:
            export = check.prepare_export(self.artifacts)
        self.addCleanup(shutil.rmtree, export)
        self.assertEqual(export.parent, Path("/private/tmp"))
        self.assertEqual(export.stat().st_mode & 0o777, 0o700)
        self.assertEqual(self.artifacts.stat().st_mode & 0o777, 0o700)
        self.assertEqual({call.args[0] for call in chown.call_args_list},
                         {export, export / "host.txt", export / "outside-link"})
        for call in chown.call_args_list:
            self.assertEqual(call.args[1:], (501, 20))
            self.assertFalse(call.kwargs["follow_symlinks"])

    def test_root_without_valid_sudo_user_fails_before_creating_export(self):
        for identity in ({}, {"SUDO_UID": "0", "SUDO_GID": "20"},
                         {"SUDO_UID": "501", "SUDO_GID": "-1"}):
            with self.subTest(identity=identity), patch.dict(os.environ, identity, clear=True), \
                    patch.object(check.os, "geteuid", return_value=0), \
                    patch.object(check.tempfile, "mkdtemp") as mkdir:
                with self.assertRaisesRegex(RuntimeError, "sudo from a non-root user"):
                    check.prepare_export(self.artifacts)
                mkdir.assert_not_called()


if __name__ == "__main__":
    unittest.main()
