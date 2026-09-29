//! Frame protocol between an unprivileged VMM and the long-running vmnet service.
//!
//! No `vmnet_*` calls live here. The service and `--net vmnet-proxy` share these
//! types; `--net vmnet` stays the in-process backend.

use std::collections::{HashSet, VecDeque};
use std::io::{self, ErrorKind, Read};
use std::net::Ipv4Addr;
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

pub const MAGIC: &[u8; 8] = b"VMMNET\x01\x00";
const _: () = assert!(MAGIC.len() == 8);

pub const VERSION: u32 = 1;
pub const MAX_FRAME: u32 = 2048;
pub const MIN_FRAME: u32 = 14;
pub const MIN_NEGOTIATED_FRAME: u32 = 1514;
pub const MAX_DISCARD_FRAME: u32 = 65536;
pub const MAX_CLIENTS: usize = 16;
pub const CLIENT_OUTQ: usize = 64;
pub const CLIENT_HELLO_LEN: usize = 24;
pub const SERVER_HELLO_LEN: usize = 20;

pub const STATUS_OK: u32 = 0;
pub const STATUS_DUPLICATE_MAC: u32 = 1;
pub const STATUS_UNSUPPORTED_VERSION: u32 = 2;
pub const STATUS_REJECTED_MAC: u32 = 3;
pub const STATUS_BAD_MAX_FRAME: u32 = 4;
pub const STATUS_UNAUTHORIZED: u32 = 5;
pub const STATUS_TOO_MANY_CLIENTS: u32 = 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientHello {
    pub version: u32,
    pub mac: [u8; 6],
    pub flags: u16,
    pub max_frame: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ServerHello {
    pub version: u32,
    pub status: u32,
    pub max_frame: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeError {
    Truncated,
    BadMagic,
}

impl ClientHello {
    pub fn encode(self) -> [u8; CLIENT_HELLO_LEN] {
        let mut out = [0u8; CLIENT_HELLO_LEN];
        out[..8].copy_from_slice(MAGIC);
        out[8..12].copy_from_slice(&self.version.to_le_bytes());
        out[12..18].copy_from_slice(&self.mac);
        out[18..20].copy_from_slice(&self.flags.to_le_bytes());
        out[20..24].copy_from_slice(&self.max_frame.to_le_bytes());
        out
    }
}

impl ServerHello {
    pub fn new(status: u32, max_frame: u32) -> Self {
        Self {
            version: VERSION,
            status,
            max_frame,
        }
    }

    pub fn encode(self) -> [u8; SERVER_HELLO_LEN] {
        let mut out = [0u8; SERVER_HELLO_LEN];
        out[..8].copy_from_slice(MAGIC);
        out[8..12].copy_from_slice(&self.version.to_le_bytes());
        out[12..16].copy_from_slice(&self.status.to_le_bytes());
        out[16..20].copy_from_slice(&self.max_frame.to_le_bytes());
        out
    }
}

pub fn decode_client_hello(buf: &[u8]) -> Result<ClientHello, DecodeError> {
    if buf.len() < CLIENT_HELLO_LEN {
        return Err(DecodeError::Truncated);
    }
    if &buf[..8] != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    Ok(ClientHello {
        version: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
        mac: buf[12..18].try_into().unwrap(),
        flags: u16::from_le_bytes(buf[18..20].try_into().unwrap()),
        max_frame: u32::from_le_bytes(buf[20..24].try_into().unwrap()),
    })
}

pub fn decode_server_hello(buf: &[u8]) -> Result<ServerHello, DecodeError> {
    if buf.len() < SERVER_HELLO_LEN {
        return Err(DecodeError::Truncated);
    }
    if &buf[..8] != MAGIC {
        return Err(DecodeError::BadMagic);
    }
    Ok(ServerHello {
        version: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
        status: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
        max_frame: u32::from_le_bytes(buf[16..20].try_into().unwrap()),
    })
}

pub fn mac_acceptable(mac: &[u8; 6]) -> bool {
    *mac != [0u8; 6] && mac[0] & 0x01 == 0
}

/// MAC table. Duplicate lookup happens before the capacity check.
#[derive(Debug)]
pub struct Registry {
    owner: u32,
    macs: HashSet<[u8; 6]>,
}

impl Registry {
    pub fn new(owner: u32) -> Self {
        Self {
            owner,
            macs: HashSet::new(),
        }
    }

    pub fn owner(&self) -> u32 {
        self.owner
    }

    pub fn len(&self) -> usize {
        self.macs.len()
    }

    /// Used by the protocol tests. The service admits through `admit`.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.macs.is_empty()
    }

    /// Used by the protocol tests. The service admits through `admit`.
    #[allow(dead_code)]
    pub fn contains(&self, mac: &[u8; 6]) -> bool {
        self.macs.contains(mac)
    }

    pub fn release(&mut self, mac: &[u8; 6]) {
        self.macs.remove(mac);
    }

    pub fn admission(&self, peer_uid: u32, hello: &ClientHello) -> u32 {
        if peer_uid != self.owner {
            return STATUS_UNAUTHORIZED;
        }
        if hello.version != VERSION || hello.flags != 0 {
            return STATUS_UNSUPPORTED_VERSION;
        }
        if !mac_acceptable(&hello.mac) {
            return STATUS_REJECTED_MAC;
        }
        if !(MIN_NEGOTIATED_FRAME..=MAX_FRAME).contains(&hello.max_frame) {
            return STATUS_BAD_MAX_FRAME;
        }
        if self.macs.contains(&hello.mac) {
            return STATUS_DUPLICATE_MAC;
        }
        if self.macs.len() >= MAX_CLIENTS {
            return STATUS_TOO_MANY_CLIENTS;
        }
        STATUS_OK
    }

    /// Insert on `STATUS_OK`. Negotiated `max_frame` is 0 for every other status.
    pub fn admit(&mut self, peer_uid: u32, hello: &ClientHello) -> ServerHello {
        let status = self.admission(peer_uid, hello);
        let max_frame = if status == STATUS_OK {
            self.macs.insert(hello.mac);
            hello.max_frame.min(MAX_FRAME)
        } else {
            0
        };
        ServerHello::new(status, max_frame)
    }
}

/// Full 24-byte hello, or a short read that must close with no status byte.
pub enum HelloResult {
    Incomplete,
    Reply(ServerHello),
}

pub fn server_hello_for_raw(reg: &mut Registry, peer_uid: u32, raw: &[u8]) -> HelloResult {
    if raw.len() < CLIENT_HELLO_LEN {
        return HelloResult::Incomplete;
    }
    if peer_uid != reg.owner() {
        return HelloResult::Reply(ServerHello::new(STATUS_UNAUTHORIZED, 0));
    }
    let hello = match decode_client_hello(&raw[..CLIENT_HELLO_LEN]) {
        Ok(hello) => hello,
        Err(DecodeError::BadMagic) => {
            return HelloResult::Reply(ServerHello::new(STATUS_UNSUPPORTED_VERSION, 0));
        }
        Err(DecodeError::Truncated) => return HelloResult::Incomplete,
    };
    HelloResult::Reply(reg.admit(peer_uid, &hello))
}

/// Read `buf` or fail. macOS rejects `SO_RCVTIMEO` on `AF_UNIX` (`EINVAL`), so the
/// handshake deadline is `poll`, not `UnixStream::set_read_timeout`.
pub fn read_exact_deadline(
    stream: &mut std::os::unix::net::UnixStream,
    buf: &mut [u8],
    timeout: Duration,
) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    let mut filled = 0;
    while filled < buf.len() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(io::Error::new(ErrorKind::TimedOut, "handshake timed out"));
        }
        let mut pfd = libc::pollfd {
            fd: stream.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let wait_ms = i32::try_from(left.as_millis()).unwrap_or(i32::MAX);
        let rc = unsafe { libc::poll(&mut pfd, 1, wait_ms) };
        if rc == 0 {
            return Err(io::Error::new(ErrorKind::TimedOut, "handshake timed out"));
        }
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        match stream.read(&mut buf[filled..]) {
            Ok(0) => {
                return Err(io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "handshake closed",
                ))
            }
            Ok(n) => filled += n,
            Err(err) if err.kind() == ErrorKind::Interrupted => continue,
            Err(err) if err.kind() == ErrorKind::WouldBlock => continue,
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

pub fn encode_record(frame: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + frame.len());
    out.extend_from_slice(&(frame.len() as u32).to_le_bytes());
    out.extend_from_slice(frame);
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Ingress {
    Frame(Vec<u8>),
    Runt,
    Oversize,
    Close,
}

/// Length class is decided before any source-MAC check.
#[derive(Debug)]
struct RecordReader {
    buf: Vec<u8>,
    max_frame: usize,
    frame_runt: u64,
    frame_oversize: u64,
    closed: bool,
}

impl RecordReader {
    fn new(max_frame: usize) -> Self {
        Self {
            buf: Vec::new(),
            max_frame,
            frame_runt: 0,
            frame_oversize: 0,
            closed: false,
        }
    }

    fn push(&mut self, data: &[u8]) -> Vec<Ingress> {
        if self.closed {
            return Vec::new();
        }
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            if self.buf.len() < 4 {
                break;
            }
            let mut lenb = [0u8; 4];
            lenb.copy_from_slice(&self.buf[..4]);
            let n = u32::from_le_bytes(lenb) as usize;
            if n == 0 || n > MAX_DISCARD_FRAME as usize {
                self.closed = true;
                self.buf.clear();
                out.push(Ingress::Close);
                break;
            }
            if self.buf.len() < 4 + n {
                break;
            }
            if n < MIN_FRAME as usize {
                self.frame_runt += 1;
                self.buf.drain(..4 + n);
                out.push(Ingress::Runt);
            } else if n > self.max_frame {
                self.frame_oversize += 1;
                self.buf.drain(..4 + n);
                out.push(Ingress::Oversize);
            } else {
                let frame = self.buf[4..4 + n].to_vec();
                self.buf.drain(..4 + n);
                out.push(Ingress::Frame(frame));
            }
        }
        out
    }

    fn eof(&mut self) -> Ingress {
        self.closed = true;
        self.buf.clear();
        Ingress::Close
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientRecord {
    /// Free the MAC. The stream is no longer framed, or the peer hung up.
    Close,
    /// `frame_runt` or `frame_oversize`. Do not write, switch, or count `tx_spoof`.
    Discard,
    Forward(Vec<u8>),
    Spoof,
}

/// Service-to-VMM records. Length classes match the service, with no source check:
/// frames from `vmnet_read` are not the guest's own MAC.
#[derive(Debug)]
pub struct Downlink {
    reader: RecordReader,
}

impl Downlink {
    pub fn new(max_frame: usize) -> Self {
        Self {
            reader: RecordReader::new(max_frame),
        }
    }

    pub fn push(&mut self, data: &[u8]) -> Vec<ClientRecord> {
        self.reader
            .push(data)
            .into_iter()
            .map(|event| match event {
                Ingress::Close => ClientRecord::Close,
                Ingress::Runt | Ingress::Oversize => ClientRecord::Discard,
                Ingress::Frame(frame) => ClientRecord::Forward(frame),
            })
            .collect()
    }
}

/// One attached client's inbound records. Source check runs only on a real frame.
#[derive(Debug)]
pub struct ClientIngress {
    reader: RecordReader,
    mac: [u8; 6],
    pub tx_spoof: u64,
}

impl ClientIngress {
    pub fn new(mac: [u8; 6], max_frame: usize) -> Self {
        Self {
            reader: RecordReader::new(max_frame),
            mac,
            tx_spoof: 0,
        }
    }

    pub fn frame_runt(&self) -> u64 {
        self.reader.frame_runt
    }

    pub fn frame_oversize(&self) -> u64 {
        self.reader.frame_oversize
    }

    pub fn push(&mut self, data: &[u8]) -> Vec<ClientRecord> {
        self.reader
            .push(data)
            .into_iter()
            .map(|event| self.disposition(event))
            .collect()
    }

    pub fn eof(&mut self) -> ClientRecord {
        let event = self.reader.eof();
        self.disposition(event)
    }

    fn disposition(&mut self, event: Ingress) -> ClientRecord {
        match event {
            Ingress::Close => ClientRecord::Close,
            Ingress::Runt | Ingress::Oversize => ClientRecord::Discard,
            Ingress::Frame(frame) => {
                if frame.get(6..12) == Some(&self.mac) {
                    ClientRecord::Forward(frame)
                } else {
                    self.tx_spoof += 1;
                    ClientRecord::Spoof
                }
            }
        }
    }
}

/// Per-client send queue. `EAGAIN` keeps the partial record; the 65th frame is dropped whole.
#[derive(Debug)]
pub struct OutQueue {
    outq: VecDeque<Vec<u8>>,
    outbound: Vec<u8>,
    pub drop_slow_client: u64,
}

impl OutQueue {
    pub fn new() -> Self {
        Self {
            outq: VecDeque::new(),
            outbound: Vec::new(),
            drop_slow_client: 0,
        }
    }

    pub fn queued(&self) -> usize {
        self.outq.len()
    }

    pub fn outbound(&self) -> &[u8] {
        &self.outbound
    }

    /// `false` when this frame was dropped before any of its bytes were queued.
    pub fn enqueue_frame(&mut self, frame: &[u8]) -> bool {
        if self.outq.len() >= CLIENT_OUTQ {
            self.drop_slow_client += 1;
            return false;
        }
        self.outq.push_back(encode_record(frame));
        true
    }

    pub fn flush_outbound(
        &mut self,
        write: &mut dyn FnMut(&[u8]) -> io::Result<usize>,
    ) -> io::Result<()> {
        loop {
            if self.outbound.is_empty() {
                match self.outq.pop_front() {
                    Some(record) => self.outbound = record,
                    None => return Ok(()),
                }
            }
            match write(&self.outbound) {
                Ok(0) => {
                    return Err(io::Error::new(ErrorKind::WriteZero, "peer closed"));
                }
                Ok(n) if n > self.outbound.len() => {
                    return Err(io::Error::new(
                        ErrorKind::InvalidInput,
                        "write reported more bytes than the buffer",
                    ));
                }
                Ok(n) => {
                    self.outbound.drain(..n);
                }
                Err(err) if err.kind() == ErrorKind::Interrupted => continue,
                Err(err) if err.kind() == ErrorKind::WouldBlock => return Ok(()),
                Err(err) => return Err(err),
            }
        }
    }
}

impl Default for OutQueue {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DhcpDnsScope {
    /// In-process `--net vmnet` and bridge: only `192.168.x.1`.
    GatewayOnly,
    /// Service path: gateway addresses and anything in `192.168.18.0/24`.
    SharedBridge,
    /// Addresses inside one prefix. `network` is already masked.
    Network { network: u32, prefix: u32 },
}

impl DhcpDnsScope {
    fn matches(self, ip: &[u8]) -> bool {
        let gateway = ip.len() == 4 && ip[0] == 192 && ip[1] == 168 && ip[3] == 1;
        match self {
            DhcpDnsScope::GatewayOnly => gateway,
            DhcpDnsScope::SharedBridge => {
                gateway || (ip[0] == 192 && ip[1] == 168 && ip[2] == 18)
            }
            DhcpDnsScope::Network { network, prefix } => {
                if ip.len() != 4 || prefix == 0 || prefix > 32 {
                    return false;
                }
                let addr = u32::from_be_bytes([ip[0], ip[1], ip[2], ip[3]]);
                let mask = if prefix == 32 {
                    u32::MAX
                } else {
                    u32::MAX << (32 - prefix)
                };
                addr & mask == network & mask
            }
        }
    }
}

/// Rewrite DHCP option 6 in place. Returns whether any byte changed.
///
/// The walk stops at the IPv4 total length and the UDP length. It does not
/// insert options or change those lengths.
pub fn rewrite_dhcp_dns(buf: &mut [u8], scope: DhcpDnsScope, resolvers: &[Ipv4Addr]) -> bool {
    if resolvers.is_empty() || buf.len() < 14 + 20 + 8 + 240 {
        return false;
    }
    if buf.get(12..14) != Some(&[0x08, 0x00]) {
        return false;
    }
    let ver_ihl = buf[14];
    if ver_ihl >> 4 != 4 {
        return false;
    }
    let ihl = (ver_ihl & 0x0f) as usize * 4;
    if ihl < 20 || buf.len() < 14 + ihl + 8 {
        return false;
    }
    if buf[14 + 9] != 17 {
        return false;
    }
    let total_len = u16::from_be_bytes([buf[16], buf[17]]) as usize;
    if total_len < ihl + 8 {
        return false;
    }
    let ip_end = 14 + total_len;
    if ip_end > buf.len() {
        return false;
    }
    let frag = u16::from_be_bytes([buf[20], buf[21]]);
    if frag & 0x3fff != 0 {
        return false;
    }
    let udp = 14 + ihl;
    let src = u16::from_be_bytes([buf[udp], buf[udp + 1]]);
    let dst = u16::from_be_bytes([buf[udp + 2], buf[udp + 3]]);
    if src != 67 || dst != 68 {
        return false;
    }
    let udp_len = u16::from_be_bytes([buf[udp + 4], buf[udp + 5]]) as usize;
    if udp_len < 8 + 240 || udp + udp_len > ip_end {
        return false;
    }
    let bootp = udp + 8;
    if buf[bootp] != 2 {
        return false;
    }
    let cookie = bootp + 236;
    if buf.get(cookie..cookie + 4) != Some(&[99, 130, 83, 99]) {
        return false;
    }
    let udp_end = udp + udp_len;
    let mut opt = cookie + 4;
    let mut modified = false;
    let mut slot_i = 0usize;
    while opt < udp_end {
        let code = buf[opt];
        if code == 0 {
            opt += 1;
            continue;
        }
        if code == 255 {
            break;
        }
        if opt + 1 >= udp_end {
            break;
        }
        let len = buf[opt + 1] as usize;
        let val = opt + 2;
        if val + len > udp_end {
            break;
        }
        if code == 6 {
            let mut chunk = 0;
            while chunk + 4 <= len {
                let slot = &mut buf[val + chunk..val + chunk + 4];
                if scope.matches(slot) {
                    let repl = resolvers[slot_i % resolvers.len()].octets();
                    slot_i += 1;
                    if slot != repl {
                        slot.copy_from_slice(&repl);
                        modified = true;
                    }
                }
                chunk += 4;
            }
        }
        opt = val + len;
    }
    if modified {
        buf[udp + 6] = 0;
        buf[udp + 7] = 0;
    }
    modified
}

pub fn select_dns_resolvers(text: &str) -> Vec<Ipv4Addr> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let Some(start) = lines.iter().position(|line| *line == "DNS configuration") else {
        return fallback_resolvers();
    };
    let end = lines
        .iter()
        .skip(start + 1)
        .position(|line| line.starts_with("DNS configuration (for scoped queries)"))
        .map(|offset| start + 1 + offset)
        .unwrap_or(lines.len());
    let mut chosen = Vec::new();
    for block in resolver_blocks(&lines[start + 1..end]) {
        if skip_resolver_block(&block) {
            continue;
        }
        let if_index = block.iter().any(|(key, _)| *key == "if_index");
        for (key, value) in &block {
            if !key.starts_with("nameserver") {
                continue;
            }
            let Some(addr) = value.split_whitespace().next().and_then(|tok| tok.parse().ok())
            else {
                continue;
            };
            if !keep_resolver(addr, if_index) || chosen.contains(&addr) {
                continue;
            }
            chosen.push(addr);
            if chosen.len() == 3 {
                return chosen;
            }
        }
    }
    if chosen.is_empty() {
        fallback_resolvers()
    } else {
        chosen
    }
}

fn fallback_resolvers() -> Vec<Ipv4Addr> {
    vec![Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(8, 8, 8, 8)]
}

fn resolver_blocks<'a>(lines: &[&'a str]) -> Vec<Vec<(&'a str, &'a str)>> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<(&str, &str)>> = None;
    for line in lines {
        if line.starts_with("resolver #") {
            if let Some(block) = current.take() {
                blocks.push(block);
            }
            current = Some(Vec::new());
            continue;
        }
        let Some(block) = current.as_mut() else {
            continue;
        };
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        block.push((key.trim(), value.trim()));
    }
    if let Some(block) = current {
        blocks.push(block);
    }
    blocks
}

