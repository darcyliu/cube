"""Exercise the guest rc script's tablet selection without a FreeBSD VM."""
from pathlib import Path
import os
import subprocess
import tempfile
import unittest


class FreeBSDCubeHidStartup(unittest.TestCase):
    def test_service_identity_matches_installed_name(self):
        source = (Path(__file__).resolve().parents[1] / "freebsd-cube-hid.rc").read_text()
        self.assertIn("# PROVIDE: cube_hid", source)
        self.assertIn("name=cube_hid", source)
        self.assertIn("rcvar=cube_hid_enable", source)
        self.assertIn("start_cmd=cube_hid_start", source)

    def test_reprobes_only_the_descriptor_matched_tablet(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            source = (Path(__file__).resolve().parents[1] / "freebsd-cube-hid.rc").read_text()
            source = source.replace(
                ". /etc/rc.subr",
                'load_rc_config() { :; }; run_rc_command() { "$start_cmd"; }')
            script = root / "cube_hid"
            script.write_text(source)
            bin_dir = root / "bin"
            bin_dir.mkdir()
            (bin_dir / "sysctl").write_text("#!/bin/sh\nexit 0\n")
            (bin_dir / "kldload").write_text("#!/bin/sh\nexit 1\n")
            (bin_dir / "usbconfig").write_text("""#!/bin/sh
if [ "$#" -eq 0 ]; then
    printf '%s\\n' 'ugen0.1: <XHCI root HUB>' \\
        'ugen0.2: <QEMU Tablet>' 'ugen0.3: <QEMU Tablet>'
elif [ "$3" = dump_device_desc ]; then
    if [ "$2" = ugen0.1 ]; then
        printf '%s\\n' 'iProduct = <XHCI root HUB>'
    elif [ "$2" = ugen0.2 ] && [ "${DUPLICATE_TABLET:-0}" = 0 ]; then
        printf '%s\\n' 'iProduct = <VMM USB Keyboard>' 'iSerialNumber = <kbd0>'
    else
        printf '%s\\n' 'iProduct = <VMM USB Tablet>' 'iSerialNumber = <tablet0>'
    fi
elif [ "$3" = reset ]; then
    printf '%s\\n' "$2" >> "$USB_RESET_LOG"
fi
""")
            for name in ("sysctl", "kldload", "usbconfig"):
                (bin_dir / name).chmod(0o755)
            log = root / "reset.log"
            env = {**os.environ, "PATH": str(bin_dir) + os.pathsep + os.environ["PATH"],
                   "USB_RESET_LOG": str(log)}
            result = subprocess.run(["sh", str(script), "start"], env=env,
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(log.read_text(), "ugen0.3\n")

            log.unlink()
            result = subprocess.run(["sh", str(script), "start"],
                                    env={**env, "DUPLICATE_TABLET": "1"},
                                    capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(log.exists(), "ambiguous tablets must not be reset")
