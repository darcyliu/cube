//! Minimal MMIO xHCI host controller for FreeBSD `generic-xhci`.
//!
//! Layout mirrors QEMU (`CAPLENGTH=0x40`, runtime@0x1000, doorbell@0x2000).
//! Implemented enough for FreeBSD to attach, enumerate two HS HID devices on
//! root ports 1–2, and complete interrupt-IN HID reports.
//!
//! ## Commands / transfers supported
//! - Enable Slot, Disable Slot, Address Device, Configure Endpoint, Evaluate
//!   Context, Reset Endpoint, Stop Endpoint, Set TR Dequeue, No-Op
//! - Control transfers (Setup/Data/Status) for enumeration + HID class reqs
//! - Interrupt IN Normal TRBs (complete when a report is queued, else defer)
//!
//! ## Gaps (follow-up if attach stalls)
//! - No streams / isoch / bulk beyond control+interrupt
//! - No USB3 protocol / companion ports; HS only
//! - Limited error paths (most failures → TRB Error / Stall event)
//! - Slot contexts written with a simplified template FreeBSD accepts

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::Arc;

use crate::devices::MmioDevice;
use crate::memory::GuestMemory;

use super::hid::{self, HidFrontend, HidKind, PORT_TABLET};

pub const MMIO_XHCI_SIZE: u64 = 0x1_0000; // 64 KiB

const CAPLENGTH: u64 = 0x40;
const OFF_OPER: u64 = CAPLENGTH;
const OFF_RUNTIME: u64 = 0x1000;
const OFF_DOORBELL: u64 = 0x2000;

const MAX_SLOTS: u32 = 8;
const MAX_INTRS: u32 = 1;
const MAX_PORTS: u32 = 2;

// USBCMD
const USBCMD_RS: u32 = 1 << 0;
const USBCMD_HCRST: u32 = 1 << 1;
const USBCMD_INTE: u32 = 1 << 2;

// USBSTS
const USBSTS_HCH: u32 = 1 << 0;
const USBSTS_HSE: u32 = 1 << 2;
const USBSTS_EINT: u32 = 1 << 3;
const USBSTS_PCD: u32 = 1 << 4;
const USBSTS_CNR: u32 = 1 << 11;

// PORTSC
const PORTSC_CCS: u32 = 1 << 0;
const PORTSC_PED: u32 = 1 << 1;
const PORTSC_PR: u32 = 1 << 4;
const PORTSC_PP: u32 = 1 << 9;
const PORTSC_CSC: u32 = 1 << 17;
const PORTSC_PEC: u32 = 1 << 18;
const PORTSC_PRC: u32 = 1 << 21;
const PORTSC_SPEED_HS: u32 = 3 << 10; // Port Speed = High Speed

// Interrupter
const IMAN_IP: u32 = 1 << 0;
const IMAN_IE: u32 = 1 << 1;

// TRB types
const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_EVENT_DATA: u32 = 7;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_DISABLE_SLOT: u32 = 10;
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_EP: u32 = 12;
const TRB_EVALUATE_CTX: u32 = 13;
const TRB_RESET_EP: u32 = 14;
const TRB_STOP_EP: u32 = 15;
const TRB_SET_TR_DEQUEUE: u32 = 16;
const TRB_RESET_DEVICE: u32 = 17;
const TRB_NOOP_CMD: u32 = 23;
const TRB_TRANSFER_EVENT: u32 = 32;
const TRB_CMD_COMPLETE: u32 = 33;
const TRB_PORT_STATUS: u32 = 34;

// Completion codes
const CC_SUCCESS: u32 = 1;
const CC_TRB_ERROR: u32 = 5;
const CC_STALL: u32 = 6;
const CC_SHORT_PACKET: u32 = 13;

#[derive(Clone, Copy, Default)]
struct Trb {
    ptr: u64,
    status: u32,
    control: u32,
}

impl Trb {
    fn type_(&self) -> u32 {
        (self.control >> 10) & 0x3f
    }
    fn cycle(&self) -> bool {
        self.control & 1 != 0
    }
    fn ioc(&self) -> bool {
        self.control & (1 << 5) != 0
    }
    fn chain(&self) -> bool {
        self.control & (1 << 4) != 0
    }
    fn slot_id(&self) -> u32 {
        (self.control >> 24) & 0xff
    }
    fn epid(&self) -> u32 {
        (self.control >> 16) & 0x1f
    }
    fn transfer_len(&self) -> u32 {
        self.status & 0x1_ffff
    }
}

#[derive(Clone, Copy, Default)]
struct EndpointState {
    dequeue: u64,
    cycle: bool,
    configured: bool,
}

#[derive(Clone, Copy)]
struct SlotState {
    enabled: bool,
    addressed: bool,
    port: u8,
    device_ctx: u64,
    eps: [EndpointState; 31], // EP0..EP30 (we use 1=EP0, 3=IN EP1)
}

impl Default for SlotState {
    fn default() -> Self {
        Self {
            enabled: false,
            addressed: false,
            port: 0,
            device_ctx: 0,
            eps: [EndpointState::default(); 31],
        }
    }
}

/// Deferred interrupt-IN waiting for a HID report.
struct PendingIntr {
    slot: u8,
    epid: u8,
    trb_addr: u64,
    buf: u64,
    len: u32,
    ioc: bool,
}

pub struct XhciController {
    mem: Arc<GuestMemory>,
    irq_num: u32,
    hid: HidFrontend,

    // Operational
    usbcmd: u32,
    usbsts: u32,
    pagesize: u32,
    dnctrl: u32,
    crcr: u64,
    dcbaap: u64,
    config: u32,
    portsc: [u32; MAX_PORTS as usize],

    // Runtime interrupter 0
    iman: u32,
    imod: u32,
    erstsz: u32,
    erstba: u64,
    erdp: u64,

    cmd_ring: u64,
    cmd_cycle: bool,
    er_enqueue: u64, // index into segment 0
    er_cycle: bool,

    slots: [SlotState; (MAX_SLOTS + 1) as usize], // 1..=MAX_SLOTS
    pending_intr: VecDeque<PendingIntr>,

    // Control transfer assembly per slot
    setup: [Option<SetupState>; (MAX_SLOTS + 1) as usize],

    irq_level: bool,
}

#[derive(Clone, Copy)]
struct SetupState {
    bm_request_type: u8,
    b_request: u8,
    w_value: u16,
    w_index: u16,
    w_length: u16,
    data_buf: u64,
    data_len: u32,
}

impl XhciController {
    pub fn new(irq_num: u32, mem: Arc<GuestMemory>, abs_max_x: u16, abs_max_y: u16) -> Self {
        let mut portsc = [0u32; MAX_PORTS as usize];
        for p in &mut portsc {
            // Connected, powered, HS, CSC set so software notices.
            *p = PORTSC_CCS | PORTSC_PP | PORTSC_SPEED_HS | PORTSC_CSC;
        }
        Self {
            mem,
            irq_num,
            hid: HidFrontend::new(abs_max_x, abs_max_y),
            usbcmd: 0,
            usbsts: USBSTS_HCH,
            pagesize: 1, // 4K (bit0)
            dnctrl: 0,
            crcr: 0,
            dcbaap: 0,
            config: 0,
            portsc,
            iman: 0,
            imod: 0,
            erstsz: 0,
            erstba: 0,
            erdp: 0,
            cmd_ring: 0,
            cmd_cycle: true,
            er_enqueue: 0,
            er_cycle: true,
            slots: std::array::from_fn(|_| SlotState::default()),
            pending_intr: VecDeque::new(),
            setup: [None; (MAX_SLOTS + 1) as usize],
            irq_level: false,
        }
    }

