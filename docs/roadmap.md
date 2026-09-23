# Follow-up roadmap

Recorded 2026-09-20 after the device/lifecycle hardening and unattended-console
fixes in `06fb2b3`. Updated 2026-09-21: item 1 is complete with regression coverage
and user-confirmed guest SSH forwarding. Item 2 is complete with a user-confirmed
full privileged install → boot → reboot regression run. Item 3 is complete for
the offline UEFI/ZFS disk-boot path, including EFI persistence and cold start.
Item 4 is complete with a user-confirmed FreeBSD 14.4 desktop regression and
recorded framebuffer timings. The next open work is item 5.
Check off criteria only when supported by tests or a recorded manual run.

## 1. Reliable networking and configurable forwarding

Previously, network initialization failures could fall back to a mock backend
that drops packets, and SSH forwarding hardcoded `192.168.18.2:22`. The new
`--forward HOST_IP:PORT=GUEST_IP:PORT` option supports explicit destinations;
`--ssh-port` now learns this VM's IPv4 address from DHCP ACKs instead of assuming
a particular macOS vmnet subnet. Static addresses use an explicit rule.

- [x] Fail clearly when an explicitly requested backend cannot start; retain
  mock networking as an explicit choice.
- [x] Support configurable host bind address/port and guest destination/port.
- [x] Report forwarding bind and connection failures with actionable context.
- [x] Verify guest connectivity and SSH forwarding, including an alternate
  guest address, an occupied host port, and backend initialization failure.
- [x] Update CLI help and networking documentation to match the behavior.

Local regression tests cover rule parsing, a 128 KiB bidirectional transfer,
TCP half-close, listener cleanup, occupied-port startup failure, and backend
failure without fallback. `scripts/check_network.py` checks CLI failures and has
an optional real guest SSH check using read-only installer media. The user
confirmed the rebuilt `--ssh-port` fix works after the guest moved from the
assumed `192.168.18.2` to `192.168.2.2`. All 38 tests pass in debug and release.
The optional read-only-media SSH test remains available for future repeatable
validation; it was not run during this change because sudo authentication was
unavailable to the agent.

Start with `src/devices/virtio/net.rs`, `src/forward.rs`, and `src/main.rs`.
See [networking](networking.md) and [entitlements](entitlements.md).

## 2. Full install → boot → reboot regression test

`scripts/check_install.py` now covers this workflow using disposable resources.
The user confirmed a successful privileged FreeBSD 14.4 run covering installation,
clean poweroff, installed-disk SSH without media, reboot with a changed boot time,
marker persistence, returning SSH, and final clean poweroff.

- [x] Install FreeBSD onto a newly created disposable disk with isolated seed,
  console log, UART mailbox, and EFI store where applicable.
- [x] Require an explicit installer success result and clean VM exit.
- [x] Boot the installed disk without the installation media and verify SSH
  command execution through the configured forwarding rule.
- [x] Create a marker in the guest, reboot, and verify it persists and SSH returns.
- [x] Bound each phase with a timeout, retain diagnostic logs on failure, and
  clean up only resources created by the test.
- [x] Document prerequisites and a reproducible invocation. Never use an
  existing user disk as the test destination.

See the [installation regression runbook](install-regression.md) for commands,
scope, and recorded validation. Failure-guard tests cover echoed success markers,
exit status/poweroff, phase deadlines, early exits, and mailbox protection.
Depends on item 1 for reliable forwarding. UEFI/ZFS remains in item 3.

## 3. Complete UEFI/ZFS boot verification

The disposable FreeBSD 14.4 / EDK2 stable202408 regression passes installation,
installed-disk UEFI/ZFS boot, guest reboot, and cold start. It verifies an ONLINE
root pool, a persistent guest marker and EFI variable, and unchanged boot order.
Verification used offline UART commands; ZFS SSH coverage remains optional.

- [x] Reproduce and explain the firmware driver-load errors; fix failures that
  prevent the supported boot path.
- [x] Verify installation and boot of a disposable FreeBSD ZFS system via UEFI.
- [x] Verify reboot and cold start with the same EFI store and persistent boot
  configuration.
- [x] Record firmware and FreeBSD versions, exact commands, and remaining limits.
- [x] Add the verified path to the installation regression workflow.

