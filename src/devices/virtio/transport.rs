//! Shared modern VirtIO MMIO register state.
//!
//! Devices keep their config space and queue processing. Feature negotiation,
//! status, interrupt status, queue selection, queue addresses, and reset live
//! here so every device applies the same rules.

use super::{
    VIRTIO_F_VERSION_1, VIRTIO_MMIO_DEVICE_FEATURES, VIRTIO_MMIO_DEVICE_FEATURES_SEL,
    VIRTIO_MMIO_DEVICE_ID, VIRTIO_MMIO_DRIVER_FEATURES, VIRTIO_MMIO_DRIVER_FEATURES_SEL,
    VIRTIO_MMIO_INTERRUPT_ACK, VIRTIO_MMIO_INTERRUPT_STATUS, VIRTIO_MMIO_MAGIC,
    VIRTIO_MMIO_MAGIC_VALUE, VIRTIO_MMIO_QUEUE_DESC_HIGH, VIRTIO_MMIO_QUEUE_DESC_LOW,
    VIRTIO_MMIO_QUEUE_DEVICE_HIGH, VIRTIO_MMIO_QUEUE_DEVICE_LOW, VIRTIO_MMIO_QUEUE_DRIVER_HIGH,
    VIRTIO_MMIO_QUEUE_DRIVER_LOW, VIRTIO_MMIO_QUEUE_NOTIFY, VIRTIO_MMIO_QUEUE_NUM,
    VIRTIO_MMIO_QUEUE_NUM_MAX, VIRTIO_MMIO_QUEUE_READY, VIRTIO_MMIO_QUEUE_SEL,
    VIRTIO_MMIO_SHM_LEN_HIGH, VIRTIO_MMIO_SHM_LEN_LOW, VIRTIO_MMIO_STATUS, VIRTIO_MMIO_VENDOR,
    VIRTIO_MMIO_VENDOR_ID, VIRTIO_MMIO_VERSION, VIRTIO_MMIO_VERSION_2, VIRTIO_STATUS_DRIVER_OK,
    VIRTIO_STATUS_FAILED, VIRTIO_STATUS_FEATURES_OK, VIRTIO_STATUS_NEEDS_RESET, VirtQueue,
};

/// What a transport register write asks the device to do.
/// Queue state has already been updated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteEffect {
    None,
    Notify {
        queue: u32,
    },
    Reset,
    QueueReset {
        queue: u32,
    },
    QueueReady {
        queue: u32,
    },
    InterruptAck {
        line_low: bool,
    },
    DriverOk,
    /// FEATURES_OK was refused, or DRIVER_OK arrived with a feature set the
    /// device will not run. Illegal bits are not kept.
    NegotiationFailed,
}

#[derive(Clone, Debug, Default)]
pub struct Diagnostics {
    pub notifies: u64,
    pub completions: u64,
    pub bytes: u64,
    pub drops: u64,
    pub malformed: u64,
    pub interrupts: u64,
    pub resets: u64,
}

#[derive(Clone)]
struct QueueSlot {
    queue: VirtQueue,
    epoch: u64,
    /// False after a rejected QUEUE_NUM write, until a legal size is written.
    size_accepted: bool,
}

/// Modern MMIO transport for one device. `queue_max` is the per-queue capacity
/// advertised in QueueNumMax. The initial size is that maximum so a driver
/// that writes the same value, and unit tests that program rings directly,
/// keep working. A later illegal QUEUE_NUM write is rejected rather than clamped.
pub struct MmioTransport {
    device_id: u32,
    offered: u64,
    driver_features: u64,
    device_sel: u32,
    driver_sel: u32,
    status: u32,
    interrupt_status: u32,
    queue_sel: u32,
    queues: Vec<QueueSlot>,
    irq: u32,
    /// FreeBSD 14.4 virtio_p9fs sets DRIVER_OK without FEATURES_OK.
    allow_driver_ok_without_features_ok: bool,
    /// EDK2 VirtioGpuDxe leaves QueueNum at zero on the modern MMIO path.
    /// Device models that opt into this compatibility quirk retain QueueNumMax.
    zero_queue_num_uses_max: bool,
    diag: Diagnostics,
}

