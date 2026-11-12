//! Virtio-gpu 2D (device ID 16). One scanout, BGRA formats only.
//! VirGL, blob resources, and EDID are not offered.

use super::*;
use crate::devices::MmioDevice;
use crate::devices::display::{DamageRect, FramePublisher};
use crate::memory::GuestMemory;
use std::collections::HashMap;
use std::sync::Arc;

const OFFERED: u64 = VIRTIO_F_VERSION_1;
const QUEUE_MAX: u32 = 64;
const CONTROL_Q: usize = 0;
const CURSOR_Q: usize = 1;
const HDR_LEN: usize = 24;
const MAX_SCANOUTS: usize = 16;
const MAX_RESOURCES: usize = 64;
const MAX_DIM: u32 = 4096;
const MAX_BYTES: usize = 16 * 1024 * 1024;
const MAX_ENTRIES: usize = 256;
const CURSOR_MAX: u32 = 64;
const FLAG_FENCE: u32 = 1;
const EVENT_DISPLAY: u32 = 1;
const FORMAT_BGRA: u32 = 1;
const FORMAT_BGRX: u32 = 2;

const GET_DISPLAY_INFO: u32 = 0x0100;
const RESOURCE_CREATE_2D: u32 = 0x0101;
const RESOURCE_UNREF: u32 = 0x0102;
const SET_SCANOUT: u32 = 0x0103;
const RESOURCE_FLUSH: u32 = 0x0104;
const TRANSFER_TO_HOST_2D: u32 = 0x0105;
const RESOURCE_ATTACH_BACKING: u32 = 0x0106;
const RESOURCE_DETACH_BACKING: u32 = 0x0107;
const UPDATE_CURSOR: u32 = 0x0300;
const MOVE_CURSOR: u32 = 0x0301;
const RESP_OK_NODATA: u32 = 0x1100;
const RESP_OK_DISPLAY_INFO: u32 = 0x1101;
const RESP_ERR_UNSPEC: u32 = 0x1200;
const RESP_ERR_OUT_OF_MEMORY: u32 = 0x1201;
const RESP_ERR_INVALID_SCANOUT: u32 = 0x1202;
const RESP_ERR_INVALID_RESOURCE: u32 = 0x1203;
const RESP_ERR_INVALID_PARAMETER: u32 = 0x1205;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum GpuMode {
    #[default]
    Simple,
    Virtio,
    None,
}

impl GpuMode {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "simple" => Ok(Self::Simple),
            "virtio" => Ok(Self::Virtio),
            "none" => Ok(Self::None),
            _ => Err("--gpu requires simple, virtio, or none".into()),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct CmdHdr {
    type_: u32,
    flags: u32,
    fence_id: u64,
    ctx_id: u32,
}

impl CmdHdr {
    fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < HDR_LEN {
            return None;
        }
        let u32_at = |off: usize| u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
        Some(Self {
            type_: u32_at(0),
            flags: u32_at(4),
            fence_id: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            ctx_id: u32_at(16),
        })
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl Rect {
    fn parse(buf: &[u8]) -> Self {
        let u32_at = |off: usize| u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
        Self {
            x: u32_at(0),
            y: u32_at(4),
            width: u32_at(8),
            height: u32_at(12),
        }
    }

    fn fits_in(self, width: u32, height: u32) -> bool {
        self.width != 0
            && self.height != 0
            && self
                .x
                .checked_add(self.width)
                .is_some_and(|end| end <= width)
            && self
                .y
                .checked_add(self.height)
                .is_some_and(|end| end <= height)
    }

    /// Shared coordinate space: scanout and flush rectangles both name pixels
    /// in the resource, and that same origin is the scanout on the display.
    fn intersect(self, other: Self) -> Option<Self> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self
            .x
            .saturating_add(self.width)
            .min(other.x.saturating_add(other.width));
        let y1 = self
            .y
            .saturating_add(self.height)
            .min(other.y.saturating_add(other.height));
        if x1 > x0 && y1 > y0 {
            Some(Self {
                x: x0,
                y: y0,
                width: x1 - x0,
                height: y1 - y0,
            })
        } else {
            None
        }
    }
}

struct Resource {
    format: u32,
    width: u32,
    height: u32,
    host: Vec<u8>,
    backing: Vec<(u64, u32)>,
}

struct Cursor {
    resource_id: u32,
    x: u32,
    y: u32,
    hot_x: u32,
    hot_y: u32,
    visible: bool,
    width: u32,
    height: u32,
}

pub struct VirtioGpu {
    transport: MmioTransport,
    mem: Arc<GuestMemory>,
    mode_w: u32,
    mode_h: u32,
    events: u32,
    resources: HashMap<u32, Resource>,
    scanout_resource: u32,
    scanout: Rect,
    cursor: Cursor,
    frames: Arc<FramePublisher>,
    scanout_published: bool,
}

impl VirtioGpu {
    pub fn new(
        irq: u32,
        mem: Arc<GuestMemory>,
        width: u32,
        height: u32,
        frames: Arc<FramePublisher>,
    ) -> Self {
        let (published_w, published_h) = frames.geometry();
        assert_eq!(
            (published_w, published_h),
            (width, height),
            "virtio-gpu scanout and frame publisher disagree"
        );
        let mut transport =
            MmioTransport::new(VIRTIO_DEV_GPU, OFFERED, &[QUEUE_MAX, QUEUE_MAX], irq);
        // EDK2 VirtioGpuDxe does not call SetQueueSize on its modern MMIO path,
        // so VirtioMmioDeviceLib writes QueueNum=0 immediately before enabling
        // an otherwise valid ring. QEMU's common firmware path uses PCI; retain
        // strict QueueNum validation everywhere except this GPU compatibility.
        transport.enable_zero_queue_num_compat();
        Self {
            transport,
            mem,
            mode_w: width,
            mode_h: height,
            events: 0,
            resources: HashMap::new(),
            scanout_resource: 0,
            scanout: Rect::default(),
            cursor: Cursor {
                resource_id: 0,
                x: 0,
                y: 0,
                hot_x: 0,
                hot_y: 0,
                visible: false,
                width: 0,
                height: 0,
            },
            frames,
            scanout_published: false,
        }
    }

    fn config_byte(&self, index: usize) -> u8 {
        let value = match index {
            0..=3 => self.events.to_le_bytes()[index],
            8..=11 => 1u32.to_le_bytes()[index - 8],
            _ => 0,
        };
        value
    }

