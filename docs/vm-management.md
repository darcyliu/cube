# Named VM configuration and management

`scripts/vmmctl.py` manages one VMM process per named JSON configuration. It
uses Python's standard library and the existing VMM binary. The manager keeps
an instance identity, logs, EFI variables, and a UART mailbox under
`~/.local/state/vmm/NAME/` by default. The control socket lives under a
private `/tmp/vmmctl-UID/` directory with a short, instance-specific name so
deep artifact paths work on macOS. Its exact path appears in `status`. Use
`--state-root DIR` on every command to choose another
root. VM names contain letters, digits, `_`, or `-` and are unique within that
root. `status`, `shutdown`, and `stop` accept either the configuration path or
the bare VM name, so they still work if the JSON file is moved after launch.

Place a file such as `vms/freebsd-zfs.json` in the repository:

```json
{
  "name": "freebsd-zfs",
  "binary": "../target/debug/vmm",
  "boot": {"mode": "uefi", "firmware": "../firmware/QEMU_EFI.fd"},
  "disks": [{"path": "/path/to/freebsd-zfs.img"}],
  "ram_mb": 2048,
  "cpus": 2,
  "network": {
    "mode": "vmnet",
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

```sh
sudo -v   # only needed for vmnet/bridge when the binary lacks a networking entitlement
python3 -B scripts/vmmctl.py start vms/freebsd-zfs.json --sudo
python3 -B scripts/vmmctl.py status vms/freebsd-zfs.json --sudo
python3 -B scripts/vmmctl.py shutdown vms/freebsd-zfs.json --timeout 120 --sudo
# If the guest cannot be reached, explicitly stop the host process:
python3 -B scripts/vmmctl.py stop vms/freebsd-zfs.json --timeout 10 --sudo
```

`start` returns the instance ID, VMM PID, and resource claims. `status` asks the
live manager over its Unix socket; it does not trust a stale PID file. After an
exit, it reports the last recorded exit code. Logs are `vmm.log`, `console.log`,
and `manager.log` in the instance directory. A new run reuses the EFI store and
replaces the run logs. The manager reserves the VM name, each writable disk or
COW overlay, and each TCP listener port against other managed VMs. Shared
read-only disks are allowed. The VMM's own disk locks and socket binds still
enforce these constraints against processes launched outside the manager.
`--sudo` elevates the foreground command before it detaches the manager; use it
on later commands for the same privileged instance. The detached manager cannot
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
`--ssh-port`, `--vnc`, `--display`, `--efi-vars`, `--disk`, and `--forward`.
Each supplied scalar replaces its JSON value. Any `--disk` replaces the entire
disk list; any `--forward` replaces the entire forwarding list. CLI paths are
relative to the current directory. An override affects that run only; it does
not rewrite the configuration file. The effective VMM command and resource
claims are retained in `launch.json` for inspection.

The local regression suite starts two fake VMM processes concurrently and
checks instance identity, private runtime paths, conflicting resources, and
shutdown/stop behavior. `scripts/check_vm_management.py` boots two real FreeBSD
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
sudo -v
python3 -B scripts/check_vm_shutdown.py target/debug/vmm \
  --disk /path/to/install-artifacts/installed.img \
  --ssh-key /path/to/install-artifacts/id_ed25519 \
  --firmware firmware/QEMU_EFI.fd \
  --efi-vars /path/to/install-artifacts/efi-vars.fd
```

This check requires working vmnet and noninteractive sudo for the VMM. Failure
artifacts are retained; `--keep` also retains a passing clone.

## Shared folders

The optional `shares` array contains `tag`, `path` and `read_only` (default
`true`). Paths are relative to the JSON file. Repeatable `--share TAG PATH` and
`--share-rw TAG PATH` options replace that array for a launch; CLI paths are
relative to the working directory. The effective exports appear in launch and
status records. Shared directories can be used by multiple managed VMs.
See the [shared-folder runbook](shared-folders.md) for guest commands and the
single-user ownership mapping.
