//! PCI CAM at ports `0xCF8`/`0xCFC` and the PIIX4 PM block it enables.
//! No ECAM, no MCFG, and no BARs that decode.

use std::time::Instant;

/// PM timer frequency, 3.579545 MHz, low 24 bits.
pub const PM_TIMER_HZ: u64 = 3_579_545;

pub const PMBA_DEFAULT: u32 = 0xB000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PciAction {
    None,
    Shutdown,
    Reboot,
}

/// `(PMBA & ~1) + 8`. FADT `PM_TMR_BLK` uses this same function.
pub fn pm_timer_port(pmba: u32) -> u32 {
    (pmba & !1) + 8
}

pub fn pm_timer_ticks(now_ns: u64) -> u32 {
    ((now_ns as u128).saturating_mul(PM_TIMER_HZ as u128) / 1_000_000_000) as u32 & 0x00ff_ffff
}

/// SLP_TYP 0 with SLP_EN. SLP_EN is bit 13 and SLP_TYP is bits 12:10.
pub fn pm1_cnt_is_shutdown(value: u16) -> bool {
    value & (1 << 13) != 0 && (value >> 10) & 7 == 0
}

struct Slot {
    cfg: [u8; 256],
}

pub struct PciHost {
    address: u32,
    host: Slot,
    isa: Slot,
    pm: Slot,
    pmba: u32,
    pmiose: bool,
    pm1_sts: u16,
    pm1_en: u16,
    pm1_cnt: u16,
    cf9: u8,
    action: PciAction,
    clock: Instant,
}

impl PciHost {
    pub fn new() -> Self {
        let mut host = Slot { cfg: [0; 256] };
        put_id(&mut host, 0x8086, 0x1237, 0x06, 0x00, false);
        let mut isa = Slot { cfg: [0; 256] };
        // Function 0 of a multi-function device. Otherwise 00:01.3 is never scanned.
        put_id(&mut isa, 0x8086, 0x7000, 0x06, 0x01, true);
        let mut pm = Slot { cfg: [0; 256] };
        put_id(&mut pm, 0x8086, 0x7113, 0x06, 0x80, false);
        pm.cfg[0x40] = (PMBA_DEFAULT & 0xff) as u8;
        pm.cfg[0x41] = ((PMBA_DEFAULT >> 8) & 0xff) as u8;
        Self {
            address: 0,
            host,
            isa,
            pm,
            pmba: PMBA_DEFAULT,
            pmiose: false,
            pm1_sts: 0,
            pm1_en: 0,
            pm1_cnt: 0,
            cf9: 0,
            action: PciAction::None,
            clock: Instant::now(),
        }
    }

    pub fn pm_timer_port(&self) -> u32 {
        pm_timer_port(self.pmba)
    }

    pub fn pmiose(&self) -> bool {
        self.pmiose
    }

    pub fn take_action(&mut self) -> PciAction {
        let action = self.action;
        self.action = PciAction::None;
        action
    }

    pub fn try_in(&mut self, port: u16, size: usize) -> Option<u64> {
        if !matches!(size, 1 | 2 | 4) {
            return None;
        }
        if port == 0xCF9 && size == 1 {
            return Some(u64::from(self.cf9));
        }
        if config_span(port, size) {
            return Some(if port < 0xCFC {
                read_le(self.address, (port - 0xCF8) as usize, size)
            } else if self.address & (1 << 31) == 0 {
                ones(size)
            } else {
                self.config_read(port, size)
            });
        }
        if let Some(off) = self.pm_off(port, size) {
            return Some(self.pm_read(off, size));
        }
        if !self.pmiose && covers(port, size, 0xB000, 12) {
            return Some(ones(size));
        }
        None
    }

