//! One vCPU. HLT sets activity state 1 and blocks in `hv_vcpu_run` (not a sleep).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use vmm::devices::PioBus;
use vmm::terminal::host_log;
use vmm::x86::{
    self, CR0_LONG_MODE, CR4_LONG_MODE, CS_AR, CS_SELECTOR, DATA_AR, DATA_LIMIT, DS_SELECTOR,
    EFER_LONG_MODE, GDT_GPA, GDT_LIMIT, PAYLOAD_GPA, STACK_GPA, TR_AR, TR_LIMIT, TR_SELECTOR,
    TSS_GPA, UNUSABLE_AR,
};

use crate::hypervisor::*;

const MAX_EXITS: u64 = 100_000;
const CR_MASK: u64 = 0xffff_ffff;

pub struct VcpuDone {
    pub exec_ns: Option<u64>,
    pub error: Option<String>,
}

/// Pending flag plus `wfi_cv`. HLT itself stays inside `hv_vcpu_run`;
/// the condvar is only for a thread that is not in that call.
pub struct Kick {
    vcpu: AtomicU32,
    ready: AtomicBool,
    pending: AtomicBool,
    lock: Mutex<()>,
    cv: Condvar,
}

impl Kick {
    pub fn new() -> Self {
        Self {
            vcpu: AtomicU32::new(0),
            ready: AtomicBool::new(false),
            pending: AtomicBool::new(false),
            lock: Mutex::new(()),
            cv: Condvar::new(),
        }
    }

    fn arm(&self, id: hv_vcpuid_t) {
        self.vcpu.store(id, Ordering::Relaxed);
        self.ready.store(true, Ordering::Release);
    }

    fn disarm(&self) {
        self.ready.store(false, Ordering::Release);
    }

    pub fn signal(&self) {
        self.pending.store(true, Ordering::Release);
        {
            let _guard = self.lock.lock().unwrap();
            self.cv.notify_all();
        }
        if self.ready.load(Ordering::Acquire) {
            let id = self.vcpu.load(Ordering::Relaxed);
            unsafe {
                let _ = hv_vcpu_interrupt(&id, 1);
            }
        }
    }

    fn take(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }
}

pub fn run(kick: Arc<Kick>, pio: Arc<PioBus>, cr3: u64, ram_bytes: u64) -> VcpuDone {
    let mut done = VcpuDone {
        exec_ns: None,
        error: None,
    };
    let mut id: hv_vcpuid_t = 0;
    if let Err(e) = check_hv(unsafe { hv_vcpu_create(&mut id, HV_VCPU_DEFAULT) }) {
        done.error = Some(format!("hv_vcpu_create: {e}"));
        return done;
    }
    kick.arm(id);
    if let Err(e) = setup_and_loop(id, &kick, &pio, cr3, ram_bytes) {
        done.error = Some(e);
    }
    // Header: must be called by the owning thread. Shutdown only prints this.
    let mut exec = 0u64;
    match check_hv(unsafe { hv_vcpu_get_exec_time(id, &mut exec) }) {
        Ok(()) => done.exec_ns = Some(exec),
        Err(e) => {
            let note = format!("hv_vcpu_get_exec_time: {e}");
            done.error = Some(match done.error.take() {
                Some(prev) => format!("{prev}; {note}"),
                None => note,
            });
        }
    }
    kick.disarm();
    unsafe {
        let _ = hv_vcpu_destroy(id);
    }
    done
}

fn setup_and_loop(
    id: hv_vcpuid_t,
    kick: &Kick,
    pio: &PioBus,
    cr3: u64,
    ram_bytes: u64,
) -> Result<(), String> {
    write_controls(id)?;
    plant_long_mode(id, cr3)?;
    run_loop(id, kick, pio, ram_bytes)
}

