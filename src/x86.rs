//! Guest paging and CR shadow helper. No Hypervisor.framework calls.

use crate::devices::ioapic::IOAPIC_MMIO;
use crate::devices::lapic::LAPIC_MMIO;
use crate::memory::GuestMemory;

/// 64 MiB identity map at GPA 0, entered at [`PAYLOAD_GPA`].
pub const GUEST_RAM_BYTES: usize = 64 * 1024 * 1024;
pub const PAYLOAD_GPA: u64 = 0x10_0000;
pub const STACK_GPA: u64 = 0x8_0000;
pub const GDT_GPA: u64 = 0x1000;
pub const TSS_GPA: u64 = 0x1800;
/// 40-byte GDT (null, code, data, 16-byte TSS) minus one.
pub const GDT_LIMIT: u64 = 0x27;

pub const CS_SELECTOR: u64 = 0x08;
pub const DS_SELECTOR: u64 = 0x10;
pub const TR_SELECTOR: u64 = 0x18;
/// 64-bit code, present, DPL 0, L=1, D=0, G=1.
pub const CS_AR: u64 = 0xa09b;
/// 32-bit data, present, writable, G=1, D/B=1.
pub const DATA_AR: u64 = 0xc093;
/// 64-bit busy TSS.
pub const TR_AR: u64 = 0x008b;
pub const UNUSABLE_AR: u64 = 1 << 16;
pub const DATA_LIMIT: u64 = 0xffff_ffff;
pub const TR_LIMIT: u64 = 0x67;

/// PE | MP | ET | NE | WP | PG.
pub const CR0_LONG_MODE: u64 = (1 << 0) | (1 << 1) | (1 << 4) | (1 << 5) | (1 << 16) | (1 << 31);
/// PAE. Long mode needs this; VMXE stays clear.
pub const CR4_LONG_MODE: u64 = 1 << 5;
/// LME | LMA. LMA must already be set when VM-entry is in IA-32e mode.
pub const EFER_LONG_MODE: u64 = (1 << 8) | (1 << 10);

const PML4_GPA: u64 = 0x2000;
const PDPT_GPA: u64 = 0x3000;
const PD_GPA: u64 = 0x4000;
const PT_GPA: u64 = 0x5000;
const PTE_PRESENT: u64 = 1 << 0;
const PTE_RW: u64 = 1 << 1;
const PTE_ACCESSED: u64 = 1 << 5;
const PTE_DIRTY: u64 = 1 << 6;
const TABLE_FLAGS: u64 = PTE_PRESENT | PTE_RW | PTE_ACCESSED;
const PAGE_FLAGS: u64 = TABLE_FLAGS | PTE_DIRTY;

/// v1 CR masks are all-ones, so the read shadow equals the guest-visible value.
pub fn publish_cr(guest_visible: u64) -> (u64, u64) {
    (guest_visible, guest_visible)
}

/// `hv_vmx_read_capability` packs forced-1 bits in the low half and allowed-1 bits in the high half.
pub fn vmx_cap_control(cap: u64, desired: u64) -> u64 {
    let must_1 = cap & 0xffff_ffff;
    let may_1 = cap >> 32;
    (desired | must_1) & may_1
}

