//! Port `0x402`. One-byte writes are logged on stderr for the AP trampoline.

use super::PioDevice;

pub struct DebugCon;

impl PioDevice for DebugCon {
    fn pio_in(&self, _offset: u16, _size: usize) -> u64 {
        0xff
    }

    fn pio_out(&self, _offset: u16, data: u64, _size: usize) {
        crate::host_log!("port 0x402 <- {:02x}", data & 0xff);
    }
}
