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
    fn ioctl(fd: i32, request: u64, ...) -> i32;
    fn signal(sig: i32, func: extern "C" fn(i32)) -> usize;
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WinSize {
    pub ws_row: u16,
    pub ws_col: u16,
    pub ws_xpixel: u16,
    pub ws_ypixel: u16,
}

impl Default for WinSize {
    fn default() -> Self {
        Self {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        }
    }
}

pub const TIOCGWINSZ: u64 = 0x40087468;
pub const SIGWINCH: i32 = 28;

static RAW_MODE_ACTIVE: AtomicBool = AtomicBool::new(false);
static WINCH_PENDING: AtomicBool = AtomicBool::new(false);

extern "C" fn sigwinch_handler(_sig: i32) {
    WINCH_PENDING.store(true, Ordering::SeqCst);
}

/// Query current host terminal window dimensions via TIOCGWINSZ.
/// Falls back to 80x24 if not connected to a TTY or ioctl fails.
pub fn get_terminal_size() -> WinSize {
    let mut ws = WinSize::default();
    unsafe {
        if isatty(0) == 1 {
            let res = ioctl(0, TIOCGWINSZ, &mut ws);
            if res != 0 || ws.ws_col == 0 || ws.ws_row == 0 {
                return WinSize::default();
            }
        }
    }
    ws
}

/// Install the SIGWINCH signal handler to detect terminal window resize events.
pub fn install_sigwinch_handler() {
    unsafe {
        signal(SIGWINCH, sigwinch_handler);
    }
}

/// Check if a window resize event has occurred since last check.
/// If yes, clears the flag and returns the new terminal dimensions.
pub fn check_and_clear_resize() -> Option<WinSize> {
    if WINCH_PENDING.swap(false, Ordering::SeqCst) {
        Some(get_terminal_size())
    } else {
        None
    }
}

pub struct TerminalGuard {
    orig_termios: Termios,
    is_tty: bool,
}

impl TerminalGuard {
    pub fn enter_raw_mode() -> Self {
        let is_tty = unsafe { isatty(0) == 1 };
        let mut orig_termios = Termios::default();

        if is_tty {
            install_sigwinch_handler();
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
