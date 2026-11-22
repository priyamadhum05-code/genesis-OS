//! Project Genesis — Full EL1 Exception Handling & TrapFrame
//!
//! Implements a 2 KiB-aligned vector table (stage4_vectors),
//! full general-purpose and system register preservation via TrapFrame,
//! synchronous exception diagnostic dumps over UART, and IRQ dispatch/eret.

use core::sync::atomic::{AtomicU64, Ordering};

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TrapFrame {
    pub x: [u64; 31],    // x0..x30 (bytes 0..248)
    pub spsr_el1: u64,   // byte 248
    pub elr_el1: u64,    // byte 256
    pub esr_el1: u64,    // byte 264
    pub far_el1: u64,    // byte 272
    pub sp_el0: u64,     // byte 280
    pub ttbr0_root: u64, // byte 288
    pub pid: u64,        // byte 296
}

static STAGE4_TICKS: AtomicU64 = AtomicU64::new(0);
static STAGE4_TIMER_INTERVAL: AtomicU64 = AtomicU64::new(0);
static STAGE4_MAX_TICKS: AtomicU64 = AtomicU64::new(5);
static STAGE4_UART_VA: AtomicU64 = AtomicU64::new(0);

pub fn stage4_init_runtime(uart_va: u64) {
    STAGE4_UART_VA.store(uart_va, Ordering::Relaxed);
}

pub fn stage4_arm_timer_ticks(interval: u64, max_ticks: u64) {
    STAGE4_TIMER_INTERVAL.store(interval, Ordering::Relaxed);
    STAGE4_MAX_TICKS.store(max_ticks, Ordering::Relaxed);
    STAGE4_TICKS.store(0, Ordering::SeqCst);
}

pub fn stage4_arm_timer_interval(interval: u64) {
    stage4_arm_timer_ticks(interval, 5);
}

pub fn stage4_get_ticks() -> u64 {
    STAGE4_TICKS.load(Ordering::SeqCst)
}

pub fn stage4_vectors_va() -> u64 {
    #[cfg(target_os = "uefi")]
    {
        let ptr = unsafe { stage4_vectors_start.as_ptr() as u64 };
        if ptr >= 0x8000_0000_0000_0000 {
            ptr
        } else {
            ptr - 0x4800_0000 + 0xFFFF_FFFF_8000_0000
        }
    }
    #[cfg(not(target_os = "uefi"))]
    0xFFFF_FFFF_8000_B000
}

