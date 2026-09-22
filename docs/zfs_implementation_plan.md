# FreeBSD UFS / ZFS boot status and follow-up

Updated 2026-09-21 after the disposable installation regression.

The original plan prioritized direct ELF boot with preloaded ZFS modules and
excluded UEFI. That plan is superseded: direct module boot remains experimental,
and UEFI is the verified stock FreeBSD root-on-ZFS path.

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

See [the ZFS runbook](freebsd_zfs_runbook.md) for reproducible commands and exact
validation scope, and [boot architecture](freebsd_zfs_boot.md) for the current
implementation.

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
