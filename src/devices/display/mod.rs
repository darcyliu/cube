//! Guest display: simple-framebuffer (FDT) + optional embedded VNC server.
//! A virtio-gpu scanout publishes into `FramePublisher`. Frontends keep reading
//! guest memory until the first flush.

pub mod keysym;
pub mod vnc;

use std::sync::Mutex;

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

/// One damaged rectangle in a published scanout, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DamageRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

struct PublishedFrame {
    width: u32,
    height: u32,
    stride: usize,
    pixels: Vec<u8>,
    published: bool,
    damage: Vec<DamageRect>,
    cursor: Option<PublishedCursor>,
}

struct PublishedCursor {
    x: u32,
    y: u32,
    hot_x: u32,
    hot_y: u32,
    width: u32,
    height: u32,
    opaque: bool,
    pixels: Vec<u8>,
}

/// Host-side scanout. VNC and the native window read this when it has been
/// published; otherwise they keep scanning the simple framebuffer.
pub struct FramePublisher {
    inner: Mutex<PublishedFrame>,
}

impl FramePublisher {
    pub fn new(width: u32, height: u32) -> Self {
        let stride = width as usize * 4;
        let pixels = vec![0u8; stride * height as usize];
        Self {
            inner: Mutex::new(PublishedFrame {
                width,
                height,
                stride,
                pixels,
                published: false,
                damage: Vec::new(),
                cursor: None,
            }),
        }
    }

    pub fn geometry(&self) -> (u32, u32) {
        let frame = self.inner.lock().unwrap();
        (frame.width, frame.height)
    }

    /// Copy `src` into the scanout at `rect` and remember the rectangle.
    /// `src_stride` is the byte stride of `src`.
    pub fn blit(&self, rect: DamageRect, src: &[u8], src_stride: usize) {
        if rect.width == 0 || rect.height == 0 {
            return;
        }
        let mut frame = self.inner.lock().unwrap();
        if rect.x >= frame.width || rect.y >= frame.height {
            return;
        }
        let width = rect.width.min(frame.width - rect.x) as usize;
        let height = rect.height.min(frame.height - rect.y) as usize;
        let row_bytes = width * 4;
        if src_stride < row_bytes || height == 0 {
            return;
        }
        let need = src_stride
            .saturating_mul(height - 1)
            .saturating_add(row_bytes);
        if src.len() < need {
            return;
        }
        for row in 0..height {
            let src_off = row * src_stride;
            let dst_off = (rect.y as usize + row) * frame.stride + rect.x as usize * 4;
            frame.pixels[dst_off..dst_off + row_bytes]
                .copy_from_slice(&src[src_off..src_off + row_bytes]);
        }
        frame.published = true;
        let damage = DamageRect {
            x: rect.x,
            y: rect.y,
            width: width as u32,
            height: height as u32,
        };
        if frame.damage.len() >= 64 {
            let full = DamageRect {
                x: 0,
                y: 0,
                width: frame.width,
                height: frame.height,
            };
            frame.damage.clear();
            frame.damage.push(full);
        } else {
            frame.damage.push(damage);
        }
    }

    /// Copy the scanout when a flush has published it.
    pub fn copy_published(&self, dest: &mut [u8]) -> bool {
        let frame = self.inner.lock().unwrap();
        if !frame.published || dest.len() != frame.pixels.len() {
            return false;
        }
        dest.copy_from_slice(&frame.pixels);
        if let Some(cursor) = &frame.cursor {
            composite_cursor(dest, frame.width, frame.height, frame.stride, cursor);
        }
        true
    }

    /// Install a BGRA/BGRX cursor image and mark its covered area damaged.
    #[allow(clippy::too_many_arguments)]
    pub fn set_cursor(
        &self,
        x: u32,
        y: u32,
        hot_x: u32,
        hot_y: u32,
        width: u32,
        height: u32,
        opaque: bool,
        pixels: &[u8],
    ) {
        let Some(bytes) = (width as usize)
            .checked_mul(height as usize)
            .and_then(|count| count.checked_mul(4))
        else {
            return;
        };
        if width == 0 || height == 0 || pixels.len() < bytes {
            return;
        }
        let mut frame = self.inner.lock().unwrap();
        let (frame_width, frame_height) = (frame.width, frame.height);
        if let Some(old) = frame.cursor.take() {
            push_cursor_damage(&mut frame.damage, frame_width, frame_height, &old);
        }
        let cursor = PublishedCursor {
            x,
            y,
            hot_x,
            hot_y,
            width,
            height,
            opaque,
            pixels: pixels[..bytes].to_vec(),
        };
        push_cursor_damage(&mut frame.damage, frame_width, frame_height, &cursor);
        frame.cursor = Some(cursor);
    }

