# Prioritized feature and product roadmap

Updated 2026-11-12. This document orders proposed work after the completed
milestones in [completed milestones](completed-milestones.md). It is a planning document, not a claim
that unchecked capabilities already exist.

Cube should advance along two coordinated tracks:

1. **Product workflow:** reliable VM management, stopped-VM snapshots, clear
   health reporting, and safe recovery.
2. **Guest experience:** a polished native display, an interoperable VNC path,
   and storage and memory features that guests can use without custom drivers.

The order below favors user value, compatibility with FreeBSD, bounded security
risk, and reuse of Cube's existing implementation. A later item must not delay
an earlier release gate unless its dependency is explicitly listed.

## Priority summary

| Priority | Area | Deliverable | Why this order |
|---|---|---|---|
| P0 | Current-feature qualification | Close the remaining VirtIO and networking verification gaps | Establish a trustworthy baseline before adding stateful features |
| P1 | Cube.app management parity | Edit existing VM hardware, display, network, and share settings; add actionable health | Exposes capabilities Cube already has and creates the UI foundation for later work |
| P2 | Stopped-VM snapshots and clones | Safe `cubectl snapshot` workflow plus Cube.app snapshot UI | Highest-value new workflow; avoids live device-state serialization |
| P3 | Storage reclamation | VirtIO block discard and write-zeroes for raw and COW backends | Controls long-term disk growth and improves snapshot/export efficiency |
| P4 | Shared display core and native polish | Common frame/damage/cursor model; scaling, Retina, full screen, input fixes | Makes the native window the best local experience without weakening VNC |
| P5 | VNC protocol and efficiency | Negotiated compression, cursor, desktop-name, and resize extensions | Preserves headless, firmware, automation, and tunneled remote workflows |
| P6 | End-to-end dynamic display resize | VirtIO-GPU EDID/display events connected to native and VNC resize requests | Depends on the shared display model and a successful FreeBSD driver spike |
| P7 | Clipboard guest integration | Explicit, bounded, text-only clipboard through a guest agent | Requires a guest-side component; VNC clipboard packets alone are insufficient |
| P8 | Automatic balloon policy | Optional host-pressure controller with safe minimums and hysteresis | Builds on the verified manual controller after observability is in place |
| P9 | Live snapshots | Quiesce the guest and serialize RAM, vCPUs, GIC, and device state | High value but substantially harder than stopped snapshots |
| P10 | Advanced graphics | Multi-head and, only with a demonstrated workload, 3D acceleration | Largest compatibility and security surface; not needed for a strong 2D product |

## Dependency order

```text
P0 qualification
  ├── P1 Cube.app parity and health
  │     └── P2 stopped snapshots and clones
  │            └── P9 live snapshots
  ├── P3 block discard/write-zeroes
  └── P4 shared display core and native polish
         ├── P5 VNC upgrades
         ├── P6 dynamic guest resize
         └── P7 clipboard guest integration

P1 health metrics ──► P8 automatic balloon policy
P6 proven 2D path ──► P10 multi-head/3D evaluation
```

## P0. Close current qualification gaps

