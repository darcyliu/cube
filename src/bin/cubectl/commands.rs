use std::fs::{self, OpenOptions};
use std::net::ToSocketAddrs;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::{
    build_launch_spec, find_config_for_target, resolve_target_dir, validate_vm_name, BootConfig,
    DiskItem, NetworkConfig, ShareItem, ShutdownConfig, VmConfig, VncConfig,
};
use crate::server::{active_resources, do_action, live_status};
use crate::unattended::{
    auto_detect_iso, find_default_firmware, resolve_ssh_public_key, run_unattended_install,
};
use crate::util::{
    self, canonical_path_str, create_sparse_file, default_state_root, default_vms_root, ensure_cube_entitlements,
    find_cube_binary, parse_disk_spec, require, resolve_path, write_json_atomic,
};
use crate::{CreateArgs, InstallArgs, SshArgs, StartArgs};

pub fn do_create(args: CreateArgs, state_root: &Path) -> Result<(), String> {
    validate_vm_name(&args.name)?;

    let dest_dir = if let Some(d) = args.dest {
        d
    } else {
        default_vms_root().join(&args.name)
    };
    fs::create_dir_all(&dest_dir).map_err(|e| format!("cannot create destination directory {:?}: {}", dest_dir, e))?;

    let config_path = if let Some(o) = args.output {
        o
    } else {
        dest_dir.join(format!("{}.json", args.name))
    };
    if config_path.exists() && !args.force {
        return Err(format!("configuration file {:?} already exists; choose another name or use --force to overwrite", config_path));
    }

    // Resolve or detect installation ISO
    let installer_iso = if let Some(ref iso) = args.iso {
        let resolved = resolve_path(&iso.to_string_lossy(), Path::new("."))?;
        require(resolved.is_file(), format!("ISO file not found: {:?}", resolved))?;
        Some(canonical_path_str(&resolved)?)
    } else if let Some(ref ver) = args.version {
        let downloaded = util::ensure_freebsd_iso(ver, None)?;
        Some(canonical_path_str(&downloaded)?)
    } else if args.unattended {
        if let Some(detected) = auto_detect_iso() {
            Some(canonical_path_str(&detected)?)
        } else {
            return Err("--unattended requires --iso <PATH> or --version <VER> (or a FreeBSD-*-arm64*.iso in current directory)".into());
        }
    } else {
        None
    };

    // Disks setup
    let raw_specs = if args.disks.is_empty() {
        vec![format!("rootfs.img:20G")]
    } else {
        args.disks
    };

    let mut disks = Vec::new();
    let mut disk_summaries = Vec::new();
    let mut primary_disk_path = None;

    for (idx, spec) in raw_specs.iter().enumerate() {
        let parsed = parse_disk_spec(spec)?;
        let disk_path = resolve_path(&parsed.path_str, &dest_dir)?;

        if idx == 0 {
            primary_disk_path = Some(disk_path.clone());
        }

        if let Some(size) = parsed.size_bytes {
            if !disk_path.exists() {
                create_sparse_file(&disk_path, size)?;
                let gib = size as f64 / (1024.0 * 1024.0 * 1024.0);
                disk_summaries.push(format!("{} ({:.1} GiB sparse target)", disk_path.display(), gib));
            } else {
                disk_summaries.push(format!("{} (existing)", disk_path.display()));
            }
        } else {
            require(disk_path.is_file(), format!("disk file does not exist: {:?}", disk_path))?;
            disk_summaries.push(format!("{} (existing)", disk_path.display()));
        }

        let canonical_str = canonical_path_str(&disk_path)?;
        if parsed.cow || parsed.read_only {
            disks.push(DiskItem::Detailed {
                path: canonical_str,
                cow: parsed.cow,
                read_only: parsed.read_only,
            });
        } else {
            disks.push(DiskItem::Simple(canonical_str));
        }
    }

    let (ssh_key_opt, ssh_key_desc, ssh_priv_key) = resolve_ssh_public_key(args.ssh_key.as_deref());

    // Run unattended installation if requested
    if args.unattended {
        let iso_canonical = installer_iso.as_ref().unwrap();
        let target_disk = primary_disk_path.ok_or("no target disk configured")?;
        require(!disks[0].is_cow() && !disks[0].is_read_only(), "installation requires a writable, non-COW target disk")?;
        // Verify target disk is not currently locked by another process
        drop(util::lock_disk(&target_disk)?);
        let cube_bin = find_cube_binary(args.binary.as_deref(), None, Path::new("."))?;
        ensure_cube_entitlements(&cube_bin);

        let hostname = args.hostname.as_deref().unwrap_or(&args.name);
        let inst_ram = args.installer_ram.unwrap_or(args.ram);
        let inst_cpus = args.installer_cpus.unwrap_or(args.cpus);

        let effective_base = if let Some(ref ver) = args.version {
            let (_, default_base) = util::resolve_freebsd_version(ver)?;
            if args.base_dist == "distsets" && default_base == "pkgbase" {
                "pkgbase".to_string()
            } else {
                args.base_dist.clone()
            }
        } else {
            args.base_dist.clone()
        };

        run_unattended_install(
            &args.name,
            Path::new(iso_canonical),
            &target_disk,
            &dest_dir,
            &cube_bin,
            hostname,
            &args.password,
            ssh_key_opt.as_deref(),
            ssh_key_desc.as_deref(),
            &args.filesystem,
            &effective_base,
            inst_ram,
            inst_cpus,
        )?;
    } else if let Some(ref iso_path) = installer_iso {
        disks.push(DiskItem::Detailed {
            path: iso_path.clone(),
            cow: false,
            read_only: true,
        });
        disk_summaries.push(format!("{} (installer ISO, read-only)", iso_path));
    }

    // Boot configuration
    let boot = match args.boot.to_lowercase().as_str() {
        "uefi" => {
            let fw_path = find_default_firmware(args.firmware.as_ref(), &dest_dir)?;
            BootConfig {
                mode: "uefi".into(),
                firmware: Some(canonical_path_str(&fw_path)?),
                kernel: None,
                multiuser: true,
                root: args.root,
                bootargs: args.bootargs,
                modules: args.modules,
            }
        }
        "freebsd" => {
            let kern_str = if let Some(ref k) = args.kernel {
                let resolved = resolve_path(&k.to_string_lossy(), Path::new("."))?;
                require(resolved.is_file(), format!("kernel file not found: {:?}", resolved))?;
                canonical_path_str(&resolved)?
            } else {
                let default_k = dest_dir.join("kernel");
                if default_k.is_file() {
                    canonical_path_str(&default_k)?
                } else if Path::new("boot/freebsd/kernel").is_file() {
                    canonical_path_str(Path::new("boot/freebsd/kernel"))?
                } else if Path::new("boot/kernel/kernel").is_file() {
                    canonical_path_str(Path::new("boot/kernel/kernel"))?
                } else {
                    return Err("FreeBSD boot requires --kernel <PATH> (or extracted kernel in VM directory)".into());
                }
            };
            let default_root = if args.filesystem == "zfs" {
                "zfs:zroot/ROOT/default".to_string()
            } else {
                "ufs:/dev/vtbd0p2".to_string()
            };
            BootConfig {
                mode: "freebsd".into(),
                firmware: None,
                kernel: Some(kern_str),
                multiuser: true,
                root: args.root.or(Some(default_root)),
                bootargs: args.bootargs,
                modules: args.modules,
            }
        }
        other => return Err(format!("unsupported boot mode '{}'; choose 'uefi' or 'freebsd'", other)),
    };

    // Shares configuration
    let mut shares = Vec::new();
    for chunk in args.shares.chunks_exact(2) {
        let resolved = resolve_path(&chunk[1], Path::new("."))?;
        require(resolved.is_dir(), format!("share directory does not exist: {:?}", resolved))?;
        shares.push(ShareItem {
            tag: chunk[0].clone(),
            path: resolved.to_string_lossy().into_owned(),
            read_only: true,
        });
    }
    for chunk in args.shares_rw.chunks_exact(2) {
        let resolved = resolve_path(&chunk[1], Path::new("."))?;
        require(resolved.is_dir(), format!("share directory does not exist: {:?}", resolved))?;
        shares.push(ShareItem {
            tag: chunk[0].clone(),
            path: resolved.to_string_lossy().into_owned(),
            read_only: false,
        });
    }

    let vms_root = default_vms_root();

    // Determine explicitly reserved host ports from args.forwards
    let mut reserved_ssh_ports = Vec::new();
    for f in &args.forwards {
        if let Some((host_part, _)) = f.split_once('=') {
            if let Some((_, port_str)) = host_part.rsplit_once(':') {
                if let Ok(p) = port_str.parse::<u16>() {
                    reserved_ssh_ports.push(p);
                }
            } else if let Ok(p) = host_part.parse::<u16>() {
                reserved_ssh_ports.push(p);
            }
        }
    }

    // Network configuration: smart auto-allocation if ssh_port not specified
    let allocated_ssh_port = if let Some(p) = args.ssh_port {
        if p == 0 { None } else { Some(p) }
    } else if args.net != "none" {
        Some(find_next_available_ssh_port(state_root, &vms_root, &reserved_ssh_ports))
    } else {
        None
    };

    let network = Some(NetworkConfig {
        mode: args.net.clone(),
        mac: args.mac,
        ssh_port: allocated_ssh_port,
        forwards: args.forwards,
    });

    // VNC configuration: smart auto-allocation if VNC enabled/needed and not specified
    let vnc = if let Some(ref bind) = args.vnc {
        let effective_bind = if bind == "auto" {
            let next_vnc = find_next_available_vnc_port(state_root, &vms_root);
            format!("127.0.0.1:{}", next_vnc)
        } else {
            bind.clone()
        };
        Some(VncConfig {
            bind: effective_bind,
            display: args.display,
        })
    } else if args.display.is_some() || (installer_iso.is_some() && !args.unattended) {
        let next_vnc = find_next_available_vnc_port(state_root, &vms_root);
        Some(VncConfig {
            bind: format!("127.0.0.1:{}", next_vnc),
            display: args.display,
        })
    } else {
        None
    };

    // Shutdown configuration
    let shutdown = if let Some(key) = args.shutdown_key {
        let resolved_key = resolve_path(&key.to_string_lossy(), Path::new("."))?;
        require(resolved_key.is_file(), format!("shutdown SSH key does not exist: {:?}", resolved_key))?;
        Some(ShutdownConfig {
            host: "127.0.0.1".into(),
            port: allocated_ssh_port,
            user: "root".into(),
            identity_file: resolved_key.to_string_lossy().into_owned(),
        })
    } else if let Some(ref priv_key) = ssh_priv_key {
        Some(ShutdownConfig {
            host: "127.0.0.1".into(),
            port: allocated_ssh_port,
            user: "root".into(),
            identity_file: priv_key.to_string_lossy().into_owned(),
        })
    } else {
        None
    };

    let os_desc = if let Some(ref ver) = args.version {
        if let Ok((resolved, _)) = util::resolve_freebsd_version(ver) {
            format!("FreeBSD {}-RELEASE", resolved)
        } else {
            format!("FreeBSD {}", ver)
        }
    } else if let Some(ref iso) = installer_iso {
        let filename = Path::new(iso).file_name().unwrap_or_default().to_string_lossy();
        if filename.contains("14.4") {
            "FreeBSD 14.4-RELEASE".into()
        } else if filename.contains("14.5") {
            "FreeBSD 14.5-RELEASE".into()
        } else if filename.contains("15.1") {
            "FreeBSD 15.1-RELEASE".into()
        } else {
            "FreeBSD".into()
        }
    } else {
        "FreeBSD".into()
    };

    let config = VmConfig {
        name: args.name.clone(),
        os: Some(os_desc),
        binary: args.binary.map(|b| b.to_string_lossy().into_owned()),
        boot,
        ram_mb: args.ram,
        cpus: args.cpus,
        disks,
        shares,
        network,
        vnc,
        efi_vars: None,
        shutdown,
        no_reboot: false,
        mem_prealloc: args.mem_prealloc,
    };

    let json_text = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())? + "\n";
    fs::write(&config_path, json_text).map_err(|e| format!("cannot write configuration {:?}: {}", config_path, e))?;

    // Remember custom destinations before the first launch so GUI and CLI
    // operations can resolve the VM by name immediately after creation.
    let registration_dir = state_root.join(&config.name);
    fs::create_dir_all(&registration_dir).map_err(|e| format!("cannot register VM: {}", e))?;
    write_json_atomic(&registration_dir.join("registration.json"), &serde_json::json!({
        "config": canonical_path_str(&config_path)?
    }))?;

    if args.unattended {
        println!("✓ Successfully created and installed VM '{}'", args.name);
        println!("  Configuration: {}", config_path.display());
        println!("  Boot Mode:     {}", config.boot.mode);
        println!("  Resources:     {} vCPUs, {} MiB RAM", config.cpus, config.ram_mb);
        for disk in disk_summaries {
            println!("  Disk:          {}", disk);
        }
        if let Some(ref net) = config.network {
            let ssh_desc = net.ssh_port.map(|p| p.to_string()).unwrap_or_else(|| "none".into());
            println!("  Network:       {} (SSH port: {})", net.mode, ssh_desc);
        }
        println!("  Credentials:   root / {}", args.password);
        if let Some(desc) = ssh_key_desc {
            println!("  SSH Key:       {}", desc);
        }
        println!();
        println!("Next steps to use your VM:");
        println!("  1. Start the VM:");
        println!("     cubectl start {}", args.name);
        if let Some(ref net) = config.network {
            if let Some(port) = net.ssh_port {
                println!("  2. Connect via SSH once booted:");
                println!("     ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -p {} root@127.0.0.1", port);
            }
        }
    } else {
        println!("✓ Successfully created VM '{}'", args.name);
        println!("  Configuration: {}", config_path.display());
        println!("  Boot Mode:     {}", config.boot.mode);
        println!("  Resources:     {} vCPUs, {} MiB RAM", config.cpus, config.ram_mb);
        for disk in disk_summaries {
            println!("  Disk:          {}", disk);
        }
        if let Some(ref net) = config.network {
            let ssh_desc = net.ssh_port.map(|p| p.to_string()).unwrap_or_else(|| "none".into());
            println!("  Network:       {} (SSH port: {})", net.mode, ssh_desc);
        }
        if let Some(ref v) = config.vnc {
            println!("  VNC Display:   {} (res: {})", v.bind, v.display.as_deref().unwrap_or("auto"));
        }

        if let Some(ref _iso) = installer_iso {
            println!();
            println!("Next steps to install the OS:");
            println!("  1. Start the VM:");
            println!("     cubectl start {}", args.name);
            if let Some(ref v) = config.vnc {
                println!("  2. Connect to the installer GUI:");
                println!("     open vnc://{}", v.bind);
            }
            println!("  3. When installation finishes, stop VM and eject the installer ISO:");
            println!("     cubectl stop {}", args.name);
            println!("     cubectl eject {}", args.name);
            println!("  4. Start into your newly installed OS:");
            println!("     cubectl start {}", args.name);
        } else {
            println!();
            println!("To start this VM:");
            println!("  cubectl start {}", args.name);
        }
    }

    Ok(())
}

