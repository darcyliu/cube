//! Virtio-fs (device ID 26) over the shared modern MMIO transport.
//! One hiprio queue and one request queue. No DAX or notification queue.
//! The FUSE server runs in the unprivileged share helper.

use super::*;
use crate::devices::MmioDevice;
use crate::memory::GuestMemory;
use crate::shared_fs::{Backend, MAX_MESSAGE};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

const FEATURES: u64 = VIRTIO_F_VERSION_1 | VIRTIO_RING_F_INDIRECT_DESC;
const QUEUE_MAX: u32 = 128;
const HIPRIO: usize = 0;
const REQUEST: usize = 1;
const HEADER: usize = 40;

const FUSE_LOOKUP: u32 = 1;
const FUSE_FORGET: u32 = 2;
const FUSE_GETATTR: u32 = 3;
const FUSE_OPEN: u32 = 14;
const FUSE_READ: u32 = 15;
const FUSE_STATFS: u32 = 17;
const FUSE_INIT: u32 = 26;
const FUSE_OPENDIR: u32 = 27;
const FUSE_READDIR: u32 = 28;
const FUSE_INTERRUPT: u32 = 36;
const FUSE_DESTROY: u32 = 38;
const FUSE_BATCH_FORGET: u32 = 42;

struct State {
    transport: MmioTransport,
    epoch: u64,
    notified: bool,
    stopped: bool,
    backend_failed: bool,
}

impl State {
    fn new(irq: u32) -> Self {
        Self {
            transport: MmioTransport::new(VIRTIO_DEV_FS, FEATURES, &[QUEUE_MAX, QUEUE_MAX], irq),
            epoch: 0,
            notified: false,
            stopped: false,
            backend_failed: false,
        }
    }

    fn signal(&self) {
        let pending = self.transport.interrupt_status() != 0;
        #[cfg(not(test))]
        unsafe {
            crate::hypervisor::hv_gic_set_spi(self.transport.irq(), pending);
        }
        if pending {
            crate::vcpu::kick_vcpu();
        }
    }

    fn fail(&mut self, error: &str) {
        eprintln!("[VirtIO-FS] {error}");
        self.transport.set_needs_reset();
        let malformed = self.transport.diagnostics().malformed.saturating_add(1);
        self.transport.diagnostics_mut().malformed = malformed;
        self.transport.set_interrupt_bit(2);
        self.signal();
    }
}

#[derive(Debug)]
struct Request {
    queue: usize,
    head: u16,
    epoch: u64,
    bytes: Vec<u8>,
    output: Vec<Descriptor>,
    reply: bool,
}

fn min_reply(opcode: u32) -> Option<usize> {
    match opcode {
        FUSE_FORGET | FUSE_INTERRUPT | FUSE_DESTROY | FUSE_BATCH_FORGET => None,
        FUSE_INIT => Some(80),
        FUSE_LOOKUP => Some(144),
        FUSE_GETATTR => Some(120),
        FUSE_STATFS => Some(96),
        FUSE_OPEN | FUSE_OPENDIR => Some(32),
        _ => Some(16),
    }
}

fn take_request(
    state: &mut State,
    mem: &GuestMemory,
    queue: usize,
) -> Result<Option<Request>, String> {
    if !state.transport.operational() {
        return Ok(None);
    }
    let Some(mut q) = state.transport.queue(queue).copied() else {
        return Ok(None);
    };
    if !q.ready || q.pending_count(mem)? == 0 {
        return Ok(None);
    }
    let head = mem.read_u16(q.avail_ring + 4 + 2 * (q.last_avail_idx as u64 % q.size as u64))?;
    let descs = q.descriptors(
        mem,
        head,
        state.transport.negotiated(VIRTIO_RING_F_INDIRECT_DESC),
        MAX_MESSAGE,
    )?;
    let mut bytes = Vec::new();
    let mut output = Vec::new();
    let mut writing = false;
    for d in descs {
        if d.flags & VRING_DESC_F_WRITE != 0 {
            writing = true;
            output.push(d);
        } else if writing {
            return Err("virtiofs readable descriptor after writable".into());
        } else {
            if bytes.len().saturating_add(d.len as usize) > MAX_MESSAGE {
                return Err("virtiofs request exceeds the message cap".into());
            }
            let start = bytes.len();
            bytes.resize(start + d.len as usize, 0);
            if d.len > 0 {
                mem.read(d.addr, &mut bytes[start..])?;
            }
        }
    }
    if bytes.len() < HEADER {
        return Err("virtiofs request is shorter than a FUSE header".into());
    }
    let declared = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    if declared != bytes.len() || declared > MAX_MESSAGE {
        return Err("virtiofs FUSE length does not match the descriptor chain".into());
    }
    let opcode = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
    let reply = min_reply(opcode);
    let capacity: usize = output.iter().map(|d| d.len as usize).sum();
    if let Some(need) = reply {
        if capacity < need {
            return Err(format!(
                "virtiofs response buffer is too small for FUSE opcode {opcode}: {capacity} < {need}"
            ));
        }
    }
    if matches!(opcode, FUSE_READ | FUSE_READDIR) && bytes.len() >= 60 {
        let fit = capacity.saturating_sub(16).min(65536) as u32;
        let size = u32::from_le_bytes(bytes[56..60].try_into().unwrap());
        if size > fit {
            bytes[56..60].copy_from_slice(&fit.to_le_bytes());
        }
    }
    q.last_avail_idx = q.last_avail_idx.wrapping_add(1);
    let epoch = state.epoch;
    if let Some(slot) = state.transport.queue_mut(queue) {
        *slot = q;
    }
    Ok(Some(Request {
        queue,
        head,
        epoch,
        bytes,
        output,
        reply: reply.is_some(),
    }))
}

