# Named VM configuration and management

Native `cubectl` manages one Cube VMM process per named JSON configuration.
The native `cubectl` CLI is built in Rust, requires no Python runtime, and is bundled inside `Cube.app`.

### Where VM files are stored

By default, VMs are separated into two directories:
1. **VM Files & Virtual Disks**: stored under `~/Cube/<NAME>/` (e.g. `~/Cube/my-freebsd/my-freebsd.json`, `rootfs.img`).
   - **Customizing per VM**: Pass `--dest <PATH>` when creating (e.g. `cubectl create my-vm --dest /Volumes/Data/Cube/my-vm`).
   - **Changing the global default**: Run `cubectl config set vms-dir <PATH>` or set `export CUBE_VMS_DIR=/Volumes/Data/Cube` in your shell profile.
   - **Moving existing VMs**: Move the directory (`mv ~/Cube/my-vm /new/path/my-vm`) and update any absolute disk paths in the `.json` file.
2. **Runtime State, Logs & Sockets**: stored under `~/.local/state/cube/<NAME>/` (or override with `--state-root <DIR>`). The manager control socket lives under `/tmp/cubectl-UID/`.

Place a file such as `vms/freebsd-zfs.json` in the repository:

```json
{
  "name": "freebsd-zfs",
  "binary": "../target/debug/cube",
  "boot": {"mode": "uefi", "firmware": "../firmware/QEMU_EFI.fd"},
  "disks": [{"path": "/path/to/freebsd-zfs.img"}],
  "ram_mb": 2048,
  "cpus": 2,
  "network": {
    "mode": "vmnet-proxy",
    "ssh_port": 2222,
    "forwards": ["127.0.0.1:8080=192.168.2.77:80"]
  },
  "vnc": {"bind": "127.0.0.1:5901", "display": "1280x720"},
  "shutdown": {
    "host": "127.0.0.1", "port": 2222, "user": "root",
    "identity_file": "/path/to/guest-ssh-key"
  }
}
```

For direct FreeBSD kernel boot, use
`"boot": {"mode": "freebsd", "kernel": "../boot/kernel/kernel", "multiuser": true}`.
`disks` accepts path strings or objects with `path`, `cow`, and `read_only`.
Set `read_only: true` for shared installation media. `efi_vars` may be set to
an explicit path; otherwise the manager uses the instance's private
`efi-vars.fd`. Paths in the JSON file are relative to that file. The manager
assigns a stable locally administered MAC from the VM name unless `network.mac`
is set. `network.mode` defaults to `none`; `vnc` and `shutdown` are optional.
The SSH endpoint used for shutdown must be reachable from the host. The example
uses `ssh_port` forwarding to a DHCP guest; use a static `network.forwards`
rule and matching shutdown port for a static guest address.

### Choosing UEFI or direct kernel boot

Cube supports two FreeBSD boot paths. They expose the same virtual devices, but
they differ in who selects and prepares the kernel.

| | UEFI boot | Direct kernel boot |
|---|---|---|
| Configuration | `"mode": "uefi"` with a firmware path | `"mode": "freebsd"` with a kernel path |
| Boot sequence | EDK2 → disk EFI loader → FreeBSD loader → kernel | Cube → host-supplied FreeBSD kernel |
| Kernel source | Loaded from the guest disk | Loaded from a host file |
| `/boot/loader.conf` | Read by the FreeBSD loader | Bypassed |
| Kernel modules | The loader can load modules from the guest disk | Must be built in, loaded at runtime, or supplied with `--module` |
| EFI variables | Persist through the VM's `efi-vars.fd` | Not used |
| Best suited to | Installed systems, ZFS, and normal Cube.app use | Kernel development, diagnostics, and disposable tests |

Use UEFI for a normal installed VM. It follows the conventional FreeBSD boot
chain, supports the disk's bootloader and `/boot/loader.conf`, and automatically
uses a private EFI variable store unless `efi_vars` or `--efi-vars` selects one.
Guest kernel upgrades take effect because the next boot loads the kernel from
the disk.

```sh
cube --uefi firmware/QEMU_EFI.fd \
  --disk /path/to/installed.img \
  --efi-vars /path/to/efi-vars.fd
```

