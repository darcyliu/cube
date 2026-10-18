use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::Command;

const EXT4_SUPER_OFFSET: u64 = 1024;
const EXT4_SUPER_SIZE: usize = 1024;
const EXT4_ROOT_INODE: u32 = 2;
const EXT4_EXTENT_MAGIC: u16 = 0xF30A;
const EXT4_FEATURE_RO_COMPAT_METADATA_CSUM: u32 = 0x400;
const EXT4_GOOD_OLD_INODE_SIZE: usize = 128;

const FALLBACK_ROOT_HASH: &str =
    "$6$vmmdebian$cTEwBCyPjJIDXBkb6D0Y/V9H8hJhcCWneA6klgFte2bqsmPQJ2jolV/JDKPAKdgyR0WxLRni1t7Asnlj2GI7w/";

pub fn crc32c(data: &[u8], mut crc: u32) -> u32 {
    const TABLE: [u32; 256] = {
        let poly = 0x82F6_3B78u32;
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut j = 0;
            while j < 8 {
                c = if (c & 1) != 0 { (c >> 1) ^ poly } else { c >> 1 };
                j += 1;
            }
            table[i] = c;
            i += 1;
        }
        table
    };
    for &b in data {
        crc = TABLE[((crc ^ (b as u32)) & 0xFF) as usize] ^ (crc >> 8);
    }
    crc
}

#[derive(Clone, Debug)]
pub struct Superblock {
    pub inodes_count: u32,
    pub blocks_count: u32,
    pub first_data_block: u32,
    pub log_block_size: u32,
    pub block_size: u64,
    pub blocks_per_group: u32,
    pub inodes_per_group: u32,
    pub magic: u16,
    pub inode_size: u16,
    pub feature_incompat: u32,
    pub feature_ro_compat: u32,
    pub uuid: [u8; 16],
    pub desc_size: u16,
    pub has_metadata_csum: bool,
}

impl Superblock {
    pub fn parse(buf: &[u8]) -> Result<Self, String> {
        if buf.len() < EXT4_SUPER_SIZE {
            return Err("Superblock buffer too small".into());
        }
        let magic = u16::from_le_bytes([buf[56], buf[57]]);
        if magic != 0xEF53 {
            return Err(format!("Bad ext4 magic: 0x{magic:04X}"));
        }
        let log_block_size = u32::from_le_bytes(buf[24..28].try_into().unwrap());
        let block_size = 1024u64 << log_block_size;
        let desc_size_raw = u16::from_le_bytes(buf[254..256].try_into().unwrap());
        let feature_incompat = u32::from_le_bytes(buf[96..100].try_into().unwrap());
        let desc_size = if (feature_incompat & 0x80) != 0 && desc_size_raw != 0 {
            desc_size_raw
        } else {
            32
        };
        let feature_ro_compat = u32::from_le_bytes(buf[100..104].try_into().unwrap());
        let has_metadata_csum = (feature_ro_compat & EXT4_FEATURE_RO_COMPAT_METADATA_CSUM) != 0;
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&buf[104..120]);

        Ok(Superblock {
            inodes_count: u32::from_le_bytes(buf[0..4].try_into().unwrap()),
            blocks_count: u32::from_le_bytes(buf[4..8].try_into().unwrap()),
            first_data_block: u32::from_le_bytes(buf[20..24].try_into().unwrap()),
            log_block_size,
            block_size,
            blocks_per_group: u32::from_le_bytes(buf[32..36].try_into().unwrap()),
            inodes_per_group: u32::from_le_bytes(buf[40..44].try_into().unwrap()),
            magic,
            inode_size: u16::from_le_bytes(buf[88..90].try_into().unwrap()),
            feature_incompat,
            feature_ro_compat,
            uuid,
            desc_size,
            has_metadata_csum,
        })
    }
}

pub struct Ext4Fs<'a> {
    file: &'a mut File,
    part_offset: u64,
    pub sb: Superblock,
    gdt_offset: u64,
}

