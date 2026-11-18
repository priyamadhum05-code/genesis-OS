//! Virtual Memory Management (VMM) for Project Genesis.
//!
//! Provides translation-table descriptor construction, 4-level table walking (L0–L3)
//! for 48-bit Virtual Addressing with a 4 KiB translation granule, CPU memory feature
//! detection, permission modeling, and frame-allocator decoupling.

pub mod address_space;
pub mod frame_allocator;
pub mod dynamic_mapper;
pub mod page_table;
pub mod pe;
pub mod preflight;
pub mod transition;

pub use pe::{parse_pe_image, ParsedPeImage, PeSection};

pub use address_space::AddressSpace;
pub use dynamic_mapper::DynamicMapper;
pub use frame_allocator::{
    IdentityPhysAccess, PhysAccess, PmmFrameAllocator, MAX_PAGE_TABLE_FRAMES,
};
pub use page_table::{
    AccessPermission, DescriptorInfo, DescriptorLevel, FrameAllocator, MemoryType, PagePermissions,
    PageTable, PhysicalWxTracker, VmmError, PAGE_SIZE,
};
pub use preflight::{
    phys_to_virt, run_stage3c1_preflight, virt_to_phys, DormantTablePool, PreflightReport,
    DRAM_PADDR_END, DRAM_PADDR_START, DRAM_SIZE, HHDM_BASE, KERNEL_IMAGE_PADDR,
    KERNEL_RESERVE_SIZE, KERNEL_VA_BASE, MMIO_VA_BASE,
};
pub use transition::{
    get_stage3c3_layout, get_transition_layout, stage3c2_mmu_transition,
    stage3c3_jump_higher_half, stage3c3_vectors_start, verify_firmware_identity_mapping,
    verify_higher_half_pe_header, verify_stack_usability, Stage3c3Layout, TransitionLayout,
    GENESIS_MAIR_VALUE, GENESIS_TCR_VALUE, TRANSITION_CLEAN_END, TRANSITION_CLEAN_PAGES,
    TRANSITION_CLEAN_START,
};

/// AArch64 Physical Address range supported by the hardware, derived from `ID_AA64MMFR0_EL1.PARange`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaRange {
    Bits32, // 4 GiB
    Bits36, // 64 GiB
    Bits40, // 1 TiB
    Bits42, // 4 TiB
    Bits44, // 16 TiB
    Bits48, // 256 TiB
    Bits52, // 4 PiB (ARMv8.2-A LVA/LPA)
    Reserved(u8),
}

/// Decoded CPU memory model feature flags from `ID_AA64MMFR0_EL1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuMemoryFeatures {
    pub pa_range: PaRange,
    pub max_pa: u64,
    pub tgran4_supported: bool,
    pub tgran16_supported: bool,
    pub tgran64_supported: bool,
}

/// Returns the maximum addressable physical address for a given `PaRange`.
/// For standard 4 KiB descriptors (non-LPA2), output addresses are limited to 48 bits.
pub const fn max_physical_address(pa_range: PaRange) -> u64 {
    match pa_range {
        PaRange::Bits32 => 1u64 << 32,
        PaRange::Bits36 => 1u64 << 36,
        PaRange::Bits40 => 1u64 << 40,
        PaRange::Bits42 => 1u64 << 42,
        PaRange::Bits44 => 1u64 << 44,
        PaRange::Bits48 | PaRange::Bits52 => 1u64 << 48,
        PaRange::Reserved(_) => 1u64 << 32, // Safe conservative fallback
    }
}

/// Returns the 3-bit IPS (Intermediate Physical Address Size) value for `TCR_EL1`.
/// Clamped to 48-bit (0b101) for standard non-LPA2 4 KiB translation descriptors.
pub const fn ips_field(pa_range: PaRange) -> u64 {
    match pa_range {
        PaRange::Bits32 => 0b000,
        PaRange::Bits36 => 0b001,
        PaRange::Bits40 => 0b010,
        PaRange::Bits42 => 0b011,
        PaRange::Bits44 => 0b100,
        PaRange::Bits48 | PaRange::Bits52 => 0b101,
        PaRange::Reserved(_) => 0b000,
    }
}

/// Parses and strictly validates the architectural `ID_AA64MMFR0_EL1` register value.
///
/// Returns `Err` if the physical address range or 4 KiB translation granule
/// uses an unknown, reserved, or unsupported hardware encoding.
///
/// In particular, TGran4 == 0x1 indicates FEAT_LPA2 (52-bit PA/VA with alternative
/// descriptor bit positions). Because Genesis implements standard ARMv8-A 48-bit
/// descriptors, FEAT_LPA2 is explicitly rejected rather than silently treated as
/// standard 4 KiB support.
pub fn parse_mmfr0(val: u64) -> Result<CpuMemoryFeatures, VmmError> {
    let parange_field = (val & 0xF) as u8;
    let tgran4_field = ((val >> 28) & 0xF) as u8;
    let tgran64_field = ((val >> 24) & 0xF) as u8;
    let tgran16_field = ((val >> 20) & 0xF) as u8;

    let pa_range = match parange_field {
        0b0000 => PaRange::Bits32,
        0b0001 => PaRange::Bits36,
        0b0010 => PaRange::Bits40,
        0b0011 => PaRange::Bits42,
        0b0100 => PaRange::Bits44,
        0b0101 => PaRange::Bits48,
        0b0110 => PaRange::Bits52,
        other => return Err(VmmError::UnsupportedPhysicalAddressRange(other)),
    };

    // ARMv8-A D19.2.66:
    // 0b0000 = Standard 4 KiB granule supported.
    // 0b0001 = 4 KiB granule supported with FEAT_LPA2 (52-bit PA/VA). Requires alternative descriptor packing.
    // 0b1111 = 4 KiB granule NOT supported.
    // Other values are reserved.
    let tgran4_supported = match tgran4_field {
        0x0 => true,
        0x1 => return Err(VmmError::UnsupportedLpa2Granule),
        0xF => return Err(VmmError::UnsupportedTranslationGranule),
        _ => return Err(VmmError::UnsupportedTranslationGranule),
    };

    let tgran64_supported = tgran64_field != 0xF;
    let tgran16_supported = tgran16_field == 0x1;

    let max_pa = max_physical_address(pa_range);

    Ok(CpuMemoryFeatures {
        pa_range,
        max_pa,
        tgran4_supported,
        tgran16_supported,
        tgran64_supported,
    })
}
