//! VirtIO Block Device (`virtio-blk`) implementation over Modern MMIO.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::Arc;

use super::*;
use crate::devices::MmioDevice;
use crate::memory::GuestMemory;

// VirtIO Block Request Types
const VIRTIO_BLK_T_IN: u32 = 0;    // Read from disk
const VIRTIO_BLK_T_OUT: u32 = 1;   // Write to disk
const VIRTIO_BLK_T_FLUSH: u32 = 4; // Flush disk cache
const VIRTIO_BLK_T_GET_ID: u32 = 8; // Get device ID

// VirtIO Block Status Codes
const VIRTIO_BLK_S_OK: u8 = 0;
const VIRTIO_BLK_S_IOERR: u8 = 1;
const VIRTIO_BLK_S_UNSUPP: u8 = 2;

// VirtIO Feature Bits
const VIRTIO_BLK_F_RO: u64 = 1 << 5;
const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;
const VIRTIO_F_VERSION_1: u64 = 1 << 32;

pub struct VirtioBlock {
    file: Option<File>,
    capacity_sectors: u64,
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

impl VirtioBlock {
    pub fn new(disk_path: Option<&str>, irq_num: u32, mem: Arc<GuestMemory>) -> Result<Self, String> {
        let (file, capacity_sectors) = if let Some(path) = disk_path {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .or_else(|_| OpenOptions::new().read(true).open(path))
                .map_err(|e| format!("Failed to open disk image '{}': {}", path, e))?;

            let file_size = file
                .metadata()
                .map_err(|e| format!("Failed to query disk metadata: {}", e))?
                .len();
            let sectors = file_size / 512;
            println!(
                "[VirtIO-Blk] Opened disk '{}': {:.2} MB ({} sectors)",
                path,
                file_size as f64 / (1024.0 * 1024.0),
                sectors
            );
            (Some(file), sectors)
        } else {
            println!("[VirtIO-Blk] Initialized empty mock disk (0 sectors)");
            (None, 0)
        };

        Ok(Self {
            file,
            capacity_sectors,
            status: 0,
            device_features_sel: 0,
            driver_features_sel: 0,
            driver_features: 0,
            queue: VirtQueue {
                max_size: 128,
                size: 128,
                ..Default::default()
            },
            queue_sel: 0,
            interrupt_status: 0,
            irq_num,
            mem,
        })
    }

    fn process_queue(&mut self) {
        if !self.queue.ready || self.queue.avail_ring == 0 || self.queue.desc_table == 0 {
            return;
        }

        let q_size = self.queue.size;
        if q_size == 0 {
            return;
        }

        // Read available ring idx (at offset 2 of avail_ring GPA)
        let avail_idx = match self.mem.read_u16(self.queue.avail_ring + 2) {
            Ok(idx) => idx,
            Err(e) => {
                eprintln!("[VirtIO-Blk Error] Failed to read avail ring idx: {}", e);
                return;
            }
        };

        let mut curr_idx = self.queue.last_avail_idx;
        let mut processed_any = false;

        while curr_idx != avail_idx {
            let ring_elem_offset = self.queue.avail_ring + 4 + 2 * (curr_idx as u64 % q_size as u64);
            let head_desc = match self.mem.read_u16(ring_elem_offset) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("[VirtIO-Blk Error] Failed to read avail ring element: {}", e);
                    break;
                }
            };

            let bytes_transferred = self.handle_request(head_desc, q_size);

            // Add to used ring:
            // Used ring format: flags (u16), idx (u16), ring[u32 id, u32 len]
            if let Ok(used_idx) = self.mem.read_u16(self.queue.used_ring + 2) {
                let elem_offset = self.queue.used_ring + 4 + 8 * (used_idx as u64 % q_size as u64);
                let _ = self.mem.write_u32(elem_offset, head_desc as u32);
                let _ = self.mem.write_u32(elem_offset + 4, bytes_transferred);
                let _ = self.mem.write_u16(self.queue.used_ring + 2, used_idx.wrapping_add(1));
            }

