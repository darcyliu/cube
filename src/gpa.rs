//! Host view of guest physical memory. RAM and the code flash are separate
//! buffers. The binary maps the code flash read/execute.

use std::sync::Arc;

use crate::memory::GuestMemory;

/// OVMF code window. The varstore below this address is not part of it.
pub const CODE_FLASH_BASE: u64 = 0xFFC8_4000;
pub const CODE_FLASH_SIZE: u64 = 3_653_632;
pub const VARS_FLASH_BASE: u64 = 0xFFC0_0000;
pub const VARS_FLASH_SIZE: u64 = 540_672;
pub const RESET_VECTOR_GPA: u64 = 0xFFFF_FFF0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GpaRegion {
    Ram,
    CodeFlash,
    Unmapped,
}

pub struct GuestSpace {
    ram: Arc<GuestMemory>,
    code: Option<Arc<GuestMemory>>,
}

impl GuestSpace {
    pub fn new(ram: Arc<GuestMemory>, code: Option<Arc<GuestMemory>>) -> Self {
        Self { ram, code }
    }

    pub fn ram(&self) -> &GuestMemory {
        &self.ram
    }

    pub fn region(&self, gpa: u64) -> GpaRegion {
        if self.ram_contains(gpa, 1) {
            GpaRegion::Ram
        } else if self.code_contains(gpa, 1) {
            GpaRegion::CodeFlash
        } else {
            GpaRegion::Unmapped
        }
    }

    /// Architectural code window, whether or not a host buffer was registered.
    pub fn code_window_contains(&self, gpa: u64, len: usize) -> bool {
        let end = gpa.saturating_add(len as u64);
        gpa >= CODE_FLASH_BASE && end <= CODE_FLASH_BASE.saturating_add(CODE_FLASH_SIZE)
    }

    pub fn read_gpa(&self, gpa: u64, buf: &mut [u8]) -> Result<(), String> {
        if buf.is_empty() {
            return Ok(());
        }
        if self.ram_contains(gpa, buf.len()) {
            return self.ram.read(gpa, buf);
        }
        if self.code_contains(gpa, buf.len()) {
            return self
                .code
                .as_ref()
                .expect("code_contains implies a buffer")
                .read(gpa, buf);
        }
        Err(format!(
            "unmapped GPA {gpa:#x} len {} ({:?})",
            buf.len(),
            self.region(gpa)
        ))
    }

    pub fn write_gpa(&self, gpa: u64, buf: &[u8]) -> Result<(), String> {
        if buf.is_empty() || self.code_window_contains(gpa, buf.len()) {
            return Ok(());
        }
        if self.ram_contains(gpa, buf.len()) {
            return self.ram.write(gpa, buf);
        }
        Err(format!("unmapped GPA write {gpa:#x} len {}", buf.len()))
    }

    fn ram_contains(&self, gpa: u64, len: usize) -> bool {
        self.ram.checked_offset(gpa, len).is_ok()
    }

    fn code_contains(&self, gpa: u64, len: usize) -> bool {
        self.code.as_ref().is_some_and(|code| {
            code.guest_base() == CODE_FLASH_BASE && code.checked_offset(gpa, len).is_ok()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_gpa_splits_ram_code_flash_and_unmapped() {
        let ram = Arc::new(GuestMemory::allocate(0, 0x2000).unwrap());
        ram.write(0x100, &[0xAB]).unwrap();
        let code = Arc::new(GuestMemory::from_buffer(CODE_FLASH_BASE, &[0xCD, 0xEF]).unwrap());
        let space = GuestSpace::new(ram, Some(code));

        let mut byte = [0u8; 1];
        space.read_gpa(0x100, &mut byte).unwrap();
        assert_eq!(byte, [0xAB]);
        assert_eq!(space.region(0x100), GpaRegion::Ram);

        space.read_gpa(CODE_FLASH_BASE, &mut byte).unwrap();
        assert_eq!(byte, [0xCD]);
        assert_eq!(space.region(CODE_FLASH_BASE + 1), GpaRegion::CodeFlash);
        assert_ne!(space.region(CODE_FLASH_BASE), GpaRegion::Ram);

        // Varstore sits below the code window and is not RAM.
        let unmapped = 0xFFC0_0000u64;
        assert_eq!(space.region(unmapped), GpaRegion::Unmapped);
        assert_ne!(space.region(unmapped), GpaRegion::Ram);
        assert!(space.read_gpa(unmapped, &mut byte).is_err());
        assert!(space.code_window_contains(CODE_FLASH_BASE, 1));
        assert!(!space.code_window_contains(unmapped, 1));
    }

    #[test]
    fn reset_vector_sits_in_the_code_window() {
        assert_eq!(CODE_FLASH_BASE + CODE_FLASH_SIZE, 0x1_0000_0000);
        assert_eq!(VARS_FLASH_BASE + VARS_FLASH_SIZE, CODE_FLASH_BASE);
        let space = GuestSpace::new(Arc::new(GuestMemory::allocate(0, 0x1000).unwrap()), None);
        assert!(space.code_window_contains(RESET_VECTOR_GPA, 1));
        let file_off = RESET_VECTOR_GPA - CODE_FLASH_BASE;
        assert!(file_off + 16 <= CODE_FLASH_SIZE);
    }
}
