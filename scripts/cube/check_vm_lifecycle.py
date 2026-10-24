#!/usr/bin/env python3
"""Diskless HVF smoke tests. Requires a signed VMM and Apple Silicon macOS.

Usage: python3 scripts/check_vm_lifecycle.py /path/to/vmm [--kernel /path/to/kernel]
Optional --firmware exercises UEFI using a temporary variable store.
All generated kernels, stores and logs live in a temporary directory.
"""
import sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
sys.path.insert(0, str(Path(__file__).resolve().parent))

import argparse
import os
import socket
import struct
import subprocess
import tempfile
import time


def wait_for_file(path, text, timeout=45, count=1):
    deadline = time.monotonic() + timeout
    output = ""
    while time.monotonic() < deadline:
        output = path.read_text(errors="replace") if path.exists() else ""
        if output.count(text) >= count:
            return output
        time.sleep(0.1)
    raise AssertionError(f"Timed out waiting for {text!r} in {path}:\n{output[-4000:]}")


OFF = "movz x0, #8\nmovk x0, #0x8400, lsl #16\nhvc #0\nb .\n"
RESET = "movz x0, #9\nmovk x0, #0x8400, lsl #16\nhvc #0\nb .\n"
CYCLE = """
    mov x3, #1
again:
    mov x1, #1
    adr x2, secondary
    movz x0, #3
    movk x0, #0xc400, lsl #16
    hvc #0
    cbnz x0, again
    adr x4, counter
wait:
    ldr x5, [x4]
    cmp x5, x3
    b.ne wait
    cmp x3, #2
    b.eq done
    mov x3, #2
    b again
secondary:
    adr x4, counter
    str x0, [x4]
    dmb sy
    movz x0, #2
    movk x0, #0x8400, lsl #16
    hvc #0
    b .
done:
""" + OFF + "\n.balign 8\ncounter: .quad 0\n"


from support.process import VM, kernel
from support.vnc import read, connect, key, pointer


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary")
    parser.add_argument("--kernel")
    parser.add_argument("--firmware")
    args = parser.parse_args()
    binary = str(Path(args.binary).resolve())
    for command, expected in [
        ([binary, "--display", "1024x768"], "--display requires --vnc"),
        ([binary, "--vnc", "--display", "16384x16384"], "maximum is 256 MiB"),
        ([binary, "--retina"], "--retina requires --gui native"),
        ([binary, "--retinaa"], "Unknown argument '--retinaa'"),
    ]:
        result = subprocess.run(command, text=True, capture_output=True)
        assert result.returncode != 0, result.stdout + result.stderr
        assert expected in result.stdout + result.stderr, result.stdout + result.stderr
    print("PASS display geometry CLI validation", flush=True)
    machines = []
    with tempfile.TemporaryDirectory(prefix="vmm-lifecycle-") as temp:
        directory = Path(temp)
        try:
            for name, source in [("parked-cpu", OFF), ("cpu-off-on", CYCLE)]:
                path = kernel(directory, name, source)
                vm = VM(binary, directory, name, ["--kernel", path, "--cpus", 2, "--ram", 256])
                machines.append(vm)
                vm.finish()
                assert "SYSTEM_OFF" in vm.text(), vm.text()
                if name == "cpu-off-on":
                    assert vm.text().count("CPU_ON target") == 2
                print("PASS", name, flush=True)
            reset = kernel(directory, "reset", RESET)
            vm = VM(binary, directory, "reboot", ["--kernel", reset, "--cpus", 2, "--ram", 256])
            machines.append(vm)
            vm.wait_for("SYSTEM_RESET")
            # Replace only our disposable kernel so the reboot powers off.
            replacement = kernel(directory, "off", OFF)
            os.replace(replacement, reset)
            vm.finish()
            assert vm.text().count("Preparing to boot") >= 2
            assert "SYSTEM_OFF" in vm.text()
            print("PASS reboot", flush=True)
            if args.kernel:
                with socket.socket() as sock:
                    sock.bind(("127.0.0.1", 0))
                    port = sock.getsockname()[1]
                serial = directory / "freebsd.console.log"
                vm = VM(binary, directory, "freebsd", ["--freebsd", args.kernel, "--cpus", 2,
                                                         "--vnc", f"127.0.0.1:{port}",
                                                         "--display", "1024x768",
                                                         "--serial-log", serial])
                machines.append(vm)
                wait_for_file(serial, "mountroot>", timeout=90)
                sock, size = connect("127.0.0.1", port)
                assert size == (1024, 768), size
                key(sock, 0xffe1, 1)  # Drop the connection while Shift is held.
                sock.close()
                time.sleep(0.2)
                sock, size = connect("127.0.0.1", port)
                assert size == (1024, 768), size
                # Request a small rectangle in RGB565; payload must match it.
                fmt = struct.pack("!BBBBHHHBBBxxx", 16, 16, 0, 1, 31, 63, 31, 11, 5, 0)
                sock.sendall(bytes(4) + fmt)
                sock.sendall(struct.pack("!BBHHHH", 3, 0, 10, 20, 30, 40))
                header = read(sock, 16)
                assert struct.unpack("!HHHH", header[4:12]) == (10, 20, 30, 40), header
                assert len(read(sock, 30 * 40 * 2)) == 2400
                # Exercise absolute edges, buttons and both wheel directions.
                for event in [(0, 0, 0), (1, 1023, 767), (0, 1023, 767),
                              (8, 512, 384), (16, 512, 384), (0, 512, 384)]:
                    pointer(sock, *event)
                for symbol in [ord("."), 0xff0d]:
                    key(sock, symbol, 1)
                    key(sock, symbol, 0)
                    time.sleep(0.1)
                serial_output = wait_for_file(serial, "mountroot>", count=2)
                assert "mountroot> ." in serial_output, serial_output[-2000:]
                sock.close()
                vm.wait_for("Session metrics:", count=2)
                vm.proc.terminate()
                code = vm.proc.wait(timeout=15)
                assert code == 0, vm.text()[-4000:]
                vm.log.close()
                print("PASS FreeBSD SMP, custom VNC geometry, region/RGB565, pointer events, held-key cleanup, metrics, SIGTERM", flush=True)
            if args.firmware:
                vm = VM(binary, directory, "uefi", ["--uefi", args.firmware, "--cpus", 2,
                                                    "--efi-vars", directory / "vars.fd"])
                machines.append(vm)
                vm.wait_for("UEFI firmware (version", timeout=60)
                vm.finish(stop=True)
                assert (directory / "vars.fd").stat().st_size > 0
                print("PASS UEFI firmware entry and shutdown with unused CPU", flush=True)
        except BaseException:
            for vm in machines:
                print(f"--- {vm.path.name} ---\n{vm.text()[-4000:]}")
            raise
        finally:
            for vm in machines:
                vm.cleanup()


if __name__ == "__main__":
    main()
