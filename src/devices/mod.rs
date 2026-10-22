//! MMIO / port I/O buses and devices that do not call the hypervisor.

pub mod cmos;
pub mod debugcon;
pub mod fw_cfg;
pub mod i8042;
pub mod ioapic;
pub mod lapic;
pub mod pci;
pub mod pflash;
pub mod pic;
pub mod pit;
pub mod serial;
pub mod virtio;

use std::sync::{Arc, Mutex};

pub trait MmioDevice: Send {
    fn read(&mut self, offset: u64, size: usize) -> u64;
    fn write(&mut self, offset: u64, data: u64, size: usize);
    fn shutdown(&mut self) -> Result<(), String> {
        Ok(())
    }
}

impl MmioDevice for Box<dyn MmioDevice> {
    fn shutdown(&mut self) -> Result<(), String> {
        (**self).shutdown()
    }
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        (**self).read(offset, size)
    }
    fn write(&mut self, offset: u64, data: u64, size: usize) {
        (**self).write(offset, data, size);
    }
}

#[derive(Clone)]
struct MmioEntry {
    base: u64,
    size: u64,
    device: Arc<Mutex<dyn MmioDevice>>,
}

#[derive(Clone)]
pub struct MmioBus {
    devices: Vec<MmioEntry>,
}

impl MmioBus {
    fn check_range(&self, base: u64, size: u64) {
        let end = base.checked_add(size).expect("MMIO range overflow");
        assert!(
            size != 0
                && self
                    .devices
                    .iter()
                    .all(|e| end <= e.base || base >= e.base + e.size),
            "overlapping MMIO device range"
        );
    }
    pub fn shutdown(&self) -> Result<(), String> {
        let mut errors = Vec::new();
        for entry in &self.devices {
            match entry.device.lock() {
                Ok(mut device) => {
                    if let Err(e) = device.shutdown() {
                        errors.push(e);
                    }
                }
                Err(_) => errors.push(format!("Device at {:#x} panicked", entry.base)),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    pub fn clear(&mut self) {
        self.devices.clear();
    }
    pub fn new() -> Self {
        Self {
            devices: Vec::new(),
        }
    }

    pub fn register(&mut self, base: u64, size: u64, device: Box<dyn MmioDevice>) {
        self.check_range(base, size);
        self.devices.push(MmioEntry {
            base,
            size,
            device: Arc::new(Mutex::new(device)),
        });
    }

    #[allow(dead_code)]
    pub fn register_shared(&mut self, base: u64, size: u64, device: Arc<Mutex<dyn MmioDevice>>) {
        self.check_range(base, size);
        self.devices.push(MmioEntry { base, size, device });
    }

    pub fn read(&self, gpa: u64, size: usize) -> Result<u64, String> {
        for entry in &self.devices {
            if matches!(size, 1 | 2 | 4 | 8)
                && gpa
                    .checked_sub(entry.base)
                    .is_some_and(|offset| offset < entry.size && size as u64 <= entry.size - offset)
            {
                let offset = gpa - entry.base;
                let mut dev = entry.device.lock().unwrap();
                return Ok(dev.read(offset, size));
            }
        }
        Err(format!(
            "Unhandled MMIO read at GPA 0x{:08x} (size {})",
            gpa, size
        ))
    }

    pub fn write(&self, gpa: u64, data: u64, size: usize) -> Result<(), String> {
        for entry in &self.devices {
            if matches!(size, 1 | 2 | 4 | 8)
                && gpa
                    .checked_sub(entry.base)
                    .is_some_and(|offset| offset < entry.size && size as u64 <= entry.size - offset)
            {
                let offset = gpa - entry.base;
                let mut dev = entry.device.lock().unwrap();
                dev.write(offset, data, size);
                return Ok(());
            }
        }
        Err(format!(
            "Unhandled MMIO write at GPA 0x{:08x} (val 0x{:x}, size {})",
            gpa, data, size
        ))
    }
}

impl Default for MmioBus {
    fn default() -> Self {
        Self::new()
    }
}

/// Port I/O device. `offset` is from the registered base port.
pub trait PioDevice: Send + Sync {
    fn pio_in(&self, offset: u16, size: usize) -> u64;
    fn pio_out(&self, offset: u16, data: u64, size: usize);
}

struct PioEntry {
    base: u16,
    size: u16,
    device: Arc<dyn PioDevice>,
}

pub struct PioBus {
    devices: Vec<PioEntry>,
}

impl PioBus {
    pub fn new() -> Self {
        Self {
            devices: Vec::new(),
        }
    }

    pub fn register(&mut self, base: u16, size: u16, device: Arc<dyn PioDevice>) {
        let end = base as u32 + size as u32;
        assert!(
            size != 0
                && end <= 0x1_0000
                && self.devices.iter().all(|e| {
                    let other = e.base as u32 + e.size as u32;
                    end <= e.base as u32 || base as u32 >= other
                }),
            "overlapping PIO range"
        );
        self.devices.push(PioEntry { base, size, device });
    }

    pub fn read(&self, port: u16, size: usize) -> Result<u64, String> {
        self.dispatch(port, size)
            .map(|(dev, offset)| dev.pio_in(offset, size))
            .ok_or_else(|| format!("Unhandled PIO read at port {port:#04x} (size {size})"))
    }

    pub fn write(&self, port: u16, data: u64, size: usize) -> Result<(), String> {
        match self.dispatch(port, size) {
            Some((dev, offset)) => {
                dev.pio_out(offset, data, size);
                Ok(())
            }
            None => Err(format!(
                "Unhandled PIO write at port {port:#04x} (val {data:#x}, size {size})"
            )),
        }
    }

    fn dispatch(&self, port: u16, size: usize) -> Option<(&Arc<dyn PioDevice>, u16)> {
        if !matches!(size, 1 | 2 | 4) {
            return None;
        }
        let end = port as usize + size;
        for entry in &self.devices {
            let base = entry.base as usize;
            if port as usize >= base && end <= base + entry.size as usize {
                return Some((&entry.device, port - entry.base));
            }
        }
        None
    }
}

impl Default for PioBus {
    fn default() -> Self {
        Self::new()
    }
}