fn complete(
    state: &mut State,
    mem: &GuestMemory,
    request: Request,
    reply: &[u8],
) -> Result<(), String> {
    if request.epoch != state.epoch || !state.transport.operational() {
        let drops = state.transport.diagnostics().drops.saturating_add(1);
        state.transport.diagnostics_mut().drops = drops;
        return Ok(());
    }
    let Some(mut queue) = state.transport.queue(request.queue).copied() else {
        let drops = state.transport.diagnostics().drops.saturating_add(1);
        state.transport.diagnostics_mut().drops = drops;
        return Ok(());
    };
    if !queue.ready {
        let drops = state.transport.diagnostics().drops.saturating_add(1);
        state.transport.diagnostics_mut().drops = drops;
        return Ok(());
    }
    let written = if request.reply {
        let capacity: usize = request.output.iter().map(|d| d.len as usize).sum();
        if reply.len() < 16 || reply.len() > capacity {
            return Err("virtiofs reply does not fit the response descriptors".into());
        }
        let mut offset = 0;
        for d in &request.output {
            if offset >= reply.len() {
                break;
            }
            let n = (reply.len() - offset).min(d.len as usize);
            mem.write(d.addr, &reply[offset..offset + n])?;
            offset += n;
        }
        reply.len() as u32
    } else {
        0
    };
    publish_used(mem, &mut queue, request.head, written);
    let avail = queue.avail_ring;
    if let Some(slot) = state.transport.queue_mut(request.queue) {
        *slot = queue;
    }
    let diag = state.transport.diagnostics_mut();
    diag.completions = diag.completions.saturating_add(1);
    diag.bytes = diag.bytes.saturating_add(u64::from(written));
    if avail_wants_interrupt(mem, avail) {
        state.transport.set_interrupt_bit(1);
        state.signal();
    }
    Ok(())
}

fn config_space(tag: &str) -> Vec<u8> {
    let mut config = vec![0u8; 40];
    let n = tag.len().min(35);
    config[..n].copy_from_slice(&tag.as_bytes()[..n]);
    config[36..40].copy_from_slice(&1u32.to_le_bytes());
    config
}

pub struct VirtioFs {
    shared: Arc<(Mutex<State>, Condvar)>,
    child: Arc<Mutex<Option<std::process::Child>>>,
    worker: Option<JoinHandle<()>>,
    config: Vec<u8>,
}

