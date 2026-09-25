# Running the verification suites

Use `scripts/verify.py` as the single entry point after code changes and in CI.
It builds and signs a debug VMM, runs Rust tests in debug and release, discovers
all `scripts/test_*.py` tests, and invokes the existing integration scripts.
Checks run sequentially, except the management check intentionally boots two
isolated VMs together to exercise concurrent ownership.

## Quick checks after a code change

From the repository root:

```sh
python3 -B scripts/verify.py
# Equivalent:
make verify
```

This needs native Apple Silicon macOS, Rust/Cargo, Xcode command-line tools,
Python 3, `dtc`, and the system `codesign`, `bash`, and `ssh-keygen` commands.
The machine must allow Hypervisor.framework execution. Rosetta, Linux, and hosts
without usable virtualization are not supported by this suite. The runner does
not download dependencies, guest media, or firmware explicitly; Cargo may need
network access for uncached dependencies.

The default `quick` suite needs no ISO, firmware, or sudo. It includes diskless
VM lifecycle checks and network startup/error checks. Passing it does not claim
that a full installation or guest networking was tested.

## All offline checks

Provide an ARM64 **disc1** ISO containing `base.txz` and `kernel.txz`, plus EDK2
firmware. The runner extracts the kernel into its private artifact directory.

```sh
python3 -B scripts/verify.py --suite offline \
  --iso FreeBSD-14.4-RELEASE-arm64-aarch64-disc1.iso \
  --firmware firmware/QEMU_EFI.fd
```

This additionally needs `hdiutil`, `tar`, and `ssh`. It runs without root and uses
serial-console commands for disposable UFS and UEFI/ZFS installations. Allow
2 GiB guest RAM and several GiB of disk space per installation. Failed runs
retain their disposable disks; repeated failures can consume substantial space.

## Full suite, including real networking

Choose an **unused** guest IPv4 address on the actual vmnet shared subnet and a
reachable host address on that subnet. The runner validates subnet consistency,
but cannot establish whether an address belongs to another machine. The example
addresses below must be replaced if your subnet differs.

```sh
sudo -v
python3 -B scripts/verify.py --suite full --sudo \
  --iso FreeBSD-14.4-RELEASE-arm64-aarch64-disc1.iso \
  --firmware firmware/QEMU_EFI.fd \
  --guest-cidr 192.168.2.77/24 --host-ip 192.168.2.1
```

`--sudo` elevates only the three real-network guest checks with `sudo -n`.
It never opens a password prompt, builds as root, or silently substitutes mock
networking. If authentication expires or is unavailable, the run fails. A CI
host needs noninteractive sudo configured for its trusted test workload, or a
VMM signed with authorized networking entitlements. For the latter, omit
`--sudo` and pass the signed VMM through `--binary` so it is not re-signed.
Run the Python verifier as your regular user after `sudo -v`; placing `sudo`
before `python3` makes the entire artifact directory root-owned.
If you do launch the entire verifier through sudo, the shared-folder check
creates its disposable export separately under `/private/tmp` and assigns it
to the original sudo user. This lets the unprivileged filesystem helper access
the export without making the root-owned artifact tree accessible. The step's
artifact directory contains `export-path.txt`; failed/kept runs retain both
locations. Direct root launches without `SUDO_UID`/`SUDO_GID` cannot run this check.
The network SSH check retains `network-guest-ssh.console.log` beside its step
log so a failed guest setup can be distinguished from a forwarding failure.
The VMM's existing vmnet initialization can modify host IP forwarding and its
PF NAT anchor; run the full suite on an appropriate test host.

| Check | quick | offline | full |
|---|:---:|:---:|:---:|
| `cargo test --locked` and `--release` | yes | yes | yes |
| Offline COW inspect/export, malformed-input and lock tests (Rust) | yes | yes | yes |
| Python `test_*.py` discovery | yes | yes | yes |
| Build and sign VMM (or verify `--binary`) | yes | yes | yes |
| `check_vm_lifecycle.py`: diskless CPU/reset | yes | yes | yes |
| Lifecycle: custom-resolution FreeBSD VNC, metrics, and firmware | — | yes | yes |
| `check_network.py`: startup and bind errors | yes | yes | yes |
| `check_unattended_console.py` | — | yes | yes |
| Shared folders: host/guest changes, permissions and reboot | — | yes | yes |
| Two concurrent managed FreeBSD VMs and listener collision | — | yes | yes |
| `check_install.py`: UFS offline | — | yes | yes |
| `check_install.py`: UEFI/ZFS offline, reboot/cold start | — | yes | yes |
| Network: alternate static address and SSH forwarding | — | — | yes |
| Installation: UFS and UEFI/ZFS with SSH | — | — | yes |
| FreeBSD X11/scfb input and update measurements | — | — | yes (UFS) |

