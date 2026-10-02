//! Linux ARM64 (AArch64) kernel image parser and loader.
//!
//! Implements the standard Linux ARM64 boot protocol as specified in:
//! `Documentation/arch/arm64/booting.rst`
//!
//! The kernel image begins with a 64-byte header:
//!
//! | Offset | Field       | Description                                                 |
//! |--------|-------------|-------------------------------------------------------------|
//! | 0x00   | code0       | Executable code (branch to stext)                           |
//! | 0x04   | code1       | Executable code                                             |
//! | 0x08   | text_offset | Image load offset from 2MB-aligned RAM base (little-endian)  |
//! | 0x10   | image_size  | Effective kernel image size (little-endian)                 |
//! | 0x18   | flags       | Kernel execution flags (bit 0: 0=LE, 1=BE; bit 3: placement)|
//! | 0x20   | res2        | Reserved                                                    |
//! | 0x28   | res3        | Reserved                                                    |
//! | 0x30   | res4        | Reserved                                                    |
//! | 0x38   | magic       | Magic number: 0x644d5241 ("ARM\x64" little-endian)          |

#![allow(dead_code)]

use crate::memory::GuestMemory;
use std::io::Write;
use std::process::{Command, Stdio};

/// ARM64 Linux Kernel Image Magic: "ARM\x64" in little-endian.
pub const LINUX_ARM64_MAGIC: u32 = 0x644d5241;

/// Standard GZIP magic bytes: 0x1f, 0x8b
pub const GZIP_MAGIC: [u8; 2] = [0x1f, 0x8b];

