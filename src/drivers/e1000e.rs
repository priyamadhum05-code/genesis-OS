//! Project Genesis — Physical PCIe Gigabit NIC (Intel e1000e / 82574L) Driver
//!
//! Provides bare-metal Gigabit Ethernet initialization, descriptor ring management,
//! DMA packet transmission, and zero-copy packet reception over PCIe MMIO.

use crate::drivers::pci::PciDeviceInfo;
use crate::drivers::xhci::{clean_dcache_range, invalidate_dcache_range};
use crate::pmm::PhysicalPageAllocator;

pub const E1000E_MMIO_VA_BASE: u64 = 0xFFFF_FFFF_E500_0000;

pub const NUM_RX_DESC: usize = 32;
pub const NUM_TX_DESC: usize = 32;

// Register Offsets
pub const REG_CTRL: u32 = 0x00000;
pub const REG_STATUS: u32 = 0x00008;
pub const REG_EERD: u32 = 0x00014;
pub const REG_ICR: u32 = 0x000C0;
pub const REG_IMS: u32 = 0x000D0;
pub const REG_IMC: u32 = 0x000D8;
pub const REG_RCTL: u32 = 0x00100;
pub const REG_TCTL: u32 = 0x00400;
pub const REG_TIPG: u32 = 0x00410;
pub const REG_RDBAL: u32 = 0x02800;
pub const REG_RDBAH: u32 = 0x02804;
pub const REG_RDLEN: u32 = 0x02808;
pub const REG_RDH: u32 = 0x02810;
pub const REG_RDT: u32 = 0x02818;
pub const REG_TDBAL: u32 = 0x03800;
pub const REG_TDBAH: u32 = 0x03804;
pub const REG_TDLEN: u32 = 0x03808;
pub const REG_TDH: u32 = 0x03810;
pub const REG_TDT: u32 = 0x03818;
pub const REG_MTA_BASE: u32 = 0x05200;
pub const REG_RAL0: u32 = 0x05400;
pub const REG_RAH0: u32 = 0x05404;

// Control Register Bits
pub const CTRL_FD: u32 = 1 << 0;
pub const CTRL_ASDE: u32 = 1 << 5;
pub const CTRL_SLU: u32 = 1 << 6;
pub const CTRL_RST: u32 = 1 << 26;

// Status Register Bits
pub const STATUS_FD: u32 = 1 << 0;
pub const STATUS_LU: u32 = 1 << 1;

// Receive Control Bits
pub const RCTL_EN: u32 = 1 << 1;
pub const RCTL_SBP: u32 = 1 << 2;
pub const RCTL_UPE: u32 = 1 << 3;
pub const RCTL_MPE: u32 = 1 << 4;
pub const RCTL_LPE: u32 = 1 << 5;
pub const RCTL_BAM: u32 = 1 << 15;
pub const RCTL_BSIZE_2048: u32 = 0 << 16;
pub const RCTL_SECRC: u32 = 1 << 26;

// Transmit Control Bits
pub const TCTL_EN: u32 = 1 << 1;
pub const TCTL_PSP: u32 = 1 << 3;

// Transmit Descriptor Command & Status
pub const TXD_CMD_EOP: u8 = 1 << 0;
pub const TXD_CMD_IFCS: u8 = 1 << 1;
pub const TXD_CMD_RS: u8 = 1 << 3;
pub const TXD_STAT_DD: u8 = 1 << 0;

// Receive Descriptor Status
pub const RXD_STAT_DD: u8 = 1 << 0;
pub const RXD_STAT_EOP: u8 = 1 << 1;

#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct E1000RxDesc {
    pub buffer_addr: u64,
    pub length: u16,
    pub checksum: u16,
    pub status: u8,
    pub errors: u8,
    pub special: u16,
}

#[repr(C, align(16))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct E1000TxDesc {
    pub buffer_addr: u64,
    pub length: u16,
    pub cso: u8,
    pub cmd: u8,
    pub status: u8,
    pub css: u8,
    pub special: u16,
}