fn write_controls(id: hv_vcpuid_t) -> Result<(), String> {
    let pin_cap = read_cap(HV_VMX_CAP_PINBASED)?;
    let cpu_cap = read_cap(HV_VMX_CAP_PROCBASED)?;
    let cpu2_cap = read_cap(HV_VMX_CAP_PROCBASED2)?;
    let entry_cap = read_cap(HV_VMX_CAP_ENTRY)?;
    let exit_cap = read_cap(HV_VMX_CAP_EXIT)?;

    let pin = x86::vmx_cap_control(pin_cap, PIN_BASED_INTR | PIN_BASED_NMI);
    if pin & PIN_BASED_INTR == 0 {
        return Err(format!(
            "external-interrupt exiting is unavailable (pin cap {pin_cap:#x})"
        ));
    }
    // 10.15 vmcs_lut maps pin, VPID, the MSR-bitmap address, and EPTP onto one
    // slot. Writing any of them replaces the framework EPT. HVF fills those
    // fields itself; the must-1 bits already include the controls we need.

    // The MSR-bitmap address shares the EPT slot on 10.15. Do not set the
    // control unless the capability forces it: an enabled bitmap with no host
    // address fails VM-entry. This guest only OUTs and HLTs.
    let cpu_desired = CPU_BASED_HLT
        | CPU_BASED_UNCOND_IO
        | CPU_BASED_CR8_LOAD
        | CPU_BASED_CR8_STORE
        | CPU_BASED_RDPMC
        | CPU_BASED_MWAIT
        | CPU_BASED_MONITOR
        | CPU_BASED_SECONDARY_CTLS
        | CPU_BASED_TSC_OFFSET;
    let cpu = x86::vmx_cap_control(cpu_cap, cpu_desired);
    let need = CPU_BASED_HLT | CPU_BASED_UNCOND_IO | CPU_BASED_SECONDARY_CTLS;
    if cpu & need != need {
        return Err(format!(
            "primary controls missing HLT/I/O/secondary: got {cpu:#x} cap {cpu_cap:#x}"
        ));
    }

    let cpu2 = x86::vmx_cap_control(cpu2_cap, CPU_BASED2_EPT | CPU_BASED2_UNRESTRICTED);
    let need2 = CPU_BASED2_EPT | CPU_BASED2_UNRESTRICTED;
    if cpu2 & need2 != need2 {
        return Err(format!(
            "EPT and unrestricted guest are required: got {cpu2:#x} cap {cpu2_cap:#x}"
        ));
    }
    wvmcs(id, VMCS_CTRL_CPU_BASED2, cpu2)?;
    wvmcs(id, VMCS_CTRL_CPU_BASED, cpu)?;
    if cpu & CPU_BASED_TPR_SHADOW != 0 {
        // The virtual-APIC address shares the EPT slot on 10.15, so only the
        // threshold is writable. This host does not force the control on.
        wvmcs(id, VMCS_CTRL_TPR_THRESHOLD, 0)?;
    }

    let mut entry_desired = VMENTRY_GUEST_IA32E | VMENTRY_LOAD_EFER;
    if entry_cap >> 32 & VMENTRY_LOAD_IA32_PAT != 0 {
        entry_desired |= VMENTRY_LOAD_IA32_PAT;
    }
    let entry = x86::vmx_cap_control(entry_cap, entry_desired);
    if entry & (VMENTRY_GUEST_IA32E | VMENTRY_LOAD_EFER)
        != (VMENTRY_GUEST_IA32E | VMENTRY_LOAD_EFER)
    {
        return Err(format!(
            "entry controls cannot load IA-32e EFER: got {entry:#x} cap {entry_cap:#x}"
        ));
    }
    wvmcs(id, VMCS_CTRL_VMENTRY_CONTROLS, entry)?;
    if entry & VMENTRY_LOAD_IA32_PAT != 0 {
        // SDM 26.3.1.1 rejects a zero PAT when load-PAT is set.
        wvmcs(id, VMCS_GUEST_IA32_PAT, PAT_RESET)?;
    }

    let mut exit_desired = 0u64;
    for bit in [
        VMEXIT_SAVE_EFER,
        VMEXIT_LOAD_EFER,
        VMEXIT_SAVE_IA32_PAT,
        VMEXIT_LOAD_IA32_PAT,
    ] {
        if exit_cap >> 32 & bit != 0 {
            exit_desired |= bit;
        }
    }
    let exitc = x86::vmx_cap_control(exit_cap, exit_desired);
    wvmcs(id, VMCS_CTRL_VMEXIT_CONTROLS, exitc)?;

    wvmcs(id, VMCS_CTRL_EXC_BITMAP, 0)?;
    wvmcs(id, VMCS_CTRL_CR3_COUNT, 0)?;
    wvmcs(id, VMCS_CTRL_CR0_MASK, CR_MASK)?;
    wvmcs(id, VMCS_CTRL_CR4_MASK, CR_MASK)?;
    // HVF arms the VMX preemption timer inside hv_vcpu_run. A zero value exits at once.
    wvmcs(id, VMCS_GUEST_VMX_TIMER_VALUE, u64::MAX)?;
    Ok(())
}

