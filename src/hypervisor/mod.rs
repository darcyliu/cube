//! VMX Hypervisor.framework bindings from the macOS 10.15 SDK.
//! There is no `HV_DEADLINE_FOREVER` and no `hv_vmx_vcpu_get_cap_write_vmcs`.
//! HLT blocks in `hv_vcpu_run`. A finite deadline would be `hv_vcpu_run_until`
//! (mach absolute time, VMX preemption timer, `HV_UNSUPPORTED` if the CPU lacks it).

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

pub type hv_vmx_capability_t = i32;
pub const HV_VMX_CAP_PINBASED: hv_vmx_capability_t = 0;
pub const HV_VMX_CAP_PROCBASED: hv_vmx_capability_t = 1;
pub const HV_VMX_CAP_PROCBASED2: hv_vmx_capability_t = 2;
pub const HV_VMX_CAP_ENTRY: hv_vmx_capability_t = 3;
pub const HV_VMX_CAP_EXIT: hv_vmx_capability_t = 4;
pub const HV_VMX_CAP_PREEMPTION_TIMER: hv_vmx_capability_t = 32;

// VMCS field encodings from the 10.15 `hv_arch_vmx.h`.
pub const VMCS_VPID: u32 = 0x0000;
pub const VMCS_CTRL_MSR_BITMAPS: u32 = 0x2004;
pub const VMCS_CTRL_TSC_OFFSET: u32 = 0x2010;
pub const VMCS_CTRL_VIRTUAL_APIC: u32 = 0x2012;
pub const VMCS_CTRL_EPTP: u32 = 0x201a;
pub const VMCS_GUEST_PHYSICAL_ADDRESS: u32 = 0x2400;
pub const VMCS_GUEST_LINK_POINTER: u32 = 0x2800;
pub const VMCS_GUEST_IA32_DEBUGCTL: u32 = 0x2802;
pub const VMCS_GUEST_IA32_PAT: u32 = 0x2804;
pub const VMCS_GUEST_IA32_EFER: u32 = 0x2806;
pub const VMCS_CTRL_PIN_BASED: u32 = 0x4000;
pub const VMCS_CTRL_CPU_BASED: u32 = 0x4002;
pub const VMCS_CTRL_EXC_BITMAP: u32 = 0x4004;
pub const VMCS_CTRL_CR3_COUNT: u32 = 0x400a;
pub const VMCS_CTRL_VMEXIT_CONTROLS: u32 = 0x400c;
pub const VMCS_CTRL_VMENTRY_CONTROLS: u32 = 0x4012;
pub const VMCS_CTRL_TPR_THRESHOLD: u32 = 0x401c;
pub const VMCS_CTRL_CPU_BASED2: u32 = 0x401e;
pub const VMCS_RO_INSTR_ERROR: u32 = 0x4400;
pub const VMCS_RO_EXIT_REASON: u32 = 0x4402;
pub const VMCS_RO_VMEXIT_INSTR_LEN: u32 = 0x440c;
pub const VMCS_GUEST_ES: u32 = 0x0800;
pub const VMCS_GUEST_CS: u32 = 0x0802;
pub const VMCS_GUEST_SS: u32 = 0x0804;
pub const VMCS_GUEST_DS: u32 = 0x0806;
pub const VMCS_GUEST_FS: u32 = 0x0808;
pub const VMCS_GUEST_GS: u32 = 0x080a;
pub const VMCS_GUEST_LDTR: u32 = 0x080c;
pub const VMCS_GUEST_TR: u32 = 0x080e;
pub const VMCS_GUEST_ES_LIMIT: u32 = 0x4800;
pub const VMCS_GUEST_CS_LIMIT: u32 = 0x4802;
pub const VMCS_GUEST_SS_LIMIT: u32 = 0x4804;
pub const VMCS_GUEST_DS_LIMIT: u32 = 0x4806;
pub const VMCS_GUEST_FS_LIMIT: u32 = 0x4808;
pub const VMCS_GUEST_GS_LIMIT: u32 = 0x480a;
pub const VMCS_GUEST_LDTR_LIMIT: u32 = 0x480c;
pub const VMCS_GUEST_TR_LIMIT: u32 = 0x480e;
pub const VMCS_GUEST_GDTR_LIMIT: u32 = 0x4810;
pub const VMCS_GUEST_IDTR_LIMIT: u32 = 0x4812;
pub const VMCS_GUEST_ES_AR: u32 = 0x4814;
pub const VMCS_GUEST_CS_AR: u32 = 0x4816;
pub const VMCS_GUEST_SS_AR: u32 = 0x4818;
pub const VMCS_GUEST_DS_AR: u32 = 0x481a;
pub const VMCS_GUEST_FS_AR: u32 = 0x481c;
pub const VMCS_GUEST_GS_AR: u32 = 0x481e;
pub const VMCS_GUEST_LDTR_AR: u32 = 0x4820;
pub const VMCS_GUEST_TR_AR: u32 = 0x4822;
pub const VMCS_GUEST_INTERRUPTIBILITY: u32 = 0x4824;
pub const VMCS_GUEST_ACTIVITY_STATE: u32 = 0x4826;
pub const VMCS_GUEST_VMX_TIMER_VALUE: u32 = 0x482e;
pub const VMCS_CTRL_CR0_MASK: u32 = 0x6000;
pub const VMCS_CTRL_CR4_MASK: u32 = 0x6002;
pub const VMCS_CTRL_CR0_SHADOW: u32 = 0x6004;
pub const VMCS_CTRL_CR4_SHADOW: u32 = 0x6006;
pub const VMCS_RO_EXIT_QUALIFIC: u32 = 0x6400;
pub const VMCS_GUEST_CR0: u32 = 0x6800;
pub const VMCS_GUEST_CR3: u32 = 0x6802;
pub const VMCS_GUEST_CR4: u32 = 0x6804;
pub const VMCS_GUEST_ES_BASE: u32 = 0x6806;
pub const VMCS_GUEST_CS_BASE: u32 = 0x6808;
pub const VMCS_GUEST_SS_BASE: u32 = 0x680a;
pub const VMCS_GUEST_DS_BASE: u32 = 0x680c;
pub const VMCS_GUEST_FS_BASE: u32 = 0x680e;
pub const VMCS_GUEST_GS_BASE: u32 = 0x6810;
pub const VMCS_GUEST_LDTR_BASE: u32 = 0x6812;
pub const VMCS_GUEST_TR_BASE: u32 = 0x6814;
pub const VMCS_GUEST_GDTR_BASE: u32 = 0x6816;
pub const VMCS_GUEST_IDTR_BASE: u32 = 0x6818;
pub const VMCS_GUEST_DR7: u32 = 0x681a;
pub const VMCS_GUEST_RSP: u32 = 0x681c;
pub const VMCS_GUEST_RIP: u32 = 0x681e;
pub const VMCS_GUEST_RFLAGS: u32 = 0x6820;
pub const VMCS_GUEST_DEBUG_EXC: u32 = 0x6822;

