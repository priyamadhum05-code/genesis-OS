#![no_std]
#![no_main]

extern crate alloc;
use alloc::{boxed::Box, vec::Vec};

use core::{arch::asm, panic::PanicInfo, ptr};
use genesis_stage1::{
    arch::{
        parse_fdt_devices, stage4_arm_timer_interval, stage4_get_ticks, stage4_init_runtime,
        stage4_vectors_va, ArmTimer, Gicv3,
    },
    memory_map,
    pmm::{self, PhysicalPageAllocator, ReservedRange, KERNEL_PMM_WORDS},
    vmm::{
        get_stage3c3_layout, get_transition_layout, parse_pe_image, run_stage3c1_preflight,
        stage3c2_mmu_transition, stage3c3_jump_higher_half, stage3c3_vectors_start,
        verify_firmware_identity_mapping, verify_higher_half_pe_header, verify_stack_usability,
        DormantTablePool, DynamicMapper, PagePermissions, GENESIS_MAIR_VALUE, GENESIS_TCR_VALUE,
        HHDM_BASE, KERNEL_IMAGE_PADDR, KERNEL_RESERVE_SIZE, KERNEL_VA_BASE, MMIO_VA_BASE,
        PAGE_SIZE, TRANSITION_CLEAN_START,
    },
    serial_decimal, serial_hex, serial_write,
};

#[global_allocator]
static KERNEL_HEAP: genesis_stage1::mm::LockedHeap = genesis_stage1::mm::LockedHeap::empty();

const EFI_SUCCESS: usize = 0;
const EFI_INVALID_PARAMETER: usize = 2;
const EFI_BUFFER_TOO_SMALL: usize = 5;
const FDT_GUID: Guid = Guid {
    data1: 0xb1b621d5, data2: 0xf19c, data3: 0x41a5,
    data4: [0x83, 0x0b, 0xd9, 0x15, 0x2c, 0x69, 0xaa, 0xe0],
};
const FDT_CLIENT_GUID: Guid = Guid {
    data1: 0xe11faca0, data2: 0x4710, data3: 0x4c8e,
    data4: [0xa7, 0xa2, 0x01, 0xba, 0xa2, 0x59, 0x1b, 0x4c],
};
const FDT_CAP: usize = 128 * 1024;
const STACK_SIZE: usize = 32 * 1024;

#[repr(C)] #[derive(Clone, Copy)] struct Guid { data1: u32, data2: u16, data3: u16, data4: [u8; 8] }
#[repr(C)] struct ConfigTable { guid: Guid, table: *const u8 }
#[repr(C)] struct TextOutput { reset: usize, output_string: extern "efiapi" fn(*mut TextOutput, *const u16) -> usize }
#[repr(C)] pub struct SystemTable {
    header: [u64; 3], vendor: *const u16, revision: u32, _pad: u32,
    _console_in_handle: usize, _console_in: usize, _console_out_handle: usize, console_out: *mut TextOutput,
    _stderr_handle: usize, _stderr: usize, _runtime: usize, boot_services: *const usize,
    table_count: usize, tables: *const ConfigTable,
}
#[repr(C)] struct Handoff { magic: [u8; 8], version: u32, size: u32, fdt: *const u8, fdt_size: usize, map: *const u8, map_size: usize, descriptor_size: usize, descriptor_version: u32, uart_base: usize, observed_el: u64 }
#[repr(C)] #[derive(Clone, Copy)] struct Node { parent_ac: u32, parent_sc: u32, child_ac: u32, child_sc: u32, uart: bool, disabled: bool, reg: usize, reg_size: usize }
#[repr(C)] struct FdtClient { methods: [usize; 9] }

static mut FDT_COPY: [u8; FDT_CAP] = [0; FDT_CAP];
static mut MEMORY_MAP: [u8; memory_map::BUFFER_CAPACITY] = [0; memory_map::BUFFER_CAPACITY];
static mut HANDOFF: Handoff = Handoff { magic: [0;8], version:0, size:0, fdt:ptr::null(), fdt_size:0, map:ptr::null(), map_size:0, descriptor_size:0, descriptor_version:0, uart_base:0, observed_el:0 };
static mut KERNEL_STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
static mut KERNEL_PMM: PhysicalPageAllocator<KERNEL_PMM_WORDS> = PhysicalPageAllocator::new();
static mut DORMANT_TTBR0_POOL: DormantTablePool = DormantTablePool::new();
static mut DORMANT_TTBR1_POOL: DormantTablePool = DormantTablePool::new();