    fn poke(&self) {
        #[cfg(not(test))]
        unsafe {
            crate::hypervisor::hv_gic_set_spi(
                self.transport.irq(),
                self.transport.interrupt_status() != 0,
            );
        }
        if self.transport.interrupt_status() != 0 {
            crate::vcpu::kick_vcpu();
        }
    }

    fn reset_device(&mut self) {
        self.resources.clear();
        self.scanout_resource = 0;
        self.scanout = Rect::default();
        self.cursor.visible = false;
        self.cursor.resource_id = 0;
        self.cursor.width = 0;
        self.cursor.height = 0;
        self.frames.clear();
        self.events = 0;
        self.scanout_published = false;
    }

    /// Length-check a command and report the response size without mutating.
    fn command_ready(data: &[u8]) -> Result<(CmdHdr, usize), ()> {
        let Some(hdr) = CmdHdr::parse(data) else {
            return Err(());
        };
        let need = match hdr.type_ {
            GET_DISPLAY_INFO => HDR_LEN,
            RESOURCE_CREATE_2D => 40,
            RESOURCE_UNREF | RESOURCE_DETACH_BACKING => 32,
            SET_SCANOUT | RESOURCE_FLUSH => 48,
            TRANSFER_TO_HOST_2D => 56,
            RESOURCE_ATTACH_BACKING => 32,
            UPDATE_CURSOR | MOVE_CURSOR => 56,
            _ => HDR_LEN,
        };
        if data.len() < need {
            return Err(());
        }
        if hdr.type_ == RESOURCE_ATTACH_BACKING {
            let nr = u32::from_le_bytes(data[28..32].try_into().unwrap()) as usize;
            let bytes = 32usize.saturating_add(nr.saturating_mul(16));
            if nr > MAX_ENTRIES || data.len() < bytes {
                return Err(());
            }
        }
        let reply = if hdr.type_ == GET_DISPLAY_INFO {
            HDR_LEN + MAX_SCANOUTS * 24
        } else {
            HDR_LEN
        };
        Ok((hdr, reply))
    }

    fn response(hdr: &CmdHdr, type_: u32, extra: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; HDR_LEN + extra.len()];
        out[0..4].copy_from_slice(&type_.to_le_bytes());
        let flags = hdr.flags & FLAG_FENCE;
        out[4..8].copy_from_slice(&flags.to_le_bytes());
        out[8..16].copy_from_slice(&hdr.fence_id.to_le_bytes());
        out[16..20].copy_from_slice(&hdr.ctx_id.to_le_bytes());
        out[HDR_LEN..].copy_from_slice(extra);
        out
    }

    fn handle(&mut self, data: &[u8]) -> Result<Vec<u8>, ()> {
        let (hdr, _) = Self::command_ready(data)?;
        let reply = match hdr.type_ {
            GET_DISPLAY_INFO => self.display_info(&hdr),
            RESOURCE_CREATE_2D => self.create_2d(&hdr, data),
            RESOURCE_UNREF => self.unref(&hdr, data),
            SET_SCANOUT => self.set_scanout(&hdr, data),
            RESOURCE_FLUSH => self.flush_resource(&hdr, data),
            TRANSFER_TO_HOST_2D => self.transfer(&hdr, data)?,
            RESOURCE_ATTACH_BACKING => self.attach(&hdr, data),
            RESOURCE_DETACH_BACKING => self.detach(&hdr, data),
            UPDATE_CURSOR => self.update_cursor(&hdr, data),
            MOVE_CURSOR => self.move_cursor(&hdr, data),
            _ => Self::response(&hdr, RESP_ERR_UNSPEC, &[]),
        };
        Ok(reply)
    }

    fn display_info(&self, hdr: &CmdHdr) -> Vec<u8> {
        let mut extra = vec![0u8; MAX_SCANOUTS * 24];
        extra[0..4].copy_from_slice(&0u32.to_le_bytes());
        extra[4..8].copy_from_slice(&0u32.to_le_bytes());
        extra[8..12].copy_from_slice(&self.mode_w.to_le_bytes());
        extra[12..16].copy_from_slice(&self.mode_h.to_le_bytes());
        extra[16..20].copy_from_slice(&1u32.to_le_bytes());
        Self::response(hdr, RESP_OK_DISPLAY_INFO, &extra)
    }

    fn create_2d(&mut self, hdr: &CmdHdr, data: &[u8]) -> Vec<u8> {
        let id = u32::from_le_bytes(data[24..28].try_into().unwrap());
        let format = u32::from_le_bytes(data[28..32].try_into().unwrap());
        let width = u32::from_le_bytes(data[32..36].try_into().unwrap());
        let height = u32::from_le_bytes(data[36..40].try_into().unwrap());
        if id == 0 || self.resources.contains_key(&id) {
            return Self::response(hdr, RESP_ERR_INVALID_RESOURCE, &[]);
        }
        if !matches!(format, FORMAT_BGRA | FORMAT_BGRX)
            || width == 0
            || height == 0
            || width > MAX_DIM
            || height > MAX_DIM
        {
            return Self::response(hdr, RESP_ERR_INVALID_PARAMETER, &[]);
        }
        let bytes = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(4);
        if bytes == 0 || bytes > MAX_BYTES || self.resources.len() >= MAX_RESOURCES {
            return Self::response(hdr, RESP_ERR_OUT_OF_MEMORY, &[]);
        }
        self.resources.insert(
            id,
            Resource {
                format,
                width,
                height,
                host: vec![0u8; bytes],
                backing: Vec::new(),
            },
        );
        Self::response(hdr, RESP_OK_NODATA, &[])
    }

    fn unref(&mut self, hdr: &CmdHdr, data: &[u8]) -> Vec<u8> {
        let id = u32::from_le_bytes(data[24..28].try_into().unwrap());
        if self.resources.remove(&id).is_none() {
            return Self::response(hdr, RESP_ERR_INVALID_RESOURCE, &[]);
        }
        if self.scanout_resource == id {
            self.scanout_resource = 0;
            self.scanout = Rect::default();
        }
        if self.cursor.resource_id == id {
            self.cursor.visible = false;
            self.cursor.resource_id = 0;
            self.frames.clear_cursor();
        }
        Self::response(hdr, RESP_OK_NODATA, &[])
    }

