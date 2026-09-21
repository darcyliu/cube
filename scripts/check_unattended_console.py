#!/usr/bin/env python3
"""Check installer console helpers against a read-only FreeBSD installer ISO.

Usage: python3 scripts/check_unattended_console.py /path/to/signed/vmm KERNEL ISO
No destination disk is attached and bsdinstall is never invoked.
"""
import os
from pathlib import Path
import pty
import subprocess
import sys
import tempfile
import tty

sys.dont_write_bytecode = True
from check_vm_lifecycle import VM


def main():
    binary, kernel, iso = map(lambda p: str(Path(p).resolve()), sys.argv[1:])
    installer = Path(__file__).with_name("unattended-install.sh").read_text()
    helpers = installer.split("automator_log() {", 1)[1].split(
        "# Background console automation feeder", 1
    )[0]
    helpers = "automator_log() {" + helpers
    with tempfile.TemporaryDirectory(prefix="vmm-installer-check-") as temp:
        directory = Path(temp)
        env = dict(os.environ, VMM_UART_IN=str(directory / "uart-in"))
        env.update(UART_IN=env["VMM_UART_IN"], CONSOLE_LOG=str(directory / "serial.log"), CYAN="", RESET="")
        # Verify diagnostics explicitly return to column zero on a raw terminal.
        master, slave = pty.openpty()
        try:
            tty.setraw(slave)
            subprocess.run(["bash", "-c", helpers + '\nautomator_log "test"'],
                           env=env, stdout=slave, check=True)
            assert os.read(master, 1024) == b"\r\n[Automator] test\r\n"
        finally:
            os.close(master)
            os.close(slave)
        vm = VM(binary, directory, "installer", ["--freebsd", kernel,
                "--cdrom", iso, "--root", "cd9660:/dev/vtbd0", "--cpus", 2,
                "--serial-log", env["CONSOLE_LOG"]], env=env)
        try:
            vm.wait_for("RETURN for /bin/sh", timeout=90)
            # Run the installer's actual mailbox publishing and prompt detection.
            script = helpers + '''
set -e
send_uart ""
for attempt in {1..30}; do
    if shell_ready; then
        # Octal escapes prevent the echoed input from matching the output marker.
        send_uart "printf '\\\\126\\\\115\\\\115\\\\137\\\\111\\\\116\\\\112\\\\105\\\\103\\\\124\\\\137\\\\117\\\\113\\\\n'"
        exit 0
    fi
    sleep 1
done
echo "Shell prompt not detected" >&2
exit 1
'''
            subprocess.run(["bash", "-c", script], env=env, check=True, timeout=45)
            vm.wait_for("VMM_INJECT_OK", timeout=15)
            vm.finish(stop=True)
            print("PASS installer shell selection, prompt detection, UART command, raw-terminal alignment")
        except BaseException:
            print(vm.text()[-5000:])
            raise
        finally:
            vm.cleanup()


if __name__ == "__main__":
    main()
