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

/// Represents an ARM64 CPU Model identifying Main ID Register (MIDR_EL1).
///
/// In FreeBSD ARM64 (identcpu.c), the CPU name and hw.model are derived
/// directly from MIDR_EL1 (Implementer, Architecture, Part Number, Revision).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuModel {
    pub midr: u64,
    pub name: &'static str,
}

impl CpuModel {
    // Recognized Apple Silicon cores (FreeBSD arm64/identcpu.c)
    pub const APPLE_M1_FIRESTORM: Self = Self {
        midr: 0x610f_0230,
        name: "Apple M1 Firestorm",
    };
    pub const APPLE_M1_ICESTORM: Self = Self {
        midr: 0x610f_0220,
        name: "Apple M1 Icestorm",
    };
    pub const APPLE_M1_PRO: Self = Self {
        midr: 0x610f_0250,
        name: "Apple M1 Pro Firestorm",
    };
    pub const APPLE_M1_MAX: Self = Self {
        midr: 0x610f_0290,
        name: "Apple M1 Max Firestorm",
    };
    pub const APPLE_M2_AVALANCHE: Self = Self {
        midr: 0x610f_0330,
        name: "Apple M2 Avalanche",
    };
    pub const APPLE_M2_BLIZZARD: Self = Self {
        midr: 0x610f_0320,
        name: "Apple M2 Blizzard",
    };
    pub const APPLE_M2_PRO: Self = Self {
        midr: 0x610f_0350,
        name: "Apple M2 Pro Avalanche",
    };
    pub const APPLE_M2_MAX: Self = Self {
        midr: 0x610f_0390,
        name: "Apple M2 Max Avalanche",
    };

    // Standard ARM Neoverse & Cortex server cores
    pub const ARM_NEOVERSE_N2: Self = Self {
        midr: 0x410f_d490,
        name: "ARM Neoverse-N2",
    };
    pub const ARM_NEOVERSE_N1: Self = Self {
        midr: 0x410f_d0c0,
        name: "ARM Neoverse-N1",
    };
    pub const ARM_NEOVERSE_V1: Self = Self {
        midr: 0x410f_d400,
        name: "ARM Neoverse-V1",
    };
    pub const ARM_CORTEX_A78: Self = Self {
        midr: 0x410f_d410,
        name: "ARM Cortex-A78",
    };
    pub const ARM_CORTEX_A72: Self = Self {
        midr: 0x410f_d080,
        name: "ARM Cortex-A72",
    };
    pub const ARM_CORTEX_A53: Self = Self {
        midr: 0x410f_d030,
        name: "ARM Cortex-A53",
    };
    pub const AMPERE_ONE: Self = Self {
        midr: 0xc00f_ac30,
        name: "Ampere AmpereOne AC03",
    };

    /// Automatically detects host Apple Silicon model via macOS sysctl.
    pub fn detect_host() -> Self {
        if let Some(brand) = get_host_brand_string() {
            let b = brand.to_lowercase();
            if b.contains("m1 max") || b.contains("m1 ultra") {
                return Self::APPLE_M1_MAX;
            } else if b.contains("m1 pro") {
                return Self::APPLE_M1_PRO;
            } else if b.contains("m1") {
                return Self::APPLE_M1_FIRESTORM;
            } else if b.contains("m2 max") || b.contains("m2 ultra") {
                return Self::APPLE_M2_MAX;
            } else if b.contains("m2 pro") {
                return Self::APPLE_M2_PRO;
            } else if b.contains("m2") {
                return Self::APPLE_M2_AVALANCHE;
            } else if b.contains("m3") || b.contains("m4") {
                // FreeBSD 14.x recognizes up to M2; Avalanche is the best modern Apple match
                return Self::APPLE_M2_AVALANCHE;
            }
        }
        // Default to Apple M1 Max / Apple Silicon
        Self::APPLE_M1_MAX
    }

