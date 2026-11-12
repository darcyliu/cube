//! Virtio-vsock stream device (ID 19).
//!
//! Guest port numbers do not open host sockets on their own. A guest REQUEST
//! connects only when that port is listed in an explicit map. Host programs
//! open streams by writing `connect <guest_port>` to the control socket.
//! Datagram and seqpacket are not offered.

use super::*;
use crate::devices::MmioDevice;
use crate::memory::GuestMemory;
use std::collections::{HashMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

// Stream sockets are baseline virtio-vsock functionality. The standardized
// device-specific feature is SEQPACKET (bit 1), which Cube does not implement.
const OFFERED: u64 = VIRTIO_F_VERSION_1;
const QUEUE_MAX: u32 = 128;
const RX_Q: usize = 0;
const TX_Q: usize = 1;
const EV_Q: usize = 2;
const HDR_LEN: usize = 44;
const HOST_CID: u64 = 2;
const TYPE_STREAM: u16 = 1;
const OP_REQUEST: u16 = 1;
const OP_RESPONSE: u16 = 2;
const OP_RST: u16 = 3;
const OP_SHUTDOWN: u16 = 4;
const OP_RW: u16 = 5;
const OP_CREDIT_UPDATE: u16 = 6;
const OP_CREDIT_REQUEST: u16 = 7;
const SHUTDOWN_RCV: u32 = 1;
const SHUTDOWN_SEND: u32 = 2;
const BUF_ALLOC: u32 = 64 * 1024;
const MAX_PAYLOAD: usize = 64 * 1024;
const MAX_CONNS: usize = 64;
const VM_BYTE_CAP: usize = 1024 * 1024;
const EVENT_RESET: u32 = 0;
const HOST_PORT_BASE: u32 = 0x4000_0000;
const MAX_GUEST_CID: u64 = u32::MAX as u64 - 1;

pub fn valid_guest_cid(cid: u64) -> bool {
    (3..=MAX_GUEST_CID).contains(&cid)
}

#[derive(Clone, Debug, Default)]
pub struct VsockConfig {
    pub cid: Option<u64>,
    pub listen: Option<PathBuf>,
    pub maps: Vec<(u32, PathBuf)>,
}

impl VsockConfig {
    pub fn enabled(&self) -> bool {
        self.cid.is_some() || self.listen.is_some() || !self.maps.is_empty()
    }
}

/// Exclusive CID lock. The file is removed when the lease is dropped.
/// A file whose recorded pid is no longer running can be replaced.
#[derive(Debug)]
pub struct CidLease {
    path: PathBuf,
    cid: u64,
    _file: File,
}

enum LeaseError {
    Reserved,
    InUse,
    Io(String),
}

impl std::fmt::Display for LeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reserved => write!(f, "vsock CID is reserved"),
            Self::InUse => write!(f, "vsock CID is in use"),
            Self::Io(msg) => write!(f, "{msg}"),
        }
    }
}

impl CidLease {
    pub fn cid(&self) -> u64 {
        self.cid
    }

    pub fn acquire(dir: &Path, cid: u64) -> Result<Self, String> {
        Self::try_acquire(dir, cid).map_err(|e| match e {
            LeaseError::Reserved => format!("vsock CID {cid} is reserved"),
            LeaseError::InUse => format!("vsock CID {cid} is in use"),
            LeaseError::Io(msg) => msg,
        })
    }

    pub fn allocate(dir: &Path) -> Result<Self, String> {
        for cid in 3..4096u64 {
            match Self::try_acquire(dir, cid) {
                Ok(lease) => return Ok(lease),
                Err(LeaseError::InUse) => continue,
                Err(err) => return Err(err.to_string()),
            }
        }
        Err("no free vsock CID".into())
    }

    fn try_acquire(dir: &Path, cid: u64) -> Result<Self, LeaseError> {
        if !valid_guest_cid(cid) {
            return Err(LeaseError::Reserved);
        }
        fs::create_dir_all(dir).map_err(|e| LeaseError::Io(e.to_string()))?;
        let path = dir.join(cid.to_string());
        let mut file = match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => file,
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                if !stale_lock(&path) {
                    return Err(LeaseError::InUse);
                }
                let _ = fs::remove_file(&path);
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create_new(true)
                    .open(&path)
                    .map_err(|_| LeaseError::InUse)?
            }
            Err(err) => return Err(LeaseError::Io(err.to_string())),
        };
        writeln!(file, "{}", std::process::id()).map_err(|e| LeaseError::Io(e.to_string()))?;
        let _ = file.sync_all();
        Ok(Self {
            path,
            cid,
            _file: file,
        })
    }
}

impl Drop for CidLease {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn stale_lock(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    let Ok(pid) = text.trim().parse::<i32>() else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    let alive = unsafe { libc::kill(pid, 0) };
    alive == -1 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[derive(Clone, Copy, Debug)]
struct Header {
    src_cid: u64,
    dst_cid: u64,
    src_port: u32,
    dst_port: u32,
    len: u32,
    type_: u16,
    op: u16,
    flags: u32,
    buf_alloc: u32,
    fwd_cnt: u32,
}

impl Header {
    fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < HDR_LEN {
            return None;
        }
        let bytes8 = |off: usize| -> Option<[u8; 8]> { buf[off..off + 8].try_into().ok() };
        let bytes4 = |off: usize| -> Option<[u8; 4]> { buf[off..off + 4].try_into().ok() };
        let bytes2 = |off: usize| -> Option<[u8; 2]> { buf[off..off + 2].try_into().ok() };
        Some(Self {
            src_cid: u64::from_le_bytes(bytes8(0)?),
            dst_cid: u64::from_le_bytes(bytes8(8)?),
            src_port: u32::from_le_bytes(bytes4(16)?),
            dst_port: u32::from_le_bytes(bytes4(20)?),
            len: u32::from_le_bytes(bytes4(24)?),
            type_: u16::from_le_bytes(bytes2(28)?),
            op: u16::from_le_bytes(bytes2(30)?),
            flags: u32::from_le_bytes(bytes4(32)?),
            buf_alloc: u32::from_le_bytes(bytes4(36)?),
            fwd_cnt: u32::from_le_bytes(bytes4(40)?),
        })
    }

    fn encode(&self, payload: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(HDR_LEN + payload.len());
        out.extend_from_slice(&self.src_cid.to_le_bytes());
        out.extend_from_slice(&self.dst_cid.to_le_bytes());
        out.extend_from_slice(&self.src_port.to_le_bytes());
        out.extend_from_slice(&self.dst_port.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.type_.to_le_bytes());
        out.extend_from_slice(&self.op.to_le_bytes());
        out.extend_from_slice(&self.flags.to_le_bytes());
        out.extend_from_slice(&self.buf_alloc.to_le_bytes());
        out.extend_from_slice(&self.fwd_cnt.to_le_bytes());
        out.extend_from_slice(payload);
        out
    }
}

fn guest_key(hdr: &Header) -> (u32, u32) {
    (hdr.src_port, hdr.dst_port)
}

fn make_hdr(cid: u64, conn: &Conn, op: u16, len: u32, flags: u32) -> Header {
    Header {
        src_cid: HOST_CID,
        dst_cid: cid,
        src_port: conn.host_port,
        dst_port: conn.guest_port,
        len,
        type_: TYPE_STREAM,
        op,
        flags,
        buf_alloc: BUF_ALLOC,
        fwd_cnt: conn.fwd_cnt,
    }
}

struct OutPkt {
    hdr: Header,
    payload: Vec<u8>,
}

struct TxPacket {
    epoch: u64,
    hdr: Header,
    payload: Vec<u8>,
}

struct Conn {
    guest_port: u32,
    host_port: u32,
    socket: Option<UnixStream>,
    established: bool,
    local_initiated: bool,
    peer_buf_alloc: u32,
    peer_fwd_cnt: u32,
    tx_cnt: u32,
    rx_cnt: u32,
    fwd_cnt: u32,
    to_guest: VecDeque<u8>,
    to_host: VecDeque<u8>,
    guest_shut_send: bool,
    guest_shut_rcv: bool,
    want_shut_send: bool,
    want_shut_rcv: bool,
    sent_shut_send: bool,
    sent_shut_rcv: bool,
    do_shut_rd: bool,
    do_shut_wr: bool,
    host_eof: bool,
    want_rst: bool,
    credit_pending: bool,
}

impl Conn {
    fn new(guest_port: u32, host_port: u32, socket: UnixStream, local_initiated: bool) -> Self {
        Self {
            guest_port,
            host_port,
            socket: Some(socket),
            established: !local_initiated,
            local_initiated,
            peer_buf_alloc: 0,
            peer_fwd_cnt: 0,
            tx_cnt: 0,
            rx_cnt: 0,
            fwd_cnt: 0,
            to_guest: VecDeque::new(),
            to_host: VecDeque::new(),
            guest_shut_send: false,
            guest_shut_rcv: false,
            want_shut_send: false,
            want_shut_rcv: false,
            sent_shut_send: false,
            sent_shut_rcv: false,
            do_shut_rd: false,
            do_shut_wr: false,
            host_eof: false,
            want_rst: false,
            credit_pending: false,
        }
    }

