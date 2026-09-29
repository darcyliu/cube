#!/usr/bin/env python3
"""Verify a running FreeBSD X11 desktop through VNC and SSH.

The guest must be disposable, have Xorg/scfb, twm, xterm and xinput installed,
and expose root SSH on localhost. See docs/freebsd-desktop.md.
"""
import argparse
import json
import math
from pathlib import Path
import re
import socket
import struct
import subprocess
import time


def read(sock, size):
    result = b""
    while len(result) < size:
        data = sock.recv(size - len(result))
        if not data:
            raise EOFError("VNC disconnected")
        result += data
    return result


def connect(host, port):
    sock = socket.create_connection((host, port), timeout=10)
    assert read(sock, 12) == b"RFB 003.008\n"
    sock.sendall(b"RFB 003.008\n")
    assert read(sock, 2) == b"\x01\x01"
    sock.sendall(b"\x01")
    assert read(sock, 4) == bytes(4)
    sock.sendall(b"\x01")
    init = read(sock, 24)
    width, height = struct.unpack_from("!HH", init)
    read(sock, struct.unpack_from("!I", init, 20)[0])
    return sock, (width, height)


def key(sock, symbol, down):
    sock.sendall(struct.pack("!BBHI", 4, down, 0, symbol))


def pointer(sock, mask, x, y):
    sock.sendall(struct.pack("!BBHH", 5, mask, x, y))


def update(sock, width, height, incremental):
    started = time.monotonic()
    sock.sendall(struct.pack("!BBHHHH", 3, incremental, 0, 0, width, height))
    header = read(sock, 4)
    rectangles = struct.unpack_from("!H", header, 2)[0]
    payload = 0
    for _ in range(rectangles):
        rect = read(sock, 12)
        _, _, rect_width, rect_height, encoding = struct.unpack("!HHHHi", rect)
        assert encoding == 0, f"unexpected VNC encoding {encoding}"
        size = rect_width * rect_height * 4
        read(sock, size)
        payload += size
    return {"seconds": time.monotonic() - started,
            "rectangles": rectangles, "payload_bytes": payload}


def xi2_events(text):
    """Return (event name, body) pairs from xinput --test-xi2 output."""
    return re.findall(r"^EVENT type \d+ \(([^)]+)\)\s*\n(.*?)(?=^EVENT type|\Z)",
                      text, re.MULTILINE | re.DOTALL)


def pointer_position(text):
    """Parse xdotool getmouselocation --shell without evaluating shell text."""
    values = dict(re.findall(r"^(X|Y)=(-?\d+)$", text, re.MULTILINE))
    if set(values) != {"X", "Y"}:
        raise AssertionError(f"Cannot read X11 pointer coordinates: {text!r}")
    return int(values["X"]), int(values["Y"])


def pointer_at_target(actual, target):
    # Direct X11 queries on the Plasma guest report (1, 1) for a VNC move to
    # (0, 0). Allow one pixel per axis, consistently during polling and final
    # validation, while retaining the actual coordinates in the evidence log.
    return all(abs(observed - expected) <= 1 for observed, expected in zip(actual, target))


def wait_for_pointer(query, target, timeout=5):
    deadline = time.monotonic() + timeout
    while True:
        actual = query()
        if pointer_at_target(actual, target):
            return actual
        if time.monotonic() >= deadline:
            raise AssertionError(
                f"X11 pointer expected within 1px per axis of {target}, last observed {actual}")
        time.sleep(0.1)


