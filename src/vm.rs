//! Map guest RAM at GPA 0 and run a payload or OVMF. vCPU 0 is the BSP.
//! Extra vCPUs are created, then wait for INIT/SIPI outside `hv_vcpu_run`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread;

use vmm::devices::MmioBus;
use vmm::devices::PioBus;
use vmm::devices::cmos::Cmos;
use vmm::devices::debugcon::DebugCon;
use vmm::devices::fw_cfg::{FwCfg, FwCfgPio};
use vmm::devices::i8042::I8042;
use vmm::devices::lapic::Lapic;
use vmm::devices::pci::PciHost;
use vmm::devices::pflash::CfiPflash;
use vmm::devices::pic::Pic;
use vmm::devices::pit::Pit;
use vmm::devices::serial::{ConsoleHooks, HostConsole, Ns16550};
use vmm::devices::virtio::block::VirtioBlock;
use vmm::devices::virtio::net::{VirtioNet, net_learns_dhcp};
use vmm::forward::{self, ForwardRule, PortForward};
use vmm::gpa::{self, GuestSpace};
use vmm::memory::GuestMemory;
use vmm::terminal::{self, TerminalGuard};
use vmm::x86::{self, GUEST_RAM_BYTES, PAYLOAD_GPA, UEFI_RAM_BYTES};

use crate::hypervisor::*;
use crate::machine::Machine;
use crate::vcpu::{self, Entry, Kick, VcpuDone};

/// One virtio-blk image. `cow` uses `<path>.cow` and cannot be combined with `read_only`.
pub struct DiskSpec {
    pub path: PathBuf,
    pub cow: bool,
    pub read_only: bool,
}

/// `--net` omitted stays `none`, so the payload and OVMF smokes do not need root.
pub struct NetConfig {
    pub mode: String,
    pub mac: [u8; 6],
    pub socket: Option<PathBuf>,
    pub ssh_port: Option<u16>,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            mode: "none".into(),
            mac: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
            socket: None,
            ssh_port: None,
        }
    }
}

struct VmGuard;

impl Drop for VmGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = hv_vm_destroy();
        }
    }
}

pub fn run_payload(
    path: Option<PathBuf>,
    serial_log: Option<PathBuf>,
    cpus: u32,
    disks: Vec<DiskSpec>,
    net: NetConfig,
    forwards: Vec<ForwardRule>,
) -> Vec<VcpuDone> {
    match run_payload_inner(path, serial_log, cpus, disks, net, forwards) {
        Ok(done) => done,
        Err(error) => vec![failed(error)],
    }
}

pub fn run_uefi(
    firmware: PathBuf,
    serial_log: Option<PathBuf>,
    cpus: u32,
    disks: Vec<DiskSpec>,
    net: NetConfig,
    forwards: Vec<ForwardRule>,
) -> Vec<VcpuDone> {
    match run_uefi_inner(firmware, serial_log, cpus, disks, net, forwards) {
        Ok(done) => done,
        Err(error) => vec![failed(error)],
    }
}

fn failed(error: String) -> VcpuDone {
    VcpuDone {
        index: 0,
        exec_ns: None,
        error: Some(error),
    }
}

fn run_payload_inner(
    path: Option<PathBuf>,
    serial_log: Option<PathBuf>,
    cpus: u32,
    disks: Vec<DiskSpec>,
    net: NetConfig,
    forwards: Vec<ForwardRule>,
) -> Result<Vec<VcpuDone>, String> {
    let n = cpu_count(cpus)?;
    let bytes = load_payload(path)?;
    let mem = Arc::new(GuestMemory::allocate(0, GUEST_RAM_BYTES)?);
    let cr3 = x86::install_identity_map(&mem)?;
    x86::write_gdt_tss(&mem)?;
    let entry = PAYLOAD_GPA;
    if entry as usize + bytes.len() > mem.size() {
        return Err("payload does not fit in guest RAM".into());
    }
    mem.write(entry, &bytes)?;
    // A missing image, forward bind, or NIC must not call hv_vm_create.
    let blocks = load_blocks(&mem, &disks)?;
    let (nic, _forwards) = attach_net(&mem, &net, &forwards)?;
    let pci = assemble_pci(blocks, nic, false)?;
    let gsis = pci.as_ref().map(PciHost::virtio_gsis).unwrap_or_default();

    let _vm = create_vm(&mem)?;
    let slot: Arc<OnceLock<Weak<Machine>>> = Arc::new(OnceLock::new());
    let serial = open_serial(serial_log.as_deref(), &slot)?;
    let (kicks, lapics) = vcpu_parts(n);
    let machine = Arc::new(Machine::new(
        GuestSpace::new(Arc::clone(&mem), None),
        standard_pio(serial.clone()),
        MmioBus::new(),
        pci,
        None,
        false,
        lapics,
        kicks.clone(),
    ));
    let _ = slot.set(Arc::downgrade(&machine));
    machine.wire_uart_irq(&serial);
    machine.wire_net_irq();
    idle_intx(&machine, &gsis);
    boot_cpus(machine, kicks, serial, Entry::Payload { cr3 })
}

