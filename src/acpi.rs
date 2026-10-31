//! ACPI tables, e820, and QEMU bios-linker-loader records.
//! `_PRT` lists only the INTx pins passed to [`build_with_prt`].

use crate::devices::pci::{PMBA_DEFAULT, pm_timer_port};

const OEM_ID: &[u8; 6] = b"VMMINT";
const OEM_TABLE: &[u8; 8] = b"VMMINTEL";

pub struct FirmwareFiles {
    pub e820: Vec<u8>,
    pub rsdp: Vec<u8>,
    pub tables: Vec<u8>,
    pub loader: Vec<u8>,
}

pub fn build(ncpus: u32, ram_size: u64) -> FirmwareFiles {
    build_with_prt(ncpus, ram_size, &[])
}

/// `prt` entries are `(slot << 16 | function, gsi)` for each INTA pin.
pub fn build_with_prt(ncpus: u32, ram_size: u64, prt: &[(u32, u32)]) -> FirmwareFiles {
    let dsdt = build_dsdt(prt);
    let madt = build_madt(ncpus);
    let mut fadt = vec![0u8; 276];
    let fadt_off = 52usize;
    let dsdt_off = fadt_off + fadt.len();
    let madt_off = dsdt_off + dsdt.len();
    fill_fadt(&mut fadt, dsdt_off as u64);
    let mut tables = vec![0u8; fadt_off];
    fill_xsdt(&mut tables, fadt_off as u64, madt_off as u64);
    tables.extend_from_slice(&fadt);
    tables.extend_from_slice(&dsdt);
    tables.extend_from_slice(&madt);
    checksum_table(&mut tables, 0);
    checksum_table(&mut tables, fadt_off);
    checksum_table(&mut tables, dsdt_off);
    checksum_table(&mut tables, madt_off);
    let rsdp = build_rsdp();
    let loader = build_loader(&tables, fadt_off, dsdt_off, madt_off);
    FirmwareFiles {
        e820: e820_bytes(ram_size),
        rsdp,
        tables,
        loader,
    }
}

pub fn e820_bytes(ram_size: u64) -> Vec<u8> {
    e820_reserving(ram_size, 0, 0)
}

/// Carve `[reserved_base, reserved_base + reserved_len)` out of the high usable
/// range. A zero length keeps the three-entry map `e820_bytes` has always returned.
pub fn e820_reserving(ram_size: u64, reserved_base: u64, reserved_len: u64) -> Vec<u8> {
    let mut out = Vec::with_capacity(80);
    push_e820(&mut out, 0, 640 * 1024, 1);
    push_e820(&mut out, 0xA_0000, 384 * 1024, 2);
    let high = 0x10_0000u64;
    let reserved_end = reserved_base.saturating_add(reserved_len);
    let carve = reserved_len > 0
        && reserved_base > high
        && reserved_end <= ram_size
        && reserved_base < reserved_end;
    if !carve {
        push_e820(&mut out, high, ram_size.saturating_sub(high), 1);
        return out;
    }
    push_e820(&mut out, high, reserved_base - high, 1);
    push_e820(&mut out, reserved_base, reserved_len, 2);
    if reserved_end < ram_size {
        push_e820(&mut out, reserved_end, ram_size - reserved_end, 1);
    }
    out
}

fn push_e820(out: &mut Vec<u8>, addr: u64, size: u64, kind: u32) {
    out.extend_from_slice(&addr.to_le_bytes());
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&kind.to_le_bytes());
}

fn build_rsdp() -> Vec<u8> {
    let mut r = vec![0u8; 36];
    r[..8].copy_from_slice(b"RSD PTR ");
    r[9..15].copy_from_slice(OEM_ID);
    r[15] = 2;
    r[20..24].copy_from_slice(&36u32.to_le_bytes());
    checksum_range(&mut r, 8, 0, 20);
    checksum_range(&mut r, 32, 0, 36);
    r
}

