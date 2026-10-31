//! Minimal RFB 3.8 (VNC) server for a guest framebuffer region.
//!
//! macOS Screen Sharing is picky with third-party servers; prefer TigerVNC
//! Viewer (`brew install --cask tigervnc-viewer`) if Screen Sharing drops.

use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::devices::usb::xhci::XhciController;
use crate::memory::GuestMemory;

use super::{FramebufferConfig, Ramfb, keysym};

const RFB_VERSION: &[u8] = b"RFB 003.008\n";

#[derive(Clone, Copy, Debug)]
struct PixelFormat {
    bytes: usize,
    big_endian: bool,
    max: [u16; 3],
    shift: [u8; 3],
}

impl PixelFormat {
    fn native() -> Self {
        Self {
            bytes: 4,
            big_endian: false,
            max: [255; 3],
            shift: [16, 8, 0],
        }
    }

    fn parse(p: &[u8]) -> Result<Self, String> {
        if p.len() != 16 || ![8, 16, 32].contains(&p[0]) || p[3] != 1 {
            return Err("VNC requires 8/16/32-bit true-colour pixels".into());
        }
        let format = Self {
            bytes: (p[0] / 8) as usize,
            big_endian: p[2] != 0,
            max: [
                u16::from_be_bytes([p[4], p[5]]),
                u16::from_be_bytes([p[6], p[7]]),
                u16::from_be_bytes([p[8], p[9]]),
            ],
            shift: [p[10], p[11], p[12]],
        };
        let mut mask = 0u32;
        for i in 0..3 {
            let max = format.max[i] as u32;
            let bits = 32 - max.leading_zeros();
            if max == 0 || max & (max + 1) != 0 || bits + format.shift[i] as u32 > p[0] as u32 {
                return Err("Invalid VNC colour mask".into());
            }
            let channel = max << format.shift[i];
            if mask & channel != 0 {
                return Err("Overlapping VNC colour masks".into());
            }
            mask |= channel;
        }
        Ok(format)
    }

    fn encode_row(&self, pixels: &[u8], out: &mut Vec<u8>) {
        out.clear();
        for pixel in pixels.chunks_exact(4) {
            let rgb = [pixel[2], pixel[1], pixel[0]];
            let mut value = 0u32;
            for (channel, (max, shift)) in rgb.iter().zip(self.max.iter().zip(&self.shift)) {
                value |= ((*channel as u32 * *max as u32 + 127) / 255) << *shift;
            }
            if self.big_endian {
                out.extend_from_slice(&value.to_be_bytes()[4 - self.bytes..]);
            } else {
                out.extend_from_slice(&value.to_le_bytes()[..self.bytes]);
            }
        }
    }
}

pub struct VncServer {
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Drop for VncServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let Some(worker) = self.worker.take() else {
            return;
        };
        // Joining the worker from the worker thread deadlocks.
        if worker.thread().id() == thread::current().id() {
            return;
        }
        let _ = worker.join();
    }
}

/// A session owns its pressed keys. Drop releases them even after protocol errors.
struct SessionInput {
    keys: BTreeSet<u16>,
    position: (u16, u16),
    usb: Option<Arc<Mutex<XhciController>>>,
}

impl SessionInput {
    fn key(&mut self, code: u16, down: bool) {
        if down {
            self.keys.insert(code);
        } else {
            self.keys.remove(&code);
        }
        let Some(usb) = &self.usb else {
            return;
        };
        // Poisoned lock: keep delivering. Do not touch PciHost here.
        lock_xhci(usb).key_event(code, down);
    }

    fn pointer(&mut self, x: u16, y: u16, buttons: u8) {
        self.position = (x, y);
        let Some(usb) = &self.usb else {
            return;
        };
        lock_xhci(usb).pointer_event(x, y, buttons);
    }
}

