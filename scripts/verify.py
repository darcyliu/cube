#!/usr/bin/env python3
"""Run VMM checks sequentially, retaining logs and JSON/JUnit reports for CI."""
import argparse
from dataclasses import dataclass
import ipaddress
import json
import os
from pathlib import Path
import platform
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import xml.etree.ElementTree as ET

REPO = Path(__file__).resolve().parent.parent

# Let each existing script execute its finally/cleanup blocks on CI cancellation.
# In particular, check_install's VMs use their own process sessions.
PYTHON_CHECK = (
    "import os,runpy,signal,sys; "
    "signal.signal(signal.SIGTERM, lambda *_: "
    "(_ for _ in ()).throw(KeyboardInterrupt())); "
    "sys.argv=sys.argv[1:]; sys.path.insert(0,os.path.dirname(sys.argv[0])); "
    "runpy.run_path(sys.argv[0],run_name='__main__')"
)


@dataclass
class Step:
    name: str
    command: list
    needs: tuple = ()
    timeout: int = 300
    privileged: bool = False
    status: str = "pending"
    seconds: float = 0
    message: str = ""
    log: str = ""


def parse_args(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--suite", choices=("quick", "offline", "full"), default="quick")
    parser.add_argument("--iso", type=Path, help="ARM64 disc1 ISO; required for offline/full")
    parser.add_argument("--firmware", type=Path, help="EDK2 firmware; required for offline/full")
    parser.add_argument("--binary", type=Path, help="use an existing signed binary; skip build/sign")
    parser.add_argument("--target-dir", type=Path,
                        default=Path(os.environ.get("CARGO_TARGET_DIR", REPO / "target")))
    parser.add_argument("--artifacts", type=Path, default=REPO / "test-results",
                        help="parent for a new private run directory (default: test-results)")
    parser.add_argument("--sudo", action="store_true", help="sudo -n only for full-suite guest checks")
    parser.add_argument("--guest-cidr", type=ipaddress.IPv4Interface,
                        help="unused static guest address on vmnet subnet; required for full")
    parser.add_argument("--host-ip", type=ipaddress.IPv4Address,
                        help="reachable host address on that subnet; required for full")
    parser.add_argument("--list", action="store_true", help="print selected commands without running")
    args = parser.parse_args(argv)
    if args.suite != "quick" and not (args.iso and args.firmware):
        parser.error("offline/full requires --iso and --firmware")
    if args.suite == "full" and not (args.guest_cidr and args.host_ip):
        parser.error("full requires --guest-cidr and --host-ip for the alternate-address SSH check")
    if args.suite != "full" and (args.sudo or args.guest_cidr or args.host_ip):
        parser.error("--sudo, --guest-cidr, and --host-ip apply only to --suite full")
    if args.suite == "quick" and (args.iso or args.firmware):
        parser.error("select --suite offline or full to use guest media")
    if args.guest_cidr and args.host_ip not in args.guest_cidr.network:
        parser.error("--host-ip must be on the --guest-cidr subnet")
    if args.guest_cidr and args.guest_cidr.ip == args.host_ip:
        parser.error("guest and host addresses must differ")
    if args.guest_cidr and args.guest_cidr.ip in (args.guest_cidr.network.network_address,
                                               args.guest_cidr.network.broadcast_address):
        parser.error("--guest-cidr must identify a usable guest address")
    for name in ("iso", "firmware", "binary", "target_dir", "artifacts"):
        if getattr(args, name) is not None:
            setattr(args, name, getattr(args, name).expanduser().resolve())
    return args


def make_plan(args, directory):
    binary = args.binary or args.target_dir / "debug/vmm"
    def check(name, script, extra=(), needs=(), timeout=300, privileged=False):
        return Step(name, [sys.executable, "-u", "-B", "-c", PYTHON_CHECK,
                          str(REPO / "scripts" / script), *map(str, extra)],
                    needs, timeout, privileged)
    steps = [
        Step("rust-debug", ["cargo", "test", "--locked"], timeout=900),
        Step("rust-release", ["cargo", "test", "--locked", "--release"], timeout=900),
        Step("python-tests", [sys.executable, "-u", "-B", "-m", "unittest", "discover",
                              "-s", "scripts", "-p", "test_*.py", "-v"]),
    ]
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
        for fs in ("ufs", "zfs"):
            extra = [binary, "--iso", args.iso, "--artifacts", directory,
                     "--filesystem", fs, "--console-only"]
            if fs == "zfs":
                extra += ["--firmware", args.firmware]
            steps.append(check(f"install-{fs}-offline", "check_install.py", extra,
                               ("signed-binary",), timeout=1800))
    if args.suite == "full":
        if args.sudo:
            steps.append(Step("sudo-access", ["sudo", "-n", "true"], timeout=10))
            ready += ("sudo-access",)
        steps.append(check("network-guest-ssh", "check_network.py",
            [binary, "--kernel", kernel, "--iso", args.iso,
             "--guest-cidr", args.guest_cidr, "--host-ip", args.host_ip,
             "--console-log", directory / "network-guest-ssh.console.log"], ready,
            privileged=True))
        for fs in ("ufs", "zfs"):
            extra = [binary, "--iso", args.iso, "--artifacts", directory, "--filesystem", fs]
            if fs == "ufs":
                extra += ["--desktop"]
            if fs == "zfs":
                extra += ["--firmware", args.firmware]
            steps.append(check(f"install-{fs}-ssh", "check_install.py", extra, ready,
                               timeout=3000 if fs == "ufs" else 1800, privileged=True))
    return steps


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
    for name in ("iso", "firmware", "binary"):
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
    env = dict(os.environ, CARGO_TARGET_DIR=str(args.target_dir), PYTHONUNBUFFERED="1",
               PYTHONDONTWRITEBYTECODE="1")
    # Keep temporary resources from older scripts inside this run too.
    env["TMPDIR"] = str(directory)
    command = list(step.command)
    privileged = step.privileged and args.sudo
    if privileged:
        command = ["sudo", "-n", "/usr/bin/env", "TMPDIR=" + str(directory),
                   "PATH=" + os.environ["PATH"], *command]
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
    if args.list:
        for step in make_plan(args, args.artifacts / "RUN"):
            prefix = "sudo -n " if step.privileged and args.sudo else ""
            print(f"{step.name}: {prefix}{shlex.join(step.command)}")
        return 0
    args.artifacts.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="vmm-verify-", dir=args.artifacts))
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
