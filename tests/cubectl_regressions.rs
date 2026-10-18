use std::{fs, path::{Path, PathBuf}, process::{Child, Command, Output, Stdio}, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};
use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        Self::with_stamp(stamp)
    }
    fn with_stamp(stamp: u128) -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        loop {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("cubectl-regression-{}-{stamp}-{id}", std::process::id()));
            // Timestamp precision is not a uniqueness guarantee. Atomically
            // claim the directory; never reuse another fixture's files.
            match fs::create_dir(&dir) {
                Ok(()) => return Self(dir),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create fixture {}: {error}", dir.display()),
            }
        }
    }

    fn cmd(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_cubectl"));
        cmd.current_dir(&self.0).arg("--state-root").arg(self.0.join("state"));
        cmd
    }
    fn run(&self, args: &[&str]) -> Output { self.cmd().args(args).output().unwrap() }
    fn file(&self, name: &str, data: &[u8]) { fs::write(self.0.join(name), data).unwrap(); }
    fn script(&self, name: &str, data: &str) {
        self.file(name, data.as_bytes());
        fs::set_permissions(self.0.join(name), fs::Permissions::from_mode(0o755)).unwrap();
    }
    fn config(&self, name: &str, disk: &str) {
        self.file(&format!("{name}.json"), serde_json::to_vec(&serde_json::json!({
            "name": name, "binary": self.0.join("fake"),
            "boot": {"mode": "uefi", "firmware": self.0.join("fw")},
            "disks": [disk]
        })).unwrap().as_slice());
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
struct ChildGuard(Child);
impl ChildGuard {
    fn spawn(command: &mut Command) -> Self {
        // The interactive manager and its guest share this dedicated group.
        Self(command.process_group(0).spawn().unwrap())
    }
}
impl Drop for ChildGuard {
    fn drop(&mut self) {
        if matches!(self.0.try_wait(), Ok(None)) {
            unsafe { libc::killpg(self.0.id() as i32, libc::SIGKILL); }
        }
        let _ = self.0.wait();
    }
}
fn success(out: &Output) { assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr)); }
fn wait_for(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !predicate() {
        assert!(Instant::now() < deadline, "condition timed out");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn relative_create_destination_can_be_started() {
    let f = Fixture::new();
    f.file("fw", b"firmware"); f.script("fake", "#!/bin/sh\nexit 0\n");
    success(&f.run(&["create", "relative", "--dest", "relative-dir", "--firmware", "fw", "--binary", "fake", "--disk", "disk.img:1M"]));
    let config: serde_json::Value = serde_json::from_slice(&fs::read(f.0.join("relative-dir/relative.json")).unwrap()).unwrap();
    assert!(Path::new(config["disks"][0].as_str().unwrap()).is_absolute());
    success(&f.run(&["start", "relative-dir/relative.json", "--binary", "fake", "-i"]));
}

#[test]
fn interactive_lifecycle_and_disk_ownership() {
    let f = Fixture::new();
    f.file("fw", b"firmware"); f.file("disk", b"original disk"); f.file("other-disk", b"other");
    f.script("fake", "#!/bin/sh\nexec sleep 30\n");
    f.config("one", "disk"); f.config("two", "other-disk"); f.config("alias", "disk");
    let mut one = ChildGuard::spawn(f.cmd().args(["start", "one.json", "-i"]).stdin(Stdio::null()).stdout(Stdio::null()));
    wait_for(|| String::from_utf8_lossy(&f.run(&["status", "one"]).stdout).contains("running"));
    let install = f.run(&["install", "one.json", "--iso", "missing.iso"]);
    assert!(!install.status.success());
    assert!(String::from_utf8_lossy(&install.stderr).contains("running VM"));
    let alias_install = f.run(&["install", "alias.json", "--iso", "missing.iso"]);
    assert!(String::from_utf8_lossy(&alias_install.stderr).contains("disk is in use"));
    let other_root = Command::new(env!("CARGO_BIN_EXE_cubectl"))
        .current_dir(&f.0).args(["--state-root", "other-state", "start", "alias.json", "-i"]).output().unwrap();
    assert!(String::from_utf8_lossy(&other_root.stderr).contains("disk is in use"), "{}", String::from_utf8_lossy(&other_root.stderr));
    assert!(!f.run(&["delete", "one.json"]).status.success());
    assert_eq!(fs::read(f.0.join("disk")).unwrap(), b"original disk");
    // A distinct VM must start while the interactive VM is still alive.
    let mut two = ChildGuard::spawn(f.cmd().args(["start", "two.json", "-i"]).stdin(Stdio::null()).stdout(Stdio::null()));
    wait_for(|| String::from_utf8_lossy(&f.run(&["status", "two"]).stdout).contains("running"));
    success(&f.run(&["stop", "one", "--timeout", "1"]));
    success(&f.run(&["stop", "two", "--timeout", "1"]));
    wait_for(|| one.0.try_wait().unwrap().is_some());
    wait_for(|| two.0.try_wait().unwrap().is_some());
    assert!(String::from_utf8_lossy(&f.run(&["status", "one"]).stdout).contains("stopped"));
}

// Minimal ext4 fixture: MBR, 1 KiB blocks, 128-byte inodes, extent-backed files.
fn put16(b: &mut [u8], off: usize, n: u16) { b[off..off+2].copy_from_slice(&n.to_le_bytes()); }
fn put32(b: &mut [u8], off: usize, n: u32) { b[off..off+4].copy_from_slice(&n.to_le_bytes()); }
fn inode(b: &mut [u8], n: usize, block: usize, data: &[u8]) {
    let off = 4096 + 3072 + (n-1)*128;
    put32(b, off+4, data.len() as u32);
    put16(b, off+40, 0xf30a); put16(b, off+42, 1); put16(b, off+44, 4);
    put16(b, off+56, 1); put32(b, off+60, block as u32);
    let start = 4096+block*1024;
    b[start..start+data.len()].copy_from_slice(data);
}
fn directory(entries: &[(&str,u32)]) -> Vec<u8> {
    let mut b = vec![0;1024]; let mut pos = 0;
    for (i,(name,ino)) in entries.iter().enumerate() {
        let len = if i+1 == entries.len() {1024-pos} else {(8+name.len()+3)&!3};
        put32(&mut b,pos,*ino);put16(&mut b,pos+4,len as u16);b[pos+6]=name.len() as u8;b[pos+7]=1;
        b[pos+8..pos+8+name.len()].copy_from_slice(name.as_bytes());pos+=len;
    }
    b
}
fn image() -> Vec<u8> {
    let mut b = vec![0;64*1024]; b[450]=0x83;put32(&mut b,454,8);
    for (off,n) in [(0,16),(4,48),(20,1),(24,0),(32,48),(40,16)] { put32(&mut b,5120+off,n); }
    put16(&mut b,5120+56,0xef53);put16(&mut b,5120+88,128);put32(&mut b,4096+2048+8,3);
    inode(&mut b,2,10,&directory(&[("etc",3),("usr",6)]));
    inode(&mut b,3,11,&directory(&[("shadow",4),("machine-id",5)]));
    inode(&mut b,4,12,b"root:!:1:0:99999:7:::\n"); inode(&mut b,5,13,b"uninitialized\n");
    inode(&mut b,6,14,&directory(&[("lib",7)]));inode(&mut b,7,15,&directory(&[("systemd",8)]));
    inode(&mut b,8,16,&directory(&[("system",9)]));inode(&mut b,9,17,&directory(&[("serial-getty@.service",10)]));
    inode(&mut b,10,18,b"[Service]\nExecStart=-/sbin/agetty %I\n");b
}
#[test]
fn cloud_patching_is_repeatable_and_hash_failure_does_not_modify_image() {
    let f = Fixture::new(); let original = image();f.file("disk",&original);
    // Force the unsupported-openssl path regardless of the host installation.
    f.script("openssl", "#!/bin/sh\nexit 1\n");
    let failed = f.cmd().env("PATH", &f.0).args(["patch-shadow","disk","custom-password"]).output().unwrap();
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("cannot hash password"), "unexpected error: {}", String::from_utf8_lossy(&failed.stderr));
    assert_eq!(fs::read(f.0.join("disk")).unwrap(),original);
    success(&f.cmd().env("PATH", &f.0).args(["patch-shadow","disk","root"]).output().unwrap());
    let once = fs::read(f.0.join("disk")).unwrap();
    let machine_id = &once[4096+13*1024..4096+13*1024+33];
    assert!(machine_id[..32].iter().all(u8::is_ascii_hexdigit));assert_eq!(machine_id[32],b'\n');
    success(&f.cmd().env("PATH", &f.0).args(["patch-shadow","disk","root"]).output().unwrap());
    assert_eq!(fs::read(f.0.join("disk")).unwrap(), once);
    assert_eq!(String::from_utf8_lossy(&once).matches("--autologin root").count(),1);
}

#[test]
fn installation_requires_marker_and_successful_exit() {
    for (console, exit, expected_success) in [
        ("# bsdinstall script /media/installerconfig && echo VMM_INSTALL_COMPLETE && poweroff", 0, false),
        ("VMM_INSTALL_COMPLETE", 1, false),
        ("VMM_INSTALL_COMPLETE", 0, true),
    ] {
        let f = Fixture::new(); f.file("fw",b"fw");f.file("disk",b"target");f.file("installer.iso",b"iso");
        f.config("install", "disk");
        f.script("hdiutil", "#!/bin/sh\nexit 0\n");
        f.script("tar", "#!/bin/sh\nwhile [ $# -gt 0 ]; do\nif [ \"$1\" = '-C' ]; then shift; /bin/mkdir -p \"$1/boot/kernel\"; : > \"$1/boot/kernel/kernel\"; exit 0; fi\nshift\ndone\nexit 1\n");
        f.script("fake", &format!("#!/bin/sh\nwhile [ $# -gt 0 ]; do\nif [ \"$1\" = '--serial-log' ]; then shift; printf '%s\\n' '{console}' > \"$1\"; exit {exit}; fi\nshift\ndone\nexit 2\n"));
        let path = format!("{}:{}", f.0.display(), std::env::var("PATH").unwrap_or_default());
        let before = fs::read(f.0.join("install.json")).unwrap();
        let result = f.cmd().env("PATH",path).args(["install","install.json","--iso","installer.iso"]).output().unwrap();
        assert_eq!(result.status.success(), expected_success, "{}",String::from_utf8_lossy(&result.stderr));
        if !expected_success {
            assert_eq!(fs::read(f.0.join("install.json")).unwrap(), before);
            assert!(f.0.join("install-failed.log").is_file());
        }
    }
}

#[test]
fn interactive_shutdown_captures_poweroff_log() {
    let f = Fixture::new(); f.file("fw",b"fw");f.file("disk",b"disk");f.file("key",b"test key");
    f.script("fake", "#!/bin/sh\ntrap 'echo \"Guest requested PSCI SYSTEM_OFF\"; exit 0' USR1\necho $$ > guest.pid\nwhile :; do /bin/sleep 0.1; done\n");
    f.script("ssh", "#!/bin/sh\n/bin/kill -USR1 \"$(/bin/cat guest.pid)\"\n");
    f.config("shutdown", "disk");
    let config_path = f.0.join("shutdown.json");
    let mut config: serde_json::Value = serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    config["shutdown"] = serde_json::json!({"identity_file": "key"});
    fs::write(&config_path,serde_json::to_vec(&config).unwrap()).unwrap();
    let path = format!("{}:{}",f.0.display(),std::env::var("PATH").unwrap_or_default());
    let mut child = ChildGuard::spawn(f.cmd().env("PATH",path).args(["start","shutdown.json","-i"]).stdin(Stdio::null()).stdout(Stdio::null()));
    wait_for(|| f.0.join("guest.pid").exists() && String::from_utf8_lossy(&f.run(&["status","shutdown"]).stdout).contains("running"));
    let result = f.run(&["shutdown","shutdown","--timeout","3"]);
    success(&result);
    assert!(String::from_utf8_lossy(&result.stdout).contains("guest-poweroff"));
    wait_for(|| child.0.try_wait().unwrap().is_some());
}

#[test]
fn simultaneous_fixtures_with_identical_timestamps_are_isolated() {
    let barrier = std::sync::Barrier::new(16);
    let fixtures: Vec<_> = std::thread::scope(|scope| {
        let threads: Vec<_> = (0..16).map(|_| {
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                let fixture = Fixture::with_stamp(123);
                fixture.file("sentinel", b"owned");
                fixture
            })
        }).collect();
        threads.into_iter().map(|thread| thread.join().unwrap()).collect()
    });
    let unique: std::collections::HashSet<_> = fixtures.iter().map(|f| &f.0).collect();
    assert_eq!(unique.len(), fixtures.len());
    let mut fixtures = fixtures;
    while let Some(fixture) = fixtures.pop() {
        drop(fixture);
        for remaining in &fixtures {
            assert_eq!(fs::read(remaining.0.join("sentinel")).unwrap(), b"owned");
        }
    }
}
