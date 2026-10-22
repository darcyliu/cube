use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::util::{canonical_path_buf, find_cube_binary, require, resolve_path};

pub fn find_default_firmware(user_fw: Option<&PathBuf>, cube_binary: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(fw) = user_fw {
        let resolved = resolve_path(&fw.to_string_lossy(), Path::new("."))?;
        require(resolved.is_file(), format!("firmware file not found: {:?}", resolved))?;
        return canonical_path_buf(&resolved);
    }

    // macOS current_exe() is the path used to launch, so ~/.local/bin/cubectl stays a
    // symlink and its parents are not the Cube.app bundle. Resolve first, then walk.
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(found) = firmware_near_binary(&current_exe) {
            return Ok(found);
        }
    }

    if let Ok(cube_bin) = find_cube_binary(cube_binary, None, Path::new(".")) {
        if let Some(found) = firmware_near_binary(&cube_bin) {
            return Ok(found);
        }
    }

    Err("UEFI boot requires --firmware <PATH>".into())
}

/// Firmware embedded beside a `cubectl` or `cube` binary.
///
/// Accepts a dev build (`.../firmware/QEMU_EFI.fd`), the main app
/// (`Cube.app/Contents/MacOS/cubectl` -> `Contents/Resources/firmware/QEMU_EFI.fd`),
/// and the nested helper (`Contents/Helpers/cube.app/.../cube`). Symlinks are
/// resolved before the walk so a `~/.local/bin` launcher still finds the bundle.
fn firmware_near_binary(exe: &Path) -> Option<PathBuf> {
    let mut curr = exe.canonicalize().ok()?;
    for _ in 0..8 {
        let Some(parent) = curr.parent() else { break };
        for rel in ["Resources/firmware/QEMU_EFI.fd", "firmware/QEMU_EFI.fd"] {
            let cand = parent.join(rel);
            if cand.is_file() {
                return canonical_path_buf(&cand).ok();
            }
        }
        curr = parent.to_path_buf();
    }
    None
}

pub fn auto_detect_iso() -> Option<PathBuf> {
    if let Ok(entries) = fs::read_dir(".") {
        for entry in entries.flatten() {
            let p = entry.path();
            if let Some(name) = p.file_name().and_then(|n| n.to_str()) {
                if name.ends_with(".iso") && (name.contains("arm64") || name.contains("aarch64")) {
                    return Some(p);
                }
            }
        }
    }
    None
}

pub fn resolve_ssh_public_key(specified: Option<&str>) -> (Option<String>, Option<String>, Option<PathBuf>) {
    if let Some(s) = specified {
        let trimmed = s.trim();
        let path = Path::new(trimmed);
        if path.is_file() {
            if let Ok(content) = fs::read_to_string(path) {
                let key_str = content.trim().to_string();
                let priv_key = if trimmed.ends_with(".pub") {
                    let p = PathBuf::from(&trimmed[..trimmed.len() - 4]);
                    if p.is_file() { Some(p) } else { None }
                } else {
                    None
                };
                return (Some(key_str), Some(format!("{} (user specified)", path.display())), priv_key);
            }
        }
        return (Some(trimmed.to_string()), Some("inline SSH public key".into()), None);
    }

    // Check user preference from settings.json if configured
    let state_root = crate::util::default_state_root();
    let settings = crate::util::load_cube_settings(&state_root);
    if let Some(ref custom_path) = settings.ssh_key_path {
        if let Ok(resolved) = crate::util::resolve_path(custom_path, Path::new(".")) {
            if resolved.is_file() {
                if let Ok(content) = fs::read_to_string(&resolved) {
                    let key_str = content.trim().to_string();
                    if !key_str.is_empty() {
                        let priv_candidate = resolved.with_extension("");
                        let priv_key = if priv_candidate.is_file() { Some(priv_candidate) } else { None };
                        return (Some(key_str), Some(format!("{} (configured in preferences)", custom_path)), priv_key);
                    }
                }
            }
        }
    }

    if let Some(home) = std::env::var_os("HOME") {
        let home_path = PathBuf::from(home);
        for key_type in &["id_ed25519", "id_rsa", "id_ecdsa"] {
            let pub_path = home_path.join(".ssh").join(format!("{}.pub", key_type));
            if pub_path.is_file() {
                if let Ok(content) = fs::read_to_string(&pub_path) {
                    let key_str = content.trim().to_string();
                    if !key_str.is_empty() {
                        let priv_path = home_path.join(".ssh").join(key_type);
                        let priv_key = if priv_path.is_file() { Some(priv_path) } else { None };
                        return (Some(key_str), Some(format!("~/.ssh/{}.pub (auto-detected)", key_type)), priv_key);
                    }
                }
            }
        }
    }

    (None, None, None)
}

