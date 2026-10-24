#!/usr/bin/env python3
"""Plan checks for the FreeBSD install front door. No download, sudo, or hypervisor."""
import importlib.util
import io
import json
import os
from contextlib import redirect_stdout
from unittest.mock import patch
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location(
    "install_vm", (Path(__file__).resolve().parents[1] / "install-vm.py"))
install_vm = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(install_vm)


class PlanTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        # Never migrate or read the operator's real ISO cache from a unit test.
        home = patch.object(Path, "home", return_value=Path(temporary.name))
        home.start()
        self.addCleanup(home.stop)

    def test_default_is_interactive_14_4_distribution_sets(self):
        spec = install_vm.specification()
        self.assertEqual((spec.version, spec.base, spec.filesystem, spec.unattended, spec.desktop),
                         ("14.4", "distsets", "zfs", False, None))
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
        with self.assertRaisesRegex(ValueError, "filesystem"):
            install_vm.specification(filesystem="ext4")

    def test_interactive_15_explains_the_package_menu(self):
        spec = install_vm.specification(version="15.0")
        self.assertIn("Packages", spec.hint)
        self.assertFalse(spec.unattended)

    def test_desktop_command_keeps_a_named_pkgbase_install(self):
        root = Path("/tmp/vms")
        spec = install_vm.specification(version="15", unattended=True, desktop="plasma",
                                        name="work", dest_root=root, ram=2048)
        argv = install_vm.command(spec, Path("/images/disc.iso"), Path("/usr/local/bin/cube"),
                                  cubectl=Path("/usr/local/bin/cubectl"), desktop_user="darcy")
        self.assertEqual(argv[:2], ["/usr/local/bin/cubectl", "create"])
        self.assertIn("work", argv)
        self.assertEqual(argv[argv.index("--base") + 1], "pkgbase")
        self.assertEqual(argv[argv.index("--iso") + 1], "/images/disc.iso")
        self.assertEqual(argv[argv.index("--binary") + 1], "/usr/local/bin/cube")
        self.assertEqual(argv[argv.index("--ram") + 1], "2048")
        self.assertEqual(argv[argv.index("--cpus") + 1], "4")
        self.assertEqual(argv[argv.index("--display") + 1], "1280x800")
        self.assertIn("--vnc", argv)
        self.assertIn("--unattended", argv)
        self.assertNotIn("--net", argv)
        elevated = install_vm.command(spec, Path("/images/disc.iso"), Path("/usr/local/bin/cube"),
                                      cubectl=Path("/usr/local/bin/cubectl"), desktop_user="darcy", sudo=True)
        self.assertEqual(elevated[elevated.index("--net") + 1], "vmnet")

    def test_base_and_interactive_commands_pass_the_installer_mode(self):
        spec = install_vm.specification(version="15.1", unattended=True, filesystem="ufs",
                                        dest_root=Path("/tmp/vms"))
        argv = install_vm.command(spec, Path("/images/disc.iso"), Path("/cube"),
                                  cubectl=Path("/cubectl"), password="secret")
        self.assertEqual(argv[:2], ["/cubectl", "create"])
        self.assertIn("--unattended", argv)
        self.assertEqual(argv[argv.index("--base") + 1], "pkgbase")
        self.assertEqual(argv[argv.index("--filesystem") + 1], "ufs")
        self.assertEqual(argv[argv.index("--boot") + 1], "freebsd")
        self.assertEqual(argv[argv.index("--password") + 1], "secret")
        self.assertEqual(argv[argv.index("--disk") + 1], "installed.img:20G")
        interactive = install_vm.command(
            install_vm.specification(version="14.5", dest_root=Path("/tmp/vms")),
            Path("/images/disc.iso"), Path("/cube"), cubectl=Path("/cubectl"))
        self.assertNotIn("--unattended", interactive)
        self.assertNotIn("--base", interactive)
        self.assertNotIn("--filesystem", interactive)
        self.assertNotIn("--boot", interactive)
        self.assertEqual(interactive[interactive.index("--binary") + 1], "/cube")

        zfs = install_vm.command(
            install_vm.specification(version="14.4", unattended=True,
                                      filesystem="zfs", dest_root=Path("/tmp/vms")),
            Path("/images/disc.iso"), Path("/cube"), cubectl=Path("/cubectl"))
        self.assertNotIn("--boot", zfs)

    def test_iso_cache_skips_a_finished_image_and_replaces_a_partial_download(self):
        with tempfile.TemporaryDirectory() as temporary:
            cache = Path(temporary)
            filename, _ = install_vm.iso_location("14.4")
            ready = cache / filename
            with ready.open("wb") as image:
                image.truncate(100 * 1024 * 1024)
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
            self.assertEqual((spec.dest / "kernel").read_bytes(), b"elf")
            config = json.loads(config_path.read_text())
            self.assertEqual(config["shutdown"]["identity_file"], str(key))
            self.assertEqual(config["disks"], [{"path": "installed.img"}])
            self.assertEqual(config["network"]["mode"], "vmnet-proxy")
            self.assertEqual(install_vm.vm_config(spec, Path("/vmm"), key, sudo=True)["network"]["mode"],
                             "vmnet")
            self.assertEqual(json.loads((spec.dest / "source.json").read_text())["version"], "15.1")
            self.assertEqual(json.loads((spec.dest / "source.json").read_text())["filesystem"], "zfs")
            without_key = install_vm.vm_config(spec, Path("/vmm"), None)
            self.assertNotIn("shutdown", without_key)

    def test_interactive_next_steps_do_not_claim_install_or_invent_a_kernel(self):
        spec = install_vm.specification(version="14.4", name="box", dest_root=Path("/tmp/vms"))
        output = io.StringIO()
        with redirect_stdout(output):
            install_vm.print_next_steps(spec)
        text = output.getvalue()
        self.assertIn("created for interactive installation", text)
        self.assertIn("cubectl start box", text)
        self.assertIn("cubectl eject box", text)
        self.assertNotIn("installation finished successfully", text)
        self.assertNotIn("--freebsd", text)
        self.assertNotIn("/kernel", text)

    def test_unattended_next_steps_use_the_saved_cubectl_configuration(self):
        spec = install_vm.specification(version="14.4", unattended=True, name="box",
                                         dest_root=Path("/tmp/vms"))
        output = io.StringIO()
        with redirect_stdout(output):
            install_vm.print_next_steps(spec, root_password="root-secret")
        text = output.getvalue()
        self.assertIn("installation finished successfully", text)
        self.assertIn("Root user:     root", text)
        self.assertIn("Root password: root-secret", text)
        self.assertNotIn("Desktop user:", text)
        self.assertIn("cubectl start box", text)
        self.assertNotIn("--freebsd", text)

    def test_desktop_next_steps_print_both_logins(self):
        spec = install_vm.specification(version="14.4", unattended=True, desktop="xfce",
                                         name="desktop", dest_root=Path("/tmp/vms"))
        output = io.StringIO()
        with redirect_stdout(output):
            install_vm.print_next_steps(
                spec,
                root_password="root-secret",
                desktop_user="cube-user",
                desktop_password="desktop-secret",
            )
        text = output.getvalue()
        self.assertIn("Root password: root-secret", text)
        self.assertIn("Desktop user:     cube-user", text)
        self.assertIn("Desktop password: desktop-secret", text)

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
        capture = root / "installerconfig"
        stub = root / "hdiutil"
        stub.write_text('#!/bin/bash\nfor last; do :; done\ncp "$last/installerconfig" "$CAPTURE"\n')
        stub.chmod(0o755)
        subprocess.run(["bash", str(install_vm.SCRIPTS / "create-seed.sh"),
                        "--output", str(root / "seed.iso"), "--base", base,
                        "--hostname", "box", "--password", "secret",
                        "--ssh-key", "ssh-ed25519 AAAA test"], cwd=root,
                       env=dict(os.environ, PATH=str(root) + os.pathsep + os.environ["PATH"],
                                CAPTURE=str(capture)),
                       check=True, capture_output=True, text=True)
        return capture.read_text()

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
