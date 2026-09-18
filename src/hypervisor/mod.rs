//! Low-level bindings and abstractions for Apple's Hypervisor.framework on macOS ARM64.

#![allow(non_camel_case_types, non_upper_case_globals, dead_code)]

use std::fmt;

pub type hv_return_t = i32;

pub const HV_SUCCESS: hv_return_t = 0;
pub const HV_ERROR: hv_return_t = -85377023; // 0xfae94001 as signed i32
pub const HV_BUSY: hv_return_t = -85377022;
pub const HV_BAD_ARGUMENT: hv_return_t = -85377021;
pub const HV_ILLEGAL_GUEST_STATE: hv_return_t = -85377020;
pub const HV_NO_RESOURCES: hv_return_t = -85377019;
pub const HV_NO_DEVICE: hv_return_t = -85377018;
pub const HV_DENIED: hv_return_t = -85377017;
pub const HV_EXISTS: hv_return_t = -85377016;
pub const HV_UNSUPPORTED: hv_return_t = -85377009;

// Memory mapping permissions
pub const HV_MEMORY_READ: u64 = 1 << 0;
pub const HV_MEMORY_WRITE: u64 = 1 << 1;
pub const HV_MEMORY_EXEC: u64 = 1 << 2;

// Exit reasons
pub const HV_EXIT_REASON_CANCELED: u32 = 0;
pub const HV_EXIT_REASON_EXCEPTION: u32 = 1;
pub const HV_EXIT_REASON_VTIMER_ACTIVATED: u32 = 2;
pub const HV_EXIT_REASON_UNKNOWN: u32 = 3;

// Register indices (HV_REG_*)
pub const HV_REG_X0: u32 = 0;
pub const HV_REG_X1: u32 = 1;
pub const HV_REG_X2: u32 = 2;
pub const HV_REG_X3: u32 = 3;
pub const HV_REG_X4: u32 = 4;
pub const HV_REG_X5: u32 = 5;
pub const HV_REG_X6: u32 = 6;
pub const HV_REG_X7: u32 = 7;
pub const HV_REG_X8: u32 = 8;
pub const HV_REG_X9: u32 = 9;
pub const HV_REG_X10: u32 = 10;
pub const HV_REG_X11: u32 = 11;
pub const HV_REG_X12: u32 = 12;
pub const HV_REG_X13: u32 = 13;
pub const HV_REG_X14: u32 = 14;
pub const HV_REG_X15: u32 = 15;
pub const HV_REG_X16: u32 = 16;
pub const HV_REG_X17: u32 = 17;
pub const HV_REG_X18: u32 = 18;
pub const HV_REG_X19: u32 = 19;
pub const HV_REG_X20: u32 = 20;
pub const HV_REG_X21: u32 = 21;
pub const HV_REG_X22: u32 = 22;
pub const HV_REG_X23: u32 = 23;
pub const HV_REG_X24: u32 = 24;
pub const HV_REG_X25: u32 = 25;
pub const HV_REG_X26: u32 = 26;
pub const HV_REG_X27: u32 = 27;
pub const HV_REG_X28: u32 = 28;
pub const HV_REG_X29: u32 = 29;
pub const HV_REG_FP: u32 = 29;
pub const HV_REG_X30: u32 = 30;
pub const HV_REG_LR: u32 = 30;
pub const HV_REG_PC: u32 = 31;
pub const HV_REG_FPCR: u32 = 32;
pub const HV_REG_FPSR: u32 = 33;
pub const HV_REG_CPSR: u32 = 34;

