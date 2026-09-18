//! High-level Virtual Machine instance managing memory, MMIO devices, and GIC.

#![allow(dead_code)]

use std::sync::Arc;

use crate::devices::MmioBus;
use crate::hypervisor::*;
use crate::memory::GuestMemory;

pub struct Vm {
    memory: Arc<GuestMemory>,
    mmio: MmioBus,
    gic_enabled: bool,
}

impl Vm {
    /// Initializes a new Virtual Machine instance and maps physical RAM.
    pub fn new(ram_base: u64, ram_size: usize) -> HvResult<Self> {
        unsafe {
            check_hv(hv_vm_create(std::ptr::null()))?;
        }

        let memory = Arc::new(GuestMemory::new(ram_base, ram_size)?);
        let mmio = MmioBus::new();

        Ok(Self {
            memory,
            mmio,
            gic_enabled: false,
        })
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
        self.memory.clone()
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