#[no_mangle]
pub extern "C" fn stage4_exception_dispatch(frame: &mut TrapFrame, kind: u64) -> *mut TrapFrame {
    let uart = STAGE4_UART_VA.load(Ordering::Relaxed) as usize;

    if kind == 4 {
        // Current EL with SPx Sync (e.g. kernel_yield via SVC)
        let ec = (frame.esr_el1 >> 26) & 0x3F;
        if ec == 0x15 && frame.x[8] == 124 {
            frame.x[0] = 0;
            return crate::sched::scheduler_tick(frame as *mut TrapFrame);
        }
    }

    if kind == 5 || kind == 9 {
        // Current EL with SPx IRQ (5) or Lower EL AArch64 IRQ (9)
        let iar = unsafe { crate::arch::gic::acknowledge_irq() };
        let intid = (iar & 0xFFFFFF) as u32;

        if intid == 30 || intid == 27 {
            // Timer PPI (INTID 30: Non-secure physical timer, INTID 27: Virtual timer)
            let interval = STAGE4_TIMER_INTERVAL.load(Ordering::Relaxed);
            unsafe {
                crate::arch::timer::ArmTimer::arm_periodic(interval);
                crate::arch::gic::end_of_irq(iar);
            }

            let count = STAGE4_TICKS.fetch_add(1, Ordering::SeqCst) + 1;
            let max_ticks = STAGE4_MAX_TICKS.load(Ordering::Relaxed);
            if uart != 0 && max_ticks > 0 && count <= max_ticks {
                unsafe {
                    crate::arch::serial::serial_write(uart, b"GENESIS: timer tick ");
                    crate::arch::serial::serial_decimal(uart, count);
                    crate::arch::serial::serial_write(uart, b"\r\n");
                }
            }

            if max_ticks > 0 && count >= max_ticks {
                return crate::sched::scheduler_tick_to_main(frame as *mut TrapFrame);
            }

            return crate::sched::scheduler_tick(frame as *mut TrapFrame);
        } else if intid < 1020 {
            // Unhandled valid IRQ: acknowledge end-of-interrupt to prevent CPU hang
            unsafe {
                crate::arch::gic::end_of_irq(iar);
            }
        }
        // INTIDs 1020..=1023 are spurious/special: do NOT write to ICC_EOIR1_EL1
        return frame as *mut TrapFrame;
    }

    if kind == 8 {
        // Lower EL AArch64 Synchronous Exception (Syscalls / Faults)
        let ec = (frame.esr_el1 >> 26) & 0x3F;
        if ec == 0x15 {
            // SVC instruction in AArch64
            let sys_num = frame.x[8];
            match sys_num {
                1 | 64 => {
                    // SYS_WRITE (fd = arg0, buf = arg1 / arg0, count = arg2 / arg1)
                    // Standard Linux AArch64: x0 = fd, x1 = buf, x2 = count
                    // Minimal genesis binary: x0 = buf, x1 = count or x0 = fd, x1 = buf, x2 = count
                    let (buf, count) = if frame.x[0] <= 10 && frame.x[1] >= 4096 && frame.x[2] > 0 {
                        // Standard ABI: x0 = fd, x1 = buf, x2 = count
                        (frame.x[1], frame.x[2] as usize)
                    } else {
                        // Compact ABI: x0 = buf, x1 = count
                        (frame.x[0], frame.x[1] as usize)
                    };

                    if buf >= 4096
                        && buf < 0x0000_8000_0000_0000
                        && buf.checked_add(count as u64).map_or(false, |end| end <= 0x0000_8000_0000_0000)
                    {
                        if uart != 0 {
                            let slice = unsafe { core::slice::from_raw_parts(buf as *const u8, count) };
                            unsafe {
                                crate::arch::serial::serial_write(uart, slice);
                            }
                        }
                        frame.x[0] = count as u64;
                    } else {
                        frame.x[0] = (-14i64) as u64; // -EFAULT
                    }
                    return frame as *mut TrapFrame;
                }
                93 => {
                    // SYS_EXIT (status = arg0)
                    let exit_code = frame.x[0] as i32;
                    let is_multitask = crate::sched::is_current_task_multitask();
                    if is_multitask {
                        crate::sched::finish_current_task();
                        return crate::sched::scheduler_tick(frame as *mut TrapFrame);
                    } else {
                        frame.elr_el1 = crate::proc::stage9_user_return_va();
                        frame.spsr_el1 = 0x3C5; // EL1h with DAIF masked
                        frame.x[0] = exit_code as u64;
                        return frame as *mut TrapFrame;
                    }
                }
                124 => {
                    // SYS_YIELD
                    frame.x[0] = 0;
                    if crate::sched::is_current_task_multitask() {
                        return crate::sched::scheduler_tick(frame as *mut TrapFrame);
                    } else {
                        return frame as *mut TrapFrame;
                    }
                }
                172 => {
                    // SYS_GETPID
                    frame.x[0] = crate::sched::current_task_pid();
                    return frame as *mut TrapFrame;
                }
                200 => {
                    // SYS_IPC_SEND: x0 = target_pid, x1 = buf, x2 = count
                    let target_pid = frame.x[0];
                    let buf = frame.x[1];
                    let count = frame.x[2] as usize;
                    let sender_pid = crate::sched::current_task_pid();
                    if buf < 0x0000_8000_0000_0000
                        && buf.checked_add(count as u64).map_or(false, |end| end <= 0x0000_8000_0000_0000)
                    {
                        let slice = unsafe { core::slice::from_raw_parts(buf as *const u8, count) };
                        match crate::proc::ipc::IPC_MAILBOX.lock().send(sender_pid, target_pid, slice) {
                            Ok(()) => frame.x[0] = 0,
                            Err(e) => frame.x[0] = e as u64,
                        }
                    } else {
                        frame.x[0] = (-14i64) as u64; // -EFAULT
                    }
                    return frame as *mut TrapFrame;
                }
                201 => {
                    // SYS_IPC_RECV: x0 = sender_filter (0 = any), x1 = buf, x2 = max_len
                    let sender_filter = frame.x[0];
                    let buf = frame.x[1];
                    let max_len = frame.x[2] as usize;
                    let receiver_pid = crate::sched::current_task_pid();
                    if buf < 0x0000_8000_0000_0000
                        && buf.checked_add(max_len as u64).map_or(false, |end| end <= 0x0000_8000_0000_0000)
                    {
                        let slice = unsafe { core::slice::from_raw_parts_mut(buf as *mut u8, max_len) };
                        match crate::proc::ipc::IPC_MAILBOX.lock().recv(receiver_pid, sender_filter, slice) {
                            Ok((_sender, bytes_read)) => frame.x[0] = bytes_read as u64,
                            Err(e) => frame.x[0] = e as u64,
                        }
                    } else {
                        frame.x[0] = (-14i64) as u64; // -EFAULT
                    }
                    return frame as *mut TrapFrame;
                }
                202 => {
                    // SYS_UPTIME_MS: return system uptime in milliseconds
                    let t0: u64;
                    let cntfrq: u64;
                    #[cfg(target_os = "uefi")]
                    unsafe {
                        core::arch::asm!("mrs {}, cntvct_el0", out(reg) t0, options(nomem, nostack));
                        core::arch::asm!("mrs {}, cntfrq_el0", out(reg) cntfrq, options(nomem, nostack));
                    }
                    #[cfg(not(target_os = "uefi"))]
                    {
                        t0 = 0;
                        cntfrq = 1_000_000;
                    }
                    frame.x[0] = if cntfrq > 0 { (t0 * 1000) / cntfrq } else { 0 };
                    return frame as *mut TrapFrame;
                }
                _ => {
                    frame.x[0] = (-38i64) as u64; // -ENOSYS
                    return frame as *mut TrapFrame;
                }
            }
        } else {
            // Unexpected user-space exception (page fault, illegal instruction)
            if uart != 0 {
                unsafe {
                    crate::arch::serial::serial_write(uart, b"GENESIS: el0 user-space fault EC=");
                    crate::arch::serial::serial_hex(uart, ec);
                    crate::arch::serial::serial_write(uart, b" ELR=");
                    crate::arch::serial::serial_hex(uart, frame.elr_el1);
                    crate::arch::serial::serial_write(uart, b" FAR=");
                    crate::arch::serial::serial_hex(uart, frame.far_el1);
                    crate::arch::serial::serial_write(uart, b"\r\n");
                }
            }
            frame.elr_el1 = crate::proc::stage9_user_return_va();
            frame.spsr_el1 = 0x3C5;
            frame.x[0] = (-1i64) as u64;
            return frame as *mut TrapFrame;
        }
    }

    // Synchronous, SError, FIQ or unexpected exception: Diagnostic dump and halt
    if uart != 0 {
        unsafe {
            crate::arch::serial::serial_write(uart, b"STAGE4_UNEXPECTED_EXCEPTION:\r\n");
            crate::arch::serial::serial_write(uart, b"  KIND:     "); crate::arch::serial::serial_hex(uart, kind); crate::arch::serial::serial_write(uart, b"\r\n");
            crate::arch::serial::serial_write(uart, b"  ESR_EL1:  "); crate::arch::serial::serial_hex(uart, frame.esr_el1); crate::arch::serial_write(uart, b"\r\n");
            let ec = (frame.esr_el1 >> 26) & 0x3F;
            crate::arch::serial::serial_write(uart, b"  EC:       "); crate::arch::serial::serial_hex(uart, ec); crate::arch::serial::serial_write(uart, b"\r\n");
            crate::arch::serial::serial_write(uart, b"  ELR_EL1:  "); crate::arch::serial::serial_hex(uart, frame.elr_el1); crate::arch::serial_write(uart, b"\r\n");
            crate::arch::serial::serial_write(uart, b"  FAR_EL1:  "); crate::arch::serial::serial_hex(uart, frame.far_el1); crate::arch::serial_write(uart, b"\r\n");
            crate::arch::serial::serial_write(uart, b"  SPSR_EL1: "); crate::arch::serial::serial_hex(uart, frame.spsr_el1); crate::arch::serial_write(uart, b"\r\n");
        }
    }

    loop {
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack, preserves_flags));
        }
    }
}

