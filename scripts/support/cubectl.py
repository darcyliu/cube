"""Invoke the public native CLI with an isolated state root."""
import json
import os
from pathlib import Path
import subprocess
import signal

REPO = Path(__file__).resolve().parents[2]


def binary_path():
    return Path(os.environ.get("CUBECTL_BIN", REPO / "target/debug/cubectl")).resolve()


class Cubectl:
    def __init__(self, binary, state_root, env=None):
        self.binary = Path(binary).resolve()
        if not self.binary.is_file():
            raise RuntimeError(f"cubectl is missing: {self.binary}; run cargo build --bin cubectl")
        self.state_root = Path(state_root).resolve()
        self.env = env

    def run(self, *args, timeout=40):
        command = [str(self.binary), "--state-root", str(self.state_root), *map(str, args)]
        proc = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                text=True, env=self.env, start_new_session=True)
        try:
            stdout, stderr = proc.communicate(timeout=timeout)
            return subprocess.CompletedProcess(command, proc.returncode, stdout, stderr)
        finally:
            # Native installation launches a guest in this process group. Stop
            # both on timeout/cancellation, including if the CLI already exited.
            try:
                os.killpg(proc.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                os.killpg(proc.pid, signal.SIGKILL)
                proc.wait(timeout=5)

    def call(self, *args, timeout=40):
        result = self.run(*args, timeout=timeout)
        if result.returncode:
            raise RuntimeError(result.stderr.strip() or result.stdout.strip())
        return json.loads(result.stdout)

    def running(self, target):
        return self.call("status", target).get("state") == "running"
