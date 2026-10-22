//! Queue notify and the ISR stay in `VirtioPciTransport`.

use super::transport::{PciIdent, VirtioPciDev};
use super::*;
use crate::memory::GuestMemory;
use crate::shared_fs::{Backend, MAX_MESSAGE, ShareConfig};
use std::sync::Arc;

const QUEUE_SIZE: u32 = 128;
/// Tag string in device config. FreeBSD `virtio_p9fs` reads it only if offered.
const VIRTIO_9P_F_MOUNT_TAG: u64 = 1;
const FEATURES: u64 = VIRTIO_F_VERSION_1 | VIRTIO_RING_F_INDIRECT_DESC | VIRTIO_9P_F_MOUNT_TAG;
/// Room for a 120-byte tag plus the u16 length, inside the 16 KiB BAR.
const CONFIG_LEN: u32 = 128;

trait NineP {
    fn exchange(&mut self, bytes: &[u8]) -> Result<Vec<u8>, String>;
}

impl NineP for Backend {
    fn exchange(&mut self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        self.request(bytes)
    }
}

struct Request {
    head: u16,
    epoch: u64,
    bytes: Vec<u8>,
    output: Vec<Descriptor>,
}

pub struct P9Core<T> {
    mem: Arc<GuestMemory>,
    io: T,
    config: Vec<u8>,
    status: u32,
    driver_features: u64,
    queue: VirtQueue,
    epoch: u64,
    backend_failed: bool,
    config_pending: bool,
}

impl P9Core<Backend> {
    pub fn open(config: &ShareConfig, mem: Arc<GuestMemory>) -> Result<Self, String> {
        let backend = Backend::start(config)?;
        eprintln!(
            "[VMM] 9P share '{}' {} ({})",
            config.tag,
            config.path.display(),
            if config.read_only { "ro" } else { "rw" }
        );
        Ok(Self::new(config.tag.clone(), mem, backend))
    }
}

impl<T> P9Core<T> {
    fn new(tag: String, mem: Arc<GuestMemory>, io: T) -> Self {
        let mut config = (tag.len() as u16).to_le_bytes().to_vec();
        config.extend(tag.as_bytes());
        Self {
            mem,
            io,
            config,
            status: 0,
            driver_features: 0,
            queue: VirtQueue {
                max_size: QUEUE_SIZE,
                size: QUEUE_SIZE,
                ..Default::default()
            },
            epoch: 0,
            backend_failed: false,
            config_pending: false,
        }
    }

    fn fail(&mut self, error: &str) {
        eprintln!("[VirtIO-9P] {error}");
        self.status |= VIRTIO_STATUS_NEEDS_RESET;
        self.config_pending = true;
    }

    fn fresh_queue(&self) -> VirtQueue {
        VirtQueue {
            max_size: QUEUE_SIZE,
            size: QUEUE_SIZE,
            ..Default::default()
        }
    }

    /// One avail slot. Rejects a malformed chain before any filesystem call.
    fn take_request(&mut self) -> Result<Option<Request>, String> {
        if self.status
            & (VIRTIO_STATUS_DRIVER_OK | VIRTIO_STATUS_NEEDS_RESET | VIRTIO_STATUS_FAILED)
            != VIRTIO_STATUS_DRIVER_OK
            || !self.queue.ready
        {
            return Ok(None);
        }
        if self.queue.pending_count(&self.mem)? == 0 {
            return Ok(None);
        }
        let indirect = self.driver_features & VIRTIO_RING_F_INDIRECT_DESC != 0;
        let head = {
            let q = &self.queue;
            self.mem
                .read_u16(q.avail_ring + 4 + 2 * (q.last_avail_idx as u64 % q.size as u64))?
        };
        let descs = self
            .queue
            .descriptors(&self.mem, head, indirect, 2 * MAX_MESSAGE)?;
        let mut bytes = Vec::new();
        let mut output = Vec::new();
        let mut capacity = 0usize;
        for d in descs {
            if d.flags & VRING_DESC_F_WRITE != 0 {
                capacity += d.len as usize;
                output.push(d);
            } else {
                if !output.is_empty() || bytes.len() + d.len as usize > MAX_MESSAGE {
                    return Err("invalid request descriptor ordering/size".into());
                }
                let start = bytes.len();
                bytes.resize(start + d.len as usize, 0);
                self.mem.read(d.addr, &mut bytes[start..])?;
            }
        }
        if bytes.len() < 7 || capacity < 11 || capacity > MAX_MESSAGE {
            return Err("invalid 9P request/response capacity".into());
        }
        let minimum = match bytes[4] {
            100 => 21,
            104 | 16 | 72 => 20,
            12 | 112 | 14 | 114 => 24,
            _ => 11,
        };
        if capacity < minimum {
            return Err("response buffer too small for request before filesystem I/O".into());
        }
        self.queue.last_avail_idx = self.queue.last_avail_idx.wrapping_add(1);
        Ok(Some(Request {
            head,
            epoch: self.epoch,
            bytes,
            output,
        }))
    }