impl<'a> Ext4Fs<'a> {
    pub fn open(file: &'a mut File, part_offset: u64) -> Result<Self, String> {
        let mut sb_buf = vec![0u8; EXT4_SUPER_SIZE];
        file.seek(SeekFrom::Start(part_offset + EXT4_SUPER_OFFSET))
            .map_err(|e| e.to_string())?;
        file.read_exact(&mut sb_buf).map_err(|e| e.to_string())?;
        let sb = Superblock::parse(&sb_buf)?;
        let gdt_block = (sb.first_data_block + 1) as u64;
        let gdt_offset = gdt_block * sb.block_size;
        Ok(Ext4Fs {
            file,
            part_offset,
            sb,
            gdt_offset,
        })
    }

    fn pread(&mut self, offset: u64, len: usize) -> Result<Vec<u8>, String> {
        self.file
            .seek(SeekFrom::Start(self.part_offset + offset))
            .map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; len];
        self.file.read_exact(&mut buf).map_err(|e| e.to_string())?;
        Ok(buf)
    }

    fn pwrite(&mut self, offset: u64, data: &[u8]) -> Result<(), String> {
        self.file
            .seek(SeekFrom::Start(self.part_offset + offset))
            .map_err(|e| e.to_string())?;
        self.file.write_all(data).map_err(|e| e.to_string())?;
        Ok(())
    }

    pub fn read_block(&mut self, blk_no: u64) -> Result<Vec<u8>, String> {
        self.pread(blk_no * self.sb.block_size, self.sb.block_size as usize)
    }

    pub fn write_block(&mut self, blk_no: u64, data: &[u8]) -> Result<(), String> {
        if data.len() != self.sb.block_size as usize {
            return Err("write_block data size mismatch".into());
        }
        self.pwrite(blk_no * self.sb.block_size, data)
    }

    fn get_inode_table_block(&mut self, group: u32) -> Result<u64, String> {
        let off = self.gdt_offset + (group as u64) * (self.sb.desc_size as u64);
        let buf = self.pread(off, self.sb.desc_size as usize)?;
        let lo = u32::from_le_bytes(buf[8..12].try_into().unwrap()) as u64;
        let hi = if self.sb.desc_size >= 44 {
            u16::from_le_bytes(buf[40..42].try_into().unwrap()) as u64
        } else {
            0
        };
        Ok((hi << 32) | lo)
    }

    pub fn read_inode_raw(&mut self, ino: u32) -> Result<(Vec<u8>, u64), String> {
        if ino == 0 {
            return Err("Inode 0 is invalid".into());
        }
        let zero_ino = ino - 1;
        let group = zero_ino / self.sb.inodes_per_group;
        let idx = zero_ino % self.sb.inodes_per_group;
        let table_blk = self.get_inode_table_block(group)?;
        let inode_off = table_blk * self.sb.block_size + (idx as u64) * (self.sb.inode_size as u64);
        let buf = self.pread(inode_off, self.sb.inode_size as usize)?;
        Ok((buf, inode_off))
    }

    pub fn inode_size_bytes(&self, ibuf: &[u8]) -> u64 {
        let lo = u32::from_le_bytes(ibuf[4..8].try_into().unwrap()) as u64;
        let hi = if ibuf.len() >= 112 {
            u32::from_le_bytes(ibuf[108..112].try_into().unwrap()) as u64
        } else {
            0
        };
        (hi << 32) | lo
    }

    pub fn get_extents(&mut self, ibuf: &[u8]) -> Result<Vec<(u32, u64, u32)>, String> {
        let i_block = &ibuf[40..100.min(ibuf.len())];
        self.parse_extent_node(i_block)
    }

    fn parse_extent_node(&mut self, buf: &[u8]) -> Result<Vec<(u32, u64, u32)>, String> {
        if buf.len() < 12 {
            return Ok(Vec::new());
        }
        let magic = u16::from_le_bytes(buf[0..2].try_into().unwrap());
        if magic != EXT4_EXTENT_MAGIC {
            return Ok(Vec::new());
        }
        let entries = u16::from_le_bytes(buf[2..4].try_into().unwrap()) as usize;
        let depth = u16::from_le_bytes(buf[6..8].try_into().unwrap());
        let mut extents = Vec::new();

        if depth == 0 {
            for i in 0..entries {
                let off = 12 + i * 12;
                if off + 12 > buf.len() {
                    break;
                }
                let ee_block = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
                let ee_len_raw = u16::from_le_bytes(buf[off + 4..off + 6].try_into().unwrap());
                let ee_len = if ee_len_raw > 32768 {
                    (ee_len_raw - 32768) as u32
                } else {
                    ee_len_raw as u32
                };
                let ee_start_hi = u16::from_le_bytes(buf[off + 6..off + 8].try_into().unwrap()) as u64;
                let ee_start_lo = u32::from_le_bytes(buf[off + 8..off + 12].try_into().unwrap()) as u64;
                let disk_blk = (ee_start_hi << 32) | ee_start_lo;
                extents.push((ee_block, disk_blk, ee_len));
            }
        } else {
            for i in 0..entries {
                let off = 12 + i * 12;
                if off + 12 > buf.len() {
                    break;
                }
                let leaf_lo = u32::from_le_bytes(buf[off + 4..off + 8].try_into().unwrap()) as u64;
                let leaf_hi = u16::from_le_bytes(buf[off + 8..off + 10].try_into().unwrap()) as u64;
                let leaf_blk = (leaf_hi << 32) | leaf_lo;
                let child_buf = self.read_block(leaf_blk)?;
                let sub = self.parse_extent_node(&child_buf)?;
                extents.extend(sub);
            }
        }
        Ok(extents)
    }

    pub fn read_file(&mut self, ino: u32) -> Result<Vec<u8>, String> {
        let (ibuf, _) = self.read_inode_raw(ino)?;
        let size = self.inode_size_bytes(&ibuf);
        let bs = self.sb.block_size;
        let mut extents = self.get_extents(&ibuf)?;
        extents.sort_by_key(|x| x.0);

        let mut data = vec![0u8; size as usize];
        for (file_blk, disk_blk, length) in extents {
            for i in 0..length {
                let blk_data = self.read_block(disk_blk + i as u64)?;
                let dst_off = ((file_blk + i) as u64 * bs) as usize;
                if dst_off >= size as usize {
                    break;
                }
                let copy_len = (bs as usize).min(size as usize - dst_off);
                data[dst_off..dst_off + copy_len].copy_from_slice(&blk_data[..copy_len]);
            }
        }
        Ok(data)
    }

    pub fn list_dir(&mut self, dir_ino: u32) -> Result<Vec<(String, u32, u8)>, String> {
        let content = self.read_file(dir_ino)?;
        let mut entries = Vec::new();
        let mut pos = 0;
        while pos + 8 <= content.len() {
            let inum = u32::from_le_bytes(content[pos..pos + 4].try_into().unwrap());
            let rec_len = u16::from_le_bytes(content[pos + 4..pos + 6].try_into().unwrap()) as usize;
            let name_len = content[pos + 6] as usize;
            let ftype = content[pos + 7];
            if rec_len == 0 {
                break;
            }
            if inum != 0 && pos + 8 + name_len <= content.len() {
                let name = String::from_utf8_lossy(&content[pos + 8..pos + 8 + name_len]).to_string();
                entries.push((name, inum, ftype));
            }
            pos += rec_len;
        }
        Ok(entries)
    }

    pub fn lookup_dir(&mut self, dir_ino: u32, target: &str) -> Result<Option<u32>, String> {
        let entries = self.list_dir(dir_ino)?;
        for (name, inum, _) in entries {
            if name == target {
                return Ok(Some(inum));
            }
        }
        Ok(None)
    }

    pub fn resolve_path(&mut self, path: &str) -> Result<Option<u32>, String> {
        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let mut ino = EXT4_ROOT_INODE;
        for part in parts {
            match self.lookup_dir(ino, part)? {
                Some(child) => ino = child,
                None => return Ok(None),
            }
        }
        Ok(Some(ino))
    }

    fn inode_csum_seed(&self, ino: u32, ibuf: &[u8]) -> u32 {
        let r#gen = if ibuf.len() >= 104 {
            u32::from_le_bytes(ibuf[100..104].try_into().unwrap())
        } else {
            0
        };
        let mut seed = crc32c(&self.sb.uuid, 0xFFFF_FFFF);
        seed = crc32c(&ino.to_le_bytes(), seed);
        crc32c(&r#gen.to_le_bytes(), seed)
    }

    pub fn set_inode_checksum(&self, ino: u32, ibuf: &mut [u8]) {
        if !self.sb.has_metadata_csum || ibuf.len() < 126 {
            return;
        }
        ibuf[124..126].copy_from_slice(&[0, 0]);
        if ibuf.len() >= 132 {
            ibuf[130..132].copy_from_slice(&[0, 0]);
        }
        let dummy = [0u8, 0];
        let seed = self.inode_csum_seed(ino, ibuf);
        let mut csum = crc32c(&ibuf[..124], seed);
        csum = crc32c(&dummy, csum);
        let end_old = EXT4_GOOD_OLD_INODE_SIZE.min(ibuf.len());
        csum = crc32c(&ibuf[126..end_old], csum);
        if ibuf.len() > EXT4_GOOD_OLD_INODE_SIZE {
            let end_mid = 130.min(ibuf.len());
            csum = crc32c(&ibuf[EXT4_GOOD_OLD_INODE_SIZE..end_mid], csum);
            csum = crc32c(&dummy, csum);
            if ibuf.len() > 132 {
                csum = crc32c(&ibuf[132..], csum);
            }
        }
        let lo = (csum & 0xFFFF) as u16;
        ibuf[124..126].copy_from_slice(&lo.to_le_bytes());
        if ibuf.len() >= 132 {
            let hi = ((csum >> 16) & 0xFFFF) as u16;
            ibuf[130..132].copy_from_slice(&hi.to_le_bytes());
        }
    }

    pub fn verify_inode_checksum(&self, ino: u32, ibuf: &[u8]) -> bool {
        if !self.sb.has_metadata_csum || ibuf.len() < 126 {
            return true;
        }
        let stored_lo = u16::from_le_bytes(ibuf[124..126].try_into().unwrap());
        let stored_hi = if ibuf.len() >= 132 {
            u16::from_le_bytes(ibuf[130..132].try_into().unwrap())
        } else {
            0
        };
        let mut copy = ibuf.to_vec();
        self.set_inode_checksum(ino, &mut copy);
        let re_lo = u16::from_le_bytes(copy[124..126].try_into().unwrap());
        let re_hi = if copy.len() >= 132 {
            u16::from_le_bytes(copy[130..132].try_into().unwrap())
        } else {
            0
        };
        stored_lo == re_lo && (ibuf.len() < 132 || stored_hi == re_hi)
    }

    pub fn write_file(&mut self, ino: u32, new_content: &[u8]) -> Result<(), String> {
        let (ibuf, inode_off) = self.read_inode_raw(ino)?;
        let old_size = self.inode_size_bytes(&ibuf);
        let bs = self.sb.block_size;
        let new_size = new_content.len() as u64;
        let mut extents = self.get_extents(&ibuf)?;
        extents.sort_by_key(|x| x.0);
        let allocated: u64 = extents.iter().map(|(_, _, l)| (*l as u64) * bs).sum();
        let capacity = old_size.max(allocated);
        if new_size > capacity {
            return Err(format!(
                "New content ({new_size}B) exceeds allocated space ({capacity}B)"
            ));
        }

        for (file_blk, disk_blk, length) in extents {
            for i in 0..length {
                let dst_off = ((file_blk + i) as u64 * bs) as usize;
                if dst_off >= new_content.len() {
                    break;
                }
                let src_chunk = &new_content[dst_off..(dst_off + bs as usize).min(new_content.len())];
                let mut block_data = self.read_block(disk_blk + i as u64)?;
                block_data[..src_chunk.len()].copy_from_slice(src_chunk);
                if (dst_off + src_chunk.len()) as u64 >= new_size && src_chunk.len() < bs as usize {
                    for b in &mut block_data[src_chunk.len()..] {
                        *b = 0;
                    }
                }
                self.write_block(disk_blk + i as u64, &block_data)?;
            }
        }

        if new_size != old_size {
            let mut raw = ibuf;
            let lo = (new_size & 0xFFFF_FFFF) as u32;
            let hi = (new_size >> 32) as u32;
            raw[4..8].copy_from_slice(&lo.to_le_bytes());
            if raw.len() >= 112 {
                raw[108..112].copy_from_slice(&hi.to_le_bytes());
            }
            if self.sb.has_metadata_csum {
                self.set_inode_checksum(ino, &mut raw);
            }
            self.pwrite(inode_off, &raw)?;
        }
        Ok(())
    }
}

