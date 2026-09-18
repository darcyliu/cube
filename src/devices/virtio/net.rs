//! VirtIO Network Device (`virtio-net`) implementation over Modern MMIO.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use super::*;
use crate::devices::MmioDevice;
use crate::memory::GuestMemory;

// VirtIO Network Features
const VIRTIO_NET_F_MAC_BIT: u64 = 1 << 5;
const VIRTIO_NET_F_STATUS_BIT: u64 = 1 << 16;
const VIRTIO_F_VERSION_1_BIT: u64 = 1 << 32;

// VirtIO Network Status
const VIRTIO_NET_S_LINK_UP_VAL: u16 = 1;

struct NetDebugStats {
    tx_pkts: AtomicU64,
    tx_bytes: AtomicU64,
    rx_pkts: AtomicU64,
    rx_bytes: AtomicU64,
    rx_drop: AtomicU64,
    rx_nomem: AtomicU64,
    irq_raise: AtomicU64,
    irq_ack: AtomicU64,
    irq_defer: AtomicU64,
    tx_notify: AtomicU64,
    vmnet_err: AtomicU64,
}

static NET_STATS: NetDebugStats = NetDebugStats {
    tx_pkts: AtomicU64::new(0),
    tx_bytes: AtomicU64::new(0),
    rx_pkts: AtomicU64::new(0),
    rx_bytes: AtomicU64::new(0),
    rx_drop: AtomicU64::new(0),
    rx_nomem: AtomicU64::new(0),
    irq_raise: AtomicU64::new(0),
    irq_ack: AtomicU64::new(0),
    irq_defer: AtomicU64::new(0),
    tx_notify: AtomicU64::new(0),
    vmnet_err: AtomicU64::new(0),
};

fn net_log(msg: &str) {
    if let Ok(mut f) = OpenOptions::new()
        .create(true)
        .append(true)
        .open("/tmp/vmm-net.log")
    {
        let _ = writeln!(f, "{}", msg);
        let _ = f.flush();
    }
}

/// Network backend trait for sending and receiving Ethernet frames.
pub trait NetBackend: Send {
    fn send_packet(&mut self, packet: &[u8]) -> Result<(), String>;
}

// -----------------------------------------------------------------------------
// macOS vmnet.framework FFI & Backend
// -----------------------------------------------------------------------------

#[repr(C)]
struct VmPktDesc {
    vm_pkt_size: u64,
    vm_pkt_iov: *mut Iovec,
    vm_pkt_iovcnt: u32,
    vm_flags: u32,
}

#[repr(C)]
struct Iovec {
    iov_base: *mut c_void,
    iov_len: usize,
}

#[repr(C)]
struct BlockDescriptor {
    reserved: usize,
    size: usize,
}

static BLOCK_DESCRIPTOR: BlockDescriptor = BlockDescriptor {
    reserved: 0,
    size: std::mem::size_of::<BlockLiteral>(),
};

#[repr(C)]
struct BlockLiteral {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: unsafe extern "C" fn(*mut BlockLiteral, u32, *mut c_void),
    descriptor: *const BlockDescriptor,
}

unsafe impl Sync for BlockLiteral {}
unsafe impl Send for BlockLiteral {}

#[derive(Clone, Copy)]
struct SendPtr(*mut c_void);
unsafe impl Send for SendPtr {}
impl SendPtr {
    fn get(&self) -> *mut c_void {
        self.0
    }
}

unsafe extern "C" {
    static _NSConcreteGlobalBlock: [u8; 0];
}

#[link(name = "vmnet", kind = "framework")]
unsafe extern "C" {
    fn vmnet_start_interface(
        interface_desc: *mut c_void,
        queue: *mut c_void,
        handler: *const c_void,
    ) -> *mut c_void;

    fn vmnet_stop_interface(
        interface: *mut c_void,
        queue: *mut c_void,
        handler: *const c_void,
    ) -> u32;

    fn vmnet_read(
        interface: *mut c_void,
        packets: *mut VmPktDesc,
        pktcnt: *mut i32,
    ) -> u32;

    fn vmnet_write(
        interface: *mut c_void,
        packets: *mut VmPktDesc,
        pktcnt: *mut i32,
    ) -> u32;

    fn vmnet_interface_set_event_callback(
        interface: *mut c_void,
        event_mask: u32,
        queue: *mut c_void,
        callback: *const c_void,
    ) -> u32;
}

