# Project Genesis — Stage 3C Virtual Memory Transition Architecture & Audit

**Date:** 2026-11-17  
**Milestone:** Phase 3 (Virtual Memory Management) — Stage 3C-1 Audit & Reconciliation  
**Target:** AArch64 UEFI on QEMU ARM64 `virt-11.1` (Cortex-A76, 256 MiB DRAM)  
**Status:** Memory Layout Reconciled, System-Wide W^X Verified, Transition Hazards Analyzed  

---

## 1. Concrete Physical Memory Layout & Allocation Reconciliation

### 1.1 Verified PE Sections and Static Allocations
All static structures within Genesis Stage 1 are placed within the 4 MiB kernel reserve window (`0x4800_0000` to `0x4840_0000`).
The exact layout, derived from PE headers, runtime symbols, and measured physical pointers, is:

| Region / Symbol | Physical Address Range | Size | Alignment | Permissions | Purpose |
|---|---|---|---|---|---|
| **PE Header** | `0x4800_0000` .. `0x4800_1000` | 4,096 B (1 page) | 4 KiB | RO / NX | PE/COFF header and metadata |
| **`.text`** | `0x4800_1000` .. `0x4800_B000` | 40,960 B (10 pages) | 4 KiB | RO / X | Kernel entry, runtime, VMM code |
| **`.rdata`** | `0x4800_B000` .. `0x4800_C000` | 4,096 B (1 page) | 4 KiB | RO / NX | String constants, read-only tables |
| **`.data` (Total)** | `0x4800_C000` .. `0x4825_7000` | 2,404,352 B (587 pages) | 4 KiB | RW / NX | Writable kernel data and static buffers |
| ↳ `KERNEL_PMM` | `0x4800_C000` .. `0x4801_C020` | 65,568 B (16 pages + 32 B) | 4 KiB | RW / NX | PMM allocator metadata and bitmap |
| ↳ `MEMORY_MAP` | `0x4801_C020` .. `0x4803_C020` | 131,072 B (32 pages) | 16 B | RW / NX | UEFI memory map buffer (128 KiB) |
| ↳ `KERNEL_STACK` | `0x4803_C020` .. `0x4804_4020` | 32,768 B (8 pages) | 16 B | RW / NX | Active kernel execution stack (32 KiB) |
| ↳ `FDT_COPY` | `0x4804_4020` .. `0x4824_4020` | 2,097,152 B (512 pages) | 16 B | RW / NX | Resident Flattened Device Tree copy |
| ↳ `DORMANT_TTBR0_POOL` | `0x4824_4020` .. `0x4824_D030` | 36,880 B (9 pages + 16 B) | 16 B | RW / NX | TTBR0 page-table pool storage |
| ↳ `DORMANT_TTBR1_POOL` | `0x4824_D030` .. `0x4825_6040` | 36,880 B (9 pages + 16 B) | 16 B | RW / NX | TTBR1 page-table pool storage |
| ↳ `HANDOFF` | `0x4825_6040` .. `0x4825_6090` | 80 B | 16 B | RW / NX | Handoff metadata structure |
| ↳ `.data` BSS / padding | `0x4825_6090` .. `0x4825_7000` | 3,952 B | — | RW / NX | Section padding to page boundary |
| **`.reloc`** | `0x4825_7000` .. `0x4825_8000` | 4,096 B (1 page) | 4 KiB | RO / NX | Base relocations |
| **Reserve Buffer** | `0x4825_8000` .. `0x4840_0000` | 1,736,704 B (424 pages) | 4 KiB | RW / NX | Unused reserve within 4 MiB footprint |
| **PL011 UART MMIO** | `0x0900_0000` .. `0x0900_1000` | 4,096 B (1 page) | 4 KiB | RW / NX | Device MMIO (AttrIndx 0, nGnRnE) |

### 1.2 Stack Pointer Bounds Verification
- **Allocated Stack Range:** `0x4803_C020` .. `0x4804_4020` (Size: 32,768 bytes).
- **Stack Growth:** Decrementing downward from stack end `0x4804_4020`.
- **Measured SP:** `0x4804_3D40`.
- **Verification:**
  - $0x4803\_C020 \le 0x4804\_3D40 < 0x4804\_4020$.
  - Used stack space: $0x4804\_4020 - 0x4804\_3D40 = 0x2E0 = 736$ bytes.
  - Remaining headroom: $0x4804\_3D40 - 0x4803\_C020 = 0x7D20 = 32,032$ bytes ($> 31$ KiB).
  - The SP pointer is strictly within the allocated stack boundary with ample headroom.

