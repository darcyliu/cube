//! Long-running shared-mode vmnet process.
//!
//! Started as root (`sudo vmm --vmnet-service`), then `setuid` to `SUDO_UID`.
//! Guests connect with `--net vmnet-proxy`. `--net vmnet` does not use this module.

use std::ffi::{CStr, CString};
use std::fs;
use std::io::{self, Read, Write};
use std::net::Ipv4Addr;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use crate::vmnet_proto::{
    decode_client_hello, server_hello_for_raw, ClientHello, ClientIngress, ClientRecord, HelloResult,
    OutQueue, Registry, CLIENT_HELLO_LEN, STATUS_OK,
};
use crate::vmnet_sys::{IoOutcome, VmnetPort};

const SUN_PATH_LIMIT: usize = 104;

static STOP: AtomicBool = AtomicBool::new(false);

unsafe extern "C" fn on_stop(_: i32) {
    STOP.store(true, Ordering::Relaxed);
}

pub fn run(args: &[String]) -> Result<(), String> {
    let parsed = parse_service_args(args)?;
    if unsafe { libc::geteuid() } != 0 {
        return Err(
            "vmnet service requires root. Run `make vmnet-service`, or `sudo ./target/debug/vmm --vmnet-service` from your user shell."
                .into(),
        );
    }
    let user = invoking_user()?;
    let custom_socket = parsed.socket.is_some();
    let path = parsed
        .socket
        .clone()
        .unwrap_or_else(|| user.home.join("Library/Application Support/vmm/vmnet.sock"));
    check_sun_path(&path)?;

    let mut port = match &parsed.net {
        ServiceNet::Shared => match &parsed.subnet {
            Some(subnet) => {
                eprintln!(
                    "[vmnet] requesting subnet {} gateway {} dhcp {}-{}",
                    subnet.cidr, subnet.gateway, subnet.dhcp_first, subnet.end
                );
                VmnetPort::start_shared_range(&subnet.gateway, &subnet.end, &subnet.mask)?
            }
            None => VmnetPort::start_shared()?,
        },
        ServiceNet::Bridged { iface } => {
            let name = match iface {
                Some(name) => name.clone(),
                None => default_host_iface().unwrap_or_else(|| "en0".into()),
            };
            warn_if_wifi(&name);
            VmnetPort::start_bridged(&name)?
        }
    };
    let dns_scope = if let Some(iface) = port.host_iface.clone() {
        eprint_network_interface(&iface);
        eprintln!("[vmnet] LAN bridge: DHCP offers are forwarded unchanged");
        None
    } else {
        log_shared_interface(port.shared_gateway);
        if !port.reported_network {
            log_bridge100_range();
        }
        let nat_cidr = port
            .shared_cidr
            .clone()
            .or_else(|| parsed.subnet.as_ref().map(|subnet| subnet.cidr.clone()))
            .unwrap_or_else(|| "192.168.18.0/24".to_string());
        if let (Some(requested), Some(actual)) = (&parsed.subnet, &port.shared_cidr) {
            if requested.cidr != *actual {
                eprintln!(
                    "[vmnet] vmnet kept {actual} instead of requested {}",
                    requested.cidr
                );
            }
        }
        enable_forwarding();
        install_pf_nat(&nat_cidr);
        Some(
            network_scope(&nat_cidr).unwrap_or(crate::vmnet_proto::DhcpDnsScope::SharedBridge),
        )
    };
    let resolvers = if dns_scope.is_some() {
        load_resolvers()
    } else {
        Vec::new()
    };

    if !custom_socket {
        let dir = path.parent().ok_or("socket path has no directory")?;
        repair_socket_dir(dir, user.uid, user.gid)?;
    }
    parent_allows_bind(&path, user.uid)?;
    match probe_socket(&path)? {
        Probe::AlreadyListening => {
            return Err(format!(
                "vmnet service already listening on {}",
                path.display()
            ));
        }
        Probe::Absent | Probe::RemovedStale => {}
    }

    let listener = bind_listener(&path, user.uid, user.gid)?;
    let _bound = BoundSocket {
        path: path.clone(),
        unlink: true,
    };
    drop_privileges(user.uid, user.gid)?;
    eprintln!("[vmnet] listening on {}", path.display());
    serve(listener, &mut port, &resolvers, user.uid, dns_scope)
}

pub fn default_socket_path() -> Result<PathBuf, String> {
    socket_path_for(unsafe { libc::geteuid() })
}

pub fn socket_path_for(uid: u32) -> Result<PathBuf, String> {
    let (_name, home) = lookup_user(uid)?;
    Ok(home.join("Library/Application Support/vmm/vmnet.sock"))
}

enum ServiceNet {
    Shared,
    /// `None` selects the default-route interface when the service starts.
    Bridged { iface: Option<String> },
}

struct RequestedSubnet {
    cidr: String,
    gateway: String,
    dhcp_first: String,
    end: String,
    mask: String,
}

struct ServiceArgs {
    socket: Option<PathBuf>,
    net: ServiceNet,
    subnet: Option<RequestedSubnet>,
}

fn parse_service_args(args: &[String]) -> Result<ServiceArgs, String> {
    if args.get(1).map(String::as_str) != Some("--vmnet-service") {
        return Err("--vmnet-service must be the first argument".into());
    }
    let mut socket = None;
    let mut net = ServiceNet::Shared;
    let mut subnet = None;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--subnet" => {
                if subnet.is_some() {
                    return Err("--subnet was given twice".into());
                }
                let Some(value) = args.get(i + 1) else {
                    return Err("--subnet requires a CIDR, for example 192.168.18.0/24".into());
                };
                subnet = Some(parse_subnet(value)?);
                i += 2;
            }
            "--vmnet-socket" => {
                let Some(value) = args.get(i + 1) else {
                    return Err("--vmnet-socket requires a path".into());
                };
                socket = Some(PathBuf::from(value));
                i += 2;
            }
            "--bridge" => {
                if matches!(net, ServiceNet::Bridged { .. }) {
                    return Err("--bridge was given twice".into());
                }
                let iface = args.get(i + 1).filter(|value| !value.starts_with('-'));
                if let Some(iface) = iface {
                    if iface.is_empty() {
                        return Err("--bridge requires a host interface name".into());
                    }
                    net = ServiceNet::Bridged {
                        iface: Some(iface.clone()),
                    };
                    i += 2;
                } else {
                    net = ServiceNet::Bridged { iface: None };
                    i += 1;
                }
            }
            other => {
                return Err(format!(
                    "--vmnet-service accepts only --vmnet-socket, --bridge, and --subnet; unexpected {other}"
                ));
            }
        }
    }
    if subnet.is_some() && matches!(net, ServiceNet::Bridged { .. }) {
        return Err("--subnet applies to NAT mode; omit it with --bridge".into());
    }
    Ok(ServiceArgs { socket, net, subnet })
}