    fn send_credit(&self) -> u32 {
        let inflight = self.tx_cnt.wrapping_sub(self.peer_fwd_cnt);
        self.peer_buf_alloc.saturating_sub(inflight)
    }

    fn recv_room(&self) -> u32 {
        let inflight = self.rx_cnt.wrapping_sub(self.fwd_cnt);
        BUF_ALLOC.saturating_sub(inflight)
    }
}

enum Plan {
    None,
    Connect {
        path: PathBuf,
        guest_port: u32,
        host_port: u32,
        peer_buf: u32,
        peer_fwd: u32,
    },
}

struct IoJob {
    key: (u32, u32),
    socket: UnixStream,
    to_write: Vec<u8>,
    read_into: usize,
    shut_rd: bool,
    shut_wr: bool,
}

struct IoResult {
    written: usize,
    write_closed: bool,
    read_data: Vec<u8>,
    eof: bool,
    read_err: bool,
}

impl IoJob {
    fn perform(&mut self) -> IoResult {
        if self.shut_wr {
            shut(&self.socket, libc::SHUT_WR);
        }
        if self.shut_rd {
            shut(&self.socket, libc::SHUT_RD);
        }
        let mut written = 0;
        let mut write_closed = false;
        if !self.to_write.is_empty() {
            match self.socket.write(&self.to_write) {
                Ok(0) => write_closed = true,
                Ok(n) => written = n,
                Err(err) if pause(&err) => {}
                Err(_) => write_closed = true,
            }
        }
        let mut read_data = Vec::new();
        let mut eof = false;
        let mut read_err = false;
        if self.read_into > 0 {
            let mut buf = vec![0u8; self.read_into];
            match self.socket.read(&mut buf) {
                Ok(0) => eof = true,
                Ok(n) => read_data.extend_from_slice(&buf[..n]),
                Err(err) if pause(&err) => {}
                Err(_) => read_err = true,
            }
        }
        IoResult {
            written,
            write_closed,
            read_data,
            eof,
            read_err,
        }
    }
}

fn pause(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
    )
}

fn shut(stream: &UnixStream, how: i32) {
    unsafe {
        libc::shutdown(stream.as_raw_fd(), how);
    }
}

fn nosigpipe(stream: &UnixStream) {
    let one: libc::c_int = 1;
    unsafe {
        libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_NOSIGPIPE,
            (&one as *const libc::c_int).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }
}

fn signal(irq: u32, level_high: bool, kick: bool) {
    #[cfg(not(test))]
    unsafe {
        crate::hypervisor::hv_gic_set_spi(irq, level_high);
    }
    #[cfg(test)]
    let _ = (irq, level_high);
    if kick && level_high {
        crate::vcpu::kick_vcpu();
    }
}

struct State {
    transport: MmioTransport,
    cid: u64,
    maps: HashMap<u32, PathBuf>,
    conns: HashMap<(u32, u32), Conn>,
    tx_q: VecDeque<TxPacket>,
    rx_q: VecDeque<OutPkt>,
    event_pending: bool,
    epoch: u64,
    host_port_next: u32,
    stopped: bool,
}

impl State {
    fn vm_to_guest_bytes(&self) -> usize {
        self.conns.values().map(|c| c.to_guest.len()).sum::<usize>()
            + self.rx_q.iter().map(|p| p.payload.len()).sum::<usize>()
    }

    fn queue_rst(&mut self, hdr: &Header) {
        if hdr.op == OP_RST {
            return;
        }
        let fwd = self
            .conns
            .get(&guest_key(hdr))
            .map(|c| c.fwd_cnt)
            .unwrap_or(0);
        self.rx_q.push_back(OutPkt {
            hdr: Header {
                src_cid: HOST_CID,
                dst_cid: hdr.src_cid,
                src_port: hdr.dst_port,
                dst_port: hdr.src_port,
                len: 0,
                type_: hdr.type_,
                op: OP_RST,
                flags: 0,
                buf_alloc: BUF_ALLOC,
                fwd_cnt: fwd,
            },
            payload: Vec::new(),
        });
    }

    fn plan_tx(&mut self, pkt: &TxPacket) -> Plan {
        let hdr = &pkt.hdr;
        if hdr.src_cid != self.cid || hdr.dst_cid != HOST_CID {
            self.queue_rst(hdr);
            let drops = &mut self.transport.diagnostics_mut().drops;
            *drops = drops.saturating_add(1);
            return Plan::None;
        }
        if let Some(conn) = self.conns.get_mut(&guest_key(hdr)) {
            conn.peer_buf_alloc = hdr.buf_alloc;
            conn.peer_fwd_cnt = hdr.fwd_cnt;
        }
        if hdr.type_ != TYPE_STREAM {
            self.queue_rst(hdr);
            return Plan::None;
        }
        match hdr.op {
            OP_RST => {
                self.conns.remove(&guest_key(hdr));
                Plan::None
            }
            OP_REQUEST => self.plan_request(hdr),
            OP_RESPONSE => self.plan_response(hdr),
            OP_RW => self.plan_rw(hdr, &pkt.payload),
            OP_CREDIT_UPDATE => Plan::None,
            OP_CREDIT_REQUEST => {
                if let Some(conn) = self.conns.get_mut(&guest_key(hdr)) {
                    conn.credit_pending = true;
                } else {
                    self.queue_rst(hdr);
                }
                Plan::None
            }
            OP_SHUTDOWN => self.plan_shutdown(hdr),
            _ => {
                self.queue_rst(hdr);
                Plan::None
            }
        }
    }

    fn plan_request(&mut self, hdr: &Header) -> Plan {
        let key = guest_key(hdr);
        if hdr.src_cid != self.cid || hdr.dst_cid != HOST_CID || hdr.src_port == 0 {
            self.queue_rst(hdr);
            return Plan::None;
        }
        if self.conns.contains_key(&key) || self.conns.len() >= MAX_CONNS {
            self.queue_rst(hdr);
            return Plan::None;
        }
        let Some(path) = self.maps.get(&hdr.dst_port).cloned() else {
            self.queue_rst(hdr);
            return Plan::None;
        };
        Plan::Connect {
            path,
            guest_port: hdr.src_port,
            host_port: hdr.dst_port,
            peer_buf: hdr.buf_alloc,
            peer_fwd: hdr.fwd_cnt,
        }
    }

    fn plan_response(&mut self, hdr: &Header) -> Plan {
        let key = guest_key(hdr);
        let local = self.conns.get(&key).map(|conn| conn.local_initiated);
        match local {
            None => self.queue_rst(hdr),
            Some(false) => {
                self.queue_rst(hdr);
                self.conns.remove(&key);
            }
            Some(true) => {
                self.conns.get_mut(&key).expect("present").established = true;
            }
        }
        Plan::None
    }

    fn plan_rw(&mut self, hdr: &Header, payload: &[u8]) -> Plan {
        let key = guest_key(hdr);
        let reject = match self.conns.get(&key) {
            Some(conn) => {
                !conn.established
                    || conn.guest_shut_send
                    || hdr.len as u64 > u64::from(conn.recv_room())
                    || payload.len() != hdr.len as usize
            }
            None => true,
        };
        if reject {
            self.conns.remove(&key);
            self.queue_rst(hdr);
            let drops = &mut self.transport.diagnostics_mut().drops;
            *drops = drops.saturating_add(1);
            return Plan::None;
        }
        let conn = self.conns.get_mut(&key).expect("checked");
        conn.to_host.extend(payload);
        conn.rx_cnt = conn.rx_cnt.wrapping_add(hdr.len);
        Plan::None
    }

    fn plan_shutdown(&mut self, hdr: &Header) -> Plan {
        let key = guest_key(hdr);
        let Some(conn) = self.conns.get_mut(&key) else {
            self.queue_rst(hdr);
            return Plan::None;
        };
        if hdr.flags & SHUTDOWN_SEND != 0 {
            conn.guest_shut_send = true;
            conn.do_shut_wr = true;
        }
        if hdr.flags & SHUTDOWN_RCV != 0 {
            conn.guest_shut_rcv = true;
            conn.do_shut_rd = true;
            conn.to_guest.clear();
        }
        Plan::None
    }

