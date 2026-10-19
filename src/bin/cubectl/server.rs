use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::config::{resolve_target_dir, InstanceRecord, LaunchSpec};
use crate::util::{control_socket_path, ensure_control_root, query_socket, require, write_json_atomic};

pub fn live_status(state_dir: &Path) -> Option<serde_json::Value> {
    let sock = control_socket_path(state_dir);
    if !sock.exists() {
        return None;
    }
    let req = serde_json::json!({"action": "status"});
    query_socket(&sock, &req, Duration::from_millis(1500)).ok()
}

pub fn active_resources(root: &Path, skip: &Path) -> HashSet<String> {
    let mut used = HashSet::new();
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path == skip || !path.is_dir() {
                continue;
            }
            if let Some(status) = live_status(&path) {
                if status.get("state").and_then(|s| s.as_str()) == Some("running") {
                    if let Some(res) = status.get("resources").and_then(|r| r.as_array()) {
                        for item in res {
                            if let Some(s) = item.as_str() {
                                used.insert(s.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    used
}

pub fn serve(state_dir: &Path) -> Result<(), String> {
    serve_inner(state_dir, None)
}

pub fn serve_interactive(state_dir: &Path, start_lock: fs::File) -> Result<(), String> {
    serve_inner(state_dir, Some(start_lock))
}

fn serve_inner(state_dir: &Path, start_lock: Option<fs::File>) -> Result<(), String> {
    let lock_path = state_dir.join("instance.lock");
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| format!("cannot open lock {:?}: {}", lock_path, e))?;

    use std::os::unix::io::AsRawFd;
    let rc = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        return Err("instance already running".into());
    }

    let spec_path = state_dir.join("launch.json");
    let spec_data = fs::read_to_string(&spec_path).map_err(|e| format!("cannot read {:?}: {}", spec_path, e))?;
    let spec: LaunchSpec = serde_json::from_str(&spec_data).map_err(|e| e.to_string())?;

    drop(crate::util::lock_resources(&spec.resources)?);

    let control = control_socket_path(state_dir);
    ensure_control_root(&control)?;
    let _ = fs::remove_file(&control);

    let listener = UnixListener::bind(&control).map_err(|e| format!("cannot bind {:?}: {}", control, e))?;
    fs::set_permissions(&control, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;

    let log_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(state_dir.join("vmm.log"))
        .map_err(|e| format!("cannot open vmm.log: {}", e))?;

    let uart_path = state_dir.join("uart");
    let mut cmd = if spec.sudo {
        let mut c = Command::new("sudo");
        c.args(["-n", "/usr/bin/env", &format!("VMM_UART_IN={}", uart_path.display())]);
        c.args(&spec.command);
        c
    } else {
        let mut c = Command::new(&spec.command[0]);
        c.args(&spec.command[1..]);
        c.env("VMM_UART_IN", &uart_path);
        c
    };

    let interactive_log = if spec.interactive { Some(log_file.try_clone().map_err(|e| e.to_string())?) } else { None };
    if spec.interactive {
        cmd.stdout(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
        cmd.stdout(log_file.try_clone().map_err(|e| e.to_string())?);
        cmd.stderr(log_file);
        cmd.process_group(0);
    }
    // Interactive children share the foreground terminal's process group.

    let mut child = cmd.spawn().map_err(|e| format!("failed to spawn {:?}: {}", spec.command[0], e))?;
    let mut output_thread = interactive_log.map(|mut log| {
        let mut output = child.stdout.take().expect("interactive stdout is piped");
        std::thread::spawn(move || {
            let mut buffer = [0; 8192];
            while let Ok(count) = output.read(&mut buffer) {
                if count == 0 { break; }
                let _ = log.write_all(&buffer[..count]);
                let mut stdout = std::io::stdout().lock();
                let _ = stdout.write_all(&buffer[..count]);
                let _ = stdout.flush();
            }
        })
    });
    let child_pid = child.id();
    let started = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64();

    let record = InstanceRecord {
        id: spec.id.clone(),
        name: spec.name.clone(),
        config: spec.config.clone(),
        pid: child_pid,
        manager_pid: std::process::id(),
        started,
        resources: spec.resources.clone(),
        control_socket: control.to_string_lossy().into_owned(),
        shares: spec.shares.clone(),
        ssh_port: spec.ssh_port,
        vnc: spec.vnc.clone(),
    };
    write_json_atomic(&state_dir.join("instance.json"), &serde_json::to_value(&record).unwrap())?;

    drop(start_lock);

    let exit_code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code().unwrap_or(1),
            Ok(None) => {}
            Err(_) => break 1,
        }

        match listener.accept() {
            Ok((mut stream, _)) => {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_ok() && !line.is_empty() {
                    let resp = handle_client_request(&line, &mut child, &spec, state_dir, &record, &mut output_thread);
                    let mut payload = serde_json::to_string(&resp).unwrap();
                    payload.push('\n');
                    let _ = stream.write_all(payload.as_bytes());
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => {}
        }
    };

    if let Some(thread) = output_thread.take() { let _ = thread.join(); }
    let ended = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs_f64();
    let last = serde_json::json!({
        "id": record.id,
        "name": record.name,
        "config": record.config,
        "pid": record.pid,
        "manager_pid": record.manager_pid,
        "started": record.started,
        "resources": record.resources,
        "control_socket": record.control_socket,
        "shares": record.shares,
        "ssh_port": record.ssh_port,
        "vnc": record.vnc,
        "state": "stopped",
        "exit_code": exit_code,
        "ended": ended
    });
    let _ = write_json_atomic(&state_dir.join("last.json"), &last);
    let _ = fs::remove_file(&control);
    let _ = fs::remove_file(state_dir.join("instance.json"));

    if spec.interactive && exit_code != 0 {
        return Err(format!("VM exited with code {exit_code}"));
    }
    Ok(())
}

fn handle_client_request(
    line: &str,
    child: &mut std::process::Child,
    spec: &LaunchSpec,
    state_dir: &Path,
    record: &InstanceRecord,
    output_thread: &mut Option<std::thread::JoinHandle<()>>,
) -> serde_json::Value {
    let req: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return serde_json::json!({"ok": false, "error": format!("invalid json: {}", e)}),
    };

    let action = match req.get("action").and_then(|a| a.as_str()) {
        Some(a) => a,
        None => return serde_json::json!({"ok": false, "error": "missing action"}),
    };

    match action {
        "status" => {
            if child.try_wait().ok().flatten().is_some() {
                serde_json::json!({
                    "ok": false,
                    "state": "stopped",
                    "error": "VM process has terminated",
                    "id": record.id,
                    "name": record.name,
                    "ssh_port": record.ssh_port,
                    "vnc": record.vnc
                })
            } else {
                serde_json::json!({
                    "ok": true,
                    "state": "running",
                    "id": record.id,
                    "name": record.name,
                    "config": record.config,
                    "pid": record.pid,
                    "manager_pid": record.manager_pid,
                    "started": record.started,
                    "resources": record.resources,
                    "control_socket": record.control_socket,
                    "shares": record.shares,
                    "ssh_port": record.ssh_port,
                    "vnc": record.vnc
                })
            }
        }
        "stop" => {
            let timeout = req.get("timeout").and_then(|t| t.as_u64()).unwrap_or(10);
            let signal = force_stop_child(child, spec.sudo, spec.interactive, Duration::from_secs(timeout));
            serde_json::json!({"ok": true, "result": "forced-stop", "signal": signal})
        }
        "shutdown" => {
            let timeout = req.get("timeout").and_then(|t| t.as_u64()).unwrap_or(120);
            let force_after = req.get("force_after").and_then(|f| f.as_bool()).unwrap_or(false);
            guest_shutdown(child, spec, state_dir, Duration::from_secs(timeout), force_after, output_thread)
        }
        other => serde_json::json!({"ok": false, "error": format!("unknown action: {}", other)}),
    }
}

fn force_stop_child(child: &mut std::process::Child, sudo: bool, interactive: bool, timeout: Duration) -> &'static str {
    if child.try_wait().ok().flatten().is_some() {
        return "none";
    }
    let pid = child.id() as i32;
    let signal_target = if interactive { pid } else { -pid };
    if sudo {
        let _ = Command::new("sudo").args(["-n", "/bin/kill", "-15", "--", &signal_target.to_string()]).output();
    } else {
        unsafe { libc::kill(signal_target, libc::SIGTERM) };
    }

    let start = Instant::now();
    while start.elapsed() < timeout {
        if child.try_wait().ok().flatten().is_some() {
            return "SIGTERM";
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    if sudo {
        let _ = Command::new("sudo").args(["-n", "/bin/kill", "-9", "--", &signal_target.to_string()]).output();
    } else {
        unsafe { libc::kill(signal_target, libc::SIGKILL) };
    }
    let _ = child.wait();
    "SIGKILL"
}

fn guest_shutdown(
    child: &mut std::process::Child,
    spec: &LaunchSpec,
    state_dir: &Path,
    timeout: Duration,
    force_after: bool,
    output_thread: &mut Option<std::thread::JoinHandle<()>>,
) -> serde_json::Value {
    let cfg = match spec.shutdown {
        Some(ref c) => c,
        None => return serde_json::json!({"ok": false, "error": "no guest SSH shutdown configured; use stop"}),
    };

    let known_hosts = state_dir.join("known_hosts");
    let target = format!("{}@{}", cfg.user, cfg.host);
    let port_str = cfg.port.to_string();

    let start = Instant::now();
    let ssh_out = Command::new("ssh")
        .args([
            "-F", "/dev/null",
            "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=5",
            "-o", "StrictHostKeyChecking=accept-new",
            "-o", &format!("UserKnownHostsFile={}", known_hosts.display()),
            "-i", &cfg.identity_file,
            "-p", &port_str,
            &target,
            "/sbin/shutdown -p now",
        ])
        .output();

    let ssh_err = match ssh_out {
        Ok(ref o) => String::from_utf8_lossy(&o.stderr).into_owned(),
        Err(ref e) => e.to_string(),
    };

    while start.elapsed() < timeout {
        if let Ok(Some(code)) = child.try_wait() {
            if let Some(thread) = output_thread.take() { let _ = thread.join(); }
            let log_text = fs::read_to_string(state_dir.join("vmm.log")).unwrap_or_default();
            let graceful = code.success() && log_text.contains("Guest requested PSCI SYSTEM_OFF");
            return serde_json::json!({
                "ok": graceful,
                "result": if graceful { "guest-poweroff" } else { "guest-exit-error" },
                "exit_code": code.code(),
                "ssh": ssh_err
            });
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    if force_after {
        let sig = force_stop_child(child, spec.sudo, spec.interactive, Duration::from_secs(10));
        serde_json::json!({"ok": false, "result": "forced-after-timeout", "signal": sig})
    } else {
        serde_json::json!({
            "ok": false,
            "error": "guest shutdown timed out; VM is still running",
            "ssh": ssh_err
        })
    }
}

pub fn do_action(target: &str, state_root: &Path, action: &str, timeout: u64, force_after: bool) -> Result<(), String> {
    let (state_dir, _) = resolve_target_dir(target, state_root)?;
    let sock = control_socket_path(&state_dir);
    require(sock.exists(), "VM is not running")?;

    let req = serde_json::json!({
        "action": action,
        "timeout": timeout,
        "force_after": force_after
    });

    let resp = query_socket(&sock, &req, Duration::from_secs(timeout + 20))?;
    println!("{}", serde_json::to_string_pretty(&resp).unwrap());
    if resp.get("ok").and_then(|o| o.as_bool()) == Some(true) {
        Ok(())
    } else {
        Err("action failed".into())
    }
}
