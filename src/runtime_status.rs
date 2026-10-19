//! Structured guest network status for the cubectl manager.
use std::fs;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub struct Publisher {
    stop: mpsc::Sender<()>,
    worker: Option<JoinHandle<()>>,
}

fn publish(path: &Path, instance_id: &str, ip: Option<Ipv4Addr>) -> std::io::Result<()> {
    let data = serde_json::to_vec(&serde_json::json!({
        "version": 1,
        "instance_id": instance_id,
        "ip": ip,
    }))?;
    let next = path.with_extension("next");
    fs::write(&next, data)?;
    fs::rename(next, path)
}

impl Publisher {
    /// Standalone cube invocations do not publish unless a manager requests it.
    pub fn from_environment(address: Arc<Mutex<Option<Ipv4Addr>>>) -> Result<Option<Self>, String> {
        let Some(path) = std::env::var_os("CUBE_RUNTIME_STATUS_PATH") else { return Ok(None) };
        let id = std::env::var("CUBE_INSTANCE_ID")
            .map_err(|_| "runtime status requires CUBE_INSTANCE_ID".to_string())?;
        Self::start(PathBuf::from(path), id, address).map(Some)
    }

    fn start(path: PathBuf, id: String, address: Arc<Mutex<Option<Ipv4Addr>>>) -> Result<Self, String> {
        let initial = *address.lock().unwrap();
        publish(&path, &id, initial).map_err(|e| format!("cannot publish runtime status: {e}"))?;
        let (stop, receiver) = mpsc::channel();
        let worker = thread::Builder::new().name("runtime-status".into()).spawn(move || {
            let mut previous = initial;
            let mut reported_error = false;
            while let Err(mpsc::RecvTimeoutError::Timeout) = receiver.recv_timeout(Duration::from_millis(250)) {
                let current = *address.lock().unwrap();
                if current != previous {
                    match publish(&path, &id, current) {
                        Ok(()) => { previous = current; reported_error = false; }
                        Err(e) => {
                            if !reported_error { eprintln!("[Cube] Cannot update runtime status: {e}"); }
                            reported_error = true;
                        }
                    }
                }
            }
            // A NIC reset or in-process reboot must not leave an old IP behind.
            let _ = publish(&path, &id, None);
        }).map_err(|e| format!("cannot start runtime status publisher: {e}"))?;
        Ok(Self { stop, worker: Some(worker) })
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() { let _ = worker.join(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    fn wait_for_ip(path: &Path, expected: Option<&str>) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let value: serde_json::Value = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
            assert_eq!(value["version"], 1);
            assert_eq!(value["instance_id"], "launch-1");
            if value["ip"].as_str() == expected { return; }
            assert!(Instant::now() < deadline, "runtime status did not become {expected:?}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn publishes_address_changes_resets_and_shutdown_without_logs() {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("cube-runtime-{}-{stamp}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let path = dir.join("runtime.json");
        let address = Arc::new(Mutex::new(None));
        let publisher = Publisher::start(path.clone(), "launch-1".into(), address.clone()).unwrap();
        wait_for_ip(&path, None);
        *address.lock().unwrap() = Some(Ipv4Addr::new(192, 168, 64, 2));
        wait_for_ip(&path, Some("192.168.64.2"));
        *address.lock().unwrap() = Some(Ipv4Addr::new(192, 168, 64, 3));
        wait_for_ip(&path, Some("192.168.64.3"));
        *address.lock().unwrap() = None;
        wait_for_ip(&path, None);
        *address.lock().unwrap() = Some(Ipv4Addr::new(192, 168, 64, 4));
        wait_for_ip(&path, Some("192.168.64.4"));
        drop(publisher);
        wait_for_ip(&path, None);
        fs::remove_dir_all(dir).unwrap();
    }
}
