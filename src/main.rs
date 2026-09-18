mod devices;
mod elf;
mod fdt;
mod hypervisor;
mod memory;
mod terminal;
mod vcpu;
mod vm;

use std::env;
use std::fs;
use std::process;

use devices::uart::Pl011Uart;
use devices::virtio::block::VirtioBlock;
use vcpu::{Vcpu, VcpuExitStatus};
use vm::Vm;

const DEFAULT_RAM_BASE: u64 = 0x4000_0000;
const DEFAULT_BAREMETAL_RAM_SIZE: usize = 64 * 1024 * 1024; // 64 MB
const OS_RAM_SIZE: usize = 512 * 1024 * 1024;              // 512 MB for FreeBSD/Linux

const MMIO_GICD_BASE: u64 = 0x0800_0000;
const MMIO_GICR_BASE: u64 = 0x080a_0000;
const MMIO_UART_BASE: u64 = 0x0900_0000;
const MMIO_UART_SIZE: u64 = 0x1000;
const UART_IRQ: u32 = 33; // SPI 1 (intid 33)

const MMIO_VIRTIO_BLK_BASE: u64 = 0x0a00_0000;
const MMIO_VIRTIO_BLK_SIZE: u64 = 0x0200;
const VIRTIO_BLK_IRQ: u32 = 34; // SPI 2 (intid 34)

// Embedded default payload built by build.rs
const DEFAULT_PAYLOAD: &[u8] = include_bytes!("../guest/payload.bin");

fn main() {
    let args: Vec<String> = env::args().collect();

    // Parse command line arguments
    let mut kernel_path: Option<String> = None;
    let mut disk_path: Option<String> = None;
    let mut is_freebsd = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--freebsd" | "-f" => {
                is_freebsd = true;
                if i + 1 < args.len() && !args[i + 1].starts_with('-') {
                    i += 1;
                    kernel_path = Some(args[i].clone());
                } else if std::path::Path::new("boot/kernel/kernel").exists() {
                    kernel_path = Some("boot/kernel/kernel".to_string());
                }
            }
            "--kernel" | "-k" => {
                if i + 1 < args.len() {
                    i += 1;
                    kernel_path = Some(args[i].clone());
                }
            }
            "--disk" | "-d" => {
                if i + 1 < args.len() {
                    i += 1;
                    disk_path = Some(args[i].clone());
                }
            }
            "--help" | "-h" => {
                println!("Usage: vmm [OPTIONS] [PAYLOAD/KERNEL]");
                println!();
                println!("Options:");
                println!("  -f, --freebsd [PATH]   Boot FreeBSD kernel (defaults to boot/kernel/kernel)");
                println!("  -k, --kernel <PATH>    Boot specified ELF kernel");
                println!("  -d, --disk <PATH>      Attach raw disk image as VirtIO block device");
                println!("  -h, --help             Display this help message");
                return;
            }
            arg if !arg.starts_with('-') => {
                if kernel_path.is_none() {
                    kernel_path = Some(arg.to_string());
                }
            }
            _ => {
                eprintln!("\x1b[1;33m[VMM Warning] Unknown argument '{}'\x1b[0m", args[i]);
            }
        }
        i += 1;
    }

    if is_freebsd && kernel_path.is_none() && std::path::Path::new("boot/kernel/kernel").exists() {
        kernel_path = Some("boot/kernel/kernel".to_string());
    }

    if let Some(ref path) = kernel_path {
        let file_data = match fs::read(path) {
            Ok(data) => data,
            Err(e) => {
                eprintln!("\x1b[1;31m[VMM Error] Failed to read file '{}': {}\x1b[0m", path, e);
                process::exit(1);
            }
        };

        let is_elf = file_data.len() >= 4 && &file_data[0..4] == b"\x7fELF";

        if is_elf || is_freebsd {
            boot_os_kernel(path, &file_data, disk_path.as_deref());
            return;
        } else {
            boot_flat_binary(path, &file_data);
            return;
        }
    }

    // Default: run built-in baremetal test payload
    println!("\x1b[1;36m==================================================\x1b[0m");
    println!("\x1b[1;36m  macOS Hypervisor.framework VMM (ARM64)\x1b[0m");
    println!("\x1b[1;36m==================================================\x1b[0m");
    boot_flat_binary("embedded EL1 baremetal test suite", DEFAULT_PAYLOAD);
}

