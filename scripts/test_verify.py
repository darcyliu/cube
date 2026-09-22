#!/usr/bin/env python3
"""Runner selection, failure reporting, and cancellation without starting a VM."""
import contextlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

sys.dont_write_bytecode = True
import verify


class VerifyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)

    def options(self, suite="quick", *extra):
        argv = ["--suite", suite, "--artifacts", str(self.directory)]
        if suite != "quick":
            argv += ["--iso", "test.iso", "--firmware", "test.fd"]
        if suite == "full":
            argv += ["--guest-cidr", "192.168.2.77/24", "--host-ip", "192.168.2.1"]
        return verify.parse_args([*argv, *extra])

    def test_full_includes_every_existing_check_and_both_install_modes(self):
        steps = verify.make_plan(self.options("full", "--sudo"), self.directory)
        names = {s.name for s in steps}
        self.assertTrue({"rust-debug", "rust-release", "python-tests", "lifecycle",
                         "network-errors", "installer-console", "network-guest-ssh",
                         "install-ufs-offline", "install-zfs-offline",
                         "install-ufs-ssh", "install-zfs-ssh"} <= names)
        seen = set()
        for step in steps:
            self.assertTrue(set(step.needs) <= seen)
            seen.add(step.name)
        for step in steps:
            if step.privileged:
                self.assertIn("sudo-access", step.needs)
                self.assertNotIn("--console-only", step.command)

    def test_quick_needs_no_media_or_privilege(self):
        steps = verify.make_plan(self.options(), self.directory)
        self.assertFalse(any(s.privileged for s in steps))
        self.assertFalse(any(s.name.startswith("install-") for s in steps))
        self.assertNotIn("--firmware", next(s.command for s in steps if s.name == "lifecycle"))

    def test_offline_checks_kernel_firmware_and_both_filesystems(self):
        steps = verify.make_plan(self.options("offline"), self.directory)
        self.assertFalse(any(s.privileged for s in steps))
        lifecycle = next(s for s in steps if s.name == "lifecycle")
        self.assertIn("--kernel", lifecycle.command)
        self.assertIn("--firmware", lifecycle.command)
        self.assertEqual(sum("--console-only" in s.command for s in steps), 2)

    def test_existing_binary_is_not_rebuilt_or_resigned(self):
        steps = verify.make_plan(self.options("quick", "--binary", sys.executable), self.directory)
        self.assertNotIn("build", [s.name for s in steps])
        self.assertIn("--verify", next(s.command for s in steps if s.name == "signed-binary"))

    def test_missing_full_inputs_fail_without_creating_resources(self):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            verify.parse_args(["--suite", "full"])
        self.assertEqual(list(self.directory.iterdir()), [])

    def test_failed_build_skips_vm_checks_but_keeps_independent_results(self):
        steps = [verify.Step("build", ["fake"]),
                 verify.Step("vm", ["fake"], needs=("build",)),
                 verify.Step("independent", ["fake"])]
        calls = []
        def run(step, args, directory):
            calls.append(step.name)
            step.status = "fail" if step.name == "build" else "pass"
            step.message = "injected"
        with patch.object(verify, "preflight"), patch.object(verify, "make_plan", return_value=steps), \
                patch.object(verify, "run_step", side_effect=run), contextlib.redirect_stdout(io.StringIO()):
            result = verify.main(["--artifacts", str(self.directory)])
        self.assertEqual(result, 1)
        self.assertEqual(calls, ["build", "independent"])
        run_dir = next(self.directory.iterdir())
        summary = json.loads((run_dir / "summary.json").read_text())
        self.assertEqual([s["status"] for s in summary["steps"]], ["pass", "fail", "skip", "pass"])
        root = ET.parse(run_dir / "junit.xml").getroot()
        self.assertEqual(root.attrib["failures"], "1")
        self.assertEqual(root.attrib["skipped"], "1")

    def test_preflight_failure_is_not_a_successful_empty_suite(self):
        with patch.object(verify, "preflight", side_effect=RuntimeError("no hypervisor")), \
                patch.object(verify, "run_step") as runner, contextlib.redirect_stdout(io.StringIO()):
            code = verify.main(["--artifacts", str(self.directory)])
        self.assertEqual(code, 1)
        runner.assert_not_called()
        root = ET.parse(next(self.directory.iterdir()) / "junit.xml").getroot()
        self.assertEqual(root.attrib["failures"], "1")

    def test_timeout_gives_child_a_chance_to_clean_up(self):
        marker = self.directory / "cleaned"
        ready = self.directory / "ready"
        source = ("from pathlib import Path; import time\n"
                  f"Path({str(ready)!r}).touch()\n"
                  "try: time.sleep(60)\n"
                  f"finally: Path({str(marker)!r}).touch()\n")
        step = verify.Step("timeout", [sys.executable, "-c", source], timeout=1)
        with contextlib.redirect_stdout(io.StringIO()):
            verify.run_step(step, self.options(), self.directory)
        self.assertEqual(step.status, "fail")
        self.assertTrue(ready.exists())
        self.assertTrue(marker.exists())
        self.assertIn("timed out", step.message)

    def test_signal_wrapper_runs_script_cleanup(self):
        script = self.directory / "term.py"
        marker = self.directory / "signal-cleaned"
        script.write_text("import os,signal\nfrom pathlib import Path\n"
                          "try: os.kill(os.getpid(), signal.SIGTERM)\n"
                          f"finally: Path({str(marker)!r}).touch()\n")
        result = subprocess.run([sys.executable, "-c", verify.PYTHON_CHECK, str(script)],
                                capture_output=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(marker.exists())

    def test_wrapper_preserves_sibling_imports_and_arguments(self):
        (self.directory / "sibling.py").write_text("VALUE = 'imported'\n")
        script = self.directory / "check.py"
        script.write_text("import sibling,sys\nprint(sibling.VALUE, sys.argv[1])\n")
        result = subprocess.run([sys.executable, "-c", verify.PYTHON_CHECK, str(script), "argument"],
                                capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "imported argument\n")


if __name__ == "__main__":
    unittest.main()
