//! VirtIO Block Device (`virtio-blk`) implementation over Modern MMIO.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static BLK_IOS: AtomicU64 = AtomicU64::new(0);

use super::*;
use crate::devices::MmioDevice;
use crate::memory::GuestMemory;
use cube::cow::{self, CLUSTER_SIZE as COW_CLUSTER_SIZE, MAGIC as COW_MAGIC};

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

        let layout = cow::Layout::expected(base_size)?;
        let num_clusters = layout.table.len();

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

        let table_bytes = (num_clusters * 8) as u64;
        let cluster_table = if overlay_len != 0 {
            cow::read_layout(&mut overlay_file, base_size)?.table
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
            layout.table
        };

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

struct BlockInner {
    backend: Option<Box<dyn BlockBackend>>,
    capacity_sectors: u64,
    transport: MmioTransport,
    serial_id: String,
    mem: Arc<GuestMemory>,
    read_only: bool,
    queue_count: u32,
    next_queue: usize,
    work_pending: bool,
}

pub struct VirtioBlock {
    inner: Arc<Mutex<BlockInner>>,
    wake: Arc<Condvar>,
    running: Arc<AtomicBool>,
    workers: Vec<thread::JoinHandle<()>>,
}

use std::sync::atomic::AtomicBool;
use std::sync::{Condvar, Mutex};
use std::thread;

use super::transport::WriteEffect;

const VIRTIO_BLK_F_MQ: u64 = 1 << 12;
const VTBLK_NUM_QUEUES_OFFSET: usize = 34;

impl BlockInner {
    fn signal_irq(&mut self) {
        self.transport.raise_interrupt(1);
        unsafe {
            crate::hypervisor::hv_gic_set_spi(self.transport.irq(), true);
        }
        crate::vcpu::kick_vcpu();
    }