    pub fn try_out(&mut self, port: u16, data: u64, size: usize) -> bool {
        if !matches!(size, 1 | 2 | 4) {
            return false;
        }
        if port == 0xCF9 && size == 1 {
            self.cf9 = data as u8;
            // Bit 1 is the system reset (0x06 and 0x0E).
            if data & 0x02 != 0 {
                self.action = PciAction::Reboot;
            }
            return true;
        }
        if config_span(port, size) {
            if port < 0xCFC {
                self.address = write_le(self.address, (port - 0xCF8) as usize, data, size);
            } else if self.address & (1 << 31) != 0 {
                self.config_write(port, data, size);
            }
            return true;
        }
        if let Some(off) = self.pm_off(port, size) {
            self.pm_write(off, data, size);
            return true;
        }
        if !self.pmiose && covers(port, size, 0xB000, 12) {
            return true;
        }
        false
    }

    fn config_read(&self, port: u16, size: usize) -> u64 {
        let Some(slot) = self.slot() else {
            return ones(size);
        };
        let off = ((self.address & 0xfc) as usize) + (port - 0xCFC) as usize;
        read_le_bytes(&slot.cfg, off, size)
    }

    fn config_write(&mut self, port: u16, data: u64, size: usize) {
        let off = ((self.address & 0xfc) as usize) + (port - 0xCFC) as usize;
        let bus = ((self.address >> 16) & 0xff) as u8;
        let dev = ((self.address >> 11) & 0x1f) as u8;
        let func = ((self.address >> 8) & 7) as u8;
        if bus != 0 {
            return;
        }
        {
            let slot = match (dev, func) {
                (0, 0) => &mut self.host,
                (1, 0) => &mut self.isa,
                (1, 3) => &mut self.pm,
                _ => return,
            };
            write_bytes(&mut slot.cfg, off, data, size);
        }
        if dev == 1 && func == 3 {
            self.pmba = u32::from_le_bytes(self.pm.cfg[0x40..0x44].try_into().unwrap());
            self.pmiose = self.pm.cfg[0x80] & 1 != 0;
        }
    }

    fn slot(&self) -> Option<&Slot> {
        let bus = ((self.address >> 16) & 0xff) as u8;
        let dev = ((self.address >> 11) & 0x1f) as u8;
        let func = ((self.address >> 8) & 7) as u8;
        if bus != 0 {
            return None;
        }
        match (dev, func) {
            (0, 0) => Some(&self.host),
            (1, 0) => Some(&self.isa),
            (1, 3) => Some(&self.pm),
            _ => None,
        }
    }

    fn pm_off(&self, port: u16, size: usize) -> Option<usize> {
        if !self.pmiose {
            return None;
        }
        let base = (self.pmba & !1) as u16;
        if covers(port, size, base, 12) {
            Some((port - base) as usize)
        } else {
            None
        }
    }

    fn pm_read(&self, off: usize, size: usize) -> u64 {
        let mut raw = [0u8; 12];
        raw[0..2].copy_from_slice(&self.pm1_sts.to_le_bytes());
        raw[2..4].copy_from_slice(&self.pm1_en.to_le_bytes());
        raw[4..6].copy_from_slice(&self.pm1_cnt.to_le_bytes());
        let ticks = pm_timer_ticks(self.clock.elapsed().as_nanos() as u64);
        raw[8..12].copy_from_slice(&ticks.to_le_bytes());
        read_le_bytes(&raw, off, size)
    }

    fn pm_write(&mut self, off: usize, data: u64, size: usize) {
        for i in 0..size {
            let at = off + i;
            let byte = ((data >> (8 * i)) & 0xff) as u8;
            match at {
                0 | 1 => poke(&mut self.pm1_sts, at, byte),
                2 | 3 => poke(&mut self.pm1_en, at - 2, byte),
                4 | 5 => poke(&mut self.pm1_cnt, at - 4, byte),
                _ => {}
            }
        }
        if pm1_cnt_is_shutdown(self.pm1_cnt) {
            self.action = PciAction::Shutdown;
        }
    }
}

impl Default for PciHost {
    fn default() -> Self {
        Self::new()
    }
}