def validate_events(text, width, height, pointer_positions=None):
    events = xi2_events(text)
    missing = []

    def details(name):
        return [int(value) for event, body in events if event == name
                for value in re.findall(r"^\s*detail:\s*(\d+)\s*$", body, re.MULTILINE)]

    motion = []
    for event, body in events:
        if event != "Motion":
            continue
        match = re.search(r"^\s*root:\s*(-?[\d.]+)/(-?[\d.]+)\s*$", body, re.MULTILINE)
        if match:
            # XI2 positions include subpixel fractions within the final pixel.
            motion.append(tuple(math.floor(float(value)) for value in match.groups()))
    # Normal Motion events selected on root may be delivered to a desktop or
    # panel window instead. Live checks query X11 after each VNC move; retain
    # event parsing for standalone historical-log validation.
    if pointer_positions is not None:
        motion = pointer_positions
    for point in ((0, 0), (width - 1, height - 1)):
        if not any(0 <= x < width and 0 <= y < height and
                   pointer_at_target((x, y), point) for x, y in motion):
            missing.append(f"Pointer root {point[0]}/{point[1]} within 1px per axis")
    # Raw events reach the root observer even when xterm/twm owns the target
    # window or a button grab. Count only raw events to avoid master/slave copies.
    for name, detail in (("RawButtonPress", 1), ("RawButtonPress", 4),
                         ("RawButtonPress", 5), ("KeyPress", 50),
                         ("KeyPress", 38), ("KeyRelease", 38),
                         ("KeyRelease", 50), ("KeyRelease", 37)):
        if detail not in details(name):
            missing.append(f"{name} detail {detail}")
    if details("RawButtonRelease").count(1) < 2:
        missing.append("two RawButtonRelease detail 1 events")
    if missing:
        raise AssertionError(f"Missing X11 events {missing}:\n{text[-6000:]}")


