# macOS ARM64 Hypervisor.framework VMM

A lightweight, zero-external-dependency Virtual Machine Monitor (VMM) written in **Rust** using Apple's native **`Hypervisor.framework`** on Apple Silicon (ARM64).

It supports booting both custom baremetal ARM64 payloads at EL1 and full operating system kernels like **FreeBSD 14-RELEASE**.

---

## Features

- **Direct Hypervisor.framework FFI**: Zero-dependency bindings to Apple's low-level ARM64 hypervisor APIs (`hv_vm_*`, `hv_vcpu_*`, `hv_gic_*`).
- **Memory Management**: Allocates 16KB-page-aligned host RAM and maps it into Guest Physical Address (GPA) space (`hv_vm_map`).
- **EL1 Execution**: Launches vCPUs running at Exception Level 1 (Supervisor Mode, `CPSR=0x3c5`) with flat physical addressing.
- **In-Kernel ARM GICv3**: Hardware-accelerated interrupt routing using macOS 15's native `hv_gic_create()` (GICD at `0x08000000`, GICR at `0x080a0000`).
- **Device Tree (FDT) Synthesis**: Generates an ARM virt Device Tree blob describing memory, vCPU topology, PSCI, timers, and console.
- **ELF64 Kernel Loader**: Parses and loads uncompressed AArch64 kernel ELFs (e.g. FreeBSD `kernel`) at 2MiB physical alignment.
- **MMIO Bus & PL011 UART**: Hardware-accelerated Stage-2 Data Abort handling (`EC 0x24` with `ESR_EL2` syndrome decoding) routed to a virtual ARM PL011 UART console (`0x09000000`).
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
│   └─────────────┬─────────────┘         └───────────────────────────┘  │
│                 │                                                      │
│                 ▼                                                      │
│   ┌───────────────────────────┐         ┌───────────────────────────┐  │
│   │   vCPU Thread             │  MMIO   │ MmioBus                   │  │
│   │   (hv_vcpu_run)           ├────────►│ 0x09000000: PL011 UART    │  │
│   │   - Stage 2 Data Aborts   │         └─────────────┬─────────────┘  │
│   │   - Debug Regs (OSLAR)    │                       │                │
│   │   - PSCI HVC calls        │                       ▼                │
│   └─────────────┬─────────────┘                  Host stdout           │
└─────────────────┼──────────────────────────────────────────────────────┘
                  │ Hardware Virtualization (Stage 2 Translation)
                  ▼
┌────────────────────────────────────────────────────────────────────────┐
│  Guest Execution (FreeBSD AArch64 / Baremetal EL1)                     │
│                                                                        │
│   1. Boots at GPA 0x40200800 with X0 = 0x48000000 (FDT)                │
│   2. Parses Device Tree: discovers GICv3, Architected Timer, and PL011 │
│   3. Switches to Stage 1 virtual addressing via TTBR0/TTBR1            │
│   4. Probes hardware devices, initializes scheduler, and boots OS      │
└────────────────────────────────────────────────────────────────────────┘
```

---

## Quick Start

### 1. Run the Built-In Baremetal Test Payload
```bash
make run
```

### 2. Boot Official FreeBSD 14-RELEASE
```bash
# Automatically downloads the official ARM64 kernel and boots it:
make freebsd
```

Or manually:
```bash
make fetch-freebsd
./target/debug/vmm --freebsd boot/kernel/kernel
```

---

## Project Structure

```
vmm/
├── Cargo.toml               # Rust package manifest
├── build.rs                 # Links Hypervisor.framework & assembles guest payload
├── entitlements.plist       # com.apple.security.hypervisor entitlement
├── Makefile                 # Convenient build, sign, and run automation
├── guest/
│   └── payload.S            # Baremetal ARM64 assembly test program
└── src/
    ├── main.rs              # CLI entry point, VM wiring, and run orchestration
    ├── hypervisor/
    │   └── mod.rs           # FFI bindings (VM, vCPU, GICv3), register definitions
    ├── memory.rs            # GuestMemory (16KB page-aligned allocator and mapper)
    ├── vcpu.rs              # vCPU abstraction, ESR_EL2 syndrome decoding & trap loop
    ├── vm.rs                # High-level VM container with GICv3 initialization
    ├── elf.rs               # ELF64 parser and segment loader
    ├── fdt.rs               # Device Tree synthesizer (via dtc)
    └── devices/
        ├── mod.rs           # MmioDevice trait and MmioBus dispatcher
        └── uart.rs          # ARM PL011-compatible virtual serial console
```