fn plant_long_mode(id: hv_vcpuid_t, cr3: u64) -> Result<(), String> {
    // Selectors first: a later register write can rebuild the descriptor cache.
    for (reg, sel) in [
        (HV_X86_CS, CS_SELECTOR),
        (HV_X86_SS, DS_SELECTOR),
        (HV_X86_DS, DS_SELECTOR),
        (HV_X86_ES, DS_SELECTOR),
        (HV_X86_FS, 0),
        (HV_X86_GS, 0),
        (HV_X86_TR, TR_SELECTOR),
        (HV_X86_LDTR, 0),
    ] {
        wreg(id, reg, sel)?;
    }

    wvmcs(id, VMCS_GUEST_CS, CS_SELECTOR)?;
    wvmcs(id, VMCS_GUEST_SS, DS_SELECTOR)?;
    wvmcs(id, VMCS_GUEST_DS, DS_SELECTOR)?;
    wvmcs(id, VMCS_GUEST_ES, DS_SELECTOR)?;
    wvmcs(id, VMCS_GUEST_FS, 0)?;
    wvmcs(id, VMCS_GUEST_GS, 0)?;
    wvmcs(id, VMCS_GUEST_TR, TR_SELECTOR)?;
    wvmcs(id, VMCS_GUEST_LDTR, 0)?;

    for (limit, ar, base) in [
        (VMCS_GUEST_CS_LIMIT, VMCS_GUEST_CS_AR, VMCS_GUEST_CS_BASE),
        (VMCS_GUEST_SS_LIMIT, VMCS_GUEST_SS_AR, VMCS_GUEST_SS_BASE),
        (VMCS_GUEST_DS_LIMIT, VMCS_GUEST_DS_AR, VMCS_GUEST_DS_BASE),
        (VMCS_GUEST_ES_LIMIT, VMCS_GUEST_ES_AR, VMCS_GUEST_ES_BASE),
    ] {
        let ar_val = if limit == VMCS_GUEST_CS_LIMIT {
            CS_AR
        } else {
            DATA_AR
        };
        wvmcs(id, limit, DATA_LIMIT)?;
        wvmcs(id, ar, ar_val)?;
        wvmcs(id, base, 0)?;
    }
    for (limit, ar, base) in [
        (VMCS_GUEST_FS_LIMIT, VMCS_GUEST_FS_AR, VMCS_GUEST_FS_BASE),
        (VMCS_GUEST_GS_LIMIT, VMCS_GUEST_GS_AR, VMCS_GUEST_GS_BASE),
        (
            VMCS_GUEST_LDTR_LIMIT,
            VMCS_GUEST_LDTR_AR,
            VMCS_GUEST_LDTR_BASE,
        ),
    ] {
        wvmcs(id, limit, 0)?;
        wvmcs(id, ar, UNUSABLE_AR)?;
        wvmcs(id, base, 0)?;
    }
    wvmcs(id, VMCS_GUEST_TR_LIMIT, TR_LIMIT)?;
    wvmcs(id, VMCS_GUEST_TR_AR, TR_AR)?;
    wvmcs(id, VMCS_GUEST_TR_BASE, TSS_GPA)?;

    wreg(id, HV_X86_GDT_BASE, GDT_GPA)?;
    wreg(id, HV_X86_GDT_LIMIT, GDT_LIMIT)?;
    wreg(id, HV_X86_IDT_BASE, 0)?;
    wreg(id, HV_X86_IDT_LIMIT, 0xffff)?;
    wvmcs(id, VMCS_GUEST_GDTR_BASE, GDT_GPA)?;
    wvmcs(id, VMCS_GUEST_GDTR_LIMIT, GDT_LIMIT)?;
    wvmcs(id, VMCS_GUEST_IDTR_BASE, 0)?;
    wvmcs(id, VMCS_GUEST_IDTR_LIMIT, 0xffff)?;

    let (cr0, cr0_shadow) = x86::publish_cr(CR0_LONG_MODE);
    let (cr4_visible, cr4_shadow) = x86::publish_cr(CR4_LONG_MODE);
    // Hardware CR4.VMXE must be 1 in VMX non-root. The all-ones mask hides it.
    let cr4 = cr4_visible | (1 << 13);
    wreg(id, HV_X86_CR0, cr0)?;
    wreg(id, HV_X86_CR3, cr3)?;
    wreg(id, HV_X86_CR4, cr4)?;
    wreg(id, HV_X86_RIP, PAYLOAD_GPA)?;
    wreg(id, HV_X86_RSP, STACK_GPA)?;
    wreg(id, HV_X86_RFLAGS, 2)?;
    wreg(id, HV_X86_XCR0, 1)?;
    wreg(id, HV_X86_DR7, 0x400)?;

    wvmcs(id, VMCS_GUEST_CR0, cr0)?;
    wvmcs(id, VMCS_CTRL_CR0_SHADOW, cr0_shadow)?;
    wvmcs(id, VMCS_GUEST_CR4, cr4)?;
    wvmcs(id, VMCS_CTRL_CR4_SHADOW, cr4_shadow)?;
    wvmcs(id, VMCS_GUEST_CR3, cr3)?;
    wvmcs(id, VMCS_GUEST_RIP, PAYLOAD_GPA)?;
    wvmcs(id, VMCS_GUEST_RSP, STACK_GPA)?;
    wvmcs(id, VMCS_GUEST_RFLAGS, 2)?;
    wvmcs(id, VMCS_GUEST_DR7, 0x400)?;
    wvmcs(id, VMCS_GUEST_IA32_EFER, EFER_LONG_MODE)?;
    // 10.15 rejects VM entry when EFER.LMA is set unless this MSR is native.
    check_hv(unsafe { hv_vcpu_enable_native_msr(id, IA32_KERNEL_GS_BASE, true) })
        .map_err(|e| format!("hv_vcpu_enable_native_msr(KERNEL_GS_BASE): {e}"))?;
    wvmcs(id, VMCS_GUEST_INTERRUPTIBILITY, 0)?;
    wvmcs(id, VMCS_GUEST_DEBUG_EXC, 0)?;
    wvmcs(id, VMCS_GUEST_ACTIVITY_STATE, ACTIVITY_ACTIVE)?;
    Ok(())
}

