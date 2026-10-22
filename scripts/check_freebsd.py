#!/usr/bin/env python3
"""Boot the FreeBSD amd64 disc1 ISO and require a COM1 loader or installer prompt.

The ISO is virtio-blk (vtbd0), not an AHCI CD. This script does not set
vfs.root.mountfrom and does not type the mountroot line. A mountroot prompt
is a pass; the line to type by hand is cd9660:/dev/vtbd0.
"""

import os
import subprocess
import sys
import threading
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target", "debug", "vmm")
CODE = os.path.join(ROOT, "firmware", "OVMF_CODE.fd")
VARS = os.path.join(ROOT, "firmware", "OVMF_VARS.fd")
ISO = os.path.join(ROOT, "FreeBSD-14.4-RELEASE-amd64-disc1.iso")
ENTITLEMENTS = os.path.join(ROOT, "entitlements.plist")
DISK = os.path.join(ROOT, "target", "check_freebsd-disk.img")
LOG = os.path.join(ROOT, "target", "check_freebsd.log")
SERIAL = os.path.join(ROOT, "target", "check_freebsd-serial.log")
# Later entries win so a mountroot stop is not reported as only the loader.
MARKERS = (
    b"FreeBSD/amd64",
    b"FreeBSD EFI",
    b"BTX loader",
    b"Loading /boot/",
    b"Hit [Enter] to boot",
    b"Welcome to FreeBSD",
    b"bsdinstall",
    b"mountroot>",
)
TIMEOUT_S = 10 * 60
DISK_BYTES = 8 * 1024 * 1024 * 1024


def guest_argv(binary, iso, disk, serial):
    return [
        binary,
        "--uefi",
        "firmware/OVMF_CODE.fd",
        "--cdrom",
        iso,
        "--disk",
        disk,
        "--cpus",
        "2",
        "--ram",
        "1024",
        "--net",
        "vmnet-proxy",
        "--serial-log",
        serial,
    ]


def _read(pipe, sink):
    try:
        while True:
            chunk = pipe.read1(4096)
            if not chunk:
                break
            sink.append(chunk)
    except Exception:
        pass


def _blob(stdout):
    blob = b"".join(stdout)
    try:
        with open(SERIAL, "rb") as fh:
            blob += fh.read()
    except Exception:
        pass
    return blob


def matched(blob):
    found = None
    for marker in MARKERS:
        if marker in blob:
            found = marker
    return found


def empty_disk():
    os.makedirs(os.path.dirname(DISK), exist_ok=True)
    fd = os.open(DISK, os.O_CREAT | os.O_RDWR | os.O_TRUNC, 0o644)
    try:
        os.ftruncate(fd, DISK_BYTES)
    finally:
        os.close(fd)
    # A previous store would change the boot. The first open copies OVMF_VARS.
    try:
        os.remove(DISK + ".vars.fd")
    except OSError:
        pass


def main():
    if not os.path.isfile(BIN):
        print("missing %s (cargo build first)" % BIN, file=sys.stderr)
        return 1
    if not os.path.isfile(CODE) or not os.path.isfile(VARS):
        status = subprocess.call(["make", "fetch-uefi"], cwd=ROOT)
        if status != 0:
            return status
    if not os.path.isfile(ISO):
        status = subprocess.call(["make", "fetch-iso"], cwd=ROOT)
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
    empty_disk()
    try:
        os.remove(SERIAL)
    except OSError:
        pass
    command = guest_argv(BIN, ISO, DISK, SERIAL)
    proc = subprocess.Popen(
        command,
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
    saw = None
    while time.time() < deadline:
        saw = matched(_blob(out_parts))
        if saw is not None:
            break
        if proc.poll() is not None:
            break
        time.sleep(0.25)
    if saw is None:
        saw = matched(_blob(out_parts))

    if saw is not None and proc.poll() is None:
        try:
            proc.stdin.write(b"\x1d")
            proc.stdin.flush()
        except Exception:
            pass
        try:
            proc.wait(timeout=15)
        except subprocess.TimeoutExpired:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=5)
    elif proc.poll() is None:
        proc.kill()
        proc.wait(timeout=5)

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
    if saw is None:
        saw = matched(stdout + serial)
    try:
        os.makedirs(os.path.dirname(LOG), exist_ok=True)
        with open(LOG, "wb") as fh:
            fh.write(b"--- command ---\n")
            fh.write((" ".join(command) + "\n").encode())
            fh.write(b"--- stdout ---\n")
            fh.write(stdout)
            fh.write(b"\n--- serial ---\n")
            fh.write(serial)
            fh.write(b"\n--- stderr ---\n")
            fh.write(stderr)
            fh.write(("\n--- exit %s ---\n" % proc.returncode).encode())
            if saw is not None:
                fh.write(b"--- marker ---\n")
                fh.write(saw + b"\n")
    except Exception as exc:
        print("could not write %s: %s" % (LOG, exc), file=sys.stderr)

    print("serial log: %s" % SERIAL)
    if saw is None:
        sys.stderr.buffer.write(stderr[-4000:])
        print(
            "FreeBSD loader, installer, or mountroot prompt not seen within %ss"
            % TIMEOUT_S,
            file=sys.stderr,
        )
        print("log: %s" % LOG, file=sys.stderr)
        return 1
    print("COM1 marker: %s" % saw.decode("ascii", "replace"))
    if saw == b"mountroot>":
        print("mountroot prompt; the line to type is cd9660:/dev/vtbd0")
    if proc.returncode != 0:
        sys.stderr.buffer.write(stderr[-4000:])
        print("vmm exited %s" % proc.returncode, file=sys.stderr)
        print("log: %s" % LOG, file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
