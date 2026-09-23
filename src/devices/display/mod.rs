//! Guest display: simple-framebuffer (FDT) + optional embedded VNC server.

pub mod keysym;
pub mod vnc;

/// Framebuffer GPA (inside default 1 GiB RAM window starting at 0x4000_0000).
pub const FB_GPA: u64 = 0x5000_0000;
/// Minimum bytes reserved in the FDT `reg` property.
pub const FB_REG_SIZE: u64 = 0x0080_0000;
const FB_PAGE_SIZE: u64 = 4096;
/// Bound framebuffer allocation and guest-RAM reservation to 256 MiB.
pub const FB_MAX_BYTES: u64 = 256 * 1024 * 1024;

pub const FB_WIDTH: u32 = 1280;
pub const FB_HEIGHT: u32 = 720;
pub const FB_BPP: u32 = 4;
pub const FB_STRIDE: u32 = FB_WIDTH * FB_BPP;
#[allow(dead_code)]
pub const FB_BYTES: usize = (FB_STRIDE * FB_HEIGHT) as usize;

#[derive(Clone, Copy, Debug)]
pub struct FramebufferConfig {
    pub gpa: u64,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub reg_size: u64,
}

impl Default for FramebufferConfig {
    fn default() -> Self {
        Self {
            gpa: FB_GPA,
            width: FB_WIDTH,
            height: FB_HEIGHT,
            stride: FB_STRIDE,
            reg_size: FB_REG_SIZE,
        }
    }
}

impl FramebufferConfig {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err("display width and height must be nonzero".into());
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
        let reg_size = bytes
            .checked_add(FB_PAGE_SIZE - 1)
            .map(|value| value & !(FB_PAGE_SIZE - 1))
            .ok_or_else(|| "framebuffer reservation overflow".to_string())?
            .max(FB_REG_SIZE);
        Ok(Self {
            gpa: FB_GPA,
            width,
            height,
            stride,
            reg_size,
        })
    }

    pub fn pixel_bytes(&self) -> usize {
        self.stride as usize * self.height as usize
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_geometry_computes_stride_and_reservation() {
        let hd = FramebufferConfig::new(1920, 1080).unwrap();
        assert_eq!(hd.stride, 7680);
        assert_eq!(hd.pixel_bytes(), 8_294_400);
        assert_eq!(hd.reg_size, FB_REG_SIZE);

        let uhd = FramebufferConfig::new(3840, 2160).unwrap();
        assert_eq!(uhd.pixel_bytes(), 33_177_600);
        assert_eq!(uhd.reg_size, 33_177_600);
        assert_eq!(uhd.reg_size % FB_PAGE_SIZE, 0);
    }

    #[test]
    fn display_geometry_rejects_invalid_or_excessive_sizes() {
        assert!(
            FramebufferConfig::new(0, 720)
                .unwrap_err()
                .contains("nonzero")
        );
        assert!(
            FramebufferConfig::new(65_536, 1)
                .unwrap_err()
                .contains("VNC")
        );
        assert!(
            FramebufferConfig::new(16_384, 16_384)
                .unwrap_err()
                .contains("maximum")
        );
    }
}
