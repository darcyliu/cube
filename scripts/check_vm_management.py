#!/usr/bin/env python3
"""Boot two read-only FreeBSD VMs concurrently through vmmctl (Apple Silicon)."""
import argparse
import json
from pathlib import Path
import shutil
import socket
import tempfile
import time

import vmmctl


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def options():
    return argparse.Namespace(binary=None, ram=None, cpus=None, net=None, mac=None,
                              ssh_port=None, forward=None, vnc=None, display=None,
                              efi_vars=None, disk=None, sudo=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--kernel", type=Path, required=True)
    parser.add_argument("--iso", type=Path, required=True)
    parser.add_argument("--keep", action="store_true")
    args = parser.parse_args()
    binary, kernel, iso = (p.resolve() for p in (args.binary, args.kernel, args.iso))
    for path in (binary, kernel, iso):
        if not path.is_file():
            parser.error(f"missing input: {path}")
    directory = Path(tempfile.mkdtemp(prefix="vmm-management-"))
    state_root = directory / "state"
    ports = [free_port(), free_port()]
    while ports[1] == ports[0]:
        ports[1] = free_port()
    active = []
    passed = False
    print(f"Artifacts: {directory}", flush=True)
    try:
        for name, port in zip(("first", "second"), ports):
            config = directory / (name + ".json")
            config.write_text(json.dumps({
                "name": name, "binary": str(binary),
                "boot": {"mode": "freebsd", "kernel": str(kernel),
                         "root": "cd9660:/dev/vtbd0", "multiuser": False},
                "disks": [{"path": str(iso), "read_only": True}],
                "ram_mb": 1024, "cpus": 2, "network": {"mode": "none"},
                "vnc": {"bind": f"127.0.0.1:{port}", "display": "800x600"},
            }))
            result = vmmctl.start(config, state_root, state_root / name, options())
            assert result["state"] == "running"
            active.append(state_root / name)
            assert "listen-port:" + str(port) in result["resources"]
        first, second = active
        one = json.loads((first / "launch.json").read_text())
        two = json.loads((second / "launch.json").read_text())
        for flag in ("--serial-log", "--efi-vars"):
            assert one["command"][one["command"].index(flag) + 1] != \
                   two["command"][two["command"].index(flag) + 1]
        assert str(first / "uart") != str(second / "uart")
        deadline = time.monotonic() + 120
        for state in active:
            console = state / "console.log"
            while "start_init: trying /sbin/init" not in (console.read_text(errors="replace")
                                                           if console.exists() else ""):
                assert vmmctl.live_status(state), f"VM exited before init: {state}"
                assert time.monotonic() < deadline, f"Timed out booting {state}"
                time.sleep(0.2)
        assert all(vmmctl.live_status(state) for state in active)
        for port in ports:
            with socket.create_connection(("127.0.0.1", port), timeout=5) as client:
                assert client.recv(12) == b"RFB 003.008\n"
        print("PASS two concurrent VMs with private EFI/serial/UART state and VNC ports", flush=True)
        collision = directory / "collision.json"
        third = json.loads((directory / "second.json").read_text())
        third["name"] = "collision"
        third["vnc"]["bind"] = f"127.0.0.1:{ports[0]}"
        collision.write_text(json.dumps(third))
        try:
            vmmctl.start(collision, state_root, state_root / "collision", options())
        except ValueError as error:
            assert "resource already used" in str(error), error
        else:
            raise AssertionError("conflicting VNC port was accepted")
        print("PASS conflicting listener rejected before VM launch", flush=True)
        passed = True
    finally:
        for state in active:
            if vmmctl.live_status(state):
                vmmctl.query(state, {"action": "stop", "timeout": 10}, timeout=20)
        if passed and not args.keep:
            shutil.rmtree(directory)
        else:
            print(f"Artifacts retained: {directory}", flush=True)


if __name__ == "__main__":
    main()