fn fill_xsdt(buf: &mut [u8], fadt_off: u64, madt_off: u64) {
    write_header(&mut buf[..36], *b"XSDT", 52, 1);
    buf[36..44].copy_from_slice(&fadt_off.to_le_bytes());
    buf[44..52].copy_from_slice(&madt_off.to_le_bytes());
}

fn fill_fadt(fadt: &mut [u8], dsdt_off: u64) {
    write_header(&mut fadt[..36], *b"FACP", 276, 6);
    let pm_tmr = u32::from(pm_timer_port(PMBA_DEFAULT) as u16);
    fadt[46..48].copy_from_slice(&9u16.to_le_bytes()); // SCI_INT
    fadt[56..60].copy_from_slice(&0xB000u32.to_le_bytes());
    fadt[64..68].copy_from_slice(&0xB004u32.to_le_bytes());
    fadt[76..80].copy_from_slice(&pm_tmr.to_le_bytes());
    fadt[88] = 4;
    fadt[89] = 2;
    fadt[91] = 4;
    fadt[108] = 0x32;
    fadt[109] = 0x02; // 8042, VGA-not-present clear
    // WBINVD | PROC_C1 | SLP_BUTTON | RESET_REG_SUP. Reduced-hardware stays clear.
    fadt[112..116].copy_from_slice(&0x425u32.to_le_bytes());
    write_gas(&mut fadt[116..128], 0xCF9, 8, 1);
    fadt[128] = 0x06;
    fadt[140..148].copy_from_slice(&dsdt_off.to_le_bytes());
    fadt[40..44].copy_from_slice(&(dsdt_off as u32).to_le_bytes());
    write_gas(&mut fadt[148..160], 0xB000, 32, 2);
    write_gas(&mut fadt[172..184], 0xB004, 16, 2);
    write_gas(&mut fadt[208..220], u64::from(pm_tmr), 32, 3);
}

fn write_gas(buf: &mut [u8], address: u64, bit_width: u8, access: u8) {
    buf[0] = 1; // system I/O
    buf[1] = bit_width;
    buf[3] = access;
    buf[4..12].copy_from_slice(&address.to_le_bytes());
}

fn build_madt(ncpus: u32) -> Vec<u8> {
    let n = ncpus.clamp(1, 16) as usize;
    let mut m = vec![0u8; 44 + 8 * n];
    let len = m.len() as u32;
    write_header(&mut m[..36], *b"APIC", len, 1);
    // IOAPIC and overrides are appended after the local-APIC entries.
    m[4..8].copy_from_slice(&len.to_le_bytes());
    m[36..40].copy_from_slice(&0xFEE0_0000u32.to_le_bytes());
    m[40..44].copy_from_slice(&1u32.to_le_bytes()); // PCAT_COMPAT
    for cpu in 0..n {
        let at = 44 + cpu * 8;
        m[at] = 0;
        m[at + 1] = 8;
        m[at + 2] = cpu as u8;
        m[at + 3] = cpu as u8;
        m[at + 4] = 1;
    }
    let mut io = vec![0u8; 12];
    io[0] = 1;
    io[1] = 12;
    io[4..8].copy_from_slice(&0xFEC0_0000u32.to_le_bytes());
    m.extend_from_slice(&io);
    m.extend_from_slice(&override_irq(0, 2, 0));
    m.extend_from_slice(&override_irq(9, 9, 0x000F));
    let len = m.len() as u32;
    m[4..8].copy_from_slice(&len.to_le_bytes());
    m
}

fn override_irq(src: u8, gsi: u32, flags: u16) -> [u8; 10] {
    let mut e = [0u8; 10];
    e[0] = 2;
    e[1] = 10;
    e[3] = src;
    e[4..8].copy_from_slice(&gsi.to_le_bytes());
    e[8..10].copy_from_slice(&flags.to_le_bytes());
    e
}

