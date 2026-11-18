//! Bounded parser for the UEFI memory map retained in the boot handoff.

use core::{mem, ptr};

pub const BUFFER_CAPACITY: usize = 128 * 1024;
pub const EFI_CONVENTIONAL_MEMORY: u32 = 7;
pub const EFI_PAGE_SIZE: u64 = 4096;
const EFI_MEMORY_DESCRIPTOR_PREFIX_SIZE: usize = 40;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryDescriptor {
    pub kind: u32,
    pub physical_start: u64,
    pub virtual_start: u64,
    pub page_count: u64,
    pub attribute: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MapError {
    NullBuffer,
    EmptyMap,
    InvalidStride,
    InvalidLength,
    DescriptorOutOfBounds,
    AddressOverflow,
    MisalignedPhysicalAddress,
    EmptyRegion,
    PageRangeOverflow,
    PageCountOverflow,
}

/// Iterates over all descriptors in the UEFI memory map, validating prefix bounds,
/// alignment, non-zero page count, and page range arithmetic before invoking `visitor`.
///
/// # Safety
/// `map` must refer to `map_size` initialized bytes returned by UEFI's GetMemoryMap
/// and remain readable for the duration of this call. All fields are read as unaligned
/// scalars so descriptor stride need not equal the Rust layout size or provide
/// alignment for references.
pub unsafe fn for_each_descriptor<F>(
    map: *const u8,
    map_size: usize,
    descriptor_size: usize,
    mut visitor: F,
) -> Result<(), MapError>
where
    F: FnMut(&MemoryDescriptor) -> Result<(), MapError>,
{
    if map.is_null() {
        return Err(MapError::NullBuffer);
    }
    if map_size == 0 {
        return Err(MapError::EmptyMap);
    }
    if descriptor_size < EFI_MEMORY_DESCRIPTOR_PREFIX_SIZE
        || descriptor_size < mem::size_of::<u32>()
    {
        return Err(MapError::InvalidStride);
    }
    if map_size > BUFFER_CAPACITY || map_size > isize::MAX as usize {
        return Err(MapError::InvalidLength);
    }
    if (map as usize).checked_add(map_size).is_none() {
        return Err(MapError::AddressOverflow);
    }
    if map_size < descriptor_size {
        return Err(MapError::DescriptorOutOfBounds);
    }
    if map_size % descriptor_size != 0 {
        return Err(MapError::InvalidLength);
    }

    let mut offset = 0usize;
    while offset < map_size {
        let prefix_end = offset
            .checked_add(EFI_MEMORY_DESCRIPTOR_PREFIX_SIZE)
            .ok_or(MapError::InvalidLength)?;
        if prefix_end > map_size {
            return Err(MapError::InvalidLength);
        }

        let descriptor = map.add(offset);
        let kind = ptr::read_unaligned(descriptor.cast::<u32>());
        let physical_start = ptr::read_unaligned(descriptor.add(8).cast::<u64>());
        let virtual_start = ptr::read_unaligned(descriptor.add(16).cast::<u64>());
        let page_count = ptr::read_unaligned(descriptor.add(24).cast::<u64>());
        let attribute = ptr::read_unaligned(descriptor.add(32).cast::<u64>());

        if physical_start % EFI_PAGE_SIZE != 0 {
            return Err(MapError::MisalignedPhysicalAddress);
        }
        if page_count == 0 {
            return Err(MapError::EmptyRegion);
        }
        let region_bytes = page_count
            .checked_mul(EFI_PAGE_SIZE)
            .ok_or(MapError::PageRangeOverflow)?;
        physical_start
            .checked_add(region_bytes)
            .ok_or(MapError::PageRangeOverflow)?;

        let desc = MemoryDescriptor {
            kind,
            physical_start,
            virtual_start,
            page_count,
            attribute,
        };

        visitor(&desc)?;

        offset = offset
            .checked_add(descriptor_size)
            .ok_or(MapError::InvalidLength)?;
    }

    if offset != map_size {
        return Err(MapError::InvalidLength);
    }
    Ok(())
}

/// Counts pages whose EFI descriptor type is EFI_CONVENTIONAL_MEMORY.
///
/// # Safety
/// `map` must refer to `map_size` initialized bytes returned by UEFI's
/// GetMemoryMap and remain readable for the duration of this call. All fields
/// are read as unaligned scalars so descriptor stride need not equal the Rust
/// layout size or provide alignment for references.
pub unsafe fn conventional_pages(
    map: *const u8,
    map_size: usize,
    descriptor_size: usize,
) -> Result<u64, MapError> {
    let mut conventional_total = 0u64;
    for_each_descriptor(map, map_size, descriptor_size, |desc| {
        if desc.kind == EFI_CONVENTIONAL_MEMORY {
            conventional_total = accumulate_conventional_pages(conventional_total, desc.page_count)?;
        }
        Ok(())
    })?;
    Ok(conventional_total)
}

fn accumulate_conventional_pages(total: u64, pages: u64) -> Result<u64, MapError> {
    total.checked_add(pages).ok_or(MapError::PageCountOverflow)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{accumulate_conventional_pages, conventional_pages, MapError};
    use std::vec::Vec;

    const CONVENTIONAL: u32 = 7;
    const RESERVED: u32 = 0;
    const PREFIX: usize = 40;

    fn descriptor(kind: u32, physical_start: u64, pages: u64, stride: usize) -> Vec<u8> {
        assert!(stride >= PREFIX);
        let mut bytes = std::vec![0xa5; stride];
        bytes[0..4].copy_from_slice(&kind.to_le_bytes());
        bytes[8..16].copy_from_slice(&physical_start.to_le_bytes());
        bytes[24..32].copy_from_slice(&pages.to_le_bytes());
        bytes
    }

    fn parse(bytes: &[u8], stride: usize) -> Result<u64, MapError> {
        // The test backing slice is live and initialized for the call.
        unsafe { conventional_pages(bytes.as_ptr(), bytes.len(), stride) }
    }

    #[test]
    fn rejects_descriptor_stride_smaller_than_minimum_prefix() {
        let bytes = std::vec![0; PREFIX - 1];
        assert_eq!(parse(&bytes, PREFIX - 1), Err(MapError::InvalidStride));
    }

    #[test]
    fn accepts_larger_descriptor_stride_with_padding() {
        let stride = 48;
        let mut bytes = descriptor(CONVENTIONAL, 0x4000, 3, stride);
        bytes.extend(descriptor(CONVENTIONAL, 0x8000, 5, stride));
        assert_eq!(parse(&bytes, stride), Ok(8));
    }

    #[test]
    fn rejects_map_length_not_divisible_by_descriptor_stride() {
        let mut bytes = descriptor(CONVENTIONAL, 0x4000, 1, PREFIX);
        bytes.push(0);
        assert_eq!(parse(&bytes, PREFIX), Err(MapError::InvalidLength));
    }

    #[test]
    fn rejects_descriptor_that_extends_past_supplied_buffer() {
        let bytes = descriptor(CONVENTIONAL, 0x4000, 1, PREFIX);
        assert_eq!(parse(&bytes, PREFIX + 8), Err(MapError::DescriptorOutOfBounds));
    }

    #[test]
    fn rejects_misaligned_physical_start() {
        let bytes = descriptor(CONVENTIONAL, 0x4001, 1, PREFIX);
        assert_eq!(parse(&bytes, PREFIX), Err(MapError::MisalignedPhysicalAddress));
    }

    #[test]
    fn rejects_zero_page_descriptor() {
        let bytes = descriptor(CONVENTIONAL, 0x4000, 0, PREFIX);
        assert_eq!(parse(&bytes, PREFIX), Err(MapError::EmptyRegion));
    }

    #[test]
    fn rejects_conventional_page_count_overflow() {
        assert_eq!(
            accumulate_conventional_pages(u64::MAX - 1, 2),
            Err(MapError::PageCountOverflow)
        );
    }

    #[test]
    fn ignores_non_conventional_memory_types() {
        let mut bytes = descriptor(RESERVED, 0x4000, 99, PREFIX);
        bytes.extend(descriptor(CONVENTIONAL, 0x8000, 7, PREFIX));
        assert_eq!(parse(&bytes, PREFIX), Ok(7));
    }

    #[test]
    fn totals_valid_conventional_memory_descriptors() {
        let mut bytes = descriptor(CONVENTIONAL, 0x4000, 2, PREFIX);
        bytes.extend(descriptor(RESERVED, 0x8000, 11, PREFIX));
        bytes.extend(descriptor(CONVENTIONAL, 0x10000, 13, PREFIX));
        assert_eq!(parse(&bytes, PREFIX), Ok(15));
    }
}
