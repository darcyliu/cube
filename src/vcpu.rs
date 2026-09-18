//! vCPU management and execution loop for ARM64.

#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread;
use std::time::Duration;

use crate::devices::MmioBus;
use crate::hypervisor::*;

static VCPU_ID: AtomicU64 = AtomicU64::new(0);
static GLOBAL_VCPU_MANAGER: Mutex<Option<Weak<VcpuManager>>> = Mutex::new(None);

/// Parameters passed to a secondary vCPU when booted via PSCI CPU_ON
#[derive(Debug, Clone, Copy)]
pub struct SecondaryBootArgs {
    pub entry_point: u64,
    pub context_id: u64,
}

/// Slot representing a secondary vCPU waiting for PSCI CPU_ON release
pub struct SecondaryVcpuSlot {
    pub vcpu_index: usize,
    pub mpidr: u64,
    pub condvar: Condvar,
    pub boot_args: Mutex<Option<SecondaryBootArgs>>,
    pub is_online: AtomicBool,
}

/// Multi-vCPU manager coordinating secondary core boot, shutdown, and interrupt kicking.
pub struct VcpuManager {
    num_cpus: usize,
    slots: Vec<Arc<SecondaryVcpuSlot>>,
    active_vcpu_ids: Mutex<Vec<u64>>,
    shutdown_requested: AtomicBool,
    /// Generation counter + condvar used to wake vCPUs that trapped WFI.
    /// `hv_vcpus_exit` only interrupts `hv_vcpu_run`; it does not wake `thread::sleep`.
    wfi_gen: Mutex<u64>,
    wfi_cv: Condvar,
}

impl VcpuManager {
    pub fn new(num_cpus: usize) -> Arc<Self> {
        let count = num_cpus.max(1);
        let mut slots = Vec::new();
        for i in 1..count {
            slots.push(Arc::new(SecondaryVcpuSlot {
                vcpu_index: i,
                mpidr: 0x8000_0000 | (i as u64 & 0xff),
                condvar: Condvar::new(),
                boot_args: Mutex::new(None),
                is_online: AtomicBool::new(false),
            }));
        }

        let mgr = Arc::new(Self {
            num_cpus: count,
            slots,
            active_vcpu_ids: Mutex::new(Vec::new()),
            shutdown_requested: AtomicBool::new(false),
            wfi_gen: Mutex::new(0),
            wfi_cv: Condvar::new(),
        });

        let mut global = GLOBAL_VCPU_MANAGER.lock().unwrap();
        *global = Some(Arc::downgrade(&mgr));

        mgr
    }

    pub fn num_cpus(&self) -> usize {
        self.num_cpus
    }

    pub fn slots(&self) -> &[Arc<SecondaryVcpuSlot>] {
        &self.slots
    }

    pub fn register_vcpu_id(&self, id: u64) {
        let mut ids = self.active_vcpu_ids.lock().unwrap();
        if !ids.contains(&id) {
            ids.push(id);
        }
    }

    pub fn unregister_vcpu_id(&self, id: u64) {
        let mut ids = self.active_vcpu_ids.lock().unwrap();
        ids.retain(|&x| x != id);
    }

    pub fn kick_all(&self) {
        {
            let mut generation = self.wfi_gen.lock().unwrap();
            *generation = generation.wrapping_add(1);
            self.wfi_cv.notify_all();
        }
        let ids = self.active_vcpu_ids.lock().unwrap();
        if !ids.is_empty() {
            unsafe {
                let _ = hv_vcpus_exit(ids.as_ptr(), ids.len() as u32);
            }
        }
    }

    fn wfi_generation(&self) -> u64 {
        *self.wfi_gen.lock().unwrap()
    }

    /// Park a vCPU that trapped WFI until a device IRQ kick or a short timeout.
    ///
    /// `observed_gen` is the generation sampled before parking. If a kick already
    /// happened, return immediately. The timeout keeps the ARM generic timer
    /// ticking even if no SPI arrives; the condvar makes UART/virtio kicks
    /// (including Ctrl-C) wake the guest immediately.
    fn wait_for_wfi_wake(&self, observed_gen: u64) {
        let guard = self.wfi_gen.lock().unwrap();
        if *guard != observed_gen {
            return;
        }
        let _ = self.wfi_cv.wait_timeout(guard, Duration::from_micros(50));
    }

