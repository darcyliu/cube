//! POSIX terminal raw-mode manager for interactive console input.

use std::sync::atomic::{AtomicBool, Ordering};

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Termios {
    c_iflag: usize,
    c_oflag: usize,
    c_cflag: usize,
    c_lflag: usize,
    c_cc: [u8; 20],
    c_ispeed: usize,
    c_ospeed: usize,
}

const TCSANOW: i32 = 0;

unsafe extern "C" {
    fn isatty(fd: i32) -> i32;
    fn tcgetattr(fd: i32, termios_p: *mut Termios) -> i32;
    fn tcsetattr(fd: i32, optional_actions: i32, termios_p: *const Termios) -> i32;
    fn cfmakeraw(termios_p: *mut Termios);
}

static RAW_MODE_ACTIVE: AtomicBool = AtomicBool::new(false);

pub struct TerminalGuard {
    orig_termios: Termios,
    is_tty: bool,
}

impl TerminalGuard {
    pub fn enter_raw_mode() -> Self {
        let is_tty = unsafe { isatty(0) == 1 };
        let mut orig_termios = Termios::default();

        if is_tty {
            unsafe {
                if tcgetattr(0, &mut orig_termios) == 0 {
                    let mut raw = orig_termios;
                    cfmakeraw(&mut raw);
                    tcsetattr(0, TCSANOW, &raw);
                    RAW_MODE_ACTIVE.store(true, Ordering::SeqCst);
                }
            }
        }

        Self {
            orig_termios,
            is_tty,
        }
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        if self.is_tty && RAW_MODE_ACTIVE.swap(false, Ordering::SeqCst) {
            unsafe {
                tcsetattr(0, TCSANOW, &self.orig_termios);
            }
        }
    }
}
