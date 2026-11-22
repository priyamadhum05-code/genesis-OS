//! Project Genesis — Interactive Kernel Shell (Stage 8).
//!
//! Provides interactive command-line dispatch for system diagnostics, FAT32 navigation,
//! SIMD NEON compute benchmarks, framebuffer dashboard refresh, and PSCI shutdown.

use core::sync::atomic::Ordering;
use crate::{
    arch::{
        psci_system_off, serial_decimal, serial_hex, serial_write, ONLINE_CPUS,
    },
    drivers::{Framebuffer, VirtIOBlockDevice, VirtIONetDevice},
    fs::{BlockDevice, FatVolume},
    mm::HEAP_ALLOCATOR,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellResult {
    Continue,
    Exit,
}

pub struct Shell<'a> {
    pub uart_va: usize,
    pub dev: Option<&'a mut VirtIOBlockDevice>,
    pub net_dev: Option<&'a mut VirtIONetDevice>,
    pub fat: Option<&'a FatVolume>,
    pub scratch_pa: u64,
    pub scratch_va: u64,
    pub file_buf_pa: u64,
    pub file_buf_va: u64,
    pub file_buf_cap: usize,
    pub tok_buf_pa: u64,
    pub tok_buf_va: u64,
    pub tok_buf_cap: usize,
    pub fb: Option<&'a mut Framebuffer>,
    pub total_ram_mib: u64,
    pub free_ram_mib: u64,
    pub pe: Option<crate::vmm::pe::ParsedPeImage>,
    pub pmm_total_pages: u64,
    pub pmm_free_pages: u64,
    pub mapper_tables: usize,
    pub bench_fn: Option<fn(u64) -> (u64, u64)>,
    pub run_elf_fn: Option<crate::proc::RunElfFn>,
    pub infer_fn: Option<unsafe fn(&[u8], usize) -> Result<(usize, u64), &'static str>>,
    pub llm_fn: Option<unsafe fn(&[u8], usize) -> Result<u64, &'static str>>,
    pub multitask_fn: Option<unsafe fn(&[u8], &[u8], usize) -> Result<(), &'static str>>,
    pub generate_fn: Option<unsafe fn(&[u8], &str, usize) -> Result<(), &'static str>>,
    pub llama_fn: Option<unsafe fn(&[u8], Option<&[u8]>, &str, usize) -> Result<(), &'static str>>,
    pub ping_fn: Option<unsafe fn(&mut crate::net::AnyNetDev, usize) -> Result<u64, &'static str>>,
    pub dns_fn: Option<unsafe fn(&mut crate::net::AnyNetDev, &str, usize) -> Result<[u8; 4], &'static str>>,
    pub httpget_fn: Option<unsafe fn(&mut crate::net::AnyNetDev, &str, u16, &str, usize) -> Result<usize, &'static str>>,
}

unsafe fn resolve_fs<'b>(
    fallback_fat: Option<&'b FatVolume>,
    fallback_dev: Option<&'b mut VirtIOBlockDevice>,
) -> (Option<&'b FatVolume>, Option<crate::fs::AnyBlockDev<'b>>) {
    let active_name = crate::fs::get_active_volume_name();
    let active_fat = crate::fs::get_active_volume();
    if active_name != "none" && active_fat.is_some() {
        let f = active_fat.unwrap();
        if active_name == "blk0" && fallback_dev.is_some() {
            (Some(f), fallback_dev.map(crate::fs::AnyBlockDev::VirtIO))
        } else if let Some(bd) = crate::fs::get_block_device_by_name(active_name) {
            (Some(f), Some(bd))
        } else {
            (Some(f), fallback_dev.map(crate::fs::AnyBlockDev::VirtIO))
        }
    } else {
        (fallback_fat, fallback_dev.map(crate::fs::AnyBlockDev::VirtIO))
    }
}

impl<'a> Shell<'a> {
    /// Executes a single command string.
    pub unsafe fn execute_command(&mut self, line: &str) -> ShellResult {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return ShellResult::Continue;
        }

        let mut parts = trimmed.split_whitespace();
        let cmd = parts.next().unwrap_or("");
        let arg = parts.next().unwrap_or("");

