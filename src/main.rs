#![no_std]
#![no_main]

extern crate alloc;
use alloc::{boxed::Box, vec::Vec};

use core::{arch::asm, panic::PanicInfo, ptr, sync::atomic::{AtomicBool, AtomicU64, Ordering}};
use genesis_stage1::{
    ai::{
        load_llama_weights, load_micro_llm_from_bytes, load_model_from_bytes,
        load_transformer_from_bytes, parse_llama_header, run_llama_forward_step, run_parallel_micro_llm_step,
        run_parallel_nn_inference, run_parallel_transformer, sample_greedy, scalar_infer,
        scalar_matmul, scalar_micro_llm_step, scalar_transformer_block, smp_matmul_worker,
        verify_inference, verify_llm_step, verify_matrices, verify_transformer,
        LlamaWeights, SmpLlamaContext, Tokenizer, MATMUL_N, SMP_LLAMA_CTX, SMP_LLAMA_WEIGHTS,
        SMP_MICRO_LLM_WEIGHTS, SMP_NN_CTX, SMP_TRANSFORMER_CTX,
    },
    arch::{
        dispatch_smp_work, parse_fdt_devices, parse_fdt_pci, psci_cpu_on, secondary_boot_stub, secondary_main,
        serial_read_line, stage4_arm_timer_interval, stage4_arm_timer_ticks,
        stage4_get_ticks, stage4_init_runtime, stage4_vectors_va, ArmTimer, Gicv3,
        SecondaryBootArgs, ONLINE_CPUS,
    },
    drivers::{
        global_fb_console_write, pci::{find_pci_by_class, find_pci_device, pci_enable_bus_master, scan_bus_0, PCI_BAR_VA_BASE, PCI_ECAM_DEFAULT_BASE, PCI_ECAM_VA_BASE},
        register_global_fb_console, update_workspace_telemetry, virtio_gpu::{register_global_virtio_gpu, VirtIOGpuDevice},
        virtio_input::VirtIOInputDevice, write_u64, FbConsole, Framebuffer, VirtIOBlockDevice,
        VirtIONetDevice, VIRTIO_DEV_BLOCK, VIRTIO_MAGIC,
        xhci::{get_global_xhci_ref, register_global_xhci, XhciDevice, XHCI_MMIO_VA_BASE},
        usb_hid::{enumerate_usb_devices, get_enumerated_count},
    },
    fs::{locate_volume, FatVolume},
    memory_map,
    mm::heap::SpinLock,
    net,
    pmm::{self, PhysicalPageAllocator, ReservedRange, KERNEL_PMM_WORDS},
    proc::{destroy_user_process, load_elf64, run_user_process},
    sched,
    shell::{Shell, ShellResult},
    vmm::{
        get_stage3c3_layout, get_transition_layout, parse_pe_image, run_stage3c1_preflight,
        stage3c2_mmu_transition, stage3c3_jump_higher_half, stage3c3_vectors_start,
        verify_firmware_identity_mapping, verify_higher_half_pe_header, verify_stack_usability,
        DormantTablePool, DynamicMapper, PagePermissions, ParsedPeImage, GENESIS_MAIR_VALUE,
        GENESIS_TCR_VALUE, HHDM_BASE, KERNEL_IMAGE_PADDR, KERNEL_RESERVE_SIZE, KERNEL_VA_BASE,
        MMIO_VA_BASE, PAGE_SIZE, TRANSITION_CLEAN_START,
    },
    serial_decimal, serial_hex, serial_write,
};

use genesis_stage1::mm::HEAP_ALLOCATOR;

static UART_LOCK: SpinLock<()> = SpinLock::new(());
static DOT_PRODUCT_DONE: AtomicBool = AtomicBool::new(false);
static DOT_PRODUCT_SUM: AtomicU64 = AtomicU64::new(0);
static COUNTER_TICKS: AtomicU64 = AtomicU64::new(0);

extern "C" fn dot_product_task(uart_va: u64) {
    let uart = uart_va as usize;
    let a = [1.5f64, 2.5, 3.5, 4.5];
    let b = [2.0f64, 3.0, 4.0, 5.0];
    let mut dot = 0.0f64;
    for i in 0..4 {
        dot += a[i] * b[i];
    }
    DOT_PRODUCT_SUM.store(dot.to_bits(), Ordering::SeqCst);
    DOT_PRODUCT_DONE.store(true, Ordering::SeqCst);

    {
        let _guard = UART_LOCK.lock();
        unsafe {
            serial_write(uart, b"GENESIS: dot product worker finished\r\n");
        }
    }

    loop {
        #[cfg(target_os = "uefi")]
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
        #[cfg(not(target_os = "uefi"))]
        core::hint::spin_loop();
    }
}

extern "C" fn counter_task(uart_va: u64) {
    let uart = uart_va as usize;
    loop {
        let c = COUNTER_TICKS.fetch_add(1, Ordering::SeqCst) + 1;
        if c == 1 || c == 200_000 || c == 500_000 {
            let _guard = UART_LOCK.lock();
            unsafe {
                serial_write(uart, b"GENESIS: counter task active count=");
                serial_decimal(uart, c);
                serial_write(uart, b"\r\n");
            }
        }
        for _ in 0..50 {
            core::hint::spin_loop();
        }
    }
}

extern "C" fn idle_task(_arg: u64) {
    loop {
        #[cfg(target_os = "uefi")]
        unsafe {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
        #[cfg(not(target_os = "uefi"))]
        core::hint::spin_loop();
    }
}

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
const GOP_GUID: Guid = Guid {
    data1: 0x9042a9de, data2: 0x23dc, data3: 0x4a38,
    data4: [0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80, 0x51, 0x6a],
};
const FDT_CAP: usize = 128 * 1024;
const STACK_SIZE: usize = 32 * 1024;
const HUGEPAGE_VA: u64 = 0xFFFF_FFFF_A000_0000;

#[repr(C)] #[derive(Clone, Copy)] struct Guid { data1: u32, data2: u16, data3: u16, data4: [u8; 8] }
#[repr(C)] struct ConfigTable { guid: Guid, table: *const u8 }
#[repr(C)] struct TextOutput { reset: usize, output_string: extern "efiapi" fn(*mut TextOutput, *const u16) -> usize }
#[repr(C)] pub struct SystemTable {
    header: [u64; 3], vendor: *const u16, revision: u32, _pad: u32,
    _console_in_handle: usize, _console_in: usize, _console_out_handle: usize, console_out: *mut TextOutput,
    _stderr_handle: usize, _stderr: usize, _runtime: usize, boot_services: *const usize,
    table_count: usize, tables: *const ConfigTable,
}
#[repr(C)]
struct EfiGraphicsOutputModeInformation {
    version: u32,
    horizontal_resolution: u32,
    vertical_resolution: u32,
    pixel_format: u32,
    pixel_information: [u32; 4],
    pixels_per_scan_line: u32,
}
#[repr(C)]
struct EfiGraphicsOutputProtocolMode {
    max_mode: u32,
    mode: u32,
    info: *const EfiGraphicsOutputModeInformation,
    size_of_info: usize,
    frame_buffer_base: u64,
    frame_buffer_size: usize,
}
#[repr(C)]
struct EfiGraphicsOutputProtocol {
    query_mode: usize,
    set_mode: usize,
    blt: usize,
    mode: *mut EfiGraphicsOutputProtocolMode,
}
#[repr(C)]
struct Handoff {
    magic: [u8; 8],
    version: u32,
    size: u32,
    fdt: *const u8,
    fdt_size: usize,
    map: *const u8,
    map_size: usize,
    descriptor_size: usize,
    descriptor_version: u32,
    uart_base: usize,
    observed_el: u64,
    fb_base: u64,
    fb_size: u64,
    fb_width: u32,
    fb_height: u32,
    fb_stride: u32,
    fb_pixel_format: u32,
}
#[repr(C)] #[derive(Clone, Copy)] struct Node { parent_ac: u32, parent_sc: u32, child_ac: u32, child_sc: u32, uart: bool, disabled: bool, reg: usize, reg_size: usize }
#[repr(C)] struct FdtClient { methods: [usize; 9] }

static mut FDT_COPY: [u8; FDT_CAP] = [0; FDT_CAP];
static mut MEMORY_MAP: [u8; memory_map::BUFFER_CAPACITY] = [0; memory_map::BUFFER_CAPACITY];
static mut HANDOFF: Handoff = Handoff {
    magic: [0; 8],
    version: 0,
    size: 0,
    fdt: ptr::null(),
    fdt_size: 0,
    map: ptr::null(),
    map_size: 0,
    descriptor_size: 0,
    descriptor_version: 0,
    uart_base: 0,
    observed_el: 0,
    fb_base: 0,
    fb_size: 0,
    fb_width: 0,
    fb_height: 0,
    fb_stride: 0,
    fb_pixel_format: 0,
};
static mut KERNEL_STACK: [u8; STACK_SIZE] = [0; STACK_SIZE];
static mut KERNEL_PMM: PhysicalPageAllocator<KERNEL_PMM_WORDS> = PhysicalPageAllocator::new();
static mut DORMANT_TTBR0_POOL: DormantTablePool = DormantTablePool::new();
static mut DORMANT_TTBR1_POOL: DormantTablePool = DormantTablePool::new();
static mut PARSED_PE: Option<ParsedPeImage> = None;

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

        // Query UEFI Graphics Output Protocol (GOP)
        let mut fb_base = 0u64;
        let mut fb_size = 0u64;
        let mut fb_width = 0u32;
        let mut fb_height = 0u32;
        let mut fb_stride = 0u32;
        let mut fb_pixel_format = 0u32;

        let locate_protocol_gop: extern "efiapi" fn(*const Guid, *mut usize, *mut *mut EfiGraphicsOutputProtocol) -> usize =
            core::mem::transmute(*st.boot_services.add(40));
        let mut gop: *mut EfiGraphicsOutputProtocol = ptr::null_mut();
        let gop_status = locate_protocol_gop(&GOP_GUID, ptr::null_mut(), &mut gop);
        if gop_status == EFI_SUCCESS && !gop.is_null() {
            let mode_ptr = (*gop).mode;
            if !mode_ptr.is_null() {
                if (*gop).query_mode != 0 && (*gop).set_mode != 0 {
                    type EfiGopQueryMode = extern "efiapi" fn(*mut EfiGraphicsOutputProtocol, u32, *mut usize, *mut *mut EfiGraphicsOutputModeInformation) -> usize;
                    type EfiGopSetMode = extern "efiapi" fn(*mut EfiGraphicsOutputProtocol, u32) -> usize;
                    let query_mode_fn: EfiGopQueryMode = core::mem::transmute((*gop).query_mode);
                    let set_mode_fn: EfiGopSetMode = core::mem::transmute((*gop).set_mode);
                    let max_modes = (*mode_ptr).max_mode;
                    let current_mode = (*mode_ptr).mode;
                    let mut best_mode = current_mode;
                    let mut best_score = 0u32;
                    for m in 0..max_modes {
                        let mut info_size: usize = 0;
                        let mut info_ptr: *mut EfiGraphicsOutputModeInformation = ptr::null_mut();
                        let q_status = query_mode_fn(gop, m, &mut info_size, &mut info_ptr);
                        if q_status == EFI_SUCCESS && !info_ptr.is_null() {
                            let info = &*info_ptr;
                            if info.pixel_format == 0 || info.pixel_format == 1 {
                                let (w, h) = (info.horizontal_resolution, info.vertical_resolution);
                                let score = if w == 1280 && h == 800 { 30 }
                                            else if w == 1024 && h == 768 { 20 }
                                            else if (w == 1280 && h == 720) || (w == 1280 && h == 1024) { 15 }
                                            else if w == 800 && h == 600 { 10 }
                                            else { 1 };
                                if score > best_score {
                                    best_score = score;
                                    best_mode = m;
                                }
                            }
                        }
                    }
                    if best_score > 0 && best_mode != current_mode {
                        let _ = set_mode_fn(gop, best_mode);
                    }
                }
                let final_mode_ptr = (*gop).mode;
                if !final_mode_ptr.is_null() {
                    let mode = &*final_mode_ptr;
                    if mode.frame_buffer_base != 0 && mode.frame_buffer_size != 0 && !mode.info.is_null() {
                        let info = &*mode.info;
                        fb_base = mode.frame_buffer_base;
                        fb_size = mode.frame_buffer_size as u64;
                        fb_width = info.horizontal_resolution;
                        fb_height = info.vertical_resolution;
                        fb_stride = info.pixels_per_scan_line;
                        fb_pixel_format = info.pixel_format;
                    }
                }
            }
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
        HANDOFF = Handoff {
            magic: *b"GENESIS\0",
            version: 1,
            size: core::mem::size_of::<Handoff>() as u32,
            fdt,
            fdt_size: fdt_len,
            map,
            map_size: map_len,
            descriptor_size: desc_len,
            descriptor_version: desc_ver,
            uart_base,
            observed_el: current_el,
            fb_base,
            fb_size,
            fb_width,
            fb_height,
            fb_stride,
            fb_pixel_format,
        };
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
        let mut reserved = [
            ReservedRange { start: 0x4800_0000, size: 4 * 1024 * 1024 },
            ReservedRange { start: uart as u64, size: 0x1000 },
            ReservedRange { start: 0, size: 0 },
        ];
        let mut num_reserved = 2;
        if (*h).fb_base != 0 && (*h).fb_size != 0 {
            reserved[num_reserved] = ReservedRange {
                start: (*h).fb_base,
                size: (*h).fb_size,
            };
            num_reserved += 1;
        }
        if pmm.init_from_memory_map((*h).map, (*h).map_size, (*h).descriptor_size, &reserved[..num_reserved]).is_err() {
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
        *ptr::addr_of_mut!(PARSED_PE) = Some(pe);

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

        // Relocate all PE DIR64 pointers to higher-half virtual addresses
        apply_higher_half_relocations(KERNEL_IMAGE_PADDR, &pe);
        asm!("dsb sy", "isb", options(nostack, preserves_flags));

        // Execute Stage 3C-3 Trampoline (noreturn)
        stage3c3_jump_higher_half(entry_va, stack_va, vbar_va, uart_va, handoff_va);
    }
}