    /// Parses user-provided CPU model alias or hex MIDR value.
    pub fn parse(s: &str) -> Result<Self, String> {
        let lower = s.trim().to_lowercase();
        match lower.as_str() {
            "auto" | "host" => Ok(Self::detect_host()),
            "m1" | "m1-firestorm" | "firestorm" => Ok(Self::APPLE_M1_FIRESTORM),
            "m1-icestorm" | "icestorm" => Ok(Self::APPLE_M1_ICESTORM),
            "m1-pro" | "m1-pro-firestorm" => Ok(Self::APPLE_M1_PRO),
            "m1-max" | "m1-max-firestorm" => Ok(Self::APPLE_M1_MAX),
            "m2" | "m2-avalanche" | "avalanche" => Ok(Self::APPLE_M2_AVALANCHE),
            "m2-blizzard" | "blizzard" => Ok(Self::APPLE_M2_BLIZZARD),
            "m2-pro" | "m2-pro-avalanche" => Ok(Self::APPLE_M2_PRO),
            "m2-max" | "m2-max-avalanche" => Ok(Self::APPLE_M2_MAX),
            "neoverse-n2" | "n2" => Ok(Self::ARM_NEOVERSE_N2),
            "neoverse-n1" | "n1" => Ok(Self::ARM_NEOVERSE_N1),
            "neoverse-v1" | "v1" => Ok(Self::ARM_NEOVERSE_V1),
            "cortex-a78" | "a78" => Ok(Self::ARM_CORTEX_A78),
            "cortex-a72" | "a72" => Ok(Self::ARM_CORTEX_A72),
            "cortex-a53" | "a53" => Ok(Self::ARM_CORTEX_A53),
            "ampere-one" | "ampereone" => Ok(Self::AMPERE_ONE),
            other => {
                let hex_str = other.strip_prefix("0x").unwrap_or(other);
                if let Ok(val) = u64::from_str_radix(hex_str, 16) {
                    Ok(Self {
                        midr: val,
                        name: "Custom MIDR",
                    })
                } else {
                    Err(format!(
                        "Unknown CPU model '{}'. Supported: auto, host, m1, m1-pro, m1-max, m2, m2-pro, m2-max, neoverse-n2, neoverse-n1, neoverse-v1, cortex-a78, cortex-a72, cortex-a53, ampere-one, or 0x<hex>",
                        s
                    ))
                }
            }
        }
    }
}

/// Reads the host CPU brand string via macOS sysctl machdep.cpu.brand_string without external crates.
fn get_host_brand_string() -> Option<String> {
    unsafe extern "C" {
        fn sysctlbyname(
            name: *const std::os::raw::c_char,
            oldp: *mut std::os::raw::c_void,
            oldlenp: *mut usize,
            newp: *const std::os::raw::c_void,
            newlen: usize,
        ) -> std::os::raw::c_int;
    }

    let name = c"machdep.cpu.brand_string";
    let mut size: usize = 0;
    unsafe {
        if sysctlbyname(
            name.as_ptr(),
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null(),
            0,
        ) != 0
            || size == 0
        {
            return None;
        }
        let mut buf = vec![0u8; size];
        if sysctlbyname(
            name.as_ptr(),
            buf.as_mut_ptr() as *mut _,
            &mut size,
            std::ptr::null(),
            0,
        ) != 0
        {
            return None;
        }
        while buf.last() == Some(&0) {
            buf.pop();
        }
        String::from_utf8(buf).ok()
    }
}

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
    cpu_model: CpuModel,
    slots: Vec<Arc<SecondaryVcpuSlot>>,
    active_vcpu_ids: Mutex<Vec<u64>>,
    shutdown_requested: AtomicBool,
    reset_requested: AtomicBool,
    failure: Mutex<Option<String>>,
    /// Generation counter + condvar used to wake vCPUs that trapped WFI.
    /// `hv_vcpus_exit` only interrupts `hv_vcpu_run`; it does not wake `thread::sleep`.
    wfi_gen: Mutex<u64>,
    wfi_cv: Condvar,
}

