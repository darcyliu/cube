//! Map 64 MiB at GPA 0 and run the payload. vCPU 0 is the BSP.
//! Extra vCPUs are created, then wait for INIT/SIPI outside `hv_vcpu_run`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::thread;

use vmm::devices::PioBus;
use vmm::devices::debugcon::DebugCon;
use vmm::devices::lapic::Lapic;
use vmm::devices::pic::Pic;
use vmm::devices::pit::Pit;
use vmm::devices::serial::{ConsoleHooks, HostConsole, Ns16550};
use vmm::gpa::GuestSpace;
use vmm::memory::GuestMemory;
use vmm::terminal::{self, TerminalGuard};
use vmm::x86::{self, GUEST_RAM_BYTES, PAYLOAD_GPA};

use crate::hypervisor::*;
use crate::machine::Machine;
use crate::vcpu::{self, Kick, VcpuDone};

struct VmGuard;

impl Drop for VmGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = hv_vm_destroy();
        }
    }
}

pub fn run_payload(path: Option<PathBuf>, serial_log: Option<PathBuf>, cpus: u32) -> Vec<VcpuDone> {
    match run_inner(path, serial_log, cpus) {
        Ok(done) => done,
        Err(error) => vec![VcpuDone {
            index: 0,
            exec_ns: None,
            error: Some(error),
        }],
    }
}

fn run_inner(
    path: Option<PathBuf>,
    serial_log: Option<PathBuf>,
    cpus: u32,
) -> Result<Vec<VcpuDone>, String> {
    if !(1..=16).contains(&cpus) {
        return Err("--cpus must be from 1 to 16".into());
    }
    let n = cpus as usize;
    let bytes = load_payload(path)?;
    let mem = GuestMemory::allocate(0, GUEST_RAM_BYTES)?;
    let cr3 = x86::install_identity_map(&mem)?;
    x86::write_gdt_tss(&mem)?;
    let entry = PAYLOAD_GPA;
    if entry as usize + bytes.len() > mem.size() {
        return Err("payload does not fit in guest RAM".into());
    }
    mem.write(entry, &bytes)?;

    check_hv(unsafe { hv_vm_create(HV_VM_DEFAULT) }).map_err(|e| {
        format!("{e} (codesign --entitlements entitlements.plist --force -s - target/debug/vmm)")
    })?;
    // Dropped after every vCPU thread has destroyed its vCPU.
    let _vm = VmGuard;
    check_hv(unsafe {
        hv_vm_map(
            mem.host_ptr().cast(),
            mem.guest_base(),
            mem.size(),
            HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC,
        )
    })
    .map_err(|e| format!("hv_vm_map: {e}"))?;

    let mut kicks = Vec::with_capacity(n);
    let mut lapics = Vec::with_capacity(n);
    for i in 0..n {
        kicks.push(Arc::new(if i == 0 { Kick::bsp() } else { Kick::ap() }));
        lapics.push(Mutex::new(Lapic::new(i as u32)));
    }

    let slot: Arc<OnceLock<Weak<Machine>>> = Arc::new(OnceLock::new());
    let slot_shutdown = slot.clone();
    let slot_kick = slot.clone();
    let serial = Arc::new(Ns16550::new(
        serial_log.as_deref(),
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
    )?);
    let mut pio = PioBus::new();
    pio.register(0x3f8, 8, serial.clone());
    pio.register(0x20, 2, Arc::new(Pic::new()));
    pio.register(0xa0, 2, Arc::new(Pic::new()));
    pio.register(0x40, 4, Arc::new(Pit::new()));
    pio.register(0x402, 1, Arc::new(DebugCon));

    let machine = Arc::new(Machine::new(
        GuestSpace::new(mem, None),
        pio,
        lapics,
        kicks.clone(),
    ));
    let _ = slot.set(Arc::downgrade(&machine));
    machine.wire_uart_irq(&serial);

    let mut console = HostConsole::start(serial);
    let _raw = TerminalGuard::enter_raw_mode();
    terminal::install_shutdown_handlers();

    let mut handles = Vec::with_capacity(n);
    for (i, kick) in kicks.iter().enumerate() {
        let kick = Arc::clone(kick);
        let machine_thread = machine.clone();
        match thread::Builder::new()
            .name(format!("vmm-vcpu-{i}"))
            .spawn(move || vcpu::run(i, kick, machine_thread, cr3))
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
