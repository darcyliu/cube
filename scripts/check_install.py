#!/usr/bin/env python3
"""Disposable FreeBSD UFS install, SSH boot, and persistent reboot regression.

Requires Apple Silicon macOS, a signed VMM, and an ARM64 disc1 ISO.
Run with sudo for vmnet. Failure artifacts are always retained.
"""
import argparse
import json
import os
from pathlib import Path
import re
import secrets
import shlex
import shutil
import signal
import socket
import subprocess
import tempfile
import time


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def line_present(text, marker):
    # A command echoed by the guest terminal is not a completion result.
    return marker in text.splitlines()


class Machine:
    def __init__(self, binary, directory, name, args):
        self.console = directory / f"{name}.console.log"
        self.path = directory / f"{name}.vmm.log"
        self.mailbox = directory / f"{name}.uart"
        command = [str(binary), *map(str, args), "--serial-log", str(self.console)]
        (directory / f"{name}.command.json").write_text(json.dumps(command, indent=2))
        self.log = self.path.open("wb")
        try:
            self.proc = subprocess.Popen(command, cwd=directory, stdin=subprocess.DEVNULL,
                stdout=self.log, stderr=subprocess.STDOUT, start_new_session=True,
                env=dict(os.environ, VMM_UART_IN=str(self.mailbox)))
        except BaseException:
            self.log.close()
            raise

    def text(self, console=False):
        path = self.console if console else self.path
        return path.read_text(errors="replace") if path.exists() else ""

    def wait_for(self, predicate, deadline, description):
        while True:
            if predicate():
                return
            require(self.proc.poll() is None, f"VM exited while waiting for {description}")
            require(time.monotonic() < deadline, f"Timed out waiting for {description}")
            time.sleep(0.2)

    def send(self, command, deadline):
        self.wait_for(lambda: not self.mailbox.exists(), deadline, "UART mailbox")
        pending = self.mailbox.with_suffix(".next")
        pending.write_text(command + "\n")
        pending.replace(self.mailbox)

    def powered_off(self, deadline):
        code = self.proc.wait(timeout=max(0.01, deadline - time.monotonic()))
        require(code == 0 and "Guest requested PSCI SYSTEM_OFF" in self.text(),
                f"Expected clean guest poweroff, exit={code}")

    def cleanup(self):
        if self.proc.poll() is None:
            os.killpg(self.proc.pid, signal.SIGTERM)
            try:
                self.proc.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(self.proc.pid, signal.SIGKILL)
                self.proc.wait(timeout=10)
        self.log.close()


