//! fw_cfg item list, big-endian file directory, and DMA descriptor.

use std::sync::{Arc, Mutex};

use crate::memory::GuestMemory;

use super::display::{self, Ramfb};
use super::{MmioDevice, PioDevice};

const FW_CFG_SIGNATURE: u16 = 0x00;
const FW_CFG_ID: u16 = 0x01;
const FW_CFG_FILE_DIR: u16 = 0x19;
const FW_CFG_FILE_FIRST: u16 = 0x20;

const FW_CFG_VERSION: u32 = 1;
const FW_CFG_VERSION_DMA: u32 = 2;

const FW_CFG_DMA_CTL_ERROR: u32 = 0x01;
const FW_CFG_DMA_CTL_READ: u32 = 0x02;
const FW_CFG_DMA_CTL_SKIP: u32 = 0x04;
const FW_CFG_DMA_CTL_SELECT: u32 = 0x08;
const FW_CFG_DMA_CTL_WRITE: u32 = 0x10;

const REG_DATA: u64 = 0x00;
const REG_SELECTOR: u64 = 0x08;
const REG_DMA: u64 = 0x10;

struct FwCfgItem {
    selector: u16,
    name: [u8; 56],
    data: Vec<u8>,
    writable: bool,
}

pub struct FwCfg {
    items: Vec<FwCfgItem>,
    selector: u16,
    offset: usize,
    dma_addr: u64,
    ram: Arc<GuestMemory>,
    /// `etc/ramfb`. Guest DMA writes update the framebuffer reader.
    ramfb: Option<Arc<Ramfb>>,
}

impl FwCfg {
    pub fn from_files(ram: Arc<GuestMemory>, files: &[(&str, &[u8])]) -> Self {
        Self::build(ram, files, None)
    }

    /// Same directory as [`Self::from_files`], plus a writable `etc/ramfb` blob.
    pub fn from_files_with_ramfb(
        ram: Arc<GuestMemory>,
        files: &[(&str, &[u8])],
        ramfb: Arc<Ramfb>,
    ) -> Self {
        Self::build(ram, files, Some(ramfb))
    }

    fn build(ram: Arc<GuestMemory>, files: &[(&str, &[u8])], ramfb: Option<Arc<Ramfb>>) -> Self {
        let mut items = Vec::new();
        items.push(fixed_item(FW_CFG_SIGNATURE, b"QEMU".to_vec()));
        items.push(fixed_item(
            FW_CFG_ID,
            (FW_CFG_VERSION | FW_CFG_VERSION_DMA).to_le_bytes().to_vec(),
        ));

        for (index, (name, data)) in files.iter().enumerate() {
            items.push(file_item(
                FW_CFG_FILE_FIRST + index as u16,
                name,
                data.to_vec(),
                false,
            ));
        }
        if let Some(ramfb) = &ramfb {
            let index = files.len();
            let cfg = ramfb.config();
            items.push(file_item(
                FW_CFG_FILE_FIRST + index as u16,
                "etc/ramfb",
                display::descriptor_bytes(&cfg).to_vec(),
                true,
            ));
        }

        let dir = build_file_dir(&items);
        items.insert(2, fixed_item(FW_CFG_FILE_DIR, dir));

        Self {
            items,
            selector: 0,
            offset: 0,
            dma_addr: 0,
            ram,
            ramfb,
        }
    }

    fn current_data(&self) -> &[u8] {
        self.items
            .iter()
            .find(|i| i.selector == self.selector)
            .map(|i| i.data.as_slice())
            .unwrap_or(&[])
    }

    /// PIO transport at `0x510`. Selector writes are native little-endian:
    /// OVMF `IoWrite16` does not swap, and QEMU's combined I/O port does not either.
    pub fn pio_in(&mut self, offset: u16, size: usize) -> u64 {
        match offset {
            0 | 1 => {
                let mut val = 0u64;
                for i in 0..size {
                    val |= u64::from(self.read_data_byte()) << (8 * i);
                }
                val
            }
            _ => 0,
        }
    }

