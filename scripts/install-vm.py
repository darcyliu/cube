#!/usr/bin/env python3
"""Create or install a FreeBSD VM: choose interactive or unattended setup.

Uses native cubectl to create, partition, install, and manage VM instances.

Examples:
  python3 -B scripts/install-vm.py --version 14.4
  python3 -B scripts/install-vm.py --version 14.4 --unattended
  python3 -B scripts/install-vm.py --version 15.1 --unattended --name freebsd15
  python3 -B scripts/install-vm.py --version 15 --unattended --desktop plasma --name work
  python3 -B scripts/install-vm.py --version 15 --unattended --desktop plasma --name work --skip-verify

Networking uses the vmnet service (--net vmnet-proxy) and does not need root.
Pass --sudo to run the installer as root with in-process vmnet instead.
Start the service once with `make vmnet-service` before a default install.
"""
import argparse
from dataclasses import dataclass
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import subprocess
import sys
import time

REPO = Path(__file__).resolve().parent.parent
SCRIPTS = REPO / "scripts"
NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]{0,47}\Z")
HOST = re.compile(r"[A-Za-z0-9][A-Za-z0-9_-]{0,62}\Z")

# disc1 for 15.0 and later carries a package repository and not base.txz.
# 15.1's `bsdinstall script` installs that repository when DISTRIBUTIONS is unset.
# 15.0's script always unpacks distribution sets, which that disc does not contain.
RELEASES = {
    "14.4": ("distsets", True, None),
    "14.5": ("distsets", True, None),
    "15.0": ("pkgbase", False,
             "FreeBSD 15.0 disc1 has no distribution sets, and its scripted "
             "installer cannot install the packaged base on that disc. "
             "Use --version 15.1 for an unattended install, or run 15.0 "
             "without --unattended and choose Packages in bsdinstall."),
    "15.1": ("pkgbase", True, None),
}
ALIASES = {"15": "15.1"}

COMMON_PACKAGES = "xorg-minimal xf86-video-scfb xf86-input-libinput xinput xdpyinfo xdotool"
DESKTOP_PACKAGES = {
    "xfce": f"{COMMON_PACKAGES} xfce dbus lightdm lightdm-gtk-greeter dejavu adwaita-icon-theme",
    "plasma": f"{COMMON_PACKAGES} plasma6-plasma konsole dbus sddm dejavu",
}


@dataclass(frozen=True)
class Install:
    version: str
    base: str
    filesystem: str
    unattended: bool
    desktop: str | None
    name: str
    dest: Path
    disk_gib: int
    ram: int
    cpus: int
    hostname: str
    hint: str | None


def resolve_version(version):
    resolved = ALIASES.get(version, version)
    info = RELEASES.get(resolved)
    if info is None:
        known = ", ".join([*RELEASES, *ALIASES])
        raise ValueError(f"unknown FreeBSD version {version!r}; choose {known}")
    return resolved, info


def specification(version="14.4", unattended=False, desktop=None, name=None,
                  dest_root=None, disk_gib=20, ram=None, cpus=None, hostname=None,
                  filesystem="zfs"):
    version, (base, scripted, reason) = resolve_version(version)
    if desktop not in (None, "xfce", "plasma"):
        raise ValueError("desktop must be xfce or plasma")
    if filesystem not in ("zfs", "ufs"):
        raise ValueError("filesystem must be zfs or ufs")
    if (unattended or desktop) and not scripted:
        raise ValueError(reason)
    if desktop and not unattended:
        raise ValueError("a desktop install runs unattended; pass --unattended")
    if name is None:
        slug = version.replace(".", "-")
        name = f"freebsd-{slug}-{desktop}" if desktop else f"freebsd-{slug}"
    if not NAME.fullmatch(name):
        raise ValueError("name must be 1–48 letters, digits, underscores, or hyphens")
    if not isinstance(disk_gib, int) or not 1 <= disk_gib <= 2048:
        raise ValueError("disk size must be 1–2048 GiB")
    if ram is None:
        ram = 4096 if desktop else 1024
    if cpus is None:
        cpus = 4 if desktop else 2
    if not isinstance(ram, int) or not 128 <= ram <= 65536:
        raise ValueError("ram must be 128–65536 MiB")
    if not isinstance(cpus, int) or not 1 <= cpus <= 16:
        raise ValueError("cpus must be 1–16")
    if hostname is None:
        hostname = name
    if not HOST.fullmatch(hostname):
        raise ValueError("hostname must be 1–63 letters, digits, underscores, or hyphens")
    root = (dest_root or (Path.home() / "Cube")).expanduser()
    hint = None
    if not unattended and version.startswith("15"):
        hint = ("FreeBSD 15 disc1 installs the base system from packages. "
                "In bsdinstall choose Packages and the offline set, then Auto (UFS) on vtbd0.")
    return Install(version, base, filesystem, unattended, desktop, name, (root / name).resolve(),
                   disk_gib, ram, cpus, hostname, hint)


