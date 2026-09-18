//! VirtIO Modern MMIO Transport definitions and data structures.

#![allow(dead_code)]

pub mod block;

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

// VirtIO Device Status Bits
pub const VIRTIO_STATUS_ACKNOWLEDGE: u32 = 1;
pub const VIRTIO_STATUS_DRIVER: u32 = 2;
pub const VIRTIO_STATUS_DRIVER_OK: u32 = 4;
pub const VIRTIO_STATUS_FEATURES_OK: u32 = 8;
pub const VIRTIO_STATUS_FAILED: u32 = 128;

// Descriptor Flags
pub const VRING_DESC_F_NEXT: u16 = 1;
pub const VRING_DESC_F_WRITE: u16 = 2;

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
}