impl Drop for SessionInput {
    fn drop(&mut self) {
        for code in self.keys.clone() {
            self.key(code, false);
        }
        self.pointer(self.position.0, self.position.1, 0);
    }
}

fn lock_xhci(usb: &Mutex<XhciController>) -> std::sync::MutexGuard<'_, XhciController> {
    usb.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// One session at a time. The listener is loopback-only and unauthenticated.
pub fn spawn(
    mem: Arc<GuestMemory>,
    fb: Arc<Ramfb>,
    bind: SocketAddr,
    usb: Option<Arc<Mutex<XhciController>>>,
) -> Result<VncServer, String> {
    if !bind.ip().is_loopback() {
        return Err(
            "VNC without authentication must bind to loopback; use an SSH tunnel for remote access"
                .into(),
        );
    }
    let listener = TcpListener::bind(bind).map_err(|e| format!("VNC bind {bind}: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("VNC set blocking: {e}"))?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop_thr = Arc::clone(&stop);
    let mem_thr = Arc::clone(&mem);
    let fb_thr = Arc::clone(&fb);
    let usb_thr = usb.clone();

    let worker = thread::Builder::new()
        .name("vmm-vnc".into())
        .spawn(move || {
            let cfg = fb_thr.config();
            crate::host_log!(
                "[VNC] Listening on {bind} ({}x{}, stride {})",
                cfg.width,
                cfg.height,
                cfg.stride
            );
            crate::host_log!("[VNC] Connect with: open vnc://{bind}");
            crate::host_log!(
                "[VNC] Tip: if macOS Screen Sharing drops, use TigerVNC Viewer (brew install --cask tigervnc-viewer)"
            );
            while !stop_thr.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, peer)) => {
                        crate::host_log!("[VNC] Client connected from {peer}");
                        let mem_c = Arc::clone(&mem_thr);
                        let fb_c = Arc::clone(&fb_thr);
                        let stop_c = Arc::clone(&stop_thr);
                        let usb_c = usb_thr.clone();
                        if let Err(e) = handle_client(stream, mem_c, fb_c, stop_c, usb_c) {
                            crate::host_log!("[VNC] Client session ended: {e}");
                        }
                        crate::host_log!("[VNC] Client disconnected");
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(25));
                    }
                    Err(e) => {
                        crate::host_log!("[VNC] accept error: {e}");
                        thread::sleep(Duration::from_millis(200));
                    }
                }
            }
        })
        .map_err(|e| format!("spawn VNC thread: {e}"))?;

    Ok(VncServer {
        stop,
        worker: Some(worker),
    })
}

