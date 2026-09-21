#!/usr/bin/env python3
"""Failure guards for the installation runner; no hypervisor or root required."""
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
from check_install import Machine, line_present, main


class InstallGuards(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)

    def machine(self, source):
        vm = Machine(sys.executable, self.directory, "fake", ["-c", source])
        self.addCleanup(vm.cleanup)
        return vm

    def test_echo_is_not_installer_success(self):
        self.assertFalse(line_present("# echo VMM_INSTALL_OK && poweroff\n", "VMM_INSTALL_OK"))
        self.assertFalse(line_present("VMM_INSTALL_OK_extra\n", "VMM_INSTALL_OK"))
        self.assertTrue(line_present("\r\nVMM_INSTALL_OK\r\n", "VMM_INSTALL_OK"))

    def test_zero_exit_without_poweroff_fails(self):
        vm = self.machine("pass")
        with self.assertRaisesRegex(RuntimeError, "clean guest poweroff"):
            vm.powered_off(time.monotonic() + 5)

    def test_poweroff_with_nonzero_exit_fails(self):
        vm = self.machine("print('Guest requested PSCI SYSTEM_OFF'); raise SystemExit(1)")
        with self.assertRaisesRegex(RuntimeError, "exit=1"):
            vm.powered_off(time.monotonic() + 5)

    def test_clean_poweroff(self):
        vm = self.machine("print('Guest requested PSCI SYSTEM_OFF')")
        vm.powered_off(time.monotonic() + 5)

    def test_phase_timeout_terminates_only_owned_process(self):
        vm = self.machine("import time; time.sleep(60)")
        with self.assertRaisesRegex(RuntimeError, "Timed out"):
            vm.wait_for(lambda: False, time.monotonic() + 0.1, "success")
        with self.assertRaises(subprocess.TimeoutExpired):
            vm.powered_off(time.monotonic() + 0.1)
        vm.cleanup()
        self.assertIsNotNone(vm.proc.poll())
        self.assertTrue(vm.path.exists())

    def test_unconsumed_mailbox_is_not_overwritten(self):
        vm = self.machine("import time; time.sleep(60)")
        vm.mailbox.write_text("previous\n")
        with self.assertRaisesRegex(RuntimeError, "UART mailbox"):
            vm.send("new", time.monotonic() + 0.1)
        self.assertEqual(vm.mailbox.read_text(), "previous\n")

    def test_early_exit_fails_wait(self):
        vm = self.machine("pass")
        vm.proc.wait(timeout=5)
        with self.assertRaisesRegex(RuntimeError, "VM exited"):
            vm.wait_for(lambda: False, time.monotonic() + 5, "success")

    def invoke_main(self, runner, *extra):
        iso = self.directory / "input.iso"
        iso.write_bytes(b"source media")
        argv = ["check_install.py", sys.executable, "--iso", str(iso),
                "--artifacts", str(self.directory), *extra]
        with patch.object(sys, "argv", argv), patch("check_install.run", side_effect=runner), \
                patch("builtins.print"):
            main()
        self.assertEqual(iso.read_bytes(), b"source media")

    def test_failure_retains_artifacts_and_unrelated_files(self):
        sibling = self.directory / "existing-user-disk.img"
        sibling.write_bytes(b"untouched")
        def fail(args, directory):
            (directory / "installed.img").write_bytes(b"diagnostic disk")
            (directory / "install.console.log").write_text("installer failed")
            raise RuntimeError("injected failure")
        with self.assertRaisesRegex(RuntimeError, "injected failure"):
            self.invoke_main(fail)
        runs = list(self.directory.glob("vmm-install-*"))
        self.assertEqual(len(runs), 1)
        self.assertEqual((runs[0] / "installed.img").read_bytes(), b"diagnostic disk")
        self.assertIn("injected failure", (runs[0] / "failure.txt").read_text())
        self.assertEqual(sibling.read_bytes(), b"untouched")

    def test_success_cleans_only_new_directory(self):
        sibling = self.directory / "existing-user-disk.img"
        sibling.write_bytes(b"untouched")
        self.invoke_main(lambda args, directory: None)
        self.assertEqual(list(self.directory.glob("vmm-install-*")), [])
        self.assertEqual(sibling.read_bytes(), b"untouched")

    def test_keep_preserves_success_artifacts(self):
        self.invoke_main(lambda args, directory: None, "--keep")
        self.assertEqual(len(list(self.directory.glob("vmm-install-*/run.json"))), 1)


if __name__ == "__main__":
    unittest.main()
