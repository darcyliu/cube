//! FreeBSD loader preload metadata (MODINFO) serializer.
//!
//! On ARM64, `parse_boot_param()` treats X0 as either an FDT blob (and fakes a
//! kernel-only preload table) or a loader MODINFO stream. Root-on-ZFS needs the
//! latter so `zfs.ko` is linked before `vfs_mountroot`.
//!
//! Record layout (`sys/sys/linker.h`): `{ u32 type; u32 size; u8 val[size]; }`
//! padded to 8 bytes (`sizeof(u_long)` on aarch64).
//!
//! `MODINFOMD_DTBP` is **0x1000** on arm/arm64 (not 0x000e). Combined with
//! `MODINFO_METADATA` that is type `0x9000`.

#![allow(dead_code)]

pub const MODINFO_END: u32 = 0x0000;
pub const MODINFO_NAME: u32 = 0x0001;
pub const MODINFO_TYPE: u32 = 0x0002;
pub const MODINFO_ADDR: u32 = 0x0003;
pub const MODINFO_SIZE: u32 = 0x0004;
pub const MODINFO_METADATA: u32 = 0x8000;

pub const MODINFOMD_ENVP: u32 = 0x0006;
pub const MODINFOMD_HOWTO: u32 = 0x0007;
pub const MODINFOMD_KERNEND: u32 = 0x0008;
/// ARM64 DTB pointer (`sys/sys/linker.h`).
pub const MODINFOMD_DTBP: u32 = 0x1000;

pub const RB_SINGLE: i32 = 0x0000_0002;
pub const RB_VERBOSE: i32 = 0x0000_0800;
pub const RB_SERIAL: i32 = 0x0000_1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleKind {
    Kernel,
    Module,
}

#[derive(Debug, Clone)]
pub struct PreloadedFile {
    pub name: String,
    pub kind: ModuleKind,
    pub gpa: u64,
    pub size: u64,
}

struct Builder {
    buf: Vec<u8>,
}

impl Builder {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }

    fn align8(&mut self) {
        while self.buf.len() % 8 != 0 {
            self.buf.push(0);
        }
    }

    fn push_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn push_record(&mut self, typ: u32, data: &[u8]) {
        self.push_u32(typ);
        self.push_u32(data.len() as u32);
        self.buf.extend_from_slice(data);
        self.align8();
    }

    fn push_string(&mut self, typ: u32, s: &str) {
        let mut data = s.as_bytes().to_vec();
        data.push(0);
        self.push_record(typ, &data);
    }

    fn push_u64_field(&mut self, typ: u32, v: u64) {
        self.push_record(typ, &v.to_le_bytes());
    }
}

pub fn encode_kenv(vars: &[&str]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in vars {
        out.extend_from_slice(v.as_bytes());
        out.push(0);
    }
    out.push(0);
    out
}

fn push_file(b: &mut Builder, f: &PreloadedFile) {
    let kind = match f.kind {
        ModuleKind::Kernel => "elf kernel",
        ModuleKind::Module => "elf obj module",
    };
    b.push_string(MODINFO_NAME, &f.name);
    b.push_string(MODINFO_TYPE, kind);
    b.push_u64_field(MODINFO_ADDR, f.gpa);
    b.push_u64_field(MODINFO_SIZE, f.size);
}

/// Serialize a loader metadata table whose ENVP/KERNEND pointers are GPAs.
///
/// Kernel-scoped records (DTBP, HOWTO, KERNEND, ENVP) MUST sit between the
/// kernel's SIZE and the next MODINFO_NAME. `preload_search_info(kmdp, …)`
/// stops at the following NAME, so trailing metadata is invisible and
/// `KERNEND==0` makes `freebsd_parse_boot_param` fall back to a DTB-less
/// fake preload (silent hang, no UART).
///
/// `metadata_gpa` is where the returned blob will be written (for ENVP).
pub fn build_preload(
    files: &[PreloadedFile],
    dtb_gpa: u64,
    kenv: &[u8],
    howto: i32,
    metadata_gpa: u64,
) -> Vec<u8> {
    let mut b = Builder::new();

    let (kernel, rest) = match files.split_first() {
        Some((k, r)) => (k, r),
        None => panic!("build_preload requires a kernel PreloadedFile first"),
    };
    push_file(&mut b, kernel);

    // Match locore fake_preload: NAME/TYPE/ADDR/SIZE + DTBP + KERNEND.
    // Do NOT emit ENVP/HOWTO here. parse_boot_param calls init_static_kenv(envp)
    // *before* UART exists; a bad ENVP pointer aborts with a blank console.
    // Bootargs/howto come from the DTB (parse_fdt_bootargs) after try_load_dtb.
    let _ = (kenv, howto);
    b.push_u64_field(MODINFO_METADATA | MODINFOMD_DTBP, dtb_gpa);

    let kernend_off = b.buf.len();
    b.push_u64_field(MODINFO_METADATA | MODINFOMD_KERNEND, 0);

    for f in rest {
        push_file(&mut b, f);
    }

    b.push_u32(MODINFO_END);
    b.push_u32(0);
    b.align8();

    let kernend = metadata_gpa + b.buf.len() as u64;
    b.buf[kernend_off + 8..kernend_off + 16].copy_from_slice(&kernend.to_le_bytes());

    b.buf
}

