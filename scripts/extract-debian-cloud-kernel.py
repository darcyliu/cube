#!/usr/bin/env python3
"""
Extracts distribution kernel (vmlinuz-*) and initramfs (initrd.img-*)
directly from an installed Debian disk image (ext4 partitions).
Supports dedicated /boot partitions and unified root partitions.
Zero external dependencies (uses only standard Python struct & os).
"""

import sys
import os
import struct

def extract_kernel_and_initrd(disk_image_path, out_kernel_path, out_initrd_path, out_rootpart_path=None):
    if not os.path.exists(disk_image_path):
        print(f"[Error] Image not found: {disk_image_path}", file=sys.stderr)
        sys.exit(1)

    with open(disk_image_path, "rb") as f:
        f.seek(512)
        gpt_header = f.read(512)
        if gpt_header[:8] != b"EFI PART":
            print("[Error] Not a valid GPT disk image", file=sys.stderr)
            sys.exit(1)

        part_entry_lba = struct.unpack("<Q", gpt_header[72:80])[0]
        num_parts = struct.unpack("<I", gpt_header[80:84])[0]
        part_entry_size = struct.unpack("<I", gpt_header[84:88])[0]

        f.seek(part_entry_lba * 512)
        entries = f.read(min(num_parts, 128) * part_entry_size)

        boot_info = None
        root_part_idx = None

        for idx in range(min(num_parts, 128)):
            part_entry = entries[idx * part_entry_size:(idx + 1) * part_entry_size]
            if len(part_entry) < 40:
                continue
            first_lba = struct.unpack("<Q", part_entry[32:40])[0]
            if first_lba == 0:
                continue
            candidate = first_lba * 512
            f.seek(candidate + 1024 + 0x38)
            sb_magic = f.read(2)
            if len(sb_magic) != 2 or struct.unpack("<H", sb_magic)[0] != 0xef53:
                continue

            part_num = idx + 1
            f.seek(candidate + 1024)
            sb = f.read(1024)
            block_size = 1024 << struct.unpack("<I", sb[24:28])[0]
            inodes_per_group = struct.unpack("<I", sb[40:44])[0]
            inode_size = struct.unpack("<H", sb[88:90])[0]
            desc_size = struct.unpack("<H", sb[254:256])[0] if (struct.unpack("<I", sb[96:100])[0] & 0x80) else 32

            def read_inode_for(inode_num, part_offset=candidate, b_sz=block_size, in_pg=inodes_per_group, in_sz=inode_size, d_sz=desc_size):
                group = (inode_num - 1) // in_pg
                index = (inode_num - 1) % in_pg
                gdt_offset = part_offset + b_sz + group * d_sz
                f.seek(gdt_offset)
                gdt = f.read(d_sz)
                itable_blk = struct.unpack("<I", gdt[8:12])[0] | (struct.unpack("<H", gdt[40:42])[0] << 32)
                f.seek(part_offset + itable_blk * b_sz + index * in_sz)
                return f.read(in_sz)

            def read_extents_for(extent_data, part_offset=candidate, b_sz=block_size):
                magic, num_ext, _, depth, _ = struct.unpack("<HHHH I", extent_data[:12])
                if magic != 0xf30a:
                    return []
                blocks = []
                if depth == 0:
                    for i in range(num_ext):
                        off = 12 + i * 12
                        block, count, start_hi, start_lo = struct.unpack("<IHHI", extent_data[off:off+12])
                        start = start_lo | (start_hi << 32)
                        blocks.append((block, count, start))
                else:
                    for i in range(num_ext):
                        off = 12 + i * 12
                        block, leaf_lo, leaf_hi, _ = struct.unpack("<IIHH", extent_data[off:off+12])
                        leaf = leaf_lo | (leaf_hi << 32)
                        f.seek(part_offset + leaf * b_sz)
                        sub_hdr = f.read(b_sz)
                        blocks.extend(read_extents_for(sub_hdr, part_offset, b_sz))
                return blocks

            def list_dir_for(dir_inode_num, part_offset=candidate, b_sz=block_size):
                inode = read_inode_for(dir_inode_num, part_offset=part_offset)
                extents = read_extents_for(inode[40:100], part_offset=part_offset, b_sz=b_sz)
                res = []
                for _, count, p_blk in extents:
                    f.seek(part_offset + p_blk * b_sz)
                    data = f.read(count * b_sz)
                    pos = 0
                    while pos < len(data):
                        inum, rec_len, name_len, ftype = struct.unpack("<IHBB", data[pos:pos+8])
                        if inum == 0 or rec_len == 0:
                            break
                        name = data[pos+8:pos+8+name_len].decode("utf-8", "ignore")
                        res.append((name, inum, ftype))
                        pos += rec_len
                return res

            root_entries = list_dir_for(2)
            names = set(e[0] for e in root_entries)

            if names.intersection({"etc", "usr", "var", "bin", "sbin"}):
                root_part_idx = part_num
                print(f"[Cube] Partition {part_num} identified as root filesystem (/dev/vda{part_num})")

            # Check if kernel/initrd are in root (dedicated /boot partition)
            k_candidates = [(n, i) for n, i, t in root_entries if n.startswith("vmlinuz-") and t in (0, 1)]
            i_candidates = [(n, i) for n, i, t in root_entries if n.startswith("initrd.img-") and t in (0, 1)]
            if k_candidates and i_candidates and not boot_info:
                boot_info = {
                    "part_num": part_num,
                    "part_offset": candidate,
                    "block_size": block_size,
                    "inodes_per_group": inodes_per_group,
                    "inode_size": inode_size,
                    "desc_size": desc_size,
                    "kernel": sorted(k_candidates)[-1],
                    "initrd": sorted(i_candidates)[-1],
                }
                print(f"[Cube] Partition {part_num} identified as dedicated /boot partition")

            # Check if kernel/initrd are in /boot subdirectory (unified root partition)
            if "boot" in names and not boot_info:
                boot_inum = next((i for n, i, _ in root_entries if n == "boot"), None)
                if boot_inum:
                    boot_entries = list_dir_for(boot_inum)
                    k_c = [(n, i) for n, i, t in boot_entries if n.startswith("vmlinuz-") and t in (0, 1)]
                    i_c = [(n, i) for n, i, t in boot_entries if n.startswith("initrd.img-") and t in (0, 1)]
                    if k_c and i_c:
                        boot_info = {
                            "part_num": part_num,
                            "part_offset": candidate,
                            "block_size": block_size,
                            "inodes_per_group": inodes_per_group,
                            "inode_size": inode_size,
                            "desc_size": desc_size,
                            "kernel": sorted(k_c)[-1],
                            "initrd": sorted(i_c)[-1],
                        }
                        print(f"[Cube] Partition {part_num} has /boot directory with kernel/initrd")

        if not boot_info:
            print("[Error] No kernel/initramfs found on any partition", file=sys.stderr)
            sys.exit(1)

        b_off = boot_info["part_offset"]
        b_sz = boot_info["block_size"]
        in_pg = boot_info["inodes_per_group"]
        in_sz = boot_info["inode_size"]
        d_sz = boot_info["desc_size"]

        def read_file(inode_num, dest_path):
            group = (inode_num - 1) // in_pg
            index = (inode_num - 1) % in_pg
            gdt_offset = b_off + b_sz + group * d_sz
            f.seek(gdt_offset)
            gdt = f.read(d_sz)
            itable_blk = struct.unpack("<I", gdt[8:12])[0] | (struct.unpack("<H", gdt[40:42])[0] << 32)
            f.seek(b_off + itable_blk * b_sz + index * in_sz)
            inode = f.read(in_sz)

            size = struct.unpack("<I", inode[4:8])[0] | (struct.unpack("<I", inode[108:112])[0] << 32)

            def get_extents(extent_data):
                magic, num_ext, _, depth, _ = struct.unpack("<HHHH I", extent_data[:12])
                if magic != 0xf30a:
                    return []
                blks = []
                if depth == 0:
                    for i in range(num_ext):
                        off = 12 + i * 12
                        blk, cnt, s_hi, s_lo = struct.unpack("<IHHI", extent_data[off:off+12])
                        start = s_lo | (s_hi << 32)
                        blks.append((blk, cnt, start))
                else:
                    for i in range(num_ext):
                        off = 12 + i * 12
                        blk, l_lo, l_hi, _ = struct.unpack("<IIHH", extent_data[off:off+12])
                        leaf = l_lo | (leaf_hi << 32)
                        f.seek(b_off + leaf * b_sz)
                        sub_hdr = f.read(b_sz)
                        blks.extend(get_extents(sub_hdr))
                return blks

            extents = get_extents(inode[40:100])
            extents.sort(key=lambda x: x[0])
            os.makedirs(os.path.dirname(os.path.abspath(dest_path)), exist_ok=True)
            with open(dest_path, "wb") as out:
                rem = size
                for _, count, p_blk in extents:
                    f.seek(b_off + p_blk * b_sz)
                    chunk = f.read(min(count * b_sz, rem))
                    out.write(chunk)
                    rem -= len(chunk)
                    if rem <= 0:
                        break
            return size

        k_name, k_inum = boot_info["kernel"]
        i_name, i_inum = boot_info["initrd"]

        print(f"[Cube] Extracting kernel '{k_name}' -> {out_kernel_path}...")
        k_sz = read_file(k_inum, out_kernel_path)
        print(f"[Cube] Extracted kernel ({k_sz / (1024*1024):.2f} MB)")

        print(f"[Cube] Extracting initramfs '{i_name}' -> {out_initrd_path}...")
        i_sz = read_file(i_inum, out_initrd_path)
        print(f"[Cube] Extracted initramfs ({i_sz / (1024*1024):.2f} MB)")

        root_dev = f"/dev/vda{root_part_idx}" if root_part_idx else "/dev/vda1"
        print(f"[Cube] Detected root partition device: {root_dev}")
        if out_rootpart_path:
            os.makedirs(os.path.dirname(os.path.abspath(out_rootpart_path)), exist_ok=True)
            with open(out_rootpart_path, "w") as rf:
                rf.write(root_dev + "\n")
            print(f"[Cube] Saved root partition device to {out_rootpart_path}")

if __name__ == "__main__":
    if len(sys.argv) < 4:
        print("Usage: extract-debian-cloud-kernel.py <disk_image> <out_kernel> <out_initrd> [out_rootpart]")
        sys.exit(1)
    rootpart_out = sys.argv[4] if len(sys.argv) >= 5 else None
    extract_kernel_and_initrd(sys.argv[1], sys.argv[2], sys.argv[3], rootpart_out)