pub fn do_install(args: InstallArgs, state_root: &Path) -> Result<(), String> {
    let config_path = find_config_for_target(&args.target, state_root)?;
    let content = fs::read_to_string(&config_path).map_err(|e| format!("cannot read {:?}: {}", config_path, e))?;
    let mut config: VmConfig = serde_json::from_str(&content).map_err(|e| format!("json error in {:?}: {}", config_path, e))?;
    let dest_dir = config_path.parent().unwrap_or_else(|| Path::new("."));

    require(!config.disks.is_empty(), "VM configuration has no disks")?;
    let target_disk_path = resolve_path(config.disks[0].path(), dest_dir)?;
    require(target_disk_path.is_file(), format!("target disk does not exist: {:?}", target_disk_path))?;

    require(!config.disks[0].is_cow() && !config.disks[0].is_read_only(), "installation requires a writable, non-COW target disk")?;
    require(live_status(&state_root.join(&config.name)).is_none(), "cannot install into a running VM")?;
    // Verify target disk is not currently locked by another process
    drop(util::lock_disk(&target_disk_path)?);

    let iso_path = if let Some(ref iso) = args.iso {
        let p = resolve_path(&iso.to_string_lossy(), Path::new("."))?;
        require(p.is_file(), format!("specified ISO not found: {:?}", p))?;
        p
    } else if let Some(ref ver) = args.version {
        util::ensure_freebsd_iso(ver, None)?
    } else {
        let mut attached_iso = None;
        for d in &config.disks {
            if d.path().to_lowercase().ends_with(".iso") {
                if let Ok(p) = resolve_path(d.path(), dest_dir) {
                    if p.is_file() {
                        attached_iso = Some(p);
                        break;
                    }
                }
            }
        }
        if let Some(iso) = attached_iso {
            iso
        } else if let Some(detected) = auto_detect_iso() {
            detected
        } else {
            return Err("no installer ISO found; specify --iso <PATH> or --version <VER>".into());
        }
    };

    let cube_bin = find_cube_binary(None, config.binary.as_deref(), dest_dir)?;
    ensure_cube_entitlements(&cube_bin);

    let (ssh_key_opt, ssh_key_desc, ssh_priv_key) = resolve_ssh_public_key(args.ssh_key.as_deref());
    let ram = args.ram.unwrap_or(config.ram_mb);
    let cpus = args.cpus.unwrap_or(config.cpus);
    let hostname = args.hostname.as_deref().unwrap_or(&config.name);

    run_unattended_install(
        &config.name,
        &iso_path,
        &target_disk_path,
        dest_dir,
        &cube_bin,
        hostname,
        &args.password,
        ssh_key_opt.as_deref(),
        ssh_key_desc.as_deref(),
        &args.filesystem,
        &args.base_dist,
        ram,
        cpus,
    )?;

    config.disks.retain(|d| !d.path().to_lowercase().ends_with(".iso"));
    if config.os.is_none() {
        let os_desc = if let Some(ref ver) = args.version {
            if let Ok((resolved, _)) = util::resolve_freebsd_version(ver) {
                format!("FreeBSD {}-RELEASE", resolved)
            } else {
                format!("FreeBSD {}", ver)
            }
        } else {
            let filename = iso_path.file_name().unwrap_or_default().to_string_lossy();
            if filename.contains("14.4") {
                "FreeBSD 14.4-RELEASE".into()
            } else if filename.contains("14.5") {
                "FreeBSD 14.5-RELEASE".into()
            } else if filename.contains("15.1") {
                "FreeBSD 15.1-RELEASE".into()
            } else {
                "FreeBSD".into()
            }
        };
        config.os = Some(os_desc);
    }
    if let Some(priv_key) = ssh_priv_key {
        if config.shutdown.is_none() {
            config.shutdown = Some(ShutdownConfig {
                host: "127.0.0.1".into(),
                port: config.network.as_ref().and_then(|n| n.ssh_port),
                user: "root".into(),
                identity_file: canonical_path_str(&priv_key)?,
            });
        }
    }

    let json_val = serde_json::to_value(&config).map_err(|e| e.to_string())?;
    write_json_atomic(&config_path, &json_val)?;

    println!("✓ Unattended OS installation complete for '{}'!", config.name);
    println!("  Configuration updated: {}", config_path.display());
    println!("\nStart your VM anytime with:\n  cubectl start {}", config.name);
    Ok(())
}