// System registers (HV_SYS_REG_*)
pub const HV_SYS_REG_MIDR_EL1: u16 = 0xc000;
pub const HV_SYS_REG_MPIDR_EL1: u16 = 0xc005;
pub const HV_SYS_REG_SCTLR_EL1: u16 = 0xc080;
pub const HV_SYS_REG_CPACR_EL1: u16 = 0xc082;
pub const HV_SYS_REG_TTBR0_EL1: u16 = 0xc100;
pub const HV_SYS_REG_TTBR1_EL1: u16 = 0xc101;
pub const HV_SYS_REG_TCR_EL1: u16 = 0xc102;
pub const HV_SYS_REG_SPSR_EL1: u16 = 0xc200;
pub const HV_SYS_REG_ELR_EL1: u16 = 0xc201;
pub const HV_SYS_REG_SP_EL0: u16 = 0xc208;
pub const HV_SYS_REG_ESR_EL1: u16 = 0xc290;
pub const HV_SYS_REG_FAR_EL1: u16 = 0xc300;
pub const HV_SYS_REG_MAIR_EL1: u16 = 0xc510;
pub const HV_SYS_REG_VBAR_EL1: u16 = 0xc600;
pub const HV_SYS_REG_SP_EL1: u16 = 0xe208;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HvVcpuExitException {
    pub syndrome: u64,
    pub virtual_address: u64,
    pub physical_address: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct HvVcpuExit {
    pub reason: u32,
    _pad: u32,
    pub exception: HvVcpuExitException,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvError(pub hv_return_t);

impl fmt::Display for HvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            HV_SUCCESS => write!(f, "HV_SUCCESS"),
            HV_ERROR => write!(f, "HV_ERROR: generic error"),
            HV_BUSY => write!(f, "HV_BUSY: resource busy"),
            HV_BAD_ARGUMENT => write!(f, "HV_BAD_ARGUMENT: invalid argument"),
            HV_ILLEGAL_GUEST_STATE => write!(f, "HV_ILLEGAL_GUEST_STATE: guest in illegal state"),
            HV_NO_RESOURCES => write!(f, "HV_NO_RESOURCES: out of resources"),
            HV_NO_DEVICE => write!(f, "HV_NO_DEVICE: no device"),
            HV_DENIED => write!(f, "HV_DENIED: access denied (check entitlements / code signing)"),
            HV_EXISTS => write!(f, "HV_EXISTS: resource already exists"),
            HV_UNSUPPORTED => write!(f, "HV_UNSUPPORTED: operation unsupported"),
            other => write!(f, "Hypervisor error: 0x{:08x}", other),
        }
    }
}

impl std::error::Error for HvError {}

pub type HvResult<T> = Result<T, HvError>;

#[inline]
pub fn check_hv(ret: hv_return_t) -> HvResult<()> {
    if ret == HV_SUCCESS {
        Ok(())
    } else {
        Err(HvError(ret))
    }
}

unsafe extern "C" {
    pub fn hv_vm_create(config: *const core::ffi::c_void) -> hv_return_t;
    pub fn hv_vm_destroy() -> hv_return_t;
    pub fn hv_vm_map(
        addr: *mut core::ffi::c_void,
        ipa: u64,
        size: usize,
        flags: u64,
    ) -> hv_return_t;
    pub fn hv_vm_unmap(ipa: u64, size: usize) -> hv_return_t;

    pub fn hv_vcpu_create(
        vcpu: *mut u64,
        exit: *mut *mut HvVcpuExit,
        config: *const core::ffi::c_void,
    ) -> hv_return_t;
    pub fn hv_vcpu_destroy(vcpu: u64) -> hv_return_t;
    pub fn hv_vcpu_run(vcpu: u64) -> hv_return_t;

    pub fn hv_vcpu_get_reg(vcpu: u64, reg: u32, value: *mut u64) -> hv_return_t;
    pub fn hv_vcpu_set_reg(vcpu: u64, reg: u32, value: u64) -> hv_return_t;

    pub fn hv_vcpu_get_sys_reg(vcpu: u64, reg: u16, value: *mut u64) -> hv_return_t;
    pub fn hv_vcpu_set_sys_reg(vcpu: u64, reg: u16, value: u64) -> hv_return_t;
}
