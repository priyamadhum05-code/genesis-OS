#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
QEMU="$(brew --prefix qemu)/bin/qemu-system-aarch64"
FW="$(brew --prefix qemu)/share/qemu"
[ -f build/edk2-vars.fd ] || cp "$FW/edk2-arm-vars.fd" build/edk2-vars.fd
exec "$QEMU" -machine virt-11.1,gic-version=3 -cpu cortex-a76 -smp 1 -m 256M \
  -drive if=pflash,format=raw,unit=0,readonly=on,file="$FW/edk2-aarch64-code.fd" \
  -drive if=pflash,format=raw,unit=1,file=build/edk2-vars.fd \
  -drive if=none,format=raw,file=fat:rw:build/esp,id=esp \
  -device virtio-blk-device,drive=esp -serial stdio \
  -monitor unix:build/qemu-monitor.sock,server=on,wait=off \
  -display none -no-reboot "$@"
