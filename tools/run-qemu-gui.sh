#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
QEMU="$(brew --prefix qemu)/bin/qemu-system-aarch64"
FW="$(brew --prefix qemu)/share/qemu"
[ -f build/edk2-vars.fd ] || cp "$FW/edk2-arm-vars.fd" build/edk2-vars.fd
[ -f build/esp/WEIGHTS.BIN ] || python3 -c "
import struct
with open('build/esp/WEIGHTS.BIN', 'wb') as f:
    for i in range(64):
        for k in range(64):
            f.write(struct.pack('<f', (i + k + 1) * 0.01))
    for k in range(64):
        for j in range(64):
            f.write(struct.pack('<f', (k * 2 + j + 1) * 0.005))
"
exec "$QEMU" -machine virt-11.1,gic-version=3 -cpu cortex-a76 -smp "${QEMU_SMP:-4}" -m 256M \
  -drive if=pflash,format=raw,unit=0,readonly=on,file="$FW/edk2-aarch64-code.fd" \
  -drive if=pflash,format=raw,unit=1,file=build/edk2-vars.fd \
  -drive if=none,format=raw,file=fat:rw:build/esp,id=esp \
  -device virtio-blk-device,drive=esp \
  -device ramfb \
  -device virtio-gpu-pci \
  -serial stdio \
  -monitor unix:build/qemu-monitor.sock,server=on,wait=off \
  -display default -no-reboot "$@"