impl MmioTransport {
    pub fn new(device_id: u32, offered: u64, queue_max: &[u32], irq: u32) -> Self {
        Self::with_policy(device_id, offered, queue_max, irq, false)
    }

    pub fn with_policy(
        device_id: u32,
        offered: u64,
        queue_max: &[u32],
        irq: u32,
        allow_driver_ok_without_features_ok: bool,
    ) -> Self {
        assert!(
            !queue_max.is_empty() && queue_max.len() <= 256,
            "virtio queue count out of range"
        );
        assert!(
            queue_max
                .iter()
                .all(|n| *n > 0 && n.is_power_of_two() && *n <= 32768),
            "virtio queue max must be a power of two in 1..=32768"
        );
        let queues = queue_max
            .iter()
            .map(|&max_size| QueueSlot {
                queue: VirtQueue {
                    max_size,
                    size: max_size,
                    ..VirtQueue::default()
                },
                epoch: 0,
                size_accepted: true,
            })
            .collect();
        Self {
            device_id,
            offered,
            driver_features: 0,
            device_sel: 0,
            driver_sel: 0,
            status: 0,
            interrupt_status: 0,
            queue_sel: 0,
            queues,
            irq,
            allow_driver_ok_without_features_ok,
            zero_queue_num_uses_max: false,
            diag: Diagnostics::default(),
        }
    }

    /// Accept a zero QueueNum write as "keep QueueNumMax" for a known broken
    /// driver. This is intentionally opt-in so normal devices still reject the
    /// invalid modern virtqueue size required by the VirtIO specification.
    pub fn enable_zero_queue_num_compat(&mut self) {
        if self.status == 0 {
            self.zero_queue_num_uses_max = true;
        }
    }

    pub fn device_id(&self) -> u32 {
        self.device_id
    }

    pub fn irq(&self) -> u32 {
        self.irq
    }

    pub fn offered(&self) -> u64 {
        self.offered
    }

    pub fn set_offered(&mut self, offered: u64) {
        if self.status & (VIRTIO_STATUS_FEATURES_OK | VIRTIO_STATUS_DRIVER_OK) == 0 {
            self.offered = offered;
        }
    }

    pub fn status(&self) -> u32 {
        self.status
    }

    pub fn driver_features(&self) -> u64 {
        self.driver_features
    }

    pub fn interrupt_status(&self) -> u32 {
        self.interrupt_status
    }

    pub fn diagnostics(&self) -> &Diagnostics {
        &self.diag
    }

    pub fn diagnostics_mut(&mut self) -> &mut Diagnostics {
        &mut self.diag
    }

    pub fn diagnostics_line(&self) -> String {
        let d = &self.diag;
        format!(
            "virtio id {} status {:#x} features {:#x} queues {} notifies {} completions {} bytes {} drops {} malformed {} interrupts {} resets {}",
            self.device_id,
            self.status,
            self.driver_features,
            self.queues.len(),
            d.notifies,
            d.completions,
            d.bytes,
            d.drops,
            d.malformed,
            d.interrupts,
            d.resets,
        )
    }

    pub fn queue_count(&self) -> usize {
        self.queues.len()
    }

    pub fn queue(&self, index: usize) -> Option<&VirtQueue> {
        self.queues.get(index).map(|s| &s.queue)
    }

    pub fn queue_mut(&mut self, index: usize) -> Option<&mut VirtQueue> {
        self.queues.get_mut(index).map(|s| &mut s.queue)
    }

    pub fn queue_epoch(&self, index: usize) -> u64 {
        self.queues.get(index).map(|s| s.epoch).unwrap_or(0)
    }

    /// Feature negotiation completed successfully and the device is not in an
    /// error state.  This is normally followed by DRIVER_OK before any queue
    /// traffic.  FreeBSD 14's base virtio-gpu driver is an exception: it polls
    /// GET_DISPLAY_INFO from its attach routine, before the bus can set
    /// DRIVER_OK after that routine returns.
    pub fn features_accepted(&self) -> bool {
        self.status & VIRTIO_STATUS_FEATURES_OK != 0
            && self.status & (VIRTIO_STATUS_NEEDS_RESET | VIRTIO_STATUS_FAILED) == 0
            && self.features_valid()
    }

