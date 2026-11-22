//! Process and EL0 User-Space Management for Project Genesis (Stage 9A).

pub mod elf;
pub mod ipc;

pub use elf::{
    destroy_user_page_table, destroy_user_process, invalidate_asid, load_elf64, setup_user_stack_args,
    Elf64Header, Elf64Phdr, ElfProcess, USER_STACK_BASE, USER_STACK_PAGES, USER_STACK_TOP,
};
pub use ipc::{IpcMailbox, IPC_MAILBOX, SYS_IPC_RECV, SYS_IPC_SEND};

pub type RunElfFn = unsafe fn(&[u8], &str, usize) -> Result<i32, &'static str>;
pub static mut GLOBAL_RUN_ELF_FN: Option<RunElfFn> = None;

pub fn register_global_run_elf(f: RunElfFn) {
    unsafe {
        *core::ptr::addr_of_mut!(GLOBAL_RUN_ELF_FN) = Some(f);
    }
}

pub fn get_global_run_elf() -> Option<RunElfFn> {
    unsafe { *core::ptr::addr_of!(GLOBAL_RUN_ELF_FN) }
}

#[cfg(target_os = "uefi")]
core::arch::global_asm!(
    r#"
    .text
    .balign 16
    .global stage9_enter_user
    .global stage9_user_return

stage9_enter_user:
    // x0 = entry_va, x1 = user_sp, x2 = ttbr0_root, x3 = argc, x4 = argv_va
    // Save EL1 callee-saved registers x19..x30 on SP_EL1 (96 bytes)
    stp     x19, x20, [sp, #-16]!
    stp     x21, x22, [sp, #-16]!
    stp     x23, x24, [sp, #-16]!
    stp     x25, x26, [sp, #-16]!
    stp     x27, x28, [sp, #-16]!
    stp     x29, x30, [sp, #-16]!

    // Program TTBR0_EL1 to process root PA
    msr     ttbr0_el1, x2
    isb

    // Clear TCR_EL1.EPD0 (bit 7) to enable translation walks in TTBR0_EL1
    mrs     x5, tcr_el1
    bic     x5, x5, #(1 << 7)
    msr     tcr_el1, x5
    isb
    tlbi    vmalle1is
    dsb     ish
    isb

    // Setup EL0 execution context
    msr     sp_el0, x1
    msr     elr_el1, x0
    msr     spsr_el1, xzr       // EL0t mode, interrupts unmasked

    // Pass argc in x0, argv in x1
    mov     x0, x3
    mov     x1, x4
    mov     x2, #0
    mov     x3, #0
    mov     x4, #0
    mov     x5, #0
    mov     x6, #0
    mov     x7, #0
    mov     x8, #0
    mov     x9, #0
    mov     x10, #0
    mov     x11, #0
    mov     x12, #0
    mov     x13, #0
    mov     x14, #0
    mov     x15, #0
    mov     x16, #0
    mov     x17, #0
    mov     x18, #0
    mov     x19, #0
    mov     x20, #0
    mov     x21, #0
    mov     x22, #0
    mov     x23, #0
    mov     x24, #0
    mov     x25, #0
    mov     x26, #0
    mov     x27, #0
    mov     x28, #0
    mov     x29, #0
    mov     x30, #0

    eret

stage9_user_return:
    // Re-enable TCR_EL1.EPD0 = 1 (disable TTBR0_EL1 walks while kernel executes)
    mrs     x1, tcr_el1
    orr     x1, x1, #(1 << 7)
    msr     tcr_el1, x1
    isb
    tlbi    vmalle1is
    dsb     ish
    isb

    // Restore EL1 callee-saved registers x19..x30 from SP_EL1 (96 bytes)
    ldp     x29, x30, [sp], #16
    ldp     x27, x28, [sp], #16
    ldp     x25, x26, [sp], #16
    ldp     x23, x24, [sp], #16
    ldp     x21, x22, [sp], #16
    ldp     x19, x20, [sp], #16

    // x0 holds the process exit status
    ret
    "#
);

#[cfg(target_os = "uefi")]
extern "C" {
    pub fn stage9_enter_user(entry_va: u64, user_sp: u64, ttbr0_root: u64, argc: u64, argv_va: u64) -> i32;
    pub static stage9_user_return: [u8; 0];
}

#[cfg(not(target_os = "uefi"))]
#[allow(non_upper_case_globals)]
pub static stage9_user_return: [u8; 0] = [];

pub fn stage9_user_return_va() -> u64 {
    #[cfg(target_os = "uefi")]
    {
        let ptr = unsafe { stage9_user_return.as_ptr() as u64 };
        if ptr >= 0x8000_0000_0000_0000 {
            ptr
        } else {
            ptr - 0x4800_0000 + 0xFFFF_FFFF_8000_0000
        }
    }
    #[cfg(not(target_os = "uefi"))]
    0xFFFF_FFFF_8000_0000
}

pub unsafe fn run_user_process(entry_va: u64, user_sp: u64, ttbr0_root: u64) -> i32 {
    run_user_process_with_args(entry_va, user_sp, ttbr0_root, 0, 0)
}

pub unsafe fn run_user_process_with_args(
    entry_va: u64,
    user_sp: u64,
    ttbr0_root: u64,
    argc: u64,
    argv_va: u64,
) -> i32 {
    #[cfg(target_os = "uefi")]
    {
        stage9_enter_user(entry_va, user_sp, ttbr0_root, argc, argv_va)
    }
    #[cfg(not(target_os = "uefi"))]
    {
        let _ = (entry_va, user_sp, ttbr0_root, argc, argv_va);
        0
    }
}