/// Boots an OS kernel ELF (such as FreeBSD ARM64) with GICv3 and Device Tree.
fn boot_os_kernel(kernel_path: &str, elf_data: &[u8], disk_path: Option<&str>) {
    println!("\x1b[1;36m==================================================\x1b[0m");
    println!("\x1b[1;36m  macOS Hypervisor.framework VMM (ARM64)\x1b[0m");
    println!("\x1b[1;36m==================================================\x1b[0m");
    println!("[VMM] Preparing to boot OS kernel from '{}'...", kernel_path);
    println!("[VMM] Allocating 512MB RAM at GPA 0x{:08x}...", DEFAULT_RAM_BASE);

    let mut vm = match Vm::new(DEFAULT_RAM_BASE, OS_RAM_SIZE) {
        Ok(vm) => vm,
        Err(e) => {
            eprintln!("\x1b[1;31m[VMM Error] Failed to initialize VM: {}\x1b[0m", e);
            eprintln!("[VMM Hint] Ensure binary is signed with 'com.apple.security.hypervisor' entitlement.");
            process::exit(1);
        }
    };

    // Initialize native in-kernel ARM GICv3
    println!("[VMM] Initializing in-kernel ARM GICv3 device...");
    if let Err(e) = vm.init_gic(MMIO_GICD_BASE, MMIO_GICR_BASE) {
        eprintln!("\x1b[1;31m[VMM Error] Failed to create GICv3 device: {}\x1b[0m", e);
        process::exit(1);
    }
    println!("\x1b[1;32m[VMM] In-kernel GICv3 initialized successfully.\x1b[0m");

    // Attach interactive PL011 UART console
    vm.mmio_mut()
        .register(MMIO_UART_BASE, MMIO_UART_SIZE, Box::new(Pl011Uart::new(UART_IRQ, true)));
    println!(
        "[VMM] Registered interactive PL011 UART console at GPA 0x{:08x} (IRQ {})",
        MMIO_UART_BASE, UART_IRQ
    );

    // Attach VirtIO-MMIO Block Device
    let virtio_blk = match VirtioBlock::new(disk_path, VIRTIO_BLK_IRQ, vm.memory()) {
        Ok(blk) => blk,
        Err(e) => {
            eprintln!("\x1b[1;31m[VMM Error] Failed to initialize VirtIO block device: {}\x1b[0m", e);
            process::exit(1);
        }
    };
    vm.mmio_mut()
        .register(MMIO_VIRTIO_BLK_BASE, MMIO_VIRTIO_BLK_SIZE, Box::new(virtio_blk));
    println!(
        "[VMM] Registered VirtIO-MMIO block device at GPA 0x{:08x} (IRQ {})",
        MMIO_VIRTIO_BLK_BASE, VIRTIO_BLK_IRQ
    );

    // Load kernel ELF: FreeBSD locore requires 2MB alignment from RAM base
    let kernel_phys_load = DEFAULT_RAM_BASE + 0x0020_0000; // 0x40200000
    let loaded = match elf::load_elf64(elf_data, &vm.memory(), kernel_phys_load) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("\x1b[1;31m[VMM Error] Failed to load kernel ELF: {}\x1b[0m", e);
            process::exit(1);
        }
    };

    // Generate and load Device Tree Blob (DTB)
    let dtb_addr = DEFAULT_RAM_BASE + 0x0800_0000; // 0x48000000 (128MB offset)
    let bootargs = "boot_verbose=1 boot_single=1 console=ttyAMA0 earlycon=pl011,0x09000000";
    let dtb_bytes = match fdt::generate_dtb(DEFAULT_RAM_BASE, OS_RAM_SIZE as u64, bootargs) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("\x1b[1;31m[VMM Error] Failed to generate DTB: {}\x1b[0m", e);
            process::exit(1);
        }
    };

    if let Err(e) = vm.memory().write(dtb_addr, &dtb_bytes) {
        eprintln!("\x1b[1;31m[VMM Error] Failed to write DTB to RAM: {}\x1b[0m", e);
        process::exit(1);
    }

    // Create vCPU #0
    let mut vcpu = match Vcpu::new() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("\x1b[1;31m[VMM Error] Failed to create vCPU: {}\x1b[0m", e);
            process::exit(1);
        }
    };

    if let Err(e) = vcpu.setup_os_boot_state(loaded.entry_point, dtb_addr, 0) {
        eprintln!("\x1b[1;31m[VMM Error] Failed to setup OS boot registers: {}\x1b[0m", e);
        process::exit(1);
    }

    println!("[VMM] Entering interactive console (raw terminal). Press Ctrl-] to quit.");
    println!("--------------------------------------------------");

    // Set terminal to raw mode so keystrokes are sent immediately to the guest
    let _term_guard = terminal::TerminalGuard::enter_raw_mode();

    run_loop(&mut vcpu, vm.mmio_mut());
}

/// Boots a raw flat binary payload (like the baremetal test).
fn boot_flat_binary(name: &str, payload_data: &[u8]) {
    let mut vm = match Vm::new(DEFAULT_RAM_BASE, DEFAULT_BAREMETAL_RAM_SIZE) {
        Ok(vm) => vm,
        Err(e) => {
            eprintln!("\x1b[1;31m[VMM Error] Failed to initialize VM: {}\x1b[0m", e);
            process::exit(1);
        }
    };

    vm.mmio_mut()
        .register(MMIO_UART_BASE, MMIO_UART_SIZE, Box::new(Pl011Uart::new(UART_IRQ, false)));

    if let Err(e) = vm.memory().write(DEFAULT_RAM_BASE, payload_data) {
        eprintln!("\x1b[1;31m[VMM Error] Failed to load payload: {}\x1b[0m", e);
        process::exit(1);
    }
    println!(
        "[VMM] Loaded {} ({} bytes) at GPA 0x{:08x}",
        name,
        payload_data.len(),
        DEFAULT_RAM_BASE
    );

    let mut vcpu = match Vcpu::new() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("\x1b[1;31m[VMM Error] Failed to create vCPU: {}\x1b[0m", e);
            process::exit(1);
        }
    };

    let stack_top = DEFAULT_RAM_BASE + 0x0100_0000;
    if let Err(e) = vcpu.setup_boot_state(DEFAULT_RAM_BASE, stack_top) {
        eprintln!("\x1b[1;31m[VMM Error] Failed to configure vCPU: {}\x1b[0m", e);
        process::exit(1);
    }

    println!("[VMM] Entering guest execution loop...");
    println!("--------------------------------------------------");

    run_loop(&mut vcpu, vm.mmio_mut());
}

fn run_loop(vcpu: &mut Vcpu, mmio: &mut devices::MmioBus) {
    match vcpu.run(mmio) {
        Ok(status) => {
            println!("--------------------------------------------------");
            match status {
                VcpuExitStatus::Shutdown => {
                    println!("\x1b[1;32m[VMM] VM exited: Guest requested PSCI SYSTEM_OFF.\x1b[0m");
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

    println!("\x1b[1;32m[VMM] Virtual machine terminated cleanly.\x1b[0m");
}