def run(args, directory):
    machines = []
    def command(argv, timeout=120):
        with (directory / "prepare.log").open("ab") as log:
            subprocess.run(list(map(str, argv)), cwd=directory, stdout=log,
                           stderr=subprocess.STDOUT, check=True, timeout=timeout)

    try:
        key = directory / "id_ed25519"
        command(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", key])
        command(["bash", Path(__file__).with_name("create-seed.sh").resolve(),
                 "--output", directory / "seed.iso", "--ssh-key", key.with_suffix(".pub"),
                 "--hostname", "vmm-install-test", "--password", secrets.token_hex(24)])
        command(["tar", "-xf", args.iso, "-C", directory, "boot/kernel/kernel"])
        disk = directory / "installed.img"
        # Exclusive creation, always inside our newly allocated private directory.
        with disk.open("xb") as output:
            output.truncate(args.disk_gib * 1024**3)
        common = ["--freebsd", directory / "boot/kernel/kernel", "--disk", disk,
                  "--ram", args.ram, "--cpus", args.cpus,
                  "--efi-vars", directory / "efi-vars.fd"]
        vm = Machine(args.binary, directory, "install", [*common, "--net", "none",
            "--disk", str(directory / "seed.iso") + ",ro", "--cdrom", args.iso,
            "--root", "cd9660:/dev/vtbd2"])
        machines.append(vm)
        deadline = time.monotonic() + args.install_timeout
        vm.wait_for(lambda: "RETURN for /bin/sh" in vm.text(True), deadline, "installer shell selection")
        vm.send("", deadline)
        vm.wait_for(lambda: re.search(r"(?:^|\s)(?:\S*root@\S*)?#(?:\s|$)", vm.text(True)),
                    deadline, "installer shell prompt")
        marker = "VMM_INSTALL_OK_" + secrets.token_hex(16)
        # Keep the literal marker out of echoed terminal input.
        encoded = "".join(f"\\{ord(c):03o}" for c in marker)
        vm.send("mdmfs -s 64M md1 /tmp && mdmfs -s 64M md2 /var && "
                "mount -t cd9660 /dev/vtbd1 /media && "
                "bsdinstall script /media/installerconfig </dev/null && "
                f"printf '\\n{encoded}\\n' && poweroff", deadline)
        vm.wait_for(lambda: line_present(vm.text(True), marker), deadline, "installer success result")
        vm.powered_off(deadline)
        print("PASS unattended installation and clean poweroff", flush=True)

        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", args.ssh_port))
            port = reservation.getsockname()[1]
        vm = Machine(args.binary, directory, "boot", [*common, "--net", "vmnet",
                     "--multiuser", "--ssh-port", port])
        machines.append(vm)
        ssh = ["ssh", "-F", "/dev/null", "-i", str(key), "-p", str(port),
               "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes", "-o", "ConnectTimeout=2",
               "-o", "ConnectionAttempts=1", "-o", "StrictHostKeyChecking=accept-new",
               "-o", "UserKnownHostsFile=" + str(directory / "known_hosts"),
               "-o", "GlobalKnownHostsFile=/dev/null", "root@127.0.0.1"]

        def execute(remote, deadline):
            remaining = deadline - time.monotonic()
            require(remaining > 0, "SSH phase timed out")
            try:
                result = subprocess.run([*ssh, remote], capture_output=True, text=True,
                                        timeout=min(10, remaining))
            except subprocess.TimeoutExpired:
                return None
            with (directory / "ssh.log").open("a") as log:
                log.write(f"$ {remote}\nexit={result.returncode}\n{result.stdout}{result.stderr}\n")
            return result

        def await_ssh(remote, expected, deadline):
            def ready():
                result = execute(remote, deadline)
                return result is not None and result.returncode == 0 and result.stdout == expected
            vm.wait_for(ready, deadline, "SSH command execution")

        deadline = time.monotonic() + args.boot_timeout
        await_ssh("printf VMM_SSH_OK", "VMM_SSH_OK", deadline)
        token = secrets.token_hex(32)
        marker_path = "/root/vmm-regression-marker"
        await_ssh(f"printf '%s' {shlex.quote(token)} > {marker_path} && sync && cat {marker_path}",
                  token, deadline)
        before = execute("sysctl -n kern.boottime", deadline)
        require(before is not None and before.returncode == 0 and before.stdout.strip(),
                "Could not read guest boot time")
        print("PASS installed disk boots without media and executes SSH commands", flush=True)
        reset_count = vm.text().count("Guest requested PSCI SYSTEM_RESET")
        deadline = time.monotonic() + args.reboot_timeout
        # SSH can disconnect before reporting success; require PSCI reset instead.
        execute("/sbin/shutdown -r now", deadline)
        vm.wait_for(lambda: vm.text().count("Guest requested PSCI SYSTEM_RESET") > reset_count,
                    deadline, "guest reboot")
        await_ssh(f"test \"$(sysctl -n kern.boottime)\" != {shlex.quote(before.stdout.strip())} "
                  f"&& cat {marker_path}", token, deadline)
        print("PASS guest reboot, new boot time, persistent marker, and returning SSH", flush=True)
        deadline = time.monotonic() + args.shutdown_timeout
        execute("/sbin/shutdown -p now", deadline)
        vm.powered_off(deadline)
        print("PASS final clean poweroff", flush=True)
    finally:
        for vm in machines:
            vm.cleanup()


def positive(value):
    result = int(value)
    if result <= 0:
        raise argparse.ArgumentTypeError("must be positive")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--iso", required=True, type=Path)
    parser.add_argument("--artifacts", type=Path, help="parent for a new private run directory")
    parser.add_argument("--keep", action="store_true", help="also retain artifacts on success")
    parser.add_argument("--disk-gib", type=positive, default=12)
    parser.add_argument("--ram", type=positive, default=2048)
    parser.add_argument("--cpus", type=positive, default=2)
    parser.add_argument("--ssh-port", type=int, default=0, help="loopback port; default: allocate a free port")
    for phase, default in [("install", 900), ("boot", 180), ("reboot", 180), ("shutdown", 60)]:
        parser.add_argument(f"--{phase}-timeout", type=positive, default=default)
    args = parser.parse_args()
    for name in ("binary", "iso"):
        path = getattr(args, name).resolve()
        if not path.is_file():
            parser.error(f"{name} is not a file: {path}")
        setattr(args, name, path)
    if not 0 <= args.ssh_port <= 65535:
        parser.error("--ssh-port must be 0..65535")
    directory = Path(tempfile.mkdtemp(prefix="vmm-install-", dir=args.artifacts)).resolve()
    print(f"Artifacts: {directory}", flush=True)
    (directory / "run.json").write_text(json.dumps(vars(args), default=str, indent=2))
    try:
        run(args, directory)
    except BaseException as error:
        (directory / "failure.txt").write_text(f"{type(error).__name__}: {error}\n")
        print(f"FAIL: {error}\nArtifacts retained: {directory}", flush=True)
        raise
    else:
        if not args.keep:
            shutil.rmtree(directory)
        print("PASS full install → boot → reboot regression", flush=True)


if __name__ == "__main__":
    main()
