//! ELF64 Loader for Project Genesis (Stage 9A).
//!
//! Parses 64-bit ELF headers and PT_LOAD program headers,
//! validates architecture (EM_AARCH64 = 183), allocates KERNEL_PMM frames,
//! copies segment data and zeroes .bss, sets up a 16 KiB user stack,
//! and maps pages into a per-process TTBR0_EL1 page table with strict W ^ X permissions.

use crate::{
    pmm::PhysicalPageAllocator,
    vmm::{
        dynamic_mapper::DynamicMapper,
        page_table::{PagePermissions, PAGE_SIZE},
    },
};

pub const ELF_MAGIC: [u8; 4] = [0x7F, b'E', b'L', b'F'];
pub const ELF_CLASS_64: u8 = 2;
pub const ELF_DATA_2LSB: u8 = 1;
pub const EM_AARCH64: u16 = 183;
pub const PT_LOAD: u32 = 1;

pub const PF_X: u32 = 1;
pub const PF_W: u32 = 2;
pub const PF_R: u32 = 4;

pub const USER_STACK_BASE: u64 = 0x0000_7FFF_F000_0000;
pub const USER_STACK_PAGES: usize = 4; // 16 KiB
pub const USER_STACK_TOP: u64 = USER_STACK_BASE + (USER_STACK_PAGES as u64) * PAGE_SIZE;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Elf64Header {
    pub ident: [u8; 16],
    pub e_type: u16,
    pub e_machine: u16,
    pub e_version: u32,
    pub e_entry: u64,
    pub e_phoff: u64,
    pub e_shoff: u64,
    pub e_flags: u32,
    pub e_ehsize: u16,
    pub e_phentsize: u16,
    pub e_phnum: u16,
    pub e_shentsize: u16,
    pub e_shnum: u16,
    pub e_shstrndx: u16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Elf64Phdr {
    pub p_type: u32,
    pub p_flags: u32,
    pub p_offset: u64,
    pub p_vaddr: u64,
    pub p_paddr: u64,
    pub p_filesz: u64,
    pub p_memsz: u64,
    pub p_align: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct ElfProcess {
    pub entry_va: u64,
    pub user_sp: u64,
    pub ttbr0_root: u64,
}

impl Elf64Header {
    pub fn parse(bytes: &[u8]) -> Result<Self, &'static str> {
        if bytes.len() < 64 {
            return Err("ELF buffer smaller than 64-byte header");
        }
        if &bytes[0..4] != &ELF_MAGIC {
            return Err("Invalid ELF magic");
        }
        if bytes[4] != ELF_CLASS_64 {
            return Err("ELF is not 64-bit");
        }
        if bytes[5] != ELF_DATA_2LSB {
            return Err("ELF is not little-endian");
        }
        let e_machine = u16::from_le_bytes([bytes[18], bytes[19]]);
        if e_machine != EM_AARCH64 {
            return Err("ELF machine is not AArch64 (183)");
        }
        let hdr = unsafe { core::ptr::read_unaligned(bytes.as_ptr() as *const Elf64Header) };
        Ok(hdr)
    }
}

impl Elf64Phdr {
    pub fn parse(bytes: &[u8], offset: usize) -> Result<Self, &'static str> {
        if offset.checked_add(56).ok_or("Offset overflow")? > bytes.len() {
            return Err("Program header extends beyond ELF data");
        }
        let phdr = unsafe { core::ptr::read_unaligned(bytes.as_ptr().add(offset) as *const Elf64Phdr) };
        Ok(phdr)
    }
}

