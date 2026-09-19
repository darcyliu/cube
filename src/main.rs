mod devices;
mod elf;
mod fdt;
mod forward;
mod hypervisor;
mod memory;
mod terminal;
mod vcpu;
mod vm;

use std::env;
use std::fs;
use std::process;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread;

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

/// Scans the primary disk's GPT partition table to locate a FreeBSD UFS root partition.
/// Warns if a Root-on-ZFS partition is found, as direct ELF booting requires UFS.
fn detect_root_partition(disk_path: &str) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = fs::File::open(disk_path).ok()?;
    let mut sector = [0u8; 512];
    file.seek(SeekFrom::Start(512)).ok()?;
    file.read_exact(&mut sector).ok()?;
    if &sector[0..8] != b"EFI PART" {
        return None;
    }
    let part_lba = u64::from_le_bytes(sector[72..80].try_into().ok()?);
    let num_entries = u32::from_le_bytes(sector[80..84].try_into().ok()?);
    let entry_size = u32::from_le_bytes(sector[84..88].try_into().ok()?) as usize;
    if entry_size < 128 || num_entries == 0 {
        return None;
    }
    file.seek(SeekFrom::Start(part_lba * 512)).ok()?;
    let read_entries = (num_entries as usize).min(128);
    let mut entries_buf = vec![0u8; read_entries * entry_size];
    file.read_exact(&mut entries_buf).ok()?;

    // FreeBSD UFS GUID: 516e7cb6-6ecf-11d6-8ff8-00022d09712b (little-endian bytes)
    const FREEBSD_UFS_GUID: [u8; 16] = [
        0xb6, 0x7c, 0x6e, 0x51, 0xcf, 0x6e, 0xd6, 0x11, 0x8f, 0xf8, 0x00, 0x02, 0x2d, 0x09, 0x71, 0x2b,
    ];
    // FreeBSD ZFS GUID: 516e7cba-6ecf-11d6-8ff8-00022d09712b
    const FREEBSD_ZFS_GUID: [u8; 16] = [
        0xba, 0x7c, 0x6e, 0x51, 0xcf, 0x6e, 0xd6, 0x11, 0x8f, 0xf8, 0x00, 0x02, 0x2d, 0x09, 0x71, 0x2b,
    ];

    let mut found_zfs = None;
    for idx in 0..read_entries {
        let entry = &entries_buf[idx * entry_size..(idx + 1) * entry_size];
        let type_guid: [u8; 16] = match entry[0..16].try_into() {
            Ok(g) => g,
            Err(_) => continue,
        };
        if type_guid == FREEBSD_UFS_GUID {
            return Some(format!("ufs:/dev/vtbd0p{}", idx + 1));
        } else if type_guid == FREEBSD_ZFS_GUID {
            found_zfs = Some(idx + 1);
        }
    }

    if let Some(zfs_idx) = found_zfs {
        eprintln!(
            "\x1b[1;33m[VMM Warning] Detected Root-on-ZFS partition (part {}) on '{}'.\x1b[0m",
            zfs_idx, disk_path
        );
        eprintln!(
            "\x1b[1;33m[VMM Hint] Direct ELF kernel booting requires an 'Auto (UFS)' root filesystem (in-kernel ufs_mount).\x1b[0m"
        );
        eprintln!(
            "\x1b[1;33m[VMM Hint] If you installed with Auto (ZFS), please reinstall using 'Auto (UFS) Guided Partitioning'.\x1b[0m"
        );
    }
    None
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
    let mut is_multiuser = false;
    let mut ssh_port: Option<u16> = None;
    let mut net_mode: Option<String> = None;
    let mut mac_addr: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    let mut cpus: usize = 1;
    let mut cpu_model = CpuModel::detect_host();
    let mut serial_log: Option<String> = None;

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
                println!("  -u, --multiuser          Boot FreeBSD into multi-user mode (omits single-user -s)");
                println!("      --ssh-port <PORT>    Forward host TCP port to guest SSH (e.g. 2222 -> 192.168.18.2:22)");
                println!("  -k, --kernel <PATH>      Boot specified ELF kernel");
                println!("  -c, --cpus <N>           Number of virtual CPUs (default: 1)");
                println!("      --cpu-model <MODEL>  CPU model/MIDR spoofing: auto/host (default), m1, m1-pro, m1-max, m2, m2-pro, m2-max, neoverse-n2, cortex-a78, or 0x<hex>");
                println!("      --serial-log <PATH>  Mirror guest serial console output to specified log file");
                println!("  -d, --disk <PATH>[,cow][,ro] Attach disk image (can be specified multiple times)");
                println!("      --cdrom, --iso <PATH>    Attach read-only ISO image as VirtIO block device");
                println!("      --cow                Enable Copy-On-Write overlay for all attached disks");
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

    if global_cow {
        for d in &mut disk_configs {
            d.cow = true;
        }
    }

    if is_freebsd && kernel_path.is_none() && std::path::Path::new("boot/kernel/kernel").exists() {
        kernel_path = Some("boot/kernel/kernel".to_string());
    }

    // Default root device: auto-detect from primary disk GPT partition table, or fall back to ufs:/dev/vtbd0p2
    if !disk_configs.is_empty() && root_dev.is_none() {
        if let Some(detected) = detect_root_partition(&disk_configs[0].path) {
            println!("[VMM] Auto-detected root partition: {}", detected);
            root_dev = Some(detected);
        } else {
            root_dev = Some("ufs:/dev/vtbd0p2".to_string());
        }
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
    let dtb_bytes = match fdt::generate_dtb(DEFAULT_RAM_BASE, ram_size as u64, bootargs, cpus, &virtio_devices) {
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

    if let Err(e) = vcpu0.setup_os_boot_state(loaded.entry_point, dtb_addr, 0, cpu_model.midr) {
        eprintln!("\x1b[1;31m[VMM Error] Failed to setup OS boot registers: {}\x1b[0m", e);
        process::exit(1);
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