    pub fn pio_out(&mut self, offset: u16, data: u64, size: usize) {
        match (offset, size) {
            (0, 2) | (0, 4) => self.select(data as u16),
            (4, 4) => {
                let hi = u64::from((data as u32).swap_bytes());
                self.dma_addr = (self.dma_addr & 0xffff_ffff) | (hi << 32);
            }
            (8, 4) => {
                let lo = u64::from((data as u32).swap_bytes());
                self.dma_addr = (self.dma_addr & 0xffff_ffff_0000_0000) | lo;
                self.do_dma();
            }
            _ => {}
        }
    }

    fn select(&mut self, sel: u16) {
        self.selector = sel;
        self.offset = 0;
    }

    fn read_data_byte(&mut self) -> u8 {
        let data = self.current_data();
        if self.offset < data.len() {
            let b = data[self.offset];
            self.offset += 1;
            b
        } else {
            0
        }
    }

    fn do_dma(&mut self) {
        let addr = self.dma_addr;
        if addr == 0 {
            return;
        }
        let mut desc = [0u8; 16];
        if self.ram.read(addr, &mut desc).is_err() {
            return;
        }
        let control = u32::from_be_bytes(desc[0..4].try_into().unwrap());
        let length = u32::from_be_bytes(desc[4..8].try_into().unwrap()) as usize;
        let dma_buf = u64::from_be_bytes(desc[8..16].try_into().unwrap());

        if control & FW_CFG_DMA_CTL_SELECT != 0 {
            self.select((control >> 16) as u16);
        }

        let mut err = 0u32;
        if control & FW_CFG_DMA_CTL_READ != 0
            && (length > 16 * 1024 * 1024 || self.ram.checked_offset(dma_buf, length).is_err())
        {
            err = FW_CFG_DMA_CTL_ERROR;
        } else if control & FW_CFG_DMA_CTL_READ != 0 {
            let data = self.current_data();
            let start = self.offset.min(data.len());
            let end = (start + length).min(data.len());
            let chunk = &data[start..end];
            if self.ram.write(dma_buf, chunk).is_err() {
                err = FW_CFG_DMA_CTL_ERROR;
            } else if chunk.len() < length {
                let zeros = vec![0u8; length - chunk.len()];
                let _ = self.ram.write(dma_buf + chunk.len() as u64, &zeros);
            }
            self.offset = end;
        } else if control & FW_CFG_DMA_CTL_WRITE != 0 {
            if length > 0 && !self.dma_write(dma_buf, length) {
                err = FW_CFG_DMA_CTL_ERROR;
            }
        } else if control & FW_CFG_DMA_CTL_SKIP != 0 {
            self.offset = self.offset.saturating_add(length);
        }

        let result = err.to_be_bytes();
        let _ = self.ram.write(addr, &result);
    }

    /// Copy `length` guest bytes into the selected file. Only `etc/ramfb` is writable.
    /// A write that does not fit is refused without changing the blob.
    fn dma_write(&mut self, dma_buf: u64, length: usize) -> bool {
        if length > 16 * 1024 * 1024 {
            return false;
        }
        let selector = self.selector;
        let offset = self.offset;
        let Some(index) = self.items.iter().position(|item| item.selector == selector) else {
            return false;
        };
        let item = &self.items[index];
        let Some(end) = offset.checked_add(length) else {
            return false;
        };
        if !item.writable
            || end > item.data.len()
            || self.ram.checked_offset(dma_buf, length).is_err()
        {
            return false;
        }
        let mut tmp = vec![0u8; length];
        if self.ram.read(dma_buf, &mut tmp).is_err() {
            return false;
        }
        let item = &mut self.items[index];
        item.data[offset..end].copy_from_slice(&tmp);
        self.offset = end;
        if item_name(&item.name) == b"etc/ramfb" {
            let bytes = item.data.clone();
            if let Some(ramfb) = &self.ramfb {
                ramfb.guest_wrote(&bytes);
            }
        }
        true
    }
}

