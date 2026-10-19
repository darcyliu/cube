//! High-level Virtual Machine instance managing memory, MMIO devices, and GIC.

#![allow(dead_code)]

use std::sync::Arc;

use crate::devices::MmioBus;
use crate::hypervisor::*;
use crate::memory::GuestMemory;

pub struct Vm {
    memory: Option<Arc<GuestMemory>>,
    flash: Option<GuestMemory>,
    flash_vars: Option<GuestMemory>,
    mmio: MmioBus,
    gic_enabled: bool,
}

impl Vm {
    /// Initializes a new Virtual Machine instance and maps physical RAM.
    pub fn new(ram_base: u64, ram_size: usize, prealloc: bool) -> HvResult<Self> {
        unsafe {
            check_hv(hv_vm_create(std::ptr::null()))?;
        }

        let memory = match GuestMemory::new(ram_base, ram_size, prealloc) {
            Ok(memory) => Arc::new(memory),
            Err(e) => {
                unsafe {
                    let _ = hv_vm_destroy();
                }
                return Err(e);
            }
        };
        let mmio = MmioBus::new();

        Ok(Self {
            memory: Some(memory),
            flash: None,
            flash_vars: None,
            mmio,
            gic_enabled: false,
        })
    }

    /// Map a ROM/flash image at `base` (typically GPA 0 for EDK2).
    pub fn map_firmware(&mut self, base: u64, data: &[u8]) -> Result<(), String> {
        let flash = GuestMemory::new(base, data.len(), false)
            .map_err(|e| format!("Failed to map firmware at 0x{base:x}: {e}"))?;
        flash.write(base, data)?;
        self.flash = Some(flash);
        Ok(())
    }

    /// Map writable EFI variable store (QEMU virt flash1 at 0x0400_0000).
    pub fn map_firmware_vars(&mut self, base: u64, data: &[u8]) -> Result<(), String> {
        let flash = GuestMemory::new(base, data.len(), false)
            .map_err(|e| format!("Failed to map EFI vars at 0x{base:x}: {e}"))?;
        flash.write(base, data)?;
        self.flash_vars = Some(flash);
        Ok(())
    }

    /// Initializes native in-kernel ARM GICv3 device (macOS 15+).
    /// Must be called BEFORE creating any vCPUs.
    pub fn init_gic(&mut self, distributor_base: u64, redistributor_base: u64) -> HvResult<()> {
        unsafe {
            let config = hv_gic_config_create();
            if config.is_null() {
                return Err(HvError(HV_NO_RESOURCES));
            }

            let ret_dist = hv_gic_config_set_distributor_base(config, distributor_base);
            if ret_dist != HV_SUCCESS {
                os_release(config);
                return Err(HvError(ret_dist));
            }

            let ret_redist = hv_gic_config_set_redistributor_base(config, redistributor_base);
            if ret_redist != HV_SUCCESS {
                os_release(config);
                return Err(HvError(ret_redist));
            }

            let ret_create = hv_gic_create(config);
            os_release(config);
            check_hv(ret_create)?;
        }
        self.gic_enabled = true;
        Ok(())
    }

    pub fn gic_enabled(&self) -> bool {
        self.gic_enabled
    }

    pub fn memory(&self) -> Arc<GuestMemory> {
        self.memory.as_ref().unwrap().clone()
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
        self.mmio.clear();
        self.flash.take();
        self.flash_vars.take();
        self.memory.take();
        unsafe {
            let _ = hv_vm_destroy();
        }
    }
}
