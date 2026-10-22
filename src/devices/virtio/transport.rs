//! Non-transitional virtio-pci 1.0 transport. No MSI-X and no virtio-mmio
//! register file. INTx is active-low. Block is one queue, 9p is one queue,
//! and net is RX then TX.

use std::thread::JoinHandle;

use super::VirtQueue;
use super::block::VirtioBlock;
use super::net::{NetIrqHook, TxSubmit, VirtioNet};

/// Electrical level of an INTx pin. PCI INTx is active-low, so `high == false`
/// is the asserted state the I/O APIC delivers when the pin is programmed low.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciIrq {
    pub gsi: u32,
    pub high: bool,
}

pub(crate) const PCI_HOLE_START: u64 = 0xC000_0000;
pub(crate) const PCI_HOLE_END: u64 = 0xFEC0_0000;
pub(crate) const VIRTIO_BAR_SIZE: u64 = 0x4000;
/// Clear of the six virtio-blk defaults (`hole + n * 16 KiB`).
pub(crate) const NET_BAR: u32 = 0xC010_0000;

const CMD_MEM: u16 = 1 << 1;
const CMD_MASTER: u16 = 1 << 2;
const CMD_INTX_DISABLE: u16 = 1 << 10;
const BAR_MASK: u32 = !(VIRTIO_BAR_SIZE as u32 - 1);

const COMMON_LEN: u64 = 0x38;
const NOTIFY_OFF: u64 = 0x1000;
const ISR_OFF: u64 = 0x2000;
const DEVCFG_OFF: u64 = 0x3000;
const DEVCFG_LEN: u64 = 32;

pub struct PciIdent {
    pub device_id: u16,
    pub subsystem_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub config_len: u32,
}

/// One virtio function that shares the block/9p register file. Net stays on
/// its own path because TX returns a batch the vCPU sends after dropping PCI.
pub trait VirtioPciDev: Send {
    fn pci_ident(&self) -> PciIdent;
    fn queue(&self) -> &VirtQueue;
    fn queue_mut(&mut self) -> &mut VirtQueue;
    fn set_queue_ready(&mut self, ready: bool);
    fn device_feature_word(&self, select: u32) -> u32;
    fn driver_feature_word(&self, select: u32) -> u32;
    fn set_driver_feature_word(&mut self, select: u32, word: u32);
    fn status(&self) -> u32;
    fn set_status(&mut self, status: u32);
    fn read_config(&self, offset: usize, size: usize) -> u64;
    /// Returns whether the guest should see a queue interrupt (ISR bit 0).
    fn process_queue(&mut self) -> bool;
    /// ISR bit 1. Cleared when observed so a later read does not re-raise.
    fn consume_config_isr(&mut self) -> bool {
        false
    }
}

enum Dev {
    Block(VirtioBlock),
    Net(VirtioNet),
    P9(Box<dyn VirtioPciDev>),
}

impl Dev {
    fn num_queues(&self) -> u16 {
        match self {
            Dev::Block(_) | Dev::P9(_) => 1,
            Dev::Net(_) => 2,
        }
    }

    fn device_feature_word(&self, select: u32) -> u32 {
        match self {
            Dev::Block(dev) => dev.device_feature_word(select),
            Dev::Net(dev) => dev.device_feature_word(select),
            Dev::P9(dev) => dev.device_feature_word(select),
        }
    }

    fn driver_feature_word(&self, select: u32) -> u32 {
        match self {
            Dev::Block(dev) => dev.driver_feature_word(select),
            Dev::Net(dev) => dev.driver_feature_word(select),
            Dev::P9(dev) => dev.driver_feature_word(select),
        }
    }

    fn set_driver_feature_word(&mut self, select: u32, word: u32) {
        match self {
            Dev::Block(dev) => dev.set_driver_feature_word(select, word),
            Dev::Net(dev) => dev.set_driver_feature_word(select, word),
            Dev::P9(dev) => dev.set_driver_feature_word(select, word),
        }
    }

    fn poke_driver_feature(&mut self, select: u32, byte_off: usize, byte: u8) {
        let mut word = self.driver_feature_word(select);
        poke_u32(&mut word, byte_off, byte);
        self.set_driver_feature_word(select, word);
    }

    fn status(&self) -> u32 {
        match self {
            Dev::Block(dev) => dev.status(),
            Dev::Net(dev) => dev.status(),
            Dev::P9(dev) => dev.status(),
        }
    }