The boot delay came from syncing EFI buffered writes once per four-byte word;
the flash backend now persists each buffer at confirmation. All 39 Rust tests
pass in debug and release; 13 runner tests pass, as do real offline UFS and ZFS
lifecycle runs. The six firmware driver messages are identified in the runbook.
Firmware TLS context initialization remains unresolved, outside local disk boot.

See [ZFS implementation plan](zfs_implementation_plan.md),
[ZFS boot notes](freebsd_zfs_boot.md), and [ZFS runbook](freebsd_zfs_runbook.md).
Those documents now reflect the verified path and supersede the old direct
MODINFO implementation plan.

## 4. Graphical desktop support

The current VNC path provides a simple framebuffer and input devices. A real
FreeBSD 14.4 X11 desktop session passed the disposable regression on 2026-09-22.

- [x] Add configurable framebuffer resolution with memory/stride validation.
- [x] Verify a documented FreeBSD desktop setup, including pointer edges,
  buttons, wheel, modifiers, and reconnect behavior.
- [x] Measure framebuffer update cost during desktop use before selecting
  further display optimizations.
- [x] Investigate FreeBSD guest-driver compatibility and firmware requirements
  for virtio-gpu; record a go/no-go decision before implementation.

`--display WIDTHxHEIGHT` now drives one validated geometry through the FDT, VNC
handshake, and both tablet devices. Reservations grow from the 8 MiB minimum,
are page aligned, and are limited to 256 MiB. The lifecycle check exercises a
non-default resolution and invalid geometry.

FreeBSD 14.4's ARM64 GENERIC kernel includes its base-system 2D virtio-gpu
console driver, while ArmVirtQemu EDK2 includes VirtioGpuDxe. The decision is to
defer the host device: the FreeBSD driver is a `vt(4)` framebuffer rather than
an accelerated DRM desktop, so measurements from simple-framebuffer come first.
The user-confirmed run passed unattended install, XI2 pointer edges, primary
button, both wheel directions, modifiers, disconnect releases, VNC reconnect,
installed-disk SSH, guest reboot, marker persistence, and clean poweroff.
Its 32 full/incremental framebuffer updates averaged 38.4 updates/s and
14.0 MiB/s of raw pixel payload. The VMM read 96.0 MiB of framebuffer data
in 2.6 ms, compared 90.0 MiB of changed rows in 3.6 ms, and spent 291.0 ms
sending 11.7 MiB across the initial and reconnected VNC sessions. Sending
dominated these measured server costs. These measurements support keeping the
simple-framebuffer path for this milestone; further display optimization can
be based on workloads that show a limiting cost. The regression enables
FreeBSD 14.4's `usbhid`/`hms` path at runtime and reprobes the tablet because
direct UFS kernel boot bypasses `loader.conf`. The retained guest log confirms
`uhid0` detached and `hms0` attached with five buttons and X/Y/wheel axes.

See [GUI architecture](gui_architecture.md) and the
[FreeBSD desktop runbook](freebsd-desktop.md).

## 5. Per-VM configuration and management

VM settings currently require command-line arguments and separate paths. A
host-side stop flushes backends but does not ask the guest OS to shut down.

- [ ] Define a per-VM configuration for disks, RAM, CPUs, networking, VNC, and
  EFI variables, with documented command-line override behavior.
- [ ] Isolate runtime files and identify running VM instances reliably.
- [ ] Add status reporting and define a guest-coordinated shutdown mechanism.
- [ ] Distinguish graceful shutdown from forced stop, including timeout behavior.
- [ ] Verify two concurrent VMs do not share stores, mailboxes, or conflicting
  listeners accidentally.

## 6. Offline disk-overlay management

COW overlays have validation and locking, but no dedicated management commands.

- [ ] Add read-only inspection of geometry, allocation, base association, and
  consistency errors.
- [ ] Export a base-plus-overlay view to a new standalone image.
- [ ] Define offline commit semantics and recovery behavior before implementing
  writes to the base image.
- [ ] Respect active-image locks and reject incompatible or corrupt inputs.
- [ ] Test exported contents, partial final clusters, malformed overlays, and
  failure handling without modifying original images during inspection/export.

Start with the COW backend in `src/devices/virtio/block.rs`. These tools concern
disk contents; full VM snapshots including memory and device state are outside
this item's scope.
