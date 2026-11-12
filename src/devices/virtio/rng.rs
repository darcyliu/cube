//! VirtIO Random Number Generator (`virtio-rng`) device over Modern MMIO.
//!
//! Conforms to VirtIO 1.0/1.1 Specification, Section 5.4 (Entropy Device).
//! Uses macOS `arc4random_buf` to supply cryptographically secure random bytes
//! to the guest OS without any external crate dependencies.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::transport::WriteEffect;
use super::*;
use crate::devices::MmioDevice;
use crate::memory::GuestMemory;

const VTRNG_QUEUE_SIZE: u32 = 64;

unsafe extern "C" {
    fn arc4random_buf(buf: *mut std::ffi::c_void, nbytes: usize);
}

fn fill_random(dest: &mut [u8]) {
    unsafe {
        arc4random_buf(dest.as_mut_ptr() as *mut std::ffi::c_void, dest.len());
    }
}

pub struct VirtioRng {
    transport: MmioTransport,
    mem: Arc<GuestMemory>,
}

impl VirtioRng {
    pub fn new(irq_num: u32, mem: Arc<GuestMemory>) -> Self {
        Self {
            transport: MmioTransport::new(
                VIRTIO_DEV_RNG,
                VIRTIO_F_VERSION_1,
                &[VTRNG_QUEUE_SIZE],
                irq_num,
            ),
            mem,
        }
    }

    fn process_queue(&mut self) {
        if !self.transport.operational() {
            return;
        }
        let queue = self.transport.queue(0).copied().unwrap();
        if !queue.ready || queue.avail_ring == 0 || queue.desc_table == 0 || queue.size == 0 {
            return;
        }
        let count = match queue.pending_count(&self.mem) {
            Ok(n) => n,
            Err(_) => {
                self.transport.set_needs_reset();
                return;
            }
        };
        let mut queue = *self.transport.queue(0).unwrap();
        let mut processed_any = false;
        for _ in 0..count {
            std::sync::atomic::fence(Ordering::SeqCst);
            let avail_idx = match self.mem.read_u16(queue.avail_ring + 2) {
                Ok(idx) => idx,
                Err(e) => {
                    eprintln!("[VirtIO-Rng Error] Failed to read avail ring idx: {}", e);
                    break;
                }
            };
            if queue.last_avail_idx == avail_idx {
                break;
            }
            let head = match self.mem.read_u16(
                queue.avail_ring + 4 + 2 * (queue.last_avail_idx as u64 % queue.size as u64),
            ) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("[VirtIO-Rng Error] Failed to read head index: {}", e);
                    break;
                }
            };
            queue.last_avail_idx = queue.last_avail_idx.wrapping_add(1);
            let descriptors = match queue.descriptors(&self.mem, head, false, 1024 * 1024) {
                Ok(d) if d.iter().all(|d| d.flags & VRING_DESC_F_WRITE != 0) => d,
                _ => {
                    self.transport.set_needs_reset();
                    *self.transport.queue_mut(0).unwrap() = queue;
                    return;
                }
            };
            let mut total_written = 0;
            let mut rng_buf = [0; 512];
            for d in descriptors {
                let mut offset = 0;
                while offset < d.len as usize {
                    let n = (d.len as usize - offset).min(rng_buf.len());
                    fill_random(&mut rng_buf[..n]);
                    if self
                        .mem
                        .write(d.addr + offset as u64, &rng_buf[..n])
                        .is_err()
                    {
                        self.transport.set_needs_reset();
                        *self.transport.queue_mut(0).unwrap() = queue;
                        return;
                    }
                    offset += n;
                    total_written += n as u32;
                }
            }
            publish_used(&self.mem, &mut queue, head, total_written);
            self.transport.diagnostics_mut().completions += 1;
            self.transport.diagnostics_mut().bytes += total_written as u64;
            processed_any = true;
        }
        *self.transport.queue_mut(0).unwrap() = queue;
        if processed_any && avail_wants_interrupt(&self.mem, queue.avail_ring) {
            self.transport.raise_interrupt(1);
            unsafe {
                crate::hypervisor::hv_gic_set_spi(self.transport.irq(), true);
            }
        }
    }
}

impl MmioDevice for VirtioRng {
    fn read(&mut self, offset: u64, _size: usize) -> u64 {
        self.transport.read(offset)
    }

    fn write(&mut self, offset: u64, data: u64, _size: usize) {
        match self.transport.write(offset, data) {
            WriteEffect::Notify { queue: 0 } => self.process_queue(),
            WriteEffect::Reset => unsafe {
                crate::hypervisor::hv_gic_set_spi(self.transport.irq(), false);
            },
            WriteEffect::InterruptAck { line_low: true } => unsafe {
                crate::hypervisor::hv_gic_set_spi(self.transport.irq(), false);
            },
            _ => {}
        }
    }
}