    fn set_status(&mut self, status: u32) {
        match self {
            Dev::Block(dev) => dev.set_status(status),
            Dev::Net(dev) => dev.set_status(status),
            Dev::P9(dev) => dev.set_status(status),
        }
    }

    fn read_config(&self, offset: usize, size: usize) -> u64 {
        match self {
            Dev::Block(dev) => dev.read_config(offset, size),
            Dev::Net(dev) => dev.read_config(offset, size),
            Dev::P9(dev) => dev.read_config(offset, size),
        }
    }

    fn queue(&self, sel: u16) -> Option<VirtQueue> {
        match self {
            Dev::Block(dev) if sel == 0 => Some(dev.queue),
            Dev::P9(dev) if sel == 0 => Some(*dev.queue()),
            Dev::Net(dev) => dev.queue(sel),
            _ => None,
        }
    }

    fn update_queue(&mut self, sel: u16, update: impl FnOnce(&mut VirtQueue)) {
        match self {
            Dev::Block(dev) if sel == 0 => update(&mut dev.queue),
            Dev::P9(dev) if sel == 0 => update(dev.queue_mut()),
            Dev::Net(dev) => dev.update_queue(sel, update),
            _ => {}
        }
    }

    fn set_ready_low(&mut self, sel: u16, byte: u8) {
        let ready = byte & 1 == 1;
        match self {
            Dev::Block(dev) if sel == 0 => dev.queue.ready = ready,
            Dev::P9(dev) if sel == 0 => dev.set_queue_ready(ready),
            Dev::Net(dev) => dev.set_ready_low(sel, ready),
            _ => {}
        }
    }

    fn set_bus_master(&self, enabled: bool) {
        if let Dev::Net(dev) = self {
            dev.set_bus_master(enabled);
        }
    }

    fn notify(&mut self, queue: u16) -> (bool, Option<TxSubmit>) {
        match self {
            Dev::Block(dev) => (queue == 0 && dev.process_queue(), None),
            Dev::P9(dev) => (queue == 0 && dev.process_queue(), None),
            Dev::Net(dev) => dev.notify(queue),
        }
    }

    fn take_irq_pending(&self) -> bool {
        match self {
            Dev::Block(_) | Dev::P9(_) => false,
            Dev::Net(dev) => dev.take_irq_pending(),
        }
    }

    fn consume_config_isr(&mut self) -> bool {
        match self {
            Dev::P9(dev) => dev.consume_config_isr(),
            Dev::Block(_) | Dev::Net(_) => false,
        }
    }
}

pub(crate) struct VirtioPciTransport {
    dev_no: u8,
    gsi: u32,
    command: u16,
    bar_raw: u32,
    isr: u8,
    dev_feat_sel: u32,
    drv_feat_sel: u32,
    queue_sel: u16,
    interrupt_line: u8,
    cache_line: u8,
    latency: u8,
    config_len: u64,
    template: [u8; 256],
    dev: Dev,
}

impl VirtioPciTransport {
    pub(crate) fn new(dev: u8, gsi: u32, bar: u32, block: VirtioBlock) -> Self {
        Self::build(
            dev,
            gsi,
            bar,
            0x1042,
            0x01,
            0x00,
            2,
            DEVCFG_LEN as u32,
            Dev::Block(block),
        )
    }

    pub(crate) fn new_net(dev: u8, gsi: u32, bar: u32, net: VirtioNet) -> Self {
        Self::build(
            dev,
            gsi,
            bar,
            0x1041,
            0x02,
            0x00,
            1,
            DEVCFG_LEN as u32,
            Dev::Net(net),
        )
    }

    pub(crate) fn new_p9(dev: u8, gsi: u32, bar: u32, device: Box<dyn VirtioPciDev>) -> Self {
        let ident = device.pci_ident();
        Self::build(
            dev,
            gsi,
            bar,
            ident.device_id,
            ident.class,
            ident.subclass,
            ident.subsystem_id,
            ident.config_len,
            Dev::P9(device),
        )
    }

    fn build(
        dev: u8,
        gsi: u32,
        bar: u32,
        device_id: u16,
        class: u8,
        subclass: u8,
        subsystem: u16,
        config_len: u32,
        backend: Dev,
    ) -> Self {
        Self {
            dev_no: dev,
            gsi,
            command: 0,
            bar_raw: bar,
            isr: 0,
            dev_feat_sel: 0,
            drv_feat_sel: 0,
            queue_sel: 0,
            interrupt_line: 0,
            cache_line: 0,
            latency: 0,
            config_len: u64::from(config_len),
            template: config_template(device_id, class, subclass, subsystem, config_len),
            dev: backend,
        }
    }