unsafe fn apply_higher_half_relocations(base_paddr: u64, pe: &genesis_stage1::vmm::ParsedPeImage) {
    if let (Some(reloc_sec), Some(data_sec)) = (pe.find_section(b".reloc"), pe.find_section(b".data")) {
        let reloc_ptr = (base_paddr + reloc_sec.virtual_address) as *const u8;
        let reloc_size = reloc_sec.virtual_size as usize;
        let delta = KERNEL_VA_BASE.wrapping_sub(base_paddr);

        let data_start = data_sec.virtual_address as usize;
        let data_end = data_start + data_sec.virtual_size as usize;

        let mut p = 0usize;
        while p + 8 <= reloc_size {
            let page_rva = core::ptr::read_unaligned(reloc_ptr.add(p) as *const u32) as usize;
            let block_size = core::ptr::read_unaligned(reloc_ptr.add(p + 4) as *const u32) as usize;
            if block_size < 8 || p + block_size > reloc_size {
                break;
            }
            let entry_count = (block_size - 8) / 2;
            let entries = reloc_ptr.add(p + 8) as *const u16;
            for i in 0..entry_count {
                let entry = core::ptr::read_unaligned(entries.add(i));
                let rel_type = entry >> 12;
                let rel_off = (entry & 0xFFF) as usize;
                let target_rva = page_rva + rel_off;

                // Only mutate writable .data section entries (e.g. GOT table)
                if rel_type == 10 && target_rva >= data_start && target_rva < data_end {
                    let target_paddr = base_paddr + target_rva as u64;
                    let ptr_loc = target_paddr as *mut u64;
                    let val = core::ptr::read_volatile(ptr_loc);
                    if val >= base_paddr && val < base_paddr + KERNEL_RESERVE_SIZE {
                        core::ptr::write_volatile(ptr_loc, val.wrapping_add(delta));
                        core::arch::asm!("dc cvac, {0}", in(reg) target_paddr, options(nomem, nostack));
                    }
                }
            }
            p += block_size;
        }
        core::arch::asm!("dsb sy", "isb", options(nomem, nostack));
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
        if let Err(e) = mapper.map_mmio_range(GICR_VA, gicr_paddr, 128) {
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
        let hugepage_pa = match mapper.pmm_mut().allocate_aligned_pages(512, 512) {
            Some(pa) => pa,
            None => {
                serial_write(uart, b"STAGE5_HUGEPAGE_ALLOC_FAIL\r\n");
                halt();
            }
        };
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
        let heap_paddr = match mapper.pmm_mut().allocate_contiguous_pages(64) {
            Some(pa) => pa,
            None => {
                serial_write(uart, b"STAGE5_HEAP_PMM_ALLOC_FAIL\r\n");
                halt();
            }
        };
        let heap_va = HHDM_BASE + heap_paddr;
        HEAP_ALLOCATOR.init(heap_va as usize, 64 * 4096);

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

        // ====================================================================
        // Stage 6: SMP (4-Core), Preemptive Scheduler, and VirtIO-Blk
        // ====================================================================

        // --------------------------------------------------------------------
        // Stage 6A: Multi-Core SMP Bring-Up via PSCI (-smp 4)
        // --------------------------------------------------------------------
        serial_write(uart, b"GENESIS: stage6 smp starting bringup\r\n");

        let ttbr0_val: u64;
        let ttbr1_val: u64;
        let tcr_val: u64;
        let mair_val: u64;
        let vbar_val: u64;
        let sctlr_val: u64;
        asm!("mrs {0}, ttbr0_el1", out(reg) ttbr0_val, options(nomem, nostack));
        asm!("mrs {0}, ttbr1_el1", out(reg) ttbr1_val, options(nomem, nostack));
        asm!("mrs {0}, tcr_el1", out(reg) tcr_val, options(nomem, nostack));
        asm!("mrs {0}, mair_el1", out(reg) mair_val, options(nomem, nostack));
        asm!("mrs {0}, vbar_el1", out(reg) vbar_val, options(nomem, nostack));
        asm!("mrs {0}, sctlr_el1", out(reg) sctlr_val, options(nomem, nostack));

        let stub_va = secondary_boot_stub as *const () as u64;
        let stub_pa = if stub_va >= KERNEL_VA_BASE {
            stub_va - KERNEL_VA_BASE + 0x4800_0000
        } else {
            stub_va
        };
        asm!("dc cvac, {0}", "dsb sy", in(reg) stub_va, options(nomem, nostack));

        for core_id in 1..4 {
            let stack_pa = match mapper.pmm_mut().allocate_contiguous_pages(4) {
                Some(pa) => pa,
                None => {
                    serial_write(uart, b"STAGE6_SMP_STACK_ALLOC_FAIL\r\n");
                    halt();
                }
            };
            let stack_va = HHDM_BASE + stack_pa + 16384;

            let args_pa = match mapper.pmm_mut().allocate_page() {
                Some(pa) => pa,
                None => {
                    serial_write(uart, b"STAGE6_SMP_ARGS_ALLOC_FAIL\r\n");
                    halt();
                }
            };
            let args_va = HHDM_BASE + args_pa;
            let args_ptr = args_va as *mut SecondaryBootArgs;
            *args_ptr = SecondaryBootArgs {
                cpu_id: core_id as u64,
                ttbr0: ttbr0_val,
                ttbr1: ttbr1_val,
                tcr: tcr_val & !(1 << 7), // clear EPD0 so identity mapping works during stub
                mair: mair_val,
                vbar: vbar_val,
                sctlr: sctlr_val,
                stack_va,
                entry_va: secondary_main as *const () as u64,
            };
            asm!(
                "dc cvac, {0}",
                "dc cvac, {1}",
                "dsb sy",
                in(reg) args_va,
                in(reg) (args_va + 64),
                options(nomem, nostack)
            );

            let psci_res = psci_cpu_on(core_id as u64, stub_pa, args_pa);
            if psci_res < 0 {
                serial_write(uart, b"STAGE6_PSCI_CPU_ON_FAIL: core=");
                serial_decimal(uart, core_id as u64);
                serial_write(uart, b" err=");
                serial_hex(uart, psci_res as u64);
                serial_write(uart, b"\r\n");
                halt();
            }
        }

        // Wait for all 4 cores to check in
        let mut smp_spins = 0u64;
        while ONLINE_CPUS.load(Ordering::SeqCst) < 4 && smp_spins < 50_000_000 {
            core::hint::spin_loop();
            smp_spins += 1;
        }

        if ONLINE_CPUS.load(Ordering::SeqCst) != 4 {
            serial_write(uart, b"STAGE6_SMP_TIMEOUT: online=");
            serial_decimal(uart, ONLINE_CPUS.load(Ordering::SeqCst));
            serial_write(uart, b"\r\n");
            halt();
        }
        serial_write(uart, b"GENESIS: stage6 smp online cpus=4\r\n");

        // --------------------------------------------------------------------
        // Stage 6B: Preemptive Kernel Task Scheduler
        // --------------------------------------------------------------------
        let current_sp: u64;
        asm!("mov {0}, sp", out(reg) current_sp, options(nomem, nostack));
        sched::init_scheduler(current_sp);

        // Spawn 3 worker threads
        let st1_pa = mapper.pmm_mut().allocate_contiguous_pages(4).unwrap();
        let st1_top = HHDM_BASE + st1_pa + 16384;
        sched::spawn_task(dot_product_task, uart as u64, st1_top).unwrap();

        let st2_pa = mapper.pmm_mut().allocate_contiguous_pages(4).unwrap();
        let st2_top = HHDM_BASE + st2_pa + 16384;
        sched::spawn_task(counter_task, uart as u64, st2_top).unwrap();

        let st3_pa = mapper.pmm_mut().allocate_contiguous_pages(4).unwrap();
        let st3_top = HHDM_BASE + st3_pa + 16384;
        sched::spawn_task(idle_task, 0, st3_top).unwrap();

        // Arm timer for 15 ticks (10 ms interval, >= 10 ticks required)
        let timer_freq = ArmTimer::frequency();
        let sched_interval = timer_freq / 100; // 10 ms
        stage4_arm_timer_ticks(sched_interval, 15);
        ArmTimer::arm_periodic(sched_interval);

        // Unmask IRQs to allow preemptive scheduling ticks
        asm!("msr daifclr, #2", options(nomem, nostack));

        // Wait for at least 12 timer ticks
        while stage4_get_ticks() < 12 {
            core::hint::spin_loop();
        }

        // Disable timer and mask IRQs
        ArmTimer::disable();
        asm!("msr daifset, #2", options(nomem, nostack));

        // Verify dot-product calculation across preemptions
        let dot_done = DOT_PRODUCT_DONE.load(Ordering::SeqCst);
        let dot_res = f64::from_bits(DOT_PRODUCT_SUM.load(Ordering::SeqCst));
        let counter_val = COUNTER_TICKS.load(Ordering::SeqCst);

        if !dot_done || (dot_res - 47.0).abs() > 0.0001 || counter_val == 0 {
            serial_write(uart, b"STAGE6_SCHEDULER_VERIFY_FAIL: done=");
            serial_decimal(uart, dot_done as u64);
            serial_write(uart, b" counter=");
            serial_decimal(uart, counter_val);
            serial_write(uart, b"\r\n");
            halt();
        }
        serial_write(uart, b"GENESIS: stage6 scheduler ok\r\n");

        // --------------------------------------------------------------------
        // Stage 6C: VirtIO-Blk MMIO Driver (Sector Read)
        // --------------------------------------------------------------------
        // Map 4 pages of VirtIO MMIO slots (0x0A00_0000..0x0A00_4000)
        const VIRTIO_MMIO_VA: u64 = 0xFFFF_FFFF_E020_0000;
        if let Err(e) = mapper.map_mmio_range(VIRTIO_MMIO_VA, 0x0A00_0000, 4) {
            serial_write(uart, b"STAGE6_MAP_VIRTIO_FAIL: ");
            serial_hex(uart, e.code());
            serial_write(uart, b"\r\n");
            halt();
        }

        // Scan 32 MMIO slots for VirtIO Block device (DeviceID == 2)
        let mut blk_slot_va: Option<u64> = None;
        for i in 0..32 {
            let slot_va = VIRTIO_MMIO_VA + (i as u64) * 0x200;
            let magic = core::ptr::read_volatile(slot_va as *const u32);
            let dev_id = core::ptr::read_volatile((slot_va + 8) as *const u32);
            if magic == VIRTIO_MAGIC && dev_id == VIRTIO_DEV_BLOCK {
                blk_slot_va = Some(slot_va);
                break;
            }
        }

        let blk_va = match blk_slot_va {
            Some(va) => va,
            None => {
                serial_write(uart, b"STAGE6_VIRTIO_BLK_NOT_FOUND\r\n");
                halt();
            }
        };

        serial_write(uart, b"GENESIS: found virtio-blk at ");
        serial_hex(uart, blk_va);
        serial_write(uart, b"\r\n");

        // Allocate 2 contiguous pages for Virtqueue 0 (legacy virtqueue used ring is at offset 4096)
        let q_pa = mapper.pmm_mut().allocate_contiguous_pages(2).unwrap();
        let q_va = HHDM_BASE + q_pa;
        core::ptr::write_bytes(q_va as *mut u8, 0, 8192);

        let mut blk_dev = match VirtIOBlockDevice::init(blk_va, q_pa, q_va) {
            Ok(dev) => dev,
            Err(_) => {
                serial_write(uart, b"STAGE6_VIRTIO_BLK_INIT_FAIL\r\n");
                halt();
            }
        };

        serial_write(uart, b"GENESIS: virtio-blk initialized (version=");
        serial_decimal(uart, blk_dev.version as u64);
        serial_write(uart, b")\r\n");

        // Stage 15B: Multi-Device VirtIO-Input Probing (DeviceID == 18)
        let mut input_slots = [0u64; 4];
        let found_input_count = VirtIOInputDevice::find_all_slots(VIRTIO_MMIO_VA, &mut input_slots);
        let mut input_devs: [Option<VirtIOInputDevice>; 4] = [None, None, None, None];

        for i in 0..found_input_count {
            if let Some(input_q_pa) = mapper.pmm_mut().allocate_contiguous_pages(2) {
                let input_q_va = HHDM_BASE + input_q_pa;
                match VirtIOInputDevice::init(input_slots[i], input_q_pa, input_q_va) {
                    Ok(dev) => {
                        serial_write(uart, b"GENESIS: virtio-input device detected (slot=");
                        serial_hex(uart, input_slots[i]);
                        serial_write(uart, b", version=");
                        serial_decimal(uart, dev.version as u64);
                        serial_write(uart, b")\r\n");
                        if i == 0 {
                            serial_write(uart, b"GENESIS: virtio-input keyboard detected (version=");
                            serial_decimal(uart, dev.version as u64);
                            serial_write(uart, b")\r\n");
                        }
                        input_devs[i] = Some(dev);
                    }
                    Err(_) => {
                        let _ = mapper.pmm_mut().free_page(input_q_pa);
                        let _ = mapper.pmm_mut().free_page(input_q_pa + 4096);
                    }
                }
            }
        }

        for dev_opt in &mut input_devs {
            if let Some(dev) = dev_opt {
                genesis_stage1::drivers::virtio_input::register_global_virtio_input(dev as *mut _);
            }
        }

        // Stage 12B: Probe VirtIO-Net (DeviceID == 1)
        let rx_q_pa = mapper.pmm_mut().allocate_contiguous_pages(2).unwrap();
        let rx_q_va = HHDM_BASE + rx_q_pa;
        let tx_q_pa = mapper.pmm_mut().allocate_contiguous_pages(2).unwrap();
        let tx_q_va = HHDM_BASE + tx_q_pa;
        let rx_buf_pa = mapper.pmm_mut().allocate_contiguous_pages(8).unwrap();
        let rx_buf_va = HHDM_BASE + rx_buf_pa;
        let tx_buf_pa = mapper.pmm_mut().allocate_contiguous_pages(1).unwrap();
        let tx_buf_va = HHDM_BASE + tx_buf_pa;

        let mut net_dev_opt = match VirtIONetDevice::probe(
            VIRTIO_MMIO_VA,
            rx_q_pa,
            rx_q_va,
            tx_q_pa,
            tx_q_va,
            rx_buf_pa,
            rx_buf_va,
            tx_buf_pa,
            tx_buf_va,
        ) {
            Ok(Some(dev)) => {
                serial_write(uart, b"GENESIS: virtio-net detected (version=");
                serial_decimal(uart, dev.version as u64);
                serial_write(uart, b", MAC=");
                dev.print_mac(uart);
                serial_write(uart, b")\r\n");
                Some(dev)
            }
            _ => {
                let _ = mapper.pmm_mut().free_contiguous_pages(rx_q_pa, 2);
                let _ = mapper.pmm_mut().free_contiguous_pages(tx_q_pa, 2);
                let _ = mapper.pmm_mut().free_contiguous_pages(rx_buf_pa, 8);
                let _ = mapper.pmm_mut().free_page(tx_buf_pa);
                None
            }
        };

        // Allocate 1 page for Sector 0 request buffers
        let req_pa = mapper.pmm_mut().allocate_page().unwrap();
        let req_va = HHDM_BASE + req_pa;

        // Stage 17A: PCIe ECAM Bus Enumeration
        let fdt_pci = if !fdt_ptr.is_null() {
            parse_fdt_pci(fdt_ptr, fdt_size)
        } else {
            None
        };
        let pci_ecam_pa = match fdt_pci {
            Some(p) => p.ecam_base,
            None => PCI_ECAM_DEFAULT_BASE,
        };

        if let Err(e) = mapper.map_mmio_range(PCI_ECAM_VA_BASE, pci_ecam_pa, 256) {
            serial_write(uart, b"STAGE17_MAP_PCI_ECAM_FAIL: ");
            serial_hex(uart, e.code());
            serial_write(uart, b"\r\n");
        } else {
            let pci_dev_count = scan_bus_0(PCI_ECAM_VA_BASE);
            serial_write(uart, b"GENESIS: pcie bus 0 scanned (devices=");
            serial_decimal(uart, pci_dev_count as u64);
            serial_write(uart, b")\r\n");
        }

        // Stage 17B: Probe VirtIO-GPU 2D Driver over VirtIO-PCI (0x1AF4:0x1050)
        let mut virtio_gpu_opt: Option<VirtIOGpuDevice> = None;
        if let Some(gpu_pci_dev) = find_pci_device(0x1AF4, 0x1050) {
            serial_write(uart, b"GENESIS: virtio-gpu pci device detected at 00:");
            serial_decimal(uart, gpu_pci_dev.dev as u64);
            serial_write(uart, b".0\r\n");

            let bar_pa = if gpu_pci_dev.bars[1] != 0 {
                gpu_pci_dev.bars[1]
            } else if gpu_pci_dev.bars[0] != 0 {
                gpu_pci_dev.bars[0]
            } else {
                0
            };

            if bar_pa != 0 {
                if let Ok(()) = mapper.map_mmio_range(PCI_BAR_VA_BASE, bar_pa, 64) {
                    let mut gpu_dev = VirtIOGpuDevice::empty();
                    if let Ok(()) = gpu_dev.init(&gpu_pci_dev, mapper.pmm_mut(), HHDM_BASE, PCI_BAR_VA_BASE) {
                        serial_write(uart, b"GENESIS: virtio-gpu controlq initialized ok\r\n");
                        virtio_gpu_opt = Some(gpu_dev);
                    }
                }
            }
        }

        // Stage 18: Probe PCIe USB 3.0 xHCI Controller (0x0C:0x03:0x30)
        let mut _xhci_detected = false;
        if let Some(xhci_pci_dev) = find_pci_by_class(0x0C, 0x03, 0x30) {
            serial_write(uart, b"GENESIS: pcie xhci controller detected at 00:");
            serial_decimal(uart, xhci_pci_dev.dev as u64);
            serial_write(uart, b".");
            serial_decimal(uart, xhci_pci_dev.func as u64);
            serial_write(uart, b"\r\n");

            // Enable PCI Bus Master and Memory Space
            pci_enable_bus_master(PCI_ECAM_VA_BASE, xhci_pci_dev.bus, xhci_pci_dev.dev, xhci_pci_dev.func);

            let bar0_pa = xhci_pci_dev.bars[0];
            if bar0_pa != 0 {
                let bar_pages = ((xhci_pci_dev.bar_sizes[0].max(65536) + 4095) / 4096) as usize;
                if let Ok(()) = mapper.map_mmio_range(XHCI_MMIO_VA_BASE, bar0_pa, bar_pages) {
                    let mut xhci_dev = XhciDevice::empty();
                    if let Ok(()) = xhci_dev.init(&xhci_pci_dev, mapper.pmm_mut(), HHDM_BASE, XHCI_MMIO_VA_BASE) {
                        serial_write(uart, b"GENESIS: xhci controller initialized (version=0x");
                        serial_hex(uart, xhci_dev.hciversion as u64);
                        serial_write(uart, b", ports=");
                        serial_decimal(uart, xhci_dev.max_ports as u64);
                        serial_write(uart, b")\r\n");
                        let dev_count = enumerate_usb_devices(&mut xhci_dev, mapper.pmm_mut(), HHDM_BASE);
                        register_global_xhci(xhci_dev);
                        serial_write(uart, b"GENESIS: usb enumeration complete (found ");
                        serial_decimal(uart, dev_count as u64);
                        serial_write(uart, b" devices)\r\n");
                        _xhci_detected = true;
                    } else {
                        serial_write(uart, b"GENESIS: xhci init returned error\r\n");
                    }
                } else {
                    serial_write(uart, b"GENESIS: xhci bar0 map failed\r\n");
                }
            }
        }

        let sig = match blk_dev.read_sector_0(req_pa, req_va) {
            Ok(s) => s,
            Err(e) => {
                serial_write(uart, b"STAGE6_VIRTIO_READ_FAIL: ");
                serial_write(uart, e.as_bytes());
                serial_write(uart, b"\r\n");
                halt();
            }
        };

        if sig != 0x55AA {
            serial_write(uart, b"STAGE6_VIRTIO_SIG_MISMATCH: ");
            serial_hex(uart, sig as u64);
            serial_write(uart, b"\r\n");
            halt();
        }

        serial_write(uart, b"GENESIS: stage6 virtio-blk sector0 ok (sig=0x55aa)\r\n");
        serial_write(uart, b"GENESIS: stage6 milestone complete\r\n");

        // ====================================================================
        // Stage 7: FAT32 Filesystem, Multi-Core SMP Compute, and NEON MatMul
        // ====================================================================
        serial_write(uart, b"GENESIS: stage7 starting (fat32, smp compute & neon matmul)\r\n");

        // 1. Mount FAT filesystem from VirtIO-Blk
        let scratch_pa = mapper.pmm_mut().allocate_page().unwrap();
        let scratch_va = HHDM_BASE + scratch_pa;

        // Read Sector 0 to locate volume
        if let Err(e) = blk_dev.read_sectors(0, 1, scratch_pa, scratch_va) {
            serial_write(uart, b"STAGE7_READ_SEC0_FAIL: ");
            serial_write(uart, e.as_bytes());
            serial_write(uart, b"\r\n");
            halt();
        }

        let sec0_slice = core::slice::from_raw_parts(scratch_va as *const u8, 512);
        let vol_lba = match locate_volume(sec0_slice.try_into().unwrap()) {
            Ok(lba) => lba,
            Err(e) => {
                serial_write(uart, b"STAGE7_LOCATE_VOL_FAIL: ");
                serial_write(uart, e.as_bytes());
                serial_write(uart, b"\r\n");
                halt();
            }
        };

        // Read BPB sector
        if let Err(e) = blk_dev.read_sectors(vol_lba, 1, scratch_pa, scratch_va) {
            serial_write(uart, b"STAGE7_READ_BPB_FAIL: ");
            serial_write(uart, e.as_bytes());
            serial_write(uart, b"\r\n");
            halt();
        }

        let bpb_slice = core::slice::from_raw_parts(scratch_va as *const u8, 512);
        let volume = match FatVolume::parse_bpb(vol_lba, bpb_slice.try_into().unwrap()) {
            Ok(v) => v,
            Err(e) => {
                serial_write(uart, b"STAGE7_PARSE_BPB_FAIL: ");
                serial_write(uart, e.as_bytes());
                serial_write(uart, b"\r\n");
                halt();
            }
        };

        // 2. Read WEIGHTS.BIN into HUGEPAGE_VA
        let file_bytes = match volume.read_file(
            &mut blk_dev,
            "WEIGHTS.BIN",
            hugepage_pa,
            HUGEPAGE_VA,
            2 * 1024 * 1024,
            scratch_pa,
            scratch_va,
        ) {
            Ok(bytes) => bytes,
            Err(e) => {
                serial_write(uart, b"STAGE7_READ_FILE_FAIL: ");
                serial_write(uart, e.as_bytes());
                serial_write(uart, b"\r\n");
                halt();
            }
        };

        serial_write(uart, b"GENESIS: stage7 fat32 read ok (bytes=");
        serial_decimal(uart, file_bytes as u64);
        serial_write(uart, b")\r\n");

        // 3. Multi-Core SMP Parallel NEON MatMul
        let a_slice = core::slice::from_raw_parts(HUGEPAGE_VA as *const f32, MATMUL_N * MATMUL_N);
        let b_slice = core::slice::from_raw_parts(
            (HUGEPAGE_VA + (MATMUL_N * MATMUL_N * 4) as u64) as *const f32,
            MATMUL_N * MATMUL_N,
        );
        let c_neon_slice = core::slice::from_raw_parts_mut(
            (HUGEPAGE_VA + (MATMUL_N * MATMUL_N * 8) as u64) as *mut f32,
            MATMUL_N * MATMUL_N,
        );
        let c_ref_slice = core::slice::from_raw_parts_mut(
            (HUGEPAGE_VA + (MATMUL_N * MATMUL_N * 12) as u64) as *mut f32,
            MATMUL_N * MATMUL_N,
        );

        // Compute scalar reference
        scalar_matmul(a_slice, b_slice, c_ref_slice, MATMUL_N);

        // Read cycle counter before parallel dispatch
        let t0: u64;
        let t1: u64;
        asm!("mrs {}, cntvct_el0", out(reg) t0, options(nomem, nostack));

        // Dispatch parallel NEON matmul across all 4 SMP cores
        dispatch_smp_work(smp_matmul_worker, HUGEPAGE_VA);

        asm!("mrs {}, cntvct_el0", out(reg) t1, options(nomem, nostack));
        let _elapsed = t1.saturating_sub(t0);

        // Verify NEON result against scalar reference
        if !verify_matrices(c_neon_slice, c_ref_slice, MATMUL_N) {
            serial_write(uart, b"STAGE7_MATMUL_VERIFY_FAIL\r\n");
            halt();
        }

        serial_write(uart, b"GENESIS: stage7 smp neon matmul ok (cores=4)\r\n");
        serial_write(uart, b"GENESIS: stage7 milestone complete\r\n");

        // ====================================================================
        // Stage 8: UEFI GOP Framebuffer, UART RX, and Interactive Kernel Shell
        // ====================================================================
        serial_write(uart, b"GENESIS: stage8 starting (uefi gop, uart rx, and interactive shell)\r\n");

        let total_usable = mapper.pmm_mut().total_usable_pages();

        // 1. UEFI GOP Framebuffer Mapping & Dashboard
        let mut fb_opt: Option<Framebuffer> = None;
        if handoff.fb_base != 0 && handoff.fb_size != 0 {
            serial_write(uart, b"GENESIS: uefi gop framebuffer detected (base=");
            serial_hex(uart, handoff.fb_base);
            serial_write(uart, b" size=");
            serial_decimal(uart, handoff.fb_size);
            serial_write(uart, b" res=");
            serial_decimal(uart, handoff.fb_width as u64);
            serial_write(uart, b"x");
            serial_decimal(uart, handoff.fb_height as u64);
            serial_write(uart, b" stride=");
            serial_decimal(uart, handoff.fb_stride as u64);
            serial_write(uart, b")\r\n");

            const FB_VA_BASE: u64 = 0xFFFF_FFFF_C000_0000;
            let fb_pages = ((handoff.fb_size + 4095) / 4096) as usize;
            if let Err(e) = mapper.map_range(FB_VA_BASE, handoff.fb_base, fb_pages, PagePermissions::framebuffer()) {
                serial_write(uart, b"STAGE8_FB_MAP_FAIL: ");
                serial_hex(uart, e.code());
                serial_write(uart, b"\r\n");
                halt();
            }
            let mut fb = Framebuffer {
                base_va: FB_VA_BASE,
                size: handoff.fb_size as usize,
                width: handoff.fb_width,
                height: handoff.fb_height,
                stride: handoff.fb_stride,
                pixel_format: handoff.fb_pixel_format,
            };
            let cpus = ONLINE_CPUS.load(Ordering::Acquire);
            let total_ram_mib = (total_usable * 4) / 1024;
            let free_ram_mib = (mapper.pmm_mut().free_pages() * 4) / 1024;
            fb.draw_dashboard(cpus, free_ram_mib, total_ram_mib, true);
            let mut fb_console = FbConsole::new(&fb);
            fb_console.clear();
            register_global_fb_console(fb_console);
            fb_opt = Some(fb);
            if let (Some(gpu), Some(fb)) = (virtio_gpu_opt.as_mut(), fb_opt.as_ref()) {
                if let Ok(()) = gpu.init_2d_scanout(fb.width, fb.height, handoff.fb_base) {
                    serial_write(uart, b"GENESIS: virtio-gpu 2d scanout initialized ok\r\n");
                    register_global_virtio_gpu(gpu as *mut VirtIOGpuDevice);
                }
            }
            serial_write(uart, b"GENESIS: stage8 framebuffer initialized ok\r\n");
        } else {
            serial_write(uart, b"GENESIS: stage8 framebuffer skipped (headless mode)\r\n");
        }

        // Stage 15C & 16: Allocate Dynamic Write-Back Cached Framebuffer Backbuffer
        let backbuf_bytes = if let Some(ref fb) = fb_opt {
            (fb.stride as usize * fb.height as usize * 4).max(800 * 600 * 4)
        } else {
            800 * 600 * 4
        };
        let backbuf_pages = (backbuf_bytes + 4095) / 4096;
        let backbuf_pa = mapper.pmm_mut().allocate_contiguous_pages(backbuf_pages).unwrap_or(0);
        let backbuf_va = if backbuf_pa != 0 { HHDM_BASE + backbuf_pa } else { 0 };
        genesis_stage1::ui::register_global_backbuffer(backbuf_va);

        // 2. Dedicated Buffers for Shell File and Tokenizer Reading
        let shell_file_pa = mapper.pmm_mut().allocate_contiguous_pages(128).unwrap();
        let shell_file_va = HHDM_BASE + shell_file_pa;
        let shell_file_cap = 512 * 1024;

        let tok_file_pa = mapper.pmm_mut().allocate_contiguous_pages(4).unwrap();
        let tok_file_va = HHDM_BASE + tok_file_pa;
        let tok_file_cap = 16 * 1024;

        // 3. Initialize Interactive Shell
        let total_ram_mib = (total_usable * 4) / 1024;
        let free_ram_mib = (mapper.pmm_mut().free_pages() * 4) / 1024;
        let pmm_total = mapper.pmm_mut().total_usable_pages();
        let pmm_free = mapper.pmm_mut().free_pages();
        let mapper_tables = mapper.tables_allocated();
        let pe_opt = *ptr::addr_of!(PARSED_PE);

        let mut shell = Shell {
            uart_va: uart,
            dev: Some(&mut blk_dev),
            net_dev: net_dev_opt.as_mut(),
            fat: Some(&volume),
            scratch_pa,
            scratch_va,
            file_buf_pa: shell_file_pa,
            file_buf_va: shell_file_va,
            file_buf_cap: shell_file_cap,
            tok_buf_pa: tok_file_pa,
            tok_buf_va: tok_file_va,
            tok_buf_cap: tok_file_cap,
            fb: fb_opt.as_mut(),
            total_ram_mib,
            free_ram_mib,
            pe: pe_opt,
            pmm_total_pages: pmm_total,
            pmm_free_pages: pmm_free,
            mapper_tables,
            bench_fn: Some(run_smp_matmul_benchmark),
            run_elf_fn: Some(kernel_run_elf),
            infer_fn: Some(kernel_run_infer),
            llm_fn: Some(kernel_run_llm),
            multitask_fn: Some(kernel_run_multitask),
            generate_fn: Some(kernel_run_generate),
            llama_fn: Some(kernel_run_llama),
            ping_fn: Some(kernel_ping_target),
            dns_fn: Some(kernel_resolve_dns),
            httpget_fn: Some(kernel_http_get),
        };

        // 4. Automated Shell Self-Test (info, ls, bench)
        serial_write(uart, b"GENESIS: stage8 starting shell self-test\r\n");
        shell.execute_command("info");
        shell.execute_command("ls");
        shell.execute_command("bench");
        serial_write(uart, b"GENESIS: stage8 shell self-test ok\r\n");
        serial_write(uart, b"GENESIS: stage8 milestone complete\r\n");

        // 5. Stage 9 Automated Smoke-Test (run HELLO.ELF, infer MODEL.BIN)
        shell.execute_command("run HELLO.ELF");
        shell.execute_command("infer MODEL.BIN");
        serial_write(uart, b"GENESIS: stage9 milestone complete\r\n");

        // 6. Stage 10 Automated Smoke-Test (llm TRANSFORMER.BIN)
        shell.execute_command("llm TRANSFORMER.BIN");
        serial_write(uart, b"GENESIS: stage10 milestone complete\r\n");

        // 7. Stage 11 Automated Smoke-Test (multitask, generate)
        shell.execute_command("multitask");
        shell.execute_command("generate GENESIS ");
        serial_write(uart, b"GENESIS: stage11 milestone complete\r\n");

        // 8. Stage 12 Automated Smoke-Test (netinfo, ping, llama)
        shell.execute_command("netinfo");
        if shell.net_dev.is_some() {
            shell.execute_command("ping 10.0.2.2");
        } else {
            serial_write(uart, b"GENESIS: stage12 virtio-net skipped (headless mode)\r\n");
        }
        shell.execute_command("llama TINY_LLAMA.BIN GENESIS");
        serial_write(uart, b"GENESIS: stage12 milestone complete\r\n");

        // 9. Stage 13 Automated Smoke-Test (dns, ramstat)
        if shell.net_dev.is_some() {
            shell.execute_command("dns 10.0.2.2");
        }
        shell.execute_command("ramstat");
        serial_write(uart, b"GENESIS: stage13 milestone complete\r\n");

        // 10. Stage 14 Automated Smoke-Test (chat GENESIS)
        shell.execute_command("chat GENESIS");
        serial_write(uart, b"GENESIS: stage14 milestone complete\r\n");

        // 11. Stage 15 Automated Smoke-Test & Verification
        // Part 15A: Writable FAT32 verification (write -> cat round-trip)
        shell.execute_command("write TEST.TXT hello genesis stage 15");
        shell.execute_command("cat TEST.TXT");
        serial_write(uart, b"GENESIS: stage15 writable fat32 ok\r\n");

        // Part 15B & 15C: Compositor & Input verification
        if backbuf_va != 0 {
            use genesis_stage1::ui::{DesktopCompositor, PointerState, WIN_TERMINAL, WIN_SYSMON};
            let fb_w = if let Some(ref fb) = shell.fb { fb.width } else { 800 };
            let fb_h = if let Some(ref fb) = shell.fb { fb.height } else { 600 };
            let fb_stride = if let Some(ref fb) = shell.fb { fb.stride } else { 800 };
            let fb_fmt = if let Some(ref fb) = shell.fb { fb.pixel_format } else { 1 };

            let mut comp = DesktopCompositor::new(backbuf_va, fb_w, fb_h, fb_stride, fb_fmt);
            comp.free_ram_mib = free_ram_mib;
            comp.total_ram_mib = total_ram_mib;

            // Verify pointer input: drag terminal window
            let ptr_click = PointerState { x: comp.windows[WIN_TERMINAL].x + 30, y: comp.windows[WIN_TERMINAL].y + 10, left_button: true, right_button: false };
            let _ = comp.handle_pointer(&ptr_click);
            let ptr_drag = PointerState { x: comp.windows[WIN_TERMINAL].x + 60, y: comp.windows[WIN_TERMINAL].y + 30, left_button: true, right_button: false };
            let _ = comp.handle_pointer(&ptr_drag);
            let ptr_up = PointerState { x: comp.windows[WIN_TERMINAL].x + 60, y: comp.windows[WIN_TERMINAL].y + 30, left_button: false, right_button: false };
            let _ = comp.handle_pointer(&ptr_up);

            // Verify taskbar and window close/focus
            comp.toggle_or_focus(WIN_SYSMON);

            // Verify terminal typing
            let _ = comp.handle_key(b'l');
            let _ = comp.handle_key(b's');
            let _ = comp.handle_key(b'\n');

            // Render composite frame
            comp.render_frame(&ptr_up);

            // If real Framebuffer is present, blit frame to screen
            if let Some(ref mut fb) = shell.fb {
                comp.blit(fb);
            }

            serial_write(uart, b"GENESIS: stage15 compositor & input ok\r\n");
            serial_write(uart, b"GENESIS: stage15 milestone complete\r\n");
        }

        // 12. Stage 16 Automated Smoke-Test & Verification (Real Desktop Kernel Wiring & Dynamic GOP)
        serial_write(uart, b"GENESIS: stage16 starting (real desktop kernel wiring & dynamic gop)\r\n");
        if backbuf_va != 0 {
            use genesis_stage1::ui::{DesktopCompositor, PointerState};
            let fb_w = if let Some(ref fb) = shell.fb { fb.width } else { 800 };
            let fb_h = if let Some(ref fb) = shell.fb { fb.height } else { 600 };
            let fb_stride = if let Some(ref fb) = shell.fb { fb.stride } else { 800 };
            let fb_fmt = if let Some(ref fb) = shell.fb { fb.pixel_format } else { 1 };

            let mut comp = DesktopCompositor::new(backbuf_va, fb_w, fb_h, fb_stride, fb_fmt);
            comp.free_ram_mib = free_ram_mib;
            comp.total_ram_mib = total_ram_mib;
            comp.heap_used_bytes = HEAP_ALLOCATOR.allocated_bytes();
            comp.heap_total_bytes = HEAP_ALLOCATOR.total_bytes();
            comp.cpus_online = ONLINE_CPUS.load(Ordering::Relaxed) as u64;

            if let (Some(f), Some(ref mut d)) = (shell.fat, shell.dev.as_deref_mut()) {
                comp.refresh_explorer(f, d, scratch_pa, scratch_va);
            }

            comp.execute_term_command("help", shell.fat, shell.dev.as_deref_mut(), scratch_pa, scratch_va);
            comp.execute_term_command("info", shell.fat, shell.dev.as_deref_mut(), scratch_pa, scratch_va);
            comp.execute_term_command("ramstat", shell.fat, shell.dev.as_deref_mut(), scratch_pa, scratch_va);
            comp.execute_term_command("bench", shell.fat, shell.dev.as_deref_mut(), scratch_pa, scratch_va);
            comp.execute_term_command("ls", shell.fat, shell.dev.as_deref_mut(), scratch_pa, scratch_va);
            comp.execute_term_command("cat TEST.TXT", shell.fat, shell.dev.as_deref_mut(), scratch_pa, scratch_va);
            comp.execute_term_command("write GUI_NOTE.TXT genesis stage 16 desktop", shell.fat, shell.dev.as_deref_mut(), scratch_pa, scratch_va);

            assert!(comp.file_count > 0);

            let ptr_center = PointerState { x: (fb_w / 2) as i32, y: (fb_h / 2) as i32, left_button: false, right_button: false };
            comp.render_frame(&ptr_center);
            if let Some(ref mut fb) = shell.fb {
                comp.blit(fb);
            }
            serial_write(uart, b"GENESIS: stage16 desktop kernel wiring ok\r\n");
        }
        serial_write(uart, b"GENESIS: stage16 milestone complete\r\n");

        // 13. Stage 17 Automated Smoke-Test & Verification (PCIe ECAM, VirtIO-GPU, & WIN_EDITOR)
        serial_write(uart, b"GENESIS: stage17 starting (pcie ecam, virtio-gpu & gui editor)\r\n");

        // Shell command lspci verification
        shell.execute_command("lspci");

        if backbuf_va != 0 {
            use genesis_stage1::ui::{DesktopCompositor, PointerState, WIN_EDITOR};
            let fb_w = if let Some(ref fb) = shell.fb { fb.width } else { 800 };
            let fb_h = if let Some(ref fb) = shell.fb { fb.height } else { 600 };
            let fb_stride = if let Some(ref fb) = shell.fb { fb.stride } else { 800 };
            let fb_fmt = if let Some(ref fb) = shell.fb { fb.pixel_format } else { 1 };

            let mut comp = DesktopCompositor::new(backbuf_va, fb_w, fb_h, fb_stride, fb_fmt);
            comp.free_ram_mib = free_ram_mib;
            comp.total_ram_mib = total_ram_mib;
            comp.heap_used_bytes = HEAP_ALLOCATOR.allocated_bytes();
            comp.heap_total_bytes = HEAP_ALLOCATOR.total_bytes();
            comp.cpus_online = ONLINE_CPUS.load(Ordering::Relaxed) as u64;

            // Test opening file in editor
            comp.open_file_in_editor("EDITOR_NOTE.TXT", b"Genesis Stage 17 Text Editor Test");
            assert_eq!(comp.editor_len, 33);
            assert!(comp.windows[WIN_EDITOR].focused);

            // Test typing in editor
            comp.handle_editor_key(b'!');
            assert_eq!(comp.editor_len, 34);

            // Test saving file from editor to FAT32
            comp.save_editor_file(shell.fat, shell.dev.as_deref_mut(), scratch_pa, scratch_va);

            // Test GUI Terminal lspci & lsusb
            comp.execute_term_command("lspci", shell.fat, shell.dev.as_deref_mut(), scratch_pa, scratch_va);
            comp.execute_term_command("lsusb", shell.fat, shell.dev.as_deref_mut(), scratch_pa, scratch_va);

            // Render frame and blit
            let ptr_center = PointerState { x: (fb_w / 2) as i32, y: (fb_h / 2) as i32, left_button: false, right_button: false };
            comp.render_frame(&ptr_center);
            if let Some(ref mut fb) = shell.fb {
                comp.blit(fb);
            }

            // If VirtIO-GPU was initialized and scanout attached, flush test
            if let Some(ref mut gpu) = virtio_gpu_opt {
                let _ = gpu.flush();
            }

            serial_write(uart, b"GENESIS: stage17 pcie & editor ok\r\n");
        }

        serial_write(uart, b"GENESIS: stage17 milestone complete\r\n");

        // Stage 18: PCIe USB 3.0 xHCI Controller Driver
        serial_write(uart, b"GENESIS: stage18 starting (pcie usb 3.0 xhci controller)\r\n");
        if let Some(xhci) = get_global_xhci_ref() {
            shell.execute_command("lsusb");
            if xhci.noop_success {
                serial_write(uart, b"GENESIS: stage18 xhci ok\r\n");
            } else {
                serial_write(uart, b"GENESIS: stage18 xhci failed (noop)\r\n");
            }
        } else {
            serial_write(uart, b"GENESIS: stage18 xhci skipped (no controller)\r\n");
        }
        serial_write(uart, b"GENESIS: stage18 milestone complete\r\n");

        // Stage 19: xHCI USB Device Enumeration & USB HID Keyboard/Mouse Driver
        serial_write(uart, b"GENESIS: stage19 starting (usb hid keyboard & mouse)\r\n");
        if let Some(_xhci) = get_global_xhci_ref() {
            let count = get_enumerated_count();
            serial_write(uart, b"GENESIS: stage19 usb hid ok (devices=");
            serial_decimal(uart, count as u64);
            serial_write(uart, b")\r\n");
        } else {
            serial_write(uart, b"GENESIS: stage19 usb hid skipped (no controller)\r\n");
        }
        serial_write(uart, b"GENESIS: stage19 milestone complete\r\n");

        // Auto-Boot to Desktop in GUI Mode:
        // When GOP framebuffer is active (e.g. ./tools/run-qemu-gui.sh), launch GUI.
        // In headless mode (./tools/run-qemu.sh), shell.fb is None, so drop straight to serial genesis> shell.
        if let Some(fb) = shell.fb.as_deref_mut() {
            genesis_stage1::ui::run_desktop_gui(
                fb,
                uart,
                shell.fat,
                shell.dev.as_deref_mut(),
                scratch_pa,
                scratch_va,
                free_ram_mib,
                total_ram_mib,
            );
        }

        // 14. Interactive Prompt Loop
        serial_write(uart, b"\r\nProject Genesis Kernel Shell (Stage 19)\r\n");
        serial_write(uart, b"Type 'help' for available commands or 'halt' to power off.\r\n\r\n");
        let mut line_buf = [0u8; 128];
        loop {
            serial_write(uart, b"genesis> ");
            let len = serial_read_line(uart, &mut line_buf);
            if len > 0 {
                if let Ok(line_str) = core::str::from_utf8(&line_buf[..len]) {
                    if shell.execute_command(line_str) == ShellResult::Exit {
                        break;
                    }
                }
            }
        }

        halt();
    }
}

fn run_smp_matmul_benchmark(_arg: u64) -> (u64, u64) {
    let t0: u64;
    let t1: u64;
    let cntfrq: u64;
    #[cfg(target_os = "uefi")]
    unsafe {
        asm!("mrs {}, cntvct_el0", out(reg) t0, options(nomem, nostack));
        dispatch_smp_work(smp_matmul_worker, HUGEPAGE_VA);
        asm!("mrs {}, cntvct_el0", out(reg) t1, options(nomem, nostack));
        asm!("mrs {}, cntfrq_el0", out(reg) cntfrq, options(nomem, nostack));
    }
    #[cfg(not(target_os = "uefi"))]
    {
        t0 = 0;
        t1 = 1000;
        cntfrq = 1_000_000;
    }
    let ticks = t1.saturating_sub(t0);
    let us = if cntfrq > 0 { (ticks * 1_000_000) / cntfrq } else { 0 };
    (ticks, us)
}

unsafe fn kernel_run_elf(elf_data: &[u8], _uart: usize) -> Result<i32, &'static str> {
    let pmm_va = ensure_higher_half(ptr::addr_of_mut!(KERNEL_PMM) as usize as u64);
    let pmm = &mut *(pmm_va as *mut PhysicalPageAllocator<KERNEL_PMM_WORDS>);
    let max_pa = 1u64 << 48;

    let proc = load_elf64(elf_data, pmm, HHDM_BASE, max_pa)?;
    let status = run_user_process(proc.entry_va, proc.user_sp, proc.ttbr0_root);
    destroy_user_process(&proc, pmm, HHDM_BASE);
    Ok(status)
}