    fn set_scanout(&mut self, hdr: &CmdHdr, data: &[u8]) -> Vec<u8> {
        let rect = Rect::parse(&data[24..40]);
        let scanout = u32::from_le_bytes(data[40..44].try_into().unwrap());
        let id = u32::from_le_bytes(data[44..48].try_into().unwrap());
        if scanout != 0 {
            return Self::response(hdr, RESP_ERR_INVALID_SCANOUT, &[]);
        }
        if id == 0 {
            self.scanout_resource = 0;
            self.scanout = Rect::default();
            return Self::response(hdr, RESP_OK_NODATA, &[]);
        }
        let Some(resource) = self.resources.get(&id) else {
            return Self::response(hdr, RESP_ERR_INVALID_RESOURCE, &[]);
        };
        if !rect.fits_in(resource.width, resource.height)
            || rect.width > self.mode_w
            || rect.height > self.mode_h
        {
            return Self::response(hdr, RESP_ERR_INVALID_PARAMETER, &[]);
        }
        self.scanout_resource = id;
        self.scanout = rect;
        Self::response(hdr, RESP_OK_NODATA, &[])
    }

    fn attach(&mut self, hdr: &CmdHdr, data: &[u8]) -> Vec<u8> {
        let id = u32::from_le_bytes(data[24..28].try_into().unwrap());
        let nr = u32::from_le_bytes(data[28..32].try_into().unwrap()) as usize;
        let Some(resource) = self.resources.get(&id) else {
            return Self::response(hdr, RESP_ERR_INVALID_RESOURCE, &[]);
        };
        let need = resource.host.len();
        let mut entries = Vec::with_capacity(nr);
        let mut total = 0usize;
        for i in 0..nr {
            let off = 32 + i * 16;
            let addr = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
            let len = u32::from_le_bytes(data[off + 8..off + 12].try_into().unwrap());
            if len == 0 || addr.checked_add(u64::from(len)).is_none() {
                return Self::response(hdr, RESP_ERR_INVALID_PARAMETER, &[]);
            }
            if self.mem.checked_offset(addr, len as usize).is_err() {
                return Self::response(hdr, RESP_ERR_INVALID_PARAMETER, &[]);
            }
            total = total.saturating_add(len as usize);
            entries.push((addr, len));
        }
        if entries.is_empty() || total < need || total > MAX_BYTES {
            return Self::response(hdr, RESP_ERR_INVALID_PARAMETER, &[]);
        }
        let mut ranges: Vec<(u64, u64)> = entries
            .iter()
            .map(|(addr, len)| (*addr, addr + u64::from(*len)))
            .collect();
        ranges.sort_unstable();
        if ranges.windows(2).any(|pair| pair[1].0 < pair[0].1) {
            return Self::response(hdr, RESP_ERR_INVALID_PARAMETER, &[]);
        }
        self.resources.get_mut(&id).expect("present").backing = entries;
        Self::response(hdr, RESP_OK_NODATA, &[])
    }

    fn detach(&mut self, hdr: &CmdHdr, data: &[u8]) -> Vec<u8> {
        let id = u32::from_le_bytes(data[24..28].try_into().unwrap());
        let Some(resource) = self.resources.get_mut(&id) else {
            return Self::response(hdr, RESP_ERR_INVALID_RESOURCE, &[]);
        };
        resource.backing.clear();
        Self::response(hdr, RESP_OK_NODATA, &[])
    }

    fn transfer(&mut self, hdr: &CmdHdr, data: &[u8]) -> Result<Vec<u8>, ()> {
        let rect = Rect::parse(&data[24..40]);
        let offset = u64::from_le_bytes(data[40..48].try_into().unwrap());
        let id = u32::from_le_bytes(data[48..52].try_into().unwrap());
        let Some(resource) = self.resources.get(&id) else {
            return Ok(Self::response(hdr, RESP_ERR_INVALID_RESOURCE, &[]));
        };
        if resource.backing.is_empty() || !rect.fits_in(resource.width, resource.height) {
            return Ok(Self::response(hdr, RESP_ERR_INVALID_PARAMETER, &[]));
        }
        let stride = resource.width as usize * 4;
        let row_bytes = rect.width as usize * 4;
        let last = (rect.height as u64 - 1)
            .checked_mul(stride as u64)
            .and_then(|rows| rows.checked_add(row_bytes as u64))
            .and_then(|tail| offset.checked_add(tail));
        let Some(end) = last else {
            return Ok(Self::response(hdr, RESP_ERR_INVALID_PARAMETER, &[]));
        };
        let backing_len: u64 = resource
            .backing
            .iter()
            .map(|(_, len)| u64::from(*len))
            .sum();
        if end > backing_len {
            return Ok(Self::response(hdr, RESP_ERR_INVALID_PARAMETER, &[]));
        }
        let mut copied = vec![0u8; rect.height as usize * row_bytes];
        for row in 0..rect.height as usize {
            let at = offset + row as u64 * stride as u64;
            let dest = &mut copied[row * row_bytes..(row + 1) * row_bytes];
            if read_backing(&self.mem, &resource.backing, at, dest).is_err() {
                return Err(());
            }
        }
        let resource = self.resources.get_mut(&id).expect("present");
        for row in 0..rect.height as usize {
            let dst = ((rect.y as usize + row) * resource.width as usize + rect.x as usize) * 4;
            let src = &copied[row * row_bytes..(row + 1) * row_bytes];
            resource.host[dst..dst + row_bytes].copy_from_slice(src);
        }
        Ok(Self::response(hdr, RESP_OK_NODATA, &[]))
    }

    fn flush_resource(&mut self, hdr: &CmdHdr, data: &[u8]) -> Vec<u8> {
        let rect = Rect::parse(&data[24..40]);
        let id = u32::from_le_bytes(data[40..44].try_into().unwrap());
        if self.scanout_resource != id || id == 0 {
            if !self.resources.contains_key(&id) {
                return Self::response(hdr, RESP_ERR_INVALID_RESOURCE, &[]);
            }
            return Self::response(hdr, RESP_OK_NODATA, &[]);
        }
        let Some(resource) = self.resources.get(&id) else {
            return Self::response(hdr, RESP_ERR_INVALID_RESOURCE, &[]);
        };
        if !rect.fits_in(resource.width, resource.height) {
            return Self::response(hdr, RESP_ERR_INVALID_PARAMETER, &[]);
        }
        let Some(visible) = rect.intersect(self.scanout) else {
            return Self::response(hdr, RESP_OK_NODATA, &[]);
        };
        let stride = resource.width as usize * 4;
        let src_off = (visible.y as usize * resource.width as usize + visible.x as usize) * 4;
        let damage = DamageRect {
            x: visible.x - self.scanout.x,
            y: visible.y - self.scanout.y,
            width: visible.width,
            height: visible.height,
        };
        self.frames.blit(damage, &resource.host[src_off..], stride);
        if !self.scanout_published {
            println!(
                "[VirtIO-GPU] scanout active: {}x{} resource {} received first RESOURCE_FLUSH",
                self.scanout.width, self.scanout.height, id
            );
            self.scanout_published = true;
        }
        Self::response(hdr, RESP_OK_NODATA, &[])
    }