fn parse_subnet(text: &str) -> Result<RequestedSubnet, String> {
    let Some((addr_text, prefix_text)) = text.split_once('/') else {
        return Err(format!("--subnet {text} is not an IPv4 CIDR"));
    };
    let addr: Ipv4Addr = addr_text
        .parse()
        .map_err(|_| format!("--subnet {text} is not an IPv4 CIDR"))?;
    let prefix: u32 = prefix_text
        .parse()
        .map_err(|_| format!("--subnet {text} is not an IPv4 CIDR"))?;
    if !(8..=30).contains(&prefix) {
        return Err(format!("--subnet {text} must use a prefix from /8 through /30"));
    }
    let mask_bits = u32::MAX << (32 - prefix);
    let addr_bits = u32::from(addr);
    if addr_bits & !mask_bits != 0 {
        let network = Ipv4Addr::from(addr_bits & mask_bits);
        return Err(format!(
            "--subnet {text} is not a network address; use {network}/{prefix}"
        ));
    }
    if !is_rfc1918(addr_bits) {
        return Err(format!(
            "--subnet {text} must be RFC1918 (10.0.0.0/8, 172.16.0.0/12, or 192.168.0.0/16)"
        ));
    }
    let broadcast = addr_bits | !mask_bits;
    let gateway = addr_bits + 1;
    if gateway >= broadcast {
        return Err(format!("--subnet {text} has no DHCP addresses"));
    }
    Ok(RequestedSubnet {
        cidr: format!("{addr}/{prefix}"),
        gateway: Ipv4Addr::from(gateway).to_string(),
        dhcp_first: Ipv4Addr::from(gateway + 1).to_string(),
        end: Ipv4Addr::from(broadcast - 1).to_string(),
        mask: Ipv4Addr::from(mask_bits).to_string(),
    })
}

fn is_rfc1918(ip: u32) -> bool {
    let a = ip >> 24;
    let b = (ip >> 16) & 0xff;
    a == 10 || (a == 172 && (16..=31).contains(&b)) || (a == 192 && b == 168)
}

fn network_scope(cidr: &str) -> Option<crate::vmnet_proto::DhcpDnsScope> {
    let subnet = parse_subnet(cidr).ok()?;
    let gateway: Ipv4Addr = subnet.gateway.parse().ok()?;
    let mask: Ipv4Addr = subnet.mask.parse().ok()?;
    Some(crate::vmnet_proto::DhcpDnsScope::Network {
        network: gateway.to_bits() & mask.to_bits(),
        prefix: mask.to_bits().leading_ones(),
    })
}

struct Invoker {
    uid: u32,
    gid: u32,
    home: PathBuf,
}