fn skip_resolver_block(block: &[(&str, &str)]) -> bool {
    for (key, value) in block {
        if *key == "flags" && value.split(',').any(|flag| flag.trim() == "Scoped") {
            return true;
        }
        if *key == "domain" && *value != "" && *value != "." {
            return true;
        }
    }
    false
}

fn keep_resolver(addr: Ipv4Addr, if_index: bool) -> bool {
    if addr.is_unspecified()
        || addr.is_loopback()
        || addr.is_multicast()
        || addr.is_broadcast()
        || is_link_local(addr)
        || is_bridge_subnet(addr)
    {
        return false;
    }
    if if_index && !globally_routable(addr) {
        return false;
    }
    true
}

fn is_link_local(addr: Ipv4Addr) -> bool {
    let o = addr.octets();
    o[0] == 169 && o[1] == 254
}

fn is_bridge_subnet(addr: Ipv4Addr) -> bool {
    let o = addr.octets();
    o[0] == 192 && o[1] == 168 && o[2] == 18
}

fn is_rfc1918(addr: Ipv4Addr) -> bool {
    match addr.octets() {
        [10, ..] => true,
        [172, second, ..] if (16..32).contains(&second) => true,
        [192, 168, ..] => true,
        _ => false,
    }
}

fn is_cgnat(addr: Ipv4Addr) -> bool {
    let o = addr.octets();
    o[0] == 100 && (64..128).contains(&o[1])
}

