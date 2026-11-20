//! PCIe NVMe SSD Driver for Project Genesis (Stage 20).
//!
//! Implements PCIe NVMe controller initialization, Admin Submission/Completion Queues,
//! Identify Controller & Namespace commands, I/O Queue creation, and synchronous
//! LBA read/write operations using PRPs.

use core::ptr;
use crate::pmm::PhysicalPageAllocator;
use crate::drivers::pci::PciDeviceInfo;

pub const NVME_MMIO_VA_BASE: u64 = 0xFFFF_FFFF_E400_0000;

// Admin Opcodes
pub const NVME_ADMIN_OP_CREATE_SQ: u8 = 0x01;
pub const NVME_ADMIN_OP_CREATE_CQ: u8 = 0x05;
pub const NVME_ADMIN_OP_IDENTIFY: u8 = 0x06;

// NVM I/O Opcodes
pub const NVME_IO_OP_WRITE: u8 = 0x01;
pub const NVME_IO_OP_READ: u8 = 0x02;

// Identify CNS values
pub const NVME_IDENTIFY_CNS_NAMESPACE: u32 = 0x00;
pub const NVME_IDENTIFY_CNS_CONTROLLER: u32 = 0x01;

pub const QUEUE_SIZE: u16 = 64;

#[inline]
pub unsafe fn clean_dcache_range(va: u64, len: usize) {
    #[cfg(target_os = "uefi")]
    {
        let start = va & !63;
        let end = (va + len as u64 + 63) & !63;
        let mut p = start;
        while p < end {
            core::arch::asm!("dc cvac, {0}", in(reg) p, options(nomem, nostack));
            p += 64;
        }
        core::arch::asm!("dsb sy", options(nomem, nostack));
    }
    #[cfg(not(target_os = "uefi"))]
    let _ = (va, len);
}

#[inline]
pub unsafe fn invalidate_dcache_range(va: u64, len: usize) {
    #[cfg(target_os = "uefi")]
    {
        let start = va & !63;
        let end = (va + len as u64 + 63) & !63;
        let mut p = start;
        while p < end {
            core::arch::asm!("dc ivac, {0}", in(reg) p, options(nomem, nostack));
            p += 64;
        }
        core::arch::asm!("dsb sy", options(nomem, nostack));
    }
    #[cfg(not(target_os = "uefi"))]
    let _ = (va, len);
}

/// NVMe Submission Queue Entry (64 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NvmeSqe {
    pub cdw0: u32,
    pub nsid: u32,
    pub cdw2: u32,
    pub cdw3: u32,
    pub mptr: u64,
    pub prp1: u64,
    pub prp2: u64,
    pub cdw10: u32,
    pub cdw11: u32,
    pub cdw12: u32,
    pub cdw13: u32,
    pub cdw14: u32,
    pub cdw15: u32,
}

/// NVMe Completion Queue Entry (16 bytes).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NvmeCqe {
    pub dw0: u32,
    pub dw1: u32,
    pub sq_head: u16,
    pub sq_id: u16,
    pub cid: u16,
    pub status: u16,
}

impl NvmeCqe {
    pub fn phase(&self) -> u8 {
        (self.status & 1) as u8
    }

    pub fn status_code(&self) -> u16 {
        (self.status >> 1) & 0x7FFF
    }
}

/// NVMe Controller Device Instance.
#[derive(Clone, Copy, Debug)]
pub struct NvmeDevice {
    pub mmio_va: u64,
    pub dstrd: u8,
    pub mqes: u16,
    pub model: [u8; 40],
    pub serial: [u8; 20],
    pub total_blocks: u64,
    pub lba_bytes: u32,

    // Admin Queue (QID 0)
    pub asq_pa: u64,
    pub asq_va: u64,
    pub acq_pa: u64,
    pub acq_va: u64,
    pub asq_tail: u16,
    pub acq_head: u16,
    pub acq_phase: u16,

    // I/O Queue (QID 1)
    pub iosq_pa: u64,
    pub iosq_va: u64,
    pub iocq_pa: u64,
    pub iocq_va: u64,
    pub iosq_tail: u16,
    pub iocq_head: u16,
    pub iocq_phase: u16,

    pub next_cid: u16,
    pub is_initialized: bool,
}

