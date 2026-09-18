//! VirtIO Block Device (`virtio-blk`) implementation over Modern MMIO.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

static BLK_IOS: AtomicU64 = AtomicU64::new(0);

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
const VIRTIO_BLK_F_SIZE_MAX: u64 = 1 << 1;
const VIRTIO_BLK_F_SEG_MAX: u64 = 1 << 2;
const VIRTIO_BLK_F_RO: u64 = 1 << 5;
const VIRTIO_BLK_F_BLK_SIZE: u64 = 1 << 6;
const VIRTIO_BLK_F_FLUSH: u64 = 1 << 9;
const VIRTIO_F_VERSION_1: u64 = 1 << 32;

/// Match the last combination that booted and finished fsck: 1MB segments,
/// 128-entry queue. (bhyve uses queue_size-2; we can tighten after fsck works.)
const VTBLK_QUEUE_SIZE: u32 = 128;
const VTBLK_SEG_MAX: u32 = 126;

pub struct VirtioBlock {
    file: Option<File>,
    capacity_sectors: u64,
    status: u32,
    device_features_sel: u32,
    driver_features_sel: u32,
    driver_features: u64,
    queue: VirtQueue,
    queue_sel: u32,
    used_idx: u16,
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
                max_size: VTBLK_QUEUE_SIZE,
                size: VTBLK_QUEUE_SIZE,
                ..Default::default()
            },
            queue_sel: 0,
            used_idx: 0,
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

        // Drain the avail ring until it stops moving. Publish used.idx with a
        // SeqCst fence so the other vCPU cannot observe the index before the
        // used-ring slot (fsck Phase 1 hangs without this on SMP).
        let mut processed_any = false;
        loop {
            std::sync::atomic::fence(Ordering::SeqCst);
            let avail_idx = match self.mem.read_u16(self.queue.avail_ring + 2) {
                Ok(idx) => idx,
                Err(e) => {
                    eprintln!("[VirtIO-Blk Error] Failed to read avail ring idx: {}", e);
                    break;
                }
            };
            if self.queue.last_avail_idx == avail_idx {
                break;
            }

            let ring_elem_offset = self.queue.avail_ring
                + 4
                + 2 * (self.queue.last_avail_idx as u64 % q_size as u64);
            let head_desc = match self.mem.read_u16(ring_elem_offset) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("[VirtIO-Blk Error] Failed to read avail ring element: {}", e);
                    break;
                }
            };

            let bytes_transferred = self.handle_request(head_desc, q_size);
            publish_used(&self.mem, &mut self.queue, head_desc, bytes_transferred);
            self.used_idx = self.queue.used_idx;
            self.queue.last_avail_idx = self.queue.last_avail_idx.wrapping_add(1);
            processed_any = true;
            let n = BLK_IOS.fetch_add(1, Ordering::Relaxed) + 1;
            if n == 1 || n % 256 == 0 {
                if let Ok(mut f) = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open("/tmp/vmm-blk.log")
                {
                    let _ = writeln!(
                        f,
                        "ios={} last_avail={} used_idx={} qsize={}",
                        n, self.queue.last_avail_idx, self.used_idx, q_size
                    );
                }
            }
        }

        if processed_any {
            self.interrupt_status |= 1;
            // Always raise: vtblk_ident() busy-polls GET_ID but later bios
            // wait on the IRQ. Skipping for NO_INTERRUPT wedged attach.
            unsafe {
                crate::hypervisor::hv_gic_set_spi(self.irq_num, true);
            }
            crate::vcpu::kick_vcpu();
        }
    }

    fn read_desc(&self, table: u64, idx: u16) -> Option<(u64, u32, u16, u16)> {
        let da = table + (idx as u64 * 16);
        let addr = self.mem.read_u64(da).ok()?;
        let len = self.mem.read_u32(da + 8).ok()?;
        let flags = self.mem.read_u16(da + 12).ok()?;
        let next = self.mem.read_u16(da + 14).ok()?;
        Some((addr, len, flags, next))
    }

    /// Walk a descriptor chain. Matches QEMU virtqueue_split_pop: if the head
    /// has VRING_DESC_F_INDIRECT, `next` indexes the indirect table, not the
    /// virtqueue (see qemu/hw/virtio/virtio.c).
    fn collect_descriptors(&self, head_desc: u16, q_size: u32) -> Vec<(u64, u32, u16)> {
        let Some((addr, len, flags, next)) = self.read_desc(self.queue.desc_table, head_desc)
        else {
            return Vec::new();
        };

        if (flags & VRING_DESC_F_INDIRECT) != 0 {
            if len == 0 || len % 16 != 0 {
                return Vec::new();
            }
            return self.walk_desc_table(addr, (len / 16) as u16);
        }

        let mut descriptors = vec![(addr, len, flags)];
        let mut desc_idx = next;
        let mut steps = 1u32;
        let mut has_next = (flags & VRING_DESC_F_NEXT) != 0;
        while has_next && steps < q_size {
            let Some((a, l, f, n)) = self.read_desc(self.queue.desc_table, desc_idx) else {
                break;
            };
            descriptors.push((a, l, f));
            has_next = (f & VRING_DESC_F_NEXT) != 0;
            desc_idx = n;
            steps += 1;
        }
        descriptors
    }

    fn walk_desc_table(&self, table: u64, max: u16) -> Vec<(u64, u32, u16)> {
        let mut descriptors = Vec::new();
        if max == 0 {
            return descriptors;
        }
        // FreeBSD virtqueue_init_indirect_list pre-links next=i+1, then
        // vq_ring_enqueue_segments overwrites the first `needed` entries
        // (qemu/hw/virtio/virtio.c virtqueue_split_pop does the same walk).
        let mut i = 0u16;
        for _ in 0..max {
            let Some((addr, len, flags, next)) = self.read_desc(table, i) else {
                break;
            };
            descriptors.push((addr, len, flags));
            if (flags & VRING_DESC_F_NEXT) == 0 {
                break;
            }
            // VQ_RING_DESC_CHAIN_END is 32768; treat anything out of range
            // as end-of-chain rather than looping.
            if next >= max {
                break;
            }
            i = next;
        }
        descriptors
    }

    fn handle_request(&mut self, head_desc: u16, q_size: u32) -> u32 {
        let descriptors = self.collect_descriptors(head_desc, q_size);
        let n = BLK_IOS.load(Ordering::Relaxed);
        if n < 8 {
            if let Ok(mut f) = OpenOptions::new()
                .create(true)
                .append(true)
                .open("/tmp/vmm-blk.log")
            {
                let _ = writeln!(
                    f,
                    "req head={} ndesc={} first_flags={:#x}",
                    head_desc,
                    descriptors.len(),
                    descriptors.first().map(|d| d.2).unwrap_or(0)
                );
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
                // Do not advertise SIZE_MAX: FreeBSD vtblk_attach fails with
                // ENOTSUP if size_max < maxphys, which is how we got ENODEV.
                let features = VIRTIO_BLK_F_SEG_MAX
                    | VIRTIO_BLK_F_BLK_SIZE
                    | VIRTIO_BLK_F_FLUSH
                    | VIRTIO_RING_F_INDIRECT_DESC
                    | VIRTIO_F_VERSION_1;
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
            // struct virtio_blk_config: capacity, size_max, seg_max, geometry, blk_size
            off if off >= VIRTIO_MMIO_CONFIG => {
                let config_offset = (off - VIRTIO_MMIO_CONFIG) as usize;
                let mut cfg = [0u8; 32];
                cfg[0..8].copy_from_slice(&self.capacity_sectors.to_le_bytes());
                cfg[12..16].copy_from_slice(&VTBLK_SEG_MAX.to_le_bytes());
                cfg[20..24].copy_from_slice(&512u32.to_le_bytes());
                if config_offset + size <= cfg.len() {
                    let mut val = 0u64;
                    for (i, &b) in cfg[config_offset..config_offset + size].iter().enumerate() {
                        val |= (b as u64) << (i * 8);
                    }
                    val
                } else {
                    0
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
                        max_size: VTBLK_QUEUE_SIZE,
                        size: VTBLK_QUEUE_SIZE,
                        ..Default::default()
                    };
                    self.used_idx = 0;
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
