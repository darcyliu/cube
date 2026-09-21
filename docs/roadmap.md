# Follow-up roadmap

Recorded 2026-09-20 after the device/lifecycle hardening and unattended-console
fixes in `06fb2b3`. Updated 2026-09-21: item 1 is complete with regression coverage
and user-confirmed guest SSH forwarding. Next is the full installation regression
test in item 2.
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

Current checks cover diskless CPU lifecycle, VNC input, and installer shell
interaction. They do not verify that an unattended installation produces a
bootable system with working SSH.

- [ ] Install FreeBSD onto a newly created disposable disk with isolated seed,
  console log, UART mailbox, and EFI store where applicable.
- [ ] Require an explicit installer success result and clean VM exit.
- [ ] Boot the installed disk without the installation media and verify SSH
  command execution through the configured forwarding rule.
- [ ] Create a marker in the guest, reboot, and verify it persists and SSH returns.
- [ ] Bound each phase with a timeout, retain diagnostic logs on failure, and
  clean up only resources created by the test.
- [ ] Document prerequisites and a reproducible invocation. Never use an
  existing user disk as the test destination.

Build on `scripts/check_vm_lifecycle.py`, `scripts/check_unattended_console.py`,
and `scripts/unattended-install.sh`. Depends on item 1 for reliable forwarding.

## 3. Complete UEFI/ZFS boot verification

Firmware entry and shutdown pass, and EFI-store persistence has a regression
test. The tested firmware also reported driver-load errors; a full installed
UEFI/ZFS boot has not been verified in this work.

- [ ] Reproduce and explain the firmware driver-load errors; fix failures that
  prevent the supported boot path.
- [ ] Verify installation and boot of a disposable FreeBSD ZFS system via UEFI.
- [ ] Verify reboot and cold start with the same EFI store and persistent boot
  configuration.
- [ ] Record firmware and FreeBSD versions, exact commands, and remaining limits.
- [ ] Add the verified path to the installation regression workflow.

See [ZFS implementation plan](zfs_implementation_plan.md),
[ZFS boot notes](freebsd_zfs_boot.md), and [ZFS runbook](freebsd_zfs_runbook.md).
Reconcile those documents with the test results rather than assuming every
previously proposed step is still needed.

## 4. Graphical desktop support

The current VNC path provides a simple framebuffer and input devices. Console
input has been tested; a complete desktop session needs separate validation.

- [ ] Add configurable framebuffer resolution with memory/stride validation.
- [ ] Verify a documented FreeBSD desktop setup, including pointer edges,
  buttons, wheel, modifiers, and reconnect behavior.
- [ ] Measure framebuffer update cost during desktop use before selecting
  further display optimizations.
- [ ] Investigate FreeBSD guest-driver compatibility and firmware requirements
  for virtio-gpu; record a go/no-go decision before implementation.

See [GUI architecture](gui_architecture.md). Virtio-gpu is an investigation,
not a prerequisite for the initial desktop checks.

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
