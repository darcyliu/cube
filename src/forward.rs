//! Bounded TCP forwarding with synchronous startup errors and owned shutdown.
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardRule {
    pub bind: SocketAddr,
    pub target: SocketAddr,
}

impl ForwardRule {
    pub fn parse(value: &str) -> Result<Self, String> {
        let (bind, target) = value
            .split_once('=')
            .ok_or("Expected HOST_IP:PORT=GUEST_IP:PORT")?;
        let rule = Self {
            bind: bind
                .parse()
                .map_err(|e| format!("Invalid forwarding bind '{bind}': {e}"))?,
            target: target
                .parse()
                .map_err(|e| format!("Invalid forwarding destination '{target}': {e}"))?,
        };
        if rule.bind.port() == 0 || rule.target.port() == 0 || rule.target.ip().is_unspecified() {
            return Err("Forwarding requires nonzero ports and a concrete destination IP".into());
        }
        Ok(rule)
    }
}

pub struct PortForward {
    #[cfg(test)]
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

pub type GuestAddress = Arc<Mutex<Option<Ipv4Addr>>>;

enum Destination {
    Fixed(SocketAddr),
    GuestSsh(GuestAddress),
}

impl Destination {
    fn resolve(&self) -> Result<SocketAddr, String> {
        match self {
            Self::Fixed(address) => Ok(*address),
            Self::GuestSsh(address) => address.lock().unwrap()
                .map(|ip| SocketAddr::from((ip, 22)))
                .ok_or_else(|| "No DHCP ACK for this VM yet; wait for guest DHCP, or use --forward HOST_IP:PORT=GUEST_IP:22 for a static address".into()),
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Fixed(address) => address.to_string(),
            Self::GuestSsh(_) => "guest DHCP address:22".into(),
        }
    }
}

pub fn start_ssh_forward(port: u16, address: GuestAddress) -> Result<PortForward, String> {
    start_forward(
        SocketAddr::from(([127, 0, 0, 1], port)),
        Destination::GuestSsh(address),
    )
}

impl Drop for PortForward {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Direction {
    bytes: [u8; 8192],
    start: usize,
    end: usize,
    eof: bool,
    closed: bool,
}

impl Direction {
    fn new() -> Self {
        Self {
            bytes: [0; 8192],
            start: 0,
            end: 0,
            eof: false,
            closed: false,
        }
    }

    fn pump(&mut self, from: &mut TcpStream, to: &mut TcpStream) -> io::Result<()> {
        if self.start == self.end && !self.eof {
            match from.read(&mut self.bytes) {
                Ok(0) => self.eof = true,
                Ok(n) => {
                    self.start = 0;
                    self.end = n;
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e),
            }
        }
        if self.start < self.end {
            match to.write(&self.bytes[self.start..self.end]) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(n) => self.start += n,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e),
            }
        }
        if self.eof && self.start == self.end && !self.closed {
            to.shutdown(Shutdown::Write)?;
            self.closed = true;
        }
        Ok(())
    }
}

struct Connection {
    client: TcpStream,
    target: TcpStream,
    upstream: Direction,
    downstream: Direction,
}

impl Connection {
    fn pump(&mut self) -> io::Result<bool> {
        self.upstream.pump(&mut self.client, &mut self.target)?;
        self.downstream.pump(&mut self.target, &mut self.client)?;
        Ok(!(self.upstream.closed && self.downstream.closed))
    }
}

pub fn start_port_forward(rule: &ForwardRule) -> Result<PortForward, String> {
    start_forward(rule.bind, Destination::Fixed(rule.target))
}