    fn install_guest_conn(
        &mut self,
        guest_port: u32,
        host_port: u32,
        socket: UnixStream,
        peer_buf: u32,
        peer_fwd: u32,
    ) -> bool {
        if self.stopped || self.conns.len() >= MAX_CONNS {
            return false;
        }
        let key = (guest_port, host_port);
        if self.conns.contains_key(&key) {
            return false;
        }
        let mut conn = Conn::new(guest_port, host_port, socket, false);
        conn.peer_buf_alloc = peer_buf;
        conn.peer_fwd_cnt = peer_fwd;
        let hdr = make_hdr(self.cid, &conn, OP_RESPONSE, 0, 0);
        self.conns.insert(key, conn);
        self.rx_q.push_back(OutPkt {
            hdr,
            payload: Vec::new(),
        });
        true
    }

    fn enqueue_outputs(&mut self) {
        let keys: Vec<(u32, u32)> = self.conns.keys().copied().collect();
        for key in keys {
            self.enqueue_one(key);
        }
    }

    fn enqueue_one(&mut self, key: (u32, u32)) {
        let cid = self.cid;
        let Some(conn) = self.conns.get_mut(&key) else {
            return;
        };
        if conn.want_rst {
            let hdr = make_hdr(cid, conn, OP_RST, 0, 0);
            self.conns.remove(&key);
            self.rx_q.push_back(OutPkt {
                hdr,
                payload: Vec::new(),
            });
            return;
        }
        if !conn.established {
            return;
        }
        let rw = if !conn.guest_shut_rcv && !conn.to_guest.is_empty() {
            let credit = conn.send_credit();
            if credit > 0 {
                let n = (credit as usize).min(conn.to_guest.len()).min(4096);
                let payload: Vec<u8> = conn.to_guest.drain(..n).collect();
                conn.tx_cnt = conn.tx_cnt.wrapping_add(n as u32);
                let hdr = make_hdr(cid, conn, OP_RW, n as u32, 0);
                conn.credit_pending = false;
                Some((hdr, payload))
            } else {
                None
            }
        } else {
            None
        };
        let sent_rw = rw.is_some();
        if let Some((hdr, payload)) = rw {
            self.rx_q.push_back(OutPkt { hdr, payload });
        }
        let Some(conn) = self.conns.get_mut(&key) else {
            return;
        };
        let credit_pkt = if conn.credit_pending && !sent_rw {
            conn.credit_pending = false;
            Some(make_hdr(cid, conn, OP_CREDIT_UPDATE, 0, 0))
        } else {
            None
        };
        if let Some(hdr) = credit_pkt {
            self.rx_q.push_back(OutPkt {
                hdr,
                payload: Vec::new(),
            });
        }
        let Some(conn) = self.conns.get_mut(&key) else {
            return;
        };
        let shut_send = if conn.to_guest.is_empty() && conn.want_shut_send && !conn.sent_shut_send {
            conn.sent_shut_send = true;
            conn.want_shut_send = false;
            Some(make_hdr(cid, conn, OP_SHUTDOWN, 0, SHUTDOWN_SEND))
        } else {
            None
        };
        if let Some(hdr) = shut_send {
            self.rx_q.push_back(OutPkt {
                hdr,
                payload: Vec::new(),
            });
        }
        let Some(conn) = self.conns.get_mut(&key) else {
            return;
        };
        let shut_rcv = if conn.want_shut_rcv && !conn.sent_shut_rcv {
            conn.sent_shut_rcv = true;
            conn.want_shut_rcv = false;
            Some(make_hdr(cid, conn, OP_SHUTDOWN, 0, SHUTDOWN_RCV))
        } else {
            None
        };
        if let Some(hdr) = shut_rcv {
            self.rx_q.push_back(OutPkt {
                hdr,
                payload: Vec::new(),
            });
        }
        let remove = self.conns.get(&key).is_some_and(|conn| {
            conn.guest_shut_send
                && conn.guest_shut_rcv
                && conn.to_host.is_empty()
                && conn.to_guest.is_empty()
                && !conn.want_shut_send
                && !conn.want_shut_rcv
        });
        if remove {
            self.conns.remove(&key);
        }
    }

    fn take_io_jobs(&mut self) -> Vec<IoJob> {
        let mut budget = VM_BYTE_CAP.saturating_sub(self.vm_to_guest_bytes());
        let mut jobs = Vec::new();
        for (key, conn) in &mut self.conns {
            let Some(socket) = conn.socket.take() else {
                continue;
            };
            let to_write: Vec<u8> = conn.to_host.drain(..).collect();
            let read_into = if budget == 0 || conn.host_eof || conn.guest_shut_rcv || conn.want_rst
            {
                0
            } else {
                let room = (BUF_ALLOC as usize)
                    .saturating_sub(conn.to_guest.len())
                    .min(16 * 1024)
                    .min(budget);
                budget = budget.saturating_sub(room);
                room
            };
            let shut_rd = conn.do_shut_rd;
            let shut_wr = conn.do_shut_wr;
            conn.do_shut_rd = false;
            conn.do_shut_wr = false;
            jobs.push(IoJob {
                key: *key,
                socket,
                to_write,
                read_into,
                shut_rd,
                shut_wr,
            });
        }
        jobs
    }

    fn finish_io(&mut self, job: IoJob, result: IoResult) {
        let Some(conn) = self.conns.get_mut(&job.key) else {
            return;
        };
        if result.write_closed {
            conn.want_shut_rcv = true;
        } else if result.written < job.to_write.len() {
            let mut restored: VecDeque<u8> =
                job.to_write.into_iter().skip(result.written).collect();
            restored.append(&mut conn.to_host);
            conn.to_host = restored;
        }
        if result.written > 0 {
            conn.fwd_cnt = conn.fwd_cnt.wrapping_add(result.written as u32);
            conn.credit_pending = true;
        }
        if result.read_err {
            conn.want_rst = true;
        } else {
            conn.to_guest.extend(result.read_data);
            if result.eof {
                conn.host_eof = true;
                conn.want_shut_send = true;
            }
        }
        conn.socket = Some(job.socket);
    }

    fn harvest_tx(&mut self, mem: &GuestMemory) -> bool {
        if !self.transport.operational() {
            return false;
        }
        let Some(mut queue) = self.transport.queue(TX_Q).copied() else {
            return false;
        };
        if !queue.ready || queue.desc_table == 0 || queue.avail_ring == 0 || queue.used_ring == 0 {
            return false;
        }
        let count = match queue.pending_count(mem) {
            Ok(n) => n,
            Err(_) => {
                self.transport.set_needs_reset();
                return false;
            }
        };
        let epoch = self.epoch;
        let mut curr = queue.last_avail_idx;
        let mut processed = false;
        for _ in 0..count {
            let slot = queue.avail_ring + 4 + 2 * (curr as u64 % queue.size as u64);
            let Ok(head) = mem.read_u16(slot) else {
                self.transport.set_needs_reset();
                break;
            };
            let descs = match queue.descriptors(mem, head, false, HDR_LEN + MAX_PAYLOAD) {
                Ok(descs)
                    if !descs.is_empty()
                        && descs.iter().all(|d| d.flags & VRING_DESC_F_WRITE == 0) =>
                {
                    descs
                }
                _ => {
                    self.transport.set_needs_reset();
                    break;
                }
            };
            let mut raw = Vec::new();
            let mut readable = true;
            for desc in &descs {
                let start = raw.len();
                raw.resize(start + desc.len as usize, 0);
                if desc.len > 0 && mem.read(desc.addr, &mut raw[start..]).is_err() {
                    readable = false;
                    break;
                }
            }
            if !readable {
                self.transport.set_needs_reset();
                break;
            }
            let Some(hdr) = Header::parse(&raw) else {
                self.transport.set_needs_reset();
                break;
            };
            let Some(need) = HDR_LEN.checked_add(hdr.len as usize) else {
                self.transport.set_needs_reset();
                break;
            };
            if hdr.len as usize > MAX_PAYLOAD || raw.len() < need {
                self.transport.set_needs_reset();
                break;
            }
            let payload = raw[HDR_LEN..need].to_vec();
            publish_used(mem, &mut queue, head, 0);
            let diag = self.transport.diagnostics_mut();
            diag.completions = diag.completions.saturating_add(1);
            self.tx_q.push_back(TxPacket {
                epoch,
                hdr,
                payload,
            });
            curr = curr.wrapping_add(1);
            processed = true;
        }
        queue.last_avail_idx = curr;
        let avail = queue.avail_ring;
        if let Some(slot) = self.transport.queue_mut(TX_Q) {
            *slot = queue;
        }
        if processed && avail_wants_interrupt(mem, avail) {
            self.transport.raise_interrupt(1);
            true
        } else {
            false
        }
    }