impl VcpuManager {
    pub fn new(num_cpus: usize, cpu_model: CpuModel) -> Arc<Self> {
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
            cpu_model,
            slots,
            active_vcpu_ids: Mutex::new(Vec::new()),
            shutdown_requested: AtomicBool::new(false),
            reset_requested: AtomicBool::new(false),
            failure: Mutex::new(None),
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

    pub fn cpu_model(&self) -> CpuModel {
        self.cpu_model
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
        self.slots
            .iter()
            .find(|s| (s.mpidr & 0xff) == target_aff)
            .cloned()
    }

    pub fn request_shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::Relaxed);
        for slot in &self.slots {
            // Use the waiter's mutex so a notification cannot be lost between
            // checking the predicate and entering the condvar wait.
            let _guard = slot.boot_args.lock().unwrap();
            slot.condvar.notify_all();
        }
        self.kick_all();
    }

    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::Relaxed)
    }

    pub fn wait_for_boot(&self, slot: &SecondaryVcpuSlot) -> Option<SecondaryBootArgs> {
        let mut args = slot.boot_args.lock().unwrap();
        while args.is_none() && !self.is_shutdown_requested() {
            args = slot.condvar.wait(args).unwrap();
        }
        if self.is_shutdown_requested() {
            None
        } else {
            args.take()
        }
    }

    pub fn reset_requested(&self) -> bool {
        self.reset_requested.load(Ordering::Relaxed)
    }

    pub fn fail(&self, error: String) {
        *self.failure.lock().unwrap() = Some(error);
        self.request_shutdown();
    }

    pub fn failure(&self) -> Option<String> {
        self.failure.lock().unwrap().clone()
    }
}

