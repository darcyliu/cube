use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

pub fn require(cond: bool, msg: impl Into<String>) -> Result<(), String> {
    if !cond {
        Err(msg.into())
    } else {
        Ok(())
    }
}

pub fn sha256_bytes(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];

    let mut h0 = 0x6a09e667u32;
    let mut h1 = 0xbb67ae85u32;
    let mut h2 = 0x3c6ef372u32;
    let mut h3 = 0xa54ff53au32;
    let mut h4 = 0x510e527fu32;
    let mut h5 = 0x9b05688cu32;
    let mut h6 = 0x1f83d9abu32;
    let mut h7 = 0x5be0cd19u32;

    let len = data.len();
    let bit_len = (len as u64).wrapping_mul(8);
    let mut padded = data.to_vec();
    padded.push(0x80);
    while (padded.len() % 64) != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut w = [0u32; 64];
        for (i, part) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([part[0], part[1], part[2], part[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }

        let mut a = h0;
        let mut b = h1;
        let mut c = h2;
        let mut d = h3;
        let mut e = h4;
        let mut f = h5;
        let mut g = h6;
        let mut h = h7;

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
        h5 = h5.wrapping_add(f);
        h6 = h6.wrapping_add(g);
        h7 = h7.wrapping_add(h);
    }

    let mut out = [0u8; 32];
    out[0..4].copy_from_slice(&h0.to_be_bytes());
    out[4..8].copy_from_slice(&h1.to_be_bytes());
    out[8..12].copy_from_slice(&h2.to_be_bytes());
    out[12..16].copy_from_slice(&h3.to_be_bytes());
    out[16..20].copy_from_slice(&h4.to_be_bytes());
    out[20..24].copy_from_slice(&h5.to_be_bytes());
    out[24..28].copy_from_slice(&h6.to_be_bytes());
    out[28..32].copy_from_slice(&h7.to_be_bytes());
    out
}

pub fn sha256_hex(data: &[u8]) -> String {
    let bytes = sha256_bytes(data);
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CubeSettings {
    #[serde(default)]
    pub vms_dir: Option<String>,
    #[serde(default)]
    pub ssh_key_path: Option<String>,
    #[serde(default)]
    pub terminal_app: Option<String>,
}

pub fn settings_path(state_root: &Path) -> PathBuf {
    state_root.join("settings.json")
}

pub fn load_cube_settings(state_root: &Path) -> CubeSettings {
    let path = settings_path(state_root);
    if let Ok(data) = fs::read(&path) {
        if let Ok(settings) = serde_json::from_slice::<CubeSettings>(&data) {
            return settings;
        }
    }
    CubeSettings::default()
}

pub fn save_cube_settings(state_root: &Path, settings: &CubeSettings) -> Result<(), String> {
    fs::create_dir_all(state_root).map_err(|e| format!("cannot create state root {:?}: {}", state_root, e))?;
    let path = settings_path(state_root);
    let json = serde_json::to_string_pretty(settings).map_err(|e| format!("cannot serialize settings: {}", e))?;
    fs::write(&path, json).map_err(|e| format!("cannot write settings {:?}: {}", path, e))?;
    Ok(())
}

pub fn default_state_root() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let cube_state = home.join(".local/state/cube");
    let vmm_state = home.join(".local/state/vmm");
    if !cube_state.exists() && vmm_state.exists() {
        vmm_state
    } else {
        cube_state
    }
}

pub fn resolve_path(value: &str, base: &Path) -> Result<PathBuf, String> {
    require(!value.is_empty(), "path must be nonempty")?;
    let path = PathBuf::from(value);
    let expanded = if let Ok(stripped) = path.strip_prefix("~") {
        let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        home.join(stripped)
    } else if path.is_relative() {
        base.join(path)
    } else {
        path
    };
    Ok(expanded)
}