pub fn build_cidata_seed(
    staging_dir: &Path,
    seed_iso_path: &Path,
    hostname: &str,
    password: &str,
    ssh_pub_key: Option<&str>,
    filesystem: &str,
    base_dist: &str,
) -> Result<(), String> {
    let cidata_dir = staging_dir.join("cidata");
    if cidata_dir.exists() {
        let _ = fs::remove_dir_all(&cidata_dir);
    }
    fs::create_dir_all(&cidata_dir).map_err(|e| format!("cannot create cidata directory: {}", e))?;

    let meta_data = format!("instance-id: {}-01\nlocal-hostname: {}\n", hostname, hostname);
    fs::write(cidata_dir.join("meta-data"), meta_data).map_err(|e| format!("cannot write meta-data: {}", e))?;

    let mut user_data = format!(
        "#cloud-config\nhostname: {}\npassword: {}\nchpasswd: {{ expire: False }}\nssh_pwauth: True\n",
        hostname, password
    );
    if let Some(pub_key) = ssh_pub_key {
        user_data.push_str(&format!("ssh_authorized_keys:\n  - \"{}\"\n", pub_key.trim()));
    }
    fs::write(cidata_dir.join("user-data"), user_data).map_err(|e| format!("cannot write user-data: {}", e))?;

    let mut script = String::new();
    if filesystem == "zfs" {
        script.push_str("export ZFSBOOT_DISKS=\"vtbd0\"\n");
        script.push_str("export ZFSBOOT_VDEV_TYPE=\"stripe\"\n");
        script.push_str("export ZFSBOOT_POOL_NAME=\"zroot\"\n");
        script.push_str("export ZFSBOOT_PARTITION_SCHEME=\"GPT\"\n");
        script.push_str("export ZFSBOOT_BOOT_TYPE=\"UEFI\"\n");
        script.push_str("export ZFSBOOT_SWAP_SIZE=\"1g\"\n");
        script.push_str("export ZFSBOOT_CONFIRM_LAYOUT=\"\"\n");
    } else {
        script.push_str("PARTITIONS=\"vtbd0\"\n");
    }

    if base_dist == "pkgbase" {
        script.push_str("export BSDINSTALL_PKG_REPOS_DIR=\"/usr/freebsd-packages/repos\"\n");
        script.push_str("export COMPONENTS=\"base\"\n");
    } else {
        script.push_str("DISTRIBUTIONS=\"kernel.txz base.txz\"\n");
    }
    script.push_str("export nonInteractive=\"YES\"\n\n");

    script.push_str("#!/bin/sh\n");
    script.push_str(&format!(
        "cat << 'RC' > /etc/rc.conf\n\
hostname=\"{}\"\n\
ifconfig_vtnet0=\"DHCP\"\n\
sshd_enable=\"YES\"\n\
growfs_enable=\"YES\"\n\
sendmail_enable=\"NONE\"\n\
dumpdev=\"NO\"\n\
RC\n\n\
echo \"{}\" | pw usermod root -h 0\n\
sed -i '' 's/#PermitRootLogin.*/PermitRootLogin yes/' /etc/ssh/sshd_config\n\
sed -i '' 's/#PasswordAuthentication.*/PasswordAuthentication yes/' /etc/ssh/sshd_config\n\
cat << 'DHCP' > /etc/dhclient.conf\n\
supersede domain-name-servers 1.1.1.1, 8.8.8.8;\n\
DHCP\n\
echo \"nameserver 1.1.1.1\" > /etc/resolv.conf\n\
echo \"nameserver 8.8.8.8\" >> /etc/resolv.conf\n",
        hostname, password
    ));

    if let Some(pub_key) = ssh_pub_key {
        script.push_str(&format!(
            "mkdir -p /root/.ssh\n\
echo \"{}\" >> /root/.ssh/authorized_keys\n\
chmod 700 /root/.ssh && chmod 600 /root/.ssh/authorized_keys\n",
            pub_key.trim()
        ));
    }

    if filesystem == "zfs" {
        script.push_str(
            "sysrc zfs_enable=\"YES\"\n\
cat << 'LOADER' >> /boot/loader.conf\n\
zfs_load=\"YES\"\n\
boot_serial=\"YES\"\n\
autoboot_delay=\"1\"\n\
LOADER\n",
        );
    } else {
        script.push_str(
            "cat << 'LOADER' >> /boot/loader.conf\n\
boot_serial=\"YES\"\n\
autoboot_delay=\"1\"\n\
LOADER\n",
        );
    }

    fs::write(cidata_dir.join("installerconfig"), script)
        .map_err(|e| format!("cannot write installerconfig: {}", e))?;

    let status = Command::new("hdiutil")
        .args([
            "makehybrid",
            "-o",
            seed_iso_path.to_str().unwrap(),
            "-iso",
            "-joliet",
            "-default-volume-name",
            "CIDATA",
            cidata_dir.to_str().unwrap(),
        ])
        .output()
        .map_err(|e| format!("failed to execute hdiutil: {}", e))?;
    require(
        status.status.success(),
        format!("hdiutil makehybrid failed: {}", String::from_utf8_lossy(&status.stderr)),
    )?;
    Ok(())
}

