//! Classic virtio-balloon inflate/deflate.
//!
//! Guest PFNs are 4 KiB. This host maps 16 KiB pages, so a host page is marked
//! reusable only when all four subpages are inflated, and it is marked for
//! reuse before the guest is told it may use the page again.
//! `hv_vm_map` is left in place: reclamation is `madvise`, not unmap.

use super::*;
use crate::devices::MmioDevice;
use crate::memory::{GUEST_PAGE_SIZE, GuestMemory};
use serde_json::{Value, json};
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const MUST_TELL_HOST: u64 = 1 << 0;
const OFFERED: u64 = MUST_TELL_HOST | VIRTIO_F_VERSION_1;
const QUEUE_MAX: u32 = 128;
const PFN_BYTES: u64 = 4096;
const SUBPAGES: u32 = (GUEST_PAGE_SIZE as u32) / (PFN_BYTES as u32);
pub const MADV_FREE_REUSABLE: i32 = 7;
pub const MADV_FREE_REUSE: i32 = 8;
/// Largest target step after the guest has started, 64 MiB of 4 KiB pages.
const TARGET_STEP_PAGES: u32 = 16 * 1024;
const CONTROL_REQUEST_MAX: usize = 4096;
const CONTROL_POLL: Duration = Duration::from_millis(20);
const TARGET_STEP_TIMEOUT: Duration = Duration::from_secs(30);

pub struct VirtioBalloon {
    transport: MmioTransport,
    mem: Arc<GuestMemory>,
    protected: Vec<(u64, u64)>,
    /// Inflated-subpage mask for each host page. Bit 0 is the lowest 4 KiB.
    groups: Vec<u8>,
    num_pages: u32,
    actual: u32,
    held: u32,
}

impl VirtioBalloon {
    pub fn new(
        irq: u32,
        mem: Arc<GuestMemory>,
        protected: &[(u64, u64)],
        target_pages: u32,
    ) -> Self {
        let host_pages = mem.size() / GUEST_PAGE_SIZE;
        let mut dev = Self {
            transport: MmioTransport::new(
                VIRTIO_DEV_BALLOON,
                OFFERED,
                &[QUEUE_MAX, QUEUE_MAX],
                irq,
            ),
            mem,
            protected: protected.to_vec(),
            groups: vec![0; host_pages],
            num_pages: 0,
            actual: 0,
            held: 0,
        };
        dev.set_target_pages(target_pages);
        dev
    }

    pub fn held_pages(&self) -> u32 {
        self.held
    }

    pub fn target_pages(&self) -> u32 {
        self.num_pages
    }

    pub fn actual_pages(&self) -> u32 {
        self.actual
    }

    pub fn set_target_pages(&mut self, pages: u32) -> u32 {
        let asked = pages.min(self.max_pages());
        let applied = if self.transport.status() & VIRTIO_STATUS_DRIVER_OK == 0 {
            asked
        } else if asked > self.num_pages {
            self.num_pages.saturating_add(TARGET_STEP_PAGES).min(asked)
        } else {
            self.num_pages.saturating_sub(TARGET_STEP_PAGES).max(asked)
        };
        if applied != self.num_pages {
            self.num_pages = applied;
            if self.transport.operational() {
                self.transport.raise_interrupt(2);
                self.poke();
            }
        }
        applied
    }

    fn max_pages(&self) -> u32 {
        let mut blocked = 0u32;
        for &(base, len) in &self.protected {
            let end = base.saturating_add(len);
            let mut gpa = base & !(PFN_BYTES - 1);
            while gpa < end {
                if self.page_index(gpa).is_some() {
                    blocked = blocked.saturating_add(1);
                }
                gpa = gpa.saturating_add(PFN_BYTES);
            }
        }
        ((self.mem.size() / PFN_BYTES as usize) as u32).saturating_sub(blocked)
    }