fn handle_client(
    mut stream: TcpStream,
    mem: Arc<GuestMemory>,
    ramfb: Arc<Ramfb>,
    stop: Arc<AtomicBool>,
    usb: Option<Arc<Mutex<XhciController>>>,
) -> Result<(), String> {
    let mut input = SessionInput {
        keys: BTreeSet::new(),
        position: (0, 0),
        usb,
    };
    stream.set_nonblocking(false).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_millis(250)))
        .ok();
    stream.set_write_timeout(Some(Duration::from_secs(2))).ok();
    stream.set_nodelay(true).ok();

    stream
        .write_all(RFB_VERSION)
        .map_err(|e| format!("send version: {e}"))?;
    stream.flush().ok();

    let mut ver = [0u8; 12];
    read_exact_timeout(&mut stream, &mut ver, &stop)
        .map_err(|e| format!("read client version: {e}"))?;
    let ver_str = String::from_utf8_lossy(&ver);
    crate::device_trace!("[VNC] Client version: {}", ver_str.trim());

    if !matches!(&ver, b"RFB 003.003\n" | b"RFB 003.007\n" | b"RFB 003.008\n") {
        return Err("Unsupported RFB version".into());
    }
    let minor = ver[10] - b'0';
    let use_v3_3 = minor <= 3;

    if use_v3_3 {
        stream
            .write_all(&1u32.to_be_bytes())
            .map_err(|e| format!("send security(3.3): {e}"))?;
        stream.flush().ok();
        crate::device_trace!("[VNC] Negotiated RFB 3.3-style security None");
    } else {
        stream
            .write_all(&[1u8, 1u8])
            .map_err(|e| format!("send security types: {e}"))?;
        stream.flush().ok();

        let mut chosen = [0u8; 1];
        read_exact_timeout(&mut stream, &mut chosen, &stop)
            .map_err(|e| format!("read security choice: {e}"))?;
        if chosen[0] != 1 {
            let _ = stream.write_all(&1u32.to_be_bytes());
            return Err(format!("unsupported security {}", chosen[0]));
        }
        if minor >= 8 {
            stream
                .write_all(&0u32.to_be_bytes())
                .map_err(|e| format!("send SecurityResult: {e}"))?;
        }
        stream.flush().ok();
        crate::device_trace!("[VNC] Negotiated security None (3.8)");
    }

    let mut shared = [0u8; 1];
    read_exact_timeout(&mut stream, &mut shared, &stop)
        .map_err(|e| format!("read ClientInit: {e}"))?;
    crate::device_trace!("[VNC] ClientInit shared={}", shared[0]);

    // Size is whatever the guest has published at connect time. Later updates
    // re-read the descriptor but stay inside this rectangle.
    let announced = ramfb.config();
    let mut init = Vec::with_capacity(32);
    init.extend_from_slice(&(announced.width as u16).to_be_bytes());
    init.extend_from_slice(&(announced.height as u16).to_be_bytes());
    init.push(32);
    init.push(24);
    init.push(0);
    init.push(1);
    init.extend_from_slice(&255u16.to_be_bytes());
    init.extend_from_slice(&255u16.to_be_bytes());
    init.extend_from_slice(&255u16.to_be_bytes());
    init.push(16);
    init.push(8);
    init.push(0);
    init.extend_from_slice(&[0, 0, 0]);
    let name = b"vmm ramfb";
    init.extend_from_slice(&(name.len() as u32).to_be_bytes());
    init.extend_from_slice(name);
    stream
        .write_all(&init)
        .map_err(|e| format!("send ServerInit: {e}"))?;
    stream.flush().ok();
    crate::device_trace!(
        "[VNC] ServerInit sent ({}x{}), waiting for FramebufferUpdateRequest",
        announced.width,
        announced.height
    );

    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .ok();

    let mut pixel_format = PixelFormat::native();
    let mut session_fb = announced;
    let mut fb_buf = vec![0u8; session_fb.pixel_bytes()];
    let mut previous = vec![0u8; session_fb.pixel_bytes()];
    let mut have_full_frame = false;
    let mut msg = [0u8; 1];
    let mut metrics = SessionMetrics::new();

    while !stop.load(Ordering::Relaxed) {
        match stream.read(&mut msg) {
            Ok(0) => return Err("client closed".into()),
            Ok(_) => {}
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(e) => return Err(e.to_string()),
        }

        match msg[0] {
            0 => {
                let mut rest = [0u8; 19];
                read_exact_timeout(&mut stream, &mut rest, &stop)?;
                pixel_format = PixelFormat::parse(&rest[3..])?;
                have_full_frame = false;
            }
            2 => {
                let mut hdr = [0u8; 3];
                read_exact_timeout(&mut stream, &mut hdr, &stop)?;
                let n = u16::from_be_bytes([hdr[1], hdr[2]]) as usize;
                if n > 0 {
                    let mut enc = vec![0u8; n * 4];
                    read_exact_timeout(&mut stream, &mut enc, &stop)?;
                }
            }
            3 => {
                let mut rest = [0u8; 9];
                read_exact_timeout(&mut stream, &mut rest, &stop)?;
                metrics.requests += 1;
                let current = ramfb.config();
                if current != session_fb {
                    session_fb = current;
                    fb_buf.resize(session_fb.pixel_bytes(), 0);
                    previous.resize(session_fb.pixel_bytes(), 0);
                    have_full_frame = false;
                }
                let read_started = Instant::now();
                // Config lock is not held across this copy or the socket write.
                mem.read(session_fb.gpa, &mut fb_buf)
                    .map_err(|e| format!("FB read: {e}"))?;
                metrics.read_time += read_started.elapsed();
                metrics.read_bytes += session_fb.pixel_bytes() as u64;
                let mut clip = session_fb;
                clip.width = session_fb.width.min(announced.width);
                clip.height = session_fb.height.min(announced.height);
                let requested = Rect::from_request(&rest, clip);
                let compare = rest[0] != 0 && have_full_frame;
                if compare {
                    metrics.compared_bytes +=
                        u64::from(requested.width) * u64::from(requested.height) * 4;
                }
                let diff_started = Instant::now();
                let rect = if compare {
                    changed_rect(requested, session_fb.stride as usize, &fb_buf, &previous)
                } else {
                    requested
                };
                metrics.diff_time += diff_started.elapsed();
                let send_started = Instant::now();
                metrics.sent_bytes +=
                    send_raw_update(&mut stream, session_fb, &fb_buf, pixel_format, rect)? as u64;
                metrics.send_time += send_started.elapsed();
                metrics.sent_pixels += u64::from(rect.width) * u64::from(rect.height);
                for y in rect.y..rect.y + rect.height {
                    let start = y as usize * session_fb.stride as usize + rect.x as usize * 4;
                    let end = start + rect.width as usize * 4;
                    previous[start..end].copy_from_slice(&fb_buf[start..end]);
                }
                if rect.x == 0
                    && rect.y == 0
                    && rect.width == clip.width as u16
                    && rect.height == clip.height as u16
                {
                    have_full_frame = true;
                }
                thread::sleep(Duration::from_millis(16));
            }
            4 => {
                // KeyEvent: down(u8), pad(u16), keysym(u32 BE)
                let mut rest = [0u8; 7];
                read_exact_timeout(&mut stream, &mut rest, &stop)?;
                let down = rest[0] != 0;
                let keysym_val = u32::from_be_bytes([rest[3], rest[4], rest[5], rest[6]]);
                if let Some(code) = keysym::keysym_to_linux(keysym_val) {
                    input.key(code, down);
                } else {
                    crate::device_trace!("[VNC] unmapped keysym {keysym_val:#x}");
                }
            }
            5 => {
                // PointerEvent: mask(u8), x(u16 BE), y(u16 BE)
                let mut rest = [0u8; 5];
                read_exact_timeout(&mut stream, &mut rest, &stop)?;
                let buttons = rest[0];
                let x = u16::from_be_bytes([rest[1], rest[2]]);
                let y = u16::from_be_bytes([rest[3], rest[4]]);
                input.pointer(x, y, buttons);
            }
            255 => {
                // QEMU extension: subtype(u8), down(u16), keysym(u32), keycode(u32).
                let mut rest = [0u8; 11];
                read_exact_timeout(&mut stream, &mut rest, &stop)?;
                if rest[0] != 0 {
                    return Err("Unsupported QEMU VNC subtype".into());
                }
                let down = u16::from_be_bytes([rest[1], rest[2]]) != 0;
                let keysym_val = u32::from_be_bytes([rest[3], rest[4], rest[5], rest[6]]);
                // QEMU keycodes are XT scancodes, not Linux evdev codes.
                match keysym::keysym_to_linux(keysym_val) {
                    Some(code) => input.key(code, down),
                    None => {
                        crate::device_trace!("[VNC] unmapped ext keysym {keysym_val:#x}");
                    }
                }
            }
            6 => {
                let mut hdr = [0u8; 7];
                read_exact_timeout(&mut stream, &mut hdr, &stop)?;
                let len = u32::from_be_bytes([hdr[3], hdr[4], hdr[5], hdr[6]]) as usize;
                if len >= 1_000_000 {
                    return Err("ClientCutText too large".into());
                }
                if len > 0 {
                    let mut text = vec![0u8; len];
                    read_exact_timeout(&mut stream, &mut text, &stop)?;
                }
            }
            other => return Err(format!("Unsupported VNC message type {other}")),
        }
    }
    Ok(())
}

