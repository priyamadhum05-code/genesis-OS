#!/usr/bin/env python3
"""
Project Genesis — m1n1 / Asahi Linux ARM64 Boot Image Generator

Prepends a 64-byte standard Linux AArch64 Image header (ARM\\x64) to a raw
kernel binary so it can be directly booted or chainloaded by m1n1 on Apple Silicon.
"""

import sys
import struct
import os

LINUX_ARM64_MAGIC = b"ARM\x64" # 0x644D5241
TEXT_OFFSET_DEFAULT = 0x80000 # 512 KiB standard AArch64 text offset
BRANCH_OPCODE = 0x14000010 # b 0x40 (branch past 64-byte header)
NOP_OPCODE = 0xD503201F # nop

def build_arm64_header(payload_size: int, text_offset: int = TEXT_OFFSET_DEFAULT) -> bytes:
    """
    Constructs the 64-byte Linux ARM64 Image header:
      0x00: code0 (branch past header: b 0x40)
      0x04: code1 (nop)
      0x08: text_offset (little-endian 64-bit)
      0x10: image_size (little-endian 64-bit)
      0x18: flags (little-endian 64-bit: 0 = LE, 4KB pages)
      0x20: res2 (8 bytes zero)
      0x28: res3 (8 bytes zero)
      0x30: res4 (8 bytes zero)
      0x38: magic (4 bytes 'ARM\x64')
      0x3C: res5 (4 bytes zero)
    """
    total_size = payload_size + 64
    flags = 0 # bit 0: endianness (0 = LE), bits 1-2: page size (0 = 4KB)

    header = bytearray(64)
    struct.pack_into("<I", header, 0x00, BRANCH_OPCODE)
    struct.pack_into("<I", header, 0x04, NOP_OPCODE)
    struct.pack_into("<Q", header, 0x08, text_offset)
    struct.pack_into("<Q", header, 0x10, total_size)
    struct.pack_into("<Q", header, 0x18, flags)
    struct.pack_into("<Q", header, 0x20, 0)
    struct.pack_into("<Q", header, 0x28, 0)
    struct.pack_into("<Q", header, 0x30, 0)
    header[0x38:0x3C] = LINUX_ARM64_MAGIC
    struct.pack_into("<I", header, 0x3C, 0)

    return bytes(header)

def main():
    input_path = sys.argv[1] if len(sys.argv) > 1 else "build/genesis.bin"
    output_path = sys.argv[2] if len(sys.argv) > 2 else "build/genesis-m1n1.img"

    if os.path.exists(input_path):
        with open(input_path, "rb") as f:
            payload = f.read()
    else:
        # Generate dummy 4096-byte payload if input not found (for dry-run / unit testing)
        payload = b"\x00" * 4096

    header = build_arm64_header(len(payload))

    os.makedirs(os.path.dirname(os.path.abspath(output_path)), exist_ok=True)
    with open(output_path, "wb") as f:
        f.write(header)
        f.write(payload)

    print(f"GENESIS m1n1 Image created: {output_path}")
    print(f"  Header size: 64 bytes")
    print(f"  Payload size: {len(payload)} bytes")
    print(f"  Total size: {len(header) + len(payload)} bytes")
    print(f"  Magic: {header[0x38:0x3C]} (ARM\\x64)")
    print(f"  Text Offset: 0x{TEXT_OFFSET_DEFAULT:X}")

if __name__ == "__main__":
    main()
