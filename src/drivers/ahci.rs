//! Project Genesis — Native PCIe AHCI 1.0 SATA Controller Driver (Stage 27).
//!
//! Provides HBA generic host control reset and initialization, port enumeration
//! via PI and PxSSTS.DET, Command List, Received FIS, and Command Table setup,
//! IDENTIFY DEVICE (0xEC), READ DMA EXT (0x25), and WRITE DMA EXT (0x35).
//! Implements the unified `BlockDevice` trait for FAT32 filesystem access.

use crate::drivers::pci::PciDeviceInfo;
use crate::drivers::xhci::{clean_dcache_range, invalidate_dcache_range};
use crate::fs::BlockDevice;
use crate::pmm::PhysicalPageAllocator;

pub const AHCI_MMIO_VA_BASE: u64 = 0xFFFF_FFFF_E700_0000;

// AHCI Generic Host Control (GHC) Register Offsets
pub const HOST_CAP: usize = 0x00;
pub const HOST_GHC: usize = 0x04;
pub const HOST_IS: usize = 0x08;
pub const HOST_PI: usize = 0x0C;
pub const HOST_VS: usize = 0x10;

// GHC Register Bits
pub const GHC_HR: u32 = 1 << 0;  // HBA Reset
pub const GHC_IE: u32 = 1 << 1;  // Interrupt Enable
pub const GHC_AE: u32 = 1 << 31; // AHCI Enable

// Port Register Offsets (Port p = 0x100 + p * 0x80)
pub const PORT_CLB: usize = 0x00;
pub const PORT_CLBU: usize = 0x04;
pub const PORT_FB: usize = 0x08;
pub const PORT_FBU: usize = 0x0C;
pub const PORT_IS: usize = 0x10;
pub const PORT_IE: usize = 0x14;
pub const PORT_CMD: usize = 0x18;
pub const PORT_TFD: usize = 0x20;
pub const PORT_SIG: usize = 0x24;
pub const PORT_SSTS: usize = 0x28;
pub const PORT_SCTL: usize = 0x2C;
pub const PORT_SERR: usize = 0x30;
pub const PORT_SACT: usize = 0x34;
pub const PORT_CI: usize = 0x38;

// Port CMD Register Bits
pub const PORT_CMD_ST: u32 = 1 << 0;  // Start
pub const PORT_CMD_SUD: u32 = 1 << 1; // Spin-Up Device
pub const PORT_CMD_POD: u32 = 1 << 2; // Power-On Device
pub const PORT_CMD_FRE: u32 = 1 << 4; // FIS Receive Enable
pub const PORT_CMD_FR: u32 = 1 << 14; // FIS Receive Running
pub const PORT_CMD_CR: u32 = 1 << 15; // Command List Running

// Task File Data (TFD) Status Bits
pub const ATA_DEV_BUSY: u32 = 0x80;
pub const ATA_DEV_DRQ: u32 = 0x08;
pub const ATA_DEV_ERR: u32 = 0x01;

// Device Signatures
pub const SATA_SIG_ATA: u32 = 0x00000101;
pub const SATA_SIG_ATAPI: u32 = 0xEB140101;

// ATA Commands
pub const ATA_CMD_READ_DMA_EXT: u8 = 0x25;
pub const ATA_CMD_WRITE_DMA_EXT: u8 = 0x35;
pub const ATA_CMD_IDENTIFY: u8 = 0xEC;

