//! Map 64 MiB at GPA 0 and run the long-mode payload on one vCPU.

use std::path::PathBuf;
use std::sync::Arc;
use std::thread;

use vmm::devices::PioBus;
use vmm::devices::serial::{ConsoleHooks, HostConsole, Ns16550};
use vmm::memory::GuestMemory;
use vmm::terminal::{self, TerminalGuard};
use vmm::x86::{self, GUEST_RAM_BYTES, PAYLOAD_GPA};

use crate::hypervisor::*;
use crate::vcpu::{self, Kick, VcpuDone};

pub struct Finished {
    pub exec_ns: Option<u64>,
    pub error: Option<String>,
}

struct VmGuard;

impl Drop for VmGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = hv_vm_destroy();
        }
    }
}

pub fn run_payload(path: Option<PathBuf>, serial_log: Option<PathBuf>) -> Finished {
    match run_inner(path, serial_log) {
        Ok(done) => done,
        Err(error) => Finished {
            exec_ns: None,
            error: Some(error),
        },
    }
}

fn run_inner(path: Option<PathBuf>, serial_log: Option<PathBuf>) -> Result<Finished, String> {
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
    // Dropped after the vCPU thread has destroyed its vCPU.
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

    let kick = Arc::new(Kick::new());
    let kick_shutdown = kick.clone();
    let kick_rx = kick.clone();
    let serial = Arc::new(Ns16550::new(
        serial_log.as_deref(),
        ConsoleHooks {
            on_shutdown: Arc::new(move || {
                terminal::request_shutdown();
                kick_shutdown.signal();
            }),
            on_kick: Arc::new(move || kick_rx.signal()),
        },
    )?);
    let mut pio = PioBus::new();
    pio.register(0x3f8, 8, serial.clone());
    let pio = Arc::new(pio);

    let mut console = HostConsole::start(serial);
    let _raw = TerminalGuard::enter_raw_mode();
    terminal::install_shutdown_handlers();

    let kick_vcpu = kick.clone();
    let handle = thread::Builder::new()
        .name("vmm-vcpu".into())
        .spawn(move || vcpu::run(kick_vcpu, pio, cr3, GUEST_RAM_BYTES as u64))
        .map_err(|e| format!("spawn vCPU thread: {e}"))?;
    let done = match handle.join() {
        Ok(done) => done,
        Err(_) => VcpuDone {
            exec_ns: None,
            error: Some("vCPU thread panicked".into()),
        },
    };
    console.stop();
    Ok(Finished {
        exec_ns: done.exec_ns,
        error: done.error,
    })
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