    fn complete(&mut self, request: Request, mut reply: Vec<u8>) -> Result<(), String> {
        if request.epoch != self.epoch || !self.queue.ready {
            return Ok(());
        }
        let capacity: usize = request.output.iter().map(|d| d.len as usize).sum();
        if reply.len() > capacity {
            reply = vec![
                11,
                0,
                0,
                0,
                7,
                request.bytes[5],
                request.bytes[6],
                22,
                0,
                0,
                0,
            ];
        }
        let mut offset = 0;
        for d in &request.output {
            let n = (reply.len() - offset).min(d.len as usize);
            self.mem.write(d.addr, &reply[offset..offset + n])?;
            offset += n;
            if offset == reply.len() {
                break;
            }
        }
        publish_used(&self.mem, &mut self.queue, request.head, reply.len() as u32);
        Ok(())
    }

    /// Walk every pending 9P request. Returns whether ISR bit 0 should be set.
    fn process_queue(&mut self) -> bool
    where
        T: NineP,
    {
        if self.status
            & (VIRTIO_STATUS_DRIVER_OK | VIRTIO_STATUS_NEEDS_RESET | VIRTIO_STATUS_FAILED)
            != VIRTIO_STATUS_DRIVER_OK
            || !self.queue.ready
        {
            return false;
        }
        let count = match self.queue.pending_count(&self.mem) {
            Ok(n) => n,
            Err(error) => {
                self.fail(&error);
                return false;
            }
        };
        let mut published = false;
        for _ in 0..count {
            let request = match self.take_request() {
                Ok(Some(request)) => request,
                Ok(None) => break,
                Err(error) => {
                    self.fail(&error);
                    break;
                }
            };
            match self.io.exchange(&request.bytes) {
                Ok(reply) => {
                    if let Err(error) = self.complete(request, reply) {
                        self.fail(&error);
                        break;
                    }
                    published = true;
                }
                Err(error) => {
                    self.backend_failed = true;
                    self.fail(&error);
                    break;
                }
            }
        }
        published && avail_wants_interrupt(&self.mem, self.queue.avail_ring)
    }
}

impl<T: NineP + Send> VirtioPciDev for P9Core<T> {
    fn pci_ident(&self) -> PciIdent {
        PciIdent {
            device_id: 0x1049,
            subsystem_id: 9,
            // QEMU virtio-9p-pci class_id 0x2: base class 0, subclass 2.
            class: 0x00,
            subclass: 0x02,
            config_len: CONFIG_LEN,
        }
    }

    fn queue(&self) -> &VirtQueue {
        &self.queue
    }

    fn queue_mut(&mut self) -> &mut VirtQueue {
        &mut self.queue
    }

    fn set_queue_ready(&mut self, ready: bool) {
        if !ready {
            self.epoch = self.epoch.wrapping_add(1);
            self.queue.last_avail_idx = 0;
            self.queue.used_idx = 0;
        }
        self.queue.ready = ready;
    }

    fn device_feature_word(&self, select: u32) -> u32 {
        match select {
            0 => FEATURES as u32,
            1 => (FEATURES >> 32) as u32,
            _ => 0,
        }
    }

    fn driver_feature_word(&self, select: u32) -> u32 {
        match select {
            0 => self.driver_features as u32,
            1 => (self.driver_features >> 32) as u32,
            _ => 0,
        }
    }

