mod devices;
mod hypervisor;
mod memory;
mod vcpu;
mod vm;

use std::env;
use std::fs;
use std::process;

use devices::uart::Pl011Uart;
use vcpu::{Vcpu, VcpuExitStatus};
use vm::Vm;

const GUEST_RAM_BASE: u64 = 0x4000_0000;
const GUEST_RAM_SIZE: usize = 64 * 1024 * 1024; // 64 MB
const GUEST_ENTRY: u64 = 0x4000_0000;
const GUEST_STACK: u64 = 0x4100_0000; // 16 MB stack offset

const MMIO_UART_BASE: u64 = 0x0900_0000;
const MMIO_UART_SIZE: u64 = 0x1000;

// Embedded default payload built by build.rs
const DEFAULT_PAYLOAD: &[u8] = include_bytes!("../guest/payload.bin");

fn main() {
    println!("\x1b[1;36m==================================================\x1b[0m");
    println!("\x1b[1;36m  macOS Hypervisor.framework Prototype VMM (ARM64)\x1b[0m");
    println!("\x1b[1;36m==================================================\x1b[0m");

    // 1. Determine payload
    let args: Vec<String> = env::args().collect();
    let (payload_name, payload_data): (String, Vec<u8>) = if args.len() > 1 {
        let path = &args[1];
        match fs::read(path) {
            Ok(data) => (format!("external file '{}'", path), data),
            Err(e) => {
                eprintln!("\x1b[1;31m[VMM Error] Failed to read payload file '{}': {}\x1b[0m", path, e);
                process::exit(1);
            }
        }
    } else {
        ("embedded EL1 baremetal test suite".to_string(), DEFAULT_PAYLOAD.to_vec())
    };

    println!("[VMM] Creating virtual machine instance...");
    let mut vm = match Vm::new(GUEST_RAM_BASE, GUEST_RAM_SIZE) {
        Ok(vm) => vm,
        Err(e) => {
            eprintln!("\x1b[1;31m[VMM Error] Failed to initialize VM: {}\x1b[0m", e);
            eprintln!("[VMM Hint] Ensure binary is signed with 'com.apple.security.hypervisor' entitlement.");
            process::exit(1);
        }
    };

    println!(
        "[VMM] Mapped 64MB guest physical RAM at GPA 0x{:08x}..0x{:08x}",
        GUEST_RAM_BASE,
        GUEST_RAM_BASE + GUEST_RAM_SIZE as u64
    );

    // 2. Attach MMIO devices
    vm.mmio_mut()
        .register(MMIO_UART_BASE, MMIO_UART_SIZE, Box::new(Pl011Uart::new()));
    println!(
        "[VMM] Registered PL011 UART console at GPA 0x{:08x}..0x{:08x}",
        MMIO_UART_BASE,
        MMIO_UART_BASE + MMIO_UART_SIZE
    );

    // 3. Load payload into guest RAM
    if let Err(e) = vm.memory_mut().write(GUEST_ENTRY, &payload_data) {
        eprintln!("\x1b[1;31m[VMM Error] Failed to load payload into RAM: {}\x1b[0m", e);
        process::exit(1);
    }
    println!(
        "[VMM] Loaded {} ({} bytes) at entry GPA 0x{:08x}",
        payload_name,
        payload_data.len(),
        GUEST_ENTRY
    );

    // 4. Initialize vCPU
    println!("[VMM] Creating and configuring vCPU #0...");
    let mut vcpu = match Vcpu::new() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("\x1b[1;31m[VMM Error] Failed to create vCPU: {}\x1b[0m", e);
            process::exit(1);
        }
    };

    if let Err(e) = vcpu.setup_boot_state(GUEST_ENTRY, GUEST_STACK) {
        eprintln!("\x1b[1;31m[VMM Error] Failed to configure vCPU boot state: {}\x1b[0m", e);
        process::exit(1);
    }
    println!(
        "[VMM] vCPU #0 ready: PC=0x{:08x}, SP_EL1=0x{:08x}, CPSR=0x3c5 (EL1h)",
        GUEST_ENTRY, GUEST_STACK
    );

    // 5. Enter execution loop
    println!("[VMM] Entering guest execution loop...");
    println!("--------------------------------------------------");

    match vcpu.run(vm.mmio_mut()) {
        Ok(status) => {
            println!("--------------------------------------------------");
            match status {
                VcpuExitStatus::Shutdown => {
                    println!("\x1b[1;32m[VMM] VM exited normally: Guest requested PSCI SYSTEM_OFF.\x1b[0m");
                }
                VcpuExitStatus::Reset => {
                    println!("\x1b[1;33m[VMM] VM exited: Guest requested PSCI SYSTEM_RESET.\x1b[0m");
                }
                VcpuExitStatus::Halted => {
                    println!("\x1b[1;34m[VMM] VM execution stopped: Guest executed WFI (Halted).\x1b[0m");
                }
                VcpuExitStatus::Canceled => {
                    println!("\x1b[1;33m[VMM] VM execution canceled.\x1b[0m");
                }
            }
        }
        Err(e) => {
            println!("--------------------------------------------------");
            eprintln!("\x1b[1;31m[VMM Error] Guest execution fault: {}\x1b[0m", e);
            process::exit(1);
        }
    }

    println!("[VMM] Tearing down vCPU and VM resources...");
    drop(vcpu);
    drop(vm);
    println!("\x1b[1;32m[VMM] Virtual machine terminated cleanly.\x1b[0m");
}