#[cfg(target_os = "uefi")]
core::arch::global_asm!(
    r#"
    .text
    .balign 2048
    .global stage4_vectors_start
    .global stage4_vectors_end

stage4_vectors_start:
    // Slot 0 (0x000): Current EL with SP0 Sync
    sub     sp, sp, #304
    stp     x0, x1, [sp, #0]
    mov     x1, #0
    b       stage4_save_and_dispatch
    .fill   28, 4, 0xd503201f

    // Slot 1 (0x080): Current EL with SP0 IRQ
    sub     sp, sp, #304
    stp     x0, x1, [sp, #0]
    mov     x1, #1
    b       stage4_save_and_dispatch
    .fill   28, 4, 0xd503201f

    // Slot 2 (0x100): Current EL with SP0 FIQ
    sub     sp, sp, #304
    stp     x0, x1, [sp, #0]
    mov     x1, #2
    b       stage4_save_and_dispatch
    .fill   28, 4, 0xd503201f

    // Slot 3 (0x180): Current EL with SP0 SError
    sub     sp, sp, #304
    stp     x0, x1, [sp, #0]
    mov     x1, #3
    b       stage4_save_and_dispatch
    .fill   28, 4, 0xd503201f

    // Slot 4 (0x200): Current EL with SPx Sync (Instruction/Data Abort)
    sub     sp, sp, #304
    stp     x0, x1, [sp, #0]
    mov     x1, #4
    b       stage4_save_and_dispatch
    .fill   28, 4, 0xd503201f

    // Slot 5 (0x280): Current EL with SPx IRQ
    sub     sp, sp, #304
    stp     x0, x1, [sp, #0]
    mov     x1, #5
    b       stage4_save_and_dispatch
    .fill   28, 4, 0xd503201f

    // Slot 6 (0x300): Current EL with SPx FIQ
    sub     sp, sp, #304
    stp     x0, x1, [sp, #0]
    mov     x1, #6
    b       stage4_save_and_dispatch
    .fill   28, 4, 0xd503201f

    // Slot 7 (0x380): Current EL with SPx SError
    sub     sp, sp, #304
    stp     x0, x1, [sp, #0]
    mov     x1, #7
    b       stage4_save_and_dispatch
    .fill   28, 4, 0xd503201f

    // Slot 8 (0x400): Lower EL AArch64 Sync
    sub     sp, sp, #304
    stp     x0, x1, [sp, #0]
    mov     x1, #8
    b       stage4_save_and_dispatch
    .fill   28, 4, 0xd503201f

    // Slot 9 (0x480): Lower EL AArch64 IRQ
    sub     sp, sp, #304
    stp     x0, x1, [sp, #0]
    mov     x1, #9
    b       stage4_save_and_dispatch
    .fill   28, 4, 0xd503201f

    // Slot 10 (0x500): Lower EL AArch64 FIQ
    sub     sp, sp, #304
    stp     x0, x1, [sp, #0]
    mov     x1, #10
    b       stage4_save_and_dispatch
    .fill   28, 4, 0xd503201f

    // Slot 11 (0x580): Lower EL AArch64 SError
    sub     sp, sp, #304
    stp     x0, x1, [sp, #0]
    mov     x1, #11
    b       stage4_save_and_dispatch
    .fill   28, 4, 0xd503201f

    // Slot 12 (0x600): Lower EL AArch32 Sync
    sub     sp, sp, #304
    stp     x0, x1, [sp, #0]
    mov     x1, #12
    b       stage4_save_and_dispatch
    .fill   28, 4, 0xd503201f

    // Slot 13 (0x680): Lower EL AArch32 IRQ
    sub     sp, sp, #304
    stp     x0, x1, [sp, #0]
    mov     x1, #13
    b       stage4_save_and_dispatch
    .fill   28, 4, 0xd503201f

    // Slot 14 (0x700): Lower EL AArch32 FIQ
    sub     sp, sp, #304
    stp     x0, x1, [sp, #0]
    mov     x1, #14
    b       stage4_save_and_dispatch
    .fill   28, 4, 0xd503201f

    // Slot 15 (0x780): Lower EL AArch32 SError
    sub     sp, sp, #304
    stp     x0, x1, [sp, #0]
    mov     x1, #15
    b       stage4_save_and_dispatch
    .fill   28, 4, 0xd503201f
stage4_vectors_end:

    .balign 16
    .global stage4_save_and_dispatch
stage4_save_and_dispatch:
    stp     x2, x3, [sp, #16]
    stp     x4, x5, [sp, #32]
    stp     x6, x7, [sp, #48]
    stp     x8, x9, [sp, #64]
    stp     x10, x11, [sp, #80]
    stp     x12, x13, [sp, #96]
    stp     x14, x15, [sp, #112]
    stp     x16, x17, [sp, #128]
    stp     x18, x19, [sp, #144]
    stp     x20, x21, [sp, #160]
    stp     x22, x23, [sp, #176]
    stp     x24, x25, [sp, #192]
    stp     x26, x27, [sp, #208]
    stp     x28, x29, [sp, #224]
    str     x30, [sp, #240]

    mrs     x2, spsr_el1
    mrs     x3, elr_el1
    mrs     x4, esr_el1
    mrs     x5, far_el1
    stp     x2, x3, [sp, #248]
    stp     x4, x5, [sp, #264]

    mrs     x6, sp_el0
    str     x6, [sp, #280]

    mov     x0, sp          // arg 0 = &mut TrapFrame
    // x1 is already kind   // arg 1 = kind: u64
    bl      stage4_exception_dispatch
    mov     sp, x0          // switch to returned TrapFrame (for preemptive task switch)

    // Restore SP_EL0
    ldr     x6, [sp, #280]
    msr     sp_el0, x6

    // Restore system registers
    ldp     x2, x3, [sp, #248]
    msr     spsr_el1, x2
    msr     elr_el1, x3

    // Restore general registers
    ldp     x2, x3, [sp, #16]
    ldp     x4, x5, [sp, #32]
    ldp     x6, x7, [sp, #48]
    ldp     x8, x9, [sp, #64]
    ldp     x10, x11, [sp, #80]
    ldp     x12, x13, [sp, #96]
    ldp     x14, x15, [sp, #112]
    ldp     x16, x17, [sp, #128]
    ldp     x18, x19, [sp, #144]
    ldp     x20, x21, [sp, #160]
    ldp     x22, x23, [sp, #176]
    ldp     x24, x25, [sp, #192]
    ldp     x26, x27, [sp, #208]
    ldp     x28, x29, [sp, #224]
    ldr     x30, [sp, #240]

    ldp     x0, x1, [sp, #0]
    add     sp, sp, #304
    eret
    "#
);

#[cfg(target_os = "uefi")]
extern "C" {
    pub static stage4_vectors_start: [u8; 0];
    pub static stage4_vectors_end: [u8; 0];
}

#[cfg(not(target_os = "uefi"))]
#[allow(non_upper_case_globals)]
pub static stage4_vectors_start: [u8; 0] = [];
#[cfg(not(target_os = "uefi"))]
#[allow(non_upper_case_globals)]
pub static stage4_vectors_end: [u8; 0] = [];
