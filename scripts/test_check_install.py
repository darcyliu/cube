#!/usr/bin/env python3
"""Failure guards for the installation runner; no hypervisor or root required."""
from pathlib import Path
import json
import os
import subprocess
import sys
import tempfile
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
from check_install import (ConsoleSession, Machine, desktop_packages, password_from_file,
                           desktop_session, find_usb_tablet, line_present, main)
import vmmctl


class InstallGuards(unittest.TestCase):
    def test_tablet_detection_uses_descriptors_not_database_names_or_addresses(self):
        listing = """ugen2.1: <XHCI root HUB Generic> at usbus2
ugen2.8: <QEMU Tablet Adomax Technology Co., Ltd> at usbus2
ugen2.4: <QEMU Tablet Adomax Technology Co., Ltd> at usbus2
"""
        descriptors = {
            "ugen2.1": "  iProduct = 0x0002 <XHCI root HUB>\n",
            "ugen2.8": "  iProduct = 0x0003 <VMM USB Keyboard>\n"
                       "  iSerialNumber = 0x0004 <kbd0>\n",
            "ugen2.4": "  iProduct = 0x0002 <VMM USB Tablet>\n"
                       "  iSerialNumber = 0x0005 <tablet0>\n",
        }
        def execute(command, deadline):
            self.assertEqual(deadline, 123)
            return subprocess.CompletedProcess(command, 0, descriptors[command.split()[2]], "")
        self.assertEqual(find_usb_tablet(execute, listing, 123), "ugen2.4")
        descriptors["ugen2.4"] = descriptors["ugen2.8"]
        with self.assertRaisesRegex(RuntimeError, "Expected one VMM USB tablet"):
            find_usb_tablet(execute, listing, 123)

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

    def test_desktop_rejects_offline_console_before_allocating_disk(self):
        with self.assertRaises(SystemExit) as error:
            self.invoke_main(lambda args, directory: self.fail("must not run"),
                             "--desktop", "--console-only")
        self.assertEqual(error.exception.code, 2)
        self.assertEqual(list(self.directory.glob("vmm-install-*")), [])

    def test_desktop_options_reach_the_disposable_runner(self):
        captured = {}
        def record(args, directory):
            captured.update(desktop=args.desktop, display=args.display,
                            vnc_port=args.vnc_port, timeout=args.desktop_timeout)
        self.invoke_main(record, "--desktop", "--display", "1280x720",
                         "--vnc-port", "5905")
        self.assertEqual(captured, {"desktop": True, "display": "1280x720",
                                    "vnc_port": 5905, "timeout": 1200})

    def test_xfce_variant_implies_desktop_and_has_a_boot_login_manager(self):
        captured = {}
        def record(args, directory):
            captured.update(desktop=args.desktop, environment=args.desktop_environment)
        self.invoke_main(record, "--desktop-environment", "xfce")
        self.assertEqual(captured, {"desktop": True, "environment": "xfce"})
        self.assertIn("xfce dbus lightdm lightdm-gtk-greeter dejavu adwaita-icon-theme",
                      desktop_packages("xfce"))
        self.assertIn("xfce4/xinitrc", desktop_session("xfce")[0])
        with self.assertRaises(SystemExit):
            self.invoke_main(lambda args, directory: self.fail("must not run"),
                             "--desktop-environment", "xfce", "--console-only")

    def test_xfce_login_can_use_custom_user_and_password_file(self):
        password_file = self.directory / "desktop-password"
        password_file.write_text("custom pass with spaces!\n")
        captured = {}
        def record(args, directory):
            captured.update(user=args.desktop_user,
                            password=password_from_file(args.desktop_password_file,
                                                        "fallback", "Desktop"))
        self.invoke_main(record, "--desktop-environment", "xfce",
                         "--desktop-user", "darcy", "--desktop-password-file", str(password_file),
                         "--keep")
        self.assertEqual(captured, {"user": "darcy", "password": "custom pass with spaces!"})
        run = next(self.directory.glob("vmm-install-*"))
        self.assertNotIn("custom pass with spaces!", (run / "run.json").read_text())
        with self.assertRaises(SystemExit):
            self.invoke_main(lambda args, directory: self.fail("must not run"),
                             "--desktop-environment", "xfce", "--desktop-user", "root")
        with self.assertRaises(SystemExit):
            self.invoke_main(lambda args, directory: self.fail("must not run"),
                             "--desktop-user", "darcy")
        password_file.write_text("two\nlines\n")
        with self.assertRaises(SystemExit):
            self.invoke_main(lambda args, directory: self.fail("must not run"),
                             "--desktop-environment", "xfce",
                             "--desktop-password-file", str(password_file))

    def test_root_password_file_is_independent_of_desktop_password(self):
        root_file = self.directory / "root-password"
        root_file.write_text("root secret\n")
        desktop_file = self.directory / "desktop-password"
        desktop_file.write_text("desktop secret\n")
        captured = {}
        def record(args, directory):
            captured["root"] = password_from_file(args.root_password_file, "fallback", "Root")
            captured["desktop"] = password_from_file(args.desktop_password_file,
                                                      "fallback", "Desktop")
        self.invoke_main(record, "--desktop-environment", "xfce",
                         "--root-password-file", str(root_file),
                         "--desktop-password-file", str(desktop_file), "--keep")
        self.assertEqual(captured, {"root": "root secret", "desktop": "desktop secret"})
        run = next(self.directory.glob("vmm-install-*"))
        self.assertNotIn("root secret", (run / "run.json").read_text())
        self.assertNotIn("desktop secret", (run / "run.json").read_text())
        root_file.write_text("two\nlines\n")
        with self.assertRaises(SystemExit):
            self.invoke_main(lambda args, directory: self.fail("must not run"),
                             "--root-password-file", str(root_file))

    def test_root_password_helper_uses_key_and_private_credential_file(self):
        install = self.directory / "retained"
        install.mkdir()
        (install / "id_ed25519").write_text("test key")
        (install / "desktop-login.txt").write_text("Username: darcy\nPassword: test secret\n")
        tools = self.directory / "tools"
        tools.mkdir()
        sudo = tools / "sudo"
        sudo.write_text('#!/bin/sh\n[ "$1" = -v ] && exit 0\nexec "$@"\n')
        sudo.chmod(0o755)
        ssh = tools / "ssh"
        ssh.write_text('#!/bin/sh\ncat > "$CAPTURE"\n')
        ssh.chmod(0o755)
        capture = self.directory / "ssh-input"
        subprocess.run(["bash", str(Path(__file__).with_name("set_freebsd_root_password.sh")),
                        str(install)], check=True, capture_output=True, text=True,
                       env=dict(os.environ, PATH=str(tools) + os.pathsep + os.environ["PATH"],
                                CAPTURE=str(capture)), timeout=10)
        self.assertEqual(capture.read_text(), "test secret\n")
        login = install / "root-login.txt"
        self.assertEqual(login.read_text(), "Username: root\nPassword: test secret\n")
        self.assertEqual(login.stat().st_mode & 0o777, 0o600)

    def test_retained_xfce_install_has_reusable_vm_config(self):
        def installed(args, directory):
            (directory / "installed.img").write_bytes(b"disk")
            kernel = directory / "boot/kernel/kernel"
            kernel.parent.mkdir(parents=True)
            kernel.write_bytes(b"kernel")
            (directory / "id_ed25519").write_text("key")
        self.invoke_main(installed, "--desktop-environment", "xfce", "--keep")
        run = next(self.directory.glob("vmm-install-*"))
        config = json.loads((run / "reuse-vm.json").read_text())
        self.assertEqual(config["boot"], {"mode": "freebsd", "kernel": "boot/kernel/kernel",
                                          "multiuser": True})
        self.assertEqual(config["disks"], [{"path": "installed.img"}])
        self.assertEqual(config["vnc"]["bind"], "127.0.0.1:5901")
        self.assertTrue((run / config["boot"]["kernel"]).is_file())
        self.assertTrue((run / config["disks"][0]["path"]).is_file())
        options = SimpleNamespace(binary=None, ram=None, cpus=None, disk=None, net=None,
                                  mac=None, ssh_port=None, forward=None, vnc=None,
                                  display=None, efi_vars=None, sudo=False)
        spec = vmmctl.launch_spec(run / "reuse-vm.json", run / "state", options)
        self.assertIn(str((run / "installed.img").resolve()), spec["command"])
        self.assertIn(str((run / "boot/kernel/kernel").resolve()), spec["command"])
        self.assertEqual(spec["shutdown"]["identity_file"], str((run / "id_ed25519").resolve()))

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
