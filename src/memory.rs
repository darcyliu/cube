//! Guest physical memory management aligned to Apple Silicon page boundaries.

#![allow(dead_code)]

use std::ptr::NonNull;
use std::sync::atomic::{AtomicU16, Ordering};

use crate::hypervisor::{
    check_hv, hv_vm_map, hv_vm_unmap, HvResult, HV_MEMORY_EXEC, HV_MEMORY_READ, HV_MEMORY_WRITE,
};

/// Host page size on Apple Silicon is 16KB (0x4000).
pub const GUEST_PAGE_SIZE: usize = 0x4000;

pub struct GuestMemory {
    ptr: NonNull<u8>,
    guest_base: u64,
    size: usize,
    mapped: bool,
}

unsafe impl Send for GuestMemory {}
unsafe impl Sync for GuestMemory {}

impl GuestMemory {
    /// Allocates and maps a block of guest physical memory at `guest_base`.
    /// `size` will be rounded up to the nearest 16KB page multiple.
    /// When `prealloc` is false, anonymous mmap is demand-paged by the host OS.
    /// When `prealloc` is true, pages are pre-faulted upfront on launch.
    pub fn new(guest_base: u64, requested_size: usize, prealloc: bool) -> HvResult<Self> {
        let size = requested_size
            .checked_add(GUEST_PAGE_SIZE - 1)
            .map(|n| n & !(GUEST_PAGE_SIZE - 1))
            .filter(|&n| n > 0 && guest_base.checked_add(n as u64).is_some())
            .ok_or(crate::hypervisor::HvError(
                crate::hypervisor::HV_BAD_ARGUMENT,
            ))?;

        // Allocate memory via anonymous mmap: page-aligned and zero-filled by the OS
        let raw_ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANON | libc::MAP_PRIVATE,
                -1,
                0,
            )
        };
        if raw_ptr == libc::MAP_FAILED || raw_ptr.is_null() {
            return Err(crate::hypervisor::HvError(
                crate::hypervisor::HV_NO_RESOURCES,
            ));
        }

        // If pre-allocation is explicitly requested, pre-fault all pages upfront
        if prealloc {
            unsafe {
                std::ptr::write_bytes(raw_ptr as *mut u8, 0, size);
            }
        }

        let ptr = NonNull::new(raw_ptr as *mut u8).ok_or(crate::hypervisor::HvError(
            crate::hypervisor::HV_NO_RESOURCES,
        ))?;

        // Map into VM address space with Read, Write, and Execute permissions
        let flags = HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC;
        unsafe {
            if let Err(e) = check_hv(hv_vm_map(ptr.as_ptr() as *mut _, guest_base, size, flags)) {
                libc::munmap(raw_ptr, size);
                return Err(e);
            }
        }

        Ok(Self {
            ptr,
            guest_base,
            size,
            mapped: true,
        })
    }

    #[cfg(test)]
    pub fn new_test(guest_base: u64, size: usize) -> Self {
        let raw_ptr = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANON | libc::MAP_PRIVATE,
                -1,
                0,
            )
        };
        assert!(raw_ptr != libc::MAP_FAILED && !raw_ptr.is_null());
        let ptr = NonNull::new(raw_ptr as *mut u8).unwrap();
        Self {
            ptr,
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
        let offset = self.checked_offset(gpa, 2)?;
        let ptr = unsafe { self.ptr.as_ptr().add(offset) };
        if (ptr as usize) % std::mem::align_of::<AtomicU16>() == 0 {
            // Queue indices are aligned and shared with concurrently running vCPUs.
            let value = unsafe { AtomicU16::from_ptr(ptr.cast()).load(Ordering::Acquire) };
            Ok(u16::from_le(value))
        } else {
            let mut bytes = [0u8; 2];
            self.read(gpa, &mut bytes)?;
            Ok(u16::from_le_bytes(bytes))
        }
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
        let ptr = unsafe { self.ptr.as_ptr().add(offset) };
        if (ptr as usize) % std::mem::align_of::<AtomicU16>() == 0 {
            // Publish the entire index at once, after preceding completion writes.
            unsafe { AtomicU16::from_ptr(ptr.cast()).store(val.to_le(), Ordering::Release) };
        } else {
            // Unaligned data accesses remain supported, but are not atomic.
            let bytes = val.to_le_bytes();
            unsafe {
                ptr.write_volatile(bytes[0]);
                ptr.add(1).write_volatile(bytes[1]);
            }
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
            libc::munmap(self.ptr.as_ptr() as *mut libc::c_void, self.size);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aligned_indices_do_not_tear_across_byte_boundaries() {
        let mem = std::sync::Arc::new(GuestMemory::new_test(0, 0x4000));
        mem.write_u16(2, 0x00ff).unwrap();
        let writer_mem = mem.clone();
        let writer = std::thread::spawn(move || {
            for _ in 0..100_000 {
                for value in [0x0100, 0xffff, 0, 0x00ff] {
                    writer_mem.write_u16(2, value).unwrap();
                }
            }
        });
        for _ in 0..100_000 {
            assert!(matches!(mem.read_u16(2).unwrap(), 0x00ff | 0x0100 | 0xffff | 0));
        }
        writer.join().unwrap();
        mem.write_u16(5, 0xabcd).unwrap();
        assert_eq!(mem.read_u16(5).unwrap(), 0xabcd);
        let mut bytes = [0; 2];
        mem.read(5, &mut bytes).unwrap();
        assert_eq!(bytes, [0xcd, 0xab]);
    }

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
