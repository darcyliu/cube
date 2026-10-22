#!/usr/bin/env python3
"""Run the signed vmm binary and require the long-mode COM1 banner."""

import os
import subprocess
import sys
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "debug", "vmm")
BANNER = b"[vmm-intel] long mode guest on COM1"
LOG = os.path.join(ROOT, "target", "check_payload.log")
TIMEOUT_S = 10


def _read(pipe, sink):
    try:
        while True:
            # read() waits for 4096 bytes or EOF. The banner is shorter than that
            # and is flushed one OUT at a time, so only read1 returns it while
            # the guest is still halted.
            chunk = pipe.read1(4096)
            if not chunk:
                break
            sink.append(chunk)
    except Exception:
        pass


def main():
    if not os.path.isfile(BIN):
        print("missing %s (cargo build first)" % BIN, file=sys.stderr)
        return 1
    proc = subprocess.Popen(
        [BIN, "--payload"],
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
        if BANNER in b"".join(out_parts):
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
            proc.wait(timeout=3)
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
    try:
        os.makedirs(os.path.dirname(LOG), exist_ok=True)
        with open(LOG, "wb") as fh:
            fh.write(b"--- stdout ---\n")
            fh.write(stdout)
            fh.write(b"\n--- stderr ---\n")
            fh.write(stderr)
            fh.write(("\n--- exit %s ---\n" % proc.returncode).encode())
    except Exception as exc:
        print("could not write %s: %s" % (LOG, exc), file=sys.stderr)

    if not saw:
        sys.stderr.buffer.write(stderr)
        print("banner not seen within %ss" % TIMEOUT_S, file=sys.stderr)
        print("log: %s" % LOG, file=sys.stderr)
        return 1
    if proc.returncode != 0:
        sys.stderr.buffer.write(stderr)
        print("vmm exited %s" % proc.returncode, file=sys.stderr)
        print("log: %s" % LOG, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