    pub(crate) fn dev(&self) -> u8 {
        self.dev_no
    }

    pub(crate) fn gsi(&self) -> u32 {
        self.gsi
    }

    pub(crate) fn prt(&self) -> (u32, u32) {
        ((u32::from(self.dev_no) << 16), self.gsi)
    }

    /// Memory decode is off until the guest sets the command bit. The address
    /// has to sit in the 32-bit PCI hole; a BAR over RAM is not claimed here.
    pub(crate) fn mapped_base(&self) -> Option<u64> {
        if self.command & CMD_MEM == 0 {
            return None;
        }
        let base = u64::from(self.bar_raw & BAR_MASK);
        let end = base.checked_add(VIRTIO_BAR_SIZE)?;
        if base < PCI_HOLE_START || end > PCI_HOLE_END {
            return None;
        }
        Some(base)
    }

    pub(crate) fn cfg_read(&self, off: usize, size: usize) -> u64 {
        if size == 0 || off >= 256 || size > 256 - off {
            return 0;
        }
        let cfg = self.cfg_image();
        read_bytes(&cfg, off, size)
    }

    pub(crate) fn cfg_write(&mut self, off: usize, data: u64, size: usize) -> Vec<PciIrq> {
        let before = self.electrical_high();
        for i in 0..size {
            let at = off + i;
            if at >= 256 {
                break;
            }
            let byte = ((data >> (8 * i)) & 0xff) as u8;
            match at {
                0x04 | 0x05 => {
                    let mut raw = self.command.to_le_bytes();
                    raw[at - 0x04] = byte;
                    self.command = u16::from_le_bytes(raw);
                }
                0x0c => self.cache_line = byte,
                0x0d => self.latency = byte,
                0x10..=0x13 => {
                    let mut raw = self.bar_raw.to_le_bytes();
                    raw[at - 0x10] = byte;
                    self.bar_raw = u32::from_le_bytes(raw);
                }
                0x3c => self.interrupt_line = byte,
                _ => {}
            }
        }
        // Publish before any later queue walk. The RX thread reads this
        // without taking PciHost.
        self.publish_bus_master();
        self.irq_delta(before)
    }

    pub(crate) fn bar_read(&mut self, offset: u64, size: usize) -> (u64, Vec<PciIrq>) {
        let before = self.electrical_high();
        let value = if fits(offset, size, 0, COMMON_LEN) {
            self.read_common(offset as usize, size)
        } else if offset == ISR_OFF {
            let isr = u64::from(self.isr);
            self.isr = 0;
            isr
        } else if fits(offset, size, DEVCFG_OFF, self.config_len) {
            self.dev.read_config((offset - DEVCFG_OFF) as usize, size)
        } else {
            0
        };
        (value, self.irq_delta(before))
    }

    pub(crate) fn bar_write(
        &mut self,
        offset: u64,
        data: u64,
        size: usize,
    ) -> (Vec<PciIrq>, Option<TxSubmit>) {
        // Ready and notify both lock the queue. The bus-master flag has to
        // move first so the RX thread observes the new command bit.
        self.publish_bus_master();
        let before = self.electrical_high();
        let mut tx = None;
        if fits(offset, size, 0, COMMON_LEN) {
            self.write_common(offset as usize, data, size);
        } else if offset == NOTIFY_OFF && matches!(size, 1 | 2 | 4) {
            // Bus-master clear: drop the notify and do not touch guest memory.
            if self.command & CMD_MASTER != 0 {
                let (raise, batch) = self.dev.notify(data as u16);
                if raise {
                    self.isr |= 1;
                }
                tx = batch;
            }
        }
        // QUEUE_READY can publish RX buffers without a notify. Fold that into
        // the same ISR the vCPU path already owns, so the RX hook is not called
        // while this lock is held.
        if self.dev.take_irq_pending() {
            self.isr |= 1;
        }
        if self.dev.consume_config_isr() {
            self.isr |= 2;
        }
        (self.irq_delta(before), tx)
    }

    fn publish_bus_master(&self) {
        self.dev.set_bus_master(self.command & CMD_MASTER != 0);
    }

    pub(crate) fn set_irq_hook(&self, hook: NetIrqHook) {
        if let Dev::Net(net) = &self.dev {
            net.set_irq_hook(hook);
        }
    }

    /// RX publishes used buffers off the vCPU thread. Fold that into ISR here,
    /// under the same lock as the guest's ISR read.
    pub(crate) fn latch_irq(&mut self) -> Vec<PciIrq> {
        let Dev::Net(net) = &self.dev else {
            return Vec::new();
        };
        if !net.take_irq_pending() {
            return Vec::new();
        }
        let before = self.electrical_high();
        self.isr |= 1;
        self.irq_delta(before)
    }

