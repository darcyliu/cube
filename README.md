# macOS ARM64 Hypervisor.framework Prototype VMM

A lightweight, zero-external-dependency Virtual Machine Monitor (VMM) written in **Rust** using Apple's native **`Hypervisor.framework`** on Apple Silicon (ARM64).

---

## Features

- **Direct Hypervisor.framework FFI**: Zero-dependency bindings to Apple's low-level ARM64 hypervisor APIs (`hv_vm_*`, `hv_vcpu_*`).
- **Memory Management**: Allocates 16KB-page-aligned host RAM and maps it into Guest Physical Address (GPA) space (`hv_vm_map`).
- **EL1 Execution**: Launches vCPUs running at Exception Level 1 (Supervisor Mode, `CPSR=0x3c5`) with flat physical addressing.
- **MMIO Bus & PL011 UART**: Hardware-accelerated Stage-2 Data Abort handling (`EC 0x24` with `ESR_EL2` syndrome decoding) routed to a virtual ARM PL011 UART console (`0x09000000`).
- **PSCI Hypercall Handler**: Intercepts `HVC #0` (`EC 0x16`) to support ARM Power State Coordination Interface (`SYSTEM_OFF` 0x84000008, `SYSTEM_RESET` 0x84000009).
- **Embedded & External Payloads**: Ships with an embedded baremetal test payload and can load arbitrary flat binary kernels from the command line.
- **Automated Entitlement Signing**: Seamlessly signs debug and release binaries with `com.apple.security.hypervisor`.

---

## Architecture

```
                    Host macOS (Apple Silicon ARM64)
┌────────────────────────────────────────────────────────────────────────┐
│  Rust VMM Process                                                      │
│                                                                        │
│   ┌────────────────┐                      ┌──────────────────────┐     │
│   │   Vm Manager   │                      │  GuestMemory (64MB)  │     │
│   │  (hv_vm_create)├─────────────────────►│  GPA: 0x40000000     │     │
│   └───────┬────────┘                      └──────────────────────┘     │
│           │                                                            │
│           ▼                                                            │
│   ┌────────────────┐                      ┌──────────────────────┐     │
│   │   vCPU Thread  │  Trapped MMIO Exits  │       MmioBus        │     │
│   │  (hv_vcpu_run) ├─────────────────────►│  0x09000000: PL011   │     │
│   └───────┬────────┘                      └──────────┬───────────┘     │
│           │                                          │                 │
│           │                                          ▼                 │
│           │                                     Host stdout            │
└───────────┼────────────────────────────────────────────────────────────┘
            │ Hardware Virtualization (Stage 2 Translation)
            ▼
┌────────────────────────────────────────────────────────────────────────┐
│  Guest Execution (AArch64 EL1)                                         │
│                                                                        │
│   1. Boots at GPA 0x40000000 with SP_EL1 at 0x41000000                 │
│   2. Outputs strings via MMIO byte writes to 0x09000000 (UART)         │
│   3. Performs memory read/write validation                             │
│   4. Issues `HVC #0` with x0=0x84000008 (PSCI SYSTEM_OFF) to shutdown  │
└────────────────────────────────────────────────────────────────────────┘
```

---

## Quick Start

### Build & Run
```bash
# Build, sign with hypervisor entitlement, and run:
make run
```

Or manually:
```bash
cargo build
codesign --entitlements entitlements.plist --force -s - target/debug/vmm
./target/debug/vmm
```

### Loading an External Binary
```bash
./target/debug/vmm path/to/payload.bin
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
│   ├── payload.S            # Baremetal ARM64 assembly test program
│   └── payload.bin          # Extracted flat raw binary payload
└── src/
    ├── main.rs              # CLI entry point, VM wiring, and run orchestration
    ├── hypervisor/
    │   └── mod.rs           # FFI bindings, register definitions, error wrappers
    ├── memory.rs            # GuestMemory (16KB page-aligned allocator and mapper)
    ├── vcpu.rs              # vCPU abstraction, ESR_EL2 syndrome decoding & run loop
    ├── vm.rs                # High-level VM container
    └── devices/
        ├── mod.rs           # MmioDevice trait and MmioBus dispatcher
        └── uart.rs          # ARM PL011-compatible virtual serial console
```
