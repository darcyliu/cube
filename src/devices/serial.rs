//! 16550 at COM1. IRQ 4 is counted and not delivered (no APIC in this PR).

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use super::PioDevice;

const RX_CAP: usize = 4096;
const COM1_IRQ: u8 = 4;

const MCR_DTR: u8 = 1 << 0;
const MCR_RTS: u8 = 1 << 1;
const MCR_OUT1: u8 = 1 << 2;
const MCR_OUT2: u8 = 1 << 3;
const MCR_LOOP: u8 = 1 << 4;

const MSR_DCTS: u8 = 1 << 0;
const MSR_DDSR: u8 = 1 << 1;
const MSR_TERI: u8 = 1 << 2;
const MSR_DDCD: u8 = 1 << 3;
const MSR_CTS: u8 = 1 << 4;
const MSR_DSR: u8 = 1 << 5;
const MSR_RI: u8 = 1 << 6;
const MSR_DCD: u8 = 1 << 7;
const MSR_LINES: u8 = MSR_CTS | MSR_DSR | MSR_DCD;

const LSR_DR: u8 = 1 << 0;
const LSR_OE: u8 = 1 << 1;
const LSR_THRE: u8 = 1 << 5;
const LSR_TEMT: u8 = 1 << 6;

const IER_RX: u8 = 1 << 0;
const IER_THRE: u8 = 1 << 1;

pub struct ConsoleHooks {
    pub on_shutdown: Arc<dyn Fn() + Send + Sync>,
    pub on_kick: Arc<dyn Fn() + Send + Sync>,
}

impl Default for ConsoleHooks {
    fn default() -> Self {
        Self {
            on_shutdown: Arc::new(|| {}),
            on_kick: Arc::new(|| {}),
        }
    }
}

struct Regs {
    rx: VecDeque<u8>,
    ier: u8,
    fcr: u8,
    lcr: u8,
    mcr: u8,
    scr: u8,
    dll: u8,
    dlm: u8,
    lsr_oe: bool,
    thr_pending: bool,
    msr_delta: u8,
    irq_pulses: u32,
}

impl Regs {
    fn new() -> Self {
        Self {
            rx: VecDeque::new(),
            ier: 0,
            fcr: 0,
            lcr: 0,
            mcr: 0,
            scr: 0,
            dll: 1,
            dlm: 0,
            lsr_oe: false,
            thr_pending: true,
            msr_delta: 0,
            irq_pulses: 0,
        }
    }

    fn dlab(&self) -> bool {
        self.lcr & 0x80 != 0
    }

    fn trigger(&self) -> usize {
        if self.fcr & 1 == 0 {
            return 1;
        }
        match self.fcr & 0xc0 {
            0x00 => 1,
            0x40 => 4,
            0x80 => 8,
            _ => 14,
        }
    }

    fn pulse(&mut self) {
        self.irq_pulses = self.irq_pulses.wrapping_add(1);
    }

    fn modem_lines(&self) -> u8 {
        if self.mcr & MCR_LOOP != 0 {
            let mut v = 0;
            if self.mcr & MCR_RTS != 0 {
                v |= MSR_CTS;
            }
            if self.mcr & MCR_DTR != 0 {
                v |= MSR_DSR;
            }
            if self.mcr & MCR_OUT1 != 0 {
                v |= MSR_RI;
            }
            if self.mcr & MCR_OUT2 != 0 {
                v |= MSR_DCD;
            }
            v
        } else {
            MSR_LINES
        }
    }

    fn push_rx(&mut self, byte: u8) {
        if self.rx.len() >= RX_CAP {
            self.lsr_oe = true;
            return;
        }
        let was_empty = self.rx.is_empty();
        self.rx.push_back(byte);
        if was_empty && self.ier & IER_RX != 0 {
            self.pulse();
        }
    }

    fn rearm_rx(&mut self) {
        if self.ier & IER_RX != 0 && self.rx.len() >= self.trigger() {
            self.pulse();
        }
    }

    fn read_reg(&mut self, offset: u16) -> u8 {
        match offset {
            0 if self.dlab() => self.dll,
            0 => {
                let byte = self.rx.pop_front().unwrap_or(0);
                self.rearm_rx();
                byte
            }
            1 if self.dlab() => self.dlm,
            1 => self.ier,
            2 => {
                let fifo = if self.fcr & 1 != 0 { 0xc0 } else { 0 };
                if self.ier & IER_RX != 0 && !self.rx.is_empty() {
                    self.rearm_rx();
                    fifo | 0x04
                } else if self.ier & IER_THRE != 0 && self.thr_pending {
                    self.thr_pending = false;
                    fifo | 0x02
                } else {
                    fifo | 0x01
                }
            }
            3 => self.lcr,
            4 => self.mcr,
            5 => {
                let mut v = LSR_THRE | LSR_TEMT;
                if !self.rx.is_empty() {
                    v |= LSR_DR;
                }
                if self.lsr_oe {
                    v |= LSR_OE;
                    self.lsr_oe = false;
                }
                v
            }
            6 => {
                let v = self.msr_delta | self.modem_lines();
                self.msr_delta = 0;
                v
            }
            7 => self.scr,
            _ => 0xff,
        }
    }

