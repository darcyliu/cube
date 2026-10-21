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

That target also installs a small PF NAT anchor (`com.apple/cube`) so the guest
subnet `192.168.18.0/24` can reach the internet via your default interface.
See [networking.md](networking.md).

If you start without sudo and `--net vmnet` fails, the VMM exits with an error.
Re-run under sudo (or with an authorized networking signature). For offline
operation, explicitly select `--net mock` to discard packets or `--net none` to
omit the NIC.

Unprivileged guests use a long-running service instead of putting
`com.apple.vm.networking` on this binary. `make sign` stays hypervisor-only.
Build as your user, start the service once, then boot without sudo:

```sh
make all
make vmnet-service
./target/debug/cube --uefi --disk freebsd-installed.img --net vmnet-proxy
```

`--net vmnet` is still the in-process shared interface and still needs root.
`--net bridge` and `--net bridge:en0` create the bridged interface in this
process, so they need root for the same reason, and they do not install PF NAT.
`--net vmnet-proxy` only talks to the socket. Do not sudo a proxy guest: the
service rejects uid 0. A second guest needs its own `--mac`.
`scripts/verify.py --suite full` covers the proxy. In-process vmnet is
`sudo python3 scripts/verify.py --suite full --sudo --keep-root`, and
in-process bridge is `--bridge en0 --keep-root` on that same command. See
[verification.md](verification.md).

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

- `--net vmnet|vmnet-proxy|bridge[:iface]|mock|none` CLI — [networking.md](networking.md)
- Full-suite network modes — [verification.md](verification.md)
- UEFI ZFS boot — [freebsd_zfs_runbook.md](freebsd_zfs_runbook.md)

## DMG distribution signing

`make dmg` requires a **Developer ID Application** certificate and a Developer ID
provisioning profile granting VM Networking. By default it uses
`Cube_Dist.provisionprofile`. The certificate must be one of the exact
`DeveloperCertificates` embedded in that profile; sharing a Team ID is not enough.
The signing script validates both entitlement files, expiration, profile type,
and certificate membership. Distribution signing failures stop the build instead
of falling back to ad-hoc signing. The packaging script also validates the actual
signatures and embedded profiles in both Cube.app and its cube.app helper.

If the error says “No matching signing identity authorized by profile”, regenerate
the Developer ID profile in the Apple Developer account with the installed
Developer ID Application certificate selected, then replace
`Cube_Dist.provisionprofile`. Alternatively, install the certificate and private key
already authorized by the profile. Do not substitute an Apple Distribution or
Mac App Store profile.

```sh
security find-identity -v -p codesigning
make dmg PROFILE=/path/to/updated.provisionprofile
```

Apple documents the certificate/profile relationship in
[TN3125](https://developer.apple.com/documentation/technotes/tn3125-inside-code-signing-provisioning-profiles).