    fn set_driver_feature_word(&mut self, select: u32, word: u32) {
        if self.status & (VIRTIO_STATUS_FEATURES_OK | VIRTIO_STATUS_DRIVER_OK) != 0 {
            return;
        }
        match select {
            0 => self.driver_features = (self.driver_features & !0xffff_ffff) | u64::from(word),
            1 => {
                self.driver_features =
                    (self.driver_features & 0xffff_ffff) | (u64::from(word) << 32)
            }
            _ => {}
        }
    }

    fn status(&self) -> u32 {
        self.status
    }

    fn set_status(&mut self, status: u32) {
        if status == 0 {
            let epoch = self.epoch.wrapping_add(1);
            self.driver_features = 0;
            self.queue = self.fresh_queue();
            self.status = 0;
            self.config_pending = false;
            self.epoch = epoch;
            if let Err(error) = self.io.exchange(&[]) {
                self.backend_failed = true;
                eprintln!("[VirtIO-9P] {error}");
            }
            return;
        }
        self.status = status;
        if self.backend_failed && status & VIRTIO_STATUS_DRIVER_OK != 0 {
            self.fail("filesystem helper unavailable; restart the VM");
            return;
        }
        // FreeBSD 14.4 virtio_p9fs omits virtio_finalize_features(). Accept
        // DRIVER_OK when the negotiated bits are a valid modern set.
        let bad =
            self.driver_features & !FEATURES != 0 || self.driver_features & VIRTIO_F_VERSION_1 == 0;
        if self.status & VIRTIO_STATUS_FEATURES_OK != 0 && bad {
            self.status &= !VIRTIO_STATUS_FEATURES_OK;
        }
        if self.status & VIRTIO_STATUS_DRIVER_OK != 0 && bad {
            self.fail("DRIVER_OK with invalid features");
        }
    }

    fn read_config(&self, offset: usize, size: usize) -> u64 {
        if size == 0 || size > 8 {
            return 0;
        }
        let mut raw = [0u8; 8];
        for (i, slot) in raw.iter_mut().enumerate().take(size) {
            *slot = self.config.get(offset + i).copied().unwrap_or(0);
        }
        u64::from_le_bytes(raw)
    }

    fn process_queue(&mut self) -> bool {
        P9Core::process_queue(self)
    }

