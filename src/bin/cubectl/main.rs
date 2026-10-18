//! cubectl: native CLI manager for named Cube VM instances.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

pub mod config;
pub mod ext4;
pub mod server;
pub mod unattended;
pub mod util;
pub mod commands;

// ---------------------------------------------------------------------------
// CLI Argument Parser via Clap
// ---------------------------------------------------------------------------

#[derive(Parser, Debug)]
#[command(name = "cubectl", about = "Launch and manage named Cube VM instances from JSON configuration files", version)]
pub struct Cli {
    #[arg(long, global = true, help = "Base directory for VM instances (default: ~/.local/state/cube)")]
    pub state_root: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Create a new VM configuration file and optional sparse disk
    Create(CreateArgs),

    /// Install OS into a VM using automated unattended seed installation
    Install(InstallArgs),

    /// Eject installer ISO(s) from a VM configuration
    Eject {
        /// Path to config or VM name
        target: String,
    },

    /// Delete a VM, its disks, and state directory
    #[command(alias = "rm")]
    Delete {
        /// VM name or path to configuration file
        target: String,

        /// Force stop running VM before deleting
        #[arg(short = 'f', long = "force")]
        force: bool,

        /// Keep disk images and only delete VM configuration and state
        #[arg(long = "keep-disks")]
        keep_disks: bool,
    },

    /// Start a named VM instance in the background
    Start(StartArgs),

    /// Query the status of a running or stopped VM instance
    Status {
        /// Path to config or VM name
        target: String,
    },

    /// List all known VM instances in the state root
    List {
        #[arg(long, help = "Output list as JSON")]
        json: bool,
    },

    /// Request clean guest shutdown via SSH
    Shutdown {
        /// Path to config or VM name
        target: String,

        #[arg(long, default_value_t = 120, help = "Seconds to wait for shutdown")]
        timeout: u64,

        #[arg(long, help = "Force stop host process if guest shutdown times out")]
        force_after_timeout: bool,
    },

    /// Stop a running VM immediately (SIGTERM with SIGKILL fallback)
    Stop {
        /// Path to config or VM name
        target: String,

        #[arg(long, default_value_t = 10, help = "Seconds to wait before SIGKILL")]
        timeout: u64,
    },

    /// Inject preseed.cfg into a Linux initrd.gz CPIO archive
    PreseedInitrd {
        /// Path to base/input initrd.gz
        input: PathBuf,
        /// Path to output preseeded initrd.gz
        output: PathBuf,
        /// Path to preseed.cfg file
        preseed: PathBuf,
    },

    /// Extract Linux kernel and initramfs from a disk image (ext4)
    ExtractKernel {
        /// Path to source disk image
        disk: PathBuf,
        /// Path to write extracted kernel
        kernel: PathBuf,
        /// Path to write extracted initrd
        initrd: PathBuf,
        /// Path to write detected root partition device (e.g. /dev/vda1)
        #[arg(long)]
        rootpart: Option<PathBuf>,
    },

    /// Prepare a cloud image for serial login by patching ext4 /etc/shadow
    PatchShadow {
        /// Path to cloud disk image
        disk: PathBuf,
        /// Root password to set (default: root)
        #[arg(default_value = "root")]
        password: String,
    },

    #[command(name = "_serve", hide = true)]
    _Serve {
        state_dir: PathBuf,
    },
}

#[derive(Parser, Debug, Clone)]
pub struct StartArgs {
    /// Path to JSON VM configuration file or VM name
    pub config: PathBuf,

    #[arg(long, help = "Override binary path")]
    pub binary: Option<PathBuf>,

    #[arg(long, help = "Override RAM in MiB")]
    pub ram: Option<u64>,

    #[arg(long, help = "Override vCPU count")]
    pub cpus: Option<u32>,

    #[arg(long, help = "Override network mode")]
    pub net: Option<String>,

    #[arg(long, help = "Override network MAC address")]
    pub mac: Option<String>,

    #[arg(long, help = "Override SSH forwarding port")]
    pub ssh_port: Option<u16>,

    #[arg(long = "forward", help = "Replace port forwards")]
    pub forwards: Vec<String>,

    #[arg(long, help = "Override VNC bind address (IP:PORT)")]
    pub vnc: Option<String>,

    #[arg(long, help = "Override display resolution (WIDTHxHEIGHT)")]
    pub display: Option<String>,

