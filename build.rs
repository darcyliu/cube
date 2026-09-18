use std::fs;
use std::process::Command;

fn main() {
    println!("cargo:rustc-link-lib=framework=Hypervisor");
    println!("cargo:rerun-if-changed=guest/payload.S");

    // Assemble guest payload using clang targeting aarch64
    let status = Command::new("clang")
        .args([
            "-target",
            "aarch64-linux-gnu",
            "-c",
            "guest/payload.S",
            "-o",
            "guest/payload.o",
        ])
        .status();

    if let Ok(st) = status {
        if st.success() {
            if let Ok(elf_bytes) = fs::read("guest/payload.o") {
                if let Some(text_section) = extract_elf_text_section(&elf_bytes) {
                    let _ = fs::write("guest/payload.bin", text_section);
                }
            }
        }
    }
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
