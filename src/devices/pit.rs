//! 8254 stub. Ports `0x40`–`0x43` accept I/O. IRQ 0 is never asserted.

use std::sync::Mutex;

use super::PioDevice;

pub struct Pit {
    regs: Mutex<[u8; 4]>,
}

impl Pit {
    pub fn new() -> Self {
        Self {
            regs: Mutex::new([0; 4]),
        }
    }
}

impl Default for Pit {
    fn default() -> Self {
        Self::new()
    }
}

impl PioDevice for Pit {
    fn pio_in(&self, offset: u16, size: usize) -> u64 {
        let regs = self.regs.lock().unwrap();
        let mut value = 0u64;
        for i in 0..size {
            let idx = (offset as usize + i) % 4;
            value |= (regs[idx] as u64) << (8 * i);
        }
        value
    }

    fn pio_out(&self, offset: u16, data: u64, size: usize) {
        let mut regs = self.regs.lock().unwrap();
        for i in 0..size {
            let idx = (offset as usize + i) % 4;
            regs[idx] = ((data >> (8 * i)) & 0xff) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_count_write() {
        let pit = Pit::new();
        pit.pio_out(0, 0x12, 1);
        pit.pio_out(3, 0x34, 1);
        assert_eq!(pit.pio_in(0, 1), 0x12);
        assert_eq!(pit.pio_in(3, 1), 0x34);
    }
}
