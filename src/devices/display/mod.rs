//! Guest display: simple-framebuffer (FDT) + optional embedded VNC server.

pub mod vnc;
pub mod keysym;

/// Framebuffer GPA (inside default 1 GiB RAM window starting at 0x4000_0000).
pub const FB_GPA: u64 = 0x5000_0000;
/// Bytes reserved in the FDT `reg` property (8 MiB, per gui_architecture.md).
pub const FB_REG_SIZE: u64 = 0x0080_0000;

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
    pub fn pixel_bytes(&self) -> usize {
        self.stride as usize * self.height as usize
    }
}
