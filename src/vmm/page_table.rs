//! AArch64 4 KiB Translation-Table Core Engine.
//!
//! Implements descriptor construction, L0–L3 indexing for 48-bit Virtual Addressing,
//! block and page descriptor validation, permission representation, and frame-allocator
//! decoupled mapping/unmapping with rollback on failure and automatic intermediate table pruning.

pub const PAGE_SIZE: u64 = 4096;
pub const L2_BLOCK_SIZE: u64 = 2 * 1024 * 1024; // 2 MiB
pub const L1_BLOCK_SIZE: u64 = 1024 * 1024 * 1024; // 1 GiB
pub const TABLE_ENTRY_COUNT: usize = 512;

/// Abstract interface for physical frame allocation and access.
///
/// Decouples translation-table management from concrete memory regimes.
/// The caller is responsible for providing physical-to-virtual access,
/// allowing host unit testing via mock memory, identity-mapped boot testing,
/// and future direct-physical-mapped higher-half execution.
pub trait FrameAllocator {
    /// Allocates one zeroed 4 KiB physical frame for a translation table.
    fn allocate_frame(&mut self) -> Option<u64>;

    /// Deallocates a previously allocated 4 KiB translation-table frame.
    fn deallocate_frame(&mut self, paddr: u64);

    /// Provides mutable access to the 512 64-bit entries of the frame at `paddr`.
    ///
    /// # Safety
    /// `paddr` must be an allocated 4 KiB-aligned translation table frame.
    unsafe fn frame_mut(&mut self, paddr: u64) -> &mut [u64; TABLE_ENTRY_COUNT];

    /// Provides read-only access to the 512 64-bit entries of the frame at `paddr`.
    ///
    /// # Safety
    /// `paddr` must be an allocated 4 KiB-aligned translation table frame.
    unsafe fn frame_ref(&self, paddr: u64) -> &[u64; TABLE_ENTRY_COUNT];
}

impl<'a, A: FrameAllocator> FrameAllocator for &'a mut A {
    fn allocate_frame(&mut self) -> Option<u64> {
        (**self).allocate_frame()
    }

    fn deallocate_frame(&mut self, paddr: u64) {
        (**self).deallocate_frame(paddr)
    }

    unsafe fn frame_mut(&mut self, paddr: u64) -> &mut [u64; TABLE_ENTRY_COUNT] {
        (**self).frame_mut(paddr)
    }