    fn page_index(&self, gpa: u64) -> Option<(usize, u8)> {
        let delta = gpa.checked_sub(self.mem.guest_base())?;
        if delta >= self.mem.size() as u64 || gpa % PFN_BYTES != 0 {
            return None;
        }
        let pfn = (delta / PFN_BYTES) as usize;
        let host = pfn / SUBPAGES as usize;
        let sub = (pfn % SUBPAGES as usize) as u8;
        self.groups.get(host)?;
        Some((host, 1u8 << sub))
    }

    fn protected(&self, gpa: u64) -> bool {
        self.protected
            .iter()
            .any(|&(base, len)| gpa >= base && gpa < base.saturating_add(len))
    }

    fn config_byte(&self, index: usize) -> u8 {
        let value = match index {
            0..=3 => self.num_pages.to_le_bytes()[index],
            4..=7 => self.actual.to_le_bytes()[index - 4],
            _ => 0,
        };
        value
    }

    fn poke(&self) {
        #[cfg(not(test))]
        unsafe {
            crate::hypervisor::hv_gic_set_spi(
                self.transport.irq(),
                self.transport.interrupt_status() != 0,
            );
        }
        if self.transport.interrupt_status() != 0 {
            crate::vcpu::kick_vcpu();
        }
    }

    fn release_all(&mut self) {
        let base = self.mem.guest_base();
        for (index, group) in self.groups.iter_mut().enumerate() {
            if group.count_ones() == SUBPAGES {
                let gpa = base + index as u64 * GUEST_PAGE_SIZE as u64;
                let _ = self.mem.advise_range(gpa, GUEST_PAGE_SIZE, MADV_FREE_REUSE);
            }
            *group = 0;
        }
        self.held = 0;
    }

    fn apply_pfns(&mut self, pfns: &[u32], inflate: bool) -> Result<(), String> {
        let mut parsed = Vec::with_capacity(pfns.len());
        for &pfn in pfns {
            let gpa = u64::from(pfn) * PFN_BYTES;
            if self.protected(gpa) || self.page_index(gpa).is_none() {
                return Err(format!("balloon pfn {pfn:#x} is outside reclaimable RAM"));
            }
            parsed.push(gpa);
        }
        for gpa in parsed {
            let (host, bit) = self.page_index(gpa).expect("validated");
            let before = self.groups[host];
            if inflate {
                if before & bit != 0 {
                    continue;
                }
                let next = before | bit;
                if next.count_ones() == SUBPAGES && before.count_ones() != SUBPAGES {
                    let page = self.mem.guest_base() + host as u64 * GUEST_PAGE_SIZE as u64;
                    self.mem
                        .advise_range(page, GUEST_PAGE_SIZE, MADV_FREE_REUSABLE)?;
                }
                self.groups[host] = next;
                self.held = self.held.saturating_add(1);
            } else if before & bit != 0 {
                if before.count_ones() == SUBPAGES {
                    let page = self.mem.guest_base() + host as u64 * GUEST_PAGE_SIZE as u64;
                    self.mem
                        .advise_range(page, GUEST_PAGE_SIZE, MADV_FREE_REUSE)?;
                }
                self.groups[host] = before & !bit;
                self.held = self.held.saturating_sub(1);
            }
        }
        Ok(())
    }