    /// DRIVER_OK is set, the device does not need a reset, and the feature set
    /// was accepted. Queue processing must use this rather than a bare kick.
    pub fn operational(&self) -> bool {
        if self.status & VIRTIO_STATUS_DRIVER_OK == 0
            || self.status & (VIRTIO_STATUS_NEEDS_RESET | VIRTIO_STATUS_FAILED) != 0
        {
            return false;
        }
        if self.status & VIRTIO_STATUS_FEATURES_OK != 0 {
            return self.features_accepted();
        }
        self.allow_driver_ok_without_features_ok && self.features_valid()
    }

    pub fn negotiated(&self, feature: u64) -> bool {
        self.operational() && self.driver_features & feature == feature
    }

    pub fn set_needs_reset(&mut self) {
        self.status |= VIRTIO_STATUS_NEEDS_RESET;
        self.diag.malformed = self.diag.malformed.saturating_add(1);
    }

    /// Record a used-ring publication. Returns true when the interrupt line
    /// transitions from idle to pending.
    pub fn raise_interrupt(&mut self, bit: u32) -> bool {
        let was = self.interrupt_status;
        self.interrupt_status |= bit;
        if was == 0 && self.interrupt_status != 0 {
            self.diag.interrupts = self.diag.interrupts.saturating_add(1);
            true
        } else {
            false
        }
    }

    pub fn set_interrupt_bit(&mut self, bit: u32) {
        self.interrupt_status |= bit;
    }

    pub fn read(&self, offset: u64) -> u64 {
        match offset {
            VIRTIO_MMIO_MAGIC_VALUE => VIRTIO_MMIO_MAGIC as u64,
            VIRTIO_MMIO_VERSION => VIRTIO_MMIO_VERSION_2 as u64,
            VIRTIO_MMIO_DEVICE_ID => self.device_id as u64,
            VIRTIO_MMIO_VENDOR_ID => VIRTIO_MMIO_VENDOR as u64,
            VIRTIO_MMIO_DEVICE_FEATURES => self.feature_page(self.device_sel),
            VIRTIO_MMIO_QUEUE_NUM_MAX => self
                .selected()
                .map(|s| s.queue.max_size as u64)
                .unwrap_or(0),
            VIRTIO_MMIO_QUEUE_NUM => self
                .selected()
                .filter(|s| s.size_accepted)
                .map(|s| s.queue.size as u64)
                .unwrap_or(0),
            VIRTIO_MMIO_QUEUE_READY => self
                .selected()
                .map(|s| u64::from(s.queue.ready))
                .unwrap_or(0),
            VIRTIO_MMIO_INTERRUPT_STATUS => self.interrupt_status as u64,
            VIRTIO_MMIO_STATUS => self.status as u64,
            // No Cube device currently exposes a shared-memory region. The
            // MMIO transport must return this sentinel for an unavailable
            // selection. Zero describes a bogus zero-length region at GPA 0
            // and makes Linux virtio-fs reject an otherwise valid no-DAX device.
            VIRTIO_MMIO_SHM_LEN_LOW | VIRTIO_MMIO_SHM_LEN_HIGH => u32::MAX as u64,
            _ => 0,
        }
    }

