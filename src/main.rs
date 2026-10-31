//! Intel-only x86_64 VMM. The library has no `compile_error!` so
//! `cargo test --lib` still builds on other hosts.

#[cfg(not(all(target_arch = "x86_64", target_os = "macos")))]
compile_error!("vmm-intel builds only for x86_64-apple-darwin");

mod hypervisor;
mod machine;
mod vcpu;
mod vm;

use std::os::unix::process::CommandExt;
use std::process;

use vmm::cli::{self, Boot};

fn main() {
    let raw: Vec<String> = std::env::args().collect();
    if raw.get(1).is_some_and(|arg| arg == "--internal-9p-worker") {
        if let Err(error) = vmm::shared_fs::worker(&raw) {
            eprintln!("[9P helper] {error}");
            process::exit(1);
        }
        return;
    }
    enforce_host_gate();
    if raw.get(1).is_some_and(|arg| arg == "--vmnet-service") {
        if let Err(error) = vmm::vmnet_service::run(&raw) {
            eprintln!("[vmnet] {error}");
            process::exit(1);
        }
        return;
    }
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli::parse(&args) {
        cli::Cli::Help => {
            println!("{}", cli::help_text());
            process::exit(1);
        }
        cli::Cli::Error(message) => {
            eprintln!("{message}");
            process::exit(1);
        }
        cli::Cli::Run(run) => {
            let dones = match run.boot {
                Boot::Payload(path) => vm::run_payload(
                    path,
                    run.serial_log,
                    run.cpus,
                    run.ram_mb,
                    run.disks,
                    run.net,
                    run.forwards,
                    run.share,
                    run.vnc,
                    run.display,
                ),
                Boot::Uefi(path) => vm::run_uefi(
                    path,
                    run.serial_log,
                    run.cpus,
                    run.ram_mb,
                    run.disks,
                    run.net,
                    run.forwards,
                    run.efi_vars,
                    run.share,
                    run.vnc,
                    run.display,
                ),
            };
            let mut failed = false;
            for done in dones {
                if let Some(ns) = done.exec_ns {
                    eprintln!("[VMM] vCPU {} exec time: {ns} ns", done.index);
                }
                if let Some(error) = done.error {
                    eprintln!("{error}");
                    failed = true;
                }
            }
            if failed {
                process::exit(1);
            }
            if vmm::terminal::reboot_requested() {
                let exe = match std::env::current_exe() {
                    Ok(path) => path,
                    Err(error) => {
                        eprintln!("re-exec: {error}");
                        process::exit(1);
                    }
                };
                let error = process::Command::new(exe)
                    .args(std::env::args().skip(1))
                    .exec();
                eprintln!("re-exec failed: {error}");
                process::exit(1);
            }
        }
    }
}

/// Marketing major >= 10, before any `hv_*` call.
/// 10.15 may omit `kern.osproductversion`; Darwin 19 is macOS 10.15.
/// Darwin 20 is macOS 11. Do not compare the Darwin major to 26.
fn enforce_host_gate() {
    if translated_or_arm_host() {
        eprintln!(
            "vmm-intel refuses to run under Rosetta or on Apple Silicon \
             (sysctl.proc_translated=1 or hw.optional.arm64=1)."
        );
        process::exit(1);
    }
    match host_marketing_major() {
        Some(major) if major >= 10 => {}
        Some(major) => {
            eprintln!(
                "vmm-intel requires macOS marketing major >= 10 \
                 (this host reports {major}). Darwin 19 is macOS 10.15."
            );
            process::exit(1);
        }
        None => {
            eprintln!(
                "vmm-intel could not read the macOS version \
                 (kern.osproductversion, kern.osrelease, or sw_vers)."
            );
            process::exit(1);
        }
    }
}

fn host_marketing_major() -> Option<u32> {
    if let Some(version) = sysctl_string("kern.osproductversion")
        && let Some(major) = first_component(&version)
    {
        return Some(major);
    }
    if let Some(release) = sysctl_string("kern.osrelease")
        && let Some(darwin) = first_component(&release)
    {
        return Some(marketing_major_from_darwin(darwin));
    }
    sw_vers_product_version().and_then(|version| first_component(&version))
}

fn marketing_major_from_darwin(darwin_major: u32) -> u32 {
    match darwin_major {
        // Darwin 5 is Mac OS X 10.1; Darwin 19 is macOS 10.15.
        5..=19 => 10,
        // Darwin 20 is macOS 11. Later marketing numbers come from
        // kern.osproductversion when the kernel provides it.
        20.. => darwin_major - 9,
        older => older,
    }
}

fn first_component(version: &str) -> Option<u32> {
    version.split(['.', ' ', '\n']).next()?.parse().ok()
}

fn translated_or_arm_host() -> bool {
    sysctl_int("sysctl.proc_translated").is_some_and(|v| v == 1)
        || sysctl_int("hw.optional.arm64").is_some_and(|v| v == 1)
}

fn sysctl_string(name: &str) -> Option<String> {
    let mut buf = sysctl_raw(name)?;
    if buf.last() == Some(&0) {
        buf.pop();
    }
    String::from_utf8(buf).ok()
}

fn sysctl_int(name: &str) -> Option<u64> {
    let buf = sysctl_raw(name)?;
    match buf.len() {
        4 => Some(u32::from_ne_bytes(buf[..4].try_into().ok()?) as u64),
        8 => Some(u64::from_ne_bytes(buf[..8].try_into().ok()?)),
        _ => None,
    }
}

fn sysctl_raw(name: &str) -> Option<Vec<u8>> {
    let c_name = std::ffi::CString::new(name).ok()?;
    let mut len: usize = 0;
    let rc = unsafe {
        libc::sysctlbyname(
            c_name.as_ptr(),
            std::ptr::null_mut(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len == 0 || len > 4096 {
        return None;
    }
    let mut buf = vec![0u8; len];
    let rc = unsafe {
        libc::sysctlbyname(
            c_name.as_ptr(),
            buf.as_mut_ptr().cast(),
            &mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len > buf.len() {
        return None;
    }
    buf.truncate(len);
    Some(buf)
}

fn sw_vers_product_version() -> Option<String> {
    let out = std::process::Command::new("/usr/bin/sw_vers")
        .arg("-productVersion")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn darwin_19_is_macos_10_15() {
        assert_eq!(marketing_major_from_darwin(19), 10);
        assert_eq!(marketing_major_from_darwin(20), 11);
        assert_eq!(marketing_major_from_darwin(24), 15);
        assert!(marketing_major_from_darwin(19) >= 10);
        assert_eq!(first_component("10.15.7"), Some(10));
        assert_eq!(first_component("11.7.10"), Some(11));
        assert_eq!(first_component("26.0"), Some(26));
        assert_eq!(first_component("19.6.0"), Some(19));
    }
}
