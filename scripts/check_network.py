#!/usr/bin/env python3
"""Check network startup errors; optionally verify SSH from read-only FreeBSD media.

python3 scripts/check_network.py BINARY
python3 scripts/check_network.py BINARY --kernel KERNEL --iso ISO \
    --guest-cidr 192.168.18.77/24 --host-ip 192.168.18.1

The no-argument check is unprivileged and does not need the vmnet service.
The guest check starts the VMM with --net vmnet-proxy. Start the service with
`make vmnet-service` and run this script as that user. Pass --sudo to re-run
as root with in-process --net vmnet. Pass --net bridge or --net bridge:en0 to
re-run as root with in-process bridge; use an unused LAN address. Do not sudo
the script yourself for a proxy guest: the service rejects uid 0.

Choose an unused guest address on the vmnet shared subnet, or on the LAN for
bridge. The guest check opens no writable disk. In-process --net vmnet uses
the VMM's host PF/NAT setup. In-process bridge does not.
"""
import argparse
import ipaddress
import os
from pathlib import Path
import re
import re
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


def guest_net(sudo, requested):
    mode = "vmnet" if sudo and requested is None else (requested or "vmnet-proxy")
    iface = mode.removeprefix("bridge:")
    valid = mode in ("vmnet-proxy", "vmnet", "bridge") or (
        mode.startswith("bridge:") and iface != mode
        and re.fullmatch(r"[A-Za-z][A-Za-z0-9]{0,15}", iface) is not None)
    return mode, valid


def in_process_net(mode):
    return mode == "vmnet" or mode == "bridge" or mode.startswith("bridge:")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary")
    parser.add_argument("--kernel")
    parser.add_argument("--iso")
    parser.add_argument("--guest-cidr", type=ipaddress.IPv4Interface)
    parser.add_argument("--host-ip", type=ipaddress.IPv4Address)
    parser.add_argument("--console-log", type=Path,
                        help="retain the guest serial console at this path")
    parser.add_argument("--sudo", action="store_true",
                        help="re-run the guest check as root with in-process vmnet")
    parser.add_argument("--net",
                        help="vmnet-proxy (default), vmnet, bridge, or bridge:IFACE")
    args = parser.parse_args()
    guest_options = [args.kernel, args.iso, args.guest_cidr, args.host_ip]
    if any(guest_options) and not all(guest_options):
        parser.error("guest check requires --kernel, --iso, --guest-cidr, and --host-ip")
    if (args.sudo or args.net) and not args.kernel:
        parser.error("--sudo and --net apply to the guest check")
    mode, valid = guest_net(args.sudo, args.net)
    if not valid:
        parser.error("--net must be vmnet-proxy, vmnet, bridge, or bridge:IFACE")
    if args.sudo and mode != "vmnet":
        parser.error("--sudo selects in-process vmnet")
    if args.kernel and in_process_net(mode) and os.geteuid() != 0:
        os.execvp("sudo", ["sudo", sys.executable, "-B", __file__, *sys.argv[1:]])
    if args.kernel and not in_process_net(mode) and os.geteuid() == 0:
        parser.error("vmnet-proxy rejects root; re-run as your user, or pass --sudo "
                     "for in-process vmnet, or --net bridge[:IFACE] for in-process bridge")
    binary = str(Path(args.binary).resolve())
    with tempfile.TemporaryDirectory(prefix="vmm-network-") as temp:
        directory = Path(temp)
        image = kernel(directory, "idle", "b .\n")
        # Keep the guest's mode. Reusing that name here made the live VM
        # start with the missing-tap backend from the last probe.
        for probe, message in [("invalid-mode", "Unknown network mode"),
                               ("tap:" + str(directory / "missing-tap"), "Cannot start tap backend")]:
            result = subprocess.run([binary, "--kernel", str(image), "--net", probe],
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
        console = args.console_log or directory / "guest.console.log"
        vm = VM(binary, directory, "guest", ["--freebsd", args.kernel,
            "--cdrom", args.iso, "--root", "cd9660:/dev/vtbd0", "--net", mode,
            "--cpus", 2, "--serial-log", console,
            "--forward", f"127.0.0.1:{port}={args.guest_cidr.ip}:22222"],
            env=dict(os.environ, VMM_UART_IN=str(mailbox)))
        try:
            vm.wait_for("RETURN for /bin/sh", timeout=90)
            publish(mailbox, "")
            vm.wait_for("root@", timeout=30)
            command = " && ".join([
                "mdmfs -s 64M md1 /tmp", "mdmfs -s 64M md2 /var",
                "install -d -m 0755 /var/empty",
                f"ifconfig vtnet0 inet {args.guest_cidr} up",
                f"ping -c 2 {args.host_ip}",
                "ssh-keygen -q -t ed25519 -N '' -f /tmp/hostkey",
                f"printf '%s\\n' {shlex.quote(public)} > /tmp/authorized_keys",
                "/usr/sbin/sshd -f /dev/null -h /tmp/hostkey -p 22222 "
                "-o PidFile=/tmp/sshd.pid -o AuthorizedKeysFile=/tmp/authorized_keys "
                "-o StrictModes=no -o PermitRootLogin=yes -o PasswordAuthentication=no -o UsePAM=no",
            ])
            # Escape the marker's first byte so terminal echo cannot look like
            # successful completion. Report the first failing setup command.
            publish(mailbox, command +
                    "; rc=$?; printf '\\n\\126MM_NETWORK_READY:%s\\n' \"$rc\"")
            deadline = time.monotonic() + 60
            while True:
                output = console.read_text(errors="replace") if console.exists() else ""
                match = re.search(r"(?m)^VMM_NETWORK_READY:(\d+)\r*$", output)
                if match:
                    assert match.group(1) == "0", "Guest network/sshd setup failed"
                    break
                assert vm.proc.poll() is None, "VM exited during guest network/sshd setup"
                assert time.monotonic() < deadline, "Guest network/sshd setup timed out"
                time.sleep(0.2)
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
            if console.exists():
                print("Guest console:\n" + console.read_text(errors="replace")[-8000:])
            print("VMM log:\n" + vm.text()[-3000:])
            raise
        finally:
            vm.cleanup()


if __name__ == "__main__":
    main()
