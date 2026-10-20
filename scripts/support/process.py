"""Direct cube process fixture and synthetic AArch64 kernel builder."""
import struct
import subprocess
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


