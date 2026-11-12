//! VirtIO Modern MMIO Transport definitions and data structures.

#![allow(dead_code)]

pub mod balloon;
pub mod block;
pub mod console;
pub mod fs;
pub mod gpu;
pub mod input;
pub mod net;
pub mod p9;
pub mod rng;
pub mod transport;
pub mod vsock;

pub use transport::{MmioTransport, WriteEffect};

pub use console::VirtioConsole;
pub use input::VirtioInput;

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
pub const VIRTIO_MMIO_SHM_SEL: u64 = 0x0ac;
pub const VIRTIO_MMIO_SHM_LEN_LOW: u64 = 0x0b0;
pub const VIRTIO_MMIO_SHM_LEN_HIGH: u64 = 0x0b4;
pub const VIRTIO_MMIO_SHM_BASE_LOW: u64 = 0x0b8;
pub const VIRTIO_MMIO_SHM_BASE_HIGH: u64 = 0x0bc;
pub const VIRTIO_MMIO_CONFIG: u64 = 0x100;

// Magic and Version constants
pub const VIRTIO_MMIO_MAGIC: u32 = 0x74726976; // 'virt' in little-endian
pub const VIRTIO_MMIO_VERSION_2: u32 = 2; // VirtIO 1.0+ modern version
pub const VIRTIO_MMIO_VENDOR: u32 = 0x554d4551; // 'QEMU'

// VirtIO Device IDs
pub const VIRTIO_DEV_NET: u32 = 1;
pub const VIRTIO_DEV_BLOCK: u32 = 2;
pub const VIRTIO_DEV_CONSOLE: u32 = 3;
pub const VIRTIO_DEV_RNG: u32 = 4;
pub const VIRTIO_DEV_BALLOON: u32 = 5;
pub const VIRTIO_DEV_9P: u32 = 9;
pub const VIRTIO_DEV_GPU: u32 = 16;
pub const VIRTIO_DEV_INPUT: u32 = 18;
pub const VIRTIO_DEV_VSOCK: u32 = 19;
pub const VIRTIO_DEV_FS: u32 = 26;

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

pub const VIRTIO_STATUS_NEEDS_RESET: u32 = 64;
pub const MAX_CHAIN_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct Descriptor {
    pub addr: u64,
    pub len: u32,
    pub flags: u16,
}

impl VirtQueue {
    pub fn validate(&self, mem: &crate::memory::GuestMemory) -> Result<(), String> {
        if !self.ready
            || self.size == 0
            || self.size > self.max_size
            || self.size > 32768
            || !self.size.is_power_of_two()
            || self.desc_table % 16 != 0
            || self.avail_ring % 2 != 0
            || self.used_ring % 4 != 0
        {
            return Err("Invalid virtqueue configuration".into());
        }
        mem.checked_offset(self.desc_table, self.size as usize * 16)?;
        mem.checked_offset(self.avail_ring, 6 + self.size as usize * 2)?;
        mem.checked_offset(self.used_ring, 6 + self.size as usize * 8)?;
        Ok(())
    }

    /// Snapshot producer progress once per notification. A guest cannot make
    /// one MMIO access run forever by continuously updating avail.idx.
    pub fn pending_count(&self, mem: &crate::memory::GuestMemory) -> Result<u16, String> {
        self.validate(mem)?;
        let idx = mem.read_u16(self.avail_ring + 2)?;
        std::sync::atomic::fence(std::sync::atomic::Ordering::Acquire);
        let count = idx.wrapping_sub(self.last_avail_idx);
        if count as u32 > self.size {
            return Err("Virtqueue producer overrun".into());
        }
        Ok(count)
    }