#[derive(Clone, Copy, Debug)]
pub struct E1000eDevice {
    pub mmio_va: u64,
    pub mac: [u8; 6],
    pub rx_desc_pa: u64,
    pub rx_desc_va: u64,
    pub tx_desc_pa: u64,
    pub tx_desc_va: u64,
    pub rx_buf_pa: [u64; NUM_RX_DESC],
    pub rx_buf_va: [u64; NUM_RX_DESC],
    pub tx_buf_pa: [u64; NUM_TX_DESC],
    pub tx_buf_va: [u64; NUM_TX_DESC],
    pub rx_cur: usize,
    pub tx_cur: usize,
    pub packets_tx: u64,
    pub packets_rx: u64,
    pub is_initialized: bool,
}

impl E1000eDevice {
    pub const fn empty() -> Self {
        Self {
            mmio_va: 0,
            mac: [0; 6],
            rx_desc_pa: 0,
            rx_desc_va: 0,
            tx_desc_pa: 0,
            tx_desc_va: 0,
            rx_buf_pa: [0; NUM_RX_DESC],
            rx_buf_va: [0; NUM_RX_DESC],
            tx_buf_pa: [0; NUM_TX_DESC],
            tx_buf_va: [0; NUM_TX_DESC],
            rx_cur: 0,
            tx_cur: 0,
            packets_tx: 0,
            packets_rx: 0,
            is_initialized: false,
        }
    }

    #[inline]
    pub unsafe fn read_reg(&self, offset: u32) -> u32 {
        let ptr = (self.mmio_va + offset as u64) as *const u32;
        core::ptr::read_volatile(ptr)
    }

    #[inline]
    pub unsafe fn write_reg(&self, offset: u32, val: u32) {
        let ptr = (self.mmio_va + offset as u64) as *mut u32;
        core::ptr::write_volatile(ptr, val);
    }

    /// Checks if physical link is established.
    pub unsafe fn is_link_up(&self) -> bool {
        (self.read_reg(REG_STATUS) & STATUS_LU) != 0
    }

    /// Prints MAC address in formatted `xx:xx:xx:xx:xx:xx` form.
    pub unsafe fn print_mac(&self, uart: usize) {
        for i in 0..6 {
            let b = self.mac[i];
            let hi = (b >> 4) & 0xF;
            let lo = b & 0xF;
            let c_hi = if hi < 10 { b'0' + hi } else { b'a' + (hi - 10) };
            let c_lo = if lo < 10 { b'0' + lo } else { b'a' + (lo - 10) };
            crate::arch::serial_write(uart, &[c_hi, c_lo]);
            if i < 5 {
                crate::arch::serial_write(uart, b":");
            }
        }
    }

    /// Initialize controller, reset hardware, allocate rings and enable TX/RX.
    pub unsafe fn init<const WORDS: usize>(
        &mut self,
        _pci_dev: &PciDeviceInfo,
        pmm: &mut PhysicalPageAllocator<WORDS>,
        hhdm_base: u64,
        mmio_va: u64,
    ) -> Result<(), &'static str> {
        self.mmio_va = mmio_va;

        // 1. Mask/disable all interrupts immediately
        self.write_reg(REG_IMC, 0xFFFF_FFFF);
        let _ = self.read_reg(REG_ICR);

        // 2. Read MAC address from RAL0/RAH0 or EEPROM before or during reset
        let ral = self.read_reg(REG_RAL0);
        let rah = self.read_reg(REG_RAH0);
        let mut mac = [
            (ral & 0xFF) as u8,
            ((ral >> 8) & 0xFF) as u8,
            ((ral >> 16) & 0xFF) as u8,
            ((ral >> 24) & 0xFF) as u8,
            (rah & 0xFF) as u8,
            ((rah >> 8) & 0xFF) as u8,
        ];

        if (rah & (1 << 31)) == 0 || mac == [0; 6] {
            // Read from EEPROM via EERD
            for word in 0..3 {
                self.write_reg(REG_EERD, 1 | ((word as u32) << 8));
                let mut spins = 0;
                let mut data = 0u16;
                while spins < 100_000 {
                    let val = self.read_reg(REG_EERD);
                    if (val & (1 << 4)) != 0 || (val & (1 << 1)) != 0 {
                        data = (val >> 16) as u16;
                        break;
                    }
                    spins += 1;
                    core::hint::spin_loop();
                }
                mac[word * 2] = (data & 0xFF) as u8;
                mac[word * 2 + 1] = ((data >> 8) & 0xFF) as u8;
            }
        }

        // If MAC is still zero, use fallback QEMU default
        if mac == [0; 6] {
            mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        }
        self.mac = mac;

