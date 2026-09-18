//! Minimal ARM PL011-compatible UART device emulation with bidirectional console I/O.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;

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
const UART_RIS: u64 = 0x3c;  // Raw Interrupt Status
const UART_MIS: u64 = 0x40;  // Masked Interrupt Status
const UART_ICR: u64 = 0x44;  // Interrupt Clear Register

// PL011 Flag Register bits:
// Bit 7: TXFE (Transmit FIFO empty)
// Bit 5: TXFF (Transmit FIFO full)
// Bit 4: RXFE (Receive FIFO empty)
// Bit 6: RXFF (Receive FIFO full)
const UART_FR_TXFE: u64 = 1 << 7;
const UART_FR_RXFE: u64 = 1 << 4;

// Interrupt bits:
// Bit 4: RXIM / RXIS (Receive interrupt)
// Bit 5: TXIM / TXIS (Transmit interrupt)
const INT_RX: u32 = 1 << 4;

pub struct Pl011Uart {
    cr: u32,
    imsc: u32,
    rx_queue: Arc<Mutex<VecDeque<u8>>>,
    irq_num: u32,
}

impl Pl011Uart {
    pub fn new(irq_num: u32, interactive_stdin: bool) -> Self {
        let rx_queue = Arc::new(Mutex::new(VecDeque::new()));

        if interactive_stdin {
            let q_clone = Arc::clone(&rx_queue);
            thread::Builder::new()
                .name("vmm-stdin".into())
                .spawn(move || {
                    let mut stdin = io::stdin().lock();
                    let mut buf = [0u8; 128];
                    loop {
                        match stdin.read(&mut buf) {
                            Ok(0) => break, // EOF
                            Ok(n) => {
                                let mut q = q_clone.lock().unwrap();
                                for &b in &buf[..n] {
                                    // Ctrl-] (0x1d) escape to quit VM
                                    if b == 0x1d {
                                        eprintln!("\r\n[VMM] Escape key Ctrl-] pressed. Exiting VM.\r\n");
                                        std::process::exit(0);
                                    }
                                    q.push_back(b);
                                }
                                drop(q);

                                // Raise GIC SPI interrupt
                                unsafe {
                                    crate::hypervisor::hv_gic_set_spi(irq_num, true);
                                }
                            }
                            Err(_) => break,
                        }
                    }
                })
                .expect("Failed to spawn stdin thread");
        }

        Self {
            cr: 0x301, // RXE | TXE | UARTEN
            imsc: 0,
            rx_queue,
            irq_num,
        }
    }
}

impl MmioDevice for Pl011Uart {
    fn read(&mut self, offset: u64, _size: usize) -> u64 {
        match offset {
            UART_DR => {
                let mut q = self.rx_queue.lock().unwrap();
                let byte = q.pop_front().unwrap_or(0);
                if q.is_empty() {
                    unsafe {
                        crate::hypervisor::hv_gic_set_spi(self.irq_num, false);
                    }
                }
                byte as u64
            }
            UART_FR => {
                let q = self.rx_queue.lock().unwrap();
                let rx_empty = if q.is_empty() { UART_FR_RXFE } else { 0 };
                UART_FR_TXFE | rx_empty
            }
            UART_CR => self.cr as u64,
            UART_IMSC => self.imsc as u64,
            UART_RIS => {
                let q = self.rx_queue.lock().unwrap();
                if q.is_empty() {
                    0
                } else {
                    INT_RX as u64
                }
            }
            UART_MIS => {
                let q = self.rx_queue.lock().unwrap();
                let ris = if q.is_empty() { 0 } else { INT_RX };
                (ris & self.imsc) as u64
            }
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
                // Clear interrupts
                let val = data as u32;
                if (val & INT_RX) != 0 {
                    let q = self.rx_queue.lock().unwrap();
                    if q.is_empty() {
                        unsafe {
                            crate::hypervisor::hv_gic_set_spi(self.irq_num, false);
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