fn build_dsdt(prt: &[(u32, u32)]) -> Vec<u8> {
    let mut aml = Vec::new();
    aml.extend(name_pkg(*b"_S5_"));
    aml.extend(name_pkg(*b"_S0_"));
    let mut pci_body = hid_crs(0x030A_D041, &pci_crs());
    if !prt.is_empty() {
        pci_body.extend(name_prt(prt));
    }
    let mut body = Vec::new();
    body.extend(device(*b"PCI0", &pci_body));
    body.extend(device(*b"COM1", &hid_crs(0x0105_D041, &com_crs())));
    body.extend(device(*b"RTC_", &hid_crs(0x000B_D041, &rtc_crs())));
    aml.extend(scope_sb(&body));
    let mut dsdt = vec![0u8; 36 + aml.len()];
    let len = dsdt.len() as u32;
    write_header(&mut dsdt[..36], *b"DSDT", len, 2);
    dsdt[36..].copy_from_slice(&aml);
    dsdt
}

fn pci_crs() -> Vec<u8> {
    let mut c = Vec::new();
    c.extend(word_desc(2, 0, 0, 1));
    c.extend(word_desc(1, 0x0000, 0x0CF7, 0x0CF8));
    c.extend(word_desc(1, 0x0D00, 0xFFFF, 0xF300));
    c.extend(dword_mem(0xC000_0000, 0xFEBF_FFFF, 0x3EC0_0000));
    c.extend_from_slice(&[0x79, 0x00]);
    c
}

fn com_crs() -> Vec<u8> {
    let mut c = io_port(0x3F8, 8);
    c.extend_from_slice(&[0x22, 0x10, 0x00, 0x79, 0x00]);
    c
}

fn rtc_crs() -> Vec<u8> {
    let mut c = io_port(0x70, 2);
    c.extend_from_slice(&[0x79, 0x00]);
    c
}

fn io_port(base: u16, len: u8) -> Vec<u8> {
    let mut v = vec![0x47, 0x01];
    v.extend_from_slice(&base.to_le_bytes());
    v.extend_from_slice(&base.to_le_bytes());
    v.push(0);
    v.push(len);
    v
}

fn word_desc(kind: u8, min: u16, max: u16, len: u16) -> Vec<u8> {
    let mut v = vec![0x88, 0x0D, 0x00, kind, 0x0C, 0x00];
    v.extend_from_slice(&0u16.to_le_bytes());
    v.extend_from_slice(&min.to_le_bytes());
    v.extend_from_slice(&max.to_le_bytes());
    v.extend_from_slice(&0u16.to_le_bytes());
    v.extend_from_slice(&len.to_le_bytes());
    v
}

