//! Stage 3C-2 Translation Regime Transition & Transition Vector Table.
//!
//! Implements the stackless MMU transition sequence, 2 KiB-aligned transition vector table,
//! separate diagnostic handlers, firmware identity pre-check loop, and post-transition
//! verification routines.
//!
//! NOTE: Under the Stage 3C-2 authorization directive, the live MMU transition
//! is NOT executed during this milestone. Only static gates and offline tests are performed.

#![allow(dead_code)]

pub const TRANSITION_CLEAN_START: u64 = 0x4800_0000;
pub const TRANSITION_CLEAN_END: u64 = 0x4827_2000;
pub const TRANSITION_CLEAN_PAGES: usize = 626;
pub const GENESIS_TCR_VALUE: u64 = 0x0000_0002_B510_3510;
pub const GENESIS_MAIR_VALUE: u64 = 0x0000_0000_FFBB_4400;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransitionLayout {
    pub v_start: u64,
    pub v_end: u64,
    pub h_start: u64,
    pub h_end: u64,
    pub t_start: u64,
    pub t_end: u64,
}

impl TransitionLayout {
    /// Validates vector table geometry, non-emptiness, pairwise disjointness,
    /// and containment within the approved `.text` region.
    pub fn verify(&self) -> Result<(), &'static str> {
        self.verify_with_bounds(0x4800_1000, 0x4802_0000)
    }

    pub fn verify_with_bounds(&self, text_start: u64, text_end: u64) -> Result<(), &'static str> {
        // 1. Vector table alignment: bits [10:0] must be zero
        if self.v_start % 2048 != 0 {
            return Err("Vector table is not 2 KiB aligned");
        }

        // 2. Vector table size: exactly 2,048 bytes (16 slots x 128 bytes)
        if self.v_end.checked_sub(self.v_start) != Some(2048) {
            return Err("Vector table size is not exactly 2,048 bytes");
        }

        // 3. Non-empty spans for handlers and transition code
        if self.h_end <= self.h_start {
            return Err("Handler span is empty or inverted");
        }
        if self.t_end <= self.t_start {
            return Err("Transition routine span is empty or inverted");
        }

        // 4. Strict pairwise disjointness
        // Vector table vs Handlers
        if !(self.v_end <= self.h_start || self.h_end <= self.v_start) {
            return Err("Vector table overlaps handler bodies");
        }
        // Vector table vs Transition routine
        if !(self.v_end <= self.t_start || self.t_end <= self.v_start) {
            return Err("Vector table overlaps transition routine");
        }
        // Handlers vs Transition routine
        if !(self.h_end <= self.t_start || self.t_end <= self.h_start) {
            return Err("Handler bodies overlap transition routine");
        }

        // 5. Containment within executable .text section
        if self.v_start < text_start || self.v_end > text_end {
            return Err("Vector table extends outside .text");
        }
        if self.h_start < text_start || self.h_end > text_end {
            return Err("Handler bodies extend outside .text");
        }
        if self.t_start < text_start || self.t_end > text_end {
            return Err("Transition routine extends outside .text");
        }

        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stage3c3Layout {
    pub v_start: u64,
    pub v_end: u64,
    pub h_start: u64,
    pub h_end: u64,
}

impl Stage3c3Layout {
    /// Validates higher-half vector table geometry, non-emptiness, pairwise disjointness,
    /// and containment within the approved `.text` region (`0x4800_1000..0x4800_F000`).
    pub fn verify(&self, text_start: u64, text_end: u64) -> Result<(), &'static str> {
        // 1. Vector table alignment: bits [10:0] must be zero
        if self.v_start % 2048 != 0 {
            return Err("Stage 3C-3 vector table is not 2 KiB aligned");
        }

        // 2. Vector table size: exactly 2,048 bytes (16 slots x 128 bytes)
        if self.v_end.checked_sub(self.v_start) != Some(2048) {
            return Err("Stage 3C-3 vector table size is not exactly 2,048 bytes");
        }

        // 3. Non-empty span for diagnostic handlers
        if self.h_end <= self.h_start {
            return Err("Stage 3C-3 handler span is empty or inverted");
        }

