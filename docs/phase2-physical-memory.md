# Phase 2 Physical Memory Management — COMPLETE

Phase 2 establishes reliable physical memory management for Project Genesis on AArch64 QEMU `virt-11.1` under EDK II UEFI.

## 1. Architecture & Design Decisions

### Allocator Choice: Dual-Bitmap Physical Page Allocator
Genesis uses a fixed-capacity, page-aligned dual-bitmap allocator located in Genesis-owned static storage (`KERNEL_PMM` in `.data`):
- **Granule / Page Size:** 4096 bytes (4 KiB) (`EFI_PAGE_SIZE`), matching the UEFI standard and Cortex-A76 translation granule.
- **Dual-Bitmap Structure:**
  - `usable_bitmap`: Invariant mask indicating pages that belong to the conventional DRAM pool managed by Genesis (1 = usable conventional page, 0 = reserved / firmware / MMIO / Genesis static image).
  - `allocated_bitmap`: Dynamic allocation state (1 = allocated or unavailable, 0 = free and available for allocation).
- **Metadata Overhead:**
  - Capacity: 4,096 64-bit words tracks 262,144 pages (1 GiB physical address span).
  - Memory footprint: 32 KiB for `usable_bitmap` + 32 KiB for `allocated_bitmap` = 64 KiB total in BSS/data.
  - Zero dynamic heap requirement; completely eliminates bootstrap circular dependencies.

### Invariant & Safety Protections
1. **Dynamic Tracked Range & Bounded Capacity:**
   - The allocator derives its tracked base address dynamically from the memory map: `base_paddr = min_conventional_paddr`.
   - The span is calculated as `max_conventional_paddr - min_conventional_paddr`.
   - If the required page span exceeds allocator capacity, initialization aborts with `AllocError::CapacityExceeded` rather than silently omitting or truncating physical memory.
2. **Strict Usability & Deallocation Invariant:**
   - A page can **only** be allocated if `usable == 1` and `allocated == 0`.
   - A page can **only** be freed if `usable == 1` and `allocated == 1`.
   - Firmware-owned memory (runtime services, ACPI, SMBIOS), MMIO (such as PL011 at `0x09000000`), holes, and Genesis internal storage (kernel code, stack, handoff, memory-map buffer) have `usable == 0` and `allocated == 1`.
   - Calling `free_page()` on any reserved, firmware, never-allocated, or out-of-bounds address is immediately rejected (`AllocError::ProtectedRegion` or `AllocError::OutOfBounds`).
   - Calling `free_page()` on an already-free page is immediately rejected (`AllocError::DoubleFree`).
3. **Atomic Range Validation:**
   - In `free_contiguous_pages(paddr, count)`, the entire range `[paddr, paddr + count * 4096)` is validated for alignment, bounds, usability, and allocated status **before** any bit is modified.
   - If any page in the range fails validation, the function immediately returns an error and modifies **zero** bits in the allocator state.
4. **Physical Addressability Boundary:**
   - Physical address tracking is completely decoupled from memory dereferencing.
   - The allocator does not dereference physical addresses as raw pointers to manage or test them, avoiding synchronous Data Abort exceptions under UEFI firmware page-table configurations before Genesis initializes its own virtual memory subsystem in Phase 3.

### Algorithmic Complexity
- **Single Page Allocation:**
  - Best-case: $O(1)$ when free pages exist in the first checked word.
  - Worst-case: $O(W) = O(N)$ where $N$ is the page span and $W = N / 64$ words, accelerated by checking 64 pages per word comparison and finding bits via `(!word).trailing_zeros()` (compiles to AArch64 `rbit` + `clz`).
- **Contiguous Allocation:**
  - Worst-case: $O(N)$ linear scan across the tracked span.
- **Single Page Free:**
  - $O(1)$ address translation, bounds check, and bit clear.

## 2. Acceptance Criteria & Test Evidence

### Host Unit Test Suite (18 tests, 0 failures)
Ran via `cargo test --lib --target aarch64-apple-darwin`:
- Memory Map Parser:
  - `rejects_descriptor_stride_smaller_than_minimum_prefix`: PASS
  - `accepts_larger_descriptor_stride_with_padding`: PASS
  - `rejects_map_length_not_divisible_by_descriptor_stride`: PASS
  - `rejects_descriptor_that_extends_past_supplied_buffer`: PASS
  - `rejects_misaligned_physical_start`: PASS
  - `rejects_zero_page_descriptor`: PASS
  - `rejects_conventional_page_count_overflow`: PASS
  - `ignores_non_conventional_memory_types`: PASS
  - `totals_valid_conventional_memory_descriptors`: PASS
- Physical Memory Allocator:
  - `rejects_out_of_bounds_and_capacity_exceeded`: PASS
  - `tracks_conventional_memory_and_excludes_reserved`: PASS
  - `single_page_alloc_and_free`: PASS
  - `contiguous_alloc_and_free_across_word_boundary`: PASS
  - `rejects_double_free`: PASS
  - `rejects_free_of_protected_or_untracked_pages`: PASS
  - `rejects_misaligned_free_address`: PASS
  - `atomic_contiguous_free_preserves_state_on_partial_failure`: PASS
  - `exhaustion_and_reallocation`: PASS

### Isolated Stage 1 Regression
Executed in `build/phase2-validation/regression` via `./tools/test-boot.sh`:
- 20/20 consecutive cold boots: **PASS** (QEMU exit 0 for each).
- Original accepted Stage 1 captures in `build/logs/cold-boot-01.serial.log` through `cold-boot-20.serial.log` were preserved unmodified.

### Phase 2 QEMU Boot Verification
Executed via `./tools/test-phase2.sh`:
- Captured serial output in `build/phase2-validation/phase2.serial.log`:
  ```text
  GENESIS: stage1 boot ok
  GENESIS: stage2 conventional pages=56316
  GENESIS: pmm init ok total_usable_pages=55868 free_pages=55868
  GENESIS: pmm test ok
  ```
- Memory Reservation Verification:
  - Total conventional pages from UEFI memory map: 56,316.
  - Reserved range `0x4800_0000..0x4840_0000` (Genesis kernel image, static buffers, stack) overlapped 448 conventional pages within the span.
  - Usable PMM pool initialized to exactly `56,316 - 448 = 55,868` pages, confirming defense-in-depth kernel memory protection.
- QEMU monitor socket shutdown: **SUCCESS**.
- QEMU process exit code: **0** (confirmed and recorded in `build/phase2-validation/phase2-summary.log`).

## 3. Historical Milestone Distinction & Qualifications
- **Stage 1 (Boot & Handoff):** 20/20 cold boots confirmed with exit status 0. Accepted captures preserved.
- **Stage 2 (Memory-Map Validation):** 56,333 conventional pages confirmed. The historical qualification regarding the interrupted background smoke test runner whose QEMU exit code was not captured to disk remains documented as-is; it is not altered.
- **Phase 2 (Physical Memory Management):** Full allocator implementation, 18/18 host tests passed, 20/20 Stage 1 regression passed, and QEMU smoke test passed with exit code 0 explicitly captured to disk.
