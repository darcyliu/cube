//! MMIO subsystem and device bus.

pub mod fw_cfg;
pub mod rtc;
pub mod uart;
pub mod pflash;
pub mod virtio;
pub mod display;
pub mod usb;

use std::sync::{Arc, Mutex};

pub trait MmioDevice: Send {
    fn read(&mut self, offset: u64, size: usize) -> u64;
    fn write(&mut self, offset: u64, data: u64, size: usize);
}

impl MmioDevice for Box<dyn MmioDevice> {
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
    pub fn new() -> Self {
        Self {
            devices: Vec::new(),
        }
    }

    pub fn register(&mut self, base: u64, size: u64, device: Box<dyn MmioDevice>) {
        self.devices.push(MmioEntry {
            base,
            size,
            device: Arc::new(Mutex::new(device)),
        });
    }

    #[allow(dead_code)]
    pub fn register_shared(&mut self, base: u64, size: u64, device: Arc<Mutex<dyn MmioDevice>>) {
        self.devices.push(MmioEntry { base, size, device });
    }

    pub fn read(&self, gpa: u64, size: usize) -> Result<u64, String> {
        for entry in &self.devices {
            if gpa >= entry.base && gpa < entry.base + entry.size {
                let offset = gpa - entry.base;
                let mut dev = entry.device.lock().unwrap();
                return Ok(dev.read(offset, size));
            }
        }
        Err(format!("Unhandled MMIO read at GPA 0x{:08x} (size {})", gpa, size))
    }

    pub fn write(&self, gpa: u64, data: u64, size: usize) -> Result<(), String> {
        for entry in &self.devices {
            if gpa >= entry.base && gpa < entry.base + entry.size {
                let offset = gpa - entry.base;
                let mut dev = entry.device.lock().unwrap();
                dev.write(offset, data, size);
                return Ok(());
            }
        }
        Err(format!("Unhandled MMIO write at GPA 0x{:08x} (val 0x{:x}, size {})", gpa, data, size))
    }
}

impl Default for MmioBus {
    fn default() -> Self {
        Self::new()
    }
}
