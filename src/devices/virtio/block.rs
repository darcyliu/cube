//! VirtIO Block Device (`virtio-blk`) implementation over Modern MMIO.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static BLK_IOS: AtomicU64 = AtomicU64::new(0);

use super::*;
use crate::devices::MmioDevice;
use crate::memory::GuestMemory;

// VirtIO Block Request Types
const VIRTIO_BLK_T_IN: u32 = 0; // Read from disk
const VIRTIO_BLK_T_OUT: u32 = 1; // Write to disk
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

fn check_disk_range(size: u64, offset: u64, len: usize) -> io::Result<()> {
    if offset > size || len as u64 > size - offset {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Disk request outside capacity",
        ))
    } else {
        Ok(())
    }
}

/// Standard raw disk file backend.
pub struct RawFileBackend {
    file: File,
    size: u64,
    read_only: bool,
}

impl Drop for RawFileBackend {
    fn drop(&mut self) {
        // A concurrently spawned child can briefly inherit the descriptor.
        // Release ownership now rather than waiting for its copy to close.
        let _ = self.file.unlock();
    }
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

        if ro {
            file.try_lock_shared()
                .map_err(|e| format!("Disk '{path}' is in use: {e}"))?;
        } else {
            file.try_lock()
                .map_err(|e| format!("Disk '{path}' is in use: {e}"))?;
        }
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
        check_disk_range(self.size, offset, buf.len())?;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(buf)
    }

    fn write_all_at(&mut self, offset: u64, buf: &[u8]) -> io::Result<()> {
        if self.read_only {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "Disk image is opened read-only",
            ));
        }
        check_disk_range(self.size, offset, buf.len())?;
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

impl Drop for CowBackend {
    fn drop(&mut self) {
        let _ = self.overlay_file.unlock();
        let _ = self.base_file.unlock();
    }
}