fn run_loop(id: hv_vcpuid_t, kick: &Kick, pio: &PioBus, ram_bytes: u64) -> Result<(), String> {
    let trace = std::env::var_os("VMM_TRACE_EXITS").is_some_and(|v| v == "1");
    let mut hist = [0u64; 66];
    let mut exits = 0u64;
    let mut want_hlt = false;
    let mut logged_irq = false;
    // hv_vcpu_run reports an EPT fault after the kernel installs the page.
    let mut ept_retry: Option<(u64, u64)> = None;

    loop {
        if exits > MAX_EXITS {
            return Err(format!("stopped after {exits} VM exits"));
        }
        if vmm::terminal::shutdown_requested() && !want_hlt {
            break;
        }
        if want_hlt {
            // Already-latched kick: do not block, and do not sleep.
            if kick.take() {
                host_log(format_args!(
                    "[VMM] kick pending before HLT (no new VM-exit reason)"
                ));
                let _ = std::io::Write::flush(&mut std::io::stderr());
                if vmm::terminal::shutdown_requested() {
                    wvmcs(id, VMCS_GUEST_ACTIVITY_STATE, ACTIVITY_ACTIVE)?;
                    break;
                }
                want_hlt = true;
                continue;
            }
            wvmcs(id, VMCS_GUEST_ACTIVITY_STATE, ACTIVITY_HLT)?;
        }
        wvmcs(id, VMCS_GUEST_VMX_TIMER_VALUE, u64::MAX)?;

        let rc = unsafe { hv_vcpu_run(id) };
        if rc != HV_SUCCESS {
            let reason = read_vmcs(id, VMCS_RO_EXIT_REASON).unwrap_or(0);
            return Err(format!(
                "hv_vcpu_run: {} exit_reason={reason:#x} ({})",
                HvError(rc),
                dump_guest(id)
            ));
        }
        exits += 1;

        let raw = read_vmcs(id, VMCS_RO_EXIT_REASON)?;
        let basic = (raw & 0xffff) as u16;
        let entry_failure = raw & (1 << 31) != 0;
        if basic < 65 {
            hist[basic as usize] += 1;
        } else {
            hist[65] += 1;
        }
        if want_hlt {
            // Clear HLT before inject or shutdown. The next halt rewrites it.
            wvmcs(id, VMCS_GUEST_ACTIVITY_STATE, ACTIVITY_ACTIVE)?;
        }
        let was_hlt = want_hlt;
        want_hlt = false;

        if trace {
            let rip = read_reg(id, HV_X86_RIP).unwrap_or(0);
            host_log(format_args!(
                "[VMM] exit reason {basic} failure={entry_failure} rip={rip:#x}"
            ));
        }

        let kicked = kick.take();
        if kicked {
            // Do not assume this is reason 1. Record whatever the VMCS says.
            host_log(format_args!("[VMM] kick exit reason: {basic}"));
            let _ = std::io::Write::flush(&mut std::io::stderr());
        }

        if entry_failure || basic == VMX_REASON_VMENTRY_GUEST {
            return Err(format!(
                "invalid guest state (reason {basic}): {}",
                dump_guest(id)
            ));
        }
        if basic == VMX_REASON_TRIPLE_FAULT {
            return Err(format!("triple fault: {}", dump_guest(id)));
        }
        if basic == VMX_REASON_EPT_MISCONFIG {
            return Err(format!("EPT misconfiguration: {}", dump_guest(id)));
        }

        match basic {
            VMX_REASON_IO => handle_io(id, pio)?,
            VMX_REASON_HLT => {
                advance_rip(id)?;
                want_hlt = true;
            }
            VMX_REASON_IRQ => {
                if !logged_irq {
                    host_log(format_args!(
                        "[VMM] external interrupt exit reason {basic}; re-enter"
                    ));
                    logged_irq = true;
                }
                if was_hlt {
                    want_hlt = true;
                }
            }
            VMX_REASON_VMX_TIMER => {
                if was_hlt {
                    want_hlt = true;
                }
            }
            VMX_REASON_CPUID => {
                return Err(format!("CPUID exit: {}; stopping vCPU", dump_guest(id)));
            }
            VMX_REASON_EPT_VIOLATION => {
                let gpa = read_vmcs(id, VMCS_GUEST_PHYSICAL_ADDRESS).unwrap_or(0);
                if gpa < ram_bytes {
                    let rip = read_reg(id, HV_X86_RIP).unwrap_or(0);
                    if ept_retry == Some((gpa, rip)) {
                        return Err(format!(
                            "EPT fault on mapped RAM at {gpa:#x} is not MMIO; {}",
                            dump_guest(id)
                        ));
                    }
                    ept_retry = Some((gpa, rip));
                    if was_hlt {
                        want_hlt = true;
                    }
                } else {
                    return Err(format!(
                        "EPT fault on unmapped GPA {gpa:#x}: {}",
                        dump_guest(id)
                    ));
                }
            }
            _ if kicked => {
                if was_hlt {
                    want_hlt = true;
                }
            }
            other => {
                return Err(format!(
                    "unhandled VM-exit reason {other}: {}",
                    dump_guest(id)
                ));
            }
        }

        if vmm::terminal::shutdown_requested() {
            wvmcs(id, VMCS_GUEST_ACTIVITY_STATE, ACTIVITY_ACTIVE)?;
            break;
        }
    }

    if trace {
        let mut parts = Vec::new();
        for (reason, count) in hist.iter().copied().enumerate() {
            if count > 0 {
                parts.push(format!("{reason}:{count}"));
            }
        }
        host_log(format_args!("[VMM] exit histogram {}", parts.join(" ")));
    }
    Ok(())
}