fn globally_routable(addr: Ipv4Addr) -> bool {
    !addr.is_unspecified()
        && !addr.is_loopback()
        && !addr.is_multicast()
        && !addr.is_broadcast()
        && !is_link_local(addr)
        && !is_rfc1918(addr)
        && !is_cgnat(addr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::thread;
    use std::time::Duration;

    fn mac(last: u8) -> [u8; 6] {
        [0x52, 0x54, 0x00, 0x12, 0x34, last]
    }

    fn hello(mac: [u8; 6], flags: u16, max_frame: u32) -> ClientHello {
        ClientHello {
            version: VERSION,
            mac,
            flags,
            max_frame,
        }
    }

    fn read_downlink_frame(stream: &mut UnixStream, max_frame: usize) -> Vec<u8> {
        let mut link = Downlink::new(max_frame);
        let mut buf = [0u8; 256];
        for _ in 0..8 {
            let n = stream.read(&mut buf).unwrap();
            assert_ne!(n, 0, "downlink closed");
            for record in link.push(&buf[..n]) {
                if let ClientRecord::Forward(frame) = record {
                    return frame;
                }
            }
        }
        panic!("downlink produced no frame");
    }

    fn frame_from(src: [u8; 6]) -> Vec<u8> {
        let mut frame = vec![0u8; 14];
        frame[0] = 0xff;
        frame[5] = 0xff;
        frame[6..12].copy_from_slice(&src);
        frame[12] = 0x08;
        frame[13] = 0x00;
        frame
    }

    #[test]
    fn magic_is_8_bytes_and_hellos_are_fixed_size() {
        assert_eq!(MAGIC.len(), 8);
        assert_eq!(MAGIC, b"VMMNET\x01\x00");
        assert_eq!(hello(mac(0x56), 0, MAX_FRAME).encode().len(), 24);
        assert_eq!(ServerHello::new(STATUS_OK, MAX_FRAME).encode().len(), 20);
        let encoded = hello(mac(0x56), 0, MAX_FRAME).encode();
        assert_eq!(&encoded[..8], MAGIC);
        assert_eq!(encoded[7], 0);
    }

    #[test]
    fn nonzero_flags_rejected() {
        let mut reg = Registry::new(10);
        let bad = hello(mac(0x56), 1, MAX_FRAME);
        let raw = bad.encode();
        let decoded = decode_client_hello(&raw).unwrap();
        assert_eq!(decoded.flags, 1);
        assert_eq!(reg.admission(10, &decoded), STATUS_UNSUPPORTED_VERSION);
        assert!(reg.admit(10, &decoded).max_frame == 0);
        assert!(reg.is_empty());
        let good = hello(mac(0x56), 0, MAX_FRAME);
        assert_eq!(decode_client_hello(&good.encode()).unwrap().flags, 0);
        assert_eq!(reg.admit(10, &good).status, STATUS_OK);
    }

    #[test]
    fn hello_roundtrip_and_frame_roundtrip() {
        let dir = crate::test_support::TestDir::new();
        let path = dir.path("vmnet.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut raw = [0u8; CLIENT_HELLO_LEN];
            stream.read_exact(&mut raw).unwrap();
            let mut reg = Registry::new(7);
            let HelloResult::Reply(reply) = server_hello_for_raw(&mut reg, 7, &raw) else {
                panic!("short hello");
            };
            assert_eq!(reply.status, STATUS_OK);
            stream.write_all(&reply.encode()).unwrap();
            let mut ingress = ClientIngress::new(mac(0x56), MAX_FRAME as usize);
            let mut buf = [0u8; 64];
            let n = stream.read(&mut buf).unwrap();
            let got = ingress.push(&buf[..n]);
            assert_eq!(got, vec![ClientRecord::Forward(frame_from(mac(0x56)))]);
            let back = frame_from(mac(0x01));
            stream.write_all(&encode_record(&back)).unwrap();
            back
        });
        let mut client = UnixStream::connect(&path).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        client
            .write_all(&hello(mac(0x56), 0, MAX_FRAME).encode())
            .unwrap();
        let mut raw = [0u8; SERVER_HELLO_LEN];
        client.read_exact(&mut raw).unwrap();
        let reply = decode_server_hello(&raw).unwrap();
        assert_eq!(reply.status, STATUS_OK);
        assert_eq!(reply.max_frame, MAX_FRAME);
        let sent = frame_from(mac(0x56));
        client.write_all(&encode_record(&sent)).unwrap();
        let got = read_downlink_frame(&mut client, reply.max_frame as usize);
        let expected = server.join().unwrap();
        assert_eq!(got, expected);
        assert_eq!(expected.len(), 14);
    }

    #[test]
    fn duplicate_mac_is_rejected_and_first_client_keeps_it() {
        let dir = crate::test_support::TestDir::new();
        let path = dir.path("vmnet.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let mut reg = Registry::new(4);

        let path_c = path.clone();
        let first = thread::spawn(move || {
            let mut stream = UnixStream::connect(&path_c).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .write_all(&hello(mac(0x56), 0, MAX_FRAME).encode())
                .unwrap();
            let mut raw = [0u8; SERVER_HELLO_LEN];
            stream.read_exact(&mut raw).unwrap();
            assert_eq!(decode_server_hello(&raw).unwrap().status, STATUS_OK);
            stream
                .write_all(&encode_record(&frame_from(mac(0x56))))
                .unwrap();
            stream
        });
        let (mut server, _) = listener.accept().unwrap();
        server
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut raw = [0u8; CLIENT_HELLO_LEN];
        server.read_exact(&mut raw).unwrap();
        match server_hello_for_raw(&mut reg, 4, &raw) {
            HelloResult::Reply(reply) => {
                assert_eq!(reply.status, STATUS_OK);
                server.write_all(&reply.encode()).unwrap();
            }
            HelloResult::Incomplete => panic!("short hello"),
        }
        let _first = first.join().unwrap();
        let mut ingress = ClientIngress::new(mac(0x56), MAX_FRAME as usize);
        let mut buf = [0u8; 64];
        let n = server.read(&mut buf).unwrap();
        assert_eq!(
            ingress.push(&buf[..n]),
            vec![ClientRecord::Forward(frame_from(mac(0x56)))]
        );

        let path_c = path.clone();
        let second = thread::spawn(move || {
            let mut stream = UnixStream::connect(&path_c).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            stream
                .write_all(&hello(mac(0x56), 0, MAX_FRAME).encode())
                .unwrap();
            let mut raw = [0u8; SERVER_HELLO_LEN];
            stream.read_exact(&mut raw).unwrap();
            assert_eq!(
                decode_server_hello(&raw).unwrap().status,
                STATUS_DUPLICATE_MAC
            );
            let mut extra = [0u8; 1];
            assert_eq!(stream.read(&mut extra).unwrap(), 0);
        });
        let (mut server2, _) = listener.accept().unwrap();
        let mut raw = [0u8; CLIENT_HELLO_LEN];
        server2.read_exact(&mut raw).unwrap();
        match server_hello_for_raw(&mut reg, 4, &raw) {
            HelloResult::Reply(reply) => {
                assert_eq!(reply.status, STATUS_DUPLICATE_MAC);
                server2.write_all(&reply.encode()).unwrap();
            }
            HelloResult::Incomplete => panic!("short hello"),
        }
        drop(server2);
        second.join().unwrap();
        assert_eq!(reg.len(), 1);
        assert!(reg.contains(&mac(0x56)));

        reg.release(&mac(0x56));
        let again = hello(mac(0x56), 0, MAX_FRAME);
        assert_eq!(reg.admit(4, &again).status, STATUS_OK);
        assert!(reg.contains(&mac(0x56)));
    }

    #[test]
    fn duplicate_mac_wins_over_capacity() {
        let mut reg = Registry::new(1);
        for i in 0..MAX_CLIENTS {
            let reply = reg.admit(1, &hello(mac(i as u8), 0, MAX_FRAME));
            assert_eq!(reply.status, STATUS_OK, "client {i}");
        }
        assert_eq!(
            reg.admit(1, &hello(mac(0), 0, MAX_FRAME)).status,
            STATUS_DUPLICATE_MAC
        );
        assert_eq!(
            reg.admit(1, &hello(mac(0x40), 0, MAX_FRAME)).status,
            STATUS_TOO_MANY_CLIENTS
        );
        assert_eq!(reg.len(), MAX_CLIENTS);
    }

    #[test]
    fn rejected_mac_and_bad_frame_size() {
        let mut reg = Registry::new(1);
        for bad in [[0u8; 6], [0xff; 6], [0x01, 0x00, 0x5e, 0x00, 0x00, 0x01]] {
            let reply = reg.admit(1, &hello(bad, 0, MAX_FRAME));
            assert_eq!(reply.status, STATUS_REJECTED_MAC);
            assert_eq!(reply.max_frame, 0);
        }
        assert_eq!(
            reg.admit(1, &hello(mac(1), 0, 1513)).status,
            STATUS_BAD_MAX_FRAME
        );
        assert_eq!(
            reg.admit(1, &hello(mac(1), 0, 2049)).status,
            STATUS_BAD_MAX_FRAME
        );
        assert_eq!(reg.admit(1, &hello(mac(1), 0, 1514)).status, STATUS_OK);
        assert_eq!(
            reg.admit(1, &hello(mac(2), 0, 2048)).max_frame,
            2048
        );
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn oversize_but_bounded_record_is_discarded() {
        let mut ingress = ClientIngress::new(mac(0x56), 1514);
        let mut big = vec![0xab; 2048];
        big[6..12].copy_from_slice(&[9, 9, 9, 9, 9, 9]);
        assert_eq!(
            ingress.push(&encode_record(&big)),
            vec![ClientRecord::Discard]
        );
        assert_eq!(ingress.frame_oversize(), 1);
        assert_eq!(ingress.tx_spoof, 0);
        let huge = vec![0u8; 65536];
        assert_eq!(
            ingress.push(&encode_record(&huge)),
            vec![ClientRecord::Discard]
        );
        assert_eq!(ingress.frame_oversize(), 2);
        assert_eq!(ingress.tx_spoof, 0);
        let good = frame_from(mac(0x56));
        assert_eq!(
            ingress.push(&encode_record(&good)),
            vec![ClientRecord::Forward(good)]
        );

        let mut closed = ClientIngress::new(mac(0x56), 1514);
        let mut too_big = (65537u32).to_le_bytes().to_vec();
        too_big.extend_from_slice(&encode_record(&frame_from(mac(0x56))));
        assert_eq!(closed.push(&too_big), vec![ClientRecord::Close]);
        assert_eq!(closed.frame_oversize(), 0);

        let mut torn = ClientIngress::new(mac(0x56), 1514);
        let mut partial = 300u32.to_le_bytes().to_vec();
        partial.extend_from_slice(&[0u8; 10]);
        assert!(torn.push(&partial).is_empty());
        assert_eq!(torn.eof(), ClientRecord::Close);
    }

    #[test]
    fn record_lengths_below_14_are_not_written() {
        let mut ingress = ClientIngress::new(mac(0x56), MAX_FRAME as usize);
        assert_eq!(
            ingress.push(&encode_record(&[0x11])),
            vec![ClientRecord::Discard]
        );
        assert_eq!(
            ingress.push(&encode_record(&[0u8; 13])),
            vec![ClientRecord::Discard]
        );
        assert_eq!(ingress.frame_runt(), 2);
        assert_eq!(ingress.frame_oversize(), 0);
        assert_eq!(ingress.tx_spoof, 0);
        let good = frame_from(mac(0x56));
        assert_eq!(
            ingress.push(&encode_record(&good)),
            vec![ClientRecord::Forward(good)]
        );
        let mut zero = ClientIngress::new(mac(0x56), MAX_FRAME as usize);
        assert_eq!(zero.push(&0u32.to_le_bytes()), vec![ClientRecord::Close]);
    }

    #[test]
    fn unauthorized_peer_is_not_inserted() {
        let mut reg = Registry::new(10);
        let reply = reg.admit(11, &hello(mac(0x56), 0, MAX_FRAME));
        assert_eq!(reply.status, STATUS_UNAUTHORIZED);
        assert!(reg.is_empty());
        let raw = hello(mac(0x56), 0, MAX_FRAME).encode();
        match server_hello_for_raw(&mut reg, 11, &raw) {
            HelloResult::Reply(reply) => assert_eq!(reply.status, STATUS_UNAUTHORIZED),
            HelloResult::Incomplete => panic!("full hello"),
        }
        assert!(reg.is_empty());
    }

    fn build_dhcp(opt6: &[u8], trailer: &[u8]) -> Vec<u8> {
        assert!(opt6.len() % 4 == 0);
        let options_len = 2 + opt6.len() + 3 + 1;
        let bootp_len = 240 + options_len;
        let udp_len = 8 + bootp_len;
        let ip_total = 20 + udp_len;
        let mut buf = vec![0u8; 14 + ip_total + trailer.len()];
        buf[12] = 0x08;
        buf[13] = 0x00;
        buf[14] = 0x45;
        buf[16..18].copy_from_slice(&(ip_total as u16).to_be_bytes());
        buf[23] = 17;
        buf[34..38].copy_from_slice(&[0, 67, 0, 68]);
        buf[38..40].copy_from_slice(&(udp_len as u16).to_be_bytes());
        buf[40] = 0xff;
        buf[41] = 0xff;
        buf[42] = 2;
        buf[58..62].copy_from_slice(&[192, 168, 18, 20]);
        let cookie = 42 + 236;
        buf[cookie..cookie + 4].copy_from_slice(&[99, 130, 83, 99]);
        let mut i = cookie + 4;
        buf[i] = 6;
        buf[i + 1] = opt6.len() as u8;
        buf[i + 2..i + 2 + opt6.len()].copy_from_slice(opt6);
        i += 2 + opt6.len();
        buf[i] = 53;
        buf[i + 1] = 1;
        buf[i + 2] = 5;
        i += 3;
        buf[i] = 255;
        buf[14 + ip_total..].copy_from_slice(trailer);
        buf
    }

    fn option6(buf: &[u8]) -> &[u8] {
        let cookie = 42 + 236;
        let start = cookie + 6;
        let len = buf[cookie + 5] as usize;
        &buf[start..start + len]
    }

    #[test]
    fn rewrite_replaces_bridge_gateway_and_clears_udp_checksum() {
        let mut buf = build_dhcp(&[192, 168, 18, 1], &[]);
        let before_len = buf.len();
        assert!(rewrite_dhcp_dns(
            &mut buf,
            DhcpDnsScope::GatewayOnly,
            &[Ipv4Addr::new(1, 1, 1, 1)]
        ));
        assert_eq!(option6(&buf), &[1, 1, 1, 1]);
        assert_eq!(&buf[40..42], &[0, 0]);
        assert_eq!(&buf[58..62], &[192, 168, 18, 20]);
        assert_eq!(buf[buf.len() - 2], 5);
        assert_eq!(buf.len(), before_len);
    }

    #[test]
    fn rewrite_gateway_only_leaves_192_168_18_50() {
        let mut gateway = build_dhcp(&[192, 168, 18, 50], &[]);
        let original = gateway.clone();
        assert!(!rewrite_dhcp_dns(
            &mut gateway,
            DhcpDnsScope::GatewayOnly,
            &[Ipv4Addr::new(1, 1, 1, 1)]
        ));
        assert_eq!(gateway, original);
        assert!(rewrite_dhcp_dns(
            &mut gateway,
            DhcpDnsScope::SharedBridge,
            &[Ipv4Addr::new(1, 1, 1, 1)]
        ));
        assert_eq!(option6(&gateway), &[1, 1, 1, 1]);
        assert_eq!(&gateway[40..42], &[0, 0]);
    }

    #[test]
    fn network_scope_matches_only_that_prefix() {
        let scope = DhcpDnsScope::Network {
            network: u32::from(Ipv4Addr::new(10, 1, 0, 0)),
            prefix: 16,
        };
        assert!(scope.matches(&[10, 1, 0, 1]));
        assert!(scope.matches(&[10, 1, 255, 254]));
        assert!(!scope.matches(&[10, 2, 0, 1]));
    }

    #[test]
    fn rewrite_does_not_scan_past_udp_length() {
        let trailer = [6u8, 4, 192, 168, 1, 1];
        for scope in [DhcpDnsScope::GatewayOnly, DhcpDnsScope::SharedBridge] {
            let mut buf = build_dhcp(&[9, 9, 9, 9], &trailer);
            let original = buf.clone();
            assert!(!rewrite_dhcp_dns(
                &mut buf,
                scope,
                &[Ipv4Addr::new(1, 1, 1, 1)]
            ));
            assert_eq!(buf, original);
        }
    }

    #[test]
    fn rewrite_fills_two_slots_with_fallback_pair() {
        let mut buf = build_dhcp(&[192, 168, 1, 1, 192, 168, 2, 1], &[]);
        assert!(rewrite_dhcp_dns(
            &mut buf,
            DhcpDnsScope::GatewayOnly,
            &[Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(8, 8, 8, 8)]
        ));
        assert_eq!(option6(&buf), &[1, 1, 1, 1, 8, 8, 8, 8]);
        assert_eq!(&buf[40..42], &[0, 0]);
    }

    #[test]
    fn rewrite_ignores_non_dhcp() {
        let mut arp = vec![0u8; 64];
        arp[12] = 0x08;
        arp[13] = 0x06;
        let original = arp.clone();
        assert!(!rewrite_dhcp_dns(
            &mut arp,
            DhcpDnsScope::SharedBridge,
            &[Ipv4Addr::new(1, 1, 1, 1)]
        ));
        assert_eq!(arp, original);

        let mut udp = build_dhcp(&[192, 168, 18, 1], &[]);
        udp[34] = 0;
        udp[35] = 53;
        udp[36] = 0;
        udp[37] = 53;
        let original = udp.clone();
        assert!(!rewrite_dhcp_dns(
            &mut udp,
            DhcpDnsScope::GatewayOnly,
            &[Ipv4Addr::new(1, 1, 1, 1)]
        ));
        assert_eq!(udp, original);
    }

    #[test]
    fn scutil_ignores_scoped_section_and_bridge_subnet() {
        let text = "\
DNS configuration

resolver #1
  nameserver[0] : 192.168.1.1
  if_index : 14 (en0)
resolver #2
  nameserver[0] : 8.8.4.4
  flags : Scoped, Request A records
resolver #3
  nameserver[0] : 192.168.18.1

DNS configuration (for scoped queries)

resolver #1
  nameserver[0] : 9.9.9.9
  flags : Scoped, Request A records
";
        assert_eq!(
            select_dns_resolvers(text),
            vec![Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(8, 8, 8, 8)]
        );
    }

    #[test]
    fn scutil_keeps_public_nameserver() {
        let text = "\
DNS configuration

resolver #1
  nameserver[0] : 9.9.9.9
resolver #2
  nameserver[0] : 1.0.0.1
  if_index : 14 (en0)
";
        assert_eq!(
            select_dns_resolvers(text),
            vec![Ipv4Addr::new(9, 9, 9, 9), Ipv4Addr::new(1, 0, 0, 1)]
        );
    }

    #[test]
    fn scutil_keeps_public_nameservers_in_a_mixed_if_index_block() {
        let text = "\
DNS configuration

resolver #1
  nameserver[0] : 8.8.8.8
  nameserver[1] : 1.1.1.1
  nameserver[2] : 192.168.50.1
  if_index : 14 (en0)
";
        assert_eq!(
            select_dns_resolvers(text),
            vec![Ipv4Addr::new(8, 8, 8, 8), Ipv4Addr::new(1, 1, 1, 1)]
        );
    }

    #[test]
    fn scutil_search_domain_does_not_skip_resolver() {
        let text = "\
DNS configuration

resolver #1
  search domain[0] : local
  nameserver[0] : 9.9.9.9
resolver #2
  domain : corp.example
  nameserver[0] : 1.2.3.4
";
        assert_eq!(
            select_dns_resolvers(text),
            vec![Ipv4Addr::new(9, 9, 9, 9)]
        );
    }

    #[test]
    fn scutil_keeps_unscoped_rfc1918() {
        let text = "\
DNS configuration

resolver #1
  nameserver[0] : 10.1.2.3
";
        assert_eq!(
            select_dns_resolvers(text),
            vec![Ipv4Addr::new(10, 1, 2, 3)]
        );
    }

    #[test]
    fn scutil_empty_falls_back() {
        assert_eq!(
            select_dns_resolvers(""),
            vec![Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(8, 8, 8, 8)]
        );
    }

    #[test]
    fn outq_keeps_partial_record_and_drops_65th() {
        let mut queue = OutQueue::new();
        let frame = frame_from(mac(0x56));
        for i in 0..CLIENT_OUTQ {
            let mut tagged = frame.clone();
            tagged[5] = i as u8;
            assert!(queue.enqueue_frame(&tagged));
        }
        assert!(!queue.enqueue_frame(&frame));
        assert_eq!(queue.drop_slow_client, 1);
        assert_eq!(queue.queued(), CLIENT_OUTQ);

        let mut written = Vec::new();
        queue
            .flush_outbound(&mut |buf| {
                if written.len() >= 3 {
                    return Err(io::Error::new(ErrorKind::WouldBlock, "full"));
                }
                let n = (3 - written.len()).min(buf.len());
                written.extend_from_slice(&buf[..n]);
                Ok(n)
            })
            .unwrap();
        assert_eq!(written.len(), 3);
        assert!(!queue.outbound().is_empty());
        assert_eq!(queue.queued(), CLIENT_OUTQ - 1);

        queue
            .flush_outbound(&mut |buf| {
                written.extend_from_slice(buf);
                Ok(buf.len())
            })
            .unwrap();
        assert!(queue.outbound().is_empty());
        assert_eq!(queue.queued(), 0);

        let mut ingress = ClientIngress::new(mac(0x56), MAX_FRAME as usize);
        let records = ingress.push(&written);
        assert_eq!(records.len(), CLIENT_OUTQ);
        for (i, record) in records.into_iter().enumerate() {
            match record {
                ClientRecord::Forward(got) => {
                    let mut expect = frame.clone();
                    expect[5] = i as u8;
                    assert_eq!(got, expect);
                }
                other => panic!("record {i} was {other:?}"),
            }
        }
    }
}
