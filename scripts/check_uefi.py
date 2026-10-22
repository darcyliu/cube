#!/usr/bin/env python3
"""Boot OVMF and require a COM1 banner. No disk."""

import os
import subprocess
import sys
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "debug", "vmm")
CODE = os.path.join(ROOT, "firmware", "OVMF_CODE.fd")
VARS = os.path.join(ROOT, "firmware", "OVMF_VARS.fd")
ENTITLEMENTS = os.path.join(ROOT, "entitlements.plist")
LOG = os.path.join(ROOT, "target", "check_uefi.log")
SERIAL = os.path.join(ROOT, "target", "check_uefi-serial.log")
MARKERS = (b"UEFI Interactive Shell", b"EDK II", b"BdsDxe")
TIMEOUT_S = 60


def _read(pipe, sink):
    try:
        while True:
            chunk = pipe.read1(4096)
            if not chunk:
                break
            sink.append(chunk)
    except Exception:
        pass


def _seen(stdout):
    blob = b"".join(stdout)
    try:
        with open(SERIAL, "rb") as fh:
            blob += fh.read()
    except Exception:
        pass
    return any(marker in blob for marker in MARKERS)


def main():
    if not os.path.isfile(BIN):
        print("missing %s (cargo build first)" % BIN, file=sys.stderr)
        return 1
    if not os.path.isfile(CODE) or not os.path.isfile(VARS):
        status = subprocess.call(["make", "fetch-uefi"], cwd=ROOT)
        if status != 0:
            return status
    status = subprocess.call(
        [
            "codesign",
            "--entitlements",
            ENTITLEMENTS,
            "--force",
            "-s",
            "-",
            BIN,
        ]
    )
    if status != 0:
        print("codesign failed", file=sys.stderr)
        return status
    try:
        os.remove(SERIAL)
    except OSError:
        pass
    proc = subprocess.Popen(
        [
            BIN,
            "--cpus",
            "1",
            "--uefi",
            "firmware/OVMF_CODE.fd",
            "--serial-log",
            SERIAL,
        ],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=ROOT,
    )
    out_parts = []
    err_parts = []
    threads = [
        threading.Thread(target=_read, args=(proc.stdout, out_parts)),
        threading.Thread(target=_read, args=(proc.stderr, err_parts)),
    ]
    for t in threads:
        t.daemon = True
        t.start()

    deadline = time.time() + TIMEOUT_S
    saw = False
    while time.time() < deadline:
        if _seen(out_parts):
            saw = True
            break
        if proc.poll() is not None:
            break
        time.sleep(0.05)

    if saw and proc.poll() is None:
        try:
            proc.stdin.write(b"\x1d")
            proc.stdin.flush()
        except Exception:
            pass
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.terminate()
            try:
                proc.wait(timeout=2)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=2)
    elif proc.poll() is None:
        proc.kill()
        proc.wait(timeout=2)

    for t in threads:
        t.join(timeout=1)
    stdout = b"".join(out_parts)
    stderr = b"".join(err_parts)
    serial = b""
    try:
        with open(SERIAL, "rb") as fh:
            serial = fh.read()
    except Exception:
        pass
    if not saw:
        saw = any(marker in stdout or marker in serial for marker in MARKERS)
    try:
        os.makedirs(os.path.dirname(LOG), exist_ok=True)
        with open(LOG, "wb") as fh:
            fh.write(b"--- stdout ---\n")
            fh.write(stdout)
            fh.write(b"\n--- serial ---\n")
            fh.write(serial)
            fh.write(b"\n--- stderr ---\n")
            fh.write(stderr)
            fh.write(("\n--- exit %s ---\n" % proc.returncode).encode())
    except Exception as exc:
        print("could not write %s: %s" % (LOG, exc), file=sys.stderr)

    if not saw:
        sys.stderr.buffer.write(stderr[-4000:])
        print("OVMF banner not seen within %ss" % TIMEOUT_S, file=sys.stderr)
        print("log: %s" % LOG, file=sys.stderr)
        return 1
    if proc.returncode != 0:
        sys.stderr.buffer.write(stderr[-4000:])
        print("vmm exited %s" % proc.returncode, file=sys.stderr)
        print("log: %s" % LOG, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