fn handle_io(id: hv_vcpuid_t, pio: &PioBus) -> Result<(), String> {
    let qual = read_vmcs(id, VMCS_RO_EXIT_QUALIFIC)?;
    let size = match qual & 7 {
        0 => 1,
        1 => 2,
        3 => 4,
        other => return Err(format!("I/O size encoding {other}")),
    };
    if qual & (1 << 4) != 0 {
        return Err(format!(
            "string I/O is not implemented (qual {qual:#x}) at {}",
            dump_guest(id)
        ));
    }
    let port = ((qual >> 16) & 0xffff) as u16;
    let is_in = qual & (1 << 3) != 0;
    if is_in {
        let value = pio.read(port, size)?;
        let rax = read_reg(id, HV_X86_RAX)?;
        let mask = match size {
            1 => 0xff,
            2 => 0xffff,
            _ => 0xffff_ffff,
        };
        let new_rax = if size == 4 {
            value & mask
        } else {
            (rax & !mask) | (value & mask)
        };
        wreg(id, HV_X86_RAX, new_rax)?;
    } else {
        let rax = read_reg(id, HV_X86_RAX)?;
        pio.write(port, rax, size)?;
    }
    advance_rip(id)
}

fn advance_rip(id: hv_vcpuid_t) -> Result<(), String> {
    let len = read_vmcs(id, VMCS_RO_VMEXIT_INSTR_LEN)?;
    if len == 0 {
        return Err("VM-exit instruction length is 0".into());
    }
    let rip = read_reg(id, HV_X86_RIP)?;
    wreg(id, HV_X86_RIP, rip.wrapping_add(len))
}

