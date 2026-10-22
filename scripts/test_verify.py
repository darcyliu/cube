#!/usr/bin/env python3
"""Host gate and suite selection. Does not start a VM."""

import os
import sys
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
import check_freebsd
import verify


class VerifyTests(unittest.TestCase):
    def test_darwin_19_is_marketing_10_not_26(self):
        self.assertEqual(verify.marketing_major_from_darwin(19), 10)
        self.assertEqual(verify.marketing_major_from_darwin(5), 10)
        self.assertEqual(verify.marketing_major_from_darwin(20), 11)
        self.assertEqual(verify.marketing_major_from_darwin(25), 16)
        self.assertNotEqual(verify.marketing_major_from_darwin(19), 26)
        self.assertEqual(verify.first_component("10.15.7"), 10)
        self.assertEqual(verify.first_component("19.6.0"), 19)

    def test_gate_allows_marketing_10_and_refuses_rosetta(self):
        self.assertIsNone(verify.gate_error(False, 10))
        self.assertIsNone(verify.gate_error(False, 11))
        self.assertIsNone(verify.gate_error(False, 26))
        self.assertIn("marketing major >= 10", verify.gate_error(False, 9))
        self.assertIn("Darwin 19 is macOS 10.15", verify.gate_error(False, 9))
        self.assertIn("Rosetta", verify.gate_error(True, 26))
        self.assertIn("could not read", verify.gate_error(False, None))
        # A Darwin-major == 26 check would reject this mapping.
        self.assertGreaterEqual(verify.marketing_major_from_darwin(19), 10)

    def test_absent_sysctls_are_not_translated(self):
        with patch.object(verify, "sysctl_int", return_value=None):
            self.assertFalse(verify.translated_or_arm_host())
        with patch.object(verify, "sysctl_int", side_effect=lambda name: 1 if name == "sysctl.proc_translated" else None):
            self.assertTrue(verify.translated_or_arm_host())
        with patch.object(verify, "sysctl_int", side_effect=lambda name: 1 if name == "hw.optional.arm64" else None):
            self.assertTrue(verify.translated_or_arm_host())
        with patch.object(verify, "sysctl_int", return_value=0):
            self.assertFalse(verify.translated_or_arm_host())

    def test_product_version_wins_over_darwin(self):
        def fake(name):
            if name == "kern.osproductversion":
                return "10.15.7"
            if name == "kern.osrelease":
                return "19.6.0"
            return None
        with patch.object(verify, "sysctl_text", side_effect=fake):
            self.assertEqual(verify.host_marketing_major(), 10)

    def test_darwin_fallback_when_product_version_is_absent(self):
        def fake(name):
            if name == "kern.osrelease":
                return "19.6.0"
            return None
        with patch.object(verify, "sysctl_text", side_effect=fake):
            self.assertEqual(verify.host_marketing_major(), 10)

    def test_quick_is_lib_payload_and_ovmf_full_adds_freebsd(self):
        quick = verify.commands("quick")
        full = verify.commands("full")
        self.assertEqual([name for name, _cmd, _limit in quick],
                         ["lib-tests", "build", "sign", "payload", "ovmf"])
        self.assertEqual([name for name, _cmd, _limit in full],
                         ["lib-tests", "build", "sign", "payload", "ovmf", "freebsd"])
        lib = next(cmd for name, cmd, _limit in quick if name == "lib-tests")
        self.assertEqual(lib[:4], ["cargo", "+1.89.0", "test", "--lib"])
        self.assertIn("--offline", lib)
        freebsd = next(cmd for name, cmd, _limit in full if name == "freebsd")
        self.assertTrue(freebsd[-1].endswith("check_freebsd.py"))
        joined = " ".join(freebsd)
        self.assertNotIn("--net none", joined)
        self.assertNotIn("check_install.py", joined)
        sign = next(cmd for name, cmd, _limit in quick if name == "sign")
        self.assertEqual(sign[:3], ["codesign", "--entitlements", str(verify.REPO / "entitlements.plist")])

    def test_freebsd_guest_command_matches_the_installer_doc(self):
        cmd = check_freebsd.guest_argv(
            "/tmp/vmm",
            "/tmp/FreeBSD-14.4-RELEASE-amd64-disc1.iso",
            "/tmp/disk.img",
            "/tmp/install.log",
        )
        self.assertLess(cmd.index("--cdrom"), cmd.index("--disk"))
        self.assertEqual(cmd[cmd.index("--uefi") + 1], "firmware/OVMF_CODE.fd")
        self.assertEqual(cmd[cmd.index("--cpus") + 1], "2")
        self.assertEqual(cmd[cmd.index("--ram") + 1], "1024")
        self.assertEqual(cmd[cmd.index("--net") + 1], "vmnet-proxy")
        self.assertEqual(cmd[cmd.index("--cdrom") + 1], "/tmp/FreeBSD-14.4-RELEASE-amd64-disc1.iso")
        self.assertEqual(cmd[cmd.index("--serial-log") + 1], "/tmp/install.log")
        text = " ".join(cmd)
        self.assertNotIn("vfs.root.mountfrom", text)
        self.assertNotIn("ahci", text.lower())
        self.assertNotIn("--net none", text)
        self.assertEqual(check_freebsd.TIMEOUT_S, 600)

    def test_full_refuses_root_because_the_proxy_rejects_it(self):
        with patch.object(verify, "translated_or_arm_host", return_value=False), \
                patch.object(verify, "host_marketing_major", return_value=10), \
                patch.object(verify.os, "geteuid", return_value=0):
            with self.assertRaises(RuntimeError) as raised:
                verify.preflight("full")
        self.assertIn("vmnet-proxy", str(raised.exception))
        with patch.object(verify, "translated_or_arm_host", return_value=False), \
                patch.object(verify, "host_marketing_major", return_value=10), \
                patch.object(verify.os, "geteuid", return_value=501), \
                patch.object(verify.shutil, "which", return_value="/usr/bin/true"):
            verify.preflight("quick")

    def test_list_does_not_require_the_host_gate(self):
        with patch.object(verify, "preflight") as preflight:
            code = verify.main(["--list", "--suite", "full"])
        self.assertEqual(code, 0)
        preflight.assert_not_called()


class ThisHostTests(unittest.TestCase):
    def test_this_darwin_host_is_allowed_without_major_26(self):
        if os.uname().sysname != "Darwin":
            self.skipTest("host gate reads Darwin sysctls")
        self.assertFalse(verify.translated_or_arm_host())
        major = verify.host_marketing_major()
        self.assertIsNotNone(major)
        self.assertGreaterEqual(major, 10)
        self.assertIsNone(verify.gate_error(False, major))
        release = verify.sysctl_text("kern.osrelease")
        if release and release.startswith("19."):
            self.assertEqual(verify.marketing_major_from_darwin(19), 10)
            self.assertGreaterEqual(major, 10)


if __name__ == "__main__":
    unittest.main()