    pub fn descriptors(
        &self,
        mem: &crate::memory::GuestMemory,
        head: u16,
        indirect: bool,
        max_bytes: usize,
    ) -> Result<Vec<Descriptor>, String> {
        self.validate(mem)?;
        let mut table = self.desc_table;
        let mut count = self.size;
        let mut idx = head as u32;
        let mut seen = vec![false; count as usize];
        let mut result = Vec::new();
        let mut total = 0usize;
        let mut in_indirect = false;
        let mut seen_write = false;
        let chain_limit = self.size as usize;
        loop {
            if result.len() >= chain_limit {
                return Err("descriptor chain exceeds queue size".into());
            }
            if idx >= count || seen.get(idx as usize).copied().unwrap_or(true) {
                return Err("Invalid/cyclic descriptor chain".into());
            }
            seen[idx as usize] = true;
            let entry = table
                .checked_add(idx as u64 * 16)
                .ok_or("descriptor address overflow")?;
            let mut raw = [0u8; 16];
            mem.read(entry, &mut raw)?;
            let addr = u64::from_le_bytes(raw[..8].try_into().unwrap());
            let len = u32::from_le_bytes(raw[8..12].try_into().unwrap());
            let flags = u16::from_le_bytes(raw[12..14].try_into().unwrap());
            let next = u16::from_le_bytes(raw[14..].try_into().unwrap());
            if flags & !7 != 0 {
                return Err("Unknown descriptor flags".into());
            }
            if flags & VRING_DESC_F_INDIRECT != 0 {
                let entries = len / 16;
                let remaining = chain_limit.saturating_sub(result.len());
                if !indirect
                    || in_indirect
                    || flags & VRING_DESC_F_NEXT != 0
                    || len == 0
                    || len % 16 != 0
                    || entries > self.max_size
                    || entries as usize > remaining
                    || addr % 16 != 0
                    || addr.checked_add(len as u64).is_none()
                {
                    return Err("Invalid indirect descriptor table".into());
                }
                mem.checked_offset(addr, len as usize)?;
                table = addr;
                count = entries;
                seen = vec![false; count as usize];
                idx = 0;
                in_indirect = true;
                continue;
            }
            let write = flags & VRING_DESC_F_WRITE != 0;
            if !write && seen_write {
                return Err("invalid descriptor direction".into());
            }
            seen_write |= write;
            total = total
                .checked_add(len as usize)
                .filter(|&n| n <= max_bytes)
                .ok_or("Descriptor chain exceeds transfer limit")?;
            if len > 0 {
                mem.checked_offset(addr, len as usize)?;
            }
            result.push(Descriptor { addr, len, flags });
            if flags & VRING_DESC_F_NEXT == 0 {
                return Ok(result);
            }
            idx = next as u32;
        }
    }
}

