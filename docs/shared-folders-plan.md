# Host–guest shared folders: analysis and implementation plan

Date: 2026-09-25. Original design proposal.

Implementation update: an initial FreeBSD 14.4 shared-folder implementation now
exists. See [the runbook](shared-folders.md) for delivered behavior, verification
and limitations. The analysis and milestones below record the original plan;
not every hardening or performance milestone is complete.

## Recommendation and scope

Implement **VirtIO-9P over the existing modern MMIO transport**, starting with
FreeBSD 14.4 ARM64 and a read-only share, then add explicitly enabled writes.
Expose a host directory inside the guest; in writable mode, changes made on
either side affect the same host files. This does not mount a guest disk on
macOS. Reverse sharing of an arbitrary guest directory is a separate feature.

Target source trees and file exchange first. Keep guest root filesystems,
databases, and workloads requiring fully local filesystem semantics on the
existing virtual disks. FreeBSD p9fs documents limitations around open files
that have been unlinked and changes in process privileges [2].

## What the current implementation provides

| Area | Evidence in this repository | Implication |
| --- | --- | --- |
| Hypervisor | `src/vm.rs`, `src/hypervisor/mod.rs` use Hypervisor.framework and explicit guest RAM mappings | Folder sharing needs a device and host filesystem service; it does not follow from mapping RAM. |
| Device bus | `src/devices/mod.rs`: `MmioDevice`, per-device mutexes, registration and shutdown | A new device fits the existing dispatch path. Blocking filesystem work must leave the vCPU/MMIO handler promptly. |
| VirtIO | `src/devices/virtio/mod.rs`: modern registers, split queues, bounded descriptor parsing, indirect descriptors, used-ring publication | Reuse queue validation; there is no general transport object that already implements every device's register state machine. |
| Storage | `src/devices/virtio/block.rs`: raw/COW sector backends | A directory cannot be passed to `--disk`; filesystem sharing requires file operations and metadata, not sectors. |
| Discovery | `src/main.rs` builds a list consumed by `src/fdt.rs::generate_dtb` | Add one device and mount tag per share through the same device list for direct and UEFI boots. |
| Async lifecycle | `src/devices/virtio/net.rs` has a worker and shutdown/join hooks; `src/main.rs` stops vCPUs before device shutdown | Reuse lifecycle patterns, with explicit protection against completions after queue reset. |
| Management | `scripts/vmmctl.py::launch_spec` builds CLI arguments and resource records | Add typed share configuration, relative-path handling, and share status. |
| Networking | VirtIO-net, vmnet/bridge/TAP, host TCP forwarding | Network filesystems are possible alternatives, but depend on guest networking and a server. |

### Prerequisite: device address and interrupt allocation

Additional disks are assigned starting at `0x0a000800`, IRQ 39
(`src/main.rs`, disk registration loop). Graphics registers keyboard and tablet
devices at `0x0a000800`/IRQ 39 and `0x0a000a00`/IRQ 40. A fourth disk uses IRQ 41,
also assigned to xHCI. The bus registers entries without overlap validation and
dispatches the first matching address.

These are static allocation conflicts visible in the source, not failures
reproduced in a guest during this review. Before adding shares, introduce a
single validated platform device layout. Reserve fixed platform ranges, allocate
dynamic disks and shares from a separate bounded region, and validate MMIO
overlaps and IRQ uniqueness before creating devices. Generate both bus
registrations and FDT entries from that layout. Keep existing boot-device
locations where possible; regression-test firmware boot with multiple disks.

## Protocol choice and compatibility evidence

| Approach | Assessment |
| --- | --- |
| VirtIO-9P | Recommended. Fits the present transport and works without a NIC. FreeBSD 14.4 added p9fs [1]; its driver uses `VIRTIO_DRIVER_MODULE`, which registers both MMIO and PCI [3,4]. |
| Virtio-fs | Reassess for a later Linux/performance milestone. It uses a filesystem protocol based on FUSE [6]; this repository has no corresponding device/backend. A usable macOS backend and the exact FreeBSD guest release would need independent validation. |
| NFS/SMB | Candidate fallback if the guest compatibility spike fails. Requires a host server, export/identity configuration, and network reachability. Evaluate as a separate integration; do not add an NFS server implementation to this VMM. |
| SSHFS | Possible separate workflow with a guest package and SSH service. Adds deployment and authentication requirements. |
| Shared disk image | Unsuitable for a live host directory. Existing image locks and COW behavior serve a different storage model. |

FreeBSD's 14.4 client defaults to `9P2000.L` [5]. Audit actual operations before
freezing the server subset: the client also contains older protocol operations,
including `Tcreate`. Supporting only a Linux-oriented list of `.L` operations
could leave FreeBSD writes broken. The source supports the choice of transport;
it is not proof that the release media boots and mounts successfully here.

