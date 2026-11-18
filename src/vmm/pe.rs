//! Dynamic PE32+ (ARM64 UEFI) Section & Header Parser.
//!
//! Parses loaded PE32+ image headers and sections at runtime, extracting:
//! - Total image virtual size and page count
//! - Section RVAs, sizes, page counts, and permissions (RO/RW, X/NX)
//! - Header page count

use crate::vmm::page_table::PAGE_SIZE;

pub const MAX_PE_SECTIONS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeSection {
    pub name: [u8; 8],
    pub virtual_address: u64, // RVA
    pub virtual_size: u64,
    pub page_count: usize,
    pub is_executable: bool,
    pub is_writable: bool,
    pub is_readable: bool,
}

impl PeSection {
    pub fn name_bytes(&self) -> &[u8] {
        let mut len = 0;
        while len < 8 && self.name[len] != 0 {
            len += 1;
        }
        &self.name[..len]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParsedPeImage {
    pub image_base: u64,
    pub size_of_image: u64,
    pub total_pages: usize,
    pub header_pages: usize,
    pub sections: [Option<PeSection>; MAX_PE_SECTIONS],
    pub section_count: usize,
}

impl ParsedPeImage {
    pub fn find_section(&self, prefix: &[u8]) -> Option<&PeSection> {
        for sec_opt in &self.sections[..self.section_count] {
            if let Some(sec) = sec_opt {
                if sec.name_bytes().starts_with(prefix) {
                    return Some(sec);
                }
            }
        }
        None
    }

    /// Generates a mock parsed PE representation matching Genesis default layout.
    /// Useful for host unit tests where raw memory at KERNEL_IMAGE_PADDR cannot be read.
    pub fn mock_genesis() -> Self {
        let mut sections = [None; MAX_PE_SECTIONS];
        let mut count = 0;

        // .text (19 pages)
        sections[count] = Some(PeSection {
            name: *b".text\0\0\0",
            virtual_address: 0x1000,
            virtual_size: 19 * PAGE_SIZE,
            page_count: 19,
            is_executable: true,
            is_writable: false,
            is_readable: true,
        });
        count += 1;

        // .rdata (2 pages)
        sections[count] = Some(PeSection {
            name: *b".rdata\0\0",
            virtual_address: 0x1_4000,
            virtual_size: 2 * PAGE_SIZE,
            page_count: 2,
            is_executable: false,
            is_writable: false,
            is_readable: true,
        });
        count += 1;

        // .data (603 pages)
        sections[count] = Some(PeSection {
            name: *b".data\0\0\0",
            virtual_address: 0x1_6000,
            virtual_size: 603 * PAGE_SIZE,
            page_count: 603,
            is_executable: false,
            is_writable: true,
            is_readable: true,
        });
        count += 1;

        // .reloc (1 page)
        sections[count] = Some(PeSection {
            name: *b".reloc\0\0",
            virtual_address: 0x27_1000,
            virtual_size: 1 * PAGE_SIZE,
            page_count: 1,
            is_executable: false,
            is_writable: false,
            is_readable: true,
        });
        count += 1;

        Self {
            image_base: 0x4800_0000,
            size_of_image: 626 * PAGE_SIZE,
            total_pages: 626,
            header_pages: 1,
            sections,
            section_count: count,
        }
    }
}

/// Parses a PE32+ image at `base_paddr` in memory.
pub unsafe fn parse_pe_image(base: u64) -> Result<ParsedPeImage, &'static str> {
    if base == 0 || base % PAGE_SIZE != 0 {
        return Err("Invalid PE image base");
    }

    // 1. DOS Header
    let dos_magic = core::ptr::read_volatile(base as *const u16);
    if dos_magic != 0x5A4D {
        return Err("Invalid DOS magic");
    }

    let e_lfanew = core::ptr::read_volatile((base + 0x3C) as *const u32) as u64;
    if e_lfanew < 0x40 || e_lfanew > 0x1000 {
        return Err("Invalid e_lfanew offset");
    }

    // 2. PE Signature
    let pe_sig = core::ptr::read_volatile((base + e_lfanew) as *const u32);
    if pe_sig != 0x0000_4550 {
        return Err("Invalid PE signature");
    }

    // 3. COFF File Header (20 bytes at e_lfanew + 4)
    let coff_offset = base + e_lfanew + 4;
    let number_of_sections = core::ptr::read_volatile((coff_offset + 2) as *const u16) as usize;
    if number_of_sections == 0 || number_of_sections > MAX_PE_SECTIONS {
        return Err("Section count out of range");
    }

    let opt_hdr_size = core::ptr::read_volatile((coff_offset + 16) as *const u16) as u64;

    // 4. Optional Header (at e_lfanew + 24)
    let opt_offset = base + e_lfanew + 24;
    let opt_magic = core::ptr::read_volatile(opt_offset as *const u16);
    if opt_magic != 0x020B {
        return Err("Not a PE32+ (64-bit) executable");
    }

    // In PE32+, SizeOfImage is at offset 56 in optional header
    let size_of_image = core::ptr::read_volatile((opt_offset + 56) as *const u32) as u64;
    let total_pages = ((size_of_image + PAGE_SIZE - 1) / PAGE_SIZE) as usize;

    // 5. Section Table (at e_lfanew + 24 + opt_hdr_size)
    let sec_table_offset = opt_offset + opt_hdr_size;
    let mut sections = [None; MAX_PE_SECTIONS];

    let mut first_sec_rva = size_of_image;

    for i in 0..number_of_sections {
        let sec_ptr = sec_table_offset + (i as u64) * 40;
        let mut name = [0u8; 8];
        for b in 0..8 {
            name[b] = core::ptr::read_volatile((sec_ptr + b as u64) as *const u8);
        }

        let virt_size = core::ptr::read_volatile((sec_ptr + 8) as *const u32) as u64;
        let virt_addr = core::ptr::read_volatile((sec_ptr + 12) as *const u32) as u64;
        let chars = core::ptr::read_volatile((sec_ptr + 36) as *const u32);

        if virt_addr < first_sec_rva {
            first_sec_rva = virt_addr;
        }

        let page_count = ((virt_size + PAGE_SIZE - 1) / PAGE_SIZE) as usize;
        let is_executable = (chars & 0x2000_0000) != 0;
        let is_writable = (chars & 0x8000_0000) != 0;
        let is_readable = (chars & 0x4000_0000) != 0;

        sections[i] = Some(PeSection {
            name,
            virtual_address: virt_addr,
            virtual_size: virt_size,
            page_count,
            is_executable,
            is_writable,
            is_readable,
        });
    }

    let header_pages = if first_sec_rva > 0 && first_sec_rva <= size_of_image {
        ((first_sec_rva + PAGE_SIZE - 1) / PAGE_SIZE) as usize
    } else {
        1
    };

    Ok(ParsedPeImage {
        image_base: base,
        size_of_image,
        total_pages,
        header_pages,
        sections,
        section_count: number_of_sections,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_pe_parsing_and_lookup() {
        let pe = ParsedPeImage::mock_genesis();
        assert_eq!(pe.total_pages, 626);
        assert_eq!(pe.header_pages, 1);
        assert_eq!(pe.section_count, 4);

        let text = pe.find_section(b".text").unwrap();
        assert_eq!(text.virtual_address, 0x1000);
        assert_eq!(text.page_count, 19);
        assert!(text.is_executable);
        assert!(!text.is_writable);

        let rdata = pe.find_section(b".rdata").unwrap();
        assert_eq!(rdata.virtual_address, 0x1_4000);
        assert_eq!(rdata.page_count, 2);
        assert!(!rdata.is_executable);
        assert!(!rdata.is_writable);

        let data = pe.find_section(b".data").unwrap();
        assert_eq!(data.virtual_address, 0x1_6000);
        assert_eq!(data.page_count, 603);
        assert!(!data.is_executable);
        assert!(data.is_writable);

        let reloc = pe.find_section(b".reloc").unwrap();
        assert_eq!(reloc.virtual_address, 0x27_1000);
        assert_eq!(reloc.page_count, 1);
        assert!(!reloc.is_executable);
        assert!(!reloc.is_writable);
    }
}