/// Identity-map `[0, ram)` with 4 KiB pages. Returns the PML4 guest physical address (CR3).
pub fn install_identity_map(mem: &GuestMemory) -> Result<u64, String> {
    if mem.guest_base() != 0 {
        return Err("identity map requires guest RAM at GPA 0".into());
    }
    let ram = mem.size() as u64;
    if ram % 0x1000 != 0 {
        return Err("identity map requires a 4 KiB multiple".into());
    }
    let n_pages = (ram / 0x1000) as usize;
    let n_pt = n_pages.div_ceil(512);
    if n_pt == 0 || n_pt > 512 {
        return Err(format!(
            "identity map page-table count {n_pt} is out of range"
        ));
    }
    let tables_end = PT_GPA + (n_pt as u64) * 0x1000;
    if tables_end > PAYLOAD_GPA || tables_end > STACK_GPA || tables_end > ram {
        return Err("page tables overlap the payload, stack, or end of RAM".into());
    }
    mem.write_u64(PML4_GPA, PDPT_GPA | TABLE_FLAGS)?;
    mem.write_u64(PDPT_GPA, PD_GPA | TABLE_FLAGS)?;
    for i in 0..n_pt {
        let pt = PT_GPA + (i as u64) * 0x1000;
        mem.write_u64(PD_GPA + (i as u64) * 8, pt | TABLE_FLAGS)?;
        for j in 0..512 {
            let pfn = i * 512 + j;
            if pfn >= n_pages {
                break;
            }
            let gpa = (pfn as u64) * 0x1000;
            mem.write_u64(pt + (j as u64) * 8, gpa | PAGE_FLAGS)?;
        }
    }
    // A not-present guest translation is #PF. EPT never sees the LAPIC or I/O APIC.
    let mut extra = tables_end;
    map_identity_page(mem, IOAPIC_MMIO, &mut extra)?;
    map_identity_page(mem, LAPIC_MMIO, &mut extra)?;
    if extra > PAYLOAD_GPA || extra > STACK_GPA || extra > ram {
        return Err("MMIO page tables overlap the payload, stack, or end of RAM".into());
    }
    Ok(PML4_GPA)
}

fn map_identity_page(mem: &GuestMemory, gpa: u64, extra: &mut u64) -> Result<(), String> {
    if gpa & 0xfff != 0 {
        return Err(format!("MMIO page {gpa:#x} is not 4 KiB aligned"));
    }
    let pdpt = ensure_table(mem, PDPT_GPA + ((gpa >> 30) & 0x1ff) * 8, extra)?;
    let pt = ensure_table(mem, pdpt + ((gpa >> 21) & 0x1ff) * 8, extra)?;
    mem.write_u64(pt + ((gpa >> 12) & 0x1ff) * 8, gpa | PAGE_FLAGS)
}

fn ensure_table(mem: &GuestMemory, entry_gpa: u64, extra: &mut u64) -> Result<u64, String> {
    let entry = mem.read_u64(entry_gpa)?;
    if entry & PTE_PRESENT != 0 {
        return Ok(entry & 0x000f_ffff_ffff_f000);
    }
    let page = *extra;
    *extra += 0x1000;
    mem.write_u64(entry_gpa, page | TABLE_FLAGS)?;
    Ok(page)
}

pub fn write_gdt_tss(mem: &GuestMemory) -> Result<(), String> {
    let code = seg_desc(0x0f_ffff, 0, 0x9b, 0xa0);
    let data = seg_desc(0x0f_ffff, 0, 0x93, 0xc0);
    let tss = tss_desc(TSS_GPA);
    mem.write(GDT_GPA, &[0u8; 8])?;
    mem.write(GDT_GPA + 8, &code)?;
    mem.write(GDT_GPA + 16, &data)?;
    mem.write(GDT_GPA + 24, &tss)?;
    // I/O bitmap base just past the 104-byte TSS so the bitmap is not consulted.
    mem.write_u16(TSS_GPA + 102, (TR_LIMIT as u16).saturating_add(1))?;
    Ok(())
}

fn seg_desc(limit: u32, base: u32, access: u8, flags_hi: u8) -> [u8; 8] {
    [
        (limit & 0xff) as u8,
        ((limit >> 8) & 0xff) as u8,
        (base & 0xff) as u8,
        ((base >> 8) & 0xff) as u8,
        ((base >> 16) & 0xff) as u8,
        access,
        ((limit >> 16) as u8 & 0x0f) | (flags_hi & 0xf0),
        ((base >> 24) & 0xff) as u8,
    ]
}

fn tss_desc(base: u64) -> [u8; 16] {
    let mut d = [0u8; 16];
    d[..8].copy_from_slice(&seg_desc(TR_LIMIT as u32, base as u32, 0x8b, 0));
    d[8] = ((base >> 32) & 0xff) as u8;
    d[9] = ((base >> 40) & 0xff) as u8;
    d[10] = ((base >> 48) & 0xff) as u8;
    d[11] = ((base >> 56) & 0xff) as u8;
    d
}