fn start_forward(bind: SocketAddr, destination: Destination) -> Result<PortForward, String> {
    let label = destination.label();
    let listener = TcpListener::bind(bind).map_err(|e| {
        format!(
            "Cannot bind TCP forward {} -> {}: {e}. Choose a free host address/port.",
            bind, label
        )
    })?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    #[cfg(test)]
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let stop = Arc::new(AtomicBool::new(false));
    let active = stop.clone();
    let worker = thread::Builder::new().name("vmm-forward".into()).spawn(move || {
        crate::host_log!("[Port Forward] TCP {bind} -> {label}");
        let mut connections: Vec<Connection> = Vec::new();
        while !active.load(Ordering::Relaxed) {
            match listener.accept() {
                Ok((client, peer)) => {
                    if connections.len() >= 64 {
                        crate::host_log!("[Port Forward] {bind}: rejecting {peer}; 64 active connections");
                    } else {
                        let target_address = match destination.resolve() {
                            Ok(address) => address,
                            Err(_) => {
                                let mut found = None;
                                for _ in 0..10 {
                                    thread::sleep(Duration::from_millis(100));
                                    if let Ok(addr) = destination.resolve() {
                                        found = Some(addr);
                                        break;
                                    }
                                }
                                match found {
                                    Some(addr) => addr,
                                    None => {
                                        crate::host_log!("[Port Forward] {bind}: guest address not yet discovered via DHCP; dropping connection");
                                        continue;
                                    }
                                }
                            }
                        };
                        let connection = (|| -> io::Result<Connection> {
                            let target = TcpStream::connect_timeout(&target_address, Duration::from_millis(2000))?;
                            client.set_nonblocking(true)?;
                            target.set_nonblocking(true)?;
                            Ok(Connection { client, target, upstream: Direction::new(), downstream: Direction::new() })
                        })();
                        match connection {
                            Ok(connection) => connections.push(connection),
                            Err(e) => crate::host_log!("[Port Forward] {peer} via {bind} -> {target_address}: {e}. Check the guest address, service, and firewall; retry after the guest is ready."),
                        }
                    }
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted) => {},
                Err(e) => { crate::host_log!("[Port Forward] Accept on {bind} failed: {e}"); break; }
            }
            connections.retain_mut(|connection| match connection.pump() {
                Ok(alive) => alive,
                Err(e) => { crate::host_log!("[Port Forward] Transfer {bind} -> {label} failed: {e}"); false }
            });
            thread::sleep(Duration::from_millis(2));
        }
    }).map_err(|e| format!("Cannot start TCP forward worker: {e}"))?;
    Ok(PortForward {
        stop,
        worker: Some(worker),
        #[cfg(test)]
        address,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_destination_waits_for_dhcp_and_tracks_address_changes() {
        let address = Arc::new(Mutex::new(None));
        let destination = Destination::GuestSsh(address.clone());
        assert!(destination.resolve().unwrap_err().contains("No DHCP ACK"));
        for ip in [
            Ipv4Addr::new(192, 168, 2, 2),
            Ipv4Addr::new(192, 168, 18, 7),
        ] {
            *address.lock().unwrap() = Some(ip);
            assert_eq!(destination.resolve().unwrap(), SocketAddr::from((ip, 22)));
        }
    }

    #[test]
    fn parses_addresses_and_rejects_bad_rules() {
        assert_eq!(
            ForwardRule::parse("127.0.0.1:2222=192.168.18.7:22")
                .unwrap()
                .target,
            "192.168.18.7:22".parse::<SocketAddr>().unwrap()
        );
        assert!(ForwardRule::parse("[::1]:2222=[::1]:22").is_ok());
        for bad in [
            "2222",
            "localhost:22=192.168.18.2:22",
            "127.0.0.1:0=192.168.18.2:22",
            "127.0.0.1:22=0.0.0.0:22",
        ] {
            assert!(ForwardRule::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn forwards_large_payload_half_close_and_releases_listener() {
        let guest = TcpListener::bind("127.0.0.1:0").unwrap();
        guest.set_nonblocking(true).unwrap();
        let mut rule = ForwardRule {
            bind: guest.local_addr().unwrap(),
            target: guest.local_addr().unwrap(),
        };
        assert!(
            start_port_forward(&rule)
                .err()
                .unwrap()
                .contains("Cannot bind")
        );
        rule.bind = "127.0.0.1:0".parse().unwrap();
        let forward = start_port_forward(&rule).unwrap();
        let bind = forward.address;
        let peer = thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut stream = loop {
                match guest.accept() {
                    Ok((s, _)) => break s,
                    Err(e)
                        if e.kind() == io::ErrorKind::WouldBlock
                            && std::time::Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => panic!("guest accept: {e}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut data = Vec::new();
            stream.read_to_end(&mut data).unwrap();
            assert_eq!(data.len(), 128 * 1024);
            assert!(data.iter().all(|&b| b == 42));
            stream.write_all(&data).unwrap();
        });
        let mut client = TcpStream::connect(bind).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client
            .set_write_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        client.write_all(&vec![42; 128 * 1024]).unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        assert_eq!(response.len(), 128 * 1024);
        assert!(response.iter().all(|&b| b == 42));
        peer.join().unwrap();
        drop(forward);
        assert!(TcpListener::bind(bind).is_ok());
    }
}
