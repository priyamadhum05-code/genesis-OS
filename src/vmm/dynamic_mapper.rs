//! Dynamic Page-Table Mapper for Project Genesis.
//!
//! Provides runtime page-table mapping in TTBR1_EL1 backed by KERNEL_PMM,
//! zeroing newly allocated tables via HHDM, and issuing AArch64 context
//! synchronization and TLB invalidation barriers (dsb ishst + tlbi vaae1is + isb).

use crate::{
    pmm::PhysicalPageAllocator,
    vmm::page_table::{
        is_block, is_canonical_va, is_table, is_valid, l0_index, l1_index, l2_index, l3_index,
        make_l2_block_descriptor, make_page_descriptor, make_table_descriptor, output_address,
        PagePermissions, VmmError, L2_BLOCK_SIZE, PAGE_SIZE,
    },
};

pub struct DynamicMapper<'a, const N: usize> {
    root_paddr: u64,
    max_pa: u64,
    pmm: &'a mut PhysicalPageAllocator<N>,
    hhdm_base: u64,
}

impl<'a, const N: usize> DynamicMapper<'a, N> {
    pub fn new(
        root_paddr: u64,
        max_pa: u64,
        pmm: &'a mut PhysicalPageAllocator<N>,
        hhdm_base: u64,
    ) -> Self {
        Self {
            root_paddr,
            max_pa,
            pmm,
            hhdm_base,
        }
    }

    pub fn pmm_mut(&mut self) -> &mut PhysicalPageAllocator<N> {
        self.pmm
    }

    /// Maps a single 4 KiB virtual page `va` to physical page `pa` with `perms`.
    /// Traverses L0->L1->L2->L3, allocating and zeroing intermediate tables via HHDM
    /// as needed, and synchronizes the CPU translation regime.
    pub fn map_page(&mut self, va: u64, pa: u64, perms: PagePermissions) -> Result<(), VmmError> {
        if va % PAGE_SIZE != 0 {
            return Err(VmmError::MisalignedVirtualAddress);
        }
        if pa % PAGE_SIZE != 0 {
            return Err(VmmError::MisalignedPhysicalAddress);
        }
        if pa.checked_add(PAGE_SIZE).ok_or(VmmError::ArithmeticOverflow)? > self.max_pa {
            return Err(VmmError::PhysicalAddressOutOfRange);
        }

        // L0 traversal
        let l0_idx = l0_index(va);
        let l0_table = self.get_table_mut(self.root_paddr);
        let l0_entry = l0_table[l0_idx];
        let l1_paddr = if is_valid(l0_entry) {
            if !is_table(l0_entry) {
                return Err(VmmError::TableCollision);
            }
            output_address(l0_entry)
        } else {
            let frame = self.allocate_and_zero_frame()?;
            let l0_table = self.get_table_mut(self.root_paddr);
            l0_table[l0_idx] = make_table_descriptor(frame);
            frame
        };

        // L1 traversal
        let l1_idx = l1_index(va);
        let l1_table = self.get_table_mut(l1_paddr);
        let l1_entry = l1_table[l1_idx];
        let l2_paddr = if is_valid(l1_entry) {
            if is_block(l1_entry) {
                return Err(VmmError::BlockCollision);
            }
            if !is_table(l1_entry) {
                return Err(VmmError::TableCollision);
            }
            output_address(l1_entry)
        } else {
            let frame = self.allocate_and_zero_frame()?;
            let l1_table = self.get_table_mut(l1_paddr);
            l1_table[l1_idx] = make_table_descriptor(frame);
            frame
        };

        // L2 traversal
        let l2_idx = l2_index(va);
        let l2_table = self.get_table_mut(l2_paddr);
        let l2_entry = l2_table[l2_idx];
        let l3_paddr = if is_valid(l2_entry) {
            if is_block(l2_entry) {
                return Err(VmmError::BlockCollision);
            }
            if !is_table(l2_entry) {
                return Err(VmmError::TableCollision);
            }
            output_address(l2_entry)
        } else {
            let frame = self.allocate_and_zero_frame()?;
            let l2_table = self.get_table_mut(l2_paddr);
            l2_table[l2_idx] = make_table_descriptor(frame);
            frame
        };

        // L3 entry write
        let l3_idx = l3_index(va);
        let l3_table = self.get_table_mut(l3_paddr);
        let l3_entry = l3_table[l3_idx];
        if is_valid(l3_entry) {
            return Err(VmmError::AlreadyMapped);
        }
        l3_table[l3_idx] = make_page_descriptor(pa, perms);

        // AArch64 Architectural Barrier Sequence
        self.invalidate_tlb_page(va);

        Ok(())
    }

