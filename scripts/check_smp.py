#!/usr/bin/env python3
"""Two vCPUs: COM1 must echo one UART byte, and the AP must write port 0x402."""

import os
import subprocess
import sys
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "debug", "vmm")
SRC = os.path.join(ROOT, "guest", "smp.S")
OBJ = os.path.join(ROOT, "target", "smp.o")
PAYLOAD = os.path.join(ROOT, "target", "smp.bin")
LOG = os.path.join(ROOT, "target", "check_smp.log")
ENTITLEMENTS = os.path.join(ROOT, "entitlements.plist")
UART = os.path.join(ROOT, "target", "smp-uart-in.bin")
NEED_OUT = b"Q"
NEED_ERR = b"port 0x402 <- a5"
TIMEOUT_S = 15


def extract_text(data):
    if len(data) < 64 or data[:4] != b"\x7fELF" or data[4] != 2:
        raise SystemExit("smp.o is not a 64-bit ELF")
    e_shoff = int.from_bytes(data[40:48], "little")
    e_shentsize = int.from_bytes(data[58:60], "little")
    e_shnum = int.from_bytes(data[60:62], "little")
    e_shstrndx = int.from_bytes(data[62:64], "little")

    def shdr(index):
        off = e_shoff + index * e_shentsize
        return data[off : off + 64]

    names_hdr = shdr(e_shstrndx)
    names_off = int.from_bytes(names_hdr[24:32], "little")
    names_size = int.from_bytes(names_hdr[32:40], "little")
    names = data[names_off : names_off + names_size]
    for index in range(e_shnum):
        hdr = shdr(index)
        name_idx = int.from_bytes(hdr[0:4], "little")
        end = names.find(b"\x00", name_idx)
        if names[name_idx:end] == b".text":
            off = int.from_bytes(hdr[24:32], "little")
            size = int.from_bytes(hdr[32:40], "little")
            return data[off : off + size]
    raise SystemExit("no .text in smp.o")


def assemble():
    os.makedirs(os.path.dirname(OBJ), exist_ok=True)
    status = subprocess.call(
        [
            "clang",
            "-target",
            "x86_64-unknown-none",
            "-c",
            SRC,
            "-o",
            OBJ,
        ]
    )
    if status != 0:
        raise SystemExit("clang failed to assemble guest/smp.S")
    with open(OBJ, "rb") as fh:
        text = extract_text(fh.read())
    if not text:
        raise SystemExit("smp .text is empty")
    with open(PAYLOAD, "wb") as fh:
        fh.write(text)


def codesign():
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
        raise SystemExit("codesign failed")


def _read(pipe, sink):
    try:
        while True:
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
    assemble()
    codesign()
    with open(UART, "wb") as fh:
        fh.write(NEED_OUT)
    env = os.environ.copy()
    env["VMM_UART_IN"] = UART
    proc = subprocess.Popen(
        [BIN, "--payload", PAYLOAD, "--cpus", "2"],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=ROOT,
        env=env,
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
        stdout = b"".join(out_parts)
        stderr = b"".join(err_parts)
        if NEED_OUT in stdout and NEED_ERR in stderr:
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
        print(
            "missing COM1 %r or %r within %ss" % (NEED_OUT, NEED_ERR, TIMEOUT_S),
            file=sys.stderr,
        )
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
