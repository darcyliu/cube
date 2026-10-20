#!/usr/bin/env python3
"""Install a FreeBSD VM: choose the release, unattended or interactive, and an optional desktop.

14.4 is the default and the release this tree has been tested with. 14.5 uses
the same distribution-set installer. 15.1 (also selected by --version 15)
installs the packaged base system that ships on its disc1 image. 15.0's
scripted installer cannot use that package repository, so 15.0 is interactive
only.

Examples:
  python3 -B scripts/install-vm.py --version 14.4
  python3 -B scripts/install-vm.py --version 15.1 --unattended --name freebsd15
  python3 -B scripts/install-vm.py --version 15 --unattended --desktop plasma --name work
  python3 -B scripts/install-vm.py --version 15 --unattended --desktop plasma --name work --skip-verify

Networking uses the vmnet service (--net vmnet-proxy) and does not need root.
Pass --sudo to run the installer as root with in-process vmnet instead.
Start the service once with `make vmnet-service` before a default install.
A finished install prints a vmmctl start line and the same boot as a direct
vmm command. --skip-verify applies to a desktop install: packages, the login
account, and the Xorg configuration are written, and the X11 input, reboot,
and cold-start checks are skipped.
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


@dataclass(frozen=True)
class Install:
    version: str
    base: str
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
                  dest_root=None, disk_gib=20, ram=None, cpus=None, hostname=None):
    version, (base, scripted, reason) = resolve_version(version)
    if desktop not in (None, "xfce", "plasma"):
        raise ValueError("desktop must be xfce or plasma")
    if (unattended or desktop) and not scripted:
        raise ValueError(reason)
    if desktop and not unattended:
        raise ValueError("a desktop install runs unattended; pass --unattended")
    if name is None:
        # vmmctl names cannot contain the dots in a release number.
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
    root = (dest_root or (Path.home() / "VMs")).expanduser()
    hint = None
    if not unattended and version.startswith("15"):
        hint = ("FreeBSD 15 disc1 installs the base system from packages. "
                "In bsdinstall choose Packages and the offline set, then Auto (UFS) on vtbd0.")
    return Install(version, base, unattended, desktop, name, (root / name).resolve(),
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

    # A finished disc1 image is hundreds of megabytes. A short file is a
    # failed earlier download and must not be reused.
    if destination.is_file() and destination.stat().st_size >= 100 * 1024 * 1024:
        return destination
    partial = cache_dir / (filename + ".partial")
    print(f"Downloading {url}", flush=True)
    run(["curl", "-fL", "--retry", "3", "--progress-bar", "-o", str(partial), url], check=True)
    partial.replace(destination)
    return destination


def command(spec, iso, binary, *, password="root", root_password_file=None,
            desktop_user=None, desktop_password_file=None, display="1280x800",
            sudo=False, skip_verify=False):
    iso, binary = str(iso), str(binary)
    if spec.desktop:
        argv = [sys.executable, "-B", str(SCRIPTS / "check_install.py"), binary,
                "--iso", iso, "--desktop-environment", spec.desktop,
                "--disk-gib", str(spec.disk_gib), "--ram", str(spec.ram),
                "--cpus", str(spec.cpus), "--display", display,
                "--desktop-timeout", "3600", "--keep",
                "--artifacts", str(spec.dest.parent), "--run-name", spec.name,
                "--base", spec.base]
        if desktop_user:
            argv += ["--desktop-user", desktop_user]
        if desktop_password_file:
            argv += ["--desktop-password-file", str(desktop_password_file)]
        if root_password_file:
            argv += ["--root-password-file", str(root_password_file)]
        if skip_verify:
            argv.append("--skip-verify")
        if sudo:
            argv.append("--sudo")
        return argv
    script = "unattended-install.sh" if spec.unattended else "install-from-iso.sh"
    argv = [str(SCRIPTS / script), "--iso", iso,
            "--disk", str(spec.dest / "installed.img"),
            "--size", f"{spec.disk_gib}g", "--cpus", str(spec.cpus),
            "--ram", str(spec.ram), "--vmm", binary]
    if spec.unattended:
        argv += ["--hostname", spec.hostname, "--password", password,
                 "--base", spec.base, "--seed", str(spec.dest / "seed.iso")]
    if sudo:
        argv.append("--sudo")
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


def direct_start(spec, binary, sudo=False, display="1280x800"):
    """Foreground vmm command matching the saved config's boot."""
    parts = ["sudo"] if sudo else []
    parts += [shlex.quote(str(binary)),
              "--freebsd", shlex.quote(str(spec.dest / "boot" / "kernel" / "kernel")),
              "--multiuser",
              "--disk", shlex.quote(str(spec.dest / "installed.img")),
              "--ram", str(spec.ram), "--cpus", str(spec.cpus),
              "--net", "vmnet" if sudo else "vmnet-proxy",
              "--ssh-port", "2222"]
    if spec.desktop:
        parts += ["--vnc", "127.0.0.1:5901", "--display", shlex.quote(display)]
    return " ".join(parts)