    #[arg(long, help = "Override EFI vars store path")]
    pub efi_vars: Option<PathBuf>,

    #[arg(long = "disk", help = "Replace disks with specified paths")]
    pub disks: Vec<String>,

    #[arg(long = "share", num_args = 2, value_names = ["TAG", "PATH"], help = "Replace shares (read-only)")]
    pub shares: Vec<String>,

    #[arg(long = "share-rw", num_args = 2, value_names = ["TAG", "PATH"], help = "Replace shares (read-write)")]
    pub shares_rw: Vec<String>,

    #[arg(long, help = "Run VMM via sudo for in-process vmnet or bridge")]
    pub sudo: bool,

    #[arg(short = 'i', long = "interactive", help = "Run VM in foreground with interactive terminal attached")]
    pub interactive: bool,

    #[arg(long, help = "Exit instead of rebooting when guest requests reset")]
    pub no_reboot: bool,
}

#[derive(Parser, Debug, Clone)]
pub struct CreateArgs {
    /// Name of the VM (letters, digits, _, -)
    pub name: String,

    /// Installation ISO path (attached as read-only installer disk, enables VNC)
    #[arg(long = "iso")]
    pub iso: Option<PathBuf>,

    /// FreeBSD release version to auto-download if --iso is omitted (e.g. 14.4, 14.5, 15.1, or 15)
    #[arg(long = "version")]
    pub version: Option<String>,

    /// Overwrite existing configuration file if it already exists
    #[arg(short = 'f', long = "force")]
    pub force: bool,

    /// Destination directory for VM files (default: ~/VMs/<name>)
    #[arg(long)]
    pub dest: Option<PathBuf>,

    /// Write config file directly to this path instead of <dest>/<name>.json
    #[arg(long)]
    pub output: Option<PathBuf>,

    /// Boot mode ("uefi" or "freebsd")
    #[arg(long, default_value = "uefi")]
    pub boot: String,

    /// UEFI firmware path (required for uefi mode unless firmware/QEMU_EFI.fd exists)
    #[arg(long)]
    pub firmware: Option<PathBuf>,

    /// Kernel path (required for freebsd mode unless boot/kernel/kernel exists)
    #[arg(long)]
    pub kernel: Option<PathBuf>,

    /// Direct kernel root mount (e.g. ufs:/dev/vtbd0p2 or zfs:zroot/ROOT/default)
    #[arg(long)]
    pub root: Option<String>,

    /// Extra boot arguments
    #[arg(long)]
    pub bootargs: Option<String>,

    /// Kernel modules to preload
    #[arg(long = "module")]
    pub modules: Vec<String>,

    /// RAM size in MiB (default: 2048)
    #[arg(long, default_value_t = 2048)]
    pub ram: u64,

    /// Number of vCPUs (1-16, default: 2)
    #[arg(long, default_value_t = 2)]
    pub cpus: u32,

    /// Disk specifications: PATH, PATH:SIZE (e.g. disk.img:20G), PATH,cow, or PATH,ro
    #[arg(long = "disk")]
    pub disks: Vec<String>,

    /// Shared folders (read-only): TAG PATH
    #[arg(long = "share", num_args = 2, value_names = ["TAG", "PATH"])]
    pub shares: Vec<String>,

    /// Shared folders (read-write): TAG PATH
    #[arg(long = "share-rw", num_args = 2, value_names = ["TAG", "PATH"])]
    pub shares_rw: Vec<String>,

    /// Network mode ("vmnet-proxy", "vmnet", "bridge", "mock", "none")
    #[arg(long, default_value = "vmnet-proxy")]
    pub net: String,

    /// Network MAC address (default: auto-generated from VM name)
    #[arg(long)]
    pub mac: Option<String>,

    /// Host port forward to guest SSH
    #[arg(long)]
    pub ssh_port: Option<u16>,

    /// Port forwards (e.g. 8080=80 or 127.0.0.1:8080=192.168.64.2:80)
    #[arg(long = "forward")]
    pub forwards: Vec<String>,

    /// VNC bind address (e.g. 127.0.0.1:5901)
    #[arg(long)]
    pub vnc: Option<String>,

    /// Display resolution (e.g. 1280x800)
    #[arg(long)]
    pub display: Option<String>,

    /// SSH private key path for graceful shutdown
    #[arg(long)]
    pub shutdown_key: Option<PathBuf>,

