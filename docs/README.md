# Cube documentation

Start with the [project README](../README.md) for building and launching Cube.
This index groups the documents by purpose without changing their existing paths.

## Guides

- [VM management](vm-management.md): named configurations, launch, status, and shutdown.
- [Cube.app](cube_app.md): app networking, signing, and desktop installation.
- [FreeBSD runbook](freebsd_runbook.md): installation and direct-kernel boot.
- [FreeBSD desktop](freebsd-desktop.md): display setup and desktop checks.
- [Networking](networking.md) and [entitlements](entitlements.md): connectivity and host permissions.
- [Shared folders](shared-folders.md): delivered 9P and virtio-fs behavior and limits.
- [Offline overlays](overlay-management.md): inspection and export.

## Verification

- [Verification suites](verification.md): test selection, prerequisites, and results.
- [Installation regression](install-regression.md): disposable install and lifecycle checks.
- [UEFI/ZFS regression](freebsd_zfs_runbook.md): commands and dated storage/firmware evidence.
- [VirtIO live verification](virtio-live-verification.md): authoritative live qualification checklist and recorded runs.

Dated passing results describe the recorded configuration and workload. They do
not imply that every current configuration or later change has been verified.

## Architecture

- [Display and input](gui_architecture.md): implemented paths, design history, and limits.
- [ZFS boot](freebsd_zfs_boot.md): supported boot architecture, implementation status, and experimental alternatives.
- [vmnet service design](vmnet-service-design.md) and [wire protocol](vmnet_protocol.md).
- [Hypervisor networking background](hypervisor_networking.md).

## Planning and delivery history

- [Next features](next-features.md): authoritative priority order for future work.
- [Completed milestones](completed-milestones.md): historical delivery and acceptance evidence.
- [VirtIO capability audit](virtio-feature-plan.md): implementation support matrix and original delivery rationale; live qualification belongs in the verification checklist.
- [Shared-folder implementation plan](shared-folders-plan.md): original design rationale; current user-facing support belongs in the shared-folder guide.
- [Superseded ZFS plan](zfs_implementation_plan.md): compatibility link to the consolidated boot status.

Keep current behavior in guides and architecture documents, recorded test evidence
in runbooks, and future priorities in the next-features roadmap. Historical plans
and completed milestones should link to those sources rather than maintain a
second current-status checklist.