    unsafe fn frame_ref(&self, paddr: u64) -> &[u64; TABLE_ENTRY_COUNT] {
        (**self).frame_ref(paddr)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryType {
    NormalWriteBack,    // MAIR AttrIndx 0
    DeviceNgNre,        // MAIR AttrIndx 1
    NormalNonCacheable, // MAIR AttrIndx 2
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessPermission {
    KernelRw, // AP = 0b00
    KernelRo, // AP = 0b10
    UserRw,   // AP = 0b01
    UserRo,   // AP = 0b11
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PagePermissions {
    pub mem_type: MemoryType,
    pub access: AccessPermission,
    pub privileged_execute: bool,   // PXN = !privileged_execute
    pub unprivileged_execute: bool, // UXN = !unprivileged_execute
}

impl PagePermissions {
    pub const fn kernel_code() -> Self {
        Self {
            mem_type: MemoryType::NormalWriteBack,
            access: AccessPermission::KernelRo,
            privileged_execute: true,
            unprivileged_execute: false,
        }
    }

    pub const fn kernel_rodata() -> Self {
        Self {
            mem_type: MemoryType::NormalWriteBack,
            access: AccessPermission::KernelRo,
            privileged_execute: false,
            unprivileged_execute: false,
        }
    }

    pub const fn kernel_data() -> Self {
        Self {
            mem_type: MemoryType::NormalWriteBack,
            access: AccessPermission::KernelRw,
            privileged_execute: false,
            unprivileged_execute: false,
        }
    }

    pub const fn device_mmio() -> Self {
        Self {
            mem_type: MemoryType::DeviceNgNre,
            access: AccessPermission::KernelRw,
            privileged_execute: false,
            unprivileged_execute: false,
        }
    }

    pub const fn framebuffer() -> Self {
        Self {
            mem_type: MemoryType::NormalNonCacheable,
            access: AccessPermission::KernelRw,
            privileged_execute: false,
            unprivileged_execute: false,
        }
    }

    pub const fn user_code() -> Self {
        Self {
            mem_type: MemoryType::NormalWriteBack,
            access: AccessPermission::UserRo,
            privileged_execute: false,
            unprivileged_execute: true,
        }
    }

    pub const fn user_rodata() -> Self {
        Self {
            mem_type: MemoryType::NormalWriteBack,
            access: AccessPermission::UserRo,
            privileged_execute: false,
            unprivileged_execute: false,
        }
    }

    pub const fn user_data() -> Self {
        Self {
            mem_type: MemoryType::NormalWriteBack,
            access: AccessPermission::UserRw,
            privileged_execute: false,
            unprivileged_execute: false,
        }
    }

    /// Converts permission attributes into architectural descriptor bits.
    pub const fn to_descriptor_bits(&self) -> u64 {
        let mut bits = 0u64;

        // AttrIndx[2:0] (bits [4:2])
        // Configured to match firmware MAIR_EL1 (0x00000000FFBB4400):
        // Index 0: 0x00 = Device-nGnRnE
        // Index 1: 0x44 = Normal Non-Cacheable
        // Index 2: 0xBB = Normal Write-Through
        // Index 3: 0xFF = Normal Write-Back RW-Allocate
        let attr_indx = match self.mem_type {
            MemoryType::DeviceNgNre => 0u64,
            MemoryType::NormalNonCacheable => 1u64,
            MemoryType::NormalWriteBack => 3u64,
        };
        bits |= (attr_indx & 0x7) << 2;

        // AP[2:1] (bits [7:6])
        let ap = match self.access {
            AccessPermission::KernelRw => 0b00u64,
            AccessPermission::KernelRo => 0b10u64,
            AccessPermission::UserRw => 0b01u64,
            AccessPermission::UserRo => 0b11u64,
        };
        bits |= (ap & 0x3) << 6;

        // Shareability SH[1:0] (bits [9:8])
        let sh = match self.mem_type {
            MemoryType::NormalWriteBack | MemoryType::NormalNonCacheable => 0b11u64, // Inner Shareable
            MemoryType::DeviceNgNre => 0b00u64, // Non-shareable
        };
        bits |= (sh & 0x3) << 8;

        // AF (Access Flag, bit 10): set to 1
        bits |= 1u64 << 10;

        // PXN (Privileged Execute-Never, bit 53)
        if !self.privileged_execute {
            bits |= 1u64 << 53;
        }

        // UXN (Unprivileged Execute-Never, bit 54)
        if !self.unprivileged_execute {
            bits |= 1u64 << 54;
        }

        bits
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VmmError {
    NonCanonicalAddress,
    MisalignedVirtualAddress,
    MisalignedPhysicalAddress,
    PhysicalAddressOutOfRange,
    FrameAllocationFailed,
    AlreadyMapped,
    NotMapped,
    BlockCollision,
    TableCollision,
    ArithmeticOverflow,
    UnsupportedPhysicalAddressRange(u8),
    UnsupportedTranslationGranule,
    UnsupportedLpa2Granule,
    InaccessiblePhysicalAddress,
    FrameNotOwned,
    WxDirectViolation(u64),
    WxPhysicalAliasViolation(u64),
    TrackerCapacityExceeded,
    SectionNotFound,
}

impl VmmError {
    pub const fn code(&self) -> u64 {
        match self {
            Self::NonCanonicalAddress => 1,
            Self::MisalignedVirtualAddress => 2,
            Self::MisalignedPhysicalAddress => 3,
            Self::PhysicalAddressOutOfRange => 4,
            Self::FrameAllocationFailed => 5,
            Self::AlreadyMapped => 6,
            Self::NotMapped => 7,
            Self::BlockCollision => 8,
            Self::TableCollision => 9,
            Self::ArithmeticOverflow => 10,
            Self::UnsupportedPhysicalAddressRange(_) => 11,
            Self::UnsupportedTranslationGranule => 12,
            Self::UnsupportedLpa2Granule => 13,
            Self::InaccessiblePhysicalAddress => 14,
            Self::FrameNotOwned => 15,
            Self::WxDirectViolation(_) => 16,
            Self::WxPhysicalAliasViolation(_) => 17,
            Self::TrackerCapacityExceeded => 18,
            Self::SectionNotFound => 19,
        }
    }
}

// L0–L3 Virtual Address Indexing (48-bit VA, 4 KiB Granule)
pub const fn l0_index(va: u64) -> usize {
    ((va >> 39) & 0x1FF) as usize
}

pub const fn l1_index(va: u64) -> usize {
    ((va >> 30) & 0x1FF) as usize
}

pub const fn l2_index(va: u64) -> usize {
    ((va >> 21) & 0x1FF) as usize
}

pub const fn l3_index(va: u64) -> usize {
    ((va >> 12) & 0x1FF) as usize
}

pub const fn page_offset(va: u64) -> usize {
    (va & 0xFFF) as usize
}

/// Checks if an address is canonical under 48-bit VA.
/// Bits [63:48] must be all zeroes (TTBR0) or all ones (TTBR1).
pub const fn is_canonical_va(va: u64) -> bool {
    let top = (va >> 48) & 0xFFFF;
    top == 0x0000 || top == 0xFFFF
}

// Descriptor Constructors & Inspectors
pub const fn is_valid(desc: u64) -> bool {
    (desc & 1) != 0
}

pub const fn is_table(desc: u64) -> bool {
    (desc & 0b11) == 0b11
}

pub const fn is_block(desc: u64) -> bool {
    (desc & 0b11) == 0b01
}

pub const fn output_address(desc: u64) -> u64 {
    desc & 0x0000_FFFF_FFFF_F000
}

pub const fn make_table_descriptor(next_table_paddr: u64) -> u64 {
    (next_table_paddr & 0x0000_FFFF_FFFF_F000) | 0b11
}

pub const fn make_page_descriptor(paddr: u64, perms: PagePermissions) -> u64 {
    (paddr & 0x0000_FFFF_FFFF_F000) | perms.to_descriptor_bits() | 0b11
}

pub const fn make_l2_block_descriptor(paddr: u64, perms: PagePermissions) -> u64 {
    (paddr & 0x0000_FFFF_FFE0_0000) | perms.to_descriptor_bits() | 0b01
}

/// Structure managing an AArch64 4-level translation table hierarchy.
pub struct PageTable {
    root_paddr: u64,
    max_pa: u64,
}

impl PageTable {
    pub const fn new(root_paddr: u64, max_pa: u64) -> Self {
        Self { root_paddr, max_pa }
    }

    pub fn root_paddr(&self) -> u64 {
        self.root_paddr
    }

    pub fn max_pa(&self) -> u64 {
        self.max_pa
    }

    /// Maps a single 4 KiB virtual page to a physical page.
    ///
    /// Provides atomic rollback: if allocation of any intermediate table fails
    /// or a duplicate mapping is detected, all newly created tables in this operation
    /// are deallocated, restoring previous page table state.
    pub fn map_page<A: FrameAllocator>(
        &mut self,
        va: u64,
        pa: u64,
        perms: PagePermissions,
        alloc: &mut A,
    ) -> Result<(), VmmError> {
        if !is_canonical_va(va) {
            return Err(VmmError::NonCanonicalAddress);
        }
        if va % PAGE_SIZE != 0 {
            return Err(VmmError::MisalignedVirtualAddress);
        }
        if pa % PAGE_SIZE != 0 {
            return Err(VmmError::MisalignedPhysicalAddress);
        }
        if pa >= self.max_pa {
            return Err(VmmError::PhysicalAddressOutOfRange);
        }

        // Track intermediate tables allocated during this call for rollback:
        // (table_paddr, parent_table_paddr, parent_index)
        let mut new_tables: [(u64, u64, usize); 3] = [(0, 0, 0); 3];
        let mut new_table_count = 0usize;

        let result = (|| -> Result<(), VmmError> {
            // Level 0
            let l0_idx = l0_index(va);
            let l0_entry = unsafe { alloc.frame_ref(self.root_paddr)[l0_idx] };
            let l1_paddr = if is_valid(l0_entry) {
                if !is_table(l0_entry) {
                    return Err(VmmError::TableCollision);
                }
                output_address(l0_entry)
            } else {
                let frame = alloc.allocate_frame().ok_or(VmmError::FrameAllocationFailed)?;
                new_tables[new_table_count] = (frame, self.root_paddr, l0_idx);
                new_table_count += 1;
                unsafe {
                    alloc.frame_mut(self.root_paddr)[l0_idx] = make_table_descriptor(frame);
                }
                frame
            };

            // Level 1
            let l1_idx = l1_index(va);
            let l1_entry = unsafe { alloc.frame_ref(l1_paddr)[l1_idx] };
            let l2_paddr = if is_valid(l1_entry) {
                if is_block(l1_entry) {
                    return Err(VmmError::BlockCollision);
                }
                if !is_table(l1_entry) {
                    return Err(VmmError::TableCollision);
                }
                output_address(l1_entry)
            } else {
                let frame = alloc.allocate_frame().ok_or(VmmError::FrameAllocationFailed)?;
                new_tables[new_table_count] = (frame, l1_paddr, l1_idx);
                new_table_count += 1;
                unsafe {
                    alloc.frame_mut(l1_paddr)[l1_idx] = make_table_descriptor(frame);
                }
                frame
            };

            // Level 2
            let l2_idx = l2_index(va);
            let l2_entry = unsafe { alloc.frame_ref(l2_paddr)[l2_idx] };
            let l3_paddr = if is_valid(l2_entry) {
                if is_block(l2_entry) {
                    return Err(VmmError::BlockCollision);
                }
                if !is_table(l2_entry) {
                    return Err(VmmError::TableCollision);
                }
                output_address(l2_entry)
            } else {
                let frame = alloc.allocate_frame().ok_or(VmmError::FrameAllocationFailed)?;
                new_tables[new_table_count] = (frame, l2_paddr, l2_idx);
                new_table_count += 1;
                unsafe {
                    alloc.frame_mut(l2_paddr)[l2_idx] = make_table_descriptor(frame);
                }
                frame
            };

            // Level 3 (Leaf Page)
            let l3_idx = l3_index(va);
            let l3_entry = unsafe { alloc.frame_ref(l3_paddr)[l3_idx] };
            if is_valid(l3_entry) {
                return Err(VmmError::AlreadyMapped);
            }

            unsafe {
                alloc.frame_mut(l3_paddr)[l3_idx] = make_page_descriptor(pa, perms);
            }

            Ok(())
        })();

        if result.is_err() {
            // Rollback newly created tables in reverse order
            for i in (0..new_table_count).rev() {
                let (table_paddr, parent_paddr, parent_idx) = new_tables[i];
                unsafe {
                    alloc.frame_mut(parent_paddr)[parent_idx] = 0;
                }
                alloc.deallocate_frame(table_paddr);
            }
        }

        result
    }

    /// Unmaps a single 4 KiB virtual page and returns the unmapped physical address.
    ///
    /// Automatically prunes intermediate tables that become completely empty.
    pub fn unmap_page<A: FrameAllocator>(
        &mut self,
        va: u64,
        alloc: &mut A,
    ) -> Result<u64, VmmError> {
        if !is_canonical_va(va) {
            return Err(VmmError::NonCanonicalAddress);
        }
        if va % PAGE_SIZE != 0 {
            return Err(VmmError::MisalignedVirtualAddress);
        }

        let l0_idx = l0_index(va);
        let l0_entry = unsafe { alloc.frame_ref(self.root_paddr)[l0_idx] };
        if !is_valid(l0_entry) || !is_table(l0_entry) {
            return Err(VmmError::NotMapped);
        }
        let l1_paddr = output_address(l0_entry);

        let l1_idx = l1_index(va);
        let l1_entry = unsafe { alloc.frame_ref(l1_paddr)[l1_idx] };
        if !is_valid(l1_entry) {
            return Err(VmmError::NotMapped);
        }
        if is_block(l1_entry) {
            return Err(VmmError::BlockCollision);
        }
        let l2_paddr = output_address(l1_entry);

        let l2_idx = l2_index(va);
        let l2_entry = unsafe { alloc.frame_ref(l2_paddr)[l2_idx] };
        if !is_valid(l2_entry) {
            return Err(VmmError::NotMapped);
        }
        if is_block(l2_entry) {
            return Err(VmmError::BlockCollision);
        }
        let l3_paddr = output_address(l2_entry);

        let l3_idx = l3_index(va);
        let l3_entry = unsafe { alloc.frame_ref(l3_paddr)[l3_idx] };
        if !is_valid(l3_entry) {
            return Err(VmmError::NotMapped);
        }

        let pa = output_address(l3_entry);
        unsafe {
            alloc.frame_mut(l3_paddr)[l3_idx] = 0;
        }

        // Automatic empty table pruning:
        // Intermediate tables are deallocated ONLY AFTER parent descriptors are safely cleared to 0.
        if is_table_empty(unsafe { alloc.frame_ref(l3_paddr) }) {
            unsafe {
                alloc.frame_mut(l2_paddr)[l2_idx] = 0;
            }
            alloc.deallocate_frame(l3_paddr);

            if is_table_empty(unsafe { alloc.frame_ref(l2_paddr) }) {
                unsafe {
                    alloc.frame_mut(l1_paddr)[l1_idx] = 0;
                }
                alloc.deallocate_frame(l2_paddr);

                if is_table_empty(unsafe { alloc.frame_ref(l1_paddr) }) {
                    unsafe {
                        alloc.frame_mut(self.root_paddr)[l0_idx] = 0;
                    }
                    alloc.deallocate_frame(l1_paddr);
                }
            }
        }

        Ok(pa)
    }

    /// Maps a 2 MiB virtual address range to a physical 2 MiB block at Level 2.
    pub fn map_l2_block<A: FrameAllocator>(
        &mut self,
        va: u64,
        pa: u64,
        perms: PagePermissions,
        alloc: &mut A,
    ) -> Result<(), VmmError> {
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

        let mut new_tables: [(u64, u64, usize); 2] = [(0, 0, 0); 2];
        let mut new_table_count = 0usize;

        let result = (|| -> Result<(), VmmError> {
            let l0_idx = l0_index(va);
            let l0_entry = unsafe { alloc.frame_ref(self.root_paddr)[l0_idx] };
            let l1_paddr = if is_valid(l0_entry) {
                if !is_table(l0_entry) {
                    return Err(VmmError::TableCollision);
                }
                output_address(l0_entry)
            } else {
                let frame = alloc.allocate_frame().ok_or(VmmError::FrameAllocationFailed)?;
                new_tables[new_table_count] = (frame, self.root_paddr, l0_idx);
                new_table_count += 1;
                unsafe {
                    alloc.frame_mut(self.root_paddr)[l0_idx] = make_table_descriptor(frame);
                }
                frame
            };

            let l1_idx = l1_index(va);
            let l1_entry = unsafe { alloc.frame_ref(l1_paddr)[l1_idx] };
            let l2_paddr = if is_valid(l1_entry) {
                if is_block(l1_entry) {
                    return Err(VmmError::BlockCollision);
                }
                output_address(l1_entry)
            } else {
                let frame = alloc.allocate_frame().ok_or(VmmError::FrameAllocationFailed)?;
                new_tables[new_table_count] = (frame, l1_paddr, l1_idx);
                new_table_count += 1;
                unsafe {
                    alloc.frame_mut(l1_paddr)[l1_idx] = make_table_descriptor(frame);
                }
                frame
            };

            let l2_idx = l2_index(va);
            let l2_entry = unsafe { alloc.frame_ref(l2_paddr)[l2_idx] };
            if is_valid(l2_entry) {
                return Err(VmmError::AlreadyMapped);
            }

            unsafe {
                alloc.frame_mut(l2_paddr)[l2_idx] = make_l2_block_descriptor(pa, perms);
            }

            Ok(())
        })();

        if result.is_err() {
            for i in (0..new_table_count).rev() {
                let (table_paddr, parent_paddr, parent_idx) = new_tables[i];
                unsafe {
                    alloc.frame_mut(parent_paddr)[parent_idx] = 0;
                }
                alloc.deallocate_frame(table_paddr);
            }
        }

        result
    }

    /// Translates a virtual address to its mapped physical address.
    pub fn translate<A: FrameAllocator>(
        &self,
        va: u64,
        alloc: &A,
    ) -> Result<u64, VmmError> {
        if !is_canonical_va(va) {
            return Err(VmmError::NonCanonicalAddress);
        }

        let l0_idx = l0_index(va);
        let l0_entry = unsafe { alloc.frame_ref(self.root_paddr)[l0_idx] };
        if !is_valid(l0_entry) || !is_table(l0_entry) {
            return Err(VmmError::NotMapped);
        }
        let l1_paddr = output_address(l0_entry);

        let l1_idx = l1_index(va);
        let l1_entry = unsafe { alloc.frame_ref(l1_paddr)[l1_idx] };
        if !is_valid(l1_entry) {
            return Err(VmmError::NotMapped);
        }
        if is_block(l1_entry) {
            // L1 1 GiB block
            return Ok(output_address(l1_entry) + (va & 0x3FFF_FFFF));
        }
        let l2_paddr = output_address(l1_entry);

        let l2_idx = l2_index(va);
        let l2_entry = unsafe { alloc.frame_ref(l2_paddr)[l2_idx] };
        if !is_valid(l2_entry) {
            return Err(VmmError::NotMapped);
        }
        if is_block(l2_entry) {
            // L2 2 MiB block
            return Ok(output_address(l2_entry) + (va & 0x1F_FFFF));
        }
        let l3_paddr = output_address(l2_entry);

        let l3_idx = l3_index(va);
        let l3_entry = unsafe { alloc.frame_ref(l3_paddr)[l3_idx] };
        if !is_valid(l3_entry) {
            return Err(VmmError::NotMapped);
        }

        Ok(output_address(l3_entry) + (va & 0xFFF))
    }

    /// Queries the translation table for the descriptor mapped to `va`, accounting for
    /// hierarchical table-level permission limits (APTable[1], PXNTable, UXNTable) per ARM DDI 0487.
    pub fn get_descriptor<A: FrameAllocator>(
        &self,
        va: u64,
        alloc: &A,
    ) -> Result<DescriptorInfo, VmmError> {
        if !is_canonical_va(va) {
            return Err(VmmError::NonCanonicalAddress);
        }

        let l0_idx = l0_index(va);
        let l0_entry = unsafe { alloc.frame_ref(self.root_paddr)[l0_idx] };
        if !is_valid(l0_entry) || !is_table(l0_entry) {
            return Err(VmmError::NotMapped);
        }
        let mut table_ro = ((l0_entry >> 62) & 1) != 0;
        let mut table_pxn = ((l0_entry >> 59) & 1) != 0;
        let mut table_uxn = ((l0_entry >> 60) & 1) != 0;

        let l1_paddr = output_address(l0_entry);
        let l1_idx = l1_index(va);
        let l1_entry = unsafe { alloc.frame_ref(l1_paddr)[l1_idx] };
        if !is_valid(l1_entry) {
            return Err(VmmError::NotMapped);
        }
        if is_block(l1_entry) {
            return Ok(DescriptorInfo::decode(
                l1_entry,
                DescriptorLevel::Level1Block,
                table_ro,
                table_pxn,
                table_uxn,
            ));
        }
        table_ro |= ((l1_entry >> 62) & 1) != 0;
        table_pxn |= ((l1_entry >> 59) & 1) != 0;
        table_uxn |= ((l1_entry >> 60) & 1) != 0;

        let l2_paddr = output_address(l1_entry);
        let l2_idx = l2_index(va);
        let l2_entry = unsafe { alloc.frame_ref(l2_paddr)[l2_idx] };
        if !is_valid(l2_entry) {
            return Err(VmmError::NotMapped);
        }
        if is_block(l2_entry) {
            return Ok(DescriptorInfo::decode(
                l2_entry,
                DescriptorLevel::Level2Block,
                table_ro,
                table_pxn,
                table_uxn,
            ));
        }
        table_ro |= ((l2_entry >> 62) & 1) != 0;
        table_pxn |= ((l2_entry >> 59) & 1) != 0;
        table_uxn |= ((l2_entry >> 60) & 1) != 0;

        let l3_paddr = output_address(l2_entry);
        let l3_idx = l3_index(va);
        let l3_entry = unsafe { alloc.frame_ref(l3_paddr)[l3_idx] };
        if !is_valid(l3_entry) {
            return Err(VmmError::NotMapped);
        }

        Ok(DescriptorInfo::decode(
            l3_entry,
            DescriptorLevel::Level3Page,
            table_ro,
            table_pxn,
            table_uxn,
        ))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DescriptorLevel {
    Level1Block,
    Level2Block,
    Level3Page,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DescriptorInfo {
    pub raw: u64,
    pub level: DescriptorLevel,
    pub output_paddr: u64,
    pub attr_indx: u8,
    pub ap: u8,
    pub pxn: bool,
    pub uxn: bool,
    pub table_ro: bool,
    pub table_pxn: bool,
    pub table_uxn: bool,
}

impl DescriptorInfo {
    pub fn decode(
        raw: u64,
        level: DescriptorLevel,
        table_ro: bool,
        table_pxn: bool,
        table_uxn: bool,
    ) -> Self {
        let output_paddr = output_address(raw);
        let attr_indx = ((raw >> 2) & 0x7) as u8;
        let ap = ((raw >> 6) & 0x3) as u8;
        let pxn = ((raw >> 53) & 1) != 0;
        let uxn = ((raw >> 54) & 1) != 0;
        Self {
            raw,
            level,
            output_paddr,
            attr_indx,
            ap,
            pxn,
            uxn,
            table_ro,
            table_pxn,
            table_uxn,
        }
    }

    /// True if the mapping effectively permits writes (accounting for APTable[1]).
    pub fn is_writable(&self) -> bool {
        !self.table_ro && (self.ap == 0b00 || self.ap == 0b01)
    }

    /// True if the mapping effectively permits EL1 execution (accounting for PXNTable).
    pub fn is_executable(&self) -> bool {
        !self.table_pxn && !self.pxn
    }
}

pub const MAX_TRACKED_PHYSICAL_PAGES: usize = 2048;

/// Tracker auditing unique physical pages for W^X conflicts across arbitrary virtual mappings.
pub struct PhysicalWxTracker {
    paddrs: [u64; MAX_TRACKED_PHYSICAL_PAGES],
    is_writable: [bool; MAX_TRACKED_PHYSICAL_PAGES],
    is_executable: [bool; MAX_TRACKED_PHYSICAL_PAGES],
    count: usize,
}

impl PhysicalWxTracker {
    pub const fn new() -> Self {
        Self {
            paddrs: [0; MAX_TRACKED_PHYSICAL_PAGES],
            is_writable: [false; MAX_TRACKED_PHYSICAL_PAGES],
            is_executable: [false; MAX_TRACKED_PHYSICAL_PAGES],
            count: 0,
        }
    }

    pub fn count(&self) -> usize {
        self.count
    }

    /// Records a virtual mapping to a physical page and verifies W^X compliance.
    ///
    /// Fails immediately if:
    /// 1. A single mapping is both writable and executable (Direct W^X violation).
    /// 2. A physical page was previously mapped executable and is now mapped writable,
    ///    or vice-versa (Physical W^X alias violation).
    pub fn record_mapping(&mut self, paddr: u64, writable: bool, executable: bool) -> Result<(), VmmError> {
        let page_paddr = paddr & !0xFFF;

        if writable && executable {
            return Err(VmmError::WxDirectViolation(page_paddr));
        }

        for i in 0..self.count {
            if self.paddrs[i] == page_paddr {
                if self.is_executable[i] && writable {
                    return Err(VmmError::WxPhysicalAliasViolation(page_paddr));
                }
                if self.is_writable[i] && executable {
                    return Err(VmmError::WxPhysicalAliasViolation(page_paddr));
                }
                self.is_writable[i] |= writable;
                self.is_executable[i] |= executable;
                return Ok(());
            }
        }

        if self.count >= MAX_TRACKED_PHYSICAL_PAGES {
            return Err(VmmError::TrackerCapacityExceeded);
        }

        self.paddrs[self.count] = page_paddr;
        self.is_writable[self.count] = writable;
        self.is_executable[self.count] = executable;
        self.count += 1;
        Ok(())
    }
}

fn is_table_empty(entries: &[u64; TABLE_ENTRY_COUNT]) -> bool {
    entries.iter().all(|&e| e == 0)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use std::collections::HashMap;

    pub struct MockMemoryBackend {
        frames: HashMap<u64, [u64; TABLE_ENTRY_COUNT]>,
        next_paddr: u64,
        pub fail_after: Option<usize>,
        pub alloc_count: usize,
        pub dealloc_count: usize,
    }

    impl MockMemoryBackend {
        pub fn new() -> Self {
            Self {
                frames: HashMap::new(),
                next_paddr: 0x1000,
                fail_after: None,
                alloc_count: 0,
                dealloc_count: 0,
            }
        }

        pub fn active_frame_count(&self) -> usize {
            self.frames.len()
        }
    }

    impl FrameAllocator for MockMemoryBackend {
        fn allocate_frame(&mut self) -> Option<u64> {
            if let Some(limit) = self.fail_after {
                if self.alloc_count >= limit {
                    return None;
                }
            }
            let paddr = self.next_paddr;
            self.next_paddr += PAGE_SIZE;
            self.frames.insert(paddr, [0; TABLE_ENTRY_COUNT]);
            self.alloc_count += 1;
            Some(paddr)
        }

        fn deallocate_frame(&mut self, paddr: u64) {
            assert!(self.frames.remove(&paddr).is_some(), "Deallocating unallocated frame");
            self.dealloc_count += 1;
        }

        unsafe fn frame_mut(&mut self, paddr: u64) -> &mut [u64; TABLE_ENTRY_COUNT] {
            self.frames.get_mut(&paddr).expect("Frame missing in frame_mut")
        }

        unsafe fn frame_ref(&self, paddr: u64) -> &[u64; TABLE_ENTRY_COUNT] {
            self.frames.get(&paddr).expect("Frame missing in frame_ref")
        }
    }

    #[test]
    fn descriptor_bitfield_construction_and_permissions() {
        let code_perm = PagePermissions::kernel_code();
        let desc = make_page_descriptor(0x4000_1000, code_perm);
        assert!(is_valid(desc));
        assert_eq!(output_address(desc), 0x4000_1000);
        // AP = 0b10 (Read-Only) -> bits [7:6] == 0b10
        assert_eq!((desc >> 6) & 0x3, 0b10);
        // PXN = 0 (Privileged execute enabled) -> bit 53 == 0
        assert_eq!((desc >> 53) & 1, 0);
        // UXN = 1 (Unprivileged execute disabled) -> bit 54 == 1
        assert_eq!((desc >> 54) & 1, 1);
        // AttrIndx = 3 (Normal Write-Back RW-Allocate matching firmware MAIR_EL1) -> bits [4:2] == 3
        assert_eq!((desc >> 2) & 0x7, 3);

        let mmio_perm = PagePermissions::device_mmio();
        let mmio_desc = make_page_descriptor(0x0900_0000, mmio_perm);
        assert!(is_valid(mmio_desc));
        // AttrIndx = 0 (Device-nGnRnE matching firmware MAIR_EL1) -> bits [4:2] == 0
        assert_eq!((mmio_desc >> 2) & 0x7, 0);
        // PXN = 1 -> bit 53 == 1
        assert_eq!((mmio_desc >> 53) & 1, 1);

        // Stage 9A: EL0 User-Space Permissions
        // user_code(): AP = 0b11 (RO EL0+EL1), UXN = 0 (EL0 exec), PXN = 1 (EL1 NX)
        let u_code = PagePermissions::user_code();
        let u_code_desc = make_page_descriptor(0x0000_1000, u_code);
        assert!(is_valid(u_code_desc));
        assert_eq!((u_code_desc >> 6) & 0x3, 0b11); // AP = 0b11
        assert_eq!((u_code_desc >> 53) & 1, 1);     // PXN = 1
        assert_eq!((u_code_desc >> 54) & 1, 0);     // UXN = 0
        assert_eq!((u_code_desc >> 2) & 0x7, 3);    // Normal WB-WA

        // user_rodata(): AP = 0b11 (RO EL0+EL1), UXN = 1 (NX), PXN = 1 (EL1 NX)
        let u_rodata = PagePermissions::user_rodata();
        let u_ro_desc = make_page_descriptor(0x0000_2000, u_rodata);
        assert!(is_valid(u_ro_desc));
        assert_eq!((u_ro_desc >> 6) & 0x3, 0b11);   // AP = 0b11
        assert_eq!((u_ro_desc >> 53) & 1, 1);       // PXN = 1
        assert_eq!((u_ro_desc >> 54) & 1, 1);       // UXN = 1

        // user_data(): AP = 0b01 (RW EL0+EL1), UXN = 1 (NX), PXN = 1 (EL1 NX)
        let u_data = PagePermissions::user_data();
        let u_data_desc = make_page_descriptor(0x0000_3000, u_data);
        assert!(is_valid(u_data_desc));
        assert_eq!((u_data_desc >> 6) & 0x3, 0b01); // AP = 0b01
        assert_eq!((u_data_desc >> 53) & 1, 1);     // PXN = 1
        assert_eq!((u_data_desc >> 54) & 1, 1);     // UXN = 1
    }

    #[test]
    fn indexing_and_canonical_checks() {
        assert!(is_canonical_va(0x0000_0000_4000_0000));
        assert!(is_canonical_va(0xFFFF_8000_0000_0000));
        assert!(!is_canonical_va(0x0001_0000_0000_0000));

        // 1. Direct Physical Map Base: 0xFFFF_8000_0000_0000
        // Bit 47 is 1, bits [46:39] are 0 -> L0 = 0b1_0000_0000 = 256
        let phys_map = 0xFFFF_8000_0000_0000u64;
        assert_eq!(l0_index(phys_map), 256);
        assert_eq!(l1_index(phys_map), 0);
        assert_eq!(l2_index(phys_map), 0);
        assert_eq!(l3_index(phys_map), 0);
        assert_eq!(page_offset(phys_map), 0);

        // 2. Kernel Higher-Half Base: 0xFFFF_FFFF_8000_0000 (-2 GiB)
        // Lower 48 bits: 0xFFFF_8000_0000
        let kernel_va = 0xFFFF_FFFF_8000_0000u64;
        assert_eq!(l0_index(kernel_va), 511);
        assert_eq!(l1_index(kernel_va), 510);
        assert_eq!(l2_index(kernel_va), 0);
        assert_eq!(l3_index(kernel_va), 0);
        assert_eq!(page_offset(kernel_va), 0);

        // 3. UART MMIO Higher-Half Base: 0xFFFF_FFFF_F000_0000 (-256 MiB)
        let mmio_va = 0xFFFF_FFFF_F000_0000u64;
        assert_eq!(l0_index(mmio_va), 511);
        assert_eq!(l1_index(mmio_va), 511);
        assert_eq!(l2_index(mmio_va), 384);
        assert_eq!(l3_index(mmio_va), 0);
        assert_eq!(page_offset(mmio_va), 0);

        // 4. Kernel Image Physical Trampoline: 0x4800_0000
        let kernel_pa = 0x4800_0000u64;
        assert_eq!(l0_index(kernel_pa), 0);
        assert_eq!(l1_index(kernel_pa), 1);
        assert_eq!(l2_index(kernel_pa), 64);
        assert_eq!(l3_index(kernel_pa), 0);
        assert_eq!(page_offset(kernel_pa), 0);

        // 5. Kernel Image Second Block: 0x4820_0000
        let kernel_pa2 = 0x4820_0000u64;
        assert_eq!(l0_index(kernel_pa2), 0);
        assert_eq!(l1_index(kernel_pa2), 1);
        assert_eq!(l2_index(kernel_pa2), 65);
        assert_eq!(l3_index(kernel_pa2), 0);
        assert_eq!(page_offset(kernel_pa2), 0);

        // 6. UART Physical Trampoline: 0x0900_0000
        let uart_pa = 0x0900_0000u64;
        assert_eq!(l0_index(uart_pa), 0);
        assert_eq!(l1_index(uart_pa), 0);
        assert_eq!(l2_index(uart_pa), 72);
        assert_eq!(l3_index(uart_pa), 0);
        assert_eq!(page_offset(uart_pa), 0);

        // 7. Canonical Range Extremities:
        // Lowest address in TTBR1: 0xFFFF_0000_0000_0000
        let ttbr1_min = 0xFFFF_0000_0000_0000u64;
        assert_eq!(l0_index(ttbr1_min), 0);
        assert_eq!(l1_index(ttbr1_min), 0);
        assert_eq!(l2_index(ttbr1_min), 0);
        assert_eq!(l3_index(ttbr1_min), 0);

        // Highest address in TTBR0: 0x0000_FFFF_FFFF_FFFF
        let ttbr0_max = 0x0000_FFFF_FFFF_FFFFu64;
        assert_eq!(l0_index(ttbr0_max), 511);
        assert_eq!(l1_index(ttbr0_max), 511);
        assert_eq!(l2_index(ttbr0_max), 511);
        assert_eq!(l3_index(ttbr0_max), 511);
        assert_eq!(page_offset(ttbr0_max), 0xFFF);

        let va = 0xFFFF_8000_4000_1234u64;
        assert_eq!(l0_index(va), 256);
        assert_eq!(page_offset(va), 0x234);
    }

    #[test]
    fn single_page_map_translate_unmap() {
        let mut mock = MockMemoryBackend::new();
        let root = mock.allocate_frame().unwrap();
        let mut pt = PageTable::new(root, 1u64 << 48);

        let va = 0xFFFF_8000_4000_0000u64;
        let pa = 0x4000_0000u64;

        pt.map_page(va, pa, PagePermissions::kernel_data(), &mut mock).unwrap();

        // Check translation
        let translated = pt.translate(va, &mock).unwrap();
        assert_eq!(translated, pa);

        // Check offset preservation
        let translated_offset = pt.translate(va + 0x120, &mock).unwrap();
        assert_eq!(translated_offset, pa + 0x120);

        // Unmap
        let unmapped_pa = pt.unmap_page(va, &mut mock).unwrap();
        assert_eq!(unmapped_pa, pa);

        // Subsequent translation must fail
        assert_eq!(pt.translate(va, &mock), Err(VmmError::NotMapped));

        // Since it was the only page, all intermediate tables (L1, L2, L3) should be pruned
        // Only the root table remains
        assert_eq!(mock.active_frame_count(), 1);
    }

    #[test]
    fn contiguous_page_mapping_and_unmapping() {
        let mut mock = MockMemoryBackend::new();
        let root = mock.allocate_frame().unwrap();
        let mut pt = PageTable::new(root, 1u64 << 48);

        let base_va = 0xFFFF_8000_0000_0000u64;
        let base_pa = 0x4000_0000u64;

        // Map 10 consecutive pages
        for i in 0..10 {
            pt.map_page(
                base_va + (i * PAGE_SIZE),
                base_pa + (i * PAGE_SIZE),
                PagePermissions::kernel_data(),
                &mut mock,
            ).unwrap();
        }

        for i in 0..10 {
            assert_eq!(
                pt.translate(base_va + (i * PAGE_SIZE), &mock).unwrap(),
                base_pa + (i * PAGE_SIZE)
            );
        }

        // Unmap first 9
        for i in 0..9 {
            pt.unmap_page(base_va + (i * PAGE_SIZE), &mut mock).unwrap();
        }
        // Tables are still active because page 9 remains mapped
        assert!(mock.active_frame_count() > 1);

        // Unmap last page
        pt.unmap_page(base_va + (9 * PAGE_SIZE), &mut mock).unwrap();
        // Now all intermediate tables are pruned
        assert_eq!(mock.active_frame_count(), 1);
    }

    #[test]
    fn rejects_misaligned_and_non_canonical_addresses() {
        let mut mock = MockMemoryBackend::new();
        let root = mock.allocate_frame().unwrap();
        let mut pt = PageTable::new(root, 1u64 << 48);

        // Misaligned VA
        assert_eq!(
            pt.map_page(0x1001, 0x2000, PagePermissions::kernel_data(), &mut mock),
            Err(VmmError::MisalignedVirtualAddress)
        );

        // Misaligned PA
        assert_eq!(
            pt.map_page(0x1000, 0x2001, PagePermissions::kernel_data(), &mut mock),
            Err(VmmError::MisalignedPhysicalAddress)
        );

        // Non-canonical VA
        assert_eq!(
            pt.map_page(0x0001_0000_0000_0000, 0x2000, PagePermissions::kernel_data(), &mut mock),
            Err(VmmError::NonCanonicalAddress)
        );

        // PA out of range
        assert_eq!(
            pt.map_page(0x1000, 1u64 << 48, PagePermissions::kernel_data(), &mut mock),
            Err(VmmError::PhysicalAddressOutOfRange)
        );
    }

    #[test]
    fn rejects_duplicate_mapping_and_preserves_state() {
        let mut mock = MockMemoryBackend::new();
        let root = mock.allocate_frame().unwrap();
        let mut pt = PageTable::new(root, 1u64 << 48);

        let va = 0xFFFF_8000_1000_0000u64;
        pt.map_page(va, 0x1000, PagePermissions::kernel_data(), &mut mock).unwrap();

        // Attempting to map same VA again fails with AlreadyMapped
        let res = pt.map_page(va, 0x2000, PagePermissions::kernel_data(), &mut mock);
        assert_eq!(res, Err(VmmError::AlreadyMapped));

        // Original mapping remains intact
        assert_eq!(pt.translate(va, &mock).unwrap(), 0x1000);
    }

    #[test]
    fn rejects_unmapping_unmapped_address() {
        let mut mock = MockMemoryBackend::new();
        let root = mock.allocate_frame().unwrap();
        let mut pt = PageTable::new(root, 1u64 << 48);

        assert_eq!(
            pt.unmap_page(0xFFFF_8000_0000_0000, &mut mock),
            Err(VmmError::NotMapped)
        );
    }

    #[test]
    fn partial_operation_rollback_on_allocation_failure() {
        let mut mock = MockMemoryBackend::new();
        let root = mock.allocate_frame().unwrap();
        let mut pt = PageTable::new(root, 1u64 << 48);

        // Root table is frame 1 (active_frame_count = 1).
        // Mapping a new page requires 3 new tables (L1, L2, L3).
        // Allow only 2 allocations (L1 and L2 succeed, L3 fails).
        mock.fail_after = Some(3); // Frame 1 was root, so total 3 allocations allowed (root + L1 + L2)

        let va = 0xFFFF_8000_0000_0000u64;
        let res = pt.map_page(va, 0x4000_0000, PagePermissions::kernel_data(), &mut mock);
        assert_eq!(res, Err(VmmError::FrameAllocationFailed));

        // Verify Rollback:
        // L1 and L2 must be deallocated; root table entry must be reset to 0;
        // active frame count must return to 1!
        assert_eq!(mock.active_frame_count(), 1);
        let l0_idx = l0_index(va);
        let l0_entry = unsafe { mock.frame_ref(root)[l0_idx] };
        assert_eq!(l0_entry, 0);
    }

    #[test]
    fn block_mapping_and_collision_rejection() {
        let mut mock = MockMemoryBackend::new();
        let root = mock.allocate_frame().unwrap();
        let mut pt = PageTable::new(root, 1u64 << 48);

        let va = 0xFFFF_8000_0000_0000u64;
        let pa = 0x4000_0000u64;

        // Map 2 MiB block at L2
        pt.map_l2_block(va, pa, PagePermissions::kernel_data(), &mut mock).unwrap();

        // Translate across the block
        assert_eq!(pt.translate(va, &mock).unwrap(), pa);
        assert_eq!(pt.translate(va + 0x1000, &mock).unwrap(), pa + 0x1000);
        assert_eq!(pt.translate(va + 0x1F_F000, &mock).unwrap(), pa + 0x1F_F000);

        // Attempting to map a 4 KiB page inside an existing 2 MiB block must fail with BlockCollision
        let collision_res = pt.map_page(va + 0x2000, 0x5000_0000, PagePermissions::kernel_data(), &mut mock);
        assert_eq!(collision_res, Err(VmmError::BlockCollision));
    }

    #[test]
    fn cpu_feature_register_strict_validation() {
        // Value representing PARange = 0b0101 (48 bits), TGran4 = 0b0000 (Supported), TGran64 = 0b0000
        let reg_val = 0x0000_0000_0000_0005u64;
        let feat = crate::vmm::parse_mmfr0(reg_val).unwrap();
        assert_eq!(feat.pa_range, crate::vmm::PaRange::Bits48);
        assert_eq!(feat.max_pa, 1u64 << 48);
        assert!(feat.tgran4_supported);
        assert_eq!(crate::vmm::ips_field(feat.pa_range), 0b101);

        // Value with TGran4 == 0x1 (FEAT_LPA2 52-bit granule) must be explicitly rejected
        let lpa2_granule = 0x0000_0000_1000_0005u64;
        assert_eq!(
            crate::vmm::parse_mmfr0(lpa2_granule),
            Err(VmmError::UnsupportedLpa2Granule)
        );

        // Value with TGran4 unsupported (0xF in bits [31:28])
        let unsupported_granule = 0x0000_0000_F000_0005u64;
        assert_eq!(
            crate::vmm::parse_mmfr0(unsupported_granule),
            Err(VmmError::UnsupportedTranslationGranule)
        );

        // Value with TGran4 reserved (0x2 in bits [31:28])
        let reserved_granule = 0x0000_0000_2000_0005u64;
        assert_eq!(
            crate::vmm::parse_mmfr0(reserved_granule),
            Err(VmmError::UnsupportedTranslationGranule)
        );

        // Reserved PARange (0b0111 = 7 in bits [3:0])
        let reserved_parange = 0x0000_0000_0000_0007u64;
        assert_eq!(
            crate::vmm::parse_mmfr0(reserved_parange),
            Err(VmmError::UnsupportedPhysicalAddressRange(7))
        );
    }

    #[test]
    fn identity_phys_access_validation() {
        use crate::vmm::IdentityPhysAccess;

        // Misaligned base
        assert_eq!(
            IdentityPhysAccess::new(0x4000_0001, 0x5000_0000),
            Err(VmmError::MisalignedPhysicalAddress)
        );

        // Inverted range
        assert_eq!(
            IdentityPhysAccess::new(0x5000_0000, 0x4000_0000),
            Err(VmmError::InaccessiblePhysicalAddress)
        );

        // Valid range
        let access = IdentityPhysAccess::new(0x4000_0000, 0x5000_0000).unwrap();
        assert_eq!(access.accessible_base(), 0x4000_0000);
        assert_eq!(access.accessible_limit(), 0x5000_0000);
    }

    struct MockPhysMemory {
        storage: std::boxed::Box<[u8]>,
        base_paddr: u64,
        limit_paddr: u64,
    }

    impl MockPhysMemory {
        fn new(base_paddr: u64, pages: usize) -> Self {
            let size = pages * (PAGE_SIZE as usize);
            // Pre-fill with garbage 0xa5 to verify frame zeroing
            let vec = std::vec![0xa5u8; size];
            Self {
                storage: vec.into_boxed_slice(),
                base_paddr,
                limit_paddr: base_paddr + size as u64,
            }
        }
    }

    impl crate::vmm::PhysAccess for MockPhysMemory {
        unsafe fn phys_to_virt(&self, paddr: u64) -> Result<*mut [u64; TABLE_ENTRY_COUNT], VmmError> {
            if paddr % PAGE_SIZE != 0 {
                return Err(VmmError::MisalignedPhysicalAddress);
            }
            if paddr < self.base_paddr || paddr + PAGE_SIZE > self.limit_paddr {
                return Err(VmmError::InaccessiblePhysicalAddress);
            }
            let offset = (paddr - self.base_paddr) as usize;
            let ptr = self.storage.as_ptr().add(offset) as *mut [u64; TABLE_ENTRY_COUNT];
            Ok(ptr)
        }
    }

    fn make_test_map(base: u64, pages: u64) -> std::vec::Vec<u8> {
        let mut bytes = std::vec![0u8; 40];
        bytes[0..4].copy_from_slice(&7u32.to_le_bytes()); // EFI_CONVENTIONAL_MEMORY
        bytes[8..16].copy_from_slice(&base.to_le_bytes());
        bytes[24..32].copy_from_slice(&pages.to_le_bytes());
        bytes
    }

    #[test]
    fn pmm_frame_allocator_allocation_and_zeroing() {
        use crate::pmm::PhysicalPageAllocator;
        use crate::vmm::PmmFrameAllocator;

        let mut pmm = PhysicalPageAllocator::<2>::new(); // 128 pages capacity
        let map = make_test_map(0x4000_0000, 32);
        unsafe {
            pmm.init_from_memory_map(map.as_ptr(), map.len(), 40, &[]).unwrap();
        }
        assert_eq!(pmm.free_pages(), 32);

        let phys_mem = MockPhysMemory::new(0x4000_0000, 32);
        let mut frame_alloc = PmmFrameAllocator::new(&mut pmm, phys_mem);

        let f1 = frame_alloc.allocate_frame().expect("allocate frame 1");
        assert_eq!(f1, 0x4000_0000);
        assert_eq!(frame_alloc.allocated_count(), 1);
        assert!(frame_alloc.is_frame_owned(f1));

        // Requirement 2: Verify that every entry in the allocated frame was completely zeroed
        let entries = unsafe { frame_alloc.frame_ref(f1) };
        assert!(entries.iter().all(|&e| e == 0), "Allocated frame must be zeroed");

        // Free the frame
        frame_alloc.deallocate_frame(f1);
        assert_eq!(frame_alloc.allocated_count(), 0);
        assert!(!frame_alloc.is_frame_owned(f1));
    }

    #[test]
    fn pmm_frame_allocator_inaccessible_address_rejection() {
        use crate::pmm::PhysicalPageAllocator;
        use crate::vmm::PmmFrameAllocator;

        let mut pmm = PhysicalPageAllocator::<2>::new();
        let map = make_test_map(0x4000_0000, 10);
        unsafe {
            pmm.init_from_memory_map(map.as_ptr(), map.len(), 40, &[]).unwrap();
        }

        // Mock memory only exposes 1 page [0x4000_0000, 0x4000_1000)
        let phys_mem = MockPhysMemory::new(0x4000_0000, 1);
        let mut frame_alloc = PmmFrameAllocator::new(&mut pmm, phys_mem);

        // Frame 1 succeeds
        let f1 = frame_alloc.allocate_frame().unwrap();
        assert_eq!(f1, 0x4000_0000);

        // Frame 2 at 0x4000_1000 is rejected by phys_access as InaccessiblePhysicalAddress.
        // It must return None and roll back the PMM allocation so no physical pages leak.
        assert_eq!(frame_alloc.allocate_frame(), None);

        // Verify rollback: PMM free pages should still be 9 (only 1 page used)
        frame_alloc.deallocate_frame(f1);
        assert_eq!(frame_alloc.allocated_count(), 0);
    }

    #[test]
    fn address_space_page_and_range_mapping_with_rollback() {
        use crate::pmm::PhysicalPageAllocator;
        use crate::vmm::{AddressSpace, PmmFrameAllocator};

        let mut pmm = PhysicalPageAllocator::<4>::new(); // 256 pages
        let map = make_test_map(0x4000_0000, 128);
        unsafe {
            pmm.init_from_memory_map(map.as_ptr(), map.len(), 40, &[]).unwrap();
        }

        let phys_mem = MockPhysMemory::new(0x4000_0000, 128);
        let frame_alloc = PmmFrameAllocator::new(&mut pmm, phys_mem);
        let mut aspace = AddressSpace::new(frame_alloc, 1u64 << 48).unwrap();

        // 1 frame used for root L0 table
        assert_eq!(aspace.allocator().allocated_count(), 1);

        // Map range of 5 pages
        let va_base = 0xFFFF_8000_0000_0000u64;
        let pa_base = 0x4000_0000u64;
        aspace.map_range(va_base, pa_base, 5, PagePermissions::kernel_data()).unwrap();
        assert_eq!(aspace.mapped_pages_count(), 5);

        for i in 0..5 {
            let va = va_base + (i as u64) * PAGE_SIZE;
            let pa = pa_base + (i as u64) * PAGE_SIZE;
            assert_eq!(aspace.translate(va).unwrap(), pa);
        }

        // Attempting map_range that collides on page 2
        // va_base + 3*PAGE_SIZE is already mapped, so mapping 3 pages starting at va_base + 2*PAGE_SIZE
        // will succeed on page 0, but collide on page 1 and trigger atomic rollback!
        let collide_va = va_base + 2 * PAGE_SIZE;
        let res = aspace.map_range(collide_va, 0x5000_0000, 3, PagePermissions::kernel_data());
        assert_eq!(res, Err(VmmError::AlreadyMapped));

        // Original 5 mappings must still be intact
        assert_eq!(aspace.mapped_pages_count(), 5);
        for i in 0..5 {
            let va = va_base + (i as u64) * PAGE_SIZE;
            let pa = pa_base + (i as u64) * PAGE_SIZE;
            assert_eq!(aspace.translate(va).unwrap(), pa);
        }

        // Unmap all 5 pages
        aspace.unmap_range(va_base, 5).unwrap();
        assert_eq!(aspace.mapped_pages_count(), 0);

        // Intermediate tables (L1, L2, L3) are pruned in correct order; only root L0 remains
        assert_eq!(aspace.allocator().allocated_count(), 1);
    }
}