unsafe extern "C" {
    fn xpc_dictionary_create(
        keys: *const *const i8,
        values: *const *mut c_void,
        count: usize,
    ) -> *mut c_void;

    fn xpc_dictionary_set_uint64(
        dictionary: *mut c_void,
        key: *const i8,
        value: u64,
    );

    fn xpc_release(object: *mut c_void);

    fn dispatch_queue_create(
        label: *const i8,
        attr: *mut c_void,
    ) -> *mut c_void;

    fn dispatch_release(object: *mut c_void);
}

const VMNET_SHARED_MODE: u64 = 1000;
const VMNET_SUCCESS: u32 = 1000;
const VMNET_FAILURE: u32 = 1001;
const VMNET_INTERFACE_PACKETS_AVAILABLE: u32 = 1;

static VMNET_INIT_STATUS: Mutex<Option<u32>> = Mutex::new(None);

unsafe extern "C" fn vmnet_completion_invoke(
    _block: *mut BlockLiteral,
    status: u32,
    _iface_param: *mut c_void,
) {
    let mut st = VMNET_INIT_STATUS.lock().unwrap();
    *st = Some(status);
}

static VMNET_COMPLETION_BLOCK: BlockLiteral = BlockLiteral {
    isa: unsafe { _NSConcreteGlobalBlock.as_ptr() as *const c_void },
    flags: (1 << 29) | (1 << 28), // BLOCK_IS_GLOBAL
    reserved: 0,
    invoke: vmnet_completion_invoke,
    descriptor: &BLOCK_DESCRIPTOR,
};

pub struct VmnetBackend {
    interface: *mut c_void,
    dispatch_queue: *mut c_void,
}

unsafe impl Send for VmnetBackend {}

impl VmnetBackend {
    pub fn new() -> Result<Self, String> {
        let desc = unsafe { xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0) };
        if desc.is_null() {
            return Err("Failed to create XPC dictionary for vmnet".into());
        }

        unsafe {
            xpc_dictionary_set_uint64(
                desc,
                b"vmnet_operation_mode\0".as_ptr() as *const i8,
                VMNET_SHARED_MODE,
            );
        }

        let queue = unsafe {
            dispatch_queue_create(
                b"org.vmm.vmnet\0".as_ptr() as *const i8,
                std::ptr::null_mut(),
            )
        };
        if queue.is_null() {
            unsafe { xpc_release(desc) };
            return Err("Failed to create dispatch queue for vmnet".into());
        }

        {
            let mut st = VMNET_INIT_STATUS.lock().unwrap();
            *st = None;
        }

        let interface = unsafe {
            vmnet_start_interface(
                desc,
                queue,
                &VMNET_COMPLETION_BLOCK as *const BlockLiteral as *const c_void,
            )
        };

        unsafe { xpc_release(desc) };

        if interface.is_null() {
            unsafe { dispatch_release(queue) };
            return Err("vmnet_start_interface returned null".into());
        }

        // Wait up to 1 second for completion handler
        let start = std::time::Instant::now();
        let mut status = None;
        while start.elapsed() < Duration::from_millis(1000) {
            {
                let st = VMNET_INIT_STATUS.lock().unwrap();
                if let Some(s) = *st {
                    status = Some(s);
                    break;
                }
            }
            thread::sleep(Duration::from_millis(10));
        }

        match status {
            Some(VMNET_SUCCESS) => {
                println!("[VirtIO-Net] macOS vmnet NAT interface started successfully.");
                Ok(Self {
                    interface,
                    dispatch_queue: queue,
                })
            }
            Some(VMNET_FAILURE) => {
                unsafe { dispatch_release(queue) };
                Err(format!(
                    "vmnet_start_interface failed with status {} (Permission Denied). On macOS, vmnet.framework requires root privileges (sudo) or the 'com.apple.vm.networking' entitlement.",
                    VMNET_FAILURE
                ))
            }
            Some(s) => {
                unsafe { dispatch_release(queue) };
                Err(format!("vmnet_start_interface failed with status {}", s))
            }
            None => {
                unsafe { dispatch_release(queue) };
                Err("Timed out waiting for vmnet_start_interface completion handler".into())
            }
        }
    }
}