fn invoking_user() -> Result<Invoker, String> {
    let uid = std::env::var("SUDO_UID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .filter(|uid| *uid != 0)
        .ok_or(
            "missing SUDO_UID, or SUDO_UID=0. Run `make vmnet-service` as your user, or `sudo ./target/debug/vmm --vmnet-service` from a user shell.",
        )?;
    let gid = std::env::var("SUDO_GID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .ok_or("missing SUDO_GID")?;
    let (name, home) = lookup_user(uid)?;
    if let Ok(sudo_user) = std::env::var("SUDO_USER") {
        if !sudo_user.is_empty() && sudo_user != name {
            return Err(format!(
                "SUDO_USER {sudo_user} does not match getpwuid({uid}) name {name}"
            ));
        }
    }
    Ok(Invoker { uid, gid, home })
}

fn lookup_user(uid: u32) -> Result<(String, PathBuf), String> {
    let pw = unsafe { libc::getpwuid(uid) };
    if pw.is_null() {
        return Err(format!("getpwuid({uid}) failed"));
    }
    let name_ptr = unsafe { (*pw).pw_name };
    let dir_ptr = unsafe { (*pw).pw_dir };
    if name_ptr.is_null() || dir_ptr.is_null() {
        return Err(format!("getpwuid({uid}) returned an empty name or home"));
    }
    let name = unsafe { CStr::from_ptr(name_ptr) }
        .to_string_lossy()
        .into_owned();
    let home = unsafe { CStr::from_ptr(dir_ptr) }
        .to_string_lossy()
        .into_owned();
    if name.is_empty() || home.is_empty() {
        return Err(format!("getpwuid({uid}) returned an empty name or home"));
    }
    Ok((name, PathBuf::from(home)))
}

fn check_sun_path(path: &Path) -> Result<(), String> {
    let len = path.as_os_str().len();
    if len >= SUN_PATH_LIMIT {
        return Err(format!(
            "socket path {} is {len} bytes; sun_path allows {} plus a NUL",
            path.display(),
            SUN_PATH_LIMIT - 1
        ));
    }
    Ok(())
}

fn repair_socket_dir(dir: &Path, uid: u32, gid: u32) -> Result<(), String> {
    match fs::symlink_metadata(dir) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(dir).map_err(|err| format!("mkdir {}: {err}", dir.display()))?;
        }
        Err(err) => return Err(format!("lstat {}: {err}", dir.display())),
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(format!(
                "{} is a symlink; refusing to chown it",
                dir.display()
            ));
        }
        Ok(meta) if !meta.is_dir() => {
            return Err(format!("{} is not a directory", dir.display()));
        }
        Ok(_) => {}
    }
    let c_path = CString::new(dir.as_os_str().as_bytes())
        .map_err(|_| format!("socket directory {} contains a NUL", dir.display()))?;
    let chown_rc = unsafe { libc::chown(c_path.as_ptr(), uid, gid) };
    if chown_rc != 0 {
        return Err(format!(
            "chown {}: {}",
            dir.display(),
            io::Error::last_os_error()
        ));
    }
    let chmod_rc = unsafe { libc::chmod(c_path.as_ptr(), 0o700) };
    if chmod_rc != 0 {
        return Err(format!(
            "chmod {}: {}",
            dir.display(),
            io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn parent_allows_bind(path: &Path, uid: u32) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let meta = fs::symlink_metadata(parent)
        .map_err(|err| format!("lstat {}: {err}", parent.display()))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(format!(
            "socket directory {} is not a real directory",
            parent.display()
        ));
    }
    let owner = meta.uid();
    if owner != 0 && owner != uid {
        return Err(format!(
            "socket directory {} is owned by {owner}, want root or {uid}",
            parent.display()
        ));
    }
    if meta.mode() & 0o022 != 0 {
        return Err(format!(
            "socket directory {} is group- or world-writable",
            parent.display()
        ));
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum Probe {
    Absent,
    RemovedStale,
    AlreadyListening,
}

fn probe_socket(path: &Path) -> Result<Probe, String> {
    let meta = match fs::symlink_metadata(path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Probe::Absent),
        Err(err) => return Err(format!("lstat {}: {err}", path.display())),
        Ok(meta) => meta,
    };
    if meta.file_type().is_symlink() {
        return Err(format!(
            "refusing to unlink symlink {}",
            path.display()
        ));
    }
    if !meta.file_type().is_socket() {
        return Err(format!(
            "{} exists and is not a socket",
            path.display()
        ));
    }
    match UnixStream::connect(path) {
        Ok(_stream) => Ok(Probe::AlreadyListening),
        Err(err)
            if err.kind() == io::ErrorKind::ConnectionRefused
                || err.raw_os_error() == Some(libc::ECONNREFUSED) =>
        {
            fs::remove_file(path).map_err(|err| format!("unlink {}: {err}", path.display()))?;
            Ok(Probe::RemovedStale)
        }
        Err(err) => Err(format!("probe {}: {err}", path.display())),
    }
}

fn bind_listener(path: &Path, uid: u32, gid: u32) -> Result<UnixListener, String> {
    let listener = match UnixListener::bind(path) {
        Ok(listener) => listener,
        Err(err)
            if err.kind() == io::ErrorKind::AddrInUse
                || err.raw_os_error() == Some(libc::EADDRINUSE) =>
        {
            return Err(format!(
                "vmnet service already listening on {}",
                path.display()
            ));
        }
        Err(err) => return Err(format!("bind {}: {err}", path.display())),
    };
    if let Err(err) = configure_listener(&listener, path, uid, gid) {
        drop(listener);
        let _ = fs::remove_file(path);
        return Err(err);
    }
    Ok(listener)
}

fn configure_listener(
    listener: &UnixListener,
    path: &Path,
    uid: u32,
    gid: u32,
) -> Result<(), String> {
    let fd = listener.as_raw_fd();
    if unsafe { libc::listen(fd, 16) } != 0 {
        return Err(format!("listen: {}", io::Error::last_os_error()));
    }
    set_nosigpipe(fd)?;
    // fchown and fchmod return EINVAL when the fd is a socket. fstat on that
    // fd stays mode 0666 and its inode is not the directory entry.
    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| format!("socket path {} contains a NUL", path.display()))?;
    if unsafe { libc::lchown(c_path.as_ptr(), uid, gid) } != 0 {
        return Err(format!(
            "lchown {}: {}",
            path.display(),
            io::Error::last_os_error()
        ));
    }
    if unsafe {
        libc::fchmodat(
            libc::AT_FDCWD,
            c_path.as_ptr(),
            0o600,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(format!(
            "fchmodat {}: {}",
            path.display(),
            io::Error::last_os_error()
        ));
    }
    let meta = fs::symlink_metadata(path)
        .map_err(|err| format!("lstat {}: {err}", path.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!(
            "{} is a symlink; refusing to listen",
            path.display()
        ));
    }
    if !meta.file_type().is_socket() {
        return Err(format!("{} is not a socket", path.display()));
    }
    if meta.mode() & 0o077 != 0 {
        return Err(format!(
            "listen socket mode {:o} is broader than 0600",
            meta.mode() & 0o777
        ));
    }
    if meta.uid() != uid || meta.gid() != gid {
        return Err(format!(
            "listen socket owner {}:{} is not {uid}:{gid}",
            meta.uid(),
            meta.gid()
        ));
    }
    listener
        .set_nonblocking(true)
        .map_err(|err| format!("nonblocking listen: {err}"))?;
    // A swapped path can pass lstat after we chmod it. Accepting our own
    // connect is what shows this fd is the directory entry.
    let probe = UnixStream::connect(path).map_err(|err| {
        format!(
            "socket path {} does not reach this listener: {err}",
            path.display()
        )
    })?;
    match listener.accept() {
        Ok((accepted, _)) => {
            drop(accepted);
            drop(probe);
        }
        Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
            return Err(format!(
                "socket path {} does not reach this listener",
                path.display()
            ));
        }
        Err(err) => return Err(format!("accept {}: {err}", path.display())),
    }
    Ok(())
}

fn set_nosigpipe(fd: i32) -> Result<(), String> {
    let one: libc::c_int = 1;
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_NOSIGPIPE,
            &one as *const libc::c_int as *const libc::c_void,
            std::mem::size_of_val(&one) as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(format!("SO_NOSIGPIPE: {}", io::Error::last_os_error()));
    }
    Ok(())
}

struct BoundSocket {
    path: PathBuf,
    unlink: bool,
}

impl Drop for BoundSocket {
    fn drop(&mut self) {
        if self.unlink {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn drop_privileges(uid: u32, gid: u32) -> Result<(), String> {
    if std::env::var("VMM_VMNET_KEEP_ROOT").ok().as_deref() == Some("1") {
        eprintln!("[vmnet] VMM_VMNET_KEEP_ROOT=1: process remains root");
        return Ok(());
    }
    let rc = unsafe {
        libc::setgroups(0, std::ptr::null()) != 0
            || libc::setgid(gid) != 0
            || libc::setuid(uid) != 0
    };
    if rc {
        return Err(format!(
            "setuid({uid}): {}",
            io::Error::last_os_error()
        ));
    }
    eprintln!("[vmnet] dropped privileges to uid {uid}");
    Ok(())
}

fn command_output(cmd: &mut Command, timeout: Duration) -> Result<std::process::Output, String> {
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|err| err.to_string())?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {}s", timeout.as_secs()));
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(err) => return Err(err.to_string()),
        }
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_end(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_end(&mut stderr);
    }
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn command_with_stdin(
    cmd: &mut Command,
    stdin_bytes: &[u8],
    timeout: Duration,
) -> Result<std::process::Output, String> {
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|err| err.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(stdin_bytes)
            .map_err(|err| format!("write stdin: {err}"))?;
    }
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("timed out after {}s", timeout.as_secs()));
            }
            Ok(None) => thread::sleep(Duration::from_millis(20)),
            Err(err) => return Err(err.to_string()),
        }
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_end(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_end(&mut stderr);
    }
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

fn warn_if_wifi(iface: &str) {
    let Ok(output) = command_output(Command::new("ifconfig").arg(iface), Duration::from_secs(3))
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let wifi = text.lines().any(|line| {
        let line = line.trim();
        line.starts_with("type: Wi-Fi") || line.contains("IEEE802.11")
    });
    if wifi {
        eprintln!(
            "[vmnet] {iface} is Wi-Fi; bridged vmnet often rejects a wireless interface. Ethernet is the reliable choice."
        );
    }
}

fn eprint_network_interface(name: &str) {
    eprintln!("[vmnet] network interface \x1b[1;36m{name}\x1b[0m");
}