#[cfg(test)]
fn translate_4k(mem: &GuestMemory, cr3: u64, va: u64) -> Result<u64, String> {
    let pml4e = mem.read_u64((cr3 & !0xfff) + ((va >> 39) & 0x1ff) * 8)?;
    let pdpte = present_table(mem, pml4e, (va >> 30) & 0x1ff)?;
    let pde = present_table(mem, pdpte, (va >> 21) & 0x1ff)?;
    if pde & (1 << 7) != 0 {
        return Err("unexpected 2 MiB page".into());
    }
    let pte = mem.read_u64((pde & 0x000f_ffff_ffff_f000) + ((va >> 12) & 0x1ff) * 8)?;
    if pte & PTE_PRESENT == 0 {
        return Err(format!("PTE not present for {va:#x}"));
    }
    if pte & (1 << 7) != 0 {
        return Err("huge bit set in a 4 KiB PTE".into());
    }
    Ok((pte & 0x000f_ffff_ffff_f000) | (va & 0xfff))
}

#[cfg(test)]
fn present_table(mem: &GuestMemory, entry: u64, index: u64) -> Result<u64, String> {
    if entry & PTE_PRESENT == 0 {
        return Err("page table entry not present".into());
    }
    if entry & (1 << 7) != 0 {
        return Err("unexpected huge page".into());
    }
    mem.read_u64((entry & 0x000f_ffff_ffff_f000) + index * 8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_cr_keeps_pe_pg_and_pae() {
        let pe = 1u64;
        let pe_pg = 1u64 | (1 << 31);
        let pae = 1u64 << 5;
        for guest in [pe, pe_pg, pae] {
            let (cr, shadow) = publish_cr(guest);
            assert_eq!(cr, guest);
            assert_eq!(shadow, guest);
            assert_eq!(shadow & guest, guest);
        }
    }

    #[test]
    fn cap_control_forces_one_and_clears_disallowed() {
        // Forced-1 bit 0. Allowed-1 bits 0 and 3. Desired also asks for bit 1.
        let cap = (0x9u64 << 32) | 0x1;
        let got = vmx_cap_control(cap, (1 << 3) | (1 << 1));
        assert_eq!(got, (1 << 3) | 1);
    }

    #[test]
    fn identity_map_is_4k_and_covers_the_payload() {
        let mem = GuestMemory::allocate(0, GUEST_RAM_BYTES).unwrap();
        let cr3 = install_identity_map(&mem).unwrap();
        assert_eq!(cr3, PML4_GPA);
        for va in [
            0u64,
            0xfff,
            PAYLOAD_GPA,
            STACK_GPA,
            GUEST_RAM_BYTES as u64 - 1,
        ] {
            assert_eq!(translate_4k(&mem, cr3, va).unwrap(), va);
        }
        assert_eq!(translate_4k(&mem, cr3, IOAPIC_MMIO).unwrap(), IOAPIC_MMIO);
        assert_eq!(translate_4k(&mem, cr3, LAPIC_MMIO).unwrap(), LAPIC_MMIO);
        assert!(translate_4k(&mem, cr3, GUEST_RAM_BYTES as u64).is_err());
        let leaf = mem.read_u64(PT_GPA).unwrap();
        assert_eq!(leaf & PAGE_FLAGS, PAGE_FLAGS);
        assert_eq!(leaf & (1 << 7), 0);
    }

    #[test]
    fn gdt_code_descriptor_is_64bit() {
        let mem = GuestMemory::allocate(0, 0x2000).unwrap();
        write_gdt_tss(&mem).unwrap();
        let mut desc = [0u8; 8];
        mem.read(GDT_GPA + 8, &mut desc).unwrap();
        assert_eq!(desc[5], 0x9b);
        assert_eq!(desc[6], 0xaf);
        let mut tss = [0u8; 8];
        mem.read(GDT_GPA + 24, &mut tss).unwrap();
        assert_eq!(tss[5], 0x8b);
        assert_eq!(mem.read_u16(TSS_GPA + 102).unwrap(), 0x68);
    }
}
