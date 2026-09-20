mod devices;
mod elf;
mod fdt;
mod forward;
mod gpt;
mod hypervisor;
mod memory;
mod preload;
mod terminal;
mod vcpu;
mod vm;

use std::env;
use std::fs;
use std::process;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;

use devices::fw_cfg::FwCfg;
use devices::rtc::Pl031Rtc;
use devices::uart::Pl011Uart;
use devices::virtio::block::VirtioBlock;
use devices::virtio::net::VirtioNet;
use devices::virtio::rng::VirtioRng;
use devices::virtio::VirtioConsole;
use vcpu::{CpuModel, Vcpu, VcpuExitStatus, VcpuManager};
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

const MMIO_FW_CFG_BASE: u64 = 0x0902_0000;
const MMIO_FW_CFG_SIZE: u64 = 0x18;
const DEFAULT_UEFI_FW: &str = "firmware/QEMU_EFI.fd";

const MMIO_VIRTIO_BLK_BASE: u64 = 0x0a00_0000;
const MMIO_VIRTIO_BLK_SIZE: u64 = 0x0200;
const VIRTIO_BLK_IRQ: u32 = 34; // SPI 2 (intid 34)

const MMIO_VIRTIO_NET_BASE: u64 = 0x0a00_0200;
const MMIO_VIRTIO_NET_SIZE: u64 = 0x0200;
const VIRTIO_NET_IRQ: u32 = 35; // SPI 3 (intid 35)

const MMIO_VIRTIO_RNG_BASE: u64 = 0x0a00_0400;
const MMIO_VIRTIO_RNG_SIZE: u64 = 0x0200;
const VIRTIO_RNG_IRQ: u32 = 37; // SPI 5 (intid 37)

const MMIO_VIRTIO_CONSOLE_BASE: u64 = 0x0a00_0600;
const MMIO_VIRTIO_CONSOLE_SIZE: u64 = 0x0200;
const VIRTIO_CONSOLE_IRQ: u32 = 38; // SPI 6 (intid 38)

#[derive(Clone)]
pub struct DiskConfig {
    pub path: String,
    pub cow: bool,
    pub read_only: bool,
}

fn detect_root_partition(disk_path: &str) -> Option<String> {
    gpt::detect_freebsd_root_from_disk(disk_path).map(|r| gpt::freebsd_root_to_mountfrom(&r))
}

fn align_up(addr: u64, align: u64) -> u64 {
    (addr + align - 1) & !(align - 1)
}

fn module_name_from_path(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("module")
        .to_string()
}

// Embedded default payload built by build.rs
const DEFAULT_PAYLOAD: &[u8] = include_bytes!("../guest/payload.bin");

