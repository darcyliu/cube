# Codesigning & entitlements

`vmm` uses Apple Hypervisor.framework and optionally vmnet.framework.
Both are gated by entitlements on the signed binary.

## What we sign with

`make sign` / `make all` runs:

```sh
codesign --entitlements entitlements.plist --force -s - "$VMM_BIN"
```

`entitlements.plist` (used by `make sign`) requests **only**:

| Entitlement | Purpose | Ad-hoc (`-s -`) |
|---|---|---|
| `com.apple.security.hypervisor` | Create VMs via Hypervisor.framework | Granted for local builds |

Do **not** put `com.apple.vm.networking` in the ad-hoc entitlements file: on an
ad-hoc signature that restricted entitlement can make AMFI **SIGKILL** the
process at launch (`Killed: 9`). Use `sudo` + vmnet for local networking, or
`make sign-networking` once Apple has granted the capability.

`entitlements.networking.plist` + `make sign-networking` request both hypervisor
and `com.apple.vm.networking`, and must be signed with a Developer identity and
provisioning profile that authorize the restricted entitlement.

## Real NIC without restricted entitlement (recommended for now)

Run the VMM as root so vmnet shared-mode can start:

```sh
make all
sudo make sudo-freebsd-zfs-uefi DISK=freebsd-installed.img
```

That target also installs a small PF NAT anchor (`com.apple/vmm`) so the guest
subnet `192.168.18.0/24` can reach the internet via your default interface.
See [networking.md](networking.md).

If you start without sudo and vmnet fails, the VMM **falls back to a mock
backend** and prints a yellow warning — guest `vtnet0` will not reach the LAN
or Internet until you re-run under sudo (or with a real networking signature).

## Optional: Developer ID + vm.networking

If you obtain the restricted capability:

1. Use `entitlements.networking.plist` (not the ad-hoc `entitlements.plist`).
2. Set `IDENTITY`, `PROFILE`, and `BUNDLE_ID`, then run `make sign-networking`
   (builds/signs `dist/Fvmm.app` with the profile embedded).
3. Confirm:

```sh
codesign -d --entitlements - --xml "$VMM_BIN" | plutil -p -
```

You should see both entitlements as `true`. Then `make freebsd-zfs-uefi …
--net vmnet` can work without sudo.

## Related

- `--net vmnet|mock|none` CLI — [networking.md](networking.md)
- UEFI ZFS boot — [freebsd_zfs_runbook.md](freebsd_zfs_runbook.md)
