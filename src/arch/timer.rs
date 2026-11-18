//! ARMv8 Generic Timer Driver.

#[cfg(target_os = "uefi")]
use core::arch::asm;

pub struct ArmTimer;

impl ArmTimer {
    #[inline]
    pub unsafe fn frequency() -> u64 {
        #[cfg(target_os = "uefi")]
        {
            let freq: u64;
            asm!("mrs {0}, CNTFRQ_EL0", out(reg) freq, options(nomem, nostack, preserves_flags));
            freq
        }
        #[cfg(not(target_os = "uefi"))]
        62_500_000
    }

    #[inline]
    pub unsafe fn arm_periodic(ticks: u64) {
        #[cfg(target_os = "uefi")]
        {
            asm!("msr CNTP_TVAL_EL0, {0}", in(reg) ticks, options(nomem, nostack, preserves_flags));
            // ENABLE = 1 (bit 0), IMASK = 0 (bit 1)
            asm!("msr CNTP_CTL_EL0, {0}", in(reg) 1u64, options(nomem, nostack, preserves_flags));
            asm!("isb", options(nomem, nostack, preserves_flags));
        }
        #[cfg(not(target_os = "uefi"))]
        let _ = ticks;
    }

    #[inline]
    pub unsafe fn disable() {
        #[cfg(target_os = "uefi")]
        {
            asm!("msr CNTP_CTL_EL0, {0}", in(reg) 0u64, options(nomem, nostack, preserves_flags));
            asm!("isb", options(nomem, nostack, preserves_flags));
        }
    }
}