### 1.3 Page-Table Frame Allocation & Disjointness Proof
Each `DormantTablePool` contains `storage: [u8; 36880]` aligned internally to a 4 KiB boundary:
- **TTBR0 Pool:**
  - Storage buffer: `0x4824_4020` .. `0x4824_D030`.
  - 4 KiB aligned base (`base_paddr`): `0x4824_5000`.
  - Span of 8 frames: `0x4824_5000` .. `0x4824_D000` (ends before `0x4824_D030`).
  - Allocated frames (7 used):
    - `0x4824_5000` (L0 root)
    - `0x4824_6000` (L1 table)
    - `0x4824_7000` (L2 table for kernel image)
    - `0x4824_8000` (L3 table for image block 0)
    - `0x4824_9000` (L3 table for image block 1)
    - `0x4824_A000` (L2 table for UART)
    - `0x4824_B000` (L3 table for UART)
- **TTBR1 Pool:**
  - Storage buffer: `0x4824_D030` .. `0x4825_6040`.
  - 4 KiB aligned base (`base_paddr`): `0x4824_E000`.
  - Span of 8 frames: `0x4824_E000` .. `0x4825_6000` (ends before `0x4825_6040`).
  - Allocated frames (7 used):
    - `0x4824_E000` (L0 root)
    - `0x4824_F000` (L1 table)
    - `0x4825_0000` (L2 table for kernel image)
    - `0x4825_1000` (L3 table for image block 0)
    - `0x4825_2000` (L3 table for image block 1)
    - `0x4825_3000` (L2 table for MMIO)
    - `0x4825_4000` (L3 table for MMIO)

**Pairwise Disjointness Proof:**
- Maximum TTBR0 frame: `0x4824_C000`.
- Minimum TTBR1 frame: `0x4824_E000`.
- Clearance between pools: $0x4824\_E000 - 0x4824\_C000 = 8,192$ bytes (2 full pages).
- Minimum HANDOFF address: `0x4825_6040` (exceeds TTBR1 pool end `0x4825_6000`).
- Minimum RELOC address: `0x4825_7000` (exceeds HANDOFF end `0x4825_6090`).
- Every static object and allocated page-table frame is mathematically and physically disjoint. Runtime assertions verify this on every boot.

---

## 2. System-Wide W^X & Physical Alias Verification

### 2.1 The Physical Alias Problem
Verifying individual translation tables or virtual addresses in isolation is insufficient for architectural security:
- A page table might map virtual address $VA_1 \mapsto PA$ as Read-Only / Executable (EL1 code).
- A second page table (or another mapping within the same table) could map virtual address $VA_2 \mapsto PA$ as Read-Write / Non-Executable (writable data).
- Although neither mapping is simultaneously writable and executable, the physical page $PA$ is mutable via $VA_2$ and executable via $VA_1$, violating the architectural principle of W^X.

### 2.2 Concrete Physical Alias Detection Implementation
Genesis implements `PhysicalWxTracker`, an audited data structure that tracks the cumulative permission state of up to 2,048 unique physical pages across all active translation regimes (`TTBR0_EL1` and `TTBR1_EL1`):
1. **Per-Physical Page State:** Each physical frame is tracked with bit flags `HAS_WRITE` and `HAS_EXEC`.
2. **Conflict Invariant:** If `record_mapping(pa, is_writable, is_executable)` attempts to mark a page as writable when it has already been mapped executable (or vice versa), the tracker returns `Err(VmmError::WxPhysicalAliasViolation(pa))`.
3. **Hierarchical Table Permission Limit Accounting:**
   According to ARM DDI 0487 Section D5.4:
   - Bit 62 of table descriptors (`APTable[1]`): When set to 1, forces all downstream table, block, and page lookups to be Read-Only, overriding any descriptor `AP[2] = 0`.
   - Bit 59 of table descriptors (`PXNTable`): When set to 1, forces all downstream lookups to be Privileged Execute-Never (`PXN = 1`).
   - Bit 60 of table descriptors (`UXNTable`): When set to 1, forces all downstream lookups to be Unprivileged Execute-Never (`UXN = 1`).
   The Genesis `PageTable::get_descriptor` walker extracts these table bits during traversal, computing effective leaf permissions before passing them to the physical alias tracker.