            curr_idx = curr_idx.wrapping_add(1);
            processed_any = true;
        }

        self.queue.last_avail_idx = curr_idx;

        if processed_any {
            // Signal interrupt (used buffer notification)
            self.interrupt_status |= 1;
            unsafe {
                crate::hypervisor::hv_gic_set_spi(self.irq_num, true);
            }
        }
    }

    fn handle_request(&mut self, head_desc: u16, q_size: u32) -> u32 {
        // Collect descriptors in the chain
        let mut desc_idx = head_desc;
        let mut descriptors = Vec::new();
        let mut steps = 0;

        while steps < q_size {
            let desc_addr = self.queue.desc_table + (desc_idx as u64 * 16);
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

            descriptors.push((addr, len, flags));

            if (flags & VRING_DESC_F_NEXT) != 0 {
                desc_idx = next;
                steps += 1;
            } else {
                break;
            }
        }

        if descriptors.len() < 2 {
            return 0;
        }

        // Descriptor 0: Header (virtio_blk_req: type u32, reserved u32, sector u64)
        let (hdr_addr, hdr_len, _) = descriptors[0];
        if hdr_len < 16 {
            return 0;
        }

        let req_type = self.mem.read_u32(hdr_addr).unwrap_or(u32::MAX);
        let sector = self.mem.read_u64(hdr_addr + 8).unwrap_or(0);

        // Last descriptor: Status (1 byte)
        let (status_addr, _, _) = descriptors[descriptors.len() - 1];

        let mut total_bytes = 0u32;
        let mut status = VIRTIO_BLK_S_OK;

        match req_type {
            VIRTIO_BLK_T_IN => {
                // Read from disk image into guest memory
                if let Some(ref mut file) = self.file {
                    let mut file_offset = sector * 512;
                    for i in 1..descriptors.len() - 1 {
                        let (buf_addr, buf_len, _) = descriptors[i];
                        if file.seek(SeekFrom::Start(file_offset)).is_ok() {
                            let mut temp_buf = vec![0u8; buf_len as usize];
                            if let Ok(n) = file.read(&mut temp_buf) {
                                if n < temp_buf.len() {
                                    // Zero remaining
                                    temp_buf[n..].fill(0);
                                }
                                let _ = self.mem.write(buf_addr, &temp_buf);
                                total_bytes += buf_len;
                                file_offset += buf_len as u64;
                            } else {
                                status = VIRTIO_BLK_S_IOERR;
                                break;
                            }
                        } else {
                            status = VIRTIO_BLK_S_IOERR;
                            break;
                        }
                    }
                } else {
                    status = VIRTIO_BLK_S_IOERR;
                }
            }
            VIRTIO_BLK_T_OUT => {
                // Write from guest memory into disk image
                if let Some(ref mut file) = self.file {
                    let mut file_offset = sector * 512;
                    for i in 1..descriptors.len() - 1 {
                        let (buf_addr, buf_len, _) = descriptors[i];
                        let mut temp_buf = vec![0u8; buf_len as usize];
                        if self.mem.read(buf_addr, &mut temp_buf).is_ok() {
                            if file.seek(SeekFrom::Start(file_offset)).is_ok()
                                && file.write_all(&temp_buf).is_ok()
                            {
                                total_bytes += buf_len;
                                file_offset += buf_len as u64;
                            } else {
                                status = VIRTIO_BLK_S_IOERR;
                                break;
                            }
                        } else {
                            status = VIRTIO_BLK_S_IOERR;
                            break;
                        }
                    }
                } else {
                    status = VIRTIO_BLK_S_IOERR;
                }
            }
            VIRTIO_BLK_T_FLUSH => {
                if let Some(ref mut file) = self.file {
                    let _ = file.flush();
                }
            }
            VIRTIO_BLK_T_GET_ID => {
                // Device serial number string (up to 20 ASCII characters)
                let id_str = b"vmm-virtio-blk0\0\0\0\0\0";
                for i in 1..descriptors.len() - 1 {
                    let (buf_addr, buf_len, _) = descriptors[i];
                    let copy_len = (buf_len as usize).min(id_str.len());
                    let _ = self.mem.write(buf_addr, &id_str[..copy_len]);
                    total_bytes += copy_len as u32;
                }
            }
            _ => {
                status = VIRTIO_BLK_S_UNSUPP;
            }
        }

        // Write status byte to status descriptor
        let _ = self.mem.write(status_addr, &[status]);

        total_bytes
    }
}

