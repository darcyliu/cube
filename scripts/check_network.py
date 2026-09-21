#!/usr/bin/env python3
"""Check network startup errors; optionally verify SSH from read-only FreeBSD media.

python3 scripts/check_network.py BINARY
sudo python3 scripts/check_network.py BINARY --kernel KERNEL --iso ISO \
    --guest-cidr 192.168.18.77/24 --host-ip 192.168.18.1

Choose an unused guest address on the vmnet shared subnet. The guest check opens
no writable disk, but vmnet uses the VMM's existing host PF/NAT setup.
"""
import argparse
import ipaddress
import os
from pathlib import Path
import shlex
import socket
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
from check_vm_lifecycle import VM, kernel


def publish(path, command):
    deadline = time.monotonic() + 10
    while path.exists():
        if time.monotonic() > deadline:
            raise AssertionError("UART command was not consumed")
        time.sleep(0.05)
    pending = path.with_suffix(".next")
    pending.write_text(command + "\n")
    pending.replace(path)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary")
    parser.add_argument("--kernel")
    parser.add_argument("--iso")
    parser.add_argument("--guest-cidr", type=ipaddress.IPv4Interface)
    parser.add_argument("--host-ip", type=ipaddress.IPv4Address)
    args = parser.parse_args()
    guest_options = [args.kernel, args.iso, args.guest_cidr, args.host_ip]
    if any(guest_options) and not all(guest_options):
        parser.error("guest check requires --kernel, --iso, --guest-cidr, and --host-ip")
    binary = str(Path(args.binary).resolve())
    with tempfile.TemporaryDirectory(prefix="vmm-network-") as temp:
        directory = Path(temp)
        image = kernel(directory, "idle", "b .\n")
        for mode, message in [("invalid-mode", "Unknown network mode"),
                              ("tap:" + str(directory / "missing-tap"), "Cannot start tap backend")]:
            result = subprocess.run([binary, "--kernel", str(image), "--net", mode],
                                    capture_output=True, text=True, timeout=15)
            assert result.returncode != 0 and message in result.stderr, result.stdout + result.stderr
            assert "Initialized user-mode mock" not in result.stdout
        print("PASS unknown/failed backend exits without mock fallback", flush=True)
        # Binding errors must reach the CLI before guest execution begins.
        with socket.socket() as occupied:
            occupied.bind(("127.0.0.1", 0))
            occupied.listen()
            port = occupied.getsockname()[1]
            result = subprocess.run([binary, "--kernel", str(image), "--net", "none",
                "--forward", f"127.0.0.1:{port}=127.0.0.1:22"],
                capture_output=True, text=True, timeout=15)
            assert result.returncode != 0 and "Cannot bind TCP forward" in result.stderr, result.stderr
            assert "Entering interactive console" not in result.stdout
        print("PASS occupied forwarding port fails startup", flush=True)
        if not args.kernel:
            return
        key = directory / "id_ed25519"
        subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(key)], check=True)
        public = key.with_suffix(".pub").read_text().strip()
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        mailbox = directory / "uart"
        vm = VM(binary, directory, "guest", ["--freebsd", args.kernel,
            "--cdrom", args.iso, "--root", "cd9660:/dev/vtbd0", "--net", "vmnet",
            "--cpus", 2, "--forward", f"127.0.0.1:{port}={args.guest_cidr.ip}:22222"],
            env=dict(os.environ, VMM_UART_IN=str(mailbox)))
        try:
            vm.wait_for("RETURN for /bin/sh", timeout=90)
            publish(mailbox, "")
            vm.wait_for("root@", timeout=30)
            command = " && ".join([
                "mdmfs -s 64M md1 /tmp", "mdmfs -s 64M md2 /var",
                f"ifconfig vtnet0 inet {args.guest_cidr} up",
                f"ping -c 2 {args.host_ip}",
                "ssh-keygen -q -t ed25519 -N '' -f /tmp/hostkey",
                f"printf '%s\\n' {shlex.quote(public)} > /tmp/authorized_keys",
                "/usr/sbin/sshd -f /dev/null -h /tmp/hostkey -p 22222 "
                "-o PidFile=/tmp/sshd.pid -o AuthorizedKeysFile=/tmp/authorized_keys "
                "-o StrictModes=no -o PermitRootLogin=yes -o PasswordAuthentication=no -o UsePAM=no",
            ])
            publish(mailbox, command)
            ssh = ["ssh", "-F", "/dev/null", "-i", str(key), "-p", str(port),
                   "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes", "-o", "ConnectTimeout=2",
                   "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=" + str(directory / "known_hosts"),
                   "root@127.0.0.1", "printf VMM_SSH_OK"]
            deadline = time.monotonic() + 60
            while True:
                result = subprocess.run(ssh, capture_output=True, text=True, timeout=10)
                if result.returncode == 0 and result.stdout == "VMM_SSH_OK":
                    break
                if time.monotonic() > deadline:
                    raise AssertionError(result.stderr)
                time.sleep(1)
            vm.finish(stop=True)
            print("PASS guest connectivity and SSH through alternate guest address/port", flush=True)
        except BaseException:
            print(vm.text()[-6000:])
            raise
        finally:
            vm.cleanup()


if __name__ == "__main__":
    main()
