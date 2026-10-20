# Shared host folders (FreeBSD 14.4)

The VMM exposes host directories through VirtIO-9P over modern MMIO. FreeBSD
14.4's `virtio_p9fs` module mounts them without a network interface or guest
agent. Each export has its own device, mount tag and filesystem helper process.

## Launch and mount

Add one or more exports to an existing OS launch:

```sh
# Read-only by default. Quote paths containing spaces or commas.
./target/debug/cube --freebsd boot/kernel/kernel --disk freebsd-installed.img \
  --net none --share work ~/Dev/project

# Explicitly allow guest changes to the host files:
./target/debug/cube --freebsd boot/kernel/kernel --disk freebsd-installed.img \
  --net none --share-rw work ~/Dev/project
```

Inside FreeBSD, as root:

```sh
kldload virtio_p9fs
mkdir -p /mnt/host
mount -t p9fs work /mnt/host
```

For a read-only export, adding `-o ro` to the guest mount is useful but not
required for protection: the host server rejects mutations independently.
Unmount with `umount /mnt/host` before stopping the VM. An export is attached
at boot; hot plugging and automatic guest mounting are not implemented.

Use the same export flags with `--uefi`. Direct kernel boot does not read
`loader.conf`; loading the module from the running installed guest works.
For boot-time mounting, configure `virtio_p9fs` in the guest's `kld_list` and
an fstab entry, and verify startup order on your installation. UEFI can also
load the module via `virtio_p9fs_load="YES"` in `/boot/loader.conf`. This is a
shared data filesystem, not a supported root filesystem for this VMM.

Tags must be unique within a VM and contain 1–120 ASCII letters, digits,
underscores, periods or hyphens. Each flag consumes exactly two arguments.
Invalid paths, duplicate tags, and more than 64 combined additional disks and
shares fail startup. Paths are canonicalized before launch and opened again by
the filesystem helper under its effective identity.

## VM manager

Add this field to an existing `cubectl` JSON configuration:

```json
"shares": [
  {"tag": "source", "path": "../project", "read_only": true},
  {"tag": "output", "path": "../build-output", "read_only": false}
]
```

JSON paths are relative to that configuration file. CLI `--share TAG PATH` and
`--share-rw TAG PATH` paths are relative to the current directory; supplying
any share option replaces the entire configured share list. Launch and status
records include effective exports. Multiple VMs may deliberately export the
same directory; these are not exclusive disk-image claims.

## Permissions and supported behavior

- A helper process performs all host file operations as the launching user.
  When the VMM runs through sudo, the helper drops supplementary groups, GID
  and UID to `SUDO_GID`/`SUDO_UID` before opening the export. Direct host-root
  launches without a non-root sudo identity reject shares. The helper refuses
  to serve requests with host UID 0. Its effective IDs appear in the VMM log.
- Guest ownership is presented as **UID/GID 0:0**. Host mode bits are preserved,
  with setuid/setgid bits removed. Guest root can use the share within the
  host user's permissions; other guest users are subject to those guest mode
  bits. Arbitrary chown, per-user identity mapping and ACL/xattr support are
  not provided. Setting owner/group to the displayed 0:0 is a no-op on the host.
- Regular files, directories, read/write/append, truncation, chmod, timestamps,
  rename, deletion, symlinks, and regular-file hard links are supported.
  Special files, advisory locks and unsupported protocol operations return
  errors. Filesystem calls operate on single components relative to retained
  directory handles. Host I/O never follows guest-provided symlink targets;
  links are resolved by the guest in its own namespace.
- Directory handles remain capabilities for objects already walked: if the
  host moves a directory out of the export, existing handles remain valid
  until unmount/reset. Existing hard links can alias files outside an export.
  Nested mounted filesystems are not filtered. Choose export trees accordingly;
  a share is not a copy or a separate filesystem security boundary against
  the host user moving or linking its own files.
- Export local host filesystems. Requests have a 256 KiB maximum; the server
  caps FIDs at 4096, cached object IDs at 65536, path depth at 256 and helper
  descriptors at 8192 (or the inherited lower limit). Filesystem requests are
  serialized per share, outside the vCPU/MMIO lock. A helper response timeout
  or crash marks the device as needing reset; reboot the VM to recreate a
  failed helper. Other devices continue running.
- Successful writes are synced on the host before their response is sent.
  `Tfsync` is also implemented; FreeBSD 14.4 does not issue it from this client.
  This does not guarantee recovery of guest-buffered writes after a forced
  host stop, nor transactional durability of a sequence of directory changes.
- Host changes update protocol metadata versions when queried. Guest caching
  can delay visibility; there is no cross-VM locking or filesystem notification
  forwarding. Host atomic replacement can make an old unopened FID stale;
  unmount/remount refreshes the view. Guest-initiated renames update live FID
  locations so subsequent writes and links use the renamed object.
- macOS filesystem case sensitivity and filename behavior still apply. Prefer
  a case-sensitive export for projects that require case-distinct names.
  FreeBSD p9fs itself has limitations with open-unlinked files and privilege
  changes. Keep databases, guest system files and workloads that require local
  filesystem semantics on virtual disks.

## Compatibility and device layout

The tested guest is stock FreeBSD 14.4 ARM64. FreeBSD 13 needs an external guest
module and has not been validated; Linux compatibility is also unverified.
The server supports 9P2000.L and FreeBSD's legacy Tcreate and open-flag usage.
It does not negotiate 9P2000 or 9P2000.u.

FreeBSD 14.4's driver omits `virtio_finalize_features()` during attachment.
The device accepts its DRIVER_OK transition with a validated modern feature
set even when FEATURES_OK was omitted. Other clients can use the normal
FEATURES_OK handshake. The guest may report that indirect descriptors were
requested but not negotiated; direct descriptor chains work.

Disk zero remains at `0x0a000000`, IRQ 34. Additional disks and shares occupy
`0x0a020000 + index * 0x200`, IRQ `48 + index`. This avoids the previous collision
between additional disks and graphics input devices. Bus registration rejects
MMIO overlaps, and the same allocated address/IRQ tuples populate the FDT.
Extra disks may appear at different firmware device paths than before this
change; the first boot disk's address is preserved.

## Verification

```sh
python3 -B tests/cube/check_shared_folders.py /path/to/signed/vmm \
  --kernel .iso-boot/boot/kernel/kernel \
  --iso FreeBSD-14.4-RELEASE-arm64-aarch64-disc1.iso --keep
```

The check boots read-only installation media with two exports and no NIC.
It exercises guest reads/writes, host edits, host-enforced read-only mode,
append, permissions, hard links, symlinks, rename, deletion, unmount/remount,
and a guest reboot with persistent host content. Exports are disposable.
Failures retain their directory and console log; `--keep` retains successful
runs too. The check is included in `verify.py --suite offline` and `full`.
Rust tests cover malformed protocol requests, path traversal, symlink opens,
identity checks on replaced files, read-only enforcement, descriptor direction,
short reply buffers and discarding completions after reset.

The initial real-guest checks used direct kernel boot from the 14.4 ISO.
Installed UEFI/ZFS shared-folder mounts, privileged networking plus sharing,
large project performance and long-running concurrent writers require separate
validation before being treated as supported workloads.
