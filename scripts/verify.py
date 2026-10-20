#!/usr/bin/env python3
"""Run Cube checks sequentially, retaining logs and JSON/JUnit reports for CI."""
import argparse
from dataclasses import dataclass
import ipaddress
import json
import os
from pathlib import Path
import platform
import pwd
import re
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import xml.etree.ElementTree as ET

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "scripts"))
from support.network import detect_lan_network, detect_vmnet_subnet

# Let each existing script execute its finally/cleanup blocks on CI cancellation.
# In particular, check_install's VMs use their own process sessions.
PYTHON_CHECK = (
    "import os,runpy,signal,sys; "
    "signal.signal(signal.SIGTERM, lambda *_: "
    "(_ for _ in ()).throw(KeyboardInterrupt())); "
    "sys.argv=sys.argv[1:]; sys.path.insert(0,os.path.dirname(os.path.dirname(sys.argv[0]))); "
    "sys.path.insert(0,os.path.dirname(sys.argv[0])); "
    "runpy.run_path(sys.argv[0],run_name='__main__')"
)

# sudo -u would reset the environment. Dropping in this process keeps it, then
# execs the real command as SUDO_UID. A root unittest process that calls
# verify.main would otherwise setuid itself and lose its temporary directory.
RUN_AS_INVOKING_USER = (
    "import os,sys;"
    "sys.path.insert(0,sys.argv[1]);"
    "import verify;"
    "verify.drop_privileges();"
    "os.execvpe(sys.argv[2], sys.argv[2:], os.environ)"
)


@dataclass
class Step:
    name: str
    command: list
    needs: tuple = ()
    timeout: int = 300
    privileged: bool = False
    as_user: bool = False
    component: str = "shared"
    status: str = "pending"
    seconds: float = 0
    message: str = ""
    log: str = ""


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--component", choices=("cube", "cubectl", "all"), default="all")
    parser.add_argument("--cubectl", type=Path, help="use an existing native manager binary")
    parser.add_argument("--suite", choices=("quick", "offline", "full"), default="quick")
    parser.add_argument("--iso", type=Path, help="ARM64 disc1 ISO; required for offline/full")
    parser.add_argument("--firmware", type=Path, help="EDK2 firmware; required for offline/full")
    parser.add_argument("--binary", type=Path, help="use an existing signed binary; skip build/sign")
    parser.add_argument("--target-dir", type=Path,
                        default=Path(os.environ.get("CARGO_TARGET_DIR", REPO / "target")))
    parser.add_argument("--artifacts", type=Path, default=REPO / "target/test-results",
                        help="parent for a new private run directory (default: target/test-results)")
    parser.add_argument("--sudo", action="store_true",
                        help="run full-suite guest checks as root with in-process vmnet "
                             "(default is vmnet-proxy)")
    parser.add_argument("--keep-root", action="store_true",
                        help="stay root when started with sudo, instead of dropping to "
                             "SUDO_UID. sudo -n then needs no terminal ticket. Cargo's "
                             "target directory stays owned by root. The result directory "
                             "and the Python unit tests are handed back to SUDO_UID")
    parser.add_argument("--bridge", nargs="?", const="", default=None, metavar="IFACE",
                        help="run full-suite guest checks as root with in-process bridge. "
                             "Optional Ethernet name, such as en0; bare --bridge uses the host default")
    parser.add_argument("--guest-cidr", type=ipaddress.IPv4Interface,
                        help="unused static guest address on the vmnet or LAN subnet; auto-detected when omitted")
    parser.add_argument("--host-ip", type=ipaddress.IPv4Address,
                        help="reachable host address on that subnet; auto-detected when omitted")
    parser.add_argument("--list", action="store_true", help="print selected commands without running")
    args = parser.parse_args(argv)
    if args.suite != "quick" and not (args.iso and args.firmware):
        parser.error("offline/full requires --iso and --firmware")
    if args.suite != "full" and (args.sudo or args.bridge is not None or args.guest_cidr or args.host_ip):
        parser.error("--sudo, --bridge, --guest-cidr, and --host-ip apply only to --suite full")
    if args.sudo and args.bridge is not None:
        parser.error("pass --sudo for in-process vmnet, or --bridge for in-process bridge")
    if args.bridge and not re.fullmatch(r"[A-Za-z][A-Za-z0-9]{0,15}", args.bridge):
        parser.error("--bridge expects an interface name such as en0")
    if args.suite == "full" and args.component != "cubectl":
        if bool(args.guest_cidr) != bool(args.host_ip):
            parser.error("specify both --guest-cidr and --host-ip, or omit both to auto-detect")
        elif not (args.guest_cidr and args.host_ip):
            if args.bridge is not None:
                try:
                    name, label, guest_cidr, host_ip, warning = detect_lan_network(args.bridge or None)
                    if args.bridge == "":
                        args.bridge = name
                    args.guest_cidr = guest_cidr
                    args.host_ip = host_ip
                    if warning:
                        print(f"[verify] {warning}", file=sys.stderr)
                    print(f"[verify] Auto-detected LAN network: host {args.host_ip}, guest {args.guest_cidr} on {name} ({label})", file=sys.stderr)
                except Exception as e:
                    parser.error(f"cannot auto-detect LAN network for bridge: {e}")
            else:
                args.guest_cidr, args.host_ip = detect_vmnet_subnet()
                print(f"[verify] Auto-detected vmnet network: host {args.host_ip}, guest {args.guest_cidr}", file=sys.stderr)
    if args.suite == "quick" and (args.iso or args.firmware):
        parser.error("select --suite offline or full to use guest media")
    if args.guest_cidr and args.host_ip not in args.guest_cidr.network:
        parser.error("--host-ip must be on the --guest-cidr subnet")
    if args.guest_cidr and args.guest_cidr.ip == args.host_ip:
        parser.error("guest and host addresses must differ")
    if args.guest_cidr and args.guest_cidr.ip in (args.guest_cidr.network.network_address,
                                               args.guest_cidr.network.broadcast_address):
        parser.error("--guest-cidr must identify a usable guest address")
    for name in ("iso", "firmware", "binary", "cubectl", "target_dir", "artifacts"):
        if getattr(args, name) is not None:
            setattr(args, name, getattr(args, name).expanduser().resolve())
    return args