4. **Audit Scope:**
   The preflight audit examines:
   - 1,024 kernel image pages across TTBR0 (identity map `0x4800_0000` .. `0x4840_0000`).
   - 1 PL011 UART page in TTBR0 (`0x0900_0000`).
   - 1,024 kernel image pages across TTBR1 (higher-half `0xFFFF_FFFF_8000_0000`).
   - 1 PL011 UART page in TTBR1 (higher-half `0xFFFF_FFFF_F000_0000`).
   Total unique physical pages evaluated: **1,025**.
   Result: **0 direct W^X violations, 0 physical alias violations**.

---

## 3. Exception-Handler Design & Transition Safety Audit

### 3.1 Status: Implemented vs. Hypothetical
- **Currently Implemented:** Genesis does NOT install or modify `VBAR_EL1`. The firmware vector table at `0x4FD66000` remains active.
- **Hypothetical / Required for Stage 3C-2:** A dedicated EL1 transition vector table must be installed prior to switching `TTBR0_EL1`.

### 3.2 Architectural Invariants for AArch64 Vector Tables
Under ARM DDI 0487 Section D1.10:
1. **Alignment:** `VBAR_EL1` requires strict 2 KiB (2,048-byte) alignment (bits [10:0] must be 0).
2. **Structure:** 16 vector slots of 128 bytes (32 instructions) each:
   - Current EL with SP0: Synchronous, IRQ, FIQ, SError (slots 0–3)
   - Current EL with SPx: Synchronous, IRQ, FIQ, SError (slots 4–7)
   - Lower EL using AArch64: Synchronous, IRQ, FIQ, SError (slots 8–11)
   - Lower EL using AArch32: Synchronous, IRQ, FIQ, SError (slots 12–15)

### 3.3 The Translation Switch Trap & Vector Unavailability Hazard
- Firmware's `VBAR_EL1 = 0x4FD66000` is located in high firmware DRAM.
- Genesis's dormant identity trampoline in `TTBR0_EL1` maps ONLY the 4 MiB kernel window (`0x4800_0000` .. `0x4840_0000`) and the UART (`0x0900_0000`). It does NOT map `0x4FD66000`.
- **The Critical Trap:** If any synchronous abort, SError, or interrupt occurs immediately after `msr TTBR0_EL1`, the CPU attempts to fetch instructions from `0x4FD66000`. Because that address is unmapped in the new `TTBR0_EL1`, a synchronous Instruction Abort is triggered at vector entry. Since the vector itself cannot be fetched, this causes a fatal double-fault CPU lockup.
- **Architectural Mitigation:**
  Before writing `TTBR0_EL1` in Checkpoint 3C-2, Genesis must install a resident transition vector table located in `.text` (e.g. `0x4800_2000`, 2 KiB aligned).

### 3.4 Strict Transition Exception Handler Rules
Any exception handler during the MMU transition must adhere to:
1. **Register-Only Execution (`nomem`, `nostack`):**
   - Must NOT push registers to the stack. If the exception occurred during an SP transition or stack fault, stack access will fault recursively.
   - Must use scratch registers `x0`–`x3` only.
2. **UART MMIO Diagnostics:**
   - Read `ESR_EL1` (syndrome), `FAR_EL1` (fault address), and `ELR_EL1` (return address).
   - Write ASCII hex directly to PL011 UART MMIO (`0x0900_0000`) using busy-wait polling on `UARTFR.TXFF`.
3. **Classification:**
   - All exceptions during transition are treated as **terminal**. Recovery is impossible without risking state corruption; handlers print diagnostics and execute `wfi` / `b .` loop.

---

## 4. Translation-Regime Transition Analysis (ARM DDI 0487)

### 4.1 Hazard: T0SZ Modification from 24 to 16 with Active MMU
1. **EDK II Configuration:**
   - Measured `TCR_EL1 = 0x0000000280803518`.
   - `T0SZ = 24` $\implies$ Virtual address space size is $2^{64-24} = 2^{40} = 1 \text{ TiB}$.
   - Under 4 KiB granule, a 40-bit VA starts translation at **Level 1** (spanning 1 GiB per entry). EDK II's root pointer in `TTBR0_EL1` is a **Level 1 table**.
