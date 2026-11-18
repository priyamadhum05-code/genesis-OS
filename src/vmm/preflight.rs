//! Stage 3C-1 Preflight Verification & Dormant Table Construction.
//!
//! Constructs the dual-root translation tables (TTBR0 identity trampoline and TTBR1 higher-half)
//! in dormant memory, verifies CPU memory model feature registers, and executes software
//! translation checks for PC, SP, and UART MMIO without modifying any active MMU state.

use crate::vmm::{
    address_space::AddressSpace,
    page_table::{
        DescriptorInfo, FrameAllocator, PagePermissions, PhysicalWxTracker, VmmError, PAGE_SIZE,
        TABLE_ENTRY_COUNT,
    },
    parse_mmfr0, CpuMemoryFeatures,
    pe::ParsedPeImage,
};

pub const KERNEL_VA_BASE: u64 = 0xFFFF_FFFF_8000_0000;
pub const MMIO_VA_BASE: u64 = 0xFFFF_FFFF_F000_0000;
pub const KERNEL_IMAGE_PADDR: u64 = 0x4800_0000;
pub const KERNEL_RESERVE_SIZE: u64 = 4 * 1024 * 1024; // 4 MiB
pub const HHDM_BASE: u64 = 0xFFFF_8000_0000_0000;
pub const DRAM_PADDR_START: u64 = 0x4000_0000;
pub const DRAM_SIZE: u64 = 256 * 1024 * 1024;
pub const DRAM_PADDR_END: u64 = DRAM_PADDR_START + DRAM_SIZE;

#[inline]
pub const fn phys_to_virt(paddr: u64) -> u64 {
    HHDM_BASE + paddr
}

