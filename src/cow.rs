//! Read-only COW format inspection and offline export.
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

pub const MAGIC: &[u8; 8] = b"VMMCOW01";
pub const CLUSTER_SIZE: u64 = 65536;
pub const TABLE_START: u64 = 32;

pub struct Layout {
    pub base_size: u64,
    pub table: Vec<u64>,
    pub physical_clusters: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::{env, process};

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let sequence = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path =
                env::temp_dir().join(format!("cube-overlay-{}-{stamp}-{sequence}", process::id()));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn fixture(dir: &TestDir) -> (PathBuf, PathBuf) {
        let base = dir.path("base.img");
        let overlay = dir.path("base.img.cow");
        fs::write(
            &base,
            [
                &vec![1u8; CLUSTER_SIZE as usize][..],
                &vec![2u8; CLUSTER_SIZE as usize][..],
                &[3u8; 13],
            ]
            .concat(),
        )
        .unwrap();
        let layout = Layout::expected(2 * CLUSTER_SIZE + 13).unwrap();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&(CLUSTER_SIZE as u32).to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&(2 * CLUSTER_SIZE + 13).to_le_bytes());
        bytes.extend_from_slice(&3u64.to_le_bytes());
        bytes.extend_from_slice(&layout.header_size().to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&(layout.header_size() + CLUSTER_SIZE).to_le_bytes());
        bytes.extend_from_slice(&vec![7u8; CLUSTER_SIZE as usize]);
        bytes.extend_from_slice(&vec![9u8; CLUSTER_SIZE as usize]);
        fs::write(&overlay, bytes).unwrap();
        (base, overlay)
    }

    #[test]
    fn inspect_and_export_partial_final_cluster_preserve_sources() {
        let dir = TestDir::new();
        let (base, overlay) = fixture(&dir);
        let original_base = fs::read(&base).unwrap();
        let original_overlay = fs::read(&overlay).unwrap();
        let mut image = CowImage::open(&base, &overlay).unwrap();
        assert_eq!(image.layout.allocated_clusters(), 2);
        assert_eq!(image.layout.unreferenced_clusters(), 0);
        let output = dir.path("export.img");
        image.export(&output).unwrap();
        let exported = fs::read(&output).unwrap();
        assert_eq!(exported.len(), (2 * CLUSTER_SIZE + 13) as usize);
        assert!(exported[..CLUSTER_SIZE as usize]
            .iter()
            .all(|&byte| byte == 7));
        assert!(exported[CLUSTER_SIZE as usize..2 * CLUSTER_SIZE as usize]
            .iter()
            .all(|&byte| byte == 2));
        assert_eq!(&exported[2 * CLUSTER_SIZE as usize..], &[9u8; 13]);
        assert_eq!(fs::read(&base).unwrap(), original_base);
        assert_eq!(fs::read(&overlay).unwrap(), original_overlay);
        assert!(image.export(&output).is_err());
        assert_eq!(fs::read(&output).unwrap(), exported);
    }

    #[test]
    fn active_image_locks_are_respected() {
        let dir = TestDir::new();
        let (base, overlay) = fixture(&dir);
        let writer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&overlay)
            .unwrap();
        writer.try_lock().unwrap();
        assert!(CowImage::open(&base, &overlay)
            .err()
            .unwrap()
            .contains("in use"));
        writer.unlock().unwrap();
        let writer = OpenOptions::new()
            .read(true)
            .write(true)
            .open(&base)
            .unwrap();
        writer.try_lock().unwrap();
        assert!(CowImage::open(&base, &overlay)
            .err()
            .unwrap()
            .contains("in use"));
    }

    #[test]
    fn unreferenced_complete_cluster_is_reported() {
        let dir = TestDir::new();
        let (base, overlay) = fixture(&dir);
        let mut file = OpenOptions::new().append(true).open(&overlay).unwrap();
        file.write_all(&vec![0u8; CLUSTER_SIZE as usize]).unwrap();
        drop(file);
        let image = CowImage::open(&base, &overlay).unwrap();
        assert_eq!(image.layout.allocated_clusters(), 2);
        assert_eq!(image.layout.unreferenced_clusters(), 1);
    }

    #[test]
    fn malformed_overlays_and_wrong_base_are_rejected_without_changes() {
        let dir = TestDir::new();
        let (base, overlay) = fixture(&dir);
        let original = fs::read(&overlay).unwrap();
        let output = dir.path("export.img");
        let mut cases = Vec::new();
        cases.push(Vec::new());
        cases.push(original[..30].to_vec());
        cases.push(original[..original.len() - 1].to_vec());
        let mut bad_magic = original.clone();
        bad_magic[0] = 0;
        cases.push(bad_magic);
        let mut bad_flags = original.clone();
        bad_flags[12] = 1;
        cases.push(bad_flags);
        let mut bad_offset = original.clone();
        bad_offset[32..40].copy_from_slice(&1u64.to_le_bytes());
        cases.push(bad_offset);
        let mut duplicate = original.clone();
        duplicate[40..48].copy_from_slice(&original[32..40]);
        cases.push(duplicate);
        let mut past_end = original.clone();
        past_end[32..40].copy_from_slice(&(original.len() as u64).to_le_bytes());
        cases.push(past_end);
        for bytes in cases {
            fs::write(&overlay, &bytes).unwrap();
            assert!(CowImage::open(&base, &overlay).is_err());
            assert_eq!(fs::read(&overlay).unwrap(), bytes);
            assert!(!output.exists());
        }
        fs::write(&overlay, &original).unwrap();
        let wrong_base = dir.path("wrong.img");
        fs::write(&wrong_base, [0u8; 4]).unwrap();
        assert!(CowImage::open(&wrong_base, &overlay).is_err());
        assert!(CowImage::open(&base, &base).is_err());
    }
}

