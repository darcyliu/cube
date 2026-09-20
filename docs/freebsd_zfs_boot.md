# FreeBSD ZFS Booting Architecture & Implementation Guide

> **Status (2026-09):** Strategy 2 (UEFI/EDK2) is the supported root-on-ZFS path.
> Use `make fetch-uefi && make freebsd-zfs-uefi DISK=<img>`.
> Strategy 1 (MODINFO + zfs.ko) remains in tree but often hangs with no UART after preload.

This document explains why FreeBSD fails to boot from OpenZFS (`zroot`) under direct ELF kernel loaders, the underlying technical constraints of FreeBSD's modular kernel architecture on ARM64, and three concrete implementation strategies for adding native ZFS support to our lightweight AArch64 VMM.

---

## 1. The Problem: What Happens with `Auto (ZFS)`?

When running the FreeBSD installer (`bsdinstall`) and choosing **Auto (ZFS)**, the disk is partitioned as:

| Partition | Type GUID / Scheme | Filesystem | Typical Role |
| :--- | :--- | :--- | :--- |
| **`/dev/vtbd0p1`** | `C12A7328-F81F-11D2-BA4B-00A0C93EC93B` | FAT32 | EFI System Partition (`/boot/efi`) |
| **`/dev/vtbd0p2`** | `516E7CB5-6ECF-11D6-8FF8-00022D09712B` | None | FreeBSD Swap (e.g. 2 GB) |
| **`/dev/vtbd0p3`** | `516E7CBA-6ECF-11D6-8FF8-00022D09712B` | OpenZFS | Root pool (`zroot/ROOT/default`) |

### Observed Boot Failures

1. **Attempting to boot from Partition 2 (`/dev/vtbd0p2`)**:
   ```text
   Failed, superblock has critical errors
   Mounting from ufs:/dev/vtbd0p2 failed with error 2; retrying for 3 more seconds
   ```
   *Cause:* The VMM defaulted to partition 2, which is unformatted Swap space, not a UFS filesystem.

2. **Attempting to boot directly from ZFS on Partition 3 (`zfs:zroot/ROOT/default`)**:
   ```text
   Mounting from zfs:zroot/ROOT/default failed with error 19: vfs_mountroot: cannot find vfs for zfs
   ```
   *Cause:* Error 19 (`ENODEV`) means the FreeBSD kernel **does not have a registered Virtual File System (VFS) driver for `zfs`**.

---

## 2. Technical Root Cause

### A. Modular Kernel vs. Built-in Drivers
In the official FreeBSD ARM64 release (`GENERIC` kernel):
* **Statically compiled into `boot/kernel/kernel`**:
  * UFS / FFS (`ufs_mountroot`)
  * ISO9660 (`cd9660`)
  * NFS Client (`nfs_mountroot`)