    fn update_cursor(&mut self, hdr: &CmdHdr, data: &[u8]) -> Vec<u8> {
        let scanout = u32::from_le_bytes(data[24..28].try_into().unwrap());
        let x = u32::from_le_bytes(data[28..32].try_into().unwrap());
        let y = u32::from_le_bytes(data[32..36].try_into().unwrap());
        let id = u32::from_le_bytes(data[40..44].try_into().unwrap());
        let hot_x = u32::from_le_bytes(data[44..48].try_into().unwrap());
        let hot_y = u32::from_le_bytes(data[48..52].try_into().unwrap());
        if scanout != 0 {
            return Self::response(hdr, RESP_ERR_INVALID_SCANOUT, &[]);
        }
        if id == 0 {
            self.cursor.visible = false;
            self.cursor.resource_id = 0;
            self.cursor.x = x;
            self.cursor.y = y;
            self.frames.clear_cursor();
            return Self::response(hdr, RESP_OK_NODATA, &[]);
        }
        let Some(resource) = self.resources.get(&id) else {
            return Self::response(hdr, RESP_ERR_INVALID_RESOURCE, &[]);
        };
        if resource.width == 0
            || resource.height == 0
            || resource.width > CURSOR_MAX
            || resource.height > CURSOR_MAX
            || hot_x >= resource.width
            || hot_y >= resource.height
        {
            return Self::response(hdr, RESP_ERR_INVALID_PARAMETER, &[]);
        }
        self.cursor.x = x;
        self.cursor.y = y;
        self.cursor.hot_x = hot_x;
        self.cursor.hot_y = hot_y;
        self.cursor.width = resource.width;
        self.cursor.height = resource.height;
        self.cursor.resource_id = id;
        self.cursor.visible = true;
        self.frames.set_cursor(
            x,
            y,
            hot_x,
            hot_y,
            resource.width,
            resource.height,
            resource.format == FORMAT_BGRX,
            &resource.host,
        );
        Self::response(hdr, RESP_OK_NODATA, &[])
    }

    fn move_cursor(&mut self, hdr: &CmdHdr, data: &[u8]) -> Vec<u8> {
        let scanout = u32::from_le_bytes(data[24..28].try_into().unwrap());
        if scanout != 0 {
            return Self::response(hdr, RESP_ERR_INVALID_SCANOUT, &[]);
        }
        self.cursor.x = u32::from_le_bytes(data[28..32].try_into().unwrap());
        self.cursor.y = u32::from_le_bytes(data[32..36].try_into().unwrap());
        if self.cursor.visible {
            self.frames.move_cursor(self.cursor.x, self.cursor.y);
        }
        Self::response(hdr, RESP_OK_NODATA, &[])
    }

    fn process_queue(&mut self, index: usize) {
        // FreeBSD 14's vtgpu_attach() submits GET_DISPLAY_INFO and polls its
        // used ring before returning to the VirtIO bus, which is what sets
        // DRIVER_OK.  Accept queue traffic once FEATURES_OK is valid so that
        // attach-time request can complete.  Normal operation still requires
        // DRIVER_OK everywhere else.
        if !self.transport.operational() && !self.transport.features_accepted() {
            return;
        }
        let Some(mut queue) = self.transport.queue(index).copied() else {
            return;
        };
        if !queue.ready || queue.desc_table == 0 || queue.avail_ring == 0 || queue.used_ring == 0 {
            return;
        }
        let count = match queue.pending_count(&self.mem) {
            Ok(n) => n,
            Err(_) => {
                self.transport.set_needs_reset();
                return;
            }
        };
        let mut curr = queue.last_avail_idx;
        let mut processed = false;
        for _ in 0..count {
            let slot = queue.avail_ring + 4 + 2 * (curr as u64 % queue.size as u64);
            let Ok(head) = self.mem.read_u16(slot) else {
                self.transport.set_needs_reset();
                break;
            };
            let descs = match queue.descriptors(&self.mem, head, false, MAX_BYTES) {
                Ok(descs) if !descs.is_empty() => descs,
                _ => {
                    self.transport.set_needs_reset();
                    break;
                }
            };
            let mut readable = Vec::new();
            let mut writable = Vec::new();
            let mut writing = false;
            let mut bad = false;
            for desc in &descs {
                let is_write = desc.flags & VRING_DESC_F_WRITE != 0;
                if is_write {
                    writing = true;
                    writable.push(*desc);
                } else if writing {
                    bad = true;
                    break;
                } else {
                    let start = readable.len();
                    readable.resize(start + desc.len as usize, 0);
                    if desc.len > 0 && self.mem.read(desc.addr, &mut readable[start..]).is_err() {
                        bad = true;
                        break;
                    }
                }
            }
            if bad {
                self.transport.set_needs_reset();
                break;
            }
            let reply_len = match Self::command_ready(&readable) {
                Ok((_, len)) => len,
                Err(()) => {
                    self.transport.set_needs_reset();
                    break;
                }
            };
            if index == CONTROL_Q && writable.is_empty() {
                self.transport.set_needs_reset();
                break;
            }
            if !writable.is_empty() {
                let cap: usize = writable.iter().map(|d| d.len as usize).sum();
                if cap < reply_len {
                    self.transport.set_needs_reset();
                    break;
                }
            }
            let reply = match self.handle(&readable) {
                Ok(bytes) => bytes,
                Err(()) => {
                    self.transport.set_needs_reset();
                    break;
                }
            };
            let mut written = 0u32;
            if !writable.is_empty() {
                if write_chain(&self.mem, &writable, &reply).is_err() {
                    self.transport.set_needs_reset();
                    break;
                }
                written = reply.len() as u32;
            }
            publish_used(&self.mem, &mut queue, head, written);
            let diag = self.transport.diagnostics_mut();
            diag.completions = diag.completions.saturating_add(1);
            diag.bytes = diag.bytes.saturating_add(u64::from(written));
            curr = curr.wrapping_add(1);
            processed = true;
        }
        queue.last_avail_idx = curr;
        let avail = queue.avail_ring;
        if let Some(slot) = self.transport.queue_mut(index) {
            *slot = queue;
        }
        if processed && avail_wants_interrupt(&self.mem, avail) {
            self.transport.raise_interrupt(1);
            self.poke();
        }
    }
}