pub const PIN_BASED_INTR: u64 = 1 << 0;
pub const PIN_BASED_NMI: u64 = 1 << 3;
pub const CPU_BASED_HLT: u64 = 1 << 7;
pub const CPU_BASED_MWAIT: u64 = 1 << 10;
pub const CPU_BASED_RDPMC: u64 = 1 << 11;
pub const CPU_BASED_CR8_LOAD: u64 = 1 << 19;
pub const CPU_BASED_CR8_STORE: u64 = 1 << 20;
pub const CPU_BASED_TPR_SHADOW: u64 = 1 << 21;
pub const CPU_BASED_UNCOND_IO: u64 = 1 << 24;
pub const CPU_BASED_MSR_BITMAPS: u64 = 1 << 28;
pub const CPU_BASED_MONITOR: u64 = 1 << 29;
pub const CPU_BASED_SECONDARY_CTLS: u64 = 1 << 31;
pub const CPU_BASED_TSC_OFFSET: u64 = 1 << 3;
pub const CPU_BASED2_EPT: u64 = 1 << 1;
pub const CPU_BASED2_VPID: u64 = 1 << 5;
pub const CPU_BASED2_UNRESTRICTED: u64 = 1 << 7;
pub const VMEXIT_SAVE_IA32_PAT: u64 = 1 << 18;
pub const VMEXIT_LOAD_IA32_PAT: u64 = 1 << 19;
pub const VMEXIT_SAVE_EFER: u64 = 1 << 20;
pub const VMEXIT_LOAD_EFER: u64 = 1 << 21;
pub const VMENTRY_LOAD_DBG_CONTROLS: u64 = 1 << 2;
pub const VMENTRY_GUEST_IA32E: u64 = 1 << 9;
pub const VMENTRY_LOAD_IA32_PAT: u64 = 1 << 14;
pub const VMENTRY_LOAD_EFER: u64 = 1 << 15;

pub const VMX_REASON_IRQ: u16 = 1;
pub const VMX_REASON_TRIPLE_FAULT: u16 = 2;
pub const VMX_REASON_CPUID: u16 = 10;
pub const VMX_REASON_HLT: u16 = 12;
pub const VMX_REASON_IO: u16 = 30;
pub const VMX_REASON_VMENTRY_GUEST: u16 = 33;
pub const VMX_REASON_EPT_VIOLATION: u16 = 48;
pub const VMX_REASON_EPT_MISCONFIG: u16 = 49;
pub const VMX_REASON_VMX_TIMER: u16 = 52;

pub const ACTIVITY_ACTIVE: u64 = 0;
pub const ACTIVITY_HLT: u64 = 1;

pub const IA32_PAT: u32 = 0x277;
pub const IA32_KERNEL_GS_BASE: u32 = 0xc000_0102;
pub const PAT_RESET: u64 = 0x0007_0406_0007_0406;

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

    pub fn hv_vcpu_destroy(vcpu: hv_vcpuid_t) -> hv_return_t;
    pub fn hv_vcpu_get_exec_time(vcpu: hv_vcpuid_t, time: *mut u64) -> hv_return_t;
    pub fn hv_vcpu_enable_native_msr(vcpu: hv_vcpuid_t, msr: u32, enable: bool) -> hv_return_t;

    pub fn hv_vmx_vcpu_read_vmcs(vcpu: hv_vcpuid_t, field: u32, value: *mut u64) -> hv_return_t;
    pub fn hv_vmx_vcpu_write_vmcs(vcpu: hv_vcpuid_t, field: u32, value: u64) -> hv_return_t;
    pub fn hv_vmx_read_capability(field: hv_vmx_capability_t, value: *mut u64) -> hv_return_t;
    /// Present on 10.15. Not used until the APIC is emulated.
    pub fn hv_vmx_vcpu_set_apic_address(vcpu: hv_vcpuid_t, gpa: hv_gpaddr_t) -> hv_return_t;
}
