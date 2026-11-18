//! AddressSpace abstraction representing a complete virtual address space.
//!
//! Provides mapping construction, range mapping with atomic rollback on partial failure,
//! and mapping lifecycle tracking without modifying active MMU state.

use crate::vmm::page_table::{
    FrameAllocator, PagePermissions, PageTable, VmmError, PAGE_SIZE,
};

pub struct AddressSpace<F: FrameAllocator> {
    page_table: PageTable,
    allocator: F,
    mapped_pages_count: usize,
}

impl<F: FrameAllocator> AddressSpace<F> {
    /// Constructs a new AddressSpace by allocating and zeroing the root L0 table.
    pub fn new(mut allocator: F, max_pa: u64) -> Result<Self, VmmError> {
        let root_paddr = allocator
            .allocate_frame()
            .ok_or(VmmError::FrameAllocationFailed)?;
        Ok(Self {
            page_table: PageTable::new(root_paddr, max_pa),
            allocator,
            mapped_pages_count: 0,
        })
    }

    pub fn root_paddr(&self) -> u64 {
        self.page_table.root_paddr()
    }

    pub fn mapped_pages_count(&self) -> usize {
        self.mapped_pages_count
    }

    pub fn allocator(&self) -> &F {
        &self.allocator
    }

    pub fn allocator_mut(&mut self) -> &mut F {
        &mut self.allocator
    }

    pub fn into_allocator(self) -> F {
        self.allocator
    }

    /// Maps a single 4 KiB virtual page to a physical page.
    pub fn map_page(
        &mut self,
        va: u64,
        pa: u64,
        perms: PagePermissions,
    ) -> Result<(), VmmError> {
        self.page_table.map_page(va, pa, perms, &mut self.allocator)?;
        self.mapped_pages_count += 1;
        Ok(())
    }

    /// Unmaps a single 4 KiB virtual page, automatically pruning empty intermediate tables.
    pub fn unmap_page(&mut self, va: u64) -> Result<u64, VmmError> {
        let pa = self.page_table.unmap_page(va, &mut self.allocator)?;
        self.mapped_pages_count -= 1;
        Ok(pa)
    }

    /// Maps a contiguous range of `count` 4 KiB pages with atomic rollback on failure.
    pub fn map_range(
        &mut self,
        va_start: u64,
        pa_start: u64,
        count: usize,
        perms: PagePermissions,
    ) -> Result<(), VmmError> {
        for i in 0..count {
            let offset = (i as u64)
                .checked_mul(PAGE_SIZE)
                .ok_or(VmmError::ArithmeticOverflow)?;
            let va = va_start
                .checked_add(offset)
                .ok_or(VmmError::ArithmeticOverflow)?;
            let pa = pa_start
                .checked_add(offset)
                .ok_or(VmmError::ArithmeticOverflow)?;

            if let Err(e) = self.map_page(va, pa, perms) {
                // Rollback all previously mapped pages in this range call
                for j in 0..i {
                    let undo_offset = (j as u64) * PAGE_SIZE;
                    let undo_va = va_start + undo_offset;
                    self.unmap_page(undo_va).ok();
                }
                return Err(e);
            }
        }
        Ok(())
    }

    /// Unmaps a contiguous range of `count` 4 KiB pages.
    pub fn unmap_range(&mut self, va_start: u64, count: usize) -> Result<(), VmmError> {
        for i in 0..count {
            let offset = (i as u64)
                .checked_mul(PAGE_SIZE)
                .ok_or(VmmError::ArithmeticOverflow)?;
            let va = va_start
                .checked_add(offset)
                .ok_or(VmmError::ArithmeticOverflow)?;
            self.unmap_page(va)?;
        }
        Ok(())
    }

    /// Maps a 2 MiB block at Level 2.
    pub fn map_l2_block(
        &mut self,
        va: u64,
        pa: u64,
        perms: PagePermissions,
    ) -> Result<(), VmmError> {
        self.page_table.map_l2_block(va, pa, perms, &mut self.allocator)
    }

    /// Translates a virtual address to physical address using the page table.
    pub fn translate(&self, va: u64) -> Result<u64, VmmError> {
        self.page_table.translate(va, &self.allocator)
    }

    /// Queries the translation table descriptor for `va`.
    pub fn get_descriptor(&self, va: u64) -> Result<crate::vmm::page_table::DescriptorInfo, VmmError> {
        self.page_table.get_descriptor(va, &self.allocator)
    }

    /// Validates W^X across every mapped 4 KiB page in the given range.
    /// Returns Ok(()) if no mapped page is simultaneously writable and executable.
    pub fn verify_wx_range(&self, va_start: u64, page_count: usize) -> Result<(), VmmError> {
        for i in 0..page_count {
            let va = va_start + (i as u64) * PAGE_SIZE;
            if let Ok(desc) = self.get_descriptor(va) {
                if desc.is_writable() && desc.is_executable() {
                    return Err(VmmError::WxDirectViolation(desc.output_paddr));
                }
            }
        }
        Ok(())
    }

    /// Audits a virtual address range into the physical W^X tracker.
    pub fn audit_wx_range(
        &self,
        va_start: u64,
        page_count: usize,
        tracker: &mut crate::vmm::page_table::PhysicalWxTracker,
    ) -> Result<(), VmmError> {
        for i in 0..page_count {
            let va = va_start
                .checked_add((i as u64) * PAGE_SIZE)
                .ok_or(VmmError::ArithmeticOverflow)?;
            let desc = self.get_descriptor(va)?;
            let paddr = self.translate(va)?;
            tracker.record_mapping(paddr, desc.is_writable(), desc.is_executable())?;
        }
        Ok(())
    }
}
