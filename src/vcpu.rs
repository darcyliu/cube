//! vCPU management and execution loop for ARM64.

#![allow(dead_code)]

use crate::devices::MmioBus;
use crate::hypervisor::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcpuExitStatus {
    Shutdown,
    Reset,
    Halted,
    Canceled,
}

pub struct Vcpu {
    id: u64,
    exit_ptr: *mut HvVcpuExit,
}

impl Vcpu {
    /// Creates and configures a new vCPU for the current thread.
    pub fn new() -> HvResult<Self> {
        let mut id: u64 = 0;
        let mut exit_ptr: *mut HvVcpuExit = std::ptr::null_mut();

        unsafe {
            check_hv(hv_vcpu_create(&mut id, &mut exit_ptr, std::ptr::null()))?;
        }

        Ok(Self { id, exit_ptr })
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn get_reg(&self, reg: u32) -> HvResult<u64> {
        let mut val = 0u64;
        unsafe {
            check_hv(hv_vcpu_get_reg(self.id, reg, &mut val))?;
        }
        Ok(val)
    }

    pub fn set_reg(&self, reg: u32, val: u64) -> HvResult<()> {
        unsafe {
            check_hv(hv_vcpu_set_reg(self.id, reg, val))?;
        }
        Ok(())
    }

    pub fn get_sys_reg(&self, reg: u16) -> HvResult<u64> {
        let mut val = 0u64;
        unsafe {
            check_hv(hv_vcpu_get_sys_reg(self.id, reg, &mut val))?;
        }
        Ok(val)
    }

    pub fn set_sys_reg(&self, reg: u16, val: u64) -> HvResult<()> {
        unsafe {
            check_hv(hv_vcpu_set_sys_reg(self.id, reg, val))?;
        }
        Ok(())
    }

    /// Sets initial state for booting at Exception Level 1 (EL1).
    pub fn setup_boot_state(&self, entry_point: u64, stack_pointer: u64) -> HvResult<()> {
        // Initial PC
        self.set_reg(HV_REG_PC, entry_point)?;

        // Set CPSR: EL1h mode with all DAIF interrupts masked (0x3c5 = 0b0011_1100_0101)
        self.set_reg(HV_REG_CPSR, 0x3c5)?;

        // Set SP_EL1
        self.set_sys_reg(HV_SYS_REG_SP_EL1, stack_pointer)?;

        // SCTLR_EL1: standard reset value, MMU disabled (flat physical addressing)
        self.set_sys_reg(HV_SYS_REG_SCTLR_EL1, 0x00000000)?;

        // CPACR_EL1: Enable FP/NEON access at EL1 (FPEN = 0b11 at bits [21:20])
        self.set_sys_reg(HV_SYS_REG_CPACR_EL1, 3 << 20)?;

        Ok(())
    }

    /// Runs the vCPU until it exits, dispatching MMIO, PSCI calls, and exceptions.
    pub fn run(&mut self, mmio: &mut MmioBus) -> Result<VcpuExitStatus, String> {
        loop {
            unsafe {
                let ret = hv_vcpu_run(self.id);
                if ret != HV_SUCCESS {
                    return Err(format!("hv_vcpu_run failed: {}", HvError(ret)));
                }
            }

            let exit = unsafe { *self.exit_ptr };

            match exit.reason {
                HV_EXIT_REASON_EXCEPTION => {
                    let syndrome = exit.exception.syndrome;
                    let ec = (syndrome >> 26) & 0x3f;

                    match ec {
                        // EC 0x24: Data Abort from lower or same EL (e.g. MMIO)
                        0x24 => {
                            self.handle_data_abort(syndrome, exit.exception.physical_address, mmio)?;
                        }
                        // EC 0x16: Hypervisor Call (HVC) -> PSCI emulation
                        0x16 => {
                            if let Some(status) = self.handle_psci()? {
                                return Ok(status);
                            }
                        }
                        // EC 0x01: Trapped WFI / WFE instruction
                        0x01 => {
                            return Ok(VcpuExitStatus::Halted);
                        }
                        _ => {
                            let pc = self.get_reg(HV_REG_PC).unwrap_or(0);
                            return Err(format!(
                                "Unhandled Exception Class EC=0x{:02x} at PC=0x{:08x} (Syndrome=0x{:016x}, FAR=0x{:016x}, IPA=0x{:016x})",
                                ec, pc, syndrome, exit.exception.virtual_address, exit.exception.physical_address
                            ));
                        }
                    }
                }
                HV_EXIT_REASON_CANCELED => {
                    return Ok(VcpuExitStatus::Canceled);
                }
                HV_EXIT_REASON_VTIMER_ACTIVATED => {
                    // Timer event; continue execution for prototype
                }
                _ => {
                    return Err(format!("Unexpected vCPU exit reason: {}", exit.reason));
                }
            }
        }
    }

    fn handle_data_abort(
        &mut self,
        syndrome: u64,
        fault_ipa: u64,
        mmio: &mut MmioBus,
    ) -> Result<(), String> {
        let isv = (syndrome >> 24) & 1;
        if isv == 0 {
            let pc = self.get_reg(HV_REG_PC).unwrap_or(0);
            return Err(format!(
                "Data abort at IPA 0x{:08x} (PC=0x{:08x}) has ISV=0 (instruction syndrome invalid)",
                fault_ipa, pc
            ));
        }

        let sas = ((syndrome >> 22) & 3) as usize;
        let access_size = 1 << sas; // 1, 2, 4, or 8 bytes
        let srt = ((syndrome >> 16) & 0x1f) as u32; // Register index X0-X30; 31 is XZR
        let wnr = (syndrome >> 6) & 1; // 1 = Write, 0 = Read

        if wnr == 1 {
            // MMIO Write
            let val = if srt == 31 {
                0
            } else {
                self.get_reg(HV_REG_X0 + srt).map_err(|e| e.to_string())?
            };
            mmio.write(fault_ipa, val, access_size)?;
        } else {
            // MMIO Read
            let val = mmio.read(fault_ipa, access_size)?;
            if srt != 31 {
                self.set_reg(HV_REG_X0 + srt, val).map_err(|e| e.to_string())?;
            }
        }

        // Advance PC past the trapping instruction (ARM64 instructions are 4 bytes)
        let pc = self.get_reg(HV_REG_PC).map_err(|e| e.to_string())?;
        self.set_reg(HV_REG_PC, pc + 4).map_err(|e| e.to_string())?;

        Ok(())
    }

    fn handle_psci(&mut self) -> Result<Option<VcpuExitStatus>, String> {
        let func_id = self.get_reg(HV_REG_X0).map_err(|e| e.to_string())?;

        match func_id {
            // PSCI_VERSION (0x84000000): Return PSCI v0.2
            0x84000000 => {
                self.set_reg(HV_REG_X0, 0x0002).map_err(|e| e.to_string())?;
                let pc = self.get_reg(HV_REG_PC).map_err(|e| e.to_string())?;
                self.set_reg(HV_REG_PC, pc + 4).map_err(|e| e.to_string())?;
                Ok(None)
            }
            // PSCI SYSTEM_OFF (0x84000008)
            0x84000008 => Ok(Some(VcpuExitStatus::Shutdown)),
            // PSCI SYSTEM_RESET (0x84000009)
            0x84000009 => Ok(Some(VcpuExitStatus::Reset)),
            _ => Err(format!("Unhandled PSCI function ID: 0x{:08x}", func_id)),
        }
    }
}

impl Drop for Vcpu {
    fn drop(&mut self) {
        unsafe {
            let _ = hv_vcpu_destroy(self.id);
        }
    }
}
