#!/usr/bin/env python3
"""
patch-cloud-shadow.py — Prepare a Debian nocloud disk for serial login.

Debian 13 nocloud images:
  * lock root (`!*` in /etc/shadow)
  * leave /etc/machine-id as "uninitialized", which runs systemd-firstboot
    (locale/keymap/timezone/root-password prompts that are easy to miss on
    a serial console — "just press enter" is the upstream advice)
  * ship no cloud-init, so a cidata seed cannot set the password

This script patches the ext4 root filesystem in-place (no mount, no FUSE):
  1. Unlock root and set the requested password (openssl SHA-512 crypt)
  2. Write a real /etc/machine-id so systemd-firstboot is skipped
  3. Enable serial-getty autologin as root

File growth is allowed up to already-allocated ext4 blocks, with inode
CRC32C updates when metadata_csum is enabled.

Usage:
    python3 scripts/patch-cloud-shadow.py debian-cloud.img [new_password]
    python3 scripts/patch-cloud-shadow.py debian-cloud.img root
"""

import sys
import os
import struct
import hashlib
import time
import random
import string
import subprocess

# ---------------------------------------------------------------------------
# ext4 constants
# ---------------------------------------------------------------------------
EXT4_SUPER_OFFSET   = 1024
EXT4_SUPER_SIZE     = 1024
EXT4_ROOT_INODE     = 2
EXT4_INODE_DIR      = 0x4000
EXT4_INODE_REG      = 0x8000
EXT4_EXTENT_MAGIC   = 0xF30A

def read_u8(buf, off):  return struct.unpack_from('B', buf, off)[0]
def read_u16(buf, off): return struct.unpack_from('<H', buf, off)[0]
def read_u32(buf, off): return struct.unpack_from('<I', buf, off)[0]
def read_u64(buf, off):
    lo = read_u32(buf, off)
    hi = read_u32(buf, off + 4)
    return (hi << 32) | lo

# CRC-32C (Castagnoli), reflected poly 0x82F63B78. Matches Linux ext4_chksum().
_CRC32C_TABLE = None
EXT4_FEATURE_RO_COMPAT_METADATA_CSUM = 0x400
EXT4_GOOD_OLD_INODE_SIZE = 128

def _crc32c_table():
    global _CRC32C_TABLE
    if _CRC32C_TABLE is None:
        poly = 0x82F63B78
        table = []
        for i in range(256):
            crc = i
            for _ in range(8):
                crc = (crc >> 1) ^ poly if crc & 1 else crc >> 1
            table.append(crc)
        _CRC32C_TABLE = table
    return _CRC32C_TABLE

def crc32c(data, crc=0):
    table = _crc32c_table()
    crc &= 0xffffffff
    for b in data:
        crc = table[(crc ^ b) & 0xff] ^ (crc >> 8)
    return crc & 0xffffffff

# Known-good hash of "root" with salt "vmmdebian" (openssl passwd -6).
FALLBACK_ROOT_HASH = (
    '$6$vmmdebian$cTEwBCyPjJIDXBkb6D0Y/V9H8hJhcCWneA6klgFte2bqsmPQJ2jolV/'
    'JDKPAKdgyR0WxLRni1t7Asnlj2GI7w/'
)

# ---------------------------------------------------------------------------
# Superblock
# ---------------------------------------------------------------------------
class Superblock:
    def __init__(self, buf):
        self.inodes_count      = read_u32(buf, 0)
        self.blocks_count      = read_u32(buf, 4)
        self.r_blocks_count    = read_u32(buf, 8)
        self.free_blocks_count = read_u32(buf, 12)
        self.free_inodes_count = read_u32(buf, 16)
        self.first_data_block  = read_u32(buf, 20)
        self.log_block_size    = read_u32(buf, 24)  # block_size = 1024 << log_block_size
        self.blocks_per_group  = read_u32(buf, 32)
        self.inodes_per_group  = read_u32(buf, 40)
        self.magic             = read_u16(buf, 56)
        self.inode_size        = read_u16(buf, 88)
        self.block_group_nr    = read_u16(buf, 90)
        self.feature_compat    = read_u32(buf, 92)
        self.feature_incompat  = read_u32(buf, 96)
        self.feature_ro_compat = read_u32(buf, 100)
        self.uuid              = buf[104:120]
        self.desc_size         = read_u16(buf, 254) or 32  # 0 → legacy 32-byte

        self.block_size = 1024 << self.log_block_size
        self.has_metadata_csum = bool(self.feature_ro_compat & EXT4_FEATURE_RO_COMPAT_METADATA_CSUM)
        assert self.magic == 0xEF53, f"Bad ext4 magic: 0x{self.magic:04X}"