fn fixed_item(selector: u16, data: Vec<u8>) -> FwCfgItem {
    FwCfgItem {
        selector,
        name: [0; 56],
        data,
        writable: false,
    }
}

fn file_item(selector: u16, name: &str, data: Vec<u8>, writable: bool) -> FwCfgItem {
    let mut item_name = [0u8; 56];
    let raw = name.as_bytes();
    let n = raw.len().min(55);
    item_name[..n].copy_from_slice(&raw[..n]);
    FwCfgItem {
        selector,
        name: item_name,
        data,
        writable,
    }
}

fn item_name(name: &[u8; 56]) -> &[u8] {
    let end = name
        .iter()
        .position(|&byte| byte == 0)
        .unwrap_or(name.len());
    &name[..end]
}

fn build_file_dir(items: &[FwCfgItem]) -> Vec<u8> {
    let files: Vec<&FwCfgItem> = items
        .iter()
        .filter(|i| i.selector >= FW_CFG_FILE_FIRST)
        .collect();
    let mut dir = Vec::new();
    dir.extend_from_slice(&(files.len() as u32).to_be_bytes());
    for f in files {
        dir.extend_from_slice(&(f.data.len() as u32).to_be_bytes());
        dir.extend_from_slice(&f.selector.to_be_bytes());
        dir.extend_from_slice(&0u16.to_be_bytes());
        dir.extend_from_slice(&f.name);
    }
    dir
}

/// Ports `0x510` through `0x51B`: selector, data, and the two DMA dwords.
pub struct FwCfgPio {
    inner: Mutex<FwCfg>,
}

impl FwCfgPio {
    pub fn new(cfg: FwCfg) -> Self {
        Self {
            inner: Mutex::new(cfg),
        }
    }
}

impl PioDevice for FwCfgPio {
    fn pio_in(&self, offset: u16, size: usize) -> u64 {
        self.inner.lock().unwrap().pio_in(offset, size)
    }

    fn pio_out(&self, offset: u16, data: u64, size: usize) {
        self.inner.lock().unwrap().pio_out(offset, data, size);
    }
}

