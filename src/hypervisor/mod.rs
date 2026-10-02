//! VMX Hypervisor.framework bindings from the macOS 10.15 SDK.
//! Nothing in this crate calls these functions yet.

#![allow(non_camel_case_types, non_upper_case_globals, dead_code)]

use std::fmt;

pub type hv_return_t = i32;
pub type hv_vcpuid_t = libc::c_uint;
pub type hv_vm_options_t = u64;
pub type hv_vcpu_options_t = u64;
pub type hv_memory_flags_t = u64;
pub type hv_gpaddr_t = u64;
/// C `hv_x86_reg_t` is an enum that fits in `int`.
pub type hv_x86_reg_t = i32;

pub const HV_SUCCESS: hv_return_t = 0;
pub const HV_ERROR: hv_return_t = 0xfae9_4001_u32 as i32;
pub const HV_BUSY: hv_return_t = 0xfae9_4002_u32 as i32;
pub const HV_BAD_ARGUMENT: hv_return_t = 0xfae9_4003_u32 as i32;
pub const HV_NO_RESOURCES: hv_return_t = 0xfae9_4005_u32 as i32;
pub const HV_NO_DEVICE: hv_return_t = 0xfae9_4006_u32 as i32;
pub const HV_UNSUPPORTED: hv_return_t = 0xfae9_400f_u32 as i32;

pub const HV_VM_DEFAULT: hv_vm_options_t = 0;
pub const HV_VCPU_DEFAULT: hv_vcpu_options_t = 0;

pub const HV_MEMORY_READ: hv_memory_flags_t = 1 << 0;
pub const HV_MEMORY_WRITE: hv_memory_flags_t = 1 << 1;
pub const HV_MEMORY_EXEC: hv_memory_flags_t = 1 << 2;

pub const HV_X86_RIP: hv_x86_reg_t = 0;
pub const HV_X86_RFLAGS: hv_x86_reg_t = 1;
pub const HV_X86_RAX: hv_x86_reg_t = 2;
pub const HV_X86_RCX: hv_x86_reg_t = 3;
pub const HV_X86_RDX: hv_x86_reg_t = 4;
pub const HV_X86_RBX: hv_x86_reg_t = 5;
pub const HV_X86_RSI: hv_x86_reg_t = 6;
pub const HV_X86_RDI: hv_x86_reg_t = 7;
pub const HV_X86_RSP: hv_x86_reg_t = 8;
pub const HV_X86_RBP: hv_x86_reg_t = 9;
pub const HV_X86_R8: hv_x86_reg_t = 10;
pub const HV_X86_R9: hv_x86_reg_t = 11;
pub const HV_X86_R10: hv_x86_reg_t = 12;
pub const HV_X86_R11: hv_x86_reg_t = 13;
pub const HV_X86_R12: hv_x86_reg_t = 14;
pub const HV_X86_R13: hv_x86_reg_t = 15;
pub const HV_X86_R14: hv_x86_reg_t = 16;
pub const HV_X86_R15: hv_x86_reg_t = 17;
pub const HV_X86_CS: hv_x86_reg_t = 18;
pub const HV_X86_SS: hv_x86_reg_t = 19;
pub const HV_X86_DS: hv_x86_reg_t = 20;
pub const HV_X86_ES: hv_x86_reg_t = 21;
pub const HV_X86_FS: hv_x86_reg_t = 22;
pub const HV_X86_GS: hv_x86_reg_t = 23;
pub const HV_X86_IDT_BASE: hv_x86_reg_t = 24;
pub const HV_X86_IDT_LIMIT: hv_x86_reg_t = 25;
pub const HV_X86_GDT_BASE: hv_x86_reg_t = 26;
pub const HV_X86_GDT_LIMIT: hv_x86_reg_t = 27;
pub const HV_X86_LDTR: hv_x86_reg_t = 28;
pub const HV_X86_LDT_BASE: hv_x86_reg_t = 29;
pub const HV_X86_LDT_LIMIT: hv_x86_reg_t = 30;
pub const HV_X86_LDT_AR: hv_x86_reg_t = 31;
pub const HV_X86_TR: hv_x86_reg_t = 32;
pub const HV_X86_TSS_BASE: hv_x86_reg_t = 33;
pub const HV_X86_TSS_LIMIT: hv_x86_reg_t = 34;
pub const HV_X86_TSS_AR: hv_x86_reg_t = 35;
pub const HV_X86_CR0: hv_x86_reg_t = 36;
pub const HV_X86_CR1: hv_x86_reg_t = 37;
pub const HV_X86_CR2: hv_x86_reg_t = 38;
pub const HV_X86_CR3: hv_x86_reg_t = 39;
pub const HV_X86_CR4: hv_x86_reg_t = 40;
pub const HV_X86_DR0: hv_x86_reg_t = 41;
pub const HV_X86_DR1: hv_x86_reg_t = 42;
pub const HV_X86_DR2: hv_x86_reg_t = 43;
pub const HV_X86_DR3: hv_x86_reg_t = 44;
pub const HV_X86_DR4: hv_x86_reg_t = 45;
pub const HV_X86_DR5: hv_x86_reg_t = 46;
pub const HV_X86_DR6: hv_x86_reg_t = 47;
pub const HV_X86_DR7: hv_x86_reg_t = 48;
pub const HV_X86_TPR: hv_x86_reg_t = 49;
pub const HV_X86_XCR0: hv_x86_reg_t = 50;
pub const HV_X86_REGISTERS_MAX: hv_x86_reg_t = 51;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HvError(pub hv_return_t);

