//! Minimal RFB 3.8 (VNC) server for a guest simple-framebuffer region.
//!
//! macOS Screen Sharing is picky with third-party servers; prefer TigerVNC
//! Viewer (`brew install --cask tigervnc-viewer`) if Screen Sharing drops.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::devices::usb::xhci::XhciController;
use crate::devices::virtio::VirtioInput;
use crate::memory::GuestMemory;

use super::{FramebufferConfig, keysym};

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
            for i in 0..3 {
                value |= ((rgb[i] as u32 * self.max[i] as u32 + 127) / 255) << self.shift[i];
            }
            if self.big_endian {
                out.extend_from_slice(&value.to_be_bytes()[4 - self.bytes..]);
            } else {
                out.extend_from_slice(&value.to_le_bytes()[..self.bytes]);
            }
        }
    }
}

/// Start a background VNC server. Returns a stop flag (set true to request shutdown).
pub fn spawn(
    mem: Arc<GuestMemory>,
    fb: FramebufferConfig,
    bind: SocketAddr,
    keyboard: Option<Arc<Mutex<VirtioInput>>>,
    tablet: Option<Arc<Mutex<VirtioInput>>>,
    usb: Option<Arc<Mutex<XhciController>>>,
) -> Result<Arc<AtomicBool>, String> {
    let listener = TcpListener::bind(bind).map_err(|e| format!("VNC bind {bind}: {e}"))?;
    listener
        .set_nonblocking(false)
        .map_err(|e| format!("VNC set blocking: {e}"))?;

    let stop = Arc::new(AtomicBool::new(false));
    let stop_thr = Arc::clone(&stop);
    let mem_thr = Arc::clone(&mem);
    let keyboard_thr = keyboard.clone();
    let tablet_thr = tablet.clone();
    let usb_thr = usb.clone();

    thread::Builder::new()
        .name("vmm-vnc".into())
        .spawn(move || {
            crate::host_log!(
                "[VNC] Listening on {bind} ({}x{}, stride {})",
                fb.width, fb.height, fb.stride
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
                        let keyboard_c = keyboard_thr.clone();
                        let tablet_c = tablet_thr.clone();
                        let usb_c = usb_thr.clone();
                        let stop_c = Arc::clone(&stop_thr);
                        let _ = thread::Builder::new()
                            .name("vmm-vnc-client".into())
                            .spawn(move || {
                                if let Err(e) = handle_client(stream, mem_c, fb, stop_c, keyboard_c, tablet_c, usb_c) {
                                    crate::host_log!("[VNC] Client session ended: {e}");
                                } else {
                                    crate::host_log!("[VNC] Client disconnected cleanly");
                                }
                            });
                    }
                    Err(e) => {
                        crate::host_log!("[VNC] accept error: {e}");
                        thread::sleep(Duration::from_millis(200));
                    }
                }
            }
        })
        .map_err(|e| format!("spawn VNC thread: {e}"))?;

    Ok(stop)
}

