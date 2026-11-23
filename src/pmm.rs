//! Physical Memory Manager (PMM) for Project Genesis.
//!
//! Tracks physical memory pages using a dual-bitmap architecture:
//! - `usable_bitmap`: Invariant mask indicating pages that are conventional DRAM
//!   and safely managed by Genesis (1 = usable conventional page, 0 = reserved/firmware/MMIO).
//! - `allocated_bitmap`: Allocation status of usable pages (1 = allocated or unavailable,
//!   0 = free and available for allocation).
//!
//! State invariants:
//! 1. A page can only be allocated if `usable == 1` and `allocated == 0`.
//! 2. A page can only be freed if `usable == 1` and `allocated == 1`.
//! 3. Firmware-owned, MMIO, ACPI, runtime-service, and Genesis-internal regions have `usable == 0`
//!    and `allocated == 1`, making them impossible to allocate and impossible to free.
//! 4. Range validation precedes all state modifications: if any page in a multi-page operation
//!    fails validation, zero state modifications occur (strict atomicity).

use crate::memory_map::{self, MapError, EFI_CONVENTIONAL_MEMORY, EFI_PAGE_SIZE};

/// Default capacity for kernel PMM: 4096 64-bit words tracks 262,144 pages (1 GiB span).
pub const KERNEL_PMM_WORDS: usize = 4096;
pub const KERNEL_PMM_MAX_PAGES: usize = KERNEL_PMM_WORDS * 64;

pub type ContigAllocFn = unsafe fn(usize) -> Option<u64>;
static mut GLOBAL_CONTIG_ALLOC: Option<ContigAllocFn> = None;

pub fn register_contig_alloc(f: ContigAllocFn) {
    unsafe {
        *core::ptr::addr_of_mut!(GLOBAL_CONTIG_ALLOC) = Some(f);
    }
}

