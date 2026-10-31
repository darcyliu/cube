//! v1 command line. Parsing does not call the hypervisor.
//!
//! The variable-store path is `<first writable disk>.vars.fd`. It is never
//! derived from the firmware file: renaming the code flash must not move NVRAM.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use crate::devices::display;
use crate::forward::ForwardRule;
use crate::shared_fs::ShareConfig;

pub const DEFAULT_RAM_MB: u32 = 1024;
pub const MIN_RAM_MB: u32 = 128;
/// PCI MMIO starts at `0xC0000000`. RAM is `[0, size)` and must not cross it.
pub const MAX_RAM_MB: u32 = 3072;
pub const DEFAULT_CPUS: u32 = 1;
pub const MAX_CPUS: u32 = 16;

/// One virtio-blk image. `cow` uses `<path>.cow` and cannot be combined with `read_only`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskSpec {
    pub path: PathBuf,
    pub cow: bool,
    pub read_only: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Boot {
    Payload(Option<PathBuf>),
    Uefi(PathBuf),
}

/// `--net` omitted stays `none`, so the payload and OVMF smokes do not need root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetConfig {
    pub mode: String,
    pub mac: [u8; 6],
    pub socket: Option<PathBuf>,
    pub ssh_port: Option<u16>,
}

impl Default for NetConfig {
    fn default() -> Self {
        Self {
            mode: "none".into(),
            mac: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
            socket: None,
            ssh_port: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub boot: Boot,
    pub serial_log: Option<PathBuf>,
    pub cpus: u32,
    pub ram_mb: Option<u32>,
    pub disks: Vec<DiskSpec>,
    pub efi_vars: Option<PathBuf>,
    pub net: NetConfig,
    pub forwards: Vec<ForwardRule>,
    pub share: Option<ShareConfig>,
    pub vnc: Option<SocketAddr>,
    /// Guest framebuffer. Doubled when `--retina` is set; `None` is 1280x720.
    pub display: Option<(u32, u32)>,
    /// `--gui native`.
    pub gui: bool,
    /// Cocoa point size. `--retina` does not change this.
    pub window: (u32, u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cli {
    Help,
    Error(String),
    Run(Run),
}

pub fn ram_bytes(mb: u32) -> Result<usize, String> {
    if !(MIN_RAM_MB..=MAX_RAM_MB).contains(&mb) {
        return Err(format!(
            "--ram must be from {MIN_RAM_MB} to {MAX_RAM_MB} MiB"
        ));
    }
    Ok((mb as usize) * 1024 * 1024)
}

/// `--efi-vars` wins. Otherwise the first writable `--disk`, else the only
/// image, else `efi-vars.fd` in the working directory. No firmware path.
pub fn efi_store_path(explicit: Option<&Path>, disks: &[DiskSpec]) -> PathBuf {
    if let Some(path) = explicit {
        return path.to_path_buf();
    }
    let disk = disks
        .iter()
        .find(|disk| !disk.read_only)
        .or_else(|| disks.first());
    match disk {
        Some(disk) => with_suffix(&disk.path, ".vars.fd"),
        None => PathBuf::from("efi-vars.fd"),
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

pub fn help_text() -> &'static str {
    "\
vmm-intel: Intel-only x86_64 guest on macOS 10.15+ (Hypervisor.framework VMX).

This fork does not run on Apple Silicon and must not be merged back into the
AArch64 VMM.

Usage:
  vmm --uefi [PATH]
  vmm --firmware [PATH]
                         Firmware boot. PATH defaults to firmware/OVMF_CODE.fd.

Options:
  --disk, -d PATH[,cow][,ro]   virtio-blk image, repeatable
  --cdrom, --iso PATH          read-only virtio-blk, attached before --disk
  --cow                        PATH.cow overlay for every --disk (not with ro)
  --cpus, -c N                 vCPUs (default 1, max 16)
  --ram, -m MB                 guest RAM in MiB (default 1024, max 3072)
  --net, -n MODE               vmnet | vmnet-proxy | bridge[:iface] | bridged[:iface] | tap:PATH | mock | none
  --vmnet-socket PATH          proxy socket for --net vmnet-proxy
  --mac ADDR                   guest MAC address
  --forward RULE               TCP HOST_IP:PORT=GUEST_IP:PORT
  --ssh-port PORT              localhost PORT to the guest DHCP address port 22
  --efi-vars PATH              variable store (default <first writable disk>.vars.fd, otherwise efi-vars.fd)
  --share TAG PATH             one read-only virtio-9p folder
  --share-rw TAG PATH          one read-write virtio-9p folder
  --vnc [ADDR]                 loopback RFB server (default 127.0.0.1:5900)
  --gui native                 open a Cocoa window
                               UEFI with --vnc or --gui native attaches xHCI HID at 00:0a.0 (INTx GSI 24)
  --retina                     double the guest framebuffer; requires --gui native
  --display WIDTHxHEIGHT       framebuffer size; requires --vnc or --gui native
                               (default 1280x720, max 256 MiB)
  --serial-log, --console-log PATH
  -h, --help                   print this help and exit 1

With no arguments, vmm prints this help and exits 1.

  --payload [PATH]             developer long-mode guest; not the firmware boot."
}

pub fn parse(args: &[String]) -> Cli {
    if args.is_empty() {
        return Cli::Help;
    }
    let mut payload: Option<Option<PathBuf>> = None;
    let mut uefi: Option<PathBuf> = None;
    let mut serial_log = None;
    let mut cpus = DEFAULT_CPUS;
    let mut ram_mb = None;
    let mut cdrom: Option<PathBuf> = None;
    let mut disks = Vec::new();
    let mut global_cow = false;
    let mut efi_vars = None;
    let mut share = None;
    let mut net_mode: Option<String> = None;
    let mut vmnet_socket: Option<PathBuf> = None;
    let mut mac = None;
    let mut forwards = Vec::new();
    let mut ssh_port = None;
    let mut vnc = None;
    let mut display = None;
    let mut gui = false;
    let mut retina = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => return Cli::Help,
            "--cpus" | "-c" => {
                let Some(text) = value_at(args, &mut i) else {
                    return Cli::Error("--cpus needs a count".into());
                };
                let Ok(n) = text.parse::<u32>() else {
                    return Cli::Error("--cpus needs a count".into());
                };
                if !(1..=MAX_CPUS).contains(&n) {
                    return Cli::Error("--cpus must be from 1 to 16".into());
                }
                cpus = n;
            }
            "--ram" | "-m" => {
                let Some(text) = value_at(args, &mut i) else {
                    return Cli::Error("--ram needs a size in MiB".into());
                };
                let Ok(n) = text.parse::<u32>() else {
                    return Cli::Error("--ram needs a size in MiB".into());
                };
                if ram_bytes(n).is_err() {
                    return Cli::Error(format!(
                        "--ram must be from {MIN_RAM_MB} to {MAX_RAM_MB} MiB"
                    ));
                }
                ram_mb = Some(n);
            }
            "--payload" => {
                let path = args
                    .get(i + 1)
                    .filter(|next| !next.starts_with('-'))
                    .cloned();
                if path.is_some() {
                    i += 1;
                }
                payload = Some(path.map(PathBuf::from));
            }
            "--uefi" | "--firmware" => {
                if uefi.is_some() {
                    return Cli::Error("pass only one of --uefi and --firmware".into());
                }
                let path = args
                    .get(i + 1)
                    .filter(|next| !next.starts_with('-'))
                    .cloned();
                if path.is_some() {
                    i += 1;
                }
                uefi = Some(PathBuf::from(
                    path.unwrap_or_else(|| "firmware/OVMF_CODE.fd".to_string()),
                ));
            }
            "--cdrom" | "--iso" => {
                if cdrom.is_some() {
                    return Cli::Error("pass only one of --cdrom and --iso".into());
                }
                let Some(path) = value_at(args, &mut i) else {
                    return Cli::Error("--cdrom needs a path".into());
                };
                cdrom = Some(PathBuf::from(path));
            }
            "--disk" | "-d" => {
                let Some(text) = value_at(args, &mut i) else {
                    return Cli::Error("--disk needs a path".into());
                };
                match parse_disk(text) {
                    Ok(disk) => disks.push(disk),
                    Err(message) => return Cli::Error(message),
                }
            }
            "--cow" => global_cow = true,
            "--efi-vars" => {
                let Some(path) = value_at(args, &mut i) else {
                    return Cli::Error("--efi-vars needs a path".into());
                };
                efi_vars = Some(PathBuf::from(path));
            }
            "--share" | "--share-rw" => {
                if share.is_some() {
                    return Cli::Error("only one shared folder is supported".into());
                }
                let read_only = args[i] == "--share";
                let Some(tag) = value_at(args, &mut i) else {
                    return Cli::Error("--share/--share-rw requires TAG PATH".into());
                };
                let Some(path) = value_at(args, &mut i) else {
                    return Cli::Error("--share/--share-rw requires TAG PATH".into());
                };
                match ShareConfig::new(tag, path, read_only) {
                    Ok(config) => share = Some(config),
                    Err(message) => return Cli::Error(message),
                }
            }
            "--serial-log" | "--console-log" => {
                let Some(path) = value_at(args, &mut i) else {
                    return Cli::Error("--serial-log needs a path".into());
                };
                serial_log = Some(PathBuf::from(path));
            }
            "--net" | "-n" => {
                let Some(text) = value_at(args, &mut i) else {
                    return Cli::Error("--net needs a mode".into());
                };
                if let Err(message) = parse_net_mode(text) {
                    return Cli::Error(message);
                }
                net_mode = Some(text.to_string());
            }
            "--vmnet-socket" => {
                let Some(path) = value_at(args, &mut i) else {
                    return Cli::Error("--vmnet-socket needs a path".into());
                };
                vmnet_socket = Some(PathBuf::from(path));
            }
            "--mac" => {
                let Some(text) = value_at(args, &mut i) else {
                    return Cli::Error("--mac needs an address".into());
                };
                match parse_mac(text) {
                    Ok(bytes) => mac = Some(bytes),
                    Err(_) => {
                        return Cli::Error(format!("--mac needs six hex bytes, not {text}"));
                    }
                }
            }
            "--forward" => {
                let Some(text) = value_at(args, &mut i) else {
                    return Cli::Error("--forward needs HOST_IP:PORT=GUEST_IP:PORT".into());
                };
                match ForwardRule::parse(text) {
                    Ok(rule) => forwards.push(rule),
                    Err(message) => return Cli::Error(message),
                }
            }
            "--ssh-port" => {
                let Some(text) = value_at(args, &mut i) else {
                    return Cli::Error("--ssh-port needs a nonzero port".into());
                };
                match text.parse::<u16>() {
                    Ok(port) if port != 0 => ssh_port = Some(port),
                    _ => return Cli::Error("--ssh-port needs a nonzero port".into()),
                }
            }
            "--vnc" => {
                let text = args
                    .get(i + 1)
                    .filter(|next| !next.starts_with('-'))
                    .map(|value| value.as_str());
                let spec = if let Some(text) = text {
                    i += 1;
                    text
                } else {
                    "127.0.0.1:5900"
                };
                match parse_vnc(spec) {
                    Ok(addr) => vnc = Some(addr),
                    Err(message) => return Cli::Error(message),
                }
            }
            "--display" => {
                let Some(text) = value_at(args, &mut i) else {
                    return Cli::Error("--display requires WIDTHxHEIGHT".into());
                };
                match display::parse_size(text) {
                    Ok(size) => display = Some(size),
                    Err(message) => return Cli::Error(format!("--display {message}")),
                }
            }
            "--gui" => {
                let Some(text) = value_at(args, &mut i) else {
                    return Cli::Error("--gui requires 'native'".into());
                };
                if text != "native" {
                    return Cli::Error("--gui requires 'native'".into());
                }
                gui = true;
            }
            "--retina" => retina = true,
            other => return Cli::Error(format!("unknown argument {other}")),
        }
        i += 1;
    }

    if gui && vnc.is_some() {
        return Cli::Error("--gui native and --vnc cannot be combined".into());
    }
    if retina && !gui {
        return Cli::Error("--retina requires --gui native".into());
    }
    if display.is_some() && vnc.is_none() && !gui {
        return Cli::Error("--display requires --vnc or --gui native".into());
    }

    let boot = match (payload, uefi) {
        (Some(_), Some(_)) => {
            return Cli::Error("pass only one of --payload and --uefi/--firmware".into());
        }
        (None, None) => return Cli::Error("no boot path; pass --uefi".into()),
        (Some(path), None) => Boot::Payload(path),
        (None, Some(path)) => Boot::Uefi(path),
    };
    if gui && matches!(boot, Boot::Payload(_)) {
        return Cli::Error("--gui native requires --uefi".into());
    }
    let window = display.unwrap_or((display::FB_WIDTH, display::FB_HEIGHT));
    if retina {
        match retina_framebuffer_size(window) {
            Ok(doubled) => display = Some(doubled),
            Err(message) => return Cli::Error(message),
        }
    }
    // CD-ROM stays read-only. cow and ro on the same image is rejected.
    if global_cow {
        if disks.is_empty() {
            return Cli::Error("--cow needs a --disk".into());
        }
        for disk in &mut disks {
            if disk.read_only {
                return Cli::Error("--disk cannot combine cow and ro".into());
            }
            disk.cow = true;
        }
    }
    let mut net = NetConfig::default();
    if let Some(mode) = net_mode {
        net.mode = mode;
    }
    if let Some(bytes) = mac {
        net.mac = bytes;
    }
    net.socket = vmnet_socket;
    net.ssh_port = ssh_port;
    let mut images = Vec::new();
    // The ISO is vtbd0 even when --disk appears earlier on the command line.
    if let Some(path) = cdrom {
        images.push(DiskSpec {
            path,
            cow: false,
            read_only: true,
        });
    }
    images.extend(disks);
    Cli::Run(Run {
        boot,
        serial_log,
        cpus,
        ram_mb,
        disks: images,
        efi_vars,
        net,
        forwards,
        share,
        vnc,
        display,
        gui,
        window,
    })
}

fn retina_framebuffer_size(window_size: (u32, u32)) -> Result<(u32, u32), String> {
    let width = window_size
        .0
        .checked_mul(2)
        .ok_or_else(|| "--retina width overflow".to_string())?;
    let height = window_size
        .1
        .checked_mul(2)
        .ok_or_else(|| "--retina height overflow".to_string())?;
    display::parse_size(&format!("{width}x{height}"))
        .map_err(|message| format!("--retina {message}"))
}

fn parse_vnc(text: &str) -> Result<SocketAddr, String> {
    let addr: SocketAddr = text
        .parse()
        .map_err(|_| format!("--vnc needs a loopback socket address, not {text}"))?;
    if !addr.ip().is_loopback() {
        return Err(format!(
            "--vnc must bind to loopback, not {text}; use an SSH tunnel for remote access"
        ));
    }
    Ok(addr)
}

fn value_at<'a>(args: &'a [String], i: &mut usize) -> Option<&'a str> {
    let text = args.get(*i + 1)?;
    if text.starts_with('-') {
        return None;
    }
    *i += 1;
    Some(text)
}

fn parse_disk(text: &str) -> Result<DiskSpec, String> {
    let mut parts = text.split(',');
    let path = parts.next().unwrap_or("");
    if path.is_empty() {
        return Err("--disk needs a path".into());
    }
    let mut cow = false;
    let mut read_only = false;
    for flag in parts {
        match flag {
            "cow" => cow = true,
            "ro" => read_only = true,
            _ => return Err(format!("--disk flag {flag} is not cow or ro")),
        }
    }
    if cow && read_only {
        return Err("--disk cannot combine cow and ro".into());
    }
    Ok(DiskSpec {
        path: PathBuf::from(path),
        cow,
        read_only,
    })
}

fn parse_net_mode(text: &str) -> Result<(), String> {
    let ok = matches!(
        text,
        "vmnet" | "vmnet-proxy" | "mock" | "none" | "bridge" | "bridged"
    ) || text
        .strip_prefix("bridge:")
        .or_else(|| text.strip_prefix("bridged:"))
        .is_some_and(|iface| !iface.is_empty())
        || text
            .strip_prefix("tap:")
            .is_some_and(|path| !path.is_empty());
    if ok {
        Ok(())
    } else {
        Err("--net needs vmnet, vmnet-proxy, bridge[:iface], tap:PATH, mock, or none".into())
    }
}

fn parse_mac(text: &str) -> Result<[u8; 6], String> {
    let parts: Vec<&str> = text.split(|c| c == ':' || c == '-').collect();
    if parts.len() != 6 {
        return Err(text.into());
    }
    let mut mac = [0u8; 6];
    for (i, part) in parts.iter().enumerate() {
        if part.is_empty() || part.len() > 2 {
            return Err(text.into());
        }
        mac[i] = u8::from_str_radix(part, 16).map_err(|_| text.to_string())?;
    }
    Ok(mac)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    fn run(values: &[&str]) -> Run {
        match parse(&args(values)) {
            Cli::Run(run) => run,
            other => panic!("expected run, got {other:?}"),
        }
    }

    fn error(values: &[&str]) -> String {
        match parse(&args(values)) {
            Cli::Error(message) => message,
            other => panic!("expected error, got {other:?}"),
        }
    }

    #[test]
    fn help_lists_v1_flags_and_hides_the_dropped_ones() {
        let help = help_text();
        for flag in [
            "--disk",
            "-d",
            "--cdrom",
            "--iso",
            "--cow",
            "--cpus",
            "-c",
            "--ram",
            "-m",
            "--net",
            "-n",
            "--vmnet-socket",
            "--mac",
            "--forward",
            "--ssh-port",
            "--uefi",
            "--firmware",
            "--efi-vars",
            "--share",
            "--share-rw",
            "--serial-log",
            "--console-log",
            "--vnc",
            "--display",
            "--gui native",
            "--retina",
        ] {
            assert!(help.contains(flag), "missing {flag}");
        }
        assert!(help.contains("default 1"));
        assert!(help.contains("max 16"));
        assert!(help.contains("default 1024"));
        assert!(help.contains("max 3072"));
        assert!(help.contains("10.15"));
        assert!(help.contains("Firmware boot"));
        assert!(help.contains("<first writable disk>.vars.fd"));
        assert!(help.contains("otherwise efi-vars.fd"));
        assert!(help.contains("127.0.0.1:5900"));
        assert!(help.contains("1280x720"));
        assert!(help.contains("requires --vnc or --gui native"));
        assert!(help.contains("requires --gui native"));
        assert!(help.contains("00:0a.0"));
        assert!(help.contains("GSI 24"));
        assert!(help.contains("max 256 MiB"));
        assert!(help.find("--uefi").unwrap() < help.find("--payload").unwrap());
        for flag in ["--freebsd", "--kernel", "--multiuser"] {
            assert!(!help.contains(flag), "advertised {flag}");
        }
        assert!(!help.contains("Darwin"));
        assert_eq!(parse(&[]), Cli::Help);
        assert_eq!(parse(&args(&["--help"])), Cli::Help);
    }

    #[test]
    fn dropped_flags_are_unknown_and_cow_cannot_combine_with_ro() {
        for flag in ["--freebsd", "--kernel", "--multiuser", "--memory"] {
            assert!(error(&[flag]).contains("unknown argument"), "{flag}");
        }
        assert!(error(&["--uefi", "--disk", "a.img,cow,ro"]).contains("cannot combine"));
        assert!(error(&["--uefi", "--cow", "--disk", "a.img,ro"]).contains("cannot combine"));
        assert!(error(&["--uefi", "--cow", "--cdrom", "a.iso"]).contains("--cow needs a --disk"));
        let cow = run(&["--uefi", "--cdrom", "a.iso", "--cow", "--disk", "disk.img"]);
        assert!(cow.disks[0].read_only && !cow.disks[0].cow);
        assert!(cow.disks[1].cow && !cow.disks[1].read_only);
        assert_eq!(cow.disks[1].path, PathBuf::from("disk.img"));
        assert!(run(&["--uefi", "-d", "disk.img,cow"]).disks[0].cow);
        assert!(error(&["--uefi", "--share"]).contains("TAG PATH"));
        let ro = run(&["--uefi", "--share", "work", "."]);
        assert_eq!(ro.share.as_ref().map(|share| share.tag.as_str()), Some("work"));
        assert!(ro.share.unwrap().read_only);
        let rw = run(&["--uefi", "--share-rw", "work", "."]);
        assert!(!rw.share.unwrap().read_only);
        assert!(
            error(&["--uefi", "--share", "work", ".", "--share-rw", "other", "."])
                .contains("only one")
        );
    }

    #[test]
    fn ram_stays_inside_the_pci_hole() {
        assert_eq!(ram_bytes(1024).unwrap(), 1024 * 1024 * 1024);
        assert_eq!(
            ram_bytes(DEFAULT_RAM_MB).unwrap(),
            crate::x86::UEFI_RAM_BYTES
        );
        assert_eq!(ram_bytes(MAX_RAM_MB).unwrap(), 0xC000_0000);
        assert!(ram_bytes(127).is_err());
        assert!(ram_bytes(3073).is_err());
        assert!(error(&["--uefi", "--ram", "3073"]).contains("3072"));
        assert_eq!(run(&["--uefi", "-m", "1024"]).ram_mb, Some(1024));
        assert_eq!(run(&["--payload"]).ram_mb, None);
        assert_eq!(run(&["--cpus", "16", "--uefi"]).cpus, 16);
        assert!(error(&["--cpus", "17", "--uefi"]).contains("1 to 16"));
    }

    #[test]
    fn efi_store_follows_the_first_disk_not_the_firmware_file() {
        let disks = vec![
            DiskSpec {
                path: PathBuf::from("cd.iso"),
                cow: false,
                read_only: true,
            },
            DiskSpec {
                path: PathBuf::from("vms/disk.img"),
                cow: false,
                read_only: false,
            },
            DiskSpec {
                path: PathBuf::from("other.img"),
                cow: true,
                read_only: false,
            },
        ];
        assert_eq!(
            efi_store_path(None, &disks),
            PathBuf::from("vms/disk.img.vars.fd")
        );
        assert_eq!(
            efi_store_path(Some(Path::new("custom.fd")), &disks),
            PathBuf::from("custom.fd")
        );
        assert_eq!(efi_store_path(None, &[]), PathBuf::from("efi-vars.fd"));
        assert_ne!(
            efi_store_path(None, &disks),
            PathBuf::from("firmware/OVMF_CODE.fd.vars.fd")
        );
        let parsed = run(&[
            "--firmware",
            "firmware/OVMF_CODE.fd",
            "--cdrom",
            "cd.iso",
            "--disk",
            "vms/disk.img",
            "--efi-vars",
            "nvram.fd",
        ]);
        assert_eq!(parsed.efi_vars.as_deref(), Some(Path::new("nvram.fd")));
        assert!(matches!(parsed.boot, Boot::Uefi(ref path) if path.ends_with("OVMF_CODE.fd")));
        assert_eq!(
            efi_store_path(parsed.efi_vars.as_deref(), &parsed.disks),
            PathBuf::from("nvram.fd")
        );
        let implicit = run(&["--uefi", "--disk", "vms/disk.img"]);
        assert_eq!(implicit.efi_vars, None);
        assert_eq!(
            efi_store_path(None, &implicit.disks),
            PathBuf::from("vms/disk.img.vars.fd")
        );
    }

    #[test]
    fn net_flags_are_stored() {
        let none = run(&["--uefi", "--net", "none", "--mac", "52:54:00:12:34:56"]);
        assert_eq!(none.ram_mb, None);
        assert_eq!(none.net.mode, "none");
        assert_eq!(none.net.mac, [0x52, 0x54, 0x00, 0x12, 0x34, 0x56]);
        let proxy = run(&["--uefi", "--net", "vmnet-proxy", "--vmnet-socket", "sock"]);
        assert_eq!(proxy.net.mode, "vmnet-proxy");
        assert_eq!(proxy.net.socket.as_deref(), Some(Path::new("sock")));
        assert_eq!(
            run(&["--uefi", "--ssh-port", "2222"]).net.ssh_port,
            Some(2222)
        );
        assert_eq!(
            run(&["--uefi", "--forward", "127.0.0.1:9=10.0.0.2:22"])
                .forwards
                .len(),
            1
        );
        assert_eq!(
            run(&["--uefi", "--net", "bridged:en0"]).net.mode,
            "bridged:en0"
        );
        assert!(error(&["--uefi", "--net", "tap:"]).contains("--net needs"));
        assert!(error(&["--uefi", "--mac", "nope"]).contains("--mac"));
        assert!(error(&["--uefi", "--ssh-port", "0"]).contains("nonzero"));
    }

    #[test]
    fn vnc_binds_loopback_and_display_requires_it() {
        let plain = run(&["--uefi", "--vnc"]);
        assert_eq!(plain.vnc.unwrap().to_string(), "127.0.0.1:5900");
        assert_eq!(plain.display, None);
        let sized = run(&[
            "--payload",
            "--display",
            "1024x768",
            "--vnc",
            "127.0.0.1:5901",
        ]);
        assert_eq!(sized.vnc.unwrap().to_string(), "127.0.0.1:5901");
        assert_eq!(sized.display, Some((1024, 768)));
        assert!(
            error(&["--display", "1024x768"]).contains("--display requires --vnc or --gui native")
        );
        assert!(
            error(&["--uefi", "--vnc", "--display", "16384x16384"]).contains("maximum is 256 MiB")
        );
        assert!(error(&["--uefi", "--vnc", "0.0.0.0:5900"]).contains("loopback"));
        assert!(error(&["--uefi", "--vnc", "192.168.1.1:5900"]).contains("loopback"));
        assert!(error(&["--uefi", "--display"]).contains("WIDTHxHEIGHT"));
        assert!(error(&["--gui"]).contains("--gui requires 'native'"));
        assert!(error(&["--gui", "cocoa", "--uefi"]).contains("--gui requires 'native'"));
        assert!(error(&["--retina", "--uefi"]).contains("--retina requires --gui native"));
        assert!(error(&["--retina"]).contains("--retina requires --gui native"));
        assert!(error(&["--uefi", "--gui", "native", "--vnc"]).contains("cannot be combined"));
        assert!(error(&["--gui", "native", "--payload"]).contains("requires --uefi"));
        let gui = run(&["--uefi", "--gui", "native", "--display", "1024x768"]);
        assert!(gui.gui);
        assert!(gui.vnc.is_none());
        assert_eq!(gui.display, Some((1024, 768)));
        assert_eq!(gui.window, (1024, 768));
        let retina = run(&[
            "--uefi",
            "--gui",
            "native",
            "--retina",
            "--display",
            "1024x768",
        ]);
        assert_eq!(retina.display, Some((2048, 1536)));
        assert_eq!(retina.window, (1024, 768));
        let retina_default = run(&["--uefi", "--gui", "native", "--retina"]);
        assert_eq!(retina_default.display, Some((2560, 1440)));
        assert_eq!(retina_default.window, (1280, 720));
        assert_eq!(run(&["--uefi", "--gui", "native"]).window, (1280, 720));
        assert!(
            error(&[
                "--uefi",
                "--gui",
                "native",
                "--retina",
                "--display",
                "8192x8192"
            ])
            .contains("maximum is 256 MiB")
        );
        assert!(
            error(&[
                "--uefi",
                "--gui",
                "native",
                "--retina",
                "--display",
                "40000x100"
            ])
            .contains("65535")
        );
        assert!(
            retina_framebuffer_size((u32::MAX, 768))
                .unwrap_err()
                .contains("overflow")
        );
        assert!(retina_framebuffer_size((8192, 8192)).is_err());
    }
}
