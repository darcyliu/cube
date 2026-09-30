//! vmnet.framework FFI for the long-running service.
//!
//! The in-process `--net vmnet` and bridge paths keep their own calls in
//! `devices::virtio::net`. This module is what `--vmnet-service` links against.

use std::ffi::{c_void, CStr, CString};
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Mutex;

use std::time::Duration;

pub const VMNET_SHARED_MODE: u64 = 1001;
pub const VMNET_BRIDGED_MODE: u64 = 1002;
pub const VMNET_SUCCESS: u32 = 1000;
pub const VMNET_FAILURE: u32 = 1001;
pub const VMNET_INVALID_ARGUMENT: u32 = 1003;
pub const VMNET_INVALID_ACCESS: u32 = 1005;
pub const VMNET_SHARING_SERVICE_BUSY: u32 = 1009;
pub const VMNET_PACKET_TOO_BIG: u32 = 1006;
pub const VMNET_BUFFER_EXHAUSTED: u32 = 1007;
pub const VMNET_NOT_AUTHORIZED: u32 = 1010;
pub const VMNET_INTERFACE_PACKETS_AVAILABLE: u32 = 1;

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

#[repr(C)]
struct BlockLiteral {
    isa: *const c_void,
    flags: i32,
    reserved: i32,
    invoke: unsafe extern "C" fn(*mut BlockLiteral, u32, *mut c_void),
    descriptor: *const BlockDescriptor,
}

unsafe impl Send for BlockLiteral {}
unsafe impl Sync for BlockLiteral {}

static BLOCK_DESCRIPTOR: BlockDescriptor = BlockDescriptor {
    reserved: 0,
    size: std::mem::size_of::<BlockLiteral>(),
};

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

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    static kCFRunLoopDefaultMode: *const c_void;
    fn CFRunLoopRunInMode(mode: *const c_void, seconds: f64, return_after_source_handled: u8) -> i32;
}

/// vmnet delivers the start completion through this thread's run loop.
/// Sleeping here leaves a bridged start unfinished.
pub fn pump_main_runloop(seconds: f64) {
    unsafe {
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, seconds, 0);
    }
}

pub fn start_failure(status: u32) -> String {
    let detail = match status {
        VMNET_FAILURE => "general failure",
        VMNET_INVALID_ARGUMENT => "invalid argument; vmnet cannot bridge this interface",
        VMNET_INVALID_ACCESS => {
            "permission denied; run with sudo or an authorized networking signature"
        }
        VMNET_SHARING_SERVICE_BUSY => {
            "sharing service is busy; stop the vmm vmnet service and Docker's vmnet helper"
        }
        VMNET_NOT_AUTHORIZED => {
            "not authorized; run with sudo or an authorized networking signature"
        }
        _ => "unexpected status",
    };
    format!("status {status} ({detail})")
}

unsafe extern "C" {
    fn xpc_dictionary_create(
        keys: *const *const i8,
        values: *const *mut c_void,
        count: usize,
    ) -> *mut c_void;
    fn xpc_dictionary_set_uint64(dictionary: *mut c_void, key: *const i8, value: u64);
    fn xpc_dictionary_set_string(dictionary: *mut c_void, key: *const i8, value: *const i8);
    fn xpc_dictionary_get_uint64(dictionary: *mut c_void, key: *const i8) -> u64;
    fn xpc_dictionary_get_string(dictionary: *mut c_void, key: *const i8) -> *const i8;
    fn xpc_release(object: *mut c_void);
    fn dispatch_queue_create(label: *const i8, attr: *mut c_void) -> *mut c_void;
    fn dispatch_release(object: *mut c_void);
}

struct StartInfo {
    status: u32,
    mtu: u64,
    max_packet: u64,
    mac: String,
    start_address: String,
    end_address: String,
    subnet_mask: String,
}

static START_INFO: Mutex<Option<StartInfo>> = Mutex::new(None);

unsafe extern "C" fn start_invoke(_block: *mut BlockLiteral, status: u32, param: *mut c_void) {
    let mut info = StartInfo {
        status,
        mtu: 0,
        max_packet: 0,
        mac: String::new(),
        start_address: String::new(),
        end_address: String::new(),
        subnet_mask: String::new(),
    };
    if status == VMNET_SUCCESS && !param.is_null() {
        unsafe {
            info.mtu = xpc_dictionary_get_uint64(param, b"vmnet_mtu\0".as_ptr() as *const i8);
            info.max_packet =
                xpc_dictionary_get_uint64(param, b"vmnet_max_packet_size\0".as_ptr() as *const i8);
            info.mac = xpc_string(param, b"vmnet_mac_address\0");
            info.start_address = xpc_string(param, b"vmnet_start_address\0");
            info.end_address = xpc_string(param, b"vmnet_end_address\0");
            info.subnet_mask = xpc_string(param, b"vmnet_subnet_mask\0");
        }
    }
    *START_INFO.lock().unwrap() = Some(info);
}