No new feature should obscure regressions in the devices just delivered. Finish
the pending entries in the
[VirtIO live-verification checklist](virtio-live-verification.md#result-checklist):

- Qualify virtio-rng attachment and `/dev/hwrng` reads in a live Linux guest.
- Repeat forward TCP and UDP network tests after the proxy optimizations.
- Run a five-minute network soak and a two-VM isolation test.
- Run mixed block multi-queue, network multi-queue, balloon pressure, vsock, and
  virtio-fs traffic, followed by device reset and clean shutdown.
- Retain queue, interrupt, drop, timeout, and reset counters with the result.

Acceptance gate: all runs complete without corruption, unexpected reset,
unbounded memory growth, proxy/client drops, or a stuck shutdown. Record the
commands and results in the live-verification runbook.

## P1. Cube.app management parity and health

Cube.app already creates, starts, stops, and inspects named VMs. The next GUI
milestone should expose the current VMM safely instead of inventing a second
configuration model.

### Edit VM

Add an **Edit VM** inspector for stopped guests with:

- CPU count and RAM size.
- Simple framebuffer or virtio-gpu, resolution, and native/VNC frontend.
- Initial balloon target and runtime-control availability.
- Block queue count and network queue-pair count.
- Network mode, MAC address, SSH forwarding, and vmnet service state.
- Multiple disks, EFI firmware/variable store, and serial-log location.
- Multiple shared folders, access mode, and 9P/virtio-fs compatibility notes.
- Generated `cubectl`/`cube` command preview for diagnosis.

The app must read and write the same versioned configuration accepted by
`cubectl`. Validation belongs in shared model/CLI code; the GUI may present the
error but must not maintain an independent set of device rules.

### Health and observability

Extend the existing host process telemetry with VMM-published state:

- Balloon requested, target, actual, held, and driver/convergence status.
- Disk operations, bytes, queue depth, and errors.
- Network throughput, active queue pairs, drops, proxy pressure, and link state.
- Share, vsock, VNC/native display, and guest-IP state.
- Boot phase, uptime, last exit reason, and whether shutdown was guest clean or
  host forced.

Prefer actionable summaries such as “balloon driver not loaded” or “network
service disconnected” over presenting only raw logs.

Acceptance gate: an existing stopped VM can be edited without losing unknown
configuration fields; invalid combinations are rejected before launch; running
VM resources cannot be mutated accidentally; accessibility labels and keyboard
navigation cover every new control.

Do not combine this work with a wholesale AppKit-to-SwiftUI rewrite. Extract
shared models and services incrementally while retaining the existing tested
controllers.

## P2. Stopped-VM snapshots and clones

This is the next major user-facing feature. Start with stopped VMs so Cube does
not need to serialize RAM or device state.

### CLI contract

```sh
cubectl snapshot create freebsd-dev before-upgrade
cubectl snapshot list freebsd-dev
cubectl snapshot inspect freebsd-dev before-upgrade
cubectl snapshot clone freebsd-dev before-upgrade freebsd-test
cubectl snapshot restore freebsd-dev before-upgrade
cubectl snapshot delete freebsd-dev before-upgrade
```

Implement `clone` before in-place `restore`. Creating a new VM is easier to make
fully non-destructive and provides a recovery path while restore semantics are
being hardened.

A clone must receive a fresh VM identity and its own writable disk, overlay,
and EFI-store paths. Define which configuration fields are preserved and which
are regenerated: allocate a distinct MAC address and explicitly reassign or
require resolution of conflicting host forwarding ports before launch. Shared
host directories remain external resources; disclose that their contents are
not snapshotted and that clones can still access the same directories.

### Snapshot contents

- Versioned manifest, creation time, optional description, and source VM ID.
- VM configuration and all referenced disk metadata.
- Raw disks and their COW overlays as inseparable pairs.
- EFI variable store and firmware identity/version information.
- File sizes, format identifiers, and base/overlay association data sufficient
  to reject a mismatched restore.

Use APFS file cloning when supported, with a progress-reporting ordinary copy
fallback. Apple documents clone files as copy-on-write storage suitable for
efficient copies and revisions: [About Apple File System](https://developer.apple.com/documentation/Foundation/about-apple-file-system).

### Safety requirements

- Require the VM to be stopped and hold its normal exclusive resource locks.
- Build in a private temporary directory, sync complete contents, then publish
  without overwriting an existing snapshot.
- Never modify the source disk, overlay, EFI store, or snapshot in place.
- Restore into new files first and retain the previous VM until validation and
  an optional boot check succeed.
- Commit the restored configuration, disk set, and EFI store as one recoverable
  transaction. Use a single atomic publication point or a durable recovery
  journal; after interruption, startup must select a complete old or new
  generation, never a mixture. Retain recovery files until commit is durable.
- Reject incomplete manifests, missing files, incompatible formats, and
  mismatched disk/overlay relationships.

### Cube.app workflow

Add a snapshot timeline to the VM detail view with create, inspect, clone,
restore, delete, and Reveal in Finder actions. Show logical size separately from
physical space used. Destructive restore/delete confirmation must state the VM
and snapshot names and whether a recovery copy will remain.

Acceptance gate: snapshot a stopped UFS VM and a UEFI/ZFS VM, modify a persistent
marker and EFI variable, clone/restore the snapshot, and verify the older marker,
boot state, and EFI value return. Run the source and clone simultaneously with
independent network identities and host ports, and verify that disk and EFI
writes in either VM do not affect the other or the snapshot. Inject failures
before, during, and after publication, including process termination; restart
the manager and verify that recovery selects a complete generation, preserves
the previous VM for rollback, and leaves existing snapshots unchanged.

See [offline overlay management](overlay-management.md) for the current COW
validation and export guarantees.

## P3. VirtIO block discard and write-zeroes

Offer `VIRTIO_BLK_F_DISCARD` and `VIRTIO_BLK_F_WRITE_ZEROES` only after every
advertised config limit and backend operation is implemented. The
[VirtIO block specification](https://docs.oasis-open.org/virtio/virtio/v1.4/virtio-v1.4.html)
defines both commands. Discard permits backend deallocation without guaranteeing
zeroes on subsequent reads. Write-zeroes must make subsequent reads return
zeroes, whether or not its `unmap` flag permits deallocation. Any stronger
zero-after-discard guarantee must be specified as Cube policy and tested
separately.

Backend policy:

- Raw sparse images: punch holes where macOS and the filesystem support it;
  otherwise write zeroes without claiming deallocation.
- COW overlays: remove or replace mappings only with crash-safe metadata
  ordering. For write-zeroes, use explicit zero mappings or equivalent storage
  that masks nonzero base data; removing an overlay mapping and exposing the
  base is insufficient. Define discard behavior separately.
- Read-only disks: reject mutating commands.
- Partial clusters and unaligned ranges: preserve unaffected data exactly.

Acceptance gate: FreeBSD and Linux issue trim/discard, used host space falls on
a supported sparse backend, and write-zeroes reads back as zero with and without
`unmap`, including over nonzero COW base data. Verify discard against its declared
policy, preserve neighboring sectors, and ensure crash/failure injection cannot
violate completed-operation persistence guarantees or corrupt the COW map.

## P4. Shared display core and native-window polish

Keep native and VNC frontends, but make both consume one display contract:

```text
simple framebuffer / virtio-gpu
              ↓
        DisplaySource
 frame generation + format + geometry
 damage rectangles + cursor image/hotspot
       ↙                          ↘
Native Metal presenter         VNC encoder
       ↘                          ↙
             InputRouter
       USB HID / virtio-input
```

A slow VNC client must never block guest queue processing or the native window.
Each frontend tracks its own consumed frame generation and may drop obsolete
intermediate frames without reordering guest-visible device completions.

### Native display role

Make the native Cocoa/Metal window the default local interactive experience:

- Fit-to-window, actual-pixel, and integer 1x/2x/3x scaling.
- Letterboxing instead of aspect-ratio distortion.
- Full screen and remembered per-VM position, size, and scale mode.
- Correct Retina backing-coordinate conversion and pointer mapping.
- Persistent Metal texture with damage-only uploads and event-driven drawing.
- Cursor as a separate low-latency plane.
- Complete modifier/focus-loss release behavior and improved non-US key maps.
- Visible pointer-capture/release state if relative mouse mode is added.

Apple recommends backing-coordinate conversion rather than treating
`backingScaleFactor` as physical geometry, and `MTKView.drawableSize` provides
the native-pixel render target:
[NSWindow backing scale](https://developer.apple.com/documentation/appkit/nswindow/backingscalefactor),
[MTKView drawable size](https://developer.apple.com/documentation/metalkit/mtkview/drawablesize).

Acceptance gate: scaling and input remain correct at standard and Retina scale,
across monitor moves, full-screen transitions, guest reboot, focus loss, and a
long idle screen. An idle guest must not cause continuous full-frame uploads.

## P5. VNC protocol and efficiency

VNC remains the supported path for headless runs, firmware and installer tests,
automation, and remote access through an SSH tunnel. Keep raw rectangles as the
mandatory fallback and negotiate optional capabilities per client.

Implement in this order:

1. Cursor pseudo-encoding and desktop name.
2. One measured compression encoding, initially Hextile or ZRLE.
3. `DesktopSize` and `ExtendedDesktopSize` reporting.
4. `SetDesktopSize` request parsing, initially allowed to reject every request
   until P6 supplies guest resize.
5. Per-client output state before considering more than one viewer.

The community RFB specification describes encoding negotiation,
`SetDesktopSize`, and `ExtendedDesktopSize`; RFC 6143 remains the base RFB 3.8
reference: [RFB extensions](https://github.com/rfbproto/rfbproto/blob/master/rfbproto.rst),
[RFC 6143](https://www.rfc-editor.org/rfc/rfc6143).

Keep loopback binding as the default. Document SSH port forwarding for remote
use. Do not expose the current unauthenticated server on arbitrary interfaces or
invent a custom password/encryption protocol.

Multiple viewers are not part of the first cut. They require independent pixel
formats, encodings, update cursors, output backpressure, and an explicit input
ownership policy. A later design may permit one controller and read-only
observers.

Acceptance gate: TigerVNC and at least one additional RFB 3.8 client negotiate
supported encodings, reconnect cleanly, receive correct cursor/desktop updates,
and fall back to raw. A slow or disconnected client cannot stall the VM or grow
memory without a bound.

## P6. End-to-end dynamic display resize

Run a FreeBSD 15/16 driver spike before committing the full milestone. Protocol
support in Cube does not prove that the guest console or desktop will reconfigure
an active framebuffer.

Required virtio-gpu work:

- Offer `VIRTIO_GPU_F_EDID` and implement `GET_EDID`.
- Update display info and raise `VIRTIO_GPU_EVENT_DISPLAY` on a validated host
  request.
- Wait for the guest to allocate, set, and flush the replacement scanout.
- Preserve the old scanout until the new one is active; time out to scaled
  presentation rather than displaying uninitialized memory.

Native resize should be debounced. VNC `SetDesktopSize` must validate geometry
and return success or failure through `ExtendedDesktopSize`. Both frontends use
the same resize coordinator and memory limits.

Acceptance gate: resize repeatedly between common and odd dimensions under a
FreeBSD desktop and Linux DRM guest, including full screen, reconnect, guest
reboot, rapid resize, rejected oversize requests, and allocation failure. No
stale frame, out-of-bounds copy, or mismatched pointer mapping is acceptable.

## P7. Clipboard guest integration

VNC `ClientCutText` and extended clipboard messages only move bytes between the
viewer and Cube. A guest component is still required to publish or retrieve the
desktop clipboard.

Start with a small guest agent over a dedicated virtio-console port, because the
tested FreeBSD media do not provide the virtio-vsock path used by Linux tests.
Version and bound every message. First release constraints:

- UTF-8 text only.
- Explicit per-VM host-to-guest and guest-to-host enable switches.
- Maximum payload and update rate.
- No implicit file transfer, command execution, or arbitrary host socket access.
- Clear behavior when no desktop clipboard provider is running.

Native and VNC frontends should use the same clipboard broker. Acceptance
requires bidirectional text with FreeBSD X11 and a Linux desktop, Unicode and
large-input bounds, disabled-direction enforcement, reconnect, and guest-agent
crash recovery.

## P8. Optional automatic balloon policy

Build on the verified manual `balloonctl.py` path; do not replace it. Add an
opt-in controller that observes host pressure and adjusts the target slowly.

Policy requirements:

- Configured guest minimum, maximum, and manual override.
- Hysteresis and rate limits to prevent oscillation.
- Step only after the guest `actual` value converges.
- Back off immediately on driver failure or stalled convergence.
- Surface decisions and reclaimed memory in Cube.app.
- Never infer guest-idle memory solely from host RSS.

Acceptance gate: repeated pressure/recovery cycles preserve guest workloads,
respect minimums, converge without thrashing, and return memory for immediate
guest reuse after deflation.

## P9. Live snapshots

Treat live snapshots as a separate architecture project after stopped snapshots
are dependable. A correct snapshot must capture one coherent point across:

- Guest RAM and vCPU architectural state.
- GIC distributor/redistributor and pending interrupts.
- Every VirtIO transport, queue index, worker epoch, and in-flight operation.
- Timers, RTC, PSCI, UART, USB/input, display, network, block, balloon, vsock,
  shares, and EFI state.
- Disk and EFI persistence ordered with the memory/device checkpoint.

Define quiesce, pause, serialization versioning, restore compatibility, and
failure rollback before implementing the command. External host connections
must have an explicit restore policy; silently reviving stale TCP/vsock/VNC
connections is not acceptable.

Acceptance gate: snapshot and restore under CPU, disk, network, display, and
balloon load; verify monotonic guest state and disk consistency; reject unknown
state versions; and survive failure at every publication boundary without
damaging the stopped-snapshot store.

## P10. Multi-head and 3D evaluation

Multi-head 2D may follow P6 if a real workflow needs it. VirGL/Venus/3D should
not start merely because the VirtIO feature bits exist. It requires a maintained
host renderer, capability negotiation, resource isolation, synchronization,
shader/input validation, crash containment, and guest-driver coverage.

Before implementation, document:

- The target workload and measurable benefit over 2D.
- FreeBSD/Linux guest-driver availability.
- Renderer maintenance and licensing.
- Process isolation and GPU denial-of-service limits.
- Fallback behavior on unsupported Macs and guests.

Until that case is demonstrated, invest in correct 2D presentation, resize,
input, and recovery instead.

## Release policy for roadmap work

- Keep new device feature bits off until config fields, commands, reset, bounds,
  and malformed-input tests are complete.
- Preserve one-device/one-queue/default behavior when an option is not selected.
- Add a reproducible live test before changing a status from implemented to
  live-verified.
- Do not let GUI convenience bypass CLI validation, locks, or destructive-action
  safeguards.
- Update this priority order when evidence changes; do not mark a milestone
  complete merely because its happy path works once.