pub fn do_eject(target: &str, state_root: &Path) -> Result<(), String> {
    let config_path = find_config_for_target(target, state_root)?;
    let content = fs::read_to_string(&config_path).map_err(|e| format!("cannot read {:?}: {}", config_path, e))?;
    let mut config: VmConfig = serde_json::from_str(&content).map_err(|e| format!("json error in {:?}: {}", config_path, e))?;

    let mut removed = Vec::new();
    let mut remaining = Vec::new();

    for disk in config.disks {
        let path = disk.path().to_string();
        if path.to_lowercase().ends_with(".iso") || (disk.is_read_only() && path.contains(".iso")) {
            removed.push(path);
        } else {
            remaining.push(disk);
        }
    }

    if removed.is_empty() {
        println!("No installer ISO disks found attached to {:?}.", config_path);
        return Ok(());
    }

    config.disks = remaining;
    let json_val = serde_json::to_value(&config).map_err(|e| e.to_string())?;
    write_json_atomic(&config_path, &json_val)?;

    println!("✓ Ejected installer ISO from \"{}\":", config.name);
    for r in &removed {
        println!("  - {}", r);
    }
    println!("Updated configuration: {}", config_path.display());
    Ok(())
}

fn path_is_within(path: &Path, directory: &Path) -> bool {
    match (path.canonicalize(), directory.canonicalize()) {
        (Ok(path), Ok(directory)) => path != directory && path.starts_with(directory),
        _ => false,
    }
}

