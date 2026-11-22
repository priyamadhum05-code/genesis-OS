//! Project Genesis — ARM PSCI (Power State Coordination Interface) & SMP Bring-up.
//!
//! Provides PSCI CPU_ON invocation via `hvc #0` for QEMU `virt`, secondary core
//! trampoline assembly, and SMP online tracking.

use core::sync::atomic::{AtomicU64, Ordering};

pub const PSCI_CPU_ON_64: u64 = 0xC400_0003;
pub const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;
pub const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;

pub static ONLINE_CPUS: AtomicU64 = AtomicU64::new(1); // CPU 0 is online initially

pub static SMP_WORK_READY: AtomicU64 = AtomicU64::new(0);
pub static SMP_WORK_DONE: AtomicU64 = AtomicU64::new(0);
pub static SMP_WORK_FN: AtomicU64 = AtomicU64::new(0);
pub static SMP_WORK_ARG: AtomicU64 = AtomicU64::new(0);

/// Dispatches work function `func(cpu_id, arg)` across all 4 SMP cores in parallel.
/// Cores 1..=3 run via the secondary worker loop, while CPU 0 executes inline.
pub fn dispatch_smp_work(func: extern "C" fn(u64, u64), arg: u64) {
    SMP_WORK_FN.store(func as usize as u64, Ordering::Release);
    SMP_WORK_ARG.store(arg, Ordering::Release);
    SMP_WORK_DONE.store(0, Ordering::Release);
    SMP_WORK_READY.fetch_add(1, Ordering::Release);

    #[cfg(target_os = "uefi")]
    unsafe {
        core::arch::asm!("sev", options(nomem, nostack));
    }

    // CPU 0 executes its partition
    func(0, arg);

    // Wait for secondary cores (cores 1, 2, 3) to complete
    while SMP_WORK_DONE.load(Ordering::Acquire) < 3 {
        core::hint::spin_loop();
    }
}

#[repr(C, align(64))]
#[derive(Clone, Copy, Debug)]
pub struct SecondaryBootArgs {
    pub cpu_id: u64,       // offset 0
    pub ttbr0: u64,        // offset 8
    pub ttbr1: u64,        // offset 16
    pub tcr: u64,          // offset 24
    pub mair: u64,         // offset 32
    pub vbar: u64,         // offset 40
    pub sctlr: u64,        // offset 48
    pub stack_va: u64,     // offset 56
    pub entry_va: u64,     // offset 64
}

#[inline]
pub unsafe fn psci_cpu_on(target_cpu: u64, entry_point_paddr: u64, context_id: u64) -> i64 {
    #[cfg(target_os = "uefi")]
    {
        let res: i64;
        core::arch::asm!(
            "hvc #0",
            inout("x0") PSCI_CPU_ON_64 => res,
            in("x1") target_cpu,
            in("x2") entry_point_paddr,
            in("x3") context_id,
            options(nomem, nostack)
        );
        res
    }
    #[cfg(not(target_os = "uefi"))]
    {
        let _ = (target_cpu, entry_point_paddr, context_id);
        0
    }
}

/// Shuts down the machine via PSCI SYSTEM_OFF (Function ID 0x8400_0008).
/// This call never returns.
pub unsafe fn psci_system_off() -> ! {
    #[cfg(target_os = "uefi")]
    {
        core::arch::asm!(
            "hvc #0",
            in("x0") PSCI_SYSTEM_OFF,
            options(noreturn)
        );
    }
    #[cfg(not(target_os = "uefi"))]
    {
        loop {
            core::hint::spin_loop();
        }
    }
}

/// Reboots the machine via PSCI SYSTEM_RESET (Function ID 0x8400_0009).
/// This call never returns.
pub unsafe fn psci_system_reset() -> ! {
    #[cfg(target_os = "uefi")]
    {
        core::arch::asm!(
            "hvc #0",
            in("x0") PSCI_SYSTEM_RESET,
            options(noreturn)
        );
    }
    #[cfg(not(target_os = "uefi"))]
    {
        loop {
            core::hint::spin_loop();
        }
    }
}

