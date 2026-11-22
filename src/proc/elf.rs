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
    pub stack_top_pa: u64,
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
    let mut stack_top_pa = 0u64;
    for p in 0..USER_STACK_PAGES {
        let va = USER_STACK_BASE + (p as u64) * PAGE_SIZE;
        let pa = mapper.pmm_mut().allocate_page().ok_or("PMM out of memory for user stack")?;
        if p == USER_STACK_PAGES - 1 {
            stack_top_pa = pa;
        }
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
        stack_top_pa,
    })
}

/// Sets up argc / argv strings and pointer array on the top page of the user stack.
/// Returns (argc, argv_user_va).
/// The resulting `proc.user_sp` is 16-byte aligned.
pub unsafe fn setup_user_stack_args(
    proc: &mut ElfProcess,
    args: &[&str],
    hhdm_base: u64,
) -> Result<(u64, u64), &'static str> {
    if args.is_empty() {
        return Ok((0, 0));
    }
    if proc.stack_top_pa == 0 {
        return Err("User stack frame not allocated");
    }

    let page_va = hhdm_base + proc.stack_top_pa;
    let page_bytes = core::slice::from_raw_parts_mut(page_va as *mut u8, PAGE_SIZE as usize);
    let stack_page_base_user_va = USER_STACK_TOP - PAGE_SIZE;

    // We write strings from the top of the page downwards
    let mut curr_offset = PAGE_SIZE as usize;
    let mut str_user_vas = [0u64; 32];
    let argc = args.len().min(32);

    for (i, arg) in args[..argc].iter().enumerate() {
        let bytes = arg.as_bytes();
        let needed = bytes.len() + 1; // with null terminator
        if curr_offset < needed + 256 {
            return Err("Argument strings exceed user stack page space");
        }
        curr_offset -= needed;
        page_bytes[curr_offset..curr_offset + bytes.len()].copy_from_slice(bytes);
        page_bytes[curr_offset + bytes.len()] = 0; // null terminator
        str_user_vas[i] = stack_page_base_user_va + curr_offset as u64;
    }

    // Below strings, allocate the argv pointer array:
    // (argc + 1) pointers of 8 bytes each, followed by 16-byte alignment
    let argv_len = (argc + 1) * core::mem::size_of::<u64>();
    if curr_offset < argv_len + 128 {
        return Err("Argument pointers exceed user stack page space");
    }
    curr_offset -= argv_len;
    // Align down to 16-byte boundary
    curr_offset &= !0xF;

    let argv_user_va = stack_page_base_user_va + curr_offset as u64;
    let argv_slice = core::slice::from_raw_parts_mut(
        page_bytes.as_mut_ptr().add(curr_offset) as *mut u64,
        argc + 1,
    );
    for i in 0..argc {
        argv_slice[i] = str_user_vas[i];
    }
    argv_slice[argc] = 0; // NULL terminator

    proc.user_sp = argv_user_va;
    Ok((argc as u64, argv_user_va))
}

/// Invalidate TLB entries for a specific ASID across all inner shareable cores.
pub fn invalidate_asid(asid: u16) {
    #[cfg(target_os = "uefi")]
    unsafe {
        let asid_reg = (asid as u64) << 48;
        core::arch::asm!(
            "tlbi aside1is, {0}",
            "dsb ish",
            "isb",
            in(reg) asid_reg,
            options(nostack)
        );
    }
    #[cfg(not(target_os = "uefi"))]
    let _ = asid;
}

/// Recursively tears down and frees all physical frames (leaf pages + page tables)
/// belonging to an ELF user process address space.
pub unsafe fn destroy_user_process<const N: usize>(
    proc: &ElfProcess,
    pmm: &mut PhysicalPageAllocator<N>,
    hhdm_base: u64,
) {
    destroy_user_page_table(proc.ttbr0_root, hhdm_base, pmm);
    let asid = (proc.ttbr0_root >> 48) as u16;
    if asid > 0 {
        invalidate_asid(asid);
    }
}