def vm_config(spec, binary, identity=None, sudo=False):
    config = {
        "name": spec.name,
        "binary": str(binary),
        "boot": {"mode": "freebsd", "kernel": "boot/kernel/kernel", "multiuser": True},
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
    source = source_root / ".iso-boot" / "boot" / "kernel" / "kernel"
    if not source.is_file():
        raise RuntimeError(f"installer did not extract a kernel: {source}")
    target = dest / "boot" / "kernel" / "kernel"
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(source, target)
    return target


def write_result(spec, binary, iso, identity, source_root=REPO, sudo=False):
    install_kernel(source_root, spec.dest)
    config_path = spec.dest / f"{spec.name}.json"
    config_path.write_text(json.dumps(vm_config(spec, binary, identity, sudo), indent=2) + "\n")
    source = {"version": spec.version, "base": spec.base, "desktop": spec.desktop,
              "unattended": spec.unattended, "iso": str(iso)}
    (spec.dest / "source.json").write_text(json.dumps(source, indent=2) + "\n")
    return config_path


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--version", default="14.4",
                        help="FreeBSD release: 14.4 (default), 14.5, 15.0, 15.1; 15 means 15.1")
    parser.add_argument("--unattended", action="store_true",
                        help="run bsdinstall from a seed ISO instead of the interactive installer")
    parser.add_argument("--desktop", choices=("xfce", "plasma"),
                        help="install this desktop after the base system; requires --unattended")
    parser.add_argument("--name", help="VM name and directory name (default: freebsd-14-4, or freebsd-15-1-plasma)")
    parser.add_argument("--dest", type=Path, help="parent directory for the VM (default: ~/VMs)")
    parser.add_argument("--iso", type=Path, help="use this disc1 ISO instead of downloading --version")
    parser.add_argument("--binary", type=Path, help="signed vmm binary (default: target/debug/cube)")
    parser.add_argument("--cache-dir", type=Path,
                        default=Path.home() / "Library/Caches/Cube")
    parser.add_argument("--disk-gib", type=int, default=20)
    parser.add_argument("--ram", type=int, help="MiB; default 1024, or 4096 with --desktop")
    parser.add_argument("--cpus", type=int, help="default 2, or 4 with --desktop")
    parser.add_argument("--hostname")
    parser.add_argument("--password", help="root password for an unattended base install (default: root)")
    parser.add_argument("--root-password-file", type=Path,
                        help="one-line root password file")
    parser.add_argument("--desktop-user")
    parser.add_argument("--desktop-password-file", type=Path)
    parser.add_argument("--display", help="desktop framebuffer WIDTHxHEIGHT (default: 1280x800)")
    parser.add_argument("--sudo", action="store_true",
                        help="run the installer as root with in-process vmnet "
                             "(default is vmnet-proxy, which needs the vmnet service)")
    parser.add_argument("--skip-verify", action="store_true",
                        help="desktop install only: write packages, the login account, "
                             "and Xorg configuration, then skip input, reboot, and "
                             "cold-start checks")
    args = parser.parse_args(argv)
    try:
        spec = specification(args.version, args.unattended, args.desktop, args.name,
                             args.dest, args.disk_gib, args.ram, args.cpus, args.hostname)
        if args.password is not None and spec.desktop:
            raise ValueError("--password is for a base install; use --root-password-file for a desktop")
        if args.password is not None and args.root_password_file is not None:
            raise ValueError("pass only one of --password and --root-password-file")
        if (args.desktop_user or args.desktop_password_file) and not spec.desktop:
            raise ValueError("--desktop-user and --desktop-password-file require --desktop")
        if args.display and not spec.desktop:
            raise ValueError("--display applies to a desktop install")
        if args.skip_verify and not spec.desktop:
            raise ValueError("--skip-verify applies to a desktop install")
        password = args.password if args.password is not None else "root"
        root_file = None
        if args.root_password_file is not None:
            root_file = args.root_password_file.expanduser().resolve()
            if not root_file.is_file():
                raise ValueError(f"root password file does not exist: {root_file}")
            if not spec.desktop:
                password = one_line_password(root_file)
                root_file = None
        desktop_file = None
        if args.desktop_password_file is not None:
            desktop_file = args.desktop_password_file.expanduser().resolve()
            if not desktop_file.is_file():
                raise ValueError(f"desktop password file does not exist: {desktop_file}")
        if spec.dest.exists():
            raise ValueError(f"destination already exists: {spec.dest}")
        binary = (args.binary or (REPO / "target/debug/cube")).expanduser().resolve()
        if not os.access(binary, os.X_OK):
            raise ValueError(f"Cube binary is missing or not executable: {binary}. Run make all first.")
        iso = args.iso.expanduser().resolve() if args.iso else None
        if iso is None:
            iso = ensure_iso(spec.version, args.cache_dir.expanduser())
        elif not iso.is_file():
            raise ValueError(f"ISO does not exist: {iso}")
    except ValueError as error:
        print(f"install-vm: {error}", file=sys.stderr)
        return 2

    argv = command(spec, iso, binary, password=password, root_password_file=root_file,
                   desktop_user=args.desktop_user, desktop_password_file=desktop_file,
                   display=args.display or "1280x800", sudo=args.sudo,
                   skip_verify=args.skip_verify)
    label = f"FreeBSD {spec.version}" + (f" {spec.desktop}" if spec.desktop else "")
    print(f"Installing {label} into {spec.dest}", flush=True)
    spec.dest.parent.mkdir(parents=True, exist_ok=True)
    if spec.desktop:
        print("Package installation uses the guest network and can take a long time.", flush=True)
        os.execvp(argv[0], argv)
    if spec.hint:
        print(spec.hint, flush=True)
    spec.dest.mkdir(parents=True, exist_ok=True)
    spec.dest.chmod(0o700)
    completed = subprocess.run(argv, cwd=REPO)
    if completed.returncode != 0:
        print(f"install-vm: installer exited {completed.returncode}; left {spec.dest}",
              file=sys.stderr)
        return completed.returncode
    try:
        identity = default_identity(Path.home()) if spec.unattended else None
        config_path = write_result(spec, binary, iso, identity, sudo=args.sudo)
    except (OSError, RuntimeError) as error:
        print(f"install-vm: {error}", file=sys.stderr)
        return 1
    print(f"VM config: {config_path}")
    if spec.unattended and identity is None:
        print("No ~/.ssh/id_ed25519 or id_rsa key pair found; the config has no SSH shutdown entry.")
    start = "cubectl start " + shlex.quote(str(config_path))
    if args.sudo:
        start += " --sudo"
    print("Start later: " + start)
    print("Direct start: " + direct_start(spec, binary, sudo=args.sudo))
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
