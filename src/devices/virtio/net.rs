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

    fn vmnet_read(interface: *mut c_void, packets: *mut VmPktDesc, pktcnt: *mut i32) -> u32;

    fn vmnet_write(interface: *mut c_void, packets: *mut VmPktDesc, pktcnt: *mut i32) -> u32;

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

    fn xpc_dictionary_set_uint64(dictionary: *mut c_void, key: *const i8, value: u64);

    fn xpc_dictionary_set_string(dictionary: *mut c_void, key: *const i8, value: *const i8);

    fn xpc_release(object: *mut c_void);

    fn dispatch_queue_create(label: *const i8, attr: *mut c_void) -> *mut c_void;

    fn dispatch_release(object: *mut c_void);
}

// From <vmnet/vmnet.h> operating_modes_t / vmnet_return_t
const VMNET_HOST_MODE: u64 = 1000;
const VMNET_SHARED_MODE: u64 = 1001;
const VMNET_BRIDGED_MODE: u64 = 1002;
const VMNET_SUCCESS: u32 = 1000;
const VMNET_FAILURE: u32 = 1001;
const VMNET_INTERFACE_PACKETS_AVAILABLE: u32 = 1;

#[allow(dead_code)]
const _: () = assert!(VMNET_HOST_MODE == 1000);

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
    /// Shared/NAT mode (guest on 192.168.x via vmnet + optional host PF NAT).
    pub fn new() -> Result<Self, String> {
        Self::start(VMNET_SHARED_MODE, None)
    }

    /// Bridged onto a host NIC (e.g. "en0"). Guest uses LAN DHCP — no PF NAT needed.
    pub fn new_bridged(host_if: &str) -> Result<Self, String> {
        if host_if.is_empty() {
            return Err("bridge mode requires a host interface name (e.g. bridge:en0)".into());
        }
        Self::start(VMNET_BRIDGED_MODE, Some(host_if))
    }

    fn start(mode: u64, bridge_if: Option<&str>) -> Result<Self, String> {
        let desc = unsafe { xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0) };
        if desc.is_null() {
            return Err("Failed to create XPC dictionary for vmnet".into());
        }

        unsafe {
            xpc_dictionary_set_uint64(desc, b"vmnet_operation_mode\0".as_ptr() as *const i8, mode);
            if let Some(iface) = bridge_if {
                // vmnet_shared_interface_name_key — physical iface to bridge onto
                let c_iface = std::ffi::CString::new(iface)
                    .map_err(|_| "bridge interface name contains interior NUL".to_string())?;
                xpc_dictionary_set_string(
                    desc,
                    b"shared_interface_name\0".as_ptr() as *const i8,
                    c_iface.as_ptr(),
                );
            }
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
                match mode {
                    VMNET_BRIDGED_MODE => {
                        let iface = bridge_if.unwrap_or("?");
                        println!(
                            "[VirtIO-Net] macOS vmnet BRIDGED on {iface} — guest uses LAN DHCP (no host PF NAT)."
                        );
                    }
                    VMNET_SHARED_MODE => {
                        println!("[VirtIO-Net] macOS vmnet SHARED/NAT interface started.");
                        Self::setup_host_nat();
                    }
                    _ => {
                        println!("[VirtIO-Net] macOS vmnet interface started (mode={mode}).");
                    }
                }
                Ok(Self {
                    interface,
                    dispatch_queue: queue,
                })
            }
            Some(VMNET_FAILURE) => {
                unsafe { dispatch_release(queue) };
                Err(format!(
                    "vmnet_start_interface failed with status {} (Permission Denied). On macOS, vmnet.framework requires root (sudo) or the 'com.apple.vm.networking' entitlement.",
                    VMNET_FAILURE
                ))
            }
            Some(s) => {
                unsafe { dispatch_release(queue) };
                Err(format!("vmnet_start_interface failed with status {s}"))
            }
            None => {
                unsafe { dispatch_release(queue) };
                Err("Timed out waiting for vmnet_start_interface completion handler".into())
            }
        }
    }

    /// Automatically configure macOS PF NAT packet forwarding for guest internet access.
    fn setup_host_nat() {
        // Enable host IPv4 packet forwarding
        let _ = std::process::Command::new("sysctl")
            .args(["-w", "net.inet.ip.forwarding=1"])
            .output();

        // Detect default outbound host network interface (e.g. en0)
        let iface = std::process::Command::new("netstat")
            .args(["-rn", "-f", "inet"])
            .output()
            .ok()
            .and_then(|o| {
                let stdout = String::from_utf8_lossy(&o.stdout);
                for line in stdout.lines() {
                    if line.starts_with("default") {
                        let parts: Vec<&str> = line.split_whitespace().collect();
                        if parts.len() >= 4 {
                            return Some(parts[parts.len() - 1].to_string());
                        }
                    }
                }
                None
            })
            .unwrap_or_else(|| "en0".to_string());

        // Configure macOS Packet Filter NAT anchor com.apple/vmm
        let rules = format!(
            "nat on {} from 192.168.18.0/24 to any -> ({})\npass in quick on bridge100 all\npass out quick on {} all\n",
            iface, iface, iface
        );

        if let Ok(mut child) = std::process::Command::new("pfctl")
            .args(["-a", "com.apple/vmm", "-f", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                use std::io::Write;
                let _ = stdin.write_all(rules.as_bytes());
            }
            let _ = child.wait();
            println!(
                "[VirtIO-Net] Configured macOS PF NAT packet forwarding on {}.",
                iface
            );
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
                let _ = vmnet_stop_interface(self.interface, self.dispatch_queue, std::ptr::null());
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

    fn reset(&mut self) {
        self.status = 0;
        self.device_features_sel = 0;
        self.driver_features_sel = 0;
        self.driver_features = 0;
        self.queue_sel = 0;
        for queue in [&mut self.rx_queue, &mut self.tx_queue] {
            *queue = VirtQueue {
                max_size: 1024,
                size: 1024,
                ..Default::default()
            };
        }
        self.rx_backlog.clear();
        self.interrupt_status = 0;
        self.irq_asserted = false;
        self.irq_pending_after_ack = false;
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

        match self.rx_queue.pending_count(&self.mem) {
            Ok(0) => {
                NET_STATS.rx_nomem.fetch_add(1, Ordering::Relaxed);
                return false;
            }
            Ok(_) => {}
            Err(_) => {
                self.status |= VIRTIO_STATUS_NEEDS_RESET;
                self.rx_queue.ready = false;
                return false;
            }
        }
        let ring_elem_offset = self.rx_queue.avail_ring
            + 4
            + 2 * (self.rx_queue.last_avail_idx as u64 % q_size as u64);
        let head_desc = self.mem.read_u16(ring_elem_offset).unwrap();
        let buffers = match self
            .rx_queue
            .descriptors(&self.mem, head_desc, false, 65536)
        {
            Ok(d)
                if d.iter().all(|d| d.flags & VRING_DESC_F_WRITE != 0)
                    && d.iter().map(|d| d.len as usize).sum::<usize>()
                        >= VIRTIO_NET_HDR_SIZE + packet.len() =>
            {
                d
            }
            _ => {
                self.status |= VIRTIO_STATUS_NEEDS_RESET;
                self.rx_queue.ready = false;
                return false;
            }
        };

        // Construct VirtIO Modern Net Header (12 bytes, all zeros for standard packet)
        let hdr = [0u8; VIRTIO_NET_HDR_SIZE];
        let mut total_written = 0usize;
        let total_packet_bytes = hdr.len() + packet.len();

        let mut data_to_write = Vec::with_capacity(total_packet_bytes);
        data_to_write.extend_from_slice(&hdr);
        data_to_write.extend_from_slice(packet);

        let mut src_offset = 0;
        for desc in buffers {
            let (buf_addr, buf_len) = (desc.addr, desc.len);
            if src_offset >= data_to_write.len() {
                break;
            }
            let to_write = (buf_len as usize).min(data_to_write.len() - src_offset);
            if self
                .mem
                .write(buf_addr, &data_to_write[src_offset..src_offset + to_write])
                .is_err()
            {
                self.status |= VIRTIO_STATUS_NEEDS_RESET;
                return false;
            }
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
        NET_STATS
            .rx_bytes
            .fetch_add(packet.len() as u64, Ordering::Relaxed);
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

        let count = match self.tx_queue.pending_count(&self.mem) {
            Ok(n) => n,
            Err(_) => {
                self.status |= VIRTIO_STATUS_NEEDS_RESET;
                return to_send;
            }
        };
        let mut curr_idx = self.tx_queue.last_avail_idx;
        let mut processed_any = false;

        for _ in 0..count {
            let ring_elem_offset =
                self.tx_queue.avail_ring + 4 + 2 * (curr_idx as u64 % q_size as u64);
            let head_desc = match self.mem.read_u16(ring_elem_offset) {
                Ok(h) => h,
                Err(_) => break,
            };

            let descriptors = match self
                .tx_queue
                .descriptors(&self.mem, head_desc, false, 65536)
            {
                Ok(d) if d.iter().all(|d| d.flags & VRING_DESC_F_WRITE == 0) => d,
                _ => {
                    self.status |= VIRTIO_STATUS_NEEDS_RESET;
                    break;
                }
            };
            let mut full_data = vec![0; descriptors.iter().map(|d| d.len as usize).sum()];
            let mut offset = 0;
            for d in descriptors {
                let end = offset + d.len as usize;
                if self.mem.read(d.addr, &mut full_data[offset..end]).is_err() {
                    self.status |= VIRTIO_STATUS_NEEDS_RESET;
                    return to_send;
                }
                offset = end;
            }

            // Strip 12-byte VirtIO Net header and transmit payload after dropping
            // the device lock, so RX/ACK MMIO is not blocked on vmnet_write.
            if full_data.len() > VIRTIO_NET_HDR_SIZE {
                let payload = &full_data[VIRTIO_NET_HDR_SIZE..];
                NET_STATS.tx_pkts.fetch_add(1, Ordering::Relaxed);
                NET_STATS
                    .tx_bytes
                    .fetch_add(payload.len() as u64, Ordering::Relaxed);
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

fn default_host_iface() -> String {
    std::process::Command::new("route")
        .args(["get", "default"])
        .output()
        .ok()
        .and_then(|o| {
            let stdout = String::from_utf8_lossy(&o.stdout);
            for line in stdout.lines() {
                let line = line.trim();
                if let Some(rest) = line.strip_prefix("interface:") {
                    let name = rest.trim();
                    if !name.is_empty() {
                        return Some(name.to_string());
                    }
                }
            }
            None
        })
        .unwrap_or_else(|| "en0".to_string())
}

pub struct VirtioNet {
    guest_address: crate::forward::GuestAddress,
    inner: Arc<Mutex<VirtioNetInner>>,
    running: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl VirtioNet {
    pub fn guest_address(&self) -> crate::forward::GuestAddress {
        self.guest_address.clone()
    }

    pub fn new(
        mac: [u8; 6],
        irq_num: u32,
        mem: Arc<GuestMemory>,
        backend_choice: &str,
    ) -> Result<Self, String> {
        let (backend, vmnet_iface) = match backend_choice {
            s if s == "bridge"
                || s.starts_with("bridge:")
                || s == "bridged"
                || s.starts_with("bridged:") =>
            {
                let host_if = if let Some(rest) = s
                    .strip_prefix("bridge:")
                    .or_else(|| s.strip_prefix("bridged:"))
                {
                    if rest.is_empty() {
                        default_host_iface()
                    } else {
                        rest.to_string()
                    }
                } else {
                    default_host_iface()
                };
                match VmnetBackend::new_bridged(&host_if) {
                    Ok(b) => {
                        let iface = b.interface;
                        (Box::new(b) as Box<dyn NetBackend>, Some(SendPtr(iface)))
                    }
                    Err(e) => {
                        return Err(format!(
                            "Cannot start bridge:{host_if}: {e}. Check the interface and run with sudo or an authorized networking signature; use --net mock or --net none for offline operation."
                        ));
                    }
                }
            }
            "vmnet" => match VmnetBackend::new() {
                Ok(b) => {
                    let iface = b.interface;
                    (Box::new(b) as Box<dyn NetBackend>, Some(SendPtr(iface)))
                }
                Err(e) => {
                    return Err(format!(
                        "Cannot start vmnet: {e}. Run with sudo or an authorized networking signature (docs/entitlements.md); use --net mock or --net none for offline operation."
                    ));
                }
            },
            s if s.starts_with("tap:") => {
                let path = &s[4..];
                match TapBackend::open(path) {
                    Ok(b) => (Box::new(b) as Box<dyn NetBackend>, None),
                    Err(e) => {
                        return Err(format!("Cannot start tap backend '{path}': {e}"));
                    }
                }
            }
            "mock" => (
                Box::new(UserMockBackend::new()) as Box<dyn NetBackend>,
                None,
            ),
            _ => {
                return Err(format!(
                    "Unknown network mode '{backend_choice}'; expected vmnet, bridge[:interface], tap:<path>, mock, or none"
                ));
            }
        };

        println!(
            "[VirtIO-Net] Registered network device: MAC {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} on IRQ {}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5], irq_num
        );

        let inner = Arc::new(Mutex::new(VirtioNetInner::new(mac, irq_num, mem, backend)));
        let running = Arc::new(AtomicBool::new(true));
        let guest_address = Arc::new(Mutex::new(None));
        let mut worker = None;

        // If vmnet is active, spawn background packet receiver loop
        if let Some(iface_ptr) = vmnet_iface {
            let discovered_address = guest_address.clone();
            let inner_clone = Arc::clone(&inner);
            let running_clone = Arc::clone(&running);
            worker = Some(thread::Builder::new()
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
                                .read_u16(inner.rx_queue.avail_ring.saturating_add(2))
                                .unwrap_or(0);
                            let rx_used = inner
                                .mem
                                .read_u16(inner.rx_queue.used_ring.saturating_add(2))
                                .unwrap_or(0);
                            let tx_avail = inner
                                .mem
                                .read_u16(inner.tx_queue.avail_ring.saturating_add(2))
                                .unwrap_or(0);
                            let tx_used = inner
                                .mem
                                .read_u16(inner.tx_queue.used_ring.saturating_add(2))
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
                            let pkt_len = (desc.vm_pkt_size as usize).min(buf.len());
                            let packet = &mut buf[..pkt_len];
                            if let Some(ip) = dhcp_guest_address(packet, mac) {
                                let mut address = discovered_address.lock().unwrap();
                                if *address != Some(ip) {
                                    *address = Some(ip);
                                    crate::host_log!("[VirtIO-Net] Guest DHCP address: {ip} (--ssh-port destination)");
                                }
                            }
                            patch_dhcp_response(packet);
                            let mut inner = inner_clone.lock().unwrap();
                            inner.receive_packet(packet);
                            inner.flush_deferred_interrupt();
                        } else {
                            thread::sleep(Duration::from_micros(100));
                        }
                    }
                })
                .expect("Failed to spawn vmnet RX thread"));
        }

        Ok(Self {
            guest_address,
            inner,
            running,
            worker,
        })
    }
}

// Only accept a complete DHCP ACK addressed to this NIC, never another VM's
// broadcast offer or an old host lease-file entry.
fn dhcp_guest_address(packet: &[u8], mac: [u8; 6]) -> Option<std::net::Ipv4Addr> {
    if packet.get(12..14)? != [0x08, 0x00] {
        return None;
    }
    let ip = packet.get(14..)?;
    let header_len = usize::from(*ip.first()? & 15) * 4;
    if ip[0] >> 4 != 4 || header_len < 20 || *ip.get(9)? != 17 {
        return None;
    }
    let total_len = u16::from_be_bytes([*ip.get(2)?, *ip.get(3)?]) as usize;
    let fragments = u16::from_be_bytes([*ip.get(6)?, *ip.get(7)?]);
    if fragments & 0x3fff != 0 {
        return None;
    }
    let udp = ip.get(..total_len)?.get(header_len..)?;
    if udp.get(..4)? != [0, 67, 0, 68] {
        return None;
    }
    let udp_len = u16::from_be_bytes([*udp.get(4)?, *udp.get(5)?]) as usize;
    let bootp = udp.get(..udp_len)?.get(8..)?;
    if bootp.get(..3)? != [2, 1, 6]
        || bootp.get(28..34)? != mac
        || bootp.get(236..240)? != [99, 130, 83, 99]
    {
        return None;
    }
    let mut options = bootp.get(240..)?;
    let mut ack = false;
    while let Some((&code, rest)) = options.split_first() {
        options = rest;
        if code == 255 {
            break;
        }
        if code == 0 {
            continue;
        }
        let (&len, rest) = options.split_first()?;
        let value = rest.get(..len as usize)?;
        options = rest.get(len as usize..)?;
        if code == 53 {
            ack = value == [5];
        }
    }
    let ip = std::net::Ipv4Addr::new(bootp[16], bootp[17], bootp[18], bootp[19]);
    (ack && !ip.is_unspecified() && !ip.is_broadcast() && !ip.is_multicast()).then_some(ip)
}

#[cfg(test)]
mod backend_tests {
    use super::*;

    #[test]
    fn reset_discards_firmware_queues_before_os_reinitializes_them() {
        let memory = Arc::new(GuestMemory::new_test(0, 0x4000));
        let mut inner = VirtioNetInner::new([0; 6], 35, memory, Box::new(UserMockBackend::new()));
        inner.status = VIRTIO_STATUS_DRIVER_OK;
        inner.driver_features = VIRTIO_F_VERSION_1_BIT;
        inner.rx_queue.ready = true;
        inner.rx_queue.last_avail_idx = 4;
        inner.rx_queue.used_idx = 4;
        inner.tx_queue.ready = true;
        inner.tx_queue.last_avail_idx = 2;
        inner.tx_queue.used_idx = 2;
        inner.interrupt_status = 1;
        inner.irq_asserted = true;
        inner.irq_pending_after_ack = true;
        inner.rx_backlog.push_back(vec![1]);

        inner.reset();

        assert_eq!(inner.status, 0);
        assert_eq!(inner.driver_features, 0);
        for queue in [inner.rx_queue, inner.tx_queue] {
            assert!(!queue.ready);
            assert_eq!(queue.last_avail_idx, 0);
            assert_eq!(queue.used_idx, 0);
            assert_eq!(queue.avail_ring, 0);
            assert_eq!(queue.used_ring, 0);
        }
        assert_eq!(inner.interrupt_status, 0);
        assert!(!inner.irq_asserted);
        assert!(!inner.irq_pending_after_ack);
        assert!(inner.rx_backlog.is_empty());
    }

    #[test]
    fn discovers_only_complete_ack_for_this_guest() {
        let mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
        let mut packet = vec![0; 14 + 20 + 8 + 244];
        packet[12..14].copy_from_slice(&[8, 0]);
        packet[14] = 0x45;
        packet[16..18].copy_from_slice(&272u16.to_be_bytes());
        packet[23] = 17;
        packet[34..40].copy_from_slice(&[0, 67, 0, 68, 0, 252]);
        packet[42..45].copy_from_slice(&[2, 1, 6]);
        packet[58..62].copy_from_slice(&[192, 168, 2, 2]);
        packet[70..76].copy_from_slice(&mac);
        packet[278..286].copy_from_slice(&[99, 130, 83, 99, 53, 1, 5, 255]);
        assert_eq!(
            dhcp_guest_address(&packet, mac),
            Some(std::net::Ipv4Addr::new(192, 168, 2, 2))
        );
        assert_eq!(dhcp_guest_address(&packet, [0; 6]), None);
        for end in 0..packet.len() {
            assert_eq!(dhcp_guest_address(&packet[..end], mac), None);
        }
        packet[284] = 2; // OFFER isn't an assigned lease.
        assert_eq!(dhcp_guest_address(&packet, mac), None);
    }

    #[test]
    fn failed_or_unknown_backends_do_not_become_mock() {
        let memory = Arc::new(GuestMemory::new_test(0, 0x4000));
        let dir = crate::test_support::TestDir::new();
        let missing = format!("tap:{}", dir.path("missing-tap").display());
        for mode in [missing.as_str(), "typo"] {
            assert!(VirtioNet::new([0; 6], 35, memory.clone(), mode).is_err());
        }
        assert!(VirtioNet::new([0; 6], 35, memory, "mock").is_ok());
    }
}

/// Inspects incoming packets from vmnet. If the packet is a DHCP Offer or Ack (UDP 67 -> 68),
/// replaces macOS's non-functional internal DNS server IP (192.168.18.1 or any 192.168.x.1)
/// with Cloudflare public DNS (1.1.1.1).
fn patch_dhcp_response(buf: &mut [u8]) {
    // Minimum Ethernet (14) + IPv4 (20) + UDP (8) + DHCP fixed headers (236) + cookie (4) = 282 bytes
    if buf.len() < 282 {
        return;
    }
    // Check EtherType == IPv4 (0x0800)
    if buf[12] != 0x08 || buf[13] != 0x00 {
        return;
    }
    // Check IPv4 protocol == UDP (17)
    if buf[23] != 17 {
        return;
    }
    // Check IPv4 header length
    let ihl = (buf[14] & 0x0f) as usize * 4;
    if ihl < 20 || 14 + ihl + 8 > buf.len() {
        return;
    }
    let udp_offset = 14 + ihl;
    let src_port = u16::from_be_bytes([buf[udp_offset], buf[udp_offset + 1]]);
    let dst_port = u16::from_be_bytes([buf[udp_offset + 2], buf[udp_offset + 3]]);

    // DHCP server to client: port 67 -> port 68
    if src_port != 67 || dst_port != 68 {
        return;
    }

    let dhcp_offset = udp_offset + 8;
    if dhcp_offset + 240 > buf.len() {
        return;
    }

    // Check op == 2 (BOOTREPLY)
    if buf[dhcp_offset] != 2 {
        return;
    }

    // Check DHCP magic cookie at offset 236: 0x63, 0x82, 0x53, 0x63
    let cookie_offset = dhcp_offset + 236;
    if &buf[cookie_offset..cookie_offset + 4] != [0x63, 0x82, 0x53, 0x63] {
        return;
    }

    // Scan DHCP options starting at offset 240
    let mut opt_idx = cookie_offset + 4;
    let mut modified = false;

    while opt_idx < buf.len() {
        let opt_type = buf[opt_idx];
        if opt_type == 0 {
            // Pad
            opt_idx += 1;
            continue;
        }
        if opt_type == 255 {
            // End
            break;
        }
        if opt_idx + 1 >= buf.len() {
            break;
        }
        let opt_len = buf[opt_idx + 1] as usize;
        let opt_val = opt_idx + 2;
        if opt_val + opt_len > buf.len() {
            break;
        }

        // Option 6: Domain Name Server
        if opt_type == 6 && opt_len >= 4 {
            for chunk in (0..opt_len).step_by(4) {
                if chunk + 4 <= opt_len {
                    let dns_ip = &mut buf[opt_val + chunk..opt_val + chunk + 4];
                    // If macOS offers 192.168.18.1 (or 192.168.x.1), rewrite to 1.1.1.1
                    if dns_ip[0] == 192 && dns_ip[1] == 168 && dns_ip[3] == 1 {
                        dns_ip.copy_from_slice(&[1, 1, 1, 1]);
                        modified = true;
                    }
                }
            }
        }
        opt_idx = opt_val + opt_len;
    }

    if modified {
        // Clear UDP checksum (0x0000 = disabled/not computed in IPv4 UDP, per RFC 768)
        buf[udp_offset + 6] = 0;
        buf[udp_offset + 7] = 0;
        crate::device_trace!(
            "[VirtIO-Net] Rewrote DHCP DNS server option from macOS host IP to 1.1.1.1"
        );
    }
}

impl Drop for VirtioNet {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl MmioDevice for VirtioNet {
    fn shutdown(&mut self) -> Result<(), String> {
        self.running.store(false, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().map_err(|_| "Network worker panicked")?;
        }
        Ok(())
    }
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
                    inner.driver_features =
                        (inner.driver_features & !0xffff_ffff) | (data & 0xffff_ffff);
                } else if inner.driver_features_sel == 1 {
                    inner.driver_features =
                        (inner.driver_features & 0xffff_ffff) | ((data & 0xffff_ffff) << 32);
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
                if data == 0 {
                    // UEFI may have used this NIC before handing it to the OS.
                    // FreeBSD starts fresh rings at index zero after reset.
                    inner.reset();
                    *self.guest_address.lock().unwrap() = None;
                    unsafe {
                        crate::hypervisor::hv_gic_set_spi(inner.irq_num, false);
                    }
                } else if (inner.status & VIRTIO_STATUS_FAILED) != 0 {
                    eprintln!("[VirtIO-Net] Guest marked device as FAILED");
                }
            }
            VIRTIO_MMIO_QUEUE_DESC_LOW => {
                let q = if inner.queue_sel == 0 {
                    &mut inner.rx_queue
                } else if inner.queue_sel == 1 {
                    &mut inner.tx_queue
                } else {
                    return;
                };
                q.desc_table = (q.desc_table & !0xffff_ffff) | (data & 0xffff_ffff);
            }
            VIRTIO_MMIO_QUEUE_DESC_HIGH => {
                let q = if inner.queue_sel == 0 {
                    &mut inner.rx_queue
                } else if inner.queue_sel == 1 {
                    &mut inner.tx_queue
                } else {
                    return;
                };
                q.desc_table = (q.desc_table & 0xffff_ffff) | ((data & 0xffff_ffff) << 32);
            }
            VIRTIO_MMIO_QUEUE_DRIVER_LOW => {
                let q = if inner.queue_sel == 0 {
                    &mut inner.rx_queue
                } else if inner.queue_sel == 1 {
                    &mut inner.tx_queue
                } else {
                    return;
                };
                q.avail_ring = (q.avail_ring & !0xffff_ffff) | (data & 0xffff_ffff);
            }
            VIRTIO_MMIO_QUEUE_DRIVER_HIGH => {
                let q = if inner.queue_sel == 0 {
                    &mut inner.rx_queue
                } else if inner.queue_sel == 1 {
                    &mut inner.tx_queue
                } else {
                    return;
                };
                q.avail_ring = (q.avail_ring & 0xffff_ffff) | ((data & 0xffff_ffff) << 32);
            }
            VIRTIO_MMIO_QUEUE_DEVICE_LOW => {
                let q = if inner.queue_sel == 0 {
                    &mut inner.rx_queue
                } else if inner.queue_sel == 1 {
                    &mut inner.tx_queue
                } else {
                    return;
                };
                q.used_ring = (q.used_ring & !0xffff_ffff) | (data & 0xffff_ffff);
            }
            VIRTIO_MMIO_QUEUE_DEVICE_HIGH => {
                let q = if inner.queue_sel == 0 {
                    &mut inner.rx_queue
                } else if inner.queue_sel == 1 {
                    &mut inner.tx_queue
                } else {
                    return;
                };
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