pub fn request_host_shutdown() {
    let manager = GLOBAL_VCPU_MANAGER
        .lock()
        .unwrap()
        .as_ref()
        .and_then(Weak::upgrade);
    if let Some(manager) = manager {
        manager.request_shutdown();
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
    pub fn setup_boot_state(
        &self,
        entry_point: u64,
        stack_pointer: u64,
        midr: u64,
    ) -> HvResult<()> {
        self.set_reg(HV_REG_PC, entry_point)?;
        self.set_reg(HV_REG_CPSR, 0x3c5)?; // EL1h, DAIF masked
        self.set_sys_reg(HV_SYS_REG_SP_EL1, stack_pointer)?;
        self.set_sys_reg(HV_SYS_REG_SCTLR_EL1, 0x00000000)?;
        self.set_sys_reg(HV_SYS_REG_CPACR_EL1, 3 << 20)?;
        self.set_sys_reg(HV_SYS_REG_MPIDR_EL1, 0x8000_0000)?;
        self.set_sys_reg(HV_SYS_REG_MIDR_EL1, midr)?;
        Ok(())
    }

    /// Sets initial state for booting an OS kernel (FreeBSD / Linux) according to the ARM64 boot specification:
    /// - X0 = DTB GPA (Linux / FreeBSD UFS) or FreeBSD loader MODINFO GPA (preloaded .ko / ZFS)
    /// - X1 = 0, X2 = 0, X3 = 0
    /// - PC = kernel entry point
    /// - CPSR = EL1h with DAIF interrupts masked (0x3c5)
    /// - MPIDR_EL1 = affinity value for vCPU
    /// - MIDR_EL1 = CPU identification register (VPIDR_EL2)
    pub fn setup_os_boot_state(
        &mut self,
        kernel_entry: u64,
        dtb_phys_addr: u64,
        vcpu_index: u64,
        midr: u64,
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

        // Set CPU identification register (MIDR_EL1 / VPIDR_EL2)
        self.set_sys_reg(HV_SYS_REG_MIDR_EL1, midr)?;

        // Standard MMU off at boot
        self.set_sys_reg(HV_SYS_REG_SCTLR_EL1, 0x0000_0000)?;

        // Enable FP/NEON
        self.set_sys_reg(HV_SYS_REG_CPACR_EL1, 3 << 20)?;

        // HVF advertises host FEAT_RNG via ID_AA64ISAR0 but does not emulate RNDR.
        // EDK2 RngDxe then executes RNDR and takes a Synchronous Exception. Clear
        // the RNDR field (bits 63:60) so firmware uses virtio-rng / other backends.
        match self.get_sys_reg(HV_SYS_REG_ID_AA64ISAR0_EL1) {
            Ok(isar0) => {
                let masked = isar0 & !(0xfu64 << 60);
                eprintln!("[Cube] ID_AA64ISAR0_EL1={isar0:#x} (RNDR field cleared -> {masked:#x})");
                if let Err(e) = self.set_sys_reg(HV_SYS_REG_ID_AA64ISAR0_EL1, masked) {
                    eprintln!("[Cube] Warning: failed to set ID_AA64ISAR0_EL1: {e}");
                } else if let Ok(v) = self.get_sys_reg(HV_SYS_REG_ID_AA64ISAR0_EL1) {
                    eprintln!("[Cube] ID_AA64ISAR0_EL1 readback={v:#x}");
                }
            }
            Err(e) => eprintln!("[Cube] Warning: get ID_AA64ISAR0_EL1: {e}"),
        }

        Ok(())
    }

    /// Configures state for secondary vCPUs upon PSCI CPU_ON release
    pub fn setup_secondary_boot_state(
        &mut self,
        entry_point: u64,
        context_id: u64,
        midr: u64,
    ) -> HvResult<()> {
        self.wfi_halts_vm = false;
        self.set_reg(HV_REG_PC, entry_point)?;
        self.set_reg(HV_REG_X0, context_id)?;
        self.set_reg(HV_REG_X1, 0)?;
        self.set_reg(HV_REG_X2, 0)?;
        self.set_reg(HV_REG_X3, 0)?;
        self.set_reg(HV_REG_CPSR, 0x3c5)?;
        let mpidr = 0x8000_0000 | (self.vcpu_index as u64 & 0xff);
        self.set_sys_reg(HV_SYS_REG_MPIDR_EL1, mpidr)?;
        self.set_sys_reg(HV_SYS_REG_MIDR_EL1, midr)?;
        self.set_sys_reg(HV_SYS_REG_SCTLR_EL1, 0x0000_0000)?;
        self.set_sys_reg(HV_SYS_REG_CPACR_EL1, 3 << 20)?;
        Ok(())
    }

    /// Runs the vCPU until it exits, dispatching MMIO, PSCI calls, and exceptions.
    pub fn run(&mut self, mmio: &MmioBus) -> Result<VcpuExitStatus, String> {
        let trace_boot = std::env::var_os("VMM_TRACE_BOOT").is_some();
        let mut exits: u64 = 0;
        let mut last_trace = std::time::Instant::now();
        if trace_boot {
            let pc = self.get_reg(HV_REG_PC).unwrap_or(0);
            let x0 = self.get_reg(HV_REG_X0).unwrap_or(0);
            eprintln!("[Cube TRACE] enter run PC=0x{pc:016x} X0=0x{x0:016x}");
            // Force periodic exits so we can sample PC if the guest spins with no traps.
            let vcpu_id = self.id;
            std::thread::spawn(move || {
                for i in 1..=20 {
                    std::thread::sleep(std::time::Duration::from_millis(250));
                    unsafe {
                        let ids = [vcpu_id];
                        let _ = crate::hypervisor::hv_vcpus_exit(ids.as_ptr(), 1);
                    }
                    eprintln!("[Cube TRACE] kick #{i} sent");
                }
            });
        }
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
            exits = exits.wrapping_add(1);
            if trace_boot && (exits <= 30 || last_trace.elapsed().as_millis() >= 400) {
                let pc = self.get_reg(HV_REG_PC).unwrap_or(0);
                let x0 = self.get_reg(HV_REG_X0).unwrap_or(0);
                let syndrome = exit.exception.syndrome;
                let ec = (syndrome >> 26) & 0x3f;
                let ipa = exit.exception.physical_address;
                eprintln!(
                    "[Cube TRACE] exit#{exits} reason={} EC=0x{ec:02x} PC=0x{pc:016x} IPA=0x{ipa:016x} X0=0x{x0:016x}",
                    exit.reason as u32
                );
                last_trace = std::time::Instant::now();
            }

            match exit.reason {
                HV_EXIT_REASON_EXCEPTION => {
                    let syndrome = exit.exception.syndrome;
                    let ec = (syndrome >> 26) & 0x3f;

                    match ec {
                        // EC 0x24: Data Abort from lower or same EL (e.g. MMIO)
                        0x24 => {
                            self.handle_data_abort(
                                syndrome,
                                exit.exception.physical_address,
                                mmio,
                            )?;
                        }
                        // EC 0x16 / 0x17: HVC / SMC -> PSCI + SMCCC + TRNG
                        0x16 | 0x17 => {
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
                    if trace_boot {
                        let pc = self.get_reg(HV_REG_PC).unwrap_or(0);
                        let x0 = self.get_reg(HV_REG_X0).unwrap_or(0);
                        let x1 = self.get_reg(HV_REG_X1).unwrap_or(0);
                        let x19 = self.get_reg(HV_REG_X0 + 19).unwrap_or(0);
                        let lr = self.get_reg(HV_REG_X0 + 30).unwrap_or(0);
                        let sp = self.get_sys_reg(HV_SYS_REG_SP_EL1).unwrap_or(0);
                        let cpsr = self.get_reg(HV_REG_CPSR).unwrap_or(0);
                        eprintln!(
                            "[Cube TRACE] kick-sample PC={pc:#x} X0={x0:#x} X1={x1:#x} X19={x19:#x} LR={lr:#x} SP={sp:#x} CPSR={cpsr:#x}"
                        );
                    }
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
                self.set_reg(HV_REG_X0 + srt, val)
                    .map_err(|e| e.to_string())?;
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

        // CurrentEL (S3_0_C4_C2_2) must read as EL1(=4). HVF may trap it; RAZ would
        // make EDK2 think EL0 and CpuDeadLoop (see firmware at 0x5a18).
        if is_read && op0 == 3 && op1 == 0 && crn == 4 && crm == 2 && op2 == 2 {
            if rt != 31 {
                self.set_reg(HV_REG_X0 + rt, 0x4)
                    .map_err(|e| e.to_string())?;
            }
            let pc = self.get_reg(HV_REG_PC).map_err(|e| e.to_string())?;
            self.set_reg(HV_REG_PC, pc + 4).map_err(|e| e.to_string())?;
            return Ok(());
        }

        if std::env::var_os("VMM_TRACE_BOOT").is_some() {
            let pc = self.get_reg(HV_REG_PC).unwrap_or(0);
            eprintln!(
                "[Cube TRACE] sysreg-trap PC={pc:#x} op0={op0} op1={op1} CRn={crn} CRm={crm} op2={op2} Rt={rt} read={is_read}"
            );
        }
        // op0 == 2 is ARM Debug register space (OSLAR_EL1, OSLSR_EL1, MDSCR_EL1, etc.)
        if op0 == 2 {
            if is_read && rt != 31 {
                // Return 0 for debug register reads (e.g. OSLSR_EL1: unlocked)
                self.set_reg(HV_REG_X0 + rt, 0).map_err(|e| e.to_string())?;
            }
            // Writes to debug registers (like OSLAR_EL1 lock/unlock) are safely ignored
        } else {
            // RAZ/WI unknown sysregs (EDK2 probes many; QEMU does not kill the guest).
            if std::env::var_os("VMM_TRACE_BOOT").is_some() {
                let pc = self.get_reg(HV_REG_PC).unwrap_or(0);
                eprintln!(
                    "[Cube TRACE] sysreg-RAZWI PC={pc:#x} op0={op0} op1={op1} CRn={crn} CRm={crm} op2={op2} Rt={rt} read={is_read}"
                );
            }
            if is_read && rt != 31 {
                self.set_reg(HV_REG_X0 + rt, 0).map_err(|e| e.to_string())?;
            }
        }

        // Advance PC by 4
        let pc = self.get_reg(HV_REG_PC).map_err(|e| e.to_string())?;
        self.set_reg(HV_REG_PC, pc + 4).map_err(|e| e.to_string())?;

        Ok(())
    }

    /// Handle HVC/SMC (PSCI + SMCCC + TRNG). Do not advance PC: HVF already points past the insn.
    fn handle_psci(&mut self) -> Result<Option<VcpuExitStatus>, String> {
        let func_id = self.get_reg(HV_REG_X0).map_err(|e| e.to_string())?;
        if std::env::var_os("VMM_TRACE_BOOT").is_some() {
            let x1 = self.get_reg(HV_REG_X1).unwrap_or(0);
            eprintln!("[Cube TRACE] SMCCC/PSCI call id={func_id:#x} x1={x1:#x}");
        }

        match func_id {
            // SMCCC_VERSION (0x80000000): Return SMCCC 1.1 (required by EDK2 ArmTrngLib/RngDxe)
            SMCCC_VERSION => {
                // HVF leaves PC past the HVC/SMC insn — do not +4 or we skip the return path.
                self.set_reg(HV_REG_X0, 0x0001_0001)
                    .map_err(|e| e.to_string())?;
                Ok(None)
            }
            // SMCCC_ARCH_FEATURES / WORKAROUNDS: not implemented
            SMCCC_ARCH_FEATURES
            | SMCCC_ARCH_SOC_ID
            | SMCCC_ARCH_WORKAROUND_1
            | SMCCC_ARCH_WORKAROUND_2 => {
                self.set_reg(HV_REG_X0, PSCI_NOT_SUPPORTED as u64)
                    .map_err(|e| e.to_string())?;
                Ok(None)
            }
            // ARM TRNG (DEN0098) — EDK2 RngDxe uses this when FEAT_RNG is absent.
            SMCCC_TRNG_VERSION => {
                // Version 1.0
                self.set_reg(HV_REG_X0, 0x0001_0000)
                    .map_err(|e| e.to_string())?;
                Ok(None)
            }
            SMCCC_TRNG_FEATURES => {
                // DEN0098: for RND*, success returns max entropy bits per call (32..192).
                // Returning 0 made EDK2 ArmTrngLib miscompute buffer sizes and data-abort.
                let fid = self.get_reg(HV_REG_X1).map_err(|e| e.to_string())?;
                let ret = match fid {
                    SMCCC_TRNG_RND32 | SMCCC_TRNG_RND64 => 192i64,
                    SMCCC_TRNG_VERSION | SMCCC_TRNG_FEATURES | SMCCC_TRNG_GET_UUID => 0i64,
                    _ => PSCI_NOT_SUPPORTED,
                };
                self.set_reg(HV_REG_X0, ret as u64)
                    .map_err(|e| e.to_string())?;
                Ok(None)
            }
            SMCCC_TRNG_GET_UUID => {
                // RFC-4122 UUID for "VMM TRNG" (made-up, stable)
                self.set_reg(HV_REG_X0, 0x564d_4d54_524e_4701)
                    .map_err(|e| e.to_string())?; // "VMMTRNG"
                self.set_reg(HV_REG_X1, 0x0000_4000_8000_0000u64)
                    .map_err(|e| e.to_string())?;
                self.set_reg(HV_REG_X2, 0xa000_0000_0000_0001u64)
                    .map_err(|e| e.to_string())?;
                self.set_reg(HV_REG_X3, 0x0000_0000_0000_0001u64)
                    .map_err(|e| e.to_string())?;
                Ok(None)
            }
            SMCCC_TRNG_RND32 | SMCCC_TRNG_RND64 => {
                // x1 = number of entropy bits requested (32..192). Fill x1..x3.
                let mut buf = [0u8; 24];
                getrandom::getrandom(&mut buf).map_err(|e| format!("TRNG getrandom: {e}"))?;
                let w0 = u64::from_le_bytes(buf[0..8].try_into().unwrap());
                let w1 = u64::from_le_bytes(buf[8..16].try_into().unwrap());
                let w2 = u64::from_le_bytes(buf[16..24].try_into().unwrap());
                self.set_reg(HV_REG_X0, 0).map_err(|e| e.to_string())?; // SUCCESS
                self.set_reg(HV_REG_X1, w0).map_err(|e| e.to_string())?;
                self.set_reg(HV_REG_X2, w1).map_err(|e| e.to_string())?;
                self.set_reg(HV_REG_X3, w2).map_err(|e| e.to_string())?;
                Ok(None)
            }
            // PSCI_VERSION (0x84000000): Return PSCI v1.0 (0x00010000)
            PSCI_VERSION => {
                self.set_reg(HV_REG_X0, 0x0001_0000).map_err(|e| e.to_string())?;
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
                self.set_reg(HV_REG_X0, ret as u64)
                    .map_err(|e| e.to_string())?;
                Ok(None)
            }
            // PSCI CPU_ON (0xc4000003 for 64-bit, 0x84000003 for 32-bit)
            PSCI_CPU_ON_AARCH64 | PSCI_CPU_ON_AARCH32 => {
                let target_cpu = self.get_reg(HV_REG_X1).map_err(|e| e.to_string())?;
                let entry_point = self.get_reg(HV_REG_X2).map_err(|e| e.to_string())?;
                let context_id = self.get_reg(HV_REG_X3).map_err(|e| e.to_string())?;

                let ret = if let Some(ref mgr) = self.manager {
                    if let Some(slot) = mgr.get_slot_by_mpidr(target_cpu) {
                        if slot
                            .is_online
                            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                            .is_err()
                        {
                            PSCI_ALREADY_ON
                        } else {
                            {
                                let mut boot = slot.boot_args.lock().unwrap();
                                *boot = Some(SecondaryBootArgs {
                                    entry_point,
                                    context_id,
                                });
                            }
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

                self.set_reg(HV_REG_X0, ret as u64)
                    .map_err(|e| e.to_string())?;
                Ok(None)
            }
            // PSCI CPU_OFF (0x84000002)
            PSCI_CPU_OFF => {
                if let Some(ref mgr) = self.manager {
                    if let Some(slot) = mgr.get_slot_by_mpidr(self.vcpu_index as u64) {
                        slot.is_online.store(false, Ordering::Relaxed);
                    }
                }
                Ok(Some(VcpuExitStatus::Halted))
            }
            // PSCI MIGRATE_INFO_TYPE (0x84000006): Return 2 (Trusted OS not present)
            PSCI_MIGRATE_INFO_TYPE => {
                self.set_reg(HV_REG_X0, 2).map_err(|e| e.to_string())?;
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
                    mgr.reset_requested.store(true, Ordering::Relaxed);
                    mgr.request_shutdown();
                }
                Ok(Some(VcpuExitStatus::Reset))
            }
            _ => {
                eprintln!(
                    "[Cube Warning] Unhandled PSCI function ID: 0x{:08x}",
                    func_id
                );
                self.set_reg(HV_REG_X0, PSCI_NOT_SUPPORTED as u64)
                    .map_err(|e| e.to_string())?;
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

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    #[test]
    fn shutdown_wakes_a_cpu_that_was_never_started() {
        let manager = VcpuManager::new(
            2,
            CpuModel {
                midr: 0,
                name: "test",
            },
        );
        let slot = manager.slots()[0].clone();
        let worker_manager = manager.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            tx.send(worker_manager.wait_for_boot(&slot).is_none())
                .unwrap();
        });
        manager.request_shutdown();
        assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap());
        worker.join().unwrap();
    }
    #[test]
    fn secondary_slot_can_be_released_more_than_once() {
        let manager = VcpuManager::new(
            2,
            CpuModel {
                midr: 0,
                name: "test",
            },
        );
        let slot = &manager.slots()[0];
        for entry_point in [0x4000, 0x8000] {
            *slot.boot_args.lock().unwrap() = Some(SecondaryBootArgs {
                entry_point,
                context_id: 1,
            });
            assert_eq!(
                manager.wait_for_boot(slot).unwrap().entry_point,
                entry_point
            );
            assert!(slot.boot_args.lock().unwrap().is_none());
        }
        manager.request_shutdown();
        assert!(manager.wait_for_boot(slot).is_none());
    }
}
