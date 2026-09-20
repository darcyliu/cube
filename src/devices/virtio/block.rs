//! VirtIO Block Device (`virtio-blk`) implementation over Modern MMIO.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
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

/// Match the combination: 1MB segments, 128-entry queue.
const VTBLK_QUEUE_SIZE: u32 = 128;
const VTBLK_SEG_MAX: u32 = 126;

/// Storage backend abstraction for raw files, COW overlays, and read-only images.
pub trait BlockBackend: Send {
    fn size_bytes(&self) -> u64;
    fn is_read_only(&self) -> bool;
    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()>;
    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
}

/// Standard raw disk file backend.
pub struct RawFileBackend {
    file: File,
    size: u64,
    read_only: bool,
}

impl RawFileBackend {
    pub fn open(path: &str, read_only: bool) -> Result<Self, String> {
        let (file, ro) = if read_only {
            let f = OpenOptions::new()
                .read(true)
                .open(path)
                .map_err(|e| format!("Failed to open '{}' read-only: {}", path, e))?;
            (f, true)
        } else {
            match OpenOptions::new().read(true).write(true).open(path) {
                Ok(f) => (f, false),
                Err(_) => {
                    let f = OpenOptions::new()
                        .read(true)
                        .open(path)
                        .map_err(|e| format!("Failed to open '{}': {}", path, e))?;
                    (f, true)
                }
            }
        };

        let size = file
            .metadata()
            .map_err(|e| format!("Failed to query metadata for '{}': {}", path, e))?
            .len();

        Ok(Self {
            file,
            size,
            read_only: ro,
        })
    }
}

impl BlockBackend for RawFileBackend {
    fn size_bytes(&self) -> u64 {
        self.size
    }

    fn is_read_only(&self) -> bool {
        self.read_only
    }

    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        self.file.seek(SeekFrom::Start(offset))?;
        let n = self.file.read(buf)?;
        if n < buf.len() {
            buf[n..].fill(0);
        }
        Ok(())
    }

    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
        if self.read_only {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Disk image is opened read-only",
            ));
        }
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.read_only {
            // ZFS/UFS durability: virtio FLUSH must reach stable storage, not only userspace buffers.
            self.file.flush()?;
            self.file.sync_all()?;
        }
        Ok(())
    }
}

const COW_MAGIC: &[u8; 8] = b"VMMCOW01";
const COW_CLUSTER_SIZE: u64 = 65536; // 64 KB clusters

/// Copy-On-Write overlay backend. The base image is kept strictly read-only.
/// Any sector writes are redirected to cluster blocks in an overlay file.
pub struct CowBackend {
    base_file: File,
    overlay_file: File,
    base_size: u64,
    cluster_table: Vec<u64>,
}

impl CowBackend {
    pub fn open(base_path: &str, overlay_path: &str) -> Result<Self, String> {
        let base_file = OpenOptions::new()
            .read(true)
            .open(base_path)
            .map_err(|e| format!("Failed to open base disk '{}': {}", base_path, e))?;

        let base_size = base_file
            .metadata()
            .map_err(|e| format!("Failed to query base disk metadata: {}", e))?
            .len();

        let num_clusters = ((base_size + COW_CLUSTER_SIZE - 1) / COW_CLUSTER_SIZE) as usize;

        let mut overlay_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(overlay_path)
            .map_err(|e| format!("Failed to open overlay file '{}': {}", overlay_path, e))?;

        let overlay_len = overlay_file
            .metadata()
            .map_err(|e| format!("Failed to query overlay metadata: {}", e))?
            .len();

        let table_start = 32u64;
        let table_bytes = (num_clusters * 8) as u64;
        let initial_header_size = table_start + table_bytes;

        let mut cluster_table = vec![0u64; num_clusters];

        if overlay_len >= initial_header_size {
            let mut magic = [0u8; 8];
            overlay_file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
            overlay_file.read_exact(&mut magic).map_err(|e| e.to_string())?;
            if &magic != COW_MAGIC {
                return Err(format!("Invalid COW overlay magic in '{}'", overlay_path));
            }

            overlay_file.seek(SeekFrom::Start(table_start)).map_err(|e| e.to_string())?;
            for entry in cluster_table.iter_mut() {
                let mut buf = [0u8; 8];
                overlay_file.read_exact(&mut buf).map_err(|e| e.to_string())?;
                *entry = u64::from_le_bytes(buf);
            }
        } else {
            overlay_file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
            overlay_file.write_all(COW_MAGIC).map_err(|e| e.to_string())?;
            overlay_file.write_all(&(COW_CLUSTER_SIZE as u32).to_le_bytes()).map_err(|e| e.to_string())?;
            overlay_file.write_all(&0u32.to_le_bytes()).map_err(|e| e.to_string())?;
            overlay_file.write_all(&base_size.to_le_bytes()).map_err(|e| e.to_string())?;
            overlay_file.write_all(&(num_clusters as u64).to_le_bytes()).map_err(|e| e.to_string())?;

            let zero_table = vec![0u8; table_bytes as usize];
            overlay_file.write_all(&zero_table).map_err(|e| e.to_string())?;
            overlay_file.flush().map_err(|e| e.to_string())?;
        }

        println!(
            "[VirtIO-Blk] Initialized COW overlay '{}' on base '{}' ({:.2} MB, {} clusters)",
            overlay_path,
            base_path,
            base_size as f64 / (1024.0 * 1024.0),
            num_clusters
        );

        Ok(Self {
            base_file,
            overlay_file,
            base_size,
            cluster_table,
        })
    }
}

