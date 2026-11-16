# VirtIO capability audit and implementation plan

Original audit: 2026-11-09. Status refreshed after implementation and live
verification on 2026-11-11. The delivery sections preserve the design rationale;
the table below is the current support state.

## Current support

| Capability | Status | Repository evidence and qualification |
| --- | --- | --- |
| virtio-rng | **Implemented** | `src/devices/virtio/rng.rs` implements modern MMIO device ID 4, one split queue, host CSPRNG filling, used-ring publication, reset, and IRQ handling. `src/main.rs` registers it for every VM and includes it in the FDT. |
| virtio-balloon | **Implemented and live-verified** | `src/devices/virtio/balloon.rs` implements device ID 5, classic inflate/deflate queues, `MUST_TELL_HOST`, 4 KiB guest-to-16 KiB host-page accounting, Darwin reusable/reuse advice, and a secured stepped runtime controller. FreeBSD completed `512 -> 256 -> 0 -> 512 MiB` with equal requested/target/actual/held counts and a 1.25 GiB returned-memory readback. |
| virtio-vsock | **Implemented and live-verified** | Device ID 19 provides CID allocation, RX/TX/event queues, stream credit and shutdown handling, mapped endpoints, and a secured host control socket. Bidirectional and half-closed 64 MiB streams passed with matching SHA-256. |
| virtio-blk multiqueue | **Implemented and live-verified** | Configurable request queues advertise `VIRTIO_BLK_F_MQ` and `num_queues`. Linux reported four queues; four-job depth-32 `fio` completed CRC32C verification without errors. |
| virtio-net multiqueue | **Implemented and live-verified** | Multiple RX/TX pairs plus the control queue implement `VIRTIO_NET_F_MQ`. Linux negotiated four pairs, and reverse TCP scaling passed with zero Cube/proxy drops through P1/P2/P4/P8. |
| virtio-blk indirect descriptors | **Implemented and tested** | Block negotiates `VIRTIO_F_INDIRECT_DESC`; the shared descriptor walker accepts direct chains ending in an indirect table and rejects nested, cyclic, unnegotiated, and out-of-range forms. FreeBSD negotiated `RingIndirectDesc`, and multiqueue `fio` passed. |
| virtio-net indirect descriptors | **Implemented and tested** | Net advertises the feature and enables the shared indirect parser for RX and TX after negotiation. Unit tests cover split RX/TX segments and malformed/unnegotiated cases; live multiqueue traffic passed. |
| virtio-gpu | **Implemented and live-verified** | Device ID 16 implements the required 2D resource, scanout, flush, and cursor commands. FreeBSD attached `vtgpu`; EDK2 `VirtioGpuDxe` produced the visible FreeBSD installer through VNC. 3D acceleration remains out of scope. |
| virtio-fs | **Implemented and live-verified** | Device ID 26 carries FUSE over hiprio/request queues with a read-only export. Linux mount, metadata, coherence, rejection of writes, and a four-worker 60-second `fio` read passed. DAX is not offered. |

“Net has two queues” does not mean multiqueue: those are the mandatory first RX
and TX queues. VirtIO net multiqueue means multiple RX/TX *pairs* selected via
the control queue.

## Delivery record and remaining work

