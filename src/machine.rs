//! Shared devices for every vCPU. Hypervisor calls stay in `vcpu`.

use std::sync::{Mutex, Weak};
use std::time::Instant;

use vmm::devices::PioBus;
use vmm::devices::ioapic::{IOAPIC_MMIO, IOAPIC_MMIO_LEN, IoApic, IoapicMsg};
use vmm::devices::lapic::{
    DeliveryMode, LAPIC_MMIO, LAPIC_MMIO_LEN, Lapic, LapicEffect, Peer, logical_match,
};
use vmm::gpa::GuestSpace;

use crate::vcpu::Kick;

pub struct Machine {
    pub space: GuestSpace,
    pub pio: PioBus,
    ioapic: Mutex<IoApic>,
    lapics: Vec<Mutex<Lapic>>,
    kicks: Vec<std::sync::Arc<Kick>>,
}

impl Machine {
    pub fn new(
        space: GuestSpace,
        pio: PioBus,
        lapics: Vec<Mutex<Lapic>>,
        kicks: Vec<std::sync::Arc<Kick>>,
    ) -> Self {
        Self {
            space,
            pio,
            ioapic: Mutex::new(IoApic::new()),
            lapics,
            kicks,
        }
    }

    pub fn set_gsi(self: &std::sync::Arc<Self>, gsi: u32, high: bool) {
        let msgs = self.ioapic.lock().unwrap().set_level(gsi, high);
        self.route(msgs);
    }

    pub fn wire_uart_irq(self: &std::sync::Arc<Self>, serial: &vmm::devices::serial::Ns16550) {
        let weak: Weak<Machine> = std::sync::Arc::downgrade(self);
        serial.set_irq_notify(std::sync::Arc::new(move |high| {
            if let Some(machine) = weak.upgrade() {
                machine.set_gsi(4, high);
            }
        }));
    }

    pub fn expire_timer(&self, cpu: usize) {
        self.lapics[cpu].lock().unwrap().expire_timer();
    }

    pub fn timer_remaining_ns(&self, cpu: usize) -> Option<u64> {
        let mut lapic = self.lapics[cpu].lock().unwrap();
        let now = mono_ns();
        lapic.poll(now);
        lapic.remaining_ns(now)
    }

    pub fn has_deliverable(&self, cpu: usize) -> bool {
        self.lapics[cpu]
            .lock()
            .unwrap()
            .highest_deliverable()
            .is_some()
    }

    pub fn accept_irq(&self, cpu: usize) -> Option<u8> {
        self.lapics[cpu].lock().unwrap().accept()
    }

    pub fn requeue_irq(&self, cpu: usize, vector: u8) {
        self.lapics[cpu].lock().unwrap().requeue(vector);
    }

    pub fn stop_cpus(&self) {
        vmm::terminal::request_shutdown();
        for kick in &self.kicks {
            kick.request_stop();
            kick.signal();
        }
    }

    /// UART wake. A shutdown byte must also unstick vCPUs parked outside `hv_vcpu_run`.
    pub fn on_console_kick(&self) {
        if vmm::terminal::shutdown_requested() {
            self.stop_cpus();
        } else {
            for kick in &self.kicks {
                kick.signal();
            }
        }
    }

    pub fn mmio_read(&self, cpu: usize, gpa: u64, size: usize) -> Result<u64, String> {
        if size != 4 {
            return Err(format!("MMIO read width {size} at {gpa:#x}"));
        }
        if in_range(gpa, LAPIC_MMIO, LAPIC_MMIO_LEN) {
            let off = (gpa - LAPIC_MMIO) as u32;
            let value = self.lapics[cpu].lock().unwrap().read32(off, mono_ns());
            return Ok(value as u64);
        }
        if in_range(gpa, IOAPIC_MMIO, IOAPIC_MMIO_LEN) {
            let off = (gpa - IOAPIC_MMIO) as u32;
            return Ok(self.ioapic.lock().unwrap().read32(off) as u64);
        }
        Err(format!("MMIO read at unmapped {gpa:#x}"))
    }

    pub fn mmio_write(&self, cpu: usize, gpa: u64, value: u64, size: usize) -> Result<(), String> {
        if size != 4 {
            return Err(format!(
                "MMIO write width {size} at {gpa:#x} val {value:#x}"
            ));
        }
        let value = value as u32;
        if in_range(gpa, LAPIC_MMIO, LAPIC_MMIO_LEN) {
            let off = (gpa - LAPIC_MMIO) as u32;
            let peers = self.peers();
            let effect = self.lapics[cpu]
                .lock()
                .unwrap()
                .write32(off, value, mono_ns());
            match effect {
                LapicEffect::None => {}
                LapicEffect::Icr => self.dispatch_icr(cpu, &peers),
                LapicEffect::Eoi(event) => {
                    if event.level {
                        let msgs = self.ioapic.lock().unwrap().eoi(event.vector);
                        self.route(msgs);
                    }
                }
            }
            return Ok(());
        }
        if in_range(gpa, IOAPIC_MMIO, IOAPIC_MMIO_LEN) {
            let off = (gpa - IOAPIC_MMIO) as u32;
            let msgs = self.ioapic.lock().unwrap().write32(off, value);
            self.route(msgs);
            return Ok(());
        }
        Err(format!("MMIO write at unmapped {gpa:#x}"))
    }

    fn dispatch_icr(&self, cpu: usize, peers: &[Peer]) {
        let ipis = {
            let lapic = self.lapics[cpu].lock().unwrap();
            lapic.icr_targets(lapic.icr_low(), peers)
        };
        for ipi in ipis {
            let Some(index) = self.index_of(ipi.dest) else {
                continue;
            };
            match ipi.mode {
                DeliveryMode::Init => self.kicks[index].init_ipi(),
                DeliveryMode::Startup => {
                    self.kicks[index].sipi(ipi.vector);
                }
                DeliveryMode::Fixed => {
                    self.lapics[index].lock().unwrap().raise(ipi.vector, false);
                    self.kicks[index].signal();
                }
                DeliveryMode::Other(_) => {}
            }
        }
    }

    fn route(&self, msgs: Vec<IoapicMsg>) {
        if msgs.is_empty() {
            return;
        }
        let peers = self.peers();
        for msg in msgs {
            let targets: Vec<usize> = if msg.logical {
                peers
                    .iter()
                    .enumerate()
                    .filter(|(_, peer)| logical_match(peer.dfr, peer.ldr, msg.dest))
                    .map(|(index, _)| index)
                    .collect()
            } else if msg.dest == 0xff {
                (0..peers.len()).collect()
            } else {
                self.index_of(msg.dest).into_iter().collect()
            };
            for index in targets {
                self.lapics[index]
                    .lock()
                    .unwrap()
                    .raise(msg.vector, msg.level_triggered);
                self.kicks[index].signal();
            }
        }
    }

    fn peers(&self) -> Vec<Peer> {
        self.lapics
            .iter()
            .map(|lapic| {
                let lapic = lapic.lock().unwrap();
                Peer {
                    id: lapic.id(),
                    ldr: lapic.ldr(),
                    dfr: lapic.dfr(),
                }
            })
            .collect()
    }

    fn index_of(&self, id: u32) -> Option<usize> {
        self.lapics
            .iter()
            .position(|lapic| lapic.lock().unwrap().id() == id)
    }
}

fn in_range(gpa: u64, base: u64, len: u64) -> bool {
    gpa >= base && gpa < base + len
}

pub fn mono_ns() -> u64 {
    static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    BASE.get_or_init(Instant::now).elapsed().as_nanos() as u64
}
