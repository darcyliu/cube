# FreeBSD UFS / ZFS boot — implementation plan

## Goal

Keep **UFS as the supported, regression-tested root** (it already works). Add **root-on-ZFS** for stock FreeBSD 14.4 ARM64 `GENERIC` by preloading `zfs.ko` the way `loader.efi` does — without EDK2 and without a custom kernel.

Non-goals: UEFI/EDK2, `bectl` boot menus, a VMM-side ZFS parser, host-side UFS/ZFS mounts on macOS.

---

## Current state

| | UFS | ZFS |
|---|---|---|
| VirtIO-blk (`vtbd0`) | yes | yes (disk is fine) |
| In-kernel VFS | **built into GENERIC** | **`zfs.ko` only** |
| GPT detect | UFS GUID → `ufs:/dev/vtbd0pN` | ZFS GUID → **warn, refuse** |
| Boot protocol | X0 = DTB; kernel synthesizes fake preload | same; `vfs.zfs` never registers |
| Symptom if forced | — | `error 19 ENODEV` (`cannot find vfs for zfs`) |

Boot today (`src/main.rs` `boot_os_kernel` + `vcpu::setup_os_boot_state`):

1. Load `boot/kernel/kernel` ELF at GPA `0x4020_0000`
2. Write DTB at `0x4800_0000`
3. **X0 = DTB** (Linux/FDT protocol)
4. FreeBSD `parse_boot_param()` sees an FDT header and calls `fake_preload_metadata()` — kernel + DTB only, **no extra `.ko`**

Status: **implemented** in `src/preload.rs` / `src/gpt.rs` (UFS keeps X0=DTB; ZFS uses X0=MODINFO). `docs/freebsd_zfs_boot.md` Strategy 1 originally suggested a DTB property `freebsd,module-metadata`. **That is not how FreeBSD 14 ARM64 works.** `sys/arm64/arm64/machdep_boot.c` either:

- treats X0 as FDT and *fakes* a one-module table, or
- treats X0 as a **loader MODINFO blob** (`freebsd_parse_boot_param`), pulling DTB from `MODINFOMD_DTBP`

ZFS therefore requires switching the FreeBSD boot register convention when modules are present. UFS must keep the proven X0=DTB path so we do not regress live media / Auto (UFS).

---

## Phase 0 — UFS completeness (small, first)

UFS root is already implemented. This phase is tests + docs, not a new filesystem.

**Code**

- Unit-test GPT detection in `detect_root_partition()`:
  - UFS GUID at p2 → `ufs:/dev/vtbd0p2`
  - UFS at p1 (no ESP) → `ufs:/dev/vtbd0p1`
  - ZFS GUID present, no UFS → `None` + existing warning (until Phase 3)
- Leave default fallback `ufs:/dev/vtbd0p2` for MBR/memstick images without a recognizable GPT type.
- Do not change `vfs.root.mountfrom` format or `--multiuser` flags.

**Docs / scripts (already mostly true)**

- Installer and seed: **Auto (UFS)** on `/dev/vtbd0` ([`scripts/install-from-iso.sh`](scripts/install-from-iso.sh), [`docs/freebsd_runbook.md`](docs/freebsd_runbook.md)).
- Multi-user: `/etc/fstab` with `ufs:/dev/vtbd0p2`, then `--multiuser`.
- Host cannot mount UFS; use `bsdtar` on the ISO.

**Done when:** `cargo test` covers GUID decode; `sudo make freebsd-net` still reaches single-user on `mini-memstick.img`.

---

## Phase 1 — MODINFO serializer (no boot-path change yet)

New module [`src/preload.rs`](src/preload.rs) (FreeBSD `sys/boot.h` / `sys/linker.h`).

TLV, 8-byte aligned (`roundup(..., sizeof(u_long))` on aarch64):

```
struct { u32 type; u32 size; u8 val[size]; padding }
```

Types to emit:

| type | meaning |
|---|---|
| `MODINFO_NAME` (1) | `"kernel"` / `"zfs"` |
| `MODINFO_TYPE` (2) | `"elf kernel"` and `"elf64 module"` (search both `"elf kernel"` and `"elf64 kernel"` on the kernel side) |
| `MODINFO_ADDR` (3) | `u64` GPA of the file image |
| `MODINFO_SIZE` (4) | `u64` file size |
| `MODINFO_METADATA \| MODINFOMD_DTBP` (0x800e) | `u64` DTB GPA |
| `MODINFO_METADATA \| MODINFOMD_ENVP` (0x8006) | kenv C strings, double-NUL terminated |
| `MODINFO_METADATA \| MODINFOMD_HOWTO` (0x8007) | `int` boothowto (`RB_SERIAL` \| `RB_VERBOSE`, plus `RB_SINGLE` unless `--multiuser`) |
| `MODINFO_METADATA \| MODINFOMD_KERNEND` (0x8008) | `u64` first free GPA after kernel+modules |
| `0, 0` | end |

**API sketch**

```rust
pub struct PreloadedFile { pub name: String, pub kind: ModuleKind, pub gpa: u64, pub size: u64 }
pub fn build_preload(files: &[PreloadedFile], dtb_gpa: u64, kenv: &str, howto: i32, kernend: u64) -> Vec<u8>
```

**Tests** (no VM): encode a kernel-only table and a kernel+zfs table; check alignment, end marker, string NULs, and that `MODINFO_ADDR` is little-endian u64.