    fn process_queue(&mut self, index: usize) {
        if !self.transport.operational() {
            return;
        }
        let Some(queue) = self.transport.queue(index).copied() else {
            return;
        };
        if !queue.ready || queue.avail_ring == 0 || queue.desc_table == 0 || queue.size == 0 {
            return;
        }
        let epoch = self.transport.queue_epoch(index);
        let count = match queue.pending_count(&self.mem) {
            Ok(n) => n,
            Err(_) => {
                self.transport.set_needs_reset();
                return;
            }
        };
        let mut queue = *self.transport.queue(index).unwrap();
        let mut processed_any = false;
        for _ in 0..count {
            if self.transport.queue_epoch(index) != epoch {
                break;
            }
            std::sync::atomic::fence(Ordering::SeqCst);
            let avail_idx = match self.mem.read_u16(queue.avail_ring + 2) {
                Ok(idx) => idx,
                Err(e) => {
                    eprintln!("[VirtIO-Blk Error] Failed to read avail ring idx: {e}");
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
                    eprintln!("[VirtIO-Blk Error] Failed to read avail ring element: {e}");
                    break;
                }
            };
            let bytes = self.handle_request(index, head, &mut queue);
            if self.transport.queue_epoch(index) != epoch {
                break;
            }
            publish_used(&self.mem, &mut queue, head, bytes);
            queue.last_avail_idx = queue.last_avail_idx.wrapping_add(1);
            self.transport.diagnostics_mut().completions += 1;
            self.transport.diagnostics_mut().bytes += bytes as u64;
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
                        "ios={n} q={index} last_avail={} used_idx={} qsize={}",
                        queue.last_avail_idx, queue.used_idx, queue.size
                    );
                }
            }
        }
        *self.transport.queue_mut(index).unwrap() = queue;
        if processed_any {
            // vtblk_ident() busy-polls GET_ID, but later firmware waits on the IRQ.
            // Skipping the line when NO_INTERRUPT is set wedged attach.
            self.signal_irq();
        }
    }

    fn process_one(&mut self) -> bool {
        if !self.transport.operational() {
            return false;
        }
        let count = self.queue_count as usize;
        for step in 0..count {
            let index = (self.next_queue + step) % count;
            let pending = self
                .transport
                .queue(index)
                .and_then(|q| q.pending_count(&self.mem).ok())
                .unwrap_or(0);
            if pending == 0 {
                continue;
            }
            let before = self.transport.queue(index).unwrap().last_avail_idx;
            self.process_queue(index);
            let after = self.transport.queue(index).unwrap().last_avail_idx;
            if before != after {
                self.next_queue = (index + 1) % count;
                return true;
            }
        }
        false
    }

    fn handle_request(&mut self, _index: usize, head_desc: u16, queue: &mut VirtQueue) -> u32 {
        let indirect = self.transport.negotiated(VIRTIO_RING_F_INDIRECT_DESC);
        let descriptors = match queue.descriptors(&self.mem, head_desc, indirect, MAX_CHAIN_BYTES) {
            Ok(d) => d,
            Err(_) => {
                self.transport.set_needs_reset();
                return 0;
            }
        };
        if descriptors.len() < 2 {
            self.transport.set_needs_reset();
            return 0;
        }
        let hdr = descriptors[0];
        let status_desc = descriptors[descriptors.len() - 1];
        if status_desc.len < 1 || status_desc.flags & VRING_DESC_F_WRITE == 0 {
            self.transport.set_needs_reset();
            return 0;
        }
        let reject = |mem: &GuestMemory| {
            let _ = mem.write(status_desc.addr, &[VIRTIO_BLK_S_IOERR]);
            1
        };
        if hdr.len < 16 || hdr.flags & VRING_DESC_F_WRITE != 0 {
            return reject(&self.mem);
        }
        let req_type = self.mem.read_u32(hdr.addr).unwrap_or(u32::MAX);
        let sector = self.mem.read_u64(hdr.addr + 8).unwrap_or(u64::MAX);
        let data = &descriptors[1..descriptors.len() - 1];
        let total_len: usize = data.iter().map(|d| d.len as usize).sum();
        if data.iter().any(|d| d.len > 1024 * 1024) {
            return reject(&self.mem);
        }
        if matches!(
            req_type,
            VIRTIO_BLK_T_IN | VIRTIO_BLK_T_OUT | VIRTIO_BLK_T_GET_ID
        ) {
            let writable = req_type != VIRTIO_BLK_T_OUT;
            if data
                .iter()
                .any(|d| (d.flags & VRING_DESC_F_WRITE != 0) != writable)
            {
                return reject(&self.mem);
            }
        }
        let offset = sector.checked_mul(512);
        if matches!(req_type, VIRTIO_BLK_T_IN | VIRTIO_BLK_T_OUT)
            && (total_len % 512 != 0
                || offset.is_none_or(|o| {
                    check_disk_range(self.capacity_sectors * 512, o, total_len).is_err()
                }))
        {
            return reject(&self.mem);
        }
        let mut total_bytes = 0u32;
        let mut status = VIRTIO_BLK_S_OK;
        match req_type {
            VIRTIO_BLK_T_IN => {
                if let Some(ref mut backend) = self.backend {
                    let mut file_offset = offset.unwrap();
                    for desc in data {
                        let mut temp_buf = vec![0u8; desc.len as usize];
                        if backend.read_exact_at(file_offset, &mut temp_buf).is_ok() {
                            if self.mem.write(desc.addr, &temp_buf).is_err() {
                                status = VIRTIO_BLK_S_IOERR;
                                break;
                            }
                            total_bytes += desc.len;
                            file_offset += desc.len as u64;
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
                    for desc in data {
                        let mut temp_buf = vec![0u8; desc.len as usize];
                        if self.mem.read(desc.addr, &mut temp_buf).is_ok() {
                            if backend.write_all_at(file_offset, &temp_buf).is_ok() {
                                file_offset += desc.len as u64;
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
                for desc in data {
                    let n = (desc.len as usize).min(id_bytes.len());
                    let _ = self.mem.write(desc.addr, &id_bytes[..n]);
                    total_bytes += n as u32;
                }
            }
            _ => status = VIRTIO_BLK_S_UNSUPP,
        }
        let _ = self.mem.write(status_desc.addr, &[status]);
        total_bytes + 1
    }

    fn config_byte(&self, index: usize) -> u8 {
        let mut cfg = [0u8; 64];
        cfg[0..8].copy_from_slice(&self.capacity_sectors.to_le_bytes());
        cfg[8..12].copy_from_slice(&(1024u32 * 1024).to_le_bytes());
        cfg[12..16].copy_from_slice(&VTBLK_SEG_MAX.to_le_bytes());
        cfg[20..24].copy_from_slice(&512u32.to_le_bytes());
        cfg[VTBLK_NUM_QUEUES_OFFSET..VTBLK_NUM_QUEUES_OFFSET + 2]
            .copy_from_slice(&(self.queue_count as u16).to_le_bytes());
        cfg.get(index).copied().unwrap_or(0)
    }
}

impl VirtioBlock {
    pub fn new(
        backend: Option<Box<dyn BlockBackend>>,
        irq_num: u32,
        serial_id: &str,
        mem: Arc<GuestMemory>,
    ) -> Self {
        Self::with_queues(backend, irq_num, serial_id, mem, 1)
    }

    pub fn with_queues(
        backend: Option<Box<dyn BlockBackend>>,
        irq_num: u32,
        serial_id: &str,
        mem: Arc<GuestMemory>,
        queue_count: u32,
    ) -> Self {
        let queue_count = queue_count.clamp(1, 16);
        let read_only = backend.as_ref().is_some_and(|b| b.is_read_only());
        let capacity_sectors = backend.as_ref().map(|b| b.size_bytes() / 512).unwrap_or(0);
        let mut offered = VIRTIO_BLK_F_SIZE_MAX
            | VIRTIO_BLK_F_SEG_MAX
            | VIRTIO_BLK_F_BLK_SIZE
            | VIRTIO_BLK_F_FLUSH
            | VIRTIO_RING_F_INDIRECT_DESC
            | VIRTIO_F_VERSION_1;
        if read_only {
            offered |= VIRTIO_BLK_F_RO;
        }
        if queue_count > 1 {
            offered |= VIRTIO_BLK_F_MQ;
        }
        let sizes = vec![VTBLK_QUEUE_SIZE; queue_count as usize];
        let inner = Arc::new(Mutex::new(BlockInner {
            backend,
            capacity_sectors,
            transport: MmioTransport::new(VIRTIO_DEV_BLOCK, offered, &sizes, irq_num),
            serial_id: serial_id.to_string(),
            mem,
            read_only,
            queue_count,
            next_queue: 0,
            work_pending: false,
        }));
        let wake = Arc::new(Condvar::new());
        let running = Arc::new(AtomicBool::new(true));
        let mut workers = Vec::new();
        if queue_count > 1 {
            let pool = (queue_count as usize).clamp(1, 2);
            for n in 0..pool {
                let shared = Arc::clone(&inner);
                let wake = Arc::clone(&wake);
                let running = Arc::clone(&running);
                workers.push(
                    thread::Builder::new()
                        .name(format!("vmm-blk-{n}"))
                        .spawn(move || {
                            while running.load(Ordering::Relaxed) {
                                let mut guard = shared.lock().unwrap();
                                while running.load(Ordering::Relaxed) && !guard.work_pending {
                                    guard = wake.wait(guard).unwrap();
                                }
                                if !running.load(Ordering::Relaxed) {
                                    break;
                                }
                                if !guard.process_one() {
                                    guard.work_pending = false;
                                }
                            }
                        })
                        .expect("blk worker"),
                );
            }
        }
        Self {
            inner,
            wake,
            running,
            workers,
        }
    }

    pub fn from_raw(
        path: &str,
        read_only: bool,
        irq_num: u32,
        serial_id: &str,
        mem: Arc<GuestMemory>,
    ) -> Result<Self, String> {
        Self::from_raw_queues(path, read_only, irq_num, serial_id, mem, 1)
    }

    pub fn from_raw_queues(
        path: &str,
        read_only: bool,
        irq_num: u32,
        serial_id: &str,
        mem: Arc<GuestMemory>,
        queues: u32,
    ) -> Result<Self, String> {
        let backend = RawFileBackend::open(path, read_only)?;
        let size_mb = backend.size_bytes() as f64 / (1024.0 * 1024.0);
        println!(
            "[VirtIO-Blk] Opened disk '{}' ({}) : {:.2} MB ({} sectors, {} queues)",
            path,
            if backend.is_read_only() {
                "read-only"
            } else {
                "read-write"
            },
            size_mb,
            backend.size_bytes() / 512,
            queues.max(1)
        );
        Ok(Self::with_queues(
            Some(Box::new(backend)),
            irq_num,
            serial_id,
            mem,
            queues,
        ))
    }

    pub fn from_cow(
        base_path: &str,
        overlay_path: &str,
        irq_num: u32,
        serial_id: &str,
        mem: Arc<GuestMemory>,
    ) -> Result<Self, String> {
        Self::from_cow_queues(base_path, overlay_path, irq_num, serial_id, mem, 1)
    }

    pub fn from_cow_queues(
        base_path: &str,
        overlay_path: &str,
        irq_num: u32,
        serial_id: &str,
        mem: Arc<GuestMemory>,
        queues: u32,
    ) -> Result<Self, String> {
        let backend = CowBackend::open(base_path, overlay_path)?;
        Ok(Self::with_queues(
            Some(Box::new(backend)),
            irq_num,
            serial_id,
            mem,
            queues,
        ))
    }

    fn stop_workers(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        self.wake.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

impl Drop for VirtioBlock {
    fn drop(&mut self) {
        self.stop_workers();
    }
}

impl MmioDevice for VirtioBlock {
    fn shutdown(&mut self) -> Result<(), String> {
        self.stop_workers();
        let mut inner = self.inner.lock().unwrap();
        if let Some(backend) = &mut inner.backend {
            backend.flush().map_err(|e| e.to_string())?;
        }
        eprintln!("[VirtIO-Blk] {}", inner.transport.diagnostics_line());
        Ok(())
    }

    fn read(&mut self, offset: u64, size: usize) -> u64 {
        let inner = self.inner.lock().unwrap();
        if offset >= VIRTIO_MMIO_CONFIG {
            let start = (offset - VIRTIO_MMIO_CONFIG) as usize;
            let mut val = 0u64;
            for i in 0..size {
                val |= (inner.config_byte(start + i) as u64) << (i * 8);
            }
            return val;
        }
        inner.transport.read(offset)
    }

    fn write(&mut self, offset: u64, data: u64, _size: usize) {
        let mut inner = self.inner.lock().unwrap();
        let effect = inner.transport.write(offset, data);
        let irq = inner.transport.irq();
        let multi = inner.queue_count > 1;
        match effect {
            WriteEffect::Notify { queue }
                if (queue as usize) < inner.queue_count as usize
                    && (queue == 0 || inner.transport.negotiated(VIRTIO_BLK_F_MQ)) =>
            {
                if multi {
                    inner.work_pending = true;
                    self.wake.notify_one();
                } else {
                    inner.process_queue(queue as usize);
                }
            }
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
        assert!(std::fs::read(&base).unwrap().iter().all(|&b| b == 3));
        drop(disk);
        let mut image = cow::CowImage::open(&base, &overlay).unwrap();
        let exported = dir.path("exported");
        image.export(&exported).unwrap();
        assert_eq!(std::fs::read(exported).unwrap(), data);
    }

    fn arm_queue(inner: &mut BlockInner) {
        let q = inner.transport.queue_mut(0).unwrap();
        q.ready = true;
        q.desc_table = 0x1000;
        q.avail_ring = 0x1800;
        q.used_ring = 0x1900;
    }

    #[test]
    fn huge_sector_and_wrong_descriptor_direction_are_rejected_before_io() {
        let dir = TestDir::new();
        let path = dir.file("disk", &[0x11; 1024]);
        let mem = Arc::new(GuestMemory::new_test(0, 0x10000));
        let disk = VirtioBlock::from_raw(name(&path), false, 34, "test", mem.clone()).unwrap();
        let mut inner = disk.inner.lock().unwrap();
        arm_queue(&mut inner);
        for (idx, addr, len, flags, next) in [
            (0u64, 0x2000u64, 16u32, 1u16, 1u16),
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
        let mut queue = *inner.transport.queue(0).unwrap();
        assert_eq!(inner.handle_request(0, 0, &mut queue), 1);
        assert_eq!(mem.read_u16(0x4000).unwrap(), VIRTIO_BLK_S_IOERR as u16);
        assert_eq!(std::fs::read(&path).unwrap(), [0x11; 1024]);
        mem.write_u64(0x2008, 0).unwrap();
        mem.write_u16(0x101c, VRING_DESC_F_NEXT | VRING_DESC_F_WRITE)
            .unwrap();
        assert_eq!(inner.handle_request(0, 0, &mut queue), 1);
        assert_eq!(std::fs::read(&path).unwrap(), [0x11; 1024]);
        mem.write_u16(0x101c, VRING_DESC_F_NEXT).unwrap();
        assert_eq!(inner.handle_request(0, 0, &mut queue), 1);
        assert_eq!(mem.read_u16(0x4000).unwrap(), VIRTIO_BLK_S_OK as u16);
        assert_eq!(std::fs::read(&path).unwrap()[0], 0x99);
    }

    fn negotiate(disk: &mut VirtioBlock, indirect: bool) {
        disk.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0, 4);
        let low = if indirect {
            VIRTIO_RING_F_INDIRECT_DESC
        } else {
            0
        };
        disk.write(VIRTIO_MMIO_DRIVER_FEATURES, low, 4);
        disk.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1, 4);
        disk.write(VIRTIO_MMIO_DRIVER_FEATURES, 1, 4);
        let status = VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK;
        disk.write(VIRTIO_MMIO_STATUS, status as u64, 4);
    }

    #[test]
    fn indirect_read_write_flush_malformed_and_reset() {
        let dir = TestDir::new();
        let path = dir.file("disk", &[0x11; 1024]);
        let mem = Arc::new(GuestMemory::new_test(0, 0x10000));
        let mut disk =
            VirtioBlock::from_raw(name(&path), false, 34, "serial-1", mem.clone()).unwrap();
        negotiate(&mut disk, true);
        assert!(
            disk.inner
                .lock()
                .unwrap()
                .transport
                .negotiated(VIRTIO_RING_F_INDIRECT_DESC)
        );
        {
            let inner = disk.inner.lock().unwrap();
            assert_eq!(inner.config_byte(VTBLK_NUM_QUEUES_OFFSET), 1);
            assert_eq!(inner.config_byte(0), 2); // 1024/512 = 2 sectors
        }
        disk.write(VIRTIO_MMIO_QUEUE_SEL, 0, 4);
        disk.write(VIRTIO_MMIO_QUEUE_NUM, 3, 4);
        assert_eq!(disk.read(VIRTIO_MMIO_QUEUE_NUM, 4), 0);
        disk.write(VIRTIO_MMIO_QUEUE_NUM, 8, 4);
        disk.write(VIRTIO_MMIO_QUEUE_DESC_LOW, 0x1000, 4);
        disk.write(VIRTIO_MMIO_QUEUE_DRIVER_LOW, 0x1800, 4);
        disk.write(VIRTIO_MMIO_QUEUE_DEVICE_LOW, 0x1900, 4);
        // desc 0: header, direct. desc 1: indirect table of data + status.
        mem.write_u64(0x1000, 0x2000).unwrap();
        mem.write_u32(0x1008, 16).unwrap();
        mem.write_u16(0x100c, VRING_DESC_F_NEXT).unwrap();
        mem.write_u16(0x100e, 1).unwrap();
        mem.write_u64(0x1010, 0x3000).unwrap();
        mem.write_u32(0x1018, 32).unwrap();
        mem.write_u16(0x101c, VRING_DESC_F_INDIRECT).unwrap();
        mem.write_u64(0x3000, 0x4000).unwrap();
        mem.write_u32(0x3008, 512).unwrap();
        mem.write_u16(0x300c, VRING_DESC_F_NEXT).unwrap();
        mem.write_u16(0x300e, 1).unwrap();
        mem.write_u64(0x3010, 0x5000).unwrap();
        mem.write_u32(0x3018, 1).unwrap();
        mem.write_u16(0x301c, VRING_DESC_F_WRITE).unwrap();
        mem.write_u32(0x2000, VIRTIO_BLK_T_OUT).unwrap();
        mem.write_u64(0x2008, 0).unwrap();
        mem.write(0x4000, &[0x5a; 512]).unwrap();
        mem.write_u16(0x1802, 1).unwrap();
        mem.write_u16(0x1804, 0).unwrap();
        disk.write(VIRTIO_MMIO_QUEUE_READY, 1, 4);
        disk.write(VIRTIO_MMIO_QUEUE_NOTIFY, 0, 4);
        assert_eq!(std::fs::read(&path).unwrap()[..512], [0x5a; 512]);
        assert_eq!(mem.read_u16(0x5000).unwrap(), VIRTIO_BLK_S_OK as u16);

        // Read the sector back through a direct chain on a fresh avail slot.
        mem.write_u64(0x1020, 0x2100).unwrap();
        mem.write_u32(0x1028, 16).unwrap();
        mem.write_u16(0x102c, VRING_DESC_F_NEXT).unwrap();
        mem.write_u16(0x102e, 3).unwrap();
        mem.write_u64(0x1030, 0x6000).unwrap();
        mem.write_u32(0x1038, 512).unwrap();
        mem.write_u16(0x103c, VRING_DESC_F_WRITE | VRING_DESC_F_NEXT)
            .unwrap();
        mem.write_u16(0x103e, 4).unwrap();
        mem.write_u64(0x1040, 0x5100).unwrap();
        mem.write_u32(0x1048, 1).unwrap();
        mem.write_u16(0x104c, VRING_DESC_F_WRITE).unwrap();
        mem.write_u32(0x2100, VIRTIO_BLK_T_IN).unwrap();
        mem.write_u64(0x2108, 0).unwrap();
        mem.write_u16(0x1802, 2).unwrap();
        mem.write_u16(0x1806, 2).unwrap();
        disk.write(VIRTIO_MMIO_QUEUE_NOTIFY, 0, 4);
        assert_eq!(
            mem.read(0x6000, &mut [0; 1])
                .ok()
                .map(|_| mem.read_u32(0x6000).unwrap()),
            Some(0x5a5a5a5a)
        );

        // Flush.
        mem.write_u64(0x1050, 0x2200).unwrap();
        mem.write_u32(0x1058, 16).unwrap();
        mem.write_u16(0x105c, VRING_DESC_F_NEXT).unwrap();
        mem.write_u16(0x105e, 6).unwrap();
        mem.write_u64(0x1060, 0x5200).unwrap();
        mem.write_u32(0x1068, 1).unwrap();
        mem.write_u16(0x106c, VRING_DESC_F_WRITE).unwrap();
        mem.write_u32(0x2200, VIRTIO_BLK_T_FLUSH).unwrap();
        mem.write_u16(0x1802, 3).unwrap();
        mem.write_u16(0x1808, 5).unwrap();
        disk.write(VIRTIO_MMIO_QUEUE_NOTIFY, 0, 4);
        assert_eq!(mem.read_u16(0x5200).unwrap(), VIRTIO_BLK_S_OK as u16);

        // Malformed indirect table must not be followed.
        let before = std::fs::read(&path).unwrap();
        mem.write_u64(0x1070, 0x2300).unwrap();
        mem.write_u32(0x1078, 16).unwrap();
        mem.write_u16(0x107c, VRING_DESC_F_INDIRECT | VRING_DESC_F_NEXT)
            .unwrap();
        mem.write_u16(0x1802, 4).unwrap();
        mem.write_u16(0x180a, 7).unwrap();
        disk.write(VIRTIO_MMIO_QUEUE_NOTIFY, 0, 4);
        assert_ne!(
            disk.read(VIRTIO_MMIO_STATUS, 4) & VIRTIO_STATUS_NEEDS_RESET as u64,
            0
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);

        disk.write(VIRTIO_MMIO_STATUS, 0, 4);
        let inner = disk.inner.lock().unwrap();
        assert_eq!(inner.transport.status(), 0);
        assert!(!inner.transport.queue(0).unwrap().ready);
        assert_eq!(inner.transport.queue(0).unwrap().desc_table, 0);
        assert_eq!(inner.transport.driver_features(), 0);
    }

    #[test]
    fn multiqueue_config_and_second_queue_complete_on_that_queue() {
        let dir = TestDir::new();
        let path = dir.file("disk", &[0; 1024]);
        let mem = Arc::new(GuestMemory::new_test(0, 0x20000));
        let mut disk =
            VirtioBlock::from_raw_queues(name(&path), false, 34, "mq", mem.clone(), 2).unwrap();
        assert_eq!(
            disk.read(VIRTIO_MMIO_DEVICE_FEATURES, 4) & VIRTIO_BLK_F_MQ,
            VIRTIO_BLK_F_MQ
        );
        assert_eq!(
            disk.read(VIRTIO_MMIO_CONFIG + VTBLK_NUM_QUEUES_OFFSET as u64, 2),
            2
        );
        disk.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0, 4);
        disk.write(VIRTIO_MMIO_DRIVER_FEATURES, VIRTIO_BLK_F_MQ, 4);
        disk.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1, 4);
        disk.write(VIRTIO_MMIO_DRIVER_FEATURES, 1, 4);
        disk.write(
            VIRTIO_MMIO_STATUS,
            (VIRTIO_STATUS_ACKNOWLEDGE
                | VIRTIO_STATUS_DRIVER
                | VIRTIO_STATUS_FEATURES_OK
                | VIRTIO_STATUS_DRIVER_OK) as u64,
            4,
        );
        disk.write(VIRTIO_MMIO_QUEUE_SEL, 1, 4);
        disk.write(VIRTIO_MMIO_QUEUE_NUM, 8, 4);
        disk.write(VIRTIO_MMIO_QUEUE_DESC_LOW, 0x8000, 4);
        disk.write(VIRTIO_MMIO_QUEUE_DRIVER_LOW, 0x8800, 4);
        disk.write(VIRTIO_MMIO_QUEUE_DEVICE_LOW, 0x8900, 4);
        disk.write(VIRTIO_MMIO_QUEUE_READY, 1, 4);
        mem.write_u64(0x8000, 0x9000).unwrap();
        mem.write_u32(0x8008, 16).unwrap();
        mem.write_u16(0x800c, VRING_DESC_F_NEXT).unwrap();
        mem.write_u16(0x800e, 1).unwrap();
        mem.write_u64(0x8010, 0x9100).unwrap();
        mem.write_u32(0x8018, 512).unwrap();
        mem.write_u16(0x801c, VRING_DESC_F_NEXT).unwrap();
        mem.write_u16(0x801e, 2).unwrap();
        mem.write_u64(0x8020, 0x9300).unwrap();
        mem.write_u32(0x8028, 1).unwrap();
        mem.write_u16(0x802c, VRING_DESC_F_WRITE).unwrap();
        mem.write_u32(0x9000, VIRTIO_BLK_T_OUT).unwrap();
        mem.write(0x9100, &[0x77; 512]).unwrap();
        mem.write_u16(0x8802, 1).unwrap();
        mem.write_u16(0x8804, 0).unwrap();
        disk.write(VIRTIO_MMIO_QUEUE_NOTIFY, 1, 4);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while std::fs::read(&path).unwrap()[..512] != [0x77; 512] {
            assert!(
                std::time::Instant::now() < deadline,
                "queue 1 request was not completed"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(mem.read_u16(0x8902).unwrap(), 1);
        assert_eq!(
            disk.inner
                .lock()
                .unwrap()
                .transport
                .queue(0)
                .unwrap()
                .used_idx,
            0
        );
    }
}