struct SessionMetrics {
    started: Instant,
    requests: u64,
    read_bytes: u64,
    compared_bytes: u64,
    sent_pixels: u64,
    sent_bytes: u64,
    read_time: Duration,
    diff_time: Duration,
    send_time: Duration,
}

impl SessionMetrics {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            requests: 0,
            read_bytes: 0,
            compared_bytes: 0,
            sent_pixels: 0,
            sent_bytes: 0,
            read_time: Duration::ZERO,
            diff_time: Duration::ZERO,
            send_time: Duration::ZERO,
        }
    }
}

impl Drop for SessionMetrics {
    fn drop(&mut self) {
        let seconds = self.started.elapsed().as_secs_f64();
        crate::host_log!(
            "[VNC] Session metrics: requests={}, read={:.1} MiB/{:.1} ms, compared={:.1} MiB/{:.1} ms, sent_pixels={}, wire={:.1} MiB/{:.1} ms, elapsed={:.1} s",
            self.requests,
            self.read_bytes as f64 / (1024.0 * 1024.0),
            self.read_time.as_secs_f64() * 1000.0,
            self.compared_bytes as f64 / (1024.0 * 1024.0),
            self.diff_time.as_secs_f64() * 1000.0,
            self.sent_pixels,
            self.sent_bytes as f64 / (1024.0 * 1024.0),
            self.send_time.as_secs_f64() * 1000.0,
            seconds
        );
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Rect {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
}

impl Rect {
    fn from_request(request: &[u8; 9], fb: FramebufferConfig) -> Self {
        let x = u16::from_be_bytes([request[1], request[2]]).min(fb.width as u16);
        let y = u16::from_be_bytes([request[3], request[4]]).min(fb.height as u16);
        let width = u16::from_be_bytes([request[5], request[6]]).min(fb.width as u16 - x);
        let height = u16::from_be_bytes([request[7], request[8]]).min(fb.height as u16 - y);
        Self {
            x,
            y,
            width,
            height,
        }
    }
}

fn changed_rect(rect: Rect, stride: usize, pixels: &[u8], previous: &[u8]) -> Rect {
    let mut top = rect.y + rect.height;
    let mut bottom = rect.y;
    for y in rect.y..rect.y + rect.height {
        let start = y as usize * stride + rect.x as usize * 4;
        let end = start + rect.width as usize * 4;
        if pixels[start..end] != previous[start..end] {
            top = top.min(y);
            bottom = y + 1;
        }
    }
    if bottom <= top {
        Rect {
            width: 0,
            height: 0,
            ..rect
        }
    } else {
        Rect {
            y: top,
            height: bottom - top,
            ..rect
        }
    }
}

fn send_raw_update(
    stream: &mut TcpStream,
    fb: FramebufferConfig,
    pixels: &[u8],
    format: PixelFormat,
    rect: Rect,
) -> Result<usize, String> {
    if rect.width == 0 || rect.height == 0 {
        stream.write_all(&[0, 0, 0, 0]).map_err(|e| e.to_string())?;
        return Ok(4);
    }
    let mut hdr = Vec::with_capacity(16);
    hdr.push(0);
    hdr.push(0);
    hdr.extend_from_slice(&1u16.to_be_bytes());
    hdr.extend_from_slice(&rect.x.to_be_bytes());
    hdr.extend_from_slice(&rect.y.to_be_bytes());
    hdr.extend_from_slice(&rect.width.to_be_bytes());
    hdr.extend_from_slice(&rect.height.to_be_bytes());
    hdr.extend_from_slice(&0i32.to_be_bytes());
    stream.write_all(&hdr).map_err(|e| e.to_string())?;

    let row = rect.width as usize * 4;
    let stride = fb.stride as usize;
    let mut encoded = Vec::with_capacity(fb.width as usize * format.bytes);
    for y in rect.y as usize..(rect.y + rect.height) as usize {
        let start = y * stride + rect.x as usize * 4;
        let end = start + row;
        if end > pixels.len() {
            return Err("FB buffer shorter than geometry".into());
        }
        format.encode_row(&pixels[start..end], &mut encoded);
        stream.write_all(&encoded).map_err(|e| e.to_string())?;
    }
    stream.flush().map_err(|e| e.to_string())?;
    Ok(16 + rect.width as usize * rect.height as usize * format.bytes)
}

fn read_exact_timeout(
    stream: &mut TcpStream,
    buf: &mut [u8],
    stop: &AtomicBool,
) -> Result<(), String> {
    let mut got = 0;
    let deadline = Instant::now() + Duration::from_secs(10);
    while got < buf.len() {
        if Instant::now() >= deadline {
            return Err("VNC message/handshake timed out".into());
        }
        if stop.load(Ordering::Relaxed) {
            return Err("stopped".into());
        }
        match stream.read(&mut buf[got..]) {
            Ok(0) => return Err("client closed".into()),
            Ok(n) => got += n,
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                continue;
            }
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_loopback_bind() {
        let mem = Arc::new(GuestMemory::allocate(0, 8 * 1024 * 1024).unwrap());
        let fb = Arc::new(Ramfb::reserve(8 * 1024 * 1024, 64, 64).unwrap());
        let addr = "0.0.0.0:9".parse().unwrap();
        let Err(err) = spawn(mem, fb, addr, None) else {
            panic!("non-loopback bind should fail");
        };
        assert!(err.contains("loopback"), "{err}");
    }

    #[test]
    fn clips_requested_rectangle_and_sends_only_changed_rows() {
        let fb = FramebufferConfig {
            width: 4,
            height: 4,
            stride: 16,
            gpa: 0,
            reg_size: 64,
        };
        let request = [1, 0, 2, 0, 1, 0, 9, 0, 9];
        let rect = Rect::from_request(&request, fb);
        assert_eq!(
            rect,
            Rect {
                x: 2,
                y: 1,
                width: 2,
                height: 3
            }
        );
        let old = [0; 64];
        let mut pixels = old;
        pixels[2 * 16 + 2 * 4] = 255;
        assert_eq!(
            changed_rect(rect, 16, &pixels, &old),
            Rect {
                x: 2,
                y: 2,
                width: 2,
                height: 1
            }
        );
        assert_eq!(changed_rect(rect, 16, &pixels, &pixels).height, 0);
    }

    #[test]
    fn negotiated_pixel_formats_change_wire_bytes() {
        let mut out = Vec::new();
        let rgb565 = [16, 16, 0, 1, 0, 31, 0, 63, 0, 31, 11, 5, 0, 0, 0, 0];
        let mut format = PixelFormat::parse(&rgb565).unwrap();
        format.encode_row(&[0, 0, 255, 0, 0, 255, 0, 0], &mut out);
        assert_eq!(out, [0, 248, 224, 7]);
        format.big_endian = true;
        format.encode_row(&[0, 0, 255, 0], &mut out);
        assert_eq!(out, [248, 0]);
        PixelFormat::native().encode_row(&[3, 2, 1, 255], &mut out);
        assert_eq!(out, [3, 2, 1, 0]);
    }

    #[test]
    fn rejects_invalid_shifts_and_indexed_colour() {
        let mut p = [32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0];
        p[10] = 32;
        assert!(PixelFormat::parse(&p).is_err());
        p[10] = 16;
        p[3] = 0;
        assert!(PixelFormat::parse(&p).is_err());
    }
}
