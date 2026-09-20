//! GPT partition scanning for FreeBSD root (UFS / ZFS).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

/// FreeBSD UFS GUID: 516e7cb6-6ecf-11d6-8ff8-00022d09712b (on-disk little-endian)
pub const FREEBSD_UFS_GUID: [u8; 16] = [
    0xb6, 0x7c, 0x6e, 0x51, 0xcf, 0x6e, 0xd6, 0x11, 0x8f, 0xf8, 0x00, 0x02, 0x2d, 0x09, 0x71, 0x2b,
];
/// FreeBSD ZFS GUID: 516e7cba-6ecf-11d6-8ff8-00022d09712b
pub const FREEBSD_ZFS_GUID: [u8; 16] = [
    0xba, 0x7c, 0x6e, 0x51, 0xcf, 0x6e, 0xd6, 0x11, 0x8f, 0xf8, 0x00, 0x02, 0x2d, 0x09, 0x71, 0x2b,
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreebsdRoot {
    /// 1-based GPT partition index, e.g. `ufs:/dev/vtbd0p2`
    Ufs { part: u32 },
    /// bsdinstall default dataset; override with `--root`
    Zfs,
}

/// Scan a GPT partition-entry array (not the protective MBR / header).
pub fn detect_freebsd_root(entries: &[u8], entry_size: usize) -> Option<FreebsdRoot> {
    if entry_size < 16 {
        return None;
    }
    let mut found_zfs = false;
    let n = entries.len() / entry_size;
    for idx in 0..n {
        let entry = &entries[idx * entry_size..(idx + 1) * entry_size];
        let type_guid: [u8; 16] = match entry[0..16].try_into() {
            Ok(g) => g,
            Err(_) => continue,
        };
        if type_guid == FREEBSD_UFS_GUID {
            return Some(FreebsdRoot::Ufs {
                part: (idx as u32) + 1,
            });
        }
        if type_guid == FREEBSD_ZFS_GUID {
            found_zfs = true;
        }
    }
    if found_zfs {
        Some(FreebsdRoot::Zfs)
    } else {
        None
    }
}

fn guid_label(guid: &[u8; 16]) -> &'static str {
    if guid == &FREEBSD_UFS_GUID {
        "freebsd-ufs"
    } else if guid == &FREEBSD_ZFS_GUID {
        "freebsd-zfs"
    } else if guid == &FREEBSD_SWAP_GUID {
        "freebsd-swap"
    } else if guid == &EFI_SYSTEM_GUID {
        "efi"
    } else {
        "other"
    }
}

pub const FREEBSD_SWAP_GUID: [u8; 16] = [
    0xb5, 0x7c, 0x6e, 0x51, 0xcf, 0x6e, 0xd6, 0x11, 0x8f, 0xf8, 0x00, 0x02, 0x2d, 0x09, 0x71, 0x2b,
];
pub const EFI_SYSTEM_GUID: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];

fn read_gpt_entries(disk_path: &str) -> Option<(Vec<u8>, usize)> {
    let mut file = File::open(disk_path).ok()?;
    let mut sector = [0u8; 512];
    file.seek(SeekFrom::Start(512)).ok()?;
    file.read_exact(&mut sector).ok()?;
    if &sector[0..8] != b"EFI PART" {
        return None;
    }
    let part_lba = u64::from_le_bytes(sector[72..80].try_into().ok()?);
    let num_entries = u32::from_le_bytes(sector[80..84].try_into().ok()?);
    let entry_size = u32::from_le_bytes(sector[84..88].try_into().ok()?) as usize;
    if entry_size < 128 || num_entries == 0 {
        return None;
    }
    file.seek(SeekFrom::Start(part_lba * 512)).ok()?;
    let read_entries = (num_entries as usize).min(128);
    let mut entries_buf = vec![0u8; read_entries * entry_size];
    file.read_exact(&mut entries_buf).ok()?;
    Some((entries_buf, entry_size))
}

pub fn describe_freebsd_gpt(disk_path: &str) -> Option<String> {
    let (entries, entry_size) = read_gpt_entries(disk_path)?;
    let n = entries.len() / entry_size;
    let mut parts = Vec::new();
    for idx in 0..n {
        let entry = &entries[idx * entry_size..(idx + 1) * entry_size];
        let type_guid: [u8; 16] = match entry[0..16].try_into() {
            Ok(g) => g,
            Err(_) => continue,
        };
        let first = u64::from_le_bytes(entry[32..40].try_into().ok()?);
        if first == 0 {
            continue;
        }
        parts.push(format!("p{}={}", idx + 1, guid_label(&type_guid)));
    }
    Some(parts.join(" "))
}

pub fn detect_freebsd_root_from_disk(disk_path: &str) -> Option<FreebsdRoot> {
    let (entries, entry_size) = read_gpt_entries(disk_path)?;
    detect_freebsd_root(&entries, entry_size)
}

pub fn freebsd_root_to_mountfrom(root: &FreebsdRoot) -> String {
    match root {
        FreebsdRoot::Ufs { part } => format!("ufs:/dev/vtbd0p{part}"),
        FreebsdRoot::Zfs => "zfs:zroot/ROOT/default".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(guid: &[u8; 16]) -> [u8; 128] {
        let mut e = [0u8; 128];
        e[0..16].copy_from_slice(guid);
        e
    }

    #[test]
    fn ufs_at_p2() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&entry(&[0; 16])); // ESP / unused
        buf.extend_from_slice(&entry(&FREEBSD_UFS_GUID));
        assert_eq!(
            detect_freebsd_root(&buf, 128),
            Some(FreebsdRoot::Ufs { part: 2 })
        );
        assert_eq!(
            freebsd_root_to_mountfrom(&FreebsdRoot::Ufs { part: 2 }),
            "ufs:/dev/vtbd0p2"
        );
    }

    #[test]
    fn ufs_at_p1() {
        let buf = entry(&FREEBSD_UFS_GUID);
        assert_eq!(
            detect_freebsd_root(&buf, 128),
            Some(FreebsdRoot::Ufs { part: 1 })
        );
    }

    #[test]
    fn zfs_only() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&entry(&[0; 16])); // ESP
        buf.extend_from_slice(&entry(&[
            0xb5, 0x7c, 0x6e, 0x51, 0xcf, 0x6e, 0xd6, 0x11, 0x8f, 0xf8, 0x00, 0x02, 0x2d, 0x09,
            0x71, 0x2b,
        ])); // swap
        buf.extend_from_slice(&entry(&FREEBSD_ZFS_GUID));
        assert_eq!(detect_freebsd_root(&buf, 128), Some(FreebsdRoot::Zfs));
        assert_eq!(
            freebsd_root_to_mountfrom(&FreebsdRoot::Zfs),
            "zfs:zroot/ROOT/default"
        );
    }

    #[test]
    fn ufs_preferred_over_zfs() {
        let mut buf = Vec::new();
        buf.extend_from_slice(&entry(&FREEBSD_UFS_GUID));
        buf.extend_from_slice(&entry(&FREEBSD_ZFS_GUID));
        assert_eq!(
            detect_freebsd_root(&buf, 128),
            Some(FreebsdRoot::Ufs { part: 1 })
        );
    }

    #[test]
    fn empty_gpt() {
        assert_eq!(detect_freebsd_root(&[0u8; 128], 128), None);
    }
}