#[no_mangle]
pub extern "efiapi" fn efi_main(image: usize, system_table: *mut SystemTable) -> usize {
    if system_table.is_null() { return 1; }
    unsafe {
        let st = &*system_table;
        let mut fdt_src = ptr::null();
        for i in 0..st.table_count {
            let entry = &*st.tables.add(i);
            if entry.guid.data1 == FDT_GUID.data1 && entry.guid.data2 == FDT_GUID.data2 && entry.guid.data3 == FDT_GUID.data3 && entry.guid.data4 == FDT_GUID.data4 { fdt_src = entry.table; break; }
        }
        let fdt;
        let fdt_len;
        let uart_base;
        if !fdt_src.is_null() {
            if !valid_fdt(fdt_src, FDT_CAP) { fwmsg(st, "GENESIS loader: invalid FDT\r\n"); return 2; }
            fdt_len = be32(fdt_src.add(4)) as usize;
            ptr::copy_nonoverlapping(fdt_src, ptr::addr_of_mut!(FDT_COPY) as *mut u8, fdt_len);
            fdt = ptr::addr_of!(FDT_COPY) as *const u8;
            uart_base = match find_pl011(fdt, fdt_len) { Some(v) => v, None => { fwmsg(st, "GENESIS loader: PL011 missing from FDT\r\n"); return 2; } };
        } else {
            // Some ArmVirt firmware exposes its validated FDT through EDK II's
            // FDT client protocol instead of installing EFI_DTB_TABLE_GUID.
            let locate_protocol: extern "efiapi" fn(*const Guid, *mut usize, *mut *mut FdtClient) -> usize = core::mem::transmute(*st.boot_services.add(40));
            let mut client = ptr::null_mut();
            if locate_protocol(&FDT_CLIENT_GUID, ptr::null_mut(), &mut client) != EFI_SUCCESS || client.is_null() {
                fwmsg(st, "GENESIS loader: no FDT table or FDT client\r\n"); return 2;
            }
            let find_reg: extern "efiapi" fn(*mut FdtClient, *const u8, *mut *const u8, *mut usize, *mut usize, *mut u32) -> usize = core::mem::transmute((*client).methods[5]);
            let mut reg = ptr::null(); let mut ac = 0usize; let mut sc = 0usize; let mut reg_len = 0u32;
            if find_reg(client, b"arm,pl011\0".as_ptr(), &mut reg, &mut ac, &mut sc, &mut reg_len) != EFI_SUCCESS || reg.is_null() || ac != 2 || sc != 2 || reg_len < 16 {
                fwmsg(st, "GENESIS loader: invalid PL011 FDT reg\r\n"); return 2;
            }
            let base = read_cells(reg, ac);
            let size = read_cells(reg.add(ac * 4), sc);
            if base == 0 || base > usize::MAX as u64 || size < 0x2c || base & 3 != 0 {
                fwmsg(st, "GENESIS loader: unsafe PL011 range\r\n"); return 2;
            }
            uart_base = base as usize;
            fdt = ptr::null();
            fdt_len = 0;
        }
        let map = ptr::addr_of_mut!(MEMORY_MAP) as *mut u8;

        // EFI_BOOT_SERVICES begins with a 24-byte table header (three usize
        // slots on AArch64), followed by RaiseTPL at slot 3.
        // UEFI method indices: GetMemoryMap=4 and ExitBootServices=26.
        let get_map: extern "efiapi" fn(*mut usize,*mut u8,*mut usize,*mut usize,*mut u32)->usize = core::mem::transmute(*st.boot_services.add(7));
        let exit_bs: extern "efiapi" fn(usize,usize)->usize = core::mem::transmute(*st.boot_services.add(29));
        let (mut map_len, mut key, mut desc_len, mut desc_ver) = (memory_map::BUFFER_CAPACITY, 0usize, 0usize, 0u32);
        let mut exited = false;
        for _ in 0..8 {
            map_len = memory_map::BUFFER_CAPACITY;
            let status = get_map(&mut map_len, map, &mut key, &mut desc_len, &mut desc_ver);
            if status == EFI_BUFFER_TOO_SMALL { fwmsg(st, "GENESIS loader: memory map buffer too small\r\n"); return status; }
            if status != EFI_SUCCESS { fwmsg(st, "GENESIS loader: GetMemoryMap failed\r\n"); return status; }
            let status = exit_bs(image, key);
            if status == EFI_SUCCESS { exited = true; break; }
            if status != EFI_INVALID_PARAMETER { fwmsg(st, "GENESIS loader: ExitBootServices failed\r\n"); return status; }
        }
        if !exited { fwmsg(st, "GENESIS loader: stale map retry limit\r\n"); return EFI_INVALID_PARAMETER; }
        let current_el: u64;
        asm!("mrs {0}, CurrentEL", out(reg) current_el, options(nomem, nostack, preserves_flags));
        HANDOFF = Handoff { magic:*b"GENESIS\0", version:1, size:core::mem::size_of::<Handoff>() as u32, fdt, fdt_size:fdt_len, map, map_size:map_len, descriptor_size:desc_len, descriptor_version:desc_ver, uart_base, observed_el:current_el };
        let stack_top = (ptr::addr_of_mut!(KERNEL_STACK) as usize + STACK_SIZE) & !15usize;
        asm!("mov sp, {stack}", "mov x0, {handoff}", "bl {kernel}", stack=in(reg) stack_top, handoff=in(reg) ptr::addr_of!(HANDOFF), kernel=sym kernel_entry, options(noreturn));
    }
}

unsafe fn fwmsg(st: &SystemTable, msg: &str) {
    if st.console_out.is_null() { return; }
    let mut buf = [0u16; 96];
    let mut n = 0;
    for b in msg.bytes().take(95) { buf[n] = b as u16; n += 1; }
    buf[n] = 0;
    ((*st.console_out).output_string)(st.console_out, buf.as_ptr());
}

