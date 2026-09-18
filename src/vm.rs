//! High-level Virtual Machine instance managing memory and MMIO devices.

#![allow(dead_code)]

use crate::devices::MmioBus;
use crate::hypervisor::{check_hv, hv_vm_create, hv_vm_destroy, HvResult};
use crate::memory::GuestMemory;

pub struct Vm {
    memory: GuestMemory,
    mmio: MmioBus,
}

impl Vm {
    /// Initializes a new Virtual Machine instance and maps physical RAM.
    pub fn new(ram_base: u64, ram_size: usize) -> HvResult<Self> {
        unsafe {
            check_hv(hv_vm_create(std::ptr::null()))?;
        }

        let memory = GuestMemory::new(ram_base, ram_size)?;
        let mmio = MmioBus::new();

        Ok(Self { memory, mmio })
    }

    pub fn memory(&self) -> &GuestMemory {
        &self.memory
    }

    pub fn memory_mut(&mut self) -> &mut GuestMemory {
        &mut self.memory
    }

    pub fn mmio(&self) -> &MmioBus {
        &self.mmio
    }

    pub fn mmio_mut(&mut self) -> &mut MmioBus {
        &mut self.mmio
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        unsafe {
            let _ = hv_vm_destroy();
        }
    }
}
