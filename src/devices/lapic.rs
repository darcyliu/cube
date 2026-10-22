//! Software xAPIC. One instance per vCPU. No Hypervisor.framework calls.
//!
//! The one-shot current count is `initial - elapsed_ns / divisor` at a 1 GHz
//! bus. Divisor comes from the divide-configuration register.

pub const LAPIC_MMIO: u64 = 0xFEE0_0000;
pub const LAPIC_MMIO_LEN: u64 = 0x1000;
/// Bus frequency used to turn a count into a host duration.
pub const LAPIC_BUS_HZ: u64 = 1_000_000_000;

const VERSION: u32 = 0x0005_0014;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryMode {
    Fixed,
    Init,
    Startup,
    Other(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Ipi {
    pub dest: u32,
    pub mode: DeliveryMode,
    pub vector: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EoiEvent {
    pub vector: u8,
    pub level: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Peer {
    pub id: u32,
    pub ldr: u32,
    pub dfr: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LapicEffect {
    None,
    Icr,
    Eoi(EoiEvent),
}

pub struct Lapic {
    id: u32,
    tpr: u32,
    svr: u32,
    ldr: u32,
    dfr: u32,
    esr: u32,
    icr_low: u32,
    icr_high: u32,
    lvt_timer: u32,
    lvt_thermal: u32,
    lvt_perf: u32,
    lvt_lint0: u32,
    lvt_lint1: u32,
    lvt_error: u32,
    initial: u32,
    dcr: u32,
    isr: [u32; 8],
    tmr: [u32; 8],
    irr: [u32; 8],
    timer_start_ns: u64,
    timer_divisor: u32,
    timer_armed: bool,
    timer_periodic: bool,
    forced_zero: bool,
    raised_periods: u64,
}

impl Lapic {
    pub fn new(id: u32) -> Self {
        Self {
            id,
            tpr: 0,
            svr: 0xff,
            ldr: 0,
            dfr: 0xffff_ffff,
            esr: 0,
            icr_low: 0,
            icr_high: 0,
            lvt_timer: 1 << 16,
            lvt_thermal: 1 << 16,
            lvt_perf: 1 << 16,
            lvt_lint0: 1 << 16,
            lvt_lint1: 1 << 16,
            lvt_error: 1 << 16,
            initial: 0,
            dcr: 0,
            isr: [0; 8],
            tmr: [0; 8],
            irr: [0; 8],
            timer_start_ns: 0,
            timer_divisor: 2,
            timer_armed: false,
            timer_periodic: false,
            forced_zero: false,
            raised_periods: 0,
        }
    }

    pub fn id(&self) -> u32 {
        self.id
    }

    pub fn ldr(&self) -> u32 {
        self.ldr
    }

    pub fn dfr(&self) -> u32 {
        self.dfr
    }

    pub fn enabled(&self) -> bool {
        self.svr & (1 << 8) != 0
    }

    pub fn read32(&self, offset: u32, now_ns: u64) -> u32 {
        let offset = offset & 0xfff;
        match offset {
            0x20 => self.id << 24,
            0x30 => VERSION,
            0x80 => self.tpr,
            0xA0 => self.ppr() as u32,
            0xD0 => self.ldr,
            0xE0 => self.dfr,
            0xF0 => self.svr,
            0x100..=0x170 if offset & 0xf == 0 => self.isr[((offset - 0x100) / 0x10) as usize],
            0x180..=0x1F0 if offset & 0xf == 0 => self.tmr[((offset - 0x180) / 0x10) as usize],
            0x200..=0x270 if offset & 0xf == 0 => self.irr[((offset - 0x200) / 0x10) as usize],
            0x280 => self.esr,
            0x300 => self.icr_low,
            0x310 => self.icr_high,
            0x320 => self.lvt_timer,
            0x330 => self.lvt_thermal,
            0x340 => self.lvt_perf,
            0x350 => self.lvt_lint0,
            0x360 => self.lvt_lint1,
            0x370 => self.lvt_error,
            0x380 => self.initial,
            0x390 => self.current_count(now_ns),
            0x3E0 => self.dcr,
            _ => 0,
        }
    }

    pub fn write32(&mut self, offset: u32, value: u32, now_ns: u64) -> LapicEffect {
        let offset = offset & 0xfff;
        match offset {
            0x20 => self.id = (value >> 24) & 0xff,
            0x80 => self.tpr = value & 0xff,
            0xB0 => {
                if let Some(event) = self.eoi() {
                    return LapicEffect::Eoi(event);
                }
            }
            0xD0 => self.ldr = value & 0xff00_0000,
            0xE0 => self.dfr = value & 0xf000_0000 | 0x0fff_ffff,
            0xF0 => self.svr = value & 0x1ff,
            0x280 => self.esr = 0,
            0x300 => {
                self.icr_low = value & !(1 << 12);
                return LapicEffect::Icr;
            }
            0x310 => self.icr_high = value & 0xff00_0000,
            0x320 => {
                // TSC-deadline (bit 18) is not implemented.
                self.lvt_timer = value & !(1 << 18);
                self.timer_periodic = self.lvt_timer & (1 << 17) != 0;
            }
            0x330 => self.lvt_thermal = value,
            0x340 => self.lvt_perf = value,
            0x350 => self.lvt_lint0 = value,
            0x360 => self.lvt_lint1 = value,
            0x370 => self.lvt_error = value,
            0x380 => self.arm_timer(value, now_ns),
            0x3E0 => self.dcr = value & 0xb,
            _ => {}
        }
        LapicEffect::None
    }

    pub fn icr_low(&self) -> u32 {
        self.icr_low
    }

    pub fn icr_high(&self) -> u32 {
        self.icr_high
    }

    /// Who receives the ICR that was just written to this LAPIC.
    pub fn icr_targets(&self, low: u32, peers: &[Peer]) -> Vec<Ipi> {
        let mode = match ((low >> 8) & 7) as u8 {
            0 => DeliveryMode::Fixed,
            5 => DeliveryMode::Init,
            6 => DeliveryMode::Startup,
            other => DeliveryMode::Other(other),
        };
        // INIT level de-assert is not an IPI.
        if mode == DeliveryMode::Init && low & (1 << 15) != 0 && low & (1 << 14) == 0 {
            return Vec::new();
        }
        let vector = (low & 0xff) as u8;
        let shorthand = (low >> 18) & 3;
        let logical = low & (1 << 11) != 0;
        let dest_field = (self.icr_high >> 24) & 0xff;
        let dests: Vec<u32> = match shorthand {
            1 => vec![self.id],
            2 => peers.iter().map(|p| p.id).collect(),
            3 => peers
                .iter()
                .map(|p| p.id)
                .filter(|id| *id != self.id)
                .collect(),
            _ if !logical && dest_field == 0xff => peers.iter().map(|p| p.id).collect(),
            _ if !logical => vec![dest_field],
            _ => peers
                .iter()
                .filter(|p| logical_match(p.dfr, p.ldr, dest_field))
                .map(|p| p.id)
                .collect(),
        };
        dests
            .into_iter()
            .map(|dest| Ipi { dest, mode, vector })
            .collect()
    }

    pub fn raise(&mut self, vector: u8, level: bool) {
        if vector < 16 {
            return;
        }
        set_bit(&mut self.irr, vector);
        if level {
            set_bit(&mut self.tmr, vector);
        } else {
            clear_bit(&mut self.tmr, vector);
        }
    }

    pub fn highest_deliverable(&self) -> Option<u8> {
        if !self.enabled() {
            return None;
        }
        let ppr = self.ppr() & 0xf0;
        let vec = highest(&self.irr)?;
        if vec >= 16 && (vec & 0xf0) > ppr {
            Some(vec)
        } else {
            None
        }
    }

    pub fn accept(&mut self) -> Option<u8> {
        let vec = self.highest_deliverable()?;
        clear_bit(&mut self.irr, vec);
        set_bit(&mut self.isr, vec);
        Some(vec)
    }

    /// Undo [`accept`] when the injection is not actually entered.
    pub fn requeue(&mut self, vector: u8) {
        clear_bit(&mut self.isr, vector);
        set_bit(&mut self.irr, vector);
    }

    pub fn eoi(&mut self) -> Option<EoiEvent> {
        let vector = highest(&self.isr)?;
        clear_bit(&mut self.isr, vector);
        let level = bit(&self.tmr, vector);
        clear_bit(&mut self.tmr, vector);
        Some(EoiEvent { vector, level })
    }

    pub fn current_count(&self, now_ns: u64) -> u32 {
        if self.forced_zero || self.initial == 0 {
            return 0;
        }
        if !self.timer_armed && !self.timer_periodic {
            return 0;
        }
        let ticks = self.ticks(now_ns);
        let initial = self.initial as u64;
        if self.timer_periodic {
            let into = ticks % initial;
            (initial - into) as u32
        } else if ticks >= initial {
            0
        } else {
            (initial - ticks) as u32
        }
    }

    /// Nanoseconds until the armed, unmasked timer hits zero. `None` if it
    /// will not raise.
    pub fn remaining_ns(&self, now_ns: u64) -> Option<u64> {
        if !self.timer_armed || self.initial == 0 || self.lvt_timer & (1 << 16) != 0 {
            return None;
        }
        let ticks = self.ticks(now_ns);
        let initial = self.initial as u64;
        let left = if self.timer_periodic {
            let into = ticks % initial;
            if into == 0 { initial } else { initial - into }
        } else if ticks >= initial {
            return None;
        } else {
            initial - ticks
        };
        Some(left.saturating_mul(self.timer_divisor as u64))
    }

    /// Raise the timer IRQ if the programmed count has expired.
    pub fn poll(&mut self, now_ns: u64) {
        if !self.timer_armed || self.initial == 0 {
            return;
        }
        let ticks = self.ticks(now_ns);
        let initial = self.initial as u64;
        if self.timer_periodic {
            let periods = ticks / initial;
            if periods > self.raised_periods {
                self.raised_periods = periods;
                self.raise_timer();
            }
        } else if ticks >= initial {
            self.forced_zero = true;
            self.timer_armed = false;
            self.raise_timer();
        }
    }

    /// Preemption-timer exit, or the fallback thread's deadline. One-shot
    /// goes to zero even if our nanosecond clock is a tick behind.
    pub fn expire_timer(&mut self) {
        if !self.timer_armed {
            return;
        }
        if self.timer_periodic {
            self.timer_start_ns = self
                .timer_start_ns
                .saturating_add((self.initial as u64).saturating_mul(self.timer_divisor as u64));
            self.raised_periods = 0;
            self.raise_timer();
        } else {
            self.timer_armed = false;
            self.forced_zero = true;
            self.raise_timer();
        }
    }

    fn raise_timer(&mut self) {
        if self.lvt_timer & (1 << 16) != 0 || !self.enabled() {
            return;
        }
        let vector = (self.lvt_timer & 0xff) as u8;
        self.raise(vector, false);
    }

    fn arm_timer(&mut self, count: u32, now_ns: u64) {
        self.initial = count;
        self.timer_start_ns = now_ns;
        self.timer_divisor = decode_divisor(self.dcr);
        self.timer_periodic = self.lvt_timer & (1 << 17) != 0;
        self.timer_armed = count != 0;
        self.forced_zero = count == 0;
        self.raised_periods = 0;
    }

    fn ticks(&self, now_ns: u64) -> u64 {
        let divisor = (self.timer_divisor as u64).max(1);
        now_ns.saturating_sub(self.timer_start_ns) / divisor
    }

    fn ppr(&self) -> u8 {
        let tpr = (self.tpr & 0xff) as u8;
        let isr = highest(&self.isr).map(|v| v & 0xf0).unwrap_or(0);
        if (tpr & 0xf0) >= isr { tpr } else { isr }
    }
}

pub fn decode_divisor(dcr: u32) -> u32 {
    match dcr & 0xb {
        0x0 => 2,
        0x1 => 4,
        0x2 => 8,
        0x3 => 16,
        0x8 => 32,
        0x9 => 64,
        0xA => 128,
        _ => 1,
    }
}

pub fn logical_match(dfr: u32, ldr: u32, dest: u32) -> bool {
    let model = dfr >> 28;
    let ldr_log = (ldr >> 24) & 0xff;
    let dest = dest & 0xff;
    if model == 0xF {
        ldr_log & dest != 0
    } else {
        (ldr_log & 0xf0) == (dest & 0xf0) && (ldr_log & 0x0f) & (dest & 0x0f) != 0
    }
}

fn set_bit(bits: &mut [u32; 8], vector: u8) {
    let v = vector as usize;
    bits[v / 32] |= 1 << (v % 32);
}

fn clear_bit(bits: &mut [u32; 8], vector: u8) {
    let v = vector as usize;
    bits[v / 32] &= !(1 << (v % 32));
}

fn bit(bits: &[u32; 8], vector: u8) -> bool {
    let v = vector as usize;
    bits[v / 32] & (1 << (v % 32)) != 0
}

fn highest(bits: &[u32; 8]) -> Option<u8> {
    for (i, word) in bits.iter().copied().enumerate().rev() {
        if word != 0 {
            let bit = 31 - word.leading_zeros();
            return Some((i as u8) * 32 + bit as u8);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peers(n: u32) -> Vec<Peer> {
        (0..n)
            .map(|id| Peer {
                id,
                ldr: 0,
                dfr: 0xffff_ffff,
            })
            .collect()
    }

    #[test]
    fn icr_init_then_sipi_targets_apic_id() {
        let peers = peers(3);
        // Physical INIT, destination 1, edge.
        let init_low = 0x500u32;
        let mut sender = Lapic::new(0);
        sender.write32(0x310, 1 << 24, 0);
        assert!(matches!(
            sender.write32(0x300, init_low, 0),
            LapicEffect::Icr
        ));
        let init = sender.icr_targets(init_low, &peers);
        assert_eq!(
            init,
            vec![Ipi {
                dest: 1,
                mode: DeliveryMode::Init,
                vector: 0,
            }]
        );
        // SIPI vector 0x70 to the same AP. A second shorthand excludes self.
        let sipi_low = 0x600 | 0x70;
        sender.write32(0x310, 1 << 24, 0);
        let sipi = sender.icr_targets(sipi_low, &peers);
        assert_eq!(
            sipi,
            vec![Ipi {
                dest: 1,
                mode: DeliveryMode::Startup,
                vector: 0x70,
            }]
        );
        let all_but = sender.icr_targets(0x500 | (3 << 18), &peers);
        assert_eq!(
            all_but.iter().map(|ipi| ipi.dest).collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert!(all_but.iter().all(|ipi| ipi.mode == DeliveryMode::Init));
        // INIT level de-assert is not a startup IPI.
        assert!(sender.icr_targets(0x500 | (1 << 15), &peers).is_empty());
    }

    #[test]
    fn eoi_clears_highest_isr_and_reports_level() {
        let mut lapic = Lapic::new(0);
        assert!(matches!(lapic.write32(0xF0, 0x1FF, 0), LapicEffect::None));
        lapic.raise(0x30, false);
        lapic.raise(0x41, true);
        assert_eq!(lapic.accept(), Some(0x41));
        // Lower priority stays pending until the in-service vector is EOI'd.
        assert!(lapic.highest_deliverable().is_none());
        match lapic.write32(0xB0, 0, 0) {
            LapicEffect::Eoi(event) => {
                assert_eq!(event.vector, 0x41);
                assert!(event.level);
            }
            other => panic!("expected EOI, got {other:?}"),
        }
        assert_eq!(lapic.accept(), Some(0x30));
        match lapic.eoi() {
            Some(event) => {
                assert_eq!(event.vector, 0x30);
                assert!(!event.level);
            }
            None => panic!("second ISR bit still set"),
        }
        assert!(lapic.eoi().is_none());
    }

    #[test]
    fn one_shot_count_follows_elapsed_ns_and_divisor() {
        let mut lapic = Lapic::new(0);
        // Encoding 0b1011 is divisor 1.
        lapic.write32(0x3E0, 0xB, 0);
        assert_eq!(decode_divisor(0xB), 1);
        lapic.write32(0x380, 1000, 0);
        assert_eq!(lapic.current_count(0), 1000);
        assert_eq!(lapic.current_count(250), 750);
        assert_eq!(lapic.current_count(1000), 0);
        assert_eq!(lapic.current_count(1500), 0);
        assert_eq!(lapic.remaining_ns(250), None); // LVT timer resets masked

        // Divisor 2 (register 0): 500 ns is 250 ticks.
        let mut lapic = Lapic::new(1);
        lapic.write32(0x3E0, 0, 0);
        assert_eq!(decode_divisor(0), 2);
        lapic.write32(0x320, 0x40, 0); // vector 0x40, one-shot, unmasked
        lapic.write32(0xF0, 0x1FF, 0);
        lapic.write32(0x380, 1000, 1_000);
        assert_eq!(lapic.current_count(1_000), 1000);
        assert_eq!(lapic.current_count(1_000 + 500), 750);
        assert_eq!(lapic.remaining_ns(1_000 + 500), Some(1500));
        lapic.poll(1_000 + 500);
        assert!(lapic.highest_deliverable().is_none());
        lapic.poll(1_000 + 2000);
        assert_eq!(lapic.current_count(1_000 + 2000), 0);
        assert_eq!(lapic.accept(), Some(0x40));
    }
}