    fn flush_rx(&mut self, mem: &GuestMemory) -> bool {
        self.flush_out(mem, RX_Q, false)
    }

    fn flush_event(&mut self, mem: &GuestMemory) -> bool {
        if !self.event_pending {
            return false;
        }
        let pkt = OutPkt {
            hdr: Header {
                src_cid: 0,
                dst_cid: 0,
                src_port: 0,
                dst_port: 0,
                len: 4,
                type_: 0,
                op: 0,
                flags: 0,
                buf_alloc: 0,
                fwd_cnt: 0,
            },
            payload: EVENT_RESET.to_le_bytes().to_vec(),
        };
        // Event records are a bare le32, not a vsock header. Park the bytes in
        // rx_q only for the duration of this flush, then put them back if the
        // event queue could not take them.
        let saved = std::mem::take(&mut self.rx_q);
        self.rx_q.push_back(pkt);
        let raised = self.flush_out(mem, EV_Q, true);
        let delivered = self.rx_q.is_empty();
        self.rx_q = saved;
        if delivered {
            self.event_pending = false;
        }
        raised
    }

    fn flush_out(&mut self, mem: &GuestMemory, index: usize, event: bool) -> bool {
        if !self.transport.operational() || self.rx_q.is_empty() {
            return false;
        }
        let Some(mut queue) = self.transport.queue(index).copied() else {
            return false;
        };
        if !queue.ready || queue.desc_table == 0 || queue.avail_ring == 0 || queue.used_ring == 0 {
            return false;
        }
        let count = match queue.pending_count(mem) {
            Ok(n) => n,
            Err(_) => {
                self.transport.set_needs_reset();
                return false;
            }
        };
        let start = queue.last_avail_idx;
        let mut curr = start;
        let mut processed = false;
        let mut byte_count = 0u64;
        for _ in 0..count {
            if self.rx_q.is_empty() {
                break;
            }
            let slot = queue.avail_ring + 4 + 2 * (curr as u64 % queue.size as u64);
            let Ok(head) = mem.read_u16(slot) else {
                self.transport.set_needs_reset();
                break;
            };
            let descs = match queue.descriptors(mem, head, false, HDR_LEN + MAX_PAYLOAD) {
                Ok(descs)
                    if !descs.is_empty()
                        && descs.iter().all(|d| d.flags & VRING_DESC_F_WRITE != 0) =>
                {
                    descs
                }
                _ => {
                    self.transport.set_needs_reset();
                    break;
                }
            };
            let cap: usize = descs.iter().map(|d| d.len as usize).sum();
            let min = if event { 4 } else { HDR_LEN };
            if cap < min {
                self.transport.set_needs_reset();
                break;
            }
            let bytes = if event {
                let pkt = self.rx_q.pop_front().expect("nonempty");
                pkt.payload
            } else {
                let pkt = self.rx_q.front_mut().expect("nonempty");
                let room = cap - HDR_LEN;
                if pkt.hdr.op != OP_RW && pkt.payload.len() > room {
                    break;
                }
                let n = room.min(pkt.payload.len());
                let payload: Vec<u8> = pkt.payload.drain(..n).collect();
                let mut hdr = pkt.hdr;
                hdr.fwd_cnt = hdr.fwd_cnt;
                let done = pkt.payload.is_empty();
                let bytes = hdr.encode(&payload);
                if done {
                    self.rx_q.pop_front();
                }
                byte_count = byte_count.saturating_add(n as u64);
                bytes
            };
            if write_chain(mem, &descs, &bytes).is_err() {
                self.transport.set_needs_reset();
                break;
            }
            publish_used(mem, &mut queue, head, bytes.len() as u32);
            curr = curr.wrapping_add(1);
            processed = true;
        }
        let published = curr.wrapping_sub(start);
        queue.last_avail_idx = curr;
        let avail = queue.avail_ring;
        let diag = self.transport.diagnostics_mut();
        diag.completions = diag.completions.saturating_add(u64::from(published));
        diag.bytes = diag.bytes.saturating_add(byte_count);
        if let Some(slot) = self.transport.queue_mut(index) {
            *slot = queue;
        }
        if processed && avail_wants_interrupt(mem, avail) {
            self.transport.raise_interrupt(1);
            true
        } else {
            false
        }
    }
}

fn write_chain(mem: &GuestMemory, descs: &[Descriptor], data: &[u8]) -> Result<(), ()> {
    let mut off = 0;
    for desc in descs {
        if off >= data.len() {
            break;
        }
        let n = (desc.len as usize).min(data.len() - off);
        if n > 0 {
            mem.write(desc.addr, &data[off..off + n]).map_err(|_| ())?;
            off += n;
        }
    }
    if off < data.len() { Err(()) } else { Ok(()) }
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    mem: Arc<GuestMemory>,
    irq: u32,
}

impl Shared {
    fn run_once(&self) {
        let epoch = {
            let st = self.state.lock().unwrap();
            st.epoch
        };
        let packets: Vec<TxPacket> = {
            let mut st = self.state.lock().unwrap();
            if st.epoch != epoch {
                return;
            }
            st.tx_q.drain(..).collect()
        };
        for pkt in packets {
            self.apply_tx(epoch, pkt);
        }
        self.pump_sockets(epoch);
        let mut st = self.state.lock().unwrap();
        if st.epoch != epoch {
            return;
        }
        st.enqueue_outputs();
        let rx = st.flush_rx(&self.mem);
        let ev = st.flush_event(&self.mem);
        drop(st);
        if rx || ev {
            signal(self.irq, true, true);
        }
    }

    fn apply_tx(&self, epoch: u64, pkt: TxPacket) {
        if pkt.epoch != epoch {
            return;
        }
        let plan = {
            let mut st = self.state.lock().unwrap();
            if st.epoch != epoch || st.stopped {
                return;
            }
            st.plan_tx(&pkt)
        };
        let Plan::Connect {
            path,
            guest_port,
            host_port,
            peer_buf,
            peer_fwd,
        } = plan
        else {
            return;
        };
        match UnixStream::connect(&path) {
            Ok(sock) => {
                nosigpipe(&sock);
                let _ = sock.set_nonblocking(true);
                let mut st = self.state.lock().unwrap();
                if st.epoch != epoch || st.stopped {
                    return;
                }
                if !st.install_guest_conn(guest_port, host_port, sock, peer_buf, peer_fwd) {
                    st.queue_rst(&pkt.hdr);
                }
            }
            Err(err) => {
                eprintln!(
                    "[VirtIO-Vsock] port {host_port} connect to {} failed: {err}",
                    path.display()
                );
                let mut st = self.state.lock().unwrap();
                if st.epoch == epoch {
                    st.queue_rst(&pkt.hdr);
                }
            }
        }
    }

    fn pump_sockets(&self, epoch: u64) {
        let jobs = {
            let mut st = self.state.lock().unwrap();
            if st.epoch != epoch {
                return;
            }
            st.take_io_jobs()
        };
        let finished: Vec<(IoJob, IoResult)> = jobs
            .into_iter()
            .map(|mut job| {
                let result = job.perform();
                (job, result)
            })
            .collect();
        let mut st = self.state.lock().unwrap();
        if st.epoch != epoch {
            return;
        }
        for (job, result) in finished {
            st.finish_io(job, result);
        }
    }

    fn harvest_tx(&self) {
        let raised = {
            let mut st = self.state.lock().unwrap();
            st.harvest_tx(&self.mem)
        };
        if raised {
            signal(self.irq, true, true);
        }
    }

    fn flush_guest(&self) {
        let raised = {
            let mut st = self.state.lock().unwrap();
            let rx = st.flush_rx(&self.mem);
            let ev = st.flush_event(&self.mem);
            rx || ev
        };
        if raised {
            signal(self.irq, true, true);
        }
    }

    fn reset_streams(&self) {
        let mut st = self.state.lock().unwrap();
        st.epoch = st.epoch.wrapping_add(1);
        st.tx_q.clear();
        st.rx_q.clear();
        st.conns.clear();
        st.event_pending = true;
    }