unsafe fn xpc_string(dict: *mut c_void, key: &[u8]) -> String {
    let ptr = unsafe { xpc_dictionary_get_string(dict, key.as_ptr() as *const i8) };
    if ptr.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
    }
}

pub fn shared_network_cidr(start: &str, mask: &str) -> Option<String> {
    let gateway: Ipv4Addr = start.parse().ok()?;
    let bits = mask.parse::<Ipv4Addr>().ok()?.to_bits();
    let prefix = bits.leading_ones();
    if prefix == 0 || prefix + bits.trailing_zeros() != 32 {
        return None;
    }
    let network = Ipv4Addr::from(gateway.to_bits() & bits);
    Some(format!("{network}/{prefix}"))
}

/// `start` is the gateway. Addresses after it through `end` are the DHCP pool.
pub fn describe_shared_network(start: &str, end: &str, mask: &str) -> Option<String> {
    let cidr = shared_network_cidr(start, mask)?;
    let gateway: Ipv4Addr = start.parse().ok()?;
    let last: Ipv4Addr = end.parse().ok()?;
    let dhcp = gateway
        .to_bits()
        .checked_add(1)
        .filter(|first| *first <= last.to_bits())
        .map(|first| format!(" dhcp {}-{last}", Ipv4Addr::from(first)))
        .unwrap_or_default();
    Some(format!("{cidr} gateway {gateway}{dhcp}"))
}

static START_BLOCK: BlockLiteral = BlockLiteral {
    isa: unsafe { _NSConcreteGlobalBlock.as_ptr() as *const c_void },
    flags: (1 << 29) | (1 << 28),
    reserved: 0,
    invoke: start_invoke,
    descriptor: &BLOCK_DESCRIPTOR,
};

static EVENT_PIPE: AtomicI32 = AtomicI32::new(-1);

unsafe extern "C" fn event_invoke(_block: *mut BlockLiteral, _event: u32, _info: *mut c_void) {
    let fd = EVENT_PIPE.load(Ordering::Relaxed);
    if fd < 0 {
        return;
    }
    let byte = [1u8];
    unsafe {
        let _ = libc::write(fd, byte.as_ptr() as *const c_void, 1);
    }
}

static EVENT_BLOCK: BlockLiteral = BlockLiteral {
    isa: unsafe { _NSConcreteGlobalBlock.as_ptr() as *const c_void },
    flags: (1 << 29) | (1 << 28),
    reserved: 0,
    invoke: event_invoke,
    descriptor: &BLOCK_DESCRIPTOR,
};

pub struct VmnetPort {
    interface: *mut c_void,
    queue: *mut c_void,
    pub read_budget: usize,
    seen_write_success: bool,
    pub vmnet_err: u64,
    /// Set when vmnet's completion dictionary included a usable IPv4 range.
    pub reported_network: bool,
    /// Gateway from `vmnet_start_address` in shared mode.
    pub shared_gateway: Option<Ipv4Addr>,
    /// Network prefix vmnet reported, such as `192.168.2.0/24`.
    pub shared_cidr: Option<String>,
    /// Host NIC named for `VMNET_BRIDGED_MODE`.
    pub host_iface: Option<String>,
}

unsafe impl Send for VmnetPort {}

impl VmnetPort {
    /// One shared-mode interface. Does not set a guest MAC.
    pub fn start_shared() -> Result<Self, String> {
        Self::start(VMNET_SHARED_MODE, None, None)
    }

    /// Shared mode with an explicit gateway, DHCP end, and subnet mask.
    /// All three strings are dotted IPv4. vmnet may ignore them and keep the
    /// system shared network; `shared_cidr` is what came back.
    pub fn start_shared_range(gateway: &str, end: &str, mask: &str) -> Result<Self, String> {
        Self::start(VMNET_SHARED_MODE, None, Some((gateway, end, mask)))
    }

    /// One bridged interface on `host_if`. Guest frames keep their own MAC.
    /// The XPC key is `vmnet_shared_interface_name`, the same key the in-process
    /// bridge path passes to vmnet.
    pub fn start_bridged(host_if: &str) -> Result<Self, String> {
        if host_if.is_empty() {
            return Err("bridge mode requires a host interface name (for example en0)".into());
        }
        Self::start(VMNET_BRIDGED_MODE, Some(host_if), None)
    }

