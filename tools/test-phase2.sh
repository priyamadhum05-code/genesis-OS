#!/bin/sh
set -eu
cd "$(dirname "$0")/.."

./tools/build.sh

# Run the unchanged Stage 1 regression in an isolated project-local copy so its fresh
# captures do not overwrite the accepted build/logs/cold-boot-*.serial.log.
python3 - "$PWD" <<'PY'
from pathlib import Path
import shutil
import subprocess
import sys

root = Path(sys.argv[1])
validation = root / "build" / "phase2-validation"
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
print("=== Running isolated Stage 1 20-boot regression ===")
subprocess.run(["./tools/test-boot.sh"], cwd=regression, check=True)
print("=== Stage 1 isolated regression PASSED (20/20) ===")
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
validation = root / "build" / "phase2-validation"
log = validation / "phase2.serial.log"
summary = validation / "phase2-summary.log"
monitor = root / "build" / "qemu-monitor.sock"

stage1_line = "GENESIS: stage1 boot ok"
stage2_pattern = re.compile(r"GENESIS: stage2 conventional pages=([0-9]+)")
pmm_init_pattern = re.compile(r"GENESIS: pmm init ok total_usable_pages=([0-9]+) free_pages=([0-9]+)")
pmm_test_line = "GENESIS: pmm test ok"

with log.open("wb") as output:
    proc = subprocess.Popen(["./tools/run-qemu.sh"], cwd=root, stdout=output, stderr=subprocess.STDOUT)
    deadline = time.monotonic() + 30
    sent_quit = False
    while time.monotonic() < deadline:
        if proc.poll() is not None:
            raise RuntimeError(f"QEMU exited before Phase 2 output (status {proc.returncode})")
        text = log.read_text(errors="replace")
        lines = [line.strip() for line in text.splitlines()]
        
        has_s1 = lines.count(stage1_line) == 1
        has_s2 = any(stage2_pattern.fullmatch(l) for l in lines)
        has_pmm_init = any(pmm_init_pattern.fullmatch(l) for l in lines)
        has_pmm_test = lines.count(pmm_test_line) == 1

        if has_s1 and has_s2 and has_pmm_init and has_pmm_test:
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
        raise RuntimeError("Phase 2 output missing or monitor unavailable")
    
    status = proc.wait(timeout=5)
    if status != 0:
        raise RuntimeError(f"QEMU exit status was {status}, expected 0")

text = log.read_text(errors="replace")
lines = [line.strip() for line in text.splitlines()]
stage2_matches = [stage2_pattern.fullmatch(l) for l in lines if stage2_pattern.fullmatch(l)]
pmm_init_matches = [pmm_init_pattern.fullmatch(l) for l in lines if pmm_init_pattern.fullmatch(l)]

if lines.count(stage1_line) != 1 or len(stage2_matches) != 1 or len(pmm_init_matches) != 1 or lines.count(pmm_test_line) != 1:
    raise RuntimeError("Final serial capture failed exact-line checks")

conv_pages = int(stage2_matches[0].group(1))
usable_pages = int(pmm_init_matches[0].group(1))
free_pages = int(pmm_init_matches[0].group(2))

report = [
    "=== PROJECT GENESIS PHASE 2 VALIDATION SUMMARY ===",
    f"Stage 1 serial: {stage1_line} (COUNT=1)",
    f"Stage 2 serial: GENESIS: stage2 conventional pages={conv_pages} (COUNT=1)",
    f"Phase 2 serial: GENESIS: pmm init ok total_usable_pages={usable_pages} free_pages={free_pages} (COUNT=1)",
    f"Phase 2 serial: {pmm_test_line} (COUNT=1)",
    f"QEMU process exit code: {status}",
    "Verdict: ALL PHASE 2 ACCEPTANCE CRITERIA PASSED",
]
summary_text = "\n".join(report) + "\n"
summary.write_text(summary_text)

print(summary_text)
PY