impl VirtioFs {
    pub fn open(
        tag: &str,
        irq: u32,
        mem: Arc<GuestMemory>,
        mut backend: Backend,
    ) -> Result<Self, String> {
        if tag.is_empty() || tag.len() > 35 {
            return Err("virtiofs tag must be 1–35 bytes".into());
        }
        let shared = Arc::new((Mutex::new(State::new(irq)), Condvar::new()));
        let worker_shared = shared.clone();
        let child = backend.child.clone();
        let worker = thread::Builder::new()
            .name(format!("vmm-virtiofs-{tag}"))
            .spawn(move || {
                let (lock, wake) = &*worker_shared;
                let mut epoch = 0u64;
                loop {
                    let mut state = lock.lock().unwrap();
                    while !state.notified && !state.stopped {
                        state = wake.wait(state).unwrap();
                    }
                    if state.stopped {
                        break;
                    }
                    state.notified = false;
                    if state.backend_failed {
                        continue;
                    }
                    if epoch != state.epoch {
                        epoch = state.epoch;
                        drop(state);
                        if let Err(e) = backend.request(&[]) {
                            let mut state = lock.lock().unwrap();
                            state.backend_failed = true;
                            state.fail(&e);
                        }
                        continue;
                    }
                    let found = match take_request(&mut state, &mem, HIPRIO) {
                        Ok(Some(request)) => Some(request),
                        Ok(None) => match take_request(&mut state, &mem, REQUEST) {
                            Ok(found) => found,
                            Err(e) => {
                                state.fail(&e);
                                continue;
                            }
                        },
                        Err(e) => {
                            state.fail(&e);
                            continue;
                        }
                    };
                    let Some(request) = found else {
                        continue;
                    };
                    state.notified = true;
                    drop(state);
                    let result = backend.request(&request.bytes);
                    let mut state = lock.lock().unwrap();
                    if state.stopped {
                        break;
                    }
                    match result {
                        Ok(reply) => {
                            if let Err(e) = complete(&mut state, &mem, request, &reply) {
                                state.backend_failed = true;
                                state.fail(&e);
                            }
                        }
                        Err(e) => {
                            state.backend_failed = true;
                            state.fail(&e);
                        }
                    }
                }
            })
            .map_err(|e| format!("virtiofs worker: {e}"))?;
        Ok(Self {
            shared,
            child,
            worker: Some(worker),
            config: config_space(tag),
        })
    }

    fn stop(&mut self) -> Result<(), String> {
        {
            let mut state = self.shared.0.lock().unwrap();
            state.stopped = true;
            self.shared.1.notify_all();
        }
        if let Ok(mut child) = self.child.lock() {
            if let Some(child) = child.as_mut() {
                let _ = child.kill();
            }
        }
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| "virtiofs worker panicked")?;
        }
        Ok(())
    }
}

impl Drop for VirtioFs {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

impl MmioDevice for VirtioFs {
    fn shutdown(&mut self) -> Result<(), String> {
        let line = self.shared.0.lock().unwrap().transport.diagnostics_line();
        println!("[VirtIO-FS] {line}");
        self.stop()
    }

    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if offset >= VIRTIO_MMIO_CONFIG {
            let start = (offset - VIRTIO_MMIO_CONFIG) as usize;
            let mut raw = [0u8; 8];
            if size > 8 {
                return 0;
            }
            for (i, byte) in raw[..size].iter_mut().enumerate() {
                *byte = self.config.get(start + i).copied().unwrap_or(0);
            }
            return u64::from_le_bytes(raw);
        }
        self.shared.0.lock().unwrap().transport.read(offset)
    }