    pub fn get_slot_by_mpidr(&self, mpidr: u64) -> Option<Arc<SecondaryVcpuSlot>> {
        let target_aff = mpidr & 0xff;
        self.slots.iter().find(|s| (s.mpidr & 0xff) == target_aff).cloned()
    }

    pub fn request_shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::Relaxed);
        for slot in &self.slots {
            slot.condvar.notify_all();
        }
        self.kick_all();
    }

    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::Relaxed)
    }
}

/// Forces all running vCPUs to immediately exit hv_vcpu_run so they can process pending interrupts.
pub fn kick_vcpu() {
    let mgr = {
        let global = GLOBAL_VCPU_MANAGER.lock().unwrap();
        global.as_ref().and_then(|weak| weak.upgrade())
    };
    if let Some(mgr) = mgr {
        mgr.kick_all();
        return;
    }
    let id = VCPU_ID.load(Ordering::Relaxed);
    if id != 0 {
        unsafe {
            let _ = hv_vcpus_exit(&id, 1);
        }
    }
}

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
    wfi_halts_vm: bool,
    vcpu_index: usize,
    manager: Option<Arc<VcpuManager>>,
    /// True after HV_EXIT_REASON_VTIMER_ACTIVATED until the guest deasserts CNTV.
    vtimer_masked: bool,
}

impl Vcpu {
    /// Creates and configures a new vCPU for the current thread.
    pub fn new() -> HvResult<Self> {
        Self::with_manager(0, None)
    }

    /// Creates and configures a vCPU associated with a multi-vCPU manager.
    pub fn with_manager(vcpu_index: usize, manager: Option<Arc<VcpuManager>>) -> HvResult<Self> {
        let mut id: u64 = 0;
        let mut exit_ptr: *mut HvVcpuExit = std::ptr::null_mut();

        unsafe {
            check_hv(hv_vcpu_create(&mut id, &mut exit_ptr, std::ptr::null()))?;
        }

        VCPU_ID.store(id, Ordering::Relaxed);
        if let Some(ref mgr) = manager {
            mgr.register_vcpu_id(id);
        }

        Ok(Self {
            id,
            exit_ptr,
            wfi_halts_vm: true,
            vcpu_index,
            manager,
            vtimer_masked: false,
        })
    }

