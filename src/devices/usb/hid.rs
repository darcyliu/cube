//! Virtual USB HID boot keyboard + QEMU-compatible absolute tablet.
//!
//! Descriptors mirror QEMU `hw/usb/dev-hid.c` closely enough for FreeBSD
//! `ukbd` / `ums` / `usbhid` enumeration. Tablet absolute range is 0..0x7fff
//! (same as QEMU); the frontend scales VNC pixel coordinates into that range.

#![allow(dead_code)]

use std::collections::VecDeque;

/// Port index → device kind (matches xHCI root-port wiring).
pub const PORT_KEYBOARD: usize = 1; // 1-based port id
pub const PORT_TABLET: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HidKind {
    Keyboard,
    Tablet,
}

/// Shared input queues consumed by interrupt-IN completions.
pub struct HidFrontend {
    pub abs_max_x: u16,
    pub abs_max_y: u16,
    kbd_mods: u8,
    kbd_keys: [u8; 6],
    kbd_pending: VecDeque<[u8; 8]>,
    tablet_buttons: u8,
    tablet_x: u16,
    tablet_y: u16,
    tablet_pending: VecDeque<[u8; 6]>,
}

impl HidFrontend {
    pub fn new(abs_max_x: u16, abs_max_y: u16) -> Self {
        Self {
            abs_max_x: abs_max_x.max(1),
            abs_max_y: abs_max_y.max(1),
            kbd_mods: 0,
            kbd_keys: [0; 6],
            kbd_pending: VecDeque::new(),
            tablet_buttons: 0,
            tablet_x: 0,
            tablet_y: 0,
            tablet_pending: VecDeque::new(),
        }
    }

    /// Linux evdev KEY_* → HID boot report (usage page 0x07).
    pub fn key_event(&mut self, linux_keycode: u16, down: bool) {
        if let Some(mod_bit) = linux_to_hid_modifier(linux_keycode) {
            if down {
                self.kbd_mods |= mod_bit;
            } else {
                self.kbd_mods &= !mod_bit;
            }
            self.push_kbd_report();
            return;
        }
        let Some(usage) = linux_to_hid_usage(linux_keycode) else {
            return;
        };
        if down {
            if !self.kbd_keys.contains(&usage) {
                if let Some(slot) = self.kbd_keys.iter_mut().find(|k| **k == 0) {
                    *slot = usage;
                }
            }
        } else {
            for k in &mut self.kbd_keys {
                if *k == usage {
                    *k = 0;
                }
            }
            // compact
            let mut compact = [0u8; 6];
            let mut i = 0;
            for &k in &self.kbd_keys {
                if k != 0 && i < 6 {
                    compact[i] = k;
                    i += 1;
                }
            }
            self.kbd_keys = compact;
        }
        self.push_kbd_report();
    }

    /// Absolute pointer in framebuffer pixels; buttons: bit0=L bit1=M bit2=R (RFB).
    pub fn pointer_event(&mut self, x: u16, y: u16, buttons: u8) {
        let x = x.min(self.abs_max_x);
        let y = y.min(self.abs_max_y);
        // Scale to QEMU tablet logical max 0x7fff.
        let sx = if self.abs_max_x == 0 {
            0
        } else {
            ((x as u32) * 0x7fff / self.abs_max_x as u32) as u16
        };
        let sy = if self.abs_max_y == 0 {
            0
        } else {
            ((y as u32) * 0x7fff / self.abs_max_y as u32) as u16
        };
        // RFB: 1=left, 2=middle, 4=right → HID: bit0=left, bit1=right, bit2=middle
        let hid_btn = (buttons & 1) | ((buttons & 4) >> 1) | ((buttons & 2) << 1);
        self.tablet_buttons = hid_btn & 0x1f;
        self.tablet_x = sx;
        self.tablet_y = sy;
        self.push_tablet_report();
    }

    fn push_kbd_report(&mut self) {
        let mut r = [0u8; 8];
        r[0] = self.kbd_mods;
        r[2..8].copy_from_slice(&self.kbd_keys);
        if self.kbd_pending.len() >= 32 {
            self.kbd_pending.pop_front();
        }
        self.kbd_pending.push_back(r);
    }

