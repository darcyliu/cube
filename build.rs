use std::fs;
use std::process::Command;

fn main() {
    println!("cargo:rustc-link-lib=framework=Hypervisor");
    println!("cargo:rustc-link-lib=framework=AppKit");
    println!("cargo:rustc-link-lib=framework=Metal");
    println!("cargo:rustc-link-lib=framework=MetalKit");
    println!("cargo:rerun-if-changed=src/native.m");
    println!("cargo:rerun-if-changed=guest/payload.S");

    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let out_bin = std::path::Path::new(&out_dir).join("payload.bin");
    let out_obj = std::path::Path::new(&out_dir).join("payload.o");
    let cocoa_obj = std::path::Path::new(&out_dir).join("native.o");
    let cocoa_lib = std::path::Path::new(&out_dir).join("libvmm_native.a");
    let status = Command::new("clang")
        .args(["-fobjc-arc", "-c", "src/native.m", "-o"])
        .arg(&cocoa_obj)
        .status()
        .expect("build.rs: clang is required for the native Cocoa frontend");
    assert!(status.success(), "build.rs: failed to compile native Cocoa frontend");
    let status = Command::new("ar")
        .arg("rcs")
        .arg(&cocoa_lib)
        .arg(&cocoa_obj)
        .status()
        .expect("build.rs: ar is required for the native Cocoa frontend");
    assert!(status.success(), "build.rs: failed to archive native Cocoa frontend");
    println!("cargo:rustc-link-search=native={out_dir}");
    println!("cargo:rustc-link-lib=static=vmm_native");

    // Assemble the tiny bare-metal test payload (include_bytes! in main.rs).
    // guest/*.bin is gitignored — this step must succeed on every clean build.
    let status = Command::new("clang")
        .args([
            "-target",
            "aarch64-unknown-none",
            "-c",
            "guest/payload.S",
            "-o",
            out_obj.to_str().unwrap(),
        ])
        .status()
        .unwrap_or_else(|e| {
            panic!(
                "build.rs: failed to run clang to assemble guest/payload.S: {e}
                 Install Xcode CLT / clang, or create guest/payload.bin another way."
            )
        });
    if !status.success() {
        panic!(
            "build.rs: clang failed assembling guest/payload.S (status {status}).
             Need an aarch64-capable clang (Xcode CLT)."
        );
    }

    let elf_bytes = fs::read(out_obj.to_str().unwrap()).unwrap_or_else(|e| {
        panic!("build.rs: could not read guest/payload.o after clang: {e}")
    });
    let text_section = extract_elf_text_section(&elf_bytes).unwrap_or_else(|| {
        panic!("build.rs: no .text section in guest/payload.o (expected 64-bit ELF)")
    });
    fs::write(&out_bin, text_section).unwrap_or_else(|e| {
        panic!("build.rs: could not write guest/payload.bin: {e}")
    });
}

fn extract_elf_text_section(data: &[u8]) -> Option<&[u8]> {
    if data.len() < 64 || &data[0..4] != b"\x7fELF" || data[4] != 2 {
        // Not a 64-bit ELF
        return None;
    }

    let e_shoff = u64::from_le_bytes(data[40..48].try_into().ok()?) as usize;
    let e_shentsize = u16::from_le_bytes(data[58..60].try_into().ok()?) as usize;
    let e_shnum = u16::from_le_bytes(data[60..62].try_into().ok()?) as usize;
    let e_shstrndx = u16::from_le_bytes(data[62..64].try_into().ok()?) as usize;

    if e_shstrndx >= e_shnum {
        return None;
    }

    let shstr_hdr = e_shoff + e_shstrndx * e_shentsize;
    if shstr_hdr + 40 > data.len() {
        return None;
    }
    let shstr_offset = u64::from_le_bytes(data[shstr_hdr + 24..shstr_hdr + 32].try_into().ok()?) as usize;
    let shstr_size = u64::from_le_bytes(data[shstr_hdr + 32..shstr_hdr + 40].try_into().ok()?) as usize;
    if shstr_offset + shstr_size > data.len() {
        return None;
    }
    let shstrtab = &data[shstr_offset..shstr_offset + shstr_size];

    for i in 0..e_shnum {
        let hdr = e_shoff + i * e_shentsize;
        if hdr + 40 > data.len() {
            break;
        }
        let name_idx = u32::from_le_bytes(data[hdr..hdr + 4].try_into().ok()?) as usize;
        if name_idx < shstrtab.len() {
            let name_bytes = &shstrtab[name_idx..];
            let name_end = name_bytes.iter().position(|&b| b == 0).unwrap_or(name_bytes.len());
            if &name_bytes[..name_end] == b".text" {
                let sh_offset = u64::from_le_bytes(data[hdr + 24..hdr + 32].try_into().ok()?) as usize;
                let sh_size = u64::from_le_bytes(data[hdr + 32..hdr + 40].try_into().ok()?) as usize;
                if sh_offset + sh_size <= data.len() {
                    return Some(&data[sh_offset..sh_offset + sh_size]);
                }
            }
        }
    }

    None
}
