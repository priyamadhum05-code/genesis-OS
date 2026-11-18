# Stage 2 memory-map validation — COMPLETE

Stage 2 consumes the UEFI memory map retained by the Stage 1 handoff after `ExitBootServices`. It validates the handoff-provided map length and descriptor stride, requires a full 40-byte EFI memory-descriptor prefix, and steps by the actual firmware descriptor size. Descriptor scalar fields are read unaligned, avoiding references to potentially unaligned storage. Every descriptor must have a page-aligned physical start, a nonzero page count, and a non-overflowing physical range. Only descriptors with type `EFI_CONVENTIONAL_MEMORY` (7) contribute to the total, using checked addition.

The kernel halts without printing a page count if validation fails. On success it prints the conventional page total in decimal using a fixed stack buffer; it uses neither a heap allocator nor UEFI Boot Services. The Stage 1 handoff layout and boot contract remain unchanged.

## Acceptance

- `./tools/build.sh` succeeds for `aarch64-unknown-uefi`.
- `./tools/test-stage2.sh` passes the unchanged 20-cold-boot Stage 1 regression in an isolated test copy, leaving the accepted captures untouched.
- Its Stage 2 QEMU boot emits exactly one positive decimal line of the form `GENESIS: stage2 conventional pages=N` and exactly one Stage 1 banner.
- The test harness quits QEMU through its local monitor and observes exit status 0.

## Acceptance evidence

`./tools/build.sh` succeeded. `./tools/test-stage2.sh` passed all 20 isolated Stage 1 regression boots and the Stage 2 QEMU boot. The captured Stage 2 lines were `GENESIS: stage1 boot ok` and `GENESIS: stage2 conventional pages=56333`; QEMU exited with status 0. The capture is at `build/stage2-validation/stage2.serial.log`. The existing Stage 1 captures were not regenerated or modified.

The parser's malformed-map rejection paths are bounds- and overflow-checked in code, but this acceptance run did not inject corrupted descriptors; QEMU supplied a valid firmware memory map.