fn run_uefi_inner(
    firmware: PathBuf,
    serial_log: Option<PathBuf>,
    cpus: u32,
    disks: Vec<DiskSpec>,
    net: NetConfig,
    forwards: Vec<ForwardRule>,
) -> Result<Vec<VcpuDone>, String> {
    let n = cpu_count(cpus)?;
    let code_bytes =
        std::fs::read(&firmware).map_err(|e| format!("reading {}: {e}", firmware.display()))?;
    if code_bytes.len() != gpa::CODE_FLASH_SIZE as usize {
        return Err(format!(
            "{} is {} bytes, want {}",
            firmware.display(),
            code_bytes.len(),
            gpa::CODE_FLASH_SIZE
        ));
    }
    let template_path = PathBuf::from("firmware/OVMF_VARS.fd");
    let template = std::fs::read(&template_path)
        .map_err(|e| format!("reading {}: {e}", template_path.display()))?;
    if template.len() != gpa::VARS_FLASH_SIZE as usize {
        return Err(format!(
            "{} is {} bytes, want {}",
            template_path.display(),
            template.len(),
            gpa::VARS_FLASH_SIZE
        ));
    }
    let mut store = firmware.clone().into_os_string();
    store.push(".vars.fd");
    let flash = CfiPflash::open(Path::new(&store), template, 4096)?;

    let ram = Arc::new(GuestMemory::allocate(0, UEFI_RAM_BYTES)?);
    let code = Arc::new(GuestMemory::from_buffer(gpa::CODE_FLASH_BASE, &code_bytes)?);
    // A missing image, forward bind, or NIC must not call hv_vm_create.
    let blocks = load_blocks(&ram, &disks)?;
    let (nic, _forwards) = attach_net(&ram, &net, &forwards)?;
    let pci = assemble_pci(blocks, nic, true)?.expect("UEFI always has a PCI host");
    let gsis = pci.virtio_gsis();
    let tables = vmm::acpi::build_with_prt(cpus, UEFI_RAM_BYTES as u64, &pci.prt());
    let cfg = FwCfg::from_files(
        Arc::clone(&ram),
        &[
            ("etc/e820", tables.e820.as_slice()),
            ("etc/acpi/rsdp", tables.rsdp.as_slice()),
            ("etc/acpi/tables", tables.tables.as_slice()),
            ("etc/table-loader", tables.loader.as_slice()),
        ],
    );

    let _vm = create_vm(&ram)?;
    let rx = HV_MEMORY_READ | HV_MEMORY_EXEC;
    let map_rx = unsafe { hv_vm_map(code.host_ptr().cast(), code.guest_base(), code.size(), rx) };
    if map_rx == HV_BAD_ARGUMENT {
        // Some 10.15 builds reject a mapping that was never writable.
        check_hv(unsafe {
            hv_vm_map(
                code.host_ptr().cast(),
                code.guest_base(),
                code.size(),
                rx | HV_MEMORY_WRITE,
            )
        })
        .map_err(|e| format!("hv_vm_map code: {e}"))?;
        check_hv(unsafe { hv_vm_protect(code.guest_base(), code.size(), rx) })
            .map_err(|e| format!("hv_vm_protect code: {e}"))?;
        eprintln!("[VMM] code flash mapped RWX then protected read|exec");
    } else {
        check_hv(map_rx).map_err(|e| format!("hv_vm_map code: {e}"))?;
    }

    let slot: Arc<OnceLock<Weak<Machine>>> = Arc::new(OnceLock::new());
    let serial = open_serial(serial_log.as_deref(), &slot)?;
    let (kicks, lapics) = vcpu_parts(n);
    let mut pio = standard_pio(serial.clone());
    // DMA low is a dword at 0x518, so the window runs through 0x51B.
    pio.register(0x510, 12, Arc::new(FwCfgPio::new(cfg)));
    pio.register(0x70, 2, Arc::new(Cmos::new(UEFI_RAM_BYTES as u64)));
    let kbd = Arc::new(I8042::new());
    pio.register(0x60, 5, kbd.clone());
    let mut mmio = MmioBus::new();
    mmio.register(gpa::VARS_FLASH_BASE, gpa::VARS_FLASH_SIZE, Box::new(flash));
    let machine = Arc::new(Machine::new(
        GuestSpace::new(ram, Some(code)),
        pio,
        mmio,
        Some(pci),
        Some(kbd),
        true,
        lapics,
        kicks.clone(),
    ));
    let _ = slot.set(Arc::downgrade(&machine));
    machine.wire_uart_irq(&serial);
    machine.wire_net_irq();
    idle_intx(&machine, &gsis);
    boot_cpus(machine, kicks, serial, Entry::Firmware)
}