    fn push_tablet_report(&mut self) {
        let mut r = [0u8; 6];
        r[0] = self.tablet_buttons;
        r[1..3].copy_from_slice(&self.tablet_x.to_le_bytes());
        r[3..5].copy_from_slice(&self.tablet_y.to_le_bytes());
        r[5] = 0; // wheel
        if self.tablet_pending.len() >= 64 {
            self.tablet_pending.pop_front();
        }
        self.tablet_pending.push_back(r);
    }

    pub fn take_keyboard_report(&mut self) -> Option<[u8; 8]> {
        self.kbd_pending.pop_front()
    }

    pub fn peek_keyboard_report(&self) -> Option<[u8; 8]> {
        self.kbd_pending.front().copied()
    }

    pub fn take_tablet_report(&mut self) -> Option<[u8; 6]> {
        self.tablet_pending.pop_front()
    }

    pub fn has_keyboard_report(&self) -> bool {
        !self.kbd_pending.is_empty()
    }

    pub fn has_tablet_report(&self) -> bool {
        !self.tablet_pending.is_empty()
    }

    pub fn current_keyboard_report(&self) -> [u8; 8] {
        let mut r = [0u8; 8];
        r[0] = self.kbd_mods;
        r[2..8].copy_from_slice(&self.kbd_keys);
        r
    }

    pub fn current_tablet_report(&self) -> [u8; 6] {
        let mut r = [0u8; 6];
        r[0] = self.tablet_buttons;
        r[1..3].copy_from_slice(&self.tablet_x.to_le_bytes());
        r[3..5].copy_from_slice(&self.tablet_y.to_le_bytes());
        r
    }
}

// ---- Descriptors (QEMU-compatible) ----

pub fn device_descriptor(kind: HidKind) -> [u8; 18] {
    let (id_product_lo, i_product, i_serial) = match kind {
        HidKind::Keyboard => (0x01u8, 3u8, 4u8),
        HidKind::Tablet => (0x01, 2, 5),
    };
    [
        18,   // bLength
        0x01, // DEVICE
        0x00,
        0x02, // bcdUSB 2.00
        0x00, // class (interface)
        0x00,
        0x00,
        64, // bMaxPacketSize0
        0x27,
        0x06, // idVendor 0x0627 (QEMU)
        id_product_lo,
        0x00,
        0x00,
        0x00, // bcdDevice
        1,    // iManufacturer
        i_product,
        i_serial,
        1, // bNumConfigurations
    ]
}

pub fn config_descriptor(kind: HidKind) -> Vec<u8> {
    let (iface_proto, report_len, max_pkt, interval, i_config) = match kind {
        HidKind::Keyboard => (0x01u8, 63u16, 8u16, 7u8, 6u8), // boot keyboard
        HidKind::Tablet => (0x00, 74, 8, 4, 7),
    };
    let subclass = match kind {
        HidKind::Keyboard => 0x01u8, // boot
        HidKind::Tablet => 0x00,
    };
    let total = 9 + 9 + 9 + 7; // config + iface + hid + ep
    let mut d = Vec::with_capacity(total);
    // Configuration
    d.extend_from_slice(&[
        9,
        0x02,
        (total as u8),
        (total >> 8) as u8,
        1, // bNumInterfaces
        1, // bConfigurationValue
        i_config,
        0xa0, // bmAttributes: self-powered + remote wakeup style
        50,   // bMaxPower (100mA)
    ]);
    // Interface
    d.extend_from_slice(&[
        9,
        0x04,
        0,    // bInterfaceNumber
        0,    // bAlternateSetting
        1,    // bNumEndpoints
        0x03, // HID
        subclass,
        iface_proto,
        0, // iInterface
    ]);
    // HID
    d.extend_from_slice(&[
        9,
        0x21,
        0x11,
        0x01, // bcdHID 1.11
        0x00, // country
        0x01, // num descriptors
        0x22, // report
        (report_len as u8),
        (report_len >> 8) as u8,
    ]);
    // Endpoint IN 1 interrupt
    d.extend_from_slice(&[
        7,
        0x05,
        0x81, // IN | 1
        0x03, // interrupt
        (max_pkt as u8),
        (max_pkt >> 8) as u8,
        interval,
    ]);
    d
}