extern "C" fn kernel_entry(h: *const Handoff) -> ! {
    unsafe {
        if h.is_null() || (*h).magic != *b"GENESIS\0" || (*h).version != 1 || (*h).size as usize != core::mem::size_of::<Handoff>() { halt(); }
        let uart = if (*h).fdt.is_null() { (*h).uart_base } else {
            if !valid_fdt((*h).fdt, (*h).fdt_size) { halt(); }
            match find_pl011((*h).fdt, (*h).fdt_size) { Some(v) if v == (*h).uart_base => v, _ => halt() }
        };
        let conventional_pages = match memory_map::conventional_pages((*h).map, (*h).map_size, (*h).descriptor_size) {
            Ok(pages) => pages,
            Err(_) => halt(),
        };

        // Initialize Physical Page Allocator
        let pmm = &mut *ptr::addr_of_mut!(KERNEL_PMM);
        let reserved = [
            ReservedRange { start: 0x4800_0000, size: 4 * 1024 * 1024 },
            ReservedRange { start: uart as u64, size: 0x1000 },
        ];
        if pmm.init_from_memory_map((*h).map, (*h).map_size, (*h).descriptor_size, &reserved).is_err() {
            halt();
        }
        let total_usable = pmm.total_usable_pages();
        let initial_free = pmm.free_pages();

        // Phase 2 Allocator verification tests (metadata & invariants only, without dereferencing)
        let p1 = match pmm.allocate_page() {
            Some(p) => p,
            None => halt(),
        };
        if pmm.free_pages() != initial_free - 1 || p1 % memory_map::EFI_PAGE_SIZE != 0 {
            halt();
        }
        if pmm.free_page(p1).is_err() || pmm.free_pages() != initial_free {
            halt();
        }
        if pmm.free_page(p1) != Err(pmm::AllocError::DoubleFree) {
            halt();
        }
        if pmm.free_page(uart as u64).is_ok() {
            halt();
        }
        let c4 = match pmm.allocate_contiguous_pages(4) {
            Some(p) => p,
            None => halt(),
        };
        if pmm.free_pages() != initial_free - 4 || c4 % memory_map::EFI_PAGE_SIZE != 0 {
            halt();
        }
        if pmm.free_contiguous_pages(c4, 4).is_err() || pmm.free_pages() != initial_free {
            halt();
        }

        // Preserve firmware baud/framing; enable only UART and transmitter.
        let cr = mmio_read(uart + 0x30);
        mmio_write(uart + 0x30, cr | 1 | (1 << 8));
        asm!("dsb sy", "isb", options(nostack, preserves_flags));
        serial_write(uart, b"GENESIS: stage1 boot ok\r\n");
        serial_write(uart, b"GENESIS: stage2 conventional pages=");
        serial_decimal(uart, conventional_pages);
        serial_write(uart, b"\r\n");
        serial_write(uart, b"GENESIS: pmm init ok total_usable_pages=");
        serial_decimal(uart, total_usable);
        serial_write(uart, b" free_pages=");
        serial_decimal(uart, initial_free);
        serial_write(uart, b"\r\n");
        let sctlr: u64;
        let tcr: u64;
        let mair: u64;
        let ttbr0: u64;
        let ttbr1: u64;
        let mmfr0: u64;
        let el: u64;
        let vbar: u64;
        let pc: u64;
        let sp: u64;
        asm!("mrs {0}, SCTLR_EL1", out(reg) sctlr, options(nomem, nostack, preserves_flags));
        asm!("mrs {0}, TCR_EL1", out(reg) tcr, options(nomem, nostack, preserves_flags));
        asm!("mrs {0}, MAIR_EL1", out(reg) mair, options(nomem, nostack, preserves_flags));
        asm!("mrs {0}, TTBR0_EL1", out(reg) ttbr0, options(nomem, nostack, preserves_flags));
        asm!("mrs {0}, TTBR1_EL1", out(reg) ttbr1, options(nomem, nostack, preserves_flags));
        asm!("mrs {0}, ID_AA64MMFR0_EL1", out(reg) mmfr0, options(nomem, nostack, preserves_flags));
        asm!("mrs {0}, CurrentEL", out(reg) el, options(nomem, nostack, preserves_flags));
        asm!("mrs {0}, VBAR_EL1", out(reg) vbar, options(nomem, nostack, preserves_flags));
        asm!("adr {0}, .", out(reg) pc, options(nomem, nostack, preserves_flags));
        asm!("mov {0}, sp", out(reg) sp, options(nomem, nostack, preserves_flags));

        let pmm_start = ptr::addr_of!(KERNEL_PMM) as usize;
        let pmm_end = pmm_start + core::mem::size_of_val(&*ptr::addr_of!(KERNEL_PMM));
        let map_start = ptr::addr_of!(MEMORY_MAP) as usize;
        let map_end = map_start + core::mem::size_of_val(&*ptr::addr_of!(MEMORY_MAP));
        let stack_start = ptr::addr_of!(KERNEL_STACK) as usize;
        let stack_end = stack_start + core::mem::size_of_val(&*ptr::addr_of!(KERNEL_STACK));
        let fdt_start = ptr::addr_of!(FDT_COPY) as usize;
        let fdt_end = fdt_start + core::mem::size_of_val(&*ptr::addr_of!(FDT_COPY));
        let pool0_start = ptr::addr_of!(DORMANT_TTBR0_POOL) as usize;
        let pool0_end = pool0_start + core::mem::size_of_val(&*ptr::addr_of!(DORMANT_TTBR0_POOL));
        let pool1_start = ptr::addr_of!(DORMANT_TTBR1_POOL) as usize;
        let pool1_end = pool1_start + core::mem::size_of_val(&*ptr::addr_of!(DORMANT_TTBR1_POOL));
        let handoff_start = ptr::addr_of!(HANDOFF) as usize;
        let handoff_end = handoff_start + core::mem::size_of_val(&*ptr::addr_of!(HANDOFF));

        // Strict Pairwise Disjointness Assertion across all static allocations
        let regions = [
            (pmm_start, pmm_end),
            (map_start, map_end),
            (stack_start, stack_end),
            (fdt_start, fdt_end),
            (pool0_start, pool0_end),
            (pool1_start, pool1_end),
            (handoff_start, handoff_end),
        ];
        for i in 0..regions.len() {
            for j in (i + 1)..regions.len() {
                let (s1, e1) = regions[i];
                let (s2, e2) = regions[j];
                if !(e1 <= s2 || e2 <= s1) {
                    serial_write(uart, b"STATIC_ALLOCATION_OVERLAP!\r\n");
                    halt();
                }
            }
        }

        let stack_bottom = ptr::addr_of!(KERNEL_STACK) as usize;
        let stack_top = stack_bottom + STACK_SIZE;
        if sp < stack_bottom as u64 || sp >= stack_top as u64 {
            serial_write(uart, b"STACK_OUT_OF_BOUNDS\r\n");
            halt();
        }

        // Stage 3C-1 Preflight Verification
        // Constructs dormant dual-root page tables in resident Genesis memory and
        // validates software address translation without touching active MMU registers.
        let pool0 = &mut *ptr::addr_of_mut!(DORMANT_TTBR0_POOL);
        pool0.init();

        let pool1 = &mut *ptr::addr_of_mut!(DORMANT_TTBR1_POOL);
        pool1.init();

        let pe = match parse_pe_image(KERNEL_IMAGE_PADDR) {
            Ok(p) => p,
            Err(e) => {
                serial_write(uart, b"PE_PARSE_FAILED: ");
                serial_write(uart, e.as_bytes());
                serial_write(uart, b"\r\n");
                halt();
            }
        };

        let preflight_res = run_stage3c1_preflight(mmfr0, pc, sp, uart as u64, pool0, pool1, &pe);
        let report = match preflight_res {
            Ok(rep) => rep,
            Err(e) => {
                serial_write(uart, b"PREFLIGHT_ERROR: ");
                serial_decimal(uart, e.code());
                serial_write(uart, b"\r\n");
                halt();
            }
        };

        serial_write(uart, b"GENESIS: vmm 3c-1 preflight ok\r\n");
        serial_write(uart, b"FIRMWARE_REGISTERS:\r\n");
        serial_write(uart, b"  CurrentEL: "); serial_hex(uart, el); serial_write(uart, b"\r\n");
        serial_write(uart, b"  VBAR_EL1:  "); serial_hex(uart, vbar); serial_write(uart, b"\r\n");
        serial_write(uart, b"  SCTLR_EL1: "); serial_hex(uart, sctlr); serial_write(uart, b"\r\n");
        serial_write(uart, b"  TCR_EL1:   "); serial_hex(uart, tcr); serial_write(uart, b"\r\n");
        serial_write(uart, b"  MAIR_EL1:  "); serial_hex(uart, mair); serial_write(uart, b"\r\n");
        serial_write(uart, b"  TTBR0_EL1: "); serial_hex(uart, ttbr0); serial_write(uart, b"\r\n");
        serial_write(uart, b"  TTBR1_EL1: "); serial_hex(uart, ttbr1); serial_write(uart, b"\r\n");
        serial_write(uart, b"  MMFR0_EL1: "); serial_hex(uart, mmfr0); serial_write(uart, b"\r\n");
        serial_write(uart, b"  PC:        "); serial_hex(uart, pc); serial_write(uart, b"\r\n");
        serial_write(uart, b"  SP:        "); serial_hex(uart, sp); serial_write(uart, b"\r\n");
        serial_write(uart, b"STATIC_ALLOCATIONS:\r\n");
        serial_write(uart, b"  KERNEL_PMM:         "); serial_hex(uart, pmm_start as u64); serial_write(uart, b".."); serial_hex(uart, pmm_end as u64); serial_write(uart, b"\r\n");
        serial_write(uart, b"  MEMORY_MAP:         "); serial_hex(uart, map_start as u64); serial_write(uart, b".."); serial_hex(uart, map_end as u64); serial_write(uart, b"\r\n");
        serial_write(uart, b"  KERNEL_STACK:       "); serial_hex(uart, stack_start as u64); serial_write(uart, b".."); serial_hex(uart, stack_end as u64); serial_write(uart, b"\r\n");
        serial_write(uart, b"  FDT_COPY:           "); serial_hex(uart, fdt_start as u64); serial_write(uart, b".."); serial_hex(uart, fdt_end as u64); serial_write(uart, b"\r\n");
        serial_write(uart, b"  DORMANT_TTBR0_POOL: "); serial_hex(uart, pool0_start as u64); serial_write(uart, b".."); serial_hex(uart, pool0_end as u64); serial_write(uart, b"\r\n");
        serial_write(uart, b"  DORMANT_TTBR1_POOL: "); serial_hex(uart, pool1_start as u64); serial_write(uart, b".."); serial_hex(uart, pool1_end as u64); serial_write(uart, b"\r\n");
        serial_write(uart, b"  HANDOFF:            "); serial_hex(uart, handoff_start as u64); serial_write(uart, b".."); serial_hex(uart, handoff_end as u64); serial_write(uart, b"\r\n");
        serial_write(uart, b"STACK_BOUNDS:\r\n");
        serial_write(uart, b"  Start:     "); serial_hex(uart, stack_bottom as u64); serial_write(uart, b"\r\n");
        serial_write(uart, b"  End:       "); serial_hex(uart, stack_top as u64); serial_write(uart, b"\r\n");
        serial_write(uart, b"PREFLIGHT_DORMANT_ROOTS:\r\n");
        serial_write(uart, b"  TTBR0_ROOT:"); serial_hex(uart, report.ttbr0_root); serial_write(uart, b"\r\n");
        serial_write(uart, b"  TTBR1_ROOT:"); serial_hex(uart, report.ttbr1_root); serial_write(uart, b"\r\n");
        serial_write(uart, b"ALLOCATED_TABLE_FRAMES:\r\n");
        serial_write(uart, b"  TTBR0 Frames ("); serial_decimal(uart, report.ttbr0_frame_count as u64); serial_write(uart, b"):\r\n");
        for i in 0..report.ttbr0_frame_count {
            serial_write(uart, b"    ["); serial_decimal(uart, i as u64); serial_write(uart, b"] ");
            serial_hex(uart, report.ttbr0_frames[i]);
            serial_write(uart, b"\r\n");
        }
        serial_write(uart, b"  TTBR1 Frames ("); serial_decimal(uart, report.ttbr1_frame_count as u64); serial_write(uart, b"):\r\n");
        for i in 0..report.ttbr1_frame_count {
            serial_write(uart, b"    ["); serial_decimal(uart, i as u64); serial_write(uart, b"] ");
            serial_hex(uart, report.ttbr1_frames[i]);
            serial_write(uart, b"\r\n");
        }
        serial_write(uart, b"PHYSICAL_WX_AUDIT:\r\n");
        serial_write(uart, b"  Unique physical pages verified: "); serial_decimal(uart, report.unique_physical_pages_checked as u64); serial_write(uart, b"\r\n");
        serial_write(uart, b"DESCRIPTOR_AUDIT:\r\n");
        serial_write(uart, b"  Header:    "); serial_hex(uart, report.header_desc.raw); serial_write(uart, b" [RO, NX]\r\n");
        serial_write(uart, b"  Code:      "); serial_hex(uart, report.code_desc.raw); serial_write(uart, b" [RO, X]\r\n");
        serial_write(uart, b"  Rodata:    "); serial_hex(uart, report.rodata_desc.raw); serial_write(uart, b" [RO, NX]\r\n");
        serial_write(uart, b"  Data:      "); serial_hex(uart, report.data_desc.raw); serial_write(uart, b" [RW, NX]\r\n");
        serial_write(uart, b"  Stack:     "); serial_hex(uart, report.stack_desc.raw); serial_write(uart, b" [RW, NX]\r\n");
        serial_write(uart, b"  Reloc:     "); serial_hex(uart, report.reloc_desc.raw); serial_write(uart, b" [RO, NX]\r\n");
        serial_write(uart, b"  UART:      "); serial_hex(uart, report.uart_desc.raw); serial_write(uart, b" [RW, NX, Dev]\r\n");

        serial_write(uart, b"GENESIS: pmm test ok\r\n");

        // Stage 3C-2 Static Verification & Diagnostics Gate
        let text_sec = match pe.find_section(b".text") {
            Some(s) => s,
            None => {
                serial_write(uart, b"TEXT_SECTION_NOT_FOUND\r\n");
                halt();
            }
        };
        let text_start = KERNEL_IMAGE_PADDR + text_sec.virtual_address;
        let text_end = text_start + (text_sec.page_count as u64) * PAGE_SIZE;

        let layout = get_transition_layout();
        if let Err(e) = layout.verify_with_bounds(text_start, text_end) {
            serial_write(uart, b"TRANSITION_LAYOUT_VERIFY_FAILED: ");
            serial_write(uart, e.as_bytes());
            serial_write(uart, b"\r\n");
            halt();
        }

        serial_write(uart, b"TRANSITION_LAYOUT:\r\n");
        serial_write(uart, b"  VBAR_VECTORS: "); serial_hex(uart, layout.v_start); serial_write(uart, b".."); serial_hex(uart, layout.v_end); serial_write(uart, b"\r\n");
        serial_write(uart, b"  TRANSITION:   "); serial_hex(uart, layout.t_start); serial_write(uart, b".."); serial_hex(uart, layout.t_end); serial_write(uart, b"\r\n");
        serial_write(uart, b"  HANDLERS:     "); serial_hex(uart, layout.h_start); serial_write(uart, b".."); serial_hex(uart, layout.h_end); serial_write(uart, b"\r\n");

        // Dynamic firmware identity-accessibility pre-check across loaded image pages
        let clean_pages = pe.total_pages;
        let clean_end = TRANSITION_CLEAN_START + (clean_pages as u64) * PAGE_SIZE;

        if !verify_firmware_identity_mapping(TRANSITION_CLEAN_START, clean_pages) {
            serial_write(uart, b"FIRMWARE_IDENTITY_PRECHECK_FAILED\r\n");
            halt();
        }
        serial_write(uart, b"GENESIS: stage3c2 firmware identity precheck ok (");
        serial_decimal(uart, clean_pages as u64);
        serial_write(uart, b" pages)\r\n");

        // Validate PE header validation routine logic against loaded identity image
        if let Err(e) = verify_higher_half_pe_header(TRANSITION_CLEAN_START) {
            serial_write(uart, b"PE_HEADER_VALIDATION_FAILED: ");
            serial_write(uart, e.as_bytes());
            serial_write(uart, b"\r\n");
            halt();
        }
        serial_write(uart, b"GENESIS: stage3c2 pe header validation logic ok\r\n");

        // Validate stack usability test function
        let magic = 0x1234_5678_9ABC_DEF0u64;
        let stack_res = verify_stack_usability(magic);
        let expected = (magic.wrapping_add(0xCAFE_BABE_DEAD_BEEF)) ^ (magic.wrapping_sub(0x1234_5678_9ABC_DEF0));
        if stack_res != expected {
            serial_write(uart, b"STACK_USABILITY_TEST_FAILED\r\n");
            halt();
        }
        serial_write(uart, b"GENESIS: stage3c2 stack test function ok\r\n");

        serial_write(uart, b"GENESIS: stage3c2 static gates ok (starting live transition)\r\n");

        // Execute Stage 3C-2 Live MMU Transition
        stage3c2_mmu_transition(
            report.ttbr0_root,
            report.ttbr1_root,
            GENESIS_TCR_VALUE,
            layout.v_start,
            GENESIS_MAIR_VALUE,
            TRANSITION_CLEAN_START,
            clean_end,
        );

        serial_write(uart, b"GENESIS: stage3c2 live mmu transition ok\r\n");

        // Post-Transition Runtime Verification
        let post_sctlr: u64;
        let post_tcr: u64;
        let post_mair: u64;
        let post_ttbr0: u64;
        let post_ttbr1: u64;
        let post_vbar: u64;
        asm!("mrs {0}, SCTLR_EL1", out(reg) post_sctlr, options(nomem, nostack, preserves_flags));
        asm!("mrs {0}, TCR_EL1", out(reg) post_tcr, options(nomem, nostack, preserves_flags));
        asm!("mrs {0}, MAIR_EL1", out(reg) post_mair, options(nomem, nostack, preserves_flags));
        asm!("mrs {0}, TTBR0_EL1", out(reg) post_ttbr0, options(nomem, nostack, preserves_flags));
        asm!("mrs {0}, TTBR1_EL1", out(reg) post_ttbr1, options(nomem, nostack, preserves_flags));
        asm!("mrs {0}, VBAR_EL1", out(reg) post_vbar, options(nomem, nostack, preserves_flags));

        serial_write(uart, b"POST_TRANSITION_REGISTERS:\r\n");
        serial_write(uart, b"  SCTLR_EL1: "); serial_hex(uart, post_sctlr); serial_write(uart, b"\r\n");
        serial_write(uart, b"  TCR_EL1:   "); serial_hex(uart, post_tcr); serial_write(uart, b"\r\n");
        serial_write(uart, b"  MAIR_EL1:  "); serial_hex(uart, post_mair); serial_write(uart, b"\r\n");
        serial_write(uart, b"  TTBR0_EL1: "); serial_hex(uart, post_ttbr0); serial_write(uart, b"\r\n");
        serial_write(uart, b"  TTBR1_EL1: "); serial_hex(uart, post_ttbr1); serial_write(uart, b"\r\n");
        serial_write(uart, b"  VBAR_EL1:  "); serial_hex(uart, post_vbar); serial_write(uart, b"\r\n");

        // Run stack usability test under new translation tables
        let post_magic = 0xFEED_FACE_CAFE_BEEFu64;
        let post_stack_res = verify_stack_usability(post_magic);
        let post_expected = (post_magic.wrapping_add(0xCAFE_BABE_DEAD_BEEF)) ^ (post_magic.wrapping_sub(0x1234_5678_9ABC_DEF0));
        if post_stack_res != post_expected {
            serial_write(uart, b"POST_STACK_USABILITY_TEST_FAILED\r\n");
            halt();
        }
        serial_write(uart, b"GENESIS: stage3c2 post-transition stack usability ok\r\n");

        // Run higher-half PE header check against KERNEL_VA_BASE (0xFFFF_FFFF_8000_0000)
        if let Err(e) = verify_higher_half_pe_header(KERNEL_VA_BASE) {
            serial_write(uart, b"POST_HIGHER_HALF_PE_HEADER_FAILED: ");
            serial_write(uart, e.as_bytes());
            serial_write(uart, b"\r\n");
            halt();
        }
        serial_write(uart, b"GENESIS: stage3c2 ttbr1 higher-half pe header ok\r\n");

        serial_write(uart, b"GENESIS: stage3c2 milestone complete\r\n");

        // ====================================================================
        // Stage 3C-3: Higher-Half Execution & TTBR0 Teardown
        // ====================================================================
        let layout_3c3 = get_stage3c3_layout();
        if let Err(e) = layout_3c3.verify(text_start, text_end) {
            serial_write(uart, b"STAGE3C3_LAYOUT_VERIFY_FAILED: ");
            serial_write(uart, e.as_bytes());
            serial_write(uart, b"\r\n");
            halt();
        }

        serial_write(uart, b"STAGE3C3_LAYOUT:\r\n");
        serial_write(uart, b"  VBAR_VECTORS: "); serial_hex(uart, layout_3c3.v_start); serial_write(uart, b".."); serial_hex(uart, layout_3c3.v_end); serial_write(uart, b"\r\n");
        serial_write(uart, b"  HANDLERS:     "); serial_hex(uart, layout_3c3.h_start); serial_write(uart, b".."); serial_hex(uart, layout_3c3.h_end); serial_write(uart, b"\r\n");

        // Calculate higher-half virtual addresses for entry, stack, and VBAR_EL1
        let entry_offset = (higher_half_entry as *const () as usize as u64) - KERNEL_IMAGE_PADDR;
        let entry_va = KERNEL_VA_BASE + entry_offset;
        let stack_offset = ((stack_bottom + STACK_SIZE) as u64 & !15u64) - KERNEL_IMAGE_PADDR;
        let stack_va = KERNEL_VA_BASE + stack_offset;
        let vbar_offset = (stage3c3_vectors_start.as_ptr() as u64) - KERNEL_IMAGE_PADDR;
        let vbar_va = KERNEL_VA_BASE + vbar_offset;
        let uart_va = MMIO_VA_BASE;
        let handoff_va = (h as usize as u64) - KERNEL_IMAGE_PADDR + KERNEL_VA_BASE;

        serial_write(uart, b"STAGE3C3_HIGHER_HALF_TARGETS:\r\n");
        serial_write(uart, b"  ENTRY_VA:   "); serial_hex(uart, entry_va); serial_write(uart, b"\r\n");
        serial_write(uart, b"  STACK_VA:   "); serial_hex(uart, stack_va); serial_write(uart, b"\r\n");
        serial_write(uart, b"  VBAR_VA:    "); serial_hex(uart, vbar_va); serial_write(uart, b"\r\n");
        serial_write(uart, b"  UART_VA:    "); serial_hex(uart, uart_va); serial_write(uart, b"\r\n");
        serial_write(uart, b"  HANDOFF_VA: "); serial_hex(uart, handoff_va); serial_write(uart, b"\r\n");

        serial_write(uart, b"GENESIS: stage3c3 jumping to higher-half trampoline...\r\n");

        // Execute Stage 3C-3 Trampoline (noreturn)
        stage3c3_jump_higher_half(entry_va, stack_va, vbar_va, uart_va, handoff_va);
    }
}