fn load_blocks(mem: &Arc<GuestMemory>, disks: &[DiskSpec]) -> Result<Vec<VirtioBlock>, String> {
    let mut blocks = Vec::with_capacity(disks.len());
    for (i, disk) in disks.iter().enumerate() {
        let serial = format!("vtbd{i}");
        let path = disk
            .path
            .to_str()
            .ok_or_else(|| format!("disk path is not utf-8: {}", disk.path.display()))?;
        let block = if disk.cow {
            VirtioBlock::from_cow(path, &format!("{path}.cow"), 0, &serial, Arc::clone(mem))?
        } else {
            VirtioBlock::from_raw(path, disk.read_only, 0, &serial, Arc::clone(mem))?
        };
        blocks.push(block);
    }
    Ok(blocks)
}

fn attach_net(
    mem: &Arc<GuestMemory>,
    net: &NetConfig,
    forwards: &[ForwardRule],
) -> Result<(Option<VirtioNet>, Vec<PortForward>), String> {
    if net.ssh_port.is_some() && !net_learns_dhcp(&net.mode) {
        return Err("--ssh-port requires a DHCP-capable vmnet/bridge NIC; use --forward for an explicit destination".into());
    }
    let mut running = Vec::new();
    for rule in forwards {
        running.push(forward::start_port_forward(rule)?);
    }
    if net.mode == "none" {
        return Ok((None, running));
    }
    let nic = VirtioNet::new(net.mac, Arc::clone(mem), &net.mode, net.socket.as_deref())?;
    if let Some(port) = net.ssh_port {
        running.push(forward::start_ssh_forward(port, nic.guest_address())?);
    }
    Ok((Some(nic), running))
}

fn assemble_pci(
    blocks: Vec<VirtioBlock>,
    net: Option<VirtioNet>,
    always: bool,
) -> Result<Option<PciHost>, String> {
    if !always && blocks.is_empty() && net.is_none() {
        return Ok(None);
    }
    let mut pci = PciHost::new();
    for block in blocks {
        pci.add_block(block)?;
    }
    if let Some(net) = net {
        pci.add_net(net)?;
    }
    Ok(Some(pci))
}

/// PCI INTx is active-low. Park each line high while the pin is still masked.
fn idle_intx(machine: &Machine, gsis: &[u32]) {
    for &gsi in gsis {
        machine.set_gsi(gsi, true);
    }
}

fn cpu_count(cpus: u32) -> Result<usize, String> {
    if !(1..=16).contains(&cpus) {
        return Err("--cpus must be from 1 to 16".into());
    }
    Ok(cpus as usize)
}

