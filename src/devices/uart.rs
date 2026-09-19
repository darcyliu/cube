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
// Bit 6: RTIM / RTIS (Receive Timeout interrupt)
const INT_RX: u32 = 1 << 4;
const INT_TX: u32 = 1 << 5;
const INT_RT: u32 = 1 << 6;

pub struct Pl011Uart {
    cr: u32,
    imsc: u32,
    tx_int: u32,
    rx_queue: Arc<Mutex<VecDeque<u8>>>,
    irq_num: u32,
    log_file: Option<std::fs::File>,
}

impl Pl011Uart {
    fn push_bytes(q: &Arc<Mutex<VecDeque<u8>>>, irq_num: u32, bytes: &[u8]) {
        let mut guard = q.lock().unwrap();
        for &b in bytes {
            if b == 0x1d {
                eprintln!("\r\n[VMM] Escape key Ctrl-] pressed. Exiting VM.\r\n");
                std::process::exit(0);
            }
            let byte = if b == b'\r' { b'\n' } else { b };
            guard.push_back(byte);
        }
        drop(guard);
        unsafe {
            crate::hypervisor::hv_gic_set_spi(irq_num, true);
        }
        crate::vcpu::kick_vcpu();
    }

    pub fn new(irq_num: u32, interactive_stdin: bool) -> Self {
        Self::with_log(irq_num, interactive_stdin, None)
    }

    pub fn with_log(irq_num: u32, interactive_stdin: bool, log_path: Option<&str>) -> Self {
        let rx_queue = Arc::new(Mutex::new(VecDeque::new()));
        let log_file = log_path.and_then(|p| {
            std::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(p)
                .ok()
        });

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
                                Self::push_bytes(&q_clone, irq_num, &buf[..n]);
                            }
                            Err(_) => break,
                        }
                    }
                })
                .expect("Failed to spawn stdin thread");

            // Optional host-side command injector used by automated tests and installers.
            // Bytes written to /tmp/vmm-uart-in are pushed into the PL011 RX FIFO.
            let q_inject = Arc::clone(&rx_queue);
            thread::Builder::new()
                .name("vmm-uart-inject".into())
                .spawn(move || {
                    let path = "/tmp/vmm-uart-in";
                    let _ = std::fs::remove_file(path);
                    loop {
                        if std::path::Path::new(path).exists() {
                            if let Ok(data) = std::fs::read(path) {
                                if !data.is_empty() {
                                    Self::push_bytes(&q_inject, irq_num, &data);
                                }
                            }
                            let _ = std::fs::remove_file(path);
                        }
                        thread::sleep(std::time::Duration::from_millis(50));
                    }
                })
                .expect("Failed to spawn uart inject thread");
        }

        Self {
            cr: 0x301, // RXE | TXE | UARTEN
            imsc: 0,
            tx_int: INT_TX, // TX FIFO starts empty
            rx_queue,
            irq_num,
            log_file,
        }
    }

    fn update_irq(&self) {
        let q = self.rx_queue.lock().unwrap();
        let rx_pending = if q.is_empty() { 0 } else { INT_RX | INT_RT };
        let pending = (rx_pending | self.tx_int) & self.imsc;
        unsafe {
            crate::hypervisor::hv_gic_set_spi(self.irq_num, pending != 0);
        }
        // Kick only for RX so console output does not flood hv_vcpus_exit.
        if (rx_pending & self.imsc) != 0 {
            crate::vcpu::kick_vcpu();
        }
    }
}

impl MmioDevice for Pl011Uart {
    fn read(&mut self, offset: u64, _size: usize) -> u64 {
        match offset {
            UART_DR => {
                let mut q = self.rx_queue.lock().unwrap();
                let byte = q.pop_front().unwrap_or(0);
                drop(q);
                self.update_irq();
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
                let rx = if q.is_empty() { 0 } else { INT_RX | INT_RT };
                (rx | self.tx_int) as u64
            }
            UART_MIS => {
                let q = self.rx_queue.lock().unwrap();
                let rx = if q.is_empty() { 0 } else { INT_RX | INT_RT };
                ((rx | self.tx_int) & self.imsc) as u64
            }
            _ => 0,
        }
    }

    fn write(&mut self, offset: u64, data: u64, _size: usize) {
        match offset {
            UART_DR => {
                let ch = (data & 0xff) as u8;
                let mut stdout = io::stdout().lock();
                if ch == b'\n' {
                    let _ = stdout.write_all(b"\r\n");
                } else {
                    let _ = stdout.write_all(&[ch]);
                }
                let _ = stdout.flush();
                if let Some(ref mut f) = self.log_file {
                    let _ = f.write_all(&[ch]);
                    let _ = f.flush();
                }
                // Data was sent immediately, transmit FIFO is ready for more
                self.tx_int = INT_TX;
                self.update_irq();
            }
            UART_CR => {
                self.cr = data as u32;
            }
            UART_IMSC => {
                self.imsc = data as u32;
                self.update_irq();
            }
            UART_ICR => {
                let val = data as u32;
                self.tx_int &= !val;
                self.update_irq();
            }
            _ => {}
        }
    }
}