2. **Genesis Configuration:**
   - Required `T0SZ = 16` $\implies$ Virtual address space size is $2^{64-16} = 2^{48} = 256 \text{ TiB}$.
   - Under 4 KiB granule, a 48-bit VA starts translation at **Level 0** (spanning 512 GiB per entry). Genesis's root pointer is a **Level 0 table**.
3. **The Starting-Level Inconsistency Hazard:**
   - If `TCR_EL1` is written before `TTBR0_EL1`: The hardware walker immediately reinterprets the firmware's Level 1 table address in `TTBR0_EL1` as a Level 0 table address. The entry lookup indexing switches from bits [38:30] to bits [47:39]. The walker reads garbage descriptors, triggering immediate Translation Faults.
   - If `TTBR0_EL1` is written before `TCR_EL1`: The hardware walker attempts to use Genesis's Level 0 root as a Level 1 table, reading entry 1 (bits [38:30] of `0x4800_0000`) instead of entry 0.
4. **Architectural Resolution (ARM DDI 0487 Section D5.2 & D5.10):**
   The architecture prohibits changing the base level of translation table walks while translation table walks are actively permitted on that regime, unless:
   - **Method A (Walk Gating via EPD0):**
     1. Write `TCR_EL1` with `EPD0 = 1` (disabling all TTBR0 translation table walks).
     2. Execute `isb` (context synchronization).
     3. Write new `TTBR0_EL1` (Genesis Level 0 root) and new `TTBR1_EL1` (Genesis Level 0 root).
     4. Write full `TCR_EL1` with `T0SZ = 16`, `T1SZ = 16`, and `EPD0 = 0`, `EPD1 = 0`.
     5. Execute `isb`.
     6. Execute `tlbi vmalle1is` (invalidate all cached firmware translations).
     7. Execute `dsb ish` (wait for invalidation to complete).
     8. Execute `isb` (flush pipeline).
     Because instruction fetching at the current PC is cached in the CPU L1 I-cache, gating walks for 3 instructions is fully valid and avoids any intermediate walker dereference of incompatible starting levels.
   - **Method B (Back-to-Back Atomic Register Pair Write):**
     Ensure the trampoline maps the executing instruction page identically under both regimes, disable all interrupts, and issue `msr TCR_EL1` and `msr TTBR0_EL1` back-to-back with an immediate `isb`.
     *Genesis will adopt Method A for Checkpoint 3C-2, providing mathematical walk-gating immunity.*

### 4.2 Complete Register Transition Sequence

```
[Phase 1: Commit Table Structures]
    dsb ishst                       // Ensure all translation table writes are visible to walker

[Phase 2: Gate TTBR0 Walks & Update Attributes]
    msr MAIR_EL1, x_mair            // Update memory attribute definitions
    mrs x0, TCR_EL1
    orr x0, x0, #(1 << 7)           // Set TCR_EL1.EPD0 = 1 (disable TTBR0 walks)
    msr TCR_EL1, x0
    isb                             // Context synchronization barrier

[Phase 3: Install Translation Table Roots]
    msr TTBR0_EL1, x_ttbr0_root     // Genesis identity trampoline L0 root
    msr TTBR1_EL1, x_ttbr1_root     // Genesis higher-half L0 root

[Phase 4: Configure Final TCR Configuration & Re-enable Walks]
    msr TCR_EL1, x_genesis_tcr      // T0SZ=16, T1SZ=16, TG0=4K, TG1=4K, SH=IS, ORGN/IRGN=WB-WA, EPD0=0, EPD1=0
    isb                             // Synchronize register updates

[Phase 5: Invalidate Stale Firmware Translations]
    tlbi vmalle1is                  // Invalidate all Stage 1 EL1 translations
    dsb ish                         // Wait for completion across Inner Shareable domain
    isb                             // Flush pipeline and resume execution
```

---

## 5. Verification Checklist & Gate Criteria

- [x] Host unit tests pass: 37/37 tests.
- [x] UEFI release build clean: exit code 0.
- [x] Stage 3C-1 preflight test clean: serial validation confirmed.
- [x] Zero overlap verified between all static objects and page-table frames.
- [x] Physical W^X alias detection confirmed with zero violations across 1,025 mapped pages.
- [x] Historical 20 cold-boot captures (`build/logs/cold-boot-*.serial.log`) verified untouched.
- [ ] Explicit user authorization received before Checkpoint 3C-2 implementation.