pub fn do_delete(target: &str, state_root: &Path, force: bool, keep_disks: bool) -> Result<(), String> {
    let (state_dir, maybe_explicit_config) = resolve_target_dir(target, state_root)?;
    let vm_name = state_dir.file_name().and_then(|n| n.to_str()).unwrap_or(target);

    // 1. Check if VM is running
    if let Some(status) = live_status(&state_dir) {
        if status.get("state").and_then(|s| s.as_str()) == Some("running") {
            if force {
                println!("Stopping running VM '{}'...", vm_name);
                do_action(target, state_root, "stop", 5, false)?;
            } else {
                return Err(format!(
                    "VM '{}' is currently running. Stop it first with 'cubectl stop {}' or pass --force",
                    vm_name, vm_name
                ));
            }
        }
    }

    // 2. Find configuration file and remove disks
    let config_path_res = if let Some(p) = maybe_explicit_config {
        Ok(p)
    } else {
        find_config_for_target(target, state_root)
    };

    let mut removed_files = Vec::new();

    if let Ok(config_path) = config_path_res {
        if let Ok(content) = fs::read_to_string(&config_path) {
            if let Ok(cfg) = serde_json::from_str::<VmConfig>(&content) {
                let vm_dir = config_path.parent().unwrap_or_else(|| Path::new("."));

                // Remove disks, kernel, and VM boot artifacts if !keep_disks
                if !keep_disks {
                    for disk in &cfg.disks {
                        let p = disk.path();
                        // Do not delete read-only installation media or ISOs
                        if p.to_lowercase().ends_with(".iso") || disk.is_read_only() {
                            continue;
                        }
                        let resolved = resolve_path(p, vm_dir)
                            .ok()
                            .filter(|p| p.is_file())
                            .or_else(|| resolve_path(p, Path::new(".")).ok().filter(|p| p.is_file()));
                        if let Some(r) = resolved {
                            let _disk_lock = util::lock_disk(&r)?;
                            if fs::remove_file(&r).is_ok() {
                                removed_files.push(r.display().to_string());
                            }
                        }
                    }

                    // Remove kernel if stored inside VM dir
                    if let Some(ref kern) = cfg.boot.kernel {
                        if let Ok(resolved) = resolve_path(kern, vm_dir) {
                            if resolved.is_file() && path_is_within(&resolved, vm_dir) {
                                let _ = fs::remove_file(&resolved);
                                removed_files.push(resolved.display().to_string());
                            }
                        }
                    }
                    for mod_path in &cfg.boot.modules {
                        if let Ok(resolved) = resolve_path(mod_path, vm_dir) {
                            if resolved.is_file() && path_is_within(&resolved, vm_dir) {
                                let _ = fs::remove_file(&resolved);
                                removed_files.push(resolved.display().to_string());
                            }
                        }
                    }
                    let default_kernel = vm_dir.join("kernel");
                    if default_kernel.is_file() && path_is_within(&default_kernel, vm_dir) {
                        let _ = fs::remove_file(&default_kernel);
                        removed_files.push(default_kernel.display().to_string());
                    }

                    // Remove installer and runtime artifacts
                    for artifact in &[".installed", "cidata.iso", "install-failed.log", "vm.log"] {
                        let art_path = vm_dir.join(artifact);
                        if art_path.is_file() {
                            let _ = fs::remove_file(&art_path);
                            removed_files.push(art_path.display().to_string());
                        }
                    }
                }

                // Remove EFI vars file if stored inside VM dir
                if let Some(ref efi) = cfg.efi_vars {
                    if let Ok(resolved) = resolve_path(efi, vm_dir) {
                        if resolved.is_file() {
                            let _ = fs::remove_file(&resolved);
                            removed_files.push(resolved.display().to_string());
                        }
                    }
                }
            }
        }

        // Remove config file
        if config_path.is_file() {
            let _ = fs::remove_file(&config_path);
            removed_files.push(config_path.display().to_string());
        }

        // If the VM directory is inside default_vms_root or ~/Cube, clean it up
        if let Some(vm_dir) = config_path.parent() {
            let mut roots = vec![default_vms_root()];
            if let Some(home) = std::env::var_os("HOME") {
                roots.push(PathBuf::from(&home).join("Cube"));
            }
            for root in roots {
                let is_sub = match (vm_dir.canonicalize(), root.canonicalize()) {
                    (Ok(vd), Ok(rd)) => vd.starts_with(&rd) && vd != rd,
                    _ => vm_dir.starts_with(&root) && vm_dir != root,
                };
                if is_sub {
                    if !keep_disks {
                        // Deleting a managed VM also removes all remaining
                        // contents of its directory, including local media.
                        fs::remove_dir_all(vm_dir).map_err(|e| {
                            format!("cannot remove VM directory {}: {}", vm_dir.display(), e)
                        })?;
                        removed_files.push(vm_dir.display().to_string());
                        break;
                    } else if fs::remove_dir(vm_dir).is_ok() {
                        removed_files.push(vm_dir.display().to_string());
                        break;
                    }
                }
            }
        }
    }

    // 3. Remove state directory (~/.local/state/cube/<name>)
    if state_dir.exists() {
        let _ = fs::remove_dir_all(&state_dir);
        removed_files.push(state_dir.display().to_string());
    }

    if removed_files.is_empty() {
        return Err(format!("cannot find VM '{}' to delete", target));
    }

    println!("✓ Successfully deleted VM '{}'", vm_name);
    for f in removed_files {
        println!("  - {}", f);
    }
    Ok(())
}

