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
}

impl GuestMemory {
    /// Allocates and maps a block of guest physical memory at `guest_base`.
    /// `size` will be rounded up to the nearest 16KB page multiple.
    pub fn new(guest_base: u64, requested_size: usize) -> HvResult<Self> {
        let size = (requested_size + GUEST_PAGE_SIZE - 1) & !(GUEST_PAGE_SIZE - 1);
        let layout = Layout::from_size_align(size, GUEST_PAGE_SIZE)
            .map_err(|_| crate::hypervisor::HvError(crate::hypervisor::HV_BAD_ARGUMENT))?;

        // Allocate zeroed memory
        let raw_ptr = unsafe { alloc_zeroed(layout) };
        let ptr = NonNull::new(raw_ptr)
            .ok_or(crate::hypervisor::HvError(crate::hypervisor::HV_NO_RESOURCES))?;

        // Map into VM address space with Read, Write, and Execute permissions
        let flags = HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC;
        unsafe {
            check_hv(hv_vm_map(
                ptr.as_ptr() as *mut _,
                guest_base,
                size,
                flags,
            ))?;
        }

        Ok(Self {
            ptr,
            layout,
            guest_base,
            size,
        })
    }

    pub fn guest_base(&self) -> u64 {
        self.guest_base
    }

    pub fn size(&self) -> usize {
        self.size
    }

    /// Writes raw bytes into guest memory at the specified guest physical address.
    pub fn write(&mut self, gpa: u64, data: &[u8]) -> Result<(), String> {
        if gpa < self.guest_base || gpa + (data.len() as u64) > self.guest_base + (self.size as u64)
        {
            return Err(format!(
                "Address out of bounds: GPA 0x{:x}..0x{:x} not within 0x{:x}..0x{:x}",
                gpa,
                gpa + data.len() as u64,
                self.guest_base,
                self.guest_base + self.size as u64
            ));
        }

        let offset = (gpa - self.guest_base) as usize;
        unsafe {
            let dest = self.ptr.as_ptr().add(offset);
            std::ptr::copy_nonoverlapping(data.as_ptr(), dest, data.len());
        }
        Ok(())
    }

    /// Reads raw bytes from guest memory.
    pub fn read(&self, gpa: u64, buf: &mut [u8]) -> Result<(), String> {
        if gpa < self.guest_base || gpa + (buf.len() as u64) > self.guest_base + (self.size as u64)
        {
            return Err(format!(
                "Address out of bounds: GPA 0x{:x}..0x{:x} not within 0x{:x}..0x{:x}",
                gpa,
                gpa + buf.len() as u64,
                self.guest_base,
                self.guest_base + self.size as u64
            ));
        }

        let offset = (gpa - self.guest_base) as usize;
        unsafe {
            let src = self.ptr.as_ptr().add(offset);
            std::ptr::copy_nonoverlapping(src, buf.as_mut_ptr(), buf.len());
        }
        Ok(())
    }
}

impl Drop for GuestMemory {
    fn drop(&mut self) {
        unsafe {
            let _ = hv_vm_unmap(self.guest_base, self.size);
            dealloc(self.ptr.as_ptr(), self.layout);
        }
    }
}