impl MmioDevice for VirtioBlock {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        match offset {
            VIRTIO_MMIO_MAGIC_VALUE => VIRTIO_MMIO_MAGIC as u64,
            VIRTIO_MMIO_VERSION => VIRTIO_MMIO_VERSION_2 as u64,
            VIRTIO_MMIO_DEVICE_ID => VIRTIO_DEV_BLOCK as u64,
            VIRTIO_MMIO_VENDOR_ID => VIRTIO_MMIO_VENDOR as u64,
            VIRTIO_MMIO_DEVICE_FEATURES => {
                let features = VIRTIO_BLK_F_FLUSH | VIRTIO_F_VERSION_1;
                if self.device_features_sel == 0 {
                    features & 0xffff_ffff
                } else if self.device_features_sel == 1 {
                    (features >> 32) & 0xffff_ffff
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
                if self.queue_sel == 0 && self.queue.ready {
                    1
                } else {
                    0
                }
            }
            VIRTIO_MMIO_INTERRUPT_STATUS => self.interrupt_status as u64,
            VIRTIO_MMIO_STATUS => self.status as u64,
            // Config Space: offset 0x100 is capacity (u64 in 512-byte sectors)
            off if off >= VIRTIO_MMIO_CONFIG => {
                let config_offset = off - VIRTIO_MMIO_CONFIG;
                match (config_offset, size) {
                    (0, 8) => self.capacity_sectors,
                    (0, 4) => self.capacity_sectors & 0xffff_ffff,
                    (4, 4) => (self.capacity_sectors >> 32) & 0xffff_ffff,
                    (0, 1) => self.capacity_sectors & 0xff,
                    (1, 1) => (self.capacity_sectors >> 8) & 0xff,
                    _ => 0,
                }
            }
            _ => 0,
        }
    }

    fn write(&mut self, offset: u64, data: u64, _size: usize) {
        match offset {
            VIRTIO_MMIO_DEVICE_FEATURES_SEL => {
                self.device_features_sel = data as u32;
            }
            VIRTIO_MMIO_DRIVER_FEATURES_SEL => {
                self.driver_features_sel = data as u32;
            }
            VIRTIO_MMIO_DRIVER_FEATURES => {
                if self.driver_features_sel == 0 {
                    self.driver_features =
                        (self.driver_features & !0xffff_ffff) | (data & 0xffff_ffff);
                } else if self.driver_features_sel == 1 {
                    self.driver_features =
                        (self.driver_features & 0xffff_ffff) | ((data & 0xffff_ffff) << 32);
                }
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
                    self.queue.ready = (data & 1) == 1;
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
                    self.queue.desc_table =
                        (self.queue.desc_table & 0xffff_ffff) | ((data & 0xffff_ffff) << 32);
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
                    self.queue.avail_ring =
                        (self.queue.avail_ring & 0xffff_ffff) | ((data & 0xffff_ffff) << 32);
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
                    self.queue.used_ring =
                        (self.queue.used_ring & 0xffff_ffff) | ((data & 0xffff_ffff) << 32);
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
                if data == 0 {
                    // Reset
                    self.queue = VirtQueue {
                        max_size: 128,
                        size: 128,
                        ..Default::default()
                    };
                    self.interrupt_status = 0;
                    unsafe {
                        crate::hypervisor::hv_gic_set_spi(self.irq_num, false);
                    }
                }
            }
            _ => {}
        }
    }
}