def iso_location(version):
    filename = f"FreeBSD-{version}-RELEASE-arm64-aarch64-disc1.iso"
    url = ("https://download.freebsd.org/releases/arm64/aarch64/ISO-IMAGES/"
           f"{version}/{filename}")
    return filename, url


def ensure_iso(version, cache_dir, run=subprocess.run):
    filename, url = iso_location(version)
    cache_dir.mkdir(parents=True, exist_ok=True)
    destination = cache_dir / filename

    legacy_file = Path.home() / "Library/Caches/vmm" / filename
    if not destination.exists() and legacy_file.is_file():
        try:
            legacy_file.replace(destination)
        except OSError:
            destination = legacy_file

    if destination.is_file() and destination.stat().st_size >= 100 * 1024 * 1024:
        return destination
    partial = cache_dir / (filename + ".partial")
    print(f"Downloading {url}", flush=True)
    run(["curl", "-fL", "--retry", "3", "--progress-bar", "-o", str(partial), url], check=True)
    partial.replace(destination)
    return destination


def find_cubectl(explicit=None):
    if explicit:
        return Path(explicit).expanduser().resolve()
    env_bin = os.environ.get("CUBECTL_BIN")
    if env_bin and Path(env_bin).is_file():
        return Path(env_bin).resolve()
    for target in ("debug", "release"):
        candidate = REPO / "target" / target / "cubectl"
        if candidate.is_file():
            return candidate.resolve()
    which = shutil.which("cubectl")
    if which:
        return Path(which).resolve()
    return REPO / "target/debug/cubectl"


def command(spec, iso=None, binary=None, *, password="root", root_password_file=None,
            desktop_user=None, desktop_password_file=None, display="1280x800",
            sudo=False, skip_verify=False, cubectl=None):
    ctl = find_cubectl(cubectl)
    argv = [str(ctl), "create", spec.name, "--dest", str(spec.dest)]
    if iso:
        argv += ["--iso", str(iso)]
    else:
        argv += ["--version", spec.version]
    if spec.unattended:
        argv.append("--unattended")
        effective_pwd = password
        if root_password_file:
            effective_pwd = one_line_password(root_password_file)
        argv += ["--password", effective_pwd]
        argv += ["--hostname", spec.hostname]
        argv += ["--base", spec.base]
        argv += ["--filesystem", spec.filesystem]
        if spec.filesystem == "ufs":
            # cubectl copies the ISO kernel into the VM directory before it
            # builds the final configuration, so it can resolve this direct
            # kernel boot without an explicit --kernel path.
            argv += ["--boot", "freebsd"]
    if spec.desktop:
        argv += ["--vnc", "127.0.0.1:5901", "--display", display]
    argv += ["--disk", f"installed.img:{spec.disk_gib}G",
             "--ram", str(spec.ram), "--cpus", str(spec.cpus)]
    if binary:
        argv += ["--binary", str(binary)]
    if sudo:
        argv += ["--net", "vmnet"]
    return argv