pub struct PartitionInfo {
    pub part_num: usize,
    pub offset: u64,
}

pub fn find_gpt_partitions(file: &mut File) -> Result<Vec<PartitionInfo>, String> {
    file.seek(SeekFrom::Start(512)).map_err(|e| e.to_string())?;
    let mut gpt_hdr = [0u8; 512];
    file.read_exact(&mut gpt_hdr).map_err(|e| e.to_string())?;
    if &gpt_hdr[0..8] != b"EFI PART" {
        // Fallback to MBR
        file.seek(SeekFrom::Start(446)).map_err(|e| e.to_string())?;
        let mut mbr_parts = Vec::new();
        for idx in 0..4 {
            let mut pte = [0u8; 16];
            file.read_exact(&mut pte).map_err(|e| e.to_string())?;
            if pte[4] == 0x83 {
                let lba = u32::from_le_bytes(pte[8..12].try_into().unwrap()) as u64;
                mbr_parts.push(PartitionInfo {
                    part_num: idx + 1,
                    offset: lba * 512,
                });
            }
        }
        if !mbr_parts.is_empty() {
            return Ok(mbr_parts);
        }
        return Err("Not a valid GPT or MBR disk image".into());
    }

    let part_lba = u64::from_le_bytes(gpt_hdr[72..80].try_into().unwrap());
    let num_parts = u32::from_le_bytes(gpt_hdr[80..84].try_into().unwrap()) as usize;
    let part_size = u32::from_le_bytes(gpt_hdr[84..88].try_into().unwrap()) as usize;

    file.seek(SeekFrom::Start(part_lba * 512))
        .map_err(|e| e.to_string())?;
    let count = num_parts.min(128);
    let mut entries = vec![0u8; count * part_size];
    file.read_exact(&mut entries).map_err(|e| e.to_string())?;

    let mut partitions = Vec::new();
    for idx in 0..count {
        let off = idx * part_size;
        let entry = &entries[off..off + part_size];
        if entry.len() < 48 {
            continue;
        }
        let first_lba = u64::from_le_bytes(entry[32..40].try_into().unwrap());
        if first_lba == 0 {
            continue;
        }
        partitions.push(PartitionInfo {
            part_num: idx + 1,
            offset: first_lba * 512,
        });
    }
    Ok(partitions)
}