## Proposed interface

New flags, **not currently implemented**:

```sh
cube <existing boot options> --share work ~/Dev/project
cube <existing boot options> --share-rw work ~/Dev/project
```

Each option takes two arguments (tag and path), is repeatable, and avoids
comma-delimited path parsing. Shares default to read-only. Reject duplicate or
invalid tags, missing directories, unsupported settings, and excess devices
before boot. Define a conservative tag length that fits the device config
window. Opening an export must succeed under its configured host identity.

Proposed manager configuration:

```json
{
  "shares": [
    {"tag": "work", "path": "../project", "read_only": true}
  ]
}
```

Resolve JSON paths relative to the configuration file and CLI paths relative to
the current directory. A CLI share list replaces the configured list, matching
the manager's existing disk/forward conventions. Record effective paths, modes,
and identities in launch/status output. Directories may intentionally be shared
across VMs; do not apply the exclusive disk-image ownership rule to them.

Expected FreeBSD usage, to verify during the spike:

```sh
kldload virtio_p9fs
mkdir -p /mnt/host
mount -t p9fs -o ro work /mnt/host
```

For writable shares, use a writable guest mount and the explicit host flag.
Offer persistence instructions after testing module load order and fstab mounts.
Direct kernel boot bypasses `loader.conf`; use runtime module loading from the
matching installed guest, or validate the existing `--module` preload path and
its dependencies. Do not promise automatic guest mounting based on a host flag.

## Architecture and filesystem contract

```text
Guest p9fs → VirtIO-MMIO device → bounded request queue → filesystem worker
                      ↑                                   ↓
                 used ring + IRQ ← validated response ← export root + FIDs
```

- `src/devices/virtio/p9.rs`: device ID 9, mount-tag feature/configuration, one
  split queue, feature negotiation, status/reset and IRQ handling. Initially
  advertise only implemented features, including modern VirtIO support.
- `src/shared_fs/protocol.rs`: bounded codec, request tags, version negotiation,
  negotiated message size, error responses and cancellation rules.
- `src/shared_fs/server.rs`: per-export FID state, operations, stable QID mapping
  and resource limits. FIDs identify filesystem objects and open handles; storing
  only path strings breaks rename/open-file behavior.
- `src/shared_fs/host.rs`: macOS filesystem operations, root confinement,
  permissions and metadata translation. Evaluate maintained portable components
  during the spike; require compatibility, licensing and confinement review
  before adopting a dependency.
- A worker performs filesystem I/O outside the MMIO lock. Snapshot request bytes
  and validated buffer metadata; bound pending requests, bytes, FIDs, and handles.
  Apply backpressure without losing queue heads. Publish completions with queue
  generation checks and synchronized interrupt acknowledgment. Do not implement
  network-share timeouts by abandoning threads that still hold guest memory.

Required behavior before writable release:

1. **Confinement:** hold an export-root directory handle and use descriptor-based
   traversal. Reject absolute paths, embedded NULs, invalid components and walks
   above root. Avoid a `canonicalize`-then-open check, which races with rename or
   symlink replacement. Validate all mutation operands, including rename/link
   destinations. Start with local host filesystems; nested mounts and directory
   moves out of the export need an explicit policy and adversarial tests.
2. **Host identity:** guest UID 0 must not grant host root access. A VMM started
   with sudo for vmnet must perform filesystem work in a separate helper under
   an explicit unprivileged UID/GID, dropping supplementary groups as well.
   Direct root launches without a chosen identity must reject exports. For an
   unprivileged VMM, an in-process worker can use the launching user's identity.
   Introduce a bounded helper protocol and lifecycle tests if using this split.
3. **Read-only enforcement:** check every mutating operation at the server,
   including truncating opens, setattr and link operations. Guest mount options
   alone are insufficient. Reject device nodes and unsupported special files.
4. **Metadata:** explicitly translate Darwin errors, open flags, stat fields and
   timestamps to wire semantics; 9P2000.L values are not native Darwin values.
   Define a single-host-user ownership mapping. Avoid unrestricted host chown or
   propagation of setuid/setgid. Reject unsupported ACL/xattr/locking operations
   with protocol-correct errors instead of claiming success.
5. **Visibility:** document and test guest caching behavior. Test host edits,
   atomic replacement and guest writes in both directions. Do not promise
   instantaneous coherence or host file-watch notifications. APFS case and name
   behavior remains a limitation; use a suitable case-sensitive export for trees
   that need distinct case-sensitive names.
6. **Lifecycle/durability:** define fsync and successful-write guarantees. On
   reset, discard obsolete completions and close session handles; outstanding
   host writes cannot simply be undone. Stop accepting work, drain or cancel
   according to policy, and join workers before memory/GIC teardown. Handle
   helper crashes with an explicit device failure, not a hung guest request.