fn read_backing(
    mem: &GuestMemory,
    entries: &[(u64, u32)],
    offset: u64,
    dest: &mut [u8],
) -> Result<(), ()> {
    let mut skip = offset;
    let mut filled = 0;
    for &(addr, len) in entries {
        let len = u64::from(len);
        if skip >= len {
            skip -= len;
            continue;
        }
        let take = ((len - skip) as usize).min(dest.len() - filled);
        let gpa = addr + skip;
        mem.read(gpa, &mut dest[filled..filled + take])
            .map_err(|_| ())?;
        filled += take;
        skip = 0;
        if filled == dest.len() {
            return Ok(());
        }
    }
    Err(())
}

fn write_chain(mem: &GuestMemory, descs: &[Descriptor], data: &[u8]) -> Result<(), ()> {
    let mut off = 0;
    for desc in descs {
        if off >= data.len() {
            break;
        }
        let n = (desc.len as usize).min(data.len() - off);
        if n > 0 {
            mem.write(desc.addr, &data[off..off + n]).map_err(|_| ())?;
            off += n;
        }
    }
    if off < data.len() { Err(()) } else { Ok(()) }
}

impl MmioDevice for VirtioGpu {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if offset >= VIRTIO_MMIO_CONFIG {
            let start = (offset - VIRTIO_MMIO_CONFIG) as usize;
            let mut value = 0u64;
            for byte in 0..size.min(8) {
                value |= u64::from(self.config_byte(start + byte)) << (8 * byte);
            }
            return value;
        }
        self.transport.read(offset)
    }

    fn write(&mut self, offset: u64, data: u64, size: usize) {
        if offset >= VIRTIO_MMIO_CONFIG {
            if offset == VIRTIO_MMIO_CONFIG + 4 && size == 4 {
                self.events &= !data as u32;
            }
            return;
        }
        let effect = self.transport.write(offset, data);
        match effect {
            WriteEffect::Notify { queue: 0 } => self.process_queue(0),
            WriteEffect::Notify { queue: 1 } => self.process_queue(1),
            WriteEffect::Reset => {
                self.reset_device();
                self.poke();
            }
            WriteEffect::InterruptAck { .. } => self.poke(),
            WriteEffect::DriverOk => {}
            _ => {}
        }
    }

    fn shutdown(&mut self) -> Result<(), String> {
        println!("[VirtIO-GPU] {}", self.transport.diagnostics_line());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MEM: u64 = 0x4000_0000;
    const MEM_LEN: usize = 0x4_0000;
    const CMD: u64 = MEM + 0x1000;
    const RESP: u64 = MEM + 0x3000;
    const BACK: u64 = MEM + 0x5000;
    const DESC: u64 = MEM + 0x2_0000;
    const AVAIL: u64 = MEM + 0x2_1000;
    const USED: u64 = MEM + 0x2_2000;
    const CDESC: u64 = MEM + 0x2_3000;
    const CAVAIL: u64 = MEM + 0x2_4000;
    const CUSED: u64 = MEM + 0x2_5000;

    fn setup(w: u32, h: u32) -> (Arc<GuestMemory>, Arc<FramePublisher>, VirtioGpu) {
        let mem = Arc::new(GuestMemory::new_test(MEM, MEM_LEN));
        let frames = Arc::new(FramePublisher::new(w, h));
        let dev = VirtioGpu::new(48, mem.clone(), w, h, frames.clone());
        (mem, frames, dev)
    }

    fn negotiate(dev: &mut VirtioGpu) {
        dev.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1, 4);
        dev.write(VIRTIO_MMIO_DRIVER_FEATURES, 1, 4);
        let status = (VIRTIO_STATUS_ACKNOWLEDGE
            | VIRTIO_STATUS_DRIVER
            | VIRTIO_STATUS_FEATURES_OK
            | VIRTIO_STATUS_DRIVER_OK) as u64;
        dev.write(VIRTIO_MMIO_STATUS, status, 4);
    }

    fn arm(dev: &mut VirtioGpu, mem: &GuestMemory) {
        dev.write(VIRTIO_MMIO_QUEUE_SEL, 0, 4);
        dev.write(VIRTIO_MMIO_QUEUE_NUM, 8, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DESC_LOW, DESC, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DRIVER_LOW, AVAIL, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DEVICE_LOW, USED, 4);
        mem.write_u16(AVAIL, VRING_AVAIL_F_NO_INTERRUPT).unwrap();
        dev.write(VIRTIO_MMIO_QUEUE_READY, 1, 4);
        dev.write(VIRTIO_MMIO_QUEUE_SEL, 1, 4);
        dev.write(VIRTIO_MMIO_QUEUE_NUM, 8, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DESC_LOW, CDESC, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DRIVER_LOW, CAVAIL, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DEVICE_LOW, CUSED, 4);
        mem.write_u16(CAVAIL, VRING_AVAIL_F_NO_INTERRUPT).unwrap();
        dev.write(VIRTIO_MMIO_QUEUE_READY, 1, 4);
    }

    fn desc(mem: &GuestMemory, at: u64, addr: u64, len: u32, flags: u16, next: u16) {
        mem.write_u64(at, addr).unwrap();
        mem.write_u32(at + 8, len).unwrap();
        mem.write_u16(at + 12, flags).unwrap();
        mem.write_u16(at + 14, next).unwrap();
    }

    fn submit(mem: &GuestMemory, dev: &mut VirtioGpu, seq: &mut u16, bytes: &[u8], resp_len: u32) {
        mem.write(CMD, bytes).unwrap();
        mem.write(RESP, &vec![0xA5u8; resp_len as usize]).unwrap();
        desc(mem, DESC, CMD, bytes.len() as u32, VRING_DESC_F_NEXT, 1);
        desc(mem, DESC + 16, RESP, resp_len, VRING_DESC_F_WRITE, 0);
        let slot = u64::from(*seq % 8);
        mem.write_u16(AVAIL + 4 + slot * 2, 0).unwrap();
        *seq = seq.wrapping_add(1);
        mem.write_u16(AVAIL + 2, *seq).unwrap();
        dev.write(VIRTIO_MMIO_QUEUE_NOTIFY, 0, 4);
    }

    fn resp_type(mem: &GuestMemory) -> u32 {
        mem.read_u32(RESP).unwrap()
    }

    fn hdr(type_: u32) -> Vec<u8> {
        let mut out = vec![0u8; 24];
        out[0..4].copy_from_slice(&type_.to_le_bytes());
        out
    }

    fn push_u32(buf: &mut Vec<u8>, value: u32) {
        buf.extend_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn config_offers_one_scanout_and_no_3d_features() {
        let (_mem, _frames, mut dev) = setup(8, 4);
        assert_eq!(dev.read(VIRTIO_MMIO_DEVICE_ID, 4), 16);
        assert_eq!(dev.read(VIRTIO_MMIO_DEVICE_FEATURES, 4), 0);
        dev.write(VIRTIO_MMIO_DEVICE_FEATURES_SEL, 1, 4);
        assert_eq!(dev.read(VIRTIO_MMIO_DEVICE_FEATURES, 4), 1);
        assert_eq!(dev.read(VIRTIO_MMIO_CONFIG + 8, 4), 1);
        assert_eq!(dev.read(VIRTIO_MMIO_CONFIG + 12, 4), 0);
        dev.write(VIRTIO_MMIO_QUEUE_SEL, 0, 4);
        assert_eq!(dev.read(VIRTIO_MMIO_QUEUE_NUM_MAX, 4), QUEUE_MAX as u64);
        dev.write(VIRTIO_MMIO_QUEUE_SEL, 2, 4);
        assert_eq!(dev.read(VIRTIO_MMIO_QUEUE_NUM_MAX, 4), 0);
        assert_eq!(GpuMode::parse("virtio").unwrap(), GpuMode::Virtio);
        assert!(GpuMode::parse("venus").is_err());
    }

    #[test]
    fn display_info_and_fragmented_flush_publish_damage() {
        let (mem, frames, mut dev) = setup(4, 2);
        negotiate(&mut dev);
        arm(&mut dev, &mem);
        assert_eq!(dev.read(VIRTIO_MMIO_CONFIG, 4), 0);
        dev.write(VIRTIO_MMIO_CONFIG + 4, u64::from(EVENT_DISPLAY), 4);
        assert_eq!(dev.read(VIRTIO_MMIO_CONFIG, 4), 0);
        let mut seq = 0u16;
        submit(&mem, &mut dev, &mut seq, &hdr(GET_DISPLAY_INFO), 408);
        assert_eq!(resp_type(&mem), RESP_OK_DISPLAY_INFO);
        assert_eq!(mem.read_u32(RESP + 24 + 8).unwrap(), 4);
        assert_eq!(mem.read_u32(RESP + 24 + 12).unwrap(), 2);
        assert_eq!(mem.read_u32(RESP + 24 + 16).unwrap(), 1);

        let mut create = hdr(RESOURCE_CREATE_2D);
        push_u32(&mut create, 7);
        push_u32(&mut create, FORMAT_BGRA);
        push_u32(&mut create, 4);
        push_u32(&mut create, 2);
        submit(&mem, &mut dev, &mut seq, &create, 24);
        assert_eq!(resp_type(&mem), RESP_OK_NODATA);

        let mut bad_fmt = hdr(RESOURCE_CREATE_2D);
        push_u32(&mut bad_fmt, 8);
        push_u32(&mut bad_fmt, 67);
        push_u32(&mut bad_fmt, 1);
        push_u32(&mut bad_fmt, 1);
        submit(&mem, &mut dev, &mut seq, &bad_fmt, 24);
        assert_eq!(resp_type(&mem), RESP_ERR_INVALID_PARAMETER);

        mem.write(BACK, &[0x11; 16]).unwrap();
        mem.write(BACK + 16, &[0x22; 16]).unwrap();
        let mut attach = hdr(RESOURCE_ATTACH_BACKING);
        push_u32(&mut attach, 7);
        push_u32(&mut attach, 2);
        attach.extend_from_slice(&BACK.to_le_bytes());
        push_u32(&mut attach, 16);
        push_u32(&mut attach, 0);
        attach.extend_from_slice(&(BACK + 16).to_le_bytes());
        push_u32(&mut attach, 16);
        push_u32(&mut attach, 0);
        submit(&mem, &mut dev, &mut seq, &attach, 24);
        assert_eq!(resp_type(&mem), RESP_OK_NODATA);

        let mut overlap = hdr(RESOURCE_ATTACH_BACKING);
        push_u32(&mut overlap, 7);
        push_u32(&mut overlap, 2);
        overlap.extend_from_slice(&BACK.to_le_bytes());
        push_u32(&mut overlap, 16);
        push_u32(&mut overlap, 0);
        overlap.extend_from_slice(&BACK.to_le_bytes());
        push_u32(&mut overlap, 16);
        push_u32(&mut overlap, 0);
        submit(&mem, &mut dev, &mut seq, &overlap, 24);
        assert_eq!(resp_type(&mem), RESP_ERR_INVALID_PARAMETER);
        assert_eq!(frames.copy_published(&mut [0u8; 32]), false);

        let mut xfer = hdr(TRANSFER_TO_HOST_2D);
        push_u32(&mut xfer, 0);
        push_u32(&mut xfer, 0);
        push_u32(&mut xfer, 4);
        push_u32(&mut xfer, 2);
        xfer.extend_from_slice(&0u64.to_le_bytes());
        push_u32(&mut xfer, 7);
        push_u32(&mut xfer, 0);
        submit(&mem, &mut dev, &mut seq, &xfer, 24);
        assert_eq!(resp_type(&mem), RESP_OK_NODATA);

        let mut huge = hdr(TRANSFER_TO_HOST_2D);
        push_u32(&mut huge, 3);
        push_u32(&mut huge, 0);
        push_u32(&mut huge, 2);
        push_u32(&mut huge, 1);
        huge.extend_from_slice(&0u64.to_le_bytes());
        push_u32(&mut huge, 7);
        push_u32(&mut huge, 0);
        submit(&mem, &mut dev, &mut seq, &huge, 24);
        assert_eq!(resp_type(&mem), RESP_ERR_INVALID_PARAMETER);

        let mut scan = hdr(SET_SCANOUT);
        push_u32(&mut scan, 0);
        push_u32(&mut scan, 0);
        push_u32(&mut scan, 4);
        push_u32(&mut scan, 2);
        push_u32(&mut scan, 0);
        push_u32(&mut scan, 7);
        submit(&mem, &mut dev, &mut seq, &scan, 24);
        assert_eq!(resp_type(&mem), RESP_OK_NODATA);

        let mut flush = hdr(RESOURCE_FLUSH);
        push_u32(&mut flush, 0);
        push_u32(&mut flush, 0);
        push_u32(&mut flush, 4);
        push_u32(&mut flush, 2);
        push_u32(&mut flush, 7);
        push_u32(&mut flush, 0);
        submit(&mem, &mut dev, &mut seq, &flush, 24);
        assert_eq!(resp_type(&mem), RESP_OK_NODATA);
        let mut pixels = [0u8; 32];
        assert!(frames.copy_published(&mut pixels));
        assert_eq!(&pixels[..16], &[0x11; 16]);
        assert_eq!(&pixels[16..], &[0x22; 16]);
        let damage = frames.take_damage().unwrap();
        assert_eq!(
            damage,
            vec![DamageRect {
                x: 0,
                y: 0,
                width: 4,
                height: 2
            }]
        );
        assert!(frames.take_damage().unwrap().is_empty());

        dev.write(VIRTIO_MMIO_STATUS, 0, 4);
        assert!(!frames.copy_published(&mut pixels));
    }

    #[test]
    fn short_response_does_not_create_a_resource() {
        let (mem, _frames, mut dev) = setup(4, 2);
        negotiate(&mut dev);
        arm(&mut dev, &mem);
        let mut seq = 0u16;
        let mut create = hdr(RESOURCE_CREATE_2D);
        push_u32(&mut create, 7);
        push_u32(&mut create, FORMAT_BGRA);
        push_u32(&mut create, 4);
        push_u32(&mut create, 2);
        submit(&mem, &mut dev, &mut seq, &create, 8);
        assert_ne!(
            dev.read(VIRTIO_MMIO_STATUS, 4) & u64::from(VIRTIO_STATUS_NEEDS_RESET),
            0
        );
        assert!(dev.resources.is_empty());
        let mut got = [0u8; 8];
        mem.read(RESP, &mut got).unwrap();
        assert_eq!(got, [0xA5; 8]);
    }

    #[test]
    fn flush_intersects_scanout_and_copies_that_rectangle() {
        let (mem, frames, mut dev) = setup(4, 2);
        negotiate(&mut dev);
        arm(&mut dev, &mem);
        let mut seq = 0u16;
        let mut create = hdr(RESOURCE_CREATE_2D);
        push_u32(&mut create, 7);
        push_u32(&mut create, FORMAT_BGRA);
        push_u32(&mut create, 4);
        push_u32(&mut create, 2);
        submit(&mem, &mut dev, &mut seq, &create, 24);
        assert_eq!(resp_type(&mem), RESP_OK_NODATA);

        let mut pixels = [0u8; 32];
        for (i, byte) in pixels.iter_mut().enumerate() {
            *byte = (i as u8 / 4) + 1;
        }
        mem.write(BACK, &pixels).unwrap();
        let mut attach = hdr(RESOURCE_ATTACH_BACKING);
        push_u32(&mut attach, 7);
        push_u32(&mut attach, 1);
        attach.extend_from_slice(&BACK.to_le_bytes());
        push_u32(&mut attach, 32);
        push_u32(&mut attach, 0);
        submit(&mem, &mut dev, &mut seq, &attach, 24);
        assert_eq!(resp_type(&mem), RESP_OK_NODATA);

        let mut xfer = hdr(TRANSFER_TO_HOST_2D);
        push_u32(&mut xfer, 0);
        push_u32(&mut xfer, 0);
        push_u32(&mut xfer, 4);
        push_u32(&mut xfer, 2);
        xfer.extend_from_slice(&0u64.to_le_bytes());
        push_u32(&mut xfer, 7);
        push_u32(&mut xfer, 0);
        submit(&mem, &mut dev, &mut seq, &xfer, 24);
        assert_eq!(resp_type(&mem), RESP_OK_NODATA);

        let mut scan = hdr(SET_SCANOUT);
        push_u32(&mut scan, 2);
        push_u32(&mut scan, 0);
        push_u32(&mut scan, 2);
        push_u32(&mut scan, 2);
        push_u32(&mut scan, 0);
        push_u32(&mut scan, 7);
        submit(&mem, &mut dev, &mut seq, &scan, 24);
        assert_eq!(resp_type(&mem), RESP_OK_NODATA);

        let mut flush = hdr(RESOURCE_FLUSH);
        push_u32(&mut flush, 0);
        push_u32(&mut flush, 0);
        push_u32(&mut flush, 4);
        push_u32(&mut flush, 2);
        push_u32(&mut flush, 7);
        push_u32(&mut flush, 0);
        submit(&mem, &mut dev, &mut seq, &flush, 24);
        assert_eq!(resp_type(&mem), RESP_OK_NODATA);

        let mut published = [0u8; 32];
        assert!(frames.copy_published(&mut published));
        assert_eq!(&published[..8], &pixels[8..16]);
        assert_eq!(&published[8..16], &[0u8; 8]);
        assert_eq!(&published[16..24], &pixels[24..]);
        assert_eq!(&published[24..], &[0u8; 8]);
        assert_eq!(
            frames.take_damage().unwrap(),
            vec![DamageRect {
                x: 0,
                y: 0,
                width: 2,
                height: 2
            }]
        );
    }

    #[test]
    fn short_header_needs_reset_without_writing_the_response() {
        let (mem, _frames, mut dev) = setup(4, 2);
        negotiate(&mut dev);
        arm(&mut dev, &mem);
        let mut seq = 0u16;
        submit(&mem, &mut dev, &mut seq, &[0u8; 8], 24);
        assert_ne!(
            dev.read(VIRTIO_MMIO_STATUS, 4) & u64::from(VIRTIO_STATUS_NEEDS_RESET),
            0
        );
        assert_eq!(mem.read_u16(USED + 2).unwrap(), 0);
        let mut got = [0u8; 24];
        mem.read(RESP, &mut got).unwrap();
        assert_eq!(got, [0xA5; 24]);
    }

    #[test]
    fn cursor_hot_spot_move_and_reset_clear_resources() {
        let (mem, _frames, mut dev) = setup(8, 8);
        negotiate(&mut dev);
        arm(&mut dev, &mem);
        let mut seq = 0u16;
        let mut create = hdr(RESOURCE_CREATE_2D);
        push_u32(&mut create, 3);
        push_u32(&mut create, FORMAT_BGRX);
        push_u32(&mut create, 2);
        push_u32(&mut create, 2);
        submit(&mem, &mut dev, &mut seq, &create, 24);
        assert_eq!(resp_type(&mem), RESP_OK_NODATA);

        let mut cursor = hdr(UPDATE_CURSOR);
        push_u32(&mut cursor, 0);
        push_u32(&mut cursor, 5);
        push_u32(&mut cursor, 6);
        push_u32(&mut cursor, 0);
        push_u32(&mut cursor, 3);
        push_u32(&mut cursor, 1);
        push_u32(&mut cursor, 0);
        push_u32(&mut cursor, 0);
        submit(&mem, &mut dev, &mut seq, &cursor, 24);
        assert_eq!(resp_type(&mem), RESP_OK_NODATA);
        assert!(dev.cursor.visible);
        assert_eq!((dev.cursor.hot_x, dev.cursor.hot_y), (1, 0));

        let mut bad_hot = cursor.clone();
        bad_hot[44..48].copy_from_slice(&9u32.to_le_bytes());
        submit(&mem, &mut dev, &mut seq, &bad_hot, 24);
        assert_eq!(resp_type(&mem), RESP_ERR_INVALID_PARAMETER);
        assert_eq!(dev.cursor.hot_x, 1);

        desc(&mem, CDESC, CMD, 0, 0, 0);
        let mut mov = hdr(MOVE_CURSOR);
        push_u32(&mut mov, 0);
        push_u32(&mut mov, 3);
        push_u32(&mut mov, 4);
        push_u32(&mut mov, 0);
        push_u32(&mut mov, 0);
        push_u32(&mut mov, 0);
        push_u32(&mut mov, 0);
        push_u32(&mut mov, 0);
        mem.write(CMD, &mov).unwrap();
        desc(&mem, CDESC, CMD, mov.len() as u32, 0, 0);
        mem.write_u16(CAVAIL + 4, 0).unwrap();
        mem.write_u16(CAVAIL + 2, 1).unwrap();
        dev.write(VIRTIO_MMIO_QUEUE_NOTIFY, 1, 4);
        assert_eq!((dev.cursor.x, dev.cursor.y), (3, 4));

        let unknown = hdr(0x0200);
        submit(&mem, &mut dev, &mut seq, &unknown, 24);
        assert_eq!(resp_type(&mem), RESP_ERR_UNSPEC);
        assert_eq!(
            dev.read(VIRTIO_MMIO_STATUS, 4) & u64::from(VIRTIO_STATUS_NEEDS_RESET),
            0
        );

        dev.write(VIRTIO_MMIO_STATUS, 0, 4);
        assert!(dev.resources.is_empty());
        assert!(!dev.cursor.visible);
        assert_eq!(dev.read(VIRTIO_MMIO_CONFIG, 4), 0);
    }

    #[test]
    fn freebsd_attach_can_poll_display_info_before_driver_ok() {
        let (mem, _frames, mut dev) = setup(4, 2);
        dev.write(VIRTIO_MMIO_DRIVER_FEATURES_SEL, 1, 4);
        dev.write(VIRTIO_MMIO_DRIVER_FEATURES, 1, 4);
        let status =
            (VIRTIO_STATUS_ACKNOWLEDGE | VIRTIO_STATUS_DRIVER | VIRTIO_STATUS_FEATURES_OK) as u64;
        dev.write(VIRTIO_MMIO_STATUS, status, 4);
        assert!(!dev.transport.operational());
        assert!(dev.transport.features_accepted());
        arm(&mut dev, &mem);

        let mut seq = 0;
        submit(&mem, &mut dev, &mut seq, &hdr(GET_DISPLAY_INFO), 408);
        assert_eq!(mem.read_u16(USED + 2).unwrap(), 1);
        assert_eq!(resp_type(&mem), RESP_OK_DISPLAY_INFO);
    }

    #[test]
    fn edk2_zero_queue_num_compat_completes_display_info() {
        let (mem, _frames, mut dev) = setup(4, 2);
        negotiate(&mut dev);
        dev.write(VIRTIO_MMIO_QUEUE_SEL, 0, 4);
        // EDK2 VirtioGpuDxe's modern MMIO path leaves its cached QueueNum at
        // zero before programming the otherwise valid control ring.
        dev.write(VIRTIO_MMIO_QUEUE_NUM, 0, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DESC_LOW, DESC, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DRIVER_LOW, AVAIL, 4);
        dev.write(VIRTIO_MMIO_QUEUE_DEVICE_LOW, USED, 4);
        mem.write_u16(AVAIL, VRING_AVAIL_F_NO_INTERRUPT).unwrap();
        dev.write(VIRTIO_MMIO_QUEUE_READY, 1, 4);
        assert_eq!(dev.read(VIRTIO_MMIO_QUEUE_READY, 4), 1);
        assert_eq!(dev.transport.queue(0).unwrap().size, QUEUE_MAX);

        let mut seq = 0;
        submit(&mem, &mut dev, &mut seq, &hdr(GET_DISPLAY_INFO), 408);
        assert_eq!(mem.read_u16(USED + 2).unwrap(), 1);
        assert_eq!(resp_type(&mem), RESP_OK_DISPLAY_INFO);
    }

    #[test]
    fn scanout_crop_uses_resource_coordinates_and_display_relative_damage() {
        let (_mem, frames, mut dev) = setup(4, 2);
        let mut host = Vec::new();
        for y in 0..2u8 {
            for x in 0..6u8 {
                host.extend_from_slice(&[x, y, 0, 255]);
            }
        }
        dev.resources.insert(
            7,
            Resource {
                format: FORMAT_BGRX,
                width: 6,
                height: 2,
                host,
                backing: Vec::new(),
            },
        );

        let mut set = hdr(SET_SCANOUT);
        for value in [2, 0, 4, 2, 0, 7] {
            push_u32(&mut set, value);
        }
        let set_hdr = CmdHdr::parse(&set).unwrap();
        assert_eq!(
            u32::from_le_bytes(dev.set_scanout(&set_hdr, &set)[0..4].try_into().unwrap()),
            RESP_OK_NODATA
        );

        let mut flush = hdr(RESOURCE_FLUSH);
        for value in [2, 0, 4, 2, 7, 0] {
            push_u32(&mut flush, value);
        }
        let flush_hdr = CmdHdr::parse(&flush).unwrap();
        assert_eq!(
            u32::from_le_bytes(
                dev.flush_resource(&flush_hdr, &flush)[0..4]
                    .try_into()
                    .unwrap()
            ),
            RESP_OK_NODATA
        );
        let mut pixels = [0u8; 32];
        assert!(frames.copy_published(&mut pixels));
        assert_eq!(&pixels[0..4], &[2, 0, 0, 255]);
        assert_eq!(&pixels[12..16], &[5, 0, 0, 255]);
        assert_eq!(&pixels[16..20], &[2, 1, 0, 255]);
        assert_eq!(
            frames.take_damage().unwrap().last(),
            Some(&DamageRect {
                x: 0,
                y: 0,
                width: 4,
                height: 2,
            })
        );
    }
}