pub fn extract_debian_cloud_kernel(
    disk_path: &Path,
    out_kernel: &Path,
    out_initrd: &Path,
    out_rootpart: Option<&Path>,
) -> Result<(), String> {
    let mut file = File::open(disk_path).map_err(|e| format!("Cannot open {disk_path:?}: {e}"))?;
    let partitions = find_gpt_partitions(&mut file)?;

    struct BootInfo {
        _part_num: usize,
        part_offset: u64,
        kernel_name: String,
        kernel_ino: u32,
        initrd_name: String,
        initrd_ino: u32,
    }

    let mut boot_info: Option<BootInfo> = None;
    let mut root_part_idx: Option<usize> = None;

    for part in partitions {
        let mut magic = [0u8; 2];
        if file
            .seek(SeekFrom::Start(part.offset + EXT4_SUPER_OFFSET + 0x38))
            .is_err()
            || file.read_exact(&mut magic).is_err()
        {
            continue;
        }
        if u16::from_le_bytes(magic) != 0xEF53 {
            continue;
        }

        let mut fs = match Ext4Fs::open(&mut file, part.offset) {
            Ok(fs) => fs,
            Err(_) => continue,
        };

        let root_entries = match fs.list_dir(EXT4_ROOT_INODE) {
            Ok(e) => e,
            Err(_) => continue,
        };

        let names: std::collections::HashSet<String> =
            root_entries.iter().map(|(n, _, _)| n.clone()).collect();
        if ["etc", "usr", "var", "bin", "sbin"]
            .iter()
            .any(|&req| names.contains(req))
        {
            root_part_idx = Some(part.part_num);
            println!(
                "[Cube] Partition {} identified as root filesystem (/dev/vda{})",
                part.part_num, part.part_num
            );
        }

        // Check if kernel/initrd are in root (dedicated /boot partition)
        let mut k_c: Vec<_> = root_entries
            .iter()
            .filter(|(n, _, t)| n.starts_with("vmlinuz-") && (*t == 0 || *t == 1))
            .collect();
        let mut i_c: Vec<_> = root_entries
            .iter()
            .filter(|(n, _, t)| n.starts_with("initrd.img-") && (*t == 0 || *t == 1))
            .collect();

        if !k_c.is_empty() && !i_c.is_empty() && boot_info.is_none() {
            k_c.sort_by_key(|(n, _, _)| n.as_str());
            i_c.sort_by_key(|(n, _, _)| n.as_str());
            let (k_name, k_ino, _) = k_c.last().unwrap();
            let (i_name, i_ino, _) = i_c.last().unwrap();
            boot_info = Some(BootInfo {
                _part_num: part.part_num,
                part_offset: part.offset,
                kernel_name: (*k_name).clone(),
                kernel_ino: *k_ino,
                initrd_name: (*i_name).clone(),
                initrd_ino: *i_ino,
            });
            println!(
                "[Cube] Partition {} identified as dedicated /boot partition",
                part.part_num
            );
        }

        // Check if kernel/initrd are in /boot subdirectory (unified root partition)
        if names.contains("boot") && boot_info.is_none() {
            if let Some(boot_ino) = root_entries.iter().find(|(n, _, _)| n == "boot").map(|(_, i, _)| *i) {
                if let Ok(boot_entries) = fs.list_dir(boot_ino) {
                    let mut bk_c: Vec<_> = boot_entries
                        .iter()
                        .filter(|(n, _, t)| n.starts_with("vmlinuz-") && (*t == 0 || *t == 1))
                        .collect();
                    let mut bi_c: Vec<_> = boot_entries
                        .iter()
                        .filter(|(n, _, t)| n.starts_with("initrd.img-") && (*t == 0 || *t == 1))
                        .collect();
                    if !bk_c.is_empty() && !bi_c.is_empty() {
                        bk_c.sort_by_key(|(n, _, _)| n.as_str());
                        bi_c.sort_by_key(|(n, _, _)| n.as_str());
                        let (k_name, k_ino, _) = bk_c.last().unwrap();
                        let (i_name, i_ino, _) = bi_c.last().unwrap();
                        boot_info = Some(BootInfo {
                            _part_num: part.part_num,
                            part_offset: part.offset,
                            kernel_name: (*k_name).clone(),
                            kernel_ino: *k_ino,
                            initrd_name: (*i_name).clone(),
                            initrd_ino: *i_ino,
                        });
                        println!(
                            "[Cube] Partition {} has /boot directory with kernel/initrd",
                            part.part_num
                        );
                    }
                }
            }
        }
    }

    let binfo = boot_info.ok_or_else(|| "No kernel/initramfs found on any partition".to_string())?;

    let mut fs = Ext4Fs::open(&mut file, binfo.part_offset)?;
    println!(
        "[Cube] Extracting kernel '{}' -> {}...",
        binfo.kernel_name,
        out_kernel.display()
    );
    let kernel_data = fs.read_file(binfo.kernel_ino)?;
    if let Some(parent) = out_kernel.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(out_kernel, &kernel_data).map_err(|e| e.to_string())?;
    println!(
        "[Cube] Extracted kernel ({:.2} MB)",
        kernel_data.len() as f64 / (1024.0 * 1024.0)
    );

    println!(
        "[Cube] Extracting initramfs '{}' -> {}...",
        binfo.initrd_name,
        out_initrd.display()
    );
    let initrd_data = fs.read_file(binfo.initrd_ino)?;
    if let Some(parent) = out_initrd.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::write(out_initrd, &initrd_data).map_err(|e| e.to_string())?;
    println!(
        "[Cube] Extracted initramfs ({:.2} MB)",
        initrd_data.len() as f64 / (1024.0 * 1024.0)
    );

    let root_dev = format!("/dev/vda{}", root_part_idx.unwrap_or(1));
    println!("[Cube] Detected root partition device: {root_dev}");
    if let Some(root_path) = out_rootpart {
        if let Some(parent) = root_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(root_path, format!("{root_dev}\n")).map_err(|e| e.to_string())?;
        println!("[Cube] Saved root partition device to {}", root_path.display());
    }

    Ok(())
}