impl CowBackend {
    pub fn open(base_path: &str, overlay_path: &str) -> Result<Self, String> {
        let base_file = OpenOptions::new()
            .read(true)
            .open(base_path)
            .map_err(|e| format!("Failed to open base disk '{}': {}", base_path, e))?;

        base_file
            .try_lock_shared()
            .map_err(|e| format!("Base disk is in use: {e}"))?;
        let base_size = base_file
            .metadata()
            .map_err(|e| format!("Failed to query base disk metadata: {}", e))?
            .len();

        let num_clusters = usize::try_from(base_size.div_ceil(COW_CLUSTER_SIZE))
            .ok()
            .filter(|&n| n > 0 && n <= (1 << 24))
            .ok_or("Unsupported COW image size (maximum 1 TiB)")?;

        let mut overlay_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(overlay_path)
            .map_err(|e| format!("Failed to open overlay file '{}': {}", overlay_path, e))?;

        overlay_file
            .try_lock()
            .map_err(|e| format!("Overlay is in use: {e}"))?;
        let overlay_len = overlay_file
            .metadata()
            .map_err(|e| format!("Failed to query overlay metadata: {}", e))?
            .len();

        let table_start = 32u64;
        let table_bytes = (num_clusters * 8) as u64;
        let initial_header_size = table_start + table_bytes;

        let mut cluster_table = vec![0u64; num_clusters];

        if overlay_len != 0 {
            if overlay_len < initial_header_size
                || (overlay_len - initial_header_size) % COW_CLUSTER_SIZE != 0
            {
                return Err("Truncated COW overlay".into());
            }
            let mut header = [0; 32];
            overlay_file
                .read_exact(&mut header)
                .map_err(|e| e.to_string())?;
            if &header[..8] != COW_MAGIC
                || u32::from_le_bytes(header[8..12].try_into().unwrap()) != COW_CLUSTER_SIZE as u32
                || u32::from_le_bytes(header[12..16].try_into().unwrap()) != 0
                || u64::from_le_bytes(header[16..24].try_into().unwrap()) != base_size
                || u64::from_le_bytes(header[24..32].try_into().unwrap()) != num_clusters as u64
            {
                return Err("COW header/geometry does not match base image".into());
            }
            let mut allocated = std::collections::HashSet::new();
            overlay_file
                .seek(SeekFrom::Start(table_start))
                .map_err(|e| e.to_string())?;
            for entry in cluster_table.iter_mut() {
                let mut buf = [0u8; 8];
                overlay_file
                    .read_exact(&mut buf)
                    .map_err(|e| e.to_string())?;
                *entry = u64::from_le_bytes(buf);
                if *entry != 0
                    && (*entry < initial_header_size
                        || (*entry - initial_header_size) % COW_CLUSTER_SIZE != 0
                        || entry
                            .checked_add(COW_CLUSTER_SIZE)
                            .is_none_or(|end| end > overlay_len)
                        || !allocated.insert(*entry))
                {
                    return Err("Invalid or overlapping COW cluster offset".into());
                }
            }
        } else {
            overlay_file
                .seek(SeekFrom::Start(0))
                .map_err(|e| e.to_string())?;
            overlay_file
                .write_all(COW_MAGIC)
                .map_err(|e| e.to_string())?;
            overlay_file
                .write_all(&(COW_CLUSTER_SIZE as u32).to_le_bytes())
                .map_err(|e| e.to_string())?;
            overlay_file
                .write_all(&0u32.to_le_bytes())
                .map_err(|e| e.to_string())?;
            overlay_file
                .write_all(&base_size.to_le_bytes())
                .map_err(|e| e.to_string())?;
            overlay_file
                .write_all(&(num_clusters as u64).to_le_bytes())
                .map_err(|e| e.to_string())?;

            let zero_table = vec![0u8; table_bytes as usize];
            overlay_file
                .write_all(&zero_table)
                .map_err(|e| e.to_string())?;
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
        check_disk_range(self.base_size, offset, buf.len())?;
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
                self.overlay_file
                    .read_exact(&mut buf[buf_pos..buf_pos + chunk])?;
            } else {
                if curr_offset < self.base_size {
                    self.base_file.seek(SeekFrom::Start(curr_offset))?;
                    self.base_file
                        .read_exact(&mut buf[buf_pos..buf_pos + chunk])?;
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
        check_disk_range(self.base_size, offset, buf.len())?;
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
                    let n = (self.base_size - base_cluster_start).min(COW_CLUSTER_SIZE) as usize;
                    self.base_file.read_exact(&mut cluster_buf[..n])?;
                }

                cluster_buf[intra_cluster as usize..intra_cluster as usize + chunk]
                    .copy_from_slice(&buf[buf_pos..buf_pos + chunk]);

                self.overlay_file.write_all(&cluster_buf)?;
                // Persist data before publishing the mapping. A crash must not
                // leave a durable mapping to an uninitialized cluster.
                self.overlay_file.sync_data()?;

                let entry_pos = 32 + (cluster_idx as u64) * 8;
                self.overlay_file.seek(SeekFrom::Start(entry_pos))?;
                self.overlay_file
                    .write_all(&new_cluster_offset.to_le_bytes())?;
                self.cluster_table[cluster_idx] = new_cluster_offset;
            } else {
                let target_offset = self.cluster_table[cluster_idx] + intra_cluster;
                self.overlay_file.seek(SeekFrom::Start(target_offset))?;
                self.overlay_file
                    .write_all(&buf[buf_pos..buf_pos + chunk])?;
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
            if backend.is_read_only() {
                "read-only"
            } else {
                "read-write"
            },
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
                    eprintln!("[VirtIO-Blk Error] Failed to read avail ring idx: {}", e);
                    break;
                }
            };
            if self.queue.last_avail_idx == avail_idx {
                break;
            }

            let ring_elem_offset =
                self.queue.avail_ring + 4 + 2 * (self.queue.last_avail_idx as u64 % q_size as u64);
            let head_desc = match self.mem.read_u16(ring_elem_offset) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!(
                        "[VirtIO-Blk Error] Failed to read avail ring element: {}",
                        e
                    );
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

