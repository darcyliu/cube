//! Guest framebuffer shared by `etc/ramfb` and the RFB server.
//!
//! The window is reserved at the top of guest RAM. `0x50000000` is not a
//! framebuffer GPA on this machine: RAM now starts at 0, so that constant
//! sits in ordinary memory, below the PCI hole at `0xC0000000`.

pub mod keysym;
pub mod vnc;

use std::sync::Mutex;

pub const FB_WIDTH: u32 = 1280;
pub const FB_HEIGHT: u32 = 720;
pub const FB_BPP: u32 = 4;
/// Bound the reservation and any later guest geometry to 256 MiB.
pub const FB_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// DRM_FORMAT_XRGB8888 (`XR24`).
pub const XRGB8888: u32 = 0x3432_5258;
pub const RAMFB_CFG_LEN: usize = 28;

const FB_PAGE: u64 = 4096;
/// Usable RAM below 1 MiB is the real-mode window. The carve stays above it.
const HIGH_RAM: u64 = 0x10_0000;
/// PCI MMIO starts here. The window must end at or before this GPA.
const PCI_HOLE: u64 = 0xC000_0000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FramebufferConfig {
    pub gpa: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub reg_size: u64,
}

impl FramebufferConfig {
    pub fn pixel_bytes(&self) -> usize {
        self.stride as usize * self.height as usize
    }
}

/// Active geometry. The reserved window stays put when the guest moves the scanout.
pub struct Ramfb {
    ram_end: u64,
    reserved_base: u64,
    reserved_len: u64,
    active: Mutex<FramebufferConfig>,
}

impl Ramfb {
    pub fn reserve(ram_size: u64, width: u32, height: u32) -> Result<Self, String> {
        let (stride, bytes) = geometry(width, height)?;
        let reg_size =
            page_align(bytes).ok_or_else(|| "framebuffer reservation overflow".to_string())?;
        if reg_size > ram_size || ram_size > PCI_HOLE {
            return Err("framebuffer reservation does not fit in guest RAM".into());
        }
        let gpa = ram_size - reg_size;
        // Leave a non-empty usable range above 1 MiB so e820 does not collapse.
        if gpa <= HIGH_RAM {
            return Err("framebuffer reservation does not fit in guest RAM".into());
        }
        let cfg = FramebufferConfig {
            gpa,
            width,
            height,
            stride,
            reg_size,
        };
        Ok(Self {
            ram_end: ram_size,
            reserved_base: gpa,
            reserved_len: reg_size,
            active: Mutex::new(cfg),
        })
    }

