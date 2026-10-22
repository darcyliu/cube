//! Shared devices for every vCPU. Hypervisor calls stay in `vcpu`.

use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use vmm::devices::i8042::I8042;
use vmm::devices::ioapic::{IOAPIC_MMIO, IOAPIC_MMIO_LEN, IoApic, IoapicMsg};
use vmm::devices::lapic::{
    DeliveryMode, LAPIC_MMIO, LAPIC_MMIO_LEN, Lapic, LapicEffect, Peer, logical_match,
};
use vmm::devices::pci::{PciAction, PciHost, PciIrq};
use vmm::devices::{MmioBus, PioBus};
use vmm::gpa::GuestSpace;
use vmm::terminal::host_log;

use crate::vcpu::Kick;

pub struct Machine {
    pub space: GuestSpace,
    pub pio: PioBus,
    mmio: Mutex<MmioBus>,
    pci: Option<Mutex<PciHost>>,
    i8042: Option<Arc<I8042>>,
    pub firmware: bool,
    ioapic: Mutex<IoApic>,
    lapics: Vec<Mutex<Lapic>>,
    kicks: Vec<Arc<Kick>>,
}

impl Machine {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        space: GuestSpace,
        pio: PioBus,
        mmio: MmioBus,
        pci: Option<PciHost>,
        i8042: Option<Arc<I8042>>,
        firmware: bool,
        lapics: Vec<Mutex<Lapic>>,
        kicks: Vec<Arc<Kick>>,
    ) -> Self {
        Self {
            space,
            pio,
            mmio: Mutex::new(mmio),
            pci: pci.map(Mutex::new),
            i8042,
            firmware,
            ioapic: Mutex::new(IoApic::new()),
            lapics,
            kicks,
        }
    }

    pub fn cpus(&self) -> u32 {
        self.lapics.len() as u32
    }

    pub fn cr8_read(&self, cpu: usize) -> u64 {
        u64::from(self.lapics[cpu].lock().unwrap().tpr() >> 4)
    }

    pub fn cr8_write(&self, cpu: usize, value: u64) {
        self.lapics[cpu]
            .lock()
            .unwrap()
            .set_tpr(((value as u32) & 0xf) << 4);
    }

    pub fn pio_read(&self, port: u16, size: usize) -> Result<u64, String> {
        if let Some(pci) = &self.pci
            && let Some(value) = pci.lock().unwrap().try_in(port, size)
        {
            return Ok(value);
        }
        match self.pio.read(port, size) {
            Ok(value) => Ok(value),
            Err(_) if self.firmware => {
                trace_unhandled_pio(port, false, 0, size);
                Ok(pio_ones(size))
            }
            Err(e) => Err(e),
        }
    }

    pub fn pio_write(&self, port: u16, data: u64, size: usize) -> Result<(), String> {
        if let Some(pci) = &self.pci {
            let mut pci = pci.lock().unwrap();
            if pci.try_out(port, data, size) {
                let action = pci.take_action();
                let irqs = pci.take_intx();
                drop(pci);
                self.deliver_intx(irqs);
                self.apply_pci(action);
                return Ok(());
            }
        }
        match self.pio.write(port, data, size) {
            Ok(()) => {
                if self.i8042.as_ref().is_some_and(|kbd| kbd.take_reboot()) {
                    host_log(format_args!("[VMM] 8042 reset"));
                    vmm::terminal::request_reboot();
                    self.stop_cpus();
                }
                Ok(())
            }
            Err(_) if self.firmware => {
                trace_unhandled_pio(port, true, data, size);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }

    fn apply_pci(&self, action: PciAction) {
        match action {
            PciAction::None => {}
            PciAction::Shutdown => {
                host_log(format_args!("[VMM] PM1 shutdown"));
                self.stop_cpus();
            }
            PciAction::Reboot => {
                host_log(format_args!("[VMM] port 0xCF9 reset"));
                vmm::terminal::request_reboot();
                self.stop_cpus();
            }
        }
    }

    pub fn set_gsi(&self, gsi: u32, high: bool) {
        let msgs = self.ioapic.lock().unwrap().set_level(gsi, high);
        self.route(msgs);
    }

    /// Drop the PCI lock before the I/O APIC. `high` is the electrical level.
    fn deliver_intx(&self, irqs: Vec<PciIrq>) {
        for irq in irqs {
            self.set_gsi(irq.gsi, irq.high);
        }
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
        if (in_range(gpa, LAPIC_MMIO, LAPIC_MMIO_LEN)
            || in_range(gpa, IOAPIC_MMIO, IOAPIC_MMIO_LEN))
            && size != 4
        {
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
        if let Ok(value) = self.mmio.lock().unwrap().read(gpa, size) {
            return Ok(value);
        }
        if let Some(value) = self.pci_bar_read(gpa, size) {
            return Ok(value);
        }
        if self.firmware {
            note_unmapped_mmio(gpa);
            return Ok(match size {
                1 => 0xff,
                2 => 0xffff,
                4 => 0xffff_ffff,
                _ => 0xffff_ffff_ffff_ffff,
            });
        }
        Err(format!("MMIO read at unmapped {gpa:#x}"))
    }

    pub fn mmio_write(&self, cpu: usize, gpa: u64, value: u64, size: usize) -> Result<(), String> {
        if (in_range(gpa, LAPIC_MMIO, LAPIC_MMIO_LEN)
            || in_range(gpa, IOAPIC_MMIO, IOAPIC_MMIO_LEN))
            && size != 4
        {
            return Err(format!(
                "MMIO write width {size} at {gpa:#x} val {value:#x}"
            ));
        }
        if !(in_range(gpa, LAPIC_MMIO, LAPIC_MMIO_LEN)
            || in_range(gpa, IOAPIC_MMIO, IOAPIC_MMIO_LEN))
        {
            if self.mmio.lock().unwrap().write(gpa, value, size).is_ok() {
                return Ok(());
            }
            if self.pci_bar_write(gpa, value, size) {
                return Ok(());
            }
            if self.firmware {
                note_unmapped_mmio(gpa);
                return Ok(());
            }
            return Err(format!("MMIO write at unmapped {gpa:#x}"));
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

    fn pci_bar_read(&self, gpa: u64, size: usize) -> Option<u64> {
        let pci = self.pci.as_ref()?;
        let mut pci = pci.lock().unwrap();
        let value = pci.mmio_read(gpa, size)?;
        let irqs = pci.take_intx();
        drop(pci);
        self.deliver_intx(irqs);
        Some(value)
    }

    fn pci_bar_write(&self, gpa: u64, value: u64, size: usize) -> bool {
        let Some(pci) = self.pci.as_ref() else {
            return false;
        };
        let mut pci = pci.lock().unwrap();
        if !pci.mmio_write(gpa, value, size) {
            return false;
        }
        let irqs = pci.take_intx();
        drop(pci);
        self.deliver_intx(irqs);
        true
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

fn pio_ones(size: usize) -> u64 {
    match size {
        1 => 0xff,
        2 => 0xffff,
        _ => 0xffff_ffff,
    }
}

fn trace_unhandled_pio(port: u16, write: bool, data: u64, size: usize) {
    if std::env::var("VMM_TRACE_EXITS").ok().as_deref() != Some("1") {
        return;
    }
    static SEEN: Mutex<Vec<u16>> = Mutex::new(Vec::new());
    let mut seen = SEEN.lock().unwrap();
    if seen.contains(&port) {
        return;
    }
    seen.push(port);
    if write {
        host_log(format_args!(
            "[VMM] unhandled PIO write {port:#04x} val {data:#x} size {size}"
        ));
    } else {
        host_log(format_args!(
            "[VMM] unhandled PIO read {port:#04x} size {size}"
        ));
    }
}

fn note_unmapped_mmio(gpa: u64) {
    static SEEN: Mutex<Vec<u64>> = Mutex::new(Vec::new());
    let page = gpa & !0xfff;
    let mut seen = SEEN.lock().unwrap();
    if seen.len() >= 32 || seen.contains(&page) {
        return;
    }
    seen.push(page);
    host_log(format_args!("[VMM] unmapped MMIO {gpa:#x}"));
}

pub fn mono_ns() -> u64 {
    static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    BASE.get_or_init(Instant::now).elapsed().as_nanos() as u64
}
