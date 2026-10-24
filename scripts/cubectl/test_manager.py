"""Native manager contracts using a fake guest; no hypervisor or media."""
import json
import os
from pathlib import Path
import socket
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from support.cubectl import Cubectl, binary_path


class ManagerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="cube-cli-test-", dir="/tmp")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        self.state = self.root / ("long-directory-" * 8) / "state"
        self.fake = self.root / "fake-cube"
        self.fake.write_text(f"#!{sys.executable}\nimport time\nwhile True: time.sleep(0.1)\n")
        self.fake.chmod(0o755)
        (self.root / "firmware").write_bytes(b"firmware")
        # Isolate native control/resource locks from users and parallel fixtures.
        sockets = self.root / "sockets"
        sockets.mkdir(mode=0o700)
        self.ctl = Cubectl(binary_path(), self.state,
                          dict(os.environ, CUBECTL_SOCKET_DIR=str(sockets)))
        self.names = []
        self.addCleanup(self.stop_all)

    def stop_all(self):
        for name in self.names:
            if self.ctl.running(name):
                self.ctl.call("stop", name, "--timeout", 1)

    def config(self, name, **overrides):
        disk = self.root / (name + ".img")
        disk.write_bytes(b"disk")
        value = {"name": name, "binary": str(self.fake),
                 "boot": {"mode": "uefi", "firmware": "firmware"},
                 "disks": [disk.name], "network": {"mode": "none"}}
        value.update(overrides)
        path = self.root / (name + ".json")
        path.write_text(json.dumps(value))
        self.names.append(name)
        return path

    def test_overrides_relative_paths_shares_and_long_state_path(self):
        folder = self.root / "export with spaces,comma"
        folder.mkdir()
        config = self.config("first", shares=[{"tag": "work", "path": folder.name}])
        status = self.ctl.call("start", config, "--ram", 4096, "--cpus", 4)
        spec = json.loads((self.state / "first/launch.json").read_text())
        command = spec["command"]
        self.assertEqual(command[command.index("--ram") + 1], "4096")
        self.assertEqual(command[command.index("--cpus") + 1], "4")
        self.assertIn(str(self.root / "first.img"), command)
        self.assertIn(str(folder), command)
        self.assertLess(len(status["control_socket"].encode()), 104)
        self.assertEqual(status["shares"][0]["read_only"], True)

    def test_duplicate_shares_are_rejected(self):
        config = self.config("duplicate", shares=[{"tag": "work", "path": "."}] * 2)
        result = self.ctl.run("start", config)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("duplicate", result.stderr.lower())

    def test_native_gui_override_replaces_saved_vnc(self):
        config = self.config("native", vnc={"bind": "127.0.0.1:5999", "display": "1440x900"})
        status = self.ctl.call("start", config, "--gui", "native")
        spec = json.loads((self.state / "native/launch.json").read_text())
        command = spec["command"]
        self.assertEqual(command[command.index("--gui") + 1], "native")
        self.assertEqual(command[command.index("--display") + 1], "1440x900")
        self.assertNotIn("--vnc", command)
        self.assertIsNone(status["vnc"])
        self.assertNotIn("listen-port:5999", status["resources"])

    def test_instances_conflicts_and_stop_are_independent(self):
        first = self.config("first")
        second = self.config("second")
        one = self.ctl.call("start", first)
        two = self.ctl.call("start", second)
        self.assertNotEqual(one["id"], two["id"])
        self.assertNotEqual(one["pid"], two["pid"])
        collision = self.config("collision", disks=["first.img"])
        self.assertNotEqual(self.ctl.run("start", collision).returncode, 0)
        self.assertNotEqual(self.ctl.run("start", first).returncode, 0)
        stopped = self.ctl.call("stop", "first", "--timeout", 1)
        self.assertEqual(stopped["result"], "forced-stop")
        self.assertTrue(self.ctl.running("second"))

    def test_shutdown_without_transport_does_not_force_stop(self):
        self.ctl.call("start", self.config("first"))
        result = self.ctl.run("shutdown", "first", "--timeout", 1)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no guest SSH shutdown", result.stdout)
        self.assertTrue(self.ctl.running("first"))

    def test_malformed_and_disconnected_clients_leave_manager_available(self):
        status = self.ctl.call("start", self.config("first"))
        for request in (b'[]\n', b'not-json\n', b'{"action":"unknown"}\n'):
            with socket.socket(socket.AF_UNIX) as client:
                client.settimeout(3)
                client.connect(status["control_socket"])
                client.sendall(request)
                response = json.loads(client.recv(65536))
                self.assertFalse(response["ok"])
        with socket.socket(socket.AF_UNIX) as client:
            client.connect(status["control_socket"])
            client.sendall(b'{"action":"status"}\n')
        self.assertTrue(self.ctl.running("first"))

    def test_proxy_rejects_sudo_before_launch(self):
        config = self.config("proxy", network={"mode": "vmnet-proxy"})
        result = self.ctl.run("start", config, "--sudo")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("vmnet-proxy", result.stderr)


if __name__ == "__main__":
    unittest.main()
