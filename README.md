# macOS ARM64 Hypervisor.framework VMM

A lightweight, zero-external-dependency Virtual Machine Monitor (VMM) written in **Rust** using Apple's native **`Hypervisor.framework`** on Apple Silicon (ARM64).

It supports booting both custom baremetal ARM64 payloads at EL1 and full operating system kernels like **FreeBSD 14-RELEASE**.

---

## Features

- **Direct Hypervisor.framework FFI**: Zero-dependency bindings to Apple's low-level ARM64 hypervisor APIs (`hv_vm_*`, `hv_vcpu_*`, `hv_gic_*`).
- **Memory Management**: Allocates 16KB-page-aligned host RAM and maps it into Guest Physical Address (GPA) space (`hv_vm_map`).
- **EL1 Execution**: Launches vCPUs running at Exception Level 1 (Supervisor Mode, `CPSR=0x3c5`) with flat physical addressing.
- **In-Kernel ARM GICv3**: Hardware-accelerated interrupt routing using macOS 15's native `hv_gic_create()` (GICD at `0x08000000`, GICR at `0x080a0000`).
- **VirtIO-MMIO Block Device**: Modern VirtIO v2 MMIO block device (`virtio-blk`) supporting sector read/write, disk flushing, split virtqueues, and GIC SPI interrupt signaling (`0x0a000000`, IRQ 34).
- **Interactive PL011 UART Console**: Hardware-accelerated Stage-2 Data Abort handling (`EC 0x24` with `ESR_EL2` syndrome decoding) routed to a bidirectional ARM PL011 UART console (`0x09000000`, IRQ 33) with terminal raw mode and `Ctrl-]` escape.
- **Device Tree (FDT) Synthesis**: Generates an ARM virt Device Tree blob describing memory, vCPU topology, PSCI, GICv3, timers, PL011 console, and VirtIO MMIO.
- **ELF64 Kernel Loader**: Parses and loads uncompressed AArch64 kernel ELFs (e.g. FreeBSD `kernel`) at 2MiB physical alignment.
- **PSCI Hypercall Handler**: Intercepts `HVC #0` (`EC 0x16`) to support ARM Power State Coordination Interface (`SYSTEM_OFF` 0x84000008, `SYSTEM_RESET` 0x84000009).
- **Automated Entitlement Signing**: Seamlessly signs debug and release binaries with `com.apple.security.hypervisor`.

---

## Architecture

```
                    Host macOS (Apple Silicon ARM64)
┌────────────────────────────────────────────────────────────────────────┐
│  Rust VMM Process                                                      │
│                                                                        │
│   ┌───────────────────────────┐         ┌───────────────────────────┐  │
│   │   hv_vm_create            │         │ Guest RAM (512MB)         │  │
│   │   hv_gic_create (GICv3)   ├────────►│ - 0x40200000: Kernel ELF  │  │
│   │   hv_vcpu_create          │         │ - 0x48000000: FDT / DTB   │  │
│   └─────────────┬─────────────┘         └─────────────┬─────────────┘  │
│                 │                                     │                │
│                 ▼                                     ▼ (DMA / Descs)  │
│   ┌───────────────────────────┐         ┌───────────────────────────┐  │
│   │   vCPU Thread             │  MMIO   │ MmioBus                   │  │
│   │   (hv_vcpu_run)           ├────────►│ 0x09000000: PL011 UART    │  │
│   │   - Stage 2 Data Aborts   │         │ 0x0a000000: VirtIO Block  │  │
│   │   - Debug Regs (OSLAR)    │         └───────┬───────────┬───────┘  │
│   │   - PSCI HVC calls        │                 │           │          │
│   └─────────────┬─────────────┘                 ▼           ▼          │
│                 │                          Host Console  Disk Image    │
└─────────────────┼──────────────────────────────────────────────────────┘
                  │ Hardware Virtualization (Stage 2 Translation)
                  ▼
┌────────────────────────────────────────────────────────────────────────┐
│  Guest Execution (FreeBSD AArch64 / Baremetal EL1)                     │
│                                                                        │
│   1. Boots at GPA 0x40200800 with X0 = 0x48000000 (FDT)                │
│   2. Parses Device Tree: discovers GICv3, Timer, PL011, and VirtIO-MMIO│
│   3. Switches to Stage 1 virtual addressing via TTBR0/TTBR1            │
│   4. Probes hardware devices, mounts vtblk0, and enters userland       │
└────────────────────────────────────────────────────────────────────────┘
```