impl NetBackend for VmnetBackend {
    fn send_packet(&mut self, packet: &[u8]) -> Result<(), String> {
        if self.interface.is_null() {
            return Ok(());
        }
        let mut iov = Iovec {
            iov_base: packet.as_ptr() as *mut c_void,
            iov_len: packet.len(),
        };
        let mut desc = VmPktDesc {
            vm_pkt_size: packet.len() as u64,
            vm_pkt_iov: &mut iov,
            vm_pkt_iovcnt: 1,
            vm_flags: 0,
        };
        let mut pktcnt: i32 = 1;
        let ret = unsafe { vmnet_write(self.interface, &mut desc, &mut pktcnt) };
        if ret == VMNET_SUCCESS {
            Ok(())
        } else {
            NET_STATS.vmnet_err.fetch_add(1, Ordering::Relaxed);
            Err(format!("vmnet_write error: {}", ret))
        }
    }
}

impl Drop for VmnetBackend {
    fn drop(&mut self) {
        if !self.interface.is_null() {
            unsafe {
                let _ = vmnet_stop_interface(
                    self.interface,
                    self.dispatch_queue,
                    std::ptr::null(),
                );
                dispatch_release(self.dispatch_queue);
            }
        }
    }
}

// -----------------------------------------------------------------------------
// TAP / File descriptor backend
// -----------------------------------------------------------------------------

pub struct TapBackend {
    file: std::fs::File,
}

impl TapBackend {
    pub fn open(path: &str) -> Result<Self, String> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|e| format!("Failed to open TAP device '{}': {}", path, e))?;
        println!("[VirtIO-Net] Opened TAP device '{}'", path);
        Ok(Self { file })
    }
}

impl NetBackend for TapBackend {
    fn send_packet(&mut self, packet: &[u8]) -> Result<(), String> {
        self.file
            .write_all(packet)
            .map_err(|e| format!("TAP write error: {}", e))
    }
}

// -----------------------------------------------------------------------------
// User-mode / Mock fallback backend
// -----------------------------------------------------------------------------

pub struct UserMockBackend {
    last_sent_len: usize,
}

impl UserMockBackend {
    pub fn new() -> Self {
        println!("[VirtIO-Net] Initialized user-mode mock backend (loopback / drop)");
        Self { last_sent_len: 0 }
    }
}

impl NetBackend for UserMockBackend {
    fn send_packet(&mut self, packet: &[u8]) -> Result<(), String> {
        self.last_sent_len = packet.len();
        // In user mock mode, packets are safely consumed/dropped
        Ok(())
    }
}

// -----------------------------------------------------------------------------
// VirtIO Network Device Core Implementation
// -----------------------------------------------------------------------------

pub struct VirtioNetInner {
    mac: [u8; 6],
    status: u32,
    device_features_sel: u32,
    driver_features_sel: u32,
    driver_features: u64,
    rx_queue: VirtQueue,
    tx_queue: VirtQueue,
    queue_sel: u32,
    interrupt_status: u32,
    irq_num: u32,
    mem: Arc<GuestMemory>,
    backend: Arc<Mutex<Box<dyn NetBackend>>>,
    rx_backlog: VecDeque<Vec<u8>>,
    /// GIC SPI line currently driven high.
    irq_asserted: bool,
    /// Used-buffer event arrived while the SPI was already high. Re-raise after ACK.
    irq_pending_after_ack: bool,
    /// Time of the last INTERRUPT_ACK. Re-assert is deferred until the guest ERETs.
    last_ack: std::time::Instant,
}

impl VirtioNetInner {
    pub fn new(
        mac: [u8; 6],
        irq_num: u32,
        mem: Arc<GuestMemory>,
        backend: Box<dyn NetBackend>,
    ) -> Self {
        Self {
            mac,
            status: 0,
            device_features_sel: 0,
            driver_features_sel: 0,
            driver_features: 0,
            rx_queue: VirtQueue {
                max_size: 1024,
                size: 1024,
                ..Default::default()
            },
            tx_queue: VirtQueue {
                max_size: 1024,
                size: 1024,
                ..Default::default()
            },
            queue_sel: 0,
            interrupt_status: 0,
            irq_num,
            mem,
            backend: Arc::new(Mutex::new(backend)),
            rx_backlog: VecDeque::new(),
            irq_asserted: false,
            irq_pending_after_ack: false,
            last_ack: std::time::Instant::now()
                .checked_sub(Duration::from_millis(10))
                .unwrap_or_else(std::time::Instant::now),
        }
    }