    fn consume_config_isr(&mut self) -> bool {
        let raised = self.config_pending;
        self.config_pending = false;
        raised
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::pci::PciHost;
    use crate::memory::GuestMemory;
    use crate::shared_fs::Server;
    use crate::test_support::TestDir;
    use std::sync::Arc;

    struct Local(Server);
    impl NineP for Local {
        fn exchange(&mut self, bytes: &[u8]) -> Result<Vec<u8>, String> {
            if bytes.is_empty() {
                self.0.reset();
                return Ok(Vec::new());
            }
            Ok(self.0.request(bytes))
        }
    }

    struct Idle;
    impl NineP for Idle {
        fn exchange(&mut self, bytes: &[u8]) -> Result<Vec<u8>, String> {
            if bytes.is_empty() {
                Ok(Vec::new())
            } else {
                Err("no 9p server".into())
            }
        }
    }

    #[test]
    fn scatter_response_and_reset_discard() {
        let mem = Arc::new(GuestMemory::from_buffer(0, &vec![0u8; 0x8000]).unwrap());
        let mut dev = P9Core::new("tag".into(), Arc::clone(&mem), Idle);
        dev.status = VIRTIO_STATUS_DRIVER_OK;
        dev.driver_features = VIRTIO_F_VERSION_1;
        dev.queue = VirtQueue {
            max_size: 128,
            size: 8,
            ready: true,
            desc_table: 0x100,
            avail_ring: 0x200,
            used_ring: 0x300,
            ..Default::default()
        };
        mem.write_u64(0x100, 0x1000).unwrap();
        mem.write_u32(0x108, 7).unwrap();
        mem.write_u16(0x10c, VRING_DESC_F_NEXT).unwrap();
        mem.write_u16(0x10e, 1).unwrap();
        mem.write_u64(0x110, 0x2000).unwrap();
        mem.write_u32(0x118, 256).unwrap();
        mem.write_u16(0x11c, VRING_DESC_F_WRITE).unwrap();
        mem.write_u16(0x202, 1).unwrap();
        mem.write_u16(0x204, 0).unwrap();
        mem.write(0x1000, &[7, 0, 0, 0, 120, 42, 0]).unwrap();
        mem.write_u16(0x200, VRING_AVAIL_F_NO_INTERRUPT).unwrap();

        let request = dev.take_request().unwrap().unwrap();
        dev.epoch += 1;
        dev.complete(request, vec![1; 11]).unwrap();
        assert_eq!(mem.read_u16(0x302).unwrap(), 0);
        dev.queue.last_avail_idx = 0;
        let request = dev.take_request().unwrap().unwrap();
        dev.complete(request, vec![3; 11]).unwrap();
        assert_eq!(mem.read_u16(0x302).unwrap(), 1);
        assert_eq!(mem.read_u32(0x308).unwrap(), 11);
        assert_eq!(mem.read_u16(0x2000).unwrap(), 0x0303);
    }

    #[test]
    fn bad_direction_and_short_create_reply_rejected_before_io() {
        let mem = Arc::new(GuestMemory::from_buffer(0, &vec![0u8; 0x8000]).unwrap());
        let mut dev = P9Core::new("tag".into(), Arc::clone(&mem), Idle);
        dev.status = VIRTIO_STATUS_DRIVER_OK;
        dev.driver_features = VIRTIO_F_VERSION_1;
        dev.queue = VirtQueue {
            max_size: 128,
            size: 8,
            ready: true,
            desc_table: 0x100,
            avail_ring: 0x200,
            used_ring: 0x300,
            ..Default::default()
        };
        mem.write_u64(0x100, 0x1000).unwrap();
        mem.write_u32(0x108, 7).unwrap();
        mem.write_u16(0x10c, VRING_DESC_F_NEXT).unwrap();
        mem.write_u16(0x10e, 1).unwrap();
        mem.write_u64(0x110, 0x2000).unwrap();
        mem.write_u32(0x118, 256).unwrap();
        mem.write_u16(0x11c, VRING_DESC_F_WRITE).unwrap();
        mem.write_u16(0x202, 1).unwrap();
        mem.write_u16(0x204, 0).unwrap();
        mem.write(0x1000, &[7, 0, 0, 0, 120, 42, 0]).unwrap();
        mem.write_u16(0x200, VRING_AVAIL_F_NO_INTERRUPT).unwrap();

        mem.write_u16(0x11c, 0).unwrap();
        assert!(dev.take_request().is_err());
        mem.write_u16(0x11c, VRING_DESC_F_WRITE).unwrap();
        mem.write_u32(0x118, 11).unwrap();
        mem.write(0x1004, &[114]).unwrap();
        assert!(dev.take_request().is_err());
        assert_eq!(dev.queue.last_avail_idx, 0);
    }

    #[test]
    fn process_queue_walks_one_version_request() {
        let dir = TestDir::new();
        let server = Server::new(&dir.path("."), true).unwrap();
        let mem = Arc::new(GuestMemory::from_buffer(0, &vec![0u8; 0x8000]).unwrap());
        let mut dev = P9Core::new("host".into(), Arc::clone(&mem), Local(server));
        dev.status = VIRTIO_STATUS_DRIVER_OK;
        dev.driver_features = VIRTIO_F_VERSION_1 | VIRTIO_9P_F_MOUNT_TAG;
        dev.queue.ready = true;
        dev.queue.size = 8;
        dev.queue.desc_table = 0x1000;
        dev.queue.avail_ring = 0x2000;
        dev.queue.used_ring = 0x3000;

        let mut body = Vec::new();
        body.extend_from_slice(&(MAX_MESSAGE as u32).to_le_bytes());
        body.extend_from_slice(&8u16.to_le_bytes());
        body.extend_from_slice(b"9P2000.L");
        let mut req = Vec::new();
        req.extend_from_slice(&((body.len() + 7) as u32).to_le_bytes());
        req.push(100);
        req.extend_from_slice(&7u16.to_le_bytes());
        req.extend_from_slice(&body);
        mem.write(0x4000, &req).unwrap();
        put_desc(&mem, 0, 0x4000, req.len() as u32, VRING_DESC_F_NEXT, 1);
        put_desc(&mem, 1, 0x5000, 128, VRING_DESC_F_WRITE, 0);
        mem.write_u16(0x2002, 1).unwrap();
        mem.write_u16(0x2004, 0).unwrap();

        assert!(dev.process_queue());
        assert_eq!(mem.read_u16(0x3002).unwrap(), 1);
        assert_eq!(mem.read_u32(0x3004).unwrap(), 0);
        let len = mem.read_u32(0x3008).unwrap() as usize;
        assert!(len >= 21);
        let mut got = vec![0u8; len];
        mem.read(0x5000, &mut got).unwrap();
        assert_eq!(got[4], 101);
        assert_eq!(u16::from_le_bytes([got[5], got[6]]), 7);
        assert!(got.windows(8).any(|window| window == b"9P2000.L"));
        assert!(!dev.process_queue());
    }

    #[test]
    fn pci_function_follows_the_disk_on_gsi_18() {
        let mem = Arc::new(GuestMemory::from_buffer(0, &vec![0u8; 0x1000]).unwrap());
        let mut pci = PciHost::new();
        let block_mem = Arc::clone(&mem);
        // No disk image: placement only needs a function in front of 9p.
        let block = crate::devices::virtio::block::VirtioBlock::new(None, 0, "vtbd0", block_mem);
        pci.add_block(block).unwrap();
        let p9 = P9Core::new("work".into(), mem, Idle);
        pci.add_p9(p9).unwrap();
        assert_eq!(pci.virtio_gsis(), vec![16, 18]);
        assert!(pci.prt().contains(&(4 << 16, 18)));
        assert!(!pci.virtio_gsis().contains(&17));

        assert_eq!(cfg_in(&mut pci, 0x1000, 4), 0x1042_1af4);
        assert_eq!(cfg_in(&mut pci, 0x1800, 4), 0xffff_ffff);
        assert_eq!(cfg_in(&mut pci, 0x2000, 4), 0x1049_1af4);
        assert_eq!(cfg_in(&mut pci, 0x2008, 4), 0x0002_0001);
        assert_eq!(cfg_in(&mut pci, 0x202c, 4), 0x0009_1af4);
        assert_eq!((cfg_in(&mut pci, 0x203c, 4) >> 8) & 0xff, 1);

        let mut ptr = cfg_in(&mut pci, 0x2034, 1) as u8;
        let mut types = Vec::new();
        for _ in 0..8 {
            let hdr = cfg_in(&mut pci, 0x2000 | u32::from(ptr), 4);
            assert_eq!(hdr & 0xff, 0x09);
            types.push(((hdr >> 24) & 0xff) as u8);
            let next = ((hdr >> 8) & 0xff) as u8;
            if next == 0 {
                break;
            }
            ptr = next;
        }
        assert_eq!(types, vec![1, 2, 3, 4]);

        cfg(&mut pci, 0x2004, 0x6, 2);
        assert_eq!(pci.mmio_read(0xC000_4000 + 0x3000, 2).unwrap(), 4);
        assert_eq!(
            pci.mmio_read(0xC000_4000 + 0x3002, 4).unwrap(),
            u64::from(u32::from_le_bytes(*b"work"))
        );
    }

    fn put_desc(mem: &GuestMemory, idx: u64, addr: u64, len: u32, flags: u16, next: u16) {
        let at = 0x1000 + idx * 16;
        mem.write_u64(at, addr).unwrap();
        mem.write_u32(at + 8, len).unwrap();
        mem.write_u16(at + 12, flags).unwrap();
        mem.write_u16(at + 14, next).unwrap();
    }

    fn cfg(pci: &mut PciHost, addr: u32, data: u64, size: usize) {
        assert!(pci.try_out(0xCF8, u64::from(0x8000_0000 | addr), 4));
        assert!(pci.try_out(0xCFC, data, size));
    }

    fn cfg_in(pci: &mut PciHost, addr: u32, size: usize) -> u64 {
        assert!(pci.try_out(0xCF8, u64::from(0x8000_0000 | addr), 4));
        pci.try_in(0xCFC, size).unwrap()
    }
}