    pub fn write(&mut self, offset: u64, data: u64) -> WriteEffect {
        let value = data as u32;
        match offset {
            VIRTIO_MMIO_DEVICE_FEATURES_SEL => {
                self.device_sel = value;
                WriteEffect::None
            }
            VIRTIO_MMIO_DRIVER_FEATURES_SEL => {
                self.driver_sel = value;
                WriteEffect::None
            }
            VIRTIO_MMIO_DRIVER_FEATURES => {
                self.write_driver_features(value);
                WriteEffect::None
            }
            VIRTIO_MMIO_QUEUE_SEL => {
                self.queue_sel = value;
                WriteEffect::None
            }
            VIRTIO_MMIO_QUEUE_NUM => {
                self.write_queue_num(value);
                WriteEffect::None
            }
            VIRTIO_MMIO_QUEUE_READY => self.write_queue_ready(value),
            VIRTIO_MMIO_QUEUE_NOTIFY => {
                self.diag.notifies = self.diag.notifies.saturating_add(1);
                WriteEffect::Notify { queue: value }
            }
            VIRTIO_MMIO_INTERRUPT_ACK => {
                self.interrupt_status &= !value;
                WriteEffect::InterruptAck {
                    line_low: self.interrupt_status == 0,
                }
            }
            VIRTIO_MMIO_STATUS => self.write_status(value),
            VIRTIO_MMIO_QUEUE_DESC_LOW => {
                self.write_addr(AddrHalf::DescLow, value);
                WriteEffect::None
            }
            VIRTIO_MMIO_QUEUE_DESC_HIGH => {
                self.write_addr(AddrHalf::DescHigh, value);
                WriteEffect::None
            }
            VIRTIO_MMIO_QUEUE_DRIVER_LOW => {
                self.write_addr(AddrHalf::AvailLow, value);
                WriteEffect::None
            }
            VIRTIO_MMIO_QUEUE_DRIVER_HIGH => {
                self.write_addr(AddrHalf::AvailHigh, value);
                WriteEffect::None
            }
            VIRTIO_MMIO_QUEUE_DEVICE_LOW => {
                self.write_addr(AddrHalf::UsedLow, value);
                WriteEffect::None
            }
            VIRTIO_MMIO_QUEUE_DEVICE_HIGH => {
                self.write_addr(AddrHalf::UsedHigh, value);
                WriteEffect::None
            }
            _ => WriteEffect::None,
        }
    }

    fn feature_page(&self, sel: u32) -> u64 {
        match sel {
            0 => self.offered & 0xffff_ffff,
            1 => (self.offered >> 32) & 0xffff_ffff,
            _ => 0,
        }
    }

    fn features_valid(&self) -> bool {
        self.driver_features & !self.offered == 0
            && (self.offered & VIRTIO_F_VERSION_1 == 0
                || self.driver_features & VIRTIO_F_VERSION_1 != 0)
    }

    fn write_driver_features(&mut self, value: u32) {
        if self.status & (VIRTIO_STATUS_FEATURES_OK | VIRTIO_STATUS_DRIVER_OK) != 0 {
            return;
        }
        if self.driver_sel > 1 {
            return;
        }
        let shift = self.driver_sel * 32;
        let mask = 0xffff_ffffu64 << shift;
        self.driver_features = (self.driver_features & !mask) | ((value as u64) << shift);
    }

    fn write_queue_num(&mut self, value: u32) {
        let zero_uses_max = self.zero_queue_num_uses_max;
        let Some(slot) = self.selected_mut() else {
            return;
        };
        if slot.queue.ready {
            return;
        }
        if value == 0 && zero_uses_max {
            slot.queue.size = slot.queue.max_size;
            slot.size_accepted = true;
            return;
        }
        if value == 0 || !value.is_power_of_two() || value > slot.queue.max_size {
            slot.size_accepted = false;
            return;
        }
        slot.queue.size = value;
        slot.size_accepted = true;
    }

    fn write_queue_ready(&mut self, value: u32) -> WriteEffect {
        let index = self.queue_sel;
        let Some(slot) = self.queues.get_mut(index as usize) else {
            return WriteEffect::None;
        };
        if value == 0 {
            let max_size = slot.queue.max_size;
            slot.epoch = slot.epoch.wrapping_add(1);
            slot.queue = VirtQueue {
                max_size,
                size: max_size,
                ..VirtQueue::default()
            };
            slot.size_accepted = true;
            self.diag.resets = self.diag.resets.saturating_add(1);
            return WriteEffect::QueueReset { queue: index };
        }
        if value != 1 || slot.queue.ready || !slot.size_accepted {
            return WriteEffect::None;
        }
        slot.queue.ready = true;
        WriteEffect::QueueReady { queue: index }
    }

