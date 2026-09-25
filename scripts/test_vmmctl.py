#!/usr/bin/env python3
"""Configuration and concurrent-instance checks without launching Hypervisor.framework."""
import argparse
import contextlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
import vmmctl


class ManagerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.state = self.base / ("long-directory-" * 8) / "state"
        self.binary = self.base / "fake-vmm"
        self.binary.write_text("#!/usr/bin/env python3\nimport time\nwhile True: time.sleep(0.1)\n")
        self.binary.chmod(0o755)
        (self.base / "firmware.fd").write_bytes(b"firmware")
        for name in ("first", "second", "third"):
            (self.base / (name + ".img")).write_bytes(b"disk")
        self.running = []
        self.addCleanup(self.stop_all)

    def config(self, name, port, disk=None):
        path = self.base / (name + ".json")
        path.write_text(json.dumps({
            "name": name, "binary": "fake-vmm",
            "boot": {"mode": "uefi", "firmware": "firmware.fd"},
            "disks": [{"path": disk or name + ".img"}],
            "ram_mb": 2048, "cpus": 2, "network": {"mode": "mock"},
            "vnc": {"bind": "127.0.0.1:" + str(port), "display": "1024x768"},
        }))
        return path

    def options(self):
        return argparse.Namespace(binary=None, ram=None, cpus=None, net=None, mac=None,
                                  ssh_port=None, forward=None, vnc=None, display=None,
                                  efi_vars=None, disk=None, sudo=False)

    def start(self, config):
        directory = self.state / vmmctl.read_config(config)["name"]
        result = vmmctl.start(config, self.state, directory, self.options())
        self.running.append(directory)
        return result

    def stop_all(self):
        for directory in self.running:
            if vmmctl.live_status(directory):
                vmmctl.query(directory, {"action": "stop", "timeout": 2}, timeout=5)

    def test_shared_folder_paths_modes_overrides_and_duplicates(self):
        path = self.config("first", 5901)
        folder = self.base / "export with spaces,comma"
        folder.mkdir()
        config = json.loads(path.read_text())
        config["shares"] = [{"tag": "work", "path": folder.name}]
        path.write_text(json.dumps(config))
        args = self.options()
        spec = vmmctl.launch_spec(path, self.state / "first", args)
        self.assertEqual(spec["shares"], [{"tag": "work", "path": str(folder.resolve()), "read_only": True}])
        self.assertIn("--share", spec["command"])
        self.assertNotIn("disk:" + str(folder), spec["resources"])
        args.share_rw = [["write", str(folder)]]
        spec = vmmctl.launch_spec(path, self.state / "first", args)
        self.assertEqual([s["tag"] for s in spec["shares"]], ["write"])
        self.assertIn("--share-rw", spec["command"])
        args.share = [["write", str(folder)]]
        with self.assertRaisesRegex(ValueError, "duplicate share"):
            vmmctl.launch_spec(path, self.state / "first", args)

    def test_config_paths_and_command_line_overrides(self):
        config = self.config("first", 5901)
        options = self.options()
        options.ram = 4096
        options.cpus = 4
        options.net = "none"
        options.vnc = "127.0.0.1:5999"
        spec = vmmctl.launch_spec(config, self.state / "first", options)
        command = spec["command"]
        self.assertEqual(command[command.index("--ram") + 1], "4096")
        self.assertEqual(command[command.index("--cpus") + 1], "4")
        self.assertEqual(command[command.index("--net") + 1], "none")
        self.assertEqual(command[command.index("--vnc") + 1], "127.0.0.1:5999")
        self.assertEqual(command[command.index("--disk") + 1],
                         str((self.base / "first.img").resolve()))
        self.assertEqual(command[command.index("--efi-vars") + 1],
                         str(self.state / "first" / "efi-vars.fd"))
        self.assertLess(len(str(vmmctl.control_socket_path(self.state / "first"))), 100)

    def test_sudo_elevates_before_detaching_manager(self):
        command = ["start", "guest.json", "--sudo", "--state-root", str(self.state)]
        with patch.object(vmmctl.os, "geteuid", return_value=501), \
             patch.object(vmmctl.os, "execvp", side_effect=RuntimeError("reexec")) as execvp:
            with self.assertRaisesRegex(RuntimeError, "reexec"):
                vmmctl.main(command)
        program, argv = execvp.call_args.args
        self.assertEqual(program, "sudo")
        self.assertEqual(argv[:5], ["sudo", "-n", sys.executable, "-B", vmmctl.__file__])
        self.assertNotIn("--sudo", argv)
        self.assertEqual(argv[-2:], ["--state-root", str(self.state)])

    def test_two_instances_have_distinct_identity_runtime_and_listeners(self):
        first = self.start(self.config("first", 5901))
        second = self.start(self.config("second", 5902))
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            self.assertEqual(vmmctl.main(["status", "first", "--state-root", str(self.state)]), 0)
        self.assertEqual(json.loads(output.getvalue())["id"], first["id"])
        self.assertNotEqual(first["id"], second["id"])
        self.assertNotEqual(first["pid"], second["pid"])
        self.assertTrue(vmmctl.control_socket_path(self.state / "first").exists())
        self.assertTrue(vmmctl.control_socket_path(self.state / "second").exists())
        one = json.loads((self.state / "first/launch.json").read_text())
        two = json.loads((self.state / "second/launch.json").read_text())
        for flag in ("--serial-log", "--efi-vars"):
            self.assertNotEqual(one["command"][one["command"].index(flag) + 1],
                                two["command"][two["command"].index(flag) + 1])
        self.assertNotEqual(one["resources"], two["resources"])
        with self.assertRaisesRegex(ValueError, "resource already used"):
            conflict = self.config("third", 5901)
            data = json.loads(conflict.read_text())
            data["vnc"]["bind"] = "127.0.0.2:5901"
            conflict.write_text(json.dumps(data))
            self.start(conflict)
        with self.assertRaisesRegex(ValueError, "resource already used"):
            self.start(self.config("third", 5903, disk="first.img"))
        with self.assertRaisesRegex(ValueError, "already running"):
            self.start(self.base / "first.json")
        stopped = vmmctl.query(self.state / "first", {"action": "stop", "timeout": 2}, timeout=5)
        self.assertEqual(stopped["result"], "forced-stop")
        self.assertEqual(stopped["signal"], "SIGTERM")
        deadline = time.monotonic() + 5
        while vmmctl.live_status(self.state / "first") and time.monotonic() < deadline:
            time.sleep(0.05)
        self.assertIsNone(vmmctl.live_status(self.state / "first"))
        self.assertEqual(json.loads((self.state / "first/last.json").read_text())["state"],
                         "stopped")
        self.assertIsNotNone(vmmctl.live_status(self.state / "second"))

    def test_shutdown_requires_guest_transport_and_never_forces_implicitly(self):
        self.start(self.config("first", 5901))
        response = vmmctl.query(self.state / "first",
                                {"action": "shutdown", "timeout": 1, "force_after": False})
        self.assertFalse(response["ok"])
        self.assertIn("no guest SSH shutdown configured", response["error"])
        self.assertIsNotNone(vmmctl.live_status(self.state / "first"))

    def test_shutdown_requires_psci_poweroff_and_timeout_is_explicit(self):
        shutdown = {"host": "127.0.0.1", "port": 2222, "user": "root",
                    "identity_file": str(self.base / "key")}
        spec = {"shutdown": shutdown, "sudo": False}
        directory = self.base / "guest"
        directory.mkdir()

        class FakeProc:
            def __init__(self, result):
                self.result = result

            def wait(self, timeout):
                if self.result == "timeout":
                    raise subprocess.TimeoutExpired("vmm", timeout)
                return self.result

        with patch.object(vmmctl.subprocess, "run", return_value=
                          subprocess.CompletedProcess([], 255, "", "connection closed")):
            (directory / "vmm.log").write_text("Guest requested PSCI SYSTEM_OFF\n")
            result = vmmctl.guest_shutdown(FakeProc(0), spec, directory, 5, False)
            self.assertEqual(result["result"], "guest-poweroff")
            (directory / "vmm.log").write_text("host stopped\n")
            self.assertFalse(vmmctl.guest_shutdown(FakeProc(0), spec, directory, 5, False)["ok"])
            with patch.object(vmmctl, "force_stop") as force:
                result = vmmctl.guest_shutdown(FakeProc("timeout"), spec, directory, 1, False)
                self.assertFalse(result["ok"])
                force.assert_not_called()
                result = vmmctl.guest_shutdown(FakeProc("timeout"), spec, directory, 1, True)
                self.assertEqual(result["result"], "forced-after-timeout")
                force.assert_called_once()


if __name__ == "__main__":
    unittest.main()
