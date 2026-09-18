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
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;

use devices::rtc::Pl031Rtc;
use devices::uart::Pl011Uart;
use devices::virtio::block::VirtioBlock;
use devices::virtio::net::VirtioNet;
use devices::virtio::rng::VirtioRng;
use vcpu::{Vcpu, VcpuExitStatus, VcpuManager};
use vm::Vm;

const DEFAULT_RAM_BASE: u64 = 0x4000_0000;
const DEFAULT_BAREMETAL_RAM_SIZE: usize = 64 * 1024 * 1024; // 64 MB
const DEFAULT_OS_RAM_SIZE: usize = 1024 * 1024 * 1024;      // 1024 MB (1GB) for FreeBSD/Linux

const MMIO_GICD_BASE: u64 = 0x0800_0000;
const MMIO_GICR_BASE: u64 = 0x080a_0000;
const MMIO_UART_BASE: u64 = 0x0900_0000;
const MMIO_UART_SIZE: u64 = 0x1000;
const UART_IRQ: u32 = 33; // SPI 1 (intid 33)

const MMIO_RTC_BASE: u64 = 0x0901_0000;
const MMIO_RTC_SIZE: u64 = 0x1000;
const RTC_IRQ: u32 = 36; // SPI 4 (intid 36)

const MMIO_VIRTIO_BLK_BASE: u64 = 0x0a00_0000;
const MMIO_VIRTIO_BLK_SIZE: u64 = 0x0200;
const VIRTIO_BLK_IRQ: u32 = 34; // SPI 2 (intid 34)

const MMIO_VIRTIO_NET_BASE: u64 = 0x0a00_0200;
const MMIO_VIRTIO_NET_SIZE: u64 = 0x0200;
const VIRTIO_NET_IRQ: u32 = 35; // SPI 3 (intid 35)

const MMIO_VIRTIO_RNG_BASE: u64 = 0x0a00_0400;
const MMIO_VIRTIO_RNG_SIZE: u64 = 0x0200;
const VIRTIO_RNG_IRQ: u32 = 37; // SPI 5 (intid 37)

// Embedded default payload built by build.rs
const DEFAULT_PAYLOAD: &[u8] = include_bytes!("../guest/payload.bin");