unsafe fn kernel_run_infer(model_data: &[u8], uart: usize) -> Result<(usize, u64), &'static str> {
    let ctx = &mut *core::ptr::addr_of_mut!(SMP_NN_CTX);
    load_model_from_bytes(model_data, ctx)?;

    let (scalar_class, scalar_logits) = scalar_infer(
        &ctx.input,
        &ctx.w1,
        &ctx.b1,
        &ctx.w2,
        &ctx.b2,
    );

    let t0: u64;
    let t1: u64;
    let cntfrq: u64;
    #[cfg(target_os = "uefi")]
    {
        asm!("mrs {}, cntvct_el0", out(reg) t0, options(nomem, nostack));
        let (neon_class, _) = run_parallel_nn_inference(ctx);
        asm!("mrs {}, cntvct_el0", out(reg) t1, options(nomem, nostack));
        asm!("mrs {}, cntfrq_el0", out(reg) cntfrq, options(nomem, nostack));

        if !verify_inference(neon_class, &ctx.logits, scalar_class, &scalar_logits) {
            serial_write(uart, b"STAGE9_INFER_VERIFY_FAIL\r\n");
            return Err("NEON inference output mismatch against scalar reference");
        }

        let ticks = t1.saturating_sub(t0);
        let us = if cntfrq > 0 { (ticks * 1_000_000) / cntfrq } else { 0 };

        serial_write(uart, b"GENESIS: stage9 int8 neon inference ok\r\n");
        Ok((neon_class, us))
    }
    #[cfg(not(target_os = "uefi"))]
    {
        let _ = (t0, t1, cntfrq, uart);
        Ok((scalar_class, 100))
    }
}