impl NvmeDevice {
    pub const fn empty() -> Self {
        Self {
            mmio_va: 0,
            dstrd: 0,
            mqes: 0,
            model: [0; 40],
            serial: [0; 20],
            total_blocks: 0,
            lba_bytes: 512,
            asq_pa: 0,
            asq_va: 0,
            acq_pa: 0,
            acq_va: 0,
            asq_tail: 0,
            acq_head: 0,
            acq_phase: 1,
            iosq_pa: 0,
            iosq_va: 0,
            iocq_pa: 0,
            iocq_va: 0,
            iosq_tail: 0,
            iocq_head: 0,
            iocq_phase: 1,
            next_cid: 1,
            is_initialized: false,
        }
    }

    /// Doorbell register address for queue `qid` (is_cq: false for SQ tail, true for CQ head)
    pub fn doorbell_addr(&self, qid: u16, is_cq: bool) -> u64 {
        let stride = 4u64 << self.dstrd;
        let index = (qid as u64) * 2 + if is_cq { 1 } else { 0 };
        self.mmio_va + 0x1000 + index * stride
    }

    /// Initialize controller, allocate Admin & I/O queues, and identify namespace 1.
    pub unsafe fn init<const WORDS: usize>(
        &mut self,
        _pci_dev: &PciDeviceInfo,
        pmm: &mut PhysicalPageAllocator<WORDS>,
        hhdm_base: u64,
        mmio_va: u64,
    ) -> Result<(), &'static str> {
        self.mmio_va = mmio_va;

        // 1. Read Capabilities (CAP) at offset 0x00
        let cap = ptr::read_volatile(mmio_va as *const u64);
        self.mqes = (cap & 0xFFFF) as u16;
        self.dstrd = ((cap >> 32) & 0xF) as u8;

        // 2. Disable controller (CC.EN = 0) and wait for CSTS.RDY == 0
        let cc = ptr::read_volatile((mmio_va + 0x14) as *const u32);
        if (cc & 1) != 0 {
            ptr::write_volatile((mmio_va + 0x14) as *mut u32, cc & !1);
        }
        let mut timeout = 100_000;
        while (ptr::read_volatile((mmio_va + 0x1C) as *const u32) & 1) != 0 && timeout > 0 {
            timeout -= 1;
        }
        if timeout == 0 {
            return Err("NVME_DISABLE_TIMEOUT");
        }

        // 3. Allocate Admin Submission & Completion Queues (1 page each = 4096 bytes)
        let asq_pa = pmm.allocate_page().ok_or("NVME_ALLOC_ASQ_FAIL")?;
        let asq_va = hhdm_base + asq_pa;
        ptr::write_bytes(asq_va as *mut u8, 0, 4096);
        clean_dcache_range(asq_va, 4096);

        let acq_pa = pmm.allocate_page().ok_or("NVME_ALLOC_ACQ_FAIL")?;
        let acq_va = hhdm_base + acq_pa;
        ptr::write_bytes(acq_va as *mut u8, 0, 4096);
        clean_dcache_range(acq_va, 4096);

        self.asq_pa = asq_pa;
        self.asq_va = asq_va;
        self.acq_pa = acq_pa;
        self.acq_va = acq_va;
        self.asq_tail = 0;
        self.acq_head = 0;
        self.acq_phase = 1;

        // Program AQA: Admin Submission/Completion Queue Size (0-based)
        let aqa_val = (((QUEUE_SIZE - 1) as u32) << 16) | ((QUEUE_SIZE - 1) as u32);
        ptr::write_volatile((mmio_va + 0x24) as *mut u32, aqa_val);

        // Program ASQ and ACQ base addresses
        ptr::write_volatile((mmio_va + 0x28) as *mut u64, asq_pa);
        ptr::write_volatile((mmio_va + 0x30) as *mut u64, acq_pa);

        // 4. Configure CC: CSS=0 (NVM), MPS=0 (4KB), IOSQES=6 (64B), IOCQES=4 (16B), EN=1
        let cc_val = 1u32 | (6 << 16) | (4 << 20);
        ptr::write_volatile((mmio_va + 0x14) as *mut u32, cc_val);

        // Wait for CSTS.RDY == 1
        let mut timeout = 100_000;
        while (ptr::read_volatile((mmio_va + 0x1C) as *const u32) & 1) == 0 && timeout > 0 {
            timeout -= 1;
        }
        if timeout == 0 {
            return Err("NVME_ENABLE_TIMEOUT");
        }

        // Allocate scratch page for Admin commands (Identify)
        let scratch_pa = pmm.allocate_page().ok_or("NVME_ALLOC_SCRATCH_FAIL")?;
        let scratch_va = hhdm_base + scratch_pa;

