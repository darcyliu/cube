//! Minimal ARM PL011-compatible UART device emulation for guest console I/O.

use std::io::{self, Write};
use super::MmioDevice;

/// PL011 Register Offsets
const UART_DR: u64 = 0x00;   // Data Register
const UART_FR: u64 = 0x18;   // Flag Register
#[allow(dead_code)]
const UART_IBRD: u64 = 0x24; // Integer Baud Rate
#[allow(dead_code)]
const UART_FBRD: u64 = 0x28; // Fractional Baud Rate
#[allow(dead_code)]
const UART_LCR_H: u64 = 0x2c;// Line Control Register
const UART_CR: u64 = 0x30;   // Control Register
const UART_IMSC: u64 = 0x38; // Interrupt Mask Set/Clear
const UART_MIS: u64 = 0x40;  // Masked Interrupt Status
const UART_ICR: u64 = 0x44;  // Interrupt Clear Register

// PL011 Flag Register bits:
// Bit 7: TXFE (Transmit FIFO empty) = 1
// Bit 5: TXFF (Transmit FIFO full) = 0
// Bit 4: RXFE (Receive FIFO empty) = 1
const UART_FR_VALUE: u64 = (1 << 7) | (1 << 4);

pub struct Pl011Uart {
    cr: u32,
    imsc: u32,
}

impl Pl011Uart {
    pub fn new() -> Self {
        Self {
            cr: 0x301, // RXE | TXE | UARTEN
            imsc: 0,
        }
    }
}

impl Default for Pl011Uart {
    fn default() -> Self {
        Self::new()
    }
}

impl MmioDevice for Pl011Uart {
    fn read(&mut self, offset: u64, _size: usize) -> u64 {
        match offset {
            UART_DR => 0,
            UART_FR => UART_FR_VALUE,
            UART_CR => self.cr as u64,
            UART_IMSC => self.imsc as u64,
            UART_MIS => 0,
            _ => 0,
        }
    }

    fn write(&mut self, offset: u64, data: u64, _size: usize) {
        match offset {
            UART_DR => {
                let ch = (data & 0xff) as u8;
                let mut stdout = io::stdout().lock();
                let _ = stdout.write_all(&[ch]);
                let _ = stdout.flush();
            }
            UART_CR => {
                self.cr = data as u32;
            }
            UART_IMSC => {
                self.imsc = data as u32;
            }
            UART_ICR => {
                // Clear interrupts (no-op in basic polled mode)
            }
            _ => {}
        }
    }
}
