//! Modern VirtIO-9P transport. Filesystem I/O never holds the device lock.
use super::*;
use crate::devices::MmioDevice;
use crate::memory::GuestMemory;
use crate::shared_fs::{Backend, MAX_MESSAGE};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};

const FEATURES: u64 = VIRTIO_F_VERSION_1 | VIRTIO_RING_F_INDIRECT_DESC | 1;
const QUEUE_SIZE: u32 = 128;

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
            transport: MmioTransport::with_policy(
                VIRTIO_DEV_9P,
                FEATURES,
                &[QUEUE_SIZE],
                irq,
                true,
            ),
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
        eprintln!("[VirtIO-9P] {error}");
        self.transport.set_needs_reset();
        self.transport.set_interrupt_bit(2);
        self.signal();
    }
}

pub struct VirtioP9 {
    shared: Arc<(Mutex<State>, Condvar)>,
    child: Arc<Mutex<Option<std::process::Child>>>,
    worker: Option<JoinHandle<()>>,
    config: Vec<u8>,
}

struct Request {
    head: u16,
    epoch: u64,
    bytes: Vec<u8>,
    output: Vec<Descriptor>,
}

fn take_request(state: &mut State, mem: &GuestMemory) -> Result<Option<Request>, String> {
    if !state.transport.operational() {
        return Ok(None);
    }
    let Some(mut q) = state.transport.queue(0).copied() else {
        return Ok(None);
    };
    if !q.ready {
        return Ok(None);
    }
    if q.pending_count(mem)? == 0 {
        return Ok(None);
    }
    let head = mem.read_u16(q.avail_ring + 4 + 2 * (q.last_avail_idx as u64 % q.size as u64))?;
    let descs = q.descriptors(
        mem,
        head,
        state.transport.negotiated(VIRTIO_RING_F_INDIRECT_DESC),
        2 * MAX_MESSAGE,
    )?;
    let mut bytes = Vec::new();
    let mut output = Vec::new();
    let mut capacity = 0usize;
    for d in descs {
        if d.flags & VRING_DESC_F_WRITE != 0 {
            capacity += d.len as usize;
            output.push(d);
        } else {
            if !output.is_empty() || bytes.len() + d.len as usize > MAX_MESSAGE {
                return Err("invalid request descriptor ordering/size".into());
            }
            let start = bytes.len();
            bytes.resize(start + d.len as usize, 0);
            mem.read(d.addr, &mut bytes[start..])?;
        }
    }
    if bytes.len() < 7 || capacity < 11 || capacity > MAX_MESSAGE {
        return Err("invalid 9P request/response capacity".into());
    }
    let minimum = match bytes[4] {
        100 => 21,
        104 | 16 | 72 => 20,
        12 | 112 | 14 | 114 => 24,
        _ => 11,
    };
    if capacity < minimum {
        return Err("response buffer too small for request before filesystem I/O".into());
    }
    q.last_avail_idx = q.last_avail_idx.wrapping_add(1);
    let epoch = state.epoch;
    if let Some(slot) = state.transport.queue_mut(0) {
        *slot = q;
    }
    Ok(Some(Request {
        head,
        epoch,
        bytes,
        output,
    }))
}

fn complete(
    state: &mut State,
    mem: &GuestMemory,
    request: Request,
    mut reply: Vec<u8>,
) -> Result<(), String> {
    if request.epoch != state.epoch {
        return Ok(());
    }
    let Some(mut queue) = state.transport.queue(0).copied() else {
        return Ok(());
    };
    if !queue.ready {
        return Ok(());
    }
    let capacity: usize = request.output.iter().map(|d| d.len as usize).sum();
    if reply.len() > capacity {
        reply = vec![
            11,
            0,
            0,
            0,
            7,
            request.bytes[5],
            request.bytes[6],
            22,
            0,
            0,
            0,
        ];
    }
    let mut offset = 0;
    for d in request.output {
        let n = (reply.len() - offset).min(d.len as usize);
        mem.write(d.addr, &reply[offset..offset + n])?;
        offset += n;
        if offset == reply.len() {
            break;
        }
    }
    publish_used(mem, &mut queue, request.head, reply.len() as u32);
    let avail = queue.avail_ring;
    if let Some(slot) = state.transport.queue_mut(0) {
        *slot = queue;
    }
    if avail_wants_interrupt(mem, avail) {
        state.transport.set_interrupt_bit(1);
        state.signal();
    }
    Ok(())
}