pub fn do_start(args: StartArgs, state_root: &Path) -> Result<(), String> {
    let config_path = find_config_for_target(&args.config.to_string_lossy(), state_root)?;
    let content = fs::read_to_string(&config_path).map_err(|e| format!("cannot read {:?}: {}", config_path, e))?;
    let config: VmConfig = serde_json::from_str(&content).map_err(|e| format!("json error in {:?}: {}", config_path, e))?;

    fs::create_dir_all(state_root).map_err(|e| format!("cannot create state root: {}", e))?;
    let state_dir = state_root.join(&config.name);
    fs::create_dir_all(&state_dir).map_err(|e| format!("cannot create state dir: {}", e))?;

    let root_meta = fs::symlink_metadata(state_root).map_err(|e| e.to_string())?;
    let dir_meta = fs::symlink_metadata(&state_dir).map_err(|e| e.to_string())?;
    require(!root_meta.file_type().is_symlink() && !dir_meta.file_type().is_symlink(), "state directories cannot be symlinks")?;

    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state_root.join(".start.lock"))
        .map_err(|e| format!("cannot open .start.lock: {}", e))?;

    use std::os::unix::io::AsRawFd;
    let rc = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX) };
    require(rc == 0, "cannot acquire start lock")?;

    require(live_status(&state_dir).is_none(), "this VM is already running")?;

    let spec = build_launch_spec(&config_path, &config, &state_dir, &args)?;

    let active = active_resources(state_root, &state_dir);
    let mut collisions: Vec<_> = spec.resources.iter().filter(|r| active.contains(*r)).cloned().collect();
    collisions.sort();
    require(collisions.is_empty(), format!("resource already used by a running VM: {}", collisions.join(", ")))?;

    drop(util::lock_resources(&spec.resources)?);

    if args.sudo {
        let status = Command::new("sudo").args(["-n", "true"]).output();
        require(status.map(|s| s.status.success()).unwrap_or(false), "sudo -n is unavailable; run sudo -v first")?;
    }

    write_json_atomic(&state_dir.join("launch.json"), &serde_json::to_value(&spec).unwrap())?;

    if args.interactive {
        println!("==> Starting VM '{}' interactively...", config.name);
        println!("==> (Serial console attached; press Ctrl-] to exit VMM)\n");
        return crate::server::serve_interactive(&state_dir, lock_file);
    }

    let current_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(state_dir.join("manager.log"))
        .map_err(|e| format!("cannot open manager.log: {}", e))?;

    let mut cmd = Command::new(&current_exe);
    cmd.args(["_serve", &state_dir.to_string_lossy()]);
    cmd.stdin(Stdio::null());
    cmd.stdout(log_file.try_clone().map_err(|e| e.to_string())?);
    cmd.stderr(log_file);
    cmd.process_group(0);

    let child = cmd.spawn().map_err(|e| format!("cannot spawn manager daemon: {}", e))?;
    let manager_pid = child.id();

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut initial_running_seen = false;
    while Instant::now() < deadline {
        let last_path = state_dir.join("last.json");
        if last_path.is_file() {
            if let Ok(last_data) = fs::read_to_string(&last_path) {
                if let Ok(ended) = serde_json::from_str::<serde_json::Value>(&last_data) {
                    if ended.get("id").and_then(|id| id.as_str()) == Some(&spec.id) {
                        let vmm_log = fs::read_to_string(state_dir.join("vmm.log")).unwrap_or_default();
                        let tail = if vmm_log.len() > 2000 { &vmm_log[vmm_log.len() - 2000..] } else { &vmm_log };
                        return Err(format!("VM exited during startup (code {:?}):\n{}", ended.get("exit_code"), tail.trim()));
                    }
                }
            }
        }

        // Check if manager exited
        let res = unsafe { libc::waitpid(manager_pid as i32, std::ptr::null_mut(), libc::WNOHANG) };
        if res > 0 {
            let m_log = fs::read_to_string(state_dir.join("manager.log")).unwrap_or_default();
            let v_log = fs::read_to_string(state_dir.join("vmm.log")).unwrap_or_default();
            let err = if !v_log.trim().is_empty() { v_log } else { m_log };
            let tail = if err.len() > 2000 { &err[err.len() - 2000..] } else { &err };
            return Err(format!("VM manager exited during startup:\n{}", tail.trim()));
        }

        if let Some(status) = live_status(&state_dir) {
            if status.get("id").and_then(|id| id.as_str()) == Some(&spec.id)
                && status.get("state").and_then(|s| s.as_str()) == Some("running")
            {
                if !initial_running_seen {
                    initial_running_seen = true;
                    // Give child VM 150ms to ensure it didn't immediately fail on device init
                    std::thread::sleep(Duration::from_millis(150));
                    continue;
                }
                println!("{}", serde_json::to_string_pretty(&status).unwrap());
                return Ok(());
            }
        }

        std::thread::sleep(Duration::from_millis(50));
    }

    Err(format!("VM did not start within 10s; inspect {:?}", state_dir.join("manager.log")))
}

fn ssh_identity(
    explicit: Option<PathBuf>,
    config: Option<&VmConfig>,
    config_path: Option<&Path>,
) -> Option<PathBuf> {
    explicit.or_else(|| {
        let identity = &config?.shutdown.as_ref()?.identity_file;
        let base = config_path.and_then(Path::parent).unwrap_or(Path::new("."));
        resolve_path(identity, base).ok()
    })
}