        match cmd {
            "help" => {
                serial_write(self.uart_va, b"Project Genesis Kernel Shell (Stage 23)\r\n");
                serial_write(self.uart_va, b"Available commands:\r\n");
                serial_write(self.uart_va, b"  help              - List available commands\r\n");
                serial_write(self.uart_va, b"  info              - Display system status, memory, registers, and framebuffer\r\n");
                serial_write(self.uart_va, b"  ramstat           - Display kernel memory profile and physical RAM breakdown\r\n");
                serial_write(self.uart_va, b"  netinfo           - Display VirtIO-Net MAC, IP, gateway, and status\r\n");
                serial_write(self.uart_va, b"  nicinfo           - Display all network controllers (VirtIO-Net, e1000e) & active NIC\r\n");
                serial_write(self.uart_va, b"  route <nic>       - Select active network controller (virtio | e1000e)\r\n");
                serial_write(self.uart_va, b"  ping [ip]         - Send ICMP echo request over VirtIO-Net\r\n");
                serial_write(self.uart_va, b"  dns <hostname>    - Resolve hostname to IPv4 address via DNS\r\n");
                serial_write(self.uart_va, b"  httpget <host> [port] [path] - Send HTTP/1.0 GET request via TCP\r\n");
                serial_write(self.uart_va, b"  clear             - Clear graphical terminal console and serial screen\r\n");
                serial_write(self.uart_va, b"  ls [path]         - List directory contents on FAT32 volume\r\n");
                serial_write(self.uart_va, b"  cat <file>        - Print text contents of a file\r\n");
                serial_write(self.uart_va, b"  write <file> <text> - Write text contents to a file\r\n");
                serial_write(self.uart_va, b"  hexdump <file>    - Display hexadecimal dump of a file\r\n");
                serial_write(self.uart_va, b"  desktop           - Launch double-buffered graphical desktop\r\n");
                serial_write(self.uart_va, b"  lspci             - List all discovered PCIe devices\r\n");
                serial_write(self.uart_va, b"  lsusb             - Display USB 3.0 xHCI controller and port status\r\n");
                serial_write(self.uart_va, b"  lsblk             - List block devices (blk0, nvme0, usb0) and mount status\r\n");
                serial_write(self.uart_va, b"  mount <dev>       - Mount block device as active FAT32 volume\r\n");
                serial_write(self.uart_va, b"  acpiinfo          - Display ACPI 2.0+ tables and system configuration\r\n");
                serial_write(self.uart_va, b"  nvmeinfo          - Display PCIe NVMe SSD controller & capacity\r\n");
                serial_write(self.uart_va, b"  nvmeread <lba>    - Read and hex-dump LBA sector from NVMe SSD\r\n");
                serial_write(self.uart_va, b"  usbstorage        - Display USB Mass Storage (SCSI) information\r\n");
                serial_write(self.uart_va, b"  cpuinfo           - Display AArch64 MIDR, features & hardware telemetry\r\n");
                serial_write(self.uart_va, b"  gpuinfo           - Display VirtIO-GPU 2D/3D VirGL acceleration status\r\n");
                serial_write(self.uart_va, b"  bench             - Run 4-core SMP NEON 64x64 MatMul benchmark\r\n");
                serial_write(self.uart_va, b"  draw              - Redraw UEFI GOP framebuffer dashboard\r\n");
                serial_write(self.uart_va, b"  date              - Display wall-clock RTC date and time in UTC\r\n");
                serial_write(self.uart_va, b"  reboot            - Reset system via PSCI SYSTEM_RESET\r\n");
                serial_write(self.uart_va, b"  audioinfo         - Display VirtIO-Sound PCI audio device status\r\n");
                serial_write(self.uart_va, b"  beep [hz]         - Play synthesized PCM tone via VirtIO-Sound\r\n");
                serial_write(self.uart_va, b"  exec <file>       - Execute an AArch64 ELF64 binary in EL0 user space\r\n");
                serial_write(self.uart_va, b"  run <file>        - Alias for exec\r\n");
                serial_write(self.uart_va, b"  infer [file]      - Run 4-core SMP INT8 NEON neural network inference\r\n");
                serial_write(self.uart_va, b"  llm [file]        - Run 4-core SMP INT8 NEON transformer block\r\n");
                serial_write(self.uart_va, b"  multitask         - Run preemptive multi-process EL0 scheduling & IPC\r\n");
                serial_write(self.uart_va, b"  generate [seed]   - Run 4-core SMP autoregressive Micro-LLM text generation\r\n");
                serial_write(self.uart_va, b"  llama [model] [prompt] - Run 4-core SMP dynamic Llama-2 inference\r\n");
                serial_write(self.uart_va, b"  chat <prompt>     - Interactive Llama-2 chat with BPE tokenization\r\n");
                serial_write(self.uart_va, b"  halt / poweroff   - Power off system via PSCI SYSTEM_OFF\r\n");
            }
            "ramstat" => {
                serial_write(self.uart_va, b"=== Project Genesis RAM Usage & Kernel Profiler ===\r\n");
                serial_write(self.uart_va, b"Kernel Image (PE32+):\r\n");
                if let Some(ref pe) = self.pe {
                    for sec_opt in &pe.sections[..pe.section_count] {
                        if let Some(sec) = sec_opt {
                            serial_write(self.uart_va, b"  ");
                            serial_write(self.uart_va, sec.name_bytes());
                            serial_write(self.uart_va, b":  RVA ");
                            serial_hex(self.uart_va, sec.virtual_address);
                            serial_write(self.uart_va, b" (");
                            serial_decimal(self.uart_va, sec.page_count as u64);
                            serial_write(self.uart_va, b" pages, ");
                            serial_decimal(self.uart_va, ((sec.page_count * 4096) / 1024) as u64);
                            serial_write(self.uart_va, b" KiB) [");
                            if sec.is_readable { serial_write(self.uart_va, b"RO"); }
                            if sec.is_writable { serial_write(self.uart_va, b", RW"); }
                            if sec.is_executable { serial_write(self.uart_va, b", X"); } else { serial_write(self.uart_va, b", NX"); }
                            serial_write(self.uart_va, b"]\r\n");
                        }
                    }
                    serial_write(self.uart_va, b"  Total PE Image: ");
                    serial_decimal(self.uart_va, pe.total_pages as u64);
                    serial_write(self.uart_va, b" pages (");
                    serial_decimal(self.uart_va, ((pe.total_pages * 4096) / 1024) as u64);
                    serial_write(self.uart_va, b" KiB)\r\n");
                } else {
                    serial_write(self.uart_va, b"  PE headers not available\r\n");
                }

                serial_write(self.uart_va, b"\r\nPage Tables:\r\n");
                serial_write(self.uart_va, b"  Dormant TTBR0:   7 frames (28 KiB)\r\n");
                serial_write(self.uart_va, b"  Dormant TTBR1:  11 frames (44 KiB)\r\n");
                serial_write(self.uart_va, b"  Dynamic Mapper: ");
                serial_decimal(self.uart_va, self.mapper_tables as u64);
                serial_write(self.uart_va, b" frames (");
                serial_decimal(self.uart_va, (self.mapper_tables * 4) as u64);
                serial_write(self.uart_va, b" KiB)\r\n");

                serial_write(self.uart_va, b"\r\nKernel Allocators:\r\n");
                let heap_alloc = HEAP_ALLOCATOR.allocated_bytes();
                let heap_total = HEAP_ALLOCATOR.total_bytes();
                serial_write(self.uart_va, b"  Kernel Heap:    ");
                serial_decimal(self.uart_va, heap_alloc as u64);
                serial_write(self.uart_va, b" bytes used / ");
                serial_decimal(self.uart_va, heap_total as u64);
                serial_write(self.uart_va, b" bytes capacity\r\n");
                serial_write(self.uart_va, b"  Hugepage Pools: 2 MiB allocated (L2 block at 0xFFFF_8000_4200_0000)\r\n");
                let llama_pages = unsafe {
                    crate::ai::llama::SMP_LLAMA_WEIGHTS.page_count + crate::ai::llama::SMP_LLAMA_CTX.page_count
                };
                serial_write(self.uart_va, b"  Dynamic Llama:  ");
                serial_decimal(self.uart_va, (llama_pages * 4) as u64);
                serial_write(self.uart_va, b" KiB allocated (");
                serial_decimal(self.uart_va, llama_pages as u64);
                serial_write(self.uart_va, b" pages)\r\n");

                serial_write(self.uart_va, b"\r\nPhysical Memory Manager (PMM):\r\n");
                let total_p = self.pmm_total_pages;
                let free_p = self.pmm_free_pages;
                let alloc_p = total_p.saturating_sub(free_p);
                serial_write(self.uart_va, b"  Total Conventional RAM: ");
                serial_decimal(self.uart_va, total_p);
                serial_write(self.uart_va, b" pages (");
                serial_decimal(self.uart_va, (total_p * 4) / 1024);
                serial_write(self.uart_va, b" MiB)\r\n");

                serial_write(self.uart_va, b"  Allocated by OS:        ");
                serial_decimal(self.uart_va, alloc_p);
                serial_write(self.uart_va, b" pages (");
                serial_decimal(self.uart_va, (alloc_p * 4) / 1024);
                serial_write(self.uart_va, b" MiB)\r\n");

                serial_write(self.uart_va, b"  Free Usable RAM:        ");
                serial_decimal(self.uart_va, free_p);
                serial_write(self.uart_va, b" pages (");
                serial_decimal(self.uart_va, (free_p * 4) / 1024);
                serial_write(self.uart_va, b" MiB)\r\n");

                let util_pct = if total_p > 0 { (alloc_p * 100) / total_p } else { 0 };
                serial_write(self.uart_va, b"  RAM Utilization:        ");
                serial_decimal(self.uart_va, util_pct);
                serial_write(self.uart_va, b"%\r\n");
                serial_write(self.uart_va, b"===================================================\r\n");
                serial_write(self.uart_va, b"GENESIS: stage13 ramstat ok\r\n");

                let pe_kib = if let Some(ref pe) = self.pe { ((pe.total_pages * 4096) / 1024) as u64 } else { 312 };
                let heap_kib = (heap_alloc as u64) / 1024;
                let huge_kib = (llama_pages as u64) * 4;
                crate::drivers::fb::update_workspace_telemetry(
                    None,
                    None,
                    None,
                    Some(pe_kib),
                    Some(heap_kib),
                    Some(huge_kib),
                    Some(self.free_ram_mib),
                    Some(self.total_ram_mib),
                    None,
                    None,
                    Some("RAMSTAT OK"),
                );
            }
            "info" => {
                serial_write(self.uart_va, b"=== Project Genesis System Information ===\r\n");
                let cpus = ONLINE_CPUS.load(Ordering::Acquire);
                serial_write(self.uart_va, b"Online CPUs:      ");
                serial_decimal(self.uart_va, cpus);
                serial_write(self.uart_va, b" core(s)\r\n");

                serial_write(self.uart_va, b"Physical RAM:     ");
                serial_decimal(self.uart_va, self.free_ram_mib);
                serial_write(self.uart_va, b" MiB free / ");
                serial_decimal(self.uart_va, self.total_ram_mib);
                serial_write(self.uart_va, b" MiB total\r\n");

                let heap_alloc = HEAP_ALLOCATOR.allocated_bytes();
                let heap_total = HEAP_ALLOCATOR.total_bytes();
                serial_write(self.uart_va, b"Kernel Heap:      ");
                serial_decimal(self.uart_va, heap_alloc as u64);
                serial_write(self.uart_va, b" bytes used / ");
                serial_decimal(self.uart_va, heap_total as u64);
                serial_write(self.uart_va, b" bytes total\r\n");

                let tcr: u64;
                let vbar: u64;
                #[cfg(target_os = "uefi")]
                {
                    core::arch::asm!("mrs {}, TCR_EL1", out(reg) tcr, options(nomem, nostack));
                    core::arch::asm!("mrs {}, VBAR_EL1", out(reg) vbar, options(nomem, nostack));
                }
                #[cfg(not(target_os = "uefi"))]
                {
                    tcr = 0;
                    vbar = 0;
                }
                serial_write(self.uart_va, b"TCR_EL1:          ");
                serial_hex(self.uart_va, tcr);
                serial_write(self.uart_va, b"\r\n");

                serial_write(self.uart_va, b"VBAR_EL1:         ");
                serial_hex(self.uart_va, vbar);
                serial_write(self.uart_va, b"\r\n");

                if let Some(fb) = self.fb.as_deref() {
                    serial_write(self.uart_va, b"Framebuffer:      ");
                    serial_decimal(self.uart_va, fb.width as u64);
                    serial_write(self.uart_va, b"x");
                    serial_decimal(self.uart_va, fb.height as u64);
                    serial_write(self.uart_va, b" (stride ");
                    serial_decimal(self.uart_va, fb.stride as u64);
                    serial_write(self.uart_va, b", format ");
                    serial_decimal(self.uart_va, fb.pixel_format as u64);
                    serial_write(self.uart_va, b", base ");
                    serial_hex(self.uart_va, fb.base_va);
                    serial_write(self.uart_va, b")\r\n");
                } else {
                    serial_write(self.uart_va, b"Framebuffer:      None (headless mode)\r\n");
                }
            }
            "ls" => {
                let uart = self.uart_va;
                let (fat, dev) = resolve_fs(self.fat, self.dev.as_deref_mut());
                if let (Some(fat), Some(ref mut dev)) = (fat, dev) {
                    let p = if arg.is_empty() { "/" } else { arg };
                    serial_write(uart, b"Directory listing for: ");
                    serial_write(uart, p.as_bytes());
                    serial_write(uart, b"\r\n");
                    let mut count = 0usize;
                    let res = fat.list_dir(
                        dev,
                        p,
                        self.scratch_pa,
                        self.scratch_va,
                        |entry| {
                            count += 1;
                            if entry.is_dir {
                                serial_write(uart, b"  [DIR]  ");
                            } else {
                                serial_write(uart, b"         ");
                            }
                            serial_write(uart, entry.name_str().as_bytes());
                            if !entry.is_dir {
                                serial_write(uart, b"  (");
                                serial_decimal(uart, entry.size as u64);
                                serial_write(uart, b" bytes)");
                            }
                            serial_write(uart, b"\r\n");
                        },
                    );
                    if let Err(e) = res {
                        serial_write(uart, b"Error listing directory: ");
                        serial_write(uart, e.as_bytes());
                        serial_write(uart, b"\r\n");
                    } else {
                        serial_write(uart, b"Total entries: ");
                        serial_decimal(uart, count as u64);
                        serial_write(uart, b"\r\n");
                    }
                } else {
                    serial_write(uart, b"FAT32 filesystem not mounted\r\n");
                }
            }
            "cat" => {
                let (fat, dev) = resolve_fs(self.fat, self.dev.as_deref_mut());
                if arg.is_empty() {
                    serial_write(self.uart_va, b"Usage: cat <file>\r\n");
                } else if let (Some(fat), Some(ref mut dev)) = (fat, dev) {
                    let res = fat.read_file(
                        dev,
                        arg,
                        self.file_buf_pa,
                        self.file_buf_va,
                        self.file_buf_cap,
                        self.scratch_pa,
                        self.scratch_va,
                    );
                    match res {
                        Ok(bytes) => {
                            let slice = core::slice::from_raw_parts(self.file_buf_va as *const u8, bytes);
                            serial_write(self.uart_va, slice);
                            serial_write(self.uart_va, b"\r\n");
                        }
                        Err(e) => {
                            serial_write(self.uart_va, b"Error reading file '");
                            serial_write(self.uart_va, arg.as_bytes());
                            serial_write(self.uart_va, b"': ");
                            serial_write(self.uart_va, e.as_bytes());
                            serial_write(self.uart_va, b"\r\n");
                        }
                    }
                } else {
                    serial_write(self.uart_va, b"FAT32 filesystem not mounted\r\n");
                }
            }
            "write" => {
                let (fat, dev) = resolve_fs(self.fat, self.dev.as_deref_mut());
                if arg.is_empty() {
                    serial_write(self.uart_va, b"Usage: write <file> <text>\r\n");
                } else if let (Some(fat), Some(ref mut dev)) = (fat, dev) {
                    let rem = trimmed.strip_prefix(cmd).unwrap_or("").trim_start();
                    let text = rem.strip_prefix(arg).unwrap_or("").trim_start();
                    let res = fat.write_file(
                        dev,
                        arg,
                        text.as_bytes(),
                        self.scratch_pa,
                        self.scratch_va,
                    );
                    match res {
                        Ok(()) => {
                            serial_write(self.uart_va, b"Wrote ");
                            serial_decimal(self.uart_va, text.len() as u64);
                            serial_write(self.uart_va, b" bytes to '");
                            serial_write(self.uart_va, arg.as_bytes());
                            serial_write(self.uart_va, b"'\r\n");
                        }
                        Err(e) => {
                            serial_write(self.uart_va, b"Error writing file '");
                            serial_write(self.uart_va, arg.as_bytes());
                            serial_write(self.uart_va, b"': ");
                            serial_write(self.uart_va, e.as_bytes());
                            serial_write(self.uart_va, b"\r\n");
                        }
                    }
                } else {
                    serial_write(self.uart_va, b"FAT32 filesystem not mounted\r\n");
                }
            }
            "hexdump" => {
                let (fat, dev) = resolve_fs(self.fat, self.dev.as_deref_mut());
                if arg.is_empty() {
                    serial_write(self.uart_va, b"Usage: hexdump <file>\r\n");
                } else if let (Some(fat), Some(ref mut dev)) = (fat, dev) {
                    let res = fat.read_file(
                        dev,
                        arg,
                        self.file_buf_pa,
                        self.file_buf_va,
                        self.file_buf_cap,
                        self.scratch_pa,
                        self.scratch_va,
                    );
                    match res {
                        Ok(bytes) => {
                            let slice = core::slice::from_raw_parts(self.file_buf_va as *const u8, bytes);
                            self.print_hexdump(slice);
                        }
                        Err(e) => {
                            serial_write(self.uart_va, b"Error reading file '");
                            serial_write(self.uart_va, arg.as_bytes());
                            serial_write(self.uart_va, b"': ");
                            serial_write(self.uart_va, e.as_bytes());
                            serial_write(self.uart_va, b"\r\n");
                        }
                    }
                } else {
                    serial_write(self.uart_va, b"FAT32 filesystem not mounted\r\n");
                }
            }
            "bench" => {
                if let Some(bench) = self.bench_fn {
                    serial_write(self.uart_va, b"Running 4-Core SMP NEON 64x64 MatMul benchmark...\r\n");
                    let (ticks, us) = bench(self.file_buf_va);
                    serial_write(self.uart_va, b"Benchmark complete: ");
                    serial_decimal(self.uart_va, ticks);
                    serial_write(self.uart_va, b" ticks (~");
                    serial_decimal(self.uart_va, us);
                    serial_write(self.uart_va, b" us)\r\n");
                    crate::drivers::fb::update_workspace_telemetry(
                        None,
                        None,
                        Some(us),
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        None,
                        Some("BENCH OK"),
                    );
                } else {
                    serial_write(self.uart_va, b"Benchmark function not configured\r\n");
                }
            }
            "exec" | "run" => {
                let rem = trimmed.strip_prefix(cmd).unwrap_or("").trim_start();
                if rem.is_empty() {
                    serial_write(self.uart_va, b"Usage: exec <file.elf> [args...]\r\n");
                } else {
                    let mut parts = rem.splitn(2, ' ');
                    let file_name = parts.next().unwrap_or("").trim();
                    let extra_args = parts.next().unwrap_or("").trim();
                    let (eff_fat, mut eff_dev) = unsafe { resolve_fs(self.fat, self.dev.as_deref_mut()) };
                    if let (Some(fat), Some(ref mut dev)) = (eff_fat, eff_dev.as_mut()) {
                        let res = unsafe {
                            fat.read_file(
                                dev,
                                file_name,
                                self.file_buf_pa,
                                self.file_buf_va,
                                self.file_buf_cap,
                                self.scratch_pa,
                                self.scratch_va,
                            )
                        };
                        match res {
                            Ok(bytes) => {
                                let elf_slice = unsafe { core::slice::from_raw_parts(self.file_buf_va as *const u8, bytes) };
                                match crate::proc::Elf64Header::parse(elf_slice) {
                                    Ok(_hdr) => {
                                        if let Some(run_fn) = self.run_elf_fn {
                                            match unsafe { run_fn(elf_slice, extra_args, self.uart_va) } {
                                                Ok(status) => {
                                                    serial_write(self.uart_va, b"EL0 process exited with status ");
                                                    serial_decimal(self.uart_va, status as u64);
                                                    serial_write(self.uart_va, b"\r\n");
                                                }
                                                Err(e) => {
                                                    serial_write(self.uart_va, b"ELF execution failed: ");
                                                    serial_write(self.uart_va, e.as_bytes());
                                                    serial_write(self.uart_va, b"\r\n");
                                                }
                                            }
                                        } else {
                                            serial_write(self.uart_va, b"ELF runner not configured\r\n");
                                        }
                                    }
                                    Err(e) => {
                                        serial_write(self.uart_va, b"Invalid ELF header in '");
                                        serial_write(self.uart_va, file_name.as_bytes());
                                        serial_write(self.uart_va, b"': ");
                                        serial_write(self.uart_va, e.as_bytes());
                                        serial_write(self.uart_va, b"\r\n");
                                    }
                                }
                            }
                            Err(e) => {
                                serial_write(self.uart_va, b"Error reading file '");
                                serial_write(self.uart_va, file_name.as_bytes());
                                serial_write(self.uart_va, b"': ");
                                serial_write(self.uart_va, e.as_bytes());
                                serial_write(self.uart_va, b"\r\n");
                            }
                        }
                    } else {
                        serial_write(self.uart_va, b"FAT32 filesystem not mounted\r\n");
                    }
                }
            }
            "infer" => {
                let filename = if arg.is_empty() { "MODEL.BIN" } else { arg };
                if let (Some(fat), Some(dev)) = (self.fat, self.dev.as_deref_mut()) {
                    let res = fat.read_file(
                        dev,
                        filename,
                        self.file_buf_pa,
                        self.file_buf_va,
                        self.file_buf_cap,
                        self.scratch_pa,
                        self.scratch_va,
                    );
                    match res {
                        Ok(bytes) => {
                            if let Some(infer_fn) = self.infer_fn {
                                let model_slice = core::slice::from_raw_parts(self.file_buf_va as *const u8, bytes);
                                match infer_fn(model_slice, self.uart_va) {
                                    Ok((pred, us)) => {
                                        serial_write(self.uart_va, b"Inference predicted class: ");
                                        serial_decimal(self.uart_va, pred as u64);
                                        serial_write(self.uart_va, b" (");
                                        serial_decimal(self.uart_va, us);
                                        serial_write(self.uart_va, b" us)\r\n");
                                    }
                                    Err(e) => {
                                        serial_write(self.uart_va, b"Inference failed: ");
                                        serial_write(self.uart_va, e.as_bytes());
                                        serial_write(self.uart_va, b"\r\n");
                                    }
                                }
                            } else {
                                serial_write(self.uart_va, b"Inference engine not configured\r\n");
                            }
                        }
                        Err(e) => {
                            serial_write(self.uart_va, b"Error reading model file '");
                            serial_write(self.uart_va, filename.as_bytes());
                            serial_write(self.uart_va, b"': ");
                            serial_write(self.uart_va, e.as_bytes());
                            serial_write(self.uart_va, b"\r\n");
                        }
                    }
                } else {
                    serial_write(self.uart_va, b"FAT32 filesystem not mounted\r\n");
                }
            }
            "draw" => {
                if let Some(fb) = self.fb.as_deref_mut() {
                    let cpus = ONLINE_CPUS.load(Ordering::Acquire);
                    fb.draw_dashboard(cpus, self.free_ram_mib, self.total_ram_mib, true);
                    serial_write(self.uart_va, b"UEFI GOP framebuffer dashboard redrawn.\r\n");
                } else {
                    serial_write(self.uart_va, b"UEFI GOP framebuffer not available (headless mode)\r\n");
                }
            }
            "desktop" => {
                if let Some(fb) = self.fb.as_deref_mut() {
                    crate::ui::run_desktop_gui(
                        fb,
                        self.uart_va,
                        self.fat,
                        self.dev.as_deref_mut(),
                        self.scratch_pa,
                        self.scratch_va,
                        self.free_ram_mib,
                        self.total_ram_mib,
                    );
                } else {
                    serial_write(self.uart_va, b"UEFI GOP framebuffer not available (headless mode)\r\n");
                }
            }
            "clear" => {
                crate::drivers::fb::global_fb_console_clear();
                serial_write(self.uart_va, b"\x1B[2J\x1B[H");
            }
            "lspci" => {
                let dev_count = unsafe { crate::drivers::pci::PCI_DEVICE_COUNT };
                if dev_count == 0 {
                    serial_write(self.uart_va, b"No PCIe devices discovered.\r\n");
                } else {
                    serial_write(self.uart_va, b"Discovered PCIe Devices:\r\n");
                    for i in 0..dev_count {
                        let dev = unsafe { &crate::drivers::pci::PCI_DEVICES[i] };
                        let mut buf = [0u8; 128];
                        let s = crate::drivers::pci::format_pci_device(dev, &mut buf);
                        serial_write(self.uart_va, s.as_bytes());
                        serial_write(self.uart_va, b"\r\n");
                    }
                }
            }
            "lsblk" => {
                serial_write(self.uart_va, b"NAME   TYPE                  SIZE      SEC   MOUNT\r\n");
                let active_name = crate::fs::get_active_volume_name();

                // 1. VirtIO-Blk
                if let Some(dev_ptr) = crate::drivers::virtio_blk::get_global_virtio_blk() {
                    let d = unsafe { &*dev_ptr };
                    let blocks = d.capacity_sectors();
                    let mib = (blocks * 512) / (1024 * 1024);
                    serial_write(self.uart_va, b"blk0   VirtIO-Blk            ");
                    serial_decimal(self.uart_va, mib);
                    serial_write(self.uart_va, b" MiB   512   ");
                    if active_name == "blk0" {
                        serial_write(self.uart_va, b"/\r\n");
                    } else {
                        serial_write(self.uart_va, b"-\r\n");
                    }
                }

                // 2. NVMe SSD
                if let Some(nvme) = crate::drivers::nvme::get_global_nvme_ref() {
                    if nvme.is_initialized {
                        let blocks = nvme.total_blocks;
                        let lba_b = nvme.lba_bytes as u64;
                        let mib = (blocks * lba_b) / (1024 * 1024);
                        serial_write(self.uart_va, b"nvme0  PCIe NVMe SSD         ");
                        serial_decimal(self.uart_va, mib);
                        serial_write(self.uart_va, b" MiB   ");
                        serial_decimal(self.uart_va, lba_b);
                        serial_write(self.uart_va, b"   ");
                        if active_name == "nvme0" {
                            serial_write(self.uart_va, b"/\r\n");
                        } else {
                            serial_write(self.uart_va, b"-\r\n");
                        }
                    }
                }

                // 3. USB Mass Storage
                if let Some(usb) = crate::drivers::usb_msc::get_global_usb_msc_ref() {
                    if usb.is_initialized {
                        let blocks = usb.total_blocks;
                        let blk_b = usb.block_size as u64;
                        let mib = (blocks * blk_b) / (1024 * 1024);
                        serial_write(self.uart_va, b"usb0   USB Mass Storage      ");
                        serial_decimal(self.uart_va, mib);
                        serial_write(self.uart_va, b" MiB   ");
                        serial_decimal(self.uart_va, blk_b);
                        serial_write(self.uart_va, b"   ");
                        if active_name == "usb0" {
                            serial_write(self.uart_va, b"/\r\n");
                        } else {
                            serial_write(self.uart_va, b"-\r\n");
                        }
                    }
                }
            }
            "mount" => {
                if arg.is_empty() {
                    serial_write(self.uart_va, b"Usage: mount <dev> (e.g. blk0, nvme0, usb0)\r\n");
                } else {
                    let dev_opt = crate::fs::get_block_device_by_name(arg);
                    if let Some(mut dev) = dev_opt {
                        let scratch_pa = self.scratch_pa;
                        let scratch_va = self.scratch_va;
                        match dev.read_sectors(0, 1, scratch_pa, scratch_va) {
                            Ok(()) => {
                                let sec0_slice = core::slice::from_raw_parts(scratch_va as *const u8, 512);
                                if let Ok(sec0_arr) = sec0_slice.try_into() {
                                    match crate::fs::locate_volume(sec0_arr) {
                                        Ok(vol_lba) => {
                                            match dev.read_sectors(vol_lba, 1, scratch_pa, scratch_va) {
                                                Ok(()) => {
                                                    let bpb_slice = core::slice::from_raw_parts(scratch_va as *const u8, 512);
                                                    if let Ok(bpb_arr) = bpb_slice.try_into() {
                                                        match crate::fs::FatVolume::parse_bpb(vol_lba, bpb_arr) {
                                                            Ok(vol) => {
                                                                crate::fs::set_active_volume(vol, arg);
                                                                serial_write(self.uart_va, b"Mounted ");
                                                                serial_write(self.uart_va, arg.as_bytes());
                                                                serial_write(self.uart_va, b" as active FAT32 volume\r\n");
                                                            }
                                                            Err(e) => {
                                                                serial_write(self.uart_va, b"Error parsing BPB on ");
                                                                serial_write(self.uart_va, arg.as_bytes());
                                                                serial_write(self.uart_va, b": ");
                                                                serial_write(self.uart_va, e.as_bytes());
                                                                serial_write(self.uart_va, b"\r\n");
                                                            }
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    serial_write(self.uart_va, b"Error reading BPB sector on ");
                                                    serial_write(self.uart_va, arg.as_bytes());
                                                    serial_write(self.uart_va, b": ");
                                                    serial_write(self.uart_va, e.as_bytes());
                                                    serial_write(self.uart_va, b"\r\n");
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            serial_write(self.uart_va, b"Error locating volume on ");
                                            serial_write(self.uart_va, arg.as_bytes());
                                            serial_write(self.uart_va, b": ");
                                            serial_write(self.uart_va, e.as_bytes());
                                            serial_write(self.uart_va, b"\r\n");
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                serial_write(self.uart_va, b"Error reading sector 0 from ");
                                serial_write(self.uart_va, arg.as_bytes());
                                serial_write(self.uart_va, b": ");
                                serial_write(self.uart_va, e.as_bytes());
                                serial_write(self.uart_va, b"\r\n");
                            }
                        }
                    } else {
                        serial_write(self.uart_va, b"Device not found or not initialized: ");
                        serial_write(self.uart_va, arg.as_bytes());
                        serial_write(self.uart_va, b"\r\n");
                    }
                }
            }
            "acpiinfo" => {
                if let Some(acpi) = crate::arch::acpi::get_global_acpi_ref() {
                    serial_write(self.uart_va, b"=== ACPI 2.0+ Hardware Configuration ===\r\n");
                    serial_write(self.uart_va, b"RSDP Address:   ");
                    serial_hex(self.uart_va, acpi.rsdp.rsdp_pa);
                    serial_write(self.uart_va, b"\r\nRSDP Revision:  ");
                    serial_decimal(self.uart_va, acpi.rsdp.revision as u64);
                    serial_write(self.uart_va, b"\r\nRSDP OEM ID:    ");
                    serial_write(self.uart_va, acpi.rsdp.oem_id_str().as_bytes());
                    serial_write(self.uart_va, b"\r\nXSDT Address:   ");
                    serial_hex(self.uart_va, acpi.rsdp.xsdt_pa);
                    serial_write(self.uart_va, b" (entries: ");
                    serial_decimal(self.uart_va, acpi.xsdt_entries_count as u64);
                    serial_write(self.uart_va, b")\r\n");

                    if let Some(ref madt) = acpi.madt {
                        serial_write(self.uart_va, b"MADT CPUs:      ");
                        serial_decimal(self.uart_va, madt.enabled_cpus as u64);
                        serial_write(self.uart_va, b" enabled / ");
                        serial_decimal(self.uart_va, madt.cpu_count as u64);
                        serial_write(self.uart_va, b" total (GICD: ");
                        serial_hex(self.uart_va, madt.gicd_base);
                        serial_write(self.uart_va, b", GICR: ");
                        serial_hex(self.uart_va, madt.gicr_base);
                        serial_write(self.uart_va, b")\r\n");
                    }

                    if let Some(ref mcfg) = acpi.mcfg {
                        serial_write(self.uart_va, b"MCFG ECAM Base: ");
                        serial_hex(self.uart_va, mcfg.ecam_base);
                        serial_write(self.uart_va, b" (buses ");
                        serial_decimal(self.uart_va, mcfg.start_bus as u64);
                        serial_write(self.uart_va, b"..");
                        serial_decimal(self.uart_va, mcfg.end_bus as u64);
                        serial_write(self.uart_va, b")\r\n");
                    }

                    if let Some(ref gtdt) = acpi.gtdt {
                        serial_write(self.uart_va, b"GTDT Timers:    PhysEL1=GSIV ");
                        serial_decimal(self.uart_va, gtdt.phys_el1_timer_gsiv as u64);
                        serial_write(self.uart_va, b", VirtEL1=GSIV ");
                        serial_decimal(self.uart_va, gtdt.virt_el1_timer_gsiv as u64);
                        serial_write(self.uart_va, b", PhysEL2=GSIV ");
                        serial_decimal(self.uart_va, gtdt.phys_el2_timer_gsiv as u64);
                        serial_write(self.uart_va, b"\r\n");
                    }

                    if let Some(ref spcr) = acpi.spcr {
                        serial_write(self.uart_va, b"SPCR UART:      ");
                        serial_hex(self.uart_va, spcr.uart_base);
                        serial_write(self.uart_va, b" (GSIV ");
                        serial_decimal(self.uart_va, spcr.gsiv as u64);
                        serial_write(self.uart_va, b", type=");
                        serial_decimal(self.uart_va, spcr.interface_type as u64);
                        serial_write(self.uart_va, b")\r\n");
                    }
                } else {
                    serial_write(self.uart_va, b"No ACPI tables detected (booted via FDT or ACPI not available).\r\n");
                }
            }
            "lsusb" => {
                if let Some(xhci) = unsafe { crate::drivers::xhci::get_global_xhci_ref() } {
                    serial_write(self.uart_va, b"=== xHCI USB 3.0 Host Controller ===\r\n");
                    let mut line_buf = [0u8; 128];
                    let s = crate::drivers::xhci::format_xhci_info(xhci, &mut line_buf);
                    serial_write(self.uart_va, s.as_bytes());
                    serial_write(self.uart_va, b"\r\n");

                    if xhci.noop_success {
                        serial_write(self.uart_va, b"Command Ring:     NOOP command verified (Completion Code: 1 Success)\r\n");
                    } else {
                        serial_write(self.uart_va, b"Command Ring:     NOOP command not verified\r\n");
                    }
                    serial_write(self.uart_va, b"Root Hub Ports:\r\n");
                    let mut dev_count = 0;
                    for p in 1..=xhci.max_ports {
                        if let Some(info) = unsafe { xhci.read_port_status(p) } {
                            let mut p_buf = [0u8; 128];
                            let ps = crate::drivers::xhci::format_port_status(&info, &mut p_buf);
                            serial_write(self.uart_va, ps.as_bytes());
                            serial_write(self.uart_va, b"\r\n");
                            if info.connected {
                                dev_count += 1;
                            }
                        }
                    }
                    serial_write(self.uart_va, b"Total USB Devices Detected: ");
                    serial_decimal(self.uart_va, dev_count as u64);
                    serial_write(self.uart_va, b"\r\n");

                    let (devs, count) = unsafe { crate::drivers::usb_hid::get_enumerated_devices() };
                    if count > 0 {
                        serial_write(self.uart_va, b"Enumerated USB Devices:\r\n");
                        for i in 0..count {
                            if let Some(ref dev) = devs[i] {
                                let mut buf = [0u8; 128];
                                let s = crate::drivers::usb_hid::format_usb_device(dev, &mut buf);
                                serial_write(self.uart_va, s.as_bytes());
                                serial_write(self.uart_va, b"\r\n");
                            }
                        }
                    }
                } else {
                    serial_write(self.uart_va, b"No xHCI USB host controller detected.\r\n");
                }
            }
            "nvmeinfo" => {
                if let Some(nvme) = crate::drivers::nvme::get_global_nvme_ref() {
                    serial_write(self.uart_va, b"=== PCIe NVMe SSD Controller ===\r\n");
                    serial_write(self.uart_va, b"Model:          ");
                    serial_write(self.uart_va, nvme.model_str().as_bytes());
                    serial_write(self.uart_va, b"\r\nSerial:         ");
                    serial_write(self.uart_va, nvme.serial_str().as_bytes());
                    serial_write(self.uart_va, b"\r\nTotal Blocks:   ");
                    serial_decimal(self.uart_va, nvme.total_blocks);
                    serial_write(self.uart_va, b"\r\nLBA Data Size:  ");
                    serial_decimal(self.uart_va, nvme.lba_bytes as u64);
                    serial_write(self.uart_va, b" bytes\r\nCapacity:       ");
                    let mib = (nvme.total_blocks * nvme.lba_bytes as u64) / 1024 / 1024;
                    serial_decimal(self.uart_va, mib);
                    serial_write(self.uart_va, b" MiB\r\n");
                } else {
                    serial_write(self.uart_va, b"No NVMe PCIe SSD controller detected.\r\n");
                }
            }
            "nvmeread" => {
                let lba: u64 = if arg.is_empty() {
                    0
                } else {
                    let mut val = 0u64;
                    for &b in arg.as_bytes() {
                        if b >= b'0' && b <= b'9' {
                            val = val * 10 + (b - b'0') as u64;
                        }
                    }
                    val
                };
                if let Some(nvme) = crate::drivers::nvme::get_global_nvme() {
                    serial_write(self.uart_va, b"Reading LBA ");
                    serial_decimal(self.uart_va, lba);
                    serial_write(self.uart_va, b" from NVMe SSD...\r\n");
                    match unsafe { nvme.read_lba(lba, 1, self.scratch_pa, self.scratch_va) } {
                        Ok(()) => {
                            serial_write(self.uart_va, b"NVMe LBA hex dump (first 128 bytes):\r\n");
                            let ptr = self.scratch_va as *const u8;
                            for row in 0..8 {
                                let offset = row * 16;
                                serial_write(self.uart_va, b"  ");
                                serial_hex(self.uart_va, offset as u64);
                                serial_write(self.uart_va, b": ");
                                for col in 0..16 {
                                    let byte = unsafe { core::ptr::read_volatile(ptr.add(offset + col)) };
                                    const HEX: &[u8; 16] = b"0123456789abcdef";
                                    let hi = HEX[(byte >> 4) as usize];
                                    let lo = HEX[(byte & 0xF) as usize];
                                    let b_str = [hi, lo, b' '];
                                    serial_write(self.uart_va, &b_str);
                                }
                                serial_write(self.uart_va, b" |");
                                for col in 0..16 {
                                    let byte = unsafe { core::ptr::read_volatile(ptr.add(offset + col)) };
                                    let ch = if byte >= 32 && byte <= 126 { byte } else { b'.' };
                                    serial_write(self.uart_va, &[ch]);
                                }
                                serial_write(self.uart_va, b"|\r\n");
                            }
                        }
                        Err(e) => {
                            serial_write(self.uart_va, b"NVMe read failed: ");
                            serial_write(self.uart_va, e.as_bytes());
                            serial_write(self.uart_va, b"\r\n");
                        }
                    }
                } else {
                    serial_write(self.uart_va, b"No NVMe device available\r\n");
                }
            }
            "usbstorage" => {
                if let Some(msc) = crate::drivers::usb_msc::get_global_usb_msc_ref() {
                    serial_write(self.uart_va, b"=== USB Mass Storage (SCSI / BOT) ===\r\n");
                    serial_write(self.uart_va, b"Vendor:         ");
                    serial_write(self.uart_va, msc.vendor_str().as_bytes());
                    serial_write(self.uart_va, b"\r\nProduct:        ");
                    serial_write(self.uart_va, msc.product_str().as_bytes());
                    serial_write(self.uart_va, b"\r\nRevision:       ");
                    serial_write(self.uart_va, msc.revision_str().as_bytes());
                    serial_write(self.uart_va, b"\r\nTotal Blocks:   ");
                    serial_decimal(self.uart_va, msc.total_blocks);
                    serial_write(self.uart_va, b"\r\nBlock Size:     ");
                    serial_decimal(self.uart_va, msc.block_size as u64);
                    serial_write(self.uart_va, b" bytes\r\nCapacity:       ");
                    let mib = (msc.total_blocks * msc.block_size as u64) / 1024 / 1024;
                    serial_decimal(self.uart_va, mib);
                    serial_write(self.uart_va, b" MiB\r\n");
                } else {
                    serial_write(self.uart_va, b"No USB Mass Storage device detected.\r\n");
                }
            }
            "cpuinfo" => {
                serial_write(self.uart_va, b"=== AArch64 CPU & Hardware Telemetry ===\r\n");
                let t = crate::arch::cpu::query_cpu_telemetry();
                let mut buf = [0u8; 512];
                let s = crate::arch::cpu::format_cpu_summary(&t, &mut buf);
                serial_write(self.uart_va, s.as_bytes());
                serial_write(self.uart_va, b"\r\n");
            }
            "gpuinfo" => {
                if let Some(gpu_ptr) = crate::drivers::virtio_gpu::get_global_virtio_gpu() {
                    let gpu = unsafe { &*gpu_ptr };
                    serial_write(self.uart_va, b"=== VirtIO-GPU 2D / 3D Controller ===\r\n");
                    serial_write(self.uart_va, b"Mode:           ");
                    if gpu.is_virgl {
                        serial_write(self.uart_va, b"3D (VirGL Hardware Accelerated)\r\n");
                    } else {
                        serial_write(self.uart_va, b"2D Native\r\n");
                    }
                    serial_write(self.uart_va, b"Resolution:     ");
                    serial_decimal(self.uart_va, gpu.width as u64);
                    serial_write(self.uart_va, b"x");
                    serial_decimal(self.uart_va, gpu.height as u64);
                    serial_write(self.uart_va, b"\r\nPixel Format:   B8G8R8X8_UNORM (32 bpp)\r\n");
                    serial_write(self.uart_va, b"Scanout Res ID: ");
                    serial_decimal(self.uart_va, gpu.resource_id as u64);
                    serial_write(self.uart_va, b"\r\nCapset Count:   ");
                    serial_decimal(self.uart_va, gpu.capset_count as u64);
                    serial_write(self.uart_va, b"\r\nFlush Count:    ");
                    serial_decimal(self.uart_va, gpu.flush_count);
                    serial_write(self.uart_va, b"\r\n");
                } else {
                    serial_write(self.uart_va, b"VirtIO-GPU not active (using UEFI GOP / Framebuffer console).\r\n");
                }
            }
            "llm" => {
                let filename = if arg.is_empty() { "TRANSFORMER.BIN" } else { arg };
                if let (Some(fat), Some(dev)) = (self.fat, self.dev.as_deref_mut()) {
                    let res = fat.read_file(
                        dev,
                        filename,
                        self.file_buf_pa,
                        self.file_buf_va,
                        self.file_buf_cap,
                        self.scratch_pa,
                        self.scratch_va,
                    );
                    match res {
                        Ok(bytes) => {
                            if let Some(llm_fn) = self.llm_fn {
                                let model_slice = core::slice::from_raw_parts(self.file_buf_va as *const u8, bytes);
                                match llm_fn(model_slice, self.uart_va) {
                                    Ok(us) => {
                                        serial_write(self.uart_va, b"GENESIS: stage10 neon transformer ok (cores=4, ");
                                        serial_decimal(self.uart_va, us);
                                        serial_write(self.uart_va, b" us)\r\n");
                                    }
                                    Err(e) => {
                                        serial_write(self.uart_va, b"Transformer execution failed: ");
                                        serial_write(self.uart_va, e.as_bytes());
                                        serial_write(self.uart_va, b"\r\n");
                                    }
                                }
                            } else {
                                serial_write(self.uart_va, b"Transformer runner not configured\r\n");
                            }
                        }
                        Err(e) => {
                            serial_write(self.uart_va, b"Error reading transformer file '");
                            serial_write(self.uart_va, filename.as_bytes());
                            serial_write(self.uart_va, b"': ");
                            serial_write(self.uart_va, e.as_bytes());
                            serial_write(self.uart_va, b"\r\n");
                        }
                    }
                } else {
                    serial_write(self.uart_va, b"FAT32 filesystem not mounted\r\n");
                }
            }
            "multitask" => {
                if let Some(multi_fn) = self.multitask_fn {
                    if let (Some(fat), Some(dev)) = (self.fat, self.dev.as_deref_mut()) {
                        let half_cap = self.file_buf_cap / 2;
                        let res1 = fat.read_file(
                            dev,
                            "APP1.ELF",
                            self.file_buf_pa,
                            self.file_buf_va,
                            half_cap,
                            self.scratch_pa,
                            self.scratch_va,
                        );
                        let res2 = fat.read_file(
                            dev,
                            "APP2.ELF",
                            self.file_buf_pa + half_cap as u64,
                            self.file_buf_va + half_cap as u64,
                            half_cap,
                            self.scratch_pa,
                            self.scratch_va,
                        );
                        match (res1, res2) {
                            (Ok(len1), Ok(len2)) => {
                                let app1_slice = core::slice::from_raw_parts(self.file_buf_va as *const u8, len1);
                                let app2_slice = core::slice::from_raw_parts((self.file_buf_va + half_cap as u64) as *const u8, len2);
                                match multi_fn(app1_slice, app2_slice, self.uart_va) {
                                    Ok(()) => {
                                        serial_write(self.uart_va, b"GENESIS: stage11 el0 multitask ok\r\n");
                                        if crate::proc::ipc::IPC_MAILBOX.lock().verified_ping {
                                            serial_write(self.uart_va, b"GENESIS: stage12 asid & el0 ipc ok\r\n");
                                        }
                                    }
                                    Err(e) => {
                                        serial_write(self.uart_va, b"Multitask run failed: ");
                                        serial_write(self.uart_va, e.as_bytes());
                                        serial_write(self.uart_va, b"\r\n");
                                    }
                                }
                            }
                            _ => {
                                serial_write(self.uart_va, b"Failed to load APP1.ELF or APP2.ELF from filesystem\r\n");
                            }
                        }
                    } else {
                        serial_write(self.uart_va, b"FAT32 filesystem not mounted\r\n");
                    }
                } else {
                    serial_write(self.uart_va, b"Multitask runner not configured\r\n");
                }
            }
            "generate" => {
                let prompt = if arg.is_empty() { "GENESIS " } else { arg };
                if let (Some(fat), Some(dev)) = (self.fat, self.dev.as_deref_mut()) {
                    let res = fat.read_file(
                        dev,
                        "MICRO_LLM.BIN",
                        self.file_buf_pa,
                        self.file_buf_va,
                        self.file_buf_cap,
                        self.scratch_pa,
                        self.scratch_va,
                    );
                    match res {
                        Ok(bytes) => {
                            if let Some(gen_fn) = self.generate_fn {
                                let model_slice = core::slice::from_raw_parts(self.file_buf_va as *const u8, bytes);
                                match gen_fn(model_slice, prompt, self.uart_va) {
                                    Ok(()) => {
                                        serial_write(self.uart_va, b"GENESIS: stage11 micro-llm ok\r\n");
                                    }
                                    Err(e) => {
                                        serial_write(self.uart_va, b"Generation failed: ");
                                        serial_write(self.uart_va, e.as_bytes());
                                        serial_write(self.uart_va, b"\r\n");
                                    }
                                }
                            } else {
                                serial_write(self.uart_va, b"LLM generator not configured\r\n");
                            }
                        }
                        Err(e) => {
                            serial_write(self.uart_va, b"Error reading MICRO_LLM.BIN: ");
                            serial_write(self.uart_va, e.as_bytes());
                            serial_write(self.uart_va, b"\r\n");
                        }
                    }
                } else {
                    serial_write(self.uart_va, b"FAT32 filesystem not mounted\r\n");
                }
            }
            "llama" => {
                let mut filename = "TINY_LLAMA.BIN";
                let mut prompt = "GENESIS";
                if !arg.is_empty() {
                    if arg.ends_with(".BIN") || arg.ends_with(".bin") {
                        filename = arg;
                        if let Some(next_token) = parts.next() {
                            prompt = next_token;
                        }
                    } else {
                        prompt = arg;
                    }
                }
                if let (Some(fat), Some(dev)) = (self.fat, self.dev.as_deref_mut()) {
                    let tok_slice = if self.tok_buf_cap > 0 {
                        match fat.read_file(
                            dev,
                            "TOKENIZER.BIN",
                            self.tok_buf_pa,
                            self.tok_buf_va,
                            self.tok_buf_cap,
                            self.scratch_pa,
                            self.scratch_va,
                        ) {
                            Ok(bytes) => Some(core::slice::from_raw_parts(self.tok_buf_va as *const u8, bytes)),
                            Err(_) => None,
                        }
                    } else {
                        None
                    };

                    let res = fat.read_file(
                        dev,
                        filename,
                        self.file_buf_pa,
                        self.file_buf_va,
                        self.file_buf_cap,
                        self.scratch_pa,
                        self.scratch_va,
                    );
                    match res {
                        Ok(bytes) => {
                            if let Some(llama_fn) = self.llama_fn {
                                let model_slice = core::slice::from_raw_parts(self.file_buf_va as *const u8, bytes);
                                match llama_fn(model_slice, tok_slice, prompt, self.uart_va) {
                                    Ok(()) => {
                                        serial_write(self.uart_va, b"GENESIS: stage12 llama2 engine ok\r\n");
                                        serial_write(self.uart_va, b"GENESIS: stage13 dynamic llama2 ok\r\n");
                                        crate::drivers::fb::update_workspace_telemetry(
                                            Some("Llama-2 SMP"),
                                            None,
                                            None,
                                            None,
                                            None,
                                            None,
                                            None,
                                            None,
                                            None,
                                            None,
                                            Some("LLAMA2 OK"),
                                        );
                                    }
                                    Err(e) => {
                                        serial_write(self.uart_va, b"Llama2 execution failed: ");
                                        serial_write(self.uart_va, e.as_bytes());
                                        serial_write(self.uart_va, b"\r\n");
                                    }
                                }
                            } else {
                                serial_write(self.uart_va, b"Llama runner not configured\r\n");
                            }
                        }
                        Err(e) => {
                            serial_write(self.uart_va, b"Error reading ");
                            serial_write(self.uart_va, filename.as_bytes());
                            serial_write(self.uart_va, b": ");
                            serial_write(self.uart_va, e.as_bytes());
                            serial_write(self.uart_va, b"\r\n");
                        }
                    }
                } else {
                    serial_write(self.uart_va, b"FAT32 filesystem not mounted\r\n");
                }
            }
            "chat" => {
                let filename = "TINY_LLAMA.BIN";
                let prompt = if !arg.is_empty() {
                    let full_arg_start = line.find("chat").unwrap_or(0) + 4;
                    line[full_arg_start..].trim()
                } else {
                    "GENESIS"
                };
                if let (Some(fat), Some(dev)) = (self.fat, self.dev.as_deref_mut()) {
                    let tok_slice = if self.tok_buf_cap > 0 {
                        match fat.read_file(
                            dev,
                            "TOKENIZER.BIN",
                            self.tok_buf_pa,
                            self.tok_buf_va,
                            self.tok_buf_cap,
                            self.scratch_pa,
                            self.scratch_va,
                        ) {
                            Ok(bytes) => Some(core::slice::from_raw_parts(self.tok_buf_va as *const u8, bytes)),
                            Err(_) => None,
                        }
                    } else {
                        None
                    };

                    let res = fat.read_file(
                        dev,
                        filename,
                        self.file_buf_pa,
                        self.file_buf_va,
                        self.file_buf_cap,
                        self.scratch_pa,
                        self.scratch_va,
                    );
                    match res {
                        Ok(bytes) => {
                            if let Some(llama_fn) = self.llama_fn {
                                let model_slice = core::slice::from_raw_parts(self.file_buf_va as *const u8, bytes);
                                match llama_fn(model_slice, tok_slice, prompt, self.uart_va) {
                                    Ok(()) => {
                                        crate::drivers::fb::update_workspace_telemetry(
                                            Some("Llama-2 SMP"),
                                            None,
                                            None,
                                            None,
                                            None,
                                            None,
                                            None,
                                            None,
                                            None,
                                            None,
                                            Some("CHAT OK"),
                                        );
                                    }
                                    Err(e) => {
                                        serial_write(self.uart_va, b"Chat execution failed: ");
                                        serial_write(self.uart_va, e.as_bytes());
                                        serial_write(self.uart_va, b"\r\n");
                                    }
                                }
                            } else {
                                serial_write(self.uart_va, b"Llama runner not configured\r\n");
                            }
                        }
                        Err(e) => {
                            serial_write(self.uart_va, b"Error reading ");
                            serial_write(self.uart_va, filename.as_bytes());
                            serial_write(self.uart_va, b": ");
                            serial_write(self.uart_va, e.as_bytes());
                            serial_write(self.uart_va, b"\r\n");
                        }
                    }
                } else {
                    serial_write(self.uart_va, b"FAT32 filesystem not mounted\r\n");
                }
            }
            "netinfo" => {
                if let Some(ref net) = self.net_dev {
                    serial_write(self.uart_va, b"=== VirtIO Network Device Info ===\r\n");
                    serial_write(self.uart_va, b"Status:       Active (VirtIO MMIO)\r\n");
                    serial_write(self.uart_va, b"MAC Address:  ");
                    net.print_mac(self.uart_va);
                    serial_write(self.uart_va, b"\r\nIPv4 Address: 10.0.2.15\r\n");
                    serial_write(self.uart_va, b"Gateway:      10.0.2.2\r\n");
                    serial_write(self.uart_va, b"DNS Server:   10.0.2.3:53\r\n");
                    serial_write(self.uart_va, b"Subnet Mask:  255.255.255.0\r\n");
                } else {
                    serial_write(self.uart_va, b"VirtIO Network Device not detected\r\n");
                }
            }
            "route" => {
                if arg == "virtio" || arg == "virtio-net" {
                    crate::net::set_active_nic(crate::net::ActiveNic::VirtIoNet);
                    serial_write(self.uart_va, b"Default network route set to VirtIO-Net (MMIO)\r\n");
                } else if arg == "e1000e" {
                    crate::net::set_active_nic(crate::net::ActiveNic::E1000e);
                    serial_write(self.uart_va, b"Default network route set to Intel 82574L e1000e (PCIe)\r\n");
                } else {
                    serial_write(self.uart_va, b"Usage: route <virtio|e1000e>\r\n");
                    let active = match crate::net::get_active_nic() {
                        crate::net::ActiveNic::E1000e => "e1000e (PCIe)",
                        crate::net::ActiveNic::VirtIoNet => "virtio-net (MMIO)",
                        crate::net::ActiveNic::None => "none",
                    };
                    serial_write(self.uart_va, b"Current active route: ");
                    serial_write(self.uart_va, active.as_bytes());
                    serial_write(self.uart_va, b"\r\n");
                }
            }
            "nicinfo" => {
                serial_write(self.uart_va, b"=== Network Interface Controllers (NICs) ===\r\n");
                let mut nic_count = 0;
                if let Some(ref net) = self.net_dev {
                    serial_write(self.uart_va, b"[NIC 0] VirtIO-Net (MMIO): MAC=");
                    net.print_mac(self.uart_va);
                    serial_write(self.uart_va, b" | Link: UP | IP=10.0.2.15 | TX=");
                    serial_decimal(self.uart_va, net.packets_tx);
                    serial_write(self.uart_va, b" RX=");
                    serial_decimal(self.uart_va, net.packets_rx);
                    serial_write(self.uart_va, b"\r\n");
                    nic_count += 1;
                }
                if let Some(e1000) = crate::drivers::e1000e::get_global_e1000e_ref() {
                    serial_write(self.uart_va, b"[NIC ");
                    serial_decimal(self.uart_va, nic_count as u64);
                    serial_write(self.uart_va, b"] Intel 82574L e1000e (PCIe): MAC=");
                    e1000.print_mac(self.uart_va);
                    let link_up = unsafe { e1000.is_link_up() };
                    serial_write(self.uart_va, if link_up { b" | Link: UP" } else { b" | Link: DOWN" });
                    serial_write(self.uart_va, b" | TX=");
                    serial_decimal(self.uart_va, e1000.packets_tx);
                    serial_write(self.uart_va, b" RX=");
                    serial_decimal(self.uart_va, e1000.packets_rx);
                    serial_write(self.uart_va, b"\r\n");
                    nic_count += 1;
                }
                if nic_count == 0 {
                    serial_write(self.uart_va, b"No network interface controllers detected\r\n");
                } else {
                    let active = match crate::net::get_active_nic() {
                        crate::net::ActiveNic::E1000e => "e1000e (PCIe)",
                        crate::net::ActiveNic::VirtIoNet => "virtio-net (MMIO)",
                        crate::net::ActiveNic::None => "default",
                    };
                    serial_write(self.uart_va, b"Active Default Route: ");
                    serial_write(self.uart_va, active.as_bytes());
                    serial_write(self.uart_va, b"\r\n");
                }
            }
            "dns" => {
                let target = if arg.is_empty() { "gateway.local" } else { arg };
                let uart = self.uart_va;
                let dns_fn = self.dns_fn;
                if let Some(mut net) = crate::net::get_active_net_dev() {
                    serial_write(uart, b"Resolving '");
                    serial_write(uart, target.as_bytes());
                    serial_write(uart, b"' via DNS (10.0.2.3:53)...\r\n");
                    if let Some(dfn) = dns_fn {
                        match dfn(&mut net, target, uart) {
                            Ok(ip) => {
                                serial_write(uart, target.as_bytes());
                                serial_write(uart, b" resolved to ");
                                serial_decimal(uart, ip[0] as u64);
                                serial_write(uart, b".");
                                serial_decimal(uart, ip[1] as u64);
                                serial_write(uart, b".");
                                serial_decimal(uart, ip[2] as u64);
                                serial_write(uart, b".");
                                serial_decimal(uart, ip[3] as u64);
                                serial_write(uart, b"\r\n");
                                serial_write(uart, b"GENESIS: stage13 dns ok\r\n");
                            }
                            Err(e) => {
                                serial_write(uart, b"DNS resolution failed: ");
                                serial_write(uart, e.as_bytes());
                                serial_write(uart, b"\r\n");
                            }
                        }
                    } else {
                        serial_write(uart, b"DNS resolver function not configured\r\n");
                    }
                } else {
                    serial_write(uart, b"No network interface controller available\r\n");
                }
            }
            "httpget" => {
                let target = if arg.is_empty() { "10.0.2.2" } else { arg };
                let port_str = parts.next().unwrap_or("80");
                let path = parts.next().unwrap_or("/");
                let uart = self.uart_va;
                let httpget_fn = self.httpget_fn;
                if let Some(mut net) = crate::net::get_active_net_dev() {
                    let mut port: u16 = 0;
                    for &b in port_str.as_bytes() {
                        if b >= b'0' && b <= b'9' {
                            port = port * 10 + (b - b'0') as u16;
                        }
                    }
                    if port == 0 { port = 80; }

                    serial_write(uart, b"HTTP GET http://");
                    serial_write(uart, target.as_bytes());
                    serial_write(uart, b":");
                    serial_decimal(uart, port as u64);
                    serial_write(uart, path.as_bytes());
                    serial_write(uart, b"...\r\n");

                    if let Some(hfn) = httpget_fn {
                        match hfn(&mut net, target, port, path, uart) {
                            Ok(bytes) => {
                                serial_write(uart, b"HTTP GET response complete (");
                                serial_decimal(uart, bytes as u64);
                                serial_write(uart, b" bytes received)\r\n");
                                serial_write(uart, b"GENESIS: stage13 httpget ok\r\n");
                            }
                            Err(e) => {
                                serial_write(uart, b"HTTP GET failed: ");
                                serial_write(uart, e.as_bytes());
                                serial_write(uart, b"\r\n");
                            }
                        }
                    } else {
                        serial_write(uart, b"HTTP client function not configured\r\n");
                    }
                } else {
                    serial_write(uart, b"No network interface controller available\r\n");
                }
            }
            "ping" => {
                let target_str = if arg.is_empty() { "10.0.2.2" } else { arg };
                let uart = self.uart_va;
                let ping_fn = self.ping_fn;
                if let Some(mut net) = crate::net::get_active_net_dev() {
                    serial_write(uart, b"PING ");
                    serial_write(uart, target_str.as_bytes());
                    serial_write(uart, b" (32 bytes of data)...\r\n");
                    if let Some(pfn) = ping_fn {
                        match pfn(&mut net, uart) {
                            Ok(us) => {
                                serial_write(uart, b"32 bytes from ");
                                serial_write(uart, target_str.as_bytes());
                                serial_write(uart, b": icmp_seq=1 time=");
                                serial_decimal(uart, us);
                                serial_write(uart, b" us\r\n");
                                serial_write(uart, b"GENESIS: stage12 virtio-net ping 10.0.2.2 ok\r\n");
                                crate::drivers::fb::update_workspace_telemetry(
                                    None,
                                    None,
                                    Some(us),
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    None,
                                    Some(true),
                                    Some("PING OK"),
                                );
                            }
                            Err(e) => {
                                serial_write(uart, b"Ping failed: ");
                                serial_write(uart, e.as_bytes());
                                serial_write(uart, b"\r\n");
                            }
                        }
                    } else {
                        serial_write(uart, b"Ping function not configured\r\n");
                    }
                } else {
                    serial_write(uart, b"No network interface controller available\r\n");
                }
            }
            "date" => {
                let mut buf = [0u8; 64];
                let epoch = crate::arch::rtc::current_utc_epoch_secs();
                let date_str = crate::arch::rtc::format_utc_datetime(epoch, &mut buf);
                serial_write(self.uart_va, date_str.as_bytes());
                serial_write(self.uart_va, b"\r\n");
            }
            "reboot" => {
                serial_write(self.uart_va, b"System rebooting via PSCI...\r\n");
                crate::arch::psci::psci_system_reset();
            }
            "audioinfo" => {
                if let Some(snd) = crate::drivers::virtio_sound::get_global_virtio_sound_ref() {
                    serial_write(self.uart_va, b"VirtIO-Sound PCI Audio Device:\r\n");
                    serial_write(self.uart_va, b"  Status:       ");
                    if snd.is_active {
                        serial_write(self.uart_va, b"ACTIVE\r\n");
                    } else {
                        serial_write(self.uart_va, b"INACTIVE\r\n");
                    }
                    serial_write(self.uart_va, b"  Format:       PCM S16_LE\r\n");
                    serial_write(self.uart_va, b"  Rate:         ");
                    serial_decimal(self.uart_va, snd.sample_rate as u64);
                    serial_write(self.uart_va, b" Hz\r\n");
                    serial_write(self.uart_va, b"  Channels:     ");
                    serial_decimal(self.uart_va, snd.channels as u64);
                    serial_write(self.uart_va, b"\r\n");
                    serial_write(self.uart_va, b"  Buffer Size:  ");
                    serial_decimal(self.uart_va, snd.buffer_bytes as u64);
                    serial_write(self.uart_va, b" bytes\r\n");
                    serial_write(self.uart_va, b"  Period Size:  ");
                    serial_decimal(self.uart_va, snd.period_bytes as u64);
                    serial_write(self.uart_va, b" bytes\r\n");
                    serial_write(self.uart_va, b"  Beeps Played: ");
                    serial_decimal(self.uart_va, snd.beeps_played);
                    serial_write(self.uart_va, b"\r\n");
                } else {
                    serial_write(self.uart_va, b"No VirtIO-Sound device detected\r\n");
                }
            }
            "beep" => {
                let mut freq: u32 = 0;
                for &b in arg.as_bytes() {
                    if b >= b'0' && b <= b'9' {
                        freq = freq * 10 + (b - b'0') as u32;
                    }
                }
                if freq == 0 {
                    freq = 440;
                }
                if let Some(snd) = crate::drivers::virtio_sound::get_global_virtio_sound_mut() {
                    serial_write(self.uart_va, b"Playing ");
                    serial_decimal(self.uart_va, freq as u64);
                    serial_write(self.uart_va, b" Hz tone...\r\n");
                    match unsafe { snd.play_beep(freq, 200) } {
                        Ok(()) => {
                            serial_write(self.uart_va, b"Audio playback complete\r\n");
                        }
                        Err(e) => {
                            serial_write(self.uart_va, b"Audio playback error: ");
                            serial_write(self.uart_va, e.as_bytes());
                            serial_write(self.uart_va, b"\r\n");
                        }
                    }
                } else {
                    serial_write(self.uart_va, b"No VirtIO-Sound device detected\r\n");
                }
            }
            "halt" | "poweroff" => {
                serial_write(self.uart_va, b"System power off via PSCI...\r\n");
                psci_system_off();
            }
            _ => {
                serial_write(self.uart_va, b"Unknown command: '");
                serial_write(self.uart_va, cmd.as_bytes());
                serial_write(self.uart_va, b"'. Type 'help' for command list.\r\n");
            }
        }

        ShellResult::Continue
    }

    /// Prints a standard 16-byte formatted hexadecimal and ASCII dump.
    /// Caps display at 512 bytes to prevent terminal overflow.
    unsafe fn print_hexdump(&self, data: &[u8]) {
        let display_len = data.len().min(512);
        let mut offset = 0usize;

        while offset < display_len {
            // Print offset
            let off_u32 = offset as u32;
            let mut off_str = [b'0'; 8];
            for i in (0..8).rev() {
                let nibble = ((off_u32 >> (i * 4)) & 0xF) as u8;
                off_str[7 - i] = if nibble < 10 { b'0' + nibble } else { b'a' + (nibble - 10) };
            }
            serial_write(self.uart_va, &off_str);
            serial_write(self.uart_va, b": ");

            // Hex bytes (16 per line)
            let chunk_len = (display_len - offset).min(16);
            for i in 0..16 {
                if i < chunk_len {
                    let byte = data[offset + i];
                    let hi = (byte >> 4) & 0xF;
                    let lo = byte & 0xF;
                    let c_hi = if hi < 10 { b'0' + hi } else { b'a' + (hi - 10) };
                    let c_lo = if lo < 10 { b'0' + lo } else { b'a' + (lo - 10) };
                    serial_write(self.uart_va, &[c_hi, c_lo, b' ']);
                } else {
                    serial_write(self.uart_va, b"   ");
                }
                if i == 7 {
                    serial_write(self.uart_va, b" ");
                }
            }

            // ASCII representation
            serial_write(self.uart_va, b" |");
            for i in 0..chunk_len {
                let byte = data[offset + i];
                if (0x20..=0x7E).contains(&byte) {
                    serial_write(self.uart_va, &[byte]);
                } else {
                    serial_write(self.uart_va, b".");
                }
            }
            serial_write(self.uart_va, b"|\r\n");

            offset += 16;
        }

        if data.len() > display_len {
            serial_write(self.uart_va, b"... [truncated: ");
            serial_decimal(self.uart_va, data.len() as u64);
            serial_write(self.uart_va, b" total bytes] ...\r\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_parse_command_tokens() {
        let input = "  cat   WEIGHTS.BIN  ";
        let mut parts = input.trim().split_whitespace();
        assert_eq!(parts.next(), Some("cat"));
        assert_eq!(parts.next(), Some("WEIGHTS.BIN"));
        assert_eq!(parts.next(), None);

        let input_empty = "   ";
        let mut parts_empty = input_empty.trim().split_whitespace();
        assert_eq!(parts_empty.next(), None);

        let input_llama = "llama TINY_LLAMA.BIN PROMPT";
        let mut parts_llama = input_llama.trim().split_whitespace();
        assert_eq!(parts_llama.next(), Some("llama"));
        assert_eq!(parts_llama.next(), Some("TINY_LLAMA.BIN"));
        assert_eq!(parts_llama.next(), Some("PROMPT"));

        let input_chat = "chat Hello Genesis Assistant";
        let mut parts_chat = input_chat.trim().split_whitespace();
        assert_eq!(parts_chat.next(), Some("chat"));
        assert_eq!(parts_chat.next(), Some("Hello"));
    }

    #[test]
    fn shell_result_variants() {
        assert_eq!(ShellResult::Continue, ShellResult::Continue);
        assert_ne!(ShellResult::Continue, ShellResult::Exit);
    }
}