    fn process_queue(&mut self, index: usize) {
        if !self.transport.operational() {
            return;
        }
        let Some(mut queue) = self.transport.queue(index).copied() else {
            return;
        };
        if !queue.ready || queue.size == 0 || queue.desc_table == 0 || queue.avail_ring == 0 {
            return;
        }
        let count = match queue.pending_count(&self.mem) {
            Ok(n) => n,
            Err(_) => {
                self.transport.set_needs_reset();
                return;
            }
        };
        let indirect = self.transport.negotiated(VIRTIO_RING_F_INDIRECT_DESC);
        let mut curr = queue.last_avail_idx;
        let mut processed = false;
        for _ in 0..count {
            let slot = queue.avail_ring + 4 + 2 * (curr as u64 % queue.size as u64);
            let head = match self.mem.read_u16(slot) {
                Ok(head) => head,
                Err(_) => {
                    self.transport.set_needs_reset();
                    break;
                }
            };
            let descriptors = match queue.descriptors(&self.mem, head, indirect, 1024 * 1024) {
                Ok(d) if !d.is_empty() && d.iter().all(|d| d.flags & VRING_DESC_F_WRITE == 0) => d,
                _ => {
                    self.transport.set_needs_reset();
                    break;
                }
            };
            let mut raw = Vec::new();
            let mut read_ok = true;
            for desc in &descriptors {
                if desc.len % 4 != 0 {
                    self.transport.set_needs_reset();
                    read_ok = false;
                    break;
                }
                let start = raw.len();
                raw.resize(start + desc.len as usize, 0);
                if self.mem.read(desc.addr, &mut raw[start..]).is_err() {
                    self.transport.set_needs_reset();
                    read_ok = false;
                    break;
                }
            }
            if !read_ok {
                break;
            }
            let pfns: Vec<u32> = raw
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect();
            if let Err(error) = self.apply_pfns(&pfns, index == 0) {
                eprintln!("[VirtIO-Balloon] {error}");
                self.transport.set_needs_reset();
                break;
            }
            publish_used(&self.mem, &mut queue, head, 0);
            curr = curr.wrapping_add(1);
            processed = true;
        }
        queue.last_avail_idx = curr;
        let avail = queue.avail_ring;
        if let Some(slot) = self.transport.queue_mut(index) {
            *slot = queue;
        }
        if processed && avail_wants_interrupt(&self.mem, avail) {
            self.transport.raise_interrupt(1);
            self.poke();
        }
    }
}

#[derive(Default)]
struct TargetState {
    requested_pages: u32,
    waiting_since: Option<Instant>,
    error: Option<String>,
}

impl TargetState {
    fn new(requested_pages: u32) -> Self {
        Self {
            requested_pages,
            waiting_since: None,
            error: None,
        }
    }

    fn set_requested(&mut self, pages: u32) {
        self.requested_pages = pages;
        self.waiting_since = None;
        self.error = None;
    }

    fn advance(&mut self, dev: &mut VirtioBalloon, now: Instant) {
        if !dev.transport.operational() {
            self.waiting_since = None;
            self.error = None;
            if dev.target_pages() != self.requested_pages {
                dev.set_target_pages(self.requested_pages);
            }
            return;
        }
        if dev.actual_pages() == dev.target_pages() {
            self.waiting_since = None;
            self.error = None;
            if dev.target_pages() != self.requested_pages {
                let before = dev.target_pages();
                let applied = dev.set_target_pages(self.requested_pages);
                if applied != before {
                    self.waiting_since = Some(now);
                }
            }
        } else {
            let since = self.waiting_since.get_or_insert(now);
            if now.duration_since(*since) >= TARGET_STEP_TIMEOUT && self.error.is_none() {
                self.error = Some(format!(
                    "guest balloon did not converge: advertised={} actual={}",
                    dev.target_pages(),
                    dev.actual_pages()
                ));
            }
        }
    }

    fn response(&self, dev: &VirtioBalloon) -> Value {
        let driver_ok = dev.transport.operational();
        let converged = driver_ok
            && self.requested_pages == dev.target_pages()
            && dev.target_pages() == dev.actual_pages()
            && dev.actual_pages() == dev.held_pages();
        json!({
            "ok": self.error.is_none(),
            "driver_ok": driver_ok,
            "converged": converged,
            "requested_pages": self.requested_pages,
            "requested_mib": self.requested_pages / 256,
            "target_pages": dev.target_pages(),
            "target_mib": dev.target_pages() / 256,
            "actual_pages": dev.actual_pages(),
            "actual_mib": dev.actual_pages() / 256,
            "held_pages": dev.held_pages(),
            "max_pages": dev.max_pages(),
            "error": self.error,
        })
    }
}