pub fn do_ssh(args: SshArgs, state_root: &Path) -> Result<(), String> {
    let (state_dir, explicit_config) = resolve_target_dir(&args.target, state_root)?;
    let vm_name = state_dir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| args.target.clone());

    let status = live_status(&state_dir).ok_or_else(|| {
        format!(
            "VM '{}' is not running. Start it first with 'cubectl start {}'",
            vm_name, vm_name
        )
    })?;

    // Attempt to locate the config to extract shutdown credentials / ssh_port if present
    let config_path = explicit_config.or_else(|| find_config_for_target(&vm_name, state_root).ok());
    let vm_config = config_path.as_ref().and_then(|cp| {
        fs::read_to_string(&cp).ok().and_then(|c| serde_json::from_str::<VmConfig>(&c).ok())
    });

    // 1. Determine Port & Host
    let (host, port) = if let Some(p) = args.port {
        if p == 22 {
            let ip = status
                .get("ip")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty() && *s != "0.0.0.0");
            (ip.unwrap_or("127.0.0.1").to_string(), 22)
        } else {
            ("127.0.0.1".to_string(), p)
        }
    } else {
        let configured_ssh_port = status
            .get("ssh_port")
            .and_then(|p| p.as_u64())
            .map(|p| p as u16)
            .or_else(|| vm_config.as_ref().and_then(|c| c.network.as_ref()).and_then(|n| n.ssh_port));

        if let Some(port) = configured_ssh_port {
            ("127.0.0.1".to_string(), port)
        } else if let Some(ip) = status
            .get("ip")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty() && *s != "0.0.0.0")
        {
            (ip.to_string(), 22)
        } else {
            // Default port forward fallback
            ("127.0.0.1".to_string(), 2222)
        }
    };

    // 2. Determine User
    let user = args.user.unwrap_or_else(|| {
        vm_config
            .as_ref()
            .and_then(|c| c.shutdown.as_ref())
            .map(|s| s.user.clone())
            .unwrap_or_else(|| "root".to_string())
    });

    // 3. Determine Identity File
    let identity = ssh_identity(args.identity, vm_config.as_ref(), config_path.as_deref());

    // 4. Optional --wait: wait until SSH daemon is ready and responding
    if args.wait {
        let deadline = Instant::now() + Duration::from_secs(args.wait_timeout);
        let mut ready = false;
        eprintln!("[Cube] Waiting for SSH on {}:{} to become ready...", host, port);
        while Instant::now() < deadline {
            if let Ok(mut addrs) = (host.as_str(), port).to_socket_addrs() {
                if let Some(addr) = addrs.next() {
                    if let Ok(mut stream) = std::net::TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
                        let _ = stream.set_read_timeout(Some(Duration::from_millis(1000)));
                        let mut banner = [0u8; 4];
                        use std::io::Read;
                        if stream.read_exact(&mut banner).is_ok() && &banner == b"SSH-" {
                            ready = true;
                            break;
                        }
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        if !ready {
            return Err(format!("timed out after {}s waiting for SSH on {}:{}", args.wait_timeout, host, port));
        }
        std::thread::sleep(Duration::from_millis(500));
    }

    // 5. Build and exec ssh
    let mut ssh_cmd = Command::new("ssh");
    ssh_cmd.args([
        "-o", "StrictHostKeyChecking=no",
        "-o", "UserKnownHostsFile=/dev/null",
        "-o", "LogLevel=ERROR",
        "-o", "ConnectTimeout=5",
        "-o", "ConnectionAttempts=5",
        "-p", &port.to_string(),
    ]);

    if let Some(ref id) = identity {
        if id.is_file() {
            ssh_cmd.args(["-i", &id.to_string_lossy()]);
        }
    }

    let target_dest = format!("{}@{}", user, host);
    ssh_cmd.arg(&target_dest);

    if !args.command.is_empty() {
        ssh_cmd.args(&args.command);
    }

    let err = ssh_cmd.exec();
    Err(format!("failed to execute ssh: {}", err))
}

pub fn do_status(target: &str, state_root: &Path) -> Result<(), String> {
    let (state_dir, _) = resolve_target_dir(target, state_root)?;
    if let Some(status) = live_status(&state_dir) {
        println!("{}", serde_json::to_string_pretty(&status).unwrap());
        return Ok(());
    }

    let last_path = state_dir.join("last.json");
    if last_path.is_file() {
        let content = fs::read_to_string(&last_path).map_err(|e| e.to_string())?;
        let mut parsed: serde_json::Value = serde_json::from_str(&content).map_err(|e| e.to_string())?;
        if let Some(obj) = parsed.as_object_mut() {
            obj.insert("ip".into(), serde_json::Value::Null);
        }
        println!("{}", serde_json::to_string_pretty(&parsed).unwrap());
    } else {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({"state": "stopped", "ip": null})).unwrap());
    }
    Ok(())
}

fn detect_os_from_serial_log(serial_log_path: &Path) -> Option<String> {
    if let Ok(content) = fs::read_to_string(serial_log_path) {
        for line in content.lines().take(500) {
            if let Some(idx) = line.find("FreeBSD ") {
                let rest = &line[idx..];
                let parts: Vec<&str> = rest.split_whitespace().collect();
                if parts.len() >= 2 && parts[0] == "FreeBSD" && (parts[1].contains("-RELEASE") || parts[1].contains("-CURRENT") || parts[1].contains("-STABLE")) {
                    return Some(format!("FreeBSD {}", parts[1]));
                }
            }
        }
    }
    None
}