    pub fn key_event(&mut self, linux_keycode: u16, down: bool) {
        self.hid.key_event(linux_keycode, down);
        self.try_complete_pending_intr();
    }

    pub fn pointer_event(&mut self, x: u16, y: u16, buttons: u8) {
        self.hid.pointer_event(x, y, buttons);
        self.try_complete_pending_intr();
    }

    /// Drain any deferred interrupt IN now that reports may be queued.
    pub fn poll_flush(&mut self) {
        self.try_complete_pending_intr();
    }

    fn running(&self) -> bool {
        self.usbcmd & USBCMD_RS != 0 && self.usbsts & USBSTS_HCH == 0
    }

    fn raise_irq(&mut self) {
        self.usbsts |= USBSTS_EINT;
        self.iman |= IMAN_IP;
        if self.usbcmd & USBCMD_INTE != 0 && self.iman & IMAN_IE != 0 {
            if !self.irq_level {
                self.irq_level = true;
                unsafe {
                    let _ = crate::hypervisor::hv_gic_set_spi(self.irq_num, true);
                }
                crate::vcpu::kick_vcpu();
            }
        }
    }

    fn lower_irq_if_clear(&mut self) {
        // Guest clears IP by writing 1; also clears EINT via USBSTS W1C.
        if self.iman & IMAN_IP == 0 && self.usbsts & USBSTS_EINT == 0 {
            if self.irq_level {
                self.irq_level = false;
                unsafe {
                    let _ = crate::hypervisor::hv_gic_set_spi(self.irq_num, false);
                }
            }
        }
    }

    fn hcrst(&mut self) {
        self.usbcmd = 0;
        self.usbsts = USBSTS_HCH;
        self.dnctrl = 0;
        self.crcr = 0;
        self.dcbaap = 0;
        self.config = 0;
        self.iman = 0;
        self.imod = 0;
        self.erstsz = 0;
        self.erstba = 0;
        self.erdp = 0;
        self.cmd_ring = 0;
        self.cmd_cycle = true;
        self.er_enqueue = 0;
        self.er_cycle = true;
        self.slots = std::array::from_fn(|_| SlotState::default());
        self.pending_intr.clear();
        self.setup = [None; (MAX_SLOTS + 1) as usize];
        for p in &mut self.portsc {
            *p = PORTSC_CCS | PORTSC_PP | PORTSC_SPEED_HS | PORTSC_CSC;
        }
        if self.irq_level {
            self.irq_level = false;
            unsafe {
                let _ = crate::hypervisor::hv_gic_set_spi(self.irq_num, false);
            }
        }
    }

    fn read_trb(&self, addr: u64) -> Option<Trb> {
        let ptr = self.mem.read_u64(addr).ok()?;
        let status = self.mem.read_u32(addr + 8).ok()?;
        let control = self.mem.read_u32(addr + 12).ok()?;
        Some(Trb {
            ptr,
            status,
            control,
        })
    }

    fn write_trb(&self, addr: u64, trb: &Trb) -> bool {
        self.mem.write(addr, &trb.ptr.to_le_bytes()).is_ok()
            && self.mem.write(addr + 8, &trb.status.to_le_bytes()).is_ok()
            && self
                .mem
                .write(addr + 12, &trb.control.to_le_bytes())
                .is_ok()
    }

    fn enqueue_event(&mut self, ptr: u64, status: u32, control: u32) {
        // ERST segment 0 only; local producer index + cycle.
        if self.erstsz == 0 || self.erstba == 0 {
            return;
        }
        let seg_base = match self.mem.read_u64(self.erstba) {
            Ok(v) => v & !0x3f,
            Err(_) => return,
        };
        let seg_size = match self.mem.read_u16(self.erstba + 8) {
            Ok(v) => v as u64,
            Err(_) => return,
        };
        if seg_size == 0 {
            return;
        }
        if self.er_enqueue >= seg_size {
            self.er_enqueue = 0;
        }
        if seg_base == 0 {
            return;
        }
        let addr = seg_base + self.er_enqueue * 16;
        let trb = Trb {
            ptr,
            status,
            control: control | (self.er_cycle as u32),
        };
        let _ = self.write_trb(addr, &trb);
        self.er_enqueue += 1;
        if self.er_enqueue >= seg_size {
            self.er_enqueue = 0;
            self.er_cycle = !self.er_cycle;
        }
        self.raise_irq();
    }

    fn post_cmd_completion(&mut self, cmd_trb_addr: u64, slot_id: u32, cc: u32, vf: u32) {
        // Command Completion Event
        let status = (cc << 24) | (vf & 0xff);
        let control = (slot_id << 24) | (TRB_CMD_COMPLETE << 10);
        self.enqueue_event(cmd_trb_addr, status, control);
    }

    fn post_transfer_event(
        &mut self,
        trb_addr: u64,
        transfer_len: u32,
        cc: u32,
        slot: u32,
        epid: u32,
    ) {
        // Transfer Event: ED must be 0 when ptr is the TRB address (xHCI §6.4.2.1).
        crate::device_trace!(
            "[XHCI] XFER-EVENT slot={slot} epid={epid} trb={trb_addr:#x} rem={transfer_len} cc={cc}"
        );
        let status = (cc << 24) | (transfer_len & 0xffffff);
        let control = (slot << 24) | (epid << 16) | (TRB_TRANSFER_EVENT << 10);
        self.enqueue_event(trb_addr, status, control);
    }

    /// One event per TD: prefer the trailing LINK address (last TRB of the TD),
    /// which FreeBSD always includes in its td_self..sizeof(td_trb) match window.
    fn post_td_event(
        &mut self,
        stage_addr: u64,
        after_stage: u64,
        cycle: bool,
        rem: u32,
        cc: u32,
        slot: u32,
        epid: u32,
    ) -> u64 {
        // Event pointer must fall inside FreeBSD's TD (td_self .. td_trb[]).
        // Use the stage TRB address (offset 0) — always valid. Then advance past
        // the trailing LINK onto the next TD / ring slot.
        self.post_transfer_event(stage_addr, rem, cc, slot, epid);
        let mut dequeue = after_stage;
        if let Some(trb) = self.read_trb(after_stage) {
            if trb.cycle() == cycle && trb.type_() == TRB_LINK {
                dequeue = trb.ptr & !0xf;
            }
        }
        dequeue
    }