def make_plan(args, directory):
    binary = args.binary or args.target_dir / "debug/cube"
    def check(name, script, extra=(), needs=(), timeout=300, privileged=False):
        component = "cubectl" if script in ("check_vm_management.py", "check_vm_shutdown.py", "check_installation.py") else "cube"
        return Step(name, [sys.executable, "-u", "-B", "-c", PYTHON_CHECK,
                          str(REPO / "scripts" / component / script), *map(str, extra)],
                    needs, timeout, privileged, component=component)
    rust_targets = {
        "all": [],
        "cube": ["--lib", "--bin", "cube", "--bin", "cube-overlay", "--test", "overlay_cli"],
        "cubectl": ["--bin", "cubectl", "--test", "cubectl_cli", "--test", "cubectl_regressions"],
    }[args.component]
    steps = [
        Step("rust-debug", ["cargo", "test", "--locked", *rust_targets], timeout=900),
        Step("rust-release", ["cargo", "test", "--locked", "--release", *rust_targets], timeout=900),
    ]
    manager = args.cubectl or args.target_dir / "debug/cubectl"
    manager_needs = ()
    if args.component != "cube":
        if not args.cubectl:
            steps.append(Step("build-cubectl", ["cargo", "build", "--locked", "--bin", "cubectl"], timeout=900))
            manager_needs = ("build-cubectl",)
    steps.append(Step("python-tests", [sys.executable, "-u", "-B", "-m", "unittest", "discover",
                                      "-s", "scripts", "-p", "test_*.py", "-v"],
                      needs=manager_needs, as_user=True))
    if args.binary:
        steps.append(Step("signed-binary", ["codesign", "--verify", "--strict", str(binary)]))
    else:
        steps.extend([
            Step("build", ["cargo", "build", "--locked"], timeout=900),
            Step("signed-binary", ["codesign", "--entitlements", "entitlements.plist",
                                   "--force", "-s", "-", str(binary)], needs=("build",)),
        ])
    ready = ("signed-binary",)
    lifecycle = [binary]
    if args.suite != "quick":
        kernel = directory / "boot/kernel/kernel"
        steps.append(Step("extract-kernel", ["tar", "-xf", str(args.iso), "-C", str(directory),
                                              "boot/kernel/kernel"], timeout=120))
        lifecycle += ["--kernel", kernel, "--firmware", args.firmware]
        ready += ("extract-kernel",)
    steps.extend([
        check("lifecycle", "check_vm_lifecycle.py", lifecycle, ready),
        check("network-errors", "check_network.py", [binary], ("signed-binary",)),
    ])
    if args.suite != "quick":
        steps.append(check("installer-console", "check_unattended_console.py",
                           [binary, kernel, args.iso], ready))
        steps.append(check("shared-folders", "check_shared_folders.py",
                           [binary, "--kernel", kernel, "--iso", args.iso], ready,
                           timeout=300))
        steps.append(check("vm-management", "check_vm_management.py",
                           [binary, "--cubectl", manager, "--kernel", kernel, "--iso", args.iso],
                           (*ready, *manager_needs), timeout=240))
        for fs in ("ufs", "zfs"):
            extra = [binary, "--iso", args.iso, "--artifacts", directory,
                     "--filesystem", fs, "--console-only"]
            if fs == "zfs":
                extra += ["--firmware", args.firmware]
            steps.append(check(f"install-{fs}-offline", "check_install.py", extra,
                               ("signed-binary",), timeout=1800))
    if args.suite == "full":
        privileged = args.sudo or args.bridge is not None
        if privileged:
            steps.append(Step("sudo-access", ["sudo", "-n", "true"], timeout=10))
            ready += ("sudo-access",)
        guest_net = (["--net", "bridge" if args.bridge == "" else "bridge:" + args.bridge]
                     if args.bridge is not None else ["--sudo"] if args.sudo else [])
        guest = [binary, "--kernel", kernel, "--iso", args.iso,
                 "--guest-cidr", args.guest_cidr, "--host-ip", args.host_ip,
                 "--console-log", directory / "network-guest-ssh.console.log", *guest_net]
        steps.append(check("network-guest-ssh", "check_network.py", guest, ready,
                           privileged=privileged))
        for fs in ("ufs", "zfs"):
            extra = [binary, "--iso", args.iso, "--artifacts", directory, "--filesystem", fs]
            if fs == "ufs":
                extra += ["--desktop"]
            if fs == "zfs":
                extra += ["--firmware", args.firmware]
            extra += guest_net
            steps.append(check(f"install-{fs}-ssh", "check_install.py", extra, ready,
                               timeout=3000 if fs == "ufs" else 1800, privileged=privileged))
    if args.suite != "quick" and args.component != "cube":
        native_net = ("none" if args.suite == "offline" else
                      "bridge" if args.bridge == "" else
                      "bridge:" + args.bridge if args.bridge else
                      "vmnet" if args.sudo else "vmnet-proxy")
        for fs in ("ufs", "zfs"):
            steps.append(check(f"cubectl-install-{fs}", "check_installation.py",
                               [binary, "--cubectl", manager, "--iso", args.iso,
                                "--firmware", args.firmware, "--filesystem", fs,
                                "--artifacts", directory, "--net", native_net],
                               (*ready, *manager_needs), timeout=1500,
                               privileged=args.suite == "full" and (args.sudo or args.bridge is not None)))
    steps = [step for step in steps if step.component in ("shared", args.component)
             or args.component == "all"]
    # Keep only prerequisites reachable from selected checks and always-run tests.
    if args.component == "cubectl":
        roots = {step.name for step in steps if step.component == "cubectl"
                 or step.name in ("rust-debug", "rust-release", "python-tests", "runner-tests")}
        by_name = {step.name: step for step in steps}
        pending = list(roots)
        while pending:
            for dependency in by_name[pending.pop()].needs:
                if dependency not in roots:
                    roots.add(dependency)
                    pending.append(dependency)
        steps = [step for step in steps if step.name in roots]
    return steps


