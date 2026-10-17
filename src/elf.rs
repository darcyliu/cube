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
    let e_phoff = usize::try_from(u64::from_le_bytes(elf_data[32..40].try_into().unwrap()))
        .map_err(|_| "Program header offset exceeds host address space")?;
    let e_phentsize = u16::from_le_bytes(elf_data[54..56].try_into().unwrap()) as usize;
    let e_phnum = u16::from_le_bytes(elf_data[56..58].try_into().unwrap()) as usize;
    if e_phnum == 0 || e_phentsize < 56 {
        return Err("Invalid program header table in ELF".into());
    }
    let table_len = e_phnum.checked_mul(e_phentsize)
        .ok_or("Program header table size overflow")?;
    let table_end = e_phoff.checked_add(table_len)
        .filter(|&end| end <= elf_data.len())
        .ok_or("Program header table out of bounds")?;

    struct Segment {
        vaddr: u64,
        end: u64,
        bytes: std::ops::Range<usize>,
        mem_size: usize,
        target: u64,
    }
    let mut segments = Vec::new();
    for ph in elf_data[e_phoff..table_end].chunks_exact(e_phentsize) {
        if u32::from_le_bytes(ph[..4].try_into().unwrap()) != 1 {
            continue;
        }
        let field = |offset| u64::from_le_bytes(ph[offset..offset + 8].try_into().unwrap());
        let offset = field(8);
        let vaddr = field(16);
        let file_size = field(32);
        let mem_size = field(40);
        if file_size > mem_size {
            return Err("ELF segment file size exceeds memory size".into());
        }
        let end = vaddr.checked_add(mem_size).ok_or("ELF segment address overflow")?;
        let file_end = offset.checked_add(file_size)
            .filter(|&end| end <= elf_data.len() as u64)
            .ok_or("ELF segment file range out of bounds")?;
        let mem_size = usize::try_from(mem_size)
            .map_err(|_| "ELF segment exceeds host address space")?;
        if mem_size != 0 {
            segments.push(Segment {
                vaddr, end, bytes: offset as usize..file_end as usize, mem_size, target: 0,
            });
        }
    }
    let min_vaddr = segments.iter().map(|s| s.vaddr).min()
        .ok_or("No nonempty PT_LOAD segments found in ELF")?;
    let max_vaddr = segments.iter().map(|s| s.end).max().unwrap();
    if !segments.iter().any(|s| s.vaddr <= e_entry && e_entry < s.end) {
        return Err("ELF entry point is outside loaded segments".into());
    }
    let phys_entry = phys_load_base.checked_add(e_entry - min_vaddr)
        .ok_or("ELF physical entry point overflow")?;

    // Validate every destination before modifying guest RAM or allocating BSS.
    for segment in &mut segments {
        segment.target = phys_load_base.checked_add(segment.vaddr - min_vaddr)
            .ok_or("ELF physical segment address overflow")?;
        segment.target.checked_add(segment.mem_size as u64)
            .ok_or("ELF physical segment end overflow")?;
        guest_mem.checked_offset(segment.target, segment.mem_size)?;
    }

    println!(
        "[ELF] Kernel virtual memory range: 0x{:016x}..0x{:016x} (size: {:.2} MB)",
        min_vaddr, max_vaddr, (max_vaddr - min_vaddr) as f64 / (1024.0 * 1024.0)
    );
    let zeroes = [0u8; 64 * 1024];
    for segment in segments {
        let bytes = &elf_data[segment.bytes];
        guest_mem.write(segment.target, bytes)?;
        let mut offset = bytes.len();
        while offset < segment.mem_size {
            let len = (segment.mem_size - offset).min(zeroes.len());
            guest_mem.write(segment.target + offset as u64, &zeroes[..len])?;
            offset += len;
        }
        println!(
            "  -> PT_LOAD: vaddr 0x{:016x} -> GPA 0x{:08x} (filesz: 0x{:x}, memsz: 0x{:x})",
            segment.vaddr, segment.target, bytes.len(), segment.mem_size
        );
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> Vec<u8> {
        let mut data = vec![0; 121];
        data[..4].copy_from_slice(b"\x7fELF");
        data[4] = 2;
        data[5] = 1;
        data[18..20].copy_from_slice(&183u16.to_le_bytes());
        set(&mut data, 24, 0x1000);
        set(&mut data, 32, 64);
        data[54..56].copy_from_slice(&56u16.to_le_bytes());
        data[56..58].copy_from_slice(&1u16.to_le_bytes());
        data[64..68].copy_from_slice(&1u32.to_le_bytes());
        set(&mut data, 72, 120);
        set(&mut data, 80, 0x1000);
        set(&mut data, 96, 1);
        set(&mut data, 104, 0x20001);
        data[120] = 0xa5;
        data
    }
    fn set(data: &mut [u8], offset: usize, value: u64) {
        data[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn loads_data_and_zeros_bss_including_partial_last_chunk() {
        let mem = GuestMemory::new_test(0x4000, 0x24000);
        mem.write(0x4000, &vec![0xcc; 0x24000]).unwrap();
        let loaded = load_elf64(&image(), &mem, 0x4000).unwrap();
        assert_eq!(loaded.entry_point, 0x4000);
        assert_eq!(loaded.total_memory_size, 0x20001);
        let mut bytes = vec![0; 0x20002];
        mem.read(0x4000, &mut bytes).unwrap();
        assert_eq!(bytes[0], 0xa5);
        assert!(bytes[1..0x20001].iter().all(|&b| b == 0));
        assert_eq!(bytes[0x20001], 0xcc);
    }

    #[test]
    fn rejects_malformed_ranges_before_modifying_memory() {
        let mem = GuestMemory::new_test(0x4000, 0x24000);
        for (field, value) in [
            (32, u64::MAX - 8), // program header table overflow
            (32, 100), // truncated program header table
            (72, u64::MAX), // file range overflow
            (80, u64::MAX - 1), // virtual address overflow
            (96, 0x30000), // file size larger than memory size
            (104, 1u64 << 50), // BSS too large for guest RAM
            (24, 0xfff), // entry below the first segment
            (24, 0x21001), // entry at the exclusive segment end
        ] {
            let mut data = image();
            set(&mut data, field, value);
            mem.write_u16(0x4000, 0xcccc).unwrap();
            assert!(load_elf64(&data, &mem, 0x4000).is_err(), "field {field}: {value:#x}");
            assert_eq!(mem.read_u16(0x4000).unwrap(), 0xcccc);
        }
        assert!(load_elf64(&image(), &mem, u64::MAX).is_err());
    }

    #[test]
    fn rejects_invalid_later_segment_before_loading_first() {
        let mem = GuestMemory::new_test(0x4000, 0x24000);
        let mut data = image();
        data.resize(177, 0);
        data.copy_within(64..120, 120);
        data[56..58].copy_from_slice(&2u16.to_le_bytes());
        set(&mut data, 72, 176);
        set(&mut data, 128, 176);
        set(&mut data, 136, 0x100000);
        data[176] = 0xa5;
        mem.write_u16(0x4000, 0xcccc).unwrap();
        assert!(load_elf64(&data, &mem, 0x4000).is_err());
        assert_eq!(mem.read_u16(0x4000).unwrap(), 0xcccc);
    }
}