Direct kernel boot skips EDK2 and the FreeBSD loader. It starts the specified
kernel immediately, which is useful for development and repeatable tests, but
loader tunables and module entries in `/boot/loader.conf` do not take effect.
Keep the host-side kernel file synchronized with the guest when testing an
installed disk.

```sh
cube --freebsd /path/to/kernel \
  --disk /path/to/installed.img \
  --root ufs:/dev/vtbd0p2 --multiuser
```

For stock FreeBSD root-on-ZFS, use UEFI. Cube's direct kernel/module ZFS path is
experimental; the verified path is documented in the
[FreeBSD UEFI/ZFS runbook](freebsd_zfs_runbook.md).

```sh
# ----------------------------------------------------------------------
# 1. Automated Unattended OS Installation (Approach A - Recommended)
# ----------------------------------------------------------------------
# Create, auto-generate CIDATA seed, install OS unattended, and configure VM in 1 step:
cubectl create my-freebsd \
  --iso FreeBSD-14.4-RELEASE-arm64-aarch64-disc1.iso \
  --unattended \
  --ram 2048 --cpus 2 \
  --ssh-port 2222 \
  --password root

# Install a graphical desktop as part of the same unattended workflow:
cubectl create my-desktop \
  --version 15.1 --unattended --desktop xfce \
  --ram 4096 --cpus 4 --password change-me

# KDE Plasma is also available with --desktop plasma. The desktop account is
# "cube" by default and uses --desktop-password, or --password when omitted.

# Start directly into your ready-to-use FreeBSD VM:
cubectl start my-freebsd

# Start the same managed VM in a native Cocoa window instead of its saved VNC frontend:
cubectl start my-freebsd --gui native

# Connect via SSH with key or password:
ssh root@127.0.0.1 -p 2222

# Or run unattended install on an existing VM:
cubectl install my-freebsd --iso FreeBSD-14.4-RELEASE-arm64-aarch64-disc1.iso

# ----------------------------------------------------------------------
# 2. Interactive ISO Installation (Manual / VNC)
# ----------------------------------------------------------------------
# Create VM specification with a 20GB sparse disk & attach installer ISO
cubectl create my-ubuntu \
  --iso /path/to/ubuntu-24.04-live-server-arm64.iso \
  --ram 2048 --cpus 2 \
  --ssh-port 2222

# Start VM in background (or run with -i --no-reboot for interactive installation)
cubectl start my-ubuntu -i --no-reboot

# Connect to the graphical installer (if VNC is enabled)
brew install --cask tigervnc  # one-time setup
/Applications/TigerVNC.app/Contents/MacOS/vncviewer 127.0.0.1::5901

# Once installed, stop the VM and eject the installer ISO
cubectl stop my-ubuntu
cubectl eject my-ubuntu

# Start into the installed OS
cubectl start my-ubuntu

# ----------------------------------------------------------------------
# 3. VM Lifecycle Management
# ----------------------------------------------------------------------
# List all known VMs and status
cubectl list

# Inspect live VM status
cubectl status my-freebsd

# Request clean guest shutdown over SSH
cubectl shutdown my-freebsd --timeout 120

# Force stop the VM if guest does not respond
cubectl stop my-freebsd --timeout 10

# Delete a VM, its disks, and state directory
cubectl delete my-freebsd

# Or force-stop and delete in one step:
cubectl delete my-freebsd --force
# (alias: cubectl rm my-freebsd -f)
```

`start` returns the instance ID, VMM PID, and resource claims. `status` asks the
live manager over its Unix socket; it does not trust a stale PID file. After an
exit, it reports the last recorded exit code. Logs are `vmm.log`, `console.log`,
and `manager.log` in the instance directory. A new run reuses the EFI store and
replaces the run logs. The manager reserves the VM name, each writable disk or
COW overlay, and each TCP listener port against other managed VMs. Shared
read-only disks are allowed. The VMM's own disk locks and socket binds still
enforce these constraints against processes launched outside the manager.
`--sudo` elevates the foreground command before it detaches the manager. Use it
for network mode `vmnet` or `bridge`, and on later commands for that same
privileged instance. `vmnet-proxy` rejects it. The detached manager cannot
authenticate through a terminal-scoped sudo ticket.

