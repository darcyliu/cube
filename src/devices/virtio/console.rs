//! VirtIO Console (`virtio-console`) device over Modern MMIO.
//!
//! Conforms to VirtIO 1.0/1.1 Specification, Section 5.3 (Console Device).
//! Supports `VIRTIO_CONSOLE_F_SIZE` (bit 0) for dynamic terminal window geometry
//! synchronization, forwarding host SIGWINCH events into the guest kernel via
//! VirtIO configuration change interrupts.

use std::io::{self, Write};
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::*;
use crate::devices::MmioDevice;
use crate::memory::GuestMemory;

const VTCON_QUEUE_SIZE: u32 = 64;
const VIRTIO_CONSOLE_F_SIZE: u64 = 1 << 0;

pub struct VirtioConsole {
    status: u32,
    device_features_sel: u32,
    driver_features_sel: u32,
    driver_features: u64,
    rx_queue: VirtQueue,
    tx_queue: VirtQueue,
    queue_sel: u32,
    interrupt_status: u32,
    irq_num: u32,
    mem: Arc<GuestMemory>,
    cols: u16,
    rows: u16,
}

impl VirtioConsole {
    pub fn new(irq_num: u32, mem: Arc<GuestMemory>, cols: u16, rows: u16) -> Self {
        Self {
            status: 0,
            device_features_sel: 0,
            driver_features_sel: 0,
            driver_features: 0,
            rx_queue: VirtQueue {
                max_size: VTCON_QUEUE_SIZE,
                size: VTCON_QUEUE_SIZE,
                ..Default::default()
            },
            tx_queue: VirtQueue {
                max_size: VTCON_QUEUE_SIZE,
                size: VTCON_QUEUE_SIZE,
                ..Default::default()
            },
            queue_sel: 0,
            interrupt_status: 0,
            irq_num,
            mem,
            cols,
            rows,
        }
    }

    /// Dynamically update terminal dimensions and trigger a VirtIO Configuration Change Interrupt.
    pub fn update_size(&mut self, cols: u16, rows: u16) {
        if self.cols == cols && self.rows == rows {
            return;
        }
        self.cols = cols;
        self.rows = rows;

        // Bit 1: Configuration Change Notification (VirtIO MMIO Section 4.2.2.2)
        self.interrupt_status |= 2;

        // Raise SPI interrupt if guest driver is active (DRIVER_OK = 0x4)
        if (self.status & 4) != 0 {
            unsafe {
                crate::hypervisor::hv_gic_set_spi(self.irq_num, true);
            }
            crate::vcpu::kick_vcpu();
        }
    }

    pub fn get_size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    fn active_queue_mut(&mut self) -> Option<&mut VirtQueue> {
        match self.queue_sel {
            0 => Some(&mut self.rx_queue),
            1 => Some(&mut self.tx_queue),
            _ => None,
        }
    }

    fn active_queue(&self) -> Option<&VirtQueue> {
        match self.queue_sel {
            0 => Some(&self.rx_queue),
            1 => Some(&self.tx_queue),
            _ => None,
        }
    }

    /// Process outgoing guest console data on transmitq (queue 1).
    fn process_tx_queue(&mut self) {
        if !self.tx_queue.ready || self.tx_queue.avail_ring == 0 || self.tx_queue.desc_table == 0 {
            return;
        }

        let q_size = self.tx_queue.size;
        if q_size == 0 {
            return;
        }

        let mut processed_any = false;
        loop {
            std::sync::atomic::fence(Ordering::SeqCst);
            let avail_idx = match self.mem.read_u16(self.tx_queue.avail_ring + 2) {
                Ok(idx) => idx,
                Err(e) => {
                    eprintln!("[VirtIO-Console Error] Failed to read avail ring idx: {}", e);
                    break;
                }
            };

            if self.tx_queue.last_avail_idx == avail_idx {
                break;
            }

            let head = match self.mem.read_u16(
                self.tx_queue.avail_ring + 4 + 2 * (self.tx_queue.last_avail_idx as u64 % q_size as u64),
            ) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("[VirtIO-Console Error] Failed to read head index: {}", e);
                    break;
                }
            };

            self.tx_queue.last_avail_idx = self.tx_queue.last_avail_idx.wrapping_add(1);

            let mut curr = head;
            let mut total_read = 0u32;
            let mut tx_buf = [0u8; 512];

            loop {
                if curr as u32 >= q_size {
                    break;
                }

                let desc_addr = self.tx_queue.desc_table + (curr as u64) * 16;
                let addr = match self.mem.read_u64(desc_addr) {
                    Ok(a) => a,
                    Err(_) => break,
                };
                let len = match self.mem.read_u32(desc_addr + 8) {
                    Ok(l) => l,
                    Err(_) => break,
                };
                let flags = match self.mem.read_u16(desc_addr + 12) {
                    Ok(f) => f,
                    Err(_) => break,
                };
                let next = match self.mem.read_u16(desc_addr + 14) {
                    Ok(n) => n,
                    Err(_) => break,
                };

                // Device reads data from driver (driver writes)
                let is_write = (flags & VRING_DESC_F_WRITE) != 0;
                if !is_write {
                    let mut offset = 0;
                    while offset < len as usize {
                        let chunk = (len as usize - offset).min(tx_buf.len());
                        if self.mem.read(addr + offset as u64, &mut tx_buf[..chunk]).is_ok() {
                            let _ = io::stdout().write_all(&tx_buf[..chunk]);
                        }
                        offset += chunk;
                    }
                    let _ = io::stdout().flush();
                    total_read += len;
                }

                if (flags & VRING_DESC_F_NEXT) != 0 {
                    curr = next;
                } else {
                    break;
                }
            }