fn socket_identity(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::symlink_metadata(path).ok()?;
    meta.file_type()
        .is_socket()
        .then_some((meta.dev(), meta.ino()))
}

fn remove_socket_if_same(path: &Path, identity: (u64, u64)) {
    if socket_identity(path) == Some(identity) {
        let _ = fs::remove_file(path);
    }
}

fn bind_control(path: &Path) -> Result<(UnixListener, (u64, u64)), String> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent).map_err(|e| format!("create balloon control directory: {e}"))?;
    }
    if path.exists() {
        let meta = fs::symlink_metadata(path).map_err(|e| e.to_string())?;
        if !meta.file_type().is_socket() {
            return Err(format!(
                "{} exists and is not a unix socket",
                path.display()
            ));
        }
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err(format!("{} is owned by another user", path.display()));
        }
        match UnixStream::connect(path) {
            Ok(_) => return Err(format!("{} is already in use", path.display())),
            Err(err) if err.raw_os_error() == Some(libc::ECONNREFUSED) => {
                fs::remove_file(path).map_err(|e| e.to_string())?;
            }
            Err(err) => {
                return Err(format!(
                    "cannot verify whether {} is stale: {err}",
                    path.display()
                ));
            }
        }
    }
    let listener = UnixListener::bind(path)
        .map_err(|e| format!("balloon control listen {}: {e}", path.display()))?;
    if let Err(err) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
        drop(listener);
        let _ = fs::remove_file(path);
        return Err(format!(
            "secure balloon control listener {}: {err}",
            path.display()
        ));
    }
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("set balloon control nonblocking: {e}"))?;
    let identity = socket_identity(path)
        .ok_or_else(|| format!("cannot identify balloon listener {}", path.display()))?;
    Ok((listener, identity))
}

fn read_request(stream: &mut UnixStream) -> Result<Value, String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    while bytes.len() <= CONTROL_REQUEST_MAX {
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => bytes.push(byte[0]),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(format!("read balloon request: {err}")),
        }
    }
    if bytes.len() > CONTROL_REQUEST_MAX {
        return Err("balloon control request is too large".into());
    }
    if bytes.is_empty() {
        return Err("empty balloon control request".into());
    }
    serde_json::from_slice(&bytes).map_err(|e| format!("invalid balloon control JSON: {e}"))
}

fn apply_request(
    request: Value,
    state: &mut TargetState,
    dev: &mut VirtioBalloon,
) -> Result<Value, String> {
    let object = request
        .as_object()
        .ok_or("balloon control request must be a JSON object")?;
    if object.len() != 1 {
        return Err("balloon control request must contain exactly one command".into());
    }
    if let Some(value) = object.get("set_mib") {
        let mib = value
            .as_u64()
            .ok_or("set_mib must be a non-negative integer")?;
        let requested = mib.saturating_mul(256).min(u64::from(u32::MAX)) as u32;
        state.set_requested(requested.min(dev.max_pages()));
        state.advance(dev, Instant::now());
        return Ok(state.response(dev));
    }
    if object.get("status").and_then(Value::as_bool) == Some(true) {
        state.advance(dev, Instant::now());
        return Ok(state.response(dev));
    }
    Err("unknown balloon control command; use set_mib or status:true".into())
}

fn write_response(stream: &mut UnixStream, response: Value) {
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let mut bytes = response.to_string().into_bytes();
    bytes.push(b'\n');
    let _ = stream.write_all(&bytes);
}

fn serve_client(
    mut stream: UnixStream,
    state: &mut TargetState,
    device: &Arc<Mutex<VirtioBalloon>>,
) {
    let response = match read_request(&mut stream) {
        Ok(request) => match device.lock() {
            Ok(mut dev) => apply_request(request, state, &mut dev)
                .unwrap_or_else(|error| json!({"ok": false, "error": error})),
            Err(_) => json!({"ok": false, "error": "balloon device lock is poisoned"}),
        },
        Err(error) => json!({"ok": false, "error": error}),
    };
    write_response(&mut stream, response);
}

