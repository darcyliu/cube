//! Bounded share helpers. Every export runs in a separate, unprivileged process.
//! 9P and virtio-fs are different protocols; the virtio-fs helper does not reuse
//! the 9P codec.
mod fuse;
mod server;
pub use server::MAX_MESSAGE;
use std::io::{Read, Write};
#[cfg(test)]
use std::os::fd::FromRawFd;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShareTransport {
    #[default]
    P9,
    VirtioFs,
}

impl ShareTransport {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "9p" => Ok(Self::P9),
            "virtiofs" => Ok(Self::VirtioFs),
            _ => Err("--share-transport requires 9p or virtiofs".into()),
        }
    }
}

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
    pub child: Arc<Mutex<Option<Child>>>,
    helper: Option<JoinHandle<()>>,
    input: Option<Box<dyn Write + Send>>,
    output: Box<dyn Read + Send>,
    output_fd: RawFd,
}
impl Backend {
    fn wait_readable(fd: RawFd, timeout_ms: i32) -> Result<(), String> {
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms as u64);
        loop {
            let remaining = deadline
                .saturating_duration_since(std::time::Instant::now())
                .as_millis() as i32;
            let mut fd = libc::pollfd {
                fd,
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
        Self::spawn(config, "--internal-9p-worker")
    }

    pub fn start_virtiofs(config: &ShareConfig) -> Result<Self, String> {
        Self::spawn(config, "--internal-virtiofs-worker")
    }

    fn spawn(config: &ShareConfig, worker_flag: &str) -> Result<Self, String> {
        let mut cmd = Command::new(std::env::current_exe().map_err(|e| e.to_string())?);
        cmd.arg(worker_flag)
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
        let output_fd = output.as_raw_fd();
        let mut ready = [0u8];
        if Self::wait_readable(output_fd, 5000).is_err()
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
            child: Arc::new(Mutex::new(Some(child))),
            helper: None,
            input: Some(Box::new(input)),
            output: Box::new(output),
            output_fd,
        })
    }

    /// Same framed FUSE loop as the helper process. The cargo test harness owns
    /// the binary entry point, so unit tests cannot re-exec `--internal-virtiofs-worker`.
    #[cfg(test)]
    pub fn start_virtiofs_inprocess(config: &ShareConfig) -> Result<Self, String> {
        if !config.read_only {
            return Err("virtiofs helper is read-only".into());
        }
        let (child_read, parent_write) = anon_pipe()?;
        let (parent_read, child_write) = anon_pipe()?;
        let path = config.path.clone();
        let helper = std::thread::Builder::new()
            .name("virtiofs-test-helper".into())
            .spawn(move || {
                let mut server = match fuse::Server::new(&path, true) {
                    Ok(server) => server,
                    Err(e) => {
                        eprintln!("[virtiofs helper] {e}");
                        return;
                    }
                };
                let mut input = child_read;
                let mut output = child_write;
                if let Err(e) = serve_fuse(&mut server, &mut input, &mut output) {
                    eprintln!("[virtiofs helper] {e}");
                }
            })
            .map_err(|e| format!("virtiofs helper: {e}"))?;
        let output_fd = parent_read.as_raw_fd();
        let mut output = parent_read;
        let mut ready = [0u8];
        if Self::wait_readable(output_fd, 5000).is_err()
            || output.read_exact(&mut ready).is_err()
            || ready[0] != 1
        {
            drop(parent_write);
            let _ = helper.join();
            return Err(format!(
                "share {}: virtiofs helper did not become ready",
                config.tag
            ));
        }
        Ok(Self {
            child: Arc::new(Mutex::new(None)),
            helper: Some(helper),
            input: Some(Box::new(parent_write)),
            output: Box::new(output),
            output_fd,
        })
    }

    pub fn request(&mut self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        let input = self.input.as_mut().ok_or("share helper closed")?;
        input
            .write_all(&(bytes.len() as u32).to_le_bytes())
            .and_then(|_| input.write_all(bytes))
            .map_err(|e| format!("share helper write: {e}"))?;
        input.flush().map_err(|e| e.to_string())?;
        let mut size = [0; 4];
        Self::wait_readable(self.output_fd, 30_000)?;
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

pub fn fuse_worker(args: &[String]) -> Result<(), String> {
    let (path, read_only) = helper_export(args)?;
    let mut server =
        fuse::Server::new(std::path::Path::new(path), read_only).map_err(|e| e.to_string())?;
    eprintln!(
        "[virtiofs helper] uid={} gid={} export={} mode=ro",
        unsafe { libc::geteuid() },
        unsafe { libc::getegid() },
        path
    );
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    serve_fuse(&mut server, &mut input, &mut output)
}

fn serve_fuse(
    server: &mut fuse::Server,
    input: &mut impl Read,
    output: &mut impl Write,
) -> Result<(), String> {
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
            server.request(&request)?.unwrap_or_default()
        };
        output
            .write_all(&(response.len() as u32).to_le_bytes())
            .and_then(|_| output.write_all(&response))
            .and_then(|_| output.flush())
            .map_err(|e| e.to_string())?;
    }
}

#[cfg(test)]
fn anon_pipe() -> Result<(std::fs::File, std::fs::File), String> {
    let mut fds = [0; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    for fd in fds {
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
    }
    Ok(unsafe {
        (
            std::fs::File::from_raw_fd(fds[0]),
            std::fs::File::from_raw_fd(fds[1]),
        )
    })
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.input.take();
        if let Some(helper) = self.helper.take() {
            let _ = helper.join();
        }
        if let Ok(mut child) = self.child.lock() {
            if let Some(child) = child.as_mut() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn helper_export(args: &[String]) -> Result<(&str, bool), String> {
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
    Ok((args[2].as_str(), args[3] == "ro"))
}

pub fn worker(args: &[String]) -> Result<(), String> {
    let (path, read_only) = helper_export(args)?;
    let mut server =
        server::Server::new(std::path::Path::new(path), read_only).map_err(|e| e.to_string())?;
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