    pub(crate) fn take_worker(&mut self) -> Option<JoinHandle<()>> {
        match &mut self.dev {
            Dev::Net(net) => net.take_worker(),
            Dev::Block(_) | Dev::P9(_) => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn inject_rx(&mut self, frame: &[u8]) -> Vec<PciIrq> {
        let Dev::Net(net) = &self.dev else {
            return Vec::new();
        };
        if !net.inject_rx(frame) {
            return Vec::new();
        }
        let before = self.electrical_high();
        self.isr |= 1;
        self.irq_delta(before)
    }

    fn electrical_high(&self) -> bool {
        self.isr == 0 || self.command & CMD_INTX_DISABLE != 0
    }

    fn irq_delta(&self, before: bool) -> Vec<PciIrq> {
        let after = self.electrical_high();
        if before == after {
            Vec::new()
        } else {
            vec![PciIrq {
                gsi: self.gsi,
                high: after,
            }]
        }
    }

    fn cfg_image(&self) -> [u8; 256] {
        let mut cfg = self.template;
        cfg[0x04..0x06].copy_from_slice(&self.command.to_le_bytes());
        let mut status = 0x0010u16;
        // Interrupt Disable masks the pin only. Status bit 3 stays with the ISR.
        if self.isr != 0 {
            status |= 1 << 3;
        }
        cfg[0x06..0x08].copy_from_slice(&status.to_le_bytes());
        cfg[0x0c] = self.cache_line;
        cfg[0x0d] = self.latency;
        let bar = self.bar_raw & BAR_MASK;
        cfg[0x10..0x14].copy_from_slice(&bar.to_le_bytes());
        cfg[0x3c] = self.interrupt_line;
        cfg
    }

    fn read_common(&self, offset: usize, size: usize) -> u64 {
        let raw = self.common_bytes();
        read_bytes(&raw, offset, size)
    }

    fn common_bytes(&self) -> [u8; COMMON_LEN as usize] {
        let mut raw = [0u8; COMMON_LEN as usize];
        raw[0..4].copy_from_slice(&self.dev_feat_sel.to_le_bytes());
        raw[4..8].copy_from_slice(
            &self
                .dev
                .device_feature_word(self.dev_feat_sel)
                .to_le_bytes(),
        );
        raw[8..12].copy_from_slice(&self.drv_feat_sel.to_le_bytes());
        raw[12..16].copy_from_slice(
            &self
                .dev
                .driver_feature_word(self.drv_feat_sel)
                .to_le_bytes(),
        );
        raw[0x10..0x12].copy_from_slice(&0xffffu16.to_le_bytes());
        raw[0x12..0x14].copy_from_slice(&self.dev.num_queues().to_le_bytes());
        raw[0x14] = self.dev.status() as u8;
        raw[0x16..0x18].copy_from_slice(&self.queue_sel.to_le_bytes());
        raw[0x1a..0x1c].copy_from_slice(&0xffffu16.to_le_bytes());
        if let Some(queue) = self.dev.queue(self.queue_sel) {
            raw[0x18..0x1a].copy_from_slice(&(queue.size as u16).to_le_bytes());
            let enable: u16 = u16::from(queue.ready);
            raw[0x1c..0x1e].copy_from_slice(&enable.to_le_bytes());
            raw[0x20..0x28].copy_from_slice(&queue.desc_table.to_le_bytes());
            raw[0x28..0x30].copy_from_slice(&queue.avail_ring.to_le_bytes());
            raw[0x30..0x38].copy_from_slice(&queue.used_ring.to_le_bytes());
        }
        raw
    }

    fn write_common(&mut self, offset: usize, data: u64, size: usize) {
        for i in 0..size {
            self.write_common_byte(offset + i, ((data >> (8 * i)) & 0xff) as u8);
        }
    }

    fn write_common_byte(&mut self, offset: usize, byte: u8) {
        match offset {
            0x00..=0x03 => poke_u32(&mut self.dev_feat_sel, offset, byte),
            0x08..=0x0b => poke_u32(&mut self.drv_feat_sel, offset - 0x08, byte),
            0x0c..=0x0f => self
                .dev
                .poke_driver_feature(self.drv_feat_sel, offset - 0x0c, byte),
            0x14 => self.set_status_byte(byte),
            0x16..=0x17 => poke_u16(&mut self.queue_sel, offset - 0x16, byte),
            0x18..=0x19 => {
                let sel = self.queue_sel;
                self.dev.update_queue(sel, |queue| {
                    let mut size = queue.size as u16;
                    poke_u16(&mut size, offset - 0x18, byte);
                    queue.size = u32::from(size).min(queue.max_size);
                });
            }
            0x1c => self.dev.set_ready_low(self.queue_sel, byte),
            0x20..=0x27 => {
                let sel = self.queue_sel;
                self.dev.update_queue(sel, |queue| {
                    poke_u64(&mut queue.desc_table, offset - 0x20, byte);
                });
            }
            0x28..=0x2f => {
                let sel = self.queue_sel;
                self.dev.update_queue(sel, |queue| {
                    poke_u64(&mut queue.avail_ring, offset - 0x28, byte);
                });
            }
            0x30..=0x37 => {
                let sel = self.queue_sel;
                self.dev.update_queue(sel, |queue| {
                    poke_u64(&mut queue.used_ring, offset - 0x30, byte);
                });
            }
            _ => {}
        }
    }

    fn set_status_byte(&mut self, byte: u8) {
        self.dev.set_status(u32::from(byte));
        if byte == 0 {
            self.dev_feat_sel = 0;
            self.drv_feat_sel = 0;
            self.queue_sel = 0;
            self.isr = 0;
        }
    }
}

fn fits(offset: u64, size: usize, base: u64, len: u64) -> bool {
    offset >= base && offset + size as u64 <= base + len
}

fn read_bytes(raw: &[u8], off: usize, size: usize) -> u64 {
    let mut value = 0u64;
    for i in 0..size {
        value |= u64::from(raw[off + i]) << (8 * i);
    }
    value
}

fn poke_u32(reg: &mut u32, off: usize, byte: u8) {
    let mut raw = reg.to_le_bytes();
    raw[off] = byte;
    *reg = u32::from_le_bytes(raw);
}

fn poke_u16(reg: &mut u16, off: usize, byte: u8) {
    let mut raw = reg.to_le_bytes();
    raw[off] = byte;
    *reg = u16::from_le_bytes(raw);
}

fn poke_u64(reg: &mut u64, off: usize, byte: u8) {
    let mut raw = reg.to_le_bytes();
    raw[off] = byte;
    *reg = u64::from_le_bytes(raw);
}

fn config_template(
    device_id: u16,
    class: u8,
    subclass: u8,
    subsystem: u16,
    config_len: u32,
) -> [u8; 256] {
    let mut cfg = [0u8; 256];
    cfg[0..2].copy_from_slice(&0x1AF4u16.to_le_bytes());
    cfg[2..4].copy_from_slice(&device_id.to_le_bytes());
    cfg[0x06] = 0x10; // capabilities list
    cfg[0x08] = 1; // revision
    cfg[0x0a] = subclass;
    cfg[0x0b] = class;
    cfg[0x2c..0x2e].copy_from_slice(&0x1AF4u16.to_le_bytes());
    cfg[0x2e..0x30].copy_from_slice(&subsystem.to_le_bytes());
    cfg[0x34] = 0x40;
    cfg[0x3d] = 1; // INTA
    // Vendor-specific caps only. No MSI (0x05) and no MSI-X (0x11).
    write_cap(&mut cfg, 0x40, 0x50, 16, 1, 0x0000, COMMON_LEN as u32, None);
    write_cap(&mut cfg, 0x50, 0x64, 20, 2, NOTIFY_OFF as u32, 2, Some(0));
    write_cap(&mut cfg, 0x64, 0x74, 16, 3, ISR_OFF as u32, 1, None);
    write_cap(
        &mut cfg,
        0x74,
        0x00,
        16,
        4,
        DEVCFG_OFF as u32,
        config_len,
        None,
    );
    cfg
}

#[allow(clippy::too_many_arguments)]
fn write_cap(
    cfg: &mut [u8],
    at: usize,
    next: u8,
    len: u8,
    cfg_type: u8,
    offset: u32,
    length: u32,
    multiplier: Option<u32>,
) {
    cfg[at] = 0x09;
    cfg[at + 1] = next;
    cfg[at + 2] = len;
    cfg[at + 3] = cfg_type;
    cfg[at + 8..at + 12].copy_from_slice(&offset.to_le_bytes());
    cfg[at + 12..at + 16].copy_from_slice(&length.to_le_bytes());
    if let Some(multiplier) = multiplier {
        cfg[at + 16..at + 20].copy_from_slice(&multiplier.to_le_bytes());
    }
}
