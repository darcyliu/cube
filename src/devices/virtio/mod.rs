//! VirtIO Modern MMIO Transport definitions and data structures.

#![allow(dead_code)]

pub mod block;
pub mod console;
pub mod net;
pub mod rng;

pub use console::VirtioConsole;

// VirtIO MMIO Register Offsets (Modern, Version 2)
pub const VIRTIO_MMIO_MAGIC_VALUE: u64 = 0x000;
pub const VIRTIO_MMIO_VERSION: u64 = 0x004;
pub const VIRTIO_MMIO_DEVICE_ID: u64 = 0x008;
pub const VIRTIO_MMIO_VENDOR_ID: u64 = 0x00c;
pub const VIRTIO_MMIO_DEVICE_FEATURES: u64 = 0x010;
pub const VIRTIO_MMIO_DEVICE_FEATURES_SEL: u64 = 0x014;
pub const VIRTIO_MMIO_DRIVER_FEATURES: u64 = 0x020;
pub const VIRTIO_MMIO_DRIVER_FEATURES_SEL: u64 = 0x024;
pub const VIRTIO_MMIO_QUEUE_SEL: u64 = 0x030;
pub const VIRTIO_MMIO_QUEUE_NUM_MAX: u64 = 0x034;
pub const VIRTIO_MMIO_QUEUE_NUM: u64 = 0x038;
pub const VIRTIO_MMIO_QUEUE_READY: u64 = 0x044;
pub const VIRTIO_MMIO_QUEUE_NOTIFY: u64 = 0x050;
pub const VIRTIO_MMIO_INTERRUPT_STATUS: u64 = 0x060;
pub const VIRTIO_MMIO_INTERRUPT_ACK: u64 = 0x064;
pub const VIRTIO_MMIO_STATUS: u64 = 0x070;
pub const VIRTIO_MMIO_QUEUE_DESC_LOW: u64 = 0x080;
pub const VIRTIO_MMIO_QUEUE_DESC_HIGH: u64 = 0x084;
pub const VIRTIO_MMIO_QUEUE_DRIVER_LOW: u64 = 0x090;
pub const VIRTIO_MMIO_QUEUE_DRIVER_HIGH: u64 = 0x094;
pub const VIRTIO_MMIO_QUEUE_DEVICE_LOW: u64 = 0x0a0;
pub const VIRTIO_MMIO_QUEUE_DEVICE_HIGH: u64 = 0x0a4;
pub const VIRTIO_MMIO_CONFIG: u64 = 0x100;

// Magic and Version constants
pub const VIRTIO_MMIO_MAGIC: u32 = 0x74726976; // 'virt' in little-endian
pub const VIRTIO_MMIO_VERSION_2: u32 = 2;       // VirtIO 1.0+ modern version
pub const VIRTIO_MMIO_VENDOR: u32 = 0x554d4551;  // 'QEMU'

// VirtIO Device IDs
pub const VIRTIO_DEV_NET: u32 = 1;
pub const VIRTIO_DEV_BLOCK: u32 = 2;
pub const VIRTIO_DEV_CONSOLE: u32 = 3;
pub const VIRTIO_DEV_RNG: u32 = 4;

// VirtIO Device Status Bits
pub const VIRTIO_STATUS_ACKNOWLEDGE: u32 = 1;
pub const VIRTIO_STATUS_DRIVER: u32 = 2;
pub const VIRTIO_STATUS_DRIVER_OK: u32 = 4;
pub const VIRTIO_STATUS_FEATURES_OK: u32 = 8;
pub const VIRTIO_STATUS_FAILED: u32 = 128;

// Descriptor Flags
pub const VRING_DESC_F_NEXT: u16 = 1;
pub const VRING_DESC_F_WRITE: u16 = 2;
pub const VRING_DESC_F_INDIRECT: u16 = 4;

/// Guest sets this in avail.flags while it is processing used buffers (QEMU
/// virtio_split_should_notify). Device MUST NOT interrupt if it is set.
pub const VRING_AVAIL_F_NO_INTERRUPT: u16 = 1;

// Ring feature: descriptor can point at a table of descriptors
pub const VIRTIO_RING_F_INDIRECT_DESC: u64 = 1 << 28;

// VirtIO Network Features
pub const VIRTIO_NET_F_MAC: u64 = 1 << 5;
pub const VIRTIO_NET_F_STATUS: u64 = 1 << 16;
pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;

// VirtIO Network Status
pub const VIRTIO_NET_S_LINK_UP: u16 = 1;

// VirtIO Modern Network Header length (virtio_net_hdr_v1)
pub const VIRTIO_NET_HDR_SIZE: usize = 12;

// Split Virtqueue state for a single queue
#[derive(Clone, Copy, Default)]
pub struct VirtQueue {
    pub max_size: u32,
    pub size: u32,
    pub ready: bool,
    pub desc_table: u64,
    pub avail_ring: u64,
    pub used_ring: u64,
    pub last_avail_idx: u16,
    pub used_idx: u16,
}

/// Publish a used-ring entry then used.idx, with SeqCst fences (QEMU
/// virtqueue_push / smp_wmb). Never re-read used.idx from guest memory.
pub fn publish_used(mem: &crate::memory::GuestMemory, q: &mut VirtQueue, head: u16, len: u32) {
    use std::sync::atomic::Ordering;
    let q_size = q.size.max(1) as u64;
    let elem = q.used_ring + 4 + 8 * (q.used_idx as u64 % q_size);
    let _ = mem.write_u32(elem, head as u32);
    let _ = mem.write_u32(elem + 4, len);
    std::sync::atomic::fence(Ordering::SeqCst);
    q.used_idx = q.used_idx.wrapping_add(1);
    let _ = mem.write_u16(q.used_ring + 2, q.used_idx);
    std::sync::atomic::fence(Ordering::SeqCst);
}

pub fn avail_wants_interrupt(mem: &crate::memory::GuestMemory, avail_ring: u64) -> bool {
    match mem.read_u16(avail_ring) {
        Ok(flags) => (flags & VRING_AVAIL_F_NO_INTERRUPT) == 0,
        Err(_) => true,
    }
}