pub fn parse_records(buf: &[u8]) -> Vec<(u32, Vec<u8>)> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 8 <= buf.len() {
        let typ = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
        let size = u32::from_le_bytes(buf[off + 4..off + 8].try_into().unwrap()) as usize;
        if typ == 0 && size == 0 {
            break;
        }
        let start = off + 8;
        let end = start.saturating_add(size);
        if end > buf.len() {
            break;
        }
        out.push((typ, buf[start..end].to_vec()));
        off = (end + 7) & !7;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_plus_zfs_records() {
        let files = [
            PreloadedFile {
                name: "kernel".into(),
                kind: ModuleKind::Kernel,
                gpa: 0x4020_0000,
                size: 0x1000,
            },
            PreloadedFile {
                name: "zfs".into(),
                kind: ModuleKind::Module,
                gpa: 0x4100_0000,
                size: 0x2000,
            },
        ];
        let kenv = encode_kenv(&["vfs.root.mountfrom=zfs:zroot/ROOT/default"]);
        let meta_gpa = 0x4400_0000u64;
        let blob = build_preload(
            &files,
            0x4800_0000,
            &kenv,
            RB_VERBOSE | RB_SERIAL | RB_SINGLE,
            meta_gpa,
        );

        assert_eq!(blob.len() % 8, 0);
        let recs = parse_records(&blob);
        assert_eq!(recs[0], (MODINFO_NAME, b"kernel\0".to_vec()));
        assert_eq!(recs[1], (MODINFO_TYPE, b"elf kernel\0".to_vec()));
        assert_eq!(recs[2].0, MODINFO_ADDR);
        assert_eq!(u64::from_le_bytes(recs[2].1.clone().try_into().unwrap()), 0x4020_0000);
        assert_eq!(recs[3].0, MODINFO_SIZE);
        assert_eq!(u64::from_le_bytes(recs[3].1.clone().try_into().unwrap()), 0x1000);

        // Kernel-scoped metadata must precede the next MODINFO_NAME (zfs).
        let zfs_name = recs.iter().position(|(t, v)| *t == MODINFO_NAME && v == b"zfs\0").unwrap();
        let dtbp_at = recs.iter().position(|(t, _)| *t == MODINFO_METADATA | MODINFOMD_DTBP).unwrap();
        let kernend_at = recs.iter().position(|(t, _)| *t == MODINFO_METADATA | MODINFOMD_KERNEND).unwrap();
        assert!(dtbp_at < zfs_name && kernend_at < zfs_name);
        assert!(recs.iter().all(|(t, _)| *t != MODINFO_METADATA | MODINFOMD_ENVP));
        assert!(recs.iter().all(|(t, _)| *t != MODINFO_METADATA | MODINFOMD_HOWTO));

        assert_eq!(recs[zfs_name], (MODINFO_NAME, b"zfs\0".to_vec()));
        assert_eq!(recs[zfs_name + 1], (MODINFO_TYPE, b"elf obj module\0".to_vec()));
        assert_eq!(recs[zfs_name + 2].0, MODINFO_ADDR);
        assert_eq!(
            u64::from_le_bytes(recs[zfs_name + 2].1.clone().try_into().unwrap()),
            0x4100_0000
        );

        let dtbp = &recs[dtbp_at];
        assert_eq!(u64::from_le_bytes(dtbp.1.clone().try_into().unwrap()), 0x4800_0000);

        let kernend = recs.iter().find(|(t, _)| *t == MODINFO_METADATA | MODINFOMD_KERNEND).unwrap();
        let end = u64::from_le_bytes(kernend.1.clone().try_into().unwrap());
        assert_eq!(end, meta_gpa + blob.len() as u64);
        assert!(end > meta_gpa);
    }

    #[test]
    fn kenv_double_nul() {
        let k = encode_kenv(&["a=1", "b=2"]);
        assert_eq!(&k, b"a=1\0b=2\0\0");
    }
}