pub fn default_vms_root() -> PathBuf {
    if let Some(dir) = std::env::var_os("CUBE_VMS_DIR") {
        let val = dir.to_string_lossy().trim().to_string();
        if !val.is_empty() {
            if let Ok(resolved) = resolve_path(&val, Path::new(".")) {
                return resolved;
            }
            return PathBuf::from(val);
        }
    }
    let state_root = default_state_root();
    let settings = load_cube_settings(&state_root);
    if let Some(ref dir) = settings.vms_dir {
        let trimmed = dir.trim();
        if !trimmed.is_empty() {
            if let Ok(resolved) = resolve_path(trimmed, Path::new(".")) {
                return resolved;
            }
            return PathBuf::from(trimmed);
        }
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join("Cube")
}

pub fn canonical_path_str(path: &Path) -> Result<String, String> {
    let canonical = path.canonicalize().map_err(|e| format!("cannot resolve {:?}: {}", path, e))?;
    Ok(canonical.to_string_lossy().into_owned())
}

pub fn canonical_path_buf(path: &Path) -> Result<PathBuf, String> {
    path.canonicalize().map_err(|e| format!("cannot canonicalize {:?}: {}", path, e))
}

pub fn control_socket_path(state_dir: &Path) -> PathBuf {
    let canonical = state_dir.canonicalize().unwrap_or_else(|_| state_dir.to_path_buf());
    let digest = &sha256_hex(canonical.to_string_lossy().as_bytes())[..24];
    if let Some(dir) = std::env::var_os("CUBECTL_SOCKET_DIR") {
        return PathBuf::from(dir).join(format!("{}.sock", digest));
    }
    let uid = unsafe { libc::getuid() };
    PathBuf::from("/tmp").join(format!("cubectl-{}", uid)).join(format!("{}.sock", digest))
}

pub fn ensure_control_root(control: &Path) -> Result<(), String> {
    let root = control.parent().ok_or("no parent dir for socket")?;
    if !root.exists() {
        use std::os::unix::fs::DirBuilderExt;
        let _ = fs::DirBuilder::new().mode(0o700).create(root);
    }
    let metadata = fs::symlink_metadata(root).map_err(|e| format!("cannot stat {:?}: {}", root, e))?;
    require(metadata.is_dir() && !metadata.file_type().is_symlink(), format!("{:?} is not a directory", root))?;
    let uid = unsafe { libc::getuid() };
    require(metadata.uid() == uid, format!("{:?} not owned by UID {}", root, uid))?;
    if metadata.permissions().mode() & 0o777 != 0o700 {
        let _ = fs::set_permissions(root, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

pub fn query_socket(socket_path: &Path, request: &serde_json::Value, timeout: Duration) -> Result<serde_json::Value, String> {
    let mut stream = UnixStream::connect(socket_path).map_err(|e| format!("connect failed: {}", e))?;
    stream.set_read_timeout(Some(timeout)).map_err(|e| e.to_string())?;
    stream.set_write_timeout(Some(timeout)).map_err(|e| e.to_string())?;

    let payload = serde_json::to_string(request).map_err(|e| e.to_string())? + "\n";
    stream.write_all(payload.as_bytes()).map_err(|e| format!("send error: {}", e))?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).map_err(|e| format!("recv error: {}", e))?;
    serde_json::from_str(&line).map_err(|e| format!("json decode error: {}", e))
}

pub fn write_json_atomic(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let next = path.with_extension("next");
    let content = serde_json::to_string_pretty(value).map_err(|e| e.to_string())? + "\n";
    fs::write(&next, content).map_err(|e| format!("cannot write {:?}: {}", next, e))?;
    fs::rename(&next, path).map_err(|e| format!("cannot atomic replace {:?}: {}", path, e))?;
    Ok(())
}

pub fn parse_size(size_str: &str) -> Result<u64, String> {
    let s = size_str.trim();
    let num_end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
    let (num_part, unit) = s.split_at(num_end);
    let num: u64 = num_part.parse().map_err(|_| format!("invalid size number '{}'", num_part))?;
    let mult: u64 = match unit.trim().to_uppercase().as_str() {
        "" | "B" => 1,
        "K" | "KB" | "KIB" => 1024,
        "M" | "MB" | "MIB" => 1024 * 1024,
        "G" | "GB" | "GIB" => 1024 * 1024 * 1024,
        "T" | "TB" | "TIB" => 1024 * 1024 * 1024 * 1024,
        other => return Err(format!("unknown size unit '{}' in '{}'", other, size_str)),
    };
    Ok(num * mult)
}

pub fn create_sparse_file(path: &Path, size_bytes: u64) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("cannot create parent directory {:?}: {}", parent, e))?;
    }
    let file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .map_err(|e| format!("cannot create disk file {:?}: {}", path, e))?;
    file.set_len(size_bytes)
        .map_err(|e| format!("cannot set sparse length on {:?}: {}", path, e))?;
    Ok(())
}