/// Loads a 64-bit AArch64 ELF binary into a newly created TTBR0_EL1 address space.
pub fn load_elf64<const N: usize>(
    elf_data: &[u8],
    pmm: &mut PhysicalPageAllocator<N>,
    hhdm_base: u64,
    max_pa: u64,
) -> Result<ElfProcess, &'static str> {
    let hdr = Elf64Header::parse(elf_data)?;

    if hdr.e_phoff == 0 || hdr.e_phnum == 0 {
        return Err("ELF has no program headers");
    }

    // Allocate root L0 translation table for TTBR0_EL1
    let root_pa = pmm.allocate_page().ok_or("Failed to allocate TTBR0 root table")?;
    unsafe {
        let root_ptr = (hhdm_base + root_pa) as *mut [u64; 512];
        (*root_ptr).fill(0);
    }

    let mut mapper = DynamicMapper::new(root_pa, max_pa, pmm, hhdm_base);

    // Map each PT_LOAD segment
    for i in 0..(hdr.e_phnum as usize) {
        let ph_offset = (hdr.e_phoff as usize)
            .checked_add(i * (hdr.e_phentsize as usize))
            .ok_or("Program header offset calculation overflow")?;
        let ph = Elf64Phdr::parse(elf_data, ph_offset)?;
        if ph.p_type != PT_LOAD {
            continue;
        }
        if ph.p_memsz == 0 {
            continue;
        }

        let seg_start = ph.p_vaddr;
        let seg_end = seg_start.checked_add(ph.p_memsz).ok_or("Segment address overflow")?;
        // Lower-half user space boundary check: must be < 0x0000_8000_0000_0000
        if seg_end > 0x0000_8000_0000_0000 {
            return Err("ELF segment virtual address outside lower-half user space");
        }

        // Strict W ^ X Permissions
        let perms = if (ph.p_flags & PF_X) != 0 {
            PagePermissions::user_code()
        } else if (ph.p_flags & PF_W) != 0 {
            PagePermissions::user_data()
        } else {
            PagePermissions::user_rodata()
        };

        let page_start = seg_start & !(PAGE_SIZE - 1);
        let page_end = (seg_end + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        let num_pages = ((page_end - page_start) / PAGE_SIZE) as usize;

        let file_start = ph.p_offset as usize;
        let file_end = file_start
            .checked_add(ph.p_filesz as usize)
            .ok_or("File size overflow")?;
        if file_end > elf_data.len() {
            return Err("ELF segment extends beyond file length");
        }

        for p in 0..num_pages {
            let va = page_start + (p as u64) * PAGE_SIZE;
            let pa = mapper.pmm_mut().allocate_page().ok_or("PMM out of memory for user segment")?;
            unsafe {
                let frame_ptr = (hhdm_base + pa) as *mut u8;
                frame_ptr.write_bytes(0, PAGE_SIZE as usize);

                // Copy overlapping portion from file
                let cur_page_end = va + PAGE_SIZE;
                let copy_start_va = va.max(seg_start);
                let copy_end_va = cur_page_end.min(seg_start + ph.p_filesz);
                if copy_start_va < copy_end_va {
                    let copy_len = (copy_end_va - copy_start_va) as usize;
                    let file_off = file_start + ((copy_start_va - seg_start) as usize);
                    let dest_off = (copy_start_va - va) as usize;
                    core::ptr::copy_nonoverlapping(
                        elf_data.as_ptr().add(file_off),
                        frame_ptr.add(dest_off),
                        copy_len,
                    );
                }
            }

            mapper.map_page(va, pa, perms).map_err(|_| "Failed to map user segment page")?;
        }
    }

    // Map 16 KiB User Stack at 0x0000_7FFF_F000_0000
    for p in 0..USER_STACK_PAGES {
        let va = USER_STACK_BASE + (p as u64) * PAGE_SIZE;
        let pa = mapper.pmm_mut().allocate_page().ok_or("PMM out of memory for user stack")?;
        unsafe {
            let frame_ptr = (hhdm_base + pa) as *mut u8;
            frame_ptr.write_bytes(0, PAGE_SIZE as usize);
        }
        mapper
            .map_page(va, pa, PagePermissions::user_data())
            .map_err(|_| "Failed to map user stack page")?;
    }

    Ok(ElfProcess {
        entry_va: hdr.e_entry,
        user_sp: USER_STACK_TOP,
        ttbr0_root: root_pa,
    })
}

/// Recursively tears down and frees all physical frames (leaf pages + page tables)
/// belonging to an ELF user process address space.
pub unsafe fn destroy_user_process<const N: usize>(
    proc: &ElfProcess,
    pmm: &mut PhysicalPageAllocator<N>,
    hhdm_base: u64,
) {
    destroy_user_page_table(proc.ttbr0_root, hhdm_base, pmm);
}