    fn start(
        mode: u64,
        bridge_if: Option<&str>,
        nat: Option<(&str, &str, &str)>,
    ) -> Result<Self, String> {
        let desc = unsafe { xpc_dictionary_create(std::ptr::null(), std::ptr::null(), 0) };
        if desc.is_null() {
            return Err("Failed to create XPC dictionary for vmnet".into());
        }
        let iface_name = if let Some(iface) = bridge_if {
            match CString::new(iface) {
                Ok(name) => Some(name),
                Err(_) => {
                    unsafe { xpc_release(desc) };
                    return Err(format!("bridge interface {iface} contains a NUL"));
                }
            }
        } else {
            None
        };
        let nat_names = if let Some((gateway, end, mask)) = nat {
            let make = |label: &str, value: &str| {
                CString::new(value).map_err(|_| {
                    format!("shared {label} {value} contains a NUL")
                })
            };
            match (make("gateway", gateway), make("end", end), make("mask", mask)) {
                (Ok(gateway), Ok(end), Ok(mask)) => Some((gateway, end, mask)),
                _ => {
                    unsafe { xpc_release(desc) };
                    return Err("shared address range contains a NUL".into());
                }
            }
        } else {
            None
        };
        unsafe {
            xpc_dictionary_set_uint64(
                desc,
                b"vmnet_operation_mode\0".as_ptr() as *const i8,
                mode,
            );
            if let Some(name) = iface_name.as_ref() {
                xpc_dictionary_set_string(
                    desc,
                    b"vmnet_shared_interface_name\0".as_ptr() as *const i8,
                    name.as_ptr(),
                );
            }
            if let Some((gateway, end, mask)) = nat_names.as_ref() {
                xpc_dictionary_set_string(
                    desc,
                    b"vmnet_start_address\0".as_ptr() as *const i8,
                    gateway.as_ptr(),
                );
                xpc_dictionary_set_string(
                    desc,
                    b"vmnet_end_address\0".as_ptr() as *const i8,
                    end.as_ptr(),
                );
                xpc_dictionary_set_string(
                    desc,
                    b"vmnet_subnet_mask\0".as_ptr() as *const i8,
                    mask.as_ptr(),
                );
            }
        }
        let queue = unsafe {
            dispatch_queue_create(
                b"org.vmm.vmnet.service\0".as_ptr() as *const i8,
                std::ptr::null_mut(),
            )
        };
        if queue.is_null() {
            unsafe { xpc_release(desc) };
            return Err("Failed to create dispatch queue for vmnet".into());
        }
        *START_INFO.lock().unwrap() = None;
        let interface = unsafe {
            vmnet_start_interface(
                desc,
                queue,
                &START_BLOCK as *const BlockLiteral as *const c_void,
            )
        };
        unsafe { xpc_release(desc) };
        if interface.is_null() {
            unsafe { dispatch_release(queue) };
            return Err("vmnet_start_interface returned null".into());
        }
        let started = std::time::Instant::now();
        let info = loop {
            if let Some(info) = START_INFO.lock().unwrap().take() {
                break info;
            }
            if started.elapsed() > Duration::from_secs(15) {
                abandon(interface, queue);
                return Err(
                    "vmnet did not finish starting within 15s. Stop other vmnet clients and try again"
                        .into(),
                );
            }
            pump_main_runloop(0.05);
        };
        if info.status != VMNET_SUCCESS {
            abandon(interface, queue);
            return Err(format!(
                "vmnet_start_interface failed with {}",
                start_failure(info.status)
            ));
        }
        let read_budget = match info.max_packet {
            0 => 2048,
            n if (1..=16384).contains(&n) => (n as usize).max(2048),
            n => {
                abandon(interface, queue);
                return Err(format!("vmnet_max_packet_size_key {n} is above 16384"));
            }
        };
        let port_mac = if info.mac.is_empty() {
            "(none)".to_string()
        } else {
            info.mac.clone()
        };
        let (reported_network, shared_gateway, shared_cidr, host_iface) =
            if let Some(iface) = bridge_if
        {
            eprintln!(
                "[vmnet] bridged interface up on {iface} mtu={} max_packet={} port_mac={port_mac} read_budget={read_budget}",
                info.mtu, info.max_packet
            );
            eprintln!(
                "[vmnet] guests take IPv4 addresses from the DHCP server on {iface}; no host NAT"
            );
            (false, None, None, Some(iface.to_string()))
        } else {
            eprintln!(
                "[vmnet] shared interface up mtu={} max_packet={} port_mac={port_mac} read_budget={read_budget}",
                info.mtu, info.max_packet
            );
            let gateway = info.start_address.parse().ok();
            let cidr = shared_network_cidr(&info.start_address, &info.subnet_mask);
            let reported = match describe_shared_network(
                &info.start_address,
                &info.end_address,
                &info.subnet_mask,
            ) {
                Some(range) => {
                    eprintln!("[vmnet] shared network {range}");
                    true
                }
                None => false,
            };
            (reported, gateway, cidr, None)
        };
        Ok(Self {
            interface,
            queue,
            read_budget,
            seen_write_success: false,
            vmnet_err: 0,
            reported_network,
            shared_gateway,
            shared_cidr,
            host_iface,
        })
    }