#[cfg(target_os = "uefi")]
core::arch::global_asm!(
    r#"
    .balign 16
    .global secondary_boot_stub
secondary_boot_stub:
    // Mask all DAIF
    msr     daifset, #0xf

    // Read SecondaryBootArgs fields from [x0] (physical address passed via PSCI context_id)
    ldr     x9,  [x0, #0]   // cpu_id
    ldr     x1,  [x0, #8]   // ttbr0
    ldr     x2,  [x0, #16]  // ttbr1
    ldr     x3,  [x0, #24]  // tcr
    ldr     x4,  [x0, #32]  // mair
    ldr     x5,  [x0, #40]  // vbar
    ldr     x6,  [x0, #48]  // sctlr
    ldr     x7,  [x0, #56]  // stack_va
    ldr     x8,  [x0, #64]  // entry_va

    // Invalidate local TLB and I-cache
    tlbi    vmalle1
    ic      iallu
    dsb     sy
    isb

    // Program translation and exception registers
    msr     mair_el1, x4
    msr     tcr_el1, x3
    msr     ttbr0_el1, x1
    msr     ttbr1_el1, x2
    msr     vbar_el1, x5
    isb

    // Enable NEON/FP (CPACR_EL1.FPEN = 0b11)
    mov     x10, #(3 << 20)
    msr     cpacr_el1, x10
    isb

    // Switch to higher-half stack
    mov     sp, x7

    // Enable MMU, caches, alignment checks (SCTLR_EL1)
    msr     sctlr_el1, x6
    isb

    // Jump directly to higher-half secondary_main(cpu_id)
    mov     x0, x9
    br      x8
"#
);

#[cfg(target_os = "uefi")]
extern "C" {
    pub fn secondary_boot_stub();
}

#[cfg(not(target_os = "uefi"))]
pub unsafe extern "C" fn secondary_boot_stub() {}

#[no_mangle]
pub extern "C" fn secondary_main(cpu_id: u64) -> ! {
    // 1. Disable TTBR0 (EPD0 = 1) and invalidate local TLB
    #[cfg(target_os = "uefi")]
    unsafe {
        let mut tcr: u64;
        core::arch::asm!("mrs {}, tcr_el1", out(reg) tcr, options(nomem, nostack));
        tcr |= 1 << 7; // EPD0 = 1
        core::arch::asm!(
            "msr tcr_el1, {}",
            "isb",
            "tlbi vmalle1is",
            "dsb ish",
            "isb",
            in(reg) tcr,
            options(nomem, nostack)
        );
    }

    // 2. Wake this core's GICR_WAKER
    const GICR_VA: u64 = 0xFFFF_FFFF_E010_0000;
    let r_base = (GICR_VA + cpu_id * 0x20000) as usize;
    unsafe {
        let waker = core::ptr::read_volatile((r_base + crate::arch::gic::GICR_WAKER) as *const u32);
        core::ptr::write_volatile((r_base + crate::arch::gic::GICR_WAKER) as *mut u32, waker & !(1 << 1));
        while (core::ptr::read_volatile((r_base + crate::arch::gic::GICR_WAKER) as *const u32) & (1 << 2)) != 0 {
            core::hint::spin_loop();
        }

        // 3. Init CPU interface ICC_*_EL1
        #[cfg(target_os = "uefi")]
        {
            let mut sre: u64;
            core::arch::asm!("mrs {}, ICC_SRE_EL1", out(reg) sre, options(nomem, nostack));
            sre |= 1;
            core::arch::asm!("msr ICC_SRE_EL1, {}", in(reg) sre, options(nomem, nostack));
            core::arch::asm!("isb", options(nomem, nostack));

            core::arch::asm!("msr ICC_PMR_EL1, {}", in(reg) 0xFFu64, options(nomem, nostack));
            core::arch::asm!("msr ICC_IGRPEN1_EL1, {}", in(reg) 1u64, options(nomem, nostack));
            core::arch::asm!("isb", options(nomem, nostack));
        }
    }

    // 4. Atomically increment ONLINE_CPUS
    ONLINE_CPUS.fetch_add(1, Ordering::SeqCst);

    // 5. Wait for SMP work in WFE loop
    let mut last_gen = 0u64;
    loop {
        let cur_gen = SMP_WORK_READY.load(Ordering::Acquire);
        if cur_gen != last_gen {
            last_gen = cur_gen;
            let fn_ptr = SMP_WORK_FN.load(Ordering::Acquire);
            if fn_ptr != 0 {
                let func: extern "C" fn(u64, u64) = unsafe { core::mem::transmute(fn_ptr) };
                let arg = SMP_WORK_ARG.load(Ordering::Acquire);
                func(cpu_id, arg);
            }
            SMP_WORK_DONE.fetch_add(1, Ordering::Release);
        }
        #[cfg(target_os = "uefi")]
        unsafe {
            core::arch::asm!("wfe", options(nomem, nostack));
        }
        #[cfg(not(target_os = "uefi"))]
        core::hint::spin_loop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secondary_boot_args_layout_and_offsets() {
        assert_eq!(core::mem::size_of::<SecondaryBootArgs>(), 128); // 9 * 8 = 72 padded to align 64
        assert_eq!(core::mem::align_of::<SecondaryBootArgs>(), 64);

        let dummy = SecondaryBootArgs {
            cpu_id: 1,
            ttbr0: 2,
            ttbr1: 3,
            tcr: 4,
            mair: 5,
            vbar: 6,
            sctlr: 7,
            stack_va: 8,
            entry_va: 9,
        };

        let base = &dummy as *const _ as usize;
        assert_eq!(&dummy.cpu_id as *const _ as usize - base, 0);
        assert_eq!(&dummy.ttbr0 as *const _ as usize - base, 8);
        assert_eq!(&dummy.ttbr1 as *const _ as usize - base, 16);
        assert_eq!(&dummy.tcr as *const _ as usize - base, 24);
        assert_eq!(&dummy.mair as *const _ as usize - base, 32);
        assert_eq!(&dummy.vbar as *const _ as usize - base, 40);
        assert_eq!(&dummy.sctlr as *const _ as usize - base, 48);
        assert_eq!(&dummy.stack_va as *const _ as usize - base, 56);
        assert_eq!(&dummy.entry_va as *const _ as usize - base, 64);
    }
}
