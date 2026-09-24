//! Main-thread Cocoa frontend for the existing simple framebuffer and USB HID devices.

use std::collections::BTreeSet;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crate::devices::display::FramebufferConfig;
use crate::devices::usb::xhci::XhciController;
use crate::memory::GuestMemory;

struct Attached {
    memory: Arc<GuestMemory>,
    framebuffer: FramebufferConfig,
    usb: Arc<Mutex<XhciController>>,
    keys: BTreeSet<u16>,
    position: (u16, u16),
    buttons: u8,
}

static ATTACHED: Mutex<Option<Attached>> = Mutex::new(None);
static FINISHED: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    fn vmm_cocoa_run(width: u32, height: u32) -> bool;
}

pub fn run_window(width: u32, height: u32) -> bool {
    unsafe { vmm_cocoa_run(width, height) }
}

pub fn mark_finished() {
    FINISHED.store(true, Ordering::Release);
}

pub struct Attachment;

pub fn attach(
    memory: Arc<GuestMemory>,
    framebuffer: FramebufferConfig,
    usb: Arc<Mutex<XhciController>>,
) -> Attachment {
    *ATTACHED.lock().unwrap() = Some(Attached {
        memory,
        framebuffer,
        usb,
        keys: BTreeSet::new(),
        position: (0, 0),
        buttons: 0,
    });
    Attachment
}

impl Drop for Attachment {
    fn drop(&mut self) {
        release_input();
        *ATTACHED.lock().unwrap() = None;
    }
}

fn release_input() {
    if let Some(state) = ATTACHED.lock().unwrap().as_mut() {
        if let Ok(mut usb) = state.usb.lock() {
            for key in std::mem::take(&mut state.keys) {
                usb.key_event(key, false);
            }
            usb.pointer_event(state.position.0, state.position.1, 0);
        }
        state.buttons = 0;
    }
}

#[unsafe(no_mangle)]
extern "C" fn vmm_native_finished() -> bool {
    FINISHED.load(Ordering::Acquire)
}

#[unsafe(no_mangle)]
extern "C" fn vmm_native_close() {
    crate::terminal::request_shutdown();
    release_input();
}

#[unsafe(no_mangle)]
extern "C" fn vmm_native_release_input() {
    release_input();
}

#[unsafe(no_mangle)]
extern "C" fn vmm_native_geometry(width: *mut u32, height: *mut u32) -> bool {
    let lock = ATTACHED.lock().unwrap();
    let Some(state) = lock.as_ref() else {
        return false;
    };
    unsafe {
        *width = state.framebuffer.width;
        *height = state.framebuffer.height;
    }
    true
}

#[unsafe(no_mangle)]
extern "C" fn vmm_native_copy_frame(target: *mut c_void, length: usize) -> bool {
    let lock = ATTACHED.lock().unwrap();
    let Some(state) = lock.as_ref() else {
        return false;
    };
    if target.is_null() || length != state.framebuffer.pixel_bytes() {
        return false;
    }
    let pixels = unsafe { std::slice::from_raw_parts_mut(target.cast::<u8>(), length) };
    state.memory.read(state.framebuffer.gpa, pixels).is_ok()
}

#[unsafe(no_mangle)]
extern "C" fn vmm_native_key(mac_keycode: u16, down: bool) {
    let Some(code) = mac_keycode_to_linux(mac_keycode) else {
        return;
    };
    let lock = &mut *ATTACHED.lock().unwrap();
    let Some(state) = lock.as_mut() else { return };
    if down {
        state.keys.insert(code);
    } else {
        state.keys.remove(&code);
    }
    if let Ok(mut usb) = state.usb.lock() {
        usb.key_event(code, down);
    }
}

#[unsafe(no_mangle)]
extern "C" fn vmm_native_pointer(x: u16, y: u16, buttons: u8) {
    let lock = &mut *ATTACHED.lock().unwrap();
    let Some(state) = lock.as_mut() else { return };
    state.position = (x, y);
    state.buttons = buttons & 7;
    if let Ok(mut usb) = state.usb.lock() {
        usb.pointer_event(x, y, buttons);
    }
}