    fn handle_request(&mut self, head_desc: u16, q_size: u32) -> u32 {
        let _ = q_size;
        let descriptors = match self.queue.descriptors(
            &self.mem,
            head_desc,
            self.driver_features & VIRTIO_RING_F_INDIRECT_DESC != 0,
            MAX_CHAIN_BYTES,
        ) {
            Ok(d) => d
                .into_iter()
                .map(|d| (d.addr, d.len, d.flags))
                .collect::<Vec<_>>(),
            Err(_) => {
                self.status |= VIRTIO_STATUS_NEEDS_RESET;
                return 0;
            }
        };
        if descriptors.len() < 2 {
            self.status |= VIRTIO_STATUS_NEEDS_RESET;
            return 0;
        }

        let (hdr_addr, hdr_len, hdr_flags) = descriptors[0];
        let (status_addr, status_len, status_flags) = descriptors[descriptors.len() - 1];
        if status_len < 1 || status_flags & VRING_DESC_F_WRITE == 0 {
            self.status |= VIRTIO_STATUS_NEEDS_RESET;
            return 0;
        }
        let reject = || {
            let _ = self.mem.write(status_addr, &[VIRTIO_BLK_S_IOERR]);
            1
        };
        if hdr_len < 16 || hdr_flags & VRING_DESC_F_WRITE != 0 {
            return reject();
        }
        let req_type = self.mem.read_u32(hdr_addr).unwrap_or(u32::MAX);
        let sector = self.mem.read_u64(hdr_addr + 8).unwrap_or(u64::MAX);
        let data = &descriptors[1..descriptors.len() - 1];
        let total_len: usize = data.iter().map(|d| d.1 as usize).sum();
        if data.iter().any(|d| d.1 > 1024 * 1024) {
            return reject();
        }
        if matches!(
            req_type,
            VIRTIO_BLK_T_IN | VIRTIO_BLK_T_OUT | VIRTIO_BLK_T_GET_ID
        ) {
            let writable = req_type != VIRTIO_BLK_T_OUT;
            if data
                .iter()
                .any(|d| (d.2 & VRING_DESC_F_WRITE != 0) != writable)
            {
                return reject();
            }
        }
        let offset = sector.checked_mul(512);
        if matches!(req_type, VIRTIO_BLK_T_IN | VIRTIO_BLK_T_OUT) {
            if total_len % 512 != 0
                || offset.is_none_or(|o| {
                    check_disk_range(self.capacity_sectors * 512, o, total_len).is_err()
                })
            {
                return reject();
            }
        }

        let mut total_bytes = 0u32;
        let mut status = VIRTIO_BLK_S_OK;

        match req_type {
            VIRTIO_BLK_T_IN => {
                if let Some(ref mut backend) = self.backend {
                    let mut file_offset = offset.unwrap();
                    for i in 1..descriptors.len() - 1 {
                        let (buf_addr, buf_len, _) = descriptors[i];
                        let mut temp_buf = vec![0u8; buf_len as usize];
                        if backend.read_exact_at(file_offset, &mut temp_buf).is_ok() {
                            if self.mem.write(buf_addr, &temp_buf).is_err() {
                                status = VIRTIO_BLK_S_IOERR;
                                break;
                            }
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
                    let mut file_offset = offset.unwrap();
                    for i in 1..descriptors.len() - 1 {
                        let (buf_addr, buf_len, _) = descriptors[i];
                        let mut temp_buf = vec![0u8; buf_len as usize];
                        if self.mem.read(buf_addr, &mut temp_buf).is_ok() {
                            if backend.write_all_at(file_offset, &temp_buf).is_ok() {
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

        total_bytes + 1 // status byte is device-written too
    }
}

impl MmioDevice for VirtioBlock {
    fn shutdown(&mut self) -> Result<(), String> {
        if let Some(backend) = &mut self.backend {
            backend.flush().map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        match offset {
            VIRTIO_MMIO_MAGIC_VALUE => VIRTIO_MMIO_MAGIC as u64,
            VIRTIO_MMIO_VERSION => VIRTIO_MMIO_VERSION_2 as u64,
            VIRTIO_MMIO_DEVICE_ID => VIRTIO_DEV_BLOCK as u64,
            VIRTIO_MMIO_VENDOR_ID => VIRTIO_MMIO_VENDOR as u64,
            VIRTIO_MMIO_DEVICE_FEATURES => {
                let mut features = VIRTIO_BLK_F_SIZE_MAX
                    | VIRTIO_BLK_F_SEG_MAX
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
                cfg[8..12].copy_from_slice(&(1024u32 * 1024).to_le_bytes());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestDir, name};

    #[test]
    fn raw_bounds_and_exclusive_writer() {
        let dir = TestDir::new();
        let path = dir.file("disk", &[0x11; 1024]);
        let mut disk = RawFileBackend::open(name(&path), false).unwrap();
        assert!(RawFileBackend::open(name(&path), false).is_err());
        assert!(RawFileBackend::open(name(&path), true).is_err());
        assert!(disk.write_all_at(1024, &[9; 512]).is_err());
        assert!(disk.read_exact_at(u64::MAX, &mut [0; 2]).is_err());
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 1024);
        disk.write_all_at(512, &[7; 512]).unwrap();
        disk.flush().unwrap();
        let inherited = disk.file.try_clone().unwrap();
        drop(disk);
        let a = RawFileBackend::open(name(&path), true).unwrap();
        assert!(RawFileBackend::open(name(&path), true).is_ok());
        drop(a);
        drop(inherited);
    }

    #[test]
    fn cow_rejects_truncated_header_without_modifying_it() {
        let dir = TestDir::new();
        let base = dir.file("disk", &[0; 65536]);
        let overlay = dir.file("cow", COW_MAGIC);
        assert!(CowBackend::open(name(&base), name(&overlay)).is_err());
        assert_eq!(std::fs::read(overlay).unwrap(), COW_MAGIC);
    }

    #[test]
    fn cow_checks_geometry_and_cluster_offsets() {
        let dir = TestDir::new();
        let base = dir.file("disk", &[1; 65536]);
        let overlay = dir.path("cow");
        drop(CowBackend::open(name(&base), name(&overlay)).unwrap());
        OpenOptions::new()
            .write(true)
            .open(&base)
            .unwrap()
            .set_len(32768)
            .unwrap();
        assert!(CowBackend::open(name(&base), name(&overlay)).is_err());
        OpenOptions::new()
            .write(true)
            .open(&base)
            .unwrap()
            .set_len(65536)
            .unwrap();
        let mut file = OpenOptions::new().write(true).open(&overlay).unwrap();
        file.seek(SeekFrom::Start(32)).unwrap();
        file.write_all(&1u64.to_le_bytes()).unwrap();
        assert!(CowBackend::open(name(&base), name(&overlay)).is_err());
    }

    #[test]
    fn cow_roundtrip_preserves_base_and_locks_overlay() {
        let dir = TestDir::new();
        let base = dir.file("disk", &vec![3; 100000]);
        let overlay = dir.path("cow");
        let mut disk = CowBackend::open(name(&base), name(&overlay)).unwrap();
        assert!(CowBackend::open(name(&base), name(&overlay)).is_err());
        assert!(RawFileBackend::open(name(&base), false).is_err());
        disk.write_all_at(65000, &[7; 1024]).unwrap();
        assert!(disk.write_all_at(99999, &[7; 2]).is_err());
        disk.flush().unwrap();
        drop(disk);
        let mut disk = CowBackend::open(name(&base), name(&overlay)).unwrap();
        let mut data = vec![0; 100000];
        disk.read_exact_at(0, &mut data).unwrap();
        assert!(data[..65000].iter().all(|&b| b == 3));
        assert!(data[65000..66024].iter().all(|&b| b == 7));
        assert!(data[66024..].iter().all(|&b| b == 3));
        assert!(std::fs::read(base).unwrap().iter().all(|&b| b == 3));
    }

    #[test]
    fn huge_sector_and_wrong_descriptor_direction_are_rejected_before_io() {
        let dir = TestDir::new();
        let path = dir.file("disk", &[0x11; 1024]);
        let mem = Arc::new(GuestMemory::new_test(0, 0x10000));
        let mut disk = VirtioBlock::from_raw(name(&path), false, 34, "test", mem.clone()).unwrap();
        disk.queue.ready = true;
        disk.queue.desc_table = 0x1000;
        disk.queue.avail_ring = 0x1800;
        disk.queue.used_ring = 0x1900;
        for (idx, addr, len, flags, next) in [
            (0, 0x2000, 16, 1, 1),
            (1, 0x3000, 512, 1, 2),
            (2, 0x4000, 1, 2, 0),
        ] {
            let d = 0x1000 + idx * 16;
            mem.write_u64(d, addr).unwrap();
            mem.write_u32(d + 8, len).unwrap();
            mem.write_u16(d + 12, flags).unwrap();
            mem.write_u16(d + 14, next).unwrap();
        }
        mem.write_u32(0x2000, VIRTIO_BLK_T_OUT).unwrap();
        mem.write_u64(0x2008, 1u64 << 55).unwrap();
        mem.write(0x3000, &[0x99; 512]).unwrap();
        assert_eq!(disk.handle_request(0, 128), 1);
        assert_eq!(mem.read_u16(0x4000).unwrap(), VIRTIO_BLK_S_IOERR as u16);
        assert_eq!(std::fs::read(&path).unwrap(), [0x11; 1024]);
        mem.write_u64(0x2008, 0).unwrap();
        mem.write_u16(0x101c, VRING_DESC_F_NEXT | VRING_DESC_F_WRITE)
            .unwrap();
        assert_eq!(disk.handle_request(0, 128), 1);
        assert_eq!(std::fs::read(&path).unwrap(), [0x11; 1024]);
        mem.write_u16(0x101c, VRING_DESC_F_NEXT).unwrap();
        assert_eq!(disk.handle_request(0, 128), 1);
        assert_eq!(mem.read_u16(0x4000).unwrap(), VIRTIO_BLK_S_OK as u16);
        assert_eq!(std::fs::read(path).unwrap()[0], 0x99);
    }
}