/// Recursively traverses a 4-level translation table from `root_pa` and returns
/// all mapped leaf pages and intermediate table pages to `pmm`.
pub unsafe fn destroy_user_page_table<const N: usize>(
    raw_root_pa: u64,
    hhdm_base: u64,
    pmm: &mut PhysicalPageAllocator<N>,
) {
    let root_pa = raw_root_pa & 0x0000_FFFF_FFFF_FFFF;
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
            let _ = pmm.free_page(l2_pa);
        }
        let _ = pmm.free_page(l1_pa);
    }
    let _ = pmm.free_page(root_pa);
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::boxed::Box;
    use crate::pmm::PhysicalPageAllocator;

    fn make_test_map(base_paddr: u64, pages: u64) -> std::vec::Vec<u8> {
        let mut desc = std::vec![0u8; 40];
        desc[0..4].copy_from_slice(&7u32.to_le_bytes()); // ConventionalMemory = 7
        desc[8..16].copy_from_slice(&base_paddr.to_le_bytes());
        desc[16..24].copy_from_slice(&base_paddr.to_le_bytes());
        desc[24..32].copy_from_slice(&pages.to_le_bytes());
        desc
    }

    #[test]
    fn elf_destroy_user_process_zero_leak_and_double_free() {
        #[repr(align(4096))]
        struct Mem([u8; 128 * 4096]);
        let mut mem = Box::new(Mem([0; 128 * 4096]));
        let base_ptr = mem.0.as_mut_ptr() as u64;

        let mut pmm = PhysicalPageAllocator::<2>::new();
        let map = make_test_map(0, 128);
        unsafe {
            pmm.init_from_memory_map(map.as_ptr(), map.len(), 40, &[]).unwrap();
        }

        let initial_free = pmm.free_pages();

        // Build a minimal valid ELF64 with 1 PT_LOAD segment
        let mut elf_buf = [0u8; 256];
        elf_buf[0..4].copy_from_slice(&ELF_MAGIC);
        elf_buf[4] = ELF_CLASS_64;
        elf_buf[5] = ELF_DATA_2LSB;
        elf_buf[6] = 1;
        elf_buf[18..20].copy_from_slice(&EM_AARCH64.to_le_bytes());
        elf_buf[24..32].copy_from_slice(&0x0040_0000u64.to_le_bytes()); // e_entry
        elf_buf[32..40].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
        elf_buf[52..54].copy_from_slice(&64u16.to_le_bytes()); // e_ehsize
        elf_buf[54..56].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        elf_buf[56..58].copy_from_slice(&1u16.to_le_bytes());  // e_phnum

        let ph_off = 64;
        elf_buf[ph_off..ph_off + 4].copy_from_slice(&PT_LOAD.to_le_bytes());
        elf_buf[ph_off + 4..ph_off + 8].copy_from_slice(&(PF_R | PF_X).to_le_bytes());
        elf_buf[ph_off + 8..ph_off + 16].copy_from_slice(&120u64.to_le_bytes()); // p_offset
        elf_buf[ph_off + 16..ph_off + 24].copy_from_slice(&0x0040_0000u64.to_le_bytes()); // p_vaddr
        elf_buf[ph_off + 24..ph_off + 32].copy_from_slice(&0x0040_0000u64.to_le_bytes()); // p_paddr
        elf_buf[ph_off + 32..ph_off + 40].copy_from_slice(&64u64.to_le_bytes()); // p_filesz
        elf_buf[ph_off + 40..ph_off + 48].copy_from_slice(&64u64.to_le_bytes()); // p_memsz
        elf_buf[ph_off + 48..ph_off + 56].copy_from_slice(&4096u64.to_le_bytes()); // p_align

        let proc = load_elf64(&elf_buf, &mut pmm, base_ptr, 128 * 4096).unwrap();
        assert!(pmm.free_pages() < initial_free);

        unsafe {
            destroy_user_process(&proc, &mut pmm, base_ptr);
        }

        assert_eq!(
            pmm.free_pages(),
            initial_free,
            "All pages must be 100% restored with zero leak and zero double-free"
        );
    }

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

        // Truncated ELF buffer test
        assert_eq!(Elf64Header::parse(&buf[..32]), Err("ELF buffer smaller than 64-byte header"));

        // Non-64-bit class test (32-bit = 1)
        let mut class32_buf = buf;
        class32_buf[4] = 1;
        assert_eq!(Elf64Header::parse(&class32_buf), Err("ELF is not 64-bit"));

        // Big-endian test (MSB = 2)
        let mut be_buf = buf;
        be_buf[5] = 2;
        assert_eq!(Elf64Header::parse(&be_buf), Err("ELF is not little-endian"));

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
    fn elf_load_rejects_missing_program_headers() {
        let mut mem = Box::new([0u8; 16 * 4096]);
        let base_ptr = mem.as_mut_ptr() as u64;
        let mut pmm = PhysicalPageAllocator::<2>::new();
        let map = make_test_map(0, 16);
        unsafe {
            pmm.init_from_memory_map(map.as_ptr(), map.len(), 40, &[]).unwrap();
        }

        let mut buf = [0u8; 64];
        buf[0..4].copy_from_slice(&ELF_MAGIC);
        buf[4] = ELF_CLASS_64;
        buf[5] = ELF_DATA_2LSB;
        buf[6] = 1;
        buf[18..20].copy_from_slice(&EM_AARCH64.to_le_bytes());
        buf[32..40].copy_from_slice(&0u64.to_le_bytes()); // e_phoff = 0
        buf[56..58].copy_from_slice(&0u16.to_le_bytes()); // e_phnum = 0

        assert_eq!(load_elf64(&buf, &mut pmm, base_ptr, 16 * 4096).map(|_| ()), Err("ELF has no program headers"));
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

    #[test]
    fn test_setup_user_stack_args() {
        let mut mock_stack_page = [0u8; 4096];
        let pa = mock_stack_page.as_mut_ptr() as u64;
        let hhdm = 0u64; // pa is direct pointer
        let mut proc = ElfProcess {
            entry_va: 0x400000,
            user_sp: USER_STACK_TOP,
            ttbr0_root: 0x1000,
            stack_top_pa: pa,
        };

        let args = ["hello", "world", "genesis"];
        let (argc, argv_va) = unsafe { setup_user_stack_args(&mut proc, &args, hhdm).unwrap() };

        assert_eq!(argc, 3);
        assert_eq!(argv_va % 16, 0); // 16-byte alignment
        assert_eq!(proc.user_sp, argv_va);

        // Verify argv array in mock_stack_page
        let stack_page_base_user_va = USER_STACK_TOP - PAGE_SIZE;
        let argv_offset = (argv_va - stack_page_base_user_va) as usize;
        let argv_ptrs = unsafe {
            core::slice::from_raw_parts(mock_stack_page.as_ptr().add(argv_offset) as *const u64, 4)
        };

        assert_ne!(argv_ptrs[0], 0);
        assert_ne!(argv_ptrs[1], 0);
        assert_ne!(argv_ptrs[2], 0);
        assert_eq!(argv_ptrs[3], 0); // NULL terminator

        // Check each string
        for (i, expected) in args.iter().enumerate() {
            let s_offset = (argv_ptrs[i] - stack_page_base_user_va) as usize;
            let s_len = expected.len();
            assert_eq!(&mock_stack_page[s_offset..s_offset + s_len], expected.as_bytes());
            assert_eq!(mock_stack_page[s_offset + s_len], 0); // Null terminator
        }

        // Empty args test
        let (empty_argc, empty_argv) = unsafe { setup_user_stack_args(&mut proc, &[], hhdm).unwrap() };
        assert_eq!(empty_argc, 0);
        assert_eq!(empty_argv, 0);
    }

    #[test]
    fn test_sh_elf_validation() {
        if !std::path::Path::new("build/esp/SH.ELF").exists() {
            let status = std::process::Command::new("python3")
                .arg("tools/gen-stage25-assets.py")
                .status()
                .expect("Failed to run python3 tools/gen-stage25-assets.py");
            assert!(status.success(), "gen-stage25-assets.py failed");
        }

        let elf_bytes = std::fs::read("build/esp/SH.ELF").expect("Failed to read build/esp/SH.ELF");
        let hdr = Elf64Header::parse(&elf_bytes).expect("SH.ELF must parse as valid ELF64");
        assert_eq!(hdr.ident[0..4], ELF_MAGIC);
        assert_eq!(hdr.ident[4], ELF_CLASS_64);
        assert_eq!(hdr.ident[5], ELF_DATA_2LSB);
        assert_eq!(hdr.e_machine, EM_AARCH64);
        assert_eq!(hdr.e_type, 2); // ET_EXEC
        assert_eq!(hdr.e_entry, 0x0040_0080);
        assert!(hdr.e_phnum >= 1);

        let mut prev_end = 0u64;
        let mut load_count = 0;
        for i in 0..(hdr.e_phnum as usize) {
            let off = (hdr.e_phoff as usize) + i * (hdr.e_phentsize as usize);
            let ph = Elf64Phdr::parse(&elf_bytes, off).expect("Phdr must parse");
            if ph.p_type == PT_LOAD {
                load_count += 1;
                assert!(ph.p_vaddr >= 0x0040_0000);
                assert!(ph.p_vaddr < 0x0000_8000_0000_0000);
                assert!(ph.p_filesz > 0);
                assert!(ph.p_memsz >= ph.p_filesz);
                assert!(ph.p_vaddr >= prev_end, "PT_LOAD segments must be non-overlapping");
                prev_end = ph.p_vaddr + ph.p_memsz;
            }
        }
        assert!(load_count >= 1);
    }
}