fn log_shared_interface(gateway: Option<Ipv4Addr>) {
    let Some(gateway) = gateway else {
        eprint_network_interface("bridge100");
        return;
    };
    let output = command_output(Command::new("ifconfig").arg("-a"), Duration::from_secs(3));
    let text = match output {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout).into_owned(),
        Ok(output) => {
            eprintln!(
                "[vmnet] ifconfig -a failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            return;
        }
        Err(err) => {
            eprintln!("[vmnet] ifconfig -a failed: {err}");
            return;
        }
    };
    match interface_with_inet(&text, gateway) {
        Some(name) => eprint_network_interface(&name),
        None => eprintln!("[vmnet] no interface has address {gateway}"),
    }
}

/// Interface header in `ifconfig -a`, then the `inet` line that holds `gateway`.
fn interface_with_inet(text: &str, gateway: Ipv4Addr) -> Option<String> {
    let mut current: Option<String> = None;
    for line in text.lines() {
        if !line.starts_with([' ', '\t']) {
            current = line
                .split(':')
                .next()
                .filter(|name| !name.is_empty())
                .map(str::to_string);
            continue;
        }
        let mut parts = line.split_whitespace();
        if parts.next() != Some("inet") {
            continue;
        }
        let Ok(ip) = parts.next()?.parse::<Ipv4Addr>() else {
            continue;
        };
        if ip == gateway {
            return current;
        }
    }
    None
}

fn log_bridge100_range() {
    let output = command_output(Command::new("ifconfig").arg("bridge100"), Duration::from_secs(3));
    let text = match output {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout).into_owned(),
        Ok(output) => {
            eprintln!(
                "[vmnet] ifconfig bridge100 failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            return;
        }
        Err(err) => {
            eprintln!("[vmnet] ifconfig bridge100 failed: {err}");
            return;
        }
    };
    match describe_ifconfig_inet(&text) {
        Some(range) => eprintln!("[vmnet] bridge100 {range}"),
        None => eprintln!("[vmnet] bridge100 has no IPv4 address"),
    }
}

fn describe_ifconfig_inet(text: &str) -> Option<String> {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("inet") {
            continue;
        }
        let gateway: Ipv4Addr = parts.next()?.parse().ok()?;
        if parts.next() != Some("netmask") {
            continue;
        }
        let mask_text = parts.next()?;
        let bits = if let Some(hex) = mask_text.strip_prefix("0x") {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            mask_text.parse::<Ipv4Addr>().ok()?.to_bits()
        };
        let prefix = bits.leading_ones();
        if prefix == 0 || prefix + bits.trailing_zeros() != 32 {
            continue;
        }
        let network = Ipv4Addr::from(gateway.to_bits() & bits);
        return Some(format!("{network}/{prefix} gateway {gateway}"));
    }
    None
}