        // 3. Issue Software Device Reset (CTRL.RST)
        let ctrl = self.read_reg(REG_CTRL);
        self.write_reg(REG_CTRL, ctrl | CTRL_RST);
        let mut spins = 0;
        while spins < 100_000 {
            if (self.read_reg(REG_CTRL) & CTRL_RST) == 0 {
                break;
            }
            spins += 1;
            core::hint::spin_loop();
        }

        // 4. Clear interrupts again post-reset
        self.write_reg(REG_IMC, 0xFFFF_FFFF);
        let _ = self.read_reg(REG_ICR);

        // 5. Force Link Up and Auto-Speed Detection
        let ctrl_after = self.read_reg(REG_CTRL);
        self.write_reg(REG_CTRL, ctrl_after | CTRL_SLU | CTRL_ASDE | CTRL_FD);

        // 6. Reprogram MAC in RAL0/RAH0 with Address Valid bit (bit 31)
        let ral_w = (mac[0] as u32) | ((mac[1] as u32) << 8) | ((mac[2] as u32) << 16) | ((mac[3] as u32) << 24);
        let rah_w = (mac[4] as u32) | ((mac[5] as u32) << 8) | (1 << 31);
        self.write_reg(REG_RAL0, ral_w);
        self.write_reg(REG_RAH0, rah_w);

        // 7. Clear Multicast Table Array (MTA)
        for i in 0..128 {
            self.write_reg(REG_MTA_BASE + i * 4, 0);
        }

        // 8. Allocate RX Descriptor Ring (1 page = 4096 bytes >= 32 * 16 = 512 bytes)
        let rx_desc_pa = pmm.allocate_page().ok_or("Failed to allocate RX descriptor ring")?;
        let rx_desc_va = hhdm_base + rx_desc_pa;
        core::ptr::write_bytes(rx_desc_va as *mut u8, 0, 4096);
        self.rx_desc_pa = rx_desc_pa;
        self.rx_desc_va = rx_desc_va;

        // Allocate RX Buffers (32 buffers * 2048 bytes = 16 pages)
        let rx_buf_pages = pmm.allocate_contiguous_pages(16).ok_or("Failed to allocate RX buffers")?;
        for i in 0..NUM_RX_DESC {
            let b_pa = rx_buf_pages + (i as u64 * 2048);
            let b_va = hhdm_base + b_pa;
            self.rx_buf_pa[i] = b_pa;
            self.rx_buf_va[i] = b_va;

            let desc_ptr = (rx_desc_va as *mut E1000RxDesc).add(i);
            let desc = E1000RxDesc {
                buffer_addr: b_pa,
                length: 0,
                checksum: 0,
                status: 0,
                errors: 0,
                special: 0,
            };
            core::ptr::write_volatile(desc_ptr, desc);
            clean_dcache_range(b_va, 2048);
        }
        clean_dcache_range(rx_desc_va, NUM_RX_DESC * 16);

        // Program RX Ring Registers
        self.write_reg(REG_RDBAL, rx_desc_pa as u32);
        self.write_reg(REG_RDBAH, (rx_desc_pa >> 32) as u32);
        self.write_reg(REG_RDLEN, (NUM_RX_DESC * 16) as u32);
        self.write_reg(REG_RDH, 0);
        self.write_reg(REG_RDT, (NUM_RX_DESC - 1) as u32);
        self.rx_cur = 0;

        // Enable Receiver (RCTL)
        let rctl = RCTL_EN | RCTL_BAM | RCTL_UPE | RCTL_MPE | RCTL_SECRC | RCTL_BSIZE_2048;
        self.write_reg(REG_RCTL, rctl);

        // 9. Allocate TX Descriptor Ring (1 page = 4096 bytes >= 32 * 16 = 512 bytes)
        let tx_desc_pa = pmm.allocate_page().ok_or("Failed to allocate TX descriptor ring")?;
        let tx_desc_va = hhdm_base + tx_desc_pa;
        core::ptr::write_bytes(tx_desc_va as *mut u8, 0, 4096);
        self.tx_desc_pa = tx_desc_pa;
        self.tx_desc_va = tx_desc_va;