    fn start_host_connection(&self, guest_port: u32, stream: &UnixStream) -> Result<(), String> {
        if guest_port == 0 {
            return Err("guest port 0 is not valid".into());
        }
        let stored = stream.try_clone().map_err(|e| e.to_string())?;
        nosigpipe(&stored);
        stored.set_nonblocking(true).map_err(|e| e.to_string())?;
        let mut st = self.state.lock().unwrap();
        if st.stopped {
            return Err("vsock is stopped".into());
        }
        if !st.transport.operational() {
            return Err("guest driver is not ready".into());
        }
        let host_port = alloc_host_port(&mut st)?;
        if st.conns.len() >= MAX_CONNS {
            return Err("too many vsock connections".into());
        }
        let mut conn = Conn::new(guest_port, host_port, stored, true);
        conn.established = false;
        let hdr = make_hdr(st.cid, &conn, OP_REQUEST, 0, 0);
        st.conns.insert((guest_port, host_port), conn);
        st.rx_q.push_back(OutPkt {
            hdr,
            payload: Vec::new(),
        });
        drop(st);
        self.flush_guest();
        self.wake.notify_one();
        Ok(())
    }

    fn adopt_control_stream(&self, mut stream: UnixStream) {
        nosigpipe(&stream);
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let line = match read_control_line(&mut stream) {
            Ok(line) => line,
            Err(err) => {
                let _ = write!(stream, "ERR {err}\n");
                return;
            }
        };
        let port = match parse_connect_line(&line) {
            Ok(port) => port,
            Err(err) => {
                let _ = write!(stream, "ERR {err}\n");
                return;
            }
        };
        if let Err(err) = self.start_host_connection(port, &stream) {
            let _ = write!(stream, "ERR {err}\n");
            return;
        }
        let _ = write!(stream, "OK\n");
    }
}

fn alloc_host_port(st: &mut State) -> Result<u32, String> {
    for _ in 0..65536 {
        let port = st.host_port_next;
        st.host_port_next = st.host_port_next.wrapping_add(1);
        if st.host_port_next < HOST_PORT_BASE {
            st.host_port_next = HOST_PORT_BASE;
        }
        if port < HOST_PORT_BASE || port == u32::MAX {
            continue;
        }
        if st.maps.contains_key(&port) {
            continue;
        }
        if st.conns.values().any(|c| c.host_port == port) {
            continue;
        }
        return Ok(port);
    }
    Err("no free vsock host port".into())
}

fn read_control_line(stream: &mut UnixStream) -> Result<String, String> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while buf.len() < 80 {
        match stream.read(&mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) if byte[0] == b'\r' => {}
            Ok(_) => buf.push(byte[0]),
            Err(err) if pause(&err) => return Err("control command timed out".into()),
            Err(err) => return Err(err.to_string()),
        }
    }
    if buf.len() >= 80 {
        return Err("control command is too long".into());
    }
    String::from_utf8(buf).map_err(|_| "control command is not utf-8".into())
}

fn parse_connect_line(line: &str) -> Result<u32, String> {
    let mut parts = line.split_whitespace();
    let Some(cmd) = parts.next() else {
        return Err("empty command".into());
    };
    if cmd != "connect" {
        return Err(format!("unknown command '{cmd}'"));
    }
    let Some(raw) = parts.next() else {
        return Err("connect requires a guest port".into());
    };
    if parts.next().is_some() {
        return Err("connect takes one guest port".into());
    }
    let port: u32 = raw
        .parse()
        .map_err(|_| format!("invalid guest port '{raw}'"))?;
    if port == 0 {
        return Err("guest port 0 is not valid".into());
    }
    Ok(port)
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
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
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
    let listener =
        UnixListener::bind(path).map_err(|e| format!("vsock listen {}: {e}", path.display()))?;
    if let Err(err) = fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
        drop(listener);
        let _ = fs::remove_file(path);
        return Err(format!("secure vsock listener {}: {err}", path.display()));
    }
    let identity = socket_identity(path)
        .ok_or_else(|| format!("cannot identify vsock listener {}", path.display()))?;
    Ok((listener, identity))
}

fn worker_loop(shared: Arc<Shared>) {
    loop {
        {
            let st = shared.state.lock().unwrap();
            if st.stopped {
                return;
            }
        }
        shared.run_once();
        let st = shared.state.lock().unwrap();
        if st.stopped {
            return;
        }
        let wait =
            if st.conns.is_empty() && st.tx_q.is_empty() && st.rx_q.is_empty() && !st.event_pending
            {
                Duration::from_secs(60)
            } else {
                Duration::from_millis(20)
            };
        let _ = shared.wake.wait_timeout(st, wait);
    }
}

fn control_loop(listener: UnixListener, shared: Arc<Shared>, path: PathBuf, identity: (u64, u64)) {
    loop {
        if shared.state.lock().unwrap().stopped {
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                if shared.state.lock().unwrap().stopped {
                    break;
                }
                shared.adopt_control_stream(stream);
            }
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
    remove_socket_if_same(&path, identity);
}

pub struct VirtioVsock {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    control: Option<JoinHandle<()>>,
    listen_path: Option<PathBuf>,
    listen_identity: Option<(u64, u64)>,
    joined: bool,
}

impl VirtioVsock {
    pub fn open(
        irq: u32,
        mem: Arc<GuestMemory>,
        cid: u64,
        maps: Vec<(u32, PathBuf)>,
        listen: Option<PathBuf>,
    ) -> Result<Self, String> {
        Self::build(irq, mem, cid, maps, listen, true)
    }

    fn build(
        irq: u32,
        mem: Arc<GuestMemory>,
        cid: u64,
        maps: Vec<(u32, PathBuf)>,
        listen: Option<PathBuf>,
        spawn: bool,
    ) -> Result<Self, String> {
        if !valid_guest_cid(cid) {
            return Err(format!("vsock CID {cid} is reserved"));
        }
        let mut table = HashMap::new();
        for (port, path) in maps {
            if port == 0 || path.as_os_str().is_empty() {
                return Err(format!("vsock map port {port} needs a unix socket path"));
            }
            if table.insert(port, path).is_some() {
                return Err(format!("vsock map port {port} is duplicated"));
            }
        }
        let listener = if spawn {
            match &listen {
                Some(path) => Some(bind_control(path)?),
                None => None,
            }
        } else {
            None
        };
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                transport: MmioTransport::new(
                    VIRTIO_DEV_VSOCK,
                    OFFERED,
                    &[QUEUE_MAX, QUEUE_MAX, QUEUE_MAX],
                    irq,
                ),
                cid,
                maps: table,
                conns: HashMap::new(),
                tx_q: VecDeque::new(),
                rx_q: VecDeque::new(),
                event_pending: false,
                epoch: 1,
                host_port_next: HOST_PORT_BASE,
                stopped: false,
            }),
            wake: Condvar::new(),
            mem,
            irq,
        });
        let worker = if spawn {
            Some(
                thread::Builder::new()
                    .name("vmm-vsock".into())
                    .spawn({
                        let shared = Arc::clone(&shared);
                        move || worker_loop(shared)
                    })
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        let listen_identity = listener.as_ref().map(|(_, identity)| *identity);
        let control = if let Some((listener, identity)) = listener {
            let path = listen.clone().expect("listen path");
            let cleanup_path = path.clone();
            match thread::Builder::new().name("vmm-vsock-ctl".into()).spawn({
                let shared = Arc::clone(&shared);
                move || control_loop(listener, shared, path, identity)
            }) {
                Ok(handle) => Some(handle),
                Err(err) => {
                    shared.state.lock().unwrap().stopped = true;
                    shared.wake.notify_all();
                    if let Some(handle) = worker {
                        let _ = handle.join();
                    }
                    remove_socket_if_same(&cleanup_path, identity);
                    return Err(err.to_string());
                }
            }
        } else {
            None
        };
        Ok(Self {
            shared,
            worker,
            control,
            listen_path: listen,
            listen_identity,
            joined: false,
        })
    }

    fn stop(&mut self) {
        if self.joined {
            return;
        }
        self.joined = true;
        self.shared.state.lock().unwrap().stopped = true;
        self.shared.wake.notify_all();
        if let (Some(path), Some(identity)) = (&self.listen_path, self.listen_identity) {
            if socket_identity(path) == Some(identity) {
                let _ = UnixStream::connect(path);
            }
        }
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.control.take() {
            let _ = handle.join();
        }
        if let (Some(path), Some(identity)) = (&self.listen_path, self.listen_identity) {
            remove_socket_if_same(path, identity);
        }
    }

    #[cfg(test)]
    fn drain(&self) {
        for _ in 0..4 {
            self.shared.run_once();
        }
    }

    #[cfg(test)]
    fn connection_count(&self) -> usize {
        self.shared.state.lock().unwrap().conns.len()
    }

    #[cfg(test)]
    fn adopt(&self, stream: UnixStream) {
        self.shared.adopt_control_stream(stream);
    }
}

