//! fw_cfg item list, big-endian file directory, and DMA descriptor.

use std::sync::{Arc, Mutex};

use crate::memory::GuestMemory;

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

const REG_DATA: u64 = 0x00;
const REG_SELECTOR: u64 = 0x08;
const REG_DMA: u64 = 0x10;

struct FwCfgItem {
    selector: u16,
    name: [u8; 56],
    data: Vec<u8>,
}

pub struct FwCfg {
    items: Vec<FwCfgItem>,
    selector: u16,
    offset: usize,
    dma_addr: u64,
    ram: Arc<GuestMemory>,
}

impl FwCfg {
    pub fn from_files(ram: Arc<GuestMemory>, files: &[(&str, &[u8])]) -> Self {
        let mut items = Vec::new();
        items.push(FwCfgItem {
            selector: FW_CFG_SIGNATURE,
            name: [0; 56],
            data: b"QEMU".to_vec(),
        });
        items.push(FwCfgItem {
            selector: FW_CFG_ID,
            name: [0; 56],
            data: (FW_CFG_VERSION | FW_CFG_VERSION_DMA).to_le_bytes().to_vec(),
        });

        for (index, (name, data)) in files.iter().enumerate() {
            let mut item_name = [0u8; 56];
            let raw = name.as_bytes();
            let n = raw.len().min(55);
            item_name[..n].copy_from_slice(&raw[..n]);
            items.push(FwCfgItem {
                selector: FW_CFG_FILE_FIRST + index as u16,
                name: item_name,
                data: data.to_vec(),
            });
        }

        let dir = build_file_dir(&items);
        items.insert(
            2,
            FwCfgItem {
                selector: FW_CFG_FILE_DIR,
                name: [0; 56],
                data: dir,
            },
        );

        Self {
            items,
            selector: 0,
            offset: 0,
            dma_addr: 0,
            ram,
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
        } else if control & FW_CFG_DMA_CTL_SKIP != 0 {
            self.offset = self.offset.saturating_add(length);
        }

        let result = err.to_be_bytes();
        let _ = self.ram.write(addr, &result);
    }
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
}