    pub fn set_wfi_halts_vm(&mut self, halt: bool) {
        self.wfi_halts_vm = halt;
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn vcpu_index(&self) -> usize {
        self.vcpu_index
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

    /// Sets initial state for booting at Exception Level 1 (EL1) for baremetal tests.
    pub fn setup_boot_state(&self, entry_point: u64, stack_pointer: u64) -> HvResult<()> {
        self.set_reg(HV_REG_PC, entry_point)?;
        self.set_reg(HV_REG_CPSR, 0x3c5)?; // EL1h, DAIF masked
        self.set_sys_reg(HV_SYS_REG_SP_EL1, stack_pointer)?;
        self.set_sys_reg(HV_SYS_REG_SCTLR_EL1, 0x00000000)?;
        self.set_sys_reg(HV_SYS_REG_CPACR_EL1, 3 << 20)?;
        self.set_sys_reg(HV_SYS_REG_MPIDR_EL1, 0x8000_0000)?;
        Ok(())
    }

    /// Sets initial state for booting an OS kernel (FreeBSD / Linux) according to the ARM64 boot specification:
    /// - X0 = physical address of Device Tree Blob (DTB)
    /// - X1 = 0, X2 = 0, X3 = 0
    /// - PC = kernel entry point
    /// - CPSR = EL1h with DAIF interrupts masked (0x3c5)
    /// - MPIDR_EL1 = affinity value for vCPU
    pub fn setup_os_boot_state(
        &mut self,
        kernel_entry: u64,
        dtb_phys_addr: u64,
        vcpu_index: u64,
    ) -> HvResult<()> {
        self.wfi_halts_vm = false; // Don't terminate on kernel idle WFI

        self.set_reg(HV_REG_PC, kernel_entry)?;
        self.set_reg(HV_REG_X0, dtb_phys_addr)?;
        self.set_reg(HV_REG_X1, 0)?;
        self.set_reg(HV_REG_X2, 0)?;
        self.set_reg(HV_REG_X3, 0)?;

        // Mode EL1h, interrupts masked
        self.set_reg(HV_REG_CPSR, 0x3c5)?;

        // Set affinity required for GICv3 interrupt routing (Aff0 = vcpu_index)
        let mpidr = 0x8000_0000 | (vcpu_index & 0xff);
        self.set_sys_reg(HV_SYS_REG_MPIDR_EL1, mpidr)?;

        // Standard MMU off at boot
        self.set_sys_reg(HV_SYS_REG_SCTLR_EL1, 0x0000_0000)?;

        // Enable FP/NEON
        self.set_sys_reg(HV_SYS_REG_CPACR_EL1, 3 << 20)?;

        Ok(())
    }

    /// Configures state for secondary vCPUs upon PSCI CPU_ON release
    pub fn setup_secondary_boot_state(&mut self, entry_point: u64, context_id: u64) -> HvResult<()> {
        self.wfi_halts_vm = false;
        self.set_reg(HV_REG_PC, entry_point)?;
        self.set_reg(HV_REG_X0, context_id)?;
        self.set_reg(HV_REG_X1, 0)?;
        self.set_reg(HV_REG_X2, 0)?;
        self.set_reg(HV_REG_X3, 0)?;
        self.set_reg(HV_REG_CPSR, 0x3c5)?;
        let mpidr = 0x8000_0000 | (self.vcpu_index as u64 & 0xff);
        self.set_sys_reg(HV_SYS_REG_MPIDR_EL1, mpidr)?;
        self.set_sys_reg(HV_SYS_REG_SCTLR_EL1, 0x0000_0000)?;
        self.set_sys_reg(HV_SYS_REG_CPACR_EL1, 3 << 20)?;
        Ok(())
    }

    /// Runs the vCPU until it exits, dispatching MMIO, PSCI calls, and exceptions.
    pub fn run(&mut self, mmio: &MmioBus) -> Result<VcpuExitStatus, String> {
        loop {
            if let Some(ref mgr) = self.manager {
                if mgr.is_shutdown_requested() {
                    return Ok(VcpuExitStatus::Shutdown);
                }
            }

            self.sync_vtimer();

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
                            if self.wfi_halts_vm {
                                return Ok(VcpuExitStatus::Halted);
                            } else {
                                // Advance past WFI/WFE. ISS bit 0 is 1 for WFE, 0 for WFI.
                                let pc = self.get_reg(HV_REG_PC).map_err(|e| e.to_string())?;
                                self.set_reg(HV_REG_PC, pc + 4).map_err(|e| e.to_string())?;
                                let is_wfe = (syndrome & 1) != 0;
                                if !is_wfe {
                                    if let Some(ref mgr) = self.manager {
                                        let kick_gen = mgr.wfi_generation();
                                        mgr.wait_for_wfi_wake(kick_gen);
                                    } else {
                                        thread::yield_now();
                                    }
                                }
                            }
                        }
                        // EC 0x18: System register access trap
                        0x18 => {
                            self.handle_sysreg_trap(syndrome)?;
                        }
                        // EC 0x20 / 0x21: Instruction Abort
                        0x20 | 0x21 => {
                            let pc = self.get_reg(HV_REG_PC).unwrap_or(0);
                            return Err(format!(
                                "Instruction Abort (EC 0x{:02x}) at PC=0x{:016x}, Fault IPA=0x{:016x}, FAR=0x{:016x}",
                                ec, pc, exit.exception.physical_address, exit.exception.virtual_address
                            ));
                        }
                        _ => {
                            let pc = self.get_reg(HV_REG_PC).unwrap_or(0);
                            return Err(format!(
                                "Unhandled Exception Class EC=0x{:02x} at PC=0x{:016x} (Syndrome=0x{:016x}, FAR=0x{:016x}, IPA=0x{:016x})",
                                ec, pc, syndrome, exit.exception.virtual_address, exit.exception.physical_address
                            ));
                        }
                    }
                }
                HV_EXIT_REASON_CANCELED => {
                    // vCPU was kicked by another thread (hv_vcpus_exit) to process interrupts
                    continue;
                }
                HV_EXIT_REASON_VTIMER_ACTIVATED => {
                    // HVF auto-masks the vtimer. Keep it masked until the guest
                    // clears CNTV ISTATUS / sets IMASK, then unmask so later
                    // ticks can fire. Immediate unmask livelocks hv_vcpu_run.
                    self.vtimer_masked = true;
                    self.sync_vtimer();
                    continue;
                }
                _ => {
                    return Err(format!("Unexpected vCPU exit reason: {}", exit.reason));
                }
            }
        }
    }

    /// Unmask the virtual timer once the guest is no longer asserting it.
    ///
    /// Apple HVF automatically masks CNTV on HV_EXIT_REASON_VTIMER_ACTIVATED.
    /// Until `hv_vcpu_set_vtimer_mask(false)` the guest gets no further vtimer
    /// interrupts, which freezes hardclock, TCP timers, and eventually the VM.
    fn sync_vtimer(&mut self) {
        if !self.vtimer_masked {
            return;
        }
        let ctl = match self.get_sys_reg(HV_SYS_REG_CNTV_CTL_EL0) {
            Ok(v) => v,
            Err(_) => return,
        };
        let asserting = (ctl & (CNTV_CTL_ENABLE | CNTV_CTL_IMASK | CNTV_CTL_ISTATUS))
            == (CNTV_CTL_ENABLE | CNTV_CTL_ISTATUS);
        if !asserting {
            unsafe {
                let _ = hv_vcpu_set_vtimer_mask(self.id, false);
            }
            self.vtimer_masked = false;
        }
    }

    fn handle_data_abort(
        &mut self,
        syndrome: u64,
        fault_ipa: u64,
        mmio: &MmioBus,
    ) -> Result<(), String> {
        let isv = (syndrome >> 24) & 1;
        if isv == 0 {
            let pc = self.get_reg(HV_REG_PC).unwrap_or(0);
            return Err(format!(
                "Data abort at IPA 0x{:08x} (PC=0x{:016x}) has ISV=0 (instruction syndrome invalid)",
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

    fn handle_sysreg_trap(&mut self, syndrome: u64) -> Result<(), String> {
        let op0 = (syndrome >> 20) & 3;
        let op2 = (syndrome >> 17) & 7;
        let op1 = (syndrome >> 14) & 7;
        let crn = (syndrome >> 10) & 0xf;
        let rt = ((syndrome >> 5) & 0x1f) as u32;
        let crm = (syndrome >> 1) & 0xf;
        let is_read = (syndrome & 1) == 1; // 1 = MRS (read), 0 = MSR (write)

        // op0 == 2 is ARM Debug register space (OSLAR_EL1, OSLSR_EL1, MDSCR_EL1, etc.)
        if op0 == 2 {
            if is_read && rt != 31 {
                // Return 0 for debug register reads (e.g. OSLSR_EL1: unlocked)
                self.set_reg(HV_REG_X0 + rt, 0).map_err(|e| e.to_string())?;
            }
            // Writes to debug registers (like OSLAR_EL1 lock/unlock) are safely ignored
        } else {
            let pc = self.get_reg(HV_REG_PC).unwrap_or(0);
            return Err(format!(
                "Unhandled System Register access (EC 0x18) at PC=0x{:016x}: \
                op0={}, op1={}, CRn={}, CRm={}, op2={}, Rt={}, is_read={}",
                pc, op0, op1, crn, crm, op2, rt, is_read
            ));
        }

        // Advance PC by 4
        let pc = self.get_reg(HV_REG_PC).map_err(|e| e.to_string())?;
        self.set_reg(HV_REG_PC, pc + 4).map_err(|e| e.to_string())?;

        Ok(())
    }

    fn handle_psci(&mut self) -> Result<Option<VcpuExitStatus>, String> {
        let func_id = self.get_reg(HV_REG_X0).map_err(|e| e.to_string())?;

        match func_id {
            // PSCI_VERSION (0x84000000): Return PSCI v0.2
            PSCI_VERSION => {
                self.set_reg(HV_REG_X0, 0x0002).map_err(|e| e.to_string())?;
                let pc = self.get_reg(HV_REG_PC).map_err(|e| e.to_string())?;
                self.set_reg(HV_REG_PC, pc + 4).map_err(|e| e.to_string())?;
                Ok(None)
            }
            // PSCI_FEATURES (0x8400000a)
            PSCI_FEATURES => {
                let feature = self.get_reg(HV_REG_X1).map_err(|e| e.to_string())?;
                let ret = match feature {
                    PSCI_CPU_ON_AARCH64
                    | PSCI_CPU_ON_AARCH32
                    | PSCI_CPU_OFF
                    | PSCI_SYSTEM_OFF
                    | PSCI_SYSTEM_RESET
                    | PSCI_MIGRATE_INFO_TYPE => PSCI_SUCCESS,
                    _ => PSCI_NOT_SUPPORTED,
                };
                self.set_reg(HV_REG_X0, ret as u64).map_err(|e| e.to_string())?;
                let pc = self.get_reg(HV_REG_PC).map_err(|e| e.to_string())?;
                self.set_reg(HV_REG_PC, pc + 4).map_err(|e| e.to_string())?;
                Ok(None)
            }
            // PSCI CPU_ON (0xc4000003 for 64-bit, 0x84000003 for 32-bit)
            PSCI_CPU_ON_AARCH64 | PSCI_CPU_ON_AARCH32 => {
                let target_cpu = self.get_reg(HV_REG_X1).map_err(|e| e.to_string())?;
                let entry_point = self.get_reg(HV_REG_X2).map_err(|e| e.to_string())?;
                let context_id = self.get_reg(HV_REG_X3).map_err(|e| e.to_string())?;

                let ret = if let Some(ref mgr) = self.manager {
                    if let Some(slot) = mgr.get_slot_by_mpidr(target_cpu) {
                        if slot.is_online.load(Ordering::Relaxed) {
                            PSCI_ALREADY_ON
                        } else {
                            {
                                let mut boot = slot.boot_args.lock().unwrap();
                                *boot = Some(SecondaryBootArgs {
                                    entry_point,
                                    context_id,
                                });
                            }
                            slot.is_online.store(true, Ordering::Relaxed);
                            slot.condvar.notify_one();
                            println!(
                                "[PSCI] CPU_ON target 0x{:x} (core {}) -> entry 0x{:016x}, ctx 0x{:016x}",
                                target_cpu, slot.vcpu_index, entry_point, context_id
                            );
                            PSCI_SUCCESS
                        }
                    } else {
                        PSCI_INVALID_PARAMETERS
                    }
                } else {
                    PSCI_INVALID_PARAMETERS
                };

                self.set_reg(HV_REG_X0, ret as u64).map_err(|e| e.to_string())?;
                let pc = self.get_reg(HV_REG_PC).map_err(|e| e.to_string())?;
                self.set_reg(HV_REG_PC, pc + 4).map_err(|e| e.to_string())?;
                Ok(None)
            }
            // PSCI CPU_OFF (0x84000002)
            PSCI_CPU_OFF => {
                if let Some(ref mgr) = self.manager {
                    if let Some(slot) = mgr.get_slot_by_mpidr(self.vcpu_index as u64) {
                        slot.is_online.store(false, Ordering::Relaxed);
                    }
                }
                let pc = self.get_reg(HV_REG_PC).map_err(|e| e.to_string())?;
                self.set_reg(HV_REG_PC, pc + 4).map_err(|e| e.to_string())?;
                Ok(Some(VcpuExitStatus::Halted))
            }
            // PSCI MIGRATE_INFO_TYPE (0x84000006): Return 2 (Trusted OS not present)
            PSCI_MIGRATE_INFO_TYPE => {
                self.set_reg(HV_REG_X0, 2).map_err(|e| e.to_string())?;
                let pc = self.get_reg(HV_REG_PC).map_err(|e| e.to_string())?;
                self.set_reg(HV_REG_PC, pc + 4).map_err(|e| e.to_string())?;
                Ok(None)
            }
            // PSCI SYSTEM_OFF (0x84000008)
            PSCI_SYSTEM_OFF => {
                if let Some(ref mgr) = self.manager {
                    mgr.request_shutdown();
                }
                Ok(Some(VcpuExitStatus::Shutdown))
            }
            // PSCI SYSTEM_RESET (0x84000009)
            PSCI_SYSTEM_RESET => {
                if let Some(ref mgr) = self.manager {
                    mgr.request_shutdown();
                }
                Ok(Some(VcpuExitStatus::Reset))
            }
            _ => {
                eprintln!("[VMM Warning] Unhandled PSCI function ID: 0x{:08x}", func_id);
                self.set_reg(HV_REG_X0, PSCI_NOT_SUPPORTED as u64).map_err(|e| e.to_string())?;
                let pc = self.get_reg(HV_REG_PC).map_err(|e| e.to_string())?;
                self.set_reg(HV_REG_PC, pc + 4).map_err(|e| e.to_string())?;
                Ok(None)
            }
        }
    }
}

impl Drop for Vcpu {
    fn drop(&mut self) {
        if let Some(ref mgr) = self.manager {
            mgr.unregister_vcpu_id(self.id);
        }
        unsafe {
            let _ = hv_vcpu_destroy(self.id);
        }
    }
}