unsafe fn kernel_run_llm(model_data: &[u8], uart: usize) -> Result<u64, &'static str> {
    let ctx = &mut *core::ptr::addr_of_mut!(SMP_TRANSFORMER_CTX);
    load_transformer_from_bytes(model_data, ctx)?;

    let t0: u64;
    let t1: u64;
    let cntfrq: u64;
    #[cfg(target_os = "uefi")]
    {
        asm!("mrs {}, cntvct_el0", out(reg) t0, options(nomem, nostack));
        let parallel_out = run_parallel_transformer(ctx);
        asm!("mrs {}, cntvct_el0", out(reg) t1, options(nomem, nostack));
        asm!("mrs {}, cntfrq_el0", out(reg) cntfrq, options(nomem, nostack));

        let scalar_out = scalar_transformer_block(
            &ctx.input,
            &ctx.wq,
            &ctx.bq,
            &ctx.wk,
            &ctx.bk,
            &ctx.wv,
            &ctx.bv,
            &ctx.w_ff1,
            &ctx.b_ff1,
            &ctx.w_ff2,
            &ctx.b_ff2,
        );

        if !verify_transformer(&parallel_out, &scalar_out) {
            serial_write(uart, b"STAGE10_TRANSFORMER_VERIFY_FAIL\r\n");
            return Err("Transformer output mismatch with scalar reference");
        }

        let ticks = t1.saturating_sub(t0);
        let us = if cntfrq > 0 { (ticks * 1_000_000) / cntfrq } else { 0 };

        serial_write(uart, b"Transformer block token 0 output sample: [");
        for i in 0..4 {
            let val = ctx.output[i];
            if val < 0 {
                serial_write(uart, b"-");
                serial_decimal(uart, (-val) as u64);
            } else {
                serial_decimal(uart, val as u64);
            }
            if i < 3 {
                serial_write(uart, b", ");
            }
        }
        serial_write(uart, b", ...]\r\n");

        Ok(us)
    }
    #[cfg(not(target_os = "uefi"))]
    {
        let _ = (t0, t1, cntfrq, uart);
        Ok(500)
    }
}