impl BlockBackend for CowBackend {
    fn size_bytes(&self) -> u64 {
        self.base_size
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn read_exact_at(&mut self, offset: u64, buf: &mut [u8]) -> io::Result<()> {
        let mut curr_offset = offset;
        let mut buf_pos = 0;

        while buf_pos < buf.len() {
            let cluster_idx = (curr_offset / COW_CLUSTER_SIZE) as usize;
            let intra_cluster = curr_offset % COW_CLUSTER_SIZE;
            let chunk = std::cmp::min(
                (buf.len() - buf_pos) as u64,
                COW_CLUSTER_SIZE - intra_cluster,
            ) as usize;

            if cluster_idx < self.cluster_table.len() && self.cluster_table[cluster_idx] != 0 {
                let overlay_offset = self.cluster_table[cluster_idx] + intra_cluster;
                self.overlay_file.seek(SeekFrom::Start(overlay_offset))?;
                self.overlay_file.read_exact(&mut buf[buf_pos..buf_pos + chunk])?;
            } else {
                if curr_offset < self.base_size {
                    self.base_file.seek(SeekFrom::Start(curr_offset))?;
                    let n = self.base_file.read(&mut buf[buf_pos..buf_pos + chunk])?;
                    if n < chunk {
                        buf[buf_pos + n..buf_pos + chunk].fill(0);
                    }
                } else {
                    buf[buf_pos..buf_pos + chunk].fill(0);
                }
            }

            curr_offset += chunk as u64;
            buf_pos += chunk;
        }

        Ok(())
    }

    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
        let mut curr_offset = offset;
        let mut buf_pos = 0;

        while buf_pos < buf.len() {
            let cluster_idx = (curr_offset / COW_CLUSTER_SIZE) as usize;
            if cluster_idx >= self.cluster_table.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Write out of bounds",
                ));
            }

            let intra_cluster = curr_offset % COW_CLUSTER_SIZE;
            let chunk = std::cmp::min(
                (buf.len() - buf_pos) as u64,
                COW_CLUSTER_SIZE - intra_cluster,
            ) as usize;

            if self.cluster_table[cluster_idx] == 0 {
                let new_cluster_offset = self.overlay_file.seek(SeekFrom::End(0))?;

                let mut cluster_buf = vec![0u8; COW_CLUSTER_SIZE as usize];
                let base_cluster_start = (cluster_idx as u64) * COW_CLUSTER_SIZE;
                if base_cluster_start < self.base_size {
                    self.base_file.seek(SeekFrom::Start(base_cluster_start))?;
                    let n = self.base_file.read(&mut cluster_buf)?;
                    if n < cluster_buf.len() {
                        cluster_buf[n..].fill(0);
                    }
                }

                cluster_buf[intra_cluster as usize..intra_cluster as usize + chunk]
                    .copy_from_slice(&buf[buf_pos..buf_pos + chunk]);

                self.overlay_file.write_all(&cluster_buf)?;
                self.cluster_table[cluster_idx] = new_cluster_offset;

                let entry_pos = 32 + (cluster_idx as u64) * 8;
                self.overlay_file.seek(SeekFrom::Start(entry_pos))?;
                self.overlay_file.write_all(&new_cluster_offset.to_le_bytes())?;
            } else {
                let target_offset = self.cluster_table[cluster_idx] + intra_cluster;
                self.overlay_file.seek(SeekFrom::Start(target_offset))?;
                self.overlay_file.write_all(&buf[buf_pos..buf_pos + chunk])?;
            }

            curr_offset += chunk as u64;
            buf_pos += chunk;
        }

        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.overlay_file.flush()?;
        self.overlay_file.sync_all()
    }
}