fn main() {
    let args: Vec<String> = env::args().collect();

    // Parse command line arguments
    let mut kernel_path: Option<String> = None;
    let mut disk_path: Option<String> = None;
    let mut root_dev: Option<String> = None;
    let mut custom_bootargs: Option<String> = None;
    let mut ram_mb: Option<usize> = None;
    let mut is_freebsd = false;
    let mut net_mode: Option<String> = None;
    let mut mac_addr: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    let mut cpus: usize = 1;

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
            "--root" | "-r" => {
                if i + 1 < args.len() {
                    i += 1;
                    root_dev = Some(args[i].clone());
                }
            }
            "--bootargs" | "-b" => {
                if i + 1 < args.len() {
                    i += 1;
                    custom_bootargs = Some(args[i].clone());
                }
            }
            "--ram" | "-m" => {
                if i + 1 < args.len() {
                    i += 1;
                    if let Ok(mb) = args[i].parse::<usize>() {
                        ram_mb = Some(mb);
                    } else {
                        eprintln!("\x1b[1;31m[VMM Error] Invalid RAM size: '{}'\x1b[0m", args[i]);
                        process::exit(1);
                    }
                }
            }
            "--cpus" | "-c" => {
                if i + 1 < args.len() {
                    i += 1;
                    if let Ok(c) = args[i].parse::<usize>() {
                        if c == 0 || c > 16 {
                            eprintln!("\x1b[1;31m[VMM Error] CPU count must be between 1 and 16\x1b[0m");
                            process::exit(1);
                        }
                        cpus = c;
                    } else {
                        eprintln!("\x1b[1;31m[VMM Error] Invalid CPU count: '{}'\x1b[0m", args[i]);
                        process::exit(1);
                    }
                }
            }
            "--net" | "-n" => {
                if i + 1 < args.len() {
                    i += 1;
                    net_mode = Some(args[i].clone());
                }
            }
            "--mac" => {
                if i + 1 < args.len() {
                    i += 1;
                    if let Some(mac) = parse_mac(&args[i]) {
                        mac_addr = mac;
                    } else {
                        eprintln!("\x1b[1;31m[VMM Error] Invalid MAC address: '{}'\x1b[0m", args[i]);
                        process::exit(1);
                    }
                }
            }
            "--help" | "-h" => {
                println!("Usage: vmm [OPTIONS] [PAYLOAD/KERNEL]");
                println!();
                println!("Options:");
                println!("  -f, --freebsd [PATH]     Boot FreeBSD kernel (defaults to boot/kernel/kernel)");
                println!("  -k, --kernel <PATH>      Boot specified ELF kernel");
                println!("  -c, --cpus <N>           Number of virtual CPUs (default: 1)");
                println!("  -d, --disk <PATH>        Attach raw disk image as VirtIO block device");
                println!("  -r, --root <DEV>         Root filesystem device (e.g. ufs:/dev/vtbd0p2)");
                println!("  -b, --bootargs <ARGS>    Custom kernel boot arguments");
                println!("  -m, --ram <MB>           Guest physical RAM in MB (default: 1024 for OS)");
                println!("  -n, --net <MODE>         Network mode: vmnet (default), tap:<dev>, mock, none");
                println!("      --mac <MAC>          Network MAC address (default: 52:54:00:12:34:56)");
                println!("  -h, --help               Display this help message");
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

    // Default root device when disk is provided but --root is not specified
    if disk_path.is_some() && root_dev.is_none() {
        root_dev = Some("ufs:/dev/vtbd0p2".to_string());
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
            let ram_size = ram_mb
                .map(|mb| mb * 1024 * 1024)
                .unwrap_or(DEFAULT_OS_RAM_SIZE);

            let bootargs = match custom_bootargs {
                Some(b) => b,
                None => match root_dev {
                    Some(ref root) => format!(
                        "FreeBSD: -v -s -h boot_serial=YES console=comconsole vfs.root.mountfrom={}",
                        root
                    ),
                    None => "FreeBSD: -v -s -h boot_serial=YES console=comconsole".to_string(),
                },
            };

            boot_os_kernel(
                path,
                &file_data,
                disk_path.as_deref(),
                ram_size,
                &bootargs,
                net_mode.as_deref(),
                mac_addr,
                cpus,
            );
            return;
        } else {
            let ram_size = ram_mb
                .map(|mb| mb * 1024 * 1024)
                .unwrap_or(DEFAULT_BAREMETAL_RAM_SIZE);
            boot_flat_binary(path, &file_data, ram_size);
            return;
        }
    }

    // Default: run built-in baremetal test payload
    println!("\x1b[1;36m==================================================\x1b[0m");
    println!("\x1b[1;36m  macOS Hypervisor.framework VMM (ARM64)\x1b[0m");
    println!("\x1b[1;36m==================================================\x1b[0m");
    boot_flat_binary("embedded EL1 baremetal test suite", DEFAULT_PAYLOAD, DEFAULT_BAREMETAL_RAM_SIZE);
}

fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = s.split(|c| c == ':' || c == '-').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        mac[i] = u8::from_str_radix(p, 16).ok()?;
    }
    Some(mac)
}