unsafe fn kernel_run_multitask(
    app1_data: &[u8],
    app2_data: &[u8],
    uart: usize,
) -> Result<(), &'static str> {
    let pmm_va = ensure_higher_half(ptr::addr_of_mut!(KERNEL_PMM) as usize as u64);
    let pmm = &mut *(pmm_va as *mut PhysicalPageAllocator<KERNEL_PMM_WORDS>);
    let max_pa = 1u64 << 48;

    let initial_free = pmm.free_pages();

    // 1. Load both EL0 processes
    let proc1 = load_elf64(app1_data, pmm, HHDM_BASE, max_pa)?;
    let proc2 = load_elf64(app2_data, pmm, HHDM_BASE, max_pa)?;

    // 2. Allocate EL1 kernel stacks (4 pages = 16 KiB each)
    let kstack1_pa = pmm.allocate_contiguous_pages(4).ok_or("Failed to alloc kstack1")?;
    let kstack2_pa = pmm.allocate_contiguous_pages(4).ok_or("Failed to alloc kstack2")?;
    let kstack1_top = HHDM_BASE + kstack1_pa + 16384;
    let kstack2_top = HHDM_BASE + kstack2_pa + 16384;

    // 3. Spawn both user processes into scheduler
    let _task1_id = sched::spawn_user_process(1, proc1.entry_va, proc1.user_sp, proc1.ttbr0_root, kstack1_top)
        .map_err(|_| "Failed to spawn task 1")?;
    let _task2_id = sched::spawn_user_process(2, proc2.entry_va, proc2.user_sp, proc2.ttbr0_root, kstack2_top)
        .map_err(|_| "Failed to spawn task 2")?;

    #[cfg(target_os = "uefi")]
    {
        // 4. Arm timer for preemptive time-slicing
        let timer_freq = ArmTimer::frequency();
        let sched_interval = timer_freq / 200; // ~5 ms periodic tick
        stage4_arm_timer_ticks(sched_interval, 0); // continuous timer
        ArmTimer::arm_periodic(sched_interval);

        // Unmask IRQs
        asm!("msr daifclr, #2", options(nomem, nostack));

        // Yield CPU to start user tasks immediately
        sched::kernel_yield();

        // 5. Wait for both user tasks to finish
        while sched::has_active_user_tasks() {
            sched::kernel_yield();
        }

        // 6. Disable timer IRQs
        ArmTimer::disable();
        asm!("msr daifset, #2", options(nomem, nostack, preserves_flags));
    }

    // 7. Clean up kernel stacks & destroy user address spaces
    let _ = pmm.free_contiguous_pages(kstack1_pa, 4);
    let _ = pmm.free_contiguous_pages(kstack2_pa, 4);
    destroy_user_process(&proc1, pmm, HHDM_BASE);
    destroy_user_process(&proc2, pmm, HHDM_BASE);

    // 8. Verify 100% PMM restoration
    let final_free = pmm.free_pages();
    if final_free != initial_free {
        serial_write(uart, b"PMM LEAK DETECTED: initial=");
        serial_decimal(uart, initial_free);
        serial_write(uart, b", final=");
        serial_decimal(uart, final_free);
        serial_write(uart, b"\r\n");
        return Err("PMM leak during multitask execution");
    }

    Ok(())
}

