#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
QEMU_VARS="$(brew --prefix qemu)/share/qemu/edk2-arm-vars.fd"
mkdir -p build/logs
./tools/build.sh
cp build/target/aarch64-unknown-uefi/release/genesis-stage1.efi build/esp/EFI/BOOT/BOOTAA64.EFI

python3 - "$QEMU_VARS" <<'PY'
import os
import shutil
import socket
import subprocess
import sys
import time

vars_template = sys.argv[1]
message = b"GENESIS: stage1 boot ok"
for run in range(1, 21):
    log = f"build/logs/cold-boot-{run:02}.serial.log"
    monitor = "build/qemu-monitor.sock"
    try:
        os.unlink(monitor)
    except FileNotFoundError:
        pass
    shutil.copyfile(vars_template, "build/edk2-vars.fd")
    with open(log, "wb") as output:
        proc = subprocess.Popen(["./tools/run-qemu.sh"], stdout=output, stderr=subprocess.STDOUT)
        deadline = time.monotonic() + 30
        connected = False
        while time.monotonic() < deadline:
            if proc.poll() is not None:
                raise RuntimeError(f"boot {run}: QEMU exited early with {proc.returncode}")
            with open(log, "rb") as capture:
                contents = capture.read()
            if message in contents:
                deadline_socket = time.monotonic() + 2
                while time.monotonic() < deadline_socket:
                    try:
                        connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                        connection.connect(monitor)
                        connection.sendall(b"quit\n")
                        connection.close()
                        connected = True
                        break
                    except (FileNotFoundError, ConnectionRefusedError):
                        time.sleep(0.05)
                break
            time.sleep(0.05)
        if not connected:
            proc.terminate()
            proc.wait(timeout=5)
            raise RuntimeError(f"boot {run}: expected serial message or monitor unavailable; QEMU status {proc.returncode}")
        status = proc.wait(timeout=5)
        if status != 0:
            raise RuntimeError(f"boot {run}: QEMU exit status {status}")
        if message not in contents:
            raise RuntimeError(f"boot {run}: serial banner missing")
        print(f"cold boot {run:02}/20: PASS (QEMU exit {status})")
print("20 consecutive cold boots: PASS")
PY