fn handle_client(
    mut stream: TcpStream,
    mem: Arc<GuestMemory>,
    fb: FramebufferConfig,
    stop: Arc<AtomicBool>,
    keyboard: Option<Arc<Mutex<VirtioInput>>>,
    tablet: Option<Arc<Mutex<VirtioInput>>>,
    usb: Option<Arc<Mutex<XhciController>>>,
) -> Result<(), String> {
    // Longer timeouts for the handshake — Screen Sharing can be slow.
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok();
    stream.set_write_timeout(Some(Duration::from_secs(10))).ok();
    stream.set_nodelay(true).ok();

    // ProtocolVersion
    stream
        .write_all(RFB_VERSION)
        .map_err(|e| format!("send version: {e}"))?;
    stream.flush().ok();

    let mut ver = [0u8; 12];
    read_exact_timeout(&mut stream, &mut ver, &stop)
        .map_err(|e| format!("read client version: {e}"))?;
    let ver_str = String::from_utf8_lossy(&ver);
    crate::device_trace!("[VNC] Client version: {}", ver_str.trim());

    // Parse minor version: "RFB 003.00X\n"
    let minor = ver.get(10).copied().unwrap_or(b'8').saturating_sub(b'0');
    let use_v3_3 = minor <= 3;

    if use_v3_3 {
        // RFB 3.3: server picks security type as u32 BE; None=1, no SecurityResult.
        stream
            .write_all(&1u32.to_be_bytes())
            .map_err(|e| format!("send security(3.3): {e}"))?;
        stream.flush().ok();
        crate::device_trace!("[VNC] Negotiated RFB 3.3-style security None");
    } else {
        // RFB 3.7+: list of types, client chooses, then SecurityResult for None.
        stream
            .write_all(&[1u8, 1u8]) // 1 type, None
            .map_err(|e| format!("send security types: {e}"))?;
        stream.flush().ok();

        let mut chosen = [0u8; 1];
        read_exact_timeout(&mut stream, &mut chosen, &stop)
            .map_err(|e| format!("read security choice: {e}"))?;
        if chosen[0] != 1 {
            // Tell client we failed, then bail.
            let _ = stream.write_all(&1u32.to_be_bytes());
            return Err(format!("unsupported security {}", chosen[0]));
        }
        // Security None has no SecurityResult in RFB 3.7.
        if minor >= 8 {
            stream
                .write_all(&0u32.to_be_bytes())
                .map_err(|e| format!("send SecurityResult: {e}"))?;
        }
        stream.flush().ok();
        crate::device_trace!("[VNC] Negotiated security None (3.8)");
    }

    // ClientInit (shared-flag)
    let mut shared = [0u8; 1];
    read_exact_timeout(&mut stream, &mut shared, &stop)
        .map_err(|e| format!("read ClientInit: {e}"))?;
    crate::device_trace!("[VNC] ClientInit shared={}", shared[0]);

    // ServerInit
    let mut init = Vec::with_capacity(32);
    init.extend_from_slice(&(fb.width as u16).to_be_bytes());
    init.extend_from_slice(&(fb.height as u16).to_be_bytes());
    // 32bpp little-endian true-colour; shifts for LE 0xAARRGGBB (a8r8g8b8)
    init.push(32); // bits-per-pixel
    init.push(24); // depth
    init.push(0); // big-endian-flag
    init.push(1); // true-colour-flag
    init.extend_from_slice(&255u16.to_be_bytes()); // red-max
    init.extend_from_slice(&255u16.to_be_bytes());
    init.extend_from_slice(&255u16.to_be_bytes());
    init.push(16); // red-shift
    init.push(8); // green-shift
    init.push(0); // blue-shift
    init.extend_from_slice(&[0, 0, 0]); // padding
    let name = b"vmm simplefb";
    init.extend_from_slice(&(name.len() as u32).to_be_bytes());
    init.extend_from_slice(name);
    stream
        .write_all(&init)
        .map_err(|e| format!("send ServerInit: {e}"))?;
    stream.flush().ok();
    crate::device_trace!(
        "[VNC] ServerInit sent ({}x{}), waiting for FramebufferUpdateRequest",
        fb.width, fb.height
    );

    // Steady-state: shorter poll timeout so we can notice stop flag.
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .ok();

    let mut pixel_format = PixelFormat::native();
    let mut fb_buf = vec![0u8; fb.pixel_bytes()];
    let mut msg = [0u8; 1];

    while !stop.load(Ordering::Relaxed) {
        match stream.read(&mut msg) {
            Ok(0) => return Err("client closed".into()),
            Ok(_) => {}
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                // Drain pending HID / virtio-input completions.
                if let Some(ref u) = usb {
                    if let Ok(mut g) = u.lock() {
                        g.poll_flush();
                    }
                }
                if let Some(ref kbd) = keyboard {
                    if let Ok(mut g) = kbd.lock() {
                        g.poll_flush();
                    }
                }
                if let Some(ref tab) = tablet {
                    if let Ok(mut g) = tab.lock() {
                        g.poll_flush();
                    }
                }
                continue;
            }
            Err(e) => return Err(e.to_string()),
        }

        match msg[0] {
            0 => {
                // Honour the client format: raw payload length depends on its bpp.
                let mut rest = [0u8; 19];
                read_exact_timeout(&mut stream, &mut rest, &stop)?;
                pixel_format = PixelFormat::parse(&rest[3..])?;
            }
            2 => {
                // SetEncodings — we only do raw (0); ignore the list
                let mut hdr = [0u8; 3];
                read_exact_timeout(&mut stream, &mut hdr, &stop)?;
                let n = u16::from_be_bytes([hdr[1], hdr[2]]) as usize;
                if n > 0 {
                    let mut enc = vec![0u8; n * 4];
                    read_exact_timeout(&mut stream, &mut enc, &stop)?;
                }
            }
            3 => {
                // FramebufferUpdateRequest
                let mut rest = [0u8; 9];
                read_exact_timeout(&mut stream, &mut rest, &stop)?;
                mem.read(fb.gpa, &mut fb_buf)
                    .map_err(|e| format!("FB read: {e}"))?;
                send_full_raw_update(&mut stream, fb, &fb_buf, pixel_format)?;
            }
            4 => {
                // KeyEvent: down(u8), pad(u16), keysym(u32 BE)
                let mut rest = [0u8; 7];
                read_exact_timeout(&mut stream, &mut rest, &stop)?;
                let down = rest[0] != 0;
                let keysym_val = u32::from_be_bytes([rest[3], rest[4], rest[5], rest[6]]);
                crate::device_trace!(
                    "[VNC] KeyEvent keysym={keysym_val:#x} down={down} usb={} kbd={}",
                    usb.is_some(),
                    keyboard.is_some()
                );
                if let Some(code) = keysym::keysym_to_linux(keysym_val) {
                    if let Some(ref u) = usb {
                        if let Ok(mut g) = u.lock() {
                            g.key_event(code, down);
                        }
                    } else if let Some(ref kbd) = keyboard {
                        if let Ok(mut g) = kbd.lock() {
                            g.key_event(code, down);
                        }
                    }
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
                if let Some(ref u) = usb {
                    if let Ok(mut g) = u.lock() {
                        g.pointer_event(x, y, buttons);
                    }
                } else if let Some(ref tab) = tablet {
                    if let Ok(mut g) = tab.lock() {
                        g.pointer_event(x, y, buttons);
                    }
                }
            }
            255 => {
                // QEMU Extended Key Event: down(u8), pad(u16), keysym(u32), keycode(u32)
                let mut rest = [0u8; 11];
                read_exact_timeout(&mut stream, &mut rest, &stop)?;
                let down = rest[0] != 0;
                let keysym_val = u32::from_be_bytes([rest[3], rest[4], rest[5], rest[6]]);
                let keycode = u32::from_be_bytes([rest[7], rest[8], rest[9], rest[10]]);
                crate::device_trace!("[VNC] ExtKey keysym={keysym_val:#x} keycode={keycode} down={down}");
                let code = keysym::keysym_to_linux(keysym_val).or_else(|| {
                    if keycode != 0 {
                        Some(keycode as u16)
                    } else {
                        None
                    }
                });
                match code {
                    Some(code) => {
                        if let Some(ref u) = usb {
                            if let Ok(mut g) = u.lock() {
                                g.key_event(code, down);
                            }
                        } else if let Some(ref kbd) = keyboard {
                            if let Ok(mut g) = kbd.lock() {
                                g.key_event(code, down);
                            }
                        }
                    }
                    None => crate::device_trace!("[VNC] unmapped ext keysym {keysym_val:#x}"),
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
            other => {
                // Don't kill the session on unknown types Apple might send.
                return Err(format!("Unsupported VNC message type {other}"));
            }
        }
    }
    Ok(())
}

fn send_full_raw_update(
    stream: &mut TcpStream,
    fb: FramebufferConfig,
    pixels: &[u8],
    format: PixelFormat,
) -> Result<(), String> {
    let mut hdr = Vec::with_capacity(16);
    hdr.push(0); // FramebufferUpdate
    hdr.push(0); // padding
    hdr.extend_from_slice(&1u16.to_be_bytes());
    hdr.extend_from_slice(&0u16.to_be_bytes());
    hdr.extend_from_slice(&0u16.to_be_bytes());
    hdr.extend_from_slice(&(fb.width as u16).to_be_bytes());
    hdr.extend_from_slice(&(fb.height as u16).to_be_bytes());
    hdr.extend_from_slice(&0i32.to_be_bytes()); // raw encoding
    stream.write_all(&hdr).map_err(|e| e.to_string())?;

    let row = (fb.width * 4) as usize;
    let stride = fb.stride as usize;
    let mut encoded = Vec::with_capacity(fb.width as usize * format.bytes);
    for y in 0..fb.height as usize {
        let start = y * stride;
        let end = start + row;
        if end > pixels.len() {
            return Err("FB buffer shorter than geometry".into());
        }
        format.encode_row(&pixels[start..end], &mut encoded);
        stream.write_all(&encoded).map_err(|e| e.to_string())?;
    }
    stream.flush().map_err(|e| e.to_string())?;
    Ok(())
}

fn read_exact_timeout(
    stream: &mut TcpStream,
    buf: &mut [u8],
    stop: &AtomicBool,
) -> Result<(), String> {
    let mut got = 0;
    while got < buf.len() {
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
