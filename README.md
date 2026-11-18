# Project Genesis — Bare-Metal Microkernel in Rust

Genesis is an experimental, bare-metal microkernel written in Rust (`no_std`, `no_main`) targeting AArch64 UEFI on QEMU `virt-11.1` (`cortex-a76`, 256 MiB DRAM).

## Milestones Completed

- **Stage 1 (UEFI Boot & Handoff): COMPLETE.** Boots under QEMU `virt-11.1`, validates PL011 UART from FDT / FdtClient protocol, exits UEFI Boot Services, switches to a Genesis-owned stack, and passes 20 consecutive cold boots preserved in `build/logs/cold-boot-01..20.serial.log`. See [boot contract](docs/boot-contract.md).
- **Stage 2 (Memory Map): COMPLETE.** Validates UEFI memory-map descriptors and reports conventional memory. See [Stage 2 contract](docs/stage2-memory-map.md).
- **Phase 2 (Physical Memory Manager): COMPLETE.** Page-aligned dual-bitmap allocator with dynamic address bounds, firmware/stack/image protection, atomic contiguous allocation/deallocation, and double-free rejection. See [Phase 2 physical memory](docs/phase2-physical-memory.md).
- **Stage 3C (Translation Regime & Higher-Half Migration): COMPLETE.** Dual-root translation tables (`TTBR0_EL1` identity trampoline and `TTBR1_EL1` higher-half at `0xFFFF_FFFF_8000_0000`), stackless MMU transition sequence, higher-half execution trampoline, and `TCR_EL1.EPD0 = 1` lower-half teardown. See [Stage 3C transition architecture](docs/stage3c-transition-architecture.md).
- **Stage 4 (Dynamic Higher-Half VMM, Full TrapFrame & GICv3 Timer): COMPLETE.** Higher-Half Direct Map (HHDM) at `0xFFFF_8000_0000_0000`, runtime dynamic page-table mapper backed by `KERNEL_PMM`, 288-byte EL1 TrapFrame and vector table, and GICv3 distributor/redistributor initialization with ARM Generic Timer periodic PPI interrupts (INTID 30).
- **Stage 5 (Low-RAM Optimization & AI-Ready Foundation): COMPLETE.** Dynamic PE32+ runtime section parser, boot buffer footprint optimization (trimmed BSS by 1.875 MiB), NEON/SIMD/FP activation via `CPACR_EL1.FPEN`, 2 MiB hugepage block mapping in `DynamicMapper`, and a spinlock-synchronized free-list kernel heap allocator (`LockedHeap`, `#[global_allocator]`, `alloc::boxed::Box`, `alloc::vec::Vec`) backed by `KERNEL_PMM` frames accessed via HHDM.

## Building and Testing

- **Build:** `./tools/build.sh`
- **Host Unit Tests:** `cargo test --lib --target aarch64-apple-darwin --target-dir build/host-tests`
- **Run Live QEMU:** `./tools/run-qemu.sh`
- **Stage 1 & 2 Regression (20 cold boots):** `./tools/test-stage2.sh`
- **Phase 2 Regression (20 cold boots + invariants):** `./tools/test-phase2.sh`
- **Stage 3C-1 Preflight Verification:** `./tools/test-stage3c1.sh`
