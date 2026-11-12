#!/usr/bin/env python3
"""Boot Linux without a NIC and verify Cube virtio-vsock and virtio-fs data paths.

The guest image must boot to an automatic root shell on ttyAMA0 and include
Python 3, mount(8), and the Linux virtiofs/vsock modules.  A disk prepared by
scripts/debian-cloud.sh satisfies those requirements.
"""

import argparse
import base64
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from support.guest import publish
from support.process import VM


VSOCK_PORT = 5000
VSOCK_REQUEST = b"cube-vsock-request\n"
VSOCK_REPLY = b"cube-vsock-reply\n"


class UnixEcho:
    """One-shot Unix stream peer used by --vsock-map."""

    def __init__(self, path: Path):
        self.path = path
        self.received = b""
        self.error = None
        self.ready = threading.Event()
        self.done = threading.Event()
        self.thread = threading.Thread(target=self._run, name="cube-vsock-echo", daemon=True)

    def start(self):
        self.thread.start()
        if not self.ready.wait(5):
            raise RuntimeError("host vsock echo socket did not become ready")
        if self.error is not None:
            raise RuntimeError(f"host vsock echo setup failed: {self.error}")

    def _run(self):
        listener = None
        try:
            listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            listener.bind(str(self.path))
            listener.listen(1)
            listener.settimeout(240)
            self.ready.set()
            stream, _ = listener.accept()
            with stream:
                stream.settimeout(20)
                while len(self.received) < len(VSOCK_REQUEST):
                    chunk = stream.recv(4096)
                    if not chunk:
                        break
                    self.received += chunk
                if self.received == VSOCK_REQUEST:
                    stream.sendall(VSOCK_REPLY)
        except BaseException as error:  # surfaced in the main test thread
            self.error = error
            self.ready.set()
        finally:
            if listener is not None:
                listener.close()
            self.done.set()

    def check(self):
        if not self.done.wait(10):
            raise AssertionError("host vsock echo did not finish")
        if self.error is not None:
            raise AssertionError(f"host vsock echo failed: {self.error}")
        if self.received != VSOCK_REQUEST:
            raise AssertionError(f"host received unexpected vsock data: {self.received!r}")