/// Lifetime guard for the per-VM runtime balloon control socket.
pub struct BalloonController {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    path: PathBuf,
    identity: (u64, u64),
}

impl BalloonController {
    pub fn start(path: PathBuf, device: Arc<Mutex<VirtioBalloon>>) -> Result<Self, String> {
        let (listener, identity) = bind_control(&path)?;
        let requested = device
            .lock()
            .map_err(|_| "balloon device lock is poisoned")?
            .target_pages();
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let thread_path = path.clone();
        let worker = thread::Builder::new()
            .name("cube-balloon-control".into())
            .spawn(move || {
                let mut state = TargetState::new(requested);
                while !thread_stop.load(Ordering::Acquire) {
                    loop {
                        match listener.accept() {
                            Ok((stream, _)) => serve_client(stream, &mut state, &device),
                            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
                            Err(err) => {
                                eprintln!("[VirtIO-Balloon] control accept failed: {err}");
                                thread_stop.store(true, Ordering::Release);
                                break;
                            }
                        }
                    }
                    if let Ok(mut dev) = device.lock() {
                        state.advance(&mut dev, Instant::now());
                    }
                    thread::sleep(CONTROL_POLL);
                }
                remove_socket_if_same(&thread_path, identity);
            })
            .map_err(|e| {
                remove_socket_if_same(&path, identity);
                format!("start balloon control worker: {e}")
            })?;
        Ok(Self {
            stop,
            worker: Some(worker),
            path,
            identity,
        })
    }
}

impl Drop for BalloonController {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        remove_socket_if_same(&self.path, self.identity);
    }
}