pub fn report_descriptor(kind: HidKind) -> &'static [u8] {
    match kind {
        HidKind::Keyboard => &KEYBOARD_REPORT_DESC,
        HidKind::Tablet => &TABLET_REPORT_DESC,
    }
}

pub fn string_descriptor(index: u8) -> Vec<u8> {
    let s: &str = match index {
        0 => return vec![4, 0x03, 0x09, 0x04], // langid en-US
        1 => "VMM",
        2 => "VMM USB Tablet",
        3 => "VMM USB Keyboard",
        4 => "kbd0",
        5 => "tablet0",
        6 => "HID Keyboard",
        7 => "HID Tablet",
        _ => "",
    };
    let mut out = Vec::with_capacity(2 + s.len() * 2);
    out.push((2 + s.len() * 2) as u8);
    out.push(0x03);
    for c in s.encode_utf16() {
        out.extend_from_slice(&c.to_le_bytes());
    }
    out
}

/// QEMU keyboard HID report descriptor (63 bytes).
static KEYBOARD_REPORT_DESC: [u8; 63] = [
    0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x75, 0x01, 0x95, 0x08, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7,
    0x15, 0x00, 0x25, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x05, 0x75, 0x01,
    0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, 0x95, 0x01, 0x75, 0x03, 0x91, 0x01, 0x95, 0x06,
    0x75, 0x08, 0x15, 0x00, 0x25, 0xff, 0x05, 0x07, 0x19, 0x00, 0x29, 0xff, 0x81, 0x00, 0xc0,
];

/// QEMU tablet HID report descriptor (74 bytes) — absolute X/Y 0..0x7fff.
static TABLET_REPORT_DESC: [u8; 74] = [
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x05,
    0x15, 0x00, 0x25, 0x01, 0x95, 0x05, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x03, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xff, 0x7f, 0x35, 0x00, 0x46, 0xff, 0x7f,
    0x75, 0x10, 0x95, 0x02, 0x81, 0x02, 0x05, 0x01, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7f, 0x35, 0x00,
    0x45, 0x00, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xc0, 0xc0,
];

fn linux_to_hid_modifier(code: u16) -> Option<u8> {
    Some(match code {
        29 => 0x01,  // KEY_LEFTCTRL
        42 => 0x02,  // KEY_LEFTSHIFT
        56 => 0x04,  // KEY_LEFTALT
        125 => 0x08, // KEY_LEFTMETA
        97 => 0x10,  // KEY_RIGHTCTRL
        54 => 0x20,  // KEY_RIGHTSHIFT
        100 => 0x40, // KEY_RIGHTALT
        126 => 0x80, // KEY_RIGHTMETA
        _ => return None,
    })
}

fn linux_to_hid_usage(code: u16) -> Option<u8> {
    // Common set; unknown keys are dropped.
    Some(match code {
        1 => 0x29, // Esc
        2 => 0x1e, // 1
        3 => 0x1f,
        4 => 0x20,
        5 => 0x21,
        6 => 0x22,
        7 => 0x23,
        8 => 0x24,
        9 => 0x25,
        10 => 0x26,
        11 => 0x27, // 0
        12 => 0x2d, // -
        13 => 0x2e, // =
        14 => 0x2a, // Backspace
        15 => 0x2b, // Tab
        16 => 0x14, // Q
        17 => 0x1a, // W
        18 => 0x08, // E
        19 => 0x15, // R
        20 => 0x17, // T
        21 => 0x1c, // Y
        22 => 0x18, // U
        23 => 0x0c, // I
        24 => 0x12, // O
        25 => 0x13, // P
        26 => 0x2f, // [
        27 => 0x30, // ]
        28 => 0x28, // Enter
        30 => 0x04, // A
        31 => 0x16, // S
        32 => 0x07, // D
        33 => 0x09, // F
        34 => 0x0a, // G
        35 => 0x0b, // H
        36 => 0x0d, // J
        37 => 0x0e, // K
        38 => 0x0f, // L
        39 => 0x33, // ;
        40 => 0x34, // '
        41 => 0x35, // `
        43 => 0x31, // \
        44 => 0x1d, // Z
        45 => 0x1b, // X
        46 => 0x06, // C
        47 => 0x19, // V
        48 => 0x05, // B
        49 => 0x11, // N
        50 => 0x10, // M
        51 => 0x36, // ,
        52 => 0x37, // .
        53 => 0x38, // /
        57 => 0x2c, // Space
        58 => 0x39, // CapsLock
        59 => 0x3a, // F1
        60 => 0x3b,
        61 => 0x3c,
        62 => 0x3d,
        63 => 0x3e,
        64 => 0x3f,
        65 => 0x40,
        66 => 0x41,
        67 => 0x42,
        68 => 0x43,  // F10
        87 => 0x44,  // F11
        88 => 0x45,  // F12
        102 => 0x4a, // Home
        103 => 0x52, // Up
        104 => 0x4b, // PageUp
        105 => 0x50, // Left
        106 => 0x4f, // Right
        107 => 0x4d, // End
        108 => 0x51, // Down
        109 => 0x4e, // PageDown
        110 => 0x49, // Insert
        111 => 0x4c, // Delete
        _ => return None,
    })
}