    /// `Some(byte)` is a host-visible TX byte. Loopback stays inside the chip.
    fn write_reg(&mut self, offset: u16, value: u8) -> Option<u8> {
        match offset {
            0 if self.dlab() => self.dll = value,
            0 => {
                self.thr_pending = true;
                if self.mcr & MCR_LOOP != 0 {
                    self.push_rx(value);
                    return None;
                }
                return Some(value);
            }
            1 if self.dlab() => self.dlm = value,
            1 => {
                self.ier = value & 0x0f;
                if self.ier & IER_RX != 0 && !self.rx.is_empty() {
                    self.pulse();
                }
                if self.ier & IER_THRE != 0 {
                    self.thr_pending = true;
                }
            }
            2 => {
                if value & 0x02 != 0 {
                    self.rx.clear();
                }
                if value & 0x01 == 0 {
                    self.rx.clear();
                    self.fcr = 0;
                } else {
                    self.fcr = value & 0xc9;
                }
            }
            3 => self.lcr = value,
            4 => {
                let old = self.modem_lines();
                self.mcr = value & 0x1f;
                let new = self.modem_lines();
                let changed = old ^ new;
                if changed & MSR_CTS != 0 {
                    self.msr_delta |= MSR_DCTS;
                }
                if changed & MSR_DSR != 0 {
                    self.msr_delta |= MSR_DDSR;
                }
                if changed & MSR_RI != 0 {
                    self.msr_delta |= MSR_TERI;
                }
                if changed & MSR_DCD != 0 {
                    self.msr_delta |= MSR_DDCD;
                }
            }
            7 => self.scr = value,
            _ => {}
        }
        None
    }
}

pub struct Ns16550 {
    regs: Mutex<Regs>,
    log: Mutex<Option<File>>,
    hooks: ConsoleHooks,
}

impl Ns16550 {
    pub fn new(log_path: Option<&Path>, hooks: ConsoleHooks) -> Result<Self, String> {
        let log = match log_path {
            Some(path) => Some(
                OpenOptions::new()
                    .create(true)
                    .write(true)
                    .truncate(true)
                    .open(path)
                    .map_err(|e| format!("serial log {}: {e}", path.display()))?,
            ),
            None => None,
        };
        Ok(Self {
            regs: Mutex::new(Regs::new()),
            log: Mutex::new(log),
            hooks,
        })
    }

    pub fn irq_line(&self) -> u8 {
        COM1_IRQ
    }

    pub fn irq_pulses(&self) -> u32 {
        self.regs.lock().unwrap().irq_pulses
    }

    pub fn read_port(&self, offset: u16, size: usize) -> u64 {
        let mut regs = self.regs.lock().unwrap();
        let mut value = 0u64;
        for i in 0..size {
            let byte = regs.read_reg(offset + i as u16);
            value |= (byte as u64) << (8 * i);
        }
        value
    }

    pub fn write_port(&self, offset: u16, data: u64, size: usize) {
        let mut tx = [0u8; 4];
        let mut n = 0;
        {
            let mut regs = self.regs.lock().unwrap();
            for i in 0..size {
                let byte = ((data >> (8 * i)) & 0xff) as u8;
                if let Some(out) = regs.write_reg(offset + i as u16, byte) {
                    tx[n] = out;
                    n += 1;
                }
            }
        }
        if n > 0 {
            self.emit_tx(&tx[..n]);
        }
    }

    fn emit_tx(&self, bytes: &[u8]) {
        let mut stdout = io::stdout().lock();
        for &ch in bytes {
            if ch == b'\n' {
                let _ = stdout.write_all(b"\r\n");
            } else {
                let _ = stdout.write_all(&[ch]);
            }
        }
        let _ = stdout.flush();
        if let Some(file) = self.log.lock().unwrap().as_mut() {
            let _ = file.write_all(bytes);
            let _ = file.flush();
        }
    }

    /// Host input policy shared with the old PL011: Ctrl-] shuts down, CR is stored as LF.
    pub fn push_host_bytes(&self, bytes: &[u8]) {
        for &b in bytes {
            if b == 0x1d {
                eprintln!("\r\n[VMM] Escape key Ctrl-] pressed. Exiting VM.\r\n");
                (self.hooks.on_shutdown)();
                return;
            }
            let byte = if b == b'\r' { b'\n' } else { b };
            self.regs.lock().unwrap().push_rx(byte);
        }
        (self.hooks.on_kick)();
    }

    pub fn ingest_host_file(&self, path: &Path) {
        if !path.exists() {
            return;
        }
        let data = fs::read(path).unwrap_or_default();
        let _ = fs::remove_file(path);
        if !data.is_empty() {
            self.push_host_bytes(&data);
        }
    }

    fn nudge_if_shutdown(&self) {
        if crate::terminal::shutdown_requested() {
            (self.hooks.on_kick)();
        }
    }
}

impl PioDevice for Ns16550 {
    fn pio_in(&self, offset: u16, size: usize) -> u64 {
        self.read_port(offset, size)
    }