fn dword_mem(min: u32, max: u32, len: u32) -> Vec<u8> {
    let mut v = vec![0x87, 0x17, 0x00, 0x00, 0x0C, 0x01];
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(&min.to_le_bytes());
    v.extend_from_slice(&max.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(&len.to_le_bytes());
    v
}

fn name_prt(entries: &[(u32, u32)]) -> Vec<u8> {
    let mut body = Vec::new();
    for &(addr, gsi) in entries {
        body.extend(prt_entry(addr, gsi));
    }
    let mut v = vec![0x08];
    v.extend(*b"_PRT");
    v.extend(aml_pkg(entries.len() as u8, &body));
    v
}

fn prt_entry(addr: u32, gsi: u32) -> Vec<u8> {
    let mut body = vec![0x0C];
    body.extend_from_slice(&addr.to_le_bytes());
    body.push(0x00); // INTA
    body.push(0x00); // GSI in the source index, not a link device
    body.push(0x0A);
    body.push(gsi as u8);
    aml_pkg(4, &body)
}

fn aml_pkg(count: u8, body: &[u8]) -> Vec<u8> {
    let mut after = vec![count];
    after.extend_from_slice(body);
    let mut v = vec![0x12];
    v.extend(pkg_len(after.len()));
    v.extend(after);
    v
}

fn name_pkg(name: [u8; 4]) -> Vec<u8> {
    let mut v = vec![0x08];
    v.extend(name);
    v.extend_from_slice(&[0x12, 0x04, 0x02, 0x00, 0x00]);
    v
}

fn name_dword(name: [u8; 4], value: u32) -> Vec<u8> {
    let mut v = vec![0x08];
    v.extend(name);
    v.push(0x0C);
    v.extend_from_slice(&value.to_le_bytes());
    v
}

fn hid_crs(eisa: u32, crs: &[u8]) -> Vec<u8> {
    let mut v = name_dword(*b"_HID", eisa);
    v.push(0x08);
    v.extend(*b"_CRS");
    v.extend(buffer(crs));
    v
}

fn buffer(data: &[u8]) -> Vec<u8> {
    let size_term = [0x0A, data.len() as u8];
    let mut after = size_term.to_vec();
    after.extend_from_slice(data);
    let mut v = vec![0x11];
    v.extend(pkg_len(after.len()));
    v.extend(after);
    v
}

fn device(name: [u8; 4], body: &[u8]) -> Vec<u8> {
    let mut after = name.to_vec();
    after.extend_from_slice(body);
    let mut v = vec![0x5B, 0x82];
    v.extend(pkg_len(after.len()));
    v.extend(after);
    v
}

fn scope_sb(body: &[u8]) -> Vec<u8> {
    let mut after = vec![0x5C];
    after.extend(*b"_SB_");
    after.extend_from_slice(body);
    let mut v = vec![0x10];
    v.extend(pkg_len(after.len()));
    v.extend(after);
    v
}

fn pkg_len(payload: usize) -> Vec<u8> {
    let one = 1 + payload;
    if one < 64 {
        return vec![one as u8];
    }
    let two = 2 + payload;
    vec![0x40 | ((two & 0x0f) as u8), (two >> 4) as u8]
}

fn write_header(buf: &mut [u8], sig: [u8; 4], length: u32, rev: u8) {
    buf[0..4].copy_from_slice(&sig);
    buf[4..8].copy_from_slice(&length.to_le_bytes());
    buf[8] = rev;
    buf[10..16].copy_from_slice(OEM_ID);
    buf[16..24].copy_from_slice(OEM_TABLE);
    buf[24..28].copy_from_slice(&1u32.to_le_bytes());
    buf[28..32].copy_from_slice(b"VMM ");
    buf[32..36].copy_from_slice(&1u32.to_le_bytes());
}

fn checksum_table(buf: &mut [u8], table: usize) {
    let len = u32::from_le_bytes(buf[table + 4..table + 8].try_into().unwrap()) as usize;
    checksum_range(buf, table + 9, table, len);
}

fn checksum_range(buf: &mut [u8], at: usize, start: usize, len: usize) {
    buf[at] = 0;
    let sum = buf[start..start + len]
        .iter()
        .fold(0u8, |acc, b| acc.wrapping_add(*b));
    buf[at] = 0u8.wrapping_sub(sum);
}

fn build_loader(tables: &[u8], fadt_off: usize, dsdt_off: usize, madt_off: usize) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend(alloc("etc/acpi/tables", 64, 1));
    out.extend(alloc("etc/acpi/rsdp", 16, 2));
    out.extend(pointer("etc/acpi/rsdp", "etc/acpi/tables", 24, 8));
    out.extend(pointer("etc/acpi/tables", "etc/acpi/tables", 36, 8));
    out.extend(pointer("etc/acpi/tables", "etc/acpi/tables", 44, 8));
    out.extend(pointer(
        "etc/acpi/tables",
        "etc/acpi/tables",
        (fadt_off + 40) as u32,
        4,
    ));
    out.extend(pointer(
        "etc/acpi/tables",
        "etc/acpi/tables",
        (fadt_off + 140) as u32,
        8,
    ));
    out.extend(cksum("etc/acpi/rsdp", 8, 0, 20));
    out.extend(cksum("etc/acpi/rsdp", 32, 0, 36));
    out.extend(cksum("etc/acpi/tables", 9, 0, 52));
    out.extend(cksum(
        "etc/acpi/tables",
        (fadt_off + 9) as u32,
        fadt_off as u32,
        276,
    ));
    let dsdt_len = u32::from_le_bytes(tables[dsdt_off + 4..dsdt_off + 8].try_into().unwrap());
    out.extend(cksum(
        "etc/acpi/tables",
        (dsdt_off + 9) as u32,
        dsdt_off as u32,
        dsdt_len,
    ));
    let madt_len = u32::from_le_bytes(tables[madt_off + 4..madt_off + 8].try_into().unwrap());
    out.extend(cksum(
        "etc/acpi/tables",
        (madt_off + 9) as u32,
        madt_off as u32,
        madt_len,
    ));
    out
}