Symlinks and hard links require dedicated tests. A read-only prototype may
reject them explicitly, but the usable source-tree milestone should support
safe relative symlinks, readlink, and documented hard-link behavior. Preserve
guest namespace resolution without following host symlinks outside the export.
Existing hard links can alias host files outside a directory; document that
exports are user-selected trees, not independent copies.

## Delivery sequence and acceptance gates

### 1. Guest compatibility spike

Use disposable FreeBSD 14.4 ARM64 media to verify module availability, modern
MMIO attachment, tag discovery, version negotiation, and a minimal mount/read.
Exercise direct boot and UEFI. Capture the actual operations, error handling,
and cache behavior. Inspect client vnode operations as well as its protocol
codec, particularly create/mkdir/fsync paths. Record a go/no-go result before
committing to the complete server. If blocked, evaluate NFS as a separate
fallback rather than silently changing the guest requirements.

### 2. Platform allocation and configuration

Implement the validated layout, share configuration, CLI and manager plumbing.
Test disks plus graphics plus multiple shares for unique addresses/IRQs and
matching DTB entries. Verify invalid exports fail before VM startup and that
launches without shares retain existing behavior.

### 3. Read-only vertical slice

Implement the codec, confined host access and asynchronous device path. Initial
operations include version, attach, walk (including cloning/partial walks),
getattr, statfs, open, read, readdir, clunk and flush, plus any client-required
operations found in step 1. Cap allocations before reading untrusted lengths.
Specify directory cookies and response buffer accounting.

Gate: a guest with `--net none` can mount, list and hash exported files; all
mutation attempts fail; traversal and malformed requests cannot escape the
root, crash the VMM or hang the queue. A reset followed by remount succeeds.

### 4. Writable source-tree support

Add the required create/mkdir/write/truncate/setattr/rename/unlink operations,
safe links, and durability behavior. Add explicit host identity handling before
supporting privileged VMM launches. Exercise both protocol variants/operations
actually used by the chosen FreeBSD client; do not silently downgrade protocol
negotiation to make missing operations disappear.

Gate: create, edit, atomic-save, rename and delete work in both directions;
read-only exports stay protected; content survives clean guest shutdown and
restart. Run a representative checkout/build workflow and document unsupported
semantics exposed by it. Check permissions and host ownership as a normal user
and with the VMM launched using sudo.

### 5. Regression, documentation and performance

Add `scripts/check_shared_folders.py`, integrate it into `scripts/verify.py`,
and add a runbook. Use fresh temporary exports and disposable guest disks. SSH
may orchestrate installed-guest tests; separately prove sharing without a NIC
through the existing serial automation. Cover UFS direct boot and UEFI/ZFS,
multiple exports, concurrent VMs, read-only/writable modes, host edits, guest
reboot, SIGTERM, worker failure and cleanup. Retain guest logs and hash results.

Unit tests cover codecs, protocol errors, bounded memory, descriptor direction,
short reply buffers, used lengths, reset races and read-only enforcement.
Adversarial tests cover traversal, symlink/rename races and identity boundaries.
Fuzz the protocol and descriptor-to-request boundary independently of a VM.
Run the repository's required debug/release, Python and integration verification
gates after implementation. Measure directory traversal, many small files,
sequential I/O, CPU use and shutdown latency before adding caching or workers.

## Effort and immediate next step

Planning estimate for one engineer: 2–4 days for the compatibility spike,
2–4 days for allocation/configuration, 1–2 weeks for the read-only slice,
2–3 weeks for writable semantics and privilege handling, and 1–2 weeks for
integration and hardening. These are estimates, not measured delivery dates;
helper isolation and guest filesystem behavior are the largest uncertainties.

The next implementation step should be the compatibility spike and device
layout validation. This review inspected source and upstream compatibility
evidence; it did not boot a guest, run benchmarks, or implement the feature.

## Primary references

1. [FreeBSD 14.4 release notes](https://www.freebsd.org/releases/14.4R/relnotes/).
2. [FreeBSD 14.4 p9fs manual source](https://github.com/freebsd/freebsd-src/blob/releng/14.4/share/man/man4/p9fs.4).
3. [FreeBSD 14.4 VirtIO-9P driver](https://github.com/freebsd/freebsd-src/blob/releng/14.4/sys/dev/virtio/p9fs/virtio_p9fs.c).
4. [FreeBSD VirtIO registration macro](https://github.com/freebsd/freebsd-src/blob/releng/14.4/sys/dev/virtio/virtio.h).
5. [FreeBSD 14.4 9P client](https://github.com/freebsd/freebsd-src/blob/releng/14.4/sys/fs/p9fs/p9_client.c).
6. [Virtio-fs project](https://virtio-fs.gitlab.io/).
7. [Linux 9P mount and cache documentation](https://docs.kernel.org/filesystems/9p.html).
