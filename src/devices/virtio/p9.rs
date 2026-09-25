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
    status: u32,
    feature_sel: u32,
    driver_sel: u32,
    features: u64,
    queue_sel: u32,
    queue: VirtQueue,
    interrupt: u32,
    epoch: u64,
    notified: bool,
    stopped: bool,
    backend_failed: bool,
    irq: u32,
}
impl State {
    fn new(irq: u32) -> Self {
        Self {
            status: 0,
            feature_sel: 0,
            driver_sel: 0,
            features: 0,
            queue_sel: 0,
            queue: VirtQueue {
                max_size: QUEUE_SIZE,
                ..Default::default()
            },
            interrupt: 0,
            epoch: 0,
            notified: false,
            stopped: false,
            backend_failed: false,
            irq,
        }
    }
    fn signal(&self) {
        #[cfg(not(test))]
        unsafe {
            crate::hypervisor::hv_gic_set_spi(self.irq, self.interrupt != 0);
        }
        if self.interrupt != 0 {
            crate::vcpu::kick_vcpu();
        }
    }
    fn fail(&mut self, error: &str) {
        eprintln!("[VirtIO-9P] {error}");
        self.status |= VIRTIO_STATUS_NEEDS_RESET;
        self.interrupt |= 2;
        self.signal();
    }
}
pub struct VirtioP9 {
    shared: Arc<(Mutex<State>, Condvar)>,
    child: Arc<Mutex<std::process::Child>>,
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
    if state.status & (VIRTIO_STATUS_DRIVER_OK | VIRTIO_STATUS_NEEDS_RESET | VIRTIO_STATUS_FAILED)
        != VIRTIO_STATUS_DRIVER_OK
        || !state.queue.ready
    {
        return Ok(None);
    }
    let q = &mut state.queue;
    if q.pending_count(mem)? == 0 {
        return Ok(None);
    }
    let head = mem.read_u16(q.avail_ring + 4 + 2 * (q.last_avail_idx as u64 % q.size as u64))?;
    let descs = q.descriptors(
        mem,
        head,
        state.features & VIRTIO_RING_F_INDIRECT_DESC != 0,
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
    Ok(Some(Request {
        head,
        epoch: state.epoch,
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
    if request.epoch != state.epoch || !state.queue.ready {
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
    publish_used(mem, &mut state.queue, request.head, reply.len() as u32);
    if avail_wants_interrupt(mem, state.queue.avail_ring) {
        state.interrupt |= 1;
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
            let _ = child.kill();
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
        let s = self.shared.0.lock().unwrap();
        match offset {
            VIRTIO_MMIO_MAGIC_VALUE => VIRTIO_MMIO_MAGIC as u64,
            VIRTIO_MMIO_VERSION => 2,
            VIRTIO_MMIO_DEVICE_ID => 9,
            VIRTIO_MMIO_VENDOR_ID => VIRTIO_MMIO_VENDOR as u64,
            VIRTIO_MMIO_DEVICE_FEATURES if s.feature_sel < 2 => {
                (FEATURES >> (s.feature_sel * 32)) & 0xffffffff
            }
            VIRTIO_MMIO_QUEUE_NUM_MAX if s.queue_sel == 0 => QUEUE_SIZE as u64,
            VIRTIO_MMIO_QUEUE_READY if s.queue_sel == 0 => s.queue.ready as u64,
            VIRTIO_MMIO_INTERRUPT_STATUS => s.interrupt as u64,
            VIRTIO_MMIO_STATUS => s.status as u64,
            _ => 0,
        }
    }
    fn write(&mut self, offset: u64, data: u64, size: usize) {
        if size != 4 {
            return;
        }
        let value = data as u32;
        let mut s = self.shared.0.lock().unwrap();
        match offset {
            VIRTIO_MMIO_DEVICE_FEATURES_SEL => s.feature_sel = value,
            VIRTIO_MMIO_DRIVER_FEATURES_SEL => s.driver_sel = value,
            VIRTIO_MMIO_DRIVER_FEATURES
                if s.driver_sel < 2
                    && s.status & (VIRTIO_STATUS_FEATURES_OK | VIRTIO_STATUS_DRIVER_OK) == 0 =>
            {
                let shift = s.driver_sel * 32;
                s.features = (s.features & !(0xffffffff << shift)) | ((value as u64) << shift);
            }
            VIRTIO_MMIO_QUEUE_SEL => s.queue_sel = value,
            VIRTIO_MMIO_QUEUE_NOTIFY if value == 0 => {
                s.notified = true;
                self.shared.1.notify_one();
            }
            VIRTIO_MMIO_INTERRUPT_ACK => {
                s.interrupt &= !value;
                s.signal();
            }
            VIRTIO_MMIO_STATUS => {
                if value == 0 {
                    let epoch = s.epoch.wrapping_add(1);
                    let irq = s.irq;
                    let backend_failed = s.backend_failed;
                    *s = State::new(irq);
                    s.backend_failed = backend_failed;
                    s.epoch = epoch;
                    s.notified = true;
                    s.signal();
                    self.shared.1.notify_one();
                } else {
                    s.status = value;
                    if s.backend_failed && value & VIRTIO_STATUS_DRIVER_OK != 0 {
                        s.fail("filesystem helper unavailable; restart the VM");
                    }
                    if s.status & VIRTIO_STATUS_FEATURES_OK != 0
                        && (s.features & !FEATURES != 0 || s.features & VIRTIO_F_VERSION_1 == 0)
                    {
                        s.status &= !VIRTIO_STATUS_FEATURES_OK;
                    }
                    // FreeBSD 14.4 virtio_p9fs omits virtio_finalize_features().
                    // Accept its DRIVER_OK transition only with a valid modern
                    // feature set; clients that set FEATURES_OK still use the
                    // normal feature acceptance check above.
                    if s.status & VIRTIO_STATUS_DRIVER_OK != 0
                        && (s.features & !FEATURES != 0 || s.features & VIRTIO_F_VERSION_1 == 0)
                    {
                        s.fail("DRIVER_OK with invalid features");
                    }
                }
            }
            VIRTIO_MMIO_QUEUE_READY if s.queue_sel == 0 => {
                if value == 0 {
                    s.epoch = s.epoch.wrapping_add(1);
                    s.queue.last_avail_idx = 0;
                    s.queue.used_idx = 0;
                    s.notified = true;
                    self.shared.1.notify_one();
                }
                s.queue.ready = value == 1;
            }
            _ if s.queue_sel == 0 && !s.queue.ready => {
                let q = &mut s.queue;
                let address = match offset {
                    VIRTIO_MMIO_QUEUE_NUM => {
                        q.size = value;
                        return;
                    }
                    VIRTIO_MMIO_QUEUE_DESC_LOW | VIRTIO_MMIO_QUEUE_DESC_HIGH => &mut q.desc_table,
                    VIRTIO_MMIO_QUEUE_DRIVER_LOW | VIRTIO_MMIO_QUEUE_DRIVER_HIGH => {
                        &mut q.avail_ring
                    }
                    VIRTIO_MMIO_QUEUE_DEVICE_LOW | VIRTIO_MMIO_QUEUE_DEVICE_HIGH => {
                        &mut q.used_ring
                    }
                    _ => return,
                };
                if offset & 4 == 0 {
                    *address = (*address & !0xffffffff) | value as u64;
                } else {
                    *address = (*address & 0xffffffff) | ((value as u64) << 32);
                }
            }
            _ => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn setup() -> (State, GuestMemory) {
        let mut s = State::new(48);
        s.status = VIRTIO_STATUS_DRIVER_OK;
        s.features = VIRTIO_F_VERSION_1;
        s.queue = VirtQueue {
            max_size: 128,
            size: 8,
            ready: true,
            desc_table: 0x100,
            avail_ring: 0x200,
            used_ring: 0x300,
            ..Default::default()
        };
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
        s.queue.last_avail_idx = 0;
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
        assert_eq!(s.queue.last_avail_idx, 0);
    }
}
