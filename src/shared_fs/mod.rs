//! Bounded 9P2000.L server. Every export runs in a separate, unprivileged process.
mod server;
pub use server::MAX_MESSAGE;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct ShareConfig {
    pub tag: String,
    pub path: PathBuf,
    pub read_only: bool,
}
impl ShareConfig {
    pub fn new(tag: &str, path: &str, read_only: bool) -> Result<Self, String> {
        if tag.is_empty()
            || tag.len() > 120
            || !tag
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
        {
            return Err("share tag must be 1–120 ASCII letters, digits, '.', '_' or '-'".into());
        }
        let path = std::fs::canonicalize(path).map_err(|e| format!("share {tag}: {e}"))?;
        if !path.is_dir() {
            return Err(format!("share {tag}: export must be a directory"));
        }
        Ok(Self {
            tag: tag.into(),
            path,
            read_only,
        })
    }
}

pub struct Backend {
    pub child: Arc<Mutex<Child>>,
    input: ChildStdin,
    output: ChildStdout,
}
impl Backend {
    fn wait_readable(output: &ChildStdout, timeout_ms: i32) -> Result<(), String> {
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms as u64);
        loop {
            let remaining = deadline
                .saturating_duration_since(std::time::Instant::now())
                .as_millis() as i32;
            let mut fd = libc::pollfd {
                fd: output.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let n = unsafe { libc::poll(&mut fd, 1, remaining) };
            if n > 0 {
                return Ok(());
            }
            if n == 0 {
                return Err("share helper timed out".into());
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error.to_string());
            }
        }
    }
    pub fn start(config: &ShareConfig) -> Result<Self, String> {
        let mut cmd = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
        cmd.arg("--internal-9p-worker")
            .arg(&config.path)
            .arg(if config.read_only { "ro" } else { "rw" })
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        if unsafe { libc::geteuid() } == 0 {
            let uid = std::env::var("SUDO_UID")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .filter(|&v| v != 0)
                .ok_or(
                    "root shared-folder launches require sudo from a non-root user (SUDO_UID)",
                )?;
            let gid = std::env::var("SUDO_GID")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .ok_or("root shared-folder launches require SUDO_GID")?;
            // CommandExt's uid/gid changes happen before pre_exec; drop groups here
            // together with IDs so the ordering is explicit and auditable.
            unsafe {
                cmd.pre_exec(move || {
                    if libc::setgroups(0, std::ptr::null()) != 0
                        || libc::setgid(gid) != 0
                        || libc::setuid(uid) != 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let mut child = cmd.spawn().map_err(|e| format!("share helper: {e}"))?;
        let input = child.stdin.take().unwrap();
        let mut output = child.stdout.take().unwrap();
        let mut ready = [0u8];
        if Self::wait_readable(&output, 5000).is_err()
            || output.read_exact(&mut ready).is_err()
            || ready[0] != 1
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "share {}: helper could not open export as its unprivileged user",
                config.tag
            ));
        }
        Ok(Self {
            child: Arc::new(Mutex::new(child)),
            input,
            output,
        })
    }
    pub fn request(&mut self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        self.input
            .write_all(&(bytes.len() as u32).to_le_bytes())
            .and_then(|_| self.input.write_all(bytes))
            .map_err(|e| format!("share helper write: {e}"))?;
        self.input.flush().map_err(|e| e.to_string())?;
        let mut size = [0; 4];
        Self::wait_readable(&self.output, 30_000)?;
        self.output
            .read_exact(&mut size)
            .map_err(|e| format!("share helper read: {e}"))?;
        let size = u32::from_le_bytes(size) as usize;
        if size > MAX_MESSAGE {
            return Err("oversize share helper response".into());
        }
        let mut result = vec![0; size];
        self.output
            .read_exact(&mut result)
            .map_err(|e| e.to_string())?;
        Ok(result)
    }
}
impl Drop for Backend {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

pub fn worker(args: &[String]) -> Result<(), String> {
    if args.len() != 4 || !matches!(args[3].as_str(), "ro" | "rw") {
        return Err("invalid internal helper arguments".into());
    }
    if unsafe { libc::geteuid() } == 0 {
        return Err("share helper refuses host root identity".into());
    }
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    unsafe {
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        limit.rlim_cur = limit.rlim_cur.min(8192);
        if libc::setrlimit(libc::RLIMIT_NOFILE, &limit) != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
    }
    let mut server = server::Server::new(std::path::Path::new(&args[2]), args[3] == "ro")
        .map_err(|e| e.to_string())?;
    eprintln!(
        "[9P helper] uid={} gid={} export={} mode={}",
        unsafe { libc::geteuid() },
        unsafe { libc::getegid() },
        args[2],
        args[3]
    );
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    output
        .write_all(&[1])
        .and_then(|_| output.flush())
        .map_err(|e| e.to_string())?;
    loop {
        let mut size = [0; 4];
        match input.read_exact(&mut size) {
            Ok(()) => (),
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
            Err(e) => return Err(e.to_string()),
        }
        let n = u32::from_le_bytes(size) as usize;
        if n > MAX_MESSAGE {
            return Err("oversize helper request".into());
        }
        let mut request = vec![0; n];
        input.read_exact(&mut request).map_err(|e| e.to_string())?;
        let response = if n == 0 {
            server.reset();
            Vec::new()
        } else {
            server.request(&request)
        };
        output
            .write_all(&(response.len() as u32).to_le_bytes())
            .and_then(|_| output.write_all(&response))
            .and_then(|_| output.flush())
            .map_err(|e| e.to_string())?;
    }
}
