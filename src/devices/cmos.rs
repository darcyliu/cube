//! CMOS/RTC ports `0x70`/`0x71`. Index bit 7 (NMI) is ignored.

use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use super::PioDevice;

pub struct Cmos {
    index: Mutex<u8>,
    regs: Mutex<[u8; 128]>,
    /// `Some` pins the clock for tests. `None` reads the host UTC clock.
    unix: Mutex<Option<i64>>,
}

impl Cmos {
    pub fn new(ram_size: u64) -> Self {
        let mut regs = [0u8; 128];
        regs[0x0A] = 0x26;
        regs[0x0B] = 0x06;
        regs[0x0D] = 0x80;
        let base = 640u16;
        regs[0x15] = base as u8;
        regs[0x16] = (base >> 8) as u8;
        let ext = ((ram_size.saturating_sub(0x10_0000)) / 1024).min(0xFFFF) as u16;
        regs[0x17] = ext as u8;
        regs[0x18] = (ext >> 8) as u8;
        regs[0x30] = ext as u8;
        regs[0x31] = (ext >> 8) as u8;
        let above = if ram_size > 16 * 1024 * 1024 {
            ((ram_size - 16 * 1024 * 1024) / (64 * 1024)).min(0xFFFF) as u16
        } else {
            0
        };
        regs[0x34] = above as u8;
        regs[0x35] = (above >> 8) as u8;
        Self {
            index: Mutex::new(0),
            regs: Mutex::new(regs),
            unix: Mutex::new(None),
        }
    }

    pub fn set_unix_for_test(&self, secs: i64) {
        *self.unix.lock().unwrap() = Some(secs);
    }

    fn now(&self) -> i64 {
        if let Some(secs) = *self.unix.lock().unwrap() {
            return secs;
        }
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }

    fn read_indexed(&self, index: u8) -> u8 {
        let (year, month, day, weekday, hour, minute, second) = civil_from_unix(self.now());
        let regs = self.regs.lock().unwrap();
        let reg_b = regs[0x0B];
        let binary = reg_b & 0x04 != 0;
        let hour24 = reg_b & 0x02 != 0;
        let enc = |v: u32| -> u8 { if binary { v as u8 } else { to_bcd(v) } };
        match index {
            0x00 => enc(second),
            0x02 => enc(minute),
            0x04 => enc_hour(hour, binary, hour24),
            0x06 => enc(weekday + 1),
            0x07 => enc(day),
            0x08 => enc(month),
            0x09 => enc((year % 100) as u32),
            0x0D => regs[0x0D] | 0x80,
            0x32 => enc((year / 100) as u32),
            other => regs[other as usize],
        }
    }
}

impl PioDevice for Cmos {
    fn pio_in(&self, offset: u16, _size: usize) -> u64 {
        match offset {
            0 => u64::from(*self.index.lock().unwrap()),
            _ => u64::from(self.read_indexed(*self.index.lock().unwrap() & 0x7f)),
        }
    }

    fn pio_out(&self, offset: u16, data: u64, _size: usize) {
        match offset {
            0 => *self.index.lock().unwrap() = (data as u8) & 0x7f,
            _ => {
                let index = *self.index.lock().unwrap() & 0x7f;
                if !matches!(index, 0x00 | 0x02 | 0x04 | 0x06 | 0x07 | 0x08 | 0x09 | 0x32) {
                    self.regs.lock().unwrap()[index as usize] = data as u8;
                }
            }
        }
    }
}

fn enc_hour(hour: u32, binary: bool, hour24: bool) -> u8 {
    if hour24 {
        return if binary { hour as u8 } else { to_bcd(hour) };
    }
    let pm = if hour >= 12 { 0x80 } else { 0 };
    let mut h = hour % 12;
    if h == 0 {
        h = 12;
    }
    pm | if binary { h as u8 } else { to_bcd(h) }
}

fn to_bcd(v: u32) -> u8 {
    let v = v % 100;
    (((v / 10) << 4) | (v % 10)) as u8
}

/// Howard Hinnant civil_from_days. Weekday 0 is Sunday; 1970-01-01 is Thursday.
pub fn civil_from_unix(secs: i64) -> (i32, u32, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400) as u32;
    let hour = tod / 3600;
    let minute = (tod % 3600) / 60;
    let second = tod % 60;
    let (year, month, day) = civil_from_days(days);
    let weekday = (days + 4).rem_euclid(7) as u32;
    (year, month, day, weekday, hour, minute, second)
}

fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };
    (year as i32, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_is_thursday_and_sizes_match_128mib() {
        let (year, month, day, weekday, hour, minute, second) = civil_from_unix(0);
        assert_eq!(
            (year, month, day, hour, minute, second),
            (1970, 1, 1, 0, 0, 0)
        );
        assert_eq!(weekday, 4);
        let cmos = Cmos::new(128 << 20);
        cmos.set_unix_for_test(0);
        cmos.pio_out(0, 0x09, 1);
        assert_eq!(cmos.pio_in(1, 1), 70);
        cmos.pio_out(0, 0x08, 1);
        assert_eq!(cmos.pio_in(1, 1), 1);
        cmos.pio_out(0, 0x07, 1);
        assert_eq!(cmos.pio_in(1, 1), 1);
        cmos.pio_out(0, 0x06, 1);
        assert_eq!(cmos.pio_in(1, 1), 5);
        cmos.pio_out(0, 0x32, 1);
        assert_eq!(cmos.pio_in(1, 1), 19);
        cmos.pio_out(0, 0x0A, 1);
        assert_eq!(cmos.pio_in(1, 1), 0x26);
        cmos.pio_out(0, 0x0B, 1);
        assert_eq!(cmos.pio_in(1, 1), 0x06);
        cmos.pio_out(0, 0x0D, 1);
        assert_ne!(cmos.pio_in(1, 1) & 0x80, 0);
        cmos.pio_out(0, 0x15, 1);
        assert_eq!(cmos.pio_in(1, 1), 0x80);
        cmos.pio_out(0, 0x16, 1);
        assert_eq!(cmos.pio_in(1, 1), 0x02);
        cmos.pio_out(0, 0x17, 1);
        assert_eq!(cmos.pio_in(1, 1), 0xff);
        cmos.pio_out(0, 0x18, 1);
        assert_eq!(cmos.pio_in(1, 1), 0xff);
        cmos.pio_out(0, 0x34, 1);
        assert_eq!(cmos.pio_in(1, 1), 0x00);
        cmos.pio_out(0, 0x35, 1);
        assert_eq!(cmos.pio_in(1, 1), 0x07);
        cmos.pio_out(0, 0x5B, 1);
        assert_eq!(cmos.pio_in(1, 1), 0);
        cmos.pio_out(0, 0x80, 1);
        assert_eq!(*cmos.index.lock().unwrap(), 0);
    }
}