        // 5. Identify Controller (CNS = 1)
        ptr::write_bytes(scratch_va as *mut u8, 0, 4096);
        clean_dcache_range(scratch_va, 4096);
        let mut id_ctrl_sqe = NvmeSqe::default();
        id_ctrl_sqe.cdw0 = (NVME_ADMIN_OP_IDENTIFY as u32) | ((self.alloc_cid() as u32) << 16);
        id_ctrl_sqe.nsid = 0;
        id_ctrl_sqe.prp1 = scratch_pa;
        id_ctrl_sqe.cdw10 = NVME_IDENTIFY_CNS_CONTROLLER;

        self.submit_admin_cmd(id_ctrl_sqe)?;
        invalidate_dcache_range(scratch_va, 4096);

        // Extract serial (offset 4..24) and model (offset 24..64)
        for i in 0..20 {
            self.serial[i] = ptr::read_volatile((scratch_va + 4 + i as u64) as *const u8);
        }
        for i in 0..40 {
            self.model[i] = ptr::read_volatile((scratch_va + 24 + i as u64) as *const u8);
        }

        // 6. Identify Namespace 1 (CNS = 0)
        ptr::write_bytes(scratch_va as *mut u8, 0, 4096);
        clean_dcache_range(scratch_va, 4096);
        let mut id_ns_sqe = NvmeSqe::default();
        id_ns_sqe.cdw0 = (NVME_ADMIN_OP_IDENTIFY as u32) | ((self.alloc_cid() as u32) << 16);
        id_ns_sqe.nsid = 1;
        id_ns_sqe.prp1 = scratch_pa;
        id_ns_sqe.cdw10 = NVME_IDENTIFY_CNS_NAMESPACE;

        self.submit_admin_cmd(id_ns_sqe)?;
        invalidate_dcache_range(scratch_va, 4096);

        // NSZE at offset 0 (u64)
        self.total_blocks = ptr::read_unaligned(scratch_va as *const u64);
        let flbas = ptr::read_volatile((scratch_va + 26) as *const u8);
        let format_idx = (flbas & 0xF) as u64;
        let lbads = ptr::read_volatile((scratch_va + 128 + format_idx * 4 + 2) as *const u8);
        self.lba_bytes = 1 << lbads;

        // 7. Create I/O Completion Queue (QID 1)
        let iocq_pa = pmm.allocate_page().ok_or("NVME_ALLOC_IOCQ_FAIL")?;
        let iocq_va = hhdm_base + iocq_pa;
        ptr::write_bytes(iocq_va as *mut u8, 0, 4096);
        clean_dcache_range(iocq_va, 4096);
        self.iocq_pa = iocq_pa;
        self.iocq_va = iocq_va;
        self.iocq_head = 0;
        self.iocq_phase = 1;

        let mut create_cq_sqe = NvmeSqe::default();
        create_cq_sqe.cdw0 = (NVME_ADMIN_OP_CREATE_CQ as u32) | ((self.alloc_cid() as u32) << 16);
        create_cq_sqe.prp1 = iocq_pa;
        create_cq_sqe.cdw10 = (((QUEUE_SIZE - 1) as u32) << 16) | 1u32; // QSIZE=63, QID=1
        create_cq_sqe.cdw11 = 1u32; // Physically Contiguous = 1, Interrupts disabled = 0
        self.submit_admin_cmd(create_cq_sqe)?;

        // 8. Create I/O Submission Queue (QID 1)
        let iosq_pa = pmm.allocate_page().ok_or("NVME_ALLOC_IOSQ_FAIL")?;
        let iosq_va = hhdm_base + iosq_pa;
        ptr::write_bytes(iosq_va as *mut u8, 0, 4096);
        clean_dcache_range(iosq_va, 4096);
        self.iosq_pa = iosq_pa;
        self.iosq_va = iosq_va;
        self.iosq_tail = 0;

        let mut create_sq_sqe = NvmeSqe::default();
        create_sq_sqe.cdw0 = (NVME_ADMIN_OP_CREATE_SQ as u32) | ((self.alloc_cid() as u32) << 16);
        create_sq_sqe.prp1 = iosq_pa;
        create_sq_sqe.cdw10 = (((QUEUE_SIZE - 1) as u32) << 16) | 1u32; // QSIZE=63, QID=1
        create_sq_sqe.cdw11 = (1u32 << 16) | 1u32; // CQID=1, Physically Contiguous = 1
        self.submit_admin_cmd(create_sq_sqe)?;