fn enable_forwarding() {
    let failed = |detail: String| {
        eprintln!(
            "[vmnet] sysctl net.inet.ip.forwarding=1 failed ({detail}). Guest packets can leave with source 192.168.18.x and die upstream."
        );
    };
    match command_output(
        Command::new("sysctl").args(["-w", "net.inet.ip.forwarding=1"]),
        Duration::from_secs(3),
    ) {
        Ok(output) if output.status.success() => {
            eprintln!("[vmnet] net.inet.ip.forwarding=1");
        }
        Ok(output) => failed(format!(
            "{} {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Err(err) => failed(err),
    }
}

fn default_host_iface() -> Option<String> {
    let output = command_output(
        Command::new("route").args(["get", "default"]),
        Duration::from_secs(3),
    )
    .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
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
}

fn install_pf_nat(cidr: &str) {
    let Some(iface) = default_host_iface() else {
        eprintln!("[vmnet] no default route; skipping PF NAT");
        return;
    };
    let rules = format!(
        "nat on {iface} from {cidr} to any -> ({iface})\npass in quick on bridge100 all\npass out quick on {iface} all\n"
    );
    match command_with_stdin(
        Command::new("pfctl").args(["-a", "com.apple/vmm", "-f", "-"]),
        rules.as_bytes(),
        Duration::from_secs(3),
    ) {
        Ok(output) if output.status.success() => {
            eprintln!("[vmnet] PF NAT on {iface} for {cidr}");
        }
        Ok(output) => eprintln!(
            "[vmnet] pfctl failed on {iface}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ),
        Err(err) => eprintln!("[vmnet] pfctl failed on {iface}: {err}"),
    }
}

fn load_resolvers() -> Vec<Ipv4Addr> {
    match command_output(Command::new("scutil").arg("--dns"), Duration::from_secs(3)) {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout);
            let list = crate::vmnet_proto::select_dns_resolvers(&text);
            eprintln!(
                "[vmnet] DNS resolvers: {}",
                list.iter()
                    .map(|ip| ip.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            list
        }
        Ok(output) => {
            eprintln!(
                "[vmnet] scutil --dns failed ({}); using 1.1.1.1, 8.8.8.8",
                output.status
            );
            crate::vmnet_proto::select_dns_resolvers("")
        }
        Err(err) => {
            eprintln!("[vmnet] scutil --dns failed ({err}); using 1.1.1.1, 8.8.8.8");
            crate::vmnet_proto::select_dns_resolvers("")
        }
    }
}

fn install_stop_handlers() {
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_stop as *const () as usize;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
    }
}

struct SavedCounters {
    frame_runt: u64,
    frame_oversize: u64,
    tx_spoof: u64,
    drop_slow_client: u64,
}

struct ServiceSwitch {
    clients: Vec<ClientState>,
    rx_frames: u64,
    tx_frames: u64,
    rx_oversize: u64,
    rx_unclaimed: u64,
    rx_reflect: u64,
    saved: SavedCounters,
}

struct ClientState {
    mac: [u8; 6],
    ingress: ClientIngress,
    out: OutQueue,
}

impl ServiceSwitch {
    fn new() -> Self {
        Self {
            clients: Vec::new(),
            rx_frames: 0,
            tx_frames: 0,
            rx_oversize: 0,
            rx_unclaimed: 0,
            rx_reflect: 0,
            saved: SavedCounters {
                frame_runt: 0,
                frame_oversize: 0,
                tx_spoof: 0,
                drop_slow_client: 0,
            },
        }
    }

    fn attach(&mut self, mac: [u8; 6], max_frame: usize) {
        self.clients.push(ClientState {
            mac,
            ingress: ClientIngress::new(mac, max_frame),
            out: OutQueue::new(),
        });
    }

    fn fold_removed(&mut self, client: &ClientState) {
        self.saved.frame_runt += client.ingress.frame_runt();
        self.saved.frame_oversize += client.ingress.frame_oversize();
        self.saved.tx_spoof += client.ingress.tx_spoof;
        self.saved.drop_slow_client += client.out.drop_slow_client;
    }

    fn enqueue_rx(&mut self, frame: &[u8]) {
        if frame.len() < 12 {
            return;
        }
        let mut src = [0u8; 6];
        src.copy_from_slice(&frame[6..12]);
        if self.clients.iter().any(|client| client.mac == src) {
            self.rx_reflect += 1;
            return;
        }
        if frame.len() < 14 {
            return;
        }
        self.rx_frames += 1;
        self.deliver(None, frame);
    }

    fn local_switch(&mut self, sender: &[u8; 6], frame: &[u8]) {
        if frame.len() < 14 {
            return;
        }
        self.deliver(Some(*sender), frame);
    }

    fn deliver(&mut self, skip: Option<[u8; 6]>, frame: &[u8]) {
        let mut dest = [0u8; 6];
        dest.copy_from_slice(&frame[..6]);
        let flood = dest[0] & 1 != 0;
        if !flood && !self.clients.iter().any(|client| client.mac == dest) {
            if skip.is_none() {
                self.rx_unclaimed += 1;
            }
            return;
        }
        for client in &mut self.clients {
            if skip == Some(client.mac) {
                continue;
            }
            if !flood && client.mac != dest {
                continue;
            }
            let _ = client.out.enqueue_frame(frame);
        }
    }

    fn ingest(&mut self, idx: usize, data: &[u8]) -> (bool, Vec<Vec<u8>>) {
        let records = self.clients[idx].ingress.push(data);
        let mut writes = Vec::new();
        let mut close = false;
        for record in records {
            if close {
                break;
            }
            match record {
                ClientRecord::Close => close = true,
                ClientRecord::Discard | ClientRecord::Spoof => {}
                ClientRecord::Forward(frame) => {
                    let mac = self.clients[idx].mac;
                    self.local_switch(&mac, &frame);
                    writes.push(frame);
                }
            }
        }
        (close, writes)
    }

    fn snapshot(&self) -> (u64, u64, u64, u64, u64, u64, u64, u64, u64) {
        let mut runt = self.saved.frame_runt;
        let mut oversize = self.saved.frame_oversize;
        let mut spoof = self.saved.tx_spoof;
        let mut slow = self.saved.drop_slow_client;
        for client in &self.clients {
            runt += client.ingress.frame_runt();
            oversize += client.ingress.frame_oversize();
            spoof += client.ingress.tx_spoof;
            slow += client.out.drop_slow_client;
        }
        (
            self.rx_frames,
            self.tx_frames,
            self.rx_oversize,
            runt,
            oversize,
            self.rx_unclaimed,
            self.rx_reflect,
            spoof,
            slow,
        )
    }
}

struct LiveIo {
    stream: UnixStream,
    uid: u32,
}

struct Table {
    reg: Registry,
    sw: ServiceSwitch,
    lives: Vec<LiveIo>,
}

impl Table {
    fn new(owner: u32) -> Self {
        Self {
            reg: Registry::new(owner),
            sw: ServiceSwitch::new(),
            lives: Vec::new(),
        }
    }

    fn remove(&mut self, idx: usize) {
        let mut client = self.sw.clients.remove(idx);
        let live = self.lives.remove(idx);
        self.sw.fold_removed(&client);
        self.reg.release(&client.mac);
        let _ = client.ingress.eof();
        eprintln!(
            "[vmnet] detach mac={} uid={} clients={}",
            format_mac(client.mac),
            live.uid,
            self.reg.len()
        );
    }
}

fn format_mac(mac: [u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

enum HelloRead {
    Full([u8; CLIENT_HELLO_LEN]),
    Incomplete,
}

fn read_full_hello(stream: &mut UnixStream) -> HelloRead {
    let mut buf = [0u8; CLIENT_HELLO_LEN];
    match crate::vmnet_proto::read_exact_deadline(stream, &mut buf, Duration::from_secs(2)) {
        Ok(()) => HelloRead::Full(buf),
        Err(_) => HelloRead::Incomplete,
    }
}

enum Admit {
    Attached(ClientHello),
    Closed,
}

fn finish_hello(stream: &mut UnixStream, peer: u32, reg: &mut Registry) -> Admit {
    let HelloRead::Full(raw) = read_full_hello(stream) else {
        eprintln!("[vmnet] debug: peer uid {peer} closed before ClientHello");
        return Admit::Closed;
    };
    match server_hello_for_raw(reg, peer, &raw) {
        HelloResult::Incomplete => Admit::Closed,
        HelloResult::Reply(hello) => {
            let wrote = stream.write_all(&hello.encode()).is_ok();
            if hello.status != STATUS_OK || !wrote {
                if hello.status == STATUS_OK {
                    if let Ok(decoded) = decode_client_hello(&raw) {
                        reg.release(&decoded.mac);
                    }
                }
                Admit::Closed
            } else {
                match decode_client_hello(&raw) {
                    Ok(decoded) => Admit::Attached(decoded),
                    Err(_) => Admit::Closed,
                }
            }
        }
    }
}

fn peer_uid(fd: i32) -> io::Result<u32> {
    let mut uid = 0u32;
    let mut gid = 0u32;
    let rc = unsafe { libc::getpeereid(fd, &mut uid, &mut gid) };
    if rc != 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(uid)
    }
}

fn adopt(table: &mut Table, mut stream: UnixStream) {
    let fd = stream.as_raw_fd();
    if let Err(err) = set_nosigpipe(fd) {
        eprintln!("[vmnet] {err}");
        return;
    }
    let peer = match peer_uid(fd) {
        Ok(uid) => uid,
        Err(err) => {
            eprintln!("[vmnet] getpeereid: {err}");
            return;
        }
    };
    match finish_hello(&mut stream, peer, &mut table.reg) {
        Admit::Closed => {}
        Admit::Attached(hello) => {
            if stream.set_nonblocking(true).is_err() {
                table.reg.release(&hello.mac);
                return;
            }
            let max_frame = hello.max_frame as usize;
            table.sw.attach(hello.mac, max_frame);
            table.lives.push(LiveIo { stream, uid: peer });
            eprintln!(
                "[vmnet] attach mac={} uid={peer} clients={}",
                format_mac(hello.mac),
                table.reg.len()
            );
        }
    }
}

struct EventPipe {
    read: i32,
    write: i32,
}

impl Drop for EventPipe {
    fn drop(&mut self) {
        unsafe {
            if self.read >= 0 {
                libc::close(self.read);
            }
            if self.write >= 0 {
                libc::close(self.write);
            }
        }
    }
}

fn open_event_pipe(port: &VmnetPort) -> Option<EventPipe> {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        eprintln!("[vmnet] event pipe failed; falling back to a 100 µs poll");
        return None;
    }
    for fd in fds {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0 {
            unsafe {
                libc::close(fds[0]);
                libc::close(fds[1]);
            }
            return None;
        }
    }
    if port.set_event_pipe(fds[1]) {
        Some(EventPipe {
            read: fds[0],
            write: fds[1],
        })
    } else {
        unsafe {
            libc::close(fds[0]);
            libc::close(fds[1]);
        }
        None
    }
}

fn drain_pipe(fd: i32) {
    let mut buf = [0u8; 64];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n <= 0 {
            break;
        }
    }
}

fn drain_vmnet(
    port: &mut VmnetPort,
    buf: &mut [u8],
    sw: &mut ServiceSwitch,
    resolvers: &[Ipv4Addr],
    dns_scope: Option<crate::vmnet_proto::DhcpDnsScope>,
) -> Result<(), String> {
    loop {
        match port.read(buf) {
            IoOutcome::Idle => return Ok(()),
            IoOutcome::Packet(len) => {
                let frame = &mut buf[..len];
                if let Some(scope) = dns_scope {
                    let _ = crate::vmnet_proto::rewrite_dhcp_dns(frame, scope, resolvers);
                }
                sw.enqueue_rx(frame);
            }
            IoOutcome::Oversize => sw.rx_oversize += 1,
            IoOutcome::Counted => {}
            IoOutcome::Fatal(message) => return Err(message),
        }
    }
}

fn serve(
    listener: UnixListener,
    port: &mut VmnetPort,
    resolvers: &[Ipv4Addr],
    owner: u32,
    dns_scope: Option<crate::vmnet_proto::DhcpDnsScope>,
) -> Result<(), String> {
    install_stop_handlers();
    let event = open_event_pipe(port);
    let mut table = Table::new(owner);
    let mut vmnet_buf = vec![0u8; port.read_budget];
    let mut last_log = Instant::now();
    let mut read_buf = [0u8; 16384];

    loop {
        if STOP.load(Ordering::Relaxed) {
            eprintln!("[vmnet] stopping");
            return Ok(());
        }
        let timeout_ms = if event.is_some() { 100 } else { 0 };
        let hits = poll_once(&listener, event.as_ref(), &table, timeout_ms)?;
        if hits.accept {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => adopt(&mut table, stream),
                    Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                    Err(err) => {
                        eprintln!("[vmnet] accept: {err}");
                        break;
                    }
                }
            }
        }

        let mut dead = hits.hup;
        let mut vmnet_writes = Vec::new();
        for idx in hits.read {
            if dead.contains(&idx) {
                continue;
            }
            let n = match table.lives[idx].stream.read(&mut read_buf) {
                Ok(0) => {
                    dead.push(idx);
                    continue;
                }
                Ok(n) => n,
                Err(err)
                    if err.kind() == io::ErrorKind::WouldBlock
                        || err.kind() == io::ErrorKind::Interrupted =>
                {
                    continue;
                }
                Err(_) => {
                    dead.push(idx);
                    continue;
                }
            };
            let (close, frames) = table.sw.ingest(idx, &read_buf[..n]);
            vmnet_writes.extend(frames);
            if close {
                dead.push(idx);
            }
        }

        if hits.vmnet {
            if let Some(pipe) = event.as_ref() {
                drain_pipe(pipe.read);
            }
            drain_vmnet(port, &mut vmnet_buf, &mut table.sw, resolvers, dns_scope)?;
        } else if event.is_none() {
            drain_vmnet(port, &mut vmnet_buf, &mut table.sw, resolvers, dns_scope)?;
        }

        for frame in vmnet_writes {
            table.sw.tx_frames += 1;
            match port.write(&frame) {
                IoOutcome::Fatal(message) => return Err(message),
                _ => {}
            }
        }
        flush_outputs(&mut table, &mut dead);
        dead.sort_unstable();
        dead.dedup();
        for idx in dead.into_iter().rev() {
            if idx < table.lives.len() {
                table.remove(idx);
            }
        }

        if last_log.elapsed() >= Duration::from_secs(60) {
            last_log = Instant::now();
            log_counters(&table, port.vmnet_err);
        }
        if event.is_none() {
            thread::sleep(Duration::from_micros(100));
        }
    }
}