fn enrich_vm_item(item: &mut serde_json::Value, state_root: &Path) {
    let name = match item.get("name").and_then(|n| n.as_str()) {
        Some(n) => n.to_string(),
        None => return,
    };

    let config_path = item.get("config").and_then(|c| c.as_str()).map(PathBuf::from)
        .or_else(|| find_config_for_target(&name, state_root).ok());

    if let Some(ref cp) = config_path {
        if item.get("config").is_none() {
            if let Some(obj) = item.as_object_mut() {
                obj.insert("config".into(), serde_json::json!(cp.to_string_lossy()));
            }
        }
        let needs_ssh = item.get("ssh_port").is_none() || item.get("ssh_port").map_or(false, |v| v.is_null());
        let needs_vnc = item.get("vnc").is_none() || item.get("vnc").map_or(false, |v| v.is_null());
        let needs_os = item.get("os").is_none() || item.get("os").map_or(false, |v| v.is_null());
        if needs_ssh || needs_vnc || needs_os {
            if let Ok(content) = fs::read_to_string(cp) {
                if let Ok(cfg) = serde_json::from_str::<VmConfig>(&content) {
                    if let Some(obj) = item.as_object_mut() {
                        if needs_ssh {
                            if let Some(port) = cfg.network.as_ref().and_then(|n| n.ssh_port) {
                                obj.insert("ssh_port".into(), serde_json::json!(port));
                            }
                        }
                        if needs_vnc {
                            if let Some(ref v) = cfg.vnc {
                                obj.insert("vnc".into(), serde_json::json!(v.bind));
                            }
                        }
                        if needs_os {
                            let os_val = cfg.os.clone().or_else(|| {
                                let serial_log = state_root.join(&name).join("serial.log");
                                detect_os_from_serial_log(&serial_log)
                            }).or_else(|| {
                                for d in &cfg.disks {
                                    let p = d.path();
                                    if p.contains("14.4") { return Some("FreeBSD 14.4-RELEASE".into()); }
                                    if p.contains("14.5") { return Some("FreeBSD 14.5-RELEASE".into()); }
                                    if p.contains("15.1") { return Some("FreeBSD 15.1-RELEASE".into()); }
                                }
                                Some("FreeBSD".into())
                            });
                            if let Some(os) = os_val {
                                obj.insert("os".into(), serde_json::json!(os));
                            }
                        }
                    }
                }
            }
        }
    }

    if item.get("os").is_none() || item.get("os").map_or(false, |v| v.is_null()) {
        let serial_log = state_root.join(&name).join("serial.log");
        let os = detect_os_from_serial_log(&serial_log).unwrap_or_else(|| "FreeBSD".into());
        if let Some(obj) = item.as_object_mut() {
            obj.insert("os".into(), serde_json::json!(os));
        }
    }
}

pub fn do_list(state_root: &Path, as_json: bool) -> Result<(), String> {
    let mut list = Vec::new();
    let entries = match fs::read_dir(state_root) {
        Ok(entries) => Some(entries),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    for entry in entries.into_iter().flatten().flatten() {
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }

        if let Some(status) = live_status(&p) {
            list.push(status);
        } else {
            let last_path = p.join("last.json");
            if let Ok(content) = fs::read_to_string(&last_path) {
                if let Ok(mut last) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(obj) = last.as_object_mut() {
                        obj.insert("name".into(), serde_json::json!(name));
                    }
                    list.push(last);
                    continue;
                }
            }
            list.push(serde_json::json!({
                "name": name,
                "state": "stopped"
            }));
        }
    }

    if state_root == default_state_root() {
        let vms_root = default_vms_root();
        if vms_root.is_dir() {
            if let Ok(entries) = fs::read_dir(&vms_root) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.is_dir() {
                        let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
                        if !name.starts_with('.') && !list.iter().any(|item| item.get("name").and_then(|n| n.as_str()) == Some(&name)) {
                            let cfg_file = p.join(format!("{}.json", name));
                            if cfg_file.is_file() {
                                list.push(serde_json::json!({
                                    "name": name,
                                    "state": "stopped",
                                    "config": cfg_file.to_string_lossy()
                                }));
                            }
                        }
                    }
                }
            }
        }
    }

    for item in &mut list {
        if item.get("state").and_then(|s| s.as_str()) != Some("running") {
            if let Some(obj) = item.as_object_mut() {
                obj.insert("ip".into(), serde_json::Value::Null);
            }
        }
        enrich_vm_item(item, state_root);
    }

    list.sort_by(|a, b| {
        let na = a.get("name").and_then(|n| n.as_str()).unwrap_or("");
        let nb = b.get("name").and_then(|n| n.as_str()).unwrap_or("");
        na.cmp(nb)
    });

    if as_json {
        println!("{}", serde_json::to_string_pretty(&list).unwrap());
    } else if list.is_empty() {
        println!("No VMs configured in {:?}", state_root);
    } else {
        println!("{:<20} {:<10} {:<24} {:<8} {:<10}", "NAME", "STATE", "OS", "PID", "STARTED");
        println!("{:-<20} {:-<10} {:-<24} {:-<8} {:-<10}", "", "", "", "", "");
        for item in &list {
            let name = item.get("name").and_then(|n| n.as_str()).unwrap_or("-");
            let state = item.get("state").and_then(|s| s.as_str()).unwrap_or("-");
            let os = item.get("os").and_then(|o| o.as_str()).unwrap_or("FreeBSD");
            let pid = item.get("pid").and_then(|p| p.as_u64()).map(|p| p.to_string()).unwrap_or_else(|| "-".into());
            let started = item.get("started").and_then(|s| s.as_f64()).map(|s| format!("{:.0}s ago", SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64() - s)).unwrap_or_else(|| "-".into());
            println!("{:<20} {:<10} {:<24} {:<8} {:<10}", name, state, os, pid, started);
        }
    }
    Ok(())
}

pub fn do_config_get(key: &str, _state_root: &Path) -> Result<(), String> {
    match key {
        "vms_dir" | "vms-dir" => {
            let current = util::default_vms_root();
            println!("{}", current.display());
            Ok(())
        }
        _ => Err(format!("unknown configuration key '{}'. Supported keys: vms-dir", key)),
    }
}

pub fn do_config_set(key: &str, value: &str, state_root: &Path) -> Result<(), String> {
    let mut settings = util::load_cube_settings(state_root);
    match key {
        "vms_dir" | "vms-dir" => {
            let trimmed = value.trim();
            if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("default") {
                settings.vms_dir = None;
                util::save_cube_settings(state_root, &settings)?;
                println!("Reset vms-dir to default: {}", util::default_vms_root().display());
            } else {
                let resolved = util::resolve_path(trimmed, Path::new("."))?;
                fs::create_dir_all(&resolved).map_err(|e| format!("cannot create directory {:?}: {}", resolved, e))?;
                settings.vms_dir = Some(resolved.display().to_string());
                util::save_cube_settings(state_root, &settings)?;
                println!("Set vms-dir to: {}", resolved.display());
            }
            Ok(())
        }
        _ => Err(format!("unknown configuration key '{}'. Supported keys: vms-dir", key)),
    }
}