impl MmioDevice for VirtioBalloon {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if offset >= VIRTIO_MMIO_CONFIG {
            let start = (offset - VIRTIO_MMIO_CONFIG) as usize;
            let mut value = 0u64;
            for byte in 0..size.min(8) {
                value |= u64::from(self.config_byte(start + byte)) << (byte * 8);
            }
            return value;
        }
        self.transport.read(offset)
    }

    fn write(&mut self, offset: u64, data: u64, size: usize) {
        if offset >= VIRTIO_MMIO_CONFIG {
            if offset == VIRTIO_MMIO_CONFIG + 4 && size == 4 {
                self.actual = data as u32;
            }
            return;
        }
        let effect = self.transport.write(offset, data);
        match effect {
            WriteEffect::Notify { queue: 0 } => self.process_queue(0),
            WriteEffect::Notify { queue: 1 } => self.process_queue(1),
            WriteEffect::Reset => {
                self.release_all();
                self.actual = 0;
                self.poke();
            }
            WriteEffect::InterruptAck { .. } => self.poke(),
            WriteEffect::DriverOk => {
                if self.num_pages != 0 {
                    self.transport.raise_interrupt(2);
                    self.poke();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arm(
        dev: &mut VirtioBalloon,
        mem: &GuestMemory,
        queue: u32,
        desc: u64,
        avail: u64,
        used: u64,
    ) {
        dev.transport.write(VIRTIO_MMIO_QUEUE_SEL, queue as u64);
        dev.transport.write(VIRTIO_MMIO_QUEUE_NUM, 8);
        dev.transport.write(VIRTIO_MMIO_QUEUE_DESC_LOW, desc);
        dev.transport.write(VIRTIO_MMIO_QUEUE_DRIVER_LOW, avail);
        dev.transport.write(VIRTIO_MMIO_QUEUE_DEVICE_LOW, used);
        assert_eq!(
            dev.transport.write(VIRTIO_MMIO_QUEUE_READY, 1),
            WriteEffect::QueueReady { queue }
        );
        mem.write_u16(avail, VRING_AVAIL_F_NO_INTERRUPT).unwrap();
    }

    fn negotiate(dev: &mut VirtioBalloon) {
        dev.transport.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0);
        dev.transport
            .write(VIRTIO_MMIO_DRIVER_FEATURES, MUST_TELL_HOST);
        dev.transport.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1);
        dev.transport.write(VIRTIO_MMIO_DRIVER_FEATURES, 1);
        let status = (VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK) as u64;
        dev.transport.write(VIRTIO_MMIO_STATUS, status);
        assert!(dev.transport.operational());
    }

    fn write_desc(mem: &GuestMemory, at: u64, addr: u64, len: u32) {
        mem.write(at, &addr.to_le_bytes()).unwrap();
        mem.write(at + 8, &len.to_le_bytes()).unwrap();
        mem.write_u16(at + 12, 0).unwrap();
        mem.write_u16(at + 14, 0).unwrap();
    }

    #[allow(deprecated)]
    fn footprint() -> (u64, u64) {
        let mut raw = [0u8; 160];
        let mut count = (raw.len() / 4) as u32;
        let rc = unsafe {
            libc::task_info(
                libc::mach_task_self(),
                22,
                raw.as_mut_ptr().cast(),
                &mut count,
            )
        };
        assert_eq!(rc, 0);
        let word = |off: usize| u64::from_le_bytes(raw[off..off + 8].try_into().unwrap());
        (word(80), word(144))
    }

    fn ring_base(mem_base: u64, len: usize) -> u64 {
        mem_base + len as u64 - GUEST_PAGE_SIZE as u64
    }

    #[test]
    fn full_host_page_is_reclaimed_and_partial_group_stays() {
        let mem_base = 0x4000_0000u64;
        let len = GUEST_PAGE_SIZE * 64;
        let mem = Arc::new(GuestMemory::new_test(mem_base, len));
        mem.write(mem_base, &vec![0x11u8; GUEST_PAGE_SIZE]).unwrap();
        let mut dev = VirtioBalloon::new(48, mem.clone(), &[], 256);
        assert_eq!(dev.read(VIRTIO_MMIO_CONFIG, 4), 256);
        negotiate(&mut dev);
        let rings = ring_base(mem_base, len);
        let desc = rings;
        let pfns = rings + 0x200;
        let avail = rings + 0x800;
        let used = rings + 0x900;
        write_desc(&mem, desc, pfns, 4 * 3);
        arm(&mut dev, &mem, 0, desc, avail, used);
        let base_pfn = (mem_base / PFN_BYTES) as u32;
        for sub in 0..3u32 {
            mem.write_u32(pfns + sub as u64 * 4, base_pfn + sub)
                .unwrap();
        }
        mem.write_u16(avail + 2, 1).unwrap();
        dev.write(VIRTIO_MMIO_QUEUE_NOTIFY, 0, 4);
        assert_eq!(dev.held_pages(), 3);
        assert_eq!(dev.groups[0].count_ones(), 3);

        let (reusable_before, foot_before) = footprint();
        write_desc(&mem, desc, pfns + 0x40, 4);
        mem.write_u32(pfns + 0x40, base_pfn + 3).unwrap();
        mem.write_u16(avail + 2, 2).unwrap();
        dev.write(VIRTIO_MMIO_QUEUE_NOTIFY, 0, 4);
        let (reusable_after, foot_after) = footprint();
        assert_eq!(dev.held_pages(), 4);
        assert_eq!(dev.groups[0].count_ones(), 4);
        assert!(
            foot_before.saturating_sub(foot_after) >= GUEST_PAGE_SIZE as u64 / 2
                || reusable_after.saturating_sub(reusable_before) >= GUEST_PAGE_SIZE as u64 / 2,
            "reusable {reusable_before}->{reusable_after} footprint {foot_before}->{foot_after}"
        );

        let deflate = rings + 0x100;
        write_desc(&mem, deflate, pfns + 0x80, 16);
        arm(&mut dev, &mem, 1, deflate, rings + 0xA00, rings + 0xC00);
        for sub in 0..4u32 {
            mem.write_u32(pfns + 0x80 + sub as u64 * 4, base_pfn + sub)
                .unwrap();
        }
        mem.write_u16(rings + 0xA02, 1).unwrap();
        dev.write(VIRTIO_MMIO_QUEUE_NOTIFY, 1, 4);
        assert_eq!(dev.held_pages(), 0);
        mem.write(mem_base, &[0xCD; 16]).unwrap();
        let mut got = [0u8; 16];
        mem.read(mem_base, &mut got).unwrap();
        assert_eq!(got, [0xCD; 16]);
    }

    #[test]
    fn rejects_out_of_range_protected_and_duplicate_pfns() {
        let mem_base = 0x4000_0000u64;
        let len = GUEST_PAGE_SIZE * 4;
        let mem = Arc::new(GuestMemory::new_test(mem_base, len));
        let protected = [(mem_base, GUEST_PAGE_SIZE as u64)];
        let mut dev = VirtioBalloon::new(48, mem.clone(), &protected, 8);
        negotiate(&mut dev);
        let rings = ring_base(mem_base, len);
        let desc = rings;
        let pfns = rings + 0x200;
        let avail = rings + 0x800;
        let used = rings + 0x900;
        write_desc(&mem, desc, pfns, 4);
        arm(&mut dev, &mem, 0, desc, avail, used);
        mem.write_u32(pfns, 0).unwrap();
        mem.write_u16(avail + 2, 1).unwrap();
        dev.write(VIRTIO_MMIO_QUEUE_NOTIFY, 0, 4);
        assert_ne!(dev.transport.status() & VIRTIO_STATUS_NEEDS_RESET, 0);
        assert_eq!(dev.held_pages(), 0);

        dev.write(VIRTIO_MMIO_STATUS, 0, 4);
        negotiate(&mut dev);
        arm(&mut dev, &mem, 0, desc, avail, used);
        let protected_pfn = (mem_base / PFN_BYTES) as u32;
        mem.write_u32(pfns, protected_pfn).unwrap();
        mem.write_u16(avail + 2, 1).unwrap();
        dev.write(VIRTIO_MMIO_QUEUE_NOTIFY, 0, 4);
        assert_ne!(dev.transport.status() & VIRTIO_STATUS_NEEDS_RESET, 0);
        assert_eq!(dev.held_pages(), 0);

        dev.write(VIRTIO_MMIO_STATUS, 0, 4);
        negotiate(&mut dev);
        arm(&mut dev, &mem, 0, desc, avail, used);
        let ok = protected_pfn + SUBPAGES;
        mem.write_u32(pfns, ok).unwrap();
        mem.write_u16(avail + 2, 1).unwrap();
        dev.write(VIRTIO_MMIO_QUEUE_NOTIFY, 0, 4);
        assert_eq!(dev.held_pages(), 1);
        mem.write_u16(avail + 2, 2).unwrap();
        dev.write(VIRTIO_MMIO_QUEUE_NOTIFY, 0, 4);
        assert_eq!(dev.held_pages(), 1, "duplicate pfn is not counted twice");
        dev.write(VIRTIO_MMIO_CONFIG + 4, 1, 4);
        assert_eq!(dev.read(VIRTIO_MMIO_CONFIG + 4, 4), 1);
    }

    #[test]
    fn target_is_clamped_and_stepped_after_driver_ok() {
        let mem = Arc::new(GuestMemory::new_test(0x4000_0000, GUEST_PAGE_SIZE * 2));
        let mut dev = VirtioBalloon::new(48, mem, &[], 100_000);
        let ram_pages = (GUEST_PAGE_SIZE * 2 / PFN_BYTES as usize) as u32;
        assert_eq!(dev.read(VIRTIO_MMIO_CONFIG, 4), ram_pages as u64);
        negotiate(&mut dev);
        let stepped = dev.set_target_pages(ram_pages + TARGET_STEP_PAGES);
        assert_eq!(stepped, ram_pages);
        dev.num_pages = 0;
        let stepped = dev.set_target_pages(TARGET_STEP_PAGES + 10);
        assert_eq!(stepped, TARGET_STEP_PAGES.min(ram_pages));
    }

    #[test]
    fn runtime_target_waits_for_actual_between_steps() {
        let mem = Arc::new(GuestMemory::new_test(0x4000_0000, GUEST_PAGE_SIZE * 16_384));
        let mut dev = VirtioBalloon::new(48, mem, &[], TARGET_STEP_PAGES * 4);
        negotiate(&mut dev);
        dev.transport.write(VIRTIO_MMIO_INTERRUPT_ACK, 2);
        assert_eq!(dev.transport.interrupt_status(), 0);
        dev.actual = dev.target_pages();
        let mut state = TargetState::new(dev.target_pages());
        state.set_requested(0);
        let now = Instant::now();
        state.advance(&mut dev, now);
        assert_eq!(dev.target_pages(), TARGET_STEP_PAGES * 3);
        assert_eq!(dev.transport.interrupt_status() & 2, 2);

        state.advance(&mut dev, now + Duration::from_secs(1));
        assert_eq!(
            dev.target_pages(),
            TARGET_STEP_PAGES * 3,
            "the next step waits for the guest's actual value"
        );

        dev.actual = dev.target_pages();
        state.advance(&mut dev, now + Duration::from_secs(2));
        assert_eq!(dev.target_pages(), TARGET_STEP_PAGES * 2);
        assert!(state.error.is_none());

        dev.write(VIRTIO_MMIO_STATUS, 0, 4);
        assert_eq!(dev.actual_pages(), 0);
    }

    #[test]
    fn runtime_request_clamps_and_rejects_unknown_fields() {
        let mem = Arc::new(GuestMemory::new_test(0x4000_0000, GUEST_PAGE_SIZE * 8));
        let mut dev = VirtioBalloon::new(48, mem, &[], 0);
        let mut state = TargetState::new(0);
        let response = apply_request(json!({"set_mib": 4096}), &mut state, &mut dev).unwrap();
        assert_eq!(
            response["requested_pages"].as_u64(),
            Some(u64::from(dev.max_pages()))
        );
        assert!(
            apply_request(json!({"set_mib": 0, "extra": true}), &mut state, &mut dev)
                .unwrap_err()
                .contains("exactly one")
        );
        assert!(
            apply_request(json!({"set_mib": -1}), &mut state, &mut dev)
                .unwrap_err()
                .contains("non-negative")
        );
    }

    #[test]
    fn control_socket_serves_status_and_is_removed_on_drop() {
        use std::net::Shutdown;
        use std::sync::atomic::AtomicU64;

        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = PathBuf::from(format!(
            "/tmp/cube-balloon-test-{}-{}.sock",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_file(&path);
        match UnixListener::bind(&path) {
            Ok(listener) => drop(listener),
            Err(err) if err.raw_os_error() == Some(libc::EPERM) => return,
            Err(err) => panic!("preflight balloon socket bind failed: {err}"),
        }
        let mem = Arc::new(GuestMemory::new_test(0x4000_0000, GUEST_PAGE_SIZE * 64));
        let dev = Arc::new(Mutex::new(VirtioBalloon::new(48, mem, &[], 0)));
        let controller = BalloonController::start(path.clone(), dev).unwrap();

        let mut stream = UnixStream::connect(&path).unwrap();
        stream.write_all(b"{\"status\":true}\n").unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let response: Value = serde_json::from_str(&response).unwrap();
        assert_eq!(response["ok"], true);
        assert_eq!(response["driver_ok"], false);
        assert_eq!(response["converged"], false);
        assert_eq!(response["target_pages"], 0);

        drop(controller);
        assert!(!path.exists());
    }
}
