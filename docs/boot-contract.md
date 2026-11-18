# Stage 1 UEFI-to-kernel handoff contract — COMPLETE

Stage 1 is **COMPLETE** on AArch64 QEMU `virt-11.1`, EDK II UEFI, Cortex-A76, one CPU. Acceptance evidence is 20 consecutive successful cold boots, with one `GENESIS: stage1 boot ok` banner in each preserved capture at `build/logs/cold-boot-01.serial.log` through `cold-boot-20.serial.log`. The required regression remains `./tools/test-boot.sh`.

The UEFI entry validates the system table and searches for `EFI_DTB_TABLE_GUID`. If this firmware does not publish that table, it uses the **EDK II-specific** FDT Client protocol, whose implementation validates the firmware FDT before exposing tree queries. This fallback is specific to EDK II; other firmware may expose the device tree through a different interface. The loader requires an enabled `arm,pl011` compatible node with a two-cell base and size, a nonzero aligned base, and a register range large enough for PL011.

The loader copies the FDT into Genesis-owned static storage when the configuration table is available. With the FDT Client fallback, it copies the validated PL011 base and size into the handoff; the firmware-owned FDT is not retained. Both paths retrieve the UEFI memory map immediately before `ExitBootServices`. A stale map key (`EFI_INVALID_PARAMETER`) causes a fresh map query and retry without intervening allocations. Other errors stop the handoff.

After successful `ExitBootServices`, assembly reads `CurrentEL`, leaves the existing exception level and translation regime unchanged, installs the 16-byte-aligned Genesis static stack, passes a pointer to the handoff in `x0`, and branches using the AArch64 C calling convention. The handoff is `repr(C)`, version 1, and contains its size, FDT pointer/length when copied, memory-map pointer/length/descriptor metadata, validated PL011 base, and observed exception level. The memory map and copied FDT reside in static image storage and remain valid after boot services exit.

The kernel validates the handoff header and, when present, revalidates the FDT and confirms its PL011 base matches the loader’s value. It enables the PL011 and transmitter while preserving firmware framing, writes exactly `GENESIS: stage1 boot ok` followed by CRLF, masks asynchronous exceptions, then waits forever with `wfe`.

Unsafe boundaries are limited to UEFI ABI function pointers, raw firmware tables/protocol outputs, bounded FDT byte parsing, volatile MMIO, and the stack/branch assembly. No allocator, scheduler, filesystem, graphics, network stack, or MMU transition is used.

## Reproduction

```sh
./tools/build.sh
./tools/test-boot.sh
```

The cold-boot test resets the project-local writable EDK II variables image from the installed firmware template for each run. It checks for the serial banner, sends `quit` through a local QEMU monitor socket, records each serial capture under `build/logs/`, and requires QEMU to exit with status 0.