pub fn do_config_list(state_root: &Path) -> Result<(), String> {
    let settings = util::load_cube_settings(state_root);
    let vms_dir = util::default_vms_root();
    println!("vms-dir = {}", vms_dir.display());
    if let Some(ref _explicit) = settings.vms_dir {
        println!("  (customized in {})", util::settings_path(state_root).display());
    } else {
        println!("  (default: ~/Cube)");
    }
    Ok(())
}

// MARK: - Smart Port Auto-Allocation

pub fn is_port_available(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
}

pub fn parse_vnc_port(bind: &str) -> Option<u16> {
    let clean = bind.trim();
    if let Some(idx) = clean.rfind(':') {
        let suffix = &clean[idx + 1..];
        if let Ok(val) = suffix.parse::<u16>() {
            return if val < 100 { Some(5900 + val) } else { Some(val) };
        }
    } else if let Ok(val) = clean.parse::<u16>() {
        return if val < 100 { Some(5900 + val) } else { Some(val) };
    }
    None
}

pub fn collect_used_ports(state_root: &Path, vms_root: &Path) -> (std::collections::HashSet<u16>, std::collections::HashSet<u16>) {
    let mut used_ssh = std::collections::HashSet::new();
    let mut used_vnc = std::collections::HashSet::new();
    let mut candidate_configs = Vec::new();

    // 1. Scan state_root
    if state_root.is_dir() {
        if let Ok(entries) = fs::read_dir(state_root) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
                    if !name.starts_with('.') {
                        if let Ok(cfg_path) = find_config_for_target(&name, state_root) {
                            candidate_configs.push(cfg_path);
                        }
                    }
                }
            }
        }
    }

    // 2. Scan vms_root
    if vms_root.is_dir() {
        if let Ok(entries) = fs::read_dir(vms_root) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    let name = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
                    if !name.starts_with('.') {
                        let cfg_file = p.join(format!("{}.json", name));
                        if cfg_file.is_file() {
                            candidate_configs.push(cfg_file);
                        }
                    }
                }
            }
        }
    }

    candidate_configs.sort();
    candidate_configs.dedup();

    for cfg_path in candidate_configs {
        if let Ok(content) = fs::read_to_string(&cfg_path) {
            if let Ok(cfg) = serde_json::from_str::<VmConfig>(&content) {
                if let Some(ref net) = cfg.network {
                    if let Some(port) = net.ssh_port {
                        used_ssh.insert(port);
                    }
                    for forward in &net.forwards {
                        if let Some((host_part, _)) = forward.split_once('=') {
                            if let Some((_, port_str)) = host_part.rsplit_once(':') {
                                if let Ok(p) = port_str.parse::<u16>() {
                                    used_ssh.insert(p);
                                }
                            } else if let Ok(p) = host_part.parse::<u16>() {
                                used_ssh.insert(p);
                            }
                        }
                    }
                }
                if let Some(ref vnc) = cfg.vnc {
                    if let Some(port) = parse_vnc_port(&vnc.bind) {
                        used_vnc.insert(port);
                    }
                }
            }
        }
    }

    (used_ssh, used_vnc)
}

pub fn find_next_available_ssh_port(state_root: &Path, vms_root: &Path, extra_reserved: &[u16]) -> u16 {
    let (mut used_ssh, _) = collect_used_ports(state_root, vms_root);
    for &p in extra_reserved {
        used_ssh.insert(p);
    }
    let mut port = 2222u16;
    let max_probe = port.saturating_add(500);
    while port <= max_probe {
        if !used_ssh.contains(&port) && is_port_available(port) {
            return port;
        }
        port += 1;
    }
    // Fallback: pick the first port not configured in any VM
    let mut fallback = 2222u16;
    while fallback < 65535 {
        if !used_ssh.contains(&fallback) {
            return fallback;
        }
        fallback += 1;
    }
    2222
}

pub fn find_next_available_vnc_port(state_root: &Path, vms_root: &Path) -> u16 {
    let (_, used_vnc) = collect_used_ports(state_root, vms_root);
    let mut port = 5901u16;
    let max_probe = port.saturating_add(500);
    while port <= max_probe {
        if !used_vnc.contains(&port) && is_port_available(port) {
            return port;
        }
        port += 1;
    }
    // Fallback: pick the first port not configured in any VM
    let mut fallback = 5901u16;
    while fallback < 65535 {
        if !used_vnc.contains(&fallback) {
            return fallback;
        }
        fallback += 1;
    }
    5901
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_config_identity_is_relative_to_config_but_cli_identity_is_not() {
        let config: VmConfig = serde_json::from_value(serde_json::json!({
            "name": "example",
            "boot": {"mode": "uefi"},
            "shutdown": {"identity_file": "keys/id_ed25519", "user": "root"}
        })).unwrap();
        let config_path = Path::new("/tmp/vms/example/example.json");
        assert_eq!(
            ssh_identity(None, Some(&config), Some(config_path)),
            Some(PathBuf::from("/tmp/vms/example/keys/id_ed25519"))
        );
        let override_key = PathBuf::from("my-key");
        assert_eq!(
            ssh_identity(Some(override_key.clone()), Some(&config), Some(config_path)),
            Some(override_key)
        );
    }

    #[test]
    fn test_parse_vnc_port() {
        assert_eq!(parse_vnc_port("127.0.0.1:5901"), Some(5901));
        assert_eq!(parse_vnc_port(":1"), Some(5901));
        assert_eq!(parse_vnc_port("1"), Some(5901));
        assert_eq!(parse_vnc_port("5902"), Some(5902));
        assert_eq!(parse_vnc_port("0.0.0.0:5905"), Some(5905));
        assert_eq!(parse_vnc_port("invalid"), None);
    }

    #[test]
    fn test_smart_port_allocation() {
        let dummy_dir = Path::new("/nonexistent_dir_for_test");
        let ssh_port = find_next_available_ssh_port(dummy_dir, dummy_dir, &[2222]);
        assert!(ssh_port >= 2223);
        assert_ne!(ssh_port, 2222);

        let next_ssh = find_next_available_ssh_port(dummy_dir, dummy_dir, &[2222, ssh_port]);
        assert!(next_ssh > ssh_port);

        let vnc_port = find_next_available_vnc_port(dummy_dir, dummy_dir);
        assert!(vnc_port >= 5901);
    }
}