fn main() {
    let args: Vec<String> = env::args().collect();

    // Parse command line arguments
    let mut kernel_path: Option<String> = None;
    let mut disk_configs: Vec<DiskConfig> = Vec::new();
    let mut global_cow = false;
    let mut root_dev: Option<String> = None;
    let mut custom_bootargs: Option<String> = None;
    let mut ram_mb: Option<usize> = None;
    let mut is_freebsd = false;
    let mut is_uefi = false;
    let mut firmware_path: Option<String> = None;
    let mut is_multiuser = false;
    let mut ssh_port: Option<u16> = None;
    let mut net_mode: Option<String> = None;
    let mut mac_addr: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    let mut cpus: usize = 1;
    let mut cpu_model = CpuModel::detect_host();
    let mut serial_log: Option<String> = None;
    let mut module_paths: Vec<String> = Vec::new();

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--uefi" | "--firmware" => {
                is_uefi = true;
                if i + 1 < args.len() && !args[i + 1].starts_with('-') {
                    i += 1;
                    firmware_path = Some(args[i].clone());
                }
            }
            "--freebsd" | "-f" => {
                is_freebsd = true;
                if i + 1 < args.len() && !args[i + 1].starts_with('-') {
                    i += 1;
                    kernel_path = Some(args[i].clone());
                } else if std::path::Path::new("boot/kernel/kernel").exists() {
                    kernel_path = Some("boot/kernel/kernel".to_string());
                }
            }
            "--multiuser" | "--multi-user" | "-u" => {
                is_multiuser = true;
            }
            "--ssh-port" => {
                if i + 1 < args.len() {
                    i += 1;
                    if let Ok(port) = args[i].parse::<u16>() {
                        ssh_port = Some(port);
                    } else {
                        eprintln!("\x1b[1;31m[VMM Error] Invalid SSH port: '{}'\x1b[0m", args[i]);
                        process::exit(1);
                    }
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
                    let arg = &args[i];
                    let parts: Vec<&str> = arg.split(',').collect();
                    let path = parts[0].to_string();
                    let mut cow = false;
                    let mut read_only = false;
                    for opt in &parts[1..] {
                        match *opt {
                            "cow" => cow = true,
                            "ro" | "readonly" => read_only = true,
                            _ => {
                                eprintln!("\x1b[1;33m[VMM Warning] Unknown disk option '{}'\x1b[0m", opt);
                            }
                        }
                    }
                    disk_configs.push(DiskConfig { path, cow, read_only });
                }
            }
            "--cdrom" | "--iso" => {
                if i + 1 < args.len() {
                    i += 1;
                    disk_configs.push(DiskConfig {
                        path: args[i].clone(),
                        cow: false,
                        read_only: true,
                    });
                }
            }
            "--cow" => {
                global_cow = true;
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
            "--cpu-model" | "--cpu-type" => {
                if i + 1 < args.len() {
                    i += 1;
                    match CpuModel::parse(&args[i]) {
                        Ok(model) => cpu_model = model,
                        Err(e) => {
                            eprintln!("\x1b[1;31m[VMM Error] {}\x1b[0m", e);
                            process::exit(1);
                        }
                    }
                }
            }
            "--net" | "-n" => {
                if i + 1 < args.len() {
                    i += 1;
                    net_mode = Some(args[i].clone());
                }
            }
            "--serial-log" | "--console-log" => {
                if i + 1 < args.len() {
                    i += 1;
                    serial_log = Some(args[i].clone());
                }
            }
            "--module" => {
                if i + 1 < args.len() {
                    i += 1;
                    module_paths.push(args[i].clone());
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
                println!("  -f, --freebsd [PATH]     Boot FreeBSD kernel ELF (defaults to boot/kernel/kernel)");
                println!("      --uefi [PATH]        Boot EDK2/UEFI firmware (defaults to firmware/QEMU_EFI.fd); loader.efi handles ZFS");
                println!("  -u, --multiuser          Boot FreeBSD into multi-user mode (omits single-user -s)");
                println!("      --ssh-port <PORT>    Forward host TCP port to guest SSH (e.g. 2222 -> 192.168.18.2:22)");
                println!("  -k, --kernel <PATH>      Boot specified ELF kernel");
                println!("  -c, --cpus <N>           Number of virtual CPUs (default: 1)");
                println!("      --cpu-model <MODEL>  CPU model/MIDR spoofing: auto/host (default), m1, m1-pro, m1-max, m2, m2-pro, m2-max, neoverse-n2, cortex-a78, or 0x<hex>");
                println!("      --serial-log <PATH>  Mirror guest serial console output to specified log file");
                println!("  -d, --disk <PATH>[,cow][,ro] Attach disk image (can be specified multiple times)");
                println!("      --cdrom, --iso <PATH>    Attach read-only ISO image as VirtIO block device");
                println!("      --cow                Enable Copy-On-Write overlay for all attached disks");
                println!("  -r, --root <DEV>         Root filesystem (e.g. ufs:/dev/vtbd0p2 or zfs:zroot/ROOT/default)");
                println!("      --module <PATH>      Preload a FreeBSD .ko (repeatable; zfs.ko is auto-loaded for ZFS root)");
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

    if global_cow {
        for d in &mut disk_configs {
            d.cow = true;
        }
    }

    if is_uefi && firmware_path.is_none() {
        firmware_path = Some(DEFAULT_UEFI_FW.to_string());
    }

    if is_freebsd && kernel_path.is_none() && std::path::Path::new("boot/kernel/kernel").exists() {
        kernel_path = Some("boot/kernel/kernel".to_string());
    }

    // Default root: GPT UFS or zfs:zroot/ROOT/default. Never guess ufs p2 on a ZFS disk
    // (bsdinstall Auto (ZFS) puts swap on p2 — that superblock error is swap, not UFS).
    if !disk_configs.is_empty() && root_dev.is_none() {
        let disk0 = &disk_configs[0].path;
        if let Some(layout) = gpt::describe_freebsd_gpt(disk0) {
            println!("[VMM] GPT on '{}': {}", disk0, layout);
        }
        if let Some(detected) = detect_root_partition(disk0) {
            println!("[VMM] Auto-detected root partition: {}", detected);
            root_dev = Some(detected);
        } else {
            eprintln!(
                "\x1b[1;33m[VMM Warning] No FreeBSD UFS/ZFS GPT partition on '{}'; not assuming ufs:/dev/vtbd0p2 (that is often swap).\x1b[0m",
                disk0
            );
        }
    }

    if !is_uefi {
        if let Some(ref root) = root_dev {
            if root.starts_with("zfs:") {
                eprintln!("\x1b[1;33m[VMM] ZFS root selected. Direct ELF+zfs.ko (Strategy 1) often hangs after MODINFO.");
                eprintln!("[VMM] Recommended Strategy 2: make fetch-uefi && make freebsd-zfs-uefi DISK=<img>\x1b[0m");
            }
        }
    }

    let needs_zfs = root_dev
        .as_deref()
        .is_some_and(|r| r.starts_with("zfs:"))
        || module_paths.iter().any(|p| p.ends_with("zfs.ko") || p.ends_with("/zfs"));
    if needs_zfs
        && !is_uefi
        && !module_paths.iter().any(|p| {
            std::path::Path::new(p)
                .file_stem()
                .and_then(|s| s.to_str())
                == Some("zfs")
        })
    {
        let default_zfs = "boot/kernel/zfs.ko";
        if std::path::Path::new(default_zfs).exists() {
            module_paths.push(default_zfs.to_string());
        } else {
            eprintln!(
                "\x1b[1;31m[VMM Error] Root-on-ZFS requires preloaded zfs.ko, but '{default_zfs}' was not found.\x1b[0m"
            );
            eprintln!(
                "\x1b[1;33m[VMM Hint] Run 'make fetch-freebsd' to extract boot/kernel/kernel and boot/kernel/zfs.ko from kernel.txz.\x1b[0m"
            );
            eprintln!(
                "\x1b[1;33m[VMM Hint] Or pass --module /path/to/zfs.ko (and --root zfs:pool/ROOT/default if the dataset is not zroot/ROOT/default).\x1b[0m"
            );
            process::exit(1);
        }
    }

    if is_uefi {
        let path = firmware_path.unwrap_or_else(|| DEFAULT_UEFI_FW.to_string());
        let fw = match fs::read(&path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("\x1b[1;31m[VMM Error] Failed to read UEFI firmware '{path}': {e}\x1b[0m");
                eprintln!("\x1b[1;33m[VMM Hint] Run 'make fetch-uefi' to download firmware/QEMU_EFI.fd.\x1b[0m");
                process::exit(1);
            }
        };
        let ram_size = ram_mb.map(|mb| mb * 1024 * 1024).unwrap_or(DEFAULT_OS_RAM_SIZE);
        let bootargs = custom_bootargs.unwrap_or_else(|| "console=ttyAMA0".to_string());
        if let Some(port) = ssh_port {
            forward::start_port_forward(port, "192.168.18.2:22".to_string());
        }
        boot_os_kernel(
            &path,
            &[],
            &disk_configs,
            ram_size,
            &bootargs,
            net_mode.as_deref(),
            mac_addr,
            cpus,
            cpu_model,
            serial_log.as_deref(),
            &[],
            is_multiuser,
            None,
            Some(&fw),
        );
        return;
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
                None => {
                    let s_flag = if is_multiuser { "" } else { " -s" };
                    match root_dev {
                        Some(ref root) => format!(
                            "FreeBSD: -v{} -h boot_serial=YES console=comconsole vfs.root.mountfrom={}",
                            s_flag, root
                        ),
                        None => format!("FreeBSD: -v{} -h boot_serial=YES console=comconsole", s_flag),
                    }
                }
            };

            if let Some(port) = ssh_port {
                forward::start_port_forward(port, "192.168.18.2:22".to_string());
            }

            boot_os_kernel(
                path,
                &file_data,
                &disk_configs,
                ram_size,
                &bootargs,
                net_mode.as_deref(),
                mac_addr,
                cpus,
                cpu_model,
                serial_log.as_deref(),
                &module_paths,
                is_multiuser,
                root_dev.as_deref(),
                None,
            );
            return;
        } else {
            let ram_size = ram_mb
                .map(|mb| mb * 1024 * 1024)
                .unwrap_or(DEFAULT_BAREMETAL_RAM_SIZE);
            boot_flat_binary(path, &file_data, ram_size, cpu_model);
            return;
        }
    }

    // Default: run built-in baremetal test payload
    println!("\x1b[1;36m==================================================\x1b[0m");
    println!("\x1b[1;36m  macOS Hypervisor.framework VMM (ARM64)\x1b[0m");
    println!("\x1b[1;36m==================================================\x1b[0m");
    boot_flat_binary("embedded EL1 baremetal test suite", DEFAULT_PAYLOAD, DEFAULT_BAREMETAL_RAM_SIZE, cpu_model);
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
    disk_configs: &[DiskConfig],
    ram_size: usize,
    bootargs: &str,
    net_mode: Option<&str>,
    mac_addr: [u8; 6],
    cpus: usize,
    cpu_model: CpuModel,
    serial_log: Option<&str>,
    module_paths: &[String],
    is_multiuser: bool,
    root_dev: Option<&str>,
    uefi_firmware: Option<&[u8]>,
) {
    println!("\x1b[1;36m==================================================\x1b[0m");
    println!("\x1b[1;36m  macOS Hypervisor.framework VMM (ARM64)\x1b[0m");
    println!("\x1b[1;36m==================================================\x1b[0m");
    if uefi_firmware.is_some() {
        println!("[VMM] Preparing UEFI/EDK2 boot from '{}'...", kernel_path);
    } else {
        println!("[VMM] Preparing to boot OS kernel from '{}'...", kernel_path);
    }
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
        .register(MMIO_UART_BASE, MMIO_UART_SIZE, Box::new(Pl011Uart::with_log(UART_IRQ, true, serial_log)));
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

    let mut virtio_devices: Vec<fdt::VirtioMmioDevice> = Vec::new();

    // Attach VirtIO-MMIO Block Devices
    for (idx, disk) in disk_configs.iter().enumerate() {
        let (base, irq) = if idx == 0 {
            (MMIO_VIRTIO_BLK_BASE, VIRTIO_BLK_IRQ)
        } else {
            (0x0a00_0800 + ((idx - 1) as u64) * 0x0200, 39 + ((idx - 1) as u32))
        };
        let serial_id = format!("vmm-disk-{}", idx);
        let blk_res = if disk.cow {
            let overlay_path = format!("{}.cow", disk.path);
            VirtioBlock::from_cow(&disk.path, &overlay_path, irq, &serial_id, vm.memory())
        } else {
            VirtioBlock::from_raw(&disk.path, disk.read_only, irq, &serial_id, vm.memory())
        };

        match blk_res {
            Ok(blk) => {
                vm.mmio_mut().register(base, MMIO_VIRTIO_BLK_SIZE, Box::new(blk));
                virtio_devices.push(fdt::VirtioMmioDevice {
                    base,
                    size: MMIO_VIRTIO_BLK_SIZE,
                    irq,
                });
                println!(
                    "[VMM] Registered VirtIO-MMIO block device '{}' (disk {}) at GPA 0x{:08x} (IRQ {})",
                    disk.path, idx, base, irq
                );
            }
            Err(e) => {
                eprintln!("\x1b[1;31m[VMM Error] Failed to initialize VirtIO block device '{}': {}\x1b[0m", disk.path, e);
                process::exit(1);
            }
        }
    }

    // Attach VirtIO-MMIO Network Device
    if net_mode != Some("none") {
        let virtio_net = VirtioNet::new(mac_addr, VIRTIO_NET_IRQ, vm.memory(), net_mode.unwrap_or("vmnet"));
        vm.mmio_mut()
            .register(MMIO_VIRTIO_NET_BASE, MMIO_VIRTIO_NET_SIZE, Box::new(virtio_net));
        virtio_devices.push(fdt::VirtioMmioDevice {
            base: MMIO_VIRTIO_NET_BASE,
            size: MMIO_VIRTIO_NET_SIZE,
            irq: VIRTIO_NET_IRQ,
        });
        println!(
            "[VMM] Registered VirtIO-MMIO network device at GPA 0x{:08x} (IRQ {})",
            MMIO_VIRTIO_NET_BASE, VIRTIO_NET_IRQ
        );
    }

    // Attach VirtIO-MMIO Random Number Generator
    let virtio_rng = VirtioRng::new(VIRTIO_RNG_IRQ, vm.memory());
    vm.mmio_mut()
        .register(MMIO_VIRTIO_RNG_BASE, MMIO_VIRTIO_RNG_SIZE, Box::new(virtio_rng));
    virtio_devices.push(fdt::VirtioMmioDevice {
        base: MMIO_VIRTIO_RNG_BASE,
        size: MMIO_VIRTIO_RNG_SIZE,
        irq: VIRTIO_RNG_IRQ,
    });
    println!(
        "[VMM] Registered VirtIO-MMIO RNG device at GPA 0x{:08x} (IRQ {})",
        MMIO_VIRTIO_RNG_BASE, VIRTIO_RNG_IRQ
    );

    // Attach VirtIO-MMIO Console Device (Dynamic Window Geometry & Resize Forwarding)
    let term_size = terminal::get_terminal_size();
    let virtio_console = Arc::new(Mutex::new(VirtioConsole::new(
        VIRTIO_CONSOLE_IRQ,
        vm.memory(),
        term_size.ws_col,
        term_size.ws_row,
    )));
    vm.mmio_mut()
        .register_shared(MMIO_VIRTIO_CONSOLE_BASE, MMIO_VIRTIO_CONSOLE_SIZE, virtio_console.clone());
    virtio_devices.push(fdt::VirtioMmioDevice {
        base: MMIO_VIRTIO_CONSOLE_BASE,
        size: MMIO_VIRTIO_CONSOLE_SIZE,
        irq: VIRTIO_CONSOLE_IRQ,
    });
    println!(
        "[VMM] Registered VirtIO-MMIO console device at GPA 0x{:08x} (IRQ {}, {}x{})",
        MMIO_VIRTIO_CONSOLE_BASE, VIRTIO_CONSOLE_IRQ, term_size.ws_col, term_size.ws_row
    );

    // Spawn SIGWINCH resize watcher thread to forward host terminal size changes
    terminal::install_sigwinch_handler();
    let console_watcher = virtio_console.clone();
    thread::Builder::new()
        .name("vmm-sigwinch".into())
        .spawn(move || {
            loop {
                thread::sleep(std::time::Duration::from_millis(100));
                if let Some(new_ws) = terminal::check_and_clear_resize() {
                    let mut con = console_watcher.lock().unwrap();
                    let (old_cols, old_rows) = con.get_size();
                    if old_cols != new_ws.ws_col || old_rows != new_ws.ws_row {
                        con.update_size(new_ws.ws_col, new_ws.ws_row);
                        eprintln!(
                            "\r\n[VMM] Terminal window resized: {}x{} -> {}x{} (forwarded SIGWINCH to guest)\r\n",
                            old_cols, old_rows, new_ws.ws_col, new_ws.ws_row
                        );
                    }
                }
            }
        })
        .expect("Failed to spawn sigwinch thread");

    let dtb_bytes = match fdt::generate_dtb(DEFAULT_RAM_BASE, ram_size as u64, bootargs, cpus, &virtio_devices) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("\x1b[1;31m[VMM Error] Failed to generate DTB: {}\x1b[0m", e);
            process::exit(1);
        }
    };

    // -------------------------------------------------------------------
    // Boot path dispatch: UEFI firmware vs direct ELF kernel
    // -------------------------------------------------------------------
    let (entry_point, x0_addr) = if let Some(fw) = uefi_firmware {
        // ---- UEFI / EDK2 boot ----
        // Map firmware at GPA 0, serve DTB via fw_cfg, also place DTB in RAM
        // so EDK2 can find it either way.  PC starts at 0, X0 = 0 (DTB via fw_cfg etc/fdt).
        // ArmVirtQemu PCD PcdDeviceTreeInitialBaseAddress is 0x40000000 (RAM base).
        // EDK2 ASSERTs fdt_check_header() on that address before fw_cfg is used.
        let dtb_addr = DEFAULT_RAM_BASE;
        if let Err(e) = vm.memory().write(dtb_addr, &dtb_bytes) {
            eprintln!("[1;31m[VMM Error] Failed to write DTB to RAM: {e}[0m");
            process::exit(1);
        }
        let fw_cfg = FwCfg::new(vm.memory(), dtb_bytes);
        vm.mmio_mut()
            .register(MMIO_FW_CFG_BASE, MMIO_FW_CFG_SIZE, Box::new(fw_cfg));
        println!(
            "[VMM] Registered QEMU fw_cfg at GPA 0x{MMIO_FW_CFG_BASE:08x}"
        );
        if let Err(e) = vm.map_firmware(0, fw) {
            eprintln!("\x1b[1;31m[VMM Error] {e}\x1b[0m");
            process::exit(1);
        }
        println!(
            "[VMM] Mapped EDK2 firmware ({:.2} MB) at GPA 0x00000000; PC=0, X0=0 (DTB at RAM base 0x{dtb_addr:08x} for PCD + fw_cfg etc/fdt)",
            fw.len() as f64 / (1024.0 * 1024.0)
        );
        // QEMU virt flash1: EFI NVRAM at 64MiB — CFI MMIO (not raw RAM).
        // NorFlashDxe issues CFI commands; a plain RAM map hangs Variable services.
        {
            const EFI_VARS_GPA: u64 = 0x0400_0000;
            let vars_path = std::path::Path::new("firmware/QEMU_VARS.fd");
            let vars = match std::fs::read(vars_path) {
                Ok(v) => v,
                Err(e) => {
                    eprintln!("\x1b[1;33m[VMM Warning] Could not read firmware/QEMU_VARS.fd ({e}); using erased 64MiB\x1b[0m");
                    vec![0xffu8; 64 * 1024 * 1024]
                }
            };
            let vars_len = vars.len() as u64;
            let pflash = devices::pflash::CfiPflash::new(vars);
            vm.mmio_mut().register(EFI_VARS_GPA, vars_len, Box::new(pflash));
            println!(
                "[VMM] EFI vars CFI pflash ({:.2} MB) at GPA 0x{EFI_VARS_GPA:08x} (MMIO)",
                vars_len as f64 / (1024.0 * 1024.0)
            );
        }
        // QEMU starts EDK2 with X0=0; DTB is discovered via fw_cfg `etc/fdt`
        // (and we still plant a copy in RAM). Passing X0=DTB can confuse SEC/PrePi.
        (0u64, 0u64)
    } else {
        // ---- Direct ELF kernel boot (FreeBSD / Linux) ----
        // Load kernel ELF: FreeBSD locore requires 2MB alignment from RAM base
        let kernel_phys_load = DEFAULT_RAM_BASE + 0x0020_0000; // 0x40200000
        let loaded = match elf::load_elf64(elf_data, &vm.memory(), kernel_phys_load) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("\x1b[1;31m[VMM Error] Failed to load kernel ELF: {}\x1b[0m", e);
                process::exit(1);
            }
        };

        // UFS: X0 = DTB GPA at 128MB (proven FDT locore path).
        //
        // ZFS: locore treats X0 < KERNBASE as a *physical* FDT/metadata pointer and
        // maps TTBR1 as only (kernel_end - KERNBASE + 3*L2) 2MiB blocks, rounded
        // down. 2MiB-aligning zfs.ko put DTB at +22MiB — the first byte *not*
        // mapped (11 blocks × 2MiB). Silent abort, no UART.
        // Pack DTB + .ko + MODINFO immediately after the kernel (16KiB) so the
        // offset stays inside that window (~20MiB). X0 = MODINFO KVA so locore's
        // cmp x0, KERNBASE takes the module path; DTBP/ADDR/KERNEND stay KVAs.
        let x0 = if module_paths.is_empty() {
            let dtb_addr = DEFAULT_RAM_BASE + 0x0800_0000;
            if let Err(e) = vm.memory().write(dtb_addr, &dtb_bytes) {
                eprintln!("\x1b[1;31m[VMM Error] Failed to write DTB to RAM: {}\x1b[0m", e);
                process::exit(1);
            }
            println!(
                "[VMM] DTB at GPA 0x{dtb_addr:08x} ({} bytes); X0=DTB",
                dtb_bytes.len()
            );
            dtb_addr
        } else {
            let kernbase = loaded.base_vaddr;
            let to_kva = |gpa: u64| kernbase + (gpa - kernel_phys_load);
            let page = 0x4000u64;
            let mut next = align_up(kernel_phys_load + loaded.total_memory_size, page);

            let dtb_gpa = next;
            if let Err(e) = vm.memory().write(dtb_gpa, &dtb_bytes) {
                eprintln!("\x1b[1;31m[VMM Error] Failed to write DTB to RAM: {}\x1b[0m", e);
                process::exit(1);
            }
            next = align_up(dtb_gpa + dtb_bytes.len() as u64, page);

            let mut preload_files = vec![preload::PreloadedFile {
                name: "kernel".to_string(),
                kind: preload::ModuleKind::Kernel,
                gpa: kernel_phys_load,
                size: loaded.total_memory_size,
            }];
            for path in module_paths {
                let data = match fs::read(path) {
                    Ok(d) => d,
                    Err(e) => {
                        eprintln!("\x1b[1;31m[VMM Error] Failed to read module '{path}': {e}\x1b[0m");
                        process::exit(1);
                    }
                };
                let name = module_name_from_path(path);
                if next + data.len() as u64 > DEFAULT_RAM_BASE + ram_size as u64 {
                    eprintln!(
                        "\x1b[1;31m[VMM Error] Module '{path}' ({:.2} MB) does not fit in guest RAM\x1b[0m",
                        data.len() as f64 / (1024.0 * 1024.0)
                    );
                    process::exit(1);
                }
                if let Err(e) = vm.memory().write(next, &data) {
                    eprintln!("\x1b[1;31m[VMM Error] Failed to write module '{path}' to RAM: {e}\x1b[0m");
                    process::exit(1);
                }
                println!(
                    "[VMM] Preloaded module '{name}' ({:.2} MB) at GPA 0x{next:08x} (KVA 0x{:016x}) from '{path}'",
                    data.len() as f64 / (1024.0 * 1024.0),
                    to_kva(next)
                );
                preload_files.push(preload::PreloadedFile {
                    name,
                    kind: preload::ModuleKind::Module,
                    gpa: next,
                    size: data.len() as u64,
                });
                next = align_up(next + data.len() as u64, page);
            }

            let metadata_gpa = align_up(next, 8);
            let mut files_kva = preload_files.clone();
            for f in &mut files_kva {
                f.gpa = to_kva(f.gpa);
            }
            let mut kenv_vars = vec!["boot_serial=YES", "console=comconsole"];
            let mountfrom;
            if let Some(root) = root_dev {
                mountfrom = format!("vfs.root.mountfrom={root}");
                kenv_vars.insert(0, mountfrom.as_str());
            }
            let kenv = preload::encode_kenv(&kenv_vars);
            let mut howto = preload::RB_VERBOSE | preload::RB_SERIAL;
            if !is_multiuser {
                howto |= preload::RB_SINGLE;
            }
            let blob = preload::build_preload(
                &files_kva,
                to_kva(dtb_gpa),
                &kenv,
                howto,
                to_kva(metadata_gpa),
            );
            if metadata_gpa + blob.len() as u64 > DEFAULT_RAM_BASE + ram_size as u64 {
                eprintln!("\x1b[1;31m[VMM Error] Preload metadata does not fit in guest RAM\x1b[0m");
                process::exit(1);
            }
            if let Err(e) = vm.memory().write(metadata_gpa, &blob) {
                eprintln!("\x1b[1;31m[VMM Error] Failed to write preload metadata: {e}\x1b[0m");
                process::exit(1);
            }
            let packed = metadata_gpa + blob.len() as u64 - kernel_phys_load;
            println!(
                "[VMM] DTB at GPA 0x{dtb_gpa:08x} (KVA 0x{:016x}, {} bytes)",
                to_kva(dtb_gpa),
                dtb_bytes.len()
            );
            println!(
                "[VMM] FreeBSD MODINFO at GPA 0x{metadata_gpa:08x} (KVA 0x{:016x}, {} bytes, {} files); packed +{:.2} MB; X0=KVA; KERNEND KVA 0x{:016x}",
                to_kva(metadata_gpa),
                blob.len(),
                files_kva.len(),
                packed as f64 / (1024.0 * 1024.0),
                to_kva(metadata_gpa) + blob.len() as u64
            );
            to_kva(metadata_gpa)
        };

        (loaded.entry_point, x0)
    };

    println!(
        "[VMM] CPU Model: {} (MIDR: 0x{:08x})",
        cpu_model.name, cpu_model.midr
    );

    // Set up Multi-vCPU manager
    let vcpu_mgr = VcpuManager::new(cpus, cpu_model);
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
                    if let Err(e) = vcpu.setup_secondary_boot_state(args.entry_point, args.context_id, cpu_model.midr) {
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

    if let Err(e) = vcpu0.setup_os_boot_state(entry_point, x0_addr, 0, cpu_model.midr) {
        eprintln!("\x1b[1;31m[VMM Error] Failed to setup OS boot registers: {}\x1b[0m", e);
        process::exit(1);
    }

    // Temporary EL1 stack in guest RAM for UEFI SEC (EDK2 replaces it quickly).
    if entry_point == 0 {
        let early_sp = DEFAULT_RAM_BASE + 0x0008_0000;
        if let Err(e) = vcpu0.set_sys_reg(crate::hypervisor::HV_SYS_REG_SP_EL1, early_sp) {
            eprintln!("[1;33m[VMM Warning] UEFI early SP_EL1 failed: {e}[0m");
        } else {
            println!("[VMM] UEFI early SP_EL1 at GPA 0x{early_sp:08x}");
        }
    }


    println!("[VMM] Entering interactive console (raw terminal). Press Ctrl-] to quit.");
    println!("--------------------------------------------------");

    // Set terminal to raw mode so keystrokes are sent immediately to the guest
    let term_guard = terminal::TerminalGuard::enter_raw_mode();

    run_loop(&mut vcpu0, vm.mmio(), Some(term_guard));

    // Signal shutdown to secondary vCPUs and join threads
    vcpu_mgr.request_shutdown();
    for handle in secondary_threads {
        let _ = handle.join();
    }
}

/// Boots a raw flat binary payload (like the baremetal test).
fn boot_flat_binary(name: &str, payload_data: &[u8], ram_size: usize, cpu_model: CpuModel) {
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
    if let Err(e) = vcpu.setup_boot_state(DEFAULT_RAM_BASE, stack_top, cpu_model.midr) {
        eprintln!("\x1b[1;31m[VMM Error] Failed to configure vCPU: {}\x1b[0m", e);
        process::exit(1);
    }

    println!("[VMM] Entering guest execution loop...");
    println!("--------------------------------------------------");

    run_loop(&mut vcpu, vm.mmio(), None);
}

fn run_loop(vcpu: &mut Vcpu, mmio: &devices::MmioBus, term_guard: Option<terminal::TerminalGuard>) {
    let result = vcpu.run(mmio);

    // Drop terminal guard immediately to restore cooked mode before printing exit messages.
    // This ensures \r\n expansion is active so exit lines align cleanly to the left margin.
    drop(term_guard);

    use std::io::Write;
    print!("\r\n");
    let _ = std::io::stdout().flush();

    match result {
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
