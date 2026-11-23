//! ARM Generic Interrupt Controller v3 (GICv3) Driver.

#[cfg(target_os = "uefi")]
use core::arch::asm;

pub const GICD_CTLR: usize = 0x0000;
pub const GICD_TYPER: usize = 0x0004;
pub const GICD_IIDR: usize = 0x0008;

pub const GICR_WAKER: usize = 0x0014;
pub const GICR_SGI_OFFSET: usize = 0x10000;
pub const GICR_IGROUPR0: usize = 0x0080;
pub const GICR_ISENABLER0: usize = 0x0100;
pub const GICR_ICENABLER0: usize = 0x0180;
pub const GICR_IPRIORITYR: usize = 0x0400;
pub const GICR_ICFGR1: usize = 0x0C04;

#[inline]
unsafe fn mmio_read32(addr: usize) -> u32 {
    #[cfg(target_arch = "aarch64")]
    {
        let val: u32;
        core::arch::asm!(
            "ldr {val:w}, [{addr}]",
            val = out(reg) val,
            addr = in(reg) addr,
            options(nostack, preserves_flags),
        );
        val
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        core::ptr::read_volatile(addr as *const u32)
    }
}

#[inline]
unsafe fn mmio_write32(addr: usize, val: u32) {
    #[cfg(target_arch = "aarch64")]
    {
        core::arch::asm!(
            "str {val:w}, [{addr}]",
            val = in(reg) val,
            addr = in(reg) addr,
            options(nostack, preserves_flags),
        );
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        core::ptr::write_volatile(addr as *mut u32, val);
    }
}

pub struct Gicv3 {
    gicd_base: usize,
    gicr_base: usize,
}

impl Gicv3 {
    pub const fn new(gicd_base: usize, gicr_base: usize) -> Self {
        Self {
            gicd_base,
            gicr_base,
        }
    }

    #[inline(never)]
    pub unsafe fn init(&self) {
        // 1. Initialize Distributor (GICD)
        let ctlr = self.mmio_read_d(GICD_CTLR);
        // Bit 0: EnableGrp0, Bit 1: EnableGrp1NS, Bit 4: ARE_NS
        self.mmio_write_d(GICD_CTLR, ctlr | (1 << 4) | (1 << 1) | (1 << 0));

        // 2. Wake up Redistributor (GICR) for current CPU
        let waker = self.mmio_read_r(GICR_WAKER);
        self.mmio_write_r(GICR_WAKER, waker & !(1 << 1)); // Clear ProcessorSleep

        let mut spins = 0u64;
        while (self.mmio_read_r(GICR_WAKER) & (1 << 2)) != 0 {
            // Wait for ChildrenAsleep == 0
            spins += 1;
            if spins > 100_000 {
                break;
            }
            core::hint::spin_loop();
        }

        // 3. Initialize CPU Interface system registers (ICC_*_EL1)
        #[cfg(target_os = "uefi")]
        {
            // Enable System Register Access (ICC_SRE_EL1.SRE = 1)
            let mut sre: u64;
            asm!("mrs {0}, ICC_SRE_EL1", out(reg) sre, options(nomem, nostack, preserves_flags));
            sre |= 1;
            asm!("msr ICC_SRE_EL1, {0}", in(reg) sre, options(nomem, nostack, preserves_flags));
            asm!("isb", options(nomem, nostack, preserves_flags));

            // Unmask all interrupt priorities (ICC_PMR_EL1 = 0xFF)
            asm!("msr ICC_PMR_EL1, {0}", in(reg) 0xFFu64, options(nomem, nostack, preserves_flags));

            // Enable Group 1 interrupts (ICC_IGRPEN1_EL1 = 1)
            asm!("msr ICC_IGRPEN1_EL1, {0}", in(reg) 1u64, options(nomem, nostack, preserves_flags));
            asm!("isb", options(nomem, nostack, preserves_flags));
        }

        #[cfg(target_os = "uefi")]
        asm!("dsb sy", "isb", options(nomem, nostack, preserves_flags));
    }

    #[inline(never)]
    pub unsafe fn enable_ppi(&self, intid: u32) {
        let sgi_base = self.gicr_base + GICR_SGI_OFFSET;

        // Group 1 Non-Secure
        let grp_addr = sgi_base + GICR_IGROUPR0;
        let grp = mmio_read32(grp_addr);
        mmio_write32(grp_addr, grp | (1 << intid));
        let _ = mmio_read32(grp_addr); // Flush write

        // Priority 0x00 (highest)
        let prio_addr = sgi_base + GICR_IPRIORITYR + (intid as usize & !3);
        let prio = mmio_read32(prio_addr);
        let shift = (intid % 4) * 8;
        mmio_write32(prio_addr, prio & !(0xFF << shift));
        let _ = mmio_read32(prio_addr); // Flush write

        // Enable interrupt in GICR_ISENABLER0
        let isenable_addr = sgi_base + GICR_ISENABLER0;
        mmio_write32(isenable_addr, 1 << intid);
        let _ = mmio_read32(isenable_addr); // Flush write

        #[cfg(target_os = "uefi")]
        asm!("dsb sy", "isb", options(nostack));
    }

    #[inline]
    unsafe fn mmio_read_d(&self, offset: usize) -> u32 {
        mmio_read32(self.gicd_base + offset)
    }

    #[inline]
    unsafe fn mmio_write_d(&self, offset: usize, val: u32) {
        mmio_write32(self.gicd_base + offset, val)
    }

    #[inline]
    unsafe fn mmio_read_r(&self, offset: usize) -> u32 {
        mmio_read32(self.gicr_base + offset)
    }

    #[inline]
    unsafe fn mmio_write_r(&self, offset: usize, val: u32) {
        mmio_write32(self.gicr_base + offset, val)
    }
}

#[inline]
pub unsafe fn acknowledge_irq() -> u64 {
    #[cfg(target_os = "uefi")]
    {
        let iar: u64;
        asm!("mrs {0}, ICC_IAR1_EL1", out(reg) iar, options(nomem, nostack, preserves_flags));
        iar
    }
    #[cfg(not(target_os = "uefi"))]
    0
}

#[inline]
pub unsafe fn end_of_irq(iar: u64) {
    #[cfg(target_os = "uefi")]
    {
        asm!("msr ICC_EOIR1_EL1, {0}", in(reg) iar, options(nomem, nostack, preserves_flags));
        asm!("isb", options(nomem, nostack, preserves_flags));
    }
    #[cfg(not(target_os = "uefi"))]
    let _ = iar;
}
