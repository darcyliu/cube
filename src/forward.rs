//! Lightweight TCP port forwarder for host-to-guest communication.
//!
//! Enables connecting to guest services (e.g. SSH on port 22) via localhost:
//! `ssh -p 2222 root@localhost` -> `192.168.18.2:22`.

use std::io;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::thread;
use std::time::Duration;

/// Starts a background TCP port forwarder from `127.0.0.1:<host_port>` to `target_addr`.
pub fn start_port_forward(host_port: u16, target_addr: String) {
    thread::spawn(move || {
        let bind_addr = format!("127.0.0.1:{}", host_port);
        let listener = match TcpListener::bind(&bind_addr) {
            Ok(l) => {
                println!(
                    "\x1b[1;32m[Port Forward] Listening on http://{} -> {}\x1b[0m",
                    bind_addr, target_addr
                );
                l
            }
            Err(e) => {
                eprintln!(
                    "\x1b[1;31m[Port Forward Error] Failed to bind to {}: {}\x1b[0m",
                    bind_addr, e
                );
                return;
            }
        };

        for stream in listener.incoming() {
            match stream {
                Ok(client_stream) => {
                    let target = target_addr.clone();
                    thread::spawn(move || {
                        handle_forward(client_stream, &target);
                    });
                }
                Err(e) => {
                    eprintln!("[Port Forward] Accept error: {}", e);
                }
            }
        }
    });
}

fn handle_forward(mut client: TcpStream, target_addr: &str) {
    // Retry connecting to target in case the guest service is still initializing
    let mut target = None;
    for _ in 0..10 {
        match TcpStream::connect(target_addr) {
            Ok(s) => {
                target = Some(s);
                break;
            }
            Err(_) => {
                thread::sleep(Duration::from_millis(500));
            }
        }
    }

    let mut target = match target {
        Some(t) => t,
        None => {
            eprintln!(
                "[Port Forward] Unable to connect to target {} (guest SSH still booting?)",
                target_addr
            );
            return;
        }
    };

    let mut client_read = match client.try_clone() {
        Ok(c) => c,
        Err(_) => return,
    };
    let mut target_write = match target.try_clone() {
        Ok(t) => t,
        Err(_) => return,
    };

    // Client -> Target
    let h1 = thread::spawn(move || {
        let _ = io::copy(&mut client_read, &mut target_write);
        let _ = target_write.shutdown(Shutdown::Write);
    });

    // Target -> Client
    let _ = io::copy(&mut target, &mut client);
    let _ = client.shutdown(Shutdown::Write);

    let _ = h1.join();
}