* **Compiled as dynamic loadable modules (`.ko`)**:
  * OpenZFS: [`/boot/kernel/zfs.ko`](file:///boot/kernel/zfs.ko)
  * OpenSolaris compatibility layer
  * Kernel crypto algorithms (AES-NI, SHA-256 for ZFS)

### B. Direct ELF Boot vs. Loader-Driven Boot
* **Direct ELF Boot (Current VMM)**:
  Our VMM acts as a direct ELF bootloader: it parses the ELF64 headers of `boot/kernel/kernel`, maps segments into guest RAM at `0x4020_0000`, generates a Device Tree (DTB) at `0x4800_0000`, sets `X0 = dtb_addr`, and jumps straight to the kernel's entry point (`locore.S`).
  * In this mode, **no bootloader runs**.
  * No secondary `.ko` module files are loaded into memory.
  * The kernel starts with only its built-in drivers.
* **Standard Loader Boot (Bare-metal / QEMU / AWS)**:
  Standard systems boot into FreeBSD's second-stage loader (`loader.efi`):
  1. `loader.efi` contains a standalone OpenZFS reader.
  2. It reads `/boot/loader.conf` from the ZFS filesystem.
  3. It pre-loads `kernel` **and** `zfs.ko` into physical memory.
  4. It constructs a **module metadata table** (`MODINFO`) describing every loaded file.
  5. It jumps to `kernel`, passing the metadata table so the kernel initializes `zfs.ko` before attempting to mount root.

---

## 3. Implementation Strategies

### Strategy 1: Pre-load `zfs.ko` via FreeBSD Loader Metadata (**experimental — often hangs**)

This strategy retains direct ELF boot: the VMM copies `zfs.ko` into guest RAM and builds a loader MODINFO table. FreeBSD 14 ARM64 `machdep_boot.c` does **not** read a DTB `freebsd,module-metadata` property. `parse_boot_param()` either:

- sees an FDT header in **X0** and *fakes* a kernel-only preload table (UFS path, unchanged), or
- treats **X0 as a MODINFO blob** (`freebsd_parse_boot_param`), taking the DTB from `MODINFOMD_DTBP` (`0x8000|0x1000` on arm64).

UFS boots keep X0 = DTB. ZFS (or any `--module`) sets X0 = MODINFO.

#### How it is wired

1. **Physical Memory Layout** (packed after the kernel so locore’s TTBR1 window covers it).
   locore treats X0 **&lt; KERNBASE** as a physical FDT. Module boot must pass
   X0 = `KERNBASE + (metadata_gpa - kernel_gpa)` and the same KVA translation
   in every MODINFO pointer (ADDR, DTBP, ENVP, KERNEND).
   ```text
   GPA 0x4020_0000  kernel ELF          VA 0xffff000000000000
                    DTB
                    zfs.ko
                    MODINFO             X0 = KVA of this blob
   ```

2. **MODINFO** (`src/preload.rs`, `sys/sys/linker.h`): 8-byte-aligned `{u32 type; u32 size; u8 val[]}`.
   ```text
   NAME/TYPE/ADDR/SIZE  kernel   ("elf kernel")
   METADATA|DTBP/HOWTO/KERNEND/ENVP   (must be before the next NAME)
   NAME/TYPE/ADDR/SIZE  zfs      ("elf64 module")
   END
   ```

3. **Boot Execution**:
   `make fetch-freebsd` extracts `boot/kernel/kernel` **and** `boot/kernel/zfs.ko`. GPT ZFS GUID → `vfs.root.mountfrom=zfs:zroot/ROOT/default`. Override with `--root zfs:otherpool/ROOT/default`. Extra modules: `--module path/to/foo.ko`.

---

### Strategy 2 (recommended): Boot via EDK2 / UEFI Firmware (Standard Virtualization)

This approach brings our VMM in line with standard PC virtualization (QEMU, UTM, VirtualBox).

#### How it Works
1. Map a 2MB ROM/Flash region at GPA `0x0000_0000` containing the pre-compiled ARM64 EDK2 firmware (`QEMU_EFI.fd`).
2. vCPU 0 boots at reset vector `0x0000_0000` into EDK2.
3. EDK2 initializes the VirtIO block device (`vtbd0`), mounts Partition 1 (`/boot/efi`), and launches `\EFI\BOOT\BOOTAA64.EFI`.
4. `BOOTAA64.EFI` (FreeBSD `loader.efi`) natively handles:
   - Reading ZFS pools.
   - Parsing `/boot/loader.conf`.
   - Loading `kernel` and `zfs.ko`.
   - Jumping to the kernel with full module support.

#### Pros & Cons
* **Pros**:
  * 100% universal: Boots FreeBSD (UFS & ZFS), Linux, OpenBSD, Windows ARM64 without kernel-specific code.
  * Preserves full FreeBSD loader features (boot environments `beadm`/`bectl`, safe mode, single-user menus).
* **Cons**:
  * Requires bundling a 2MB EDK2 binary.
  * Adds 1–2 seconds of UEFI firmware initialization before kernel boot.
  * Requires implementing a basic CFI/ROM MMIO flash device.

---

### Strategy 3: Monolithic Kernel with `options ZFS` (Static Kernel)

If compiling a custom FreeBSD kernel is acceptable, OpenZFS can be built directly into the kernel executable.

#### Kernel Configuration
Create a custom kernel configuration file `/usr/src/sys/arm64/conf/VMM_ZFS`:
```text
include GENERIC
ident   VMM_ZFS

# Statically compile OpenZFS into the kernel binary
options ZFS
```

#### Build & Extraction
```bash
# Build the custom kernel on a FreeBSD build host
make buildkernel KERNCONF=VMM_ZFS
make installkernel KERNCONF=VMM_ZFS DESTDIR=/tmp/zfs-kernel

# Extract kernel binary for VMM
cp /tmp/zfs-kernel/boot/kernel/kernel ./boot/kernel/kernel-zfs
```

#### Booting in VMM
Because ZFS is now compiled directly into the static ELF binary:
```bash
sudo ./target/debug/vmm --freebsd ./kernel-zfs \
    --disk freebsd-zfs.img \
    --root "zfs:zroot/ROOT/default" \
    --multiuser --net vmnet
```
* **Pros:** Requires zero changes to VMM Rust code. Boots instantly.
* **Cons:** Cannot boot official, unmodified release discs directly without extracting/swapping the kernel binary.

---

## 4. Comparison Summary

| Feature | Strategy 1: Preload `zfs.ko` | Strategy 2: EDK2 / UEFI | Strategy 3: Static Kernel |
| :--- | :--- | :--- | :--- |
| **Boot Speed** | Instant (< 0.5s) | Fast (~1.5s - 2.0s) | Instant (< 0.5s) |
| **Firmware Blobs** | None (pure Rust) | EDK2 `QEMU_EFI.fd` (2MB) | None |
| **VMM Changes** | Implemented (`src/preload.rs`) | ~200 lines (Flash/ROM mapping) | None |
| **Stock ISO Support** | Yes (extracts `zfs.ko`) | Yes (boots EFI partition) | No (custom build) |
| **Multi-OS Support** | FreeBSD only | Universal (Linux/BSD/Win) | FreeBSD only |

---

## 5. Current Recommendation & Status

**Strategy 1 is implemented** (`src/preload.rs`, `src/gpt.rs`, FreeBSD branch of `boot_os_kernel`).

1. **Auto (UFS)** still needs no extra files; X0 remains the DTB.
2. **Auto (ZFS)** requires `boot/kernel/zfs.ko` (`make fetch-freebsd`). The VMM preloads it, sets X0 to MODINFO, and mounts `zfs:zroot/ROOT/default`.
3. Missing `zfs.ko` on a ZFS disk is a hard error with a fetch hint (no more silent ENODEV).
4. Strategies 2 (EDK2) and 3 (static `options ZFS`) remain optional alternatives; they are not required for stock GENERIC root-on-ZFS.

```bash
make fetch-freebsd   # kernel + zfs.ko
sudo ./target/debug/vmm --freebsd boot/kernel/kernel --disk freebsd-zfs.img --net vmnet --cpus 2
# Auto-detects zfs:zroot/ROOT/default and preloads boot/kernel/zfs.ko
# Override dataset: --root zfs:mypool/ROOT/default
# Extra modules:    --module boot/kernel/opensolaris.ko
```
