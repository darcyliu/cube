//! Host buffer of guest RAM. `hv_vm_map` stays in the binary.

#![allow(dead_code)]

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::ptr::NonNull;

/// Intel Mac hosts use 4 KiB pages. `hv_vm_map` requires that alignment.
pub const GUEST_PAGE_SIZE: usize = 0x1000;

pub struct GuestMemory {
    ptr: NonNull<u8>,
    layout: Layout,
    guest_base: u64,
    size: usize,
}

unsafe impl Send for GuestMemory {}
unsafe impl Sync for GuestMemory {}

impl GuestMemory {
    /// Copy `bytes` into a page-aligned host buffer. The length rounds up to
    /// [`GUEST_PAGE_SIZE`]. This does not map the buffer into a VM.
    pub fn from_buffer(guest_base: u64, bytes: &[u8]) -> Result<Self, String> {
        let size = bytes
            .len()
            .checked_add(GUEST_PAGE_SIZE - 1)
            .map(|n| n & !(GUEST_PAGE_SIZE - 1))
            .filter(|&n| n > 0 && guest_base.checked_add(n as u64).is_some())
            .ok_or_else(|| "guest memory size is zero or overflows".to_string())?;
        let layout = Layout::from_size_align(size, GUEST_PAGE_SIZE)
            .map_err(|_| "guest memory alignment failed".to_string())?;
        let raw_ptr = unsafe { alloc_zeroed(layout) };
        let ptr =
            NonNull::new(raw_ptr).ok_or_else(|| "guest memory allocation failed".to_string())?;
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.as_ptr(), bytes.len());
        }
        Ok(Self {
            ptr,
            layout,
            guest_base,
            size,
        })
    }

    /// Page-aligned zeroed buffer. Does not call `hv_vm_map`.
    pub fn allocate(guest_base: u64, size: usize) -> Result<Self, String> {
        if size == 0 || size % GUEST_PAGE_SIZE != 0 {
            return Err("guest memory size must be a non-zero multiple of 4 KiB".into());
        }
        if guest_base % GUEST_PAGE_SIZE as u64 != 0 || guest_base.checked_add(size as u64).is_none()
        {
            return Err("guest memory base is misaligned or overflows".into());
        }
        let layout = Layout::from_size_align(size, GUEST_PAGE_SIZE)
            .map_err(|_| "guest memory alignment failed".to_string())?;
        let raw_ptr = unsafe { alloc_zeroed(layout) };
        let ptr =
            NonNull::new(raw_ptr).ok_or_else(|| "guest memory allocation failed".to_string())?;
        Ok(Self {
            ptr,
            layout,
            guest_base,
            size,
        })
    }

    /// Host pointer passed to `hv_vm_map` by the binary.
    pub fn host_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }

    pub fn guest_base(&self) -> u64 {
        self.guest_base
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// Validate by subtraction, so even a guest-supplied u64::MAX cannot wrap.
    pub fn checked_offset(&self, gpa: u64, len: usize) -> Result<usize, String> {
        let offset = gpa
            .checked_sub(self.guest_base)
            .and_then(|n| usize::try_from(n).ok())
            .filter(|&n| n <= self.size && len <= self.size - n)
            .ok_or_else(|| {
                format!("Guest memory range out of bounds: GPA {gpa:#x}, length {len}")
            })?;
        Ok(offset)
    }

    pub fn write(&self, gpa: u64, data: &[u8]) -> Result<(), String> {
        let offset = self.checked_offset(gpa, data.len())?;
        unsafe {
            std::ptr::copy_nonoverlapping(data.as_ptr(), self.ptr.as_ptr().add(offset), data.len());
        }
        Ok(())
    }

    pub fn read(&self, gpa: u64, buf: &mut [u8]) -> Result<(), String> {
        let offset = self.checked_offset(gpa, buf.len())?;
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.ptr.as_ptr().add(offset),
                buf.as_mut_ptr(),
                buf.len(),
            );
        }
        Ok(())
    }

    pub fn read_u16(&self, gpa: u64) -> Result<u16, String> {
        let mut bytes = [0u8; 2];
        self.read(gpa, &mut bytes)?;
        Ok(u16::from_le_bytes(bytes))
    }

    pub fn read_u32(&self, gpa: u64) -> Result<u32, String> {
        let mut bytes = [0u8; 4];
        self.read(gpa, &mut bytes)?;
        Ok(u32::from_le_bytes(bytes))
    }

    pub fn read_u64(&self, gpa: u64) -> Result<u64, String> {
        let mut bytes = [0u8; 8];
        self.read(gpa, &mut bytes)?;
        Ok(u64::from_le_bytes(bytes))
    }

    pub fn write_u16(&self, gpa: u64, val: u16) -> Result<(), String> {
        let offset = self.checked_offset(gpa, 2)?;
        let bytes = val.to_le_bytes();
        unsafe {
            self.ptr.as_ptr().add(offset).write_volatile(bytes[0]);
            self.ptr.as_ptr().add(offset + 1).write_volatile(bytes[1]);
        }
        Ok(())
    }

    pub fn write_u32(&self, gpa: u64, val: u32) -> Result<(), String> {
        let offset = self.checked_offset(gpa, 4)?;
        let bytes = val.to_le_bytes();
        unsafe {
            for (i, b) in bytes.iter().enumerate() {
                self.ptr.as_ptr().add(offset + i).write_volatile(*b);
            }
        }
        Ok(())
    }

    pub fn write_u64(&self, gpa: u64, val: u64) -> Result<(), String> {
        self.write(gpa, &val.to_le_bytes())
    }
}

impl Drop for GuestMemory {
    fn drop(&mut self) {
        unsafe {
            dealloc(self.ptr.as_ptr(), self.layout);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounds_host_buffer_up_to_4k() {
        let m = GuestMemory::from_buffer(0, &[1, 2, 3]).unwrap();
        assert_eq!(m.size(), GUEST_PAGE_SIZE);
        let mut buf = [0u8; 3];
        m.read(0, &mut buf).unwrap();
        assert_eq!(buf, [1, 2, 3]);
    }

    #[test]
    fn rejects_overflow_and_cross_boundary_accesses() {
        let m = GuestMemory::from_buffer(0x1000, &vec![0; 0x1000]).unwrap();
        for addr in [0, 0xfff, 0x1fff, u64::MAX] {
            assert!(m.read_u16(addr).is_err());
            assert!(m.write_u16(addr, 1).is_err());
            assert!(m.write_u32(addr, 1).is_err());
            assert!(m.write_u64(addr, 1).is_err());
        }
        m.write_u32(0x1ffc, 0x1234).unwrap();
        assert_eq!(m.read_u32(0x1ffc).unwrap(), 0x1234);
        assert!(m.checked_offset(0x1000, usize::MAX).is_err());
    }
}
