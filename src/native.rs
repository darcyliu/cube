//! Cocoa runs on the process main thread. Each frame reads `Ramfb::config`
//! and drops that lock before touching guest memory. Close kicks vCPUs the
//! same way the UART shutdown hook does, and does not join the VM worker.

use std::collections::BTreeSet;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use vmm::devices::display::Ramfb;
use vmm::devices::usb::xhci::XhciController;
use vmm::memory::GuestMemory;

struct Attached {
    memory: Arc<GuestMemory>,
    fb: Arc<Ramfb>,
    usb: Arc<Mutex<XhciController>>,
    /// Absolute range passed to `XhciController::new`. The guest mode can change later.
    tablet: (u16, u16),
    keys: BTreeSet<u16>,
    position: (u16, u16),
    buttons: u8,
    /// `request_shutdown` plus `Machine::stop_cpus`. Set next to the serial open.
    stop: Option<Arc<dyn Fn() + Send + Sync>>,
}

static ATTACHED: Mutex<Option<Attached>> = Mutex::new(None);
static FINISHED: AtomicBool = AtomicBool::new(false);

fn lock_attached() -> MutexGuard<'static, Option<Attached>> {
    ATTACHED
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn lock_usb(usb: &Mutex<XhciController>) -> MutexGuard<'_, XhciController> {
    usb.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

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
    fb: Arc<Ramfb>,
    usb: Arc<Mutex<XhciController>>,
    tablet: (u16, u16),
) -> Attachment {
    *lock_attached() = Some(Attached {
        memory,
        fb,
        usb,
        tablet,
        keys: BTreeSet::new(),
        position: (0, 0),
        buttons: 0,
        stop: None,
    });
    Attachment
}

/// Registered beside the serial open, once the machine slot exists.
/// Closing the window must `hv_vcpu_interrupt`; `request_shutdown` alone
/// does not wake a vCPU blocked in `hv_vcpu_run`.
pub fn set_stop_hook(hook: Arc<dyn Fn() + Send + Sync>) {
    if let Some(state) = lock_attached().as_mut() {
        state.stop = Some(hook);
    }
}

impl Drop for Attachment {
    fn drop(&mut self) {
        release_input();
        *lock_attached() = None;
    }
}

fn release_input() {
    let (keys, x, y, usb) = {
        let mut guard = lock_attached();
        let Some(state) = guard.as_mut() else {
            return;
        };
        let keys = std::mem::take(&mut state.keys);
        state.buttons = 0;
        let cfg = state.fb.config();
        let x = scale_axis(state.position.0, cfg.width, state.tablet.0);
        let y = scale_axis(state.position.1, cfg.height, state.tablet.1);
        let usb = Arc::clone(&state.usb);
        (keys, x, y, usb)
    };
    let mut usb = lock_usb(&usb);
    for key in keys {
        usb.key_event(key, false);
    }
    usb.pointer_event(x, y, 0);
}

/// Map a pixel in the current guest mode onto the tablet range from `XhciController::new`.
fn scale_axis(pos: u16, guest: u32, tablet_max: u16) -> u16 {
    let guest = guest.max(1);
    let limit = u32::from(tablet_max.saturating_sub(1));
    let pos = u32::from(pos).min(guest.saturating_sub(1));
    if guest == 1 {
        return 0;
    }
    (pos * limit / (guest - 1)) as u16
}

#[unsafe(no_mangle)]
extern "C" fn vmm_native_finished() -> bool {
    FINISHED.load(Ordering::Acquire)
}

#[unsafe(no_mangle)]
extern "C" fn vmm_native_close() {
    let stop = lock_attached()
        .as_ref()
        .and_then(|state| state.stop.clone());
    release_input();
    if let Some(stop) = stop {
        stop();
    } else {
        vmm::terminal::request_shutdown();
    }
}

#[unsafe(no_mangle)]
extern "C" fn vmm_native_release_input() {
    release_input();
}

#[unsafe(no_mangle)]
extern "C" fn vmm_native_geometry(width: *mut u32, height: *mut u32, stride: *mut u32) -> bool {
    if width.is_null() || height.is_null() || stride.is_null() {
        return false;
    }
    let lock = lock_attached();
    let Some(state) = lock.as_ref() else {
        return false;
    };
    let cfg = state.fb.config();
    unsafe {
        *width = cfg.width;
        *height = cfg.height;
        *stride = cfg.stride;
    }
    true
}

#[unsafe(no_mangle)]
extern "C" fn vmm_native_copy_frame(target: *mut c_void, length: usize) -> bool {
    let Some((memory, cfg)) = (|| {
        let lock = lock_attached();
        let state = lock.as_ref()?;
        Some((Arc::clone(&state.memory), state.fb.config()))
    })() else {
        return false;
    };
    if target.is_null() || length != cfg.pixel_bytes() {
        return false;
    }
    let pixels = unsafe { std::slice::from_raw_parts_mut(target.cast::<u8>(), length) };
    memory.read(cfg.gpa, pixels).is_ok()
}