    /// Move the current cursor, damaging both its old and new locations.
    pub fn move_cursor(&self, x: u32, y: u32) {
        let mut frame = self.inner.lock().unwrap();
        let (frame_width, frame_height) = (frame.width, frame.height);
        let Some(mut cursor) = frame.cursor.take() else {
            return;
        };
        push_cursor_damage(&mut frame.damage, frame_width, frame_height, &cursor);
        cursor.x = x;
        cursor.y = y;
        push_cursor_damage(&mut frame.damage, frame_width, frame_height, &cursor);
        frame.cursor = Some(cursor);
    }

    /// Hide the current cursor and damage its previous location.
    pub fn clear_cursor(&self) {
        let mut frame = self.inner.lock().unwrap();
        let (frame_width, frame_height) = (frame.width, frame.height);
        if let Some(cursor) = frame.cursor.take() {
            push_cursor_damage(&mut frame.damage, frame_width, frame_height, &cursor);
        }
    }

    /// Drop the published scanout so frontends fall back to guest memory.
    pub fn clear(&self) {
        let mut frame = self.inner.lock().unwrap();
        frame.pixels.fill(0);
        frame.published = false;
        frame.damage.clear();
        frame.cursor = None;
    }

    /// Damage since the last take. `None` means nothing has been published.
    pub fn take_damage(&self) -> Option<Vec<DamageRect>> {
        let mut frame = self.inner.lock().unwrap();
        if !frame.published {
            return None;
        }
        Some(std::mem::take(&mut frame.damage))
    }
}

fn cursor_rect(
    cursor: &PublishedCursor,
    frame_width: u32,
    frame_height: u32,
) -> Option<DamageRect> {
    let left = i64::from(cursor.x) - i64::from(cursor.hot_x);
    let top = i64::from(cursor.y) - i64::from(cursor.hot_y);
    let right = left + i64::from(cursor.width);
    let bottom = top + i64::from(cursor.height);
    let x0 = left.max(0).min(i64::from(frame_width));
    let y0 = top.max(0).min(i64::from(frame_height));
    let x1 = right.max(0).min(i64::from(frame_width));
    let y1 = bottom.max(0).min(i64::from(frame_height));
    (x1 > x0 && y1 > y0).then_some(DamageRect {
        x: x0 as u32,
        y: y0 as u32,
        width: (x1 - x0) as u32,
        height: (y1 - y0) as u32,
    })
}

fn push_cursor_damage(
    damage: &mut Vec<DamageRect>,
    frame_width: u32,
    frame_height: u32,
    cursor: &PublishedCursor,
) {
    if damage.len() >= 64 {
        damage.clear();
        damage.push(DamageRect {
            x: 0,
            y: 0,
            width: frame_width,
            height: frame_height,
        });
        return;
    }
    if let Some(rect) = cursor_rect(cursor, frame_width, frame_height) {
        damage.push(rect);
    }
}

fn composite_cursor(
    dest: &mut [u8],
    frame_width: u32,
    frame_height: u32,
    stride: usize,
    cursor: &PublishedCursor,
) {
    let Some(rect) = cursor_rect(cursor, frame_width, frame_height) else {
        return;
    };
    let left = i64::from(cursor.x) - i64::from(cursor.hot_x);
    let top = i64::from(cursor.y) - i64::from(cursor.hot_y);
    for y in rect.y..rect.y + rect.height {
        for x in rect.x..rect.x + rect.width {
            let cursor_x = (i64::from(x) - left) as usize;
            let cursor_y = (i64::from(y) - top) as usize;
            let src_at = (cursor_y * cursor.width as usize + cursor_x) * 4;
            let dst_at = y as usize * stride + x as usize * 4;
            let alpha = if cursor.opaque {
                255
            } else {
                cursor.pixels[src_at + 3]
            };
            for channel in 0..3 {
                let source = u32::from(cursor.pixels[src_at + channel]);
                let target = u32::from(dest[dst_at + channel]);
                dest[dst_at + channel] =
                    ((source * u32::from(alpha) + target * u32::from(255 - alpha) + 127) / 255)
                        as u8;
            }
            dest[dst_at + 3] = 255;
        }
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

    #[test]
    fn published_cursor_is_composited_moved_and_clipped() {
        let frames = FramePublisher::new(3, 2);
        frames.blit(
            DamageRect {
                x: 0,
                y: 0,
                width: 3,
                height: 2,
            },
            &[10u8; 24],
            12,
        );
        let cursor = [100, 50, 25, 128, 1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255];
        frames.set_cursor(0, 0, 1, 0, 2, 2, false, &cursor);
        let mut pixels = [0u8; 24];
        assert!(frames.copy_published(&mut pixels));
        assert_eq!(&pixels[0..4], &[1, 2, 3, 255]);
        assert_eq!(&pixels[12..16], &[7, 8, 9, 255]);

        frames.move_cursor(2, 0);
        assert!(frames.copy_published(&mut pixels));
        assert_eq!(&pixels[0..4], &[10, 10, 10, 10]);
        assert_eq!(&pixels[4..7], &[55, 30, 18]);
        frames.clear_cursor();
        assert!(frames.copy_published(&mut pixels));
        assert_eq!(pixels, [10u8; 24]);
    }
}
