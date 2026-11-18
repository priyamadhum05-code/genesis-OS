#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

./tools/build.sh

QEMU_VARS="$(brew --prefix qemu)/share/qemu/edk2-arm-vars.fd"
mkdir -p build/stage3c1-validation
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
validation = root / "build" / "stage3c1-validation"
log = validation / "stage3c1.serial.log"
summary = validation / "stage3c1-summary.log"
monitor = root / "build" / "qemu-monitor.sock"

stage1_line = "GENESIS: stage1 boot ok"
stage2_pattern = re.compile(r"GENESIS: stage2 conventional pages=([0-9]+)")
pmm_init_pattern = re.compile(r"GENESIS: pmm init ok total_usable_pages=([0-9]+) free_pages=([0-9]+)")
preflight_line = "GENESIS: vmm 3c-1 preflight ok"
pmm_test_line = "GENESIS: pmm test ok"

with log.open("wb") as output:
    proc = subprocess.Popen(["./tools/run-qemu.sh"], cwd=root, stdout=output, stderr=subprocess.STDOUT)
    deadline = time.monotonic() + 30
    sent_quit = False
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"QEMU exited prematurely (status {proc.returncode})")
        text = log.read_text(errors="replace")
        lines = [line.strip() for line in text.splitlines()]
        
        has_s1 = lines.count(stage1_line) == 1
        has_s2 = any(stage2_pattern.fullmatch(l) for l in lines)
        has_pmm_init = any(pmm_init_pattern.fullmatch(l) for l in lines)
        has_preflight = lines.count(preflight_line) == 1
        has_pmm_test = lines.count(pmm_test_line) == 1

        if has_s1 and has_s2 and has_pmm_init and has_preflight and has_pmm_test:
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
        raise RuntimeError("Required output missing or monitor unavailable")
    
    status = proc.wait(timeout=5)
    if status != 0:
        raise RuntimeError(f"QEMU exit status was {status}, expected 0")

text = log.read_text(errors="replace")
lines = [line.strip() for line in text.splitlines()]
stage2_matches = [stage2_pattern.fullmatch(l) for l in lines if stage2_pattern.fullmatch(l)]
pmm_init_matches = [pmm_init_pattern.fullmatch(l) for l in lines if pmm_init_pattern.fullmatch(l)]

if lines.count(stage1_line) != 1 or len(stage2_matches) != 1 or len(pmm_init_matches) != 1 or lines.count(preflight_line) != 1 or lines.count(pmm_test_line) != 1:
    raise RuntimeError("Final serial capture failed exact-line checks")

conv_pages = int(stage2_matches[0].group(1))
usable_pages = int(pmm_init_matches[0].group(1))
free_pages = int(pmm_init_matches[0].group(2))

report = [
    "=== PROJECT GENESIS STAGE 3C-1 PREFLIGHT VALIDATION SUMMARY ===",
    f"Stage 1 serial: {stage1_line} (COUNT=1)",
    f"Stage 2 serial: GENESIS: stage2 conventional pages={conv_pages} (COUNT=1)",
    f"Phase 2 serial: GENESIS: pmm init ok total_usable_pages={usable_pages} free_pages={free_pages} (COUNT=1)",
    f"Stage 3C-1 serial: {preflight_line} (COUNT=1)",
    f"Phase 2 serial: {pmm_test_line} (COUNT=1)",
    f"QEMU process exit code: {status}",
    "Verdict: ALL STAGE 3C-1 PREFLIGHT ACCEPTANCE CRITERIA PASSED",
]
summary_text = "\n".join(report) + "\n"
summary.write_text(summary_text)

print(summary_text)
PY