def drop_privileges():
    """Return a sudo launch to the invoking user before any files are created.

    In-process vmnet and bridge still re-enter root later, and only for the
    guest check that executes vmm. vmnet-proxy must run as this user.
    """
    if os.geteuid() != 0:
        return
    try:
        uid = int(os.environ["SUDO_UID"])
        gid = int(os.environ["SUDO_GID"])
        if not 0 < uid < 2**32 - 1 or not 0 <= gid < 2**32 - 1:
            raise ValueError("invalid sudo identity")
        account = pwd.getpwuid(uid)
    except (KeyError, ValueError) as error:
        raise RuntimeError(
            "This process is root without a usable SUDO_UID. Run verify.py as "
            "your user. --sudo and --bridge elevate only the in-process guest checks."
        ) from error
    os.initgroups(account.pw_name, gid)
    os.setgid(gid)
    os.setuid(uid)
    os.environ["HOME"] = account.pw_dir
    os.environ["USER"] = account.pw_name
    os.environ["LOGNAME"] = account.pw_name


def preflight(args):
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise RuntimeError("VMM verification requires native Apple Silicon macOS (not Rosetta)")
    for tool in ("cargo", "clang", "dtc", "codesign", "bash", "ssh-keygen"):
        if not shutil.which(tool):
            raise RuntimeError(f"Missing prerequisite: {tool}")
    if args.suite != "quick":
        for tool in ("tar", "hdiutil", "ssh"):
            if not shutil.which(tool):
                raise RuntimeError(f"Missing prerequisite: {tool}")
    for name in ("iso", "firmware", "binary", "cubectl"):
        path = getattr(args, name)
        if path is not None and not path.is_file():
            raise RuntimeError(f"{name} is not a file: {path}")
    if args.binary and not os.access(args.binary, os.X_OK):
        raise RuntimeError(f"Binary is not executable: {args.binary}")
    if os.environ.get("PYTHONOPTIMIZE") or sys.flags.optimize:
        raise RuntimeError("Unset PYTHONOPTIMIZE and do not use python -O: existing checks use assert")