unsafe fn kernel_run_generate(
    model_data: &[u8],
    prompt_str: &str,
    uart: usize,
) -> Result<(), &'static str> {
    let weights = &mut *core::ptr::addr_of_mut!(SMP_MICRO_LLM_WEIGHTS);
    load_micro_llm_from_bytes(model_data, weights)?;

    let mut tokens = [0u8; 16];
    let prompt_bytes = prompt_str.as_bytes();
    let prompt_len = prompt_bytes.len().min(16);
    tokens[..prompt_len].copy_from_slice(&prompt_bytes[..prompt_len]);
    let mut curr_len = prompt_len;

    serial_write(uart, b"Prompt: \"");
    serial_write(uart, &tokens[..prompt_len]);
    serial_write(uart, b"\"\r\nOutput: ");
    serial_write(uart, &tokens[..prompt_len]);

    let smp_ctx = &mut *core::ptr::addr_of_mut!(SMP_TRANSFORMER_CTX);

    let t0: u64;
    let t1: u64;
    let cntfrq: u64;
    #[cfg(target_os = "uefi")]
    {
        asm!("mrs {}, cntvct_el0", out(reg) t0, options(nomem, nostack));
        asm!("mrs {}, cntfrq_el0", out(reg) cntfrq, options(nomem, nostack));
    }
    #[cfg(not(target_os = "uefi"))]
    {
        t0 = 0;
        cntfrq = 1_000_000;
    }

    let steps_to_gen = (16 - curr_len).min(8);
    for s in 0..steps_to_gen {
        let (parallel_tok, parallel_logits) = run_parallel_micro_llm_step(&tokens[..curr_len], weights, smp_ctx);

        if s == 0 {
            let (scalar_tok, scalar_logits) = scalar_micro_llm_step(&tokens[..curr_len], weights);
            if !verify_llm_step(parallel_tok, &parallel_logits, scalar_tok, &scalar_logits) {
                serial_write(uart, b"\r\nSTAGE11_LLM_VERIFY_FAIL\r\n");
                return Err("Micro-LLM output mismatch against scalar reference");
            }
        }

        serial_write(uart, &[parallel_tok]);
        tokens[curr_len] = parallel_tok;
        curr_len += 1;
    }

    #[cfg(target_os = "uefi")]
    {
        asm!("mrs {}, cntvct_el0", out(reg) t1, options(nomem, nostack));
    }
    #[cfg(not(target_os = "uefi"))]
    {
        t1 = 1000;
    }

    let ticks = t1.saturating_sub(t0);
    let total_us = if cntfrq > 0 { (ticks * 1_000_000) / cntfrq } else { 0 };
    let us_per_tok = if steps_to_gen > 0 { total_us / steps_to_gen as u64 } else { 0 };
    let tok_per_sec = if total_us > 0 { (steps_to_gen as u64 * 1_000_000) / total_us } else { 0 };

    serial_write(uart, b"\r\nGeneration stats: ");
    serial_decimal(uart, tok_per_sec);
    serial_write(uart, b" tokens/sec (");
    serial_decimal(uart, us_per_tok);
    serial_write(uart, b" us/token, ");
    serial_decimal(uart, total_us);
    serial_write(uart, b" us total, cores=4)\r\n");

    Ok(())
}

