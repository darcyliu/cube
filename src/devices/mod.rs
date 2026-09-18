//! MMIO subsystem and device bus.

pub mod uart;

pub trait MmioDevice: Send {
    fn read(&mut self, offset: u64, size: usize) -> u64;
    fn write(&mut self, offset: u64, data: u64, size: usize);
}

struct MmioEntry {
    base: u64,
    size: u64,
    device: Box<dyn MmioDevice>,
}

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
        self.devices.push(MmioEntry { base, size, device });
    }

    pub fn read(&mut self, gpa: u64, size: usize) -> Result<u64, String> {
        for entry in &mut self.devices {
            if gpa >= entry.base && gpa < entry.base + entry.size {
                let offset = gpa - entry.base;
                return Ok(entry.device.read(offset, size));
            }
        }
        Err(format!("Unhandled MMIO read at GPA 0x{:08x} (size {})", gpa, size))
    }

    pub fn write(&mut self, gpa: u64, data: u64, size: usize) -> Result<(), String> {
        for entry in &mut self.devices {
            if gpa >= entry.base && gpa < entry.base + entry.size {
                let offset = gpa - entry.base;
                entry.device.write(offset, data, size);
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
