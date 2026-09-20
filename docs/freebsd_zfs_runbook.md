# FreeBSD root-on-ZFS with this VMM

## Recommended path: Strategy 2 (UEFI / EDK2)

Stock FreeBSD Auto (ZFS) expects:

`UEFI → ESP \EFI\BOOT\BOOTAA64.EFI (loader.efi) → kernel + zfs.ko → zroot/ROOT/default`

**Use this for root-on-ZFS.** Strategy 1 (direct ELF + MODINFO + `zfs.ko`) often hangs after MODINFO with no UART output.

### One-time firmware

```bash
make fetch-uefi   # firmware/QEMU_EFI.fd + firmware/QEMU_VARS.fd
```

### Install (Auto ZFS)

```bash
make install-iso
# In bsdinstall: Partitioning → Auto (ZFS) → install to vtbd0
# Default pool/dataset: zroot / zroot/ROOT/default
```

### Boot ZFS root (UEFI)

```bash
make freebsd-zfs-uefi DISK=freebsd-installed.img
# with LAN DHCP:
sudo make sudo-freebsd-zfs-uefi DISK=freebsd-installed.img
```

Equivalent:

```bash
cargo build && codesign --entitlements entitlements.plist --force -s - target/debug/vmm
./target/debug/vmm --uefi --disk freebsd-installed.img --cpus 2
```

What the VMM does:

1. Maps EDK2 at GPA 0 and EFI vars at `0x04000000`
2. Serves DTB via fw_cfg + RAM
3. Attaches virtio-blk (your GPT disk with ESP + ZFS)
4. Starts at PC=0; EDK2 finds `\EFI\BOOT\BOOTAA64.EFI` on the ESP
5. FreeBSD `loader.efi` loads kernel + `zfs.ko` from the pool and mounts root

No host-side `boot/kernel/kernel` or `zfs.ko` preload is required for UEFI boot.

### Verify in guest

```sh
zpool status
mount | head
sysctl vfs.zfs.version.spa
```

---

## Strategy 1 (experimental): direct ELF + zfs.ko

Kept for UFS-style debugging and research. **Not recommended for ZFS.**

```bash
make fetch-freebsd
make freebsd-zfs DISK=freebsd-installed.img
```

If you see a silent hang after `[VMM] Entering interactive console` with no FreeBSD UART, switch to Strategy 2.

---

## Troubleshooting

| Symptom | Fix |
|--------|-----|
| Missing `firmware/QEMU_EFI.fd` | `make fetch-uefi` |
| UEFI shell / no boot entry | Disk needs ESP with `BOOTAA64.EFI` — reinstall with Auto (ZFS) |
| Strategy 1 hang, no UART | Use `make freebsd-zfs-uefi` |
| `cannot find vfs for zfs` on Strategy 1 | Missing `zfs.ko` — `make fetch-freebsd` |
| Wrong dataset | Reinstall or fix loader.conf on the pool; UEFI path uses disk contents |
| Virtio FLUSH / pool dirty after kill -9 | Host uses `sync_all` on FLUSH; re-import should recover |

## Related docs

- `docs/freebsd_zfs_boot.md` — architecture / strategies
- `docs/zfs_implementation_plan.md` — phased plan
