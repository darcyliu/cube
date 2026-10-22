//! vCPU threads. HLT with no deadline blocks in `hv_vcpu_run`.
//! A vCPU waiting for SIPI sits on a condvar: it is outside `hv_vcpu_run`,
//! so `hv_vcpu_interrupt` cannot wake it.

use std::sync::atomic::{AtomicBool, AtomicI8, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use vmm::decode::{self, MmioOp, StoreSrc};
use vmm::devices::lapic::LAPIC_MMIO;
use vmm::gpa::GuestSpace;
use vmm::terminal::host_log;
use vmm::x86::{
    self, CR0_LONG_MODE, CR4_LONG_MODE, CS_AR, CS_SELECTOR, DATA_AR, DATA_LIMIT, DS_SELECTOR,
    EFER_LONG_MODE, GDT_GPA, GDT_LIMIT, PAYLOAD_GPA, STACK_GPA, TR_AR, TR_LIMIT, TR_SELECTOR,
    TSS_GPA, UNUSABLE_AR,
};

use crate::hypervisor::*;
use crate::machine::Machine;

const MAX_EXITS: u64 = 100_000;
const CR_MASK: u64 = 0xffff_ffff;
/// ET | NE. Real mode keeps PE and PG clear; VMX still requires these.
const CR0_REAL: u64 = (1 << 4) | (1 << 5);
const REAL_CODE_AR: u64 = 0x009b;
const REAL_DATA_AR: u64 = 0x0093;
const REAL_LIMIT: u64 = 0xffff;

const PREEMPT_UNKNOWN: i8 = 0;
const PREEMPT_OK: i8 = 1;
const PREEMPT_NO: i8 = -1;
static PREEMPT: AtomicI8 = AtomicI8::new(PREEMPT_UNKNOWN);

pub struct VcpuDone {
    pub index: u32,
    pub exec_ns: Option<u64>,
    pub error: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    WaitInit,
    WaitSipi,
    Running,
}

struct Boot {
    phase: Phase,
    vector: Option<u8>,
    stop: bool,
}

/// Pending kick flag. `lock` is held across `hv_vcpu_interrupt` and destroy.
/// `boot` parks a vCPU that has not been sent SIPI.
pub struct Kick {
    vcpu: AtomicU32,
    ready: AtomicBool,
    pending: AtomicBool,
    in_run: AtomicBool,
    lock: Mutex<()>,
    boot: Mutex<Boot>,
    cv: Condvar,
}

impl Kick {
    pub fn bsp() -> Self {
        Self::with_phase(Phase::Running)
    }

    pub fn ap() -> Self {
        Self::with_phase(Phase::WaitInit)
    }

    fn with_phase(phase: Phase) -> Self {
        Self {
            vcpu: AtomicU32::new(0),
            ready: AtomicBool::new(false),
            pending: AtomicBool::new(false),
            in_run: AtomicBool::new(false),
            lock: Mutex::new(()),
            boot: Mutex::new(Boot {
                phase,
                vector: None,
                stop: false,
            }),
            cv: Condvar::new(),
        }
    }

    fn arm(&self, id: hv_vcpuid_t) {
        self.vcpu.store(id, Ordering::Relaxed);
        self.ready.store(true, Ordering::Release);
    }

    /// `ready` is cleared under the same mutex `signal` holds across
    /// `hv_vcpu_interrupt`, then the vCPU id is destroyed before the guard drops.
    fn disarm_and_destroy(&self, id: hv_vcpuid_t) {
        let _guard = self.lock.lock().unwrap();
        self.ready.store(false, Ordering::Release);
        self.in_run.store(false, Ordering::Release);
        unsafe {
            let _ = hv_vcpu_destroy(id);
        }
    }

    pub fn signal(&self) {
        {
            let _boot = self.boot.lock().unwrap();
            self.cv.notify_all();
        }
        let _guard = self.lock.lock().unwrap();
        self.pending.store(true, Ordering::Release);
        if self.ready.load(Ordering::Acquire) && self.in_run.load(Ordering::Acquire) {
            let id = self.vcpu.load(Ordering::Relaxed);
            unsafe {
                let _ = hv_vcpu_interrupt(&id, 1);
            }
        }
    }

    /// INIT moves WaitInit/WaitSipi to WaitSipi. A running vCPU is left running.
    pub fn init_ipi(&self) {
        let mut boot = self.boot.lock().unwrap();
        if boot.phase != Phase::Running {
            boot.phase = Phase::WaitSipi;
        }
    }

    /// Only the first SIPI after INIT starts the application processor.
    pub fn sipi(&self, vector: u8) {
        let mut boot = self.boot.lock().unwrap();
        if boot.phase != Phase::WaitSipi {
            return;
        }
        boot.phase = Phase::Running;
        boot.vector = Some(vector);
        self.cv.notify_all();
    }

    pub fn request_stop(&self) {
        let mut boot = self.boot.lock().unwrap();
        boot.stop = true;
        self.cv.notify_all();
    }

    fn wait_for_sipi(&self) -> Option<u8> {
        let mut boot = self.boot.lock().unwrap();
        loop {
            if boot.stop {
                return None;
            }
            if let Some(vector) = boot.vector {
                return Some(vector);
            }
            boot = self.cv.wait(boot).unwrap();
        }
    }

    /// False when a kick is already pending: do not enter `hv_vcpu_run`.
    fn begin_run(&self) -> bool {
        let _guard = self.lock.lock().unwrap();
        if self.pending.swap(false, Ordering::AcqRel) {
            false
        } else {
            self.in_run.store(true, Ordering::Release);
            true
        }
    }

    fn end_run(&self) {
        let _guard = self.lock.lock().unwrap();
        self.in_run.store(false, Ordering::Release);
    }

    /// A kick landed after accept and before `hv_vcpu_run`. Drop `in_run`
    /// so the next `signal` stays a pending bit instead of a lost interrupt.
    fn abort_if_pending(&self) -> bool {
        let _guard = self.lock.lock().unwrap();
        if self.pending.swap(false, Ordering::AcqRel) {
            self.in_run.store(false, Ordering::Release);
            true
        } else {
            false
        }
    }

    fn take(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }
}

struct TimerWatch {
    deadline: Mutex<u64>,
    cv: Condvar,
    due: AtomicBool,
    stop: AtomicBool,
}

impl TimerWatch {
    fn new() -> Self {
        Self {
            deadline: Mutex::new(0),
            cv: Condvar::new(),
            due: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        }
    }

    fn arm(&self, deadline: u64) {
        let mut guard = self.deadline.lock().unwrap();
        *guard = deadline;
        self.cv.notify_all();
    }
}

struct TimerJoin {
    watch: Arc<TimerWatch>,
    thread: Option<thread::JoinHandle<()>>,
}

impl TimerJoin {
    fn new(watch: Arc<TimerWatch>) -> Self {
        Self {
            watch,
            thread: None,
        }
    }

    fn ensure(&mut self, kick: &Arc<Kick>) -> Result<(), String> {
        if self.thread.is_some() {
            return Ok(());
        }
        let watch = self.watch.clone();
        let kick = kick.clone();
        self.thread = Some(
            thread::Builder::new()
                .name("vmm-lapic-timer".into())
                .spawn(move || timer_thread(watch, kick))
                .map_err(|e| format!("spawn lapic timer thread: {e}"))?,
        );
        Ok(())
    }

    fn join(&mut self) {
        self.watch.stop.store(true, Ordering::Release);
        self.watch.cv.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for TimerJoin {
    fn drop(&mut self) {
        self.join();
    }
}

struct ExitInfo {
    basic: u16,
    entry_failure: bool,
    with_deadline: bool,
}

enum Attempt {
    Ran(ExitInfo),
    Aborted,
}

pub fn run(cpu: usize, kick: Arc<Kick>, machine: Arc<Machine>, cr3: u64) -> VcpuDone {
    let mut done = VcpuDone {
        index: cpu as u32,
        exec_ns: None,
        error: None,
    };
    let mut id: hv_vcpuid_t = 0;
    if let Err(e) = check_hv(unsafe { hv_vcpu_create(&mut id, HV_VCPU_DEFAULT) }) {
        done.error = Some(format!("vCPU {cpu}: hv_vcpu_create: {e}"));
        machine.stop_cpus();
        return done;
    }
    kick.arm(id);
    let watch = Arc::new(TimerWatch::new());
    let mut timers = TimerJoin::new(watch.clone());
    if let Err(e) = setup_and_loop(cpu, id, &kick, &machine, cr3, &watch, &mut timers) {
        done.error = Some(format!("vCPU {cpu}: {e}"));
        machine.stop_cpus();
    }
    // The timer thread calls `hv_vcpu_interrupt`. Join it before destroy.
    timers.join();
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
    kick.disarm_and_destroy(id);
    done
}

fn setup_and_loop(
    cpu: usize,
    id: hv_vcpuid_t,
    kick: &Arc<Kick>,
    machine: &Arc<Machine>,
    cr3: u64,
    watch: &Arc<TimerWatch>,
    timers: &mut TimerJoin,
) -> Result<(), String> {
    let cpu_based = write_controls(id)?;
    if cpu == 0 {
        plant_long_mode(id, cr3)?;
    }
    map_apic(id);
    if cpu != 0 {
        let Some(vector) = kick.wait_for_sipi() else {
            return Ok(());
        };
        plant_real_mode(id, cr3, vector)?;
    }
    run_loop(cpu, id, kick, machine, cpu_based, watch, timers)
}

fn map_apic(id: hv_vcpuid_t) {
    let rc = unsafe { hv_vmx_vcpu_set_apic_address(id, LAPIC_MMIO) };
    if rc != HV_SUCCESS {
        host_log(format_args!(
            "[VMM] hv_vmx_vcpu_set_apic_address: {} (EPT path)",
            HvError(rc)
        ));
        let _ = std::io::Write::flush(&mut std::io::stderr());
    }
}

fn write_controls(id: hv_vcpuid_t) -> Result<u64, String> {
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
    // address fails VM-entry.
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

    let entry = x86::vmx_cap_control(
        entry_cap,
        VMENTRY_GUEST_IA32E | VMENTRY_LOAD_EFER | VMENTRY_LOAD_IA32_PAT,
    );
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

    let exitc = x86::vmx_cap_control(
        exit_cap,
        VMEXIT_SAVE_EFER | VMEXIT_LOAD_EFER | VMEXIT_SAVE_IA32_PAT | VMEXIT_LOAD_IA32_PAT,
    );
    wvmcs(id, VMCS_CTRL_VMEXIT_CONTROLS, exitc)?;

    wvmcs(id, VMCS_CTRL_EXC_BITMAP, 0)?;
    wvmcs(id, VMCS_CTRL_CR3_COUNT, 0)?;
    wvmcs(id, VMCS_CTRL_CR0_MASK, CR_MASK)?;
    wvmcs(id, VMCS_CTRL_CR4_MASK, CR_MASK)?;
    // A zero preemption-timer value exits at once. `hv_vcpu_run` has no deadline.
    wvmcs(id, VMCS_GUEST_VMX_TIMER_VALUE, u64::MAX)?;
    Ok(cpu)
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
    enable_kernel_gs(id)?;
    wvmcs(id, VMCS_GUEST_INTERRUPTIBILITY, 0)?;
    wvmcs(id, VMCS_GUEST_DEBUG_EXC, 0)?;
    wvmcs(id, VMCS_GUEST_ACTIVITY_STATE, ACTIVITY_ACTIVE)?;
    Ok(())
}

fn plant_real_mode(id: hv_vcpuid_t, cr3: u64, vector: u8) -> Result<(), String> {
    let entry = read_vmcs(id, VMCS_CTRL_VMENTRY_CONTROLS)?;
    wvmcs(id, VMCS_CTRL_VMENTRY_CONTROLS, entry & !VMENTRY_GUEST_IA32E)?;
    wvmcs(id, VMCS_GUEST_IA32_EFER, 0)?;

    let base = u64::from(vector) << 12;
    let sel = u64::from(vector) << 8;
    wreg(id, HV_X86_CS, sel)?;
    wvmcs(id, VMCS_GUEST_CS, sel)?;
    wvmcs(id, VMCS_GUEST_CS_BASE, base)?;
    wvmcs(id, VMCS_GUEST_CS_LIMIT, REAL_LIMIT)?;
    wvmcs(id, VMCS_GUEST_CS_AR, REAL_CODE_AR)?;

    for (reg, vmcs_sel, limit, ar, seg_base) in [
        (
            HV_X86_SS,
            VMCS_GUEST_SS,
            VMCS_GUEST_SS_LIMIT,
            VMCS_GUEST_SS_AR,
            VMCS_GUEST_SS_BASE,
        ),
        (
            HV_X86_DS,
            VMCS_GUEST_DS,
            VMCS_GUEST_DS_LIMIT,
            VMCS_GUEST_DS_AR,
            VMCS_GUEST_DS_BASE,
        ),
        (
            HV_X86_ES,
            VMCS_GUEST_ES,
            VMCS_GUEST_ES_LIMIT,
            VMCS_GUEST_ES_AR,
            VMCS_GUEST_ES_BASE,
        ),
        (
            HV_X86_FS,
            VMCS_GUEST_FS,
            VMCS_GUEST_FS_LIMIT,
            VMCS_GUEST_FS_AR,
            VMCS_GUEST_FS_BASE,
        ),
        (
            HV_X86_GS,
            VMCS_GUEST_GS,
            VMCS_GUEST_GS_LIMIT,
            VMCS_GUEST_GS_AR,
            VMCS_GUEST_GS_BASE,
        ),
    ] {
        wreg(id, reg, 0)?;
        wvmcs(id, vmcs_sel, 0)?;
        wvmcs(id, limit, REAL_LIMIT)?;
        wvmcs(id, ar, REAL_DATA_AR)?;
        wvmcs(id, seg_base, 0)?;
    }

    // Usable 32-bit busy TSS. Unrestricted real-mode entry rejects an unusable TR.
    wreg(id, HV_X86_TR, 0)?;
    wvmcs(id, VMCS_GUEST_TR, 0)?;
    wvmcs(id, VMCS_GUEST_TR_BASE, 0)?;
    wvmcs(id, VMCS_GUEST_TR_LIMIT, TR_LIMIT)?;
    wvmcs(id, VMCS_GUEST_TR_AR, TR_AR)?;
    wreg(id, HV_X86_LDTR, 0)?;
    wvmcs(id, VMCS_GUEST_LDTR, 0)?;
    wvmcs(id, VMCS_GUEST_LDTR_BASE, 0)?;
    wvmcs(id, VMCS_GUEST_LDTR_LIMIT, 0)?;
    wvmcs(id, VMCS_GUEST_LDTR_AR, UNUSABLE_AR)?;

    let (cr0, cr0_shadow) = x86::publish_cr(CR0_REAL);
    let (cr4_visible, cr4_shadow) = x86::publish_cr(0);
    let cr4 = cr4_visible | (1 << 13);
    wreg(id, HV_X86_CR0, cr0)?;
    wreg(id, HV_X86_CR3, cr3)?;
    wreg(id, HV_X86_CR4, cr4)?;
    wreg(id, HV_X86_RIP, 0)?;
    wreg(id, HV_X86_RSP, 0)?;
    wreg(id, HV_X86_RFLAGS, 2)?;
    wreg(id, HV_X86_XCR0, 1)?;
    wreg(id, HV_X86_DR7, 0x400)?;
    wvmcs(id, VMCS_GUEST_CR0, cr0)?;
    wvmcs(id, VMCS_CTRL_CR0_SHADOW, cr0_shadow)?;
    wvmcs(id, VMCS_GUEST_CR4, cr4)?;
    wvmcs(id, VMCS_CTRL_CR4_SHADOW, cr4_shadow)?;
    wvmcs(id, VMCS_GUEST_CR3, cr3)?;
    wvmcs(id, VMCS_GUEST_RIP, 0)?;
    wvmcs(id, VMCS_GUEST_RSP, 0)?;
    wvmcs(id, VMCS_GUEST_RFLAGS, 2)?;
    wvmcs(id, VMCS_GUEST_DR7, 0x400)?;
    enable_kernel_gs(id)?;
    wvmcs(id, VMCS_GUEST_INTERRUPTIBILITY, 0)?;
    wvmcs(id, VMCS_GUEST_DEBUG_EXC, 0)?;
    wvmcs(id, VMCS_GUEST_ACTIVITY_STATE, ACTIVITY_ACTIVE)?;
    Ok(())
}

fn enable_kernel_gs(id: hv_vcpuid_t) -> Result<(), String> {
    // 10.15 rejects VM entry when EFER.LMA is set unless this MSR is native.
    check_hv(unsafe { hv_vcpu_enable_native_msr(id, IA32_KERNEL_GS_BASE, true) })
        .map_err(|e| format!("hv_vcpu_enable_native_msr(KERNEL_GS_BASE): {e}"))
}

fn run_loop(
    cpu: usize,
    id: hv_vcpuid_t,
    kick: &Arc<Kick>,
    machine: &Arc<Machine>,
    cpu_based: u64,
    watch: &Arc<TimerWatch>,
    timers: &mut TimerJoin,
) -> Result<(), String> {
    let trace = std::env::var_os("VMM_TRACE_EXITS").is_some_and(|v| v == "1");
    let mut hist = [0u64; 66];
    let mut exits = 0u64;
    let mut want_hlt = false;
    let mut logged_irq = false;
    let mut timer_exit = false;
    let mut ept_retry: Option<(u64, u64)> = None;
    // Event that was mid-delivery when the last exit happened.
    let mut reinject: Option<u64> = None;
    let mut preload = None;
    if cpu == 0 {
        preload = probe_preemption(id)?;
        if PREEMPT.load(Ordering::Acquire) == PREEMPT_NO {
            timers.ensure(kick)?;
        }
    }
    // Backstop after create. Not the SIPI wake: an AP is still outside `hv_vcpu_run`
    // until this loop, and the interrupt does not leave that park.
    unsafe {
        let _ = hv_vcpu_interrupt(&id, 1);
    }

    loop {
        if exits > MAX_EXITS {
            return Err(format!("stopped after {exits} VM exits"));
        }
        if vmm::terminal::shutdown_requested() && !want_hlt {
            break;
        }
        if watch.due.swap(false, Ordering::AcqRel) || timer_exit {
            machine.expire_timer(cpu);
            timer_exit = false;
        }
        let remain = machine.timer_remaining_ns(cpu);

        let exit = if let Some(exit) = preload.take() {
            exit
        } else {
            let pending_irq = machine.has_deliverable(cpu);
            let (blocked, idt_busy) = irq_blocks(id)?;
            let injectable = reinject.is_none() && pending_irq && !blocked;
            // IRQ-window exiting while halted does not run guest instructions.
            let window = reinject.is_none() && pending_irq && !injectable && !want_hlt && !idt_busy;
            set_irq_window(id, cpu_based, window)?;
            let activity = if reinject.is_some() || !want_hlt || injectable {
                ACTIVITY_ACTIVE
            } else {
                ACTIVITY_HLT
            };
            wvmcs(id, VMCS_GUEST_ACTIVITY_STATE, activity)?;
            if let Some(info) = reinject {
                // EPT fault mid-delivery. The IDT-vectoring field does not
                // survive the next hv_vcpu_run, so arm VM-entry with it.
                // Both valid bits set fails VM-entry; the field is read-only
                // on real hardware and this write is a no-op there.
                unsafe {
                    let _ = hv_vmx_vcpu_write_vmcs(id, VMCS_RO_IDT_VECTOR_INFO, 0);
                }
                if info & (1 << 11) != 0 {
                    let err = read_vmcs(id, VMCS_RO_IDT_VECTOR_ERROR)?;
                    wvmcs(id, VMCS_CTRL_VMENTRY_EXC_ERROR, err)?;
                }
                wvmcs(id, VMCS_CTRL_VMENTRY_IRQ_INFO, info)?;
            } else if !injectable {
                wvmcs(id, VMCS_CTRL_VMENTRY_IRQ_INFO, 0)?;
            }
            if !kick.begin_run() {
                if vmm::terminal::shutdown_requested() {
                    break;
                }
                continue;
            }
            let attempt = (|| -> Result<Attempt, String> {
                if injectable {
                    if let Some(vector) = machine.accept_irq(cpu) {
                        if kick.abort_if_pending() {
                            machine.requeue_irq(cpu, vector);
                            return Ok(Attempt::Aborted);
                        }
                        // External interrupt, valid. Delivered on this VM-entry.
                        wvmcs(
                            id,
                            VMCS_CTRL_VMENTRY_IRQ_INFO,
                            u64::from(vector) | IRQ_INFO_VALID,
                        )?;
                    } else {
                        wvmcs(id, VMCS_CTRL_VMENTRY_IRQ_INFO, 0)?;
                    }
                }
                Ok(Attempt::Ran(exec_guest(id, remain, timers, kick)?))
            })();
            kick.end_run();
            match attempt? {
                Attempt::Aborted => {
                    if vmm::terminal::shutdown_requested() {
                        break;
                    }
                    continue;
                }
                Attempt::Ran(exit) => exit,
            }
        };
        exits += 1;

        let raw_basic = exit.basic;
        let entry_failure = exit.entry_failure;
        if raw_basic < 65 {
            hist[raw_basic as usize] += 1;
        } else {
            hist[65] += 1;
        }
        if want_hlt {
            wvmcs(id, VMCS_GUEST_ACTIVITY_STATE, ACTIVITY_ACTIVE)?;
        }
        let was_hlt = want_hlt;
        want_hlt = false;

        let idt_info = read_vmcs(id, VMCS_RO_IDT_VECTOR_INFO)?;
        if idt_info & IRQ_INFO_VALID != 0 {
            reinject = Some(idt_info);
        } else {
            reinject = None;
        }

        if trace {
            let rip = read_reg(id, HV_X86_RIP).unwrap_or(0);
            host_log(format_args!(
                "[VMM] exit reason {raw_basic} failure={entry_failure} rip={rip:#x}"
            ));
        }

        let kicked = kick.take();
        if kicked {
            host_log(format_args!("[VMM] kick exit reason: {raw_basic}"));
            let _ = std::io::Write::flush(&mut std::io::stderr());
        }

        if entry_failure || raw_basic == VMX_REASON_VMENTRY_GUEST {
            return Err(format!(
                "invalid guest state (reason {raw_basic}): {}",
                dump_guest(id)
            ));
        }
        if raw_basic == VMX_REASON_TRIPLE_FAULT {
            return Err(format!("triple fault: {}", dump_guest(id)));
        }
        if raw_basic == VMX_REASON_EPT_MISCONFIG {
            return Err(format!("EPT misconfiguration: {}", dump_guest(id)));
        }
        if raw_basic == VMX_REASON_VMX_TIMER {
            // Only a deadline we armed is a LAPIC expiry. A leftover 0 is not.
            timer_exit = exit.with_deadline;
        }

        match raw_basic {
            VMX_REASON_IO => handle_io(id, machine)?,
            VMX_REASON_HLT => {
                advance_rip(id)?;
                // HLT has completed, so STI/MOV-SS blocking no longer applies.
                let bits = read_vmcs(id, VMCS_GUEST_INTERRUPTIBILITY)?;
                if bits & 0b11 != 0 {
                    wvmcs(id, VMCS_GUEST_INTERRUPTIBILITY, bits & !0b11)?;
                }
                want_hlt = true;
            }
            VMX_REASON_IRQ => {
                if !logged_irq {
                    host_log(format_args!(
                        "[VMM] external interrupt exit reason {raw_basic}; re-enter"
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
            VMX_REASON_IRQ_WND => {
                want_hlt = false;
            }
            VMX_REASON_CPUID => {
                return Err(format!("CPUID exit: {}; stopping vCPU", dump_guest(id)));
            }
            VMX_REASON_APIC_ACCESS => {
                let qual = read_vmcs(id, VMCS_RO_EXIT_QUALIFIC)?;
                let gpa = LAPIC_MMIO + (qual & 0xfff);
                emulate_mmio(id, machine, cpu, gpa)?;
            }
            VMX_REASON_EPT_VIOLATION => {
                handle_ept(id, machine, cpu, &mut ept_retry, &mut want_hlt, was_hlt)?;
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

fn probe_preemption(id: hv_vcpuid_t) -> Result<Option<ExitInfo>, String> {
    let rip_before = read_reg(id, HV_X86_RIP)?;
    let rc = unsafe { hv_vcpu_run_until(id, 0) };
    if rc == HV_UNSUPPORTED {
        PREEMPT.store(PREEMPT_NO, Ordering::Release);
        host_log(format_args!(
            "[VMM] hv_vcpu_run_until: HV_UNSUPPORTED, timer thread fallback"
        ));
        let _ = std::io::Write::flush(&mut std::io::stderr());
        // A failed probe can leave the preemption timer at 0, which storms reason 52.
        wvmcs(id, VMCS_GUEST_VMX_TIMER_VALUE, u64::MAX)?;
        return Ok(None);
    }
    if rc != HV_SUCCESS {
        return Err(format!("hv_vcpu_run_until: {}", HvError(rc)));
    }
    let exit = read_exit(id, false)?;
    let rip = read_reg(id, HV_X86_RIP)?;
    if exit.basic == VMX_REASON_VMX_TIMER && rip == rip_before {
        PREEMPT.store(PREEMPT_OK, Ordering::Release);
        host_log(format_args!("[VMM] hv_vcpu_run_until: ok"));
        let _ = std::io::Write::flush(&mut std::io::stderr());
        wvmcs(id, VMCS_GUEST_VMX_TIMER_VALUE, u64::MAX)?;
        return Ok(None);
    }
    PREEMPT.store(PREEMPT_OK, Ordering::Release);
    host_log(format_args!(
        "[VMM] hv_vcpu_run_until: ok exit {} rip {rip:#x}",
        exit.basic
    ));
    let _ = std::io::Write::flush(&mut std::io::stderr());
    Ok(Some(exit))
}

fn exec_guest(
    id: hv_vcpuid_t,
    remain: Option<u64>,
    timers: &mut TimerJoin,
    kick: &Arc<Kick>,
) -> Result<ExitInfo, String> {
    let watch = Arc::clone(&timers.watch);
    let mode = PREEMPT.load(Ordering::Acquire);
    if let Some(ns) = remain
        && mode != PREEMPT_NO
    {
        let deadline = mach_now().saturating_add(ns_to_abs(ns));
        let rc = unsafe { hv_vcpu_run_until(id, deadline) };
        if rc == HV_SUCCESS {
            if mode == PREEMPT_UNKNOWN {
                PREEMPT.store(PREEMPT_OK, Ordering::Release);
            }
            return read_exit(id, true);
        }
        if rc != HV_UNSUPPORTED {
            return Err(format!(
                "hv_vcpu_run_until: {} ({})",
                HvError(rc),
                dump_guest(id)
            ));
        }
        note_unsupported();
        timers.ensure(kick)?;
        watch.arm(deadline);
    } else if let Some(ns) = remain {
        timers.ensure(kick)?;
        watch.arm(mach_now().saturating_add(ns_to_abs(ns)));
    } else {
        watch.arm(0);
    }
    // Do not leave a 0 from `run_until` in the preemption timer.
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
    read_exit(id, remain.is_some())
}

fn note_unsupported() {
    if PREEMPT.swap(PREEMPT_NO, Ordering::Release) != PREEMPT_NO {
        host_log(format_args!(
            "[VMM] hv_vcpu_run_until: HV_UNSUPPORTED, timer thread fallback"
        ));
        let _ = std::io::Write::flush(&mut std::io::stderr());
    }
}

fn read_exit(id: hv_vcpuid_t, with_deadline: bool) -> Result<ExitInfo, String> {
    let raw = read_vmcs(id, VMCS_RO_EXIT_REASON)?;
    Ok(ExitInfo {
        basic: (raw & 0xffff) as u16,
        entry_failure: raw & (1 << 31) != 0,
        with_deadline,
    })
}

fn irq_blocks(id: hv_vcpuid_t) -> Result<(bool, bool), String> {
    let rflags = read_vmcs(id, VMCS_GUEST_RFLAGS)?;
    let intr = read_vmcs(id, VMCS_GUEST_INTERRUPTIBILITY)?;
    let idt = read_vmcs(id, VMCS_RO_IDT_VECTOR_INFO)?;
    let idt_busy = idt & IRQ_INFO_VALID != 0;
    let blocked = rflags & (1 << 9) == 0 || intr & 0b11 != 0 || idt_busy;
    Ok((blocked, idt_busy))
}

fn set_irq_window(id: hv_vcpuid_t, cpu_based: u64, on: bool) -> Result<(), String> {
    let value = if on {
        cpu_based | CPU_BASED_IRQ_WND
    } else {
        cpu_based & !CPU_BASED_IRQ_WND
    };
    wvmcs(id, VMCS_CTRL_CPU_BASED, value)
}

fn handle_ept(
    id: hv_vcpuid_t,
    machine: &Machine,
    cpu: usize,
    ept_retry: &mut Option<(u64, u64)>,
    want_hlt: &mut bool,
    was_hlt: bool,
) -> Result<(), String> {
    let gpa = read_vmcs(id, VMCS_GUEST_PHYSICAL_ADDRESS)?;
    let qual = read_vmcs(id, VMCS_RO_EXIT_QUALIFIC)?;
    let ram = machine.space.ram().size() as u64;
    if gpa < ram {
        let rip = read_reg(id, HV_X86_RIP)?;
        if *ept_retry == Some((gpa, rip)) {
            return Err(format!(
                "EPT fault on mapped RAM at {gpa:#x} is not MMIO; {}",
                dump_guest(id)
            ));
        }
        *ept_retry = Some((gpa, rip));
        if was_hlt {
            *want_hlt = true;
        }
        return Ok(());
    }
    // The code flash is not mapped. Writes are ignored; reads cannot be filled.
    if machine.space.code_window_contains(gpa, 1) && qual & 2 != 0 {
        return advance_rip(id);
    }
    emulate_mmio(id, machine, cpu, gpa)
}

fn emulate_mmio(id: hv_vcpuid_t, machine: &Machine, cpu: usize, gpa: u64) -> Result<(), String> {
    let bytes = fetch_insn(id, &machine.space)?;
    let decoded = decode::decode_mmio(&bytes).map_err(|e| {
        format!(
            "MMIO at {gpa:#x} bytes {}: {e}; {}",
            hex_bytes(&bytes),
            dump_guest(id)
        )
    })?;
    let vmcs_len = read_vmcs(id, VMCS_RO_VMEXIT_INSTR_LEN)?;
    let len = if vmcs_len != 0 {
        vmcs_len
    } else {
        decoded.len as u64
    };
    match decoded.op {
        MmioOp::Load { dest } => {
            if dest.width != 4 {
                return Err(format!("MMIO read width {} at {gpa:#x}", dest.width));
            }
            let value = machine.mmio_read(cpu, gpa, 4)?;
            wreg(id, hv_gpr(dest.index)?, value & 0xffff_ffff)?;
        }
        MmioOp::Store { src } => {
            let value = match src {
                StoreSrc::Reg(reg) => {
                    if reg.width != 4 {
                        return Err(format!("MMIO write width {} at {gpa:#x}", reg.width));
                    }
                    read_reg(id, hv_gpr(reg.index)?)? & 0xffff_ffff
                }
                StoreSrc::Imm { value, width } => {
                    if width != 4 {
                        return Err(format!("MMIO write width {width} at {gpa:#x}"));
                    }
                    value
                }
            };
            machine.mmio_write(cpu, gpa, value, 4)?;
        }
    }
    advance_rip_by(id, len)
}

fn fetch_insn(id: hv_vcpuid_t, space: &GuestSpace) -> Result<Vec<u8>, String> {
    let rip = read_reg(id, HV_X86_RIP)?;
    let mut bytes = Vec::with_capacity(15);
    for i in 0..15u64 {
        match guest_byte(id, space, rip, i) {
            Ok(byte) => bytes.push(byte),
            Err(_) if !bytes.is_empty() => break,
            Err(e) => return Err(e),
        }
    }
    if bytes.is_empty() {
        return Err(format!("cannot fetch instruction at rip {rip:#x}"));
    }
    Ok(bytes)
}

fn guest_byte(id: hv_vcpuid_t, space: &GuestSpace, rip: u64, addend: u64) -> Result<u8, String> {
    let cr0 = read_vmcs(id, VMCS_GUEST_CR0)?;
    let gpa = if cr0 & (1 << 31) != 0 {
        let cr3 = read_reg(id, HV_X86_CR3)?;
        translate_4k(space, cr3, rip.wrapping_add(addend))?
    } else {
        let base = read_vmcs(id, VMCS_GUEST_CS_BASE)?;
        base.wrapping_add(rip.wrapping_add(addend) & 0xffff)
    };
    let mut buf = [0u8; 1];
    space.read_gpa(gpa, &mut buf)?;
    Ok(buf[0])
}

fn translate_4k(space: &GuestSpace, cr3: u64, va: u64) -> Result<u64, String> {
    let mut table = cr3 & 0x000f_ffff_ffff_f000;
    let shifts = [39u32, 30, 21, 12];
    for (level, shift) in shifts.iter().copied().enumerate() {
        let idx = (va >> shift) & 0x1ff;
        let mut buf = [0u8; 8];
        space
            .read_gpa(table + idx * 8, &mut buf)
            .map_err(|e| format!("page walk {va:#x} level {level}: {e}"))?;
        let entry = u64::from_le_bytes(buf);
        if entry & 1 == 0 {
            return Err(format!("not-present page at {va:#x} level {level}"));
        }
        let addr = entry & 0x000f_ffff_ffff_f000;
        if entry & (1 << 7) != 0 && level == 1 {
            return Ok((addr & !0x3fff_ffff) | (va & 0x3fff_ffff));
        }
        if entry & (1 << 7) != 0 && level == 2 {
            return Ok((addr & !0x1f_ffff) | (va & 0x1f_ffff));
        }
        table = addr;
    }
    Ok(table | (va & 0xfff))
}

fn hv_gpr(index: u8) -> Result<hv_x86_reg_t, String> {
    Ok(match index {
        0 => HV_X86_RAX,
        1 => HV_X86_RCX,
        2 => HV_X86_RDX,
        3 => HV_X86_RBX,
        4 => HV_X86_RSP,
        5 => HV_X86_RBP,
        6 => HV_X86_RSI,
        7 => HV_X86_RDI,
        8..=15 => HV_X86_R8 + i32::from(index - 8),
        _ => return Err(format!("GPR {index} is not addressable")),
    })
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn handle_io(id: hv_vcpuid_t, machine: &Machine) -> Result<(), String> {
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
        let value = machine.pio.read(port, size)?;
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
        machine.pio.write(port, rax, size)?;
    }
    advance_rip(id)
}

fn advance_rip(id: hv_vcpuid_t) -> Result<(), String> {
    let len = read_vmcs(id, VMCS_RO_VMEXIT_INSTR_LEN)?;
    advance_rip_by(id, len)
}

fn advance_rip_by(id: hv_vcpuid_t, len: u64) -> Result<(), String> {
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

fn timer_thread(watch: Arc<TimerWatch>, kick: Arc<Kick>) {
    loop {
        let guard = watch.deadline.lock().unwrap();
        if watch.stop.load(Ordering::Acquire) {
            break;
        }
        let target = *guard;
        if target == 0 {
            let _guard = watch.cv.wait(guard).unwrap();
            continue;
        }
        let now = mach_now();
        if now >= target {
            drop(guard);
            let mut guard = watch.deadline.lock().unwrap();
            if *guard == target {
                *guard = 0;
                watch.due.store(true, Ordering::Release);
                drop(guard);
                kick.signal();
            }
            continue;
        }
        let wait_ns = abs_to_ns(target - now).max(1);
        let (_guard, _timeout) = watch
            .cv
            .wait_timeout(guard, Duration::from_nanos(wait_ns))
            .unwrap();
    }
}

fn mach_now() -> u64 {
    unsafe { mach_absolute_time() }
}

fn timebase() -> (u64, u64) {
    static TB: std::sync::OnceLock<(u64, u64)> = std::sync::OnceLock::new();
    *TB.get_or_init(|| {
        let mut info = MachTimebaseInfo { numer: 1, denom: 1 };
        unsafe {
            mach_timebase_info(&mut info);
        }
        (u64::from(info.numer.max(1)), u64::from(info.denom.max(1)))
    })
}

fn ns_to_abs(ns: u64) -> u64 {
    let (numer, denom) = timebase();
    ns.saturating_mul(denom) / numer
}

fn abs_to_ns(abs: u64) -> u64 {
    let (numer, denom) = timebase();
    abs.saturating_mul(numer) / denom
}

#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_absolute_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}