        // Allocate TX Buffers (32 buffers * 2048 bytes = 16 pages)
        let tx_buf_pages = pmm.allocate_contiguous_pages(16).ok_or("Failed to allocate TX buffers")?;
        for i in 0..NUM_TX_DESC {
            let b_pa = tx_buf_pages + (i as u64 * 2048);
            let b_va = hhdm_base + b_pa;
            self.tx_buf_pa[i] = b_pa;
            self.tx_buf_va[i] = b_va;

            let desc_ptr = (tx_desc_va as *mut E1000TxDesc).add(i);
            let desc = E1000TxDesc {
                buffer_addr: b_pa,
                length: 0,
                cso: 0,
                cmd: 0,
                status: TXD_STAT_DD,
                css: 0,
                special: 0,
            };
            core::ptr::write_volatile(desc_ptr, desc);
        }
        clean_dcache_range(tx_desc_va, NUM_TX_DESC * 16);

        // Program TX Ring Registers
        self.write_reg(REG_TDBAL, tx_desc_pa as u32);
        self.write_reg(REG_TDBAH, (tx_desc_pa >> 32) as u32);
        self.write_reg(REG_TDLEN, (NUM_TX_DESC * 16) as u32);
        self.write_reg(REG_TDH, 0);
        self.write_reg(REG_TDT, 0);
        self.tx_cur = 0;

        // Configure Inter-Packet Gap (TIPG)
        self.write_reg(REG_TIPG, 0x0060_200A);

        // Enable Transmitter (TCTL)
        let tctl = TCTL_EN | TCTL_PSP | (0x0F << 4) | (0x40 << 12);
        self.write_reg(REG_TCTL, tctl);

        self.is_initialized = true;
        Ok(())
    }

    /// Transmit a raw Ethernet frame via DMA.
    pub unsafe fn send_packet(&mut self, frame: &[u8]) -> Result<(), &'static str> {
        if !self.is_initialized {
            return Err("e1000e not initialized");
        }
        if frame.len() > 1518 {
            return Err("Packet exceeds MTU");
        }

        let idx = self.tx_cur;
        let desc_ptr = (self.tx_desc_va as *mut E1000TxDesc).add(idx);

        // Copy frame into DMA buffer
        let buf_va = self.tx_buf_va[idx];
        core::ptr::copy_nonoverlapping(frame.as_ptr(), buf_va as *mut u8, frame.len());
        clean_dcache_range(buf_va, frame.len());

        // Setup descriptor
        let desc = E1000TxDesc {
            buffer_addr: self.tx_buf_pa[idx],
            length: frame.len() as u16,
            cso: 0,
            cmd: TXD_CMD_EOP | TXD_CMD_IFCS | TXD_CMD_RS,
            status: 0,
            css: 0,
            special: 0,
        };
        core::ptr::write_volatile(desc_ptr, desc);
        clean_dcache_range(desc_ptr as u64, 16);

        // Advance tail doorbell
        self.tx_cur = (self.tx_cur + 1) % NUM_TX_DESC;
        self.write_reg(REG_TDT, self.tx_cur as u32);
        self.packets_tx += 1;

        // Poll for transmission completion
        let mut spins = 0;
        while spins < 100_000 {
            invalidate_dcache_range(desc_ptr as u64, 16);
            let st = core::ptr::read_volatile(core::ptr::addr_of!((*desc_ptr).status));
            if (st & TXD_STAT_DD) != 0 {
                return Ok(());
            }
            spins += 1;
            core::hint::spin_loop();
        }

        Ok(())
    }

    /// Poll for an incoming packet. Returns `Some(packet_len)` if received.
    pub unsafe fn recv_packet(&mut self, out: &mut [u8]) -> Option<usize> {
        if !self.is_initialized {
            return None;
        }

        let idx = self.rx_cur;
        let desc_ptr = (self.rx_desc_va as *mut E1000RxDesc).add(idx);
        invalidate_dcache_range(desc_ptr as u64, 16);

        let status = core::ptr::read_volatile(core::ptr::addr_of!((*desc_ptr).status));
        if (status & RXD_STAT_DD) != 0 {
            let length = core::ptr::read_volatile(core::ptr::addr_of!((*desc_ptr).length)) as usize;
            let buf_va = self.rx_buf_va[idx];
            invalidate_dcache_range(buf_va, length);

            let copy_len = length.min(out.len());
            core::ptr::copy_nonoverlapping(buf_va as *const u8, out.as_mut_ptr(), copy_len);

            // Clear status and clean cache
            core::ptr::write_volatile(core::ptr::addr_of_mut!((*desc_ptr).status), 0);
            clean_dcache_range(desc_ptr as u64, 16);

            // Advance RDT doorbell returning buffer to NIC
            self.write_reg(REG_RDT, idx as u32);
            self.rx_cur = (idx + 1) % NUM_RX_DESC;
            self.packets_rx += 1;

            Some(copy_len)
        } else {
            None
        }
    }
}