---

## Quick Start

### 1. Run the Built-In Baremetal Test Payload
```bash
make run
```

### 2. Boot Official FreeBSD 14-RELEASE with Real Rootfs (Live Shell)
```bash
# Automatically fetches kernel & official mini-memstick rootfs, mounts /dev/vtbd0p2, and boots to live shell:
make freebsd-root
```
When prompted:
```
Enter full pathname of shell or RETURN for /bin/sh:
```
Press `Enter` to access the live FreeBSD root `#` prompt!

### 3. Boot FreeBSD with VirtIO-Net Networking (NAT & Internet Access)
```bash
# Run with root privileges to automatically enable macOS vmnet.framework NAT:
sudo make sudo-freebsd-net

# For offline unprivileged use, explicitly disable networking:
./target/debug/vmm --freebsd boot/kernel/kernel --net none
```
Inside the FreeBSD single-user shell:
```sh
# 1. Remount root read-write:
mount -u -w /
mkdir -p /var/db /var/run

# 2. Acquire DHCP lease and set default gateway:
dhclient vtnet0
route add default 192.168.18.1

# 3. Configure DNS resolver:
rm -f /etc/resolv.conf
echo "nameserver 1.1.1.1" > /etc/resolv.conf
echo "nameserver 8.8.8.8" >> /etc/resolv.conf

# 4. Fetch web pages across the internet!
fetch -o - http://example.com | head -n 15
fetch --no-verify-peer -o - https://www.freebsd.org | head -n 15
```

For complete architectural details, troubleshooting steps, and NAT forwarding explanations, see [docs/networking.md](docs/networking.md).

Press `Ctrl-]` to stop the VM and return to the host terminal. This joins VM workers
and flushes disk backends, but does not shut down the guest OS; shut down inside
the guest first when possible. Host SIGTERM also requests this cleanup. Guest
PSCI reboot restarts the VM with the same command-line arguments.

VNC accepts one client at a time and binds only to loopback addresses, for example
`--vnc 127.0.0.1:5900`. For remote access, use an SSH tunnel. Disconnecting releases
held keys and pointer buttons. Rectangle updates and RGB565 clients are supported.

UEFI variables persist in `<first-disk>.vars.fd`, or `<firmware>.vars.fd` when
there is no disk. Use `--efi-vars PATH` for an explicit per-VM store. Writable
disk images, COW overlays, and variable stores are exclusively locked while open;
read-only images and COW base images use shared locks. Malformed or truncated
nonempty COW overlays are rejected rather than reinitialized.
Use `vmm-overlay inspect BASE OVERLAY` to validate an offline COW image or
`vmm-overlay export BASE OVERLAY OUTPUT` to create a standalone raw disk. See
[offline overlay management](docs/overlay-management.md).

UART file injection is opt-in: set `VMM_UART_IN` to a dedicated file path.
`scripts/unattended-install.sh` configures this automatically with a private
mailbox for each run, and prints the retained console-log path. Prompt timeouts
stop automation without dispatching installer commands.

For named VM configurations, isolated EFI/console runtime files, status, guest
shutdown over SSH, and explicit host stop, use `scripts/vmmctl.py`. See the
[VM management runbook](docs/vm-management.md).

For shared host folders, add `--share TAG PATH` (read-only) or
`--share-rw TAG PATH` to an OS launch. In FreeBSD 14.4, load `virtio_p9fs` and
run `mount -t p9fs TAG /mnt/host`. Sharing works with `--net none`. See
[shared folders](docs/shared-folders.md) for permissions, manager configuration,
guest setup and current limitations.