    /// Install the packets-available callback. `false` means the caller should poll.
    pub fn set_event_pipe(&self, fd: i32) -> bool {
        EVENT_PIPE.store(fd, Ordering::Relaxed);
        let rc = unsafe {
            vmnet_interface_set_event_callback(
                self.interface,
                VMNET_INTERFACE_PACKETS_AVAILABLE,
                self.queue,
                &EVENT_BLOCK as *const BlockLiteral as *const c_void,
            )
        };
        if rc == VMNET_SUCCESS {
            true
        } else {
            EVENT_PIPE.store(-1, Ordering::Relaxed);
            eprintln!("[vmnet] event callback failed ({rc}); falling back to a 100 µs poll");
            false
        }
    }

    pub fn read(&mut self, buf: &mut [u8]) -> IoOutcome {
        if buf.is_empty() {
            return IoOutcome::Counted;
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
        let mut pktcnt = 1i32;
        let code = unsafe { vmnet_read(self.interface, &mut desc, &mut pktcnt) };
        self.finish(VmnetOp::Read, code, pktcnt, desc.vm_pkt_size, buf.len())
    }

    pub fn write(&mut self, frame: &[u8]) -> IoOutcome {
        let mut iov = Iovec {
            iov_base: frame.as_ptr() as *mut c_void,
            iov_len: frame.len(),
        };
        let mut desc = VmPktDesc {
            vm_pkt_size: frame.len() as u64,
            vm_pkt_iov: &mut iov,
            vm_pkt_iovcnt: 1,
            vm_flags: 0,
        };
        let mut pktcnt = 1i32;
        let code = unsafe { vmnet_write(self.interface, &mut desc, &mut pktcnt) };
        self.finish(VmnetOp::Write, code, pktcnt, frame.len() as u64, frame.len())
    }

    fn finish(&mut self, op: VmnetOp, code: u32, pktcnt: i32, size: u64, buf_len: usize) -> IoOutcome {
        let decision = classify_vmnet_status(op, code, pktcnt, self.seen_write_success);
        if decision.set_latch {
            self.seen_write_success = true;
        }
        match decision.class {
            StatusClass::Exit => IoOutcome::Fatal(format!(
                "vmnet {op:?} status {code}. Restart the service with VMM_VMNET_KEEP_ROOT=1 if the dropped user cannot use the interface."
            )),
            StatusClass::Idle => IoOutcome::Idle,
            StatusClass::Count => {
                self.vmnet_err = self.vmnet_err.saturating_add(1);
                IoOutcome::Counted
            }
            StatusClass::Deliver => {
                if op == VmnetOp::Write {
                    return IoOutcome::Idle;
                }
                if pktcnt <= 0 || size == 0 {
                    return IoOutcome::Idle;
                }
                if size as usize > buf_len {
                    return IoOutcome::Oversize;
                }
                IoOutcome::Packet(size as usize)
            }
        }
    }
}

impl Drop for VmnetPort {
    fn drop(&mut self) {
        if !self.interface.is_null() {
            unsafe {
                let _ = vmnet_stop_interface(self.interface, self.queue, std::ptr::null());
                dispatch_release(self.queue);
            }
            self.interface = std::ptr::null_mut();
        }
    }
}

pub enum IoOutcome {
    Idle,
    /// A packet was written into the caller buffer; the usize is `vm_pkt_size`.
    Packet(usize),
    Oversize,
    Counted,
    Fatal(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmnetOp {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusClass {
    Idle,
    Deliver,
    Count,
    Exit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StatusDecision {
    pub class: StatusClass,
    pub set_latch: bool,
}

/// Latch updates only when a write returns `VMNET_SUCCESS`. An idle read does not.
fn abandon(interface: *mut c_void, queue: *mut c_void) {
    unsafe {
        if !interface.is_null() {
            let _ = vmnet_stop_interface(interface, queue, std::ptr::null());
        }
        if !queue.is_null() {
            dispatch_release(queue);
        }
    }
}

pub fn classify_vmnet_status(
    op: VmnetOp,
    code: u32,
    pktcnt: i32,
    seen_write_success: bool,
) -> StatusDecision {
    if code == VMNET_INVALID_ACCESS || code == VMNET_NOT_AUTHORIZED {
        return StatusDecision {
            class: StatusClass::Exit,
            set_latch: false,
        };
    }
    if code == VMNET_SUCCESS {
        return match op {
            VmnetOp::Write => StatusDecision {
                class: StatusClass::Deliver,
                set_latch: true,
            },
            VmnetOp::Read if pktcnt <= 0 => StatusDecision {
                class: StatusClass::Idle,
                set_latch: false,
            },
            VmnetOp::Read => StatusDecision {
                class: StatusClass::Deliver,
                set_latch: false,
            },
        };
    }
    if code == VMNET_FAILURE && op == VmnetOp::Write && !seen_write_success {
        return StatusDecision {
            class: StatusClass::Exit,
            set_latch: false,
        };
    }
    if code == VMNET_PACKET_TOO_BIG || code == VMNET_BUFFER_EXHAUSTED {
        return StatusDecision {
            class: StatusClass::Count,
            set_latch: false,
        };
    }
    StatusDecision {
        class: StatusClass::Count,
        set_latch: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_failure_names_the_vmnet_status() {
        assert!(start_failure(VMNET_SHARING_SERVICE_BUSY).contains("sharing service is busy"));
        assert!(start_failure(VMNET_INVALID_ARGUMENT).contains("cannot bridge"));
        assert!(!start_failure(VMNET_FAILURE).contains("Permission Denied"));
    }

    #[test]
    fn shared_network_is_gateway_mask_and_dhcp_pool() {
        assert_eq!(
            describe_shared_network("192.168.2.1", "192.168.2.254", "255.255.255.0").as_deref(),
            Some("192.168.2.0/24 gateway 192.168.2.1 dhcp 192.168.2.2-192.168.2.254")
        );
        assert!(describe_shared_network("", "192.168.2.254", "255.255.255.0").is_none());
        assert!(describe_shared_network("192.168.2.1", "192.168.2.254", "255.0.255.0").is_none());
    }

    #[test]
    fn idle_read_does_not_clear_write_failure_latch() {
        let mut seen = false;
        let idle = classify_vmnet_status(VmnetOp::Read, VMNET_SUCCESS, 0, seen);
        assert_eq!(idle.class, StatusClass::Idle);
        assert!(!idle.set_latch);
        seen |= idle.set_latch;
        let early = classify_vmnet_status(VmnetOp::Write, VMNET_FAILURE, 1, seen);
        assert_eq!(early.class, StatusClass::Exit);
        assert!(!early.set_latch);

        seen = false;
        let traffic = classify_vmnet_status(VmnetOp::Read, VMNET_SUCCESS, 1, seen);
        assert_eq!(traffic.class, StatusClass::Deliver);
        assert!(!traffic.set_latch);
        seen |= traffic.set_latch;
        assert_eq!(
            classify_vmnet_status(VmnetOp::Write, VMNET_FAILURE, 1, seen).class,
            StatusClass::Exit
        );

        let ok = classify_vmnet_status(VmnetOp::Write, VMNET_SUCCESS, 1, seen);
        assert!(ok.set_latch);
        seen |= ok.set_latch;
        let later = classify_vmnet_status(VmnetOp::Write, VMNET_FAILURE, 1, seen);
        assert_eq!(later.class, StatusClass::Count);
        assert!(!later.set_latch);
        assert_eq!(
            classify_vmnet_status(VmnetOp::Read, VMNET_FAILURE, 0, seen).class,
            StatusClass::Count
        );
        assert_eq!(
            classify_vmnet_status(VmnetOp::Write, VMNET_INVALID_ACCESS, 1, true).class,
            StatusClass::Exit
        );
        assert_eq!(
            classify_vmnet_status(VmnetOp::Read, VMNET_NOT_AUTHORIZED, 0, false).class,
            StatusClass::Exit
        );
        assert_eq!(
            classify_vmnet_status(VmnetOp::Write, VMNET_PACKET_TOO_BIG, 1, true).class,
            StatusClass::Count
        );
        assert_eq!(
            classify_vmnet_status(VmnetOp::Read, VMNET_BUFFER_EXHAUSTED, 0, false).class,
            StatusClass::Count
        );
    }
}