#[inline(always)]
fn ensure_higher_half(addr: u64) -> u64 {
    if addr >= 0x8000_0000_0000_0000 {
        addr
    } else {
        addr - KERNEL_IMAGE_PADDR + KERNEL_VA_BASE
    }
}

extern "C" fn higher_half_entry(uart_va: u64, handoff_va: u64) -> ! {
    unsafe {
        let uart = uart_va as usize;

        // Phase A: With TTBR0_EL1 still active
        // Read PC, SP, VBAR_EL1, and TCR_EL1
        let pc: u64;
        let sp: u64;
        let vbar: u64;
        let tcr_a: u64;
        asm!("adr {0}, .", out(reg) pc, options(nomem, nostack, preserves_flags));
        asm!("mov {0}, sp", out(reg) sp, options(nomem, nostack, preserves_flags));
        asm!("mrs {0}, VBAR_EL1", out(reg) vbar, options(nomem, nostack, preserves_flags));
        asm!("mrs {0}, TCR_EL1", out(reg) tcr_a, options(nomem, nostack, preserves_flags));

        // Assert PC, SP, and VBAR_EL1 are in the higher half: 0xFFFF_FFFF_8000_0000..0xFFFF_FFFF_8040_0000
        let kernel_va_top = KERNEL_VA_BASE + KERNEL_RESERVE_SIZE;
        if pc < KERNEL_VA_BASE || pc >= kernel_va_top {
            serial_write(uart, b"STAGE3C3_PHASE_A_PC_NOT_HIGHER_HALF!\r\n");
            halt();
        }
        if sp < KERNEL_VA_BASE || sp >= kernel_va_top {
            serial_write(uart, b"STAGE3C3_PHASE_A_SP_NOT_HIGHER_HALF!\r\n");
            halt();
        }
        if vbar < KERNEL_VA_BASE || vbar >= kernel_va_top {
            serial_write(uart, b"STAGE3C3_PHASE_A_VBAR_NOT_HIGHER_HALF!\r\n");
            halt();
        }

        serial_write(uart, b"GENESIS: stage3c3 higher-half execution active\r\n");
        serial_write(uart, b"PHASE_A_REGISTERS:\r\n");
        serial_write(uart, b"  PC:       "); serial_hex(uart, pc); serial_write(uart, b"\r\n");
        serial_write(uart, b"  SP:       "); serial_hex(uart, sp); serial_write(uart, b"\r\n");
        serial_write(uart, b"  VBAR_EL1: "); serial_hex(uart, vbar); serial_write(uart, b"\r\n");
        serial_write(uart, b"  TCR_EL1:  "); serial_hex(uart, tcr_a); serial_write(uart, b"\r\n");

        // Run stack usability test under higher-half SP
        let magic_a = 0xA1A2_A3A4_B1B2_B3B4u64;
        let res_a = verify_stack_usability(magic_a);
        let exp_a = (magic_a.wrapping_add(0xCAFE_BABE_DEAD_BEEF)) ^ (magic_a.wrapping_sub(0x1234_5678_9ABC_DEF0));
        if res_a != exp_a {
            serial_write(uart, b"STAGE3C3_PHASE_A_STACK_FAIL\r\n");
            halt();
        }
        serial_write(uart, b"GENESIS: stage3c3 phase A stack usability ok\r\n");

        // Run higher-half PE header check
        if let Err(e) = verify_higher_half_pe_header(KERNEL_VA_BASE) {
            serial_write(uart, b"STAGE3C3_PHASE_A_PE_HEADER_FAIL: ");
            serial_write(uart, e.as_bytes());
            serial_write(uart, b"\r\n");
            halt();
        }
        serial_write(uart, b"GENESIS: stage3c3 phase A higher-half pe header ok\r\n");

        // Phase B: TTBR0_EL1 Teardown (Setting TCR_EL1.EPD0 = 1)
        // Bit 7 of TCR_EL1 is EPD0. When set to 1, all TTBR0 translations generate Translation faults.
        let mut tcr_b: u64;
        asm!("mrs {0}, TCR_EL1", out(reg) tcr_b, options(nomem, nostack, preserves_flags));
        tcr_b |= 1 << 7; // EPD0 = 1: Disable TTBR0 table walks

        asm!(
            "msr TCR_EL1, {0}",
            "isb",
            "tlbi vmalle1is",
            "dsb ish",
            "isb",
            in(reg) tcr_b,
            options(nostack, preserves_flags)
        );

        // Confirm TCR_EL1.EPD0 == 1
        let confirmed_tcr: u64;
        asm!("mrs {0}, TCR_EL1", out(reg) confirmed_tcr, options(nomem, nostack, preserves_flags));
        if (confirmed_tcr & (1 << 7)) == 0 {
            serial_write(uart, b"STAGE3C3_EPD0_NOT_SET!\r\n");
            halt();
        }

        serial_write(uart, b"PHASE_B_REGISTERS:\r\n");
        serial_write(uart, b"  TCR_EL1 [EPD0=1]: "); serial_hex(uart, confirmed_tcr); serial_write(uart, b"\r\n");

        // Re-run stack usability test under isolated higher half (with TTBR0 disabled)
        let magic_b = 0x55AA_1234_9876_FEDCu64;
        let res_b = verify_stack_usability(magic_b);
        let exp_b = (magic_b.wrapping_add(0xCAFE_BABE_DEAD_BEEF)) ^ (magic_b.wrapping_sub(0x1234_5678_9ABC_DEF0));
        if res_b != exp_b {
            serial_write(uart, b"STAGE3C3_PHASE_B_STACK_FAIL\r\n");
            halt();
        }
        serial_write(uart, b"GENESIS: stage3c3 phase B stack usability ok\r\n");

        // Re-run higher-half PE header check under isolated higher half
        if let Err(e) = verify_higher_half_pe_header(KERNEL_VA_BASE) {
            serial_write(uart, b"STAGE3C3_PHASE_B_PE_HEADER_FAIL: ");
            serial_write(uart, e.as_bytes());
            serial_write(uart, b"\r\n");
            halt();
        }
        serial_write(uart, b"GENESIS: stage3c3 phase B higher-half pe header ok\r\n");

        serial_write(uart, b"GENESIS: stage3c3 higher-half isolation ok\r\n");
        serial_write(uart, b"GENESIS: stage3c3 milestone complete\r\n");

        // ====================================================================
        // Stage 4: Dynamic Higher-Half VMM, Full EL1 TrapFrame, and GIC/Timer IRQs
        // ====================================================================

        // --------------------------------------------------------------------
        // Phase 4A: Higher-Half Direct Map (HHDM) & Dynamic TTBR1 Mapper
        // --------------------------------------------------------------------
        serial_write(uart, b"GENESIS: stage4 phase 4a starting (hhdm & dynamic mapper)\r\n");

        // Verify HHDM access: Read physical DRAM via HHDM_BASE
        // Read PE header magic at HHDM_BASE + KERNEL_IMAGE_PADDR
        let hhdm_pe_ptr = (HHDM_BASE + KERNEL_IMAGE_PADDR) as *const u16;
        let hhdm_magic = core::ptr::read_volatile(hhdm_pe_ptr);
        if hhdm_magic != 0x5A4D {
            serial_write(uart, b"STAGE4_HHDM_PE_MAGIC_FAIL\r\n");
            halt();
        }
        serial_write(uart, b"GENESIS: stage4 hhdm read ok\r\n");

        // Read TTBR1_EL1 to get the root table physical address
        let ttbr1_root: u64;
        asm!("mrs {0}, TTBR1_EL1", out(reg) ttbr1_root, options(nomem, nostack, preserves_flags));
        let pmm_va = ensure_higher_half(ptr::addr_of_mut!(KERNEL_PMM) as usize as u64);
        let pmm = &mut *(pmm_va as *mut PhysicalPageAllocator<KERNEL_PMM_WORDS>);

        // Initialize DynamicMapper backed by KERNEL_PMM and HHDM
        let max_pa = 1u64 << 40; // 40-bit PA supported
        let mut mapper = DynamicMapper::new(ttbr1_root, max_pa, pmm, HHDM_BASE);
        serial_write(uart, b"GENESIS: stage4 dynamic mapper initialized\r\n");

        // --------------------------------------------------------------------
        // Phase 4B: Install Stage 4 Vector Table & TrapFrame
        // --------------------------------------------------------------------
        serial_write(uart, b"GENESIS: stage4 phase 4b installing full trap vectors\r\n");
        let stage4_vbar_va = stage4_vectors_va();

        // Configure atomic pointers for exception dispatch
        stage4_init_runtime(uart_va);

        // Install stage4_vectors into VBAR_EL1
        asm!("msr VBAR_EL1, {0}", in(reg) stage4_vbar_va, options(nomem, nostack, preserves_flags));
        asm!("isb", options(nomem, nostack, preserves_flags));

        let current_vbar: u64;
        asm!("mrs {0}, VBAR_EL1", out(reg) current_vbar, options(nomem, nostack, preserves_flags));
        if current_vbar != stage4_vbar_va {
            serial_write(uart, b"STAGE4_VBAR_INSTALL_FAIL\r\n");
            halt();
        }
        serial_write(uart, b"GENESIS: stage4 vector table installed (VBAR_EL1=");
        serial_hex(uart, current_vbar);
        serial_write(uart, b")\r\n");

        // --------------------------------------------------------------------
        // Phase 4C: FDT GIC Discovery, ARM Generic Timer, & Live IRQ Verification
        // --------------------------------------------------------------------
        serial_write(uart, b"GENESIS: stage4 phase 4c starting (fdt discovery & gic/timer irq)\r\n");

        let handoff = &*(handoff_va as *const Handoff);
        let fdt_ptr = if handoff.fdt.is_null() {
            ptr::null()
        } else {
            ensure_higher_half(handoff.fdt as usize as u64) as *const u8
        };
        let fdt_size = handoff.fdt_size;

        let (fdt_gic, fdt_timer) = if !fdt_ptr.is_null() {
            parse_fdt_devices(fdt_ptr, fdt_size)
        } else {
            (None, None)
        };

        let (gicd_paddr, gicr_paddr) = match fdt_gic {
            Some(info) if info.is_v3 => {
                serial_write(uart, b"GENESIS: fdt discovered GICv3 (GICD=");
                serial_hex(uart, info.gicd_base);
                serial_write(uart, b", GICR=");
                serial_hex(uart, info.gicr_base);
                serial_write(uart, b")\r\n");
                (info.gicd_base, info.gicr_base)
            }
            _ => {
                serial_write(uart, b"GENESIS: using standard QEMU GICv3 layout\r\n");
                (0x0800_0000u64, 0x080A_0000u64)
            }
        };

        let timer_ppi = match fdt_timer {
            Some(t) => {
                serial_write(uart, b"GENESIS: fdt discovered timer (PPI=");
                serial_decimal(uart, t.phys_ppi as u64);
                serial_write(uart, b")\r\n");
                t.phys_ppi
            }
            None => 30u32, // Default QEMU non-secure physical timer PPI
        };

        // Map GIC MMIO into higher-half TTBR1 space via DynamicMapper
        const GICD_VA: u64 = 0xFFFF_FFFF_E000_0000;
        const GICR_VA: u64 = 0xFFFF_FFFF_E010_0000;

        if let Err(e) = mapper.map_mmio_range(GICD_VA, gicd_paddr, 16) {
            serial_write(uart, b"MAP_GICD_FAIL: ");
            serial_hex(uart, e.code());
            serial_write(uart, b"\r\n");
            halt();
        }
        if let Err(e) = mapper.map_mmio_range(GICR_VA, gicr_paddr, 32) {
            serial_write(uart, b"MAP_GICR_FAIL: ");
            serial_hex(uart, e.code());
            serial_write(uart, b"\r\n");
            halt();
        }
        serial_write(uart, b"GENESIS: stage4 gic mmio mapped dynamically in ttbr1\r\n");

        // Initialize GICv3
        let gic = Gicv3::new(GICD_VA as usize, GICR_VA as usize);
        gic.init();
        gic.enable_ppi(timer_ppi);
        serial_write(uart, b"GENESIS: stage4 gicv3 initialized and timer ppi enabled\r\n");

        // Initialize ARM Generic Timer for ~10 ms periodic tick
        let freq = ArmTimer::frequency();
        let interval = freq / 100; // ~10 ms
        stage4_arm_timer_interval(interval);

        serial_write(uart, b"GENESIS: stage4 arm timer arming (freq=");
        serial_decimal(uart, freq);
        serial_write(uart, b" Hz, interval=");
        serial_decimal(uart, interval);
        serial_write(uart, b" ticks)\r\n");

        ArmTimer::arm_periodic(interval);

        // Unmask IRQs at CPU
        serial_write(uart, b"GENESIS: unmasking irqs (msr daifclr, #2)...\r\n");
        asm!("msr daifclr, #2", options(nomem, nostack, preserves_flags));

        // Await 5 timer ticks via WFI
        while stage4_get_ticks() < 5 {
            asm!("wfi", options(nomem, nostack, preserves_flags));
        }

        // Mask IRQs and disable timer
        asm!("msr daifset, #2", options(nomem, nostack, preserves_flags));
        ArmTimer::disable();

        serial_write(uart, b"GENESIS: stage4 timer irq ok (ticks=5)\r\n");
        serial_write(uart, b"GENESIS: stage4 milestone complete\r\n");

        // ====================================================================
        // Stage 5: Low-RAM & AI-Ready Kernel Foundation
        // ====================================================================
        serial_write(uart, b"GENESIS: stage5 starting (ai-ready primitives & heap)\r\n");

        // 1. Enable NEON / FP / SIMD at EL1 (CPACR_EL1.FPEN = 0b11)
        let mut cpacr: u64;
        asm!("mrs {0}, CPACR_EL1", out(reg) cpacr, options(nomem, nostack, preserves_flags));
        cpacr |= 0b11 << 20; // bits [21:20] = 0b11
        asm!("msr CPACR_EL1, {0}", in(reg) cpacr, options(nomem, nostack, preserves_flags));
        asm!("isb", options(nomem, nostack, preserves_flags));

        // Verify NEON / Floating-Point arithmetic
        let f_a: f64 = core::hint::black_box(42.5);
        let f_b: f64 = core::hint::black_box(57.5);
        let f_c = f_a + f_b;
        if f_c == 100.0 {
            serial_write(uart, b"GENESIS: stage5 f64/neon test ok\r\n");
        } else {
            serial_write(uart, b"STAGE5_FP_TEST_FAIL\r\n");
            halt();
        }

        // 2. 2 MiB Hugepage Support in DynamicMapper
        const HUGEPAGE_VA: u64 = 0xFFFF_FFFF_A000_0000;
        let hugepage_pa = 0x4200_0000u64; // Aligned 2 MiB physical DRAM block
        if let Err(e) = mapper.map_l2_block(HUGEPAGE_VA, hugepage_pa, PagePermissions::kernel_data()) {
            serial_write(uart, b"STAGE5_MAP_L2_BLOCK_FAIL: ");
            serial_hex(uart, e.code());
            serial_write(uart, b"\r\n");
            halt();
        }
        let start_ptr = HUGEPAGE_VA as *mut u64;
        let end_ptr = (HUGEPAGE_VA + 0x1F_F000) as *mut u64; // Last page in 2 MiB block
        core::ptr::write_volatile(start_ptr, 0x1122_3344_5566_7788);
        core::ptr::write_volatile(end_ptr, 0x99AA_BBCC_DDEE_FF00);
        if core::ptr::read_volatile(start_ptr) != 0x1122_3344_5566_7788
            || core::ptr::read_volatile(end_ptr) != 0x99AA_BBCC_DDEE_FF00
        {
            serial_write(uart, b"STAGE5_HUGEPAGE_ACCESS_FAIL\r\n");
            halt();
        }
        serial_write(uart, b"GENESIS: stage5 hugepage 2mib map ok\r\n");

        // 3. Dynamic Kernel Heap backed by KERNEL_PMM frames accessed via HHDM_BASE
        let heap_paddr = match pmm.allocate_contiguous_pages(64) {
            Some(pa) => pa,
            None => {
                serial_write(uart, b"STAGE5_HEAP_PMM_ALLOC_FAIL\r\n");
                halt();
            }
        };
        let heap_va = HHDM_BASE + heap_paddr;
        KERNEL_HEAP.init(heap_va as usize, 64 * 4096);

        // Verify Box dynamic allocation
        let boxed_val = Box::new(0xCAFE_BABE_1234_5678u64);
        if *boxed_val != 0xCAFE_BABE_1234_5678u64 {
            serial_write(uart, b"STAGE5_BOX_VERIFY_FAIL\r\n");
            halt();
        }

        // Verify multi-page dynamic Vec allocation (1024 u64 elements = 8 KiB = 2 pages)
        let mut vec: Vec<u64> = Vec::new();
        for i in 0..1024 {
            vec.push(i as u64);
        }
        let mut vec_ok = true;
        for i in 0..1024 {
            if vec[i] != i as u64 {
                vec_ok = false;
                break;
            }
        }
        if !vec_ok {
            serial_write(uart, b"STAGE5_VEC_VERIFY_FAIL\r\n");
            halt();
        }
        drop(vec);
        drop(boxed_val);

        // Verify re-allocation after free
        let mut vec2: Vec<u64> = Vec::new();
        vec2.push(999);
        if vec2[0] != 999 {
            serial_write(uart, b"STAGE5_REALLOC_FAIL\r\n");
            halt();
        }
        drop(vec2);

        serial_write(uart, b"GENESIS: stage5 heap test ok\r\n");
        serial_write(uart, b"GENESIS: stage5 milestone complete\r\n");

        halt();
    }
}