impl MmioDevice for FwCfg {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        match offset {
            REG_DATA => {
                let mut val = 0u64;
                for i in 0..size {
                    val |= (self.read_data_byte() as u64) << (8 * i);
                }
                val
            }
            REG_SELECTOR => self.selector as u64,
            REG_DMA => self.dma_addr.swap_bytes(),
            _ => 0,
        }
    }

    fn write(&mut self, offset: u64, data: u64, size: usize) {
        if std::env::var_os("VMM_TRACE_BOOT").is_some() {
            eprintln!("[VMM TRACE] fwcfg-wr off={offset:#x} size={size} data={data:#x}");
        }
        match offset {
            REG_SELECTOR => {
                let sel = if size <= 2 {
                    (data as u16).swap_bytes()
                } else {
                    data as u16
                };
                self.select(sel);
            }
            REG_DMA => {
                // 64-bit BE address; writing the register kicks DMA.
                self.dma_addr = if size == 8 {
                    data.swap_bytes()
                } else if size == 4 && offset == REG_DMA {
                    (self.dma_addr & 0xffff_ffff) | (data.swap_bytes() << 32)
                } else {
                    data.swap_bytes()
                };
                if size == 8 {
                    self.do_dma();
                }
            }
            0x14 => {
                self.dma_addr =
                    (self.dma_addr & 0xffff_ffff_0000_0000) | (data as u32).swap_bytes() as u64;
                self.do_dma();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_dir_contains_e820_and_not_fdt() {
        let ram = Arc::new(GuestMemory::allocate(0, 0x1000).unwrap());
        let cfg = FwCfg::from_files(ram, &[("etc/e820", &[1, 2, 3, 4])]);
        let dir = cfg
            .items
            .iter()
            .find(|i| i.selector == FW_CFG_FILE_DIR)
            .unwrap();
        assert!(!dir.data.windows(8).any(|w| w == b"etc/fdt\0"));
        assert!(dir.data.windows(9).any(|w| w == b"etc/e820\0"));
        assert_eq!(&dir.data[0..4], 1u32.to_be_bytes());
        assert_eq!(&dir.data[4..8], 4u32.to_be_bytes());
        assert_eq!(&dir.data[8..10], FW_CFG_FILE_FIRST.to_be_bytes());
    }

    #[test]
    fn pio_selector_is_native_and_dma_halves_swap() {
        let ram = Arc::new(GuestMemory::allocate(0, 0x1000).unwrap());
        let mut cfg = FwCfg::from_files(Arc::clone(&ram), &[]);
        cfg.pio_out(0, 0, 2);
        assert_eq!(cfg.pio_in(1, 1), u64::from(b'Q'));
        assert_eq!(cfg.pio_in(0, 1), u64::from(b'E'));
        let mut rest = [0u8; 2];
        rest[0] = cfg.pio_in(1, 1) as u8;
        rest[1] = cfg.pio_in(1, 1) as u8;
        assert_eq!(&rest, b"MU");

        cfg.pio_out(0, 1, 2);
        assert_eq!(cfg.pio_in(1, 1), 3);

        let mut desc = [0u8; 16];
        desc[0..4].copy_from_slice(&0x0000_0002u32.to_be_bytes());
        desc[4..8].copy_from_slice(&4u32.to_be_bytes());
        desc[8..16].copy_from_slice(&0x200u64.to_be_bytes());
        ram.write(0x100, &desc).unwrap();
        cfg.pio_out(0, 0, 2);
        let addr = 0x100u64;
        cfg.pio_out(4, u64::from(((addr >> 32) as u32).swap_bytes()), 4);
        cfg.pio_out(8, u64::from((addr as u32).swap_bytes()), 4);
        let mut got = [0u8; 4];
        ram.read(0x200, &mut got).unwrap();
        assert_eq!(&got, b"QEMU");
        let mut ctl = [0u8; 4];
        ram.read(0x100, &mut ctl).unwrap();
        assert_eq!(ctl, [0, 0, 0, 0]);
    }

    #[test]
    fn ramfb_directory_entry_tracks_a_guest_write() {
        let ram_size = 8 * 1024 * 1024;
        let ram = Arc::new(GuestMemory::allocate(0, ram_size).unwrap());
        let ramfb = Arc::new(Ramfb::reserve(ram_size as u64, 64, 64).unwrap());
        let initial = ramfb.config();
        let mut cfg = FwCfg::from_files_with_ramfb(
            Arc::clone(&ram),
            &[("etc/e820", &[1, 2, 3, 4])],
            Arc::clone(&ramfb),
        );
        let dir = cfg
            .items
            .iter()
            .find(|item| item.selector == FW_CFG_FILE_DIR)
            .unwrap()
            .data
            .clone();
        assert!(dir.windows(10).any(|w| w == b"etc/ramfb\0"));
        assert!(!dir.windows(8).any(|w| w == b"etc/fdt\0"));
        let (size, selector) = dir_file(&dir, "etc/ramfb").unwrap();
        assert_eq!(size, 28);
        let published = cfg
            .items
            .iter()
            .find(|item| item.selector == selector)
            .unwrap();
        assert!(published.writable);
        assert_eq!(
            published.data.as_slice(),
            display::descriptor_bytes(&initial)
        );

        // Little-endian guest write, the layout named in the ramfb descriptor.
        let mut le = display::descriptor_bytes(&initial);
        le[0..8].copy_from_slice(&0x20_0000u64.to_le_bytes());
        le[16..20].copy_from_slice(&32u32.to_le_bytes());
        le[20..24].copy_from_slice(&16u32.to_le_bytes());
        le[24..28].copy_from_slice(&128u32.to_le_bytes());
        guest_dma_write(&mut cfg, &ram, selector, 0x1000, 0x2000, &le);
        assert_eq!(ramfb.config().gpa, 0x20_0000);
        assert_eq!(ramfb.config().width, 32);
        assert_eq!(ramfb.config().height, 16);
        assert_eq!(ramfb.config().stride, 128);
        assert_eq!(
            cfg.items
                .iter()
                .find(|item| item.selector == selector)
                .unwrap()
                .data
                .as_slice(),
            le.as_slice()
        );

        // OVMF QemuRamfbDxe SwapBytes the fields, so the on-wire image is big-endian.
        let mut be = [0u8; 28];
        be[0..8].copy_from_slice(&0x30_0000u64.to_be_bytes());
        be[8..12].copy_from_slice(&display::XRGB8888.to_be_bytes());
        be[16..20].copy_from_slice(&8u32.to_be_bytes());
        be[20..24].copy_from_slice(&8u32.to_be_bytes());
        be[24..28].copy_from_slice(&32u32.to_be_bytes());
        guest_dma_write(&mut cfg, &ram, selector, 0x1000, 0x2000, &be);
        assert_eq!(ramfb.config().gpa, 0x30_0000);
        assert_eq!(ramfb.config().width, 8);
        assert_eq!(ramfb.config().height, 8);
        assert_eq!(ramfb.config().stride, 32);
        assert_eq!(
            cfg.items
                .iter()
                .find(|item| item.selector == selector)
                .unwrap()
                .data
                .as_slice(),
            be.as_slice()
        );

        be[0..8].copy_from_slice(&(ram_size as u64).to_be_bytes());
        guest_dma_write(&mut cfg, &ram, selector, 0x1000, 0x2000, &be);
        assert_eq!(
            ramfb.config().gpa,
            0x30_0000,
            "out-of-range write is ignored"
        );

        let (e820_size, e820_sel) = dir_file(&dir, "etc/e820").unwrap();
        assert_eq!(e820_size, 4);
        guest_dma_write(&mut cfg, &ram, e820_sel, 0x1000, 0x2000, &[9, 9, 9, 9]);
        let e820 = cfg
            .items
            .iter()
            .find(|item| item.selector == e820_sel)
            .unwrap();
        assert!(!e820.writable);
        assert_eq!(e820.data, [1, 2, 3, 4]);
        let mut ctl = [0u8; 4];
        ram.read(0x1000, &mut ctl).unwrap();
        assert_eq!(ctl, 1u32.to_be_bytes());
        let dir_now = cfg
            .items
            .iter()
            .find(|item| item.selector == FW_CFG_FILE_DIR)
            .unwrap();
        assert_eq!(dir_now.data, dir);
    }

    fn dir_file(dir: &[u8], name: &str) -> Option<(u32, u16)> {
        let count = u32::from_be_bytes(dir[0..4].try_into().ok()?) as usize;
        let mut off = 4;
        for _ in 0..count {
            let size = u32::from_be_bytes(dir[off..off + 4].try_into().ok()?);
            let selector = u16::from_be_bytes(dir[off + 4..off + 6].try_into().ok()?);
            let raw = &dir[off + 8..off + 64];
            let end = raw.iter().position(|&byte| byte == 0).unwrap_or(raw.len());
            if &raw[..end] == name.as_bytes() {
                return Some((size, selector));
            }
            off += 64;
        }
        None
    }

    fn guest_dma_write(
        cfg: &mut FwCfg,
        ram: &GuestMemory,
        selector: u16,
        desc_gpa: u64,
        src_gpa: u64,
        bytes: &[u8],
    ) {
        ram.write(src_gpa, bytes).unwrap();
        let mut desc = [0u8; 16];
        desc[0..4].copy_from_slice(&FW_CFG_DMA_CTL_WRITE.to_be_bytes());
        desc[4..8].copy_from_slice(&(bytes.len() as u32).to_be_bytes());
        desc[8..16].copy_from_slice(&src_gpa.to_be_bytes());
        ram.write(desc_gpa, &desc).unwrap();
        cfg.pio_out(0, u64::from(selector), 2);
        cfg.pio_out(4, u64::from(((desc_gpa >> 32) as u32).swap_bytes()), 4);
        cfg.pio_out(8, u64::from((desc_gpa as u32).swap_bytes()), 4);
    }
}