struct Hits {
    accept: bool,
    vmnet: bool,
    read: Vec<usize>,
    hup: Vec<usize>,
}

fn poll_once(
    listener: &UnixListener,
    event: Option<&EventPipe>,
    table: &Table,
    timeout_ms: i32,
) -> Result<Hits, String> {
    let mut fds = Vec::with_capacity(2 + table.lives.len());
    fds.push(libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    });
    if let Some(pipe) = event {
        fds.push(libc::pollfd {
            fd: pipe.read,
            events: libc::POLLIN,
            revents: 0,
        });
    }
    for (idx, live) in table.lives.iter().enumerate() {
        let mut events = libc::POLLIN;
        let out = &table.sw.clients[idx].out;
        if out.queued() > 0 || !out.outbound().is_empty() {
            events |= libc::POLLOUT;
        }
        fds.push(libc::pollfd {
            fd: live.stream.as_raw_fd(),
            events,
            revents: 0,
        });
    }
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
    if rc < 0 {
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::Interrupted {
            return Ok(Hits {
                accept: false,
                vmnet: false,
                read: Vec::new(),
                hup: Vec::new(),
            });
        }
        return Err(format!("poll: {err}"));
    }
    let mut hits = Hits {
        accept: fds[0].revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0,
        vmnet: false,
        read: Vec::new(),
        hup: Vec::new(),
    };
    let client_base = if event.is_some() {
        hits.vmnet = fds[1].revents & libc::POLLIN != 0;
        2
    } else {
        1
    };
    for (idx, pfd) in fds.iter().enumerate().skip(client_base) {
        let client = idx - client_base;
        if pfd.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
            && pfd.revents & libc::POLLIN == 0
        {
            hits.hup.push(client);
        }
        if pfd.revents & libc::POLLIN != 0 {
            hits.read.push(client);
        }
    }
    Ok(hits)
}

fn flush_outputs(table: &mut Table, dead: &mut Vec<usize>) {
    let Table { sw, lives, .. } = table;
    for idx in 0..lives.len() {
        let pending = sw.clients[idx].out.queued() > 0 || !sw.clients[idx].out.outbound().is_empty();
        if !pending {
            continue;
        }
        let ok = sw.clients[idx]
            .out
            .flush_outbound(&mut |buf| lives[idx].stream.write(buf))
            .is_ok();
        if !ok {
            dead.push(idx);
        }
    }
}