pub unsafe fn allocate_contiguous_pages(count: usize) -> Option<u64> {
    if let Some(alloc_fn) = *core::ptr::addr_of!(GLOBAL_CONTIG_ALLOC) {
        alloc_fn(count)
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReservedRange {
    pub start: u64,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocError {
    Map(MapError),
    NoConventionalMemory,
    CapacityExceeded { required_pages: usize, capacity_pages: usize },
    MisalignedAddress,
    OutOfBounds,
    ProtectedRegion,
    DoubleFree,
    ArithmeticOverflow,
}

impl From<MapError> for AllocError {
    fn from(e: MapError) -> Self {
        AllocError::Map(e)
    }
}

pub struct PhysicalPageAllocator<const WORDS: usize> {
    base_paddr: u64,
    span_pages: usize,
    total_usable_pages: u64,
    free_pages: u64,
    usable_bitmap: [u64; WORDS],
    allocated_bitmap: [u64; WORDS],
}

impl<const WORDS: usize> PhysicalPageAllocator<WORDS> {
    pub const fn new() -> Self {
        Self {
            base_paddr: 0,
            span_pages: 0,
            total_usable_pages: 0,
            free_pages: 0,
            usable_bitmap: [0; WORDS],
            allocated_bitmap: [!0; WORDS],
        }
    }

    pub const fn max_pages(&self) -> usize {
        WORDS * 64
    }

    pub fn base_paddr(&self) -> u64 {
        self.base_paddr
    }

    pub fn span_pages(&self) -> usize {
        self.span_pages
    }

    pub fn total_usable_pages(&self) -> u64 {
        self.total_usable_pages
    }

    pub fn free_pages(&self) -> u64 {
        self.free_pages
    }

    /// Initializes the allocator by inspecting the validated UEFI memory map and excluding
    /// specified reserved regions (such as kernel image, stack, and firmware structures).
    ///
    /// # Safety
    /// `map` must point to `map_size` bytes of valid UEFI memory map data.
    pub unsafe fn init_from_memory_map(
        &mut self,
        map: *const u8,
        map_size: usize,
        descriptor_size: usize,
        reserved: &[ReservedRange],
    ) -> Result<(), AllocError> {
        let mut min_conv = u64::MAX;
        let mut max_conv = 0u64;
        let mut found_conv = false;

        memory_map::for_each_descriptor(map, map_size, descriptor_size, |desc| {
            if desc.kind == EFI_CONVENTIONAL_MEMORY {
                found_conv = true;
                if desc.physical_start < min_conv {
                    min_conv = desc.physical_start;
                }
                let region_bytes = desc
                    .page_count
                    .checked_mul(EFI_PAGE_SIZE)
                    .ok_or(MapError::PageRangeOverflow)?;
                let region_end = desc
                    .physical_start
                    .checked_add(region_bytes)
                    .ok_or(MapError::PageRangeOverflow)?;
                if region_end > max_conv {
                    max_conv = region_end;
                }
            }
            Ok(())
        })?;

        if !found_conv {
            return Err(AllocError::NoConventionalMemory);
        }

        let span_bytes = max_conv
            .checked_sub(min_conv)
            .ok_or(AllocError::ArithmeticOverflow)?;
        let span_pages_u64 = span_bytes / EFI_PAGE_SIZE;
        if span_pages_u64 > (usize::MAX as u64) {
            return Err(AllocError::ArithmeticOverflow);
        }
        let span_pages = span_pages_u64 as usize;
        let capacity_pages = WORDS * 64;
        if span_pages > capacity_pages {
            return Err(AllocError::CapacityExceeded {
                required_pages: span_pages,
                capacity_pages,
            });
        }

        self.base_paddr = min_conv;
        self.span_pages = span_pages;
        self.usable_bitmap.fill(0);
        self.allocated_bitmap.fill(!0u64);
        self.total_usable_pages = 0;
        self.free_pages = 0;

        // Populate conventional memory pages
        memory_map::for_each_descriptor(map, map_size, descriptor_size, |desc| {
            if desc.kind == EFI_CONVENTIONAL_MEMORY {
                let start_idx = ((desc.physical_start - self.base_paddr) / EFI_PAGE_SIZE) as usize;
                for i in 0..(desc.page_count as usize) {
                    let page_idx = start_idx + i;
                    let w = page_idx / 64;
                    let b = page_idx % 64;
                    self.usable_bitmap[w] |= 1u64 << b;
                    self.allocated_bitmap[w] &= !(1u64 << b);
                }
            }
            Ok(())
        })?;

        // Exclude explicitly reserved ranges (e.g. Genesis kernel image, stack, handoff)
        for range in reserved {
            if range.size == 0 {
                continue;
            }
            let range_end = range
                .start
                .checked_add(range.size)
                .ok_or(AllocError::ArithmeticOverflow)?;
            let aligned_start = range.start & !(EFI_PAGE_SIZE - 1);
            let aligned_end = (range_end
                .checked_add(EFI_PAGE_SIZE - 1)
                .ok_or(AllocError::ArithmeticOverflow)?)
                & !(EFI_PAGE_SIZE - 1);

            let span_end = self
                .base_paddr
                .checked_add(span_bytes)
                .ok_or(AllocError::ArithmeticOverflow)?;

            // Overlap check
            if aligned_end > self.base_paddr && aligned_start < span_end {
                let overlap_start = if aligned_start < self.base_paddr {
                    self.base_paddr
                } else {
                    aligned_start
                };
                let overlap_end = if aligned_end > span_end {
                    span_end
                } else {
                    aligned_end
                };

                let start_page = ((overlap_start - self.base_paddr) / EFI_PAGE_SIZE) as usize;
                let page_count = ((overlap_end - overlap_start) / EFI_PAGE_SIZE) as usize;

                for i in 0..page_count {
                    let page_idx = start_page + i;
                    let w = page_idx / 64;
                    let b = page_idx % 64;
                    self.usable_bitmap[w] &= !(1u64 << b);
                    self.allocated_bitmap[w] |= 1u64 << b;
                }
            }
        }

        // Count usable and free pages
        let mut count = 0u64;
        for page_idx in 0..self.span_pages {
            let w = page_idx / 64;
            let b = page_idx % 64;
            if (self.usable_bitmap[w] & (1u64 << b)) != 0
                && (self.allocated_bitmap[w] & (1u64 << b)) == 0
            {
                count += 1;
            }
        }
        self.total_usable_pages = count;
        self.free_pages = count;

        Ok(())
    }

    /// Allocates a single 4 KiB physical page.
    ///
    /// Search complexity:
    /// - Best-case: O(1) (free bit in first checked word)
    /// - Worst-case: O(W) word scans = O(N) where N is tracked page span.
    pub fn allocate_page(&mut self) -> Option<u64> {
        if self.free_pages == 0 {
            return None;
        }

        let words = (self.span_pages + 63) / 64;
        for w in 0..words {
            let val = self.allocated_bitmap[w];
            if val != !0u64 {
                let bit = (!val).trailing_zeros() as usize;
                let page_idx = w * 64 + bit;
                if page_idx < self.span_pages {
                    self.allocated_bitmap[w] |= 1u64 << bit;
                    self.free_pages -= 1;
                    return Some(self.base_paddr + (page_idx as u64) * EFI_PAGE_SIZE);
                }
            }
        }
        None
    }

    /// Allocates `count` contiguous 4 KiB physical pages.
    ///
    /// Search complexity:
    /// - Worst-case: O(N) linear run-length scan across tracked span.
    /// Atomicity: if no contiguous run of length `count` exists, zero bits are modified.
    pub fn allocate_contiguous_pages(&mut self, count: usize) -> Option<u64> {
        if count == 0 || (count as u64) > self.free_pages {
            return None;
        }
        if count == 1 {
            return self.allocate_page();
        }

        let mut run_start = 0usize;
        let mut current_run = 0usize;

        let words = (self.span_pages + 63) / 64;
        let mut word_idx = 0usize;

        while word_idx < words {
            let alloc_w = self.allocated_bitmap[word_idx];
            if alloc_w == !0u64 {
                current_run = 0;
                run_start = (word_idx + 1) * 64;
                word_idx += 1;
                continue;
            }

            for bit in 0..64 {
                let page_idx = word_idx * 64 + bit;
                if page_idx >= self.span_pages {
                    break;
                }

                if (alloc_w & (1u64 << bit)) == 0 {
                    if current_run == 0 {
                        run_start = page_idx;
                    }
                    current_run += 1;
                    if current_run == count {
                        // Mark all pages allocated
                        for p in run_start..(run_start + count) {
                            let pw = p / 64;
                            let pb = p % 64;
                            self.allocated_bitmap[pw] |= 1u64 << pb;
                        }
                        self.free_pages -= count as u64;
                        return Some(self.base_paddr + (run_start as u64) * EFI_PAGE_SIZE);
                    }
                } else {
                    current_run = 0;
                    run_start = page_idx + 1;
                }
            }
            word_idx += 1;
        }

        None
    }

    /// Allocates `count` contiguous 4 KiB physical pages aligned to `align_pages` page boundary.
    ///
    /// For example, `count = 512, align_pages = 512` allocates a 2 MiB-aligned physical block.
    pub fn allocate_aligned_pages(&mut self, count: usize, align_pages: usize) -> Option<u64> {
        if count == 0 || align_pages == 0 || (count as u64) > self.free_pages {
            return None;
        }

        let align_bytes = (align_pages as u64).checked_mul(EFI_PAGE_SIZE)?;
        let first_aligned = (self.base_paddr + align_bytes - 1) & !(align_bytes - 1);
        let mut cand_paddr = first_aligned;
        let span_end = self.base_paddr + (self.span_pages as u64) * EFI_PAGE_SIZE;

        while cand_paddr + (count as u64) * EFI_PAGE_SIZE <= span_end {
            let start_page = ((cand_paddr - self.base_paddr) / EFI_PAGE_SIZE) as usize;
            let mut all_free = true;
            for i in 0..count {
                let page_idx = start_page + i;
                let w = page_idx / 64;
                let b = page_idx % 64;
                if (self.allocated_bitmap[w] & (1u64 << b)) != 0 {
                    all_free = false;
                    break;
                }
            }

            if all_free {
                for p in start_page..(start_page + count) {
                    let pw = p / 64;
                    let pb = p % 64;
                    self.allocated_bitmap[pw] |= 1u64 << pb;
                }
                self.free_pages -= count as u64;
                return Some(cand_paddr);
            }

            cand_paddr = cand_paddr.checked_add(align_bytes)?;
        }

        None
    }

    /// Frees a previously allocated 4 KiB physical page.
    ///
    /// Validates address alignment, span bounds, usability invariant, and double-free.
    pub fn free_page(&mut self, paddr: u64) -> Result<(), AllocError> {
        self.free_contiguous_pages(paddr, 1)
    }

    /// Frees `count` contiguous 4 KiB physical pages.
    ///
    /// Validates the ENTIRE range before modifying any state.
    /// If any page is misaligned, out of bounds, not usable (protected/firmware),
    /// or already free (double free), zero state changes occur.
    pub fn free_contiguous_pages(&mut self, paddr: u64, count: usize) -> Result<(), AllocError> {
        if count == 0 {
            return Ok(());
        }
        if paddr % EFI_PAGE_SIZE != 0 {
            return Err(AllocError::MisalignedAddress);
        }
        if paddr < self.base_paddr {
            return Err(AllocError::OutOfBounds);
        }

        let byte_count = (count as u64)
            .checked_mul(EFI_PAGE_SIZE)
            .ok_or(AllocError::ArithmeticOverflow)?;
        let end_paddr = paddr
            .checked_add(byte_count)
            .ok_or(AllocError::ArithmeticOverflow)?;

        let span_end = self
            .base_paddr
            .checked_add((self.span_pages as u64) * EFI_PAGE_SIZE)
            .ok_or(AllocError::ArithmeticOverflow)?;
        if end_paddr > span_end {
            return Err(AllocError::OutOfBounds);
        }

        let start_page = ((paddr - self.base_paddr) / EFI_PAGE_SIZE) as usize;

        // Step 1: Complete range validation (Atomicity check)
        for i in 0..count {
            let page_idx = start_page + i;
            let w = page_idx / 64;
            let b = page_idx % 64;

            // Must be a usable conventional page
            if (self.usable_bitmap[w] & (1u64 << b)) == 0 {
                return Err(AllocError::ProtectedRegion);
            }
            // Must currently be allocated
            if (self.allocated_bitmap[w] & (1u64 << b)) == 0 {
                return Err(AllocError::DoubleFree);
            }
        }

        // Step 2: Apply deallocations
        for i in 0..count {
            let page_idx = start_page + i;
            let w = page_idx / 64;
            let b = page_idx % 64;
            self.allocated_bitmap[w] &= !(1u64 << b);
        }
        self.free_pages += count as u64;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::vec::Vec;

    const CONVENTIONAL: u32 = 7;
    const RESERVED: u32 = 0;
    const PREFIX: usize = 40;

    fn descriptor(kind: u32, physical_start: u64, pages: u64, stride: usize) -> Vec<u8> {
        let mut bytes = std::vec![0xa5; stride];
        bytes[0..4].copy_from_slice(&kind.to_le_bytes());
        bytes[8..16].copy_from_slice(&physical_start.to_le_bytes());
        bytes[24..32].copy_from_slice(&pages.to_le_bytes());
        bytes
    }

    #[test]
    fn rejects_out_of_bounds_and_capacity_exceeded() {
        // Allocator with 2 words capacity = 128 pages
        let mut pmm = PhysicalPageAllocator::<2>::new();
        // Memory map with 129 pages
        let map = descriptor(CONVENTIONAL, 0x4000_0000, 129, PREFIX);
        let res = unsafe { pmm.init_from_memory_map(map.as_ptr(), map.len(), PREFIX, &[]) };
        assert_eq!(
            res,
            Err(AllocError::CapacityExceeded {
                required_pages: 129,
                capacity_pages: 128
            })
        );
    }

    #[test]
    fn tracks_conventional_memory_and_excludes_reserved() {
        let mut pmm = PhysicalPageAllocator::<4>::new(); // 256 pages capacity
        let mut map = descriptor(CONVENTIONAL, 0x4000_0000, 10, PREFIX);
        map.extend(descriptor(RESERVED, 0x4000_A000, 5, PREFIX)); // gap
        map.extend(descriptor(CONVENTIONAL, 0x4000_F000, 10, PREFIX));

        // Reserve 2 pages in the middle of first conventional chunk: [0x4000_2000, 0x4000_4000)
        let reserved = [ReservedRange {
            start: 0x4000_2000,
            size: 2 * 4096,
        }];

        unsafe {
            pmm.init_from_memory_map(map.as_ptr(), map.len(), PREFIX, &reserved)
                .unwrap();
        }

        // Conventional total was 20, minus 2 reserved = 18 usable pages
        assert_eq!(pmm.total_usable_pages(), 18);
        assert_eq!(pmm.free_pages(), 18);
        assert_eq!(pmm.base_paddr(), 0x4000_0000);
        // Span covers 0x4000_0000 to 0x4001_9000 (25 pages)
        assert_eq!(pmm.span_pages(), 25);
    }

    #[test]
    fn single_page_alloc_and_free() {
        let mut pmm = PhysicalPageAllocator::<2>::new();
        let map = descriptor(CONVENTIONAL, 0x1000, 5, PREFIX);
        unsafe {
            pmm.init_from_memory_map(map.as_ptr(), map.len(), PREFIX, &[])
                .unwrap();
        }

        let p1 = pmm.allocate_page().expect("alloc p1");
        assert_eq!(p1, 0x1000);
        assert_eq!(pmm.free_pages(), 4);

        let p2 = pmm.allocate_page().expect("alloc p2");
        assert_eq!(p2, 0x2000);
        assert_eq!(pmm.free_pages(), 3);

        pmm.free_page(p1).expect("free p1");
        assert_eq!(pmm.free_pages(), 4);

        // Reallocation can reuse p1
        let p3 = pmm.allocate_page().expect("alloc p3");
        assert_eq!(p3, 0x1000);
        assert_eq!(pmm.free_pages(), 3);
    }

    #[test]
    fn contiguous_alloc_and_free_across_word_boundary() {
        let mut pmm = PhysicalPageAllocator::<4>::new(); // 256 pages
        let map = descriptor(CONVENTIONAL, 0x10000, 150, PREFIX);
        unsafe {
            pmm.init_from_memory_map(map.as_ptr(), map.len(), PREFIX, &[])
                .unwrap();
        }

        // Allocate 70 contiguous pages (spans word 0 and word 1)
        let block = pmm.allocate_contiguous_pages(70).expect("alloc 70");
        assert_eq!(block, 0x10000);
        assert_eq!(pmm.free_pages(), 80);

        // Free the 70 contiguous pages
        pmm.free_contiguous_pages(block, 70).expect("free 70");
        assert_eq!(pmm.free_pages(), 150);
    }

    #[test]
    fn rejects_double_free() {
        let mut pmm = PhysicalPageAllocator::<2>::new();
        let map = descriptor(CONVENTIONAL, 0x1000, 4, PREFIX);
        unsafe {
            pmm.init_from_memory_map(map.as_ptr(), map.len(), PREFIX, &[])
                .unwrap();
        }

        let p = pmm.allocate_page().unwrap();
        pmm.free_page(p).unwrap();
        assert_eq!(pmm.free_page(p), Err(AllocError::DoubleFree));
    }

    #[test]
    fn rejects_free_of_protected_or_untracked_pages() {
        let mut pmm = PhysicalPageAllocator::<2>::new();
        let mut map = descriptor(CONVENTIONAL, 0x4000, 2, PREFIX);
        map.extend(descriptor(RESERVED, 0x6000, 2, PREFIX));
        map.extend(descriptor(CONVENTIONAL, 0x8000, 2, PREFIX));

        unsafe {
            pmm.init_from_memory_map(map.as_ptr(), map.len(), PREFIX, &[])
                .unwrap();
        }

        // 0x6000 is within the span but is RESERVED (usable_bitmap == 0)
        assert_eq!(pmm.free_page(0x6000), Err(AllocError::ProtectedRegion));

        // 0x1000 is below base_paddr
        assert_eq!(pmm.free_page(0x1000), Err(AllocError::OutOfBounds));

        // 0x20000 is above span
        assert_eq!(pmm.free_page(0x20000), Err(AllocError::OutOfBounds));
    }

    #[test]
    fn rejects_misaligned_free_address() {
        let mut pmm = PhysicalPageAllocator::<2>::new();
        let map = descriptor(CONVENTIONAL, 0x1000, 4, PREFIX);
        unsafe {
            pmm.init_from_memory_map(map.as_ptr(), map.len(), PREFIX, &[])
                .unwrap();
        }

        assert_eq!(pmm.free_page(0x1001), Err(AllocError::MisalignedAddress));
    }

    #[test]
    fn atomic_contiguous_free_preserves_state_on_partial_failure() {
        let mut pmm = PhysicalPageAllocator::<2>::new();
        let map = descriptor(CONVENTIONAL, 0x1000, 10, PREFIX);
        unsafe {
            pmm.init_from_memory_map(map.as_ptr(), map.len(), PREFIX, &[])
                .unwrap();
        }

        let _p = pmm.allocate_contiguous_pages(4).unwrap(); // 0x1000..0x5000
        assert_eq!(pmm.free_pages(), 6);

        // Free only page 1 (0x2000)
        pmm.free_page(0x2000).unwrap();
        assert_eq!(pmm.free_pages(), 7);

        // Now attempt to free contiguous [0x1000, 4 pages).
        // 0x2000 is already free, so the contiguous free must fail with DoubleFree.
        let res = pmm.free_contiguous_pages(0x1000, 4);
        assert_eq!(res, Err(AllocError::DoubleFree));

        // Atomicity verification: free_pages must remain exactly 7!
        // 0x1000 must still be allocated!
        assert_eq!(pmm.free_pages(), 7);
        // Freeing 0x1000 individually must succeed because it was NOT freed
        pmm.free_page(0x1000).expect("0x1000 should still be allocated");
        assert_eq!(pmm.free_pages(), 8);
    }

    #[test]
    fn exhaustion_and_reallocation() {
        let mut pmm = PhysicalPageAllocator::<1>::new(); // 64 pages capacity
        let map = descriptor(CONVENTIONAL, 0x1000, 3, PREFIX);
        unsafe {
            pmm.init_from_memory_map(map.as_ptr(), map.len(), PREFIX, &[])
                .unwrap();
        }

        let a = pmm.allocate_page().unwrap();
        let b = pmm.allocate_page().unwrap();
        let c = pmm.allocate_page().unwrap();
        assert_eq!(pmm.free_pages(), 0);

        // Next allocation fails
        assert_eq!(pmm.allocate_page(), None);
        assert_eq!(pmm.allocate_contiguous_pages(1), None);

        // Free middle page
        pmm.free_page(b).unwrap();
        assert_eq!(pmm.free_pages(), 1);

        // Reallocate succeeds
        let d = pmm.allocate_page().unwrap();
        assert_eq!(d, b);
        assert_eq!(pmm.free_pages(), 0);

        // Free all
        pmm.free_page(a).unwrap();
        pmm.free_page(c).unwrap();
        pmm.free_page(d).unwrap();
        assert_eq!(pmm.free_pages(), 3);
    }

    #[test]
    fn allocate_aligned_pages_test() {
        let mut pmm = PhysicalPageAllocator::<16>::new(); // 1024 pages
        let map = descriptor(CONVENTIONAL, 0x1000, 1024, PREFIX);
        unsafe {
            pmm.init_from_memory_map(map.as_ptr(), map.len(), PREFIX, &[])
                .unwrap();
        }

        // Request 512 pages aligned to 512 pages (2 MiB)
        let block = pmm.allocate_aligned_pages(512, 512).expect("should find 2 MiB aligned block");
        assert_eq!(block % (512 * 4096), 0);
        assert_eq!(pmm.free_pages(), 1024 - 512);

        // Cannot allocate another 512 pages because only 512 pages remain and they are not aligned to 2 MiB
        // or let's free and reallocate
        pmm.free_contiguous_pages(block, 512).unwrap();
        assert_eq!(pmm.free_pages(), 1024);
    }
}