pub struct ParsedDiskSpec {
    pub path_str: String,
    pub size_bytes: Option<u64>,
    pub cow: bool,
    pub read_only: bool,
}

pub fn parse_disk_spec(spec: &str) -> Result<ParsedDiskSpec, String> {
    let mut cow = false;
    let mut read_only = false;
    let mut base_spec = spec;

    if let Some(pos) = base_spec.rfind(',') {
        let flag = &base_spec[pos + 1..];
        if flag == "cow" {
            cow = true;
            base_spec = &base_spec[..pos];
        } else if flag == "ro" {
            read_only = true;
            base_spec = &base_spec[..pos];
        }
    }

    let (path_str, size_bytes) = if let Some(pos) = base_spec.rfind(':') {
        let potential_size = &base_spec[pos + 1..];
        if let Ok(sz) = parse_size(potential_size) {
            (base_spec[..pos].to_string(), Some(sz))
        } else {
            (base_spec.to_string(), None)
        }
    } else {
        (base_spec.to_string(), None)
    };

    Ok(ParsedDiskSpec {
        path_str,
        size_bytes,
        cow,
        read_only,
    })
}

pub fn ensure_cube_entitlements(cube_path: &Path) {
    let out = Command::new("codesign")
        .args(["-d", "--entitlements", "-", cube_path.to_str().unwrap_or("")])
        .output();
    let has_hv = if let Ok(o) = out {
        String::from_utf8_lossy(&o.stdout).contains("com.apple.security.hypervisor")
    } else {
        false
    };

    if !has_hv {
        let candidates = [
            cube_path.parent().and_then(|p| p.parent()).map(|p| p.join("entitlements.plist")),
            Some(PathBuf::from("entitlements.plist")),
        ];
        for cand in candidates.into_iter().flatten() {
            if cand.is_file() {
                let _ = Command::new("codesign")
                    .args(["--entitlements", cand.to_str().unwrap(), "--force", "-s", "-", cube_path.to_str().unwrap()])
                    .output();
                break;
            }
        }
    }
}

pub fn find_cube_binary(
    cli_binary: Option<&Path>,
    config_binary: Option<&str>,
    base: &Path,
) -> Result<PathBuf, String> {
    // 1. Explicit CLI override
    if let Some(b) = cli_binary {
        let p = resolve_path(&b.to_string_lossy(), Path::new("."))?;
        if p.is_file() {
            ensure_cube_entitlements(&p);
            return Ok(p);
        }
        return Err(format!("Cube binary not found via --binary: {:?}", p));
    }

    // 2. Explicit config.binary in JSON
    if let Some(b) = config_binary {
        if let Ok(p) = resolve_path(b, base) {
            if p.is_file() {
                ensure_cube_entitlements(&p);
                return Ok(p);
            }
        }
        if let Ok(p) = resolve_path(b, Path::new(".")) {
            if p.is_file() {
                ensure_cube_entitlements(&p);
                return Ok(p);
            }
        }
    }

    // 3. Beside current cubectl executable (e.g. target/debug/cube or Cube.app/Contents/MacOS/cube)
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(parent) = current_exe.parent() {
            let candidate = parent.join("cube");
            if candidate.is_file() {
                ensure_cube_entitlements(&candidate);
                return Ok(candidate);
            }
        }
        if let Ok(resolved_exe) = current_exe.canonicalize() {
            if let Some(parent) = resolved_exe.parent() {
                let candidate = parent.join("cube");
                if candidate.is_file() {
                    ensure_cube_entitlements(&candidate);
                    return Ok(candidate);
                }
            }
        }
    }

    // 4. ~/.local/bin/cube
    if let Some(home) = std::env::var_os("HOME") {
        let candidate = PathBuf::from(home).join(".local/bin/cube");
        if candidate.is_file() {
            ensure_cube_entitlements(&candidate);
            return Ok(candidate);
        }
    }

    // 5. Look in PATH
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join("cube");
            if candidate.is_file() {
                ensure_cube_entitlements(&candidate);
                return Ok(candidate);
            }
        }
    }

    // 6. In current working directory
    for rel in &["target/debug/cube", "target/release/cube", "cube"] {
        let candidate = Path::new(rel);
        if candidate.is_file() {
            if let Ok(p) = candidate.canonicalize() {
                ensure_cube_entitlements(&p);
                return Ok(p);
            }
        }
    }

    Err("Cube binary 'cube' not found. Ensure cube is built ('make all'), installed in PATH, or specify --binary".into())
}