        // 4. Strict pairwise disjointness
        if !(self.v_end <= self.h_start || self.h_end <= self.v_start) {
            return Err("Stage 3C-3 vector table overlaps handler bodies");
        }

        // 5. Containment within executable .text section
        if self.v_start < text_start || self.v_end > text_end {
            return Err("Stage 3C-3 vector table extends outside .text");
        }
        if self.h_start < text_start || self.h_end > text_end {
            return Err("Stage 3C-3 handler bodies extend outside .text");
        }

        Ok(())
    }
}

/// Executes a volatile read of the first 32-bit word of every single page in
/// `image_start..(image_start + page_count * 4096)` under the active firmware MMU.
///
/// If any page is unmapped or generates an abort, the CPU traps immediately to
/// the active firmware/transition vector. Completing all reads proves 100% accessibility.
pub fn verify_firmware_identity_mapping(image_start: u64, page_count: usize) -> bool {
    for i in 0..page_count {
        let va = image_start + (i as u64) * 4096;
        let _ = unsafe { core::ptr::read_volatile(va as *const u32) };
    }
    true
}

/// Verifies that `KERNEL_VA_BASE` maps the valid PE header through TTBR1:
/// - Offset 0x00: DOS Magic 0x5A4D ('MZ')
/// - Offset 0x3C: PE Header Offset (e.g. 0x78)
/// - Offset `pe_offset`: PE Signature 0x0000_4550 ('PE\0\0')
pub fn verify_higher_half_pe_header(base_va: u64) -> Result<(), &'static str> {
    unsafe {
        let magic = core::ptr::read_volatile(base_va as *const u16);
        if magic != 0x5A4D {
            return Err("DOS magic mismatch at higher-half base");
        }

        let pe_ptr = core::ptr::read_volatile((base_va + 0x3C) as *const u32);
        if pe_ptr < 0x40 || pe_ptr > 0x1000 {
            return Err("PE header offset out of reasonable bounds");
        }

        let pe_sig = core::ptr::read_volatile((base_va + (pe_ptr as u64)) as *const u32);
        if pe_sig != 0x0000_4550 {
            return Err("PE signature mismatch at higher-half offset");
        }
    }
    Ok(())
}

/// Proves that stack memory is accessible and operational by executing a non-inlined
/// function that allocates a stack frame, performs arithmetic, and returns.
#[inline(never)]
pub fn verify_stack_usability(magic: u64) -> u64 {
    let mut buffer = [magic; 8];
    buffer[3] = buffer[3].wrapping_add(0xCAFE_BABE_DEAD_BEEF);
    buffer[7] = buffer[7].wrapping_sub(0x1234_5678_9ABC_DEF0);
    buffer[3] ^ buffer[7]
}

// ============================================================================
// Bare-Metal AArch64 Assembly Implementation (Target: aarch64-unknown-uefi)
// ============================================================================