pub static mut GLOBAL_E1000E: Option<E1000eDevice> = None;

pub fn register_global_e1000e(dev: E1000eDevice) {
    unsafe {
        GLOBAL_E1000E = Some(dev);
    }
}

pub fn get_global_e1000e() -> Option<*mut E1000eDevice> {
    unsafe {
        let ptr = core::ptr::addr_of_mut!(GLOBAL_E1000E);
        (*ptr).as_mut().map(|d| d as *mut E1000eDevice)
    }
}

pub fn get_global_e1000e_ref() -> Option<&'static E1000eDevice> {
    unsafe {
        let ptr = core::ptr::addr_of!(GLOBAL_E1000E);
        (*ptr).as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn e1000e_descriptor_layout_and_sizes() {
        assert_eq!(core::mem::size_of::<E1000RxDesc>(), 16);
        assert_eq!(core::mem::align_of::<E1000RxDesc>(), 16);
        assert_eq!(core::mem::size_of::<E1000TxDesc>(), 16);
        assert_eq!(core::mem::align_of::<E1000TxDesc>(), 16);

        let rx_ring_bytes = NUM_RX_DESC * core::mem::size_of::<E1000RxDesc>();
        assert_eq!(rx_ring_bytes % 128, 0, "RX Ring size must be a multiple of 128 bytes");

        let tx_ring_bytes = NUM_TX_DESC * core::mem::size_of::<E1000TxDesc>();
        assert_eq!(tx_ring_bytes % 128, 0, "TX Ring size must be a multiple of 128 bytes");
    }

    #[test]
    fn e1000e_register_constants_and_offsets() {
        assert_eq!(REG_CTRL, 0x00000);
        assert_eq!(REG_STATUS, 0x00008);
        assert_eq!(REG_EERD, 0x00014);
        assert_eq!(REG_RCTL, 0x00100);
        assert_eq!(REG_TCTL, 0x00400);
        assert_eq!(REG_RDBAL, 0x02800);
        assert_eq!(REG_TDBAL, 0x03800);
        assert_eq!(REG_RAL0, 0x05400);
        assert_eq!(REG_RAH0, 0x05404);
    }

    #[test]
    fn e1000e_mac_rah0_av_fallback_and_decoding() {
        // Valid MAC with Address Valid (AV = bit 31 set)
        let ral = 0x12345678u32;
        let rah = 0x8000_9abcu32;
        let mac = [
            (ral & 0xFF) as u8,
            ((ral >> 8) & 0xFF) as u8,
            ((ral >> 16) & 0xFF) as u8,
            ((ral >> 24) & 0xFF) as u8,
            (rah & 0xFF) as u8,
            ((rah >> 8) & 0xFF) as u8,
        ];
        assert_eq!(mac, [0x78, 0x56, 0x34, 0x12, 0xbc, 0x9a]);
        let needs_fallback = (rah & (1 << 31)) == 0 || mac == [0; 6];
        assert!(!needs_fallback, "Valid RAH0 with AV=1 and non-zero MAC must not trigger fallback");

        // Invalid MAC because AV = 0 (uninitialized RAH0)
        let rah_no_av = 0x0000_9abcu32;
        let needs_fallback_no_av = (rah_no_av & (1 << 31)) == 0 || mac == [0; 6];
        assert!(needs_fallback_no_av, "RAH0 with AV=0 must trigger fallback even if bytes are non-zero");

        // Zero MAC with AV = 1
        let ral_zero = 0u32;
        let rah_zero = 0x8000_0000u32;
        let mac_zero = [
            (ral_zero & 0xFF) as u8,
            ((ral_zero >> 8) & 0xFF) as u8,
            ((ral_zero >> 16) & 0xFF) as u8,
            ((ral_zero >> 24) & 0xFF) as u8,
            (rah_zero & 0xFF) as u8,
            ((rah_zero >> 8) & 0xFF) as u8,
        ];
        let needs_fallback_zero = (rah_zero & (1 << 31)) == 0 || mac_zero == [0; 6];
        assert!(needs_fallback_zero, "Zero MAC must trigger fallback even if AV=1");
    }
}