fn put_id(slot: &mut Slot, vendor: u16, device: u16, class: u8, subclass: u8, multi: bool) {
    slot.cfg[0..2].copy_from_slice(&vendor.to_le_bytes());
    slot.cfg[2..4].copy_from_slice(&device.to_le_bytes());
    slot.cfg[0x0b] = class;
    slot.cfg[0x0a] = subclass;
    if multi {
        slot.cfg[0x0e] = 0x80;
    }
}

fn config_span(port: u16, size: usize) -> bool {
    covers(port, size, 0xCF8, 8)
}

fn covers(port: u16, size: usize, base: u16, len: u16) -> bool {
    let end = port as u32 + size as u32;
    port >= base && end <= u32::from(base) + u32::from(len)
}

fn ones(size: usize) -> u64 {
    match size {
        1 => 0xff,
        2 => 0xffff,
        _ => 0xffff_ffff,
    }
}

fn read_le(value: u32, off: usize, size: usize) -> u64 {
    let raw = value.to_le_bytes();
    read_le_bytes(&raw, off, size)
}

fn read_le_bytes(raw: &[u8], off: usize, size: usize) -> u64 {
    let mut v = 0u64;
    for i in 0..size {
        v |= u64::from(raw[off + i]) << (8 * i);
    }
    v
}

fn write_le(value: u32, off: usize, data: u64, size: usize) -> u32 {
    let mut raw = value.to_le_bytes();
    write_bytes(&mut raw, off, data, size);
    u32::from_le_bytes(raw)
}

fn write_bytes(raw: &mut [u8], off: usize, data: u64, size: usize) {
    for i in 0..size {
        if off + i < raw.len() {
            raw[off + i] = ((data >> (8 * i)) & 0xff) as u8;
        }
    }
}

fn poke(reg: &mut u16, off: usize, byte: u8) {
    let mut raw = reg.to_le_bytes();
    raw[off] = byte;
    *reg = u16::from_le_bytes(raw);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pm_timer_port_follows_pmba_and_iose() {
        let mut pci = PciHost::new();
        assert_eq!(pm_timer_port(0xB000), 0xB008);
        assert_eq!(pm_timer_port(0xB001), 0xB008);
        assert_eq!(pci.pm_timer_port(), 0xB008);
        assert!(!pci.pmiose());
        assert_eq!(pci.try_in(0xB000, 1), Some(0xff));
        assert_eq!(pci.try_in(0xB008, 4), Some(0xffff_ffff));
        assert_eq!(pci.try_in(0xB00B, 1), Some(0xff));

        pci.try_out(0xCF8, 0x8000_0000, 4);
        assert_eq!(pci.try_in(0xCFC, 4), Some(0x1237_8086));
        pci.try_out(0xCF8, 0x8000_1000, 4);
        assert_eq!(pci.try_in(0xCFC, 4), Some(0xffff_ffff));

        // 00:01.0 stores a BAR and does not decode it.
        pci.try_out(0xCF8, 0x8000_0810, 4);
        pci.try_out(0xCFC, 0x1234_5678, 4);
        assert_eq!(pci.try_in(0xCFC, 4), Some(0x1234_5678));

        // 00:01.3 PMBA at 0x40, PMIOSE at 0x80 bit 0.
        pci.try_out(0xCF8, 0x8000_0B40, 4);
        pci.try_out(0xCFC, 0xB000, 4);
        pci.try_out(0xCF8, 0x8000_0B80, 4);
        pci.try_out(0xCFC, 1, 1);
        assert!(pci.pmiose());
        assert_eq!(pci.pm_timer_port(), 0xB008);
        let ticks = pci.try_in(0xB008, 4).unwrap();
        assert_eq!(ticks & 0xff00_0000, 0);
        assert_ne!(ticks, 0xffff_ffff);

        pci.try_out(0xB004, 0x2000, 2);
        assert_eq!(pci.take_action(), PciAction::Shutdown);
        assert!(pm1_cnt_is_shutdown(0x2000));
        assert!(!pm1_cnt_is_shutdown(0x2400));

        pci.try_out(0xCF9, 0x06, 1);
        assert_eq!(pci.take_action(), PciAction::Reboot);
    }
}
