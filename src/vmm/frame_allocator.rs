//! Concrete PMM-backed translation-table frame allocator.
//!
//! Enforces:
//! 1. Frame zeroing: every allocated 4 KiB frame is zeroed before use.
//! 2. Decoupled physical accessibility: relies on `PhysAccess` to validate accessibility.
//!    If accessibility cannot be established, the PMM allocation is rolled back and rejected.
//! 3. Ownership and lifecycle rules: tracks allocated table frames in a bounded list.
//!    Only frames owned by this allocator can be deallocated or accessed.
//! 4. Frame leak tracking: tracks active frame count for complete verification.

use crate::pmm::PhysicalPageAllocator;
use crate::vmm::page_table::{FrameAllocator, PAGE_SIZE, TABLE_ENTRY_COUNT, VmmError};

pub const MAX_PAGE_TABLE_FRAMES: usize = 512;

/// Trait to safely establish and translate physical address accessibility.
pub trait PhysAccess {
    /// Establishes accessibility of the physical frame at `paddr` and returns a mutable pointer.
    ///
    /// # Safety
    /// Returns `Err` if the address has not been established as accessible.
    unsafe fn phys_to_virt(&self, paddr: u64) -> Result<*mut [u64; TABLE_ENTRY_COUNT], VmmError>;
}

/// Concrete identity-mapped physical access validator.
///
/// Verifies that physical addresses reside strictly within established accessible DRAM boundaries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IdentityPhysAccess {
    accessible_base: u64,
    accessible_limit: u64,
}

impl IdentityPhysAccess {
    pub const fn new(accessible_base: u64, accessible_limit: u64) -> Result<Self, VmmError> {
        if accessible_base % PAGE_SIZE != 0 || accessible_limit % PAGE_SIZE != 0 {
            return Err(VmmError::MisalignedPhysicalAddress);
        }
        if accessible_base >= accessible_limit {
            return Err(VmmError::InaccessiblePhysicalAddress);
        }
        Ok(Self {
            accessible_base,
            accessible_limit,
        })
    }

    pub fn accessible_base(&self) -> u64 {
        self.accessible_base
    }

    pub fn accessible_limit(&self) -> u64 {
        self.accessible_limit
    }
}

impl PhysAccess for IdentityPhysAccess {
    unsafe fn phys_to_virt(&self, paddr: u64) -> Result<*mut [u64; TABLE_ENTRY_COUNT], VmmError> {
        if paddr % PAGE_SIZE != 0 {
            return Err(VmmError::MisalignedPhysicalAddress);
        }
        let end = paddr
            .checked_add(PAGE_SIZE)
            .ok_or(VmmError::ArithmeticOverflow)?;
        if paddr < self.accessible_base || end > self.accessible_limit {
            return Err(VmmError::InaccessiblePhysicalAddress);
        }
        Ok(paddr as *mut [u64; TABLE_ENTRY_COUNT])
    }
}

/// FrameAllocator implementation backed directly by `PhysicalPageAllocator`.
pub struct PmmFrameAllocator<'a, const WORDS: usize, A: PhysAccess> {
    pmm: &'a mut PhysicalPageAllocator<WORDS>,
    phys_access: A,
    allocated_frames: [u64; MAX_PAGE_TABLE_FRAMES],
    allocated_count: usize,
}

impl<'a, const WORDS: usize, A: PhysAccess> PmmFrameAllocator<'a, WORDS, A> {
    pub fn new(pmm: &'a mut PhysicalPageAllocator<WORDS>, phys_access: A) -> Self {
        Self {
            pmm,
            phys_access,
            allocated_frames: [0; MAX_PAGE_TABLE_FRAMES],
            allocated_count: 0,
        }
    }

    pub fn allocated_count(&self) -> usize {
        self.allocated_count
    }

    pub fn is_frame_owned(&self, paddr: u64) -> bool {
        let mut i = 0;
        while i < self.allocated_count {
            if self.allocated_frames[i] == paddr {
                return true;
            }
            i += 1;
        }
        false
    }
}

impl<'a, const WORDS: usize, A: PhysAccess> FrameAllocator for PmmFrameAllocator<'a, WORDS, A> {
    fn allocate_frame(&mut self) -> Option<u64> {
        if self.allocated_count >= MAX_PAGE_TABLE_FRAMES {
            return None;
        }

        let paddr = self.pmm.allocate_page()?;

        // Verify physical frame accessibility before touching it
        let ptr = match unsafe { self.phys_access.phys_to_virt(paddr) } {
            Ok(p) => p,
            Err(_) => {
                // Rollback PMM allocation if frame is not established as accessible
                self.pmm.free_page(paddr).ok();
                return None;
            }
        };

        // Requirement 2: Zero every allocated page-table frame before use
        unsafe {
            (*ptr).fill(0);
        }

        // Requirement 4: Record frame ownership
        self.allocated_frames[self.allocated_count] = paddr;
        self.allocated_count += 1;

        Some(paddr)
    }

    fn deallocate_frame(&mut self, paddr: u64) {
        // Requirement 4: Ownership validation
        let mut found_idx = None;
        let mut i = 0;
        while i < self.allocated_count {
            if self.allocated_frames[i] == paddr {
                found_idx = Some(i);
                break;
            }
            i += 1;
        }

        if let Some(idx) = found_idx {
            // Swap with last element and decrement count
            self.allocated_frames[idx] = self.allocated_frames[self.allocated_count - 1];
            self.allocated_frames[self.allocated_count - 1] = 0;
            self.allocated_count -= 1;

            // Return frame to PMM
            self.pmm.free_page(paddr).expect("PMM frame deallocate");
        } else {
            panic!("Attempted to deallocate unowned frame");
        }
    }

    unsafe fn frame_mut(&mut self, paddr: u64) -> &mut [u64; TABLE_ENTRY_COUNT] {
        assert!(self.is_frame_owned(paddr), "Frame access violation: unowned frame");
        let ptr = self.phys_access.phys_to_virt(paddr).expect("Accessible frame");
        &mut *ptr
    }

    unsafe fn frame_ref(&self, paddr: u64) -> &[u64; TABLE_ENTRY_COUNT] {
        assert!(self.is_frame_owned(paddr), "Frame access violation: unowned frame");
        let ptr = self.phys_access.phys_to_virt(paddr).expect("Accessible frame");
        &*ptr
    }
}
