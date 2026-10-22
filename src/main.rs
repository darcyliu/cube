//! Intel-only x86_64 VMM. The library has no `compile_error!` so
//! `cargo test --lib` still builds on other hosts.

#[cfg(not(all(target_arch = "x86_64", target_os = "macos")))]
compile_error!("vmm-intel builds only for x86_64-apple-darwin");

mod hypervisor;
mod vcpu;
mod vm;

use std::path::PathBuf;
use std::process;

fn main() {
    enforce_host_gate();
    match parse_args() {
        Cli::Help => {
            print_help();
            process::exit(1);
        }
        Cli::Error(message) => {
            eprintln!("{message}");
            process::exit(1);
        }
        Cli::Payload { path, serial_log } => {
            let done = vm::run_payload(path, serial_log);
            if let Some(ns) = done.exec_ns {
                eprintln!("[VMM] vCPU exec time: {ns} ns");
            }
            if let Some(error) = done.error {
                eprintln!("{error}");
                process::exit(1);
            }
        }
    }
}

enum Cli {
    Help,
    Error(String),
    Payload {
        path: Option<PathBuf>,
        serial_log: Option<PathBuf>,
    },
}

fn parse_args() -> Cli {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        return Cli::Help;
    }
    let mut payload: Option<Option<PathBuf>> = None;
    let mut serial_log = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--help" | "-h" => return Cli::Help,
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
            "--serial-log" | "--console-log" => {
                let Some(path) = args.get(i + 1) else {
                    return Cli::Error("--serial-log needs a path".into());
                };
                if path.starts_with('-') {
                    return Cli::Error("--serial-log needs a path".into());
                }
                serial_log = Some(PathBuf::from(path));
                i += 1;
            }
            other => return Cli::Error(format!("unknown argument {other}")),
        }
        i += 1;
    }
    match payload {
        Some(path) => Cli::Payload { path, serial_log },
        None => Cli::Error("no boot path; pass --payload".into()),
    }
}

fn print_help() {
    println!(
        "\
vmm-intel: Intel-only x86_64 guest on macOS 10.15+ (Hypervisor.framework VMX).

This fork does not run on Apple Silicon and must not be merged back into the
AArch64 VMM.

Usage:
  vmm --payload [PATH]   Long-mode guest. PATH defaults to the built-in COM1 banner.

With no arguments, vmm prints this help and exits 1."
    );
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
