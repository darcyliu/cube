//! VirtIO Random Number Generator (`virtio-rng`) device over Modern MMIO.
//!
//! Conforms to VirtIO 1.0/1.1 Specification, Section 5.4 (Entropy Device).
//! Uses macOS `arc4random_buf` to supply cryptographically secure random bytes
//! to the guest OS without any external crate dependencies.

use std::sync::atomic::Ordering;
use std::sync::Arc;

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
    status: u32,
    device_features_sel: u32,
    driver_features_sel: u32,
    driver_features: u64,
    queue: VirtQueue,
    queue_sel: u32,
    interrupt_status: u32,
    irq_num: u32,
    mem: Arc<GuestMemory>,
}

impl VirtioRng {
    pub fn new(irq_num: u32, mem: Arc<GuestMemory>) -> Self {
        Self {
            status: 0,
            device_features_sel: 0,
            driver_features_sel: 0,
            driver_features: 0,
            queue: VirtQueue {
                max_size: VTRNG_QUEUE_SIZE,
                size: VTRNG_QUEUE_SIZE,
                ..Default::default()
            },
            queue_sel: 0,
            interrupt_status: 0,
            irq_num,
            mem,
        }
    }

    fn process_queue(&mut self) {
        if !self.queue.ready || self.queue.avail_ring == 0 || self.queue.desc_table == 0 {
            return;
        }

        let q_size = self.queue.size;
        if q_size == 0 {
            return;
        }

        let count = match self.queue.pending_count(&self.mem) {
            Ok(n) => n,
            Err(_) => {
                self.status |= VIRTIO_STATUS_NEEDS_RESET;
                return;
            }
        };
        let mut processed_any = false;
        for _ in 0..count {
            std::sync::atomic::fence(Ordering::SeqCst);
            let avail_idx = match self.mem.read_u16(self.queue.avail_ring + 2) {
                Ok(idx) => idx,
                Err(e) => {
                    eprintln!("[VirtIO-Rng Error] Failed to read avail ring idx: {}", e);
                    break;
                }
            };

            if self.queue.last_avail_idx == avail_idx {
                break;
            }

            let head = match self.mem.read_u16(
                self.queue.avail_ring + 4 + 2 * (self.queue.last_avail_idx as u64 % q_size as u64),
            ) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("[VirtIO-Rng Error] Failed to read head index: {}", e);
                    break;
                }
            };

            self.queue.last_avail_idx = self.queue.last_avail_idx.wrapping_add(1);

            let descriptors = match self.queue.descriptors(&self.mem, head, false, 1024 * 1024) {
                Ok(d) if d.iter().all(|d| d.flags & VRING_DESC_F_WRITE != 0) => d,
                _ => {
                    self.status |= VIRTIO_STATUS_NEEDS_RESET;
                    break;
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
                        self.status |= VIRTIO_STATUS_NEEDS_RESET;
                        return;
                    }
                    offset += n;
                    total_written += n as u32;
                }
            }
            publish_used(&self.mem, &mut self.queue, head, total_written);
            processed_any = true;
        }

        if processed_any && avail_wants_interrupt(&self.mem, self.queue.avail_ring) {
            self.interrupt_status |= 1;
            unsafe {
                crate::hypervisor::hv_gic_set_spi(self.irq_num, true);
            }
        }
    }
}

impl MmioDevice for VirtioRng {
    fn read(&mut self, offset: u64, _size: usize) -> u64 {
        match offset {
            VIRTIO_MMIO_MAGIC_VALUE => VIRTIO_MMIO_MAGIC as u64,
            VIRTIO_MMIO_VERSION => VIRTIO_MMIO_VERSION_2 as u64,
            VIRTIO_MMIO_DEVICE_ID => VIRTIO_DEV_RNG as u64,
            VIRTIO_MMIO_VENDOR_ID => VIRTIO_MMIO_VENDOR as u64,
            VIRTIO_MMIO_DEVICE_FEATURES => {
                if self.device_features_sel == 0 {
                    // No device-specific features
                    0
                } else if self.device_features_sel == 1 {
                    (VIRTIO_F_VERSION_1 >> 32) as u64
                } else {
                    0
                }
            }
            VIRTIO_MMIO_QUEUE_NUM_MAX => {
                if self.queue_sel == 0 {
                    self.queue.max_size as u64
                } else {
                    0
                }
            }
            VIRTIO_MMIO_QUEUE_READY => {
                if self.queue_sel == 0 {
                    self.queue.ready as u64
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
        match offset {
            VIRTIO_MMIO_DEVICE_FEATURES_SEL => {
                self.device_features_sel = data as u32;
            }
            VIRTIO_MMIO_DRIVER_FEATURES => {
                if self.driver_features_sel == 0 {
                    self.driver_features =
                        (self.driver_features & !0xffff_ffff) | (data & 0xffff_ffff);
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
                if self.queue_sel == 0 {
                    self.queue.size = (data as u32).min(self.queue.max_size);
                }
            }
            VIRTIO_MMIO_QUEUE_READY => {
                if self.queue_sel == 0 {
                    self.queue.ready = (data & 1) != 0;
                }
            }
            VIRTIO_MMIO_QUEUE_NOTIFY => {
                if data == 0 {
                    self.process_queue();
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
                    self.queue = VirtQueue {
                        max_size: VTRNG_QUEUE_SIZE,
                        size: VTRNG_QUEUE_SIZE,
                        ..Default::default()
                    };
                    self.interrupt_status = 0;
                    unsafe {
                        crate::hypervisor::hv_gic_set_spi(self.irq_num, false);
                    }
                }
            }
            VIRTIO_MMIO_QUEUE_DESC_LOW => {
                if self.queue_sel == 0 {
                    self.queue.desc_table =
                        (self.queue.desc_table & !0xffff_ffff) | (data & 0xffff_ffff);
                }
            }
            VIRTIO_MMIO_QUEUE_DESC_HIGH => {
                if self.queue_sel == 0 {
                    self.queue.desc_table = (self.queue.desc_table & 0xffff_ffff) | (data << 32);
                }
            }
            VIRTIO_MMIO_QUEUE_DRIVER_LOW => {
                if self.queue_sel == 0 {
                    self.queue.avail_ring =
                        (self.queue.avail_ring & !0xffff_ffff) | (data & 0xffff_ffff);
                }
            }
            VIRTIO_MMIO_QUEUE_DRIVER_HIGH => {
                if self.queue_sel == 0 {
                    self.queue.avail_ring = (self.queue.avail_ring & 0xffff_ffff) | (data << 32);
                }
            }
            VIRTIO_MMIO_QUEUE_DEVICE_LOW => {
                if self.queue_sel == 0 {
                    self.queue.used_ring =
                        (self.queue.used_ring & !0xffff_ffff) | (data & 0xffff_ffff);
                }
            }
            VIRTIO_MMIO_QUEUE_DEVICE_HIGH => {
                if self.queue_sel == 0 {
                    self.queue.used_ring = (self.queue.used_ring & 0xffff_ffff) | (data << 32);
                }
            }
            _ => {}
        }
    }
}
