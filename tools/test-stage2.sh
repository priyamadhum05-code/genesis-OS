#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

./tools/build.sh

# Run the unchanged Stage 1 regression in a project-local copy so its fresh
# captures do not overwrite the accepted build/logs/cold-boot-*.serial.log.
python3 - "$PWD" <<'PY'
from pathlib import Path
import shutil
import subprocess
import sys

root = Path(sys.argv[1])
validation = root / "build" / "stage2-validation"
regression = validation / "regression"
if validation.exists():
    shutil.rmtree(validation)
validation.mkdir(parents=True)

def ignore_build_and_targets(directory, names):
    if Path(directory) == root:
        return {name for name in names if name in {"build", "target", ".git"}}
    return {name for name in names if name in {"target", ".git"}}

shutil.copytree(root, regression, ignore=ignore_build_and_targets)
(regression / "build" / "esp" / "EFI" / "BOOT").mkdir(parents=True)
shutil.copy2(root / "build" / "esp" / "startup.nsh", regression / "build" / "esp" / "startup.nsh")
subprocess.run(["./tools/test-boot.sh"], cwd=regression, check=True)
PY

QEMU_VARS="$(brew --prefix qemu)/share/qemu/edk2-arm-vars.fd"
cp build/target/aarch64-unknown-uefi/release/genesis-stage1.efi build/esp/EFI/BOOT/BOOTAA64.EFI
cp "$QEMU_VARS" build/edk2-vars.fd
rm -f build/qemu-monitor.sock

python3 - "$PWD" <<'PY'
from pathlib import Path
import re
import socket
import subprocess
import sys
import time

root = Path(sys.argv[1])
log = root / "build" / "stage2-validation" / "stage2.serial.log"
monitor = root / "build" / "qemu-monitor.sock"
stage1_line = "GENESIS: stage1 boot ok"
stage2_pattern = re.compile(r"GENESIS: stage2 conventional pages=([0-9]+)")

with log.open("wb") as output:
    proc = subprocess.Popen(["./tools/run-qemu.sh"], cwd=root, stdout=output, stderr=subprocess.STDOUT)
    deadline = time.monotonic() + 30
    sent_quit = False
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"QEMU exited before Stage 2 output (status {proc.returncode})")
        text = log.read_text(errors="replace")
        lines = [line.strip() for line in text.splitlines()]
        pages = [int(match.group(1)) for line in lines if (match := stage2_pattern.fullmatch(line))]
        if lines.count(stage1_line) == 1 and len(pages) == 1 and pages[0] > 0:
            socket_deadline = time.monotonic() + 3
            while time.monotonic() < socket_deadline:
                try:
                    connection = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                    connection.connect(str(monitor))
                    connection.sendall(b"quit\n")
                    connection.close()
                    sent_quit = True
                    break
                except (FileNotFoundError, ConnectionRefusedError):
                    time.sleep(0.05)
            break
        time.sleep(0.05)

    if not sent_quit:
        proc.terminate()
        proc.wait(timeout=5)
        raise RuntimeError("Stage 2 output missing, duplicated, non-positive, or monitor unavailable")
    status = proc.wait(timeout=5)
    if status != 0:
        raise RuntimeError(f"QEMU exit status was {status}, expected 0")

text = log.read_text(errors="replace")
lines = [line.strip() for line in text.splitlines()]
matches = [stage2_pattern.fullmatch(line) for line in lines]
pages = [int(match.group(1)) for match in matches if match]
if lines.count(stage1_line) != 1 or len(pages) != 1 or pages[0] <= 0:
    raise RuntimeError("Final serial capture failed exact-line acceptance checks")
print(f"Stage 2 serial: {stage1_line}")
print(f"Stage 2 serial: GENESIS: stage2 conventional pages={pages[0]}")
print("Stage 2 QEMU exit: 0")
PY