unsafe fn find_pl011(fdt: *const u8, bound: usize) -> Option<usize> {
    let total=be32(fdt.add(4)) as usize; if total>bound || total<40 { return None; }
    let so=be32(fdt.add(8)) as usize; let stro=be32(fdt.add(12)) as usize; let ss=be32(fdt.add(32)) as usize; let sz=be32(fdt.add(36)) as usize;
    if so.checked_add(sz)? > total || stro.checked_add(ss)? > total { return None; }
    let strings=fdt.add(stro); let (mut p,end)=(fdt.add(so),fdt.add(so+sz));
    let mut nodes=[Node{parent_ac:2,parent_sc:1,child_ac:2,child_sc:1,uart:false,disabled:false,reg:0,reg_size:0};32]; let mut depth=0usize;
    while p.add(4)<=end { let token=be32(p); p=p.add(4); match token {
        1 => { if depth==nodes.len(){return None;} let parent=if depth==0 {Node{parent_ac:2,parent_sc:1,child_ac:2,child_sc:1,uart:false,disabled:false,reg:0,reg_size:0}} else {nodes[depth-1]}; nodes[depth]=Node{parent_ac:parent.child_ac,parent_sc:parent.child_sc,child_ac:parent.child_ac,child_sc:parent.child_sc,uart:false,disabled:false,reg:0,reg_size:0}; depth+=1; while p<end && *p!=0 {p=p.add(1);} p=align4(p.add(1)); }
        2 => { if depth==0{return None;} depth-=1; let n=nodes[depth]; if n.uart && !n.disabled && n.reg!=0 && n.reg_size>=0x2c && n.reg&3==0 {return Some(n.reg);} }
        3 => { if depth==0 || p.add(8)>end{return None;} let len=be32(p) as usize; let nameoff=be32(p.add(4)) as usize; p=p.add(8); if p.add(len)>end || nameoff>=ss{return None;} let name=cstr(strings.add(nameoff),ss-nameoff); let val=p; let node=&mut nodes[depth-1];
            match name {
                b"#address-cells" if len>=4 => node.child_ac=be32(val),
                b"#size-cells" if len>=4 => node.child_sc=be32(val),
                b"compatible" => node.uart=has_string(val,len,b"arm,pl011"),
                b"status" => {let status=cstr(val,len); node.disabled=status==b"disabled"||status==b"fail";},
                b"reg" => {let ac=node.parent_ac as usize; let sc=node.parent_sc as usize; if (ac==1||ac==2)&&(sc==1||sc==2)&&len>=(ac+sc)*4 {node.reg=read_cells(val,ac) as usize;node.reg_size=read_cells(val.add(ac*4),sc) as usize;}},
                _=>{}
            } p=align4(p.add(len)); }
        4 => {}
        9 => break,
        _=>return None,
    }} None
}
unsafe fn valid_fdt(p:*const u8,cap:usize)->bool {if p.is_null(){return false;} let magic=be32(p); let total=be32(p.add(4)) as usize; magic==0xd00dfeed&&total>=40&&total<=cap&&be32(p.add(20))>=17&&be32(p.add(24))<=17}
unsafe fn be32(p:*const u8)->u32 {u32::from_be(ptr::read_unaligned(p as *const u32))}
unsafe fn read_cells(p:*const u8,n:usize)->u64 {let mut v=0u64;for i in 0..n{v=(v<<32)|be32(p.add(4*i)) as u64;}v}
unsafe fn align4(p:*const u8)->*const u8 {((p as usize+3)&!3)as *const u8}
unsafe fn cstr<'a>(p:*const u8,n:usize)->&'a[u8] {let mut l=0;while l<n&&*p.add(l)!=0{l+=1;}core::slice::from_raw_parts(p,l)}
unsafe fn has_string(p:*const u8,n:usize,needle:&[u8])->bool {let mut i=0;while i<n{let s=cstr(p.add(i),n-i);if s==needle{return true;}i+=s.len()+1;}false}
unsafe fn mmio_read(addr:usize)->u32 {ptr::read_volatile(addr as *const u32)}
unsafe fn mmio_write(addr:usize,v:u32) {ptr::write_volatile(addr as *mut u32,v)}
unsafe fn halt()->! {
    asm!("msr daifset, #0xf", options(nomem, nostack, preserves_flags));
    loop { asm!("wfe", options(nomem, nostack, preserves_flags)); }
}
#[panic_handler] fn panic(_: &PanicInfo)->! {unsafe{halt()}}
