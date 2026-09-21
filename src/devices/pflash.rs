//! Minimal Intel-command-set CFI parallel flash (QEMU pflash_cfi01 subset).
//!
//! EDK2 ArmVirt NorFlashDxe talks CFI to the EFI variable bank at 0x0400_0000.
//! Mapping that GPA as plain RAM makes status/query reads look wrong and hangs
//! Variable services — this MMIO model returns a ready status and a valid CFI
//! table, and programs/erases into a backing buffer.

use super::MmioDevice;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::FileExt;

const CMD_READ_ARRAY: u8 = 0xff;
const CMD_READ_ARRAY_ALT: u8 = 0x00;
const CMD_PROGRAM: u8 = 0x10;
const CMD_PROGRAM_ALT: u8 = 0x40;
const CMD_ERASE: u8 = 0x20;
const CMD_CLEAR_STATUS: u8 = 0x50;
const CMD_STATUS: u8 = 0x70;
const CMD_DEVICE_ID: u8 = 0x90;
const CMD_CFI_QUERY: u8 = 0x98;
const CMD_WRITE_BUFFER: u8 = 0xe8;
const CMD_CONFIRM: u8 = 0xd0;

const STATUS_READY: u8 = 0x80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cycle {
    Read,
    ProgramData,
    EraseConfirm { offset: u64 },
    BufCount,
    BufData { remaining: u32, offset: u64 },
    BufConfirm,
}

pub struct CfiPflash {
    storage: Vec<u8>,
    cmd: u8,
    status: u8,
    cycle: Cycle,
    cfi: [u8; 0x60],
    block_size: u32,
    backing: Option<File>,
    persistence_error: Option<String>,
}

impl CfiPflash {
    pub fn new(storage: Vec<u8>) -> Self {
        let block_size: u32 = 256 * 1024;
        let len = storage.len() as u32;
        let blocks_per_device = (len / block_size).max(1);
        let mut cfi = [0u8; 0x60];
        cfi[0x10] = b'Q';
        cfi[0x11] = b'R';
        cfi[0x12] = b'Y';
        // Primary algorithm: Intel (1)
        cfi[0x13] = 0x01;
        cfi[0x14] = 0x00;
        cfi[0x1b] = 0x27;
        cfi[0x1c] = 0x36;
        cfi[0x1f] = 0x07;
        cfi[0x20] = 0x07;
        cfi[0x21] = 0x0a;
        cfi[0x23] = 0x04;
        cfi[0x24] = 0x04;
        cfi[0x25] = 0x04;
        let size_log2 = (len.next_power_of_two().trailing_zeros() as u8).max(1);
        cfi[0x27] = size_log2;
        cfi[0x28] = 0x02; // x8/x16
        cfi[0x2a] = 0x08; // max buffered write = 256 bytes
        cfi[0x2c] = 0x01; // one erase region
        let nblocks = blocks_per_device.saturating_sub(1);
        cfi[0x2d] = (nblocks & 0xff) as u8;
        cfi[0x2e] = ((nblocks >> 8) & 0xff) as u8;
        let bsz = block_size / 256;
        cfi[0x2f] = (bsz & 0xff) as u8;
        cfi[0x30] = ((bsz >> 8) & 0xff) as u8;

        Self {
            storage,
            cmd: CMD_READ_ARRAY,
            status: STATUS_READY,
            cycle: Cycle::Read,
            cfi,
            block_size,
            backing: None,
            persistence_error: None,
        }
    }

    /// Keep the template immutable; each VM owns a separately locked store.
    pub fn open(path: &std::path::Path, template: Vec<u8>) -> Result<Self, String> {
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(|e| format!("EFI variables {}: {e}", path.display()))?;
        file.try_lock()
            .map_err(|e| format!("EFI variable store is in use: {e}"))?;
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        let storage = if len == 0 {
            file.write_all(&template)
                .and_then(|_| file.sync_all())
                .map_err(|e| e.to_string())?;
            template
        } else {
            if len != template.len() as u64 {
                return Err("EFI variable store has incorrect size".into());
            }
            let mut bytes = vec![0; template.len()];
            file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
            bytes
        };
        let mut flash = Self::new(storage);
        flash.backing = Some(file);
        Ok(flash)
    }

    fn persist(&mut self, start: usize, end: usize) {
        if let Some(file) = &self.backing {
            if let Err(e) = file
                .write_all_at(&self.storage[start..end], start as u64)
                .and_then(|_| file.sync_data())
            {
                self.status |= 0x30;
                self.persistence_error = Some(format!("EFI variable write failed: {e}"));
            }
        }
    }

    fn set_read_array(&mut self) {
        self.cmd = CMD_READ_ARRAY;
        self.cycle = Cycle::Read;
    }

    fn write_storage(&mut self, offset: u64, data: u64, size: usize) {
        let off = offset as usize;
        if off >= self.storage.len() {
            self.status |= 0x10;
            return;
        }
        let bytes = data.to_le_bytes();
        for i in 0..size {
            if off + i < self.storage.len() {
                // NOR: programming only clears bits.
                self.storage[off + i] &= bytes[i];
            }
        }
        self.persist(off, (off + size).min(self.storage.len()));
        self.status |= STATUS_READY;
    }

    fn erase_block(&mut self, offset: u64) {
        let off = offset as usize;
        let bs = self.block_size as usize;
        let base = (off / bs) * bs;
        let end = (base + bs).min(self.storage.len());
        if base < self.storage.len() {
            for b in &mut self.storage[base..end] {
                *b = 0xff;
            }
            self.persist(base, end);
        }
        self.status |= STATUS_READY;
    }

    fn cfi_byte(&self, offset: u64) -> u8 {
        let idx = (offset as usize) & 0xff;
        if idx < self.cfi.len() {
            self.cfi[idx]
        } else {
            0
        }
    }