    /// Maps a device MMIO page (AttrIndx=0, Device-nGnRnE, RW, NX).
    pub fn map_mmio_page(&mut self, va: u64, pa: u64) -> Result<(), VmmError> {
        self.map_page(va, pa, PagePermissions::device_mmio())
    }

    /// Maps a contiguous range of pages with given permissions.
    pub fn map_range(
        &mut self,
        va_start: u64,
        pa_start: u64,
        page_count: usize,
        perms: PagePermissions,
    ) -> Result<(), VmmError> {
        for i in 0..page_count {
            let offset = (i as u64) * PAGE_SIZE;
            self.map_page(va_start + offset, pa_start + offset, perms)?;
        }
        Ok(())
    }

    /// Maps a contiguous range of device MMIO pages.
    pub fn map_mmio_range(&mut self, va_start: u64, pa_start: u64, page_count: usize) -> Result<(), VmmError> {
        self.map_range(va_start, pa_start, page_count, PagePermissions::device_mmio())
    }

    /// Maps a 2 MiB virtual block `va` to physical block `pa` with `perms`.
    pub fn map_l2_block(&mut self, va: u64, pa: u64, perms: PagePermissions) -> Result<(), VmmError> {
        if !is_canonical_va(va) {
            return Err(VmmError::NonCanonicalAddress);
        }
        if va % L2_BLOCK_SIZE != 0 {
            return Err(VmmError::MisalignedVirtualAddress);
        }
        if pa % L2_BLOCK_SIZE != 0 {
            return Err(VmmError::MisalignedPhysicalAddress);
        }
        if pa.checked_add(L2_BLOCK_SIZE).ok_or(VmmError::ArithmeticOverflow)? > self.max_pa {
            return Err(VmmError::PhysicalAddressOutOfRange);
        }

        // L0 traversal
        let l0_idx = l0_index(va);
        let l0_table = self.get_table_mut(self.root_paddr);
        let l0_entry = l0_table[l0_idx];
        let l1_paddr = if is_valid(l0_entry) {
            if !is_table(l0_entry) {
                return Err(VmmError::TableCollision);
            }
            output_address(l0_entry)
        } else {
            let frame = self.allocate_and_zero_frame()?;
            let l0_table = self.get_table_mut(self.root_paddr);
            l0_table[l0_idx] = make_table_descriptor(frame);
            frame
        };

        // L1 traversal
        let l1_idx = l1_index(va);
        let l1_table = self.get_table_mut(l1_paddr);
        let l1_entry = l1_table[l1_idx];
        let l2_paddr = if is_valid(l1_entry) {
            if is_block(l1_entry) {
                return Err(VmmError::BlockCollision);
            }
            if !is_table(l1_entry) {
                return Err(VmmError::TableCollision);
            }
            output_address(l1_entry)
        } else {
            let frame = self.allocate_and_zero_frame()?;
            let l1_table = self.get_table_mut(l1_paddr);
            l1_table[l1_idx] = make_table_descriptor(frame);
            frame
        };

        // L2 entry write (2 MiB block descriptor)
        let l2_idx = l2_index(va);
        let l2_table = self.get_table_mut(l2_paddr);
        let l2_entry = l2_table[l2_idx];
        if is_valid(l2_entry) {
            return Err(VmmError::AlreadyMapped);
        }
        l2_table[l2_idx] = make_l2_block_descriptor(pa, perms);

        self.invalidate_tlb_page(va);

        Ok(())
    }

