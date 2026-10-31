# FreeBSD amd64 installer

Host is an Intel Mac running macOS 10.15 or newer. `make fetch-iso` downloads
`FreeBSD-14.4-RELEASE-amd64-disc1.iso` and does not commit it. Create an empty
disk, start the vmnet service as your user, then boot. `--cdrom` is before
`--disk`, so the ISO is virtio-blk `vtbd0`. There is no AHCI CD device.

```sh
mkdir -p "$HOME/VMs/freebsd-amd64"
truncate -s 16g "$HOME/VMs/freebsd-amd64/disk.img"

make vmnet-service
./target/debug/vmm \
  --uefi firmware/OVMF_CODE.fd \
  --cdrom FreeBSD-14.4-RELEASE-amd64-disc1.iso \
  --disk "$HOME/VMs/freebsd-amd64/disk.img" \
  --cpus 2 \
  --ram 1024 \
  --net vmnet-proxy \
  --serial-log "$HOME/VMs/freebsd-amd64/install.log"
```

EFI variables default to `<disk>.vars.fd`
(`$HOME/VMs/freebsd-amd64/disk.img.vars.fd`). The first boot copies
`firmware/OVMF_VARS.fd` into that file. Do not sudo this command: the vmnet
proxy rejects uid 0. In-process NAT is the same command under sudo with
`--net vmnet` instead of `--net vmnet-proxy`.

The installer is interactive. This tree does not automate a desktop, ZFS, or
unattended bsdinstall. Do not set `vfs.root.mountfrom`. There is no AHCI CD.

After install, drop `--cdrom` and keep the disk, the vars file, and `--cpus 2`:

```sh
./target/debug/vmm \
  --uefi firmware/OVMF_CODE.fd \
  --disk "$HOME/VMs/freebsd-amd64/disk.img" \
  --cpus 2 \
  --ram 1024 \
  --net vmnet-proxy
```

`scripts/check_freebsd.py` boots that ISO on an empty disk with the same flags,
including `--net vmnet-proxy`. It waits up to 10 minutes and passes when the
COM1 log shows the FreeBSD loader, the installer, or a mountroot prompt. It
records `target/check_freebsd-serial.log`. `python3 -B scripts/verify.py
--suite full` runs that check. `make vmnet-service` must already be listening.
The script does not start the service and does not fall back to `--net none`.

The disc1 boot was not run. The vmnet proxy was not listening, and
non-interactive sudo cannot start `make vmnet-service`. An OVMF shell is not
a substitute.