#[unsafe(no_mangle)]
extern "C" fn vmm_native_key(mac_keycode: u16, down: bool) {
    let Some(code) = mac_keycode_to_linux(mac_keycode) else {
        return;
    };
    let usb = {
        let mut lock = lock_attached();
        let Some(state) = lock.as_mut() else { return };
        if down {
            state.keys.insert(code);
        } else {
            state.keys.remove(&code);
        }
        Arc::clone(&state.usb)
    };
    lock_usb(&usb).key_event(code, down);
}

#[unsafe(no_mangle)]
extern "C" fn vmm_native_pointer(x: u16, y: u16, buttons: u8) {
    let (usb, sx, sy) = {
        let mut lock = lock_attached();
        let Some(state) = lock.as_mut() else { return };
        state.position = (x, y);
        // Bits 0–2 are the buttons release must clear. Bits 8 and 16 are the wheel.
        state.buttons = buttons & 7;
        let cfg = state.fb.config();
        (
            Arc::clone(&state.usb),
            scale_axis(x, cfg.width, state.tablet.0),
            scale_axis(y, cfg.height, state.tablet.1),
        )
    };
    lock_usb(&usb).pointer_event(sx, sy, buttons);
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
    use std::panic::AssertUnwindSafe;
    use std::sync::atomic::AtomicBool;

    use vmm::devices::display::descriptor_bytes;

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
    fn native_frontend_tracks_the_guest_mode_and_releases_held_input() {
        let ram = 2 * 1024 * 1024;
        let memory = Arc::new(GuestMemory::allocate(0, ram).unwrap());
        let fb = Arc::new(Ramfb::reserve(ram as u64, 2, 2).unwrap());
        let initial = fb.config();
        let pixels = vec![7u8; initial.pixel_bytes()];
        memory.write(initial.gpa, &pixels).unwrap();
        let usb = Arc::new(Mutex::new(XhciController::new(
            41,
            Arc::clone(&memory),
            8,
            4,
        )));
        let attachment = attach(
            Arc::clone(&memory),
            Arc::clone(&fb),
            Arc::clone(&usb),
            (8, 4),
        );
        let mut width = 0;
        let mut height = 0;
        let mut stride = 0;
        assert!(vmm_native_geometry(&mut width, &mut height, &mut stride));
        assert_eq!((width, height, stride), (2, 2, initial.stride));
        let mut copied = vec![0u8; initial.pixel_bytes()];
        assert!(vmm_native_copy_frame(
            copied.as_mut_ptr().cast(),
            copied.len()
        ));
        assert_eq!(copied, pixels);

        vmm_native_key(56, true);
        vmm_native_key(0, true);
        assert_eq!(usb.lock().unwrap().keyboard_report()[0], 2);
        assert_eq!(usb.lock().unwrap().keyboard_report()[2], 4);
        vmm_native_pointer(1, 1, 1);
        let report = usb.lock().unwrap().tablet_report();
        assert_eq!(u16::from_le_bytes([report[1], report[2]]), 0x7fff);
        assert_eq!(u16::from_le_bytes([report[3], report[4]]), 0x7fff);
        vmm_native_pointer(1, 1, 1 | 8);
        let report = usb.lock().unwrap().tablet_report();
        assert_eq!(report[0] & 1, 1);
        assert_eq!(report[5], 1);
        vmm_native_pointer(1, 1, 1);
        assert_eq!(usb.lock().unwrap().tablet_report()[5], 0);
        vmm_native_pointer(1, 1, 16);
        assert_eq!(usb.lock().unwrap().tablet_report()[5], 255);
        vmm_native_release_input();
        assert_eq!(usb.lock().unwrap().keyboard_report(), [0; 8]);

        let mut next = initial;
        next.gpa = 0x180000;
        next.width = 4;
        next.height = 1;
        next.stride = 32;
        fb.guest_wrote(&descriptor_bytes(&next));
        assert!(vmm_native_geometry(&mut width, &mut height, &mut stride));
        assert_eq!((width, height, stride), (4, 1, 32));
        let wide = vec![9u8; next.pixel_bytes()];
        memory.write(next.gpa, &wide).unwrap();
        assert!(!vmm_native_copy_frame(copied.as_mut_ptr().cast(), 16));
        copied.resize(wide.len(), 0);
        assert!(vmm_native_copy_frame(
            copied.as_mut_ptr().cast(),
            copied.len()
        ));
        assert_eq!(copied, wide);

        let hit = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&hit);
        set_stop_hook(Arc::new(move || flag.store(true, Ordering::SeqCst)));
        vmm_native_close();
        assert!(hit.load(Ordering::SeqCst));

        vmm_native_key(0, true);
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _guard = usb.lock().unwrap();
            panic!("poison usb");
        }));
        vmm_native_release_input();
        assert_eq!(
            usb.lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .keyboard_report(),
            [0; 8]
        );
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _guard = ATTACHED.lock().unwrap();
            panic!("poison attached");
        }));
        assert!(vmm_native_geometry(&mut width, &mut height, &mut stride));
        drop(attachment);
        assert!(!vmm_native_geometry(&mut width, &mut height, &mut stride));
    }
}