pub fn extract_kernel_from_iso(iso_path: &Path, staging_dir: &Path) -> Result<PathBuf, String> {
    let status = Command::new("tar")
        .args([
            "-xf",
            iso_path.to_str().unwrap(),
            "-C",
            staging_dir.to_str().unwrap(),
            "boot/kernel/kernel",
        ])
        .output()
        .map_err(|e| format!("failed to execute tar: {}", e))?;
    require(
        status.status.success(),
        format!(
            "tar failed to extract kernel from {:?}: {}",
            iso_path,
            String::from_utf8_lossy(&status.stderr)
        ),
    )?;
    let kernel_path = staging_dir.join("boot/kernel/kernel");
    require(
        kernel_path.is_file(),
        format!("extracted kernel not found at {:?}", kernel_path),
    )?;
    Ok(kernel_path)
}

pub fn send_uart_command(uart_path: &Path, text: &str) -> Result<(), String> {
    let next_path = uart_path.with_extension("next");
    let start = Instant::now();
    while uart_path.exists() {
        if start.elapsed() > Duration::from_secs(10) {
            return Err("timeout waiting for previous UART command to be consumed by VMM".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut payload = text.to_string();
    payload.push('\n');
    fs::write(&next_path, payload).map_err(|e| format!("cannot write UART mailbox {:?}: {}", next_path, e))?;
    fs::rename(&next_path, uart_path).map_err(|e| format!("cannot publish UART command {:?}: {}", uart_path, e))?;
    Ok(())
}

pub fn read_log_tail(path: &Path, num_lines: usize) -> String {
    if let Ok(content) = fs::read_to_string(path) {
        let lines: Vec<&str> = content.lines().collect();
        if lines.len() <= num_lines {
            lines.join("\n")
        } else {
            lines[lines.len() - num_lines..].join("\n")
        }
    } else {
        "(console log unavailable)".into()
    }
}

pub fn is_shell_ready(console_content: &str) -> bool {
    if let Some(pos) = console_content.rfind("RETURN for /bin/sh") {
        let after = &console_content[pos..];
        for line in after.lines() {
            let trimmed = line.trim();
            if trimmed == "#" || trimmed.ends_with('#') || trimmed.contains("# ") {
                return true;
            }
        }
    }
    false
}

pub fn run_unattended_install(
    vm_name: &str,
    iso_path: &Path,
    target_disk: &Path,
    dest_dir: &Path,
    cube_bin: &Path,
    hostname: &str,
    password: &str,
    ssh_pub_key: Option<&str>,
    ssh_key_desc: Option<&str>,
    filesystem: &str,
    base_dist: &str,
    ram_mb: u64,
    cpus: u32,
) -> Result<(), String> {
    let staging_dir = dest_dir.join(".install_staging");
    if staging_dir.exists() {
        let _ = fs::remove_dir_all(&staging_dir);
    }
    fs::create_dir_all(&staging_dir)
        .map_err(|e| format!("cannot create staging directory {:?}: {}", staging_dir, e))?;

    let disk_size_bytes = fs::metadata(target_disk).map(|m| m.len()).unwrap_or(0);
    let disk_size_desc = if disk_size_bytes > 0 {
        format!("{:.1} GiB", disk_size_bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else {
        "target disk".into()
    };

    println!("==> Starting automated unattended installation for '{}'...", vm_name);
    println!("    Installer ISO: {}", iso_path.display());
    println!("    Target Disk:   {} ({})", target_disk.display(), disk_size_desc);
    println!("    Filesystem:    {}", filesystem);
    println!("    Root Password: {}", password);
    if let Some(desc) = ssh_key_desc {
        println!("    SSH Key:       {}", desc);
    }

    print!("  [1/4] Preparing CIDATA seed drive and installer kernel... ");
    std::io::stdout().flush().ok();

    let seed_iso = staging_dir.join("seed.iso");
    build_cidata_seed(&staging_dir, &seed_iso, hostname, password, ssh_pub_key, filesystem, base_dist)?;

    let extracted_kernel = extract_kernel_from_iso(iso_path, &staging_dir)?;
    println!("done.");

    let uart_path = staging_dir.join("uart");
    let console_log = staging_dir.join("console.log");
    let _ = fs::remove_file(&uart_path);
    let _ = fs::remove_file(&console_log);

    println!("  [2/4] Booting installer kernel into single-user mode...");

    let mut cmd = Command::new(cube_bin);
    cmd.arg("--freebsd").arg(&extracted_kernel);
    cmd.arg("--disk").arg(target_disk);
    cmd.arg("--disk").arg(format!("{},ro", seed_iso.display()));
    cmd.arg("--cdrom").arg(iso_path);
    cmd.arg("--root").arg("cd9660:/dev/vtbd2");
    cmd.arg("--net").arg("none");
    cmd.arg("--cpus").arg(cpus.to_string());
    cmd.arg("--ram").arg(ram_mb.to_string());
    cmd.arg("--serial-log").arg(&console_log);
    cmd.env("VMM_UART_IN", &uart_path);
    let stderr_log = staging_dir.join("stderr.log");
    let stderr_file = fs::File::create(&stderr_log).map_err(|e| format!("cannot create stderr log: {}", e))?;
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::from(stderr_file));

    let mut child = cmd.spawn().map_err(|e| format!("failed to launch installer VM: {}", e))?;

    struct ChildGuard<'a>(&'a mut std::process::Child, bool);
    impl<'a> Drop for ChildGuard<'a> {
        fn drop(&mut self) {
            if self.1 {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
    }
    let mut guard = ChildGuard(&mut child, true);

    let install_deadline = Instant::now() + Duration::from_secs(900); // 15 min timeout
    let mut sent_return = false;
    let mut sent_install_cmd = false;
    let mut reported_base = false;
    let mut reported_kernel = false;
    let mut install_complete = false;

    while Instant::now() < install_deadline {
        match guard.0.try_wait() {
            Ok(Some(status)) => {
                if status.success() && fs::read_to_string(&console_log)
                        .map(|s| installation_complete(&s))
                        .unwrap_or(false)
                {
                    guard.1 = false;
                    break;
                } else {
                    let tail = read_log_tail(&console_log, 20);
                    let err_tail = read_log_tail(&stderr_log, 20);
                    let failed_log = dest_dir.join("install-failed.log");
                    let mut combined = fs::read_to_string(&console_log).unwrap_or_default();
                    if let Ok(err_content) = fs::read_to_string(&stderr_log) {
                        if !err_content.is_empty() {
                            combined.push_str("\n--- VM STDERR ---\n");
                            combined.push_str(&err_content);
                        }
                    }
                    let _ = fs::write(&failed_log, combined);
                    let msg = if !err_tail.trim().is_empty() {
                        format!("VM error output:\n{}\nLast console output:\n{}", err_tail, tail)
                    } else {
                        format!("Last console output:\n{}", tail)
                    };
                    return Err(format!(
                        "installer VM exited unexpectedly (status: {}). Diagnostic log saved to {}.\n{}",
                        status,
                        failed_log.display(),
                        msg
                    ));
                }
            }
            Ok(None) => {}
            Err(e) => return Err(format!("error checking installer VM status: {}", e)),
        }

        if let Ok(content) = fs::read_to_string(&console_log) {
            if !sent_return {
                if content.contains("RETURN for /bin/sh") {
                    std::thread::sleep(Duration::from_millis(1000));
                    send_uart_command(&uart_path, "")?;
                    sent_return = true;
                }
            } else if !sent_install_cmd {
                if is_shell_ready(&content) {
                    std::thread::sleep(Duration::from_millis(1000));
                    println!("  [3/4] Running bsdinstall (partitioning disk, installing base & kernel packages)...");
                    let install_script = "mdmfs -s 64M md1 /tmp && mdmfs -s 64M md2 /var && (mount -t cd9660 /dev/iso9660/CIDATA /media 2>/dev/null || mount -t cd9660 /dev/vtbd1 /media 2>/dev/null) && bsdinstall script /media/installerconfig && echo VMM_INSTALL_COMPLETE && poweroff";
                    send_uart_command(&uart_path, install_script)?;
                    sent_install_cmd = true;
                }
            } else {
                if !reported_base && content.contains("base.txz") {
                    println!("        Extracting base.txz...");
                    reported_base = true;
                }
                if !reported_kernel && content.contains("kernel.txz") {
                    println!("        Extracting kernel.txz...");
                    reported_kernel = true;
                }
                if !install_complete && installation_complete(&content) {
                    println!("  [4/4] Installation complete! Waiting for clean guest poweroff...");
                    install_complete = true;
                }
            }
        }

        std::thread::sleep(Duration::from_millis(500));
    }

    if guard.1 {
        let _ = guard.0.kill();
        let _ = guard.0.wait();
        guard.1 = false;
        let failed_log = dest_dir.join("install-failed.log");
        let _ = fs::copy(&console_log, &failed_log);
        return Err(format!("timeout during unattended installation (log: {})", failed_log.display()));
    }

    let kernel_dest = dest_dir.join("kernel");
    if !kernel_dest.exists() && extracted_kernel.is_file() {
        let _ = fs::copy(&extracted_kernel, &kernel_dest);
    }

    let _ = fs::remove_dir_all(&staging_dir);
    println!("✓ Guest powered off cleanly.\n");
    Ok(())
}

pub fn installation_complete(log: &str) -> bool {
    log.lines().any(|line| line.trim() == "VMM_INSTALL_COMPLETE")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn completion_requires_standalone_marker() {
        assert!(!installation_complete("# bsdinstall script /media/installerconfig && echo VMM_INSTALL_COMPLETE && poweroff\ninstallation failed"));
        assert!(installation_complete("done\r\nVMM_INSTALL_COMPLETE\r\n"));
    }

    struct TempDir(PathBuf);
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn touch(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, b"fw").unwrap();
    }

    #[test]
    fn finds_app_bundle_firmware_through_external_symlink() {
        let root = std::env::temp_dir().join(format!(
            "cube-fw-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _guard = TempDir(root.clone());
        let fw = root.join("Cube.app/Contents/Resources/firmware/QEMU_EFI.fd");
        let bin = root.join("Cube.app/Contents/MacOS/cubectl");
        touch(&fw);
        touch(&bin);
        let link = root.join("local-bin/cubectl");
        fs::create_dir_all(link.parent().unwrap()).unwrap();
        symlink(&bin, &link).unwrap();

        let found = firmware_near_binary(&link).expect("bundle firmware via symlink");
        assert_eq!(found, fw.canonicalize().unwrap());

        let direct = firmware_near_binary(&bin).expect("bundle firmware via real binary");
        assert_eq!(direct, fw.canonicalize().unwrap());
        assert!(!link.parent().unwrap().join("Resources/firmware/QEMU_EFI.fd").is_file());
    }

    #[test]
    fn finds_main_bundle_firmware_from_nested_helper() {
        let root = std::env::temp_dir().join(format!(
            "cube-fw-helper-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _guard = TempDir(root.clone());
        let fw = root.join("Cube.app/Contents/Resources/firmware/QEMU_EFI.fd");
        let helper = root.join("Cube.app/Contents/Helpers/cube.app/Contents/MacOS/cube");
        touch(&fw);
        touch(&helper);

        let found = firmware_near_binary(&helper).expect("firmware from helper binary");
        assert_eq!(found, fw.canonicalize().unwrap());
    }
}