impl VirtioP9 {
    pub fn new(
        tag: &str,
        irq: u32,
        mem: Arc<GuestMemory>,
        mut backend: Backend,
    ) -> Result<Self, String> {
        let shared = Arc::new((Mutex::new(State::new(irq)), Condvar::new()));
        let worker_shared = shared.clone();
        let child = backend.child.clone();
        let worker = thread::Builder::new()
            .name(format!("9p-{tag}"))
            .spawn(move || {
                let (lock, wake) = &*worker_shared;
                let mut epoch = 0;
                loop {
                    let mut state = lock.lock().unwrap();
                    while !state.notified && !state.stopped {
                        state = wake.wait(state).unwrap();
                    }
                    if state.stopped {
                        break;
                    }
                    state.notified = false;
                    if epoch != state.epoch {
                        epoch = state.epoch;
                        drop(state);
                        if let Err(e) = backend.request(&[]) {
                            let mut state = lock.lock().unwrap();
                            state.backend_failed = true;
                            state.fail(&e);
                            break;
                        }
                        lock.lock().unwrap().notified = true;
                        continue;
                    }
                    let request = match take_request(&mut state, &mem) {
                        Ok(Some(r)) => r,
                        Ok(None) => continue,
                        Err(e) => {
                            state.fail(&e);
                            continue;
                        }
                    };
                    // Drain incrementally with at most one filesystem request in flight.
                    state.notified = true;
                    drop(state);
                    let result = backend.request(&request.bytes);
                    let mut state = lock.lock().unwrap();
                    if state.stopped {
                        break;
                    }
                    match result {
                        Ok(reply) => {
                            if let Err(e) = complete(&mut state, &mem, request, reply) {
                                state.fail(&e);
                            }
                        }
                        Err(e) => {
                            state.backend_failed = true;
                            state.fail(&e);
                            break;
                        }
                    }
                }
            })
            .map_err(|e| format!("9P worker: {e}"))?;
        let mut config = (tag.len() as u16).to_le_bytes().to_vec();
        config.extend(tag.as_bytes());
        Ok(Self {
            shared,
            child,
            worker: Some(worker),
            config,
        })
    }

    fn stop(&mut self) -> Result<(), String> {
        {
            let mut s = self.shared.0.lock().unwrap();
            s.stopped = true;
            self.shared.1.notify_all();
        }
        // Kill unresponsive filesystem helper before joining the transport thread.
        // Accepted writes are not transactional; the guest must sync/unmount first.
        if let Ok(mut child) = self.child.lock() {
            if let Some(child) = child.as_mut() {
                let _ = child.kill();
            }
        }
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| "9P worker panicked")?;
        }
        Ok(())
    }
}

impl Drop for VirtioP9 {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

impl MmioDevice for VirtioP9 {
    fn shutdown(&mut self) -> Result<(), String> {
        self.stop()
    }

    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if offset >= VIRTIO_MMIO_CONFIG {
            let start = (offset - VIRTIO_MMIO_CONFIG) as usize;
            let mut raw = [0; 8];
            if size > 8 {
                return 0;
            }
            for (i, b) in raw[..size].iter_mut().enumerate() {
                *b = self.config.get(start + i).copied().unwrap_or(0);
            }
            return u64::from_le_bytes(raw);
        }
        self.shared.0.lock().unwrap().transport.read(offset)
    }