fn create_vm(mem: &GuestMemory) -> Result<VmGuard, String> {
    check_hv(unsafe { hv_vm_create(HV_VM_DEFAULT) }).map_err(|e| {
        format!("{e} (codesign --entitlements entitlements.plist --force -s - target/debug/vmm)")
    })?;
    let guard = VmGuard;
    check_hv(unsafe {
        hv_vm_map(
            mem.host_ptr().cast(),
            mem.guest_base(),
            mem.size(),
            HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC,
        )
    })
    .map_err(|e| format!("hv_vm_map: {e}"))?;
    Ok(guard)
}

fn vcpu_parts(n: usize) -> (Vec<Arc<Kick>>, Vec<Mutex<Lapic>>) {
    let mut kicks = Vec::with_capacity(n);
    let mut lapics = Vec::with_capacity(n);
    for i in 0..n {
        kicks.push(Arc::new(if i == 0 { Kick::bsp() } else { Kick::ap() }));
        lapics.push(Mutex::new(Lapic::new(i as u32)));
    }
    (kicks, lapics)
}

fn open_serial(
    serial_log: Option<&Path>,
    slot: &Arc<OnceLock<Weak<Machine>>>,
) -> Result<Arc<Ns16550>, String> {
    let slot_shutdown = Arc::clone(slot);
    let slot_kick = Arc::clone(slot);
    Ok(Arc::new(Ns16550::new(
        serial_log,
        ConsoleHooks {
            on_shutdown: Arc::new(move || {
                terminal::request_shutdown();
                if let Some(machine) = slot_shutdown.get().and_then(|weak| weak.upgrade()) {
                    machine.stop_cpus();
                }
            }),
            on_kick: Arc::new(move || {
                if let Some(machine) = slot_kick.get().and_then(|weak| weak.upgrade()) {
                    machine.on_console_kick();
                }
            }),
        },
    )?))
}

fn standard_pio(serial: Arc<Ns16550>) -> PioBus {
    let mut pio = PioBus::new();
    pio.register(0x3f8, 8, serial);
    pio.register(0x20, 2, Arc::new(Pic::new()));
    pio.register(0xa0, 2, Arc::new(Pic::new()));
    pio.register(0x40, 4, Arc::new(Pit::new()));
    pio.register(0x402, 1, Arc::new(DebugCon));
    pio
}

fn boot_cpus(
    machine: Arc<Machine>,
    kicks: Vec<Arc<Kick>>,
    serial: Arc<Ns16550>,
    entry: Entry,
) -> Result<Vec<VcpuDone>, String> {
    let mut console = HostConsole::start(serial);
    let _raw = TerminalGuard::enter_raw_mode();
    terminal::install_shutdown_handlers();

    let n = kicks.len();
    let mut handles = Vec::with_capacity(n);
    for (i, kick) in kicks.into_iter().enumerate() {
        let machine_thread = Arc::clone(&machine);
        match thread::Builder::new()
            .name(format!("vmm-vcpu-{i}"))
            .spawn(move || vcpu::run(i, kick, machine_thread, entry))
        {
            Ok(handle) => handles.push(handle),
            Err(e) => {
                machine.stop_cpus();
                for handle in handles {
                    let _ = handle.join();
                }
                return Err(format!("spawn vCPU thread: {e}"));
            }
        }
    }

    let mut dones = Vec::with_capacity(n);
    for (i, handle) in handles.into_iter().enumerate() {
        match handle.join() {
            Ok(done) => dones.push(done),
            Err(_) => {
                machine.stop_cpus();
                dones.push(VcpuDone {
                    index: i as u32,
                    exec_ns: None,
                    error: Some("vCPU thread panicked".into()),
                });
            }
        }
    }
    console.stop();
    Ok(dones)
}

fn load_payload(path: Option<PathBuf>) -> Result<Vec<u8>, String> {
    let bytes = if let Some(path) = path {
        std::fs::read(&path).map_err(|e| format!("reading {}: {e}", path.display()))?
    } else {
        include_bytes!(concat!(env!("OUT_DIR"), "/payload.bin")).to_vec()
    };
    if bytes.is_empty() {
        return Err("payload is empty".into());
    }
    Ok(bytes)
}