pub struct VirtioBlock {
    backend: Option<Box<dyn BlockBackend>>,
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
    serial_id: String,
    mem: Arc<GuestMemory>,
}

impl VirtioBlock {
    pub fn new(
        backend: Option<Box<dyn BlockBackend>>,
        irq_num: u32,
        serial_id: &str,
        mem: Arc<GuestMemory>,
    ) -> Self {
        let capacity_sectors = backend.as_ref().map(|b| b.size_bytes() / 512).unwrap_or(0);
        Self {
            backend,
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
            serial_id: serial_id.to_string(),
            mem,
        }
    }

    pub fn from_raw(
        path: &str,
        read_only: bool,
        irq_num: u32,
        serial_id: &str,
        mem: Arc<GuestMemory>,
    ) -> Result<Self, String> {
        let backend = RawFileBackend::open(path, read_only)?;
        let size_mb = backend.size_bytes() as f64 / (1024.0 * 1024.0);
        println!(
            "[VirtIO-Blk] Opened disk '{}' ({}): {:.2} MB ({} sectors)",
            path,
            if backend.is_read_only() { "read-only" } else { "read-write" },
            size_mb,
            backend.size_bytes() / 512
        );
        Ok(Self::new(Some(Box::new(backend)), irq_num, serial_id, mem))
    }

    pub fn from_cow(
        base_path: &str,
        overlay_path: &str,
        irq_num: u32,
        serial_id: &str,
        mem: Arc<GuestMemory>,
    ) -> Result<Self, String> {
        let backend = CowBackend::open(base_path, overlay_path)?;
        Ok(Self::new(Some(Box::new(backend)), irq_num, serial_id, mem))
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
        let mut i = 0u16;
        for _ in 0..max {
            let Some((addr, len, flags, next)) = self.read_desc(table, i) else {
                break;
            };
            descriptors.push((addr, len, flags));
            if (flags & VRING_DESC_F_NEXT) == 0 {
                break;
            }
            if next >= max {
                break;
            }
            i = next;
        }
        descriptors
    }

    fn handle_request(&mut self, head_desc: u16, q_size: u32) -> u32 {
        let descriptors = self.collect_descriptors(head_desc, q_size);
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
                if let Some(ref mut backend) = self.backend {
                    let mut file_offset = sector * 512;
                    for i in 1..descriptors.len() - 1 {
                        let (buf_addr, buf_len, _) = descriptors[i];
                        let mut temp_buf = vec![0u8; buf_len as usize];
                        if backend.read_exact_at(file_offset, &mut temp_buf).is_ok() {
                            let _ = self.mem.write(buf_addr, &temp_buf);
                            total_bytes += buf_len;
                            file_offset += buf_len as u64;
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
                if let Some(ref mut backend) = self.backend {
                    let mut file_offset = sector * 512;
                    for i in 1..descriptors.len() - 1 {
                        let (buf_addr, buf_len, _) = descriptors[i];
                        let mut temp_buf = vec![0u8; buf_len as usize];
                        if self.mem.read(buf_addr, &mut temp_buf).is_ok() {
                            if backend.write_all_at(file_offset, &temp_buf).is_ok() {
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
                if let Some(ref mut backend) = self.backend {
                    if backend.flush().is_err() {
                        status = VIRTIO_BLK_S_IOERR;
                    }
                }
            }
            VIRTIO_BLK_T_GET_ID => {
                let mut id_bytes = [0u8; 20];
                let s_bytes = self.serial_id.as_bytes();
                let copy_len = s_bytes.len().min(20);
                id_bytes[..copy_len].copy_from_slice(&s_bytes[..copy_len]);
                for i in 1..descriptors.len() - 1 {
                    let (buf_addr, buf_len, _) = descriptors[i];
                    let copy_len = (buf_len as usize).min(id_bytes.len());
                    let _ = self.mem.write(buf_addr, &id_bytes[..copy_len]);
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
                let mut features = VIRTIO_BLK_F_SEG_MAX
                    | VIRTIO_BLK_F_BLK_SIZE
                    | VIRTIO_BLK_F_FLUSH
                    | VIRTIO_RING_F_INDIRECT_DESC
                    | VIRTIO_F_VERSION_1;
                if let Some(ref b) = self.backend {
                    if b.is_read_only() {
                        features |= VIRTIO_BLK_F_RO;
                    }
                }
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