def stop(proc, privileged=False):
    if proc.poll() is not None:
        return
    if privileged:
        # sudo itself forwards SIGINT to its command, including without a tty.
        subprocess.run(["sudo", "-n", "kill", "-INT", str(proc.pid)],
                       timeout=5, check=False, stdout=subprocess.DEVNULL)
    else:
        proc.send_signal(signal.SIGINT)
    try:
        proc.wait(timeout=30)  # Give script finally blocks time to stop their VMs.
    except subprocess.TimeoutExpired:
        if privileged:
            subprocess.run(["sudo", "-n", "kill", "-KILL", f"-{proc.pid}"],
                           timeout=5, check=False, stdout=subprocess.DEVNULL)
        else:
            os.killpg(proc.pid, signal.SIGKILL)
        proc.wait(timeout=10)


def run_step(step, args, directory):
    step.log = str(directory / (step.name + ".log"))
    env = dict(os.environ, CUBECTL_BIN=str(args.cubectl or args.target_dir / "debug/cubectl"),
               CARGO_TARGET_DIR=str(args.target_dir), PYTHONUNBUFFERED="1",
               PYTHONDONTWRITEBYTECODE="1")
    # Keep temporary resources from older scripts inside this run too.
    env["TMPDIR"] = str(directory)
    command = list(step.command)
    privileged = step.privileged and args.sudo
    if privileged:
        command = ["sudo", "-n", "/usr/bin/env", "TMPDIR=" + str(directory),
                   "PATH=" + os.environ["PATH"], *command]
    elif step.as_user and os.geteuid() == 0:
        command = [sys.executable, "-B", "-c", RUN_AS_INVOKING_USER,
                   str(REPO / "scripts"), *command]
    print(f"RUN  {step.name} (limit {step.timeout}s) — {step.log}", flush=True)
    start = time.monotonic()
    proc = None
    try:
        with open(step.log, "wb") as output:
            output.write((shlex.join(command) + "\n").encode())
            output.flush()
            proc = subprocess.Popen(command, cwd=REPO, env=env, stdin=subprocess.DEVNULL,
                                    stdout=output, stderr=subprocess.STDOUT, start_new_session=True)
            code = proc.wait(timeout=step.timeout)
        step.status = "pass" if code == 0 else "fail"
        step.message = f"exit {code}"
    except subprocess.TimeoutExpired:
        step.status, step.message = "fail", f"timed out after {step.timeout}s"
    except KeyboardInterrupt:
        step.status, step.message = "fail", "interrupted"
        raise
    except OSError as error:
        step.status, step.message = "fail", str(error)
    finally:
        if proc is not None and proc.poll() is None:
            stop(proc, privileged)
        step.seconds = time.monotonic() - start
    print(f"{step.status.upper():4} {step.name} ({step.seconds:.1f}s): {step.message}", flush=True)
    if step.status == "fail":
        print(Path(step.log).read_text(errors="replace")[-6000:], flush=True)