    fn replicate_byte(val: u8, size: usize) -> u64 {
        let mut r = 0u64;
        for i in 0..size {
            r |= (val as u64) << (8 * i);
        }
        r
    }

    fn read_data(&self, offset: u64, size: usize) -> u64 {
        let off = offset as usize;
        let mut buf = [0u8; 8];
        for i in 0..size {
            if off + i < self.storage.len() {
                buf[i] = self.storage[off + i];
            }
        }
        u64::from_le_bytes(buf)
    }
}

impl MmioDevice for CfiPflash {
    fn shutdown(&mut self) -> Result<(), String> {
        if let Some(e) = &self.persistence_error {
            return Err(e.clone());
        }
        if let Some(file) = &self.backing {
            file.sync_all().map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        let size = size.clamp(1, 8);
        match self.cmd {
            CMD_READ_ARRAY | CMD_READ_ARRAY_ALT => self.read_data(offset, size),
            CMD_STATUS | CMD_PROGRAM | CMD_PROGRAM_ALT | CMD_ERASE | CMD_CLEAR_STATUS
            | CMD_WRITE_BUFFER => Self::replicate_byte(self.status, size),
            CMD_CFI_QUERY => Self::replicate_byte(self.cfi_byte(offset), size),
            CMD_DEVICE_ID => {
                let boff = (offset as usize) & 0xff;
                let b = match boff {
                    0 => 0x89, // Intel
                    1 => 0x18,
                    _ => 0x00,
                };
                Self::replicate_byte(b, size)
            }
            _ => self.read_data(offset, size),
        }
    }

    fn write(&mut self, offset: u64, data: u64, size: usize) {
        let size = size.clamp(1, 8);
        let cmd = (data & 0xff) as u8;

        match self.cycle {
            Cycle::ProgramData => {
                self.write_storage(offset, data, size);
                self.cmd = CMD_STATUS;
                self.cycle = Cycle::Read;
                return;
            }
            Cycle::EraseConfirm { offset: erase_off } => {
                if cmd == CMD_CONFIRM {
                    self.erase_block(erase_off);
                    self.cmd = CMD_STATUS;
                    self.cycle = Cycle::Read;
                } else if cmd == CMD_READ_ARRAY || cmd == CMD_READ_ARRAY_ALT {
                    self.set_read_array();
                } else {
                    self.status |= 0x20;
                    self.cmd = CMD_STATUS;
                    self.cycle = Cycle::Read;
                }
                return;
            }
            Cycle::BufCount => {
                let count = ((data & 0xffff) as u32).saturating_add(1);
                self.cycle = Cycle::BufData {
                    remaining: count,
                    offset,
                };
                self.cmd = CMD_WRITE_BUFFER;
                return;
            }
            Cycle::BufData {
                remaining,
                offset: mut buf_off,
            } => {
                self.write_storage(buf_off, data, size);
                let done = remaining.saturating_sub(1);
                buf_off = buf_off.saturating_add(size as u64);
                if done == 0 {
                    self.cycle = Cycle::BufConfirm;
                } else {
                    self.cycle = Cycle::BufData {
                        remaining: done,
                        offset: buf_off,
                    };
                }
                return;
            }
            Cycle::BufConfirm => {
                if cmd == CMD_CONFIRM {
                    self.status |= STATUS_READY;
                } else {
                    self.status |= 0x10;
                }
                self.cmd = CMD_STATUS;
                self.cycle = Cycle::Read;
                return;
            }
            Cycle::Read => {}
        }

        match cmd {
            CMD_READ_ARRAY | CMD_READ_ARRAY_ALT => self.set_read_array(),
            CMD_PROGRAM | CMD_PROGRAM_ALT => {
                self.cmd = cmd;
                self.cycle = Cycle::ProgramData;
            }
            CMD_ERASE => {
                self.cmd = cmd;
                self.cycle = Cycle::EraseConfirm { offset };
            }
            CMD_CLEAR_STATUS => {
                self.status = STATUS_READY;
                self.set_read_array();
            }
            CMD_STATUS => {
                self.cmd = CMD_STATUS;
                self.cycle = Cycle::Read;
            }
            CMD_DEVICE_ID => {
                self.cmd = CMD_DEVICE_ID;
                self.cycle = Cycle::Read;
            }
            CMD_CFI_QUERY => {
                self.cmd = CMD_CFI_QUERY;
                self.cycle = Cycle::Read;
            }
            CMD_WRITE_BUFFER => {
                self.cmd = CMD_WRITE_BUFFER;
                self.cycle = Cycle::BufCount;
            }
            0xf0 => self.set_read_array(),
            _ => self.set_read_array(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn variables_survive_reopen_and_store_is_exclusive() {
        let dir = crate::test_support::TestDir::new();
        let path = dir.path("vars");
        let mut flash = CfiPflash::open(&path, vec![0xff; 0x40000]).unwrap();
        assert!(CfiPflash::open(&path, vec![0xff; 0x40000]).is_err());
        flash.write(0, CMD_PROGRAM as u64, 4);
        flash.write(0x100, 0x12345678, 4);
        flash.shutdown().unwrap();
        drop(flash);
        let mut flash = CfiPflash::open(&path, vec![0xff; 0x40000]).unwrap();
        assert_eq!(flash.read(0x100, 4), 0x12345678);
        flash.write(0, CMD_ERASE as u64, 4);
        flash.write(0, CMD_CONFIRM as u64, 4);
        drop(flash);
        let mut flash = CfiPflash::open(&path, vec![0xff; 0x40000]).unwrap();
        assert_eq!(flash.read(0x100, 4), 0xffffffff);
    }
}