/// Boots an OS kernel ELF (such as FreeBSD ARM64) with GICv3, multi-vCPU SMP, and Device Tree.
fn boot_os_kernel(
    kernel_path: &str,
    elf_data: &[u8],
    disk_path: Option<&str>,
    ram_size: usize,
    bootargs: &str,
    net_mode: Option<&str>,
    mac_addr: [u8; 6],
    cpus: usize,
) {
    println!("\x1b[1;36m==================================================\x1b[0m");
    println!("\x1b[1;36m  macOS Hypervisor.framework VMM (ARM64)\x1b[0m");
    println!("\x1b[1;36m==================================================\x1b[0m");
    println!("[VMM] Preparing to boot OS kernel from '{}'...", kernel_path);
    println!(
        "[VMM] Allocating {}MB RAM at GPA 0x{:08x}...",
        ram_size / (1024 * 1024),
        DEFAULT_RAM_BASE
    );
    println!("[VMM] Configuring {} virtual CPUs (SMP)...", cpus);
    println!("[VMM] Bootargs: \"{}\"", bootargs);

    let mut vm = match Vm::new(DEFAULT_RAM_BASE, ram_size) {
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

    // Attach ARM PL031 Real-Time Clock
    vm.mmio_mut()
        .register(MMIO_RTC_BASE, MMIO_RTC_SIZE, Box::new(Pl031Rtc::new(RTC_IRQ)));
    println!(
        "[VMM] Registered ARM PL031 RTC at GPA 0x{:08x} (IRQ {})",
        MMIO_RTC_BASE, RTC_IRQ
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

    // Attach VirtIO-MMIO Network Device
    if net_mode != Some("none") {
        let virtio_net = VirtioNet::new(mac_addr, VIRTIO_NET_IRQ, vm.memory(), net_mode.unwrap_or("vmnet"));
        vm.mmio_mut()
            .register(MMIO_VIRTIO_NET_BASE, MMIO_VIRTIO_NET_SIZE, Box::new(virtio_net));
        println!(
            "[VMM] Registered VirtIO-MMIO network device at GPA 0x{:08x} (IRQ {})",
            MMIO_VIRTIO_NET_BASE, VIRTIO_NET_IRQ
        );
    }

    // Attach VirtIO-MMIO Random Number Generator
    let virtio_rng = VirtioRng::new(VIRTIO_RNG_IRQ, vm.memory());
    vm.mmio_mut()
        .register(MMIO_VIRTIO_RNG_BASE, MMIO_VIRTIO_RNG_SIZE, Box::new(virtio_rng));
    println!(
        "[VMM] Registered VirtIO-MMIO RNG device at GPA 0x{:08x} (IRQ {})",
        MMIO_VIRTIO_RNG_BASE, VIRTIO_RNG_IRQ
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
    let dtb_bytes = match fdt::generate_dtb(DEFAULT_RAM_BASE, ram_size as u64, bootargs, cpus) {
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

    // Set up Multi-vCPU manager
    let vcpu_mgr = VcpuManager::new(cpus);
    let mut secondary_threads = Vec::new();
    let mmio_shared = vm.mmio().clone();

    // Spawn secondary worker threads for vCPUs 1..cpus-1
    for slot in vcpu_mgr.slots() {
        let slot_clone = Arc::clone(slot);
        let mgr_clone = Arc::clone(&vcpu_mgr);
        let mmio_clone = mmio_shared.clone();
        let cpu_idx = slot.vcpu_index;

        let handle = thread::Builder::new()
            .name(format!("vmm-vcpu-{}", cpu_idx))
            .spawn(move || {
                // Initialize secondary vCPU on its own OS thread (Apple Hypervisor affinity requirement)
                let mut vcpu = match Vcpu::with_manager(cpu_idx, Some(mgr_clone)) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("\x1b[1;31m[VMM Error] Failed to create secondary vCPU {}: {}\x1b[0m", cpu_idx, e);
                        return;
                    }
                };

                // Park on condvar until PSCI CPU_ON release
                let boot_args = {
                    let mut guard = slot_clone.boot_args.lock().unwrap();
                    while guard.is_none() {
                        if slot_clone.is_online.load(Ordering::Relaxed) {
                            break;
                        }
                        guard = slot_clone.condvar.wait(guard).unwrap();
                    }
                    guard.take()
                };

                if let Some(args) = boot_args {
                    if let Err(e) = vcpu.setup_secondary_boot_state(args.entry_point, args.context_id) {
                        eprintln!("\x1b[1;31m[VMM Error] Failed to setup secondary vCPU {} boot state: {}\x1b[0m", cpu_idx, e);
                        return;
                    }
                    let _ = vcpu.run(&mmio_clone);
                }
            })
            .expect("Failed to spawn secondary vCPU thread");

        secondary_threads.push(handle);
    }

    // Create boot vCPU #0
    let mut vcpu0 = match Vcpu::with_manager(0, Some(Arc::clone(&vcpu_mgr))) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("\x1b[1;31m[VMM Error] Failed to create boot vCPU 0: {}\x1b[0m", e);
            process::exit(1);
        }
    };

    if let Err(e) = vcpu0.setup_os_boot_state(loaded.entry_point, dtb_addr, 0) {
        eprintln!("\x1b[1;31m[VMM Error] Failed to setup OS boot registers: {}\x1b[0m", e);
        process::exit(1);
    }

    println!("[VMM] Entering interactive console (raw terminal). Press Ctrl-] to quit.");
    println!("--------------------------------------------------");

    // Set terminal to raw mode so keystrokes are sent immediately to the guest
    let _term_guard = terminal::TerminalGuard::enter_raw_mode();

    run_loop(&mut vcpu0, vm.mmio());

    // Signal shutdown to secondary vCPUs and join threads
    vcpu_mgr.request_shutdown();
    for handle in secondary_threads {
        let _ = handle.join();
    }
}

/// Boots a raw flat binary payload (like the baremetal test).
fn boot_flat_binary(name: &str, payload_data: &[u8], ram_size: usize) {
    let mut vm = match Vm::new(DEFAULT_RAM_BASE, ram_size) {
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

    run_loop(&mut vcpu, vm.mmio());
}

fn run_loop(vcpu: &mut Vcpu, mmio: &devices::MmioBus) {
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