def wait_for_events(path, width, height, process, timeout=15, pointer_positions=None):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        text = path.read_text(errors="replace") if path.exists() else ""
        try:
            validate_events(text, width, height, pointer_positions)
            return text
        except AssertionError:
            pass
        if process.poll() is not None:
            break
        time.sleep(0.1)
    text = path.read_text(errors="replace") if path.exists() else ""
    validate_events(text, width, height, pointer_positions)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vnc", default="127.0.0.1:5900")
    parser.add_argument("--ssh-port", type=int, required=True)
    parser.add_argument("--identity", type=Path, required=True)
    parser.add_argument("--expect-resolution",
                        help="require the VNC ServerInit size, for example 1024x768")
    parser.add_argument("--user", default="root")
    parser.add_argument("--samples", type=int, default=30)
    parser.add_argument("--artifacts", type=Path, default=Path("desktop-results"))
    args = parser.parse_args(argv)
    if args.samples < 1:
        parser.error("--samples must be positive")
    host, separator, port = args.vnc.rpartition(":")
    if not separator or not host:
        parser.error("--vnc must be HOST:PORT")
    try:
        port = int(port)
    except ValueError:
        parser.error("--vnc port must be an integer")
    args.identity = args.identity.expanduser().resolve()
    if not args.identity.is_file():
        parser.error(f"identity is not a file: {args.identity}")
    args.artifacts = args.artifacts.expanduser().resolve()
    args.artifacts.mkdir(parents=True, exist_ok=True)

    known_hosts = args.artifacts / "known_hosts"
    ssh = ["ssh", "-F", "/dev/null", "-i", str(args.identity), "-p", str(args.ssh_port),
           "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes", "-o", "ConnectTimeout=5",
           "-o", "StrictHostKeyChecking=accept-new", "-o", f"UserKnownHostsFile={known_hosts}",
           "-o", "GlobalKnownHostsFile=/dev/null", f"{args.user}@127.0.0.1"]

    probe = "test \"$(uname -s)\" = FreeBSD && pgrep Xorg >/dev/null && " \
            "env DISPLAY=:0 xdpyinfo >/dev/null && command -v xinput && command -v xdotool"
    result = subprocess.run([*ssh, probe], text=True, capture_output=True, timeout=20)
    assert result.returncode == 0, "desktop preflight failed:\n" + result.stdout + result.stderr

    devices = subprocess.run([*ssh, "env DISPLAY=:0 xinput --list --short"], text=True,
                             capture_output=True, timeout=20)
    (args.artifacts / "xinput-devices.log").write_text(devices.stdout + devices.stderr)
    assert devices.returncode == 0, "cannot list X11 input devices"

    events_path = args.artifacts / "xinput-events.log"
    events_file = events_path.open("wb")
    events = subprocess.Popen([*ssh,
        "exec env DISPLAY=:0 stdbuf -oL xinput --test-xi2 --root"],
                              stdout=events_file, stderr=subprocess.STDOUT)
    measurements = []
    positions = []
    pointer_log = args.artifacts / "pointer-positions.jsonl"
    pointer_log.write_text("")

    def query_pointer(target):
        result = subprocess.run([*ssh, "env DISPLAY=:0 xdotool getmouselocation --shell"],
                                text=True, capture_output=True, timeout=5)
        with pointer_log.open("a") as log:
            log.write(json.dumps({"target": target, "returncode": result.returncode,
                                  "stdout": result.stdout, "stderr": result.stderr}) + "\n")
        assert result.returncode == 0, "X11 pointer query failed: " + result.stderr
        return pointer_position(result.stdout)

    sock = None
    try:
        time.sleep(1)
        sock, (width, height) = connect(host, port)
        if args.expect_resolution:
            actual = f"{width}x{height}"
            assert actual == args.expect_resolution, (
                f"expected {args.expect_resolution}, VNC advertised {actual}")
        measurements.append(update(sock, width, height, False))
        # Establish a nonzero position first: an initial (0, 0) HID report may
        # produce no motion. Pace transitions so Xorg does not coalesce edges.
        pointer(sock, 0, width // 2, height // 2)
        time.sleep(0.15)
        for target in ((0, 0), (width - 1, height - 1)):
            pointer(sock, 0, *target)
            positions.append(wait_for_pointer(lambda: query_pointer(target), target))
        for mask in (1, 0, 8, 0, 16, 0):
            pointer(sock, mask, width // 2, height // 2)
            time.sleep(0.15)
        key(sock, 0xffe1, True)  # Shift
        key(sock, ord("a"), True)
        key(sock, ord("a"), False)
        key(sock, 0xffe1, False)
        for index in range(args.samples):
            pointer(sock, 0, index * (width - 1) // max(args.samples - 1, 1), height // 2)
            measurements.append(update(sock, width, height, True))

        # Disconnect with a modifier and primary button held. Session teardown
        # must synthesize both releases before the next client is accepted.
        key(sock, 0xffe3, True)  # Control
        pointer(sock, 1, width // 2, height // 2)
        sock.close()
        sock = None
        time.sleep(0.5)
        sock, reconnect_size = connect(host, port)
        assert reconnect_size == (width, height)
        pointer(sock, 0, width // 2, height // 2)
        measurements.append(update(sock, width, height, False))

        elapsed = sum(item["seconds"] for item in measurements)
        payload = sum(item["payload_bytes"] for item in measurements)
        report = {
            "resolution": {"width": width, "height": height},
            "updates": len(measurements),
            "elapsed_seconds": elapsed,
            "payload_bytes": payload,
            "updates_per_second": len(measurements) / elapsed,
            "payload_mib_per_second": payload / (1024 * 1024) / elapsed,
            "samples": measurements,
        }
        # Preserve performance evidence even when an input assertion fails.
        (args.artifacts / "display-metrics.json").write_text(
            json.dumps(report, indent=2) + "\n")
        wait_for_events(events_path, width, height, events, pointer_positions=positions)
    finally:
        if sock is not None:
            sock.close()
        events.terminate()
        try:
            events.wait(timeout=5)
        except subprocess.TimeoutExpired:
            events.kill()
            events.wait()
        events_file.close()

    print("PASS X11 pointer edges, primary button, wheel, and keyboard modifiers", flush=True)
    print("PASS disconnect releases held Control/button and VNC reconnects", flush=True)
    print(f"PASS {len(measurements)} framebuffer updates: {report['updates_per_second']:.1f}/s, "
          f"{report['payload_mib_per_second']:.1f} MiB/s payload", flush=True)
    print(f"Artifacts: {args.artifacts}", flush=True)


if __name__ == "__main__":
    main()