fn alloc(name: &str, align: u32, zone: u8) -> [u8; 128] {
    let mut e = [0u8; 128];
    e[0] = 1;
    put_name(&mut e[4..60], name);
    e[60..64].copy_from_slice(&align.to_le_bytes());
    e[64] = zone;
    e
}

fn pointer(dest: &str, src: &str, offset: u32, size: u8) -> [u8; 128] {
    let mut e = [0u8; 128];
    e[0] = 2;
    put_name(&mut e[4..60], dest);
    put_name(&mut e[60..116], src);
    e[116..120].copy_from_slice(&offset.to_le_bytes());
    e[120] = size;
    e
}

fn cksum(name: &str, offset: u32, start: u32, length: u32) -> [u8; 128] {
    let mut e = [0u8; 128];
    e[0] = 3;
    put_name(&mut e[4..60], name);
    e[60..64].copy_from_slice(&offset.to_le_bytes());
    e[64..68].copy_from_slice(&start.to_le_bytes());
    e[68..72].copy_from_slice(&length.to_le_bytes());
    e
}

fn put_name(dst: &mut [u8], name: &str) {
    let b = name.as_bytes();
    dst[..b.len()].copy_from_slice(b);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sum(bytes: &[u8]) -> u8 {
        bytes.iter().fold(0u8, |a, b| a.wrapping_add(*b))
    }

    fn decode_pkg(bytes: &[u8]) -> (usize, usize) {
        let extra = (bytes[0] >> 6) as usize;
        if extra == 0 {
            return ((bytes[0] & 0x3f) as usize, 1);
        }
        let mut len = (bytes[0] & 0x0f) as usize;
        for i in 0..extra {
            len |= (bytes[1 + i] as usize) << (4 + 8 * i);
        }
        (len, 1 + extra)
    }

    #[test]
    fn tables_checksum_reset_and_pm_timer_agree() {
        let files = build(2, 128 << 20);
        assert_eq!(sum(&files.rsdp[..20]), 0);
        assert_eq!(sum(&files.rsdp), 0);
        assert_eq!(&files.rsdp[..8], b"RSD PTR ");
        assert_eq!(
            u32::from_le_bytes(files.rsdp[16..20].try_into().unwrap()),
            0
        );

        let fadt = files.tables.windows(4).position(|w| w == b"FACP").unwrap();
        assert_eq!(
            u32::from_le_bytes(files.tables[fadt + 4..fadt + 8].try_into().unwrap()),
            276
        );
        assert_eq!(sum(&files.tables[fadt..fadt + 276]), 0);
        let pm_tmr = u32::from_le_bytes(files.tables[fadt + 76..fadt + 80].try_into().unwrap());
        assert_eq!(pm_tmr, pm_timer_port(PMBA_DEFAULT));
        assert_eq!(files.tables[fadt + 116], 1);
        assert_eq!(
            u64::from_le_bytes(files.tables[fadt + 120..fadt + 128].try_into().unwrap()),
            0xCF9
        );
        assert_eq!(files.tables[fadt + 128], 0x06);
        assert_eq!(files.tables[fadt + 109] & 0x02, 0x02);
        assert_eq!(files.tables[fadt + 109] & 0x04, 0);

        let dsdt = files.tables.windows(4).position(|w| w == b"DSDT").unwrap();
        let dsdt_len =
            u32::from_le_bytes(files.tables[dsdt + 4..dsdt + 8].try_into().unwrap()) as usize;
        assert_eq!(sum(&files.tables[dsdt..dsdt + dsdt_len]), 0);
        let aml = &files.tables[dsdt + 36..dsdt + dsdt_len];
        assert!(aml.windows(4).any(|w| w == b"_S5_"));
        assert!(!aml.windows(4).any(|w| w == b"_PRT"));
        assert_eq!(aml[20], 0x10);
        let (len, n) = decode_pkg(&aml[21..]);
        assert_eq!(21 + len, aml.len());
        assert!(n >= 1);

        let madt = files.tables.windows(4).position(|w| w == b"APIC").unwrap();
        let madt_len =
            u32::from_le_bytes(files.tables[madt + 4..madt + 8].try_into().unwrap()) as usize;
        assert_eq!(sum(&files.tables[madt..madt + madt_len]), 0);
        let locals = files.tables[madt + 44..madt + madt_len]
            .chunks(1)
            .filter(|c| c[0] == 0)
            .count();
        // Type 0 entries are 8 bytes; count the type bytes at the start of each.
        let mut cpus = 0;
        let mut i = madt + 44;
        while i + 1 < madt + madt_len {
            let kind = files.tables[i];
            let len = files.tables[i + 1] as usize;
            if kind == 0 {
                cpus += 1;
            }
            assert_ne!(kind, 9);
            i += len;
        }
        assert_eq!(cpus, 2);
        let _ = locals;

        let with_prt = build_with_prt(1, 128 << 20, &[(2 << 16, 16)]);
        let dsdt_prt = with_prt
            .tables
            .windows(4)
            .position(|w| w == b"DSDT")
            .unwrap();
        let prt_len = u32::from_le_bytes(
            with_prt.tables[dsdt_prt + 4..dsdt_prt + 8]
                .try_into()
                .unwrap(),
        ) as usize;
        assert_eq!(sum(&with_prt.tables[dsdt_prt..dsdt_prt + prt_len]), 0);
        let prt_aml = &with_prt.tables[dsdt_prt + 36..dsdt_prt + prt_len];
        let at = prt_aml.windows(4).position(|w| w == b"_PRT").unwrap();
        assert_eq!(
            &prt_aml[at + 4..at + 4 + 15],
            &[
                0x12, 0x0E, 0x01, 0x12, 0x0B, 0x04, 0x0C, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00, 0x0A,
                0x10,
            ]
        );

        assert_eq!(files.loader.len() % 128, 0);
        assert_eq!(files.loader[0], 1);
        assert_eq!(files.loader[64], 1);
        assert_eq!(files.loader[128], 1);
        assert_eq!(files.loader[128 + 64], 2);
        assert_eq!(&files.e820.len(), &60);
        assert_eq!(
            u32::from_le_bytes(files.e820[16..20].try_into().unwrap()),
            1
        );
    }

    #[test]
    fn e820_carves_a_reserved_window_off_the_top_of_ram() {
        let ram = 128 * 1024 * 1024;
        assert_eq!(e820_bytes(ram).len(), 60);
        assert_eq!(e820_reserving(ram, 0, 0).len(), 60);
        let len = 4 * 1024 * 1024;
        let base = ram - len;
        let map = e820_reserving(ram, base, len);
        assert_eq!(map.len(), 80);
        let entry = |index: usize| {
            let at = index * 20;
            (
                u64::from_le_bytes(map[at..at + 8].try_into().unwrap()),
                u64::from_le_bytes(map[at + 8..at + 16].try_into().unwrap()),
                u32::from_le_bytes(map[at + 16..at + 20].try_into().unwrap()),
            )
        };
        assert_eq!(entry(2), (0x10_0000, base - 0x10_0000, 1));
        assert_eq!(entry(3), (base, len, 2));
    }
}
