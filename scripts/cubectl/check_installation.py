#!/usr/bin/env python3
"""Exercise native create → install → start → stop/shutdown with disposable media."""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import argparse
import json
import os
import shutil
import subprocess
import tempfile
import time

from support.cubectl import Cubectl
from support.guest import in_process_net


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--cubectl", required=True, type=Path)
    parser.add_argument("--iso", required=True, type=Path)
    parser.add_argument("--firmware", required=True, type=Path)
    parser.add_argument("--filesystem", choices=("ufs", "zfs"), required=True)
    parser.add_argument("--artifacts", type=Path)
    parser.add_argument("--net", default="none")
    parser.add_argument("--keep", action="store_true")
    args = parser.parse_args()
    for field in ("binary", "cubectl", "iso", "firmware"):
        path = getattr(args, field).resolve()
        if not path.is_file():
            parser.error(f"missing {field}: {path}")
        setattr(args, field, path)
    if in_process_net(args.net) and os.geteuid() != 0:
        os.execvp("sudo", ["sudo", "-n", sys.executable, "-B", __file__, *sys.argv[1:]])
    if args.net == "vmnet-proxy" and os.geteuid() == 0:
        parser.error("vmnet-proxy must run as the service's non-root user")
    directory = Path(tempfile.mkdtemp(prefix="cubectl-install-", dir=args.artifacts)).resolve()
    state_root = directory / "state"
    ctl = Cubectl(args.cubectl, state_root)
    name = "install-test"
    passed = False
    print(f"Artifacts: {directory}", flush=True)

    def run(*command, timeout=60):
        result = ctl.run(*command, timeout=timeout)
        with (directory / "commands.log").open("a") as log:
            log.write(f"$ cubectl {' '.join(map(str, command))}\n{result.stdout}{result.stderr}\n")
        if result.returncode:
            raise RuntimeError(f"cubectl {command[0]} failed; see {directory / 'commands.log'}")
        return result

    try:
        key = directory / "id_ed25519"
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key)], check=True)
        destination = directory / "vm"
        run("create", name, "--dest", destination, "--binary", args.binary,
            "--iso", args.iso, "--firmware", args.firmware, "--net", args.net,
            "--shutdown-key", key, "--ram", 2048, "--cpus", 2)
        run("install", name, "--iso", args.iso, "--filesystem", args.filesystem,
            "--ssh-key", key.with_suffix(".pub"), timeout=960)
        config = json.loads((destination / (name + ".json")).read_text())
        disks = [item if isinstance(item, str) else item["path"] for item in config["disks"]]
        assert disks and not any(path.lower().endswith(".iso") for path in disks), config
        run("start", name)
        if args.net == "none":
            deadline = time.monotonic() + 240
            console = state_root / name / "console.log"
            while "login:" not in (console.read_text(errors="replace") if console.exists() else ""):
                if not ctl.running(name) or time.monotonic() >= deadline:
                    raise RuntimeError("installed guest did not reach login; inspect console.log")
                time.sleep(0.2)
            run("stop", name, "--timeout", 10)
        else:
            result = run("ssh", name, "--wait", "--wait-timeout", 240,
                         "--", "printf CUBECTL_SSH_OK", timeout=270)
            assert "CUBECTL_SSH_OK" in result.stdout, result.stdout
            response = json.loads(run("shutdown", name, "--timeout", 120, timeout=150).stdout)
            assert response.get("result") == "guest-poweroff", response
        assert not ctl.running(name), "VM still running after stop/shutdown"
        passed = True
        print(f"PASS native {args.filesystem} create/install/start/" +
              ("stop" if args.net == "none" else "ssh/shutdown"), flush=True)
    finally:
        if (state_root / name).exists() and ctl.running(name):
            ctl.call("stop", name, "--timeout", 10)
        if os.geteuid() == 0 and "SUDO_UID" in os.environ:
            try:
                subprocess.run(["sudo", "-n", "chown", "-R",
                                f"{os.environ['SUDO_UID']}:{os.environ.get('SUDO_GID', 0)}", str(directory)], check=False)
            except Exception:
                pass
        if passed and not args.keep:
            shutil.rmtree(directory)
        else:
            print(f"Artifacts retained: {directory}", flush=True)


if __name__ == "__main__":
    main()