impl Layout {
    pub fn expected(base_size: u64) -> Result<Self, String> {
        let count = usize::try_from(base_size.div_ceil(CLUSTER_SIZE))
            .ok()
            .filter(|&n| n > 0 && n <= (1 << 24))
            .ok_or("Unsupported COW image size (maximum 1 TiB)")?;
        Ok(Self {
            base_size,
            table: vec![0; count],
            physical_clusters: 0,
        })
    }

    pub fn header_size(&self) -> u64 {
        TABLE_START + self.table.len() as u64 * 8
    }

    pub fn allocated_clusters(&self) -> usize {
        self.table.iter().filter(|&&offset| offset != 0).count()
    }

    pub fn unreferenced_clusters(&self) -> u64 {
        self.physical_clusters - self.allocated_clusters() as u64
    }
}

/// Validate an existing overlay without changing either input file.
pub fn read_layout(file: &mut File, base_size: u64) -> Result<Layout, String> {
    let mut layout = Layout::expected(base_size)?;
    let len = file.metadata().map_err(|e| e.to_string())?.len();
    let header_size = layout.header_size();
    if len < header_size || (len - header_size) % CLUSTER_SIZE != 0 {
        return Err("Truncated COW overlay".into());
    }
    layout.physical_clusters = (len - header_size) / CLUSTER_SIZE;
    file.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut header = [0; 32];
    file.read_exact(&mut header).map_err(|e| e.to_string())?;
    if &header[..8] != MAGIC
        || u32::from_le_bytes(header[8..12].try_into().unwrap()) != CLUSTER_SIZE as u32
        || u32::from_le_bytes(header[12..16].try_into().unwrap()) != 0
        || u64::from_le_bytes(header[16..24].try_into().unwrap()) != base_size
        || u64::from_le_bytes(header[24..32].try_into().unwrap()) != layout.table.len() as u64
    {
        return Err("COW header/geometry does not match base image".into());
    }
    let mut allocated = HashSet::new();
    for entry in &mut layout.table {
        let mut bytes = [0; 8];
        file.read_exact(&mut bytes).map_err(|e| e.to_string())?;
        *entry = u64::from_le_bytes(bytes);
        if *entry != 0
            && (*entry < header_size
                || (*entry - header_size) % CLUSTER_SIZE != 0
                || entry.checked_add(CLUSTER_SIZE).is_none_or(|end| end > len)
                || !allocated.insert(*entry))
        {
            return Err("Invalid or overlapping COW cluster offset".into());
        }
    }
    Ok(layout)
}

