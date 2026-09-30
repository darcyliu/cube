#!/usr/bin/env python3
"""Verify managed guest SSH poweroff using a cloned installed FreeBSD disk."""
import argparse
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time

import vmmctl


def clone(source, destination):
    result = subprocess.run(["cp", "-c", str(source), str(destination)],
                            capture_output=True, text=True)
    if result.returncode != 0:
        raise RuntimeError("APFS clone failed; source disk was not modified: " + result.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--disk", type=Path, required=True)
    parser.add_argument("--ssh-key", type=Path, required=True)
    parser.add_argument("--firmware", type=Path, required=True)
    parser.add_argument("--efi-vars", type=Path, help="clone an existing EFI variable store")
    parser.add_argument("--boot-timeout", type=int, default=240)
    parser.add_argument("--shutdown-timeout", type=int, default=120)
    parser.add_argument("--sudo", action="store_true",
                        help="use in-process vmnet as root via sudo -n (default: vmnet-proxy)")
    parser.add_argument("--keep", action="store_true")
    args = parser.parse_args()
    if args.sudo and os.geteuid() != 0:
        # The manager is detached and cannot consume a tty-scoped sudo ticket.
        # Elevate this foreground check before creating its disposable state.
        os.execvp("sudo", ["sudo", "-n", sys.executable, "-B", __file__,
                           *sys.argv[1:]])
    if not args.sudo and os.geteuid() == 0:
        parser.error("vmnet-proxy rejects root; re-run as your user, or pass --sudo "
                     "for in-process vmnet")
    binary, disk, key, firmware = (p.expanduser().resolve() for p in
                                   (args.binary, args.disk, args.ssh_key, args.firmware))
    for path in (binary, disk, key, firmware):
        if not path.is_file():
            parser.error(f"missing input: {path}")
    if args.efi_vars and not args.efi_vars.expanduser().resolve().is_file():
        parser.error("EFI variable store does not exist")
    if args.boot_timeout <= 0 or args.shutdown_timeout <= 0:
        parser.error("timeouts must be positive")
    directory = Path(tempfile.mkdtemp(prefix="vmm-shutdown-"))
    state_root = directory / "state"
    state = state_root / "shutdown-smoke"
    passed = False
    print(f"Artifacts: {directory}", flush=True)
    try:
        clone(disk, directory / "installed.img")
        if args.efi_vars:
            state.mkdir(mode=0o700, parents=True)
            clone(args.efi_vars.expanduser().resolve(), state / "efi-vars.fd")
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        config = directory / "shutdown-smoke.json"
        config.write_text(json.dumps({
            "name": "shutdown-smoke", "binary": str(binary),
            "boot": {"mode": "uefi", "firmware": str(firmware)},
            "disks": [{"path": str(directory / "installed.img")}],
            "ram_mb": 2048, "cpus": 2,
            "network": {"mode": "vmnet" if args.sudo else "vmnet-proxy", "ssh_port": port},
            "shutdown": {"host": "127.0.0.1", "port": port,
                         "user": "root", "identity_file": str(key)},
        }))
        options = argparse.Namespace(binary=None, ram=None, cpus=None, net=None,
                                     mac=None, ssh_port=None, forward=None, vnc=None,
                                     display=None, efi_vars=None, disk=None, sudo=False)
        vmmctl.start(config, state_root, state, options)
        ssh = ["ssh", "-F", "/dev/null", "-i", str(key), "-p", str(port),
               "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes",
               "-o", "ConnectTimeout=2", "-o", "StrictHostKeyChecking=accept-new",
               "-o", "UserKnownHostsFile=" + str(state / "known_hosts"),
               "root@127.0.0.1", "printf VMM_SSH_OK"]
        deadline = time.monotonic() + args.boot_timeout
        while True:
            result = subprocess.run(ssh, capture_output=True, text=True, timeout=8)
            if result.returncode == 0 and result.stdout == "VMM_SSH_OK":
                break
            if not vmmctl.live_status(state):
                raise RuntimeError("VM exited before SSH was ready; see vmm.log and console.log")
            if time.monotonic() >= deadline:
                raise RuntimeError("SSH did not become ready; see vmm.log and console.log")
            time.sleep(1)
        print("PASS installed guest SSH is reachable", flush=True)
        response = vmmctl.query(state, {"action": "shutdown",
                                        "timeout": args.shutdown_timeout,
                                        "force_after": False},
                                timeout=args.shutdown_timeout + 25)
        if response.get("result") != "guest-poweroff":
            raise RuntimeError("managed guest shutdown failed: " + json.dumps(response))
        print("PASS SSH-coordinated guest PSCI poweroff (no forced stop)", flush=True)
        passed = True
    finally:
        if vmmctl.live_status(state):
            vmmctl.query(state, {"action": "stop", "timeout": 10}, timeout=20)
        if passed and not args.keep:
            # A sudo-launched VMM may own its EFI/serial files. This directory
            # was created exclusively by this check and contains only its clones.
            if os.geteuid() == 0:
                subprocess.run(["sudo", "-n", "chown", "-R",
                                f"{os.getuid()}:{os.getgid()}", str(directory)], check=True)
            shutil.rmtree(directory)
        else:
            print(f"Artifacts retained: {directory}", flush=True)


if __name__ == "__main__":
    main()
