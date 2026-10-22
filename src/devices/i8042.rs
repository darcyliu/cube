//! Enough of the 8042 for OVMF's keyboard self-test. Port `0x64` value `0xFE` reboots.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use super::PioDevice;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Next {
    Key,
    CommandByte,
    OutputPort,
}

struct State {
    out: VecDeque<u8>,
    cmd: u8,
    next: Next,
    output_port: u8,
}

pub struct I8042 {
    state: Mutex<State>,
    reboot: AtomicBool,
}

impl I8042 {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State {
                out: VecDeque::new(),
                cmd: 0x45,
                next: Next::Key,
                output_port: 0x01,
            }),
            reboot: AtomicBool::new(false),
        }
    }

    pub fn take_reboot(&self) -> bool {
        self.reboot.swap(false, Ordering::Relaxed)
    }
}

impl Default for I8042 {
    fn default() -> Self {
        Self::new()
    }
}

impl PioDevice for I8042 {
    fn pio_in(&self, offset: u16, _size: usize) -> u64 {
        match offset {
            0 => {
                let mut st = self.state.lock().unwrap();
                u64::from(st.out.pop_front().unwrap_or(0))
            }
            4 => {
                let st = self.state.lock().unwrap();
                let obf = if st.out.is_empty() { 0u8 } else { 1 };
                // Bit 2 set, input buffer empty (bit 1 clear).
                u64::from(0x04 | obf)
            }
            _ => 0xff,
        }
    }

    fn pio_out(&self, offset: u16, data: u64, _size: usize) {
        let byte = data as u8;
        if offset == 4 {
            let mut st = self.state.lock().unwrap();
            match byte {
                0xAA => st.out.push_back(0x55),
                0xAB | 0xA9 => st.out.push_back(0x00),
                0x20 => {
                    let cmd = st.cmd;
                    st.out.push_back(cmd);
                }
                0x60 => st.next = Next::CommandByte,
                0xD1 => st.next = Next::OutputPort,
                0xAD | 0xAE | 0xA7 | 0xA8 => {}
                0xFE => self.reboot.store(true, Ordering::Relaxed),
                _ => st.out.push_back(0xFA),
            }
            return;
        }
        if offset != 0 {
            return;
        }
        let mut st = self.state.lock().unwrap();
        match st.next {
            Next::CommandByte => {
                st.cmd = byte;
                st.next = Next::Key;
            }
            Next::OutputPort => {
                st.output_port = byte;
                st.next = Next::Key;
            }
            Next::Key => st.out.push_back(0xFA),
        }
    }
}
