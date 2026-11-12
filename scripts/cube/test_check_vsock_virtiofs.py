"""Unit tests for the host-side vsock/virtio-fs verifier helpers."""

import shlex
import socket
import subprocess
import sys
from pathlib import Path
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parent))

from check_vsock_virtiofs import (
    UnixEcho,
    VSOCK_REPLY,
    VSOCK_REQUEST,
    python_command,
    wait_for_root,
)


class VsockVirtioFsVerifierHelpers(unittest.TestCase):
    def test_unix_echo_models_vsock_map_endpoint(self):
        with tempfile.TemporaryDirectory() as temp:
            echo = UnixEcho(Path(temp) / "map.sock")
            try:
                echo.start()
            except RuntimeError as error:
                if "Operation not permitted" in str(error):
                    self.skipTest("sandbox prohibits Unix-domain socket creation")
                raise
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as stream:
                stream.connect(str(echo.path))
                stream.sendall(VSOCK_REQUEST)
                self.assertEqual(stream.recv(4096), VSOCK_REPLY)
            echo.check()

    def test_python_command_preserves_multiline_source(self):
        command = python_command("print('round-trip')\n")
        result = subprocess.run(
            ["sh", "-c", command], capture_output=True, text=True, check=True
        )
        self.assertEqual(result.stdout, "round-trip\n")
        self.assertNotIn("'round-trip'", shlex.split(command)[2])

    def test_password_prompt_is_answered_before_waiting_for_root(self):
        class Proc:
            @staticmethod
            def poll():
                return None

        class Guest:
            proc = Proc()

            def __init__(self):
                self.reads = 0

            def text(self):
                self.reads += 1
                return "Password:" if self.reads < 3 else "root@localhost:~#"

        with tempfile.TemporaryDirectory() as temp:
            mailbox = Path(temp) / "uart"
            wait_for_root(Guest(), mailbox, "root@", timeout=2)
            self.assertEqual(mailbox.read_text(), "root\n")


if __name__ == "__main__":
    unittest.main()
