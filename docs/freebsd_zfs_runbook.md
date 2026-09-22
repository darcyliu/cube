# FreeBSD UEFI/ZFS regression runbook

The verified path is EDK2 → the disk's EFI loader → the installed kernel and
`zfs.ko` → `zroot/ROOT/default`. Direct ELF/MODINFO ZFS boot remains experimental.

## Reproduce the disposable test

Requirements are Apple Silicon macOS, Python 3, Xcode command-line tools,
`dtc`, `hdiutil`, and a FreeBSD ARM64 disc1 ISO. Allow several GiB of disk space
for a new sparse 12 GiB disk and 2 GiB of guest RAM. Build and sign the VMM, then
fetch firmware if it is missing:

```sh
make all
make fetch-uefi
python3 -B scripts/check_install.py target/debug/vmm \
  --iso FreeBSD-14.4-RELEASE-arm64-aarch64-disc1.iso \
  --filesystem zfs --firmware firmware/QEMU_EFI.fd --console-only --keep
```

This runs entirely offline using serial-console commands and needs no sudo or
vmnet. To exercise SSH forwarding instead, omit `--console-only` and run with
sudo (or an authorized networking signature). The full ZFS SSH variant has not
been verified in this change; roadmap item 2 separately records a passing UFS
SSH regression.

The test always creates a new destination disk; it cannot install onto an
existing user disk. It creates a private seed, key, random password, console
mailboxes, logs, and EFI store. Failure artifacts are retained. `--keep` also
retains successful artifacts. See [installation regression](install-regression.md)
for timeout and cleanup options.

Installation boots the matching ISO kernel and loads the ISO's `zfs.ko` inside
the installer before running `bsdinstall`. The seed explicitly selects a
single-disk stripe on `vtbd0`, GPT, an EFI system partition, a 1 GiB swap
partition, and the `zroot` pool. It enables ZFS services and serial boot.

Installed-disk boots attach neither ISO nor seed and use only `--uefi`, the disk,
and the same private `--efi-vars` path. Assertions cover:

- Successful installation and clean guest poweroff with zero VMM exit status.
- Root mounted as ZFS from `zroot/ROOT/default`, pool health `ONLINE`, and EFI
  runtime access through `/dev/efi`.
- A synced guest marker, guest-triggered reset, changed boot time, and a returning
  console or SSH session with the exact marker intact.
- A custom nonvolatile EFI variable surviving both reboot and cold start.
- A new VMM process booting the same disk and EFI store, with unchanged
  `BootOrder`, followed by clean guest poweroff.

The artifacts record input paths and SHA-256 hashes, exact VMM commands, guest
versions, EFI boot configuration, firmware output, and command results. Serial
logs restart when the VMM resets; the process log preserves all boot generations.

## Recorded validation: 2026-09-21

On an Apple M1 Max, FreeBSD 14.4-RELEASE ARM64 passed the complete offline workflow
with two vCPUs, 2048 MiB RAM, and a new 12 GiB disk. The exact invocation used
`/tmp/vmm-gui-fix/debug/vmm` in place of `target/debug/vmm` above.

Firmware identified itself as `edk2-stable202408-prebuilt.qemu.org`, built
September 12, 2024 at 16:28:50; the loader reported EFI 2.70 and EDK II revision
1.00. `make fetch-uefi` sources this firmware from QEMU v9.2.0.

| Input | SHA-256 |
|---|---|
| `firmware/QEMU_EFI.fd` (64 MiB) | `47765fe344818cbc464b1c14ae658fb4b854f5c2ceffa982411731eb4865594d` |
| `FreeBSD-14.4-RELEASE-arm64-aarch64-disc1.iso` | `32a63ed91ace4967441baab0bbfb3ac33c13c02018a2f4eeb3a741d373681d7c` |

The final passing run retained artifacts, including input hashes and guest
versions, at
`/private/var/folders/tm/sxs2fzn51sd0w1jd39ddskx40000gn/T/vmm-install-k50lv70t`.
This local directory is diagnostic evidence, not a prerequisite for reproduction.

## Firmware driver-load messages

The six `Error: Image at ... start failed` messages were reproduced. A temporary
diagnostic build captured each loaded PE image when the message was emitted;
the image's embedded debug path identified its driver. Addresses below are from
the 1 GiB diagnostic probe and change with RAM size. The diagnostic instrumentation
is not part of the committed implementation.

| Image address | Driver / status | Explanation and scope |
|---|---|---|
| `0x7fd66000` | QemuRamfbDxe / Not Found | No `etc/ramfb` fw_cfg entry; this test uses the serial console. |
| `0x7fd08000` | SmbiosPlatformDxe / Not Found | No QEMU SMBIOS tables are supplied. |
| `0x7fce8000` | PciHostBridgeDxe / Unsupported | No PCI root bridges; disks use VirtIO-MMIO. |
| `0x7fca3000` | Tcg2Dxe / Unsupported | No TPM2 device; the later TPM capability messages have the same missing-device context. |
| `0x7fc28000` | QemuKernelLoaderFsDxe / Not Found | No QEMU fw_cfg kernel image; this boot loads the disk's EFI executable. |
| `0x7fa94000` | TlsDxe / Aborted | Firmware TLS context creation failed. Its underlying cause remains unresolved; HTTPS network boot is outside the verified path. |

These messages also occur on successful installed ZFS boots. They do not indicate
a failure of the block driver or disk loader. Their status paths are in the
matching upstream EDK2 sources:
[QemuRamfbDxe](https://github.com/tianocore/edk2/blob/edk2-stable202408/OvmfPkg/QemuRamfbDxe/QemuRamfb.c),
[SmbiosPlatformDxe](https://github.com/tianocore/edk2/blob/edk2-stable202408/OvmfPkg/SmbiosPlatformDxe/EntryPoint.c),
[PciHostBridgeDxe](https://github.com/tianocore/edk2/blob/edk2-stable202408/MdeModulePkg/Bus/Pci/PciHostBridgeDxe/PciHostBridge.c),
[Tcg2Dxe](https://github.com/tianocore/edk2/blob/edk2-stable202408/SecurityPkg/Tcg/Tcg2Dxe/Tcg2Dxe.c),
[QemuKernelLoaderFsDxe](https://github.com/tianocore/edk2/blob/edk2-stable202408/OvmfPkg/QemuKernelLoaderFsDxe/QemuKernelLoaderFsDxe.c),
and [TlsDxe](https://github.com/tianocore/edk2/blob/edk2-stable202408/NetworkPkg/TlsDxe/TlsDriver.c).

The actual delay before boot selection was excessive EFI-store synchronization:
every four-byte word in a buffered program performed a host disk sync. The fix
persists the whole modified range once when the guest confirms the buffer,
matching the operation boundary in EDK2's
[virtual NOR flash driver](https://github.com/tianocore/edk2/blob/edk2-stable202408/OvmfPkg/VirtNorFlashDxe/VirtNorFlash.c).

## Boot an existing installed disk

For normal use after a manual or retained test installation:

```sh
./target/debug/vmm --uefi firmware/QEMU_EFI.fd \
  --disk /path/to/installed.img --efi-vars /path/to/installed.vars.fd \
  --cpus 2 --ram 2048 --net none
```

Keep the same EFI store for reboot and cold start. No host kernel or module is
needed. The regression's seed already enables `zfs_load` and `boot_serial` in
`/boot/loader.conf`. Other firmware builds, graphical consoles, HTTPS network
boot, and direct MODINFO ZFS boot remain outside this verification.
