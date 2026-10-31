//! One I/O APIC at `0xFEC00000`. 25 redirection entries, GSI base 0.
//! `set_level` stores one level per pin, so a line cannot be shared.

pub const IOAPIC_MMIO: u64 = 0xFEC0_0000;
pub const IOAPIC_MMIO_LEN: u64 = 0x1000;
const PINS: usize = 25;
const VERSION: u32 = 0x11 | ((PINS as u32 - 1) << 16);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IoapicMsg {
    pub vector: u8,
    pub dest: u32,
    pub logical: bool,
    pub level_triggered: bool,
}

pub struct IoApic {
    id: u32,
    index: u8,
    redir: [u64; PINS],
    pin_high: [bool; PINS],
}

impl IoApic {
    pub fn new() -> Self {
        Self {
            id: 0,
            index: 0,
            // Masked, edge, active high.
            redir: [1 << 16; PINS],
            pin_high: [false; PINS],
        }
    }

    pub fn read32(&self, offset: u32) -> u32 {
        match offset & 0xfff {
            0x00 => self.index as u32,
            0x10 => self.read_win(),
            _ => 0,
        }
    }

    pub fn write32(&mut self, offset: u32, value: u32) -> Vec<IoapicMsg> {
        match offset & 0xfff {
            0x00 => {
                self.index = value as u8;
                Vec::new()
            }
            0x10 => self.write_win(value),
            0x40 => self.eoi((value & 0xff) as u8),
            _ => Vec::new(),
        }
    }

    pub fn set_level(&mut self, gsi: u32, high: bool) -> Vec<IoapicMsg> {
        let Some(pin) = pin_index(gsi) else {
            return Vec::new();
        };
        let prev = self.pin_high[pin];
        self.pin_high[pin] = high;
        if self.level(pin) {
            self.service_level(pin).into_iter().collect()
        } else {
            let was = asserted(prev, self.active_low(pin));
            let now = asserted(high, self.active_low(pin));
            if !was && now {
                self.edge_msg(pin).into_iter().collect()
            } else {
                Vec::new()
            }
        }
    }

    pub fn eoi(&mut self, vector: u8) -> Vec<IoapicMsg> {
        let mut out = Vec::new();
        for pin in 0..PINS {
            if (self.redir[pin] as u8) == vector && self.level(pin) {
                self.redir[pin] &= !(1 << 14);
                if let Some(msg) = self.service_level(pin) {
                    out.push(msg);
                }
            }
        }
        out
    }

    fn read_win(&self) -> u32 {
        match self.index {
            0x00 => self.id << 24,
            0x01 => VERSION,
            0x02 => self.id << 24,
            idx if idx >= 0x10 => {
                let pin = ((idx - 0x10) / 2) as usize;
                if pin >= PINS {
                    return 0;
                }
                if (idx - 0x10) % 2 == 0 {
                    self.redir[pin] as u32
                } else {
                    (self.redir[pin] >> 32) as u32
                }
            }
            _ => 0,
        }
    }

    fn write_win(&mut self, value: u32) -> Vec<IoapicMsg> {
        match self.index {
            0x00 => {
                self.id = (value >> 24) & 0x0f;
                Vec::new()
            }
            idx if idx >= 0x10 => {
                let pin = ((idx - 0x10) / 2) as usize;
                if pin >= PINS {
                    return Vec::new();
                }
                if (idx - 0x10) % 2 == 0 {
                    let remote = self.redir[pin] & (1 << 14);
                    self.redir[pin] =
                        (self.redir[pin] & !0xffff_ffff) | (value as u64 & !(1 << 14)) | remote;
                } else {
                    self.redir[pin] = (self.redir[pin] & 0xffff_ffff) | ((value as u64) << 32);
                }
                self.service_level(pin).into_iter().collect()
            }
            _ => Vec::new(),
        }
    }

    fn service_level(&mut self, pin: usize) -> Option<IoapicMsg> {
        if !self.level(pin) || self.masked(pin) || self.redir[pin] & (1 << 14) != 0 {
            return None;
        }
        if !asserted(self.pin_high[pin], self.active_low(pin)) {
            return None;
        }
        if self.delivery(pin) != 0 {
            return None;
        }
        self.redir[pin] |= 1 << 14;
        Some(self.msg(pin, true))
    }

    fn edge_msg(&self, pin: usize) -> Option<IoapicMsg> {
        if self.masked(pin) || self.delivery(pin) != 0 {
            return None;
        }
        Some(self.msg(pin, false))
    }

    fn msg(&self, pin: usize, level_triggered: bool) -> IoapicMsg {
        IoapicMsg {
            vector: self.redir[pin] as u8,
            dest: ((self.redir[pin] >> 56) & 0xff) as u32,
            logical: self.redir[pin] & (1 << 11) != 0,
            level_triggered,
        }
    }

    fn masked(&self, pin: usize) -> bool {
        self.redir[pin] & (1 << 16) != 0
    }

    fn level(&self, pin: usize) -> bool {
        self.redir[pin] & (1 << 15) != 0
    }

    fn active_low(&self, pin: usize) -> bool {
        self.redir[pin] & (1 << 13) != 0
    }

    fn delivery(&self, pin: usize) -> u8 {
        ((self.redir[pin] >> 8) & 7) as u8
    }
}

impl Default for IoApic {
    fn default() -> Self {
        Self::new()
    }
}

fn pin_index(gsi: u32) -> Option<usize> {
    usize::try_from(gsi).ok().filter(|pin| *pin < PINS)
}

fn asserted(high: bool, active_low: bool) -> bool {
    if active_low { !high } else { high }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_gsi4_delivers_once_until_eoi() {
        let mut io = IoApic::new();
        assert!(io.set_level(4, true).is_empty());
        io.write32(0x00, 0x19);
        assert!(io.write32(0x10, 0).is_empty());
        io.write32(0x00, 0x18);
        let msgs = io.write32(0x10, 0x8030);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].vector, 0x30);
        assert!(msgs[0].level_triggered);
        assert!(io.set_level(4, true).is_empty());
        let again = io.eoi(0x30);
        assert_eq!(again.len(), 1);
        io.set_level(4, false);
        assert!(io.eoi(0x30).is_empty());
        io.write32(0x00, 0x01);
        assert_eq!(io.read32(0x10), VERSION);
    }

    #[test]
    fn gsi_24_is_not_an_alias_of_gsi_0() {
        let mut io = IoApic::new();
        io.set_level(24, true);
        io.write32(0x00, 0x10);
        assert!(io.write32(0x10, 0x20).is_empty());
        io.write32(0x00, 0x10 + 24 * 2);
        assert!(io.write32(0x10, 0x30).is_empty());
        assert!(io.set_level(24, false).is_empty());
        let msgs = io.set_level(24, true);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].vector, 0x30);
        assert!(io.set_level(0, true).iter().all(|msg| msg.vector != 0x30));
    }
}
