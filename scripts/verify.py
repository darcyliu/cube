#!/usr/bin/env python3
"""Intel smoke runner.

--suite quick runs library tests, the payload smoke, and the OVMF smoke.
--suite full adds the FreeBSD amd64 installer smoke (--net vmnet-proxy).

The host gate matches the vmm binary: marketing major >= 10, not Darwin
major 26. Darwin 19 is macOS 10.15. Rosetta (sysctl.proc_translated==1 or
hw.optional.arm64==1) is refused. A missing sysctl is not translated.
"""

import argparse
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import sys

REPO = Path(__file__).resolve().parent.parent


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--suite", choices=("quick", "full"), default="quick")
    parser.add_argument("--list", action="store_true", help="print commands and exit")
    return parser.parse_args(argv)


def sysctl_text(name):
    try:
        out = subprocess.run(
            ["sysctl", "-n", name],
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if out.returncode != 0:
        return None
    text = out.stdout.strip()
    return text or None


def sysctl_int(name):
    text = sysctl_text(name)
    if text is None:
        return None
    try:
        return int(text, 10)
    except ValueError:
        return None


def first_component(version):
    token = version.replace("\n", " ").split(" ", 1)[0]
    head = token.split(".", 1)[0]
    if head.isdigit():
        return int(head)
    return None


def marketing_major_from_darwin(darwin_major):
    # Darwin 5 is Mac OS X 10.1; Darwin 19 is macOS 10.15.
    if 5 <= darwin_major <= 19:
        return 10
    # Darwin 20 is macOS 11. Do not treat the Darwin major as the marketing major.
    if darwin_major >= 20:
        return darwin_major - 9
    return darwin_major


def host_marketing_major():
    product = sysctl_text("kern.osproductversion")
    if product is not None:
        major = first_component(product)
        if major is not None:
            return major
    release = sysctl_text("kern.osrelease")
    if release is not None:
        darwin = first_component(release)
        if darwin is not None:
            return marketing_major_from_darwin(darwin)
    try:
        out = subprocess.run(
            ["/usr/bin/sw_vers", "-productVersion"],
            capture_output=True,
            text=True,
            timeout=5,
            check=False,
        )
    except (OSError, subprocess.TimeoutExpired):
        return None
    if out.returncode != 0:
        return None
    return first_component(out.stdout.strip())


def translated_or_arm_host():
    # Absent sysctls mean the process is not translated and not Apple Silicon.
    if sysctl_int("sysctl.proc_translated") == 1:
        return True
    if sysctl_int("hw.optional.arm64") == 1:
        return True
    return False


def gate_error(translated, major):
    if translated:
        return (
            "vmm-intel refuses to run under Rosetta or on Apple Silicon "
            "(sysctl.proc_translated=1 or hw.optional.arm64=1)."
        )
    if major is None:
        return (
            "vmm-intel could not read the macOS version "
            "(kern.osproductversion, kern.osrelease, or sw_vers)."
        )
    if major < 10:
        return (
            "vmm-intel requires macOS marketing major >= 10 "
            "(this host reports %s). Darwin 19 is macOS 10.15." % major
        )
    return None


def preflight(suite):
    error = gate_error(translated_or_arm_host(), host_marketing_major())
    if error:
        raise RuntimeError(error)
    if suite == "full" and os.geteuid() == 0:
        raise RuntimeError(
            "vmnet-proxy rejects root; run verify.py as your user"
        )
    for tool in ("cargo", "codesign"):
        if shutil.which(tool) is None:
            raise RuntimeError("missing prerequisite: %s" % tool)


def commands(suite):
    binary = str(REPO / "target" / "debug" / "vmm")
    cargo = ["cargo", "+1.89.0"]
    steps = [
        ("lib-tests", cargo + ["test", "--lib", "--locked", "--offline"], 900),
        ("build", cargo + ["build", "--locked", "--offline"], 900),
        (
            "sign",
            [
                "codesign",
                "--entitlements",
                str(REPO / "entitlements.plist"),
                "--force",
                "-s",
                "-",
                binary,
            ],
            60,
        ),
        ("payload", [sys.executable, "-B", str(REPO / "scripts" / "check_payload.py")], 60),
        ("ovmf", [sys.executable, "-B", str(REPO / "scripts" / "check_uefi.py")], 180),
    ]
    if suite == "full":
        # The FreeBSD script keeps its own 10 minute guest limit.
        steps.append(
            ("freebsd", [sys.executable, "-B", str(REPO / "scripts" / "check_freebsd.py")], 3600)
        )
    return steps


def run_step(name, command, timeout):
    print("RUN  %s (limit %ss) — %s" % (name, timeout, shlex.join(command)), flush=True)
    proc = subprocess.Popen(command, cwd=REPO, start_new_session=True)
    try:
        code = proc.wait(timeout=timeout)
    except subprocess.TimeoutExpired:
        os.killpg(proc.pid, signal.SIGKILL)
        proc.wait(timeout=10)
        print("FAIL %s timed out after %ss" % (name, timeout), flush=True)
        return 1
    if code != 0:
        print("FAIL %s exit %s" % (name, code), flush=True)
        return code
    print("PASS %s" % name, flush=True)
    return 0


def main(argv=None):
    args = parse_args(argv)
    if args.list:
        for name, command, _timeout in commands(args.suite):
            print("%s: %s" % (name, shlex.join(command)))
        return 0
    try:
        preflight(args.suite)
    except RuntimeError as error:
        print("verify.py: error: %s" % error, file=sys.stderr)
        return 1
    for name, command, timeout in commands(args.suite):
        code = run_step(name, command, timeout)
        if code != 0:
            return code
    return 0


if __name__ == "__main__":
    sys.exit(main())