def one_line_password(path):
    value = path.read_text(encoding="utf-8")
    if value.endswith("\n"):
        value = value[:-1]
    if value.endswith("\r"):
        value = value[:-1]
    if not value or any(char in value for char in "\r\n\0"):
        raise ValueError(f"password file must contain one nonempty line: {path}")
    return value


def default_identity(home):
    ssh = home / ".ssh"
    for name in ("id_ed25519", "id_rsa"):
        private = ssh / name
        if private.is_file() and (ssh / (name + ".pub")).is_file():
            return private
    return None


def vm_config(spec, binary, identity=None, sudo=False):
    config = {
        "name": spec.name,
        "binary": str(binary),
        "boot": {"mode": "freebsd", "kernel": "kernel", "multiuser": True},
        "disks": [{"path": "installed.img"}],
        "ram_mb": spec.ram,
        "cpus": spec.cpus,
        "network": {"mode": "vmnet" if sudo else "vmnet-proxy", "ssh_port": 2222},
    }
    if identity is not None:
        config["shutdown"] = {"host": "127.0.0.1", "port": 2222, "user": "root",
                              "identity_file": str(identity)}
    return config


def install_kernel(source_root, dest):
    for candidate in (
        source_root / "boot" / "freebsd" / "kernel",
        source_root / ".iso-boot" / "boot" / "kernel" / "kernel",
        source_root / "boot" / "kernel" / "kernel",
        source_root / "kernel",
    ):
        if candidate.is_file():
            target = dest / "kernel"
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(candidate, target)
            return target
    raise RuntimeError(f"installer did not extract a kernel under: {source_root}")


def write_result(spec, binary, iso, identity, source_root=REPO, sudo=False):
    install_kernel(source_root, spec.dest)
    config_path = spec.dest / f"{spec.name}.json"
    config_path.write_text(json.dumps(vm_config(spec, binary, identity, sudo), indent=2) + "\n")
    source = {"version": spec.version, "base": spec.base, "filesystem": spec.filesystem,
              "desktop": spec.desktop,
              "unattended": spec.unattended, "iso": str(iso)}
    (spec.dest / "source.json").write_text(json.dumps(source, indent=2) + "\n")
    return config_path


def provision_desktop(spec, cubectl, desktop_user="cube", password="cube",
                      display="1280x800", skip_verify=False):
    print(f"Starting {spec.name} to provision {spec.desktop} desktop...", flush=True)
    subprocess.run([str(cubectl), "start", spec.name], check=True)
    try:
        def run_ssh(remote_cmd, input_text=None, timeout=1800, retries=3):
            cmd = [str(cubectl), "ssh", spec.name, "-w", "--wait-timeout", "180", "--", remote_cmd]
            for attempt in range(1, retries + 1):
                try:
                    subprocess.run(cmd, input=input_text, text=True, check=True, timeout=timeout)
                    return
                except subprocess.CalledProcessError:
                    if attempt == retries:
                        raise
                    print(f"[Cube] SSH command failed (attempt {attempt}/{retries}), retrying in 3s...", flush=True)
                    time.sleep(3)

        print("Bootstrapping package manager...", flush=True)
        run_ssh("env ASSUME_ALWAYS_YES=yes pkg bootstrap -f")
        print(f"Installing {spec.desktop} packages (this may take a while)...", flush=True)
        run_ssh(f"pkg install -y {DESKTOP_PACKAGES[spec.desktop]}", timeout=3600)

        print(f"Configuring desktop user '{desktop_user}'...", flush=True)
        run_ssh(f"pw useradd {shlex.quote(desktop_user)} -m -G video,wheel -s /bin/sh -h 0",
                input_text=password + "\n")

        print("Enabling display manager and system services...", flush=True)
        if spec.desktop == "xfce":
            run_ssh("sysrc dbus_enable=YES lightdm_enable=YES && service dbus onestart && "
                    "install -d /usr/local/etc/lightdm && "
                    "printf '%s\\n' '[greeter]' 'theme-name=Adwaita' 'icon-theme-name=Adwaita' "
                    "'font-name=DejaVu Sans 11' > /usr/local/etc/lightdm/lightdm-gtk-greeter.conf")
        elif spec.desktop == "plasma":
            run_ssh("sysrc dbus_enable=YES sddm_enable=YES && service dbus onestart && "
                    "install -d /usr/local/etc/sddm.conf.d && "
                    "printf '%s\\n' '[General]' 'DisplayServer=x11' > /usr/local/etc/sddm.conf.d/10-cube.conf")

        hid_rc_path = REPO / "scripts/freebsd-cube-hid.rc"
        if hid_rc_path.is_file():
            run_ssh("cat > /usr/local/etc/rc.d/cube_hid && chmod 555 /usr/local/etc/rc.d/cube_hid && "
                    "sysrc cube_hid_enable=YES", input_text=hid_rc_path.read_text())

        run_ssh("install -d /usr/local/etc/X11/xorg.conf.d && "
                "printf '%s\\n' 'Section \"Device\"' '  Identifier \"Card0\"' '  Driver \"scfb\"' 'EndSection' "
                "> /usr/local/etc/X11/xorg.conf.d/20-scfb.conf")
    finally:
        print("Shutting down VM cleanly...", flush=True)
        subprocess.run([str(cubectl), "shutdown", spec.name, "--timeout", "120"], check=False)