    fn process_command_ring(&mut self) {
        if !self.running() || self.cmd_ring == 0 {
            return;
        }
        loop {
            let addr = self.cmd_ring;
            let Some(trb) = self.read_trb(addr) else {
                break;
            };
            if trb.cycle() != self.cmd_cycle {
                break;
            }
            let t = trb.type_();
            match t {
                TRB_LINK => {
                    let tc = trb.control & (1 << 1) != 0;
                    self.cmd_ring = trb.ptr & !0xf;
                    if tc {
                        self.cmd_cycle = !self.cmd_cycle;
                    }
                    continue;
                }
                TRB_ENABLE_SLOT => {
                    let mut slot_id = 0u32;
                    for i in 1..=MAX_SLOTS {
                        if !self.slots[i as usize].enabled {
                            self.slots[i as usize] = SlotState {
                                enabled: true,
                                ..Default::default()
                            };
                            slot_id = i;
                            break;
                        }
                    }
                    let cc = if slot_id != 0 {
                        CC_SUCCESS
                    } else {
                        CC_TRB_ERROR
                    };
                    self.post_cmd_completion(addr, slot_id, cc, 0);
                }
                TRB_DISABLE_SLOT => {
                    let sid = trb.slot_id() as usize;
                    if sid > 0 && sid <= MAX_SLOTS as usize {
                        self.slots[sid] = SlotState::default();
                        self.setup[sid] = None;
                    }
                    self.post_cmd_completion(addr, trb.slot_id(), CC_SUCCESS, 0);
                }
                TRB_ADDRESS_DEVICE => {
                    let sid = trb.slot_id() as usize;
                    let input_ctx = trb.ptr & !0xf;
                    let bsr = trb.control & (1 << 9) != 0;
                    let cc = self.cmd_address_device(sid, input_ctx, bsr);
                    self.post_cmd_completion(addr, trb.slot_id(), cc, 0);
                }
                TRB_CONFIGURE_EP | TRB_EVALUATE_CTX => {
                    let sid = trb.slot_id() as usize;
                    let input_ctx = trb.ptr & !0xf;
                    let cc = self.cmd_configure_ep(sid, input_ctx, t == TRB_EVALUATE_CTX);
                    self.post_cmd_completion(addr, trb.slot_id(), cc, 0);
                }
                TRB_RESET_EP | TRB_STOP_EP => {
                    self.post_cmd_completion(addr, trb.slot_id(), CC_SUCCESS, 0);
                }
                TRB_SET_TR_DEQUEUE => {
                    let sid = trb.slot_id() as usize;
                    let epid = trb.epid() as usize;
                    if sid > 0 && sid <= MAX_SLOTS as usize && epid > 0 && epid < 31 {
                        let dq = trb.ptr;
                        self.slots[sid].eps[epid - 1].dequeue = dq & !0xf;
                        self.slots[sid].eps[epid - 1].cycle = dq & 1 != 0;
                        self.slots[sid].eps[epid - 1].configured = true;
                        // xHCI: update TR Dequeue Pointer in the output EP context.
                        let mut dcba = self.slots[sid].device_ctx;
                        if dcba == 0 && self.dcbaap != 0 {
                            dcba = self.mem.read_u64(self.dcbaap + sid as u64 * 8).unwrap_or(0);
                            self.slots[sid].device_ctx = dcba;
                        }
                        if dcba != 0 {
                            let ep_off = dcba + (epid as u64) * 0x40;
                            // dw2/dw3 of EP context hold TR Dequeue Pointer (64-bit).
                            // Preserve DCS (bit 0) from the command.
                            let _ = self.mem.write(ep_off + 0x08, &(dq & !0xe).to_le_bytes());
                            // EP State = Running (1)
                            if let Ok(mut s0) = self.mem.read_u32(ep_off) {
                                s0 = (s0 & !0x7) | 1;
                                let _ = self.mem.write(ep_off, &s0.to_le_bytes());
                            }
                        }
                        crate::device_trace!(
                            "[XHCI] Set TR Dequeue slot={sid} epid={epid} dq={:#x} dcba={dcba:#x}",
                            dq & !0xf
                        );
                    } else {
                        crate::host_log!("[XHCI] Set TR Dequeue BAD sid={sid} epid={epid}");
                    }
                    self.post_cmd_completion(addr, trb.slot_id(), CC_SUCCESS, 0);
                }
                TRB_RESET_DEVICE => {
                    let sid = trb.slot_id() as usize;
                    if sid > 0 && sid <= MAX_SLOTS as usize {
                        for ep in &mut self.slots[sid].eps {
                            *ep = EndpointState::default();
                        }
                        self.slots[sid].addressed = false;
                    }
                    self.post_cmd_completion(addr, trb.slot_id(), CC_SUCCESS, 0);
                }
                TRB_NOOP_CMD => {
                    self.post_cmd_completion(addr, 0, CC_SUCCESS, 0);
                }
                other => {
                    crate::host_log!("[XHCI] unhandled command TRB type {other}");
                    self.post_cmd_completion(addr, trb.slot_id(), CC_TRB_ERROR, 0);
                }
            }
            self.cmd_ring = addr + 16;
        }
    }

    fn cmd_address_device(&mut self, sid: usize, input_ctx: u64, bsr: bool) -> u32 {
        if sid == 0 || sid > MAX_SLOTS as usize {
            return CC_TRB_ERROR;
        }
        // Input Control Context + Slot + EP0 at input_ctx
        // Read slot context (offset 0x40) for root hub port number
        let slot_ctx0 = self.mem.read_u32(input_ctx + 0x40).unwrap_or(0);
        let slot_ctx1 = self.mem.read_u32(input_ctx + 0x44).unwrap_or(0);
        let port = ((slot_ctx1 >> 16) & 0xff) as u8;
        let ep0_ctx0 = self.mem.read_u32(input_ctx + 0x40 + 0x40).unwrap_or(0);
        let ep0_ctx2 = self
            .mem
            .read_u64(input_ctx + 0x40 + 0x40 + 0x08)
            .unwrap_or(0);

        // Device context array entry
        let dcba_entry = match self.mem.read_u64(self.dcbaap + (sid as u64) * 8) {
            Ok(v) => v,
            Err(e) => {
                crate::host_log!(
                    "[XHCI] Address Device slot={sid}: DCBAA read err at {:#x}: {e}",
                    self.dcbaap
                );
                return CC_TRB_ERROR;
            }
        };
        if dcba_entry == 0 {
            crate::host_log!(
                "[XHCI] Address Device slot={sid}: DCBAA[{sid}]=0 (dcbaap={:#x})",
                self.dcbaap
            );
            return CC_TRB_ERROR;
        }

        // Write output slot + EP0 contexts (simplified)
        let out_slot0 = slot_ctx0;
        // Slot State = Addressed (2) or Default (1) if BSR
        // Context Entries lives in DW0; Slot State and USB address live in DW3.
        let _ = self.mem.write(dcba_entry, &out_slot0.to_le_bytes());
        let _ = self.mem.write(dcba_entry + 4, &slot_ctx1.to_le_bytes());
        let _ = self.mem.write(dcba_entry + 8, &0u32.to_le_bytes());
        let _ = self.mem.write(
            dcba_entry + 12,
            &(if bsr {
                1u32 << 27
            } else {
                (2u32 << 27) | sid as u32
            })
            .to_le_bytes(),
        );

        // EP0 output at +0x40
        let mut ep0_out0 = ep0_ctx0;
        ep0_out0 = (ep0_out0 & !(0x7 << 0)) | (1 << 0); // EP State Running
        let _ = self.mem.write(dcba_entry + 0x40, &ep0_out0.to_le_bytes());
        let ep0_ctx1 = self.mem.read_u32(input_ctx + 0x40 + 0x40 + 4).unwrap_or(0);
        let _ = self.mem.write(dcba_entry + 0x44, &ep0_ctx1.to_le_bytes());
        let _ = self.mem.write(dcba_entry + 0x48, &ep0_ctx2.to_le_bytes());
        let ep0_ctx3 = self.mem.read_u32(input_ctx + 0x40 + 0x40 + 12).unwrap_or(0);
        let _ = self.mem.write(dcba_entry + 0x4c, &ep0_ctx3.to_le_bytes());

        self.slots[sid].enabled = true;
        self.slots[sid].addressed = !bsr;
        self.slots[sid].port = if port == 0 { 1 } else { port };
        self.slots[sid].device_ctx = dcba_entry;
        self.slots[sid].eps[0] = EndpointState {
            dequeue: ep0_ctx2 & !0xf,
            cycle: ep0_ctx2 & 1 != 0,
            configured: true,
        };
        crate::device_trace!(
            "[XHCI] Address Device slot={sid} bsr={bsr} port={} dq={:#x} dcba={:#x}",
            self.slots[sid].port,
            ep0_ctx2 & !0xf,
            dcba_entry
        );
        CC_SUCCESS
    }