# ---------------------------------------------------------------------------
# Ext4 reader
# ---------------------------------------------------------------------------
class Ext4:
    def __init__(self, f, part_offset):
        self.f = f
        self.off = part_offset
        sb_raw = self._pread(EXT4_SUPER_OFFSET, EXT4_SUPER_SIZE)
        self.sb = Superblock(sb_raw)
        bs = self.sb.block_size
        # Group descriptor table starts at block after superblock
        gdt_block = self.sb.first_data_block + 1
        self.gdt_offset = gdt_block * bs
        self.gdt_entry_size = self.sb.desc_size

    def _pread(self, offset, length):
        self.f.seek(self.off + offset)
        return self.f.read(length)

    def read_block(self, blk_no):
        return self._pread(blk_no * self.sb.block_size, self.sb.block_size)

    def _pwrite(self, offset, data):
        self.f.seek(self.off + offset)
        self.f.write(data)

    def write_block(self, blk_no, data):
        assert len(data) == self.sb.block_size
        self._pwrite(blk_no * self.sb.block_size, data)

    # --- group descriptor ---
    def get_group_desc(self, group):
        off = self.gdt_offset + group * self.gdt_entry_size
        buf = self._pread(off, self.gdt_entry_size)
        inode_table_lo = read_u32(buf, 8)
        inode_table_hi = read_u32(buf, 40) if self.gdt_entry_size >= 44 else 0
        inode_table = (inode_table_hi << 32) | inode_table_lo
        return inode_table

    # --- inode ---
    def read_inode_raw(self, ino):
        """Returns (inode_bytes, block_in_file, byte_offset_in_block)."""
        ino -= 1  # 0-indexed
        group = ino // self.sb.inodes_per_group
        idx   = ino %  self.sb.inodes_per_group
        inode_table = self.get_group_desc(group)
        inode_off = inode_table * self.sb.block_size + idx * self.sb.inode_size
        return self._pread(inode_off, self.sb.inode_size), inode_off

    def inode_size_bytes(self, ibuf):
        lo = read_u32(ibuf, 4)
        hi = read_u32(ibuf, 108)
        return (hi << 32) | lo

    def inode_mode(self, ibuf):
        return read_u16(ibuf, 0)

    def allocated_bytes(self, ibuf):
        bs = self.sb.block_size
        return sum(length for _, _, length in self.get_extents(ibuf)) * bs

    def inode_csum_seed(self, ino, ibuf):
        """ext4 i_csum_seed = crc32c(crc32c(~0, uuid), ino_le32, gen_le32)."""
        gen = read_u32(ibuf, 100)
        seed = crc32c(self.sb.uuid, 0xffffffff)
        seed = crc32c(struct.pack('<I', ino), seed)
        seed = crc32c(struct.pack('<I', gen), seed)
        return seed

    def set_inode_checksum(self, ino, ibuf):
        """Recompute ext4 inode CRC32C (metadata_csum). ibuf is mutated."""
        if not self.sb.has_metadata_csum or len(ibuf) < 126:
            return ibuf
        raw = bytearray(ibuf)
        raw[124:126] = b'\x00\x00'
        if len(raw) >= 132:
            raw[130:132] = b'\x00\x00'
        dummy = b'\x00\x00'
        seed = self.inode_csum_seed(ino, raw)
        csum = crc32c(bytes(raw[:124]), seed)
        csum = crc32c(dummy, csum)
        csum = crc32c(bytes(raw[126:EXT4_GOOD_OLD_INODE_SIZE]), csum)
        if len(raw) > EXT4_GOOD_OLD_INODE_SIZE:
            # i_checksum_hi lives at offset 130
            csum = crc32c(bytes(raw[EXT4_GOOD_OLD_INODE_SIZE:130]), csum)
            csum = crc32c(dummy, csum)
            if len(raw) > 132:
                csum = crc32c(bytes(raw[132:]), csum)
        struct.pack_into('<H', raw, 124, csum & 0xffff)
        if len(raw) >= 132:
            struct.pack_into('<H', raw, 130, (csum >> 16) & 0xffff)
        return raw

    def write_inode_raw(self, ino, inode_off, ibuf):
        raw = self.set_inode_checksum(ino, ibuf) if self.sb.has_metadata_csum else ibuf
        self.f.seek(self.off + inode_off)
        self.f.write(bytes(raw))

    def verify_inode_checksum(self, ino, ibuf):
        """Return True if the stored inode CRC matches a fresh computation."""
        if not self.sb.has_metadata_csum or len(ibuf) < 126:
            return True
        stored_lo = read_u16(ibuf, 124)
        stored_hi = read_u16(ibuf, 130) if len(ibuf) >= 132 else 0
        recomputed = self.set_inode_checksum(ino, ibuf)
        return read_u16(recomputed, 124) == stored_lo and (
            len(ibuf) < 132 or read_u16(recomputed, 130) == stored_hi
        )

    # --- extent tree ---
    def get_extents(self, ibuf):
        """Yield (file_block, disk_block, length) tuples from extent tree."""
        # i_block is at offset 40, 60 bytes (15 * 4)
        i_block = ibuf[40:100]
        yield from self._parse_extent_node(i_block, depth_hint=None)

    def _parse_extent_node(self, buf, depth_hint):
        magic  = read_u16(buf, 0)
        if magic != EXT4_EXTENT_MAGIC:
            return
        entries = read_u16(buf, 2)
        depth   = read_u16(buf, 6)
        if depth == 0:
            # Leaf node: extents
            for i in range(entries):
                off = 12 + i * 12
                ee_block  = read_u32(buf, off)
                ee_len    = read_u16(buf, off + 4)
                ee_start_hi = read_u16(buf, off + 6)
                ee_start_lo = read_u32(buf, off + 8)
                disk_blk = (ee_start_hi << 32) | ee_start_lo
                yield (ee_block, disk_blk, ee_len)
        else:
            # Index node: recurse
            for i in range(entries):
                off = 12 + i * 12
                # ei_leaf_lo at off+4, ei_leaf_hi at off+8
                leaf_lo = read_u32(buf, off + 4)
                leaf_hi = read_u16(buf, off + 8)
                leaf_blk = (leaf_hi << 32) | leaf_lo
                child_buf = self.read_block(leaf_blk)
                yield from self._parse_extent_node(child_buf, depth - 1)

    def read_file(self, ino):
        """Read entire file content for inode ino."""
        ibuf, _ = self.read_inode_raw(ino)
        size = self.inode_size_bytes(ibuf)
        bs = self.sb.block_size
        extents = sorted(self.get_extents(ibuf), key=lambda x: x[0])
        data = bytearray(size)
        for (file_blk, disk_blk, length) in extents:
            for i in range(length):
                block_data = self.read_block(disk_blk + i)
                dst_off = (file_blk + i) * bs
                copy_len = min(bs, size - dst_off)
                if copy_len <= 0:
                    break
                data[dst_off:dst_off + copy_len] = block_data[:copy_len]
        return bytes(data)

    def write_file(self, ino, new_content):
        """
        Write new_content back into the blocks of inode ino.
        Content may grow up to already-allocated extent bytes (no new block alloc).
        """
        ibuf, inode_off = self.read_inode_raw(ino)
        old_size = self.inode_size_bytes(ibuf)
        bs = self.sb.block_size
        new_size = len(new_content)
        extents = sorted(self.get_extents(ibuf), key=lambda x: x[0])
        allocated = sum(length for _, _, length in extents) * bs
        capacity = max(old_size, allocated)
        if new_size > capacity:
            raise RuntimeError(
                f"New content ({new_size}B) exceeds allocated space ({capacity}B). "
                "Cannot grow file without block allocation."
            )

        for (file_blk, disk_blk, length) in extents:
            for i in range(length):
                dst_off = (file_blk + i) * bs
                src_chunk = new_content[dst_off:dst_off + bs]
                if not src_chunk:
                    break
                block_data = bytearray(self.read_block(disk_blk + i))
                block_data[:len(src_chunk)] = src_chunk
                # Clear stale bytes past EOF in the last partial block
                if dst_off + len(src_chunk) < new_size:
                    pass
                elif len(src_chunk) < bs:
                    block_data[len(src_chunk):] = b'\x00' * (bs - len(src_chunk))
                self.write_block(disk_blk + i, bytes(block_data))

        if new_size != old_size:
            raw = bytearray(ibuf)
            struct.pack_into('<I', raw, 4,   new_size & 0xFFFFFFFF)
            struct.pack_into('<I', raw, 108, new_size >> 32)
            self.write_inode_raw(ino, inode_off, raw)

    # --- directory walk ---
    def lookup_dir(self, dir_ino, name):
        """Return inode number of `name` in directory `dir_ino`, or None."""
        ibuf, _ = self.read_inode_raw(dir_ino)
        content = self.read_file(dir_ino)
        target = name.encode()
        offset = 0
        while offset < len(content):
            if offset + 8 > len(content):
                break
            child_ino = read_u32(content, offset)
            rec_len   = read_u16(content, offset + 4)
            name_len  = read_u8(content,  offset + 6)
            if rec_len == 0:
                break
            entry_name = content[offset + 8: offset + 8 + name_len]
            if entry_name == target and child_ino != 0:
                return child_ino
            offset += rec_len
        return None

    def resolve_path(self, path):
        """Resolve an absolute path to an inode number. Returns None if not found."""
        parts = [p for p in path.split('/') if p]
        ino = EXT4_ROOT_INODE
        for part in parts:
            ino = self.lookup_dir(ino, part)
            if ino is None:
                return None
        return ino

