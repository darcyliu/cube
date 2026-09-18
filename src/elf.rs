//! Minimal ELF64 loader for AArch64 kernels (FreeBSD / Linux).

#![allow(dead_code)]

use crate::memory::GuestMemory;

#[derive(Debug, Clone, Copy)]
pub struct LoadedElf {
    pub entry_point: u64,
    pub total_memory_size: u64,
    pub base_vaddr: u64,
}

pub fn load_elf64(
    elf_data: &[u8],
    guest_mem: &GuestMemory,
    phys_load_base: u64,
) -> Result<LoadedElf, String> {
    if elf_data.len() < 64 {
        return Err("ELF file too small to contain header".into());
    }

    // Check magic
    if &elf_data[0..4] != b"\x7fELF" {
        return Err("Invalid ELF magic".into());
    }

    // 64-bit (EI_CLASS == 2)
    if elf_data[4] != 2 {
        return Err("Only 64-bit ELF (ELFCLASS64) is supported".into());
    }

    // Little endian (EI_DATA == 1)
    if elf_data[5] != 1 {
        return Err("Only little-endian ELF (ELFDATA2LSB) is supported".into());
    }

    // Machine architecture: AArch64 is 183 (0xb7)
    let e_machine = u16::from_le_bytes(elf_data[18..20].try_into().unwrap());
    if e_machine != 183 {
        return Err(format!(
            "Unsupported machine architecture: 0x{:04x} (expected 0x00b7 for AArch64)",
            e_machine
        ));
    }

    let e_entry = u64::from_le_bytes(elf_data[24..32].try_into().unwrap());
    let e_phoff = u64::from_le_bytes(elf_data[32..40].try_into().unwrap()) as usize;
    let e_phentsize = u16::from_le_bytes(elf_data[54..56].try_into().unwrap()) as usize;
    let e_phnum = u16::from_le_bytes(elf_data[56..58].try_into().unwrap()) as usize;

    if e_phnum == 0 || e_phentsize < 56 {
        return Err("Invalid program header table in ELF".into());
    }

    // Find the minimum virtual address among all PT_LOAD segments to determine the base
    let mut min_vaddr = u64::MAX;
    let mut max_vaddr = 0u64;

    for i in 0..e_phnum {
        let ph_offset = e_phoff + i * e_phentsize;
        if ph_offset + 56 > elf_data.len() {
            return Err("Program header out of bounds".into());
        }

        let p_type = u32::from_le_bytes(elf_data[ph_offset..ph_offset + 4].try_into().unwrap());
        if p_type == 1 {
            // PT_LOAD
            let p_vaddr =
                u64::from_le_bytes(elf_data[ph_offset + 16..ph_offset + 24].try_into().unwrap());
            let p_memsz =
                u64::from_le_bytes(elf_data[ph_offset + 40..ph_offset + 48].try_into().unwrap());

            if p_vaddr < min_vaddr {
                min_vaddr = p_vaddr;
            }
            if p_vaddr + p_memsz > max_vaddr {
                max_vaddr = p_vaddr + p_memsz;
            }
        }
    }

    if min_vaddr == u64::MAX {
        return Err("No PT_LOAD segments found in ELF".into());
    }

    println!(
        "[ELF] Kernel virtual memory range: 0x{:016x}..0x{:016x} (size: {:.2} MB)",
        min_vaddr,
        max_vaddr,
        (max_vaddr - min_vaddr) as f64 / (1024.0 * 1024.0)
    );

    // Load each PT_LOAD segment into guest physical memory
    for i in 0..e_phnum {
        let ph_offset = e_phoff + i * e_phentsize;
        let p_type = u32::from_le_bytes(elf_data[ph_offset..ph_offset + 4].try_into().unwrap());

        if p_type == 1 {
            // PT_LOAD
            let p_offset =
                u64::from_le_bytes(elf_data[ph_offset + 8..ph_offset + 16].try_into().unwrap())
                    as usize;
            let p_vaddr =
                u64::from_le_bytes(elf_data[ph_offset + 16..ph_offset + 24].try_into().unwrap());
            let p_filesz =
                u64::from_le_bytes(elf_data[ph_offset + 32..ph_offset + 40].try_into().unwrap())
                    as usize;
            let p_memsz =
                u64::from_le_bytes(elf_data[ph_offset + 40..ph_offset + 48].try_into().unwrap())
                    as usize;

            let segment_offset = p_vaddr - min_vaddr;
            let target_gpa = phys_load_base + segment_offset;

            if p_filesz > 0 {
                if p_offset + p_filesz > elf_data.len() {
                    return Err(format!(
                        "Segment file data out of bounds: offset 0x{:x}, size 0x{:x}",
                        p_offset, p_filesz
                    ));
                }
                let segment_bytes = &elf_data[p_offset..p_offset + p_filesz];
                guest_mem.write(target_gpa, segment_bytes)?;
            }

            // Zero any remaining bss space
            if p_memsz > p_filesz {
                let zero_len = p_memsz - p_filesz;
                let zeroes = vec![0u8; zero_len];
                guest_mem.write(target_gpa + (p_filesz as u64), &zeroes)?;
            }

            println!(
                "  -> PT_LOAD: vaddr 0x{:016x} -> GPA 0x{:08x} (filesz: 0x{:x}, memsz: 0x{:x})",
                p_vaddr, target_gpa, p_filesz, p_memsz
            );
        }
    }

    // Physical entry point
    let entry_offset = e_entry - min_vaddr;
    let phys_entry = phys_load_base + entry_offset;

    println!(
        "[ELF] Kernel entry: vaddr 0x{:016x} -> Physical GPA 0x{:08x}",
        e_entry, phys_entry
    );

    Ok(LoadedElf {
        entry_point: phys_entry,
        total_memory_size: max_vaddr - min_vaddr,
        base_vaddr: min_vaddr,
    })
}