Run `python3 -B scripts/verify.py` (or `make verify`) after code changes. This runs
Rust tests in debug/release, Python tests, and diskless integration checks, with
retained logs and JSON/JUnit reports. Use `--suite offline` for all media-based
checks or `--suite full` to also verify real guest networking. See
[verification suites and CI usage](docs/verification.md) for prerequisites and
complete commands.

Individual checks remain available. Run `cargo test` and `cargo test --release`
for device regression tests. On Apple
Silicon macOS, after building and signing the VMM, run the diskless integration
checks with:

```sh
python3 scripts/check_vm_lifecycle.py target/debug/vmm \
  --kernel .iso-boot/boot/kernel/kernel --firmware firmware/QEMU_EFI.fd
```

The kernel and firmware arguments are optional. The checks cover parked CPUs,
CPU off/on, reboot, and (when supplied) FreeBSD VNC input and UEFI shutdown. They
use temporary kernels and variable stores without opening guest disks.

For a disposable full installation, installed-disk SSH, and reboot/persistence
check, see [the installation regression runbook](docs/install-regression.md).
Pass `--desktop-environment xfce` to the UFS installation checker for an opt-in
unattended XFCE/LightDM installation; see the [desktop runbook](docs/freebsd-desktop.md).
For Plasma 6 with SDDM, run `scripts/unattended-plasma-install.sh --iso
FreeBSD-14.4-RELEASE-arm64-aarch64-disc1.iso`; the same runbook documents its
resource defaults and retained VM artifacts.
With `--keep`, the run also writes `reuse-vm.json` beside the installed disk for
later starts through `scripts/vmmctl.py`.
Use `--desktop-user` and `--desktop-password-file` to choose the desktop login
credentials for a new install. The root account has a separate generated password;
use `--root-password-file` to choose it, or read `root-login.txt` in retained
artifacts.
For a local Cocoa/Metal window instead of a VNC viewer, launch an installed
FreeBSD VM with `--gui native` in place of `--vnc 127.0.0.1:5901`. The window
uses the same simple framebuffer and USB keyboard/tablet; see the
[desktop runbook](docs/freebsd-desktop.md) for a complete command. Add `--retina`
to opt into a 2× guest framebuffer at the same window size; set guest desktop
UI scaling separately for readable text.

---

## Project Structure

See the [follow-up roadmap](docs/roadmap.md) for prioritized work and completion
criteria covering networking, installation tests, UEFI/ZFS, graphics, VM
management, and disk-overlay tools.

```
vmm/
├── Cargo.toml               # Rust package manifest
├── build.rs                 # Links Hypervisor.framework & assembles guest payload
├── entitlements.plist       # com.apple.security.hypervisor entitlement
├── Makefile                 # Convenient build, sign, and run automation
├── docs/
│   └── networking.md        # VirtIO-Net architecture, troubleshooting & UX guide
├── guest/
│   └── payload.S            # Baremetal ARM64 assembly test program
└── src/
    ├── main.rs              # CLI entry point, VM wiring, and run orchestration
    ├── hypervisor/
    │   └── mod.rs           # FFI bindings (VM, vCPU, GICv3), register definitions
    ├── memory.rs            # GuestMemory (16KB page-aligned allocator and mapper)
    ├── terminal.rs          # Raw terminal mode guard with Ctrl-] escape handler
    ├── vcpu.rs              # vCPU abstraction, ESR_EL2 syndrome decoding & trap loop
    ├── vm.rs                # High-level VM container with GICv3 initialization
    ├── elf.rs               # ELF64 parser and segment loader
    ├── fdt.rs               # Device Tree synthesizer (via dtc)
    └── devices/
        ├── mod.rs           # MmioDevice trait and MmioBus dispatcher
        ├── uart.rs          # ARM PL011-compatible virtual serial console
        └── virtio/
            ├── mod.rs       # Modern VirtIO MMIO v2 constants and ring structures
            ├── block.rs     # VirtIO block device (virtio-blk) implementation
            └── net.rs       # VirtIO network device (virtio-net) with macOS vmnet backend
```