    fn write(&mut self, offset: u64, data: u64, size: usize) {
        if size != 4 || offset >= VIRTIO_MMIO_CONFIG {
            return;
        }
        let mut s = self.shared.0.lock().unwrap();
        if offset == VIRTIO_MMIO_INTERRUPT_ACK {
            let _ = s.transport.write(offset, data);
            s.signal();
            return;
        }
        let effect = s.transport.write(offset, data);
        match effect {
            WriteEffect::Notify { queue: 0 } => {
                s.notified = true;
                self.shared.1.notify_one();
            }
            WriteEffect::Reset => {
                s.epoch = s.epoch.wrapping_add(1);
                s.notified = true;
                s.signal();
                self.shared.1.notify_one();
            }
            WriteEffect::QueueReset { .. } => {
                s.epoch = s.epoch.wrapping_add(1);
                s.notified = true;
                self.shared.1.notify_one();
            }
            WriteEffect::DriverOk => {
                if s.backend_failed {
                    s.fail("filesystem helper unavailable; restart the VM");
                }
            }
            WriteEffect::NegotiationFailed => {
                if s.transport.status() & VIRTIO_STATUS_NEEDS_RESET != 0 {
                    eprintln!("[VirtIO-9P] DRIVER_OK with invalid features");
                    s.transport.set_interrupt_bit(2);
                    s.signal();
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> (State, GuestMemory) {
        let mut s = State::new(48);
        s.transport.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1);
        s.transport.write(VIRTIO_MMIO_DRIVER_FEATURES, 1);
        s.transport.write(
            VIRTIO_MMIO_STATUS,
            (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK) as u64,
        );
        assert!(s.transport.operational());
        {
            let q = s.transport.queue_mut(0).unwrap();
            q.size = 8;
            q.ready = true;
            q.desc_table = 0x100;
            q.avail_ring = 0x200;
            q.used_ring = 0x300;
        }
        let mem = GuestMemory::new_test(0, 0x8000);
        mem.write_u64(0x100, 0x1000).unwrap();
        mem.write_u32(0x108, 7).unwrap();
        mem.write_u16(0x10c, VRING_DESC_F_NEXT).unwrap();
        mem.write_u16(0x10e, 1).unwrap();
        mem.write_u64(0x110, 0x2000).unwrap();
        mem.write_u32(0x118, 256).unwrap();
        mem.write_u16(0x11c, VRING_DESC_F_WRITE).unwrap();
        mem.write_u16(0x202, 1).unwrap();
        mem.write_u16(0x204, 0).unwrap();
        mem.write(0x1000, &[7, 0, 0, 0, 120, 42, 0]).unwrap();
        mem.write_u16(0x200, VRING_AVAIL_F_NO_INTERRUPT).unwrap();
        (s, mem)
    }

    #[test]
    fn scatter_response_and_reset_discard() {
        let (mut s, mem) = setup();
        let request = take_request(&mut s, &mem).unwrap().unwrap();
        s.epoch += 1;
        complete(&mut s, &mem, request, vec![1; 11]).unwrap();
        assert_eq!(mem.read_u16(0x302).unwrap(), 0);
        s.transport.queue_mut(0).unwrap().last_avail_idx = 0;
        let request = take_request(&mut s, &mem).unwrap().unwrap();
        complete(&mut s, &mem, request, vec![3; 11]).unwrap();
        assert_eq!(mem.read_u16(0x302).unwrap(), 1);
        assert_eq!(mem.read_u32(0x308).unwrap(), 11);
        assert_eq!(mem.read_u16(0x2000).unwrap(), 0x0303);
    }

    #[test]
    fn bad_direction_and_short_create_reply_rejected_before_io() {
        let (mut s, mem) = setup();
        mem.write_u16(0x11c, 0).unwrap();
        assert!(take_request(&mut s, &mem).is_err());
        mem.write_u16(0x11c, VRING_DESC_F_WRITE).unwrap();
        mem.write_u32(0x118, 11).unwrap();
        mem.write(0x1004, &[114]).unwrap();
        assert!(take_request(&mut s, &mem).is_err());
        assert_eq!(s.transport.queue(0).unwrap().last_avail_idx, 0);
    }

    #[test]
    fn freebsd_driver_ok_without_features_ok_accepts_only_a_valid_set() {
        let mut ok = State::new(48);
        ok.transport.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1);
        ok.transport.write(VIRTIO_MMIO_DRIVER_FEATURES, 1);
        assert_eq!(
            ok.transport.write(
                VIRTIO_MMIO_STATUS,
                (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK) as u64,
            ),
            WriteEffect::DriverOk
        );
        assert_eq!(ok.transport.status() & VIRTIO_STATUS_FEATURES_OK, 0);
        assert!(ok.transport.operational());

        let mut bad = State::new(48);
        bad.transport.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0);
        bad.transport.write(VIRTIO_MMIO_DRIVER_FEATURES, 1 << 3);
        bad.transport.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1);
        bad.transport.write(VIRTIO_MMIO_DRIVER_FEATURES, 1);
        assert_eq!(
            bad.transport.write(
                VIRTIO_MMIO_STATUS,
                (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_DRIVER_OK) as u64,
            ),
            WriteEffect::NegotiationFailed
        );
        assert_ne!(bad.transport.status() & VIRTIO_STATUS_NEEDS_RESET, 0);
        assert!(!bad.transport.operational());
    }
}
