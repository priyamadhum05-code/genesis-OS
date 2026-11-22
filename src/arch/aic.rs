//! Project Genesis — Apple Interrupt Controller (AIC v1 / AIC v2) Driver
//!
//! Provides bare-metal support for Apple Silicon interrupt discovery, masking,
//! unmasking, interrupt acknowledgment, and end-of-interrupt signaling.

pub const AIC_INFO: u32 = 0x0004;
pub const AIC_WHOAMI: u32 = 0x2000;
pub const AIC_IACK: u32 = 0x2004;
pub const AIC_MASK_SET: u32 = 0x4000;
pub const AIC_MASK_CLR: u32 = 0x4080;
pub const AIC_SW_SET: u32 = 0x4100;
pub const AIC_SW_CLR: u32 = 0x4180;

pub const AIC_MAX_IRQ: u32 = 1024;

#[derive(Clone, Copy, Debug)]
pub struct AppleInterruptController {
    pub base_va: u64,
    pub is_v2: bool,
    pub max_irqs: u32,
    pub is_initialized: bool,
}

impl AppleInterruptController {
    pub const fn empty() -> Self {
        Self {
            base_va: 0,
            is_v2: false,
            max_irqs: 0,
            is_initialized: false,
        }
    }

    pub fn new(base_va: u64, is_v2: bool) -> Self {
        Self {
            base_va,
            is_v2,
            max_irqs: AIC_MAX_IRQ,
            is_initialized: true,
        }
    }

    #[inline]
    pub unsafe fn read_reg(&self, offset: u32) -> u32 {
        let ptr = (self.base_va + offset as u64) as *const u32;
        core::ptr::read_volatile(ptr)
    }

    #[inline]
    pub unsafe fn write_reg(&self, offset: u32, val: u32) {
        let ptr = (self.base_va + offset as u64) as *mut u32;
        core::ptr::write_volatile(ptr, val);
    }

    /// Read AIC information register (number of HW IRQs supported)
    pub unsafe fn init(&mut self) {
        let info = self.read_reg(AIC_INFO);
        let num_irqs = info & 0xFFFF;
        self.max_irqs = if num_irqs > 0 { num_irqs } else { AIC_MAX_IRQ };
        self.is_initialized = true;

        // Mask all interrupts by default
        let num_regs = (self.max_irqs + 31) / 32;
        for i in 0..num_regs {
            self.write_reg(AIC_MASK_SET + i * 4, 0xFFFF_FFFF);
        }
    }

    /// Read CPU identity register (WHOAMI)
    pub unsafe fn whoami(&self) -> u32 {
        self.read_reg(AIC_WHOAMI)
    }

    /// Acknowledge/claim active interrupt. Returns Some(irq) if an interrupt is pending.
    pub unsafe fn ack(&self) -> Option<u32> {
        let event = self.read_reg(AIC_IACK);
        let irq = event & 0xFFFF;
        let type_code = (event >> 16) & 0xFFFF;
        if irq > 0 && irq < self.max_irqs {
            Some(irq)
        } else if type_code != 0 && irq < self.max_irqs {
            Some(irq)
        } else {
            None
        }
    }

    /// Mask an interrupt line.
    pub unsafe fn mask(&self, irq: u32) {
        if irq < self.max_irqs {
            let reg_offset = AIC_MASK_SET + (irq / 32) * 4;
            let bit = 1u32 << (irq % 32);
            self.write_reg(reg_offset, bit);
        }
    }

    /// Unmask an interrupt line.
    pub unsafe fn unmask(&self, irq: u32) {
        if irq < self.max_irqs {
            let reg_offset = AIC_MASK_CLR + (irq / 32) * 4;
            let bit = 1u32 << (irq % 32);
            self.write_reg(reg_offset, bit);
        }
    }

    /// Signal End of Interrupt (EOI).
    pub unsafe fn eoi(&self, _irq: u32) {
        #[cfg(target_os = "uefi")]
        core::arch::asm!("dsb sy", options(nomem, nostack));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aic_register_constants_and_offsets() {
        assert_eq!(AIC_INFO, 0x0004);
        assert_eq!(AIC_WHOAMI, 0x2000);
        assert_eq!(AIC_IACK, 0x2004);
        assert_eq!(AIC_MASK_SET, 0x4000);
        assert_eq!(AIC_MASK_CLR, 0x4080);
        assert_eq!(AIC_SW_SET, 0x4100);
        assert_eq!(AIC_SW_CLR, 0x4180);
    }

    #[test]
    fn aic_mask_unmask_offset_calculation() {
        let irq = 35; // register 1, bit 3
        let reg_set = AIC_MASK_SET + (irq / 32) * 4;
        let reg_clr = AIC_MASK_CLR + (irq / 32) * 4;
        let bit = 1u32 << (irq % 32);
        assert_eq!(reg_set, 0x4004);
        assert_eq!(reg_clr, 0x4084);
        assert_eq!(bit, 1 << 3);
    }
}
