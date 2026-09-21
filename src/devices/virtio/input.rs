//! VirtIO Input (keyboard / absolute tablet) over Modern MMIO.
//!
//! VirtIO 1.1 §5.8 — device ID 18. Queues: eventq (0), statusq (1).
//! Events use the Linux evdev layout so FreeBSD `virtio_input` / Linux
//! `virtio-input` bind them as normal input devices for the VNC console.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::*;
use crate::devices::MmioDevice;
use crate::memory::GuestMemory;

const QUEUE_SIZE: u32 = 64;
pub const VIRTIO_DEV_INPUT: u32 = 18;

const CFG_ID_NAME: u8 = 0x01;
const CFG_ID_SERIAL: u8 = 0x02;
const CFG_ID_DEVIDS: u8 = 0x03;
const CFG_PROP_BITS: u8 = 0x10;
const CFG_EV_BITS: u8 = 0x11;
const CFG_ABS_INFO: u8 = 0x12;

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_ABS: u16 = 0x03;
pub const SYN_REPORT: u16 = 0;
pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const BTN_LEFT: u16 = 0x110;
pub const BTN_RIGHT: u16 = 0x111;
pub const BTN_MIDDLE: u16 = 0x112;
const INPUT_PROP_DIRECT: u16 = 0x01;

#[derive(Clone, Copy, Debug)]
pub struct InputEvent {
    pub type_: u16,
    pub code: u16,
    pub value: i32,
}