fn pad4(n: usize) -> usize {
    (n + 3) & !3
}

fn make_cpio_entry(name: &str, content: &[u8]) -> Vec<u8> {
    let name_bytes = [name.as_bytes(), &[0u8]].concat();
    let name_len = name_bytes.len();
    let content_len = content.len();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let header = format!(
        "070701{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}",
        1,        // ino
        0o100644, // mode (regular file rw-r--r--)
        0,        // uid
        0,        // gid
        1,        // nlink
        now,      // mtime
        content_len,
        0,        // devmajor
        0,        // devminor
        0,        // rdevmajor
        0,        // rdevminor
        name_len,
        0,        // check
    );
    assert_eq!(header.len(), 110);

    let mut entry = header.into_bytes();
    entry.extend_from_slice(&name_bytes);
    let name_pad = pad4(entry.len()) - entry.len();
    entry.extend(std::iter::repeat(0u8).take(name_pad));
    entry.extend_from_slice(content);
    let content_pad = pad4(content_len) - content_len;
    entry.extend(std::iter::repeat(0u8).take(content_pad));
    entry
}

fn make_trailer() -> Vec<u8> {
    let name = b"TRAILER!!!\0";
    let header = format!(
        "070701{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}{:08X}",
        0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        name.len(),
        0
    );
    let mut entry = header.into_bytes();
    entry.extend_from_slice(name);
    let pad = pad4(entry.len()) - entry.len();
    entry.extend(std::iter::repeat(0u8).take(pad));
    entry
}

pub fn inject_preseed_initrd(input_path: &Path, output_path: &Path, preseed_path: &Path) -> Result<(), String> {
    use std::io::Read;
    let preseed_content = fs::read(preseed_path)
        .map_err(|e| format!("cannot read preseed file {:?}: {}", preseed_path, e))?;

    let compressed_in = fs::read(input_path)
        .map_err(|e| format!("cannot read input initrd {:?}: {}", input_path, e))?;

    println!("[Cube] Reading initrd: {:?} ({} KB)", input_path, compressed_in.len() / 1024);
    let mut decoder = flate2::read::GzDecoder::new(&compressed_in[..]);
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed)
        .map_err(|e| format!("failed to decompress initrd {:?}: {}", input_path, e))?;

    println!("[Cube] Decompressed initrd: {} KB", decompressed.len() / 1024);

    let trailer_needle = b"TRAILER!!!";
    let idx = decompressed
        .windows(trailer_needle.len())
        .rposition(|w| w == trailer_needle)
        .ok_or_else(|| "no CPIO trailer found in decompressed initrd".to_string())?;

    if idx < 110 {
        return Err("corrupt CPIO archive header in initrd".to_string());
    }
    let trailer_pos = idx - 110;

    let new_entry = make_cpio_entry("preseed.cfg", &preseed_content);
    let new_trailer = make_trailer();

    let mut new_cpio = Vec::with_capacity(trailer_pos + new_entry.len() + new_trailer.len() + 512);
    new_cpio.extend_from_slice(&decompressed[..trailer_pos]);
    new_cpio.extend_from_slice(&new_entry);
    new_cpio.extend_from_slice(&new_trailer);

    let pad = (512 - (new_cpio.len() % 512)) % 512;
    new_cpio.extend(std::iter::repeat(0u8).take(pad));

    println!("[Cube] Injected preseed.cfg ({} bytes)", preseed_content.len());

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&new_cpio)
        .map_err(|e| format!("failed to compress preseeded initrd: {}", e))?;
    let recompressed = encoder.finish()
        .map_err(|e| format!("failed to finish gzip compression: {}", e))?;

    fs::write(output_path, &recompressed)
        .map_err(|e| format!("cannot write output initrd {:?}: {}", output_path, e))?;

    println!("[Cube] Written preseeded initrd: {:?} ({} KB)", output_path, recompressed.len() / 1024);
    Ok(())
}

