//! Minimal RFB keysym → Linux KEY_* map for VNC → virtio-input.

/// Map an RFB/X11 keysym to a Linux evdev keycode, if known.
pub fn keysym_to_linux(keysym: u32) -> Option<u16> {
    // Latin-1 printable + common controls. Upper/lower share the same KEY_*.
    let k = match keysym {
        0xff63 => 110,
        0xff08 => 14,           // BackSpace / Insert→BackSpace fallback unused
        0xff09 => 15,           // Tab
        0xff0d | 0xff8d => 28,  // Return / KP_Enter
        0xff1b => 1,            // Escape
        0xffff => 111,          // Delete
        0xff50 => 102,          // Home
        0xff51 => 105,          // Left
        0xff52 => 103,          // Up
        0xff53 => 106,          // Right
        0xff54 => 108,          // Down
        0xff55 => 104,          // Page_Up
        0xff56 => 109,          // Page_Down
        0xff57 => 107,          // End
        0xffe1 | 0xffe2 => 42,  // Shift_L / Shift_R → left shift
        0xffe3 | 0xffe4 => 29,  // Control_L / Control_R
        0xffe9 | 0xffea => 56,  // Alt_L / Alt_R
        0xffe7 | 0xffe8 => 125, // Meta/Super_L → Left Meta
        0x0020 => 57,           // space
        0x0027 | 0x0022 => 40,  // apostrophe
        0x002c | 0x003c => 51,  // comma
        0x002d | 0x005f => 12,  // minus
        0x002e | 0x003e => 52,  // period
        0x002f | 0x003f => 53,  // slash
        0x0030 | 0x0029 => 11,  // 0
        0x0031 | 0x0021 => 2,
        0x0032 | 0x0040 => 3,
        0x0033 | 0x0023 => 4,
        0x0034 | 0x0024 => 5,
        0x0035 | 0x0025 => 6,
        0x0036 | 0x005e => 7,
        0x0037 | 0x0026 => 8,
        0x0038 | 0x002a => 9,
        0x0039 | 0x0028 => 10,
        0x003b | 0x003a => 39, // semicolon
        0x003d | 0x002b => 13, // equal
        0x005b | 0x007b => 26, // [
        0x005c | 0x007c => 43, // \
        0x005d | 0x007d => 27, // ]
        0x0060 | 0x007e => 41, // grave
        // a-z / A-Z
        0x0061 | 0x0041 => 30,
        0x0062 | 0x0042 => 48,
        0x0063 | 0x0043 => 46,
        0x0064 | 0x0044 => 32,
        0x0065 | 0x0045 => 18,
        0x0066 | 0x0046 => 33,
        0x0067 | 0x0047 => 34,
        0x0068 | 0x0048 => 35,
        0x0069 | 0x0049 => 23,
        0x006a | 0x004a => 36,
        0x006b | 0x004b => 37,
        0x006c | 0x004c => 38,
        0x006d | 0x004d => 50,
        0x006e | 0x004e => 49,
        0x006f | 0x004f => 24,
        0x0070 | 0x0050 => 25,
        0x0071 | 0x0051 => 16,
        0x0072 | 0x0052 => 19,
        0x0073 | 0x0053 => 31,
        0x0074 | 0x0054 => 20,
        0x0075 | 0x0055 => 22,
        0x0076 | 0x0056 => 47,
        0x0077 | 0x0057 => 17,
        0x0078 | 0x0058 => 45,
        0x0079 | 0x0059 => 21,
        0x007a | 0x005a => 44,
        // F1-F12
        0xffbe => 59,
        0xffbf => 60,
        0xffc0 => 61,
        0xffc1 => 62,
        0xffc2 => 63,
        0xffc3 => 64,
        0xffc4 => 65,
        0xffc5 => 66,
        0xffc6 => 67,
        0xffc7 => 68,
        0xffc8 => 87,
        0xffc9 => 88,
        _ => return None,
    };
    Some(k)
}