/// Magic for EFI zimg compressed kernel format (used by Alpine Linux / EFI stub): "MZ\0\0zimg"
pub const ZIMG_MAGIC: [u8; 8] = [0x4d, 0x5a, 0x00, 0x00, 0x7a, 0x69, 0x6d, 0x67];

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LinuxArm64Header {
    pub code0: u32,
    pub code1: u32,
    pub text_offset: u64,
    pub image_size: u64,
    pub flags: u64,
    pub res2: u64,
    pub res3: u64,
    pub res4: u64,
    pub magic: u32,
    pub res5: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct LoadedLinuxKernel {
    /// Physical address in guest memory where execution begins
    pub entry_point: u64,
    /// Physical address where the kernel image starts
    pub load_addr: u64,
    /// Total memory footprint occupied by kernel text, data, and BSS
    pub memory_size: u64,
}

impl LinuxArm64Header {
    /// Parses a 64-byte Linux ARM64 image header from raw data.
    pub fn parse(data: &[u8]) -> Result<Self, String> {
        if data.len() < 64 {
            return Err("Data too short for Linux ARM64 image header (< 64 bytes)".into());
        }

        let magic = u32::from_le_bytes(data[0x38..0x3c].try_into().unwrap());
        if magic != LINUX_ARM64_MAGIC {
            return Err(format!(
                "Invalid Linux ARM64 image magic: 0x{:08x} (expected 0x{:08x} 'ARM\\x64')",
                magic, LINUX_ARM64_MAGIC
            ));
        }

        let code0 = u32::from_le_bytes(data[0x00..0x04].try_into().unwrap());
        let code1 = u32::from_le_bytes(data[0x04..0x08].try_into().unwrap());
        let text_offset = u64::from_le_bytes(data[0x08..0x10].try_into().unwrap());
        let image_size = u64::from_le_bytes(data[0x10..0x18].try_into().unwrap());
        let flags = u64::from_le_bytes(data[0x18..0x20].try_into().unwrap());
        let res2 = u64::from_le_bytes(data[0x20..0x28].try_into().unwrap());
        let res3 = u64::from_le_bytes(data[0x28..0x30].try_into().unwrap());
        let res4 = u64::from_le_bytes(data[0x30..0x38].try_into().unwrap());
        let res5 = u32::from_le_bytes(data[0x3c..0x40].try_into().unwrap());

        // Check endianness: bit 0 of flags. 0 = Little Endian, 1 = Big Endian
        if (flags & 1) != 0 {
            return Err("Big-endian Linux ARM64 kernels are not supported on this host".into());
        }

        Ok(Self {
            code0,
            code1,
            text_offset,
            image_size,
            flags,
            res2,
            res3,
            res4,
            magic,
            res5,
        })
    }

    /// Returns whether a byte slice starts with the Linux ARM64 image magic at offset 0x38
    /// or has an EFI zimg compressed header.
    pub fn is_linux_image(data: &[u8]) -> bool {
        if data.len() >= 8 && &data[0..8] == ZIMG_MAGIC {
            return true;
        }
        if data.len() < 64 {
            return false;
        }
        let magic = u32::from_le_bytes(data[0x38..0x3c].try_into().unwrap());
        magic == LINUX_ARM64_MAGIC
    }
}

/// Decompresses gzip-compressed data (such as vmlinuz / Image.gz) using system gzip.
/// Returns the decompressed bytes.
pub fn decompress_gzip(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut child = Command::new("gzip")
        .args(["-dc"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn gzip: {}", e))?;

    let mut stdin = child.stdin.take().ok_or("Failed to open gzip stdin")?;
    let data_vec = data.to_vec();
    let writer_handle = std::thread::spawn(move || {
        let _ = stdin.write_all(&data_vec);
        drop(stdin); // Explicitly close stdin EOF so gzip knows input is finished
    });

    let output = child
        .wait_with_output()
        .map_err(|e| format!("gzip decompression failed: {}", e))?;

    let _ = writer_handle.join();

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("gzip decompress error: {}", stderr));
    }

    Ok(output.stdout)
}

/// Loads a Linux ARM64 kernel image (uncompressed, EFI zimg, or raw gzip)
/// into guest physical memory according to the Linux ARM64 boot specification.
pub fn load_linux_image(
    kernel_data: &[u8],
    guest_mem: &GuestMemory,
    ram_base: u64,
) -> Result<LoadedLinuxKernel, String> {
    let decompressed: Vec<u8>;
    let data = if kernel_data.len() >= 8 && &kernel_data[0..8] == ZIMG_MAGIC {
        println!("[Linux] Detected EFI zimg compressed kernel format (Alpine / EFI stub).");
        let start_offset = u32::from_le_bytes(kernel_data[0x08..0x0c].try_into().unwrap()) as usize;
        let payload_size = u32::from_le_bytes(kernel_data[0x0c..0x10].try_into().unwrap()) as usize;
        if start_offset + payload_size > kernel_data.len() {
            return Err("zimg payload offset/size exceeds file size".into());
        }
        let compressed_payload = &kernel_data[start_offset..start_offset + payload_size];
        decompressed = decompress_gzip(compressed_payload)?;
        println!(
            "[Linux] Decompressed zimg kernel size: {:.2} MB ({} bytes)",
            decompressed.len() as f64 / (1024.0 * 1024.0),
            decompressed.len()
        );
        &decompressed[..]
    } else if kernel_data.len() >= 2 && &kernel_data[0..2] == GZIP_MAGIC {
        println!("[Linux] Detected raw gzip-compressed kernel image. Decompressing...");
        decompressed = decompress_gzip(kernel_data)?;
        println!(
            "[Linux] Decompressed kernel size: {:.2} MB ({} bytes)",
            decompressed.len() as f64 / (1024.0 * 1024.0),
            decompressed.len()
        );
        &decompressed[..]
    } else {
        kernel_data
    };

    let header = LinuxArm64Header::parse(data)?;

    // Modern kernels can specify text_offset = 0 or 0x80000.
    // Placement is relative to a 2MB-aligned base of RAM.
    let load_addr = ram_base + header.text_offset;
    let effective_size = if header.image_size > 0 {
        header.image_size
    } else {
        data.len() as u64
    };

    let page_size_str = match (header.flags >> 1) & 3 {
        1 => "4KB",
        2 => "16KB",
        3 => "64KB",
        _ => "unspecified",
    };

    println!(
        "[Linux] ARM64 Header: text_offset=0x{:x}, image_size=0x{:x} ({:.2} MB), page_size={}",
        header.text_offset,
        effective_size,
        effective_size as f64 / (1024.0 * 1024.0),
        page_size_str
    );

    // Write kernel image data into guest physical memory
    guest_mem.write(load_addr, data)?;

    // Zero out BSS section if effective image_size exceeds the loaded file data
    if effective_size > data.len() as u64 {
        let bss_start = load_addr + data.len() as u64;
        let bss_len = (effective_size - data.len() as u64) as usize;
        let zero_chunk_size = 64 * 1024;
        let zeroes = vec![0u8; zero_chunk_size.min(bss_len)];
        let mut written = 0;
        while written < bss_len {
            let to_write = (bss_len - written).min(zeroes.len());
            guest_mem.write(bss_start + written as u64, &zeroes[..to_write])?;
            written += to_write;
        }
    }

    println!(
        "[Linux] Loaded kernel image: GPA 0x{:08x}..0x{:08x} (entry: 0x{:08x})",
        load_addr,
        load_addr + effective_size,
        load_addr
    );

    Ok(LoadedLinuxKernel {
        entry_point: load_addr,
        load_addr,
        memory_size: effective_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_header(text_offset: u64, image_size: u64, flags: u64, magic: u32) -> [u8; 64] {
        let mut buf = [0u8; 64];
        buf[0x08..0x10].copy_from_slice(&text_offset.to_le_bytes());
        buf[0x10..0x18].copy_from_slice(&image_size.to_le_bytes());
        buf[0x18..0x20].copy_from_slice(&flags.to_le_bytes());
        buf[0x38..0x3c].copy_from_slice(&magic.to_le_bytes());
        buf
    }

    #[test]
    fn test_valid_arm64_header() {
        let buf = create_test_header(0x80000, 0x1800000, 0x0a, LINUX_ARM64_MAGIC);
        let header = LinuxArm64Header::parse(&buf).expect("Should parse valid header");
        assert_eq!(header.text_offset, 0x80000);
        assert_eq!(header.image_size, 0x1800000);
        assert_eq!(header.magic, LINUX_ARM64_MAGIC);
        assert!(LinuxArm64Header::is_linux_image(&buf));
    }

    #[test]
    fn test_invalid_magic() {
        let buf = create_test_header(0x80000, 0x1800000, 0x0a, 0xdeadbeef);
        assert!(LinuxArm64Header::parse(&buf).is_err());
        assert!(!LinuxArm64Header::is_linux_image(&buf));
    }

    #[test]
    fn test_big_endian_rejected() {
        // Bit 0 = 1 (Big Endian)
        let buf = create_test_header(0x80000, 0x1800000, 1, LINUX_ARM64_MAGIC);
        let err = LinuxArm64Header::parse(&buf).expect_err("Should reject big endian");
        assert!(err.contains("Big-endian"));
    }

    #[test]
    fn test_too_small() {
        let buf = [0u8; 32];
        assert!(LinuxArm64Header::parse(&buf).is_err());
        assert!(!LinuxArm64Header::is_linux_image(&buf));
    }
}