    fn write(&mut self, offset: u64, data: u64, size: usize) {
        if size != 4 || offset >= VIRTIO_MMIO_CONFIG {
            return;
        }
        let mut state = self.shared.0.lock().unwrap();
        if offset == VIRTIO_MMIO_INTERRUPT_ACK {
            let _ = state.transport.write(offset, data);
            state.signal();
            return;
        }
        let effect = state.transport.write(offset, data);
        match effect {
            WriteEffect::Notify { queue }
                if queue as usize == HIPRIO || queue as usize == REQUEST =>
            {
                state.notified = true;
                self.shared.1.notify_one();
            }
            WriteEffect::Reset | WriteEffect::QueueReset { .. } => {
                state.epoch = state.epoch.wrapping_add(1);
                state.notified = true;
                state.signal();
                self.shared.1.notify_one();
            }
            WriteEffect::DriverOk => {
                if state.backend_failed {
                    state.fail("filesystem helper unavailable; restart the VM");
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn negotiate(state: &mut State) {
        state.transport.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1);
        state.transport.write(VIRTIO_MMIO_DRIVER_FEATURES, 1);
        state.transport.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0);
        state
            .transport
            .write(VIRTIO_MMIO_DRIVER_FEATURES, VIRTIO_RING_F_INDIRECT_DESC);
        state.transport.write(
            VIRTIO_MMIO_STATUS,
            (VIRTIO_STATUS_ACKNOWLEDGE
                | VIRTIO_STATUS_DRIVER
                | VIRTIO_STATUS_FEATURES_OK
                | VIRTIO_STATUS_DRIVER_OK) as u64,
        );
    }

    #[test]
    fn config_offers_one_request_queue_and_no_dax() {
        let config = config_space("home");
        assert_eq!(&config[..5], b"home\0");
        assert_eq!(config.len(), 40);
        assert_eq!(u32::from_le_bytes(config[36..40].try_into().unwrap()), 1);
        let mut state = State::new(48);
        assert_eq!(
            state.transport.read(VIRTIO_MMIO_DEVICE_ID),
            VIRTIO_DEV_FS as u64
        );
        assert_eq!(
            state.transport.read(VIRTIO_MMIO_DEVICE_FEATURES),
            VIRTIO_RING_F_INDIRECT_DESC
        );
        state.transport.write(VIRTIO_MMIO_DEVICE_FEATURES_SEL, 1);
        assert_eq!(state.transport.read(VIRTIO_MMIO_DEVICE_FEATURES), 1);
        state.transport.write(VIRTIO_MMIO_QUEUE_SEL, 0);
        assert_eq!(
            state.transport.read(VIRTIO_MMIO_QUEUE_NUM_MAX),
            QUEUE_MAX as u64
        );
        state.transport.write(VIRTIO_MMIO_QUEUE_SEL, 1);
        assert_eq!(
            state.transport.read(VIRTIO_MMIO_QUEUE_NUM_MAX),
            QUEUE_MAX as u64
        );
        state.transport.write(VIRTIO_MMIO_QUEUE_SEL, 2);
        assert_eq!(state.transport.read(VIRTIO_MMIO_QUEUE_NUM_MAX), 0);
    }

    #[test]
    fn destroy_is_a_no_reply_fuse_operation() {
        assert_eq!(min_reply(FUSE_DESTROY), None);
        assert_eq!(min_reply(FUSE_STATFS), Some(96));
    }

    #[test]
    fn short_header_needs_reset_without_a_used_entry() {
        let mut state = State::new(48);
        negotiate(&mut state);
        let mem = GuestMemory::new_test(0, 0x8000);
        {
            let q = state.transport.queue_mut(REQUEST).unwrap();
            q.size = 8;
            q.ready = true;
            q.desc_table = 0x100;
            q.avail_ring = 0x200;
            q.used_ring = 0x300;
        }
        mem.write_u16(0x200, VRING_AVAIL_F_NO_INTERRUPT).unwrap();
        mem.write_u64(0x100, 0x1000).unwrap();
        mem.write_u32(0x108, 8).unwrap();
        mem.write_u16(0x10c, VRING_DESC_F_NEXT).unwrap();
        mem.write_u16(0x10e, 1).unwrap();
        mem.write_u64(0x110, 0x2000).unwrap();
        mem.write_u32(0x118, 256).unwrap();
        mem.write_u16(0x11c, VRING_DESC_F_WRITE).unwrap();
        mem.write_u16(0x202, 1).unwrap();
        mem.write_u16(0x204, 0).unwrap();
        mem.write(0x1000, &[8, 0, 0, 0, 26, 0, 0, 0]).unwrap();
        mem.write(0x2000, &[0xA5; 16]).unwrap();
        let err = take_request(&mut state, &mem, REQUEST).unwrap_err();
        assert!(err.contains("shorter"));
        state.fail(&err);
        assert_ne!(state.transport.status() & VIRTIO_STATUS_NEEDS_RESET, 0);
        assert_eq!(mem.read_u16(0x302).unwrap(), 0);
        let mut got = [0u8; 16];
        mem.read(0x2000, &mut got).unwrap();
        assert_eq!(got, [0xA5; 16]);
    }

    #[test]
    fn helper_init_round_trip() {
        let dir = crate::test_support::TestDir::new();
        dir.file("note.txt", b"note");
        let share = crate::shared_fs::ShareConfig::new("share", dir.root().to_str().unwrap(), true)
            .unwrap();
        let backend = Backend::start_virtiofs_inprocess(&share).unwrap();
        let mem = Arc::new(GuestMemory::new_test(0, 0x8000));
        let mut dev = VirtioFs::open("share", 48, mem.clone(), backend).unwrap();
        dev.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1, 4);
        dev.write(VIRTIO_MMIO_DRIVER_FEATURES, 1, 4);
        dev.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0, 4);
        dev.write(VIRTIO_MMIO_DRIVER_FEATURES, VIRTIO_RING_F_INDIRECT_DESC, 4);
        dev.write(
            VIRTIO_MMIO_STATUS,
            (VIRTIO_STATUS_ACKNOWLEDGE
                | VIRTIO_STATUS_DRIVER
                | VIRTIO_STATUS_FEATURES_OK
                | VIRTIO_STATUS_DRIVER_OK) as u64,
            4,
        );
        dev.write(VIRTIO_MMIO_QUEUE_SEL, REQUEST as u64, 4);
        dev.write(VIRTIO_MMIO_QUEUE_NUM, 8, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DESC_LOW, 0x100, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DRIVER_LOW, 0x400, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DEVICE_LOW, 0x800, 4);
        mem.write_u16(0x400, VRING_AVAIL_F_NO_INTERRUPT).unwrap();
        dev.write(VIRTIO_MMIO_QUEUE_READY, 1, 4);
        let mut init = vec![0u8; 56];
        init[0..4].copy_from_slice(&56u32.to_le_bytes());
        init[4..8].copy_from_slice(&FUSE_INIT.to_le_bytes());
        init[8..16].copy_from_slice(&1u64.to_le_bytes());
        init[40..44].copy_from_slice(&7u32.to_le_bytes());
        init[44..48].copy_from_slice(&41u32.to_le_bytes());
        mem.write(0x1000, &init).unwrap();
        mem.write(0x1800, &[0xA5; 80]).unwrap();
        mem.write_u64(0x100, 0x1000).unwrap();
        mem.write_u32(0x108, 56).unwrap();
        mem.write_u16(0x10c, VRING_DESC_F_NEXT).unwrap();
        mem.write_u16(0x10e, 1).unwrap();
        mem.write_u64(0x110, 0x1800).unwrap();
        mem.write_u32(0x118, 80).unwrap();
        mem.write_u16(0x11c, VRING_DESC_F_WRITE).unwrap();
        mem.write_u16(0x402, 1).unwrap();
        mem.write_u16(0x404, 0).unwrap();
        dev.write(VIRTIO_MMIO_QUEUE_NOTIFY, REQUEST as u64, 4);
        let mut saw = false;
        for _ in 0..100 {
            if mem.read_u16(0x802).unwrap() == 1 {
                saw = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(saw, "helper did not complete FUSE_INIT");
        assert_eq!(mem.read_u32(0x1800).unwrap(), 80);
        assert_eq!(mem.read_u32(0x1804).unwrap(), 0);
        assert_eq!(mem.read_u32(0x1810).unwrap(), 7);
        assert_eq!(mem.read_u32(0x1814).unwrap(), 31);
        let flags = mem.read_u32(0x181c).unwrap();
        assert_eq!(flags & (1 << 20), 0);
        dev.shutdown().unwrap();
    }

    #[test]
    fn hiprio_runs_before_a_malformed_request() {
        let dir = crate::test_support::TestDir::new();
        let share = crate::shared_fs::ShareConfig::new("share", dir.root().to_str().unwrap(), true)
            .unwrap();
        let backend = Backend::start_virtiofs_inprocess(&share).unwrap();
        let mem = Arc::new(GuestMemory::new_test(0, 0x8000));
        let mut dev = VirtioFs::open("share", 48, mem.clone(), backend).unwrap();
        dev.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1, 4);
        dev.write(VIRTIO_MMIO_DRIVER_FEATURES, 1, 4);
        dev.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0, 4);
        dev.write(VIRTIO_MMIO_DRIVER_FEATURES, VIRTIO_RING_F_INDIRECT_DESC, 4);
        dev.write(
            VIRTIO_MMIO_STATUS,
            (VIRTIO_STATUS_ACKNOWLEDGE
                | VIRTIO_STATUS_DRIVER
                | VIRTIO_STATUS_FEATURES_OK
                | VIRTIO_STATUS_DRIVER_OK) as u64,
            4,
        );
        dev.write(VIRTIO_MMIO_QUEUE_SEL, HIPRIO as u64, 4);
        dev.write(VIRTIO_MMIO_QUEUE_NUM, 8, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DESC_LOW, 0x100, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DRIVER_LOW, 0x200, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DEVICE_LOW, 0x280, 4);
        mem.write_u16(0x200, VRING_AVAIL_F_NO_INTERRUPT).unwrap();
        dev.write(VIRTIO_MMIO_QUEUE_READY, 1, 4);
        dev.write(VIRTIO_MMIO_QUEUE_SEL, REQUEST as u64, 4);
        dev.write(VIRTIO_MMIO_QUEUE_NUM, 8, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DESC_LOW, 0x400, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DRIVER_LOW, 0x500, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DEVICE_LOW, 0x580, 4);
        mem.write_u16(0x500, VRING_AVAIL_F_NO_INTERRUPT).unwrap();
        dev.write(VIRTIO_MMIO_QUEUE_READY, 1, 4);

        let mut interrupt = vec![0u8; 48];
        interrupt[0..4].copy_from_slice(&48u32.to_le_bytes());
        interrupt[4..8].copy_from_slice(&FUSE_INTERRUPT.to_le_bytes());
        mem.write(0x300, &interrupt).unwrap();
        mem.write_u64(0x100, 0x300).unwrap();
        mem.write_u32(0x108, 48).unwrap();
        mem.write_u16(0x202, 1).unwrap();

        mem.write(0x600, &[8, 0, 0, 0, 26, 0, 0, 0]).unwrap();
        mem.write(0x700, &[0xA5; 16]).unwrap();
        mem.write_u64(0x400, 0x600).unwrap();
        mem.write_u32(0x408, 8).unwrap();
        mem.write_u16(0x40c, VRING_DESC_F_NEXT).unwrap();
        mem.write_u16(0x40e, 1).unwrap();
        mem.write_u64(0x410, 0x700).unwrap();
        mem.write_u32(0x418, 16).unwrap();
        mem.write_u16(0x41c, VRING_DESC_F_WRITE).unwrap();
        mem.write_u16(0x502, 1).unwrap();

        dev.write(VIRTIO_MMIO_QUEUE_NOTIFY, REQUEST as u64, 4);
        let mut saw = false;
        for _ in 0..100 {
            let hiprio_done = mem.read_u16(0x282).unwrap() == 1;
            let needs_reset =
                dev.read(VIRTIO_MMIO_STATUS, 4) & u64::from(VIRTIO_STATUS_NEEDS_RESET) != 0;
            if hiprio_done && needs_reset {
                saw = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(saw, "hiprio interrupt was not completed before NEEDS_RESET");
        assert_eq!(mem.read_u32(0x284).unwrap(), 0);
        assert_eq!(mem.read_u32(0x288).unwrap(), 0);
        assert_eq!(mem.read_u16(0x582).unwrap(), 0);
        let mut got = [0u8; 16];
        mem.read(0x700, &mut got).unwrap();
        assert_eq!(got, [0xA5; 16]);
        dev.shutdown().unwrap();
    }

    #[test]
    fn reset_discards_an_in_flight_reply() {
        let mut state = State::new(48);
        negotiate(&mut state);
        let mem = GuestMemory::new_test(0, 0x8000);
        {
            let q = state.transport.queue_mut(REQUEST).unwrap();
            q.size = 8;
            q.ready = true;
            q.desc_table = 0x100;
            q.avail_ring = 0x400;
            q.used_ring = 0x800;
        }
        mem.write_u16(0x400, VRING_AVAIL_F_NO_INTERRUPT).unwrap();
        let mut init = vec![0u8; 56];
        init[0..4].copy_from_slice(&56u32.to_le_bytes());
        init[4..8].copy_from_slice(&FUSE_INIT.to_le_bytes());
        init[40..44].copy_from_slice(&7u32.to_le_bytes());
        mem.write(0x1000, &init).unwrap();
        mem.write(0x1800, &[0xA5; 80]).unwrap();
        mem.write_u64(0x100, 0x1000).unwrap();
        mem.write_u32(0x108, 56).unwrap();
        mem.write_u16(0x10c, VRING_DESC_F_NEXT).unwrap();
        mem.write_u16(0x10e, 1).unwrap();
        mem.write_u64(0x110, 0x1800).unwrap();
        mem.write_u32(0x118, 80).unwrap();
        mem.write_u16(0x11c, VRING_DESC_F_WRITE).unwrap();
        mem.write_u16(0x402, 1).unwrap();
        let request = take_request(&mut state, &mem, REQUEST).unwrap().unwrap();
        state.epoch = state.epoch.wrapping_add(1);
        complete(&mut state, &mem, request, &[0; 80]).unwrap();
        assert_eq!(mem.read_u16(0x802).unwrap(), 0);
        assert_eq!(state.transport.diagnostics().drops, 1);
        let mut got = [0u8; 16];
        mem.read(0x1800, &mut got).unwrap();
        assert_eq!(got, [0xA5; 16]);
    }
}