    fn cmd_configure_ep(&mut self, sid: usize, input_ctx: u64, evaluate: bool) -> u32 {
        if sid == 0 || sid > MAX_SLOTS as usize || !self.slots[sid].enabled {
            crate::host_log!(
                "[XHCI] ConfigureEP REJECT sid={sid} enabled={}",
                sid <= MAX_SLOTS as usize
                    && self.slots.get(sid).map(|s| s.enabled).unwrap_or(false)
            );
            return CC_TRB_ERROR;
        }
        // Input Control Context: Dword0 = Drop Flags, Dword1 = Add Flags (xHCI 6.2.5.1)
        let drop = self.mem.read_u32(input_ctx).unwrap_or(0);
        let add = self.mem.read_u32(input_ctx + 4).unwrap_or(0);
        crate::device_trace!("[XHCI] ConfigureEP slot={sid} drop={drop:#x} add={add:#x}");
        let dcba = self.slots[sid].device_ctx;
        if dcba == 0 {
            return CC_TRB_ERROR;
        }

        // 64-byte contexts (CSZ=1):
        // Input: ICC@0, Slot@0x40, EP(DCI)@(DCI+1)*0x40
        // Output: Slot@0, EP(DCI)@DCI*0x40
        const CSZ: u64 = 0x40;
        for epid in 1..32u32 {
            let bit = 1u32 << epid;
            if !evaluate && drop & bit != 0 && (add & bit == 0) {
                // EP0 (epid=1) is owned by Address Device; ignore lone drops of it.
                if epid == 1 {
                    crate::device_trace!("[XHCI] ConfigureEP ignore lone drop of EP0 slot={sid}");
                } else {
                    crate::device_trace!("[XHCI] ConfigureEP drop slot={sid} epid={epid}");
                    self.slots[sid].eps[(epid - 1) as usize] = EndpointState::default();
                }
            }
            if add & bit != 0 {
                // Evaluate Context updates parameters, never the running ring.
                // FreeBSD uses it to update EP0's max packet size after reading
                // descriptors. Reloading its original dequeue replays old TDs.
                if evaluate {
                    if epid == 1 {
                        let input = input_ctx + 2 * CSZ + 4;
                        let output = dcba + CSZ + 4;
                        let old = self.mem.read_u32(output).unwrap_or(0);
                        let new = self.mem.read_u32(input).unwrap_or(0);
                        let _ = self
                            .mem
                            .write_u32(output, (old & 0xffff) | (new & 0xffff0000));
                    }
                    continue;
                }
                let in_off = input_ctx + (epid as u64 + 1) * CSZ;
                let out_off = dcba + (epid as u64) * CSZ;
                let ctx0 = self.mem.read_u32(in_off).unwrap_or(0);
                let ctx1 = self.mem.read_u32(in_off + 4).unwrap_or(0);
                let ctx2 = self.mem.read_u64(in_off + 8).unwrap_or(0);
                let ctx3 = self.mem.read_u32(in_off + 12).unwrap_or(0);
                let mut out0 = ctx0;
                out0 = (out0 & !0x7) | 1; // Running
                let _ = self.mem.write(out_off, &out0.to_le_bytes());
                let _ = self.mem.write(out_off + 4, &ctx1.to_le_bytes());
                let _ = self.mem.write(out_off + 8, &ctx2.to_le_bytes());
                let _ = self.mem.write(out_off + 12, &ctx3.to_le_bytes());
                self.slots[sid].eps[(epid - 1) as usize] = EndpointState {
                    dequeue: ctx2 & !0xf,
                    cycle: ctx2 & 1 != 0,
                    configured: true,
                };
                crate::device_trace!(
                    "[XHCI] ConfigureEP add slot={sid} epid={epid} dq={:#x}",
                    ctx2 & !0xf
                );
            }
        }
        // Copy Slot Context when Add Context Flags bit 0 is set
        if !evaluate && add & 1 != 0 {
            let slot_in = input_ctx + CSZ;
            for i in 0..3u64 {
                if let Ok(w) = self.mem.read_u32(slot_in + i * 4) {
                    let _ = self.mem.write(dcba + i * 4, &w.to_le_bytes());
                }
            }
        }

        // Slot state Configured
        if !evaluate {
            let s3 = self.mem.read_u32(dcba + 12).unwrap_or(0);
            let _ = self
                .mem
                .write_u32(dcba + 12, (s3 & !(0x1f << 27)) | (3 << 27));
        }
        CC_SUCCESS
    }

    fn port_kind(&self, port: u8) -> HidKind {
        if port as usize == PORT_TABLET {
            HidKind::Tablet
        } else {
            HidKind::Keyboard
        }
    }

    fn doorbell(&mut self, slot: u32, db_target: u32) {
        if !self.running() {
            return;
        }
        if slot == 0 {
            // Host controller command doorbell
            self.process_command_ring();
            return;
        }
        if slot > MAX_SLOTS {
            return;
        }
        let epid = db_target & 0x1f;
        if epid == 0 {
            return;
        }
        crate::device_trace!("[XHCI] doorbell slot={slot} epid={epid}");
        self.process_endpoint(slot as usize, epid as usize);
    }

    fn resync_ep0_from_device_ctx(&mut self, sid: usize) -> bool {
        if sid == 0 || sid > MAX_SLOTS as usize {
            return false;
        }
        if self.slots[sid].device_ctx == 0 && self.dcbaap != 0 {
            if let Ok(ptr) = self.mem.read_u64(self.dcbaap + sid as u64 * 8) {
                self.slots[sid].device_ctx = ptr;
            }
        }
        let dcba = self.slots[sid].device_ctx;
        if dcba == 0 {
            return false;
        }
        let Ok(tr) = self.mem.read_u64(dcba + 0x48) else {
            return false;
        };
        if tr & !0xf == 0 {
            return false;
        }
        self.slots[sid].eps[0] = EndpointState {
            dequeue: tr & !0xf,
            cycle: tr & 1 != 0,
            configured: true,
        };
        crate::device_trace!("[XHCI] resync EP0 slot={sid} dq={:#x}", tr & !0xf);
        true
    }