pub fn resolve_freebsd_version(version: &str) -> Result<(&'static str, &'static str), String> {
    match version.trim() {
        "14.4" => Ok(("14.4", "distsets")),
        "14.5" => Ok(("14.5", "distsets")),
        "15" | "15.1" => Ok(("15.1", "pkgbase")),
        "15.0" => Err("FreeBSD 15.0 disc1 has no distribution sets and cannot install pkgbase unattended; use --version 15.1".into()),
        other => Err(format!("unknown FreeBSD version '{other}'; choose 14.4, 14.5, 15.1, or 15")),
    }
}

pub fn ensure_freebsd_iso(version: &str, cache_dir: Option<&Path>) -> Result<PathBuf, String> {
    let (resolved_ver, _) = resolve_freebsd_version(version)?;
    let filename = format!("FreeBSD-{resolved_ver}-RELEASE-arm64-aarch64-disc1.iso");
    let url = format!(
        "https://download.freebsd.org/releases/arm64/aarch64/ISO-IMAGES/{resolved_ver}/{filename}"
    );

    let cache = match cache_dir {
        Some(d) => d.to_path_buf(),
        None => {
            let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
            PathBuf::from(home).join("Library/Caches/vmm")
        }
    };
    fs::create_dir_all(&cache).map_err(|e| format!("cannot create cache directory {:?}: {}", cache, e))?;
    let dest = cache.join(&filename);

    if dest.is_file() {
        if let Ok(meta) = fs::metadata(&dest) {
            if meta.len() >= 100 * 1024 * 1024 {
                return Ok(dest);
            }
        }
    }

    let partial = cache.join(format!("{filename}.partial"));
    println!("[Cube] Downloading official FreeBSD {resolved_ver} disc1 ISO...");
    println!("[Cube] URL: {url}");
    let status = Command::new("curl")
        .args(["-fL", "--retry", "3", "--progress-bar", "-o"])
        .arg(&partial)
        .arg(&url)
        .status()
        .map_err(|e| format!("failed to run curl: {e}"))?;

    if !status.success() {
        let _ = fs::remove_file(&partial);
        return Err(format!("curl failed to download {url}"));
    }

    fs::rename(&partial, &dest).map_err(|e| format!("failed to save ISO to {dest:?}: {e}"))?;
    println!("[Cube] Saved ISO to {}", dest.display());
    Ok(dest)
}

/// Keep this descriptor open for the entire operation. Lock the disk inode so
/// aliases and different cubectl state roots cannot bypass exclusive ownership.
pub fn lock_disk(path: &Path) -> Result<std::fs::File, String> {
    use std::os::fd::AsRawFd;
    let file = OpenOptions::new().read(true).open(path)
        .map_err(|e| format!("cannot open disk {}: {e}", path.display()))?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    require(rc == 0, format!("disk is in use: {}", path.display()))?;
    Ok(file)
}

pub fn lock_resources(resources: &[String]) -> Result<Vec<std::fs::File>, String> {
    resources.iter().filter_map(|r| r.strip_prefix("disk:"))
        .filter(|p| Path::new(p).is_file())
        .map(|p| lock_disk(Path::new(p))).collect()
}