Do **not** `load_elf64` on `zfs.ko`. It is an `ET_REL` `.ko`; copy the **raw file bytes** into guest RAM. The in-kernel linker relocates it.

---

## Phase 2 — FreeBSD module boot path (ZFS-only X0 change)

When booting FreeBSD **and** `zfs.ko` is loaded:

```
GPA 0x4000_0000  RAM
    0x4020_0000  kernel ELF          (existing)
    roundup(kernel_end, 2MiB)        zfs.ko raw file
    roundup(zfs_end, 8)              MODINFO blob
    0x4800_0000  DTB                 (keep unless overlap; then place after metadata)
```

Kernel GENERIC can be tens of MB; **do not hardcode `0x4200_0000`**. Compute from `LoadedElf` high water mark.

Wire-up in [`src/main.rs`](src/main.rs) `boot_os_kernel` (FreeBSD branch only):

1. `elf::load_elf64` kernel as today
2. `guest_mem.write(zfs_gpa, zfs_ko_bytes)`
3. `fdt::generate_dtb(...)` as today (devices unchanged)
4. `build_preload(...)` and write metadata
5. `setup_os_boot_state(entry, modulep_gpa, ...)` — **X0 = metadata GPA, not DTB**

Linux boot stays X0 = DTB.

**UFS / no-zfs.ko:** keep X0 = DTB. Do not flip the working path.

CLI:

- `--module <path>` repeatable, or implicit `boot/kernel/zfs.ko` when GPT says ZFS / `--root` starts with `zfs:`
- Missing `zfs.ko` on a ZFS disk → hard error with fetch hint (not the current yellow warning then ENODEV)

[`Makefile`](Makefile) `fetch-freebsd`: extract `boot/kernel/kernel` **and** `boot/kernel/zfs.ko` from the same `kernel.txz`.

**Highest risk (spike first):** confirm 14.4 `machdep_boot.c` treats X0 as physical MODINFO when it is *not* an FDT header, and whether `preload_bootstrap_relocate` expects GPAs. If the kernel panics on UMA/metadata, dump `debug.dump_modinfo_at_boot` / compare a loader-produced blob from QEMU+EDK2. Budget a half-day to match `fake_preload_metadata` field order and `preload_kerntype` (`"elf kernel"` vs `"elf64 kernel"`).

---

## Phase 3 — Auto-detect ZFS root

In `detect_root_partition(..., is_linux=false)`:

- Prefer UFS GUID → `ufs:/dev/vtbd0pN` (unchanged)
- Else ZFS GUID → `zfs:zroot/ROOT/default` (bsdinstall default dataset)
- Optional `--root zfs:otherpool/ROOT/default` override

Bootargs already pass `vfs.root.mountfrom=...`. Also put the same string in kenv `MODINFOMD_ENVP` (loader does both).

Do not invent a ZFS label parser. Wrong dataset name is a `--root` problem, not a VMM filesystem problem.

---

## Phase 4 — Docs and installer

- Update [`docs/freebsd_zfs_boot.md`](docs/freebsd_zfs_boot.md): replace the incorrect `freebsd,module-metadata` DTB property with **X0 = MODINFO**, DTB via `MODINFOMD_DTBP`. Mark Strategy 1 as implemented; Strategies 2–3 remain alternatives.
- Runbook: Auto (ZFS) is allowed when `boot/kernel/zfs.ko` is present; Auto (UFS) remains the default recommendation.
- `install-from-iso.sh`: mention both; ZFS needs `fetch-freebsd` that includes `zfs.ko`.

**Manual verification (no CI VM):**

1. UFS regression: `sudo make freebsd-net` → single-user, `ufs:/dev/vtbd0p2`
2. ZFS: install Auto (ZFS) on a scratch disk (or a known `zroot` image) → `kldstat` shows `zfs.ko`, `mount` shows `zroot/ROOT/default`
3. Linux Alpine/Debian boot unchanged (X0 still DTB)

---

## PR / work order

```text
PR1  Phase 0  UFS GPT unit tests + doc nits          (no behavior change)
PR2  Phase 1  src/preload.rs + tests                 (dead until PR3)
PR3  Phase 2  load zfs.ko, X0=modulep when present   (depends PR2)
PR4  Phase 3  GPT ZFS → zfs:zroot/ROOT/default       (depends PR3)
PR5  Phase 4  fetch-freebsd zfs.ko, docs             (depends PR3)
```

PR1 ∥ PR2. PR3 is the only FreeBSD boot-protocol change. PR4/PR5 can stack.

---

## Explicitly out of scope

| Item | Why |
|---|---|
| EDK2 / `loader.efi` | Separate firmware project; not needed for ZFS root |
| Custom `options ZFS` kernel | Works today with `--root`; not stock GENERIC |
| Second DHCP on `bridge100` | Unrelated; see hypervisor_networking.md |
| Linux ZFS | Different stack |
| macOS mounting guest UFS/ZFS | Impossible without extra tools |

---

## Success criteria

- Stock `boot/kernel/kernel` + `boot/kernel/zfs.ko` from 14.4 `kernel.txz`
- Auto (UFS) disks still boot with X0=DTB
- Auto (ZFS) disks boot `zfs:zroot/ROOT/default` without error 19
- Linux `--linux` path untouched