    pub fn config(&self) -> FramebufferConfig {
        match self.active.lock() {
            Ok(cfg) => *cfg,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }

    pub fn reserved_base(&self) -> u64 {
        self.reserved_base
    }

    pub fn reserved_len(&self) -> u64 {
        self.reserved_len
    }

    /// Guest replaced the 28-byte `etc/ramfb` blob. Out-of-range values are ignored.
    pub fn guest_wrote(&self, bytes: &[u8]) {
        let Some(cfg) = parse_guest(bytes, self.ram_end) else {
            return;
        };
        // A poisoned lock means a previous update panicked. Skip this one.
        let Ok(mut active) = self.active.lock() else {
            return;
        };
        *active = cfg;
    }
}

pub fn parse_size(value: &str) -> Result<(u32, u32), String> {
    let (width, height) = value
        .split_once(['x', 'X'])
        .ok_or_else(|| "expected WIDTHxHEIGHT".to_string())?;
    if width.is_empty() || height.is_empty() || height.contains(['x', 'X']) {
        return Err("expected WIDTHxHEIGHT".into());
    }
    let width = width
        .parse::<u32>()
        .map_err(|_| "width must be an integer".to_string())?;
    let height = height
        .parse::<u32>()
        .map_err(|_| "height must be an integer".to_string())?;
    geometry(width, height)?;
    Ok((width, height))
}

/// Little-endian `RAMFBCfg`. OVMF writes the same fields big-endian; [`parse_guest`]
/// accepts either, chosen by which endianness makes the fourcc `XRGB8888`.
pub fn descriptor_bytes(cfg: &FramebufferConfig) -> [u8; RAMFB_CFG_LEN] {
    let mut out = [0u8; RAMFB_CFG_LEN];
    out[0..8].copy_from_slice(&cfg.gpa.to_le_bytes());
    out[8..12].copy_from_slice(&XRGB8888.to_le_bytes());
    out[16..20].copy_from_slice(&cfg.width.to_le_bytes());
    out[20..24].copy_from_slice(&cfg.height.to_le_bytes());
    out[24..28].copy_from_slice(&cfg.stride.to_le_bytes());
    out
}

fn geometry(width: u32, height: u32) -> Result<(u32, u64), String> {
    if width == 0 || height == 0 {
        return Err("width and height must be nonzero".into());
    }
    if width > u16::MAX as u32 || height > u16::MAX as u32 {
        return Err(format!(
            "display dimensions must fit the VNC protocol limit of {}x{}",
            u16::MAX,
            u16::MAX
        ));
    }
    let stride = width
        .checked_mul(FB_BPP)
        .ok_or_else(|| "framebuffer stride overflow".to_string())?;
    let bytes = u64::from(stride)
        .checked_mul(u64::from(height))
        .ok_or_else(|| "framebuffer size overflow".to_string())?;
    if bytes > FB_MAX_BYTES {
        return Err(format!(
            "framebuffer requires {} MiB; maximum is {} MiB",
            bytes.div_ceil(1024 * 1024),
            FB_MAX_BYTES / (1024 * 1024)
        ));
    }
    Ok((stride, bytes))
}

fn page_align(bytes: u64) -> Option<u64> {
    bytes
        .checked_add(FB_PAGE - 1)
        .map(|value| value & !(FB_PAGE - 1))
}

fn parse_guest(bytes: &[u8], ram_end: u64) -> Option<FramebufferConfig> {
    if bytes.len() != RAMFB_CFG_LEN || ram_end > PCI_HOLE {
        return None;
    }
    let fourcc_le = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let fourcc_be = u32::from_be_bytes(bytes[8..12].try_into().ok()?);
    let big_endian = match (fourcc_le == XRGB8888, fourcc_be == XRGB8888) {
        (true, false) => false,
        (false, true) => true,
        _ => return None,
    };
    let read_u32 = |off: usize| -> Option<u32> {
        let raw = bytes.get(off..off + 4)?.try_into().ok()?;
        Some(if big_endian {
            u32::from_be_bytes(raw)
        } else {
            u32::from_le_bytes(raw)
        })
    };
    let read_u64 = |off: usize| -> Option<u64> {
        let raw = bytes.get(off..off + 8)?.try_into().ok()?;
        Some(if big_endian {
            u64::from_be_bytes(raw)
        } else {
            u64::from_le_bytes(raw)
        })
    };
    let addr = read_u64(0)?;
    let width = read_u32(16)?;
    let height = read_u32(20)?;
    let stride = read_u32(24)?;
    let (min_stride, _) = geometry(width, height).ok()?;
    if stride < min_stride {
        return None;
    }
    let bytes_len = u64::from(stride).checked_mul(u64::from(height))?;
    if bytes_len == 0 || bytes_len > FB_MAX_BYTES {
        return None;
    }
    if addr.checked_add(bytes_len)? > ram_end {
        return None;
    }
    let reg_size = page_align(bytes_len)?;
    Some(FramebufferConfig {
        gpa: addr,
        width,
        height,
        stride,
        reg_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserve_sits_at_the_top_of_ram_not_the_arm_gpa() {
        let ram = 64 * 1024 * 1024;
        let fb = Ramfb::reserve(ram, FB_WIDTH, FB_HEIGHT).unwrap();
        let cfg = fb.config();
        assert_eq!(cfg.width, 1280);
        assert_eq!(cfg.height, 720);
        assert_eq!(cfg.stride, 1280 * 4);
        assert_eq!(cfg.gpa + cfg.reg_size, ram);
        assert_ne!(cfg.gpa, 0x5000_0000);
        assert!(cfg.gpa > HIGH_RAM);
        assert_eq!(fb.reserved_base(), cfg.gpa);
        assert_eq!(fb.reserved_len(), cfg.reg_size);
        let raw = descriptor_bytes(&cfg);
        assert_eq!(u64::from_le_bytes(raw[0..8].try_into().unwrap()), cfg.gpa);
        assert_eq!(u32::from_le_bytes(raw[8..12].try_into().unwrap()), XRGB8888);
    }

    #[test]
    fn geometry_rejects_invalid_or_excessive_sizes() {
        assert!(parse_size("0x720").unwrap_err().contains("nonzero"));
        assert!(parse_size("65536x1").unwrap_err().contains("VNC"));
        assert!(
            parse_size("16384x16384")
                .unwrap_err()
                .contains("maximum is 256 MiB")
        );
        assert!(parse_size("1280").unwrap_err().contains("WIDTHxHEIGHT"));
        assert!(Ramfb::reserve(128 * 1024 * 1024, 8000, 8000).is_err());
        assert_eq!(parse_size("1920x1080").unwrap(), (1920, 1080));
    }

    #[test]
    fn guest_write_switches_endianness_only_when_the_fourcc_matches() {
        let ram = 8 * 1024 * 1024;
        let fb = Ramfb::reserve(ram, 64, 64).unwrap();
        let initial = fb.config();
        let mut le = descriptor_bytes(&initial);
        le[16..20].copy_from_slice(&32u32.to_le_bytes());
        le[20..24].copy_from_slice(&16u32.to_le_bytes());
        le[24..28].copy_from_slice(&128u32.to_le_bytes());
        le[0..8].copy_from_slice(&0x20_0000u64.to_le_bytes());
        fb.guest_wrote(&le);
        assert_eq!(fb.config().gpa, 0x20_0000);
        assert_eq!(fb.config().width, 32);
        assert_eq!(fb.config().stride, 128);

        let mut be = [0u8; 28];
        be[0..8].copy_from_slice(&0x30_0000u64.to_be_bytes());
        be[8..12].copy_from_slice(&XRGB8888.to_be_bytes());
        be[16..20].copy_from_slice(&8u32.to_be_bytes());
        be[20..24].copy_from_slice(&8u32.to_be_bytes());
        be[24..28].copy_from_slice(&32u32.to_be_bytes());
        fb.guest_wrote(&be);
        assert_eq!(fb.config().gpa, 0x30_0000);
        assert_eq!(fb.config().width, 8);

        be[8..12].copy_from_slice(&0u32.to_be_bytes());
        fb.guest_wrote(&be);
        assert_eq!(fb.config().gpa, 0x30_0000);
        be[0..8].copy_from_slice(&ram.to_be_bytes());
        be[8..12].copy_from_slice(&XRGB8888.to_be_bytes());
        fb.guest_wrote(&be);
        assert_eq!(fb.config().gpa, 0x30_0000);
        assert_eq!(fb.reserved_base(), initial.gpa);
    }
}
