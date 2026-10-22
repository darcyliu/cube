//! PCI CAM at ports `0xCF8`/`0xCFC`, the PIIX4 PM block, and virtio-blk 1.0.
//! No ECAM and no MCFG. Host-bridge and PIIX BARs do not decode.
//! virtio-blk BAR0 does, inside the 32-bit PCI hole.

use std::time::Instant;

use super::virtio::block::VirtioBlock;
use super::virtio::transport::{PCI_HOLE_START, VIRTIO_BAR_SIZE, VirtioPciTransport};

pub use super::virtio::transport::PciIrq;

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
    blks: Vec<VirtioPciTransport>,
    pending_irqs: Vec<PciIrq>,
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
            blks: Vec::new(),
            pending_irqs: Vec::new(),
        }
    }

    /// First function is `00:02.0` (GSI 16). Later disks start at `00:04.0`
    /// so `00:03.0` stays free for a net function. At most seven: GSI 23 is
    /// the last I/O APIC pin.
    pub fn add_block(&mut self, block: VirtioBlock) -> Result<(), String> {
        let index = self.blks.len();
        let (dev, gsi) = virtio_blk_bdf(index)?;
        let bar = (PCI_HOLE_START as u32)
            .wrapping_add((index as u32).wrapping_mul(VIRTIO_BAR_SIZE as u32));
        self.blks
            .push(VirtioPciTransport::new(dev, gsi, bar, block));
        Ok(())
    }

    pub fn prt(&self) -> Vec<(u32, u32)> {
        self.blks.iter().map(VirtioPciTransport::prt).collect()
    }

    pub fn virtio_gsis(&self) -> Vec<u32> {
        self.blks.iter().map(VirtioPciTransport::gsi).collect()
    }

    pub fn take_intx(&mut self) -> Vec<PciIrq> {
        std::mem::take(&mut self.pending_irqs)
    }

    pub fn mmio_read(&mut self, gpa: u64, size: usize) -> Option<u64> {
        let (index, offset) = self.bar_hit(gpa, size)?;
        let (value, irqs) = self.blks[index].bar_read(offset, size);
        self.pending_irqs.extend(irqs);
        Some(value)
    }

    pub fn mmio_write(&mut self, gpa: u64, data: u64, size: usize) -> bool {
        let Some((index, offset)) = self.bar_hit(gpa, size) else {
            return false;
        };
        let irqs = self.blks[index].bar_write(offset, data, size);
        self.pending_irqs.extend(irqs);
        true
    }

    fn bar_hit(&self, gpa: u64, size: usize) -> Option<(usize, u64)> {
        if !matches!(size, 1 | 2 | 4 | 8) {
            return None;
        }
        let len = size as u64;
        for (index, blk) in self.blks.iter().enumerate() {
            let Some(base) = blk.mapped_base() else {
                continue;
            };
            let Some(offset) = gpa.checked_sub(base) else {
                continue;
            };
            if offset < VIRTIO_BAR_SIZE && len <= VIRTIO_BAR_SIZE - offset {
                return Some((index, offset));
            }
        }
        None
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
        let off = ((self.address & 0xfc) as usize) + (port - 0xCFC) as usize;
        if let Some(index) = self.addressed_blk() {
            return self.blks[index].cfg_read(off, size);
        }
        let Some(slot) = self.slot() else {
            return ones(size);
        };
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
        if let Some(index) = self.addressed_blk() {
            let irqs = self.blks[index].cfg_write(off, data, size);
            self.pending_irqs.extend(irqs);
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

    fn addressed_blk(&self) -> Option<usize> {
        let bus = ((self.address >> 16) & 0xff) as u8;
        let dev = ((self.address >> 11) & 0x1f) as u8;
        let func = ((self.address >> 8) & 7) as u8;
        if bus != 0 || func != 0 {
            return None;
        }
        self.blks.iter().position(|blk| blk.dev() == dev)
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

fn virtio_blk_bdf(index: usize) -> Result<(u8, u32), String> {
    // 00:03.0 / GSI 17 is reserved for virtio-net. Extra disks use 18+.
    if index >= 7 {
        return Err("too many virtio-blk devices".into());
    }
    if index == 0 {
        return Ok((2, 16));
    }
    Ok((3 + index as u8, 17 + index as u32))
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

    #[test]
    fn virtio_blk_bdfs_skip_the_net_slot() {
        assert_eq!(virtio_blk_bdf(0).unwrap(), (2, 16));
        assert_eq!(virtio_blk_bdf(1).unwrap(), (4, 18));
        assert_eq!(virtio_blk_bdf(2).unwrap(), (5, 19));
        assert!(virtio_blk_bdf(7).is_err());
    }

    #[test]
    fn virtio_pci_read_lands_in_guest_buffer() {
        use std::sync::Arc;

        use crate::devices::virtio::block::VirtioBlock;
        use crate::memory::GuestMemory;
        use crate::test_support::{TestDir, name};

        let dir = TestDir::new();
        let pattern: Vec<u8> = (0..512u16)
            .map(|i| (i.wrapping_mul(17) + 3) as u8)
            .collect();
        let path = dir.file("disk", &pattern);
        let mem = Arc::new(GuestMemory::from_buffer(0, &vec![0u8; 0x8000]).unwrap());
        let block = VirtioBlock::from_raw(name(&path), true, 0, "vtbd0", mem.clone()).unwrap();
        let mut pci = PciHost::new();
        pci.add_block(block).unwrap();

        assert_eq!(cfg_in(&mut pci, 0x0000, 4), 0x1237_8086);
        assert_eq!(cfg_in(&mut pci, 0x1000, 4), 0x1042_1af4);
        assert_eq!(cfg_in(&mut pci, 0x1008, 4), 0x0100_0001);
        assert_eq!(cfg_in(&mut pci, 0x102c, 4), 0x0002_1af4);
        assert_eq!((cfg_in(&mut pci, 0x103c, 4) >> 8) & 0xff, 1);
        assert_eq!((cfg_in(&mut pci, 0x1004, 4) >> 16) & 0x10, 0x10);
        // 00:03.0 stays empty. This host has no net function.
        assert_eq!(cfg_in(&mut pci, 0x1800, 4), 0xffff_ffff);

        let mut ptr = cfg_in(&mut pci, 0x1034, 1) as u8;
        let mut types = Vec::new();
        for _ in 0..8 {
            assert_ne!(ptr, 0);
            let hdr = cfg_in(&mut pci, 0x1000 | u32::from(ptr), 4);
            let id = (hdr & 0xff) as u8;
            let next = ((hdr >> 8) & 0xff) as u8;
            let cfg_type = ((hdr >> 24) & 0xff) as u8;
            assert_eq!(id, 0x09);
            types.push(cfg_type);
            if next == 0 {
                break;
            }
            ptr = next;
        }
        assert_eq!(types, vec![1, 2, 3, 4]);
        assert_eq!(cfg_in(&mut pci, 0x1060, 4), 0);

        cfg(&mut pci, 0x1010, 0xffff_ffff, 4);
        assert_eq!(cfg_in(&mut pci, 0x1010, 4), 0xffff_c000);
        cfg(&mut pci, 0x1010, 0xC000_0000, 4);
        cfg(&mut pci, 0x1004, 0x6, 2);

        let low = pci.mmio_read(0xC000_0004, 4).unwrap() as u32;
        let offered = (1 << 1) | (1 << 2) | (1 << 5) | (1 << 6) | (1 << 9) | (1 << 28);
        assert_eq!(low, offered);
        assert!(pci.mmio_write(0xC000_0000, 1, 4));
        assert_eq!(pci.mmio_read(0xC000_0004, 4).unwrap(), 1);
        assert!(pci.mmio_write(0xC000_0000, 0, 4));
        assert_eq!(pci.mmio_read(0xC000_3000, 8).unwrap(), 1);

        desc(&mem, 0, 0x4000, 16, 1, 1);
        desc(&mem, 1, 0x5000, 512, 1 | 2, 2);
        desc(&mem, 2, 0x6000, 1, 2, 0);
        mem.write_u32(0x4000, 0).unwrap();
        mem.write_u64(0x4008, 0).unwrap();
        mem.write_u16(0x2002, 1).unwrap();
        mem.write_u16(0x2004, 0).unwrap();
        mem.write(0x6000, &[0xff]).unwrap();

        let bar = 0xC000_0000u64;
        assert!(pci.mmio_write(bar + 0x14, 0x3, 1));
        assert!(pci.mmio_write(bar + 0x08, 0, 4));
        assert!(pci.mmio_write(bar + 0x0c, u64::from(offered), 4));
        assert!(pci.mmio_write(bar + 0x08, 1, 4));
        assert!(pci.mmio_write(bar + 0x0c, 1, 4));
        assert!(pci.mmio_write(bar + 0x14, 0xb, 1));
        assert!(pci.mmio_write(bar + 0x16, 0, 2));
        assert!(pci.mmio_write(bar + 0x18, 128, 2));
        assert!(pci.mmio_write(bar + 0x20, 0x1000, 4));
        assert!(pci.mmio_write(bar + 0x24, 0, 4));
        assert!(pci.mmio_write(bar + 0x28, 0x2000, 4));
        assert!(pci.mmio_write(bar + 0x2c, 0, 4));
        assert!(pci.mmio_write(bar + 0x30, 0x3000, 4));
        assert!(pci.mmio_write(bar + 0x34, 0, 4));
        assert!(pci.mmio_write(bar + 0x1c, 1, 2));
        assert!(pci.mmio_write(bar + 0x14, 0xf, 1));
        assert_eq!(pci.mmio_read(bar + 0x18, 2).unwrap(), 128);
        assert_eq!(pci.mmio_read(bar + 0x1c, 2).unwrap(), 1);
        let _ = pci.take_intx();

        assert!(pci.mmio_write(bar + 0x1000, 0, 2));
        let mut got = [0u8; 512];
        mem.read(0x5000, &mut got).unwrap();
        assert_eq!(got.as_slice(), pattern.as_slice());
        let mut status = [0u8; 1];
        mem.read(0x6000, &mut status).unwrap();
        assert_eq!(status[0], 0);
        assert_eq!(mem.read_u16(0x3002).unwrap(), 1);
        assert_eq!(mem.read_u32(0x3004).unwrap(), 0);
        assert_eq!(mem.read_u32(0x3008).unwrap(), 513);
        assert_eq!(
            pci.take_intx(),
            vec![PciIrq {
                gsi: 16,
                high: false
            }]
        );
        assert_eq!(pci.mmio_read(bar + 0x2000, 1).unwrap(), 1);
        assert_eq!(
            pci.take_intx(),
            vec![PciIrq {
                gsi: 16,
                high: true
            }]
        );
        assert_eq!(pci.mmio_read(bar + 0x2000, 1).unwrap(), 0);
        assert!(pci.take_intx().is_empty());
        assert_eq!(std::fs::read(path).unwrap(), pattern);
    }

    fn cfg(pci: &mut PciHost, addr: u32, data: u64, size: usize) {
        assert!(pci.try_out(0xCF8, u64::from(0x8000_0000 | addr), 4));
        assert!(pci.try_out(0xCFC, data, size));
    }

    fn cfg_in(pci: &mut PciHost, addr: u32, size: usize) -> u64 {
        assert!(pci.try_out(0xCF8, u64::from(0x8000_0000 | addr), 4));
        pci.try_in(0xCFC, size).unwrap()
    }

    fn desc(
        mem: &crate::memory::GuestMemory,
        idx: u64,
        addr: u64,
        len: u32,
        flags: u16,
        next: u16,
    ) {
        let at = 0x1000 + idx * 16;
        mem.write_u64(at, addr).unwrap();
        mem.write_u32(at + 8, len).unwrap();
        mem.write_u16(at + 12, flags).unwrap();
        mem.write_u16(at + 14, next).unwrap();
    }
}