            // Write used descriptor
            let used_idx = match self.mem.read_u16(self.tx_queue.used_ring + 2) {
                Ok(idx) => idx,
                Err(_) => break,
            };
            let elem_addr = self.tx_queue.used_ring + 4 + 8 * (used_idx as u64 % q_size as u64);
            let _ = self.mem.write_u32(elem_addr, head as u32);
            let _ = self.mem.write_u32(elem_addr + 4, total_read);

            std::sync::atomic::fence(Ordering::SeqCst);
            let _ = self.mem.write_u16(self.tx_queue.used_ring + 2, used_idx.wrapping_add(1));
            processed_any = true;
        }

        if processed_any {
            self.interrupt_status |= 1;
            unsafe {
                crate::hypervisor::hv_gic_set_spi(self.irq_num, true);
            }
            crate::vcpu::kick_vcpu();
        }
    }
}

impl MmioDevice for VirtioConsole {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if offset >= 0x100 {
            let cfg_offset = (offset - 0x100) as usize;
            let mut cfg = [0u8; 12];
            cfg[0..2].copy_from_slice(&self.cols.to_le_bytes());
            cfg[2..4].copy_from_slice(&self.rows.to_le_bytes());
            cfg[4..8].copy_from_slice(&1u32.to_le_bytes()); // max_nr_ports = 1
            cfg[8..12].copy_from_slice(&0u32.to_le_bytes()); // emerg_wr = 0

            let mut val = 0u64;
            for i in 0..size {
                if cfg_offset + i < cfg.len() {
                    val |= (cfg[cfg_offset + i] as u64) << (i * 8);
                }
            }
            return val;
        }

        match offset {
            VIRTIO_MMIO_MAGIC_VALUE => VIRTIO_MMIO_MAGIC as u64,
            VIRTIO_MMIO_VERSION => VIRTIO_MMIO_VERSION_2 as u64,
            VIRTIO_MMIO_DEVICE_ID => VIRTIO_DEV_CONSOLE as u64,
            VIRTIO_MMIO_VENDOR_ID => VIRTIO_MMIO_VENDOR as u64,
            VIRTIO_MMIO_DEVICE_FEATURES => {
                if self.device_features_sel == 0 {
                    (VIRTIO_CONSOLE_F_SIZE | VIRTIO_F_VERSION_1) as u64 & 0xffff_ffff
                } else if self.device_features_sel == 1 {
                    ((VIRTIO_CONSOLE_F_SIZE | VIRTIO_F_VERSION_1) >> 32) as u64
                } else {
                    0
                }
            }
            VIRTIO_MMIO_QUEUE_NUM_MAX => {
                if let Some(q) = self.active_queue() {
                    q.max_size as u64
                } else {
                    0
                }
            }
            VIRTIO_MMIO_QUEUE_READY => {
                if let Some(q) = self.active_queue() {
                    q.ready as u64
                } else {
                    0
                }
            }
            VIRTIO_MMIO_INTERRUPT_STATUS => self.interrupt_status as u64,
            VIRTIO_MMIO_STATUS => self.status as u64,
            _ => 0,
        }
    }

    fn write(&mut self, offset: u64, data: u64, _size: usize) {
        if offset >= 0x100 {
            let cfg_offset = offset - 0x100;
            if cfg_offset == 8 {
                // emerg_wr: write emergency character to stdout
                let ch = (data & 0xff) as u8;
                let mut stdout = io::stdout().lock();
                let _ = stdout.write_all(&[ch]);
                let _ = stdout.flush();
            }
            return;
        }

        match offset {
            VIRTIO_MMIO_DEVICE_FEATURES_SEL => {
                self.device_features_sel = data as u32;
            }
            VIRTIO_MMIO_DRIVER_FEATURES => {
                if self.driver_features_sel == 0 {
                    self.driver_features = (self.driver_features & !0xffff_ffff) | (data & 0xffff_ffff);
                } else if self.driver_features_sel == 1 {
                    self.driver_features = (self.driver_features & 0xffff_ffff) | (data << 32);
                }
            }
            VIRTIO_MMIO_DRIVER_FEATURES_SEL => {
                self.driver_features_sel = data as u32;
            }
            VIRTIO_MMIO_QUEUE_SEL => {
                self.queue_sel = data as u32;
            }
            VIRTIO_MMIO_QUEUE_NUM => {
                if let Some(q) = self.active_queue_mut() {
                    q.size = (data as u32).min(q.max_size);
                }
            }
            VIRTIO_MMIO_QUEUE_READY => {
                if let Some(q) = self.active_queue_mut() {
                    q.ready = (data & 1) != 0;
                }
            }
            VIRTIO_MMIO_QUEUE_NOTIFY => {
                if data == 1 {
                    self.process_tx_queue();
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
                    // Reset device
                    self.rx_queue = VirtQueue {
                        max_size: VTCON_QUEUE_SIZE,
                        size: VTCON_QUEUE_SIZE,
                        ..Default::default()
                    };
                    self.tx_queue = VirtQueue {
                        max_size: VTCON_QUEUE_SIZE,
                        size: VTCON_QUEUE_SIZE,
                        ..Default::default()
                    };
                    self.interrupt_status = 0;
                    unsafe {
                        crate::hypervisor::hv_gic_set_spi(self.irq_num, false);
                    }
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