#[inline]
pub const fn virt_to_phys(vaddr: u64) -> Option<u64> {
    vaddr.checked_sub(HHDM_BASE)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreflightReport {
    pub cpu_features: CpuMemoryFeatures,
    pub ttbr0_root: u64,
    pub ttbr1_root: u64,
    pub ttbr0_mapped_count: usize,
    pub ttbr1_mapped_count: usize,
    pub ttbr0_frames: [u64; DORMANT_POOL_FRAMES],
    pub ttbr0_frame_count: usize,
    pub ttbr1_frames: [u64; DORMANT_POOL_FRAMES],
    pub ttbr1_frame_count: usize,
    pub unique_physical_pages_checked: usize,
    pub header_desc: DescriptorInfo,
    pub code_desc: DescriptorInfo,
    pub rodata_desc: DescriptorInfo,
    pub data_desc: DescriptorInfo,
    pub stack_desc: DescriptorInfo,
    pub reloc_desc: DescriptorInfo,
    pub uart_desc: DescriptorInfo,
}

pub const DORMANT_POOL_FRAMES: usize = 16;
pub const DORMANT_POOL_BYTES: usize = (DORMANT_POOL_FRAMES + 1) * 4096;

/// Static pool of 4 KiB translation-table frames located in resident Genesis image memory.
///
/// Ensures strict 4 KiB physical alignment by using an internal padding buffer and dynamic
/// alignment calculation on initialization.
pub struct DormantTablePool {
    pub storage: [u8; DORMANT_POOL_BYTES],
    pub allocated: [bool; DORMANT_POOL_FRAMES],
    pub base_paddr: u64,
}

impl DormantTablePool {
    pub const fn new() -> Self {
        Self {
            storage: [0; DORMANT_POOL_BYTES],
            allocated: [false; DORMANT_POOL_FRAMES],
            base_paddr: 0,
        }
    }

    /// Initializes the pool and computes the strictly 4 KiB-aligned base physical address.
    pub fn init(&mut self) {
        let raw_ptr = self.storage.as_ptr() as u64;
        self.base_paddr = (raw_ptr + (PAGE_SIZE - 1)) & !(PAGE_SIZE - 1);
        self.allocated = [false; DORMANT_POOL_FRAMES];
    }

    pub fn base_paddr(&self) -> u64 {
        self.base_paddr
    }
}

impl FrameAllocator for DormantTablePool {
    fn allocate_frame(&mut self) -> Option<u64> {
        if self.base_paddr == 0 {
            self.init();
        }
        for idx in 0..DORMANT_POOL_FRAMES {
            if !self.allocated[idx] {
                self.allocated[idx] = true;
                let frame_paddr = self.base_paddr + (idx as u64) * PAGE_SIZE;
                unsafe {
                    self.frame_mut(frame_paddr).fill(0);
                }
                return Some(frame_paddr);
            }
        }
        None
    }

    fn deallocate_frame(&mut self, paddr: u64) {
        if paddr < self.base_paddr {
            panic!("Deallocating out-of-bounds frame");
        }
        let offset = paddr - self.base_paddr;
        let idx = (offset / PAGE_SIZE) as usize;
        if idx >= DORMANT_POOL_FRAMES || !self.allocated[idx] {
            panic!("Deallocating unallocated or invalid frame");
        }
        unsafe {
            self.frame_mut(paddr).fill(0);
        }
        self.allocated[idx] = false;
    }

    unsafe fn frame_mut(&mut self, paddr: u64) -> &mut [u64; TABLE_ENTRY_COUNT] {
        let offset = paddr.checked_sub(self.base_paddr).expect("Frame paddr underflow");
        let idx = (offset / PAGE_SIZE) as usize;
        assert!(idx < DORMANT_POOL_FRAMES && self.allocated[idx], "Accessing unallocated frame");
        let ptr = (self.base_paddr + (idx as u64) * PAGE_SIZE) as *mut [u64; TABLE_ENTRY_COUNT];
        &mut *ptr
    }

    unsafe fn frame_ref(&self, paddr: u64) -> &[u64; TABLE_ENTRY_COUNT] {
        let offset = paddr.checked_sub(self.base_paddr).expect("Frame paddr underflow");
        let idx = (offset / PAGE_SIZE) as usize;
        assert!(idx < DORMANT_POOL_FRAMES && self.allocated[idx], "Accessing unallocated frame");
        let ptr = (self.base_paddr + (idx as u64) * PAGE_SIZE) as *const [u64; TABLE_ENTRY_COUNT];
        &*ptr
    }
}

/// Constructs the dormant dual translation tables and validates software translation.
///
/// STRICT SAFETY INVARIANT:
/// This function constructs tables in dormant memory and verifies translation mappings.
/// It does NOT write any system registers (MAIR_EL1, TCR_EL1, TTBR0_EL1, TTBR1_EL1, SCTLR_EL1),
/// does NOT enable the MMU, and does NOT alter the executing translation regime.
pub fn run_stage3c1_preflight(
    mmfr0: u64,
    current_pc: u64,
    current_sp: u64,
    uart_base: u64,
    pool0: &mut DormantTablePool,
    pool1: &mut DormantTablePool,
    pe: &ParsedPeImage,
) -> Result<PreflightReport, VmmError> {
    // 1. Strictly validate CPU feature register
    let cpu_features = parse_mmfr0(mmfr0)?;
    let max_pa = cpu_features.max_pa;

    // 2. Build TTBR0 dormant identity space (covers executing code, stack, DRAM, and UART)
    // Uses Level 3 (4 KiB page) granularity across the entire 4 MiB kernel window to enforce
    // strict W^X separation between code, read-only data, and writable data/stack.
    let mut aspace_ttbr0 = AddressSpace::new(&mut *pool0, max_pa)?;
    
    let header_pages = pe.header_pages;
    aspace_ttbr0.map_range(KERNEL_IMAGE_PADDR, KERNEL_IMAGE_PADDR, header_pages, PagePermissions::kernel_rodata())?;

    let mut mapped_pages = header_pages;
    let mut cur_rva = (header_pages as u64) * PAGE_SIZE;

    for sec_opt in &pe.sections[..pe.section_count] {
        if let Some(sec) = sec_opt {
            if sec.virtual_address > cur_rva {
                let gap_pages = ((sec.virtual_address - cur_rva) / PAGE_SIZE) as usize;
                aspace_ttbr0.map_range(KERNEL_IMAGE_PADDR + cur_rva, KERNEL_IMAGE_PADDR + cur_rva, gap_pages, PagePermissions::kernel_data())?;
                mapped_pages += gap_pages;
            }

            let perms = if sec.is_executable {
                PagePermissions::kernel_code()
            } else if sec.is_writable {
                PagePermissions::kernel_data()
            } else {
                PagePermissions::kernel_rodata()
            };

            aspace_ttbr0.map_range(KERNEL_IMAGE_PADDR + sec.virtual_address, KERNEL_IMAGE_PADDR + sec.virtual_address, sec.page_count, perms)?;
            mapped_pages += sec.page_count;
            cur_rva = sec.virtual_address + (sec.page_count as u64) * PAGE_SIZE;
        }
    }

    let total_kernel_pages = (KERNEL_RESERVE_SIZE / PAGE_SIZE) as usize;
    if mapped_pages < total_kernel_pages {
        let remainder = total_kernel_pages - mapped_pages;
        aspace_ttbr0.map_range(KERNEL_IMAGE_PADDR + cur_rva, KERNEL_IMAGE_PADDR + cur_rva, remainder, PagePermissions::kernel_data())?;
    }

    // Identity map PL011 UART MMIO (0x0900_0000, 1 page): Read-Write, Non-Executable, Device-nGnRnE
    aspace_ttbr0.map_page(uart_base, uart_base, PagePermissions::device_mmio())?;

    // 3. Build TTBR1 dormant higher-half space
    // Maps identical physical regions into higher-half virtual address space (-2 GiB kernel base)
    let mut aspace_ttbr1 = AddressSpace::new(&mut *pool1, max_pa)?;

    aspace_ttbr1.map_range(KERNEL_VA_BASE, KERNEL_IMAGE_PADDR, header_pages, PagePermissions::kernel_rodata())?;

    let mut mapped_pages_ttbr1 = header_pages;
    let mut cur_rva_ttbr1 = (header_pages as u64) * PAGE_SIZE;

    for sec_opt in &pe.sections[..pe.section_count] {
        if let Some(sec) = sec_opt {
            if sec.virtual_address > cur_rva_ttbr1 {
                let gap_pages = ((sec.virtual_address - cur_rva_ttbr1) / PAGE_SIZE) as usize;
                aspace_ttbr1.map_range(KERNEL_VA_BASE + cur_rva_ttbr1, KERNEL_IMAGE_PADDR + cur_rva_ttbr1, gap_pages, PagePermissions::kernel_data())?;
                mapped_pages_ttbr1 += gap_pages;
            }

            let perms = if sec.is_executable {
                PagePermissions::kernel_code()
            } else if sec.is_writable {
                PagePermissions::kernel_data()
            } else {
                PagePermissions::kernel_rodata()
            };

            aspace_ttbr1.map_range(KERNEL_VA_BASE + sec.virtual_address, KERNEL_IMAGE_PADDR + sec.virtual_address, sec.page_count, perms)?;
            mapped_pages_ttbr1 += sec.page_count;
            cur_rva_ttbr1 = sec.virtual_address + (sec.page_count as u64) * PAGE_SIZE;
        }
    }

    if mapped_pages_ttbr1 < total_kernel_pages {
        let remainder = total_kernel_pages - mapped_pages_ttbr1;
        aspace_ttbr1.map_range(KERNEL_VA_BASE + cur_rva_ttbr1, KERNEL_IMAGE_PADDR + cur_rva_ttbr1, remainder, PagePermissions::kernel_data())?;
    }

    // Map PL011 UART MMIO to higher-half MMIO space (0xFFFF_FFFF_F000_0000)
    aspace_ttbr1.map_page(MMIO_VA_BASE, uart_base, PagePermissions::device_mmio())?;

    // 3b. Higher-Half Direct Map (HHDM) in TTBR1:
    // Maps physical DRAM (0x4000_0000..0x5000_0000, 256 MiB) at HHDM_BASE (0xFFFF_8000_0000_0000).
    // Uses 2 MiB blocks for regions outside the 4 MiB kernel window, and 4 KiB pages inside
    // the kernel window (0x4800_0000..0x4840_0000) to strictly preserve W^X (executable .text
    // frames are mapped RO, NX in HHDM so zero writable aliases are created over executable code).
    let l2_block_size: u64 = 2 * 1024 * 1024;

    // Region 1: 0x4000_0000..0x4800_0000 (128 MiB = 64 blocks of 2 MiB)
    let blocks_before = (KERNEL_IMAGE_PADDR - DRAM_PADDR_START) / l2_block_size;
    for i in 0..blocks_before {
        let paddr = DRAM_PADDR_START + i * l2_block_size;
        let vaddr = HHDM_BASE + paddr;
        aspace_ttbr1.map_l2_block(vaddr, paddr, PagePermissions::kernel_data())?;
    }

    // Region 2: Kernel Window 0x4800_0000..0x4840_0000 (1024 pages of 4 KiB)
    aspace_ttbr1.map_range(HHDM_BASE + KERNEL_IMAGE_PADDR, KERNEL_IMAGE_PADDR, header_pages, PagePermissions::kernel_rodata())?;

    let mut mapped_pages_hhdm = header_pages;
    let mut cur_rva_hhdm = (header_pages as u64) * PAGE_SIZE;

    for sec_opt in &pe.sections[..pe.section_count] {
        if let Some(sec) = sec_opt {
            if sec.virtual_address > cur_rva_hhdm {
                let gap_pages = ((sec.virtual_address - cur_rva_hhdm) / PAGE_SIZE) as usize;
                aspace_ttbr1.map_range(HHDM_BASE + KERNEL_IMAGE_PADDR + cur_rva_hhdm, KERNEL_IMAGE_PADDR + cur_rva_hhdm, gap_pages, PagePermissions::kernel_data())?;
                mapped_pages_hhdm += gap_pages;
            }

            // Strict W^X: Executable sections are mapped RO, NX in HHDM!
            let perms = if sec.is_executable {
                PagePermissions::kernel_rodata()
            } else if sec.is_writable {
                PagePermissions::kernel_data()
            } else {
                PagePermissions::kernel_rodata()
            };

            aspace_ttbr1.map_range(HHDM_BASE + KERNEL_IMAGE_PADDR + sec.virtual_address, KERNEL_IMAGE_PADDR + sec.virtual_address, sec.page_count, perms)?;
            mapped_pages_hhdm += sec.page_count;
            cur_rva_hhdm = sec.virtual_address + (sec.page_count as u64) * PAGE_SIZE;
        }
    }

    if mapped_pages_hhdm < total_kernel_pages {
        let remainder = total_kernel_pages - mapped_pages_hhdm;
        aspace_ttbr1.map_range(HHDM_BASE + KERNEL_IMAGE_PADDR + cur_rva_hhdm, KERNEL_IMAGE_PADDR + cur_rva_hhdm, remainder, PagePermissions::kernel_data())?;
    }

    // Region 3: 0x4840_0000..0x5000_0000 (124 MiB = 62 blocks of 2 MiB)
    let blocks_after = (DRAM_PADDR_END - (KERNEL_IMAGE_PADDR + KERNEL_RESERVE_SIZE)) / l2_block_size;
    for i in 0..blocks_after {
        let paddr = (KERNEL_IMAGE_PADDR + KERNEL_RESERVE_SIZE) + i * l2_block_size;
        let vaddr = HHDM_BASE + paddr;
        aspace_ttbr1.map_l2_block(vaddr, paddr, PagePermissions::kernel_data())?;
    }

    // 4. Mathematical and Architectural System-Wide W^X Verification (including Physical Aliases)
    let mut tracker = PhysicalWxTracker::new();
    aspace_ttbr0.audit_wx_range(KERNEL_IMAGE_PADDR, 1024, &mut tracker)?;
    aspace_ttbr0.audit_wx_range(uart_base, 1, &mut tracker)?;
    aspace_ttbr1.audit_wx_range(KERNEL_VA_BASE, 1024, &mut tracker)?;
    aspace_ttbr1.audit_wx_range(MMIO_VA_BASE, 1, &mut tracker)?;
    aspace_ttbr1.audit_wx_range(HHDM_BASE + KERNEL_IMAGE_PADDR, 1024, &mut tracker)?;
    let unique_pages = tracker.count();

    // 5. Preflight Software Translation Checks
    // TTBR0 checks:
    let trans_pc = aspace_ttbr0.translate(current_pc)?;
    if trans_pc != current_pc {
        return Err(VmmError::MisalignedPhysicalAddress);
    }
    let trans_sp = aspace_ttbr0.translate(current_sp)?;
    if trans_sp != current_sp {
        return Err(VmmError::MisalignedPhysicalAddress);
    }
    let trans_uart = aspace_ttbr0.translate(uart_base)?;
    if trans_uart != uart_base {
        return Err(VmmError::MisalignedPhysicalAddress);
    }

    // TTBR1 checks:
    let kernel_offset = current_pc.checked_sub(KERNEL_IMAGE_PADDR).ok_or(VmmError::ArithmeticOverflow)?;
    let higher_half_pc = KERNEL_VA_BASE.checked_add(kernel_offset).ok_or(VmmError::ArithmeticOverflow)?;
    let trans_hh_pc = aspace_ttbr1.translate(higher_half_pc)?;
    if trans_hh_pc != current_pc {
        return Err(VmmError::MisalignedPhysicalAddress);
    }

    let trans_hh_uart = aspace_ttbr1.translate(MMIO_VA_BASE)?;
    if trans_hh_uart != uart_base {
        return Err(VmmError::MisalignedPhysicalAddress);
    }

    let trans_hhdm_start = aspace_ttbr1.translate(HHDM_BASE + DRAM_PADDR_START)?;
    if trans_hhdm_start != DRAM_PADDR_START {
        return Err(VmmError::MisalignedPhysicalAddress);
    }
    let trans_hhdm_kernel = aspace_ttbr1.translate(HHDM_BASE + KERNEL_IMAGE_PADDR)?;
    if trans_hhdm_kernel != KERNEL_IMAGE_PADDR {
        return Err(VmmError::MisalignedPhysicalAddress);
    }

    // Sample descriptors directly from constructed page tables
    let text_sec = pe.find_section(b".text").ok_or(VmmError::SectionNotFound)?;
    let rdata_sec = pe.find_section(b".rdata").unwrap_or(text_sec);
    let data_sec = pe.find_section(b".data").ok_or(VmmError::SectionNotFound)?;
    let reloc_sec = pe.find_section(b".reloc").unwrap_or(data_sec);

    let header_desc = aspace_ttbr0.get_descriptor(KERNEL_IMAGE_PADDR)?;
    let code_desc = aspace_ttbr0.get_descriptor(KERNEL_IMAGE_PADDR + text_sec.virtual_address)?;
    let rodata_desc = aspace_ttbr0.get_descriptor(KERNEL_IMAGE_PADDR + rdata_sec.virtual_address)?;
    let data_desc = aspace_ttbr0.get_descriptor(KERNEL_IMAGE_PADDR + data_sec.virtual_address)?;
    let stack_desc = aspace_ttbr0.get_descriptor(current_sp & !4095)?;
    let reloc_desc = aspace_ttbr0.get_descriptor(KERNEL_IMAGE_PADDR + reloc_sec.virtual_address)?;
    let uart_desc = aspace_ttbr0.get_descriptor(uart_base)?;

    let ttbr0_root = aspace_ttbr0.root_paddr();
    let ttbr1_root = aspace_ttbr1.root_paddr();
    let ttbr0_mapped_count = aspace_ttbr0.mapped_pages_count();
    let ttbr1_mapped_count = aspace_ttbr1.mapped_pages_count();

    drop(aspace_ttbr0);
    drop(aspace_ttbr1);

    // Track every allocated frame in both pools
    let mut ttbr0_frames = [0u64; DORMANT_POOL_FRAMES];
    let mut ttbr0_frame_count = 0usize;
    for (i, &alloc) in pool0.allocated.iter().enumerate() {
        if alloc {
            ttbr0_frames[ttbr0_frame_count] = pool0.base_paddr + (i as u64) * PAGE_SIZE;
            ttbr0_frame_count += 1;
        }
    }

    let mut ttbr1_frames = [0u64; DORMANT_POOL_FRAMES];
    let mut ttbr1_frame_count = 0usize;
    for (i, &alloc) in pool1.allocated.iter().enumerate() {
        if alloc {
            ttbr1_frames[ttbr1_frame_count] = pool1.base_paddr + (i as u64) * PAGE_SIZE;
            ttbr1_frame_count += 1;
        }
    }

    Ok(PreflightReport {
        cpu_features,
        ttbr0_root,
        ttbr1_root,
        ttbr0_mapped_count,
        ttbr1_mapped_count,
        ttbr0_frames,
        ttbr0_frame_count,
        ttbr1_frames,
        ttbr1_frame_count,
        unique_physical_pages_checked: unique_pages,
        header_desc,
        code_desc,
        rodata_desc,
        data_desc,
        stack_desc,
        reloc_desc,
        uart_desc,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dormant_table_pool_allocation_and_zeroing() {
        let mut pool = DormantTablePool::new();
        pool.init();

        let f0 = pool.allocate_frame().unwrap();
        assert_eq!(f0 % PAGE_SIZE, 0);
        let entries = unsafe { pool.frame_ref(f0) };
        assert!(entries.iter().all(|&e| e == 0));

        let f1 = pool.allocate_frame().unwrap();
        assert_eq!(f1, f0 + PAGE_SIZE);

        pool.deallocate_frame(f0);
        let f2 = pool.allocate_frame().unwrap();
        assert_eq!(f2, f0);
    }

    #[test]
    fn preflight_dormant_construction_and_translation() {
        let mut pool0 = DormantTablePool::new();
        pool0.init();

        let mut pool1 = DormantTablePool::new();
        pool1.init();

        // Valid ARMv8-A MMFR0: PARange=0b0101 (48 bits), TGran4=0b0000 (Supported)
        let mmfr0 = 0x0000_0000_0000_0005u64;
        let pc = 0x4800_2000u64;
        let sp = 0x4823_0000u64;
        let uart = 0x0900_0000u64;

        let pe = ParsedPeImage::mock_genesis();
        let report = run_stage3c1_preflight(mmfr0, pc, sp, uart, &mut pool0, &mut pool1, &pe).unwrap();

        assert_eq!(report.cpu_features.pa_range, crate::vmm::PaRange::Bits48);
        assert_eq!(report.ttbr0_root % PAGE_SIZE, 0);
        assert_eq!(report.ttbr1_root % PAGE_SIZE, 0);
        assert_eq!(report.ttbr0_mapped_count, 1025);
        assert_eq!(report.ttbr1_mapped_count, 2049);

        // Verify descriptor bits and W^X classification:
        // .text is RO/X
        assert_eq!(report.code_desc.ap, 0b10); // Kernel RO
        assert_eq!(report.code_desc.pxn, false); // Executable at EL1
        assert_eq!(report.code_desc.uxn, true); // NX at EL0
        assert_eq!(report.code_desc.attr_indx, 3); // Normal WB-WA
        assert!(!report.code_desc.is_writable());
        assert!(report.code_desc.is_executable());

        // .rdata is RO/NX
        assert_eq!(report.rodata_desc.ap, 0b10);
        assert_eq!(report.rodata_desc.pxn, true);
        assert!(!report.rodata_desc.is_writable());
        assert!(!report.rodata_desc.is_executable());

        // .data is RW/NX
        assert_eq!(report.data_desc.ap, 0b00); // Kernel RW
        assert_eq!(report.data_desc.pxn, true); // NX at EL1
        assert!(report.data_desc.is_writable());
        assert!(!report.data_desc.is_executable());

        // stack is RW/NX
        assert_eq!(report.stack_desc.ap, 0b00);
        assert_eq!(report.stack_desc.pxn, true);
        assert!(report.stack_desc.is_writable());
        assert!(!report.stack_desc.is_executable());

        // UART is RW/NX (Device)
        assert_eq!(report.uart_desc.ap, 0b00);
        assert_eq!(report.uart_desc.pxn, true);
        assert_eq!(report.uart_desc.attr_indx, 0); // Device-nGnRnE
        assert!(report.uart_desc.is_writable());
        assert!(!report.uart_desc.is_executable());

        // Verify allocated table frames
        assert_eq!(report.ttbr0_frame_count, 7);
        assert_eq!(report.ttbr1_frame_count, 11);
        assert_eq!(report.ttbr0_root, report.ttbr0_frames[0]);
        assert_eq!(report.ttbr1_root, report.ttbr1_frames[0]);
        assert_eq!(report.unique_physical_pages_checked, 1025);

        // Ensure all TTBR0 frames are strictly in pool0 and TTBR1 in pool1 with zero overlap
        for &f0 in &report.ttbr0_frames[..report.ttbr0_frame_count] {
            assert!(f0 >= pool0.base_paddr && f0 < pool0.base_paddr + (DORMANT_POOL_FRAMES as u64) * PAGE_SIZE);
            for &f1 in &report.ttbr1_frames[..report.ttbr1_frame_count] {
                assert_ne!(f0, f1, "Table frame overlap detected between TTBR0 and TTBR1!");
            }
        }
    }

    #[test]
    fn physical_wx_alias_detection_fails_on_conflict() {
        let mut tracker = PhysicalWxTracker::new();
        let target_pa = 0x4800_1000u64;

        // First mapping: Executable, non-writable (RO/X)
        assert!(tracker.record_mapping(target_pa, false, true).is_ok());

        // Second mapping: Non-executable, writable (RW/NX) for same physical page -> must fail!
        let res = tracker.record_mapping(target_pa, true, false);
        assert_eq!(res, Err(VmmError::WxPhysicalAliasViolation(target_pa)));
    }

    #[test]
    fn direct_wx_violation_fails() {
        let mut tracker = PhysicalWxTracker::new();
        let target_pa = 0x4800_2000u64;

        // Single mapping that is simultaneously writable and executable
        let res = tracker.record_mapping(target_pa, true, true);
        assert_eq!(res, Err(VmmError::WxDirectViolation(target_pa)));
    }

    #[test]
    fn pool_frame_bounds_and_capacity() {
        let mut pool = DormantTablePool::new();
        pool.init();

        let raw_ptr = pool.storage.as_ptr() as u64;
        assert_eq!(pool.base_paddr % PAGE_SIZE, 0);
        assert!(pool.base_paddr >= raw_ptr);
        assert!(pool.base_paddr + (DORMANT_POOL_FRAMES as u64) * PAGE_SIZE <= raw_ptr + (DORMANT_POOL_BYTES as u64));

        // Allocate all 8 frames
        let mut allocated = [0u64; DORMANT_POOL_FRAMES];
        for i in 0..DORMANT_POOL_FRAMES {
            allocated[i] = pool.allocate_frame().unwrap();
            assert_eq!(allocated[i] % PAGE_SIZE, 0);
        }

        // 9th allocation must fail
        assert_eq!(pool.allocate_frame(), None);
    }
}