fn hash_password(password: &str) -> Result<String, String> {
    if let Ok(output) = Command::new("openssl")
        .args(["passwd", "-6", "-salt", "vmmdebian", password])
        .output()
    {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if s.starts_with("$6$") {
                return Ok(s);
            }
        }
    }
    if password == "root" {
        Ok(FALLBACK_ROOT_HASH.to_string())
    } else {
        Err("cannot hash password: openssl passwd -6 is unavailable; image was not modified".into())
    }
}

pub fn patch_cloud_shadow(disk_path: &Path, password: &str) -> Result<(), String> {
    println!("[Cube] Preparing nocloud image for serial login: {}", disk_path.display());
    println!("[Cube] Setting root password to: '{password}'");

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(disk_path)
        .map_err(|e| format!("Cannot open {disk_path:?} read/write: {e}"))?;

    let partitions = find_gpt_partitions(&mut file)?;
    let arm64_root_guid = [
        0x45, 0xb0, 0x21, 0xb9, 0xf0, 0x1d, 0xc3, 0x41, 0xaf, 0x44, 0x4c, 0x6f, 0x28, 0x0d, 0x3f,
        0xae,
    ];
    let linux_fs_guid = [
        0xaf, 0x3d, 0xc6, 0x0f, 0x83, 0x84, 0x47, 0x72, 0x8e, 0x79, 0x3d, 0x69, 0xd8, 0x47, 0x7d,
        0xe4,
    ];

    // Read full GPT entries to match type GUID
    file.seek(SeekFrom::Start(512)).map_err(|e| e.to_string())?;
    let mut gpt_hdr = [0u8; 512];
    file.read_exact(&mut gpt_hdr).map_err(|e| e.to_string())?;
    let mut target_offset = None;
    let mut fallback_offset = None;

    if &gpt_hdr[0..8] == b"EFI PART" {
        let part_lba = u64::from_le_bytes(gpt_hdr[72..80].try_into().unwrap());
        let num_parts = u32::from_le_bytes(gpt_hdr[80..84].try_into().unwrap()) as usize;
        let part_size = u32::from_le_bytes(gpt_hdr[84..88].try_into().unwrap()) as usize;
        file.seek(SeekFrom::Start(part_lba * 512)).map_err(|e| e.to_string())?;
        let count = num_parts.min(128);
        let mut entries = vec![0u8; count * part_size];
        file.read_exact(&mut entries).map_err(|e| e.to_string())?;
        for idx in 0..count {
            let off = idx * part_size;
            let entry = &entries[off..off + part_size];
            if entry.len() < 48 {
                continue;
            }
            let first_lba = u64::from_le_bytes(entry[32..40].try_into().unwrap());
            if first_lba == 0 {
                continue;
            }
            let type_guid = &entry[0..16];
            if type_guid == arm64_root_guid {
                target_offset = Some(first_lba * 512);
                break;
            }
            if type_guid == linux_fs_guid && fallback_offset.is_none() {
                fallback_offset = Some(first_lba * 512);
            }
        }
    }

    let part_offset = target_offset
        .or(fallback_offset)
        .or_else(|| partitions.first().map(|p| p.offset))
        .ok_or_else(|| "No Linux partition found".to_string())?;

    println!("[Cube] Linux partition at offset: {part_offset} (0x{part_offset:X})");

    let mut fs = Ext4Fs::open(&mut file, part_offset)?;
    println!(
        "[Cube] Block size: {}, Inode size: {}, metadata_csum={}",
        fs.sb.block_size,
        fs.sb.inode_size,
        if fs.sb.has_metadata_csum { "yes" } else { "no" }
    );

    let shadow_ino = fs
        .resolve_path("/etc/shadow")?
        .ok_or_else(|| "/etc/shadow not found in filesystem".to_string())?;
    println!("[Cube] Found /etc/shadow at inode {shadow_ino}");

    let (shadow_ibuf, _) = fs.read_inode_raw(shadow_ino)?;
    if !fs.verify_inode_checksum(shadow_ino, &shadow_ibuf) {
        return Err("ext4 inode CRC32C self-check failed; refusing to patch".into());
    }

    let shadow_content = fs.read_file(shadow_ino)?;
    println!("[Cube] Read {} bytes from /etc/shadow", shadow_content.len());

    let shadow_str = String::from_utf8_lossy(&shadow_content);
    for line in shadow_str.lines() {
        if line.starts_with("root:") {
            let part = line.split(':').nth(1).unwrap_or("");
            let preview = if part.len() > 25 { &part[..25] } else { part };
            println!("[Cube] Current root entry: root:{preview}...");
            break;
        }
    }

    let new_hash = hash_password(password)?;
    let mut patched_lines = Vec::new();
    let mut found = false;
    for line in shadow_str.lines() {
        if line.starts_with("root:") {
            let mut fields: Vec<&str> = line.split(':').collect();
            if fields.len() > 1 {
                let old_hash = fields[1];
                let old_preview = if old_hash.len() > 32 { &old_hash[..32] } else { old_hash };
                let new_preview = if new_hash.len() > 32 { &new_hash[..32] } else { &new_hash };
                println!("  Old hash: {old_preview}...");
                println!("  New hash: {new_preview}...");
                fields[1] = &new_hash;
                if fields.len() > 4 && fields[4] == "0" {
                    fields[4] = "99999";
                }
                patched_lines.push(fields.join(":"));
                found = true;
                continue;
            }
        }
        patched_lines.push(line.to_string());
    }
    if !found {
        return Err("root entry not found in /etc/shadow".into());
    }
    let mut new_shadow = patched_lines.join("\n").into_bytes();
    new_shadow.push(b'\n');

    println!(
        "[Cube] Patched shadow ({}B -> {}B)",
        shadow_content.len(),
        new_shadow.len()
    );
    fs.write_file(shadow_ino, &new_shadow)?;

    // Patch machine-id
    if let Some(ino) = fs.resolve_path("/etc/machine-id")? {
        let current = fs.read_file(ino)?;
        let trimmed = String::from_utf8_lossy(&current).trim().to_string();
        if trimmed.len() == 32 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
            println!(
                "[Cube] /etc/machine-id already set ({}...)",
                &trimmed[..8.min(trimmed.len())]
            );
        } else {
            let rand_bytes = rand_16_bytes()?;
            let mut hex_id = String::new();
            for b in rand_bytes {
                hex_id.push_str(&format!("{b:02x}"));
            }
            hex_id.push('\n');
            println!("[Cube] Writing /etc/machine-id to skip systemd-firstboot");
            fs.write_file(ino, hex_id.as_bytes())?;
        }
    }

    // Patch serial-getty autologin
    let unit_path = "/usr/lib/systemd/system/serial-getty@.service";
    if let Some(ino) = fs.resolve_path(unit_path)? {
        let data = fs.read_file(ino)?;
        if data.windows(b"--autologin".len()).any(|w| w == b"--autologin") {
            println!("[Cube] serial-getty already has --autologin");
        } else {
            let target = b"ExecStart=-/sbin/agetty ";
            let replacement = b"ExecStart=-/sbin/agetty --autologin root ";
            if let Some(pos) = data.windows(target.len()).position(|w| w == target) {
                let mut new_data = Vec::with_capacity(data.len() + replacement.len() - target.len());
                new_data.extend_from_slice(&data[..pos]);
                new_data.extend_from_slice(replacement);
                new_data.extend_from_slice(&data[pos + target.len()..]);
                println!("[Cube] Enabling serial-getty autologin as root");
                fs.write_file(ino, &new_data)?;
            }
        }
    }

    println!("[Cube] Done. Root password in {} set to '{password}'", disk_path.display());
    println!("[Cube] Serial console autologin is enabled; password login is root:{password}");
    Ok(())
}

fn rand_16_bytes() -> Result<[u8; 16], String> {
    let mut bytes = [0u8; 16];
    getrandom::getrandom(&mut bytes).map_err(|e| format!("cannot generate machine-id: {e}"))?;
    Ok(bytes)
}
