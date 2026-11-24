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
python3 tools/gen-stage9-assets.py
python3 tools/gen-stage12-assets.py
python3 tools/gen-stage25-assets.py
[ -f build/target/aarch64-unknown-uefi/release/genesis-stage1.efi ] && cp build/target/aarch64-unknown-uefi/release/genesis-stage1.efi build/esp/EFI/BOOT/BOOTAA64.EFI
if [ "${GENESIS_HVF:-0}" = "1" ]; then
  ACCEL_OPTS="-machine virt,highmem=off,gic-version=3 -accel hvf -cpu host"
else
  ACCEL_OPTS="-machine virt-11.1,gic-version=3 -cpu cortex-a76"
fi

if [ "${GENESIS_HEADLESS:-0}" = "1" ] || echo " $* " | grep -q -- " -display none "; then
  DISPLAY_OPTS=""
elif [ "$(uname)" = "Darwin" ]; then
  DISPLAY_OPTS="-display cocoa,zoom-to-fit=on"
else
  DISPLAY_OPTS="-display default"
fi

exec "$QEMU" $ACCEL_OPTS -smp "${QEMU_SMP:-4}" -m 512M \
  -drive if=pflash,format=raw,unit=0,readonly=on,file="$FW/edk2-aarch64-code.fd" \
  -drive if=pflash,format=raw,unit=1,file=build/edk2-vars.fd \
  -drive if=none,format=raw,file=fat:rw:build/esp,id=esp \
  -device virtio-blk-device,drive=esp \
  -netdev user,id=net0 -device virtio-net-device,netdev=net0 \
  -netdev user,id=net1 -device e1000e,netdev=net1 \
  -device virtio-keyboard-device \
  -device virtio-tablet-device \
  -device ramfb \
  -device virtio-gpu-pci,xres=1440,yres=900 \
  -device qemu-xhci,id=xhci \
  -device ich9-ahci,id=ahci0 \
  -device usb-kbd \
  -audiodev none,id=snd0 -device virtio-sound-pci,audiodev=snd0 \
  -serial stdio \
  -monitor unix:build/qemu-monitor.sock,server=on,wait=off \
  $DISPLAY_OPTS -no-reboot "$@"