// macOS virtual keycodes describe physical US-layout keys. Keep the mapping
// independent of typed characters so key-up and modifier transitions match.
fn mac_keycode_to_linux(key: u16) -> Option<u16> {
    Some(match key {
        0 => 30,
        1 => 31,
        2 => 32,
        3 => 33,
        4 => 35,
        5 => 34,
        6 => 44,
        7 => 45,
        8 => 46,
        9 => 47,
        11 => 48,
        12 => 16,
        13 => 17,
        14 => 18,
        15 => 19,
        16 => 21,
        17 => 20,
        18 => 2,
        19 => 3,
        20 => 4,
        21 => 5,
        22 => 7,
        23 => 6,
        24 => 13,
        25 => 10,
        26 => 8,
        27 => 12,
        28 => 9,
        29 => 11,
        30 => 27,
        31 => 24,
        32 => 22,
        33 => 26,
        34 => 23,
        35 => 25,
        36 => 28,
        37 => 38,
        38 => 36,
        39 => 40,
        40 => 37,
        41 => 39,
        42 => 43,
        43 => 51,
        44 => 53,
        45 => 49,
        46 => 50,
        47 => 52,
        48 => 15,
        49 => 57,
        50 => 41,
        51 => 14,
        53 => 1,
        55 => 125,
        56 => 42,
        57 => 58,
        58 => 56,
        59 => 29,
        60 => 54,
        61 => 100,
        62 => 97,
        54 => 126,
        65 => 83,
        67 => 55,
        69 => 78,
        71 => 69,
        75 => 98,
        76 => 96,
        78 => 74,
        81 => 13,
        82 => 82,
        83 => 79,
        84 => 80,
        85 => 81,
        86 => 75,
        87 => 76,
        88 => 77,
        89 => 71,
        91 => 72,
        92 => 73,
        96 => 63,
        97 => 64,
        98 => 65,
        99 => 61,
        100 => 66,
        101 => 67,
        103 => 87,
        109 => 68,
        111 => 88,
        118 => 62,
        120 => 60,
        115 => 102,
        116 => 104,
        117 => 111,
        119 => 107,
        121 => 109,
        122 => 59,
        123 => 105,
        124 => 106,
        125 => 108,
        126 => 103,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_physical_keys_cover_login_and_navigation() {
        for (mac, linux) in [
            (0, 30),
            (36, 28),
            (48, 15),
            (49, 57),
            (51, 14),
            (53, 1),
            (55, 125),
            (56, 42),
            (59, 29),
            (123, 105),
            (124, 106),
        ] {
            assert_eq!(mac_keycode_to_linux(mac), Some(linux));
        }
        assert_eq!(mac_keycode_to_linux(0xffff), None);
    }

    #[test]
    fn native_frontend_copies_pixels_and_releases_held_input() {
        let memory = Arc::new(GuestMemory::new_test(0x1000, 0x4000));
        let framebuffer = FramebufferConfig {
            gpa: 0x1000,
            width: 2,
            height: 2,
            stride: 8,
            reg_size: 0x4000,
        };
        let pixels = [7u8; 16];
        memory.write(framebuffer.gpa, &pixels).unwrap();
        let usb = Arc::new(Mutex::new(XhciController::new(41, memory.clone(), 2, 2)));
        let attachment = attach(memory, framebuffer, usb.clone());
        let mut width = 0;
        let mut height = 0;
        assert!(vmm_native_geometry(&mut width, &mut height));
        assert_eq!((width, height), (2, 2));
        let mut copied = [0u8; 16];
        assert!(vmm_native_copy_frame(
            copied.as_mut_ptr().cast(),
            copied.len()
        ));
        assert_eq!(copied, pixels);
        vmm_native_key(56, true);
        vmm_native_key(0, true);
        assert_eq!(usb.lock().unwrap().keyboard_report()[0], 2);
        assert_eq!(usb.lock().unwrap().keyboard_report()[2], 4);
        vmm_native_release_input();
        assert_eq!(usb.lock().unwrap().keyboard_report(), [0; 8]);
        drop(attachment);
        assert!(!vmm_native_geometry(&mut width, &mut height));
    }
}
