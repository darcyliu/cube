# FreeBSD ZFS boot architecture

As of 2026-09-21, the verified root-on-ZFS path is:

`EDK2 → ESP /EFI/BOOT/BOOTAA64.EFI → FreeBSD loader.efi → kernel + zfs.ko → zroot/ROOT/default`

The disposable regression verifies installation, installed-disk boot, guest
reboot, and cold start with persistent EFI variables and boot order. See the
[runbook](freebsd_zfs_runbook.md) for the tested firmware, commands, diagnostics,
and limits.

## UEFI path

The VMM maps firmware at GPA `0x00000000`, places the DTB at RAM base
`0x40000000`, and exposes it through QEMU fw_cfg as `etc/fdt`. A CFI flash device
at `0x04000000` provides a separately locked, persistent EFI variable store.
`--efi-vars` selects that store; otherwise it defaults to a path beside the disk.

EDK2 discovers VirtIO-MMIO block devices from the DTB. The installed GPT disk has
an EFI system partition containing FreeBSD's fallback ARM64 loader. That loader
reads the ZFS pool, loads the installed kernel and `zfs.ko`, and supplies FreeBSD
with its normal loader metadata. No host-extracted kernel or module is used for
installed-disk UEFI boot.

The regression installation still starts the ISO's extracted kernel directly,
loads `zfs.ko` inside the installer, and runs `bsdinstall` with an explicit
single-disk ZFS/GPT/UEFI layout. Installation media and seed are detached for all
subsequent boots. The test checks the mounted root dataset, pool health, and EFI
runtime access inside the installed guest.

## EFI variable persistence

`src/devices/pflash.rs` implements word program, buffered program, and block
erase operations with a backing file. A buffered program now writes and syncs its
modified range once at the guest's confirm command. Previously every four-byte
word called `sync_data`, making firmware variable initialization extremely slow
before boot selection. Single-word programs and erases remain durable operations.

The regression writes an EFI variable with nonvolatile, boot-service, and runtime
attributes. It verifies the value after guest reboot and after starting a new
VMM process using the same store. It also compares `BootOrder` across cold start.
A host-side flash test checks buffer confirmation, data addresses, and reopening.

## Direct-kernel alternatives

Direct-kernel UFS boot remains supported and regression tested. It passes an FDT
in X0, allowing FreeBSD to synthesize kernel-only preload metadata.

The experimental module path in `src/preload.rs` copies module files and supplies
a MODINFO blob in X0, including virtual-address pointers for the ARM64 kernel.
`src/gpt.rs` detects UFS and ZFS partition GUIDs; a ZFS root triggers module
preloading and a warning recommending UEFI. This path has had silent early boot
hangs and is **not** the verified root-on-ZFS path.

Older versions of this document proposed a `freebsd,module-metadata` DTB property,
claimed module preload alone completed ZFS support, and gave universal multi-OS
and boot-time guarantees. Those claims are superseded by the measured UEFI/ZFS
workflow. Custom monolithic kernels and other operating systems are outside this
verification.

## Implementation status

| Area | Current state |
|---|---|
| UFS direct boot | Verified installation, SSH, reboot, and marker persistence (roadmap item 2) |
| GPT detection | Implemented and tested for UFS and ZFS GUIDs |
| MODINFO serializer | Implemented and unit tested; successful installed ZFS boot is not established through this path |
| UEFI / VirtIO-MMIO disk boot | Verified with stock FreeBSD 14.4 and EDK2 stable202408 |
| EFI flash | Persistent per-VM store; buffered programs sync once on confirmation |
| ZFS installation | Disposable disk, explicit GPT/UEFI layout, matching ISO kernel/module |
| ZFS reboot and cold start | Verified root dataset, ONLINE pool, guest marker, EFI variable, and persistent boot order |
| Regression workflow | `scripts/check_install.py --filesystem zfs --firmware ... --console-only` |


## Remaining optional work

- Run the ZFS regression with vmnet/SSH on a privileged host. Offline UART
  execution covers the verified firmware/storage lifecycle; UFS already has a
  user-confirmed full SSH run.
- Diagnose firmware TLS context initialization further if HTTPS network boot is
  added. TPM, PCI, SMBIOS, and QEMU ramfb are not required by this disk-boot path.
- Investigate direct MODINFO ZFS boot only as a separate experimental task.
- Extend coverage to additional firmware and FreeBSD versions before claiming
  compatibility with them.

The prior proposed DTB metadata property, physical-pointer MODINFO layout, fixed
module placement, and unconditional UFS partition-2 fallback must not be treated
as implementation requirements. Consult the current code and regression results.