fn dump_guest(id: hv_vcpuid_t) -> String {
    let rip = read_vmcs(id, VMCS_GUEST_RIP).unwrap_or(0);
    let rflags = read_vmcs(id, VMCS_GUEST_RFLAGS).unwrap_or(0);
    let cr0 = read_vmcs(id, VMCS_GUEST_CR0).unwrap_or(0);
    let cr3 = read_vmcs(id, VMCS_GUEST_CR3).unwrap_or(0);
    let cr4 = read_vmcs(id, VMCS_GUEST_CR4).unwrap_or(0);
    let efer = read_vmcs(id, VMCS_GUEST_IA32_EFER).unwrap_or(0);
    let pat = read_vmcs(id, VMCS_GUEST_IA32_PAT).unwrap_or(0);
    let cs_ar = read_vmcs(id, VMCS_GUEST_CS_AR).unwrap_or(0);
    let tr_ar = read_vmcs(id, VMCS_GUEST_TR_AR).unwrap_or(0);
    let entry = read_vmcs(id, VMCS_CTRL_VMENTRY_CONTROLS).unwrap_or(0);
    let activity = read_vmcs(id, VMCS_GUEST_ACTIVITY_STATE).unwrap_or(0);
    let instr = read_vmcs(id, VMCS_RO_INSTR_ERROR).unwrap_or(0);
    format!(
        "rip={rip:#x} rflags={rflags:#x} cr0={cr0:#x} cr3={cr3:#x} cr4={cr4:#x} efer={efer:#x} pat={pat:#x} cs_ar={cs_ar:#x} tr_ar={tr_ar:#x} entry={entry:#x} activity={activity:#x} instr_error={instr:#x}"
    )
}

fn read_cap(field: hv_vmx_capability_t) -> Result<u64, String> {
    let mut value = 0u64;
    check_hv(unsafe { hv_vmx_read_capability(field, &mut value) })
        .map_err(|e| format!("hv_vmx_read_capability({field}): {e}"))?;
    Ok(value)
}

fn read_vmcs(id: hv_vcpuid_t, field: u32) -> Result<u64, String> {
    let mut value = 0u64;
    check_hv(unsafe { hv_vmx_vcpu_read_vmcs(id, field, &mut value) })
        .map_err(|e| format!("vmcs read {field:#x}: {e}"))?;
    Ok(value)
}

fn wvmcs(id: hv_vcpuid_t, field: u32, value: u64) -> Result<(), String> {
    check_hv(unsafe { hv_vmx_vcpu_write_vmcs(id, field, value) })
        .map_err(|e| format!("vmcs write {field:#x} = {value:#x}: {e}"))
}

fn read_reg(id: hv_vcpuid_t, reg: hv_x86_reg_t) -> Result<u64, String> {
    let mut value = 0u64;
    check_hv(unsafe { hv_vcpu_read_register(id, reg, &mut value) })
        .map_err(|e| format!("reg read {reg}: {e}"))?;
    Ok(value)
}

fn wreg(id: hv_vcpuid_t, reg: hv_x86_reg_t, value: u64) -> Result<(), String> {
    check_hv(unsafe { hv_vcpu_write_register(id, reg, value) })
        .map_err(|e| format!("reg write {reg} = {value:#x}: {e}"))
}