def write_reports(directory, suite, steps):
    summary = {"suite": suite, "steps": [vars(step) for step in steps]}
    (directory / "summary.json").write_text(json.dumps(summary, indent=2))
    root = ET.Element("testsuite", name="vmm-" + suite, tests=str(len(steps)),
                      failures=str(sum(s.status == "fail" for s in steps)),
                      skipped=str(sum(s.status in ("skip", "pending") for s in steps)),
                      time=f"{sum(s.seconds for s in steps):.3f}")
    for step in steps:
        case = ET.SubElement(root, "testcase", name=step.name, classname="vmm.verify",
                             time=f"{step.seconds:.3f}")
        if step.status == "fail":
            ET.SubElement(case, "failure", message=step.message).text = step.log
        elif step.status in ("skip", "pending"):
            ET.SubElement(case, "skipped", message=step.message or "not run")
        ET.SubElement(case, "system-out").text = step.log
    ET.ElementTree(root).write(directory / "junit.xml", encoding="utf-8", xml_declaration=True)


def main(argv=None):
    args = parse_args(argv)
    if args.keep_root:
        if os.geteuid() != 0:
            print("verify.py: error: --keep-root applies when the process is started with sudo",
                  file=sys.stderr)
            return 2
    else:
        try:
            drop_privileges()
        except RuntimeError as error:
            print(f"verify.py: error: {error}", file=sys.stderr)
            return 2
    if args.list:
        for step in make_plan(args, args.artifacts / "RUN"):
            prefix = "sudo -n " if step.privileged and args.sudo else ""
            print(f"{step.name}: {prefix}{shlex.join(step.command)}")
        return 0
    args.artifacts.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="vmm-verify-", dir=args.artifacts))
    # python-tests runs as SUDO_UID and creates its directories in TMPDIR.
    # mkdtemp leaves this directory owned by root, mode 0700.
    if os.geteuid() == 0:
        try:
            owner = int(os.environ["SUDO_UID"])
            group = int(os.environ["SUDO_GID"])
        except (KeyError, ValueError):
            owner = group = None
        if owner and 0 < owner < 2**32 - 1 and group is not None and 0 <= group < 2**32 - 1:
            os.chown(directory, owner, group)
            os.chmod(directory, 0o700)
    print(f"Suite: {args.suite}\nArtifacts: {directory}", flush=True)
    steps = [Step("preflight", [])] + make_plan(args, directory)
    interrupted = False
    previous = signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    try:
        try:
            preflight(args)
            steps[0].status = "pass"
        except RuntimeError as error:
            steps[0].status, steps[0].message = "fail", str(error)
            print(f"FAIL preflight: {error}", flush=True)
        for step in steps[1:]:
            failed = [name for name in ("preflight", *step.needs)
                      if next(s for s in steps if s.name == name).status != "pass"]
            if failed:
                step.status, step.message = "skip", "dependency failed: " + ", ".join(failed)
                print(f"SKIP {step.name}: {step.message}", flush=True)
            else:
                run_step(step, args, directory)
            write_reports(directory, args.suite, steps)
    except KeyboardInterrupt:
        interrupted = True
        print("Interrupted; stopping verification.", flush=True)
    except Exception as error:
        steps.append(Step("runner-error", [], status="fail", message=str(error)))
        print(f"FAIL runner: {error}", flush=True)
    finally:
        for step in steps:
            if step.status == "pending":
                step.status, step.message = "skip", "verification stopped"
        write_reports(directory, args.suite, steps)
        signal.signal(signal.SIGTERM, previous)
    failed = sum(s.status == "fail" for s in steps)
    skipped = sum(s.status == "skip" for s in steps)
    print(f"Results: {len(steps) - failed - skipped} passed, {failed} failed, {skipped} skipped")
    print(f"Reports: {directory / 'summary.json'} and {directory / 'junit.xml'}")
    if args.suite == "quick":
        print("Excluded: media and real-network checks (select --suite offline or full).")
    elif args.suite == "offline":
        print("Excluded: vmnet/SSH checks (select --suite full).")
    return 130 if interrupted else int(failed > 0 or skipped > 0)


if __name__ == "__main__":
    sys.exit(main())
