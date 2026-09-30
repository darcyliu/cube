#!/usr/bin/env python3
"""Plan checks for the FreeBSD install front door. No download, sudo, or hypervisor."""
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    "install_vm", Path(__file__).with_name("install-vm.py"))
install_vm = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(install_vm)


class PlanTests(unittest.TestCase):
    def test_default_is_interactive_14_4_distribution_sets(self):
        spec = install_vm.specification()
        self.assertEqual((spec.version, spec.base, spec.unattended, spec.desktop),
                         ("14.4", "distsets", False, None))
        self.assertEqual(spec.name, "freebsd-14-4")
        self.assertEqual((spec.ram, spec.cpus, spec.disk_gib), (1024, 2, 20))
        self.assertIsNone(spec.hint)

    def test_version_15_selects_15_1_pkgbase(self):
        spec = install_vm.specification(version="15", unattended=True, name="work")
        self.assertEqual((spec.version, spec.base, spec.name), ("15.1", "pkgbase", "work"))
        filename, url = install_vm.iso_location(spec.version)
        self.assertEqual(filename, "FreeBSD-15.1-RELEASE-arm64-aarch64-disc1.iso")
        self.assertIn("/ISO-IMAGES/15.1/", url)

    def test_15_0_unattended_and_interactive_desktop_are_rejected(self):
        with self.assertRaisesRegex(ValueError, "15.1"):
            install_vm.specification(version="15.0", unattended=True)
        with self.assertRaisesRegex(ValueError, "--unattended"):
            install_vm.specification(version="14.4", desktop="xfce")
        with self.assertRaisesRegex(ValueError, "15.1"):
            install_vm.specification(version="15.0", unattended=True, desktop="plasma")
        with self.assertRaisesRegex(ValueError, "unknown"):
            install_vm.specification(version="13.2")

    def test_interactive_15_explains_the_package_menu(self):
        spec = install_vm.specification(version="15.0")
        self.assertIn("Packages", spec.hint)
        self.assertFalse(spec.unattended)

    def test_desktop_command_keeps_a_named_pkgbase_install(self):
        root = Path("/tmp/vms")
        spec = install_vm.specification(version="15", unattended=True, desktop="plasma",
                                        name="work", dest_root=root, ram=2048)
        argv = install_vm.command(spec, Path("/images/disc.iso"), Path("/usr/local/bin/vmm"),
                                  desktop_user="darcy")
        script = str(install_vm.SCRIPTS / "check_install.py")
        self.assertEqual(argv[:3], [sys.executable, "-B", script])
        self.assertNotIn("--sudo", argv)
        elevated = install_vm.command(spec, Path("/images/disc.iso"), Path("/usr/local/bin/vmm"),
                                      desktop_user="darcy", sudo=True)
        self.assertEqual(elevated[-1], "--sudo")
        self.assertEqual(elevated[:-1], argv)
        self.assertEqual(argv[argv.index("--base") + 1], "pkgbase")
        self.assertEqual(argv[argv.index("--run-name") + 1], "work")
        self.assertEqual(argv[argv.index("--artifacts") + 1], str(spec.dest.parent))
        self.assertEqual(argv[argv.index("--desktop-environment") + 1], "plasma")
        self.assertEqual(argv[argv.index("--ram") + 1], "2048")
        self.assertIn("--keep", argv)
        self.assertEqual(argv[argv.index("--desktop-user") + 1], "darcy")
        self.assertNotIn("--skip-verify", argv)
        self.assertEqual((spec.cpus, spec.disk_gib), (4, 20))
        skipped = install_vm.command(spec, Path("/images/disc.iso"), Path("/usr/local/bin/vmm"),
                                     desktop_user="darcy", sudo=True, skip_verify=True)
        self.assertEqual(skipped[-2:], ["--skip-verify", "--sudo"])

    def test_base_and_interactive_commands_pass_the_installer_mode(self):
        spec = install_vm.specification(version="15.1", unattended=True, dest_root=Path("/tmp/vms"))
        argv = install_vm.command(spec, Path("/images/disc.iso"), Path("/vmm"), password="secret")
        self.assertTrue(argv[0].endswith("unattended-install.sh"))
        self.assertNotIn("--sudo", argv)
        self.assertEqual(argv[argv.index("--base") + 1], "pkgbase")
        self.assertEqual(argv[argv.index("--password") + 1], "secret")
        self.assertEqual(argv[argv.index("--size") + 1], "20g")
        self.assertTrue(str(argv[argv.index("--disk") + 1]).endswith("/freebsd-15-1/installed.img"))
        interactive = install_vm.command(
            install_vm.specification(version="14.5", dest_root=Path("/tmp/vms")),
            Path("/images/disc.iso"), Path("/vmm"))
        self.assertTrue(interactive[0].endswith("install-from-iso.sh"))
        self.assertNotIn("--sudo", interactive)
        elevated = install_vm.command(spec, Path("/images/disc.iso"), Path("/vmm"), sudo=True)
        self.assertEqual(elevated[-1], "--sudo")
        self.assertNotIn("--base", interactive)
        self.assertEqual(interactive[interactive.index("--vmm") + 1], "/vmm")

    def test_iso_cache_skips_a_finished_image_and_replaces_a_partial_download(self):
        with tempfile.TemporaryDirectory() as temporary:
            cache = Path(temporary)
            filename, _ = install_vm.iso_location("14.4")
            ready = cache / filename
            ready.write_bytes(b"x" * (100 * 1024 * 1024))
            calls = []
            self.assertEqual(install_vm.ensure_iso("14.4", cache, run=lambda *args, **kwargs: calls.append(args)),
                             ready)
            self.assertEqual(calls, [])
            ready.unlink()
            def fake_run(argv, check):
                self.assertTrue(check)
                self.assertIn("14.4", argv[-1])
                Path(argv[argv.index("-o") + 1]).write_bytes(b"iso-bytes")
            destination = install_vm.ensure_iso("14.4", cache, run=fake_run)
            self.assertEqual(destination.read_bytes(), b"iso-bytes")
            self.assertFalse(list(cache.glob("*.partial")))

    def test_result_copies_the_kernel_and_records_shutdown_only_with_a_key(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            kernel = root / ".iso-boot" / "boot" / "kernel" / "kernel"
            kernel.parent.mkdir(parents=True)
            kernel.write_bytes(b"elf")
            spec = install_vm.specification(version="15.1", unattended=True, name="box",
                                            dest_root=root / "VMs")
            spec.dest.mkdir(parents=True)
            key = root / "id_ed25519"
            config_path = install_vm.write_result(spec, Path("/vmm"), Path("/images/disc.iso"),
                                                  key, source_root=root)
            self.assertEqual((spec.dest / "boot/kernel/kernel").read_bytes(), b"elf")
            config = json.loads(config_path.read_text())
            self.assertEqual(config["shutdown"]["identity_file"], str(key))
            self.assertEqual(config["disks"], [{"path": "installed.img"}])
            self.assertEqual(config["network"]["mode"], "vmnet-proxy")
            self.assertEqual(install_vm.vm_config(spec, Path("/vmm"), key, sudo=True)["network"]["mode"],
                             "vmnet")
            self.assertEqual(json.loads((spec.dest / "source.json").read_text())["version"], "15.1")
            without_key = install_vm.vm_config(spec, Path("/vmm"), None)
            self.assertNotIn("shutdown", without_key)

    def test_direct_start_matches_the_saved_boot(self):
        root = Path("/tmp/vms")
        binary = Path("/usr/local/bin/vmm")
        spec = install_vm.specification(version="14.4", unattended=True, name="box",
                                        dest_root=root)
        line = install_vm.direct_start(spec, binary)
        self.assertEqual(line.split()[0], str(binary))
        self.assertTrue(line.split()[line.split().index("--freebsd") + 1]
                        .endswith("/box/boot/kernel/kernel"))
        self.assertTrue(any(part.endswith("/box/installed.img") for part in line.split()))
        self.assertIn("--net vmnet-proxy", line)
        self.assertIn("--ssh-port 2222", line)
        self.assertIn("--ram 1024", line)
        self.assertIn("--cpus 2", line)
        self.assertIn("--multiuser", line)
        self.assertNotIn("--vnc", line)
        self.assertNotIn("sudo", line)
        elevated = install_vm.direct_start(spec, binary, sudo=True)
        self.assertTrue(elevated.startswith("sudo "))
        self.assertIn("--net vmnet", elevated)
        self.assertNotIn("vmnet-proxy", elevated)
        desktop = install_vm.specification(version="15.1", unattended=True, desktop="plasma",
                                           name="work", dest_root=root)
        shown = install_vm.direct_start(desktop, binary, display="1280x800")
        self.assertIn("--vnc 127.0.0.1:5901", shown)
        self.assertIn("--display 1280x800", shown)
        self.assertIn("--ram 4096", shown)
        self.assertIn("--cpus 4", shown)
        self.assertIn("--net vmnet-proxy", shown)

    def test_main_rejects_a_bad_combination_before_using_a_binary(self):
        self.assertEqual(install_vm.main(["--version", "15.0", "--unattended"]), 2)
        self.assertEqual(install_vm.main(["--skip-verify"]), 2)
        with tempfile.TemporaryDirectory() as temporary:
            missing = Path(temporary) / "missing-vmm"
            self.assertEqual(install_vm.main([
                "--version", "14.4", "--name", "sample", "--dest", temporary,
                "--binary", str(missing)]), 2)
            self.assertFalse((Path(temporary) / "sample").exists())


class SeedTests(unittest.TestCase):
    def installerconfig(self, base):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        root = Path(temporary.name)
        iso = root / "seed.iso"
        mount = root / "mnt"
        mount.mkdir()
        subprocess.run(["bash", str(install_vm.SCRIPTS / "create-seed.sh"),
                        "--output", str(iso), "--base", base, "--hostname", "box",
                        "--password", "secret", "--ssh-key", "ssh-ed25519 AAAA test"],
                       cwd=root, check=True, capture_output=True, text=True)
        subprocess.run(["hdiutil", "attach", "-readonly", "-nobrowse",
                        "-mountpoint", str(mount), str(iso)], check=True, capture_output=True)
        self.addCleanup(subprocess.run, ["diskutil", "eject", str(mount)],
                        check=False, capture_output=True)
        return (mount / "installerconfig").read_text()

    def test_pkgbase_seed_points_at_the_disc_repository(self):
        text = self.installerconfig("pkgbase")
        self.assertIn('export COMPONENTS="base"', text)
        self.assertIn("/usr/freebsd-packages/repos", text)
        self.assertNotIn("DISTRIBUTIONS=", text)
        self.assertIn('PARTITIONS="vtbd0"', text)
        self.assertIn('hostname="box"', text)

    def test_distribution_seed_keeps_the_14_x_sets(self):
        text = self.installerconfig("distsets")
        self.assertIn('DISTRIBUTIONS="kernel.txz base.txz"', text)
        self.assertNotIn("COMPONENTS=", text)


if __name__ == "__main__":
    unittest.main()