def clone_disk(source: Path, destination: Path):
    """Use an APFS clone when available, with a portable copy fallback."""
    result = subprocess.run(
        ["cp", "-c", str(source), str(destination)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    if result.returncode != 0:
        shutil.copy2(source, destination)


def python_command(source: str) -> str:
    encoded = base64.b64encode(source.encode()).decode()
    return f"printf %s {encoded} | base64 -d | python3"


def wait_for_root(vm: VM, mailbox: Path, marker: str, timeout: int = 180):
    """Accept either a configured autologin or Debian's root password prompt."""
    deadline = time.monotonic() + timeout
    password_prompts = 0
    while time.monotonic() < deadline:
        text = vm.text()
        if marker in text:
            return
        count = text.count("Password:")
        if count > password_prompts:
            # ttyAMA0 and hvc0 may both print a prompt, but the mailbox feeds
            # ttyAMA0 only. Send one password for each newly observed round.
            publish(mailbox, "root")
            password_prompts = count
        if vm.proc.poll() is not None:
            break
        time.sleep(0.1)
    raise AssertionError(
        f"Timed out waiting for root shell marker {marker!r}:\n{vm.text()[-5000:]}"
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path, help="signed Cube binary")
    parser.add_argument("--kernel", required=True, type=Path)
    parser.add_argument("--initrd", required=True, type=Path)
    parser.add_argument("--disk", required=True, type=Path)
    parser.add_argument("--seed", type=Path, help="optional NoCloud seed disk")
    parser.add_argument("--only", choices=("both", "vsock", "virtiofs"), default="both")
    parser.add_argument("--ready-marker", default="root@", help="serial text indicating a root shell")
    parser.add_argument("--keep", action="store_true", help="retain logs and the disposable disk")
    args = parser.parse_args()

    for path in (args.binary, args.kernel, args.initrd, args.disk):
        if not path.is_file():
            parser.error(f"file does not exist: {path}")
    if args.seed is not None and not args.seed.is_file():
        parser.error(f"file does not exist: {args.seed}")

    directory = Path(tempfile.mkdtemp(prefix="cube-vsock-virtiofs-"))
    # Darwin limits sockaddr_un.sun_path to 104 bytes.  Suite artifacts live
    # below target/test-results/<run>/..., which is already long enough that a
    # socket beneath `directory` can fail before the VM starts.  Keep sockets
    # in a deliberately short disposable directory while retaining all useful
    # logs and cloned media in the artifact directory.
    socket_directory = Path(tempfile.mkdtemp(prefix="cube-vf-", dir="/tmp"))
    print(f"[verify] Artifacts and live guest log: {directory}", flush=True)
    guest_disk = directory / "guest.raw"
    clone_disk(args.disk.resolve(), guest_disk)
    mailbox = directory / "uart"
    export = directory / "export"
    export.mkdir()
    (export / "host.txt").write_text("host-original\n")
    (export / "outside-link").symlink_to("/etc/passwd")

    echo = UnixEcho(socket_directory / "v.sock") if args.only in ("both", "vsock") else None
    if echo is not None:
        echo.start()

    vm_args = [
        "--linux", args.kernel.resolve(),
        "--initrd", args.initrd.resolve(),
        "--bootargs",
        "console=ttyAMA0 earlycon root=/dev/vda1 rw systemd.firstboot=0 "
        "ipv6.disable=1 net.ifnames=0 systemd.unit=multi-user.target",
        "--disk", guest_disk,
        "--cpus", "2",
        "--ram", "1024",
    ]
    if args.seed is not None:
        vm_args += ["--disk", args.seed.resolve()]
    if args.only in ("both", "virtiofs"):
        vm_args += ["--share", "cubecheck", export, "--share-transport", "virtiofs"]
    if echo is not None:
        vm_args += [
            "--vsock-cid", "42",
            "--vsock-map", f"{VSOCK_PORT}={echo.path}",
        ]

    vm = VM(
        str(args.binary.resolve()),
        directory,
        "guest",
        vm_args,
        env=dict(os.environ, VMM_UART_IN=str(mailbox)),
    )

    def send(command: str):
        publish(mailbox, command)

    def check(command: str, marker: str, timeout=45):
        # The serial console echoes commands. Emit the success marker through
        # octal escapes so its literal text appears only after the command has
        # actually succeeded.
        encoded = "".join(f"\\{ord(char):03o}" for char in marker + "\n")
        send(f"({command}) && printf '{encoded}'")
        vm.wait_for(marker, timeout=timeout)

    try:
        print("[verify] Waiting for Debian root shell...", flush=True)
        wait_for_root(vm, mailbox, args.ready_marker)
        print("[verify] Guest shell ready.", flush=True)

        if echo is not None:
            print("[verify] Testing guest-to-host virtio-vsock echo...", flush=True)
            vsock_guest = f"""
import socket
s = socket.socket(socket.AF_VSOCK, socket.SOCK_STREAM)
s.settimeout(20)
s.connect((socket.VMADDR_CID_HOST, {VSOCK_PORT}))
s.sendall({VSOCK_REQUEST!r})
reply = b''
while len(reply) < {len(VSOCK_REPLY)}:
    part = s.recv(4096)
    if not part:
        break
    reply += part
s.close()
assert reply == {VSOCK_REPLY!r}, reply
print('VSOCK_DATA_OK', flush=True)
"""
            check(
                "modprobe vmw_vsock_virtio_transport 2>/dev/null || true; "
                + python_command(vsock_guest),
                "VSOCK_DATA_OK",
                timeout=60,
            )
            echo.check()
            print("[verify] Virtio-vsock passed.", flush=True)

        if args.only in ("both", "virtiofs"):
            print("[verify] Testing virtio-fs mount/read/read-only behavior...", flush=True)
            check(
                "modprobe virtiofs 2>/dev/null || true; mkdir -p /mnt/cubecheck; "
                "mount -t virtiofs -o ro cubecheck /mnt/cubecheck; "
                "test \"$(cat /mnt/cubecheck/host.txt)\" = host-original; "
                "test \"$(readlink /mnt/cubecheck/outside-link)\" = /etc/passwd; "
                "! touch /mnt/cubecheck/forbidden",
                "VIRTIOFS_READ_RO_OK",
                timeout=60,
            )
            (export / "host.txt").write_text("host-updated\n")
            check(
                "test \"$(cat /mnt/cubecheck/host.txt)\" = host-updated; "
                "umount /mnt/cubecheck",
                "VIRTIOFS_COHERENCE_OK",
            )
            if (export / "forbidden").exists():
                raise AssertionError("read-only virtio-fs export was modified")
            print("[verify] Virtio-fs passed.", flush=True)

        vm.finish(stop=True)
        completed = " and ".join(
            name
            for enabled, name in (
                (args.only in ("both", "vsock"), "vsock guest-to-host echo"),
                (args.only in ("both", "virtiofs"), "virtio-fs read/RO/coherence"),
            )
            if enabled
        )
        print(f"PASS: {completed}; guest networking disabled")
    except BaseException:
        args.keep = True
        print(vm.text()[-8000:])
        raise
    finally:
        vm.cleanup()
        shutil.rmtree(socket_directory, ignore_errors=True)
        if args.keep:
            print(f"Artifacts: {directory}")
        else:
            shutil.rmtree(directory)


if __name__ == "__main__":
    main()