    fn pio_out(&self, offset: u16, data: u64, size: usize) {
        self.write_port(offset, data, size);
    }
}

pub struct HostConsole {
    running: Arc<AtomicBool>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl HostConsole {
    pub fn start(uart: Arc<Ns16550>) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let mut workers = Vec::new();

        let uart_in = uart.clone();
        let active = running.clone();
        workers.push(
            thread::Builder::new()
                .name("vmm-stdin".into())
                .spawn(move || stdin_loop(uart_in, active))
                .expect("failed to spawn stdin thread"),
        );

        if let Some(path) = std::env::var_os("VMM_UART_IN") {
            let uart_inject = uart.clone();
            let active = running.clone();
            workers.push(
                thread::Builder::new()
                    .name("vmm-uart-inject".into())
                    .spawn(move || inject_loop(uart_inject, PathBuf::from(path), active))
                    .expect("failed to spawn uart inject thread"),
            );
        }

        Self { running, workers }
    }

    pub fn stop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

impl Drop for HostConsole {
    fn drop(&mut self) {
        self.stop();
    }
}

fn stdin_loop(uart: Arc<Ns16550>, active: Arc<AtomicBool>) {
    let mut buf = [0u8; 128];
    let mut stdin_open = true;
    while active.load(Ordering::Relaxed) {
        if stdin_open {
            let mut fd = libc::pollfd {
                fd: 0,
                events: libc::POLLIN,
                revents: 0,
            };
            let rc = unsafe { libc::poll(&mut fd, 1, 100) };
            if rc > 0 && fd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    stdin_open = false;
                } else {
                    uart.push_host_bytes(&buf[..n as usize]);
                }
            }
        } else {
            thread::sleep(Duration::from_millis(100));
        }
        uart.nudge_if_shutdown();
    }
}

fn inject_loop(uart: Arc<Ns16550>, path: PathBuf, active: Arc<AtomicBool>) {
    while active.load(Ordering::Relaxed) {
        uart.ingest_host_file(&path);
        thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn uart() -> Ns16550 {
        Ns16550::new(None, ConsoleHooks::default()).unwrap()
    }

    #[test]
    fn loopback_msr_follows_mcr() {
        let uart = uart();
        assert_eq!(uart.irq_line(), COM1_IRQ);
        assert_eq!(uart.read_port(6, 1) & 0xf0, MSR_LINES as u64);
        // LOOP | OUT2 | RTS. CTS follows RTS, DCD follows OUT2.
        uart.write_port(4, (MCR_LOOP | MCR_OUT2 | MCR_RTS) as u64, 1);
        let _ = uart.read_port(6, 1);
        let msr = uart.read_port(6, 1);
        assert_eq!(msr & 0xf0, (MSR_CTS | MSR_DCD) as u64);

        uart.write_port(4, (MCR_LOOP | MCR_DTR) as u64, 1);
        let _ = uart.read_port(6, 1);
        assert_eq!(uart.read_port(6, 1) & 0xf0, MSR_DSR as u64);

        uart.write_port(0, b'Z' as u64, 1);
        assert_eq!(uart.read_port(0, 1), b'Z' as u64);
        assert_eq!(uart.read_port(5, 1) & (LSR_THRE | LSR_TEMT) as u64, 0x60);
    }

    #[test]
    fn rx_empty_to_nonempty_pulses_once_and_rearms_above_trigger() {
        let uart = uart();
        uart.write_port(1, IER_RX as u64, 1);
        assert_eq!(uart.irq_pulses(), 0);
        uart.push_host_bytes(b"AB");
        assert_eq!(uart.irq_pulses(), 1);
        assert_eq!(uart.read_port(0, 1), b'A' as u64);
        assert_eq!(uart.irq_pulses(), 2);
        assert_eq!(uart.read_port(0, 1), b'B' as u64);
        assert_eq!(uart.irq_pulses(), 2);
    }

    #[test]
    fn vmm_uart_in_bytes_appear_in_rbr() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!(
            "vmm-uart-in-{}-{}.bin",
            std::process::id(),
            line!()
        ));
        fs::write(&path, b"hi\r").unwrap();
        let shut = Arc::new(AtomicBool::new(false));
        let kicks = Arc::new(AtomicUsize::new(0));
        let shut2 = shut.clone();
        let kicks2 = kicks.clone();
        let uart = Ns16550::new(
            None,
            ConsoleHooks {
                on_shutdown: Arc::new(move || shut2.store(true, Ordering::Relaxed)),
                on_kick: Arc::new(move || {
                    kicks2.fetch_add(1, Ordering::Relaxed);
                }),
            },
        )
        .unwrap();
        uart.ingest_host_file(&path);
        assert!(!path.exists());
        assert_eq!(uart.read_port(0, 1), b'h' as u64);
        assert_eq!(uart.read_port(0, 1), b'i' as u64);
        assert_eq!(uart.read_port(0, 1), b'\n' as u64);
        assert!(kicks.load(Ordering::Relaxed) >= 1);
        uart.push_host_bytes(&[0x1d]);
        assert!(shut.load(Ordering::Relaxed));
        assert_eq!(uart.read_port(5, 1) & 1, 0);
    }
}