fn log_counters(table: &Table, vmnet_err: u64) {
    let (rx, tx, rx_over, runt, over, unclaimed, reflect, spoof, slow) = table.sw.snapshot();
    if rx == 0
        && tx == 0
        && rx_over == 0
        && runt == 0
        && over == 0
        && unclaimed == 0
        && reflect == 0
        && spoof == 0
        && slow == 0
        && vmnet_err == 0
    {
        return;
    }
    eprintln!(
        "[vmnet] rx_frames={rx} tx_frames={tx} rx_oversize={rx_over} frame_runt={runt} frame_oversize={over} rx_unclaimed={unclaimed} rx_reflect={reflect} tx_spoof={spoof} drop_slow_client={slow} vmnet_err={vmnet_err} clients={}",
        table.lives.len()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mac(last: u8) -> [u8; 6] {
        [0x52, 0x54, 0x00, 0x12, 0x34, last]
    }

    fn frame(dst: [u8; 6], src: [u8; 6]) -> Vec<u8> {
        let mut packet = vec![0u8; 64];
        packet[..6].copy_from_slice(&dst);
        packet[6..12].copy_from_slice(&src);
        packet
    }

    fn take_frames(queue: &mut OutQueue) -> Vec<Vec<u8>> {
        let mut raw = Vec::new();
        queue
            .flush_outbound(&mut |buf| {
                raw.extend_from_slice(buf);
                Ok(buf.len())
            })
            .unwrap();
        let mut frames = Vec::new();
        let mut offset = 0;
        while offset + 4 <= raw.len() {
            let n = u32::from_le_bytes(raw[offset..offset + 4].try_into().unwrap()) as usize;
            offset += 4;
            frames.push(raw[offset..offset + n].to_vec());
            offset += n;
        }
        assert_eq!(offset, raw.len());
        frames
    }

    fn bind_closed_socket(path: &Path) {
        unsafe {
            let fd = libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0);
            assert!(fd >= 0);
            let mut addr: libc::sockaddr_un = std::mem::zeroed();
            let bytes = path.as_os_str().as_bytes();
            assert!(bytes.len() < addr.sun_path.len());
            addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
            for (i, byte) in bytes.iter().enumerate() {
                addr.sun_path[i] = *byte as libc::c_char;
            }
            let len = (2 + bytes.len() + 1) as libc::socklen_t;
            addr.sun_len = len as u8;
            let rc = libc::bind(fd, &addr as *const _ as *const libc::sockaddr, len);
            assert_eq!(rc, 0, "{}", io::Error::last_os_error());
            libc::close(fd);
        }
    }

    #[test]
    fn bridge_flag_selects_lan_mode() {
        let args = |parts: &[&str]| parts.iter().map(|part| part.to_string()).collect::<Vec<_>>();
        let shared = parse_service_args(&args(&["vmm", "--vmnet-service"])).unwrap();
        assert!(matches!(shared.net, ServiceNet::Shared));
        assert!(shared.socket.is_none());

        let lan = parse_service_args(&args(&["vmm", "--vmnet-service", "--bridge"])).unwrap();
        assert!(matches!(lan.net, ServiceNet::Bridged { iface: None }));

        let en = parse_service_args(&args(&[
            "vmm",
            "--vmnet-service",
            "--vmnet-socket",
            "/tmp/vmm.sock",
            "--bridge",
            "en0",
        ]))
        .unwrap();
        assert!(matches!(en.net, ServiceNet::Bridged { iface: Some(ref name) } if name == "en0"));
        assert_eq!(en.socket.unwrap(), PathBuf::from("/tmp/vmm.sock"));

        assert!(parse_service_args(&args(&["vmm", "--vmnet-service", "--bridge", "--bridge"])).is_err());
        assert!(parse_service_args(&args(&["vmm", "--vmnet-service", "--nat"])).is_err());
    }

    #[test]
    fn subnet_cidr_sets_gateway_and_dhcp_pool() {
        let net = parse_subnet("192.168.18.0/24").unwrap();
        assert_eq!(net.cidr, "192.168.18.0/24");
        assert_eq!(net.gateway, "192.168.18.1");
        assert_eq!(net.dhcp_first, "192.168.18.2");
        assert_eq!(net.end, "192.168.18.254");
        assert_eq!(net.mask, "255.255.255.0");

        let wide = parse_subnet("10.1.0.0/16").unwrap();
        assert_eq!(wide.gateway, "10.1.0.1");
        assert_eq!(wide.end, "10.1.255.254");
        assert_eq!(wide.mask, "255.255.0.0");

        assert!(parse_subnet("192.168.18.1/24").is_err());
        assert!(parse_subnet("8.8.8.0/24").is_err());
        assert!(parse_subnet("192.168.18.0/31").is_err());
        assert!(parse_subnet("192.168.18.0").is_err());

        let args = |parts: &[&str]| parts.iter().map(|part| part.to_string()).collect::<Vec<_>>();
        let parsed = parse_service_args(&args(&[
            "vmm",
            "--vmnet-service",
            "--subnet",
            "192.168.18.0/24",
        ]))
        .unwrap();
        assert_eq!(parsed.subnet.unwrap().cidr, "192.168.18.0/24");
        assert!(parse_service_args(&args(&[
            "vmm",
            "--vmnet-service",
            "--bridge",
            "--subnet",
            "192.168.18.0/24",
        ]))
        .is_err());
    }

    #[test]
    fn shared_gateway_names_bridge100() {
        let text = "\
en0: flags=8863<UP,BROADCAST,SMART,RUNNING,SIMPLEX,MULTICAST> mtu 1500
\tinet 192.168.50.37 netmask 0xffffff00 broadcast 192.168.50.255
bridge100: flags=8a63<UP,BROADCAST,SMART,RUNNING,ALLMULTI,SIMPLEX,MULTICAST> mtu 1500
\tinet 192.168.2.1 netmask 0xffffff00 broadcast 192.168.2.255
";
        assert_eq!(
            interface_with_inet(text, "192.168.2.1".parse().unwrap()).as_deref(),
            Some("bridge100")
        );
        assert_eq!(
            interface_with_inet(text, "192.168.50.37".parse().unwrap()).as_deref(),
            Some("en0")
        );
        assert!(interface_with_inet(text, "10.0.0.1".parse().unwrap()).is_none());
    }

    #[test]
    fn bridge100_inet_line_is_the_shared_range() {
        let text = "\
bridge100: flags=8a63<UP,BROADCAST,SMART,RUNNING,ALLMULTI,SIMPLEX,MULTICAST> mtu 1500
\tinet 192.168.2.1 netmask 0xffffff00 broadcast 192.168.2.255
\tinet6 fe80::1%bridge100 prefixlen 64 scopeid 0x1c
";
        assert_eq!(
            describe_ifconfig_inet(text).as_deref(),
            Some("192.168.2.0/24 gateway 192.168.2.1")
        );
        assert!(describe_ifconfig_inet("inet6 fe80::1 prefixlen 64\n").is_none());
    }

    #[test]
    fn listen_socket_mode_comes_from_the_path() {
        let dir = crate::test_support::TestDir::new();
        let path = dir.path("mode.sock");
        let uid = unsafe { libc::getuid() };
        let gid = unsafe { libc::getgid() };
        let listener = bind_listener(&path, uid, gid).unwrap();

        let meta = fs::symlink_metadata(&path).unwrap();
        assert!(meta.file_type().is_socket());
        assert!(!meta.file_type().is_symlink());
        assert_eq!(meta.mode() & 0o777, 0o600);
        assert_eq!(meta.uid(), uid);
        assert_eq!(meta.gid(), gid);

        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::fstat(listener.as_raw_fd(), &mut st) }, 0);
        assert_ne!(st.st_mode & 0o777, 0o600);
        assert_ne!(st.st_ino, meta.ino());

        let _client = UnixStream::connect(&path).unwrap();
        let (_accepted, _) = listener.accept().unwrap();
    }

    #[test]
    fn stale_socket_probe_unlinks_only_a_dead_socket() {
        let dir = crate::test_support::TestDir::new();
        let missing = dir.path("missing.sock");
        assert_eq!(probe_socket(&missing).unwrap(), Probe::Absent);

        let loose = dir.path("loose");
        fs::create_dir(&loose).unwrap();
        fs::set_permissions(&loose, fs::Permissions::from_mode(0o777)).unwrap();
        let dead = loose.join("dead.sock");
        bind_closed_socket(&dead);
        assert!(dead.exists());
        assert_eq!(probe_socket(&dead).unwrap(), Probe::RemovedStale);
        assert!(!dead.exists());
        assert!(parent_allows_bind(&dead, unsafe { libc::getuid() }).is_err());

        let link = dir.path("link.sock");
        std::os::unix::fs::symlink("nowhere", &link).unwrap();
        assert!(probe_socket(&link).is_err());
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());

        let live = dir.path("live.sock");
        let listener = UnixListener::bind(&live).unwrap();
        assert_eq!(probe_socket(&live).unwrap(), Probe::AlreadyListening);
        assert!(live.exists());
        let (mut accepted, _) = listener.accept().unwrap();
        let mut reg = Registry::new(unsafe { libc::getuid() });
        assert!(matches!(
            finish_hello(&mut accepted, unsafe { libc::getuid() }, &mut reg),
            Admit::Closed
        ));
        assert!(reg.is_empty());
    }

    #[test]
    fn enqueue_rx_unicast_broadcast_and_unclaimed() {
        let mut sw = ServiceSwitch::new();
        sw.attach(mac(1), 2048);
        sw.attach(mac(2), 2048);
        let host = mac(9);

        sw.enqueue_rx(&frame(mac(1), host));
        assert_eq!(take_frames(&mut sw.clients[0].out).len(), 1);
        assert!(take_frames(&mut sw.clients[1].out).is_empty());

        sw.enqueue_rx(&frame([0xff; 6], host));
        assert_eq!(take_frames(&mut sw.clients[0].out).len(), 1);
        assert_eq!(take_frames(&mut sw.clients[1].out).len(), 1);

        sw.enqueue_rx(&frame(mac(7), host));
        assert_eq!(sw.rx_unclaimed, 1);
        assert!(take_frames(&mut sw.clients[0].out).is_empty());
        assert!(take_frames(&mut sw.clients[1].out).is_empty());
    }

    #[test]
    fn local_switch_copies_to_other_clients_not_sender() {
        let mut sw = ServiceSwitch::new();
        sw.attach(mac(1), 2048);
        sw.attach(mac(2), 2048);

        sw.local_switch(&mac(1), &frame([0xff; 6], mac(1)));
        assert!(take_frames(&mut sw.clients[0].out).is_empty());
        assert_eq!(take_frames(&mut sw.clients[1].out).len(), 1);

        sw.local_switch(&mac(1), &frame(mac(2), mac(1)));
        assert!(take_frames(&mut sw.clients[0].out).is_empty());
        let got = take_frames(&mut sw.clients[1].out);
        assert_eq!(got.len(), 1);
        assert_eq!(&got[0][..12], &frame(mac(2), mac(1))[..12]);

        sw.local_switch(&mac(1), &frame(mac(7), mac(1)));
        assert!(take_frames(&mut sw.clients[0].out).is_empty());
        assert!(take_frames(&mut sw.clients[1].out).is_empty());
        assert_eq!(sw.rx_unclaimed, 0);
        assert_eq!(sw.rx_frames, 0);
    }

    #[test]
    fn reflected_source_is_dropped() {
        let mut sw = ServiceSwitch::new();
        sw.attach(mac(1), 2048);
        sw.enqueue_rx(&frame([0xff; 6], mac(1)));
        assert_eq!(sw.rx_reflect, 1);
        assert!(take_frames(&mut sw.clients[0].out).is_empty());
        assert_eq!(sw.rx_frames, 0);
    }

    #[test]
    fn outq_keeps_partial_record_and_drops_65th() {
        let mut sw = ServiceSwitch::new();
        sw.attach(mac(1), 2048);
        sw.attach(mac(2), 2048);
        for i in 0..64 {
            let mut packet = frame(mac(2), mac(1));
            packet[20] = i as u8;
            sw.local_switch(&mac(1), &packet);
        }
        assert_eq!(sw.clients[1].out.queued(), 64);
        sw.local_switch(&mac(1), &frame(mac(2), mac(1)));
        assert_eq!(sw.clients[1].out.drop_slow_client, 1);
        assert_eq!(sw.clients[1].out.queued(), 64);
        assert!(sw.clients[0].out.queued() == 0);

        let mut written = Vec::new();
        sw.clients[1]
            .out
            .flush_outbound(&mut |buf| {
                if written.len() >= 3 {
                    return Err(io::Error::new(io::ErrorKind::WouldBlock, "full"));
                }
                let n = (3 - written.len()).min(buf.len());
                written.extend_from_slice(&buf[..n]);
                Ok(n)
            })
            .unwrap();
        assert_eq!(written.len(), 3);
        assert!(!sw.clients[1].out.outbound().is_empty());
        assert_eq!(sw.clients[1].out.queued(), 63);

        sw.clients[1]
            .out
            .flush_outbound(&mut |buf| {
                written.extend_from_slice(buf);
                Ok(buf.len())
            })
            .unwrap();
        let mut ingress = ClientIngress::new(mac(1), 2048);
        let records = ingress.push(&written);
        assert_eq!(records.len(), 64);
        assert!(matches!(records[0], ClientRecord::Forward(_)));
        assert!(sw.clients[1].out.outbound().is_empty());
    }
}