The headings summarize delivered capability, not completion of every original
acceptance criterion. Post-optimization forward TCP/UDP, the network soak and
two-VM isolation retest, and mixed-device reset/shutdown stress remain open.
Use the [live result checklist](virtio-live-verification.md#result-checklist)
and [current priorities](next-features.md) for release qualification.

Implement the work as independently mergeable vertical slices. Do not advertise
a feature until its queue topology, config fields, reset path, and malformed
guest-input tests are complete.

### 0. Shared VirtIO transport foundation — implemented; fuzzing follow-up remains

This is the prerequisite for every new device and for safe multiqueue work.

1. Extract reusable modern-MMIO state for feature selection/negotiation, status,
   interrupt status, queue selection, queue address registers, and reset. Store
   queues in a bounded `Vec<VirtQueue>` rather than duplicating one-off register
   state machines in every device.
2. Validate driver features against the offered mask when `FEATURES_OK` is set;
   refuse invalid negotiation instead of retaining arbitrary guest bits. Require
   `DRIVER_OK` before consuming queues.
3. Queue-size writes reject zero, non-powers-of-two, and values above the
   advertised maximum. The delivered GPU-only EDK2 compatibility exception
   retains the advertised queue maximum on a zero write; other devices retain
   strict validation. See the live runbook for the firmware evidence.
4. Complete split-ring indirect parsing, including a final indirect table after
   normal descriptors, while continuing to reject nesting, cycles, invalid
   direction, unnegotiated use, integer overflow, and chains over device limits.
5. Add a device-layout allocator for all optional devices. Fixed addresses are
   already crowded; the existing dynamic aperture should allocate disks, shares,
   balloon, vsock, and GPU from one validated MMIO/IRQ plan consumed by both bus
   registration and FDT generation.
6. Add table-driven transport tests for feature pages, every queue index,
   independent queue state, reset epochs, interrupt ACK, direct/indirect chains,
   and hostile GPA/length values. These have regression coverage; a dedicated
   descriptor-parser fuzz target remains outstanding.

Acceptance gate: existing block, net, RNG, console, input, and 9P unit tests and
guest boot regressions pass through the shared transport with no feature-mask or
queue-topology regressions.

### 1. Indirect descriptors for block and net — completed and tested

1. Keep block's advertised bit, switch it to the corrected shared walker, and
   add direct-plus-indirect, read, write, flush, malformed table, and reset tests.
2. Offer `VIRTIO_F_INDIRECT_DESC` from net. Enable it only when negotiated for
   both RX and TX queues. Preserve the existing 64 KiB frame-chain bound and
   direction checks.
3. Test indirect RX buffers split across multiple writable segments and indirect
   TX headers/payloads split across readable segments. Confirm that use without
   negotiation sets `NEEDS_RESET` without touching unvalidated memory.
4. Run Linux and FreeBSD traffic tests with indirect descriptors confirmed from
   guest driver/debug counters, including saturation, DHCP, ping, TCP, reset,
   and clean shutdown.

This is the smallest useful patch and should land before multiqueue.

### 2. Block and network multiqueue — completed and live-verified

#### Block

1. Replace the single queue with `N` request queues. The delivered CLI is
   `--blk-queues N`; it defaults to one for compatibility and clamps the
   request to `min(N, vcpu_count, 16)`. Offer `VIRTIO_BLK_F_MQ` (bit 12) and
   place `num_queues` in the device config at the specified offset.
2. Route every valid queue notification to a bounded worker pool. The current
   backend is mutable and seek-based, so either serialize it behind one backend
   lock initially or add offset-based I/O before claiming parallel I/O. COW
   cluster allocation and table updates must remain serialized and crash-safe.
3. Maintain per-queue avail/used indices and reset epochs. Complete requests on
   their originating queue; do not let a reset publish a stale completion.
4. Benchmark one queue versus N queues with raw and COW disks before enabling MQ
   by default. A negotiated feature without parallel backend work is compatible
   but may not improve throughput.

#### Network

1. Implement queue layout `rx0, tx0, ... rxN-1, txN-1, controlq`; expose
   `max_virtqueue_pairs` and offer both `VIRTIO_NET_F_CTRL_VQ` and
   `VIRTIO_NET_F_MQ` (the latter requires the former).
2. Implement `VIRTIO_NET_CTRL_MQ_VQ_PAIRS_SET` with strict command/header/status
   validation. Only pair 0 is active before the command succeeds.
3. Select RX queues with a stable flow hash (five-tuple when parsable, Ethernet
   hash fallback) so packets in one flow remain ordered. Drain each TX queue
   independently, then call the backend without holding device state locks.
4. Use per-queue backlogs/counters and fair draining so one full RX queue cannot
   permanently block all traffic. Keep the single MMIO IRQ initially; coalesce
   notifications without losing the existing ACK/re-raise behavior.
5. Validate Linux and FreeBSD guest behavior because queue-pair selection and
   driver defaults differ. Test 1, 2, 4, and max queue pairs, invalid control
   commands, CPU hot load, reset, and backend disconnect.

Acceptance gate: guests negotiate the configured queue count, parallel fio and
iperf workloads remain correct under reset and shutdown, and one-queue behavior
is unchanged when MQ is disabled or not negotiated.

### 3. virtio-vsock — completed and live-verified

1. Add device ID 19 with TX, RX, and event queues and the 64-bit `guest_cid`
   config field. Allocate stable, non-reserved CIDs per running VM and reject
   collisions in the manager/service layer.
2. Implement the stream packet state machine first: request/response, read/write,
   credit update/request, shutdown, reset, bounded buffering, and connection
   teardown on device reset. Datagram and seqpacket support are separate later
   features.
3. Define a safe host interface. Recommended v1: a Unix-domain control socket
   owned by the user with explicit host-port-to-Unix-socket mappings; do not
   expose arbitrary host TCP/Unix connections merely because a guest supplies a
   destination port.
4. Add per-connection and per-VM byte/connection limits, backpressure, half-close
   behavior, and worker shutdown. Host I/O must never run while holding the MMIO
   device lock.
5. Test guest-to-host and host-to-guest streams, credit starvation, partial
   packets, simultaneous close, reset events, malformed headers, CID collision,
   and VMM shutdown. Integrate a real Linux guest test before calling it done.

### 4. virtio-balloon — completed and live-verified

Start with classic inflate/deflate only. Stats, free-page hinting, page reporting,
and automatic policy belong in later patches.

1. First run a Hypervisor.framework spike: keep RAM mapped to the VM, apply the
   candidate Darwin reclamation primitive to inactive anonymous pages, verify
   RSS/physical-footprint reduction, verify deflated pages are immediately safe
   for guest reuse, and stress concurrent vCPUs. Do not proceed if reclamation
   requires unsafe unmap/remap races.
2. Resolve the page-size mismatch explicitly. Balloon PFNs are 4 KiB units while
   this host mapping uses 16 KiB pages. Track all four guest subpages and reclaim
   a host page only when the complete host page is inflated; partial groups stay
   resident. Reject PFNs outside ordinary guest RAM and protected framebuffer or
   shared-memory regions.
3. Add device ID 5, inflate and deflate queues, `num_pages`/`actual` config, and
   `VIRTIO_BALLOON_F_MUST_TELL_HOST`. Acknowledge deflation only after the host
   mapping is ready for guest access.
4. The host target API is `--balloon-target` plus the mode-0600
   `--balloon-control` Unix socket. `scripts/balloonctl.py` exposes status and
   waited target updates. Targets clamp to reclaimable RAM and advance in
   64 MiB steps only after the guest's `actual` value converges. A future
   `cubectl` wrapper may forward to this socket; it is not required for the
   device data path.
5. Unit tests cover duplicate PFNs, partial 16 KiB groups, out-of-range and
   protected PFNs, reset accounting, target clamping and stepping, convergence,
   and control-socket lifecycle. Live FreeBSD validation with preallocated RAM
   confirmed pressure handling, host reclamation, same-VM deflation,
   reinflation, and returned-memory usability. Demand-paged live testing and a
   long repeated-cycle soak remain robustness follow-ups rather than basic
   feature blockers.

### 5. virtio-gpu 2D — completed and live-verified

Keep this deliberately 2D. VirGL/venus/3D acceleration is a different project.

1. Add device ID 16 with control and cursor queues, config events, one scanout,
   and only features actually implemented. Start with BGRA-compatible 2D formats
   used by the existing frontends.
2. Implement display-info, resource create/unref, attach/detach backing,
   transfer-to-host-2D, set-scanout, resource flush, and cursor update/move.
   Validate resource IDs, rectangle arithmetic, scatter-gather backing, total
   bytes, and overlapping mappings before copying guest memory.
3. Introduce a `DisplaySource` abstraction so VNC and the native Metal window
   can consume either the current simple framebuffer or a virtio-gpu scanout.
   Flush commands should publish damage rectangles rather than force full-frame
   scans.
4. Add `--gpu simple|virtio|none`, retaining simple framebuffer as the default
   until direct-kernel and UEFI tests pass. For UEFI, validate `VirtioGpuDxe` GOP
   output before removing any simple-framebuffer fallback.
5. Delivered coverage includes resource lifecycle and malformed-command unit
   tests plus live FreeBSD `vtgpu`, EDK2 GOP, VNC, and native-window rendering.
   Linux DRM, runtime resize/config events, multi-head operation, repeated
   reset/reconnect stress, and a full performance comparison with the recorded
   simple-framebuffer baseline remain follow-up work; they are not implied by
   the current 2D support claim.

### 6. virtio-fs without DAX, Linux first — completed and live-verified

This is the highest-risk item. The existing 9P transport and server cannot be
renamed or lightly adapted: virtio-fs uses FUSE messages and different queues.
Keep 9P as the supported FreeBSD path unless a FreeBSD virtio-fs guest driver is
independently demonstrated.

1. Run a compatibility/backend spike before committing to an implementation:
   prove the target Linux guest driver over modern MMIO; select and license-audit
   a macOS-capable FUSE server (prefer adapting a maintained libkrun/rust-vmm
   component over writing the full protocol); define sandboxing and UID/GID
   mapping. Upstream `virtiofsd` remains Linux-oriented, so do not assume it can
   simply be spawned on macOS.
2. Implement device ID 26 with the 36-byte tag, `num_request_queues`, one hiprio
   queue, one or more request queues, indirect descriptors, worker epochs, and
   bounded in-flight requests. Start with one request queue even though the wire
   format permits more.
3. Deliver cache mode without DAX first. DAX requires a shared-memory window,
   mapping/unmapping commands, eviction policy, and Hypervisor.framework mapping
   validation; make it a separate milestone after correctness and benchmarks.
4. Reuse the existing share CLI/config, helper lifecycle, read-only policy, path
   confinement requirements, and dynamic device layout, but not the 9P codec.
   Preserve `--share` behavior or add an explicit transport selector so existing
   FreeBSD configurations do not silently change protocol.
5. The delivered boundary is read-only. Lookup/open/read/readdir, symlink
   metadata and confinement, malformed FUSE lengths, host coherence, write
   rejection, helper lifecycle, and clean shutdown have automated or live
   coverage. Writable operations, xattrs, mmap/fsync semantics, request
   interruption, and reset/helper-crash stress remain future work and must not
   be inferred from the current support claim.
6. The Debian integration check covers basic file and metadata operations, and
   the live runbook covers a sustained random-read workload. Source-tree and
   many-small-file benchmarks, comparison with 9P, and DAX remain follow-ups.

## Cross-feature verification and release policy

- Unit-test every config byte and feature dependency against VirtIO 1.3, plus
  reset and negative descriptor cases for every queue.
- Add end-to-end capability probes (`/sys/bus/virtio/devices` on Linux and
  `devinfo`/driver logs on FreeBSD) so a boot test proves negotiation rather than
  merely proving that the VM started.
- Run mixed-device stress: multiqueue block + multiqueue net + balloon pressure +
  vsock traffic, then reset individual devices and shut down the VM.
- Record queue depth, operations, bytes, drops, malformed requests, interrupts,
  and reset counts in runtime diagnostics.
- Keep each new capability behind an explicit option until both Linux and the
  claimed FreeBSD guest matrix pass. Never advertise an incomplete feature bit.

## Primary references

1. [OASIS VirtIO 1.3 specification](https://docs.oasis-open.org/virtio/virtio/v1.3/virtio-v1.3.html).
2. [Linux virtio-fs documentation](https://docs.kernel.org/filesystems/virtiofs.html).
3. [Existing Cube shared-folder plan](shared-folders-plan.md).
4. [Existing Cube GUI architecture](gui_architecture.md).
5. [Cube FreeBSD virtio-gpu status](freebsd-desktop.md#virtio-gpu-status).
