#!/usr/bin/env bash
set -e

# Project Genesis — m1n1 / Apple Silicon Image Build & Validation Script (Stage 27)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

cd "${ROOT_DIR}"

echo "=== Building Genesis Kernel ==="
./tools/build.sh

KERNEL_BIN="build/target/aarch64-unknown-uefi/release/genesis-stage1.efi"
OUTPUT_IMG="build/genesis-m1n1.img"

if [ ! -f "${KERNEL_BIN}" ]; then
    echo "Error: Kernel binary not found at ${KERNEL_BIN}"
    exit 1
fi

echo "=== Packaging m1n1 ARM64 Image ==="
python3 "${ROOT_DIR}/tools/mk-m1n1-image.py" "${KERNEL_BIN}" "${OUTPUT_IMG}"

if [ ! -f "${OUTPUT_IMG}" ]; then
    echo "Error: Output image not created at ${OUTPUT_IMG}"
    exit 1
fi

# Verify header magic and branch opcode
python3 -c '
import sys
with open("build/genesis-m1n1.img", "rb") as f:
    hdr = f.read(64)
assert len(hdr) == 64, "Header too short"
assert hdr[0x38:0x3C] == b"ARM\x64", f"Invalid magic: {hdr[0x38:0x3C]}"
b_op = int.from_bytes(hdr[0:4], "little")
assert b_op == 0x14000010, f"Invalid branch opcode: 0x{b_op:08X}"
print("VERIFICATION: genesis-m1n1.img header valid (ARM\\x64 magic, b 0x40 entry)")
'

echo "=== m1n1 Image Build Successful: ${OUTPUT_IMG} ==="