    fn write_addr(&mut self, half: AddrHalf, value: u32) {
        let Some(slot) = self.selected_mut() else {
            return;
        };
        if slot.queue.ready {
            return;
        }
        let word = value as u64;
        let reg = match half {
            AddrHalf::DescLow | AddrHalf::DescHigh => &mut slot.queue.desc_table,
            AddrHalf::AvailLow | AddrHalf::AvailHigh => &mut slot.queue.avail_ring,
            AddrHalf::UsedLow | AddrHalf::UsedHigh => &mut slot.queue.used_ring,
        };
        if matches!(
            half,
            AddrHalf::DescLow | AddrHalf::AvailLow | AddrHalf::UsedLow
        ) {
            *reg = (*reg & !0xffff_ffff) | word;
        } else {
            *reg = (*reg & 0xffff_ffff) | (word << 32);
        }
    }

    fn write_status(&mut self, value: u32) -> WriteEffect {
        if value == 0 {
            self.reset_queues();
            self.driver_features = 0;
            self.device_sel = 0;
            self.driver_sel = 0;
            self.queue_sel = 0;
            self.interrupt_status = 0;
            self.status = 0;
            self.diag.resets = self.diag.resets.saturating_add(1);
            return WriteEffect::Reset;
        }
        let raw_features = self.driver_features;
        let illegal = raw_features & !self.offered != 0;
        self.driver_features &= self.offered;
        let mut status = value;
        if status & VIRTIO_STATUS_FEATURES_OK != 0 && (illegal || !self.features_valid()) {
            status &= !VIRTIO_STATUS_FEATURES_OK;
        }
        self.status = status;
        if status & VIRTIO_STATUS_DRIVER_OK != 0 {
            let accepted = if status & VIRTIO_STATUS_FEATURES_OK != 0 {
                self.features_valid()
            } else {
                // FreeBSD virtio_p9fs skips FEATURES_OK. A modern feature set
                // that contains only offered bits is accepted. Illegal bits are
                // stripped and still refuse DRIVER_OK.
                self.allow_driver_ok_without_features_ok && !illegal && self.features_valid()
            };
            if accepted {
                return WriteEffect::DriverOk;
            }
            self.status |= VIRTIO_STATUS_NEEDS_RESET;
            self.diag.malformed = self.diag.malformed.saturating_add(1);
            return WriteEffect::NegotiationFailed;
        }
        if value & VIRTIO_STATUS_FEATURES_OK != 0 && status & VIRTIO_STATUS_FEATURES_OK == 0 {
            return WriteEffect::NegotiationFailed;
        }
        WriteEffect::None
    }

    fn reset_queues(&mut self) {
        for slot in &mut self.queues {
            let max_size = slot.queue.max_size;
            slot.epoch = slot.epoch.wrapping_add(1);
            slot.queue = VirtQueue {
                max_size,
                size: max_size,
                ..VirtQueue::default()
            };
            slot.size_accepted = true;
        }
    }

    fn selected(&self) -> Option<&QueueSlot> {
        self.queues.get(self.queue_sel as usize)
    }

    fn selected_mut(&mut self) -> Option<&mut QueueSlot> {
        self.queues.get_mut(self.queue_sel as usize)
    }
}

enum AddrHalf {
    DescLow,
    DescHigh,
    AvailLow,
    AvailHigh,
    UsedLow,
    UsedHigh,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::virtio::{
        VIRTIO_MMIO_CONFIG, VIRTIO_RING_F_INDIRECT_DESC, VIRTIO_STATUS_ACKNOWLEDGE,
        VIRTIO_STATUS_DRIVER, VIRTIO_STATUS_FAILED,
    };

    fn transport(queues: &[u32]) -> MmioTransport {
        MmioTransport::new(
            4,
            VIRTIO_F_VERSION_1 | VIRTIO_RING_F_INDIRECT_DESC,
            queues,
            37,
        )
    }