    /// Custom cube binary path
    #[arg(long)]
    pub binary: Option<PathBuf>,

    /// Perform automated unattended OS installation from the ISO
    #[arg(long = "unattended")]
    pub unattended: bool,

    /// Root password for unattended installation (default: "root")
    #[arg(long = "password", default_value = "root")]
    pub password: String,

    /// Hostname for the VM (default: VM name)
    #[arg(long = "hostname")]
    pub hostname: Option<String>,

    /// SSH public key or key file for root authorized_keys
    #[arg(long = "ssh-key")]
    pub ssh_key: Option<String>,

    /// Filesystem type for unattended installation ("zfs" or "ufs", default: "zfs")
    #[arg(long = "filesystem", default_value = "zfs")]
    pub filesystem: String,

    /// Distribution base mode ("distsets" or "pkgbase", default: "distsets")
    #[arg(long = "base", default_value = "distsets")]
    pub base_dist: String,

    /// RAM allocated during the installation phase (MiB, default: same as --ram)
    #[arg(long = "installer-ram")]
    pub installer_ram: Option<u64>,

    /// vCPUs allocated during the installation phase (default: same as --cpus)
    #[arg(long = "installer-cpus")]
    pub installer_cpus: Option<u32>,
}

#[derive(Parser, Debug, Clone)]
pub struct InstallArgs {
    /// VM name or path to configuration file
    pub target: String,

    /// Path to installation ISO (default: ISO attached to VM or auto-detected)
    #[arg(long = "iso")]
    pub iso: Option<PathBuf>,

    /// FreeBSD release version to auto-download if --iso is omitted (e.g. 14.4, 14.5, 15.1, or 15)
    #[arg(long = "version")]
    pub version: Option<String>,

    /// Root password for the installed OS (default: "root")
    #[arg(long = "password", default_value = "root")]
    pub password: String,

    /// Hostname for the VM (default: VM name)
    #[arg(long = "hostname")]
    pub hostname: Option<String>,

    /// SSH public key or key file for root authorized_keys
    #[arg(long = "ssh-key")]
    pub ssh_key: Option<String>,

    /// Filesystem type ("zfs" or "ufs", default: "zfs")
    #[arg(long = "filesystem", default_value = "zfs")]
    pub filesystem: String,

    /// Distribution base mode ("distsets" or "pkgbase", default: "distsets")
    #[arg(long = "base", default_value = "distsets")]
    pub base_dist: String,

    /// RAM in MiB during installation (default: from VM config or 2048)
    #[arg(long = "ram")]
    pub ram: Option<u64>,

    /// vCPUs during installation (default: from VM config or 2)
    #[arg(long = "cpus")]
    pub cpus: Option<u32>,
}

// ---------------------------------------------------------------------------
// Main Entrypoint
// ---------------------------------------------------------------------------

fn main() {
    let cli = Cli::parse();
    let state_root = cli.state_root.unwrap_or_else(util::default_state_root);

    let res = match cli.command {
        Commands::Create(args) => commands::do_create(args),
        Commands::Install(args) => commands::do_install(args, &state_root),
        Commands::Eject { target } => commands::do_eject(&target, &state_root),
        Commands::Delete { target, force, keep_disks } => commands::do_delete(&target, &state_root, force, keep_disks),
        Commands::Start(args) => commands::do_start(args, &state_root),
        Commands::Status { target } => commands::do_status(&target, &state_root),
        Commands::List { json } => commands::do_list(&state_root, json),
        Commands::Shutdown { target, timeout, force_after_timeout } => {
            server::do_action(&target, &state_root, "shutdown", timeout, force_after_timeout)
        }
        Commands::Stop { target, timeout } => server::do_action(&target, &state_root, "stop", timeout, false),
        Commands::PreseedInitrd { input, output, preseed } => {
            util::inject_preseed_initrd(&input, &output, &preseed)
        }
        Commands::ExtractKernel { disk, kernel, initrd, rootpart } => {
            ext4::extract_debian_cloud_kernel(&disk, &kernel, &initrd, rootpart.as_deref())
        }
        Commands::PatchShadow { disk, password } => {
            ext4::patch_cloud_shadow(&disk, &password)
        }
        Commands::_Serve { state_dir } => server::serve(&state_dir),
    };

    if let Err(err) = res {
        eprintln!("cubectl: {}", err);
        std::process::exit(1);
    }
}