    fn notify_used(&mut self, avail_ring: u64) {
        self.interrupt_status |= 1;
        std::sync::atomic::fence(Ordering::SeqCst);
        // QEMU virtio_split_should_notify: do not interrupt while the driver
        // has VRING_AVAIL_F_NO_INTERRUPT set (it will poll used.idx).
        if !avail_wants_interrupt(&self.mem, avail_ring) {
            return;
        }
        if self.irq_asserted {
            self.irq_pending_after_ack = true;
            NET_STATS.irq_defer.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.raise_irq();
    }

    fn irq_needs_raise(&self) -> bool {
        !self.irq_asserted && ((self.interrupt_status & 1) != 0 || self.irq_pending_after_ack)
    }

    fn raise_irq(&mut self) {
        if self.irq_asserted {
            return;
        }
        self.irq_asserted = true;
        self.irq_pending_after_ack = false;
        self.interrupt_status |= 1;
        NET_STATS.irq_raise.fetch_add(1, Ordering::Relaxed);
        unsafe {
            crate::hypervisor::hv_gic_set_spi(self.irq_num, true);
        }
        crate::vcpu::kick_vcpu();
    }

    fn try_assert_irq(&mut self) {
        if !self.irq_needs_raise() {
            return;
        }
        self.raise_irq();
    }

    fn handle_interrupt_ack(&mut self, data: u32) {
        NET_STATS.irq_ack.fetch_add(1, Ordering::Relaxed);
        self.interrupt_status &= !data;
        self.last_ack = std::time::Instant::now();
        if self.irq_asserted {
            self.irq_asserted = false;
            unsafe {
                crate::hypervisor::hv_gic_set_spi(self.irq_num, false);
            }
        }
        if self.irq_pending_after_ack {
            self.interrupt_status |= 1;
        }
        // QEMU virtio_mmio ACK only clears ISR then update_irq. Re-raise
        // from the RX worker if used buffers were added during the ISR.
    }

    /// Raise a deferred used-buffer IRQ once the ACK holdoff has expired.
    pub fn flush_deferred_interrupt(&mut self) {
        self.try_assert_irq();
    }

    /// Receives an Ethernet frame from the network backend and delivers it into the guest RX queue.
    pub fn receive_packet(&mut self, packet: &[u8]) {
        self.drain_rx_backlog();

        if !self.rx_queue.ready || self.rx_queue.avail_ring == 0 || self.rx_queue.desc_table == 0 {
            if self.rx_backlog.len() < 64 {
                self.rx_backlog.push_back(packet.to_vec());
            } else {
                NET_STATS.rx_drop.fetch_add(1, Ordering::Relaxed);
            }
            return;
        }

        if !self.rx_backlog.is_empty() || !self.deliver_to_rx_queue(packet) {
            // Buffer in backlog if queue was full or prior backlog exists (preserve in-order delivery)
            if self.rx_backlog.len() < 64 {
                self.rx_backlog.push_back(packet.to_vec());
            } else {
                NET_STATS.rx_drop.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn deliver_to_rx_queue(&mut self, packet: &[u8]) -> bool {
        let q_size = self.rx_queue.size;
        if q_size == 0 {
            return false;
        }

        let avail_idx = match self.mem.read_u16(self.rx_queue.avail_ring + 2) {
            Ok(idx) => idx,
            Err(_) => return false,
        };
        std::sync::atomic::fence(Ordering::SeqCst);

        if self.rx_queue.last_avail_idx == avail_idx {
            NET_STATS.rx_nomem.fetch_add(1, Ordering::Relaxed);
            return false; // No empty buffers available
        }

        let ring_elem_offset =
            self.rx_queue.avail_ring + 4 + 2 * (self.rx_queue.last_avail_idx as u64 % q_size as u64);
        let head_desc = match self.mem.read_u16(ring_elem_offset) {
            Ok(h) => h,
            Err(_) => return false,
        };

        // Walk writable descriptors in chain
        let mut desc_idx = head_desc;
        let mut steps = 0;
        let mut buffers = Vec::new();

        while steps < q_size {
            let desc_addr = self.rx_queue.desc_table + (desc_idx as u64 * 16);
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

            buffers.push((addr, len));

            if (flags & VRING_DESC_F_NEXT) != 0 {
                desc_idx = next;
                steps += 1;
            } else {
                break;
            }
        }

        if buffers.is_empty() {
            return false;
        }

        // Construct VirtIO Modern Net Header (12 bytes, all zeros for standard packet)
        let hdr = [0u8; VIRTIO_NET_HDR_SIZE];
        let mut total_written = 0usize;
        let total_packet_bytes = hdr.len() + packet.len();

        let mut data_to_write = Vec::with_capacity(total_packet_bytes);
        data_to_write.extend_from_slice(&hdr);
        data_to_write.extend_from_slice(packet);

        let mut src_offset = 0;
        for (buf_addr, buf_len) in buffers {
            if src_offset >= data_to_write.len() {
                break;
            }
            let to_write = (buf_len as usize).min(data_to_write.len() - src_offset);
            let _ = self.mem.write(buf_addr, &data_to_write[src_offset..src_offset + to_write]);
            src_offset += to_write;
            total_written += to_write;
        }

        publish_used(
            &self.mem,
            &mut self.rx_queue,
            head_desc,
            total_written as u32,
        );
        self.rx_queue.last_avail_idx = self.rx_queue.last_avail_idx.wrapping_add(1);

        NET_STATS.rx_pkts.fetch_add(1, Ordering::Relaxed);
        NET_STATS.rx_bytes.fetch_add(packet.len() as u64, Ordering::Relaxed);
        self.notify_used(self.rx_queue.avail_ring);
        true
    }

    fn drain_rx_backlog(&mut self) {
        if !self.rx_queue.ready || self.rx_queue.avail_ring == 0 || self.rx_queue.desc_table == 0 {
            return;
        }
        while let Some(pkt) = self.rx_backlog.pop_front() {
            if !self.deliver_to_rx_queue(&pkt) {
                self.rx_backlog.push_front(pkt);
                break;
            }
        }
    }

    fn process_tx(&mut self) -> Vec<Vec<u8>> {
        let mut to_send = Vec::new();
        if !self.tx_queue.ready || self.tx_queue.avail_ring == 0 || self.tx_queue.desc_table == 0 {
            return to_send;
        }

        let q_size = self.tx_queue.size;
        if q_size == 0 {
            return to_send;
        }

        let avail_idx = match self.mem.read_u16(self.tx_queue.avail_ring + 2) {
            Ok(idx) => idx,
            Err(_) => return to_send,
        };

        let mut curr_idx = self.tx_queue.last_avail_idx;
        let mut processed_any = false;

        while curr_idx != avail_idx {
            let ring_elem_offset =
                self.tx_queue.avail_ring + 4 + 2 * (curr_idx as u64 % q_size as u64);
            let head_desc = match self.mem.read_u16(ring_elem_offset) {
                Ok(h) => h,
                Err(_) => break,
            };

            // Gather all bytes across the descriptor chain
            let mut desc_idx = head_desc;
            let mut steps = 0;
            let mut full_data = Vec::new();

            while steps < q_size {
                let desc_addr = self.tx_queue.desc_table + (desc_idx as u64 * 16);
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

                let mut chunk = vec![0u8; len as usize];
                if self.mem.read(addr, &mut chunk).is_ok() {
                    full_data.extend_from_slice(&chunk);
                }

                if (flags & VRING_DESC_F_NEXT) != 0 {
                    desc_idx = next;
                    steps += 1;
                } else {
                    break;
                }
            }

            // Strip 12-byte VirtIO Net header and transmit payload after dropping
            // the device lock, so RX/ACK MMIO is not blocked on vmnet_write.
            if full_data.len() > VIRTIO_NET_HDR_SIZE {
                let payload = &full_data[VIRTIO_NET_HDR_SIZE..];
                NET_STATS.tx_pkts.fetch_add(1, Ordering::Relaxed);
                NET_STATS.tx_bytes.fetch_add(payload.len() as u64, Ordering::Relaxed);
                to_send.push(payload.to_vec());
            }

            publish_used(&self.mem, &mut self.tx_queue, head_desc, 0);

            curr_idx = curr_idx.wrapping_add(1);
            processed_any = true;
        }

        self.tx_queue.last_avail_idx = curr_idx;

        if processed_any {
            self.notify_used(self.tx_queue.avail_ring);
        }
        to_send
    }
}

pub struct VirtioNet {
    inner: Arc<Mutex<VirtioNetInner>>,
    running: Arc<AtomicBool>,
}

impl VirtioNet {
    pub fn new(
        mac: [u8; 6],
        irq_num: u32,
        mem: Arc<GuestMemory>,
        backend_choice: &str,
    ) -> Self {
        let (backend, vmnet_iface) = match backend_choice {
            "vmnet" => match VmnetBackend::new() {
                Ok(b) => {
                    let iface = b.interface;
                    (Box::new(b) as Box<dyn NetBackend>, Some(SendPtr(iface)))
                }
                Err(e) => {
                    eprintln!("\x1b[1;33m[VirtIO-Net Warning] {}\x1b[0m", e);
                    eprintln!("[VirtIO-Net] Continuing with user-mode mock backend (no external routing).");
                    (Box::new(UserMockBackend::new()) as Box<dyn NetBackend>, None)
                }
            },
            s if s.starts_with("tap:") => {
                let path = &s[4..];
                match TapBackend::open(path) {
                    Ok(b) => (Box::new(b) as Box<dyn NetBackend>, None),
                    Err(e) => {
                        eprintln!("\x1b[1;31m[VirtIO-Net Error] {}\x1b[0m", e);
                        (Box::new(UserMockBackend::new()) as Box<dyn NetBackend>, None)
                    }
                }
            }
            _ => (Box::new(UserMockBackend::new()) as Box<dyn NetBackend>, None),
        };

        println!(
            "[VirtIO-Net] Registered network device: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} on IRQ {}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5], irq_num
        );

        let inner = Arc::new(Mutex::new(VirtioNetInner::new(mac, irq_num, mem, backend)));
        let running = Arc::new(AtomicBool::new(true));

        // If vmnet is active, spawn background packet receiver loop
        if let Some(iface_ptr) = vmnet_iface {
            let inner_clone = Arc::clone(&inner);
            let running_clone = Arc::clone(&running);
            thread::Builder::new()
                .name("vmm-vmnet-rx".into())
                .spawn(move || {
                    let mut buf = [0u8; 2048];
                    let mut last_stats = Instant::now();
                    let _ = File::create("/tmp/vmm-net.log");
                    net_log("[net] rx thread started");
                    while running_clone.load(Ordering::Relaxed) {
                        // Flush deferred IRQs before potentially blocking in vmnet_read.
                        // Otherwise a burst can sit in the used ring with SPI low while
                        // we wait for the next packet, and TCP stalls at 0 Bps.
                        let defer_read = {
                            let mut inner = inner_clone.lock().unwrap();
                            if !inner.rx_backlog.is_empty() {
                                inner.drain_rx_backlog();
                            }
                            inner.flush_deferred_interrupt();
                            inner.irq_needs_raise()
                        };
                        if last_stats.elapsed() >= Duration::from_secs(1) {
                            last_stats = Instant::now();
                            let inner = inner_clone.lock().unwrap();
                            let rx_avail = inner
                                .mem
                                .read_u16(inner.rx_queue.avail_ring + 2)
                                .unwrap_or(0);
                            let rx_used = inner
                                .mem
                                .read_u16(inner.rx_queue.used_ring + 2)
                                .unwrap_or(0);
                            let tx_avail = inner
                                .mem
                                .read_u16(inner.tx_queue.avail_ring + 2)
                                .unwrap_or(0);
                            let tx_used = inner
                                .mem
                                .read_u16(inner.tx_queue.used_ring + 2)
                                .unwrap_or(0);
                            net_log(&format!(
                                "tx_pkts={} tx_bytes={} rx_pkts={} rx_bytes={} rx_drop={} rx_nomem={} irq_raise={} irq_ack={} irq_defer={} tx_notify={} vmnet_err={} backlog={} irq_asserted={} isr={:#x} pending_ack={} rx_last={} rx_avail={} rx_used={} tx_last={} tx_avail={} tx_used={} rx_ready={} tx_ready={}",
                                NET_STATS.tx_pkts.load(Ordering::Relaxed),
                                NET_STATS.tx_bytes.load(Ordering::Relaxed),
                                NET_STATS.rx_pkts.load(Ordering::Relaxed),
                                NET_STATS.rx_bytes.load(Ordering::Relaxed),
                                NET_STATS.rx_drop.load(Ordering::Relaxed),
                                NET_STATS.rx_nomem.load(Ordering::Relaxed),
                                NET_STATS.irq_raise.load(Ordering::Relaxed),
                                NET_STATS.irq_ack.load(Ordering::Relaxed),
                                NET_STATS.irq_defer.load(Ordering::Relaxed),
                                NET_STATS.tx_notify.load(Ordering::Relaxed),
                                NET_STATS.vmnet_err.load(Ordering::Relaxed),
                                inner.rx_backlog.len(),
                                inner.irq_asserted,
                                inner.interrupt_status,
                                inner.irq_pending_after_ack,
                                inner.rx_queue.last_avail_idx,
                                rx_avail,
                                rx_used,
                                inner.tx_queue.last_avail_idx,
                                tx_avail,
                                tx_used,
                                inner.rx_queue.ready,
                                inner.tx_queue.ready,
                            ));
                        }

                        if defer_read {
                            thread::sleep(Duration::from_micros(50));
                            continue;
                        }

                        let mut iov = Iovec {
                            iov_base: buf.as_mut_ptr() as *mut c_void,
                            iov_len: buf.len(),
                        };
                        let mut desc = VmPktDesc {
                            vm_pkt_size: buf.len() as u64,
                            vm_pkt_iov: &mut iov,
                            vm_pkt_iovcnt: 1,
                            vm_flags: 0,
                        };
                        let mut pktcnt: i32 = 1;

                        let ret = unsafe { vmnet_read(iface_ptr.get(), &mut desc, &mut pktcnt) };
                        if ret == VMNET_SUCCESS && pktcnt > 0 && desc.vm_pkt_size > 0 {
                            let pkt_len = desc.vm_pkt_size as usize;
                            let packet = &buf[..pkt_len];
                            let mut inner = inner_clone.lock().unwrap();
                            inner.receive_packet(packet);
                            inner.flush_deferred_interrupt();
                        } else {
                            thread::sleep(Duration::from_micros(100));
                        }
                    }
                })
                .expect("Failed to spawn vmnet RX thread");
        }

        Self { inner, running }
    }
}

impl Drop for VirtioNet {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}

impl MmioDevice for VirtioNet {
    fn read(&mut self, offset: u64, _size: usize) -> u64 {
        let inner = self.inner.lock().unwrap();
        match offset {
            VIRTIO_MMIO_MAGIC_VALUE => VIRTIO_MMIO_MAGIC as u64,
            VIRTIO_MMIO_VERSION => VIRTIO_MMIO_VERSION_2 as u64,
            VIRTIO_MMIO_DEVICE_ID => VIRTIO_DEV_NET as u64,
            VIRTIO_MMIO_VENDOR_ID => VIRTIO_MMIO_VENDOR as u64,
            VIRTIO_MMIO_DEVICE_FEATURES => {
                if inner.device_features_sel == 0 {
                    (VIRTIO_NET_F_MAC_BIT | VIRTIO_NET_F_STATUS_BIT) as u64
                } else if inner.device_features_sel == 1 {
                    (VIRTIO_F_VERSION_1_BIT >> 32) as u64
                } else {
                    0
                }
            }
            VIRTIO_MMIO_QUEUE_NUM_MAX => {
                if inner.queue_sel < 2 {
                    1024
                } else {
                    0
                }
            }
            VIRTIO_MMIO_QUEUE_READY => {
                let q = if inner.queue_sel == 0 {
                    &inner.rx_queue
                } else if inner.queue_sel == 1 {
                    &inner.tx_queue
                } else {
                    return 0;
                };
                if q.ready { 1 } else { 0 }
            }
            VIRTIO_MMIO_INTERRUPT_STATUS => inner.interrupt_status as u64,
            VIRTIO_MMIO_STATUS => inner.status as u64,

            // Config space: MAC address (0..6) + Link Status (6..8)
            0x100..=0x105 => {
                let idx = (offset - 0x100) as usize;
                inner.mac[idx] as u64
            }
            0x106 => (VIRTIO_NET_S_LINK_UP_VAL & 0xff) as u64,
            0x107 => ((VIRTIO_NET_S_LINK_UP_VAL >> 8) & 0xff) as u64,

            _ => 0,
        }
    }

    fn write(&mut self, offset: u64, data: u64, _size: usize) {
        let mut inner = self.inner.lock().unwrap();
        let mut tx_packets: Option<Vec<Vec<u8>>> = None;
        match offset {
            VIRTIO_MMIO_DEVICE_FEATURES_SEL => {
                inner.device_features_sel = data as u32;
            }
            VIRTIO_MMIO_DRIVER_FEATURES_SEL => {
                inner.driver_features_sel = data as u32;
            }
            VIRTIO_MMIO_DRIVER_FEATURES => {
                if inner.driver_features_sel == 0 {
                    inner.driver_features = (inner.driver_features & !0xffff_ffff) | (data & 0xffff_ffff);
                } else if inner.driver_features_sel == 1 {
                    inner.driver_features = (inner.driver_features & 0xffff_ffff) | ((data & 0xffff_ffff) << 32);
                }
            }
            VIRTIO_MMIO_QUEUE_SEL => {
                inner.queue_sel = data as u32;
            }
            VIRTIO_MMIO_QUEUE_NUM => {
                let q = if inner.queue_sel == 0 {
                    &mut inner.rx_queue
                } else if inner.queue_sel == 1 {
                    &mut inner.tx_queue
                } else {
                    return;
                };
                q.size = data as u32;
            }
            VIRTIO_MMIO_QUEUE_READY => {
                let ready = data != 0;
                let q = if inner.queue_sel == 0 {
                    &mut inner.rx_queue
                } else if inner.queue_sel == 1 {
                    &mut inner.tx_queue
                } else {
                    return;
                };
                q.ready = ready;
                if inner.queue_sel == 0 && ready {
                    inner.drain_rx_backlog();
                }
            }
            VIRTIO_MMIO_QUEUE_NOTIFY => {
                let q_idx = data as u32;
                if q_idx == 0 {
                    // Guest added RX buffers
                    inner.drain_rx_backlog();
                    inner.flush_deferred_interrupt();
                } else if q_idx == 1 {
                    // Guest queued packets to transmit
                    NET_STATS.tx_notify.fetch_add(1, Ordering::Relaxed);
                    tx_packets = Some(inner.process_tx());
                }
            }
            VIRTIO_MMIO_INTERRUPT_ACK => {
                inner.handle_interrupt_ack(data as u32);
            }
            VIRTIO_MMIO_STATUS => {
                inner.status = data as u32;
                if (inner.status & VIRTIO_STATUS_FAILED) != 0 {
                    eprintln!("[VirtIO-Net] Guest marked device as FAILED");
                }
            }
            VIRTIO_MMIO_QUEUE_DESC_LOW => {
                let q = if inner.queue_sel == 0 { &mut inner.rx_queue } else if inner.queue_sel == 1 { &mut inner.tx_queue } else { return; };
                q.desc_table = (q.desc_table & !0xffff_ffff) | (data & 0xffff_ffff);
            }
            VIRTIO_MMIO_QUEUE_DESC_HIGH => {
                let q = if inner.queue_sel == 0 { &mut inner.rx_queue } else if inner.queue_sel == 1 { &mut inner.tx_queue } else { return; };
                q.desc_table = (q.desc_table & 0xffff_ffff) | ((data & 0xffff_ffff) << 32);
            }
            VIRTIO_MMIO_QUEUE_DRIVER_LOW => {
                let q = if inner.queue_sel == 0 { &mut inner.rx_queue } else if inner.queue_sel == 1 { &mut inner.tx_queue } else { return; };
                q.avail_ring = (q.avail_ring & !0xffff_ffff) | (data & 0xffff_ffff);
            }
            VIRTIO_MMIO_QUEUE_DRIVER_HIGH => {
                let q = if inner.queue_sel == 0 { &mut inner.rx_queue } else if inner.queue_sel == 1 { &mut inner.tx_queue } else { return; };
                q.avail_ring = (q.avail_ring & 0xffff_ffff) | ((data & 0xffff_ffff) << 32);
            }
            VIRTIO_MMIO_QUEUE_DEVICE_LOW => {
                let q = if inner.queue_sel == 0 { &mut inner.rx_queue } else if inner.queue_sel == 1 { &mut inner.tx_queue } else { return; };
                q.used_ring = (q.used_ring & !0xffff_ffff) | (data & 0xffff_ffff);
            }
            VIRTIO_MMIO_QUEUE_DEVICE_HIGH => {
                let q = if inner.queue_sel == 0 { &mut inner.rx_queue } else if inner.queue_sel == 1 { &mut inner.tx_queue } else { return; };
                q.used_ring = (q.used_ring & 0xffff_ffff) | ((data & 0xffff_ffff) << 32);
            }
            _ => {}
        }

        let backend = tx_packets.as_ref().map(|_| Arc::clone(&inner.backend));
        drop(inner);
        if let (Some(pkts), Some(backend)) = (tx_packets, backend) {
            let mut b = backend.lock().unwrap();
            for p in pkts {
                let _ = b.send_packet(&p);
            }
        }
    }
}
