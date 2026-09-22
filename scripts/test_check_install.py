#!/usr/bin/env python3
"""Failure guards for the installation runner; no hypervisor or root required."""
from pathlib import Path
import os
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
from check_install import ConsoleSession, Machine, line_present, main


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

    def test_console_preserves_output_and_command_status(self):
        class Terminal:
            output = ""
            def text(self, console=False):
                return self.output
            def send(self, command, deadline):
                result = subprocess.run(command, shell=True, capture_output=True, text=True, timeout=5)
                # Include the echoed command: it must not be mistaken for output.
                self.output += command + "\n" + result.stdout + result.stderr
            def wait_for(self, predicate, deadline, description):
                if not predicate():
                    raise RuntimeError(description)
        terminal = Terminal()
        console = ConsoleSession(terminal, "unused")
        console.logged_in = True
        for command, expected, status in [("printf exact", "exact", 0),
                                           ("printf 'two\\nlines\\n'; exit 7", "two\nlines\n", 7)]:
            result = console.execute(command, time.monotonic() + 5)
            self.assertEqual(result.stdout, expected)
            self.assertEqual(result.returncode, status)

    def test_zfs_requires_firmware_before_allocating_disk(self):
        with self.assertRaises(SystemExit) as error:
            self.invoke_main(lambda args, directory: self.fail("must not run"), "--filesystem", "zfs")
        self.assertEqual(error.exception.code, 2)
        self.assertEqual(list(self.directory.glob("vmm-install-*")), [])

    def test_seed_selects_ufs_or_efi_zfs(self):
        # Capture the real seed contents before hdiutil and cleanup, without
        # building an ISO or referring to user SSH keys.
        capture = self.directory / "config"
        stub = self.directory / "hdiutil"
        stub.write_text('#!/bin/bash\nfor last; do :; done\ncp "$last/installerconfig" "$CAPTURE"\n')
        stub.chmod(0o755)
        for filesystem in ("ufs", "zfs"):
            subprocess.run(["bash", str(Path(__file__).with_name("create-seed.sh").resolve()),
                "--filesystem", filesystem, "--ssh-key", "ssh-ed25519 test-key",
                "--password", "test-password"], cwd=self.directory,
                env=dict(os.environ, PATH=str(self.directory) + os.pathsep + os.environ["PATH"],
                         CAPTURE=str(capture)), check=True, capture_output=True, timeout=10)
            config = capture.read_text()
            self.assertIn('export nonInteractive="YES"', config)
            if filesystem == "ufs":
                self.assertIn('PARTITIONS="vtbd0"', config)
                self.assertNotIn("ZFSBOOT_DISKS", config)
            else:
                self.assertNotIn("PARTITIONS=", config)
                self.assertIn('export ZFSBOOT_DISKS="vtbd0"', config)
                self.assertIn('export ZFSBOOT_BOOT_TYPE="UEFI"', config)
                self.assertIn('sysrc zfs_enable="YES"', config)
                self.assertIn('zfs_load="YES"', config)


if __name__ == "__main__":
    unittest.main()