pub struct CowImage {
    base: File,
    overlay: File,
    pub layout: Layout,
}

impl Drop for CowImage {
    fn drop(&mut self) {
        let _ = self.overlay.unlock();
        let _ = self.base.unlock();
    }
}

impl CowImage {
    pub fn open(base_path: &Path, overlay_path: &Path) -> Result<Self, String> {
        let base = OpenOptions::new()
            .read(true)
            .open(base_path)
            .map_err(|e| format!("Cannot open base '{}': {e}", base_path.display()))?;
        base.try_lock_shared()
            .map_err(|e| format!("Base image is in use: {e}"))?;
        let overlay = OpenOptions::new()
            .read(true)
            .open(overlay_path)
            .map_err(|e| format!("Cannot open overlay '{}': {e}", overlay_path.display()))?;
        overlay
            .try_lock_shared()
            .map_err(|e| format!("Overlay is in use: {e}"))?;
        let base_meta = base.metadata().map_err(|e| e.to_string())?;
        let overlay_meta = overlay.metadata().map_err(|e| e.to_string())?;
        if base_meta.dev() == overlay_meta.dev() && base_meta.ino() == overlay_meta.ino() {
            return Err("Base and overlay refer to the same file".into());
        }
        let mut overlay = overlay;
        let layout = read_layout(&mut overlay, base_meta.len())?;
        Ok(Self {
            base,
            overlay,
            layout,
        })
    }

    /// Publish only a complete image. Existing destinations are never replaced.
    pub fn export(&mut self, destination: &Path) -> Result<(), String> {
        if destination.exists() {
            return Err(format!(
                "Destination already exists: {}",
                destination.display()
            ));
        }
        let parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let name = destination
            .file_name()
            .ok_or("Destination needs a file name")?;
        let mut temporary = None;
        for attempt in 0..16 {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| e.to_string())?
                .as_nanos();
            let path = parent.join(format!(
                ".{}.vmm-export-{}-{stamp}-{attempt}",
                name.to_string_lossy(),
                std::process::id()
            ));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => {
                    temporary = Some((path, file));
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("Cannot create temporary export: {e}")),
            }
        }
        let (path, mut output) = temporary.ok_or("Cannot reserve a temporary export path")?;
        let result = (|| -> Result<(), String> {
            self.base
                .seek(SeekFrom::Start(0))
                .map_err(|e| e.to_string())?;
            let copied = std::io::copy(&mut self.base, &mut output).map_err(|e| e.to_string())?;
            if copied != self.layout.base_size {
                return Err("Base image size changed during export".into());
            }
            let mut buffer = vec![0; CLUSTER_SIZE as usize];
            for (index, &offset) in self.layout.table.iter().enumerate() {
                if offset == 0 {
                    continue;
                }
                let start = index as u64 * CLUSTER_SIZE;
                let count = (self.layout.base_size - start).min(CLUSTER_SIZE) as usize;
                self.overlay
                    .seek(SeekFrom::Start(offset))
                    .map_err(|e| e.to_string())?;
                self.overlay
                    .read_exact(&mut buffer[..count])
                    .map_err(|e| e.to_string())?;
                output
                    .seek(SeekFrom::Start(start))
                    .map_err(|e| e.to_string())?;
                output
                    .write_all(&buffer[..count])
                    .map_err(|e| e.to_string())?;
            }
            output
                .set_len(self.layout.base_size)
                .map_err(|e| e.to_string())?;
            output.sync_all().map_err(|e| e.to_string())?;
            fs::hard_link(&path, destination).map_err(|e| format!("Cannot publish export: {e}"))?;
            Ok(())
        })();
        drop(output);
        let _ = fs::remove_file(&path);
        result
    }
}
