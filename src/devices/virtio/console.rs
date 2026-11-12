//! VirtIO Console (`virtio-console`) device over Modern MMIO.
//!
//! Conforms to VirtIO 1.0/1.1 Specification, Section 5.3 (Console Device).
//! Supports `VIRTIO_CONSOLE_F_SIZE` (bit 0) for dynamic terminal window geometry
//! synchronization, forwarding host SIGWINCH events into the guest kernel via
//! VirtIO configuration change interrupts.

use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::transport::WriteEffect;
use super::*;
use crate::devices::MmioDevice;
use crate::memory::GuestMemory;

const VTCON_QUEUE_SIZE: u32 = 64;
const VIRTIO_CONSOLE_F_SIZE: u64 = 1 << 0;

pub struct VirtioConsole {
    transport: MmioTransport,
    mem: Arc<GuestMemory>,
    cols: u16,
    rows: u16,
}

impl VirtioConsole {
    pub fn new(irq_num: u32, mem: Arc<GuestMemory>, cols: u16, rows: u16) -> Self {
        Self {
            transport: MmioTransport::new(
                VIRTIO_DEV_CONSOLE,
                VIRTIO_CONSOLE_F_SIZE | VIRTIO_F_VERSION_1,
                &[VTCON_QUEUE_SIZE, VTCON_QUEUE_SIZE],
                irq_num,
            ),
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
        self.transport.set_interrupt_bit(2);

        if self.transport.operational() {
            unsafe {
                crate::hypervisor::hv_gic_set_spi(self.transport.irq(), true);
            }
            crate::vcpu::kick_vcpu();
        }
    }

    pub fn get_size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Process outgoing guest console data on transmitq (queue 1).
    fn process_tx_queue(&mut self) {
        if !self.transport.operational() {
            return;
        }
        let Some(tx) = self.transport.queue(1).copied() else {
            return;
        };
        if !tx.ready || tx.avail_ring == 0 || tx.desc_table == 0 || tx.size == 0 {
            return;
        }
        let count = match tx.pending_count(&self.mem) {
            Ok(n) => n,
            Err(_) => {
                self.transport.set_needs_reset();
                return;
            }
        };
        let mut tx = *self.transport.queue(1).unwrap();
        let mut processed_any = false;
        for _ in 0..count {
            std::sync::atomic::fence(Ordering::SeqCst);
            let avail_idx = match self.mem.read_u16(tx.avail_ring + 2) {
                Ok(idx) => idx,
                Err(e) => {
                    eprintln!("[VirtIO-Console Error] Failed to read avail ring idx: {e}");
                    break;
                }
            };
            if tx.last_avail_idx == avail_idx {
                break;
            }
            let head = match self
                .mem
                .read_u16(tx.avail_ring + 4 + 2 * (tx.last_avail_idx as u64 % tx.size as u64))
            {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("[VirtIO-Console Error] Failed to read head index: {e}");
                    break;
                }
            };
            tx.last_avail_idx = tx.last_avail_idx.wrapping_add(1);
            let descriptors = match tx.descriptors(&self.mem, head, false, 1024 * 1024) {
                Ok(d) if d.iter().all(|d| d.flags & VRING_DESC_F_WRITE == 0) => d,
                _ => {
                    self.transport.set_needs_reset();
                    break;
                }
            };
            let mut tx_buf = [0; 512];
            let mut failed = false;
            for d in descriptors {
                let mut offset = 0;
                while offset < d.len as usize {
                    let n = (d.len as usize - offset).min(tx_buf.len());
                    if self
                        .mem
                        .read(d.addr + offset as u64, &mut tx_buf[..n])
                        .is_err()
                    {
                        self.transport.set_needs_reset();
                        failed = true;
                        break;
                    }
                    let _ = io::stdout().write_all(&tx_buf[..n]);
                    offset += n;
                }
                if failed {
                    break;
                }
            }
            if failed {
                break;
            }
            let _ = io::stdout().flush();
            publish_used(&self.mem, &mut tx, head, 0);
            self.transport.diagnostics_mut().completions += 1;
            processed_any = true;
        }
        *self.transport.queue_mut(1).unwrap() = tx;
        if processed_any {
            self.transport.raise_interrupt(1);
            unsafe {
                crate::hypervisor::hv_gic_set_spi(self.transport.irq(), true);
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

        self.transport.read(offset)
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
        let irq = self.transport.irq();
        match self.transport.write(offset, data) {
            WriteEffect::Notify { queue: 1 } => self.process_tx_queue(),
            WriteEffect::Reset => unsafe {
                crate::hypervisor::hv_gic_set_spi(irq, false);
            },
            WriteEffect::InterruptAck { line_low: true } => unsafe {
                crate::hypervisor::hv_gic_set_spi(irq, false);
            },
            _ => {}
        }
    }
}