    fn allocate_and_zero_frame(&mut self) -> Result<u64, VmmError> {
        let frame_paddr = self.pmm.allocate_page().ok_or(VmmError::FrameAllocationFailed)?;
        let table = self.get_table_mut(frame_paddr);
        table.fill(0);
        Ok(frame_paddr)
    }

    fn get_table_mut(&self, paddr: u64) -> &mut [u64; 512] {
        let ptr = (self.hhdm_base + paddr) as *mut [u64; 512];
        unsafe { &mut *ptr }
    }

    #[inline]
    fn invalidate_tlb_page(&self, va: u64) {
        #[cfg(target_os = "uefi")]
        unsafe {
            core::arch::asm!(
                "dsb ishst",
                "tlbi vaae1is, {0}",
                "dsb ish",
                "isb",
                in(reg) ((va >> 12) & 0x0FFF_FFFF_FFFF),
                options(nostack, preserves_flags)
            );
        }
        #[cfg(not(target_os = "uefi"))]
        let _ = va;
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::boxed::Box;

    fn make_test_map(base_paddr: u64, pages: u64) -> std::vec::Vec<u8> {
        let mut desc = std::vec![0u8; 40];
        desc[0..4].copy_from_slice(&7u32.to_le_bytes()); // ConventionalMemory = 7
        desc[8..16].copy_from_slice(&base_paddr.to_le_bytes());
        desc[16..24].copy_from_slice(&base_paddr.to_le_bytes());
        desc[24..32].copy_from_slice(&pages.to_le_bytes());
        desc
    }

    #[test]
    fn dynamic_mapper_alignment_error_distinction() {
        let mut pmm = PhysicalPageAllocator::<4>::new();
        let mut mapper = DynamicMapper::new(0, 0x10_0000, &mut pmm, 0);

        // Misaligned VA
        let res_va = mapper.map_page(0x1001, 0x2000, PagePermissions::kernel_data());
        assert_eq!(res_va, Err(VmmError::MisalignedVirtualAddress));

        // Misaligned PA
        let res_pa = mapper.map_page(0x2000, 0x1001, PagePermissions::kernel_data());
        assert_eq!(res_pa, Err(VmmError::MisalignedPhysicalAddress));
    }

    #[test]
    fn dynamic_mapper_l2_block_alignment_and_mapping() {
        #[repr(align(4096))]
        struct Mem([u8; 16 * 4096]);
        let mut mem = Box::new(Mem([0; 16 * 4096]));
        let base_ptr = mem.0.as_mut_ptr() as u64;

        let mut pmm = PhysicalPageAllocator::<2>::new();
        let map = make_test_map(0, 16);
        unsafe {
            pmm.init_from_memory_map(map.as_ptr(), map.len(), 40, &[]).unwrap();
        }

        let root_paddr = pmm.allocate_page().unwrap();
        let mut mapper = DynamicMapper::new(root_paddr, 0x1_0000_0000, &mut pmm, base_ptr);

        let va_block = 0xFFFF_FFFF_C000_0000u64;
        let pa_block = 0x4000_0000u64;

        // Misaligned L2 VA
        assert_eq!(
            mapper.map_l2_block(va_block + 4096, pa_block, PagePermissions::kernel_data()),
            Err(VmmError::MisalignedVirtualAddress)
        );

        // Misaligned L2 PA
        assert_eq!(
            mapper.map_l2_block(va_block, pa_block + 4096, PagePermissions::kernel_data()),
            Err(VmmError::MisalignedPhysicalAddress)
        );

        // Valid mapping
        assert!(mapper.map_l2_block(va_block, pa_block, PagePermissions::kernel_data()).is_ok());

        // Duplicate mapping -> AlreadyMapped
        assert_eq!(
            mapper.map_l2_block(va_block, pa_block, PagePermissions::kernel_data()),
            Err(VmmError::AlreadyMapped)
        );
    }
}