/// Handle a standard USB control request for our HID devices.
/// Returns response bytes (may be empty) or None to STALL.
pub fn handle_control(
    kind: HidKind,
    request_type: u8,
    request: u8,
    value: u16,
    _index: u16,
    length: u16,
) -> Option<Vec<u8>> {
    let dir_in = request_type & 0x80 != 0;
    let rtype = request_type & 0x60;
    let recip = request_type & 0x1f;

    // Standard requests
    if rtype == 0x00 {
        match (request, recip) {
            (0x06, 0x00) | (0x06, 0x01) | (0x06, 0x02) => {
                // GET_DESCRIPTOR
                let dtype = (value >> 8) as u8;
                let dindex = (value & 0xff) as u8;
                let data = match dtype {
                    0x01 => device_descriptor(kind).to_vec(),
                    0x02 => config_descriptor(kind),
                    0x03 => string_descriptor(dindex),
                    0x22 if recip == 1 => report_descriptor(kind).to_vec(),
                    0x21 if recip == 1 => config_descriptor(kind)[18..27].to_vec(),
                    _ => return None,
                };
                let n = (length as usize).min(data.len());
                return Some(data[..n].to_vec());
            }
            (0x05, _) => {
                // SET_ADDRESS — xHCI Address Device owns this; ACK empty
                return Some(Vec::new());
            }
            (0x09, _) => {
                // SET_CONFIGURATION
                return Some(Vec::new());
            }
            (0x08, _) if dir_in => {
                // GET_CONFIGURATION
                return Some(vec![1]);
            }
            (0x0b, _) if !dir_in => {
                // SET_INTERFACE
                return Some(Vec::new());
            }
            (0x0a, _) if dir_in => {
                // GET_INTERFACE
                return Some(vec![0]);
            }
            (0x00, _) if dir_in => {
                // GET_STATUS
                return Some(vec![0, 0]);
            }
            _ => {}
        }
    }

    // Class (HID) requests on interface
    if rtype == 0x20 && recip == 0x01 {
        match request {
            0x01 => {
                // GET_REPORT
                // value hi = report type (1=input), lo = report id
                return Some(match kind {
                    HidKind::Keyboard => vec![0; 8.min(length as usize)],
                    HidKind::Tablet => vec![0; 6.min(length as usize)],
                });
            }
            0x09 => {
                // SET_REPORT
                return Some(Vec::new());
            }
            0x0a => {
                // SET_IDLE
                return Some(Vec::new());
            }
            0x0b => {
                // SET_PROTOCOL
                return Some(Vec::new());
            }
            0x03 if dir_in => {
                // GET_PROTOCOL
                return Some(vec![1]); // report protocol
            }
            0x02 if dir_in => {
                // GET_IDLE
                return Some(vec![0]);
            }
            _ => {}
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn standard_interface_request_returns_hid_descriptors() {
        for kind in [HidKind::Keyboard, HidKind::Tablet] {
            let report = report_descriptor(kind);
            assert_eq!(
                handle_control(kind, 0x81, 6, 0x2200, 0, 255).unwrap(),
                report
            );
            assert_eq!(
                handle_control(kind, 0x81, 6, 0x2200, 0, 2).unwrap(),
                report[..2]
            );
            assert_eq!(
                handle_control(kind, 0x81, 6, 0x2100, 0, 255).unwrap(),
                config_descriptor(kind)[18..27]
            );
        }
    }
}