    fn process_endpoint(&mut self, sid: usize, epid: usize) {
        if sid == 0 || sid > MAX_SLOTS as usize || epid == 0 || epid > 31 {
            return;
        }
        if !self.slots[sid].eps[epid - 1].configured {
            if epid == 1 && self.slots[sid].enabled {
                let _ = self.resync_ep0_from_device_ctx(sid);
            }
            if !self.slots[sid].eps[epid - 1].configured {
                crate::device_trace!("[XHCI] doorbell slot={sid} epid={epid} ignored (EP not configured)");
                return;
            }
        }
        let kind = self.port_kind(self.slots[sid].port);

        // Interrupt IN endpoint 3 (EPID 3 = IN EP1): HID reports
        if epid == 3 {
            self.process_intr_in(sid, epid, kind);
            return;
        }

        // Control EP0 (epid 1)
        if epid == 1 {
            self.process_control(sid, kind);
        }
    }

    fn process_intr_in(&mut self, sid: usize, epid: usize, kind: HidKind) {
        if self
            .pending_intr
            .iter()
            .any(|p| p.slot as usize == sid && p.epid as usize == epid)
        {
            return;
        }
        let mut dequeue = self.slots[sid].eps[epid - 1].dequeue;
        let mut cycle = self.slots[sid].eps[epid - 1].cycle;
        loop {
            let Some(trb) = self.read_trb(dequeue) else {
                break;
            };
            if trb.cycle() != cycle {
                break;
            }
            if trb.type_() == TRB_LINK {
                let tc = trb.control & (1 << 1) != 0;
                dequeue = trb.ptr & !0xf;
                if tc {
                    cycle = !cycle;
                }
                continue;
            }
            if trb.type_() != TRB_NORMAL && trb.type_() != TRB_EVENT_DATA {
                dequeue += 16;
                continue;
            }
            if trb.type_() == TRB_EVENT_DATA {
                dequeue += 16;
                continue;
            }

            let buf = trb.ptr;
            let len = trb.transfer_len();
            let ioc = trb.ioc();
            let trb_addr = dequeue;

            let report: Option<Vec<u8>> = match kind {
                HidKind::Keyboard => self.hid.take_keyboard_report().map(|r| r.to_vec()),
                HidKind::Tablet => self.hid.take_tablet_report().map(|r| r.to_vec()),
            };

            if let Some(rep) = report {
                let n = (len as usize).min(rep.len());
                let _ = self.mem.write(buf, &rep[..n]);
                let residual = len.saturating_sub(n as u32);
                let cc = if residual != 0 {
                    CC_SHORT_PACKET
                } else {
                    CC_SUCCESS
                };
                if ioc {
                    self.post_transfer_event(trb_addr, residual, cc, sid as u32, epid as u32);
                }
                dequeue += 16;
            } else {
                self.pending_intr.push_back(PendingIntr {
                    slot: sid as u8,
                    epid: epid as u8,
                    trb_addr,
                    buf,
                    len,
                    ioc,
                });
                self.slots[sid].eps[epid - 1].dequeue = dequeue;
                self.slots[sid].eps[epid - 1].cycle = cycle;
                return;
            }
        }
        self.slots[sid].eps[epid - 1].dequeue = dequeue;
        self.slots[sid].eps[epid - 1].cycle = cycle;
    }

    fn try_complete_pending_intr(&mut self) {
        let mut pending = std::mem::take(&mut self.pending_intr);
        while let Some(p) = pending.pop_front() {
            let kind = self.port_kind(self.slots[p.slot as usize].port);
            let report: Option<Vec<u8>> = match kind {
                HidKind::Keyboard => {
                    if self.hid.has_keyboard_report() {
                        self.hid.take_keyboard_report().map(|r| r.to_vec())
                    } else {
                        None
                    }
                }
                HidKind::Tablet => {
                    if self.hid.has_tablet_report() {
                        self.hid.take_tablet_report().map(|r| r.to_vec())
                    } else {
                        None
                    }
                }
            };
            if let Some(rep) = report {
                let n = (p.len as usize).min(rep.len());
                let _ = self.mem.write(p.buf, &rep[..n]);
                let residual = p.len.saturating_sub(n as u32);
                let cc = if residual != 0 {
                    CC_SHORT_PACKET
                } else {
                    CC_SUCCESS
                };
                if p.ioc {
                    self.post_transfer_event(
                        p.trb_addr,
                        residual,
                        cc,
                        p.slot as u32,
                        p.epid as u32,
                    );
                }
                // Advance dequeue past this TRB
                let ep = &mut self.slots[p.slot as usize].eps[p.epid as usize - 1];
                if ep.dequeue == p.trb_addr {
                    ep.dequeue = p.trb_addr + 16;
                }
                // Kick again in case more TRBs / reports
                let sid = p.slot as usize;
                let epid = p.epid as usize;
                let k = kind;
                self.process_intr_in(sid, epid, k);
            } else {
                self.pending_intr.push_back(p);
            }
        }
    }

    fn process_control(&mut self, sid: usize, kind: HidKind) {
        // FreeBSD may split Setup/Data/Status across doorbells ("stepped" TDs):
        // after Setup completes it activates Data and rings again with EP dequeue
        // already pointing at a Data or Status TRB — handle that continuation.
        for _guard in 0..4 {
            if !self.process_control_once(sid, kind) {
                break;
            }
        }
    }

    /// Process one control stage starting at the current EP0 dequeue.
    /// Returns true if more stages might be ready on the ring (keep going).
    fn process_control_once(&mut self, sid: usize, kind: HidKind) -> bool {
        let mut dequeue = self.slots[sid].eps[0].dequeue;
        let mut cycle = self.slots[sid].eps[0].cycle;

        let Some(first) = self.read_trb(dequeue) else {
            crate::host_log!("[XHCI] EP0 slot={sid} read_trb fail dq={dequeue:#x}");
            return false;
        };
        if first.cycle() != cycle {
            // Empty ring slot (producer cycle not set yet) — normal between transfers.
            crate::device_trace!(
                "[XHCI] EP0 slot={sid} idle dq={dequeue:#x} want_cycle={cycle} trb_cycle={}",
                first.cycle()
            );
            return false;
        }
        if first.type_() == TRB_LINK {
            let tc = first.control & (1 << 1) != 0;
            dequeue = first.ptr & !0xf;
            if tc {
                cycle = !cycle;
            }
            self.slots[sid].eps[0].dequeue = dequeue;
            self.slots[sid].eps[0].cycle = cycle;
            // Keep going: LINK target may be Setup/Data/Status of this transfer.
            return true;
        }

        match first.type_() {
            TRB_SETUP => self.control_setup_stage(sid, kind, dequeue, cycle),
            TRB_DATA | TRB_NORMAL => {
                self.control_data_status_stages(sid, kind, dequeue, cycle, None)
            }
            TRB_STATUS => self.control_status_only(sid, dequeue, cycle),
            other => {
                crate::host_log!("[XHCI] EP0 slot={sid} unexpected TRB type={other} dq={dequeue:#x}");
                false
            }
        }
    }

