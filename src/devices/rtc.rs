//! ARM PrimeCell PL031 Real-Time Clock (RTC) emulation.
//!
//! Conforms to ARM DDI 0224B (ARM PrimeCell Real Time Clock PL031 Technical Reference Manual).
//! Provides the guest OS with the host's real UTC time via UNIX epoch seconds.

use std::time::{SystemTime, UNIX_EPOCH};
use crate::devices::MmioDevice;

// PL031 Register Offsets
const RTC_DR: u64 = 0x000;   // Data Register (RO, seconds counter)
const RTC_MR: u64 = 0x004;   // Match Register (RW, alarm match value)
const RTC_LR: u64 = 0x008;   // Load Register (RW, counter load/offset)
const RTC_CR: u64 = 0x00C;   // Control Register (RW, bit 0 = enable)
const RTC_IMSC: u64 = 0x010; // Interrupt Mask Set and Clear (RW)
const RTC_RIS: u64 = 0x014;  // Raw Interrupt Status (RO)
const RTC_MIS: u64 = 0x018;  // Masked Interrupt Status (RO)
const RTC_ICR: u64 = 0x01C;  // Interrupt Clear Register (WO)

// ARM PrimeCell Identification Registers
const RTC_PERIPH_ID0: u64 = 0xFE0; // 0x31
const RTC_PERIPH_ID1: u64 = 0xFE4; // 0x10 (Part 0x031)
const RTC_PERIPH_ID2: u64 = 0xFE8; // 0x04 (Designer: ARM, Revision 0)
const RTC_PERIPH_ID3: u64 = 0xFEC; // 0x00
const RTC_PCELL_ID0: u64 = 0xFF0;  // 0x0D
const RTC_PCELL_ID1: u64 = 0xFF4;  // 0xF0
const RTC_PCELL_ID2: u64 = 0xFF8;  // 0x05
const RTC_PCELL_ID3: u64 = 0xFFC;  // 0xB1

pub struct Pl031Rtc {
    /// Offset in seconds added to host UNIX timestamp
    offset_seconds: i64,
    /// Match register value for interrupt trigger
    match_value: u32,
    /// Interrupt mask (1 = enabled, 0 = masked)
    imsc: u32,
    /// Raw interrupt status
    is_raw_irq: bool,
    /// RTC counter enabled (bit 0 of RTC_CR)
    enabled: bool,
    /// Interrupt line (SPI)
    irq_num: u32,
}

impl Pl031Rtc {
    pub fn new(irq_num: u32) -> Self {
        Self {
            offset_seconds: 0,
            match_value: 0,
            imsc: 0,
            is_raw_irq: false,
            enabled: true,
            irq_num,
        }
    }

    /// Computes the current RTC counter value in seconds since UNIX epoch.
    fn current_counter(&self) -> u32 {
        let host_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0) as i64;
        (host_now + self.offset_seconds) as u32
    }
}

impl MmioDevice for Pl031Rtc {
    fn read(&mut self, offset: u64, _size: usize) -> u64 {
        match offset {
            RTC_DR => self.current_counter() as u64,
            RTC_MR => self.match_value as u64,
            RTC_LR => self.current_counter() as u64,
            RTC_CR => if self.enabled { 1 } else { 0 },
            RTC_IMSC => self.imsc as u64,
            RTC_RIS => if self.is_raw_irq { 1 } else { 0 },
            RTC_MIS => {
                let mis = if self.is_raw_irq && (self.imsc & 1) != 0 { 1 } else { 0 };
                mis as u64
            }
            // PrimeCell identification
            RTC_PERIPH_ID0 => 0x31,
            RTC_PERIPH_ID1 => 0x10,
            RTC_PERIPH_ID2 => 0x04,
            RTC_PERIPH_ID3 => 0x00,
            RTC_PCELL_ID0 => 0x0D,
            RTC_PCELL_ID1 => 0xF0,
            RTC_PCELL_ID2 => 0x05,
            RTC_PCELL_ID3 => 0xB1,
            _ => 0,
        }
    }

    fn write(&mut self, offset: u64, data: u64, _size: usize) {
        let val = data as u32;
        match offset {
            RTC_MR => {
                self.match_value = val;
            }
            RTC_LR => {
                // Setting load register adjusts the counter offset relative to host time
                let host_now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0) as i64;
                self.offset_seconds = (val as i64) - host_now;
            }
            RTC_CR => {
                self.enabled = (val & 1) != 0;
            }
            RTC_IMSC => {
                self.imsc = val & 1;
                // Re-evaluate masked interrupt line
                let level = self.is_raw_irq && (self.imsc & 1) != 0;
                unsafe {
                    crate::hypervisor::hv_gic_set_spi(self.irq_num, level);
                }
            }
            RTC_ICR => {
                // Writing to ICR clears the interrupt
                self.is_raw_irq = false;
                unsafe {
                    crate::hypervisor::hv_gic_set_spi(self.irq_num, false);
                }
            }
            _ => {}
        }
    }
}