// FIS Types
pub const FIS_TYPE_REG_H2D: u8 = 0x27;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct AhciCmdHeader {
    pub flags: u16,         // CFL (4:0), A (5), W (6), P (7), R (8), B (9), C (10), PMP (15:12)
    pub prdtl: u16,         // Physical Region Descriptor Table Length
    pub prdbc: u32,         // Physical Region Descriptor Byte Count transferred
    pub ctba: u32,          // Command Table Base Address (lower 32)
    pub ctbau: u32,         // Command Table Base Address (upper 32)
    pub reserved: [u32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct AhciPrdtEntry {
    pub dba: u64,           // Data Base Address (64-bit physical)
    pub reserved: u32,
    pub dbc: u32,           // Data Byte Count (bits 21:0, 0-based), bit 31 = Interrupt on completion
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct FisRegH2D {
    pub fis_type: u8,       // 0x27
    pub pm_c: u8,           // bit 7 = 1 (Command), bits 3:0 = PM port
    pub command: u8,        // ATA command
    pub feature_low: u8,
    pub lba0: u8,
    pub lba1: u8,
    pub lba2: u8,
    pub device: u8,         // 1 << 6 for LBA mode
    pub lba3: u8,
    pub lba4: u8,
    pub lba5: u8,
    pub feature_high: u8,
    pub count_low: u8,
    pub count_high: u8,
    pub icc: u8,
    pub control: u8,
    pub reserved: [u8; 4],
}

pub struct AhciDevice {
    pub mmio_va: u64,
    pub port: usize,
    pub is_initialized: bool,
    pub total_sectors: u64,
    pub sector_size: u32,
    pub model: [u8; 40],
    pub model_len: usize,
    pub dma_page_pa: u64,
    pub dma_page_va: u64,
}

pub static mut GLOBAL_AHCI: Option<AhciDevice> = None;

pub fn register_global_ahci(dev: AhciDevice) {
    unsafe {
        GLOBAL_AHCI = Some(dev);
    }
}

pub fn get_global_ahci() -> Option<&'static mut AhciDevice> {
    unsafe {
        let ptr = core::ptr::addr_of_mut!(GLOBAL_AHCI);
        (*ptr).as_mut()
    }
}

pub fn get_global_ahci_ref() -> Option<&'static AhciDevice> {
    unsafe {
        let ptr = core::ptr::addr_of!(GLOBAL_AHCI);
        (*ptr).as_ref()
    }
}

impl AhciDevice {
    pub const fn empty() -> Self {
        Self {
            mmio_va: 0,
            port: 0,
            is_initialized: false,
            total_sectors: 0,
            sector_size: 512,
            model: [0; 40],
            model_len: 0,
            dma_page_pa: 0,
            dma_page_va: 0,
        }
    }

    pub fn model_str(&self) -> &str {
        if self.model_len == 0 {
            "Generic SATA Disk"
        } else {
            core::str::from_utf8(&self.model[..self.model_len]).unwrap_or("Generic SATA Disk")
        }
    }

    #[inline]
    unsafe fn read_reg32(&self, offset: usize) -> u32 {
        core::ptr::read_volatile((self.mmio_va + offset as u64) as *const u32)
    }

    #[inline]
    unsafe fn write_reg32(&self, offset: usize, val: u32) {
        core::ptr::write_volatile((self.mmio_va + offset as u64) as *mut u32, val);
    }

    #[inline]
    unsafe fn port_reg_offset(&self, port: usize, reg: usize) -> usize {
        0x100 + port * 0x80 + reg
    }

    #[inline]
    unsafe fn read_port32(&self, port: usize, reg: usize) -> u32 {
        self.read_reg32(self.port_reg_offset(port, reg))
    }

    #[inline]
    unsafe fn write_port32(&self, port: usize, reg: usize, val: u32) {
        self.write_reg32(self.port_reg_offset(port, reg), val);
    }

    /// Stops command processing and FIS reception on `port`.
    unsafe fn stop_port(&self, port: usize) {
        let mut cmd = self.read_port32(port, PORT_CMD);
        cmd &= !PORT_CMD_ST;
        cmd &= !PORT_CMD_FRE;
        self.write_port32(port, PORT_CMD, cmd);

        // Wait until CR and FR are cleared
        let mut spins = 0u32;
        loop {
            let cur = self.read_port32(port, PORT_CMD);
            if (cur & (PORT_CMD_CR | PORT_CMD_FR)) == 0 {
                break;
            }
            spins += 1;
            if spins > 2_000_000 {
                break;
            }
            core::hint::spin_loop();
        }
    }

    /// Starts FIS reception and command processing on `port`.
    unsafe fn start_port(&self, port: usize) {
        // Wait until CR is clear before setting ST
        let mut spins = 0u32;
        while (self.read_port32(port, PORT_CMD) & PORT_CMD_CR) != 0 {
            spins += 1;
            if spins > 2_000_000 {
                break;
            }
            core::hint::spin_loop();
        }

        let mut cmd = self.read_port32(port, PORT_CMD);
        cmd |= PORT_CMD_FRE;
        self.write_port32(port, PORT_CMD, cmd);

        cmd |= PORT_CMD_ST;
        self.write_port32(port, PORT_CMD, cmd);
    }

    /// Initializes PCIe AHCI SATA controller at `mmio_va`.
    pub unsafe fn init<const WORDS: usize>(
        &mut self,
        _pci: &PciDeviceInfo,
        pmm: &mut PhysicalPageAllocator<WORDS>,
        hhdm_base: u64,
        mmio_va: u64,
    ) -> Result<(), &'static str> {
        self.mmio_va = mmio_va;

        // 1. Enable AHCI mode (GHC.AE = 1)
        let ghc = self.read_reg32(HOST_GHC);
        self.write_reg32(HOST_GHC, ghc | GHC_AE);

        // 2. Perform HBA Reset (GHC.HR = 1)
        self.write_reg32(HOST_GHC, self.read_reg32(HOST_GHC) | GHC_HR);
        let mut spins = 0u32;
        loop {
            let val = self.read_reg32(HOST_GHC);
            if (val & GHC_HR) == 0 {
                break;
            }
            spins += 1;
            if spins > 2_000_000 {
                return Err("AHCI HBA reset timed out");
            }
            core::hint::spin_loop();
        }

        // Re-enable AHCI after reset
        self.write_reg32(HOST_GHC, self.read_reg32(HOST_GHC) | GHC_AE);

        // 3. Scan implemented ports (PI) for a connected ATA device
        let pi = self.read_reg32(HOST_PI);
        let mut found_port = None;

        for p in 0..32 {
            if (pi & (1 << p)) != 0 {
                let ssts = self.read_port32(p, PORT_SSTS);
                let det = ssts & 0x0F;
                let ipm = (ssts >> 8) & 0x0F;
                if det == 3 && ipm == 1 {
                    let sig = self.read_port32(p, PORT_SIG);
                    if sig != SATA_SIG_ATAPI {
                        found_port = Some(p);
                        break;
                    }
                }
            }
        }

        let port = found_port.ok_or("No active SATA ATA drive found on AHCI ports")?;
        self.port = port;

        // 4. Allocate 1 physical page for Command List, Received FIS, Command Table, and Scratch
        let dma_pa = pmm.allocate_page().ok_or("Failed to allocate AHCI DMA page")?;
        let dma_va = hhdm_base + dma_pa;
        core::ptr::write_bytes(dma_va as *mut u8, 0, 4096);

        self.dma_page_pa = dma_pa;
        self.dma_page_va = dma_va;

        // Layout within 4096-byte page:
        // Offset 0x000..0x3FF: Command List (1024 bytes, 1K aligned)
        // Offset 0x400..0x4FF: Received FIS (256 bytes, 256B aligned)
        // Offset 0x500..0x5FF: Command Table 0 (256 bytes, 128B aligned)
        // Offset 0x600..0x7FF: IDENTIFY Scratch Buffer (512 bytes)
        self.stop_port(port);

        let clb_pa = dma_pa;
        let fb_pa = dma_pa + 1024;

        self.write_port32(port, PORT_CLB, clb_pa as u32);
        self.write_port32(port, PORT_CLBU, (clb_pa >> 32) as u32);
        self.write_port32(port, PORT_FB, fb_pa as u32);
        self.write_port32(port, PORT_FBU, (fb_pa >> 32) as u32);

        // Clear interrupt and error status
        self.write_port32(port, PORT_SERR, 0xFFFF_FFFF);
        self.write_port32(port, PORT_IS, 0xFFFF_FFFF);

        self.start_port(port);

        // 5. Send IDENTIFY DEVICE (0xEC) to retrieve model and sector count
        self.identify()?;

        self.is_initialized = true;
        Ok(())
    }

    /// Submits a command on Slot 0 of `self.port` and waits for completion.
    unsafe fn submit_slot0_cmd(
        &self,
        write: bool,
        fis: FisRegH2D,
        buf_pa: u64,
        byte_count: u32,
    ) -> Result<(), &'static str> {
        let port = self.port;

        // 1. Wait until port not busy
        let mut spins = 0u32;
        while (self.read_port32(port, PORT_TFD) & (ATA_DEV_BUSY | ATA_DEV_DRQ)) != 0 {
            spins += 1;
            if spins > 2_000_000 {
                return Err("AHCI port busy before command issue");
            }
            core::hint::spin_loop();
        }

        // 2. Setup Command Table 0 (at offset 1280 = 0x500)
        let ct_va = self.dma_page_va + 1280;
        let ct_pa = self.dma_page_pa + 1280;
        core::ptr::write_bytes(ct_va as *mut u8, 0, 256);

        // Copy CFIS (20 bytes)
        let cfis_dst = ct_va as *mut FisRegH2D;
        core::ptr::write_volatile(cfis_dst, fis);

        // Setup PRDT entry 0 (at ct_va + 0x80)
        let prdt = (ct_va + 0x80) as *mut AhciPrdtEntry;
        core::ptr::write_volatile(
            prdt,
            AhciPrdtEntry {
                dba: buf_pa,
                reserved: 0,
                dbc: (byte_count - 1) | (1 << 31), // interrupt on completion
            },
        );

        clean_dcache_range(ct_va, 256);

        // 3. Setup Command Header 0 (at offset 0)
        let cl_va = self.dma_page_va as *mut AhciCmdHeader;
        let cfl = 5u16; // 5 DWORDs = 20 bytes
        let write_bit = if write { 1u16 << 6 } else { 0 };
        let flags = cfl | write_bit;

        core::ptr::write_volatile(
            cl_va,
            AhciCmdHeader {
                flags,
                prdtl: 1,
                prdbc: 0,
                ctba: ct_pa as u32,
                ctbau: (ct_pa >> 32) as u32,
                reserved: [0; 4],
            },
        );

        clean_dcache_range(self.dma_page_va, core::mem::size_of::<AhciCmdHeader>());

        #[cfg(target_arch = "aarch64")]
        {
            core::arch::asm!("dsb sy", options(nomem, nostack));
        }

        // 4. Issue command by setting bit 0 of PxCI
        self.write_port32(port, PORT_CI, 1);

        // 5. Poll PxCI until bit 0 is cleared by HBA
        spins = 0;
        loop {
            let ci = self.read_port32(port, PORT_CI);
            if (ci & 1) == 0 {
                break;
            }
            let is = self.read_port32(port, PORT_IS);
            if (is & (1 << 30)) != 0 {
                // Task File Error
                return Err("AHCI Task File Error during command execution");
            }
            spins += 1;
            if spins > 5_000_000 {
                return Err("AHCI command timed out waiting for PxCI bit 0");
            }
            core::hint::spin_loop();
        }

        let tfd = self.read_port32(port, PORT_TFD);
        if (tfd & ATA_DEV_ERR) != 0 {
            return Err("AHCI Task File Error (ERR bit set)");
        }

        Ok(())
    }

    /// Sends IDENTIFY DEVICE (0xEC) to retrieve disk geometry and serial/model.
    pub unsafe fn identify(&mut self) -> Result<(), &'static str> {
        let scratch_pa = self.dma_page_pa + 1536;
        let scratch_va = self.dma_page_va + 1536;
        core::ptr::write_bytes(scratch_va as *mut u8, 0, 512);
        clean_dcache_range(scratch_va, 512);

        let mut fis = FisRegH2D::default();
        fis.fis_type = FIS_TYPE_REG_H2D;
        fis.pm_c = 0x80; // Command
        fis.command = ATA_CMD_IDENTIFY;
        fis.device = 0;

        self.submit_slot0_cmd(false, fis, scratch_pa, 512)?;

        invalidate_dcache_range(scratch_va, 512);

        let data = scratch_va as *const u16;

        // Model string: words 27..46 (40 bytes, byte swapped within each word)
        let mut model_bytes = [0u8; 40];
        for i in 0..20 {
            let w = core::ptr::read_volatile(data.add(27 + i));
            model_bytes[i * 2] = (w >> 8) as u8;
            model_bytes[i * 2 + 1] = (w & 0xFF) as u8;
        }

        // Trim trailing spaces
        let mut len = 40;
        while len > 0 && (model_bytes[len - 1] == b' ' || model_bytes[len - 1] == 0) {
            len -= 1;
        }
        self.model = model_bytes;
        self.model_len = len;

        // Total sectors: 48-bit LBA count in words 100..103
        let lba_48_0 = core::ptr::read_volatile(data.add(100)) as u64;
        let lba_48_1 = core::ptr::read_volatile(data.add(101)) as u64;
        let lba_48_2 = core::ptr::read_volatile(data.add(102)) as u64;
        let lba_48_3 = core::ptr::read_volatile(data.add(103)) as u64;
        let total = lba_48_0 | (lba_48_1 << 16) | (lba_48_2 << 32) | (lba_48_3 << 48);

        if total > 0 {
            self.total_sectors = total;
        } else {
            // Fallback to 28-bit LBA in words 60..61
            let lba_28_0 = core::ptr::read_volatile(data.add(60)) as u64;
            let lba_28_1 = core::ptr::read_volatile(data.add(61)) as u64;
            self.total_sectors = lba_28_0 | (lba_28_1 << 16);
        }

        self.sector_size = 512;
        Ok(())
    }

    /// Reads consecutive 512-byte sectors via READ DMA EXT (0x25).
    pub unsafe fn read_sectors(
        &mut self,
        lba: u64,
        count: u16,
        buf_pa: u64,
        buf_va: u64,
    ) -> Result<(), &'static str> {
        if count == 0 {
            return Ok(());
        }

        let mut fis = FisRegH2D::default();
        fis.fis_type = FIS_TYPE_REG_H2D;
        fis.pm_c = 0x80; // Command
        fis.command = ATA_CMD_READ_DMA_EXT;
        fis.device = 1 << 6; // LBA mode

        fis.lba0 = (lba & 0xFF) as u8;
        fis.lba1 = ((lba >> 8) & 0xFF) as u8;
        fis.lba2 = ((lba >> 16) & 0xFF) as u8;
        fis.lba3 = ((lba >> 24) & 0xFF) as u8;
        fis.lba4 = ((lba >> 32) & 0xFF) as u8;
        fis.lba5 = ((lba >> 40) & 0xFF) as u8;

        fis.count_low = (count & 0xFF) as u8;
        fis.count_high = ((count >> 8) & 0xFF) as u8;

        let total_bytes = (count as u32) * 512;
        clean_dcache_range(buf_va, total_bytes as usize);

        self.submit_slot0_cmd(false, fis, buf_pa, total_bytes)?;

        invalidate_dcache_range(buf_va, total_bytes as usize);
        Ok(())
    }

    /// Writes consecutive 512-byte sectors via WRITE DMA EXT (0x35).
    pub unsafe fn write_sectors(
        &mut self,
        lba: u64,
        count: u16,
        buf_pa: u64,
        buf_va: u64,
    ) -> Result<(), &'static str> {
        if count == 0 {
            return Ok(());
        }

        let mut fis = FisRegH2D::default();
        fis.fis_type = FIS_TYPE_REG_H2D;
        fis.pm_c = 0x80; // Command
        fis.command = ATA_CMD_WRITE_DMA_EXT;
        fis.device = 1 << 6; // LBA mode

        fis.lba0 = (lba & 0xFF) as u8;
        fis.lba1 = ((lba >> 8) & 0xFF) as u8;
        fis.lba2 = ((lba >> 16) & 0xFF) as u8;
        fis.lba3 = ((lba >> 24) & 0xFF) as u8;
        fis.lba4 = ((lba >> 32) & 0xFF) as u8;
        fis.lba5 = ((lba >> 40) & 0xFF) as u8;

        fis.count_low = (count & 0xFF) as u8;
        fis.count_high = ((count >> 8) & 0xFF) as u8;

        let total_bytes = (count as u32) * 512;
        clean_dcache_range(buf_va, total_bytes as usize);

        self.submit_slot0_cmd(true, fis, buf_pa, total_bytes)?;
        Ok(())
    }
}