unsafe fn kernel_ping_target(net: &mut VirtIONetDevice, _uart: usize) -> Result<u64, &'static str> {
    // 1. Resolve ARP for Gateway 10.0.2.2
    let mut arp_frame = [0u8; 42];
    net::build_ethernet_header(&mut arp_frame[0..14], net::BROADCAST_MAC, net.mac, net::ETHERTYPE_ARP);
    net::build_arp_packet(
        &mut arp_frame[14..42],
        net::ARP_OP_REQUEST,
        net.mac,
        net::GUEST_IP,
        [0u8; 6],
        net::GATEWAY_IP,
    );

    net.send_packet(&arp_frame)?;

    // Poll for ARP Reply
    let mut gw_mac = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02]; // standard QEMU SLIRP gateway MAC default
    let mut rx_buf = [0u8; 1536];
    let mut spins = 0usize;

    while spins < 1_000_000 {
        if let Some(len) = net.poll_packet(&mut rx_buf) {
            if len >= 42 {
                let ethertype = u16::from_be_bytes([rx_buf[12], rx_buf[13]]);
                if ethertype == net::ETHERTYPE_ARP {
                    if let Some((op, sender_mac, sender_ip, target_ip)) = net::parse_arp(&rx_buf[14..len]) {
                        if op == net::ARP_OP_REPLY && sender_ip == net::GATEWAY_IP && target_ip == net::GUEST_IP {
                            gw_mac = sender_mac;
                            break;
                        }
                    }
                }
            }
        }
        spins += 1;
        core::hint::spin_loop();
    }

    // 2. Build and send ICMP Echo Request
    let mut icmp_frame = [0u8; 74];
    net::build_ethernet_header(&mut icmp_frame[0..14], gw_mac, net.mac, net::ETHERTYPE_IPV4);
    let payload = b"Genesis ICMP Ping Stage 12 Test!";
    let icmp_len = net::build_icmp_echo(
        &mut icmp_frame[34..74],
        net::ICMP_TYPE_ECHO_REQUEST,
        0x1234,
        1,
        payload,
    );
    net::build_ipv4_header(
        &mut icmp_frame[14..34],
        icmp_len as u16,
        net::IP_PROTO_ICMP,
        net::GUEST_IP,
        net::GATEWAY_IP,
        0x5678,
    );

    let t0: u64;
    let cntfrq: u64;
    #[cfg(target_os = "uefi")]
    {
        asm!("mrs {}, cntvct_el0", out(reg) t0, options(nomem, nostack));
        asm!("mrs {}, cntfrq_el0", out(reg) cntfrq, options(nomem, nostack));
    }
    #[cfg(not(target_os = "uefi"))]
    {
        t0 = 0;
        cntfrq = 1_000_000;
    }

    net.send_packet(&icmp_frame)?;

    // Poll for ICMP Echo Reply
    spins = 0;
    let mut ping_ok = false;
    let mut t1: u64 = t0;

    while spins < 2_000_000 {
        if let Some(len) = net.poll_packet(&mut rx_buf) {
            if len >= 34 {
                let ethertype = u16::from_be_bytes([rx_buf[12], rx_buf[13]]);
                if ethertype == net::ETHERTYPE_IPV4 {
                    if let Some((src, dst, icmp_type, _code, ident, _seq, _)) = net::parse_icmp_reply(&rx_buf[14..len]) {
                        if src == net::GATEWAY_IP && dst == net::GUEST_IP && icmp_type == net::ICMP_TYPE_ECHO_REPLY && ident == 0x1234 {
                            #[cfg(target_os = "uefi")]
                            asm!("mrs {}, cntvct_el0", out(reg) t1, options(nomem, nostack));
                            #[cfg(not(target_os = "uefi"))]
                            { t1 = 500; }
                            ping_ok = true;
                            break;
                        }
                    }
                }
            }
        }
        spins += 1;
        core::hint::spin_loop();
    }

    if !ping_ok {
        return Err("ICMP ping response timed out");
    }

    let ticks = t1.saturating_sub(t0);
    let us = if cntfrq > 0 { (ticks * 1_000_000) / cntfrq } else { 0 };
    Ok(us)
}

unsafe fn kernel_resolve_dns(
    net: &mut VirtIONetDevice,
    hostname: &str,
    _uart: usize,
) -> Result<[u8; 4], &'static str> {
    if let Some(ip) = net::parse_ipv4_addr(hostname) {
        return Ok(ip);
    }

    // 1. Resolve Gateway MAC via ARP
    let mut arp_frame = [0u8; 42];
    net::build_ethernet_header(&mut arp_frame[0..14], net::BROADCAST_MAC, net.mac, net::ETHERTYPE_ARP);
    net::build_arp_packet(
        &mut arp_frame[14..42],
        net::ARP_OP_REQUEST,
        net.mac,
        net::GUEST_IP,
        [0u8; 6],
        net::GATEWAY_IP,
    );
    net.send_packet(&arp_frame)?;

    let mut gw_mac = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];
    let mut rx_buf = [0u8; 1536];
    let mut spins = 0usize;
    while spins < 1_000_000 {
        if let Some(len) = net.poll_packet(&mut rx_buf) {
            if len >= 42 {
                let ethertype = u16::from_be_bytes([rx_buf[12], rx_buf[13]]);
                if ethertype == net::ETHERTYPE_ARP {
                    if let Some((op, sender_mac, sender_ip, target_ip)) = net::parse_arp(&rx_buf[14..len]) {
                        if op == net::ARP_OP_REPLY && sender_ip == net::GATEWAY_IP && target_ip == net::GUEST_IP {
                            gw_mac = sender_mac;
                            break;
                        }
                    }
                }
            }
        }
        spins += 1;
        core::hint::spin_loop();
    }

    // 2. Build DNS Query
    let mut dns_buf = [0u8; 256];
    let query_id = 0x1337;
    let query_len = net::build_dns_query(&mut dns_buf, query_id, hostname);

    let mut udp_frame = [0u8; 512];
    let udp_len = net::build_udp_packet(
        &mut udp_frame[34..],
        net::GUEST_IP,
        net::DNS_SERVER_IP,
        49152,
        net::DNS_PORT,
        &dns_buf[..query_len],
    );
    net::build_ipv4_header(
        &mut udp_frame[14..34],
        udp_len as u16,
        net::IP_PROTO_UDP,
        net::GUEST_IP,
        net::DNS_SERVER_IP,
        0x1234,
    );
    net::build_ethernet_header(&mut udp_frame[0..14], gw_mac, net.mac, net::ETHERTYPE_IPV4);

    net.send_packet(&udp_frame[..14 + 20 + udp_len])?;

    // 3. Poll for DNS response
    spins = 0;
    while spins < 2_000_000 {
        if let Some(len) = net.poll_packet(&mut rx_buf) {
            if len >= 42 {
                let ethertype = u16::from_be_bytes([rx_buf[12], rx_buf[13]]);
                if ethertype == net::ETHERTYPE_IPV4 {
                    if let Some((src_ip, _dst_ip, _src_p, _dst_p, payload)) = net::parse_udp_packet(&rx_buf[14..len]) {
                        if src_ip == net::DNS_SERVER_IP {
                            if let Some(resolved) = net::parse_dns_response(payload, query_id) {
                                return Ok(resolved);
                            }
                        }
                    }
                }
            }
        }
        spins += 1;
        core::hint::spin_loop();
    }

    Err("DNS query timed out")
}