impl InputEvent {
    fn to_bytes(self) -> [u8; 8] {
        let mut b = [0u8; 8];
        b[0..2].copy_from_slice(&self.type_.to_le_bytes());
        b[2..4].copy_from_slice(&self.code.to_le_bytes());
        b[4..8].copy_from_slice(&self.value.to_le_bytes());
        b
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputKind {
    Keyboard,
    Tablet { abs_max_x: u16, abs_max_y: u16 },
}

pub struct VirtioInput {
    kind: InputKind,
    name: &'static str,
    status: u32,
    device_features_sel: u32,
    driver_features_sel: u32,
    driver_features: u64,
    eventq: VirtQueue,
    statusq: VirtQueue,
    queue_sel: u32,
    interrupt_status: u32,
    irq_num: u32,
    mem: Arc<GuestMemory>,
    pending: VecDeque<InputEvent>,
    cfg_select: u8,
    cfg_subsel: u8,
    cfg_size: u8,
    cfg_data: [u8; 128],
    /// Last time we printed a "flush deferred" line (host Instant as ns via… we use a counter).
    deferred_logs: u32,
}

impl VirtioInput {
    pub fn new_keyboard(irq_num: u32, mem: Arc<GuestMemory>) -> Self {
        Self::new(InputKind::Keyboard, "vmm-keyboard", irq_num, mem)
    }

    pub fn new_tablet(irq_num: u32, mem: Arc<GuestMemory>, abs_max_x: u16, abs_max_y: u16) -> Self {
        Self::new(
            InputKind::Tablet {
                abs_max_x,
                abs_max_y,
            },
            "vmm-tablet",
            irq_num,
            mem,
        )
    }

    fn new(kind: InputKind, name: &'static str, irq_num: u32, mem: Arc<GuestMemory>) -> Self {
        Self {
            kind,
            name,
            status: 0,
            device_features_sel: 0,
            driver_features_sel: 0,
            driver_features: 0,
            eventq: VirtQueue {
                max_size: QUEUE_SIZE,
                size: QUEUE_SIZE,
                ..Default::default()
            },
            statusq: VirtQueue {
                max_size: QUEUE_SIZE,
                size: QUEUE_SIZE,
                ..Default::default()
            },
            queue_sel: 0,
            interrupt_status: 0,
            irq_num,
            mem,
            pending: VecDeque::new(),
            cfg_select: 0,
            cfg_subsel: 0,
            cfg_size: 0,
            cfg_data: [0u8; 128],
            deferred_logs: 0,
        }
    }

    /// Try to drain any pending events (call from VNC poll loop).
    pub fn poll_flush(&mut self) {
        self.flush_events();
    }

    pub fn push_events(&mut self, events: &[InputEvent]) {
        const MAX_PENDING: usize = 256;
        for e in events {
            if self.pending.len() >= MAX_PENDING {
                self.pending.pop_front();
            }
            self.pending.push_back(*e);
        }
        self.flush_events();
    }

    pub fn key_event(&mut self, linux_keycode: u16, down: bool) {
        self.push_events(&[
            InputEvent {
                type_: EV_KEY,
                code: linux_keycode,
                value: if down { 1 } else { 0 },
            },
            InputEvent {
                type_: EV_SYN,
                code: SYN_REPORT,
                value: 0,
            },
        ]);
    }

    pub fn pointer_event(&mut self, x: u16, y: u16, buttons: u8) {
        let (max_x, max_y) = match self.kind {
            InputKind::Tablet {
                abs_max_x,
                abs_max_y,
            } => (abs_max_x, abs_max_y),
            InputKind::Keyboard => return,
        };
        // rate-limit logs: only when buttons change or first move would be noisy;
        // log buttoned events always

        let x = x.min(max_x);
        let y = y.min(max_y);
        self.push_events(&[
            InputEvent {
                type_: EV_ABS,
                code: ABS_X,
                value: x as i32,
            },
            InputEvent {
                type_: EV_ABS,
                code: ABS_Y,
                value: y as i32,
            },
            InputEvent {
                type_: EV_KEY,
                code: BTN_LEFT,
                value: i32::from(buttons & 1 != 0),
            },
            InputEvent {
                type_: EV_KEY,
                code: BTN_MIDDLE,
                value: i32::from(buttons & 2 != 0),
            },
            InputEvent {
                type_: EV_KEY,
                code: BTN_RIGHT,
                value: i32::from(buttons & 4 != 0),
            },
            InputEvent {
                type_: EV_SYN,
                code: SYN_REPORT,
                value: 0,
            },
        ]);
    }

    fn active_queue_mut(&mut self) -> Option<&mut VirtQueue> {
        match self.queue_sel {
            0 => Some(&mut self.eventq),
            1 => Some(&mut self.statusq),
            _ => None,
        }
    }

    fn active_queue(&self) -> Option<&VirtQueue> {
        match self.queue_sel {
            0 => Some(&self.eventq),
            1 => Some(&self.statusq),
            _ => None,
        }
    }

    fn raise_irq(&mut self) {
        self.interrupt_status |= 1;
        unsafe {
            crate::hypervisor::hv_gic_set_spi(self.irq_num, true);
        }
        crate::vcpu::kick_vcpu();
    }

    fn flush_events(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        if (self.status & VIRTIO_STATUS_DRIVER_OK) == 0 {
            if self.deferred_logs == 0 || self.deferred_logs % 64 == 0 {
                eprintln!(
                    "[input] {} flush deferred: status={:#x} (need DRIVER_OK), pending={} (guest never set DRIVER_OK — FreeBSD has no virtio_input driver)",
                    self.name, self.status, self.pending.len()
                );
            }
            self.deferred_logs = self.deferred_logs.wrapping_add(1);
            return;
        }
        if !self.eventq.ready || self.eventq.avail_ring == 0 || self.eventq.desc_table == 0 {
            if self.deferred_logs == 0 || self.deferred_logs % 64 == 0 {
                eprintln!(
                    "[input] {} flush deferred: eventq not ready (ready={} avail={:#x} desc={:#x}) pending={}",
                    self.name,
                    self.eventq.ready,
                    self.eventq.avail_ring,
                    self.eventq.desc_table,
                    self.pending.len()
                );
            }
            self.deferred_logs = self.deferred_logs.wrapping_add(1);
            return;
        }
        self.deferred_logs = 0;
        let q_size = self.eventq.size;
        if q_size == 0 {
            return;
        }

        let count = match self.eventq.pending_count(&self.mem) {
            Ok(n) => n,
            Err(_) => {
                self.status |= VIRTIO_STATUS_NEEDS_RESET;
                return;
            }
        };
        let mut processed_any = false;
        for _ in 0..count {
            if self.pending.is_empty() {
                break;
            }
            std::sync::atomic::fence(Ordering::SeqCst);
            let avail_idx = match self.mem.read_u16(self.eventq.avail_ring + 2) {
                Ok(i) => i,
                Err(_) => break,
            };
            if self.eventq.last_avail_idx == avail_idx {
                if self.deferred_logs == 0 || self.deferred_logs % 64 == 0 {
                    eprintln!(
                        "[input] {} no eventq buffers (avail.idx={} pending={})",
                        self.name,
                        avail_idx,
                        self.pending.len()
                    );
                }
                self.deferred_logs = self.deferred_logs.wrapping_add(1);
                break;
            }

            let head = match self.mem.read_u16(
                self.eventq.avail_ring
                    + 4
                    + 2 * (self.eventq.last_avail_idx as u64 % q_size as u64),
            ) {
                Ok(h) => h,
                Err(_) => break,
            };
            self.eventq.last_avail_idx = self.eventq.last_avail_idx.wrapping_add(1);

            let descriptors = match self.eventq.descriptors(
                &self.mem,
                head,
                self.driver_features & VIRTIO_RING_F_INDIRECT_DESC != 0,
                65536,
            ) {
                Ok(d) => d,
                Err(_) => {
                    self.status |= VIRTIO_STATUS_NEEDS_RESET;
                    break;
                }
            };
            let write_addr = descriptors
                .iter()
                .find(|d| d.flags & VRING_DESC_F_WRITE != 0 && d.len >= 8)
                .map(|d| (d.addr, d.len));

            let Some((addr, _len)) = write_addr else {
                publish_used(&self.mem, &mut self.eventq, head, 0);
                processed_any = true;
                continue;
            };

            let Some(ev) = self.pending.front().copied() else {
                break;
            };
            if self.mem.write(addr, &ev.to_bytes()).is_err() {
                eprintln!("[input] {} guest write failed GPA={:#x}", self.name, addr);
                break;
            }
            self.pending.pop_front();
            publish_used(&self.mem, &mut self.eventq, head, 8);
            processed_any = true;
            // delivered; keep quiet unless debugging
        }

        if processed_any && avail_wants_interrupt(&self.mem, self.eventq.avail_ring) {
            self.raise_irq();
        }
    }

    fn process_statusq(&mut self) {
        if !self.statusq.ready || self.statusq.avail_ring == 0 || self.statusq.desc_table == 0 {
            return;
        }
        let q_size = self.statusq.size;
        if q_size == 0 {
            return;
        }
        let count = match self.statusq.pending_count(&self.mem) {
            Ok(n) => n,
            Err(_) => {
                self.status |= VIRTIO_STATUS_NEEDS_RESET;
                return;
            }
        };
        let mut processed_any = false;
        for _ in 0..count {
            std::sync::atomic::fence(Ordering::SeqCst);
            let avail_idx = match self.mem.read_u16(self.statusq.avail_ring + 2) {
                Ok(i) => i,
                Err(_) => break,
            };
            if self.statusq.last_avail_idx == avail_idx {
                break;
            }
            let head = match self.mem.read_u16(
                self.statusq.avail_ring
                    + 4
                    + 2 * (self.statusq.last_avail_idx as u64 % q_size as u64),
            ) {
                Ok(h) => h,
                Err(_) => break,
            };
            self.statusq.last_avail_idx = self.statusq.last_avail_idx.wrapping_add(1);
            publish_used(&self.mem, &mut self.statusq, head, 0);
            processed_any = true;
        }
        if processed_any && avail_wants_interrupt(&self.mem, self.statusq.avail_ring) {
            self.raise_irq();
        }
    }

    fn refresh_config(&mut self) {
        self.cfg_data = [0u8; 128];
        self.cfg_size = 0;
        match self.cfg_select {
            CFG_ID_NAME => {
                let b = self.name.as_bytes();
                let n = b.len().min(127);
                self.cfg_data[..n].copy_from_slice(&b[..n]);
                self.cfg_size = n as u8;
            }
            CFG_ID_SERIAL => {
                self.cfg_data[..4].copy_from_slice(b"vmm0");
                self.cfg_size = 4;
            }
            CFG_ID_DEVIDS => {
                // bustype=BUS_VIRTUAL(0x06), vendor, product, version
                self.cfg_data[0..2].copy_from_slice(&0x06u16.to_le_bytes());
                self.cfg_data[2..4].copy_from_slice(&0x001fu16.to_le_bytes());
                let product: u16 = match self.kind {
                    InputKind::Keyboard => 1,
                    InputKind::Tablet { .. } => 2,
                };
                self.cfg_data[4..6].copy_from_slice(&product.to_le_bytes());
                self.cfg_data[6..8].copy_from_slice(&0x0001u16.to_le_bytes());
                self.cfg_size = 8;
            }
            CFG_PROP_BITS => {
                if matches!(self.kind, InputKind::Tablet { .. }) {
                    let bit = INPUT_PROP_DIRECT as usize;
                    self.cfg_data[bit / 8] |= 1 << (bit % 8);
                    self.cfg_size = (bit / 8 + 1) as u8;
                }
            }
            CFG_EV_BITS => {
                let sub = self.cfg_subsel as u16;
                match (self.kind, sub) {
                    (InputKind::Keyboard, EV_KEY) => {
                        for b in &mut self.cfg_data[..96] {
                            *b = 0xff;
                        }
                        self.cfg_size = 96;
                    }
                    (InputKind::Tablet { .. }, EV_KEY) => {
                        for code in [BTN_LEFT, BTN_RIGHT, BTN_MIDDLE] {
                            let c = code as usize;
                            self.cfg_data[c / 8] |= 1 << (c % 8);
                            self.cfg_size = self.cfg_size.max((c / 8 + 1) as u8);
                        }
                    }
                    (InputKind::Tablet { .. }, EV_ABS) => {
                        self.cfg_data[0] |= (1 << ABS_X) | (1 << ABS_Y);
                        self.cfg_size = 1;
                    }
                    (_, EV_SYN) => {
                        self.cfg_data[0] |= 1 << SYN_REPORT;
                        self.cfg_size = 1;
                    }
                    _ => {}
                }
            }
            CFG_ABS_INFO => {
                if let InputKind::Tablet {
                    abs_max_x,
                    abs_max_y,
                } = self.kind
                {
                    let max = match self.cfg_subsel as u16 {
                        ABS_X => abs_max_x as u32,
                        ABS_Y => abs_max_y as u32,
                        _ => return,
                    };
                    self.cfg_data[0..4].copy_from_slice(&0u32.to_le_bytes());
                    self.cfg_data[4..8].copy_from_slice(&max.to_le_bytes());
                    self.cfg_data[8..12].copy_from_slice(&0u32.to_le_bytes());
                    self.cfg_data[12..16].copy_from_slice(&0u32.to_le_bytes());
                    self.cfg_data[16..20].copy_from_slice(&0u32.to_le_bytes());
                    self.cfg_size = 20;
                }
            }
            _ => {}
        }
    }

    fn cfg_read(&self, offset: u64, size: usize) -> u64 {
        let mut buf = [0u8; 8];
        match offset {
            0 => {
                buf[0] = self.cfg_select;
                if size > 1 {
                    buf[1] = self.cfg_subsel;
                }
                if size > 2 {
                    buf[2] = self.cfg_size;
                }
            }
            1 => buf[0] = self.cfg_subsel,
            2 => buf[0] = self.cfg_size,
            o if (8..136).contains(&o) => {
                let start = (o - 8) as usize;
                for i in 0..size.min(8) {
                    if start + i < self.cfg_data.len() {
                        buf[i] = self.cfg_data[start + i];
                    }
                }
            }
            _ => {}
        }
        let mut val = 0u64;
        for i in 0..size.min(8) {
            val |= (buf[i] as u64) << (8 * i);
        }
        val
    }

    fn cfg_write(&mut self, offset: u64, data: u64, size: usize) {
        match offset {
            0 => {
                self.cfg_select = (data & 0xff) as u8;
                if size > 1 {
                    self.cfg_subsel = ((data >> 8) & 0xff) as u8;
                }
                self.refresh_config();
            }
            1 => {
                self.cfg_subsel = (data & 0xff) as u8;
                self.refresh_config();
            }
            _ => {}
        }
    }
}

impl MmioDevice for VirtioInput {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if offset >= VIRTIO_MMIO_CONFIG {
            return self.cfg_read(offset - VIRTIO_MMIO_CONFIG, size);
        }
        match offset {
            VIRTIO_MMIO_MAGIC_VALUE => VIRTIO_MMIO_MAGIC as u64,
            VIRTIO_MMIO_VERSION => VIRTIO_MMIO_VERSION_2 as u64,
            VIRTIO_MMIO_DEVICE_ID => VIRTIO_DEV_INPUT as u64,
            VIRTIO_MMIO_VENDOR_ID => VIRTIO_MMIO_VENDOR as u64,
            VIRTIO_MMIO_DEVICE_FEATURES => {
                if self.device_features_sel == 0 {
                    0
                } else if self.device_features_sel == 1 {
                    (VIRTIO_F_VERSION_1 >> 32) as u64
                } else {
                    0
                }
            }
            VIRTIO_MMIO_QUEUE_NUM_MAX => {
                self.active_queue().map(|q| q.max_size as u64).unwrap_or(0)
            }
            VIRTIO_MMIO_QUEUE_READY => self.active_queue().map(|q| u64::from(q.ready)).unwrap_or(0),
            VIRTIO_MMIO_INTERRUPT_STATUS => self.interrupt_status as u64,
            VIRTIO_MMIO_STATUS => self.status as u64,
            _ => 0,
        }
    }

    fn write(&mut self, offset: u64, data: u64, size: usize) {
        if offset >= VIRTIO_MMIO_CONFIG {
            self.cfg_write(offset - VIRTIO_MMIO_CONFIG, data, size);
            return;
        }
        match offset {
            VIRTIO_MMIO_DEVICE_FEATURES_SEL => self.device_features_sel = data as u32,
            VIRTIO_MMIO_DRIVER_FEATURES => {
                if self.driver_features_sel == 0 {
                    self.driver_features =
                        (self.driver_features & !0xffff_ffff) | (data & 0xffff_ffff);
                } else if self.driver_features_sel == 1 {
                    self.driver_features = (self.driver_features & 0xffff_ffff) | (data << 32);
                }
            }
            VIRTIO_MMIO_DRIVER_FEATURES_SEL => self.driver_features_sel = data as u32,
            VIRTIO_MMIO_QUEUE_SEL => self.queue_sel = data as u32,
            VIRTIO_MMIO_QUEUE_NUM => {
                if let Some(q) = self.active_queue_mut() {
                    q.size = (data as u32).min(q.max_size);
                }
            }
            VIRTIO_MMIO_QUEUE_READY => {
                if let Some(q) = self.active_queue_mut() {
                    q.ready = (data & 1) != 0;
                }
                self.flush_events();
            }
            VIRTIO_MMIO_QUEUE_NOTIFY => {
                if data == 0 {
                    self.flush_events();
                } else if data == 1 {
                    self.process_statusq();
                }
            }
            VIRTIO_MMIO_INTERRUPT_ACK => {
                self.interrupt_status &= !(data as u32);
                if self.interrupt_status == 0 {
                    unsafe {
                        crate::hypervisor::hv_gic_set_spi(self.irq_num, false);
                    }
                }
            }
            VIRTIO_MMIO_STATUS => {
                self.status = data as u32;
                if self.status == 0 {
                    self.eventq = VirtQueue {
                        max_size: QUEUE_SIZE,
                        size: QUEUE_SIZE,
                        ..Default::default()
                    };
                    self.statusq = VirtQueue {
                        max_size: QUEUE_SIZE,
                        size: QUEUE_SIZE,
                        ..Default::default()
                    };
                    self.pending.clear();
                    self.interrupt_status = 0;
                    unsafe {
                        crate::hypervisor::hv_gic_set_spi(self.irq_num, false);
                    }
                } else if (self.status & VIRTIO_STATUS_DRIVER_OK) != 0 {
                    self.flush_events();
                }
            }
            VIRTIO_MMIO_QUEUE_DESC_LOW => {
                if let Some(q) = self.active_queue_mut() {
                    q.desc_table = (q.desc_table & !0xffff_ffff) | (data & 0xffff_ffff);
                }
            }
            VIRTIO_MMIO_QUEUE_DESC_HIGH => {
                if let Some(q) = self.active_queue_mut() {
                    q.desc_table = (q.desc_table & 0xffff_ffff) | (data << 32);
                }
            }
            VIRTIO_MMIO_QUEUE_DRIVER_LOW => {
                if let Some(q) = self.active_queue_mut() {
                    q.avail_ring = (q.avail_ring & !0xffff_ffff) | (data & 0xffff_ffff);
                }
            }
            VIRTIO_MMIO_QUEUE_DRIVER_HIGH => {
                if let Some(q) = self.active_queue_mut() {
                    q.avail_ring = (q.avail_ring & 0xffff_ffff) | (data << 32);
                }
            }
            VIRTIO_MMIO_QUEUE_DEVICE_LOW => {
                if let Some(q) = self.active_queue_mut() {
                    q.used_ring = (q.used_ring & !0xffff_ffff) | (data & 0xffff_ffff);
                }
            }
            VIRTIO_MMIO_QUEUE_DEVICE_HIGH => {
                if let Some(q) = self.active_queue_mut() {
                    q.used_ring = (q.used_ring & 0xffff_ffff) | (data << 32);
                }
            }
            _ => {}
        }
    }
}