#[cfg(target_os = "uefi")]
core::arch::global_asm!(
    r#"
    .text

    // ========================================================================
    // 1. Transition Routine: stage3c2_mmu_transition
    // ========================================================================
    .balign 16
    .global stage3c2_transition_start
    .global stage3c2_transition_end
    .global stage3c2_mmu_transition

stage3c2_transition_start:
stage3c2_mmu_transition:
    // Inputs:
    //   x0: TTBR0 Level 0 root physical address
    //   x1: TTBR1 Level 0 root physical address
    //   x2: TCR_EL1 configuration value
    //   x3: VBAR_EL1 transition vector base address
    //   x4: MAIR_EL1 configuration value (GENESIS_MAIR_VALUE)
    //   x5: Cache clean start address (0x4800_0000)
    //   x6: Cache clean end address (0x4825_F000)

    // Phase 0: Mask all asynchronous exceptions (D, A, I, F)
    msr     daifset, #0xf

    // Phase 1: Install Genesis transition vector table & synchronize
    msr     vbar_el1, x3
    isb

    // Phase 2: Cache clean to PoC across [x5, x6) while MMU is still ON
    mrs     x7, ctr_el0
    ubfx    x7, x7, #16, #4
    mov     x8, #4
    lsl     x7, x8, x7              // x7 = cache line size in bytes (e.g. 64 B)

    mov     x9, x5                  // x9 = clean pointer
1:
    dc      cvac, x9                // Clean data cache line by VA to PoC
    add     x9, x9, x7
    cmp     x9, x6
    b.lo    1b
    dsb     ish                     // Drain all store buffers to PoC

    // Phase 3: Disable MMU and D-Cache
    // CRITICAL: Pure register operations. Zero stack dereference, zero calls, zero literal pool loads.
    mrs     x8, sctlr_el1
    bic     x8, x8, #(1 << 0)       // M = 0 (Disable MMU)
    bic     x8, x8, #(1 << 2)       // C = 0 (Disable D-Cache)
    msr     sctlr_el1, x8
    isb                             // Context synchronization: MMU is now OFF

    // Phase 4: Configure Genesis translation registers while table walker is dormant
    msr     tcr_el1, x2             // TCR: T0SZ=16, T1SZ=16, 4K granules, 40-bit IPS
    msr     mair_el1, x4            // MAIR: Attr0=0x00 (Device), Attr3=0xFF (Normal WB-WA)
    msr     ttbr0_el1, x0           // TTBR0: Level 0 identity root
    msr     ttbr1_el1, x1           // TTBR1: Level 0 higher-half root
    isb

    // Phase 5: Purge stale firmware TLB entries and Instruction Cache
    tlbi    vmalle1is               // Invalidate all Stage 1 EL1 translations
    dsb     ish
    isb

    ic      ialluis                 // Invalidate all Instruction Caches to PoU
    dsb     ish
    isb

    // Phase 6: Re-enable Stage 1 MMU, D-Cache, and I-Cache under Genesis tables
    mrs     x8, sctlr_el1
    orr     x8, x8, #(1 << 0)       // M = 1 (Enable MMU)
    orr     x8, x8, #(1 << 2)       // C = 1 (Enable D-Cache)
    orr     x8, x8, #(1 << 12)      // I = 1 (Enable I-Cache)
    msr     sctlr_el1, x8
    isb                             // Context synchronization: MMU is now ON

    ret
stage3c2_transition_end:

    // ========================================================================
    // 2. Emergency Transition Vector Table: stage3c2_transition_vectors
    // Strictly 2 KiB aligned, exactly 2,048 bytes (16 slots x 128 bytes)
    // Contains ONLY branch instructions and NOP padding; zero handler bodies
    // ========================================================================
    .balign 2048
    .global stage3c2_vectors_start
    .global stage3c2_vectors_end

stage3c2_vectors_start:
    // Slot 0 (0x000): Current EL with SP0 Sync
    b       stage3c2_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 1 (0x080): Current EL with SP0 IRQ
    b       stage3c2_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 2 (0x100): Current EL with SP0 FIQ
    b       stage3c2_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 3 (0x180): Current EL with SP0 SError
    b       stage3c2_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 4 (0x200): Current EL with SPx Sync (Instruction/Data Abort)
    b       stage3c2_el1_sync_handler
    .fill   31, 4, 0xd503201f

    // Slot 5 (0x280): Current EL with SPx IRQ
    b       stage3c2_el1_irq_handler
    .fill   31, 4, 0xd503201f

    // Slot 6 (0x300): Current EL with SPx FIQ
    b       stage3c2_el1_fiq_handler
    .fill   31, 4, 0xd503201f

    // Slot 7 (0x380): Current EL with SPx SError
    b       stage3c2_el1_serror_handler
    .fill   31, 4, 0xd503201f

    // Slot 8 (0x400): Lower EL AArch64 Sync
    b       stage3c2_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 9 (0x480): Lower EL AArch64 IRQ
    b       stage3c2_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 10 (0x500): Lower EL AArch64 FIQ
    b       stage3c2_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 11 (0x580): Lower EL AArch64 SError
    b       stage3c2_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 12 (0x600): Lower EL AArch32 Sync
    b       stage3c2_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 13 (0x680): Lower EL AArch32 IRQ
    b       stage3c2_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 14 (0x700): Lower EL AArch32 FIQ
    b       stage3c2_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 15 (0x780): Lower EL AArch32 SError
    b       stage3c2_unhandled_handler
    .fill   31, 4, 0xd503201f
stage3c2_vectors_end:

    // ========================================================================
    // 3. Diagnostic Handler Bodies
    // Placed completely outside the vector table range
    // ========================================================================
    .balign 16
    .global stage3c2_handlers_start
    .global stage3c2_handlers_end

stage3c2_handlers_start:

stage3c2_el1_sync_handler:
    mrs     x0, esr_el1
    mrs     x2, elr_el1

    // Extract EC (bits [31:26]) and FnV (bit [10])
    ubfx    x10, x0, #26, #6
    ubfx    x11, x0, #10, #1

    // Check for Aborts: 0x20 (Inst lower), 0x21 (Inst same), 0x24 (Data lower), 0x25 (Data same)
    cmp     x10, #0x20
    b.eq    1f
    cmp     x10, #0x21
    b.eq    1f
    cmp     x10, #0x24
    b.eq    1f
    cmp     x10, #0x25
    b.eq    1f
    cmp     x10, #0x22             // PC Alignment Fault
    b.eq    2f
    b       3f                      // Non-abort: FAR is invalid

1:  // Abort: check FnV
    cbnz    x11, 3f                 // If FnV == 1, FAR is invalid

2:  // FAR is valid
    mrs     x1, far_el1
    b       4f

3:  // FAR is invalid
    mov     x1, #0

4:  // Terminal UART output and halt
    mov     x4, #0x09000000         // PL011 UART MMIO base
    // Busy-wait poll UARTFR.TXFF (bit 5 at offset 0x18)
5:
    ldr     w5, [x4, #0x18]
    tbnz    w5, #5, 5b
    mov     w6, #'!'
    strb    w6, [x4, #0x00]
7:
    wfi
    b       7b

stage3c2_el1_irq_handler:
stage3c2_el1_fiq_handler:
stage3c2_el1_serror_handler:
stage3c2_unhandled_handler:
    mov     x4, #0x09000000
6:
    ldr     w5, [x4, #0x18]
    tbnz    w5, #5, 6b
    mov     w6, #'?'
    strb    w6, [x4, #0x00]
8:
    wfi
    b       8b

stage3c2_handlers_end:

    // ========================================================================
    // 4. Higher-Half Emergency Vector Table: stage3c3_vectors
    // Strictly 2 KiB aligned, exactly 2,048 bytes (16 slots x 128 bytes)
    // Contains ONLY branch instructions to stage3c3 handlers and NOP padding
    // ========================================================================
    .balign 2048
    .global stage3c3_vectors_start
    .global stage3c3_vectors_end

stage3c3_vectors_start:
    // Slot 0 (0x000): Current EL with SP0 Sync
    b       stage3c3_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 1 (0x080): Current EL with SP0 IRQ
    b       stage3c3_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 2 (0x100): Current EL with SP0 FIQ
    b       stage3c3_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 3 (0x180): Current EL with SP0 SError
    b       stage3c3_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 4 (0x200): Current EL with SPx Sync (Instruction/Data Abort)
    b       stage3c3_el1_sync_handler
    .fill   31, 4, 0xd503201f

    // Slot 5 (0x280): Current EL with SPx IRQ
    b       stage3c3_el1_irq_handler
    .fill   31, 4, 0xd503201f

    // Slot 6 (0x300): Current EL with SPx FIQ
    b       stage3c3_el1_fiq_handler
    .fill   31, 4, 0xd503201f

    // Slot 7 (0x380): Current EL with SPx SError
    b       stage3c3_el1_serror_handler
    .fill   31, 4, 0xd503201f

    // Slot 8 (0x400): Lower EL AArch64 Sync
    b       stage3c3_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 9 (0x480): Lower EL AArch64 IRQ
    b       stage3c3_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 10 (0x500): Lower EL AArch64 FIQ
    b       stage3c3_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 11 (0x580): Lower EL AArch64 SError
    b       stage3c3_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 12 (0x600): Lower EL AArch32 Sync
    b       stage3c3_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 13 (0x680): Lower EL AArch32 IRQ
    b       stage3c3_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 14 (0x700): Lower EL AArch32 FIQ
    b       stage3c3_unhandled_handler
    .fill   31, 4, 0xd503201f

    // Slot 15 (0x780): Lower EL AArch32 SError
    b       stage3c3_unhandled_handler
    .fill   31, 4, 0xd503201f
stage3c3_vectors_end:

    // ========================================================================
    // 5. Higher-Half Diagnostic Handler Bodies
    // Uses MMIO_VA_BASE (0xFFFF_FFFF_F000_0000) for PL011 UART output
    // Placed completely outside the vector table range
    // ========================================================================
    .balign 16
    .global stage3c3_handlers_start
    .global stage3c3_handlers_end

stage3c3_handlers_start:

stage3c3_el1_sync_handler:
    mrs     x0, esr_el1
    mrs     x2, elr_el1

    // Extract EC (bits [31:26]) and FnV (bit [10])
    ubfx    x10, x0, #26, #6
    ubfx    x11, x0, #10, #1

    // Check for Aborts: 0x20 (Inst lower), 0x21 (Inst same), 0x24 (Data lower), 0x25 (Data same)
    cmp     x10, #0x20
    b.eq    11f
    cmp     x10, #0x21
    b.eq    11f
    cmp     x10, #0x24
    b.eq    11f
    cmp     x10, #0x25
    b.eq    11f
    cmp     x10, #0x22             // PC Alignment Fault
    b.eq    12f
    b       13f                     // Non-abort: FAR is invalid

11: // Abort: check FnV
    cbnz    x11, 13f                // If FnV == 1, FAR is invalid

12: // FAR is valid
    mrs     x1, far_el1
    b       14f

13: // FAR is invalid
    mov     x1, #0

14: // Terminal UART output via higher-half MMIO_VA_BASE (0xFFFF_FFFF_F000_0000)
    movz    x4, #0xf000, lsl #16
    movk    x4, #0xffff, lsl #32
    movk    x4, #0xffff, lsl #48
15:
    ldr     w5, [x4, #0x18]
    tbnz    w5, #5, 15b
    mov     w6, #'!'
    strb    w6, [x4, #0x00]
17:
    wfi
    b       17b

stage3c3_el1_irq_handler:
stage3c3_el1_fiq_handler:
stage3c3_el1_serror_handler:
stage3c3_unhandled_handler:
    movz    x4, #0xf000, lsl #16
    movk    x4, #0xffff, lsl #32
    movk    x4, #0xffff, lsl #48
16:
    ldr     w5, [x4, #0x18]
    tbnz    w5, #5, 16b
    mov     w6, #'?'
    strb    w6, [x4, #0x00]
18:
    wfi
    b       18b

stage3c3_handlers_end:

    // ========================================================================
    // 6. Stage 3C-3 Trampoline: stage3c3_jump_higher_half
    // Masks DAIF, installs higher-half VBAR_EL1, switches SP to higher-half stack,
    // passes uart_va in x0, and branches to higher_half_entry in TTBR1.
    // ========================================================================
    .balign 16
    .global stage3c3_jump_higher_half

stage3c3_jump_higher_half:
    // Inputs:
    //   x0: entry_va (virtual address of higher_half_entry in TTBR1 space)
    //   x1: stack_va (virtual address of higher-half KERNEL_STACK top, 16-byte aligned)
    //   x2: vbar_va  (virtual address of stage3c3_vectors_start in TTBR1 space)
    //   x3: uart_va  (virtual address of UART MMIO: MMIO_VA_BASE)
    //   x4: handoff_va (virtual address of HANDOFF in TTBR1 space)

    // 1. Mask DAIF
    msr     daifset, #0xf

    // 2. Install higher-half VBAR_EL1 & synchronize
    msr     vbar_el1, x2
    isb

    // 3. Set SP to higher-half stack
    mov     sp, x1

    // 4. Branch to higher_half_entry with uart_va in x0, handoff_va in x1
    mov     x16, x0
    mov     x0, x3
    mov     x1, x4
    br      x16
    "#
);

#[cfg(target_os = "uefi")]
extern "C" {
    pub static stage3c2_transition_start: [u8; 0];
    pub static stage3c2_transition_end: [u8; 0];
    pub static stage3c2_vectors_start: [u8; 0];
    pub static stage3c2_vectors_end: [u8; 0];
    pub static stage3c2_handlers_start: [u8; 0];
    pub static stage3c2_handlers_end: [u8; 0];

    pub fn stage3c2_mmu_transition(
        ttbr0: u64,
        ttbr1: u64,
        tcr: u64,
        vbar: u64,
        mair: u64,
        clean_start: u64,
        clean_end: u64,
    );

    pub static stage3c3_vectors_start: [u8; 0];
    pub static stage3c3_vectors_end: [u8; 0];
    pub static stage3c3_handlers_start: [u8; 0];
    pub static stage3c3_handlers_end: [u8; 0];

    pub fn stage3c3_jump_higher_half(
        entry_va: u64,
        stack_va: u64,
        vbar_va: u64,
        uart_va: u64,
        handoff_va: u64,
    ) -> !;
}

#[cfg(not(target_os = "uefi"))]
#[allow(non_upper_case_globals)]
pub static stage3c3_vectors_start: [u8; 0] = [];
#[cfg(not(target_os = "uefi"))]
#[allow(non_upper_case_globals)]
pub static stage3c3_vectors_end: [u8; 0] = [];
#[cfg(not(target_os = "uefi"))]
#[allow(non_upper_case_globals)]
pub static stage3c3_handlers_start: [u8; 0] = [];
#[cfg(not(target_os = "uefi"))]
#[allow(non_upper_case_globals)]
pub static stage3c3_handlers_end: [u8; 0] = [];

#[cfg(not(target_os = "uefi"))]
pub unsafe fn stage3c3_jump_higher_half(
    _entry_va: u64,
    _stack_va: u64,
    _vbar_va: u64,
    _uart_va: u64,
    _handoff_va: u64,
) -> ! {
    panic!("Host mock for stage3c3_jump_higher_half");
}

#[cfg(target_os = "uefi")]
pub fn get_stage3c3_layout() -> Stage3c3Layout {
    Stage3c3Layout {
        v_start: unsafe { stage3c3_vectors_start.as_ptr() as u64 },
        v_end: unsafe { stage3c3_vectors_end.as_ptr() as u64 },
        h_start: unsafe { stage3c3_handlers_start.as_ptr() as u64 },
        h_end: unsafe { stage3c3_handlers_end.as_ptr() as u64 },
    }
}

// Host mock for unit tests on macOS / host
#[cfg(not(target_os = "uefi"))]
pub fn get_stage3c3_layout() -> Stage3c3Layout {
    Stage3c3Layout {
        v_start: 0x4800_3800,
        v_end: 0x4800_4000,
        h_start: 0x4800_4000,
        h_end: 0x4800_4200,
    }
}

#[cfg(not(target_os = "uefi"))]
pub unsafe fn stage3c2_mmu_transition(
    _ttbr0: u64,
    _ttbr1: u64,
    _tcr: u64,
    _vbar: u64,
    _mair: u64,
    _clean_start: u64,
    _clean_end: u64,
) {
}

#[cfg(target_os = "uefi")]
pub fn get_transition_layout() -> TransitionLayout {
    TransitionLayout {
        v_start: unsafe { stage3c2_vectors_start.as_ptr() as u64 },
        v_end: unsafe { stage3c2_vectors_end.as_ptr() as u64 },
        h_start: unsafe { stage3c2_handlers_start.as_ptr() as u64 },
        h_end: unsafe { stage3c2_handlers_end.as_ptr() as u64 },
        t_start: unsafe { stage3c2_transition_start.as_ptr() as u64 },
        t_end: unsafe { stage3c2_transition_end.as_ptr() as u64 },
    }
}

// Host mock for unit tests on macOS / host
#[cfg(not(target_os = "uefi"))]
pub fn get_transition_layout() -> TransitionLayout {
    TransitionLayout {
        t_start: 0x4800_1E00,
        t_end: 0x4800_1F00,
        v_start: 0x4800_2800,
        v_end: 0x4800_3000,
        h_start: 0x4800_3000,
        h_end: 0x4800_3200,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transition_layout_verification() {
        let layout = get_transition_layout();
        assert!(layout.verify().is_ok());

        // Test misaligned vector table fails
        let misaligned = TransitionLayout {
            v_start: 0x4800_2001,
            ..layout
        };
        assert_eq!(misaligned.verify(), Err("Vector table is not 2 KiB aligned"));

        // Test invalid size fails
        let bad_size = TransitionLayout {
            v_end: layout.v_start + 1024,
            ..layout
        };
        assert_eq!(bad_size.verify(), Err("Vector table size is not exactly 2,048 bytes"));

        // Test overlap fails
        let overlap = TransitionLayout {
            h_start: layout.v_start + 1024,
            ..layout
        };
        assert_eq!(overlap.verify(), Err("Vector table overlaps handler bodies"));
    }

    #[test]
    fn stack_usability_verification() {
        let magic = 0x1122_3344_5566_7788u64;
        let res = verify_stack_usability(magic);
        let expected = (magic.wrapping_add(0xCAFE_BABE_DEAD_BEEF)) ^ (magic.wrapping_sub(0x1234_5678_9ABC_DEF0));
        assert_eq!(res, expected);
    }

    #[test]
    fn stage3c3_layout_verification() {
        let layout = get_stage3c3_layout();
        assert!(layout.verify(0x4800_1000, 0x4801_4000).is_ok());

        // Test misaligned vector table fails
        let misaligned = Stage3c3Layout {
            v_start: 0x4800_3801,
            ..layout
        };
        assert_eq!(misaligned.verify(0x4800_1000, 0x4801_4000), Err("Stage 3C-3 vector table is not 2 KiB aligned"));

        // Test invalid size fails
        let bad_size = Stage3c3Layout {
            v_end: layout.v_start + 1024,
            ..layout
        };
        assert_eq!(bad_size.verify(0x4800_1000, 0x4801_4000), Err("Stage 3C-3 vector table size is not exactly 2,048 bytes"));

        // Test overlap fails
        let overlap = Stage3c3Layout {
            h_start: layout.v_start + 1024,
            ..layout
        };
        assert_eq!(overlap.verify(0x4800_1000, 0x4801_4000), Err("Stage 3C-3 vector table overlaps handler bodies"));
    }

    #[test]
    fn higher_half_pe_header_validation_logic() {
        // Build mock PE header buffer
        let mut mock_header = [0u8; 512];
        mock_header[0] = b'M';
        mock_header[1] = b'Z';
        mock_header[0x3C] = 0x78;
        mock_header[0x3D] = 0x00;
        mock_header[0x3E] = 0x00;
        mock_header[0x3F] = 0x00;
        mock_header[0x78] = b'P';
        mock_header[0x79] = b'E';
        mock_header[0x7A] = 0x00;
        mock_header[0x7B] = 0x00;

        let base = mock_header.as_ptr() as u64;
        assert_eq!(verify_higher_half_pe_header(base), Ok(()));

        // Test corrupted magic fails
        unsafe {
            core::ptr::write_volatile(base as *mut u8, b'X');
        }
        assert_eq!(verify_higher_half_pe_header(base), Err("DOS magic mismatch at higher-half base"));
        unsafe {
            core::ptr::write_volatile(base as *mut u8, b'M');
        }

        // Test corrupted PE sig fails
        unsafe {
            core::ptr::write_volatile((base + 0x78) as *mut u8, b'X');
        }
        assert_eq!(verify_higher_half_pe_header(base), Err("PE signature mismatch at higher-half offset"));
    }
}
