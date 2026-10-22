//! 8259 stub. Ports accept reads and writes. Nothing is injected.

use std::sync::atomic::{AtomicU8, Ordering};

use super::PioDevice;

pub struct Pic {
    cmd: AtomicU8,
    data: AtomicU8,
}

impl Pic {
    pub fn new() -> Self {
        Self {
            cmd: AtomicU8::new(0),
            // Masked IMR. Reads return what software stored.
            data: AtomicU8::new(0xff),
        }
    }
}

impl Default for Pic {
    fn default() -> Self {
        Self::new()
    }
}

impl PioDevice for Pic {
    fn pio_in(&self, offset: u16, size: usize) -> u64 {
        let mut value = 0u64;
        for i in 0..size {
            let byte = match offset + i as u16 {
                0 => self.cmd.load(Ordering::Relaxed),
                _ => self.data.load(Ordering::Relaxed),
            };
            value |= (byte as u64) << (8 * i);
        }
        value
    }

    fn pio_out(&self, offset: u16, data: u64, size: usize) {
        for i in 0..size {
            let byte = ((data >> (8 * i)) & 0xff) as u8;
            match offset + i as u16 {
                0 => self.cmd.store(byte, Ordering::Relaxed),
                _ => self.data.store(byte, Ordering::Relaxed),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stores_imr_and_does_not_need_an_irq_line() {
        let pic = Pic::new();
        pic.pio_out(1, 0x3c, 1);
        assert_eq!(pic.pio_in(1, 1), 0x3c);
        pic.pio_out(0, 0x20, 1);
        assert_eq!(pic.pio_in(0, 1), 0x20);
    }
}