impl Drop for VirtioVsock {
    fn drop(&mut self) {
        self.stop();
    }
}

impl MmioDevice for VirtioVsock {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        let st = self.shared.state.lock().unwrap();
        if offset >= VIRTIO_MMIO_CONFIG {
            let start = (offset - VIRTIO_MMIO_CONFIG) as usize;
            let bytes = st.cid.to_le_bytes();
            let mut value = 0u64;
            for byte in 0..size.min(8) {
                let index = start + byte;
                if index < 8 {
                    value |= u64::from(bytes[index]) << (8 * byte);
                }
            }
            return value;
        }
        st.transport.read(offset)
    }

    fn write(&mut self, offset: u64, data: u64, size: usize) {
        let _ = size;
        if offset >= VIRTIO_MMIO_CONFIG {
            return;
        }
        let effect = {
            let mut st = self.shared.state.lock().unwrap();
            st.transport.write(offset, data)
        };
        match effect {
            WriteEffect::Notify { queue: 1 } => self.shared.harvest_tx(),
            WriteEffect::Notify { queue: 0 }
            | WriteEffect::QueueReady { queue: 0 }
            | WriteEffect::Notify { queue: 2 }
            | WriteEffect::QueueReady { queue: 2 } => self.shared.flush_guest(),
            WriteEffect::Reset => self.shared.reset_streams(),
            WriteEffect::InterruptAck { line_low } => signal(self.shared.irq, !line_low, false),
            _ => {}
        }
        self.shared.wake.notify_one();
    }

    fn shutdown(&mut self) -> Result<(), String> {
        let line = self
            .shared
            .state
            .lock()
            .unwrap()
            .transport
            .diagnostics_line();
        println!("[VirtIO-Vsock] {line}");
        self.stop();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;
    use std::sync::Arc;

    const MEM: u64 = 0x4000_0000;
    const MEM_LEN: usize = 0x4_0000;
    const RX_DESC: u64 = MEM + 0x2_0000;
    const RX_AVAIL: u64 = MEM + 0x2_1000;
    const RX_USED: u64 = MEM + 0x2_2000;
    const TX_DESC: u64 = MEM + 0x2_3000;
    const TX_AVAIL: u64 = MEM + 0x2_4000;
    const TX_USED: u64 = MEM + 0x2_5000;
    const EV_DESC: u64 = MEM + 0x2_6000;
    const EV_AVAIL: u64 = MEM + 0x2_7000;
    const EV_USED: u64 = MEM + 0x2_8000;
    const EV_BUF: u64 = MEM + 0x2_9000;
    const TX_BUF: u64 = MEM + 0x1000;
    const RX_BUF: u64 = MEM + 0x4000;
    const RX_STRIDE: u64 = 256;
    const QSIZE: u64 = 8;

    fn device(cid: u64, maps: Vec<(u32, PathBuf)>) -> (Arc<GuestMemory>, VirtioVsock) {
        let mem = Arc::new(GuestMemory::new_test(MEM, MEM_LEN));
        let dev = VirtioVsock::build(48, mem.clone(), cid, maps, None, false).unwrap();
        (mem, dev)
    }

    fn negotiate(dev: &mut VirtioVsock) {
        dev.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1, 4);
        dev.write(VIRTIO_MMIO_DRIVER_FEATURES, 1, 4);
        let status = (VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK) as u64;
        dev.write(VIRTIO_MMIO_STATUS, status, 4);
    }

    fn write_desc(mem: &GuestMemory, at: u64, addr: u64, len: u32, flags: u16) {
        mem.write_u64(at, addr).unwrap();
        mem.write_u32(at + 8, len).unwrap();
        mem.write_u16(at + 12, flags).unwrap();
        mem.write_u16(at + 14, 0).unwrap();
    }

    fn arm(dev: &mut VirtioVsock, queue: u32, desc: u64, avail: u64, used: u64, mem: &GuestMemory) {
        dev.write(VIRTIO_MMIO_QUEUE_SEL, queue as u64, 4);
        dev.write(VIRTIO_MMIO_QUEUE_NUM, QSIZE, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DESC_LOW, desc, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DRIVER_LOW, avail, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DEVICE_LOW, used, 4);
        mem.write_u16(avail, VRING_AVAIL_F_NO_INTERRUPT).unwrap();
        dev.write(VIRTIO_MMIO_QUEUE_READY, 1, 4);
    }

    fn arm_rings(mem: &GuestMemory, dev: &mut VirtioVsock) {
        for i in 0..QSIZE {
            write_desc(
                mem,
                RX_DESC + i * 16,
                RX_BUF + i * RX_STRIDE,
                RX_STRIDE as u32,
                VRING_DESC_F_WRITE,
            );
            mem.write_u16(RX_AVAIL + 4 + i * 2, i as u16).unwrap();
        }
        mem.write_u16(RX_AVAIL + 2, QSIZE as u16).unwrap();
        arm(dev, 0, RX_DESC, RX_AVAIL, RX_USED, mem);
        arm(dev, 1, TX_DESC, TX_AVAIL, TX_USED, mem);
    }

    fn send_tx(mem: &GuestMemory, dev: &mut VirtioVsock, seq: &mut u16, bytes: &[u8]) {
        write_desc(mem, TX_DESC, TX_BUF, bytes.len() as u32, 0);
        mem.write(TX_BUF, bytes).unwrap();
        let slot = (*seq as u64) % QSIZE;
        mem.write_u16(TX_AVAIL + 4 + slot * 2, 0).unwrap();
        *seq = seq.wrapping_add(1);
        mem.write_u16(TX_AVAIL + 2, *seq).unwrap();
        dev.write(VIRTIO_MMIO_QUEUE_NOTIFY, 1, 4);
    }

    fn packet(
        op: u16,
        src_port: u32,
        dst_port: u32,
        cid: u64,
        buf_alloc: u32,
        fwd: u32,
        payload: &[u8],
        type_: u16,
        flags: u32,
    ) -> Vec<u8> {
        Header {
            src_cid: cid,
            dst_cid: HOST_CID,
            src_port,
            dst_port,
            len: payload.len() as u32,
            type_,
            op,
            flags,
            buf_alloc,
            fwd_cnt: fwd,
        }
        .encode(payload)
    }

    fn take_rx(mem: &GuestMemory, seen: &mut u16) -> Vec<(Header, Vec<u8>)> {
        let used = mem.read_u16(RX_USED + 2).unwrap();
        let mut out = Vec::new();
        while *seen != used {
            let slot = RX_USED + 4 + 8 * ((*seen as u64) % QSIZE);
            let head = mem.read_u32(slot).unwrap() as u16;
            let len = mem.read_u32(slot + 4).unwrap() as usize;
            let addr = mem.read_u64(RX_DESC + head as u64 * 16).unwrap();
            let mut buf = vec![0u8; len];
            mem.read(addr, &mut buf).unwrap();
            let hdr = Header::parse(&buf).expect("rx header");
            let payload = buf[HDR_LEN..].to_vec();
            out.push((hdr, payload));
            *seen = seen.wrapping_add(1);
        }
        out
    }

    #[test]
    fn cid_rejects_reserved_collides_and_releases_the_file() {
        let dir = TestDir::new();
        let root = dir.path("cids");
        for cid in 0..3u64 {
            let err = CidLease::acquire(&root, cid).unwrap_err();
            assert!(err.contains("reserved"), "{err}");
        }
        for cid in [u32::MAX as u64, u32::MAX as u64 + 1, u64::MAX] {
            let err = CidLease::acquire(&root, cid).unwrap_err();
            assert!(err.contains("reserved"), "{err}");
        }
        assert!(root.join("0").exists() == false);
        let held = CidLease::acquire(&root, 3).unwrap();
        assert_eq!(held.cid(), 3);
        assert!(CidLease::acquire(&root, 3).unwrap_err().contains("in use"));
        let next = CidLease::allocate(&root).unwrap();
        assert_eq!(next.cid(), 4);
        drop(held);
        assert!(!root.join("3").exists());
        let again = CidLease::acquire(&root, 3).unwrap();
        assert_eq!(again.cid(), 3);
        fs::write(root.join("9"), "2147483647\n").unwrap();
        let recycled = CidLease::acquire(&root, 9).unwrap();
        assert_eq!(recycled.cid(), 9);
    }

    #[test]
    fn control_socket_preserves_live_listener_and_secures_stale_replacement() {
        let dir = TestDir::new();
        let path = dir.path("control.sock");
        let active = UnixListener::bind(&path).unwrap();
        let err = bind_control(&path).unwrap_err();
        assert!(err.contains("already in use"), "{err}");
        assert!(socket_identity(&path).is_some());
        drop(active);

        let (listener, identity) = bind_control(&path).unwrap();
        let mode = fs::symlink_metadata(&path).unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(socket_identity(&path), Some(identity));
        drop(listener);
        remove_socket_if_same(&path, identity);
        assert!(!path.exists());
    }

    #[test]
    fn packets_with_the_wrong_cid_cannot_mutate_an_existing_stream() {
        let (_mem, dev) = device(7, vec![]);
        let (host, _peer) = UnixStream::pair().unwrap();
        let mut state = dev.shared.state.lock().unwrap();
        state.conns.insert((10, 20), Conn::new(10, 20, host, false));
        let packet = TxPacket {
            epoch: state.epoch,
            hdr: Header {
                src_cid: 8,
                dst_cid: HOST_CID,
                src_port: 10,
                dst_port: 20,
                len: 3,
                type_: TYPE_STREAM,
                op: OP_RW,
                flags: 0,
                buf_alloc: 4096,
                fwd_cnt: 0,
            },
            payload: b"bad".to_vec(),
        };
        state.plan_tx(&packet);
        assert!(state.conns.get(&(10, 20)).unwrap().to_host.is_empty());
        assert_eq!(state.rx_q.back().unwrap().hdr.op, OP_RST);
    }

    #[test]
    fn config_exposes_guest_cid_with_stream_as_the_baseline() {
        let (_mem, mut dev) = device(7, vec![]);
        assert_eq!(dev.read(VIRTIO_MMIO_DEVICE_ID, 4), VIRTIO_DEV_VSOCK as u64);
        assert_eq!(dev.read(VIRTIO_MMIO_CONFIG, 8), 7);
        assert_eq!(dev.read(VIRTIO_MMIO_DEVICE_FEATURES, 4) & 0b11, 0);
        assert_eq!(
            dev.read(VIRTIO_MMIO_DEVICE_FEATURES, 4) & VIRTIO_RING_F_INDIRECT_DESC,
            0
        );
        dev.write(VIRTIO_MMIO_DEVICE_FEATURES_SEL, 1, 4);
        assert_eq!(dev.read(VIRTIO_MMIO_DEVICE_FEATURES, 4), 1);
        dev.write(VIRTIO_MMIO_QUEUE_SEL, 0, 4);
        assert_eq!(dev.read(VIRTIO_MMIO_QUEUE_NUM_MAX, 4), QUEUE_MAX as u64);
        dev.write(VIRTIO_MMIO_QUEUE_SEL, 2, 4);
        assert_eq!(dev.read(VIRTIO_MMIO_QUEUE_NUM_MAX, 4), QUEUE_MAX as u64);
        dev.write(VIRTIO_MMIO_QUEUE_SEL, 3, 4);
        assert_eq!(dev.read(VIRTIO_MMIO_QUEUE_NUM_MAX, 4), 0);
        assert!(VirtioVsock::build(1, _mem, 2, vec![], None, false).is_err());
    }

    #[test]
    fn short_header_needs_reset_and_leaves_rx_canary() {
        let (mem, mut dev) = device(5, vec![]);
        negotiate(&mut dev);
        arm_rings(&mem, &mut dev);
        let canary = [0xA5u8; 32];
        mem.write(RX_BUF, &canary).unwrap();
        let mut seq = 0u16;
        send_tx(&mem, &mut dev, &mut seq, &[0u8; 20]);
        dev.drain();
        assert_ne!(
            dev.read(VIRTIO_MMIO_STATUS, 4) & VIRTIO_STATUS_NEEDS_RESET as u64,
            0
        );
        assert_eq!(mem.read_u16(TX_USED + 2).unwrap(), 0);
        assert_eq!(mem.read_u16(RX_USED + 2).unwrap(), 0);
        let mut got = [0u8; 32];
        mem.read(RX_BUF, &mut got).unwrap();
        assert_eq!(got, canary);

        dev.write(VIRTIO_MMIO_STATUS, 0, 4);
        negotiate(&mut dev);
        arm_rings(&mem, &mut dev);
        mem.write(RX_BUF, &canary).unwrap();
        let mut bad = packet(OP_RW, 1, 2, 5, 1024, 0, &[], TYPE_STREAM, 0);
        bad[24..28].copy_from_slice(&8u32.to_le_bytes());
        assert_eq!(bad.len(), HDR_LEN);
        send_tx(&mem, &mut dev, &mut seq, &bad);
        dev.drain();
        assert_ne!(
            dev.read(VIRTIO_MMIO_STATUS, 4) & VIRTIO_STATUS_NEEDS_RESET as u64,
            0
        );
        mem.read(RX_BUF, &mut got).unwrap();
        assert_eq!(got, canary);
    }

    #[test]
    fn unmapped_or_seqpacket_request_replies_rst_without_connecting() {
        let dir = TestDir::new();
        let path = dir.path("unused.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (mem, mut dev) = device(5, vec![(5000, path)]);
        negotiate(&mut dev);
        arm_rings(&mem, &mut dev);
        let mut seq = 0u16;
        let mut seen = 0u16;
        send_tx(
            &mem,
            &mut dev,
            &mut seq,
            &packet(OP_REQUEST, 9, 5001, 5, 1024, 0, &[], TYPE_STREAM, 0),
        );
        dev.drain();
        assert!(listener.accept().unwrap_err().kind() == std::io::ErrorKind::WouldBlock);
        assert_eq!(dev.connection_count(), 0);
        let pkts = take_rx(&mem, &mut seen);
        assert_eq!(pkts.len(), 1);
        assert_eq!(pkts[0].0.op, OP_RST);
        assert_eq!(pkts[0].0.src_cid, HOST_CID);
        assert_eq!(pkts[0].0.dst_port, 9);
        assert_eq!(pkts[0].0.src_port, 5001);

        send_tx(
            &mem,
            &mut dev,
            &mut seq,
            &packet(OP_REQUEST, 9, 5000, 5, 1024, 0, &[], 2, 0),
        );
        dev.drain();
        assert!(listener.accept().unwrap_err().kind() == std::io::ErrorKind::WouldBlock);
        assert_eq!(dev.connection_count(), 0);
        let pkts = take_rx(&mem, &mut seen);
        assert_eq!(pkts.len(), 1);
        assert_eq!(pkts[0].0.op, OP_RST);
        assert_eq!(pkts[0].0.type_, 2);
    }

    #[test]
    fn mapped_stream_connects_responds_and_exchanges_rw() {
        let dir = TestDir::new();
        let path = dir.path("svc.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (mem, mut dev) = device(5, vec![(5000, path)]);
        negotiate(&mut dev);
        arm_rings(&mem, &mut dev);
        let mut seq = 0u16;
        let mut seen = 0u16;
        send_tx(
            &mem,
            &mut dev,
            &mut seq,
            &packet(OP_REQUEST, 100, 5000, 5, 4096, 0, &[], TYPE_STREAM, 0),
        );
        dev.drain();
        let (mut host, _) = listener.accept().unwrap();
        host.set_nonblocking(true).unwrap();
        let pkts = take_rx(&mem, &mut seen);
        assert_eq!(pkts.len(), 1);
        assert_eq!(pkts[0].0.op, OP_RESPONSE);
        assert_eq!(pkts[0].0.src_cid, HOST_CID);
        assert_eq!(pkts[0].0.dst_cid, 5);
        assert_eq!(pkts[0].0.src_port, 5000);
        assert_eq!(pkts[0].0.dst_port, 100);
        assert_eq!(pkts[0].0.buf_alloc, BUF_ALLOC);

        send_tx(
            &mem,
            &mut dev,
            &mut seq,
            &packet(OP_RW, 100, 5000, 5, 4096, 0, b"hello", TYPE_STREAM, 0),
        );
        dev.drain();
        let mut got = [0u8; 8];
        let n = host.read(&mut got).unwrap();
        assert_eq!(&got[..n], b"hello");

        host.write_all(b"world").unwrap();
        dev.drain();
        let pkts = take_rx(&mem, &mut seen);
        assert!(
            pkts.iter()
                .any(|pkt| pkt.0.op == OP_CREDIT_UPDATE && pkt.0.fwd_cnt >= 5)
        );
        let rw = pkts.iter().find(|pkt| pkt.0.op == OP_RW).expect("rw");
        assert_eq!(rw.1, b"world");
    }

    #[test]
    fn credit_limits_host_to_guest_until_update() {
        let dir = TestDir::new();
        let path = dir.path("credit.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (mem, mut dev) = device(5, vec![(7, path)]);
        negotiate(&mut dev);
        arm_rings(&mem, &mut dev);
        let mut seq = 0u16;
        let mut seen = 0u16;
        send_tx(
            &mem,
            &mut dev,
            &mut seq,
            &packet(OP_REQUEST, 3, 7, 5, 4, 0, &[], TYPE_STREAM, 0),
        );
        dev.drain();
        let (mut host, _) = listener.accept().unwrap();
        host.set_nonblocking(true).unwrap();
        let _ = take_rx(&mem, &mut seen);
        host.write_all(b"abcdefgh").unwrap();
        dev.drain();
        let pkts = take_rx(&mem, &mut seen);
        assert_eq!(pkts.len(), 1);
        assert_eq!(pkts[0].0.op, OP_RW);
        assert_eq!(pkts[0].1, b"abcd");
        send_tx(
            &mem,
            &mut dev,
            &mut seq,
            &packet(OP_CREDIT_UPDATE, 3, 7, 5, 4, 4, &[], TYPE_STREAM, 0),
        );
        dev.drain();
        let pkts = take_rx(&mem, &mut seen);
        assert!(pkts.iter().any(|p| p.0.op == OP_RW && p.1 == b"efgh"));
    }

    #[test]
    fn guest_shutdown_send_half_closes_unix_and_keeps_the_other_way() {
        let dir = TestDir::new();
        let path = dir.path("half.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (mem, mut dev) = device(5, vec![(8, path)]);
        negotiate(&mut dev);
        arm_rings(&mem, &mut dev);
        let mut seq = 0u16;
        let mut seen = 0u16;
        send_tx(
            &mem,
            &mut dev,
            &mut seq,
            &packet(OP_REQUEST, 4, 8, 5, 1024, 0, &[], TYPE_STREAM, 0),
        );
        dev.drain();
        let (mut host, _) = listener.accept().unwrap();
        host.set_nonblocking(true).unwrap();
        let _ = take_rx(&mem, &mut seen);
        send_tx(
            &mem,
            &mut dev,
            &mut seq,
            &packet(
                OP_SHUTDOWN,
                4,
                8,
                5,
                1024,
                0,
                &[],
                TYPE_STREAM,
                SHUTDOWN_SEND,
            ),
        );
        dev.drain();
        let mut buf = [0u8; 4];
        assert_eq!(host.read(&mut buf).unwrap(), 0);
        host.write_all(b"still").unwrap();
        dev.drain();
        let pkts = take_rx(&mem, &mut seen);
        assert_eq!(pkts.len(), 1);
        assert_eq!(pkts[0].1, b"still");
        assert_eq!(dev.connection_count(), 1);
        send_tx(
            &mem,
            &mut dev,
            &mut seq,
            &packet(
                OP_SHUTDOWN,
                4,
                8,
                5,
                1024,
                0,
                &[],
                TYPE_STREAM,
                SHUTDOWN_RCV,
            ),
        );
        dev.drain();
        assert_eq!(dev.connection_count(), 0);
    }

    #[test]
    fn reset_drops_connections_and_queues_transport_reset_event() {
        let dir = TestDir::new();
        let path = dir.path("reset.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (mem, mut dev) = device(5, vec![(9, path)]);
        negotiate(&mut dev);
        arm_rings(&mem, &mut dev);
        let mut seq = 0u16;
        send_tx(
            &mem,
            &mut dev,
            &mut seq,
            &packet(OP_REQUEST, 1, 9, 5, 1024, 0, &[], TYPE_STREAM, 0),
        );
        dev.drain();
        let (mut host, _) = listener.accept().unwrap();
        host.set_nonblocking(true).unwrap();
        assert_eq!(dev.connection_count(), 1);
        dev.write(VIRTIO_MMIO_STATUS, 0, 4);
        assert_eq!(dev.connection_count(), 0);
        let mut buf = [0u8; 4];
        assert_eq!(host.read(&mut buf).unwrap(), 0);
        negotiate(&mut dev);
        write_desc(&mem, EV_DESC, EV_BUF, 4, VRING_DESC_F_WRITE);
        mem.write_u32(EV_BUF, 0xffff_ffff).unwrap();
        mem.write_u16(EV_AVAIL + 4, 0).unwrap();
        mem.write_u16(EV_AVAIL + 2, 1).unwrap();
        arm(&mut dev, 2, EV_DESC, EV_AVAIL, EV_USED, &mem);
        assert_eq!(mem.read_u32(EV_BUF).unwrap(), EVENT_RESET);
        assert_eq!(mem.read_u16(EV_USED + 2).unwrap(), 1);
    }

    #[test]
    fn control_connect_sends_request_and_carries_bytes() {
        let (mem, mut dev) = device(6, vec![]);
        negotiate(&mut dev);
        arm_rings(&mem, &mut dev);
        let (host, mut peer) = UnixStream::pair().unwrap();
        peer.set_nonblocking(true).unwrap();
        peer.write_all(b"connect 0\n").unwrap();
        dev.adopt(host);
        let mut err = [0u8; 64];
        let n = peer.read(&mut err).unwrap();
        assert!(std::str::from_utf8(&err[..n]).unwrap().starts_with("ERR"));
        assert_eq!(dev.connection_count(), 0);

        let (host, mut peer) = UnixStream::pair().unwrap();
        peer.write_all(b"connect 40\n").unwrap();
        dev.adopt(host);
        let mut ok = [0u8; 16];
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let n = peer.read(&mut ok).unwrap();
        assert_eq!(&ok[..n], b"OK\n");
        let mut seen = 0u16;
        let pkts = take_rx(&mem, &mut seen);
        assert_eq!(pkts.len(), 1);
        assert_eq!(pkts[0].0.op, OP_REQUEST);
        assert_eq!(pkts[0].0.src_cid, HOST_CID);
        assert_eq!(pkts[0].0.dst_cid, 6);
        assert_eq!(pkts[0].0.dst_port, 40);
        let host_port = pkts[0].0.src_port;
        let mut seq = 0u16;
        send_tx(
            &mem,
            &mut dev,
            &mut seq,
            &packet(OP_RESPONSE, 40, host_port, 6, 1024, 0, &[], TYPE_STREAM, 0),
        );
        dev.drain();
        peer.set_nonblocking(true).unwrap();
        peer.write_all(b"ping").unwrap();
        dev.drain();
        let pkts = take_rx(&mem, &mut seen);
        assert!(pkts.iter().any(|p| p.0.op == OP_RW && p.1 == b"ping"));
    }

    #[test]
    fn rw_past_credit_resets_the_connection() {
        let dir = TestDir::new();
        let path = dir.path("over.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        let (mem, mut dev) = device(5, vec![(11, path)]);
        negotiate(&mut dev);
        arm_rings(&mem, &mut dev);
        let mut seq = 0u16;
        let mut seen = 0u16;
        send_tx(
            &mem,
            &mut dev,
            &mut seq,
            &packet(OP_REQUEST, 2, 11, 5, 1024, 0, &[], TYPE_STREAM, 0),
        );
        dev.drain();
        let (host, _) = listener.accept().unwrap();
        host.set_nonblocking(true).unwrap();
        let _ = take_rx(&mem, &mut seen);
        // Queue a full credit window before the worker forwards any of it.
        // The next byte has no receive credit and resets the stream.
        let fill = vec![0x22u8; 4096];
        let mut queued = 0usize;
        while queued < BUF_ALLOC as usize {
            let n = fill.len().min(BUF_ALLOC as usize - queued);
            send_tx(
                &mem,
                &mut dev,
                &mut seq,
                &packet(OP_RW, 2, 11, 5, 1024, 0, &fill[..n], TYPE_STREAM, 0),
            );
            queued += n;
        }
        send_tx(
            &mem,
            &mut dev,
            &mut seq,
            &packet(OP_RW, 2, 11, 5, 1024, 0, &[0x33], TYPE_STREAM, 0),
        );
        dev.drain();
        let pkts = take_rx(&mem, &mut seen);
        assert!(pkts.iter().any(|p| p.0.op == OP_RST), "{pkts:?}");
        assert_eq!(dev.connection_count(), 0);
    }
}