`shutdown` sends `/sbin/shutdown -p now` over batch-mode SSH. It reports
`guest-poweroff` only after the VMM exits successfully and logs a guest PSCI
`SYSTEM_OFF`. An SSH disconnect during poweroff is expected. If the timeout
expires, the VM keeps running unless `--force-after-timeout` was supplied; that
option returns `forced-after-timeout`. `stop` sends SIGTERM to the VMM process
group, allows backend cleanup for the requested timeout, then uses SIGKILL if
needed. It is a host stop, not a guest-coordinated shutdown. Uncatchable manager
termination or host loss cannot guarantee a final state record.

Only `start` accepts `--binary`, `--ram`, `--cpus`, `--net`, `--mac`,
`--ssh-port`, `--vnc`, `--gui native`, `--display`, `--efi-vars`, `--disk`, and `--forward`.
Each supplied scalar replaces its JSON value. Any `--disk` replaces the entire
disk list; any `--forward` replaces the entire forwarding list. CLI paths are
relative to the current directory. An override affects that run only; it does
not rewrite the configuration file. The effective VMM command and resource
claims are retained in `launch.json` for inspection.

`--gui native` suppresses the saved VNC frontend for that run; the two display
frontends cannot be combined. In Cube.app, choose **View CLI Commands…**, select
**Native**, and click **Start with Native Display**. The menu bar VM submenu also
offers **Start with Native Display** when the VM is stopped.

`cubectl status` and `cubectl list --json` include an `ip` field for running
VMs. The hypervisor publishes its NIC's learned DHCP address to `runtime.json`
in the instance state directory. Updates are atomic, carry a version and launch
ID, and are checked every 250 ms; the file is rewritten only when the address
changes. The manager rejects data from previous launches and returns `null`
when no address is known or the VM is stopped. NIC resets clear the address.
Discovery does not parse logs or macOS DHCP lease files. Static guest addresses
are not detected by this mechanism. After upgrading, rebuild the app and restart
the VM to enable the structured status publisher and manager.

The local regression suite starts two fake VMM processes concurrently and
checks instance identity, private runtime paths, conflicting resources, and
shutdown/stop behavior. `tests/cubectl/check_vm_management.py` boots two real FreeBSD
guests concurrently from the same read-only ISO, checks their separate EFI,
serial, UART, and VNC resources, and rejects a third conflicting listener. It
passed on Apple Silicon on 2026-09-23 and runs in the offline/full verifier
suites. The user confirmed the privileged installed-guest SSH poweroff check
passed on 2026-09-23.

To verify it with an existing disposable install artifact, use its disk and
matching SSH key. The check clones the disk with APFS `cp -c`, starts the clone
through the manager, waits for SSH, asks the guest to power off, and requires
the VMM's PSCI `SYSTEM_OFF` record. The source disk is never opened for writing.
Use `--efi-vars` to clone a matching EFI store when booting it depends on one.

```sh
python3 -B tests/cubectl/check_vm_shutdown.py target/debug/cube \
  --cubectl target/debug/cubectl \
  --disk /path/to/install-artifacts/installed.img \
  --ssh-key /path/to/install-artifacts/id_ed25519 \
  --firmware firmware/QEMU_EFI.fd \
  --efi-vars /path/to/install-artifacts/efi-vars.fd
```

This check uses `vmnet-proxy` and needs the vmnet service. Pass `--sudo` after
`sudo -v` for in-process vmnet via noninteractive `sudo -n`. Failure artifacts
are retained; `--keep` also retains a passing clone.

## Shared folders

The optional `shares` array contains `tag`, `path` and `read_only` (default
`true`). Paths are relative to the JSON file. Repeatable `--share TAG PATH` and
`--share-rw TAG PATH` options replace that array for a launch; CLI paths are
relative to the working directory. The effective exports appear in launch and
status records. Shared directories can be used by multiple managed VMs.
See the [shared-folder runbook](shared-folders.md) for guest commands and the
single-user ownership mapping.
