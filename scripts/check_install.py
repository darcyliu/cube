#!/usr/bin/env python3
"""Disposable FreeBSD UFS or UEFI/ZFS install, boot, and reboot regression.

Requires Apple Silicon macOS, a signed VMM, and an ARM64 disc1 ISO.
Run with sudo for vmnet. --desktop-environment xfce installs an XFCE desktop
and LightDM into the disposable UFS guest. Failure artifacts are always retained.
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
import sys
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


def find_usb_tablet(execute, listing, deadline):
    # usbconfig's summary uses a VID/PID database. Both emulated devices
    # share those IDs, so only their actual string descriptors distinguish them.
    matches = []
    for device in re.findall(r"(?m)^(ugen\d+\.\d+):", listing):
        result = execute(f"usbconfig -d {device} dump_device_desc", deadline)
        require(result is not None and result.returncode == 0,
                f"Could not read USB descriptors for {device}; see ssh.log")
        if (re.search(r"(?m)^\s*iProduct\s*=.*<VMM USB Tablet>\s*$", result.stdout)
                and re.search(r"(?m)^\s*iSerialNumber\s*=.*<tablet0>\s*$", result.stdout)):
            matches.append(device)
    require(len(matches) == 1,
            f"Expected one VMM USB tablet, found {matches}; see descriptor dumps in ssh.log")
    return matches[0]


def desktop_packages(environment):
    common = "xorg-minimal xf86-video-scfb xf86-input-libinput xinput xdpyinfo"
    return common + (" xfce dbus lightdm lightdm-gtk-greeter dejavu adwaita-icon-theme"
                     if environment == "xfce"
                     else " twm xterm")


def desktop_session(environment):
    if environment == "xfce":
        return ("daemon -f -p /var/run/xfce-vmm.pid -o /var/log/xfce-vmm.log "
                "/usr/bin/env DISPLAY=:0 /bin/sh -c "
                "'. /usr/local/etc/xdg/xfce4/xinitrc'", "xfce4-session")
    return ("daemon -f -p /var/run/twm-vmm.pid -o /var/log/twm-vmm.log "
            "/usr/bin/env DISPLAY=:0 /usr/local/bin/twm && "
            "daemon -f -p /var/run/xterm-vmm.pid -o /var/log/xterm-vmm.log "
            "/usr/bin/env DISPLAY=:0 /usr/local/bin/xterm -geometry 90x28+80+80", "twm")


def desktop_username(value):
    if not re.fullmatch(r"[a-z_][a-z0-9_-]{0,31}", value) or value in ("root", "toor"):
        raise argparse.ArgumentTypeError(
            "desktop username must use 1–32 lowercase letters, digits, '_' or '-', "
            "start with a letter or '_', and not be root/toor")
    return value


def password_from_file(path, default, account):
    if path is None:
        return default
    value = path.read_text(encoding="utf-8")
    if value.endswith("\n"):
        value = value[:-1]
    if value.endswith("\r"):
        value = value[:-1]
    require(value and not any(char in value for char in "\r\n\0"),
            f"{account} password file must contain one nonempty line")
    return value


def write_reuse_config(args, directory):
    """Give a retained desktop install a stable launch recipe for vmmctl."""
    config = {
        "name": directory.name,
        "binary": str(args.binary),
        "boot": {"mode": "freebsd", "kernel": "boot/kernel/kernel", "multiuser": True},
        "disks": [{"path": "installed.img"}],
        "ram_mb": args.ram,
        "cpus": args.cpus,
        "network": {"mode": "vmnet", "ssh_port": 2222},
        "vnc": {"bind": "127.0.0.1:5901", "display": args.display},
        "shutdown": {"host": "127.0.0.1", "port": 2222, "user": "root",
                     "identity_file": "id_ed25519"},
    }
    path = directory / "reuse-vm.json"
    path.write_text(json.dumps(config, indent=2) + "\n")
    return path


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
        password = password_from_file(args.root_password_file, secrets.token_hex(24), "Root")
        root_login = directory / "root-login.txt"
        root_login.write_text(f"Username: root\nPassword: {password}\n")
        root_login.chmod(0o600)
        if args.desktop and args.desktop_environment == "xfce":
            login_password = password_from_file(args.desktop_password_file, password, "Desktop")
            login = directory / "desktop-login.txt"
            login.write_text(f"Username: {args.desktop_user}\nPassword: {login_password}\n")
            login.chmod(0o600)
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

        reservations = []
        for requested in (args.ssh_port, args.vnc_port if args.desktop else None):
            if requested is None:
                continue
            reservation = socket.socket()
            reservation.bind(("127.0.0.1", requested))
            reservations.append(reservation)
        port = reservations[0].getsockname()[1]
        vnc_port = reservations[1].getsockname()[1] if args.desktop else None
        for reservation in reservations:
            reservation.close()
        boot = ["--uefi", args.firmware] if args.filesystem == "zfs" else [
            "--freebsd", directory / "boot/kernel/kernel", "--multiuser"]
        network = ["--net", "none"] if args.console_only else ["--net", "vmnet", "--ssh-port", port]
        display = (["--vnc", f"127.0.0.1:{vnc_port}", "--display", args.display]
                   if args.desktop else [])
        vm = Machine(args.binary, directory, "boot", [*common, *boot, *network, *display])
        machines.append(vm)
        console = ConsoleSession(vm, password)
        ssh = ["ssh", "-F", "/dev/null", "-i", str(key), "-p", str(port),
               "-o", "IdentitiesOnly=yes", "-o", "BatchMode=yes", "-o", "ConnectTimeout=2",
               "-o", "ConnectionAttempts=1", "-o", "StrictHostKeyChecking=accept-new",
               "-o", "UserKnownHostsFile=" + str(directory / "known_hosts"),
               "-o", "GlobalKnownHostsFile=/dev/null", "root@127.0.0.1"]

        def execute(remote, deadline, timeout_cap=10, input_text=None):
            remaining = deadline - time.monotonic()
            require(remaining > 0, "Guest command phase timed out")
            if args.console_only:
                require(input_text is None, "Console commands cannot receive stdin")
                result = console.execute(remote, deadline)
                if result is None:
                    return None
            else:
                try:
                    result = subprocess.run([*ssh, remote], capture_output=True, text=True,
                                            input=input_text, timeout=min(timeout_cap, remaining))
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
        if args.desktop:
            deadline = time.monotonic() + args.desktop_timeout
            # FreeBSD 14 keeps the modern USB HID bridge disabled by default.
            # Direct kernel boot does not read /boot/loader.conf. Enable the
            # bridge at runtime and reset the tablet so it is probed again.
            hid = execute(
                "sysctl hw.usb.usbhid.enable=1 kern.evdev.rcpt_mask=6 && "
                "(kldload hms 2>/dev/null || true) && "
                "usbconfig",
                deadline)
            require(hid is not None and hid.returncode == 0,
                    "Could not enable the FreeBSD USB HID/evdev input path" +
                    (f":\n{hid.stdout}{hid.stderr}" if hid is not None else ""))
            tablet = find_usb_tablet(execute, hid.stdout, deadline)
            reset = execute(f"usbconfig -d {tablet} reset", deadline)
            require(reset is not None and reset.returncode == 0,
                    "Could not reprobe the USB tablet" +
                    (f":\n{reset.stdout}{reset.stderr}" if reset is not None else ""))
            attachment = execute(
                "sleep 2; dmesg | grep -E 'usbhid|hidbus|hms|uhid|ukbd|hkbd'; "
                "usbconfig show_ifdrv", deadline)
            if attachment is not None:
                (directory / "guest-hid.log").write_text(attachment.stdout + attachment.stderr)
            require(attachment is not None and re.search(r"\bhms\d+\b", attachment.stdout),
                    "FreeBSD did not attach hms to the USB tablet; see guest-hid.log" +
                    (f":\n{attachment.stdout[-4000:]}{attachment.stderr[-1000:]}"
                     if attachment is not None else ""))
            packages = execute(
                "env ASSUME_ALWAYS_YES=yes pkg bootstrap -f && "
                f"pkg install -y {desktop_packages(args.desktop_environment)}",
                deadline, args.desktop_timeout)
            require(packages is not None and packages.returncode == 0,
                    "Could not install the FreeBSD X11 desktop packages")
            if args.desktop_environment == "xfce":
                account = execute(
                    f"pw useradd {shlex.quote(args.desktop_user)} -m -G video -s /bin/sh -h 0",
                    deadline, input_text=login_password + "\n")
                require(account is not None and account.returncode == 0,
                        "Could not create the XFCE login user; see ssh.log")
                services = execute(
                    "sysrc dbus_enable=YES lightdm_enable=YES && "
                    "service dbus onestart && "
                    "command -v xfce4-session >/dev/null && "
                    "command -v lightdm >/dev/null && "
                    "test -r /usr/local/etc/xdg/xfce4/xinitrc && "
                    "pkg info -e dejavu && "
                    "test -r /usr/local/share/fonts/dejavu/DejaVuSans.ttf && "
                    "install -d /usr/local/etc/lightdm && "
                    "printf '%s\\n' '[greeter]' 'theme-name=Adwaita' "
                    "'icon-theme-name=Adwaita' 'font-name=DejaVu Sans 11' "
                    ">/usr/local/etc/lightdm/lightdm-gtk-greeter.conf",
                    deadline)
                require(services is not None and services.returncode == 0,
                        "Could not configure XFCE/D-Bus/LightDM; see ssh.log")
                hid_service = Path(__file__).with_name("freebsd-vmm-hid.rc").read_text()
                persist_hid = execute(
                    "cat > /usr/local/etc/rc.d/vmm_hid && "
                    "chmod 555 /usr/local/etc/rc.d/vmm_hid && "
                    "sysrc vmm_hid_enable=YES && service vmm_hid onestart",
                    deadline, input_text=hid_service)
                require(persist_hid is not None and persist_hid.returncode == 0,
                        "Could not install the boot-time USB tablet reprobe; see ssh.log")
            configure = execute(
                "install -d /usr/local/etc/X11/xorg.conf.d && "
                "printf '%s\n' 'Section \"Device\"' '  Identifier \"Card0\"' "
                "'  Driver \"scfb\"' 'EndSection' "
                ">/usr/local/etc/X11/xorg.conf.d/20-scfb.conf && "
                "rm -f /var/run/Xorg-vmm.pid /var/run/twm-vmm.pid "
                "/var/run/xterm-vmm.pid /var/run/xfce-vmm.pid && "
                "(pkill Xorg 2>/dev/null || true) && "
                "daemon -f -p /var/run/Xorg-vmm.pid -o /var/log/Xorg-vmm.log "
                "/usr/local/bin/Xorg :0 -ac -noreset vt9",
                deadline)
            require(configure is not None and configure.returncode == 0,
                    "Could not configure and start Xorg with scfb")
            await_command(
                "env DISPLAY=:0 xdpyinfo >/dev/null 2>&1 && printf VMM_X11_OK",
                "VMM_X11_OK", deadline)
            session_command, session_process = desktop_session(args.desktop_environment)
            desktop = execute(session_command, deadline)
            require(desktop is not None and desktop.returncode == 0,
                    "Could not start the X11 desktop clients")
            await_command(
                f"pgrep -x {session_process} >/dev/null && printf VMM_SESSION_OK",
                "VMM_SESSION_OK", deadline)
            check_log = directory / "desktop-check.log"
            check_command = [
                sys.executable, "-B", Path(__file__).with_name("check_desktop.py"),
                "--vnc", f"127.0.0.1:{vnc_port}",
                "--ssh-port", port,
                "--identity", key,
                "--expect-resolution", args.display,
                "--artifacts", directory / "desktop-results",
            ]
            with check_log.open("wb") as output:
                result = subprocess.run(list(map(str, check_command)), cwd=directory,
                                        stdout=output, stderr=subprocess.STDOUT,
                                        timeout=max(1, deadline - time.monotonic()))
            for remote, name in [
                ("cat /var/log/Xorg-vmm.log", "Xorg-vmm.log"),
                ("cat /var/log/Xorg.0.log", "Xorg.0.log"),
                ("cat /var/log/twm-vmm.log", "twm-vmm.log"),
                ("cat /var/log/xterm-vmm.log", "xterm-vmm.log"),
                ("cat /var/log/xfce-vmm.log", "xfce-vmm.log"),
                ("env DISPLAY=:0 xinput --list --long", "xinput-devices.log"),
                ("dmesg | grep -E 'usbhid|hidbus|hms|hkbd|uhid|ukbd'", "guest-hid-after-xorg.log"),
            ]:
                diagnostic = execute(remote, deadline)
                if diagnostic is not None and diagnostic.returncode == 0:
                    (directory / name).write_text(diagnostic.stdout)
            print(check_log.read_text(), end="", flush=True)
            require(result.returncode == 0,
                    "Desktop input/update verification failed; see desktop-check.log")
            vm.wait_for(lambda: vm.text().count("Session metrics:") >= 2,
                        deadline, "VNC session metrics")
            deadline = time.monotonic() + args.boot_timeout
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
        if args.desktop and args.desktop_environment == "xfce":
            await_command(
                "pkg info -e xfce && "
                "test \"$(sysctl -n hw.usb.usbhid.enable)\" = 1 && "
                "test \"$(sysctl -n kern.evdev.rcpt_mask)\" = 6 && "
                "dmesg | grep -Eq 'hms[0-9]+' && "
                "service dbus onestatus >/dev/null && "
                "service lightdm onestatus >/dev/null && pgrep -x Xorg >/dev/null && "
                "printf VMM_XFCE_BOOT_OK",
                "VMM_XFCE_BOOT_OK", deadline)
            print("PASS installed XFCE, boot-time USB tablet, and LightDM/Xorg", flush=True)
            print(f"Desktop login: {args.desktop_user} "
                  f"(credentials: {directory / 'desktop-login.txt'})",
                  flush=True)
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
    parser.add_argument("--desktop", action="store_true",
                        help="install Xorg/scfb with twm and run the graphical desktop regression")
    parser.add_argument("--desktop-environment", choices=("xfce",),
                        help="install a full desktop environment (implies --desktop)")
    parser.add_argument("--desktop-user", type=desktop_username,
                        help="XFCE login username (default: vmm)")
    parser.add_argument("--desktop-password-file", type=Path,
                        help="UTF-8 file containing the XFCE login password (default: root password)")
    parser.add_argument("--root-password-file", type=Path,
                        help="UTF-8 file containing the root password (default: generated)")
    parser.add_argument("--artifacts", type=Path, help="parent for a new private run directory")
    parser.add_argument("--keep", action="store_true", help="also retain artifacts on success")
    parser.add_argument("--disk-gib", type=positive, default=12)
    parser.add_argument("--ram", type=positive, default=2048)
    parser.add_argument("--cpus", type=positive, default=2)
    parser.add_argument("--ssh-port", type=int, default=0, help="loopback port; default: allocate a free port")
    parser.add_argument("--vnc-port", type=int, default=0,
                        help="desktop VNC loopback port; default: allocate a free port")
    parser.add_argument("--display", default="1024x768",
                        help="desktop framebuffer WIDTHxHEIGHT (default: 1024x768)")
    for phase, default in [("install", 900), ("boot", 180), ("desktop", 1200),
                           ("reboot", 180), ("shutdown", 60)]:
        parser.add_argument(f"--{phase}-timeout", type=positive, default=default)
    args = parser.parse_args()
    if args.desktop_environment:
        args.desktop = True
        args.desktop_user = args.desktop_user or "vmm"
    else:
        if args.desktop_user or args.desktop_password_file:
            parser.error("--desktop-user and --desktop-password-file require --desktop-environment xfce")
        args.desktop_environment = "twm"
    for field, account in (("desktop_password_file", "Desktop"),
                           ("root_password_file", "Root")):
        path = getattr(args, field)
        if path is None:
            continue
        path = path.expanduser().resolve()
        if not path.is_file():
            parser.error(f"{account.lower()} password file does not exist: {path}")
        try:
            password_from_file(path, "", account)
        except (OSError, UnicodeError, RuntimeError) as error:
            parser.error(str(error))
        setattr(args, field, path)
    if args.filesystem == "zfs" and not args.firmware:
        parser.error("--filesystem zfs requires --firmware")
    if args.desktop and args.console_only:
        parser.error("--desktop requires vmnet/SSH; omit --console-only")
    if args.desktop and args.filesystem != "ufs":
        parser.error("--desktop currently requires --filesystem ufs")
    for name in ("binary", "iso", *(["firmware"] if args.firmware else [])):
        path = getattr(args, name).resolve()
        if not path.is_file():
            parser.error(f"{name} is not a file: {path}")
        setattr(args, name, path)
    if not 0 <= args.ssh_port <= 65535 or not 0 <= args.vnc_port <= 65535:
        parser.error("--ssh-port and --vnc-port must be 0..65535")
    if args.desktop and args.ssh_port != 0 and args.ssh_port == args.vnc_port:
        parser.error("--ssh-port and --vnc-port must differ")
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
        if args.keep and args.desktop_environment == "xfce":
            config = write_reuse_config(args, directory)
            print(f"Reusable VM config: {config}", flush=True)
            print("Start later: python3 -B scripts/vmmctl.py start "
                  f"{shlex.quote(str(config))} --sudo", flush=True)
        if not args.keep:
            shutil.rmtree(directory)
        print("PASS full install → boot → reboot regression", flush=True)


if __name__ == "__main__":
    main()
