//! Guest physical memory management aligned to Apple Silicon page boundaries.

#![allow(dead_code)]

use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::ptr::NonNull;

use crate::hypervisor::{
    check_hv, hv_vm_map, hv_vm_unmap, HvResult, HV_MEMORY_EXEC, HV_MEMORY_READ, HV_MEMORY_WRITE,
};

/// Host page size on Apple Silicon is 16KB (0x4000).
pub const GUEST_PAGE_SIZE: usize = 0x4000;

pub struct GuestMemory {
    ptr: NonNull<u8>,
    layout: Layout,
    guest_base: u64,
    size: usize,
    mapped: bool,
}

unsafe impl Send for GuestMemory {}
unsafe impl Sync for GuestMemory {}

impl GuestMemory {
    /// Allocates and maps a block of guest physical memory at `guest_base`.
    /// `size` will be rounded up to the nearest 16KB page multiple.
    pub fn new(guest_base: u64, requested_size: usize) -> HvResult<Self> {
        let size = requested_size
            .checked_add(GUEST_PAGE_SIZE - 1)
            .map(|n| n & !(GUEST_PAGE_SIZE - 1))
            .filter(|&n| n > 0 && guest_base.checked_add(n as u64).is_some())
            .ok_or(crate::hypervisor::HvError(
                crate::hypervisor::HV_BAD_ARGUMENT,
            ))?;
        let layout = Layout::from_size_align(size, GUEST_PAGE_SIZE)
            .map_err(|_| crate::hypervisor::HvError(crate::hypervisor::HV_BAD_ARGUMENT))?;

        // Allocate zeroed memory
        let raw_ptr = unsafe { alloc_zeroed(layout) };
        let ptr = NonNull::new(raw_ptr).ok_or(crate::hypervisor::HvError(
            crate::hypervisor::HV_NO_RESOURCES,
        ))?;

        // Map into VM address space with Read, Write, and Execute permissions
        let flags = HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC;
        unsafe {
            if let Err(e) = check_hv(hv_vm_map(ptr.as_ptr() as *mut _, guest_base, size, flags)) {
                dealloc(ptr.as_ptr(), layout);
                return Err(e);
            }
        }

        Ok(Self {
            ptr,
            layout,
            guest_base,
            size,
            mapped: true,
        })
    }

    #[cfg(test)]
    pub fn new_test(guest_base: u64, size: usize) -> Self {
        let layout = Layout::from_size_align(size, GUEST_PAGE_SIZE).unwrap();
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) }).unwrap();
        Self {
            ptr,
            layout,
            guest_base,
            size,
            mapped: false,
        }
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
            if self.mapped {
                let _ = hv_vm_unmap(self.guest_base, self.size);
            }
            dealloc(self.ptr.as_ptr(), self.layout);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_overflow_and_cross_boundary_accesses() {
        let m = GuestMemory::new_test(0x4000, 0x4000);
        for addr in [0, 0x3fff, 0x7fff, u64::MAX] {
            assert!(m.read_u16(addr).is_err());
            assert!(m.write_u16(addr, 1).is_err());
            assert!(m.write_u32(addr, 1).is_err());
            assert!(m.write_u64(addr, 1).is_err());
        }
        m.write_u32(0x7ffc, 0x1234).unwrap();
        assert_eq!(m.read_u32(0x7ffc).unwrap(), 0x1234);
        assert!(m.checked_offset(0x4000, usize::MAX).is_err());
    }
}