# ---------------------------------------------------------------------------
# SHA-512-crypt (Linux /etc/shadow compatible) — pure Python, no `crypt` module
# Spec: https://www.akkadia.org/docs/sha-crypt.html
# ---------------------------------------------------------------------------
def _sha512_crypt(password, salt, rounds=5000):
    """Return a Linux-compatible $6$salt$hash string."""
    pw  = password.encode('utf-8')
    sa  = salt.encode('utf-8')
    rnd = rounds

    # Steps per spec
    A = hashlib.sha512(pw + sa + hashlib.sha512(pw + sa + pw).digest()).digest()
    # Actually implement the full spec:
    def H(data): return hashlib.sha512(data).digest()

    digest_B = H(pw + sa + pw)
    # Extend digest_B to password length
    B_ext = (digest_B * (len(pw) // 64 + 1))[:len(pw)]
    digest_A = H(pw + sa + B_ext +
                 b''.join(H(pw + sa + pw)[i:i+1]
                          if pw[i] & 1 else pw[i:i+1]
                          for i in range(len(pw))))

    digest_P = H(pw * len(pw))
    P_str = (digest_P * (len(pw) // 64 + 1))[:len(pw)]

    digest_S = H(sa * (16 + digest_A[0]))
    S_str = (digest_S * (len(sa) // 64 + 1))[:len(sa)]

    C = digest_A
    for i in range(rnd):
        tmp = b''
        tmp += P_str if (i & 1) else C
        if i % 3: tmp += S_str
        if i % 7: tmp += P_str
        tmp += C if (i & 1) else P_str
        C = H(tmp)

    # Base64-encode with SHA-crypt alphabet (different from standard base64)
    ALPHA = './0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz'
    def b64_3(a, b, c, n):
        v = (a << 16) | (b << 8) | c
        return ''.join(ALPHA[(v >> (6*i)) & 0x3f] for i in range(n))

    # SHA-512-crypt byte permutation (from spec §22 step 22)
    IDX = [
        (42,21,0),(22,43,1),(23,44,2),(45,3,24),(4,46,25),(5,47,26),
        (28,6,48),(49,29,7),(8,50,30),(31,9,51),(52,32,10),(11,53,33),
        (34,12,54),(55,35,13),(14,56,36),(37,15,57),(58,38,16),(17,59,39),
        (40,18,60),(61,41,19),(20,62,41),(63,0,0),
    ]
    out = ''
    for (x, y, z) in IDX[:-1]:
        out += b64_3(C[x], C[y], C[z], 4)
    out += b64_3(0, C[63], C[62], 3)

    rounds_str = f'rounds={rnd}$' if rnd != 5000 else ''
    return f'$6${rounds_str}{salt}${out}'

def make_shadow_hash(password, salt='vmmdebian'):
    """Generate a SHA-512 crypt hash compatible with Linux /etc/shadow."""
    try:
        out = subprocess.check_output(
            ['openssl', 'passwd', '-6', '-salt', salt, password],
            stderr=subprocess.DEVNULL,
            text=True,
        ).strip()
        if out.startswith('$6$'):
            return out
    except (subprocess.CalledProcessError, FileNotFoundError, OSError):
        pass
    if password == 'root' and salt == 'vmmdebian':
        return FALLBACK_ROOT_HASH
    alphabet = string.ascii_letters + string.digits + './'
    if not salt:
        salt = ''.join(random.choices(alphabet, k=16))
    return _sha512_crypt(password, salt)

def patch_shadow(shadow_bytes, password):
    """Replace root's hashed password in /etc/shadow content and unlock the account."""
    lines = shadow_bytes.decode('utf-8', errors='replace').splitlines(keepends=True)
    new_hash = make_shadow_hash(password)
    patched = []
    found = False
    for line in lines:
        if line.startswith('root:'):
            fields = line.rstrip('\n').split(':')
            old_hash = fields[1]
            fields[1] = new_hash
            # Do not expire the password (field 5 empty / keep max age).
            if len(fields) > 4 and fields[4] == '0':
                fields[4] = '99999'
            line = ':'.join(fields) + '\n'
            print(f"  Old hash: {old_hash[:32]}...")
            print(f"  New hash: {new_hash[:32]}...")
            found = True
        patched.append(line)
    if not found:
        raise RuntimeError("root entry not found in /etc/shadow")
    return ''.join(patched).encode('utf-8')

def patch_machine_id(fs):
    """Replace the first-boot sentinel with a real machine-id."""
    ino = fs.resolve_path('/etc/machine-id')
    if ino is None:
        print("[Cube] /etc/machine-id not found — skipping")
        return
    current = fs.read_file(ino).strip()
    if len(current) == 32 and all(c in b'0123456789abcdef' for c in current.lower()):
        print(f"[Cube] /etc/machine-id already set ({current.decode()[:8]}...)")
        return
    new_id = os.urandom(16).hex().encode('ascii') + b'\n'
    print(f"[Cube] Writing /etc/machine-id (was {current!r}) to skip systemd-firstboot")
    fs.write_file(ino, new_id)

def patch_serial_autologin(fs):
    """Make serial-getty@ autologin as root so the console is usable without a prompt."""
    path = '/usr/lib/systemd/system/serial-getty@.service'
    ino = fs.resolve_path(path)
    if ino is None:
        print(f"[Cube] {path} not found — skipping autologin")
        return
    data = fs.read_file(ino)
    if b'--autologin' in data:
        print("[Cube] serial-getty already has --autologin")
        return
    old = b'ExecStart=-/sbin/agetty '
    if old not in data:
        print("[Cube] serial-getty unit has no agetty ExecStart — skipping")
        return
    new_data = data.replace(old, b'ExecStart=-/sbin/agetty --autologin root ', 1)
    if new_data == data:
        print("[Cube] serial-getty ExecStart unchanged — skipping")
        return
    print("[Cube] Enabling serial-getty autologin as root")
    fs.write_file(ino, new_data)

# ---------------------------------------------------------------------------
# GPT partition table reader
# ---------------------------------------------------------------------------
def find_linux_partition(f):
    """
    Return the byte offset of the first Linux filesystem partition in a GPT disk.
    Tries ARM64 root GUID first, then generic Linux filesystem data GUID.
    """
    f.seek(512)
    gpt_hdr = f.read(512)
    if gpt_hdr[:8] != b'EFI PART':
        # Try MBR partition table
        f.seek(446)
        for i in range(4):
            pte = f.read(16)
            ptype = pte[4]
            if ptype == 0x83:  # Linux
                lba = struct.unpack_from('<I', pte, 8)[0]
                return lba * 512
        raise RuntimeError("No GPT or MBR Linux partition found")

    part_lba  = struct.unpack_from('<Q', gpt_hdr, 72)[0]
    num_parts = struct.unpack_from('<I', gpt_hdr, 80)[0]
    part_size = struct.unpack_from('<I', gpt_hdr, 84)[0]

    ARM64_ROOT  = bytes.fromhex('45b021b9f01dc341af444c6f280d3fae')
    LINUX_FS    = bytes.fromhex('af3dc60f83844772' + '8e793d69d8477de4')

    f.seek(part_lba * 512)
    entries = f.read(num_parts * part_size)

    fallback = None
    for i in range(min(num_parts, 128)):
        e = entries[i * part_size: (i + 1) * part_size]
        if len(e) < 48:
            continue
        type_guid = e[0:16]
        start_lba = struct.unpack_from('<Q', e, 32)[0]
        if start_lba == 0:
            continue
        if type_guid == ARM64_ROOT:
            return start_lba * 512
        if type_guid == LINUX_FS and fallback is None:
            fallback = start_lba * 512

    if fallback is not None:
        return fallback
    raise RuntimeError("No Linux root partition found in GPT")

# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------
def main():
    if len(sys.argv) < 2:
        print(__doc__)
        sys.exit(1)

    disk_path = sys.argv[1]
    password  = sys.argv[2] if len(sys.argv) >= 3 else 'root'

    if not os.path.exists(disk_path):
        print(f"[Error] Disk image not found: {disk_path}", file=sys.stderr)
        sys.exit(1)

    print(f"[Cube] Preparing nocloud image for serial login: {disk_path}")
    print(f"[Cube] Setting root password to: '{password}'")

    try:
        f = open(disk_path, 'r+b')
    except PermissionError:
        print(f"[Error] Cannot write {disk_path} (owned by another user). Re-run with sudo.",
              file=sys.stderr)
        sys.exit(1)

    with f:
        part_offset = find_linux_partition(f)
        print(f"[Cube] Linux partition at offset: {part_offset} (0x{part_offset:X})")

        fs = Ext4(f, part_offset)
        print(f"[Cube] Block size: {fs.sb.block_size}, Inode size: {fs.sb.inode_size}, "
              f"metadata_csum={'yes' if fs.sb.has_metadata_csum else 'no'}")

        shadow_ino = fs.resolve_path('/etc/shadow')
        if shadow_ino is None:
            print("[Error] /etc/shadow not found in filesystem", file=sys.stderr)
            sys.exit(1)
        print(f"[Cube] Found /etc/shadow at inode {shadow_ino}")

        shadow_ibuf, _ = fs.read_inode_raw(shadow_ino)
        if not fs.verify_inode_checksum(shadow_ino, shadow_ibuf):
            print("[Error] ext4 inode CRC32C self-check failed; refusing to patch.",
                  file=sys.stderr)
            sys.exit(1)

        shadow_content = fs.read_file(shadow_ino)
        print(f"[Cube] Read {len(shadow_content)} bytes from /etc/shadow")

        for line in shadow_content.decode('utf-8', errors='replace').splitlines():
            if line.startswith('root:'):
                print(f"[Cube] Current root entry: root:{line.split(':')[1][:25]}...")
                break

        new_shadow = patch_shadow(shadow_content, password)
        print(f"[Cube] Patched shadow ({len(shadow_content)}B -> {len(new_shadow)}B)")
        fs.write_file(shadow_ino, new_shadow)

        patch_machine_id(fs)
        patch_serial_autologin(fs)

    print(f"[Cube] Done. Root password in {disk_path} set to '{password}'")
    print(f"[Cube] Serial console autologin is enabled; password login is root:{password}")

if __name__ == '__main__':
    main()