        self.is_initialized = true;
        Ok(())
    }

    fn alloc_cid(&mut self) -> u16 {
        let cid = self.next_cid;
        self.next_cid = self.next_cid.wrapping_add(1);
        if self.next_cid == 0 {
            self.next_cid = 1;
        }
        cid
    }

    /// Submit a command to the Admin Submission Queue and wait for completion.
    pub unsafe fn submit_admin_cmd(&mut self, sqe: NvmeSqe) -> Result<NvmeCqe, &'static str> {
        let tail = self.asq_tail as usize;
        let sqe_ptr = (self.asq_va as *mut NvmeSqe).add(tail);
        ptr::write_volatile(sqe_ptr, sqe);
        clean_dcache_range(sqe_ptr as u64, core::mem::size_of::<NvmeSqe>());

        self.asq_tail = (self.asq_tail + 1) % QUEUE_SIZE;
        let sq_db = self.doorbell_addr(0, false);
        ptr::write_volatile(sq_db as *mut u32, self.asq_tail as u32);

        // Poll ACQ
        let mut spins = 1_000_000;
        while spins > 0 {
            let head = self.acq_head as usize;
            let cqe_ptr = (self.acq_va as *const NvmeCqe).add(head);
            invalidate_dcache_range(cqe_ptr as u64, core::mem::size_of::<NvmeCqe>());
            let cqe = ptr::read_volatile(cqe_ptr);

            if (cqe.status & 1) == self.acq_phase {
                self.acq_head = (self.acq_head + 1) % QUEUE_SIZE;
                if self.acq_head == 0 {
                    self.acq_phase ^= 1;
                }
                let cq_db = self.doorbell_addr(0, true);
                ptr::write_volatile(cq_db as *mut u32, self.acq_head as u32);

                if cqe.status_code() == 0 {
                    return Ok(cqe);
                } else {
                    return Err("NVME_ADMIN_CMD_FAILED");
                }
            }
            spins -= 1;
        }
        Err("NVME_ADMIN_CMD_TIMEOUT")
    }

    /// Submit an I/O command (QID 1) and wait for completion.
    pub unsafe fn submit_io_cmd(&mut self, sqe: NvmeSqe) -> Result<NvmeCqe, &'static str> {
        let tail = self.iosq_tail as usize;
        let sqe_ptr = (self.iosq_va as *mut NvmeSqe).add(tail);
        ptr::write_volatile(sqe_ptr, sqe);
        clean_dcache_range(sqe_ptr as u64, core::mem::size_of::<NvmeSqe>());

        self.iosq_tail = (self.iosq_tail + 1) % QUEUE_SIZE;
        let sq_db = self.doorbell_addr(1, false);
        ptr::write_volatile(sq_db as *mut u32, self.iosq_tail as u32);

        // Poll IOCQ
        let mut spins = 1_000_000;
        while spins > 0 {
            let head = self.iocq_head as usize;
            let cqe_ptr = (self.iocq_va as *const NvmeCqe).add(head);
            invalidate_dcache_range(cqe_ptr as u64, core::mem::size_of::<NvmeCqe>());
            let cqe = ptr::read_volatile(cqe_ptr);

            if (cqe.status & 1) == self.iocq_phase {
                self.iocq_head = (self.iocq_head + 1) % QUEUE_SIZE;
                if self.iocq_head == 0 {
                    self.iocq_phase ^= 1;
                }
                let cq_db = self.doorbell_addr(1, true);
                ptr::write_volatile(cq_db as *mut u32, self.iocq_head as u32);

                if cqe.status_code() == 0 {
                    return Ok(cqe);
                } else {
                    return Err("NVME_IO_CMD_FAILED");
                }
            }
            spins -= 1;
        }
        Err("NVME_IO_CMD_TIMEOUT")
    }

    /// Read `count` LBAs starting at `lba` into buffer at `buf_pa` (and `buf_va`).
    pub unsafe fn read_lba(&mut self, lba: u64, count: u16, buf_pa: u64, buf_va: u64) -> Result<(), &'static str> {
        if !self.is_initialized || count == 0 {
            return Err("NVME_NOT_READY");
        }
        let total_bytes = (count as usize) * (self.lba_bytes as usize);
        let mut sqe = NvmeSqe::default();
        sqe.cdw0 = (NVME_IO_OP_READ as u32) | ((self.alloc_cid() as u32) << 16);
        sqe.nsid = 1;
        sqe.prp1 = buf_pa;
        if total_bytes > 4096 {
            sqe.prp2 = buf_pa + 4096;
        }
        sqe.cdw10 = (lba & 0xFFFF_FFFF) as u32;
        sqe.cdw11 = ((lba >> 32) & 0xFFFF_FFFF) as u32;
        sqe.cdw12 = (count - 1) as u32; // NLB is 0-based

        self.submit_io_cmd(sqe)?;
        invalidate_dcache_range(buf_va, total_bytes);
        Ok(())
    }

    /// Write `count` LBAs starting at `lba` from buffer at `buf_pa` (and `buf_va`).
    pub unsafe fn write_lba(&mut self, lba: u64, count: u16, buf_pa: u64, buf_va: u64) -> Result<(), &'static str> {
        if !self.is_initialized || count == 0 {
            return Err("NVME_NOT_READY");
        }
        let total_bytes = (count as usize) * (self.lba_bytes as usize);
        clean_dcache_range(buf_va, total_bytes);

        let mut sqe = NvmeSqe::default();
        sqe.cdw0 = (NVME_IO_OP_WRITE as u32) | ((self.alloc_cid() as u32) << 16);
        sqe.nsid = 1;
        sqe.prp1 = buf_pa;
        if total_bytes > 4096 {
            sqe.prp2 = buf_pa + 4096;
        }
        sqe.cdw10 = (lba & 0xFFFF_FFFF) as u32;
        sqe.cdw11 = ((lba >> 32) & 0xFFFF_FFFF) as u32;
        sqe.cdw12 = (count - 1) as u32; // NLB is 0-based

        self.submit_io_cmd(sqe)?;
        Ok(())
    }

    /// Cleanly formatted model string
    pub fn model_str(&self) -> &str {
        core::str::from_utf8(&self.model).unwrap_or("Unknown NVMe").trim()
    }

    /// Cleanly formatted serial string
    pub fn serial_str(&self) -> &str {
        core::str::from_utf8(&self.serial).unwrap_or("Unknown Serial").trim()
    }
}