def print_next_steps(spec, root_password=None, desktop_user=None, desktop_password=None):
    print()
    if spec.unattended:
        label = f"FreeBSD {spec.version}" + (f" with {spec.desktop} desktop" if spec.desktop else "")
        print(f"✓ {label} installation finished successfully!")
        print("Login credentials:")
        print("  Root user:     root")
        print(f"  Root password: {root_password}")
        if spec.desktop:
            print(f"  Desktop user:     {desktop_user}")
            print(f"  Desktop password: {desktop_password}")
        print(f"Start VM:  cubectl start {spec.name}")
        return

    print(f"✓ FreeBSD {spec.version} VM created for interactive installation.")
    print("Complete the installation using the VNC address printed above:")
    print(f"  1. cubectl start {spec.name}")
    print("  2. Connect a VNC viewer and finish the FreeBSD installer.")
    print(f"  3. cubectl stop {spec.name}")
    print(f"  4. cubectl eject {spec.name}")
    print(f"  5. cubectl start {spec.name}")


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--version", default="14.4",
                        help="FreeBSD release: 14.4 (default), 14.5, 15.0, 15.1; 15 means 15.1")
    parser.add_argument("--unattended", action="store_true",
                        help="run automated installation instead of interactive installer")
    parser.add_argument("--filesystem", choices=("zfs", "ufs"), default="zfs",
                        help="filesystem and installed boot path: zfs uses UEFI, ufs uses direct kernel boot (default: zfs)")
    parser.add_argument("--desktop", choices=("xfce", "plasma"),
                        help="install this desktop after the base system; requires --unattended")
    parser.add_argument("--name", help="VM name and directory name (default: freebsd-14-4, or freebsd-15-1-plasma)")
    parser.add_argument("--dest", type=Path, help="parent directory for the VM (default: ~/Cube)")
    parser.add_argument("--iso", type=Path, help="use this disc1 ISO instead of downloading --version")
    parser.add_argument("--binary", type=Path, help="signed cube binary (default: target/debug/cube)")
    parser.add_argument("--cubectl", type=Path, help="cubectl binary (default: target/debug/cubectl)")
    parser.add_argument("--cache-dir", type=Path, default=Path.home() / "Library/Caches/Cube")
    parser.add_argument("--disk-gib", type=int, default=20)
    parser.add_argument("--ram", type=int, help="MiB; default 1024, or 4096 with --desktop")
    parser.add_argument("--cpus", type=int, help="default 2, or 4 with --desktop")
    parser.add_argument("--hostname")
    parser.add_argument("--password", help="root password for an unattended base install (default: root)")
    parser.add_argument("--root-password-file", type=Path, help="one-line root password file")
    parser.add_argument("--desktop-user", default="cube")
    parser.add_argument("--desktop-password-file", type=Path)
    parser.add_argument("--display", default="1280x800", help="desktop framebuffer WIDTHxHEIGHT (default: 1280x800)")
    parser.add_argument("--sudo", action="store_true",
                        help="run the installer as root with in-process vmnet "
                             "(default is vmnet-proxy, which needs the vmnet service)")
    parser.add_argument("--skip-verify", action="store_true",
                        help="desktop install only: skip input, reboot, and cold-start verification")
    args = parser.parse_args(argv)
    try:
        spec = specification(args.version, args.unattended, args.desktop, args.name,
                             args.dest, args.disk_gib, args.ram, args.cpus, args.hostname,
                             filesystem=args.filesystem)
        if args.password is not None and spec.desktop:
            raise ValueError("--password is for a base install; use --root-password-file for a desktop")
        if args.password is not None and args.root_password_file is not None:
            raise ValueError("pass only one of --password and --root-password-file")
        if args.desktop_password_file and not spec.desktop:
            raise ValueError("--desktop-password-file requires --desktop")
        if args.display != "1280x800" and not spec.desktop:
            raise ValueError("--display applies to a desktop install")
        if args.skip_verify and not spec.desktop:
            raise ValueError("--skip-verify applies to a desktop install")
        password = args.password if args.password is not None else "root"
        if args.root_password_file is not None:
            root_file = args.root_password_file.expanduser().resolve()
            if not root_file.is_file():
                raise ValueError(f"root password file does not exist: {root_file}")
            password = one_line_password(root_file)
        desktop_pwd = "cube"
        if args.desktop_password_file is not None:
            desktop_file = args.desktop_password_file.expanduser().resolve()
            if not desktop_file.is_file():
                raise ValueError(f"desktop password file does not exist: {desktop_file}")
            desktop_pwd = one_line_password(desktop_file)
        if spec.dest.exists():
            raise ValueError(f"destination already exists: {spec.dest}")
        cubectl_bin = find_cubectl(args.cubectl)
        if not os.access(cubectl_bin, os.X_OK):
            raise ValueError(f"cubectl binary is missing or not executable: {cubectl_bin}. Run cargo build --bin cubectl first.")
        binary = (args.binary or (REPO / "target/debug/cube")).expanduser().resolve()
        if not os.access(binary, os.X_OK):
            raise ValueError(f"Cube binary is missing or not executable: {binary}. Run make all first.")
        iso = args.iso.expanduser().resolve() if args.iso else None
        if iso is not None and not iso.is_file():
            raise ValueError(f"ISO does not exist: {iso}")
    except ValueError as error:
        print(f"install-vm: {error}", file=sys.stderr)
        return 2

    ctl_argv = command(spec, iso, binary, password=password,
                       desktop_user=args.desktop_user, display=args.display,
                       sudo=args.sudo, skip_verify=args.skip_verify, cubectl=cubectl_bin)

    label = f"FreeBSD {spec.version}" + (f" with {spec.desktop} desktop" if spec.desktop else "")
    print(f"Installing {label} into {spec.dest} using cubectl...", flush=True)

    completed = subprocess.run(ctl_argv)
    if completed.returncode != 0:
        print(f"install-vm: cubectl create failed ({completed.returncode})", file=sys.stderr)
        return completed.returncode

    if spec.desktop:
        provision_desktop(spec, cubectl_bin, desktop_user=args.desktop_user,
                          password=desktop_pwd, display=args.display,
                          skip_verify=args.skip_verify)

    print_next_steps(spec, root_password=password, desktop_user=args.desktop_user,
                     desktop_password=desktop_pwd)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
