#!/usr/bin/env python3
"""Boot a disposable FreeBSD 14.4 ISO and verify host directory sharing without a NIC."""
import argparse
import os
from pathlib import Path
import tempfile
import time

from check_vm_lifecycle import VM


def prepare_export(directory):
    """Keep the export reachable by the identity used by the VMM's helper.

    Under sudo, TMPDIR and every artifact ancestor may be root-only. Put just
    the disposable export beneath /private/tmp and assign it to the sudo user;
    never relax permissions on the verifier's logs, disks or SSH credentials.
    """
    owner = None
    if os.geteuid() == 0:
        try:
            uid, gid = int(os.environ["SUDO_UID"]), int(os.environ["SUDO_GID"])
            if not 0 < uid < 2**32 - 1 or not 0 <= gid < 2**32 - 1:
                raise ValueError("invalid sudo identity")
        except (KeyError, ValueError) as error:
            raise RuntimeError("Root shared-folder checks require sudo from a non-root user "
                               "(SUDO_UID and SUDO_GID)") from error
        owner = (uid, gid)
        export = Path(tempfile.mkdtemp(prefix="vmm-share-export-", dir="/private/tmp"))
    else:
        export = directory / "export"
        export.mkdir(mode=0o700)
    try:
        (export / "host.txt").write_text("host-original\n")
        (export / "outside-link").symlink_to("/etc/passwd")
        if owner is not None:
            for path in (export / "host.txt", export / "outside-link", export):
                os.chown(path, *owner, follow_symlinks=False)
        (directory / "export-path.txt").write_text(str(export) + "\n")
        return export
    except BaseException:
        import shutil
        shutil.rmtree(export)
        raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--kernel", required=True, type=Path)
    parser.add_argument("--iso", required=True, type=Path)
    parser.add_argument("--keep", action="store_true")
    args = parser.parse_args()
    directory = Path(tempfile.mkdtemp(prefix="vmm-share-check-"))
    export = prepare_export(directory)
    mailbox = directory / "uart"
    vm = VM(str(args.binary.resolve()), directory, "guest", [
        "--freebsd", args.kernel.resolve(), "--cdrom", args.iso.resolve(),
        "--root", "cd9660:/dev/vtbd0", "--cpus", "2", "--ram", "1024",
        "--share", "readonly", export, "--share-rw", "work", export,
    ], env=dict(os.environ, VMM_UART_IN=str(mailbox)))
    def send(cmd):
        pending = mailbox.with_suffix(".next")
        pending.write_text(cmd + "\n")
        pending.replace(mailbox)
    def check(cmd, marker):
        encoded = "".join("\\%03o" % ord(c) for c in marker + "\n")
        send(f"({cmd}) && printf '{encoded}'")
        vm.wait_for(marker, timeout=40)
    try:
        vm.wait_for("RETURN for /bin/sh", timeout=90)
        send("")
        vm.wait_for("root@:/ #", timeout=30)
        check("mount -t tmpfs tmpfs /tmp; mkdir -p /tmp/ro /tmp/rw; kldload virtio_p9fs; "
              "mount -t p9fs readonly /tmp/ro && mount -t p9fs work /tmp/rw", "SHARES_MOUNTED")
        check("test \"$(cat /tmp/ro/host.txt)\" = host-original && ls /tmp/ro", "SHARE_READ_OK")
        check("! touch /tmp/ro/forbidden", "SHARE_RO_OK")
        check("mkdir /tmp/rw/sub && echo guest-created > /tmp/rw/sub/data && "
              "mv /tmp/rw/sub/data /tmp/rw/sub/renamed && "
              "test \"$(cat /tmp/rw/sub/renamed)\" = guest-created", "SHARE_WRITE_OK")
        assert (export / "sub/renamed").read_text() == "guest-created\n"
        check("echo second >> /tmp/rw/sub/renamed && ln /tmp/rw/sub/renamed /tmp/rw/hard && "
              "chmod 600 /tmp/rw/sub/renamed && test $(wc -l < /tmp/rw/hard) -eq 2", "SHARE_APPEND_METADATA_OK")
        assert (export / "hard").stat().st_ino == (export / "sub/renamed").stat().st_ino
        (export / "fresh.txt").write_text("host-second\n")
        check("test \"$(cat /tmp/rw/fresh.txt)\" = host-second", "SHARE_HOST_EDIT_OK")
        (export / "host.txt").write_text("host-updated\n")
        check("test \"$(cat /tmp/rw/host.txt)\" = host-updated", "SHARE_EXISTING_EDIT_OK")
        check("ln -s host.txt /tmp/rw/relative && "
              "test \"$(cat /tmp/rw/relative)\" = host-updated && "
              "test \"$(readlink /tmp/rw/outside-link)\" = /etc/passwd", "SHARE_LINK_OK")
        check("rm /tmp/rw/sub/renamed && rmdir /tmp/rw/sub && "
              "umount /tmp/ro && umount /tmp/rw && mount -t p9fs work /tmp/rw && "
              "test \"$(cat /tmp/rw/host.txt)\" = host-updated", "SHARE_REMOUNT_OK")
        assert not (export / "forbidden").exists()
        assert not (export / "sub").exists()
        check("umount /tmp/rw", "SHARE_BEFORE_REBOOT")
        send("reboot")
        vm.wait_for("RETURN for /bin/sh", timeout=90, count=2)
        prompts = vm.text().count("root@:/ #")
        send("")
        vm.wait_for("root@:/ #", timeout=30, count=prompts + 1)
        check("mount -t tmpfs tmpfs /tmp; mkdir -p /tmp/rw; kldload virtio_p9fs; "
              "mount -t p9fs work /tmp/rw && test $(wc -l < /tmp/rw/hard) -eq 2", "SHARE_REBOOT_OK")
        vm.finish(stop=True)
        print("PASS shared folders: mount/read/write/append/metadata/rename/remove/links/host edits/read-only/remount/reboot; no NIC")
    except BaseException:
        args.keep = True
        print(vm.text()[-6000:])
        raise
    finally:
        vm.cleanup()
        print(f"Artifacts: {directory}")
        print(f"Export: {export}")
        if not args.keep:
            import shutil
            if export.parent != directory:
                shutil.rmtree(export)
            shutil.rmtree(directory)


if __name__ == "__main__":
    main()
