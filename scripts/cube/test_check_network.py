#!/usr/bin/env python3
"""The live guest keeps its network mode after the startup probes."""
import contextlib
import io
import subprocess
import sys
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import check_network


class GuestModeTests(unittest.TestCase):
    def launch(self, extra, euid):
        launched = {}
        probes = []

        def run(command, **kwargs):
            if command[0] == "ssh-keygen":
                key = Path(command[command.index("-f") + 1])
                key.write_text("priv")
                key.with_suffix(".pub").write_text("ssh-ed25519 AAAA test\n")
                return subprocess.CompletedProcess(command, 0)
            probes.append(" ".join(map(str, command)))
            if "invalid-mode" in probes[-1]:
                stderr = "Unknown network mode"
            elif "missing-tap" in probes[-1]:
                stderr = "Cannot start tap backend"
            else:
                stderr = "Cannot bind TCP forward"
            return subprocess.CompletedProcess(command, 1, "", stderr)

        class FakeVM:
            def __init__(self, binary, directory, name, args, env=None):
                launched["args"] = list(map(str, args))
                self.proc = type("Proc", (), {"poll": lambda self: None})()

            def wait_for(self, text, timeout=None):
                raise AssertionError("stop")

            def text(self):
                return ""

            def cleanup(self):
                pass

        argv = ["check_network.py", "/usr/bin/true",
                "--kernel", "kernel", "--iso", "disc.iso",
                "--guest-cidr", "192.168.2.77/24", "--host-ip", "192.168.2.1",
                *extra]
        with patch.object(sys, "argv", argv), \
                patch.object(check_network, "kernel", return_value=Path("kernel.elf")), \
                patch.object(check_network.subprocess, "run", side_effect=run), \
                patch.object(check_network, "VM", FakeVM), \
                patch.object(check_network.os, "geteuid", return_value=euid), \
                patch.object(check_network.os, "execvp") as execvp, \
                self.assertRaises(AssertionError) as caught, \
                contextlib.redirect_stdout(io.StringIO()):
            check_network.main()
        self.assertEqual(str(caught.exception), "stop")
        execvp.assert_not_called()
        self.assertTrue(any("missing-tap" in command for command in probes))
        return launched["args"]

    def test_proxy_guest_is_not_started_on_the_missing_tap(self):
        args = self.launch([], 501)
        self.assertEqual(args[args.index("--net") + 1], "vmnet-proxy")
        self.assertIn("--ssh-port", args)
        self.assertNotIn("--forward", args)

    def test_sudo_guest_stays_on_in_process_vmnet(self):
        args = self.launch(["--sudo"], 0)
        self.assertEqual(args[args.index("--net") + 1], "vmnet")
        self.assertIn("--ssh-port", args)
        self.assertNotIn("--forward", args)

    def test_bridge_guest_keeps_static_forwarding(self):
        args = self.launch(["--net", "bridge:en0"], 0)
        self.assertEqual(args[args.index("--net") + 1], "bridge:en0")
        self.assertIn("--forward", args)
        self.assertNotIn("--ssh-port", args)


if __name__ == "__main__":
    unittest.main()
