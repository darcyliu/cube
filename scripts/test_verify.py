#!/usr/bin/env python3
"""Runner selection, failure reporting, and cancellation without starting a VM."""
import contextlib
import io
import json
import os
from pathlib import Path
import pwd
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import xml.etree.ElementTree as ET

sys.dont_write_bytecode = True
import verify


class VerifyTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)

    def options(self, suite="quick", *extra):
        argv = ["--suite", suite, "--artifacts", str(self.directory)]
        if suite != "quick":
            argv += ["--iso", "test.iso", "--firmware", "test.fd"]
        if suite == "full":
            argv += ["--guest-cidr", "192.168.2.77/24", "--host-ip", "192.168.2.1"]
        return verify.parse_args([*argv, *extra])

    def test_normal_user_does_not_drop_privileges(self):
        with patch.object(verify.os, "geteuid", return_value=501), \
                patch.object(verify.os, "setuid") as setuid:
            verify.drop_privileges()
        setuid.assert_not_called()

    def test_sudo_invocation_drops_to_the_original_user(self):
        uid = os.getuid()
        if uid == 0:
            uid = int(os.environ.get("SUDO_UID", "501"))
        account = pwd.getpwuid(uid)
        with patch.object(verify.os, "geteuid", return_value=0), \
                patch.dict(os.environ, {"SUDO_UID": str(account.pw_uid),
                                        "SUDO_GID": str(account.pw_gid),
                                        "HOME": "/var/root"}), \
                patch.object(verify.os, "initgroups") as initgroups, \
                patch.object(verify.os, "setgid") as setgid, \
                patch.object(verify.os, "setuid") as setuid:
            verify.drop_privileges()
            self.assertEqual(os.environ["HOME"], account.pw_dir)
            self.assertEqual(os.environ["USER"], account.pw_name)
        initgroups.assert_called_once_with(account.pw_name, account.pw_gid)
        setgid.assert_called_once_with(account.pw_gid)
        setuid.assert_called_once_with(account.pw_uid)

    def test_keep_root_skips_the_drop_only_when_already_root(self):
        with patch.object(verify.os, "geteuid", return_value=0), \
                patch.object(verify.os, "setuid") as setuid, \
                contextlib.redirect_stdout(io.StringIO()):
            code = verify.main(["--keep-root", "--list"])
        self.assertEqual(code, 0)
        setuid.assert_not_called()
        with patch.object(verify.os, "geteuid", return_value=501), \
                contextlib.redirect_stderr(io.StringIO()):
            self.assertEqual(verify.main(["--keep-root", "--list"]), 2)

    def test_root_run_hands_the_result_directory_to_the_sudo_user(self):
        def run_main(euid):
            with patch.object(verify.os, "geteuid", return_value=euid), \
                    patch.dict(os.environ, {"SUDO_UID": "501", "SUDO_GID": "20"}), \
                    patch.object(verify.os, "chown") as chown, \
                    patch.object(verify.os, "chmod") as chmod, \
                    patch.object(verify, "preflight", side_effect=RuntimeError("stop")), \
                    patch.object(verify, "run_step"), \
                    contextlib.redirect_stdout(io.StringIO()):
                verify.main(["--artifacts", str(self.directory)]
                            + (["--keep-root"] if euid == 0 else []))
            return chown, chmod
        chown, chmod = run_main(0)
        target = Path(chown.call_args.args[0])
        created = next(path for path in self.directory.iterdir() if path.is_dir())
        self.assertEqual(target.resolve(), created.resolve())
        self.assertEqual(chown.call_args.args[1:], (501, 20))
        chmod.assert_called_once_with(target, 0o700)
        chown, chmod = run_main(501)
        chown.assert_not_called()
        chmod.assert_not_called()

    def test_python_tests_drop_only_when_the_runner_is_root(self):
        step = next(s for s in verify.make_plan(self.options(), self.directory)
                    if s.name == "python-tests")
        self.assertTrue(step.as_user)
        self.assertFalse(any(s.as_user for s in verify.make_plan(self.options("full", "--sudo"),
                                                                 self.directory)
                             if s.name != "python-tests"))

        def captured(euid):
            seen = {}

            def popen(command, **kwargs):
                seen["command"] = command

                class Proc:
                    def wait(self, timeout):
                        return 0

                    def poll(self):
                        return 0
                return Proc()

            with patch.object(verify.os, "geteuid", return_value=euid), \
                    patch.object(verify.subprocess, "Popen", side_effect=popen), \
                    contextlib.redirect_stdout(io.StringIO()):
                verify.run_step(step, self.options(), self.directory)
            return seen["command"]

        wrapped = captured(0)
        self.assertEqual(wrapped[:3], [sys.executable, "-B", "-c"])
        self.assertIn("drop_privileges", wrapped[3])
        self.assertEqual(wrapped[4], str(verify.REPO / "scripts"))
        self.assertEqual(wrapped[5:], step.command)
        self.assertEqual(captured(501), step.command)

    def test_invoking_user_wrapper_execs_the_command(self):
        if os.geteuid() == 0 and "SUDO_UID" not in os.environ:
            self.skipTest("root without SUDO_UID cannot drop")
        result = subprocess.run(
            [sys.executable, "-B", "-c", verify.RUN_AS_INVOKING_USER,
             str(verify.REPO / "scripts"), sys.executable, "-c", "print('ran-as-user')"],
            capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), "ran-as-user")

    def test_root_without_a_sudo_user_is_rejected(self):
        with patch.object(verify.os, "geteuid", return_value=0), \
                patch.dict(os.environ, {}, clear=True), \
                self.assertRaises(RuntimeError):
            verify.drop_privileges()
        with patch.object(verify.os, "geteuid", return_value=0), \
                patch.dict(os.environ, {"SUDO_UID": "0", "SUDO_GID": "0"}), \
                self.assertRaises(RuntimeError):
            verify.drop_privileges()

    def test_full_includes_every_existing_check_and_both_install_modes(self):
        steps = verify.make_plan(self.options("full", "--sudo"), self.directory)
        names = {s.name for s in steps}
        self.assertTrue({"rust-debug", "rust-release", "python-tests", "lifecycle",
                         "network-errors", "installer-console", "network-guest-ssh",
                         "vm-management", "shared-folders",
                         "install-ufs-offline", "install-zfs-offline",
                         "install-ufs-ssh", "install-zfs-ssh"} <= names)
        seen = set()
        for step in steps:
            self.assertTrue(set(step.needs) <= seen)
            seen.add(step.name)
        for step in steps:
            if step.privileged:
                self.assertIn("sudo-access", step.needs)
                self.assertNotIn("--console-only", step.command)
        ufs = next(s for s in steps if s.name == "install-ufs-ssh")
        zfs = next(s for s in steps if s.name == "install-zfs-ssh")
        self.assertIn("--desktop", ufs.command)
        self.assertNotIn("--desktop", zfs.command)
        self.assertIn("--sudo", ufs.command)
        self.assertIn("--sudo", next(s.command for s in steps if s.name == "network-guest-ssh"))
        self.assertEqual(ufs.timeout, 3000)

    def test_full_bridge_runs_in_process_bridge_as_root(self):
        steps = verify.make_plan(self.options("full", "--bridge", "en0"), self.directory)
        guest = [s for s in steps if s.name in
                 ("network-guest-ssh", "install-ufs-ssh", "install-zfs-ssh")]
        self.assertEqual(len(guest), 3)
        for step in guest:
            self.assertTrue(step.privileged)
            self.assertIn("sudo-access", step.needs)
            self.assertEqual(step.command[step.command.index("--net") + 1], "bridge:en0")
            self.assertNotIn("--sudo", step.command)
        bare = verify.make_plan(self.options("full", "--bridge"), self.directory)
        command = next(s.command for s in bare if s.name == "network-guest-ssh")
        self.assertEqual(command[command.index("--net") + 1], "bridge")

    def test_bridge_conflicts_with_sudo_and_other_suites(self):
        with contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit):
                self.options("full", "--sudo", "--bridge", "en0")
            with self.assertRaises(SystemExit):
                self.options("offline", "--bridge", "en0")
            with self.assertRaises(SystemExit):
                self.options("full", "--bridge", "../en0")

    def test_full_without_sudo_uses_the_proxy_and_stays_unprivileged(self):
        steps = verify.make_plan(self.options("full"), self.directory)
        guest = [s for s in steps if s.name in
                 ("network-guest-ssh", "install-ufs-ssh", "install-zfs-ssh")]
        self.assertEqual(len(guest), 3)
        for step in guest:
            self.assertFalse(step.privileged)
            self.assertNotIn("--sudo", step.command)

    def test_quick_needs_no_media_or_privilege(self):
        steps = verify.make_plan(self.options(), self.directory)
        self.assertFalse(any(s.privileged for s in steps))
        self.assertFalse(any(s.name.startswith("install-") for s in steps))
        self.assertNotIn("--firmware", next(s.command for s in steps if s.name == "lifecycle"))

    def test_offline_checks_kernel_firmware_and_both_filesystems(self):
        steps = verify.make_plan(self.options("offline"), self.directory)
        self.assertFalse(any(s.privileged for s in steps))
        lifecycle = next(s for s in steps if s.name == "lifecycle")
        self.assertIn("--kernel", lifecycle.command)
        self.assertIn("--firmware", lifecycle.command)
        self.assertEqual(sum("--console-only" in s.command for s in steps), 2)
        self.assertIn("vm-management", [s.name for s in steps])

    def test_existing_binary_is_not_rebuilt_or_resigned(self):
        steps = verify.make_plan(self.options("quick", "--binary", sys.executable), self.directory)
        self.assertNotIn("build", [s.name for s in steps])
        self.assertIn("--verify", next(s.command for s in steps if s.name == "signed-binary"))

    def test_missing_full_inputs_fail_without_creating_resources(self):
        with contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            verify.parse_args(["--suite", "full"])
        self.assertEqual(list(self.directory.iterdir()), [])

    def test_failed_build_skips_vm_checks_but_keeps_independent_results(self):
        steps = [verify.Step("build", ["fake"]),
                 verify.Step("vm", ["fake"], needs=("build",)),
                 verify.Step("independent", ["fake"])]
        calls = []
        def run(step, args, directory):
            calls.append(step.name)
            step.status = "fail" if step.name == "build" else "pass"
            step.message = "injected"
        with patch.object(verify.os, "geteuid", return_value=os.getuid() or 501), \
                patch.object(verify.os, "setuid") as setuid, \
                patch.object(verify, "preflight"), patch.object(verify, "make_plan", return_value=steps), \
                patch.object(verify, "run_step", side_effect=run), contextlib.redirect_stdout(io.StringIO()):
            result = verify.main(["--artifacts", str(self.directory)])
        self.assertEqual(result, 1)
        setuid.assert_not_called()
        self.assertEqual(calls, ["build", "independent"])
        run_dir = next(self.directory.iterdir())
        summary = json.loads((run_dir / "summary.json").read_text())
        self.assertEqual([s["status"] for s in summary["steps"]], ["pass", "fail", "skip", "pass"])
        root = ET.parse(run_dir / "junit.xml").getroot()
        self.assertEqual(root.attrib["failures"], "1")
        self.assertEqual(root.attrib["skipped"], "1")

    def test_preflight_failure_is_not_a_successful_empty_suite(self):
        with patch.object(verify.os, "geteuid", return_value=os.getuid() or 501), \
                patch.object(verify.os, "setuid") as setuid, \
                patch.object(verify, "preflight", side_effect=RuntimeError("no hypervisor")), \
                patch.object(verify, "run_step") as runner, contextlib.redirect_stdout(io.StringIO()):
            code = verify.main(["--artifacts", str(self.directory)])
        self.assertEqual(code, 1)
        setuid.assert_not_called()
        runner.assert_not_called()
        root = ET.parse(next(self.directory.iterdir()) / "junit.xml").getroot()
        self.assertEqual(root.attrib["failures"], "1")

    def test_timeout_gives_child_a_chance_to_clean_up(self):
        marker = self.directory / "cleaned"
        ready = self.directory / "ready"
        source = ("from pathlib import Path; import time\n"
                  f"Path({str(ready)!r}).touch()\n"
                  "try: time.sleep(60)\n"
                  f"finally: Path({str(marker)!r}).touch()\n")
        step = verify.Step("timeout", [sys.executable, "-c", source], timeout=1)
        with contextlib.redirect_stdout(io.StringIO()):
            verify.run_step(step, self.options(), self.directory)
        self.assertEqual(step.status, "fail")
        self.assertTrue(ready.exists())
        self.assertTrue(marker.exists())
        self.assertIn("timed out", step.message)

    def test_signal_wrapper_runs_script_cleanup(self):
        script = self.directory / "term.py"
        marker = self.directory / "signal-cleaned"
        script.write_text("import os,signal\nfrom pathlib import Path\n"
                          "try: os.kill(os.getpid(), signal.SIGTERM)\n"
                          f"finally: Path({str(marker)!r}).touch()\n")
        result = subprocess.run([sys.executable, "-c", verify.PYTHON_CHECK, str(script)],
                                capture_output=True, timeout=5)
        self.assertNotEqual(result.returncode, 0)
        self.assertTrue(marker.exists())

    def test_wrapper_preserves_sibling_imports_and_arguments(self):
        (self.directory / "sibling.py").write_text("VALUE = 'imported'\n")
        script = self.directory / "check.py"
        script.write_text("import sibling,sys\nprint(sibling.VALUE, sys.argv[1])\n")
        result = subprocess.run([sys.executable, "-c", verify.PYTHON_CHECK, str(script), "argument"],
                                capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "imported argument\n")


if __name__ == "__main__":
    unittest.main()