    fn accept_modern(t: &mut MmioTransport, extra_low: u32) {
        t.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0);
        t.write(VIRTIO_MMIO_DRIVER_FEATURES, extra_low as u64);
        t.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1);
        t.write(VIRTIO_MMIO_DRIVER_FEATURES, 1);
        let status = VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK;
        assert_eq!(
            t.write(VIRTIO_MMIO_STATUS, status as u64),
            WriteEffect::None
        );
        assert_ne!(t.status() & VIRTIO_STATUS_FEATURES_OK, 0);
    }

    #[test]
    fn feature_pages_mask_and_reject_unknown_bits() {
        let mut t = transport(&[8, 8]);
        assert_eq!(
            t.read(VIRTIO_MMIO_DEVICE_FEATURES),
            VIRTIO_RING_F_INDIRECT_DESC
        );
        t.write(VIRTIO_MMIO_DEVICE_FEATURES_SEL, 1);
        assert_eq!(t.read(VIRTIO_MMIO_DEVICE_FEATURES), 1);
        t.write(VIRTIO_MMIO_DEVICE_FEATURES_SEL, 2);
        assert_eq!(t.read(VIRTIO_MMIO_DEVICE_FEATURES), 0);
        t.write(VIRTIO_MMIO_DEVICE_FEATURES_SEL, 7);
        assert_eq!(t.read(VIRTIO_MMIO_DEVICE_FEATURES), 0);

        t.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0);
        t.write(VIRTIO_MMIO_DRIVER_FEATURES, 0x0000_0400);
        t.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1);
        t.write(VIRTIO_MMIO_DRIVER_FEATURES, 1);
        let status = VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK;
        assert_eq!(
            t.write(VIRTIO_MMIO_STATUS, status as u64),
            WriteEffect::NegotiationFailed
        );
        assert_eq!(t.status() & VIRTIO_STATUS_FEATURES_OK, 0);
        assert_eq!(t.driver_features() & 0x0000_0400, 0);
        assert_eq!(t.driver_features() & VIRTIO_F_VERSION_1, VIRTIO_F_VERSION_1);
    }

    #[test]
    fn unavailable_shared_memory_uses_the_virtio_sentinel() {
        let t = transport(&[8]);
        assert_eq!(t.read(VIRTIO_MMIO_SHM_LEN_LOW), u32::MAX as u64);
        assert_eq!(t.read(VIRTIO_MMIO_SHM_LEN_HIGH), u32::MAX as u64);
    }

    #[test]
    fn version_bit_and_post_negotiation_writes_are_required() {
        let mut t = transport(&[8]);
        t.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0);
        t.write(VIRTIO_MMIO_DRIVER_FEATURES, VIRTIO_RING_F_INDIRECT_DESC);
        let status = VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK;
        assert_eq!(
            t.write(VIRTIO_MMIO_STATUS, status as u64),
            WriteEffect::NegotiationFailed
        );
        t.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1);
        t.write(VIRTIO_MMIO_DRIVER_FEATURES, 1);
        assert_eq!(
            t.write(VIRTIO_MMIO_STATUS, status as u64),
            WriteEffect::None
        );
        t.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0);
        t.write(VIRTIO_MMIO_DRIVER_FEATURES, 0xffff_ffff);
        assert_eq!(
            t.driver_features() & 0xffff_ffff,
            VIRTIO_RING_F_INDIRECT_DESC
        );
        assert_eq!(
            t.write(
                VIRTIO_MMIO_STATUS,
                (status | VIRTIO_STATUS_DRIVER_OK) as u64
            ),
            WriteEffect::DriverOk
        );
        assert!(t.operational());
        assert!(t.negotiated(VIRTIO_RING_F_INDIRECT_DESC));
        assert!(!t.negotiated(1 << 3));
    }

    #[test]
    fn queue_size_rejects_zero_non_power_and_oversize() {
        let cases = [
            (0u32, false),
            (3, false),
            (6, false),
            (9, false),
            (16, false),
            (1, true),
            (2, true),
            (8, true),
        ];
        for (value, ok) in cases {
            let mut t = transport(&[8, 4]);
            t.write(VIRTIO_MMIO_QUEUE_SEL, 0);
            t.write(VIRTIO_MMIO_QUEUE_NUM, value as u64);
            assert_eq!(t.read(VIRTIO_MMIO_QUEUE_NUM) != 0, ok, "size {value}");
            if ok {
                assert_eq!(t.queue(0).unwrap().size, value);
            }
            t.write(VIRTIO_MMIO_QUEUE_SEL, 1);
            assert_eq!(t.read(VIRTIO_MMIO_QUEUE_NUM_MAX), 4);
            t.write(VIRTIO_MMIO_QUEUE_NUM, 8);
            assert_eq!(t.read(VIRTIO_MMIO_QUEUE_NUM), 0);
            assert_eq!(t.queue(0).unwrap().size, if ok { value } else { 8 });
        }
    }

    #[test]
    fn zero_queue_num_compat_is_explicit_and_retains_maximum() {
        let mut strict = transport(&[8]);
        strict.write(VIRTIO_MMIO_QUEUE_NUM, 0);
        assert_eq!(strict.read(VIRTIO_MMIO_QUEUE_NUM), 0);
        assert_eq!(strict.write(VIRTIO_MMIO_QUEUE_READY, 1), WriteEffect::None);

        let mut compatible = transport(&[8]);
        compatible.enable_zero_queue_num_compat();
        compatible.write(VIRTIO_MMIO_QUEUE_NUM, 0);
        assert_eq!(compatible.read(VIRTIO_MMIO_QUEUE_NUM), 8);
        assert_eq!(
            compatible.write(VIRTIO_MMIO_QUEUE_READY, 1),
            WriteEffect::QueueReady { queue: 0 }
        );
    }

    #[test]
    fn queues_are_independent_across_reset_and_ack() {
        let mut t = transport(&[8, 8, 8]);
        for (index, base) in [(0u32, 0x1000u64), (1, 0x2000u64), (2, 0x3000u64)] {
            t.write(VIRTIO_MMIO_QUEUE_SEL, index as u64);
            t.write(VIRTIO_MMIO_QUEUE_NUM, 4);
            t.write(VIRTIO_MMIO_QUEUE_DESC_LOW, base);
            t.write(VIRTIO_MMIO_QUEUE_DRIVER_LOW, base + 0x100);
            t.write(VIRTIO_MMIO_QUEUE_DEVICE_LOW, base + 0x200);
            assert_eq!(
                t.write(VIRTIO_MMIO_QUEUE_READY, 1),
                WriteEffect::QueueReady { queue: index }
            );
        }
        t.queue_mut(1).unwrap().last_avail_idx = 3;
        t.queue_mut(1).unwrap().used_idx = 3;
        let epoch1 = t.queue_epoch(1);
        assert_eq!(t.write(VIRTIO_MMIO_QUEUE_SEL, 1), WriteEffect::None);
        assert_eq!(
            t.write(VIRTIO_MMIO_QUEUE_READY, 0),
            WriteEffect::QueueReset { queue: 1 }
        );
        assert!(!t.queue(1).unwrap().ready);
        assert_eq!(t.queue(1).unwrap().last_avail_idx, 0);
        assert_eq!(t.queue(1).unwrap().desc_table, 0);
        assert!(t.queue(0).unwrap().ready);
        assert_eq!(t.queue(0).unwrap().desc_table, 0x1000);
        assert_eq!(t.queue(2).unwrap().used_ring, 0x3200);
        assert_ne!(t.queue_epoch(1), epoch1);
        assert_eq!(t.queue_epoch(0), 0);

        t.write(VIRTIO_MMIO_QUEUE_DESC_LOW, 0xabc);
        assert_eq!(t.queue(0).unwrap().desc_table, 0x1000);
        assert_eq!(t.read(VIRTIO_MMIO_QUEUE_SEL), 0);

        assert!(t.raise_interrupt(1));
        assert!(!t.raise_interrupt(1));
        assert_eq!(
            t.write(VIRTIO_MMIO_INTERRUPT_ACK, 1),
            WriteEffect::InterruptAck { line_low: true }
        );
        assert_eq!(t.interrupt_status(), 0);
        t.raise_interrupt(1);
        t.set_interrupt_bit(2);
        assert_eq!(
            t.write(VIRTIO_MMIO_INTERRUPT_ACK, 1),
            WriteEffect::InterruptAck { line_low: false }
        );
        assert_eq!(t.interrupt_status(), 2);

        accept_modern(&mut t, 0);
        assert_eq!(
            t.write(
                VIRTIO_MMIO_STATUS,
                (VIRTIO_STATUS_ACKNOWLEDGE
                    | VIRTIO_STATUS_DRIVER
                    | VIRTIO_STATUS_FEATURES_OK
                    | VIRTIO_STATUS_DRIVER_OK) as u64
            ),
            WriteEffect::DriverOk
        );
        assert_eq!(t.write(VIRTIO_MMIO_STATUS, 0), WriteEffect::Reset);
        assert_eq!(t.status(), 0);
        assert_eq!(t.driver_features(), 0);
        assert_eq!(t.interrupt_status(), 0);
        assert!(!t.queue(0).unwrap().ready);
        assert_eq!(t.queue(0).unwrap().desc_table, 0);
        assert_eq!(t.queue(2).unwrap().size, 8);
        assert!(!t.operational());
        assert!(t.diagnostics().resets >= 1);
        assert_eq!(t.read(VIRTIO_MMIO_CONFIG), 0);
    }

    #[test]
    fn driver_ok_without_features_ok_follows_policy() {
        let mut strict = transport(&[4]);
        strict.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1);
        strict.write(VIRTIO_MMIO_DRIVER_FEATURES, 1);
        assert_eq!(
            strict.write(VIRTIO_MMIO_STATUS, VIRTIO_STATUS_DRIVER_OK as u64),
            WriteEffect::NegotiationFailed
        );
        assert!(!strict.operational());
        assert_ne!(strict.status() & VIRTIO_STATUS_NEEDS_RESET, 0);

        let mut legacy = MmioTransport::with_policy(9, VIRTIO_F_VERSION_1 | 1, &[8], 48, true);
        legacy.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 0);
        legacy.write(VIRTIO_MMIO_DRIVER_FEATURES, 1);
        legacy.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1);
        legacy.write(VIRTIO_MMIO_DRIVER_FEATURES, 1);
        assert_eq!(
            legacy.write(VIRTIO_MMIO_STATUS, VIRTIO_STATUS_DRIVER_OK as u64),
            WriteEffect::DriverOk
        );
        assert!(legacy.operational());

        legacy.write(VIRTIO_MMIO_STATUS, 0);
        legacy.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1);
        legacy.write(VIRTIO_MMIO_DRIVER_FEATURES, 0);
        assert_eq!(
            legacy.write(VIRTIO_MMIO_STATUS, VIRTIO_STATUS_DRIVER_OK as u64),
            WriteEffect::NegotiationFailed
        );
        assert_eq!(legacy.status() & VIRTIO_STATUS_FAILED, 0);
    }

    #[test]
    fn notify_out_of_range_is_still_reported() {
        let mut t = transport(&[8]);
        assert_eq!(
            t.write(VIRTIO_MMIO_QUEUE_NOTIFY, 5),
            WriteEffect::Notify { queue: 5 }
        );
        assert_eq!(t.read(VIRTIO_MMIO_MAGIC_VALUE), VIRTIO_MMIO_MAGIC as u64);
        assert_eq!(t.read(VIRTIO_MMIO_VERSION), 2);
        assert_eq!(t.read(VIRTIO_MMIO_VENDOR_ID), VIRTIO_MMIO_VENDOR as u64);
        t.write(VIRTIO_MMIO_QUEUE_SEL, 9);
        assert_eq!(t.read(VIRTIO_MMIO_QUEUE_NUM_MAX), 0);
        assert_eq!(t.write(VIRTIO_MMIO_QUEUE_READY, 1), WriteEffect::None);
    }
}
