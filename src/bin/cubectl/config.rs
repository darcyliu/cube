use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::util::{
    canonical_path_str, default_vms_root, find_cube_binary, require, resolve_path, sha256_bytes,
};
use crate::StartArgs;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmConfig {
    pub name: String,
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default)]
    pub binary: Option<String>,
    pub boot: BootConfig,
    #[serde(default = "default_ram")]
    pub ram_mb: u64,
    #[serde(default = "default_cpus")]
    pub cpus: u32,
    #[serde(default)]
    pub disks: Vec<DiskItem>,
    #[serde(default)]
    pub shares: Vec<ShareItem>,
    #[serde(default)]
    pub network: Option<NetworkConfig>,
    #[serde(default)]
    pub vnc: Option<VncConfig>,
    #[serde(default)]
    pub efi_vars: Option<String>,
    #[serde(default)]
    pub shutdown: Option<ShutdownConfig>,
    #[serde(default)]
    pub no_reboot: bool,
    #[serde(default)]
    pub mem_prealloc: bool,
}

fn default_ram() -> u64 {
    1024
}
fn default_cpus() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootConfig {
    pub mode: String, // "uefi" | "freebsd"
    #[serde(default)]
    pub firmware: Option<String>,
    #[serde(default)]
    pub kernel: Option<String>,
    #[serde(default = "default_true")]
    pub multiuser: bool,
    #[serde(default)]
    pub root: Option<String>,
    #[serde(default)]
    pub bootargs: Option<String>,
    #[serde(default)]
    pub modules: Vec<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DiskItem {
    Simple(String),
    Detailed {
        path: String,
        #[serde(default)]
        cow: bool,
        #[serde(default)]
        read_only: bool,
    },
}

impl DiskItem {
    pub fn path(&self) -> &str {
        match self {
            DiskItem::Simple(p) => p,
            DiskItem::Detailed { path, .. } => path,
        }
    }
    pub fn is_cow(&self) -> bool {
        match self {
            DiskItem::Simple(_) => false,
            DiskItem::Detailed { cow, .. } => *cow,
        }
    }
    pub fn is_read_only(&self) -> bool {
        match self {
            DiskItem::Simple(_) => false,
            DiskItem::Detailed { read_only, .. } => *read_only,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareItem {
    pub tag: String,
    pub path: String,
    #[serde(default = "default_true")]
    pub read_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    #[serde(default = "default_net_mode")]
    pub mode: String,
    #[serde(default)]
    pub mac: Option<String>,
    #[serde(default)]
    pub ssh_port: Option<u16>,
    #[serde(default)]
    pub forwards: Vec<String>,
}

fn default_net_mode() -> String {
    "none".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VncConfig {
    pub bind: String,
    #[serde(default)]
    pub display: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownConfig {
    #[serde(default = "default_host")]
    pub host: String,
    pub port: Option<u16>,
    #[serde(default = "default_user")]
    pub user: String,
    pub identity_file: String,
}

fn default_host() -> String {
    "127.0.0.1".into()
}
fn default_user() -> String {
    "root".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LaunchSpec {
    pub id: String,
    pub name: String,
    pub config: String,
    pub command: Vec<String>,
    pub resources: Vec<String>,
    pub shutdown: Option<ResolvedShutdown>,
    pub shares: Vec<ShareItem>,
    pub sudo: bool,
    #[serde(default)]
    pub interactive: bool,
    #[serde(default)]
    pub ssh_port: Option<u16>,
    #[serde(default)]
    pub vnc: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedShutdown {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub identity_file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceRecord {
    pub id: String,
    pub name: String,
    pub config: String,
    pub pid: u32,
    pub manager_pid: u32,
    pub started: f64,
    pub resources: Vec<String>,
    pub control_socket: String,
    #[serde(default)]
    pub shares: Vec<ShareItem>,
    #[serde(default)]
    pub ssh_port: Option<u16>,
    #[serde(default)]
    pub vnc: Option<String>,
}

pub fn validate_vm_name(name: &str) -> Result<(), String> {
    require(!name.is_empty() && name.len() <= 48, "VM name must be 1–48 characters")?;
    let mut chars = name.chars();
    let first = chars.next().unwrap();
    require(first.is_ascii_alphanumeric(), "name must start with letter or digit")?;
    for c in chars {
        require(c.is_ascii_alphanumeric() || c == '_' || c == '-', "name contains invalid characters")?;
    }
    Ok(())
}

pub fn resolve_boot_file(file_str: &str, base: &Path, binary_path: &Path, file_kind: &str) -> Result<PathBuf, String> {
    // 1. Try relative to base (VM directory)
    if let Ok(p) = resolve_path(file_str, base) {
        if p.is_file() {
            return Ok(p);
        }
    }
    // 2. Try relative to current working directory
    if let Ok(p) = resolve_path(file_str, Path::new(".")) {
        if p.is_file() {
            return Ok(p);
        }
    }
    // 3. Try relative to cube binary directory or its repo/bundle root
    if let Some(bin_dir) = binary_path.parent() {
        let candidate = bin_dir.join(file_str);
        if candidate.is_file() {
            return Ok(candidate);
        }
        if let Some(repo_dir) = bin_dir.parent() {
            let candidate = repo_dir.join(file_str);
            if candidate.is_file() {
                return Ok(candidate);
            }
            if let Some(repo_parent) = repo_dir.parent() {
                let candidate = repo_parent.join(file_str);
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
    }

    Err(format!(
        "{} file not found: {:?}",
        file_kind,
        resolve_path(file_str, base).unwrap_or_else(|_| PathBuf::from(file_str))
    ))
}

pub fn build_launch_spec(
    config_path: &Path,
    config: &VmConfig,
    state_dir: &Path,
    cli_overrides: &StartArgs,
) -> Result<LaunchSpec, String> {
    validate_vm_name(&config.name)?;
    let base = config_path.parent().unwrap_or_else(|| Path::new("."));

    // 1. Resolve binary
    let binary_path = find_cube_binary(cli_overrides.binary.as_deref(), config.binary.as_deref(), base)?;
    let binary_cmd = canonical_path_str(&binary_path)?;

    let mut command = vec![binary_cmd];
    let mut resources = Vec::new();

    // 2. Boot mode
    match config.boot.mode.as_str() {
        "uefi" => {
            let fw = config.boot.firmware.as_ref().ok_or("boot.firmware required for uefi mode")?;
            let fw_path = resolve_boot_file(fw, base, &binary_path, "firmware")?;
            command.push("--uefi".into());
            command.push(canonical_path_str(&fw_path)?);
        }
        "freebsd" => {
            let kern = config.boot.kernel.as_ref().ok_or("boot.kernel required for freebsd mode")?;
            let kern_path = resolve_boot_file(kern, base, &binary_path, "kernel")?;
            command.push("--freebsd".into());
            command.push(canonical_path_str(&kern_path)?);
            if config.boot.multiuser {
                command.push("--multiuser".into());
            }
        }
        other => return Err(format!("unsupported boot.mode '{}'", other)),
    }

    if let Some(ref root) = config.boot.root {
        require(!root.is_empty(), "boot.root must be nonempty")?;
        command.push("--root".into());
        command.push(root.clone());
    }
    if let Some(ref bootargs) = config.boot.bootargs {
        command.push("--bootargs".into());
        command.push(bootargs.clone());
    }
    for m in &config.boot.modules {
        let m_path = resolve_boot_file(m, base, &binary_path, "module")?;
        command.push("--module".into());
        command.push(canonical_path_str(&m_path)?);
    }

    // 3. RAM & vCPUs
    let ram = cli_overrides.ram.unwrap_or(config.ram_mb);
    let cpus = cli_overrides.cpus.unwrap_or(config.cpus);
    require(ram >= 128 && ram <= 65536, "ram_mb must be 128–65536")?;
    require(cpus >= 1 && cpus <= 16, "cpus must be 1–16")?;
    command.extend(["--ram".into(), ram.to_string(), "--cpus".into(), cpus.to_string()]);

    // 4. Disks
    let raw_disks = if !cli_overrides.disks.is_empty() {
        cli_overrides.disks.iter().map(|d| DiskItem::Simple(d.clone())).collect::<Vec<_>>()
    } else {
        config.disks.clone()
    };
    require(!raw_disks.is_empty(), "disks must be nonempty")?;

    for item in &raw_disks {
        let is_cli = !cli_overrides.disks.is_empty();
        let d_base = if is_cli { Path::new(".") } else { base };
        let disk_path = resolve_path(item.path(), d_base)?;
        require(disk_path.is_file(), format!("disk does not exist: {:?}", disk_path))?;
        let cow = item.is_cow();
        let ro = item.is_read_only();
        require(!(cow && ro), "a disk cannot be both cow and read_only")?;

        let canonical_disk = canonical_path_str(&disk_path)?;
        let suffix = if cow { ",cow" } else if ro { ",ro" } else { "" };
        command.push("--disk".into());
        command.push(format!("{}{}", canonical_disk, suffix));

        if !ro {
            resources.push(format!("disk:{}", canonical_disk));
            if cow {
                resources.push(format!("disk:{}.cow", canonical_disk));
            }
        }
    }

    // 5. Shares
    let mut shares = Vec::new();
    let mut tags = HashSet::new();

    let share_list = if !cli_overrides.shares.is_empty() || !cli_overrides.shares_rw.is_empty() {
        let mut list = Vec::new();
        for chunk in cli_overrides.shares.chunks_exact(2) {
            list.push(ShareItem { tag: chunk[0].clone(), path: chunk[1].clone(), read_only: true });
        }
        for chunk in cli_overrides.shares_rw.chunks_exact(2) {
            list.push(ShareItem { tag: chunk[0].clone(), path: chunk[1].clone(), read_only: false });
        }
        list
    } else {
        config.shares.clone()
    };

    for item in &share_list {
        require(item.tag.len() >= 1 && item.tag.len() <= 120, "share tag must be 1–120 characters")?;
        require(tags.insert(item.tag.clone()), format!("duplicate share tag: {}", item.tag))?;
        let s_path = resolve_path(&item.path, base)?;
        require(s_path.is_dir(), format!("share directory does not exist: {:?}", s_path))?;
        let canonical_dir = canonical_path_str(&s_path)?;

        command.push(if item.read_only { "--share".into() } else { "--share-rw".into() });
        command.push(item.tag.clone());
        command.push(canonical_dir.clone());
        shares.push(ShareItem { tag: item.tag.clone(), path: canonical_dir, read_only: item.read_only });
    }

    require(
        raw_disks.len().saturating_sub(1) + shares.len() <= 64,
        "too many disks/shared folders (max 64)",
    )?;

    // 6. Network
    let net = config.network.clone().unwrap_or(NetworkConfig {
        mode: "none".into(),
        mac: None,
        ssh_port: None,
        forwards: Vec::new(),
    });
    let net_mode = cli_overrides.net.clone().unwrap_or(net.mode);
    require(!(cli_overrides.sudo && net_mode == "vmnet-proxy"),
        "--sudo cannot launch vmnet-proxy; omit --sudo and use the vmnet service")?;
    command.extend(["--net".into(), net_mode.clone()]);

    let mac = if let Some(ref m) = cli_overrides.mac {
        m.clone()
    } else if let Some(ref m) = net.mac {
        m.clone()
    } else {
        let digest = sha256_bytes(config.name.as_bytes());
        format!("52:54:{:02x}:{:02x}:{:02x}:{:02x}", digest[0], digest[1], digest[2], digest[3])
    };

    if net_mode != "none" {
        command.extend(["--mac".into(), mac.clone()]);
        resources.push(format!("mac:{}", mac.to_lowercase()));
    }

    let ssh_port = if net_mode == "none" || net_mode == "mock" || net_mode.starts_with("tap:") {
        cli_overrides.ssh_port
    } else {
        cli_overrides.ssh_port.or(net.ssh_port)
    };
    if let Some(port) = ssh_port {
        require(port > 0, "invalid ssh_port")?;
        require(
            net_mode != "none" && net_mode != "mock" && !net_mode.starts_with("tap:"),
            "ssh_port requires vmnet, vmnet-proxy, or bridge",
        )?;
        command.extend(["--ssh-port".into(), port.to_string()]);
        resources.push(format!("listen-port:{}", port));
    }

    let forwards = if !cli_overrides.forwards.is_empty() {
        cli_overrides.forwards.clone()
    } else {
        net.forwards.clone()
    };
    for rule in &forwards {
        require(rule.contains('='), format!("invalid forward rule: {}", rule))?;
        command.extend(["--forward".into(), rule.clone()]);
        let host_part = rule.split('=').next().unwrap();
        let port_part = host_part.rsplit(':').next().unwrap_or(host_part);
        resources.push(format!("listen-port:{}", port_part));
    }

    // 7. VNC & Display
    let mut vnc_bind = cli_overrides.vnc.clone();
    let mut vnc_display = cli_overrides.display.clone();
    if let Some(ref v) = config.vnc {
        if vnc_bind.is_none() {
            vnc_bind = Some(v.bind.clone());
        }
        if vnc_display.is_none() {
            vnc_display = v.display.clone();
        }
    }

    if let Some(ref bind) = vnc_bind {
        command.extend(["--vnc".into(), bind.clone()]);
        let port_part = bind.rsplit(':').next().unwrap_or(bind);
        resources.push(format!("listen-port:{}", port_part));
        if let Some(disp) = vnc_display {
            command.extend(["--display".into(), disp]);
        }
    }

    // 8. EFI vars & console log
    let efi = if let Some(ref e) = cli_overrides.efi_vars {
        resolve_path(&e.to_string_lossy(), Path::new("."))?
    } else if let Some(ref e) = config.efi_vars {
        resolve_path(e, base)?
    } else {
        state_dir.join("efi-vars.fd")
    };
    let efi_str = efi.to_string_lossy().into_owned();
    command.extend(["--efi-vars".into(), efi_str.clone(), "--serial-log".into(), state_dir.join("console.log").to_string_lossy().into_owned()]);
    resources.push(format!("efi:{}", efi_str));

    // 9. No-reboot: enabled via CLI flag, config property, or auto-detected when an ISO is attached
    let has_attached_iso = raw_disks.iter().any(|d| {
        let p = d.path().to_lowercase();
        p.ends_with(".iso") || (d.is_read_only() && p.contains(".iso"))
    });
    if cli_overrides.no_reboot || config.no_reboot || has_attached_iso {
        command.push("--no-reboot".into());
    }

    // 10. Memory pre-allocation: enabled via CLI override or config property
    if cli_overrides.mem_prealloc || config.mem_prealloc {
        command.push("--mem-prealloc".into());
    }

    // 10. Collision check within this spec
    let unique_res: HashSet<_> = resources.iter().cloned().collect();
    require(unique_res.len() == resources.len(), "configuration repeats a VM resource")?;

    // 10. Shutdown config
    let shutdown = if let Some(ref s) = config.shutdown {
        let key_path = resolve_path(&s.identity_file, base)?;
        require(key_path.is_file(), format!("shutdown SSH key does not exist: {:?}", key_path))?;
        Some(ResolvedShutdown {
            host: s.host.clone(),
            port: s.port.or(ssh_port).unwrap_or(22),
            user: s.user.clone(),
            identity_file: canonical_path_str(&key_path)?,
        })
    } else {
        None
    };

    let mut entropy = [0u8; 16];
    getrandom::getrandom(&mut entropy).map_err(|e| e.to_string())?;
    let id = entropy.iter().map(|b| format!("{:02x}", b)).collect();

    Ok(LaunchSpec {
        id,
        name: config.name.clone(),
        config: canonical_path_str(config_path)?,
        command,
        resources,
        shutdown,
        shares,
        sudo: cli_overrides.sudo,
        interactive: cli_overrides.interactive,
        ssh_port,
        vnc: vnc_bind,
    })
}

pub fn find_config_for_target(target: &str, state_root: &Path) -> Result<PathBuf, String> {
    let path = PathBuf::from(target);
    if path.is_file() {
        return Ok(path);
    }

    // Check the creation registration, then metadata from earlier launches.
    let state_dir = state_root.join(target);
    for meta_name in &["registration.json", "launch.json", "last.json"] {
        let meta_file = state_dir.join(meta_name);
        if let Ok(c) = fs::read_to_string(&meta_file) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&c) {
                if let Some(cfg_str) = val.get("config").and_then(|s| s.as_str()) {
                    let p = PathBuf::from(cfg_str);
                    if p.is_file() {
                        return Ok(p);
                    }
                }
            }
        }
    }

    // Check default_vms_root/<target>/<target>.json
    let candidate = default_vms_root().join(target).join(format!("{}.json", target));
    if candidate.is_file() {
        return Ok(candidate);
    }

    // Check fallback ~/Cube/<target>/<target>.json if default_vms_root was custom
    if let Some(home) = std::env::var_os("HOME") {
        let candidate_cube = PathBuf::from(&home).join("Cube").join(target).join(format!("{}.json", target));
        if candidate_cube.is_file() {
            return Ok(candidate_cube);
        }
    }

    Err(format!("cannot locate configuration file for VM '{}'", target))
}

pub fn resolve_target_dir(target: &str, state_root: &Path) -> Result<(PathBuf, Option<PathBuf>), String> {
    let path = PathBuf::from(target);
    if path.is_file() {
        let content = fs::read_to_string(&path).map_err(|e| format!("cannot read {:?}: {}", path, e))?;
        let cfg: VmConfig = serde_json::from_str(&content).map_err(|e| format!("json error in {:?}: {}", path, e))?;
        validate_vm_name(&cfg.name)?;
        Ok((state_root.join(&cfg.name), Some(path)))
    } else {
        validate_vm_name(target)?;
        Ok((state_root.join(target), None))
    }
}