The interactive installation, signing, seed-generation, and cleanup utilities
are not independent tests and are not executed as top-level verification steps.
Their relevant behavior is exercised by the existing checks. New Python unit
test files matching `test_*.py` are discovered automatically; new integration
scripts should be added explicitly to `make_plan()` in `scripts/verify.py`.
The full suite's UFS SSH installation passes `--desktop` to
`check_install.py`. It installs Xorg/scfb in that disposable guest, invokes
`check_desktop.py`, and retains its X11 event and performance evidence in the
suite artifact directory. See
[FreeBSD desktop verification](freebsd-desktop.md).
The standalone UFS `--desktop --keep` run passed on 2026-09-22, including all
three X11/VNC checks and the subsequent reboot and clean poweroff. The 32
framebuffer updates averaged 38.4 updates/s and 14.0 MiB/s raw payload.

## Options and results

- `--list` prints the selected commands without executing them or creating
  artifacts. Supply the suite's required input arguments; files need not exist
  for listing.
- `--target-dir PATH` selects the Cargo build directory. It defaults to
  `CARGO_TARGET_DIR` when set, otherwise the repository's `target` directory.
- `--binary PATH` uses an existing signed VMM and skips build/sign; Rust tests
  still compile and run. Use this to validate a separately built release binary.
  Without it, integration checks use the newly built debug binary.
- `--artifacts PATH` selects the parent of a new private `vmm-verify-*` directory.
  The default is the gitignored `test-results/` directory. Paths supplied on the
  command line resolve relative to the caller's current directory; commands run
  from the repository root.

Each step has a retained `.log`, with its exact command at the top. Output shows
step starts, results, and failure log tails. The runner continues independent
checks after failure; checks whose setup failed are explicitly marked skipped.
It rewrites `summary.json` and `junit.xml` after each step and on handled
cancellation. JUnit contains one testcase per top-level step; detailed Rust and
Python test output remains in the step logs.

Exit status is `0` only when all selected steps pass, `1` on failure or dependent
skips, `2` for invalid CLI arguments, and `130` on handled interruption. Missing
prerequisites fail preflight and appear in both reports. Unselected suites are
reported as excluded coverage, not as passing checks.

Outer limits are 15 minutes for each Cargo step, 5 minutes for normal check
scripts, 30 minutes for ordinary install workflows, and 50 minutes for the UFS
desktop workflow; installation phases also retain their own internal timeouts.
On a timeout or Ctrl-C/SIGTERM, the runner
interrupts the current check and gives its cleanup handlers 30 seconds to stop
VMs before forcing termination. Uncatchable termination such as SIGKILL or host
loss cannot guarantee cleanup or a final report.

Logs and reports are always retained. Successful install workflows delete their
disposable images; failed installations retain a private subdirectory with
credentials and disk images. Privileged failures may leave root-owned artifacts.
Older lifecycle/network check scripts still clean their own temporary files;
the outer logs preserve their output and printed diagnostics.

## CI usage

Use a trusted native Apple Silicon macOS runner with Hypervisor.framework
available and the prerequisites above. Prepare Cargo dependencies, ISO, and
firmware ahead of time. The same command works without CI-specific wrappers:

```sh
python3 -B scripts/verify.py --suite offline \
  --iso /path/to/FreeBSD-arm64-disc1.iso \
  --firmware /path/to/QEMU_EFI.fd \
  --artifacts "$PWD/test-results"
```

Preserve the command's exit status. Configure the CI system to collect
`test-results/vmm-verify-*/junit.xml`, `summary.json`, and top-level `*.log` even
when the command fails. Do not recursively publish retained guest disks, keys,
or seed images as public CI artifacts. Set the job timeout above the expected
suite duration so normal cancellation can run cleanup.

For the full suite, use the same command with `--suite full`, the explicit
network addresses, and suitable privilege setup. No particular hosted CI runner
image or nested-virtualization capability is assumed.

## Validation, 2026-09-22

The quick suite passed using an existing signed binary. The complete offline
suite passed all 12 top-level steps, including build/sign, 39 Rust tests in each
profile, 23 Python tests, and real UFS/ZFS installations with reboot and ZFS cold
start. JSON and JUnit reports were checked for consistent results. The full
suite's selection and dependencies are unit tested; its privileged networking
steps were not run during this runner change.