    fn control_setup_stage(
        &mut self,
        sid: usize,
        kind: HidKind,
        setup_addr: u64,
        cycle: bool,
    ) -> bool {
        let setup_trb = self.read_trb(setup_addr).unwrap();
        let setup_bytes = setup_trb.ptr.to_le_bytes();
        let bm = setup_bytes[0];
        let breq = setup_bytes[1];
        let wvalue = u16::from_le_bytes([setup_bytes[2], setup_bytes[3]]);
        let windex = u16::from_le_bytes([setup_bytes[4], setup_bytes[5]]);
        let wlength = u16::from_le_bytes([setup_bytes[6], setup_bytes[7]]);

        let mut dequeue = setup_addr + 16;

        // Always complete the Setup TD first when FreeBSD stepped later stages.
        // Peek whether Data/Status are already owned (cycle match).
        let peek = self.read_trb(dequeue);
        let next_ready = peek
            .as_ref()
            .map(|t| t.cycle() == cycle && t.type_() != TRB_SETUP)
            .unwrap_or(false);

        if !next_ready {
            // Stepped: only Setup is visible.
            crate::device_trace!(
                "[XHCI] EP0 slot={sid} port={} {kind:?} SETUP-only bm={bm:#02x} req={breq:#02x} val={wvalue:#06x} idx={windex:#06x} len={wlength} setup={setup_addr:#x} ioc={}",
                self.slots[sid].port,
                setup_trb.ioc()
            );
            // Always complete Setup TD (event at LINK if present).
            dequeue = self.post_td_event(setup_addr, dequeue, cycle, 0, CC_SUCCESS, sid as u32, 1);
            // post_td_event already followed LINK to the next TD.
            self.slots[sid].eps[0].dequeue = dequeue;
            self.slots[sid].eps[0].cycle = cycle;
            // Stash pending SETUP so the next doorbell (Data) knows the request.
            self.setup[sid] = Some(SetupState {
                bm_request_type: bm,
                b_request: breq,
                w_value: wvalue,
                w_index: windex,
                w_length: wlength,
                data_buf: 0,
                data_len: 0,
            });
            return false;
        }

        // Data/Status already on the ring — one event per TD (Setup, Data, Status).
        // Always emit Setup completion; FreeBSD matches td_self..LINK.
        dequeue = self.post_td_event(setup_addr, dequeue, cycle, 0, CC_SUCCESS, sid as u32, 1);
        self.slots[sid].eps[0].dequeue = dequeue;
        self.slots[sid].eps[0].cycle = cycle;
        self.setup[sid] = Some(SetupState {
            bm_request_type: bm,
            b_request: breq,
            w_value: wvalue,
            w_index: windex,
            w_length: wlength,
            data_buf: 0,
            data_len: 0,
        });
        self.control_data_status_stages(sid, kind, dequeue, cycle, None)
    }

    fn control_status_only(&mut self, sid: usize, status_addr: u64, mut cycle: bool) -> bool {
        let trb = self.read_trb(status_addr).unwrap();
        crate::device_trace!(
            "[XHCI] EP0 slot={sid} STATUS-only trb={status_addr:#x} ioc={}",
            trb.ioc()
        );
        self.post_transfer_event(status_addr, 0, CC_SUCCESS, sid as u32, 1);
        let mut dequeue = status_addr + 16;
        // FreeBSD TDs end with a LINK TRB back onto the EP ring — advance past it.
        dequeue = self.consume_trailing_link(dequeue, &mut cycle);
        self.slots[sid].eps[0].dequeue = dequeue;
        self.slots[sid].eps[0].cycle = cycle;
        self.setup[sid] = None;
        false // transfer complete; next SETUP arrives on a later doorbell
    }

    /// If `dequeue` points at a LINK with matching cycle, follow it once and return the target.
    fn consume_trailing_link(&self, dequeue: u64, cycle: &mut bool) -> u64 {
        let Some(trb) = self.read_trb(dequeue) else {
            return dequeue;
        };
        if trb.cycle() != *cycle || trb.type_() != TRB_LINK {
            return dequeue;
        }
        let tc = trb.control & (1 << 1) != 0;
        let next = trb.ptr & !0xf;
        if tc {
            *cycle = !*cycle;
        }
        next
    }

    fn control_data_status_stages(
        &mut self,
        sid: usize,
        kind: HidKind,
        mut dequeue: u64,
        mut cycle: bool,
        _unused: Option<()>,
    ) -> bool {
        let (bm, breq, wvalue, windex, wlength) = match self.setup[sid] {
            Some(s) => (
                s.bm_request_type,
                s.b_request,
                s.w_value,
                s.w_index,
                s.w_length,
            ),
            None => {
                crate::host_log!("[XHCI] EP0 slot={sid} data/status without pending SETUP");
                return false;
            }
        };

        let mut data_buf = 0u64;
        let mut data_len = 0u32;
        let mut data_trb_addr = 0u64;
        let mut status_trb_addr = 0u64;
        let mut status_ioc = false;
        let mut data_ioc = false;
        let mut links = 0u32;

        for _ in 0..12 {
            let Some(trb) = self.read_trb(dequeue) else {
                break;
            };
            if trb.cycle() != cycle {
                break;
            }
            if trb.type_() == TRB_LINK {
                if links >= 6 {
                    break;
                }
                links += 1;
                let tc = trb.control & (1 << 1) != 0;
                dequeue = trb.ptr & !0xf;
                if tc {
                    cycle = !cycle;
                }
                continue;
            }
            match trb.type_() {
                TRB_DATA | TRB_NORMAL => {
                    if data_trb_addr == 0 {
                        data_buf = trb.ptr;
                        data_len = trb.transfer_len();
                        data_trb_addr = dequeue;
                    }
                    // IOC lives on the last TRB of the data TD.
                    data_ioc = trb.ioc();
                    dequeue += 16;
                    if trb.ioc() && !trb.chain() {
                        continue;
                    }
                }
                TRB_STATUS => {
                    status_trb_addr = dequeue;
                    status_ioc = trb.ioc();
                    dequeue += 16;
                    break;
                }
                TRB_SETUP => break,
                _ => {
                    dequeue += 16;
                }
            }
        }

        crate::device_trace!(
            "[XHCI] EP0 slot={sid} port={} {kind:?} bm={bm:#02x} req={breq:#02x} val={wvalue:#06x} idx={windex:#06x} len={wlength} data_trb={data_trb_addr:#x} status_trb={status_trb_addr:#x} data_buf={data_buf:#x} data_len={data_len} data_ioc={data_ioc} status_ioc={status_ioc}",
            self.slots[sid].port
        );

        // If only Data is visible (Status stepped), complete Data and wait.
        if data_trb_addr == 0 && status_trb_addr == 0 {
            crate::device_trace!("[XHCI] EP0 slot={sid} no data/status TRBs yet");
            return false;
        }

        let resp = hid::handle_control(kind, bm, breq, wvalue, windex, wlength);
        match resp {
            Some(bytes) => {
                if data_trb_addr != 0 {
                    let mut rem = 0u32;
                    let mut cc = CC_SUCCESS;
                    if data_buf != 0 && (bm & 0x80) != 0 {
                        let n = if bytes.is_empty() {
                            0
                        } else {
                            (data_len as usize).min(bytes.len())
                        };
                        if n > 0 {
                            if let Err(e) = self.mem.write(data_buf, &bytes[..n]) {
                                crate::host_log!(
                                    "[XHCI] EP0 DATA write failed buf={data_buf:#x} n={n}: {e}"
                                );
                            } else {
                                crate::device_trace!("[XHCI] EP0 DATA wrote {n} bytes to {data_buf:#x}");
                            }
                        }
                        rem = data_len.saturating_sub(n as u32);
                        cc = if rem != 0 {
                            CC_SHORT_PACKET
                        } else {
                            CC_SUCCESS
                        };
                    }
                    // Event at Data TD's LINK; advances dequeue to Status TD.
                    let after_data = data_trb_addr + 16;
                    dequeue = self.post_td_event(
                        data_trb_addr,
                        after_data,
                        cycle,
                        rem,
                        cc,
                        sid as u32,
                        1,
                    );
                    let _ = (data_ioc, status_ioc);
                }

                if status_trb_addr != 0 {
                    let after_status = status_trb_addr + 16;
                    dequeue = self.post_td_event(
                        status_trb_addr,
                        after_status,
                        cycle,
                        0,
                        CC_SUCCESS,
                        sid as u32,
                        1,
                    );
                    // Status LINK may have Toggle Cycle back onto the EP ring.
                    if let Some(trb) = self.read_trb(status_trb_addr + 16) {
                        if trb.cycle() == cycle && trb.type_() == TRB_LINK {
                            if trb.control & (1 << 1) != 0 {
                                cycle = !cycle;
                            }
                        }
                    }
                    self.setup[sid] = None;
                }
            }
            None => {
                let stall_addr = if status_trb_addr != 0 {
                    status_trb_addr
                } else if data_trb_addr != 0 {
                    data_trb_addr
                } else {
                    self.slots[sid].eps[0].dequeue
                };
                self.post_transfer_event(stall_addr, 0, CC_STALL, sid as u32, 1);
                self.setup[sid] = None;
            }
        }

        self.slots[sid].eps[0].dequeue = dequeue;
        self.slots[sid].eps[0].cycle = cycle;
        // Status done ⇒ transfer complete (do not probe the next empty ring slot).
        // Status missing ⇒ Data-only for now; wait for the next doorbell.
        false
    }

