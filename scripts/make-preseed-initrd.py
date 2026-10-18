#!/usr/bin/env python3
"""
Injects preseed.cfg directly into a Debian netboot initrd.gz cpio archive so
the Debian installer runs fully unattended.

Decompresses the base initrd, injects preseed.cfg before the archive trailer,
and recompresses as a single valid gzip stream (preventing "no cpio magic" errors).

Usage:
    python3 scripts/make-preseed-initrd.py <input_initrd.gz> <output_initrd.gz> <preseed.cfg>
"""

import sys
import os
import gzip
import time

def pad4(n):
    return (n + 3) & ~3

def make_cpio_entry(name, content):
    """Build a single newc (SVR4 no-CRC) cpio entry for a regular file."""
    name_bytes = name.encode() + b'\x00'
    name_len = len(name_bytes)
    content_len = len(content)

    header = (
        b'070701'                               # magic
        + b'00000001'                           # ino
        + b'000081A4'                           # mode: 0o100644 = rw-r--r--
        + b'00000000'                           # uid
        + b'00000000'                           # gid
        + b'00000001'                           # nlink
        + ('%08X' % int(time.time())).encode()  # mtime
        + ('%08X' % content_len).encode()       # filesize
        + b'00000000'                           # devmajor
        + b'00000000'                           # devminor
        + b'00000000'                           # rdevmajor
        + b'00000000'                           # rdevminor
        + ('%08X' % name_len).encode()          # namesize
        + b'00000000'                           # check
    )
    assert len(header) == 110

    entry = header + name_bytes
    entry += b'\x00' * (pad4(len(entry)) - len(entry))
    entry += content
    entry += b'\x00' * (pad4(content_len) - content_len)
    return entry

def make_trailer():
    """CPIO archive trailer."""
    name = b'TRAILER!!!\x00'
    header = (
        b'070701'
        + b'00000000' * 13
        + ('%08X' % len(name)).encode()
        + b'00000000'
    )
    entry = header + name
    entry += b'\x00' * (pad4(len(entry)) - len(entry))
    return entry

def main():
    if len(sys.argv) != 4:
        print("Usage: make-preseed-initrd.py <input_initrd.gz> <output_initrd.gz> <preseed.cfg>")
        sys.exit(1)

    input_path, output_path, preseed_path = sys.argv[1], sys.argv[2], sys.argv[3]

    for p in [input_path, preseed_path]:
        if not os.path.exists(p):
            print(f"[Error] File not found: {p}", file=sys.stderr)
            sys.exit(1)

    print(f"[Cube] Reading initrd: {input_path} ({os.path.getsize(input_path) // 1024} KB)")
    with open(input_path, 'rb') as f:
        compressed_in = f.read()

    decompressed = gzip.decompress(compressed_in)
    print(f"[Cube] Decompressed initrd: {len(decompressed) // 1024} KB")

    with open(preseed_path, 'rb') as f:
        preseed_content = f.read()

    # Find the last TRAILER!!! in the CPIO archive
    idx = decompressed.rfind(b"TRAILER!!!")
    if idx == -1:
        print("[Error] No CPIO trailer found in decompressed initrd", file=sys.stderr)
        sys.exit(1)

    trailer_pos = idx - 110  # SVR4 newc header is 110 bytes
    new_entry = make_cpio_entry("preseed.cfg", preseed_content)
    new_trailer = make_trailer()

    new_cpio = decompressed[:trailer_pos] + new_entry + new_trailer
    # Pad to 512-byte boundary
    pad = (512 - len(new_cpio) % 512) % 512
    new_cpio += b'\x00' * pad

    print(f"[Cube] Injected preseed.cfg ({len(preseed_content)} bytes)")
    recompressed = gzip.compress(new_cpio, compresslevel=1)

    with open(output_path, 'wb') as f:
        f.write(recompressed)

    print(f"[Cube] Written preseeded initrd: {output_path} ({os.path.getsize(output_path) // 1024} KB)")

if __name__ == '__main__':
    main()
