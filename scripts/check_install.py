#!/usr/bin/env python3
"""Disposable FreeBSD UFS or UEFI/ZFS install, boot, and reboot regression.

Requires Apple Silicon macOS, a signed VMM, and an ARM64 disc1 ISO.
Run with sudo for vmnet. Failure artifacts are always retained.
"""
import argparse
import hashlib
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


def escaped(text):
    return "".join(f"\\{ord(c):03o}" for c in text)


class ConsoleSession:
    """Command execution for offline firmware tests, with fresh login per boot."""
    def __init__(self, vm, password):
        self.vm, self.password = vm, password
        self.logged_in = False

    def execute(self, remote, deadline):
        vm = self.vm
        if not self.logged_in:
            vm.wait_for(lambda: "login:" in vm.text(True), deadline, "console login")
            offset = len(vm.text(True))
            vm.send("root", deadline)
            vm.wait_for(lambda: "Password:" in vm.text(True)[offset:], deadline, "password prompt")
            vm.send(self.password, deadline)
            vm.wait_for(lambda: re.search(r"(?:^|\n)[^\n]*[#$] ", vm.text(True)[offset:]),
                        deadline, "logged-in shell")
            self.logged_in = True
        begin, end = ("VMM_" + secrets.token_hex(12) for _ in range(2))
        offset = len(vm.text(True))
        script = (f"printf '\\n{escaped(begin)}\\n'; sh -c {shlex.quote(remote)}; "
                  f"rc=$?; printf '\\n{escaped(end)}:%s\\n' \"$rc\"")
        vm.send("sh -c " + shlex.quote(script), deadline)
        # shutdown may close the session before the shell prints its result.
        if remote.startswith("/sbin/shutdown"):
            self.logged_in = False
            return None
        vm.wait_for(lambda: re.search(r"(?:^|\n)" + end + r":\d+\n", vm.text(True)[offset:]),
                    deadline, "console command result")
        output = vm.text(True)[offset:]
        output = output.split("\n" + begin + "\n", 1)[1]
        stdout, status = output.rsplit("\n" + end + ":", 1)
        return subprocess.CompletedProcess(remote, int(status.splitlines()[0]), stdout, "")


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
        password = secrets.token_hex(24)
        command(["bash", Path(__file__).with_name("create-seed.sh").resolve(),
                 "--output", directory / "seed.iso", "--ssh-key", key.with_suffix(".pub"),
                 "--hostname", "vmm-install-test", "--password", password,
                 "--filesystem", args.filesystem])
        command(["tar", "-xf", args.iso, "-C", directory, "boot/kernel/kernel"])
        disk = directory / "installed.img"
        # Exclusive creation, always inside our newly allocated private directory.
        with disk.open("xb") as output:
            output.truncate(args.disk_gib * 1024**3)
        common = ["--disk", disk,
                  "--ram", args.ram, "--cpus", args.cpus,
                  "--efi-vars", directory / "efi-vars.fd"]
        vm = Machine(args.binary, directory, "install", [*common,
            "--freebsd", directory / "boot/kernel/kernel", "--net", "none",
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
        encoded = escaped(marker)
        load_zfs = "kldload /boot/kernel/zfs.ko && " if args.filesystem == "zfs" else ""
        vm.send("mdmfs -s 64M md1 /tmp && mdmfs -s 64M md2 /var && "
                "mount -t cd9660 /dev/vtbd1 /media && "
                + load_zfs +
                "bsdinstall script /media/installerconfig </dev/null && "
                f"printf '\\n{encoded}\\n' && poweroff", deadline)
        vm.wait_for(lambda: line_present(vm.text(True), marker), deadline, "installer success result")
        vm.powered_off(deadline)
        print("PASS unattended installation and clean poweroff", flush=True)

        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", args.ssh_port))
            port = reservation.getsockname()[1]
        boot = ["--uefi", args.firmware] if args.filesystem == "zfs" else [
            "--freebsd", directory / "boot/kernel/kernel", "--multiuser"]
        network = ["--net", "none"] if args.console_only else ["--net", "vmnet", "--ssh-port", port]
        vm = Machine(args.binary, directory, "boot", [*common, *boot, *network])
        machines.append(vm)
        console = ConsoleSession(vm, password)
        ssh = ["ssh", "-F", "/dev/null", "-i", str(key), "-p", str(port),
               "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes", "-o", "ConnectTimeout=2",
               "-o", "ConnectionAttempts=1", "-o", "StrictHostKeyChecking=accept-new",
               "-o", "UserKnownHostsFile=" + str(directory / "known_hosts"),
               "-o", "GlobalKnownHostsFile=/dev/null", "root@127.0.0.1"]

        def execute(remote, deadline):
            remaining = deadline - time.monotonic()
            require(remaining > 0, "Guest command phase timed out")
            if args.console_only:
                result = console.execute(remote, deadline)
                if result is None:
                    return None
            else:
                try:
                    result = subprocess.run([*ssh, remote], capture_output=True, text=True,
                                            timeout=min(10, remaining))
                except subprocess.TimeoutExpired:
                    return None
            with (directory / ("console-commands.log" if args.console_only else "ssh.log")).open("a") as log:
                log.write(f"$ {remote}\nexit={result.returncode}\n{result.stdout}{result.stderr}\n")
            return result

        def await_command(remote, expected, deadline):
            def ready():
                result = execute(remote, deadline)
                return result is not None and result.returncode == 0 and result.stdout == expected
            vm.wait_for(ready, deadline, "guest command execution")

        deadline = time.monotonic() + args.boot_timeout
        await_command("printf VMM_COMMAND_OK", "VMM_COMMAND_OK", deadline)
        info = execute("freebsd-version -ku; uname -a", deadline)
        require(info is not None and info.returncode == 0, "Cannot record guest version")
        (directory / "guest-version.txt").write_text(info.stdout)
        def verify_zfs(deadline):
            await_command("test \"$(mount -p | awk '$2 == \"/\" {print $1, $3}')\" = "
                      "'zroot/ROOT/default zfs' && "
                      "test \"$(zpool list -H -o health zroot)\" = ONLINE && "
                      "test -c /dev/efi && printf VMM_ZFS_OK",
                      "VMM_ZFS_OK", deadline)
        if args.filesystem == "zfs":
            verify_zfs(deadline)
        token = secrets.token_hex(32)
        marker_path = "/root/vmm-regression-marker"
        await_command(f"printf '%s' {shlex.quote(token)} > {marker_path} && sync && cat {marker_path}",
                  token, deadline)
        efi_name = "8af39783-6174-4a20-94fe-05dcbb512701-VmmRegression"
        efi_read = f"efivar -p -N -b -n {efi_name}"
        if args.filesystem == "zfs":
            await_command(f"printf '%s' {token} | efivar -w -t 7 -n {efi_name}; " + efi_read,
                      token, deadline)
            boot_order = execute("efibootmgr", deadline)
            require(boot_order is not None and boot_order.returncode == 0, "Cannot read EFI boot configuration")
            order = re.search(r"BootOrder\s*:.*", boot_order.stdout)
            require(order is not None, "EFI BootOrder missing")
            boot_order_line = order.group(0)
            (directory / "efi-boot-config.txt").write_text(boot_order.stdout)
        before = execute("sysctl -n kern.boottime", deadline)
        require(before is not None and before.returncode == 0 and before.stdout.strip(),
                "Could not read guest boot time")
        access = "console" if args.console_only else "SSH"
        print(f"PASS installed disk boots without media and executes {access} commands", flush=True)
        reset_count = vm.text().count("Guest requested PSCI SYSTEM_RESET")
        boot_banner = "The FreeBSD Project."
        boot_count = vm.text().count(boot_banner)
        deadline = time.monotonic() + args.reboot_timeout
        # SSH can disconnect before reporting success; require PSCI reset instead.
        execute("/sbin/shutdown -r now", deadline)
        vm.wait_for(lambda: vm.text().count("Guest requested PSCI SYSTEM_RESET") > reset_count,
                    deadline, "guest reboot")
        if args.console_only:
            # The VMM recreates/truncates its serial log on reset. Wait for the
            # new kernel in the append-only process log before looking for login.
            vm.wait_for(lambda: vm.text().count(boot_banner) > boot_count,
                        deadline, "new FreeBSD kernel")
            console = ConsoleSession(vm, password)
        await_command(f"test \"$(sysctl -n kern.boottime)\" != {shlex.quote(before.stdout.strip())} "
                  f"&& cat {marker_path}", token, deadline)
        if args.filesystem == "zfs":
            verify_zfs(deadline)
            await_command(efi_read, token, deadline)
        print(f"PASS guest reboot, new boot time, persistent marker, and returning {access}", flush=True)
        deadline = time.monotonic() + args.shutdown_timeout
        execute("/sbin/shutdown -p now", deadline)
        vm.powered_off(deadline)
        print("PASS final clean poweroff", flush=True)
        if args.filesystem == "zfs":
            store = directory / "efi-vars.fd"
            require(store.is_file() and store.stat().st_size > 0, "Missing persistent EFI store")
            digest = hashlib.sha256(store.read_bytes()).hexdigest()
            (directory / "efi-store-before-cold-start.sha256").write_text(digest + "\n")
            vm = Machine(args.binary, directory, "cold-start", [*common, *boot, *network])
            machines.append(vm)
            console = ConsoleSession(vm, password)
            deadline = time.monotonic() + args.boot_timeout
            await_command(f"cat {marker_path}", token, deadline)
            verify_zfs(deadline)
            await_command(efi_read, token, deadline)
            cold_order = execute("efibootmgr", deadline)
            require(cold_order is not None and cold_order.returncode == 0
                    and boot_order_line in cold_order.stdout.splitlines(),
                    "EFI BootOrder changed across cold start")
            deadline = time.monotonic() + args.shutdown_timeout
            execute("/sbin/shutdown -p now", deadline)
            vm.powered_off(deadline)
            print("PASS UEFI/ZFS cold start with the same EFI store and persistent marker", flush=True)
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
    parser.add_argument("--filesystem", choices=("ufs", "zfs"), default="ufs")
    parser.add_argument("--firmware", type=Path, help="EDK2 firmware required for ZFS")
    parser.add_argument("--console-only", action="store_true", help="offline UART checks instead of SSH/vmnet")
    parser.add_argument("--artifacts", type=Path, help="parent for a new private run directory")
    parser.add_argument("--keep", action="store_true", help="also retain artifacts on success")
    parser.add_argument("--disk-gib", type=positive, default=12)
    parser.add_argument("--ram", type=positive, default=2048)
    parser.add_argument("--cpus", type=positive, default=2)
    parser.add_argument("--ssh-port", type=int, default=0, help="loopback port; default: allocate a free port")
    for phase, default in [("install", 900), ("boot", 180), ("reboot", 180), ("shutdown", 60)]:
        parser.add_argument(f"--{phase}-timeout", type=positive, default=default)
    args = parser.parse_args()
    if args.filesystem == "zfs" and not args.firmware:
        parser.error("--filesystem zfs requires --firmware")
    for name in ("binary", "iso", *(["firmware"] if args.firmware else [])):
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
        inputs = {}
        for name in ("binary", "iso", "firmware"):
            path = getattr(args, name)
            if path is not None:
                digest = hashlib.sha256()
                with path.open("rb") as source:
                    for chunk in iter(lambda: source.read(1024 * 1024), b""):
                        digest.update(chunk)
                inputs[name] = {"path": str(path), "sha256": digest.hexdigest()}
        (directory / "inputs.json").write_text(json.dumps(inputs, indent=2))
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
