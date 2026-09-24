#!/usr/bin/env python3
"""Diskless HVF smoke tests. Requires a signed VMM and Apple Silicon macOS.

Usage: python3 scripts/check_vm_lifecycle.py /path/to/vmm [--kernel /path/to/kernel]
Optional --firmware exercises UEFI using a temporary variable store.
All generated kernels, stores and logs live in a temporary directory.
"""
import argparse
import os
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import time


def kernel(directory, name, assembly):
    source = directory / (name + ".S")
    obj = directory / (name + ".o")
    source.write_text(".text\n.global _start\n_start:\n" + assembly)
    subprocess.run(["clang", "-target", "aarch64-unknown-none", "-c", str(source), "-o", str(obj)], check=True)
    data = obj.read_bytes()
    shoff = struct.unpack_from("<Q", data, 40)[0]
    shsize, count, names_idx = struct.unpack_from("<HHH", data, 58)
    names_off, names_len = struct.unpack_from("<QQ", data, shoff + names_idx * shsize + 24)
    names = data[names_off:names_off + names_len]
    for i in range(count):
        header = shoff + i * shsize
        idx = struct.unpack_from("<I", data, header)[0]
        if names[idx:].split(b"\0", 1)[0] == b".text":
            offset, length = struct.unpack_from("<QQ", data, header + 24)
            code = data[offset:offset + length]
            break
    else:
        raise AssertionError("assembly has no .text")
    # A single segment; all assembly references are local/PC-relative.
    ident = b"\x7fELF\x02\x01\x01" + bytes(9)
    elf = ident + struct.pack("<HHIQQQIHHHHHH", 2, 183, 1, 0x40000000, 64, 0, 0, 64, 56, 1, 0, 0, 0)
    elf += struct.pack("<IIQQQQQQ", 1, 7, 0x1000, 0x40000000, 0x40000000, len(code), len(code), 0x1000)
    path = directory / (name + ".elf")
    path.write_bytes(elf.ljust(0x1000, b"\0") + code)
    return path


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


class VM:
    def __init__(self, binary, directory, name, args, env=None):
        self.path = directory / (name + ".log")
        self.log = self.path.open("wb")
        self.proc = subprocess.Popen([binary, "--net", "none", *map(str, args)], stdin=subprocess.PIPE,
                                     stdout=self.log, stderr=subprocess.STDOUT, env=env)

    def text(self):
        return self.path.read_text(errors="replace")

    def wait_for(self, text, timeout=45, count=1):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if self.text().count(text) >= count:
                return
            if self.proc.poll() is not None:
                break
            time.sleep(0.1)
        raise AssertionError(f"Timed out waiting for {text!r}:\n{self.text()[-4000:]}")

    def finish(self, stop=False):
        if stop and self.proc.poll() is None:
            self.proc.stdin.write(b"\x1d")
            self.proc.stdin.flush()
        code = self.proc.wait(timeout=15)
        assert code == 0, self.text()[-4000:]
        self.log.close()

    def cleanup(self):
        if self.proc.poll() is None:
            self.proc.terminate()
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()
        self.log.close()


def read(sock, size):
    result = b""
    while len(result) < size:
        data = sock.recv(size - len(result))
        if not data:
            raise EOFError("VNC disconnected")
        result += data
    return result


def connect(port):
    sock = socket.create_connection(("127.0.0.1", port), timeout=5)
    assert read(sock, 12) == b"RFB 003.008\n"
    sock.sendall(b"RFB 003.008\n")
    assert read(sock, 2) == b"\x01\x01"
    sock.sendall(b"\x01")
    assert read(sock, 4) == bytes(4)
    sock.sendall(b"\x01")
    init = read(sock, 24)
    read(sock, struct.unpack_from("!I", init, 20)[0])
    return sock, struct.unpack_from("!HH", init)


def key(sock, symbol, down):
    sock.sendall(struct.pack("!BBHI", 4, down, 0, symbol))


def pointer(sock, mask, x, y):
    sock.sendall(struct.pack("!BBHH", 5, mask, x, y))


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
                vm = VM(binary, directory, "freebsd", ["--freebsd", args.kernel, "--cpus", 2,
                                                         "--vnc", f"127.0.0.1:{port}",
                                                         "--display", "1024x768"])
                machines.append(vm)
                vm.wait_for("mountroot>", timeout=90)
                sock, size = connect(port)
                assert size == (1024, 768), size
                key(sock, 0xffe1, 1)  # Drop the connection while Shift is held.
                sock.close()
                time.sleep(0.2)
                sock, size = connect(port)
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
                vm.wait_for("mountroot>", count=2)
                assert "\n.\n" in vm.text(), vm.text()[-2000:]
                sock.close()
                vm.wait_for("Session metrics:", count=2)
                vm.finish(stop=True)
                print("PASS FreeBSD SMP, custom VNC geometry, region/RGB565, pointer events, held-key cleanup, metrics, Ctrl-]", flush=True)
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