impl fmt::Display for HvError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            HV_SUCCESS => write!(f, "HV_SUCCESS"),
            HV_ERROR => write!(f, "HV_ERROR: generic error"),
            HV_BUSY => write!(f, "HV_BUSY: resource busy"),
            HV_BAD_ARGUMENT => write!(f, "HV_BAD_ARGUMENT: invalid argument"),
            HV_NO_RESOURCES => write!(f, "HV_NO_RESOURCES: out of resources"),
            HV_NO_DEVICE => write!(f, "HV_NO_DEVICE: no device"),
            HV_UNSUPPORTED => write!(f, "HV_UNSUPPORTED: operation unsupported"),
            other => write!(f, "Hypervisor error: 0x{other:08x}"),
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
    pub fn hv_vm_create(flags: hv_vm_options_t) -> hv_return_t;
    pub fn hv_vm_destroy() -> hv_return_t;
    pub fn hv_vm_map(
        uva: *const core::ffi::c_void,
        gpa: hv_gpaddr_t,
        size: usize,
        flags: hv_memory_flags_t,
    ) -> hv_return_t;

    pub fn hv_vcpu_create(vcpu: *mut hv_vcpuid_t, flags: hv_vcpu_options_t) -> hv_return_t;
    pub fn hv_vcpu_run(vcpu: hv_vcpuid_t) -> hv_return_t;
    pub fn hv_vcpu_run_until(vcpu: hv_vcpuid_t, deadline: u64) -> hv_return_t;
    pub fn hv_vcpu_interrupt(vcpus: *const hv_vcpuid_t, vcpu_count: libc::c_uint) -> hv_return_t;

    pub fn hv_vcpu_read_register(
        vcpu: hv_vcpuid_t,
        reg: hv_x86_reg_t,
        value: *mut u64,
    ) -> hv_return_t;
    pub fn hv_vcpu_write_register(vcpu: hv_vcpuid_t, reg: hv_x86_reg_t, value: u64) -> hv_return_t;

    pub fn hv_vmx_vcpu_read_vmcs(vcpu: hv_vcpuid_t, field: u32, value: *mut u64) -> hv_return_t;
    pub fn hv_vmx_vcpu_write_vmcs(vcpu: hv_vcpuid_t, field: u32, value: u64) -> hv_return_t;
    pub fn hv_vmx_vcpu_set_apic_address(vcpu: hv_vcpuid_t, gpa: hv_gpaddr_t) -> hv_return_t;
}