/// Recursively traverses a 4-level translation table from `root_pa` and returns
/// all mapped leaf pages and intermediate table pages to `pmm`.
pub unsafe fn destroy_user_page_table<const N: usize>(
    root_pa: u64,
    hhdm_base: u64,
    pmm: &mut PhysicalPageAllocator<N>,
) {
    let l0 = (hhdm_base + root_pa) as *const [u64; 512];
    for &l0_entry in (*l0).iter() {
        if !crate::vmm::page_table::is_valid(l0_entry) || !crate::vmm::page_table::is_table(l0_entry) {
            continue;
        }
        let l1_pa = crate::vmm::page_table::output_address(l0_entry);
        let l1 = (hhdm_base + l1_pa) as *const [u64; 512];
        for &l1_entry in (*l1).iter() {
            if !crate::vmm::page_table::is_valid(l1_entry) || !crate::vmm::page_table::is_table(l1_entry) {
                continue;
            }
            let l2_pa = crate::vmm::page_table::output_address(l1_entry);
            let l2 = (hhdm_base + l2_pa) as *const [u64; 512];
            for &l2_entry in (*l2).iter() {
                if !crate::vmm::page_table::is_valid(l2_entry) || !crate::vmm::page_table::is_table(l2_entry) {
                    continue;
                }
                let l3_pa = crate::vmm::page_table::output_address(l2_entry);
                let l3 = (hhdm_base + l3_pa) as *const [u64; 512];
                for &l3_entry in (*l3).iter() {
                    if crate::vmm::page_table::is_valid(l3_entry) {
                        let leaf_pa = crate::vmm::page_table::output_address(l3_entry);
                        let _ = pmm.free_page(leaf_pa);
                    }
                }
                let _ = pmm.free_page(l3_pa);
            }
            let _ = pmm.free_page(l1_pa);
        }
        let _ = pmm.free_page(l0_pa_intermediate(l0_entry));
    }
    let _ = pmm.free_page(root_pa);
}

#[inline]
fn l0_pa_intermediate(entry: u64) -> u64 {
    crate::vmm::page_table::output_address(entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elf64_header_parsing_and_validation() {
        let mut buf = [0u8; 64];
        buf[0..4].copy_from_slice(&ELF_MAGIC);
        buf[4] = ELF_CLASS_64;
        buf[5] = ELF_DATA_2LSB;
        buf[6] = 1; // version
        buf[18..20].copy_from_slice(&EM_AARCH64.to_le_bytes());
        buf[24..32].copy_from_slice(&0x0000_0000_0040_0000u64.to_le_bytes()); // e_entry
        buf[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
        buf[52..54].copy_from_slice(&64u16.to_le_bytes()); // e_ehsize
        buf[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        buf[56..58].copy_from_slice(&1u16.to_le_bytes());  // e_phnum

        let hdr = Elf64Header::parse(&buf).unwrap();
        assert_eq!(hdr.e_entry, 0x0000_0000_0040_0000);
        assert_eq!(hdr.e_phoff, 64);
        assert_eq!(hdr.e_phentsize, 56);
        assert_eq!(hdr.e_phnum, 1);

        // Invalid magic test
        let mut bad_buf = buf;
        bad_buf[0] = 0;
        assert_eq!(Elf64Header::parse(&bad_buf), Err("Invalid ELF magic"));

        // Non-AArch64 test (x86_64 = 62)
        let mut x86_buf = buf;
        x86_buf[18..20].copy_from_slice(&62u16.to_le_bytes());
        assert_eq!(Elf64Header::parse(&x86_buf), Err("ELF machine is not AArch64 (183)"));
    }

    #[test]
    fn elf64_phdr_parsing() {
        let mut buf = [0u8; 120];
        let off = 64;
        buf[off..off + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
        buf[off + 4..off + 8].copy_from_slice(&(PF_R | PF_X).to_le_bytes());
        buf[off + 8..off + 16].copy_from_slice(&0u64.to_le_bytes()); // offset
        buf[off + 16..off + 24].copy_from_slice(&0x0040_0000u64.to_le_bytes()); // vaddr
        buf[off + 32..off + 40].copy_from_slice(&120u64.to_le_bytes()); // filesz
        buf[off + 40..off + 48].copy_from_slice(&256u64.to_le_bytes()); // memsz

        let phdr = Elf64Phdr::parse(&buf, off).unwrap();
        assert_eq!(phdr.p_type, PT_LOAD);
        assert_eq!(phdr.p_flags, PF_R | PF_X);
        assert_eq!(phdr.p_vaddr, 0x0040_0000);
        assert_eq!(phdr.p_filesz, 120);
        assert_eq!(phdr.p_memsz, 256);
    }
}