impl BlockDevice for AhciDevice {
    unsafe fn read_blocks(
        &mut self,
        lba: u64,
        count: u16,
        buf_pa: u64,
        buf_va: u64,
    ) -> Result<(), &'static str> {
        self.read_sectors(lba, count, buf_pa, buf_va)
    }

    unsafe fn write_blocks(
        &mut self,
        lba: u64,
        count: u16,
        buf_pa: u64,
        buf_va: u64,
    ) -> Result<(), &'static str> {
        self.write_sectors(lba, count, buf_pa, buf_va)
    }

    fn block_size(&self) -> u32 {
        self.sector_size
    }

    fn total_blocks(&self) -> u64 {
        self.total_sectors
    }

    fn name(&self) -> &str {
        "ahci0"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ahci_struct_sizes_and_alignments() {
        assert_eq!(core::mem::size_of::<AhciCmdHeader>(), 32);
        assert_eq!(core::mem::align_of::<AhciCmdHeader>(), 4);

        assert_eq!(core::mem::size_of::<AhciPrdtEntry>(), 16);
        assert_eq!(core::mem::align_of::<AhciPrdtEntry>(), 8);

        assert_eq!(core::mem::size_of::<FisRegH2D>(), 20);
        assert_eq!(core::mem::align_of::<FisRegH2D>(), 1);
    }

    #[test]
    fn ahci_block_device_trait_implementation() {
        let mut dev = AhciDevice::empty();
        dev.total_sectors = 204800; // 100 MiB
        dev.sector_size = 512;
        dev.is_initialized = true;

        assert_eq!(dev.name(), "ahci0");
        assert_eq!(dev.block_size(), 512);
        assert_eq!(dev.total_blocks(), 204800);
    }

    #[test]
    fn ahci_register_constants_and_offsets() {
        assert_eq!(HOST_CAP, 0x00);
        assert_eq!(HOST_GHC, 0x04);
        assert_eq!(HOST_IS, 0x08);
        assert_eq!(HOST_PI, 0x0C);
        assert_eq!(HOST_VS, 0x10);

        assert_eq!(GHC_HR, 1);
        assert_eq!(GHC_IE, 2);
        assert_eq!(GHC_AE, 0x8000_0000);

        // Port 0 base is 0x100, Port 1 base is 0x180
        let p0_clb = 0x100 + 0 * 0x80 + PORT_CLB;
        let p1_clb = 0x100 + 1 * 0x80 + PORT_CLB;
        assert_eq!(p0_clb, 0x100);
        assert_eq!(p1_clb, 0x180);

        assert_eq!(PORT_CMD_ST, 1);
        assert_eq!(PORT_CMD_FRE, 1 << 4);
    }
}