/// LibFuzzer-style entry. Any byte string is a queue plus a descriptor image.
/// The parser must not panic and must not return a buffer it did not bounds-check.
#[cfg(test)]
pub fn fuzz_descriptor_parser(data: &[u8]) {
    if data.len() < 4 {
        return;
    }
    let mem = crate::memory::GuestMemory::new_test(0, 0x1_0000);
    let image = data.len().min(0x1000);
    let _ = mem.write(0x1000, &data[..image]);
    let size = 1u32 << (data[0] % 4);
    let q = VirtQueue {
        max_size: 8,
        size,
        ready: true,
        desc_table: 0x1000,
        avail_ring: 0x3000,
        used_ring: 0x3800,
        ..VirtQueue::default()
    };
    let head = u16::from_le_bytes([data[1], data.get(2).copied().unwrap_or(0)]) % size as u16;
    let indirect = data.get(3).copied().unwrap_or(0) & 1 == 1;
    if let Ok(chain) = q.descriptors(&mem, head, indirect, 4096) {
        for desc in chain {
            if desc.len > 0 {
                assert!(mem.checked_offset(desc.addr, desc.len as usize).is_ok());
            }
            assert_eq!(desc.flags & !7, 0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::GuestMemory;
    fn queue() -> (VirtQueue, GuestMemory) {
        (
            VirtQueue {
                max_size: 8,
                size: 8,
                ready: true,
                desc_table: 0x100,
                avail_ring: 0x200,
                used_ring: 0x300,
                ..Default::default()
            },
            GuestMemory::new_test(0, 0x4000),
        )
    }
    #[test]
    fn rejects_invalid_rings_and_producer_overrun() {
        let (mut q, mem) = queue();
        mem.write_u16(0x202, 9).unwrap();
        assert!(q.pending_count(&mem).is_err());
        q.avail_ring = u64::MAX - 1;
        assert!(q.pending_count(&mem).is_err());
        q.avail_ring = 0x200;
        q.size = 3;
        assert!(q.validate(&mem).is_err());
    }
    #[test]
    fn validates_full_chain_before_returning_any_buffer() {
        let (q, mem) = queue();
        mem.write_u64(0x100, 0x1000).unwrap();
        mem.write_u32(0x108, 16).unwrap();
        mem.write_u16(0x10c, VRING_DESC_F_NEXT).unwrap();
        assert!(q.descriptors(&mem, 0, false, 64).is_err()); // self-cycle
        mem.write_u16(0x10e, 8).unwrap();
        assert!(q.descriptors(&mem, 0, false, 64).is_err()); // bad next
        mem.write_u16(0x10c, 0).unwrap();
        assert!(q.descriptors(&mem, 0, false, 8).is_err()); // transfer cap
        mem.write_u64(0x100, u64::MAX).unwrap();
        assert!(q.descriptors(&mem, 0, false, 64).is_err());
        mem.write_u64(0x100, 0x1000).unwrap();
        assert_eq!(q.descriptors(&mem, 0, false, 64).unwrap()[0].len, 16);
    }
    #[test]
    fn rejects_nested_and_unnegotiated_indirect_tables() {
        let (q, mem) = queue();
        mem.write_u64(0x100, 0x1000).unwrap();
        mem.write_u32(0x108, 16).unwrap();
        mem.write_u16(0x10c, VRING_DESC_F_INDIRECT).unwrap();
        assert!(q.descriptors(&mem, 0, false, 64).is_err());
        mem.write_u64(0x1000, 0x1000).unwrap();
        mem.write_u32(0x1008, 16).unwrap();
        mem.write_u16(0x100c, VRING_DESC_F_INDIRECT).unwrap();
        assert!(q.descriptors(&mem, 0, true, 64).is_err());
    }

    fn write_desc(mem: &GuestMemory, at: u64, addr: u64, len: u32, flags: u16, next: u16) {
        mem.write_u64(at, addr).unwrap();
        mem.write_u32(at + 8, len).unwrap();
        mem.write_u16(at + 12, flags).unwrap();
        mem.write_u16(at + 14, next).unwrap();
    }

    #[test]
    fn accepts_final_indirect_table_after_direct_descriptors() {
        let (q, mem) = queue();
        write_desc(&mem, 0x100, 0x1800, 4, VRING_DESC_F_NEXT, 1);
        write_desc(&mem, 0x110, 0x1000, 32, VRING_DESC_F_INDIRECT, 0);
        write_desc(&mem, 0x1000, 0x2000, 8, VRING_DESC_F_NEXT, 1);
        write_desc(&mem, 0x1010, 0x2100, 8, VRING_DESC_F_WRITE, 0);
        let chain = q.descriptors(&mem, 0, true, 64).unwrap();
        assert_eq!(chain.len(), 3);
        assert_eq!(chain[0].addr, 0x1800);
        assert_eq!(chain[1].addr, 0x2000);
        assert_eq!(chain[2].flags & VRING_DESC_F_WRITE, VRING_DESC_F_WRITE);
        write_desc(
            &mem,
            0x110,
            0x1000,
            32,
            VRING_DESC_F_INDIRECT | VRING_DESC_F_NEXT,
            2,
        );
        assert!(q.descriptors(&mem, 0, true, 64).is_err());
    }

    #[test]
    fn rejects_direction_cycles_overflow_and_long_chains() {
        let (q, mem) = queue();
        write_desc(
            &mem,
            0x100,
            0x1800,
            4,
            VRING_DESC_F_WRITE | VRING_DESC_F_NEXT,
            1,
        );
        write_desc(&mem, 0x110, 0x2000, 4, VRING_DESC_F_NEXT, 0);
        assert!(
            q.descriptors(&mem, 0, false, 64)
                .unwrap_err()
                .contains("direction")
        );
        write_desc(&mem, 0x110, 0x2000, 4, 0, 0);
        write_desc(&mem, 0x100, u64::MAX - 8, 32, 0, 0);
        assert!(q.descriptors(&mem, 0, false, 64).is_err());
        write_desc(&mem, 0x100, 0x1800, u32::MAX, 0, 0);
        assert!(q.descriptors(&mem, 0, false, usize::MAX).is_err());
        let (mut small, mem) = queue();
        small.size = 2;
        write_desc(&mem, 0x100, 0x1800, 1, VRING_DESC_F_NEXT, 1);
        write_desc(&mem, 0x110, 0x1900, 1, VRING_DESC_F_NEXT, 2);
        write_desc(&mem, 0x120, 0x1a00, 1, 0, 0);
        assert!(
            small
                .descriptors(&mem, 0, false, 64)
                .unwrap_err()
                .contains("queue size")
        );
        for _ in 0..64 {
            fuzz_descriptor_parser(&[0xff, 0xfe, 3, 1, 0, 16, 0, 0, 0, 0, 0, 0, 1, 0, 1, 0]);
        }
    }

    #[test]
    fn descriptor_parser_fuzz_target_accepts_hostile_inputs() {
        let mut state = 0x1234_5678u32;
        for _ in 0..200 {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            let mut bytes = vec![0u8; 64];
            for (i, b) in bytes.iter_mut().enumerate() {
                *b = state.rotate_left(i as u32) as u8;
            }
            bytes[0] = (state % 4) as u8;
            fuzz_descriptor_parser(&bytes);
            fuzz_descriptor_parser(&[]);
            fuzz_descriptor_parser(&[0, 1, 2]);
        }
    }
}