    fn cap_read(&self, off: u64, size: usize) -> u64 {
        // Combine CAPLENGTH + HCIVERSION at 0
        let val = match off & !0x3 {
            0x00 => 0x0100_0000 | (CAPLENGTH as u32), // HCIVERSION 1.0 | CAPLENGTH
            0x04 => {
                // MaxSlots | MaxIntrs<<8 | MaxPorts<<24
                MAX_SLOTS | (MAX_INTRS << 8) | (MAX_PORTS << 24)
            }
            0x08 => 0x0000_000f, // IST etc.
            0x0c => 0,
            0x10 => {
                // AC64 | CSZ=1 (64-byte contexts) | MaxPSASize=0 | xECP at 0x20/4=8
                // Must match Address/Configure Endpoint strides (0x40).
                0x0008_0005
            }
            0x14 => OFF_DOORBELL as u32,
            0x18 => OFF_RUNTIME as u32,
            0x1c => 0,
            // Extended capability: supported protocol USB2
            0x20 => 0x0200_0002, // next=0, major=2, minor=0, ID=2 (supported protocol)
            0x24 => 0x2042_5355, // "USB "
            0x28 => 0x0000_0001 | ((MAX_PORTS as u32) << 8), // ports 1..N
            0x2c => 0,
            _ => 0,
        };
        extract(val as u64, off, size)
    }

    fn oper_read(&self, off: u64, size: usize) -> u64 {
        let val = match off {
            0x00 => self.usbcmd,
            0x04 => self.usbsts,
            0x08 => self.pagesize,
            0x14 => self.dnctrl,
            0x18 => (self.crcr & 0xffff_ffff) as u32,
            0x1c => (self.crcr >> 32) as u32,
            0x30 => (self.dcbaap & 0xffff_ffff) as u32,
            0x34 => (self.dcbaap >> 32) as u32,
            0x38 => self.config,
            o if (0x400..0x400 + MAX_PORTS as u64 * 0x10).contains(&o) => {
                let port = ((o - 0x400) / 0x10) as usize;
                let reg = (o - 0x400) % 0x10;
                if reg == 0 && port < MAX_PORTS as usize {
                    self.portsc[port]
                } else {
                    0
                }
            }
            _ => 0,
        };
        extract(val as u64, off, size)
    }

    fn oper_write(&mut self, off: u64, data: u64, size: usize) {
        if size == 8 {
            match off {
                0x18 => {
                    self.crcr = data & !0x3f;
                    self.cmd_cycle = data & 1 != 0;
                    self.cmd_ring = self.crcr & !0x3f;
                    return;
                }
                0x30 => {
                    self.dcbaap = data & !0x3f;
                    crate::device_trace!("[XHCI] DCBAAP={:#x}", self.dcbaap);
                    return;
                }
                _ => {}
            }
        }
        let data = data as u32;
        match off {
            0x00 => {
                if data & USBCMD_HCRST != 0 {
                    self.hcrst();
                    return;
                }
                let was_run = self.usbcmd & USBCMD_RS != 0;
                self.usbcmd = data & (USBCMD_RS | USBCMD_INTE | (1 << 3)/* HSEE */);
                if self.usbcmd & USBCMD_RS != 0 {
                    self.usbsts &= !USBSTS_HCH;
                    if !was_run {
                        // Announce port change
                        self.usbsts |= USBSTS_PCD;
                        for i in 0..MAX_PORTS as usize {
                            // Port Status Change Event: Port ID in control[31:24], ptr=0
                            self.portsc[i] |= PORTSC_CSC;
                            let port = (i as u32) + 1;
                            self.enqueue_event(
                                (port as u64) << 24,
                                CC_SUCCESS << 24,
                                TRB_PORT_STATUS << 10,
                            );
                        }
                    }
                } else {
                    self.usbsts |= USBSTS_HCH;
                }
            }
            0x04 => {
                // W1C
                self.usbsts &= !(data & (USBSTS_HSE | USBSTS_EINT | USBSTS_PCD));
                self.lower_irq_if_clear();
            }
            0x14 => self.dnctrl = data,
            0x18 => {
                self.crcr = (self.crcr & !0xffff_ffff) | (data as u64 & !0x3f);
                // bits: RCS=bit0
                self.cmd_cycle = data & 1 != 0;
                self.cmd_ring = self.crcr & !0x3f;
            }
            0x1c => {
                self.crcr = (self.crcr & 0xffff_ffff) | ((data as u64) << 32);
                self.cmd_ring = self.crcr & !0x3f;
            }
            0x30 => self.dcbaap = (self.dcbaap & !0xffff_ffff) | (data as u64 & !0x3f),
            0x34 => self.dcbaap = (self.dcbaap & 0xffff_ffff) | ((data as u64) << 32),
            0x38 => self.config = data & 0xff,
            o if (0x400..0x400 + MAX_PORTS as u64 * 0x10).contains(&o) => {
                let port = ((o - 0x400) / 0x10) as usize;
                let reg = (o - 0x400) % 0x10;
                if reg == 0 && port < MAX_PORTS as usize {
                    self.portsc_write(port, data);
                }
            }
            _ => {
                let _ = size;
            }
        }
    }