pub static mut GLOBAL_NVME: Option<NvmeDevice> = None;

#[allow(static_mut_refs)]
pub fn register_global_nvme(dev: NvmeDevice) {
    unsafe {
        GLOBAL_NVME = Some(dev);
    }
}

#[allow(static_mut_refs)]
pub fn get_global_nvme() -> Option<&'static mut NvmeDevice> {
    unsafe { GLOBAL_NVME.as_mut() }
}

#[allow(static_mut_refs)]
pub fn get_global_nvme_ref() -> Option<&'static NvmeDevice> {
    unsafe { GLOBAL_NVME.as_ref() }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn nvme_sqe_and_cqe_size_and_alignment() {
        assert_eq!(core::mem::size_of::<NvmeSqe>(), 64);
        assert_eq!(core::mem::size_of::<NvmeCqe>(), 16);
    }

    #[test]
    fn nvme_doorbell_address_math() {
        let mut dev = NvmeDevice::empty();
        dev.mmio_va = 0xFFFF_FFFF_E400_0000;
        dev.dstrd = 0; // 4 bytes per doorbell

        // QID 0 (Admin)
        assert_eq!(dev.doorbell_addr(0, false), 0xFFFF_FFFF_E400_1000); // ASQ Tail
        assert_eq!(dev.doorbell_addr(0, true), 0xFFFF_FFFF_E400_1004);  // ACQ Head

        // QID 1 (I/O)
        assert_eq!(dev.doorbell_addr(1, false), 0xFFFF_FFFF_E400_1008); // IOSQ Tail
        assert_eq!(dev.doorbell_addr(1, true), 0xFFFF_FFFF_E400_100C);  // IOCQ Head

        // Stride 1 (8 bytes per doorbell)
        dev.dstrd = 1;
        assert_eq!(dev.doorbell_addr(0, false), 0xFFFF_FFFF_E400_1000);
        assert_eq!(dev.doorbell_addr(0, true), 0xFFFF_FFFF_E400_1008);
        assert_eq!(dev.doorbell_addr(1, false), 0xFFFF_FFFF_E400_1010);
        assert_eq!(dev.doorbell_addr(1, true), 0xFFFF_FFFF_E400_1018);
    }

    #[test]
    fn nvme_cqe_status_decoding() {
        let cqe = NvmeCqe {
            dw0: 0,
            dw1: 0,
            sq_head: 4,
            sq_id: 1,
            cid: 42,
            status: 0x0001, // Phase = 1, Status code = 0 (Success)
        };
        assert_eq!(cqe.phase(), 1);
        assert_eq!(cqe.status_code(), 0);

        let err_cqe = NvmeCqe {
            dw0: 0,
            dw1: 0,
            sq_head: 5,
            sq_id: 1,
            cid: 43,
            status: (0x0002 << 1) | 0, // Phase = 0, Status code = 2 (Invalid Field)
        };
        assert_eq!(err_cqe.phase(), 0);
        assert_eq!(err_cqe.status_code(), 2);
    }
}