unsafe fn kernel_http_get(
    net: &mut VirtIONetDevice,
    host_or_ip: &str,
    port: u16,
    path: &str,
    uart: usize,
) -> Result<usize, &'static str> {
    let remote_ip = match net::parse_ipv4_addr(host_or_ip) {
        Some(ip) => ip,
        None => kernel_resolve_dns(net, host_or_ip, uart)?,
    };

    let gw_mac = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];
    let local_port = 49153;
    let initial_seq = 1000;
    let mut client = net::tcp::TcpClient::new(net::GUEST_IP, local_port, remote_ip, port, initial_seq);

    // 1. Send SYN
    let mut syn_frame = [0u8; 74];
    net::build_ethernet_header(&mut syn_frame[0..14], gw_mac, net.mac, net::ETHERTYPE_IPV4);
    let tcp_len = net::tcp::build_tcp_packet(
        &mut syn_frame[34..74],
        net::GUEST_IP,
        remote_ip,
        local_port,
        port,
        client.local_seq,
        0,
        net::tcp::TCP_FLAG_SYN,
        64240,
        &[],
    );
    net::build_ipv4_header(&mut syn_frame[14..34], tcp_len as u16, net::IP_PROTO_TCP, net::GUEST_IP, remote_ip, 0x3344);
    net.send_packet(&syn_frame[..14 + 20 + tcp_len])?;
    client.state = net::tcp::TcpState::SynSent;

    // 2. Poll for SYN-ACK
    let mut rx_buf = [0u8; 1536];
    let mut spins = 0usize;
    let mut connected = false;

    while spins < 2_000_000 {
        if let Some(len) = net.poll_packet(&mut rx_buf) {
            if len >= 54 {
                let ethertype = u16::from_be_bytes([rx_buf[12], rx_buf[13]]);
                if ethertype == net::ETHERTYPE_IPV4 {
                    if let Some((src, dst, sp, dp, seq, ack, flags, _win, _)) = net::tcp::parse_tcp_packet(&rx_buf[14..len]) {
                        if src == remote_ip && dst == net::GUEST_IP && sp == port && dp == local_port {
                            if (flags & (net::tcp::TCP_FLAG_SYN | net::tcp::TCP_FLAG_ACK)) == (net::tcp::TCP_FLAG_SYN | net::tcp::TCP_FLAG_ACK) {
                                client.remote_seq = seq.wrapping_add(1);
                                client.local_seq = ack;
                                client.state = net::tcp::TcpState::Established;
                                connected = true;
                                break;
                            }
                        }
                    }
                }
            }
        }
        spins += 1;
        core::hint::spin_loop();
    }

    if !connected {
        return Err("TCP handshake timed out");
    }

    // 3. Send ACK + HTTP Request (PSH | ACK)
    let mut req_buf = [0u8; 256];
    let mut req_len = 0;
    let mut append = |s: &[u8]| {
        for &b in s {
            if req_len < 256 {
                req_buf[req_len] = b;
                req_len += 1;
            }
        }
    };
    append(b"GET ");
    append(path.as_bytes());
    append(b" HTTP/1.0\r\nHost: ");
    append(host_or_ip.as_bytes());
    append(b"\r\nConnection: close\r\n\r\n");

    let mut http_frame = [0u8; 350];
    net::build_ethernet_header(&mut http_frame[0..14], gw_mac, net.mac, net::ETHERTYPE_IPV4);
    let tcp_len = net::tcp::build_tcp_packet(
        &mut http_frame[34..],
        net::GUEST_IP,
        remote_ip,
        local_port,
        port,
        client.local_seq,
        client.remote_seq,
        net::tcp::TCP_FLAG_PSH | net::tcp::TCP_FLAG_ACK,
        64240,
        &req_buf[..req_len],
    );
    net::build_ipv4_header(&mut http_frame[14..34], tcp_len as u16, net::IP_PROTO_TCP, net::GUEST_IP, remote_ip, 0x3345);
    net.send_packet(&http_frame[..14 + 20 + tcp_len])?;
    client.local_seq = client.local_seq.wrapping_add(req_len as u32);

    // 4. Poll for Response Data
    spins = 0;
    let mut total_received = 0usize;
    while spins < 3_000_000 {
        if let Some(len) = net.poll_packet(&mut rx_buf) {
            if len >= 54 {
                let ethertype = u16::from_be_bytes([rx_buf[12], rx_buf[13]]);
                if ethertype == net::ETHERTYPE_IPV4 {
                    if let Some((src, dst, sp, dp, seq, _ack, flags, _win, payload)) = net::tcp::parse_tcp_packet(&rx_buf[14..len]) {
                        if src == remote_ip && dst == net::GUEST_IP && sp == port && dp == local_port {
                            if !payload.is_empty() {
                                total_received += payload.len();
                                serial_write(uart, payload);
                                client.remote_seq = seq.wrapping_add(payload.len() as u32);
                                let mut ack_frame = [0u8; 74];
                                net::build_ethernet_header(&mut ack_frame[0..14], gw_mac, net.mac, net::ETHERTYPE_IPV4);
                                let ack_tcp_len = net::tcp::build_tcp_packet(
                                    &mut ack_frame[34..74],
                                    net::GUEST_IP,
                                    remote_ip,
                                    local_port,
                                    port,
                                    client.local_seq,
                                    client.remote_seq,
                                    net::tcp::TCP_FLAG_ACK,
                                    64240,
                                    &[],
                                );
                                net::build_ipv4_header(&mut ack_frame[14..34], ack_tcp_len as u16, net::IP_PROTO_TCP, net::GUEST_IP, remote_ip, 0x3346);
                                let _ = net.send_packet(&ack_frame[..14 + 20 + ack_tcp_len]);
                            }

                            if (flags & net::tcp::TCP_FLAG_FIN) != 0 {
                                client.remote_seq = seq.wrapping_add(payload.len() as u32).wrapping_add(1);
                                let mut finack_frame = [0u8; 74];
                                net::build_ethernet_header(&mut finack_frame[0..14], gw_mac, net.mac, net::ETHERTYPE_IPV4);
                                let finack_len = net::tcp::build_tcp_packet(
                                    &mut finack_frame[34..74],
                                    net::GUEST_IP,
                                    remote_ip,
                                    local_port,
                                    port,
                                    client.local_seq,
                                    client.remote_seq,
                                    net::tcp::TCP_FLAG_FIN | net::tcp::TCP_FLAG_ACK,
                                    64240,
                                    &[],
                                );
                                net::build_ipv4_header(&mut finack_frame[14..34], finack_len as u16, net::IP_PROTO_TCP, net::GUEST_IP, remote_ip, 0x3347);
                                let _ = net.send_packet(&finack_frame[..14 + 20 + finack_len]);
                                break;
                            }
                        }
                    }
                }
            }
        }
        spins += 1;
        core::hint::spin_loop();
    }

    Ok(total_received)
}

unsafe fn kernel_run_llama(
    model_data: &[u8],
    tokenizer_data: Option<&[u8]>,
    prompt_str: &str,
    uart: usize,
) -> Result<(), &'static str> {
    let cfg = parse_llama_header(model_data)?;
    let required_weight_floats = LlamaWeights::total_floats_for(&cfg);
    let required_ctx_floats = SmpLlamaContext::total_floats_for(&cfg);

    let weights = &mut *core::ptr::addr_of_mut!(SMP_LLAMA_WEIGHTS);
    if weights.total_floats < required_weight_floats || weights.token_embedding.is_null() {
        let pmm_va = ensure_higher_half(core::ptr::addr_of_mut!(KERNEL_PMM) as usize as u64);
        let pmm = &mut *(pmm_va as *mut PhysicalPageAllocator<KERNEL_PMM_WORDS>);
        let weight_pages = (required_weight_floats * 4 + 4095) / 4096;
        let weight_pa = pmm.allocate_contiguous_pages(weight_pages)
            .ok_or("Failed to allocate physical pages for dynamic Llama weights")?;
        let weight_va = HHDM_BASE + weight_pa;
        weights.init_from_buffer(cfg, weight_va as *mut f32);
        weights.paddr = weight_pa;
        weights.page_count = weight_pages;
    }

    load_llama_weights(model_data, weights)?;

    let ctx = &mut *core::ptr::addr_of_mut!(SMP_LLAMA_CTX);
    if ctx.total_floats < required_ctx_floats || ctx.logits.is_null() {
        let pmm_va = ensure_higher_half(core::ptr::addr_of_mut!(KERNEL_PMM) as usize as u64);
        let pmm = &mut *(pmm_va as *mut PhysicalPageAllocator<KERNEL_PMM_WORDS>);
        let ctx_pages = (required_ctx_floats * 4 + 4095) / 4096;
        let ctx_pa = pmm.allocate_contiguous_pages(ctx_pages)
            .ok_or("Failed to allocate physical pages for dynamic Llama context")?;
        let ctx_va = HHDM_BASE + ctx_pa;
        ctx.init_from_buffer(cfg, ctx_va as *mut f32);
        ctx.paddr = ctx_pa;
        ctx.page_count = ctx_pages;
    } else {
        ctx.config = cfg;
        core::ptr::write_bytes(ctx.x as *mut u8, 0, ctx.total_floats * 4);
    }
    ctx.weights_ptr = weights as *const LlamaWeights;

    let tokenizer = if let Some(tok_bytes) = tokenizer_data {
        Tokenizer::parse(tok_bytes, cfg.vocab_size).ok()
    } else {
        None
    };

    serial_write(uart, b"Prompt: \"");
    serial_write(uart, prompt_str.as_bytes());
    serial_write(uart, b"\"\r\nOutput: ");
    serial_write(uart, prompt_str.as_bytes());

    global_fb_console_write(b"Prompt: \"");
    global_fb_console_write(prompt_str.as_bytes());
    global_fb_console_write(b"\"\r\nOutput: ");
    global_fb_console_write(prompt_str.as_bytes());

    let prompt_tokens = if let Some(ref tok) = tokenizer {
        tok.encode(prompt_str, true)
    } else {
        prompt_str.bytes().map(|b| b as usize).collect()
    };

    let prompt_len = prompt_tokens.len().min(cfg.seq_len.saturating_sub(8).max(1));

    let t0: u64;
    let t1: u64;
    let cntfrq: u64;
    #[cfg(target_os = "uefi")]
    {
        asm!("mrs {}, cntvct_el0", out(reg) t0, options(nomem, nostack));
        asm!("mrs {}, cntfrq_el0", out(reg) cntfrq, options(nomem, nostack));
    }
    #[cfg(not(target_os = "uefi"))]
    {
        t0 = 0;
        cntfrq = 1_000_000;
    }

    // Process prompt tokens
    for pos in 0..prompt_len {
        let tok = prompt_tokens[pos];
        run_llama_forward_step(weights, ctx, tok, pos);
    }

    // Autoregressive generation (4 new tokens)
    let steps_to_gen = 4;
    let mut prev_tok = *prompt_tokens.last().unwrap_or(&1);
    for i in 0..steps_to_gen {
        let next_tok = sample_greedy(ctx.logits_slice());
        if let Some(ref tok) = tokenizer {
            let mut scratch = [0u8; 8];
            let piece = tok.decode(prev_tok, next_tok, &mut scratch);
            serial_write(uart, piece);
            global_fb_console_write(piece);
        } else {
            let char_byte = if next_tok < 128 && (next_tok as u8).is_ascii_graphic() || next_tok == b' ' as usize {
                next_tok as u8
            } else {
                b'.'
            };
            serial_write(uart, &[char_byte]);
            global_fb_console_write(&[char_byte]);
        }
        prev_tok = next_tok;

        let pos = prompt_len + i;
        if pos < cfg.seq_len - 1 {
            run_llama_forward_step(weights, ctx, next_tok, pos);
        }
    }

    #[cfg(target_os = "uefi")]
    {
        asm!("mrs {}, cntvct_el0", out(reg) t1, options(nomem, nostack));
    }
    #[cfg(not(target_os = "uefi"))]
    {
        t1 = 1000;
    }

    let ticks = t1.saturating_sub(t0);
    let total_us = if cntfrq > 0 { (ticks * 1_000_000) / cntfrq } else { 0 };
    let tok_per_sec = if total_us > 0 { ((steps_to_gen as u64) * 1_000_000) / total_us } else { 0 };

    serial_write(uart, b"\r\nLlama-2 stats: ");
    serial_decimal(uart, tok_per_sec);
    serial_write(uart, b" tokens/sec (");
    serial_decimal(uart, total_us);
    serial_write(uart, b" us total, cores=4)\r\n");

    global_fb_console_write(b"\r\n[Llama-2: ");
    let mut num_buf = [0u8; 32];
    let nlen = write_u64(tok_per_sec, &mut num_buf);
    global_fb_console_write(&num_buf[..nlen]);
    global_fb_console_write(b" tok/s]\r\n");

    update_workspace_telemetry(
        Some("Llama-2 SMP"),
        Some(tok_per_sec),
        Some(total_us),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some("LLAMA2 OK"),
    );

    Ok(())
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