    fn portsc_write(&mut self, port: usize, val: u32) {
        let mut p = self.portsc[port];
        // W1C change bits
        for bit in [PORTSC_CSC, PORTSC_PEC, PORTSC_PRC] {
            if val & bit != 0 {
                p &= !bit;
            }
        }
        if val & PORTSC_PR != 0 {
            // Port reset
            p |= PORTSC_PR;
            // Complete reset immediately: PED=1, PRC=1, PR=0, CCS=1, HS
            p &= !PORTSC_PR;
            p |= PORTSC_PED | PORTSC_PRC | PORTSC_CCS | PORTSC_PP | PORTSC_SPEED_HS;
            self.portsc[port] = p;
            self.usbsts |= USBSTS_PCD;
            let port_num = (port as u32) + 1;
            self.enqueue_event(
                (port_num as u64) << 24,
                CC_SUCCESS << 24,
                TRB_PORT_STATUS << 10,
            );
            return;
        }
        // LWS + PLS writes ignored for HS always-U0
        if val & PORTSC_PP != 0 {
            p |= PORTSC_PP;
        }
        // PED clear?
        if val & PORTSC_PED != 0 {
            // writing 1 to PED is ignored (RW1C on some); keep enabled when connected
        }
        self.portsc[port] = p | PORTSC_CCS | PORTSC_SPEED_HS;
    }

    fn runtime_read(&self, off: u64, size: usize) -> u64 {
        // MFINDEX at 0
        if off < 0x20 {
            return 0;
        }
        let intr = (off - 0x20) / 0x20;
        let reg = (off - 0x20) % 0x20;
        if intr != 0 {
            return 0;
        }
        let val = match reg {
            0x00 => self.iman,
            0x04 => self.imod,
            0x08 => self.erstsz,
            0x10 => (self.erstba & 0xffff_ffff) as u32,
            0x14 => (self.erstba >> 32) as u32,
            0x18 => (self.erdp & 0xffff_ffff) as u32,
            0x1c => (self.erdp >> 32) as u32,
            _ => 0,
        };
        extract(val as u64, off, size)
    }

    fn runtime_write(&mut self, off: u64, data: u64, size: usize) {
        if off < 0x20 {
            return;
        }
        let intr = (off - 0x20) / 0x20;
        let reg = (off - 0x20) % 0x20;
        if intr != 0 {
            return;
        }
        let data32 = data as u32;
        match reg {
            0x00 => {
                // IMAN: IP is W1C; IE is RW.
                if data32 & IMAN_IP != 0 {
                    self.iman &= !IMAN_IP;
                }
                self.iman = (self.iman & !IMAN_IE) | (data32 & IMAN_IE);
                self.lower_irq_if_clear();
            }
            0x04 => self.imod = data32,
            0x08 => self.erstsz = data32 & 0xffff,
            0x10 => self.erstba = (self.erstba & !0xffff_ffff) | (data & !0x3f),
            0x14 => self.erstba = (self.erstba & 0xffff_ffff) | (data << 32),
            0x18 => {
                self.erdp = (self.erdp & !0xffff_ffff) | (data & !0xf);
                // EHB clear by writing 1 to bit 3
            }
            0x1c => self.erdp = (self.erdp & 0xffff_ffff) | (data << 32),
            _ => {
                let _ = size;
            }
        }
    }
}

fn extract(val: u64, offset: u64, size: usize) -> u64 {
    // `val` is the 32-bit (or 64-bit) register already selected for
    // `offset & !3`. Only the intra-register byte offset may shift.
    // Using `offset & 7` here wrongly shifted HCSPARAMS1 (at +0x04) by 32
    // bits → FreeBSD saw MaxPorts=0 ("Invalid number of ports: 0").
    let shift = (offset & 0x3) * 8;
    match size {
        1 => (val >> shift) & 0xff,
        2 => (val >> (shift & !1)) & 0xffff,
        4 | 8 => val & 0xffff_ffff,
        _ => val,
    }
}

impl MmioDevice for XhciController {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if offset < OFF_OPER {
            self.cap_read(offset, size)
        } else if offset < OFF_RUNTIME {
            self.oper_read(offset - OFF_OPER, size)
        } else if offset < OFF_DOORBELL {
            self.runtime_read(offset - OFF_RUNTIME, size)
        } else if offset < OFF_DOORBELL + 0x400 {
            0 // doorbells are write-only
        } else {
            0
        }
    }

    fn write(&mut self, offset: u64, data: u64, size: usize) {
        if offset < OFF_OPER {
            // caps mostly RO
        } else if offset < OFF_RUNTIME {
            self.oper_write(offset - OFF_OPER, data, size);
        } else if offset < OFF_DOORBELL {
            self.runtime_write(offset - OFF_RUNTIME, data, size);
        } else if offset < OFF_DOORBELL + (MAX_SLOTS as u64 + 1) * 4 {
            let slot = ((offset - OFF_DOORBELL) / 4) as u32;
            self.doorbell(slot, data as u32);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller() -> XhciController {
        XhciController::new(41, Arc::new(GuestMemory::new_test(0, 0x10000)), 1280, 720)
    }

    #[test]
    fn evaluate_context_preserves_live_ring_and_address() {
        let mut c = controller();
        c.dcbaap = 0x1000;
        c.mem.write_u64(0x1008, 0x2000).unwrap();
        c.mem.write_u32(0x3040, 1 << 27).unwrap(); // Context Entries
        c.mem.write_u32(0x3044, 1 << 16).unwrap(); // root port
        c.mem.write_u64(0x3088, 0x4001).unwrap();
        assert_eq!(c.cmd_address_device(1, 0x3000, false), CC_SUCCESS);
        assert_eq!(c.mem.read_u32(0x2000).unwrap(), 1 << 27);
        assert_eq!(c.mem.read_u32(0x200c).unwrap(), (2 << 27) | 1);
        c.slots[1].eps[0].dequeue = 0x4800;
        c.slots[1].eps[0].cycle = false;
        c.mem.write_u32(0x3004, 3).unwrap();
        c.mem.write_u32(0x3084, 64 << 16).unwrap();
        assert_eq!(c.cmd_configure_ep(1, 0x3000, true), CC_SUCCESS);
        assert_eq!(c.slots[1].eps[0].dequeue, 0x4800);
        assert!(!c.slots[1].eps[0].cycle);
        assert_eq!(c.mem.read_u32(0x2044).unwrap() >> 16, 64);
        assert_eq!(c.mem.read_u32(0x200c).unwrap(), (2 << 27) | 1);
    }

    #[test]
    fn port_event_places_port_id_in_parameter() {
        let mut c = controller();
        c.erstsz = 1;
        c.erstba = 0x1000;
        c.mem.write_u64(0x1000, 0x2000).unwrap();
        c.mem.write_u16(0x1008, 16).unwrap();
        c.portsc_write(1, PORTSC_PR);
        let event = c.read_trb(0x2000).unwrap();
        assert_eq!(event.ptr, 2 << 24);
        assert_eq!(event.type_(), TRB_PORT_STATUS);
        assert_eq!(event.control >> 24, 0);
    }

    #[test]
    fn repeated_doorbells_do_not_duplicate_pending_report() {
        let mut c = controller();
        c.slots[1].port = 1;
        c.slots[1].eps[2] = EndpointState {
            dequeue: 0x2000,
            cycle: true,
            configured: true,
        };
        c.write_trb(
            0x2000,
            &Trb {
                ptr: 0x3000,
                status: 8,
                control: (TRB_NORMAL << 10) | 1,
            },
        );
        c.process_intr_in(1, 3, HidKind::Keyboard);
        c.process_intr_in(1, 3, HidKind::Keyboard);
        assert_eq!(c.pending_intr.len(), 1);
        c.key_event(30, true);
        assert!(c.pending_intr.is_empty());
        let mut report = [0; 8];
        c.mem.read(0x3000, &mut report).unwrap();
        assert_eq!(report[2], 4);
    }
}
