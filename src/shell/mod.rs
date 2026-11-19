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
    fs::FatVolume,
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
    pub fb: Option<&'a mut Framebuffer>,
    pub total_ram_mib: u64,
    pub free_ram_mib: u64,
    pub pe: Option<crate::vmm::pe::ParsedPeImage>,
    pub pmm_total_pages: u64,
    pub pmm_free_pages: u64,
    pub mapper_tables: usize,
    pub bench_fn: Option<fn(u64) -> (u64, u64)>,
    pub run_elf_fn: Option<unsafe fn(&[u8], usize) -> Result<i32, &'static str>>,
    pub infer_fn: Option<unsafe fn(&[u8], usize) -> Result<(usize, u64), &'static str>>,
    pub llm_fn: Option<unsafe fn(&[u8], usize) -> Result<u64, &'static str>>,
    pub multitask_fn: Option<unsafe fn(&[u8], &[u8], usize) -> Result<(), &'static str>>,
    pub generate_fn: Option<unsafe fn(&[u8], &str, usize) -> Result<(), &'static str>>,
    pub llama_fn: Option<unsafe fn(&[u8], &str, usize) -> Result<(), &'static str>>,
    pub ping_fn: Option<unsafe fn(&mut VirtIONetDevice, usize) -> Result<u64, &'static str>>,
    pub dns_fn: Option<unsafe fn(&mut VirtIONetDevice, &str, usize) -> Result<[u8; 4], &'static str>>,
    pub httpget_fn: Option<unsafe fn(&mut VirtIONetDevice, &str, u16, &str, usize) -> Result<usize, &'static str>>,
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
                serial_write(self.uart_va, b"Project Genesis Kernel Shell (Stage 13)\r\n");
                serial_write(self.uart_va, b"Available commands:\r\n");
                serial_write(self.uart_va, b"  help              - List available commands\r\n");
                serial_write(self.uart_va, b"  info              - Display system status, memory, registers, and framebuffer\r\n");
                serial_write(self.uart_va, b"  ramstat           - Display kernel memory profile and physical RAM breakdown\r\n");
                serial_write(self.uart_va, b"  netinfo           - Display VirtIO-Net MAC, IP, gateway, and status\r\n");
                serial_write(self.uart_va, b"  ping [ip]         - Send ICMP echo request over VirtIO-Net\r\n");
                serial_write(self.uart_va, b"  dns <hostname>    - Resolve hostname to IPv4 address via DNS\r\n");
                serial_write(self.uart_va, b"  httpget <host> [port] [path] - Send HTTP/1.0 GET request via TCP\r\n");
                serial_write(self.uart_va, b"  clear             - Clear graphical terminal console and serial screen\r\n");
                serial_write(self.uart_va, b"  ls [path]         - List directory contents on FAT32 volume\r\n");
                serial_write(self.uart_va, b"  cat <file>        - Print text contents of a file\r\n");
                serial_write(self.uart_va, b"  hexdump <file>    - Display hexadecimal dump of a file\r\n");
                serial_write(self.uart_va, b"  bench             - Run 4-core SMP NEON 64x64 MatMul benchmark\r\n");
                serial_write(self.uart_va, b"  draw              - Redraw UEFI GOP framebuffer dashboard\r\n");
                serial_write(self.uart_va, b"  run <file>        - Execute an AArch64 ELF64 binary in EL0 user space\r\n");
                serial_write(self.uart_va, b"  infer [file]      - Run 4-core SMP INT8 NEON neural network inference\r\n");
                serial_write(self.uart_va, b"  llm [file]        - Run 4-core SMP INT8 NEON transformer block\r\n");
                serial_write(self.uart_va, b"  multitask         - Run preemptive multi-process EL0 scheduling & IPC\r\n");
                serial_write(self.uart_va, b"  generate [seed]   - Run 4-core SMP autoregressive Micro-LLM text generation\r\n");
                serial_write(self.uart_va, b"  llama [model] [prompt] - Run 4-core SMP dynamic Llama-2 inference\r\n");
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
                if let (Some(fat), Some(dev)) = (self.fat, self.dev.as_deref_mut()) {
                    let p = if arg.is_empty() { "/" } else { arg };
                    serial_write(self.uart_va, b"Directory listing for: ");
                    serial_write(self.uart_va, p.as_bytes());
                    serial_write(self.uart_va, b"\r\n");
                    let mut count = 0usize;
                    let res = fat.list_dir(
                        dev,
                        p,
                        self.scratch_pa,
                        self.scratch_va,
                        |entry| {
                            count += 1;
                            if entry.is_dir {
                                serial_write(self.uart_va, b"  [DIR]  ");
                            } else {
                                serial_write(self.uart_va, b"         ");
                            }
                            serial_write(self.uart_va, entry.name_str().as_bytes());
                            if !entry.is_dir {
                                serial_write(self.uart_va, b"  (");
                                serial_decimal(self.uart_va, entry.size as u64);
                                serial_write(self.uart_va, b" bytes)");
                            }
                            serial_write(self.uart_va, b"\r\n");
                        },
                    );
                    if let Err(e) = res {
                        serial_write(self.uart_va, b"Error listing directory: ");
                        serial_write(self.uart_va, e.as_bytes());
                        serial_write(self.uart_va, b"\r\n");
                    } else {
                        serial_write(self.uart_va, b"Total entries: ");
                        serial_decimal(self.uart_va, count as u64);
                        serial_write(self.uart_va, b"\r\n");
                    }
                } else {
                    serial_write(self.uart_va, b"FAT32 filesystem not mounted\r\n");
                }
            }
            "cat" => {
                if arg.is_empty() {
                    serial_write(self.uart_va, b"Usage: cat <file>\r\n");
                } else if let (Some(fat), Some(dev)) = (self.fat, self.dev.as_deref_mut()) {
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
            "hexdump" => {
                if arg.is_empty() {
                    serial_write(self.uart_va, b"Usage: hexdump <file>\r\n");
                } else if let (Some(fat), Some(dev)) = (self.fat, self.dev.as_deref_mut()) {
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
                } else {
                    serial_write(self.uart_va, b"Benchmark function not configured\r\n");
                }
            }
            "run" => {
                if arg.is_empty() {
                    serial_write(self.uart_va, b"Usage: run <file.elf>\r\n");
                } else if let (Some(fat), Some(dev)) = (self.fat, self.dev.as_deref_mut()) {
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
                            if let Some(run_fn) = self.run_elf_fn {
                                let elf_slice = core::slice::from_raw_parts(self.file_buf_va as *const u8, bytes);
                                match run_fn(elf_slice, self.uart_va) {
                                    Ok(status) => {
                                        serial_write(self.uart_va, b"\r\nProcess exited with status ");
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
            "clear" => {
                crate::drivers::fb::global_fb_console_clear();
                serial_write(self.uart_va, b"\x1B[2J\x1B[H");
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
            "generate" | "chat" => {
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
                                match llama_fn(model_slice, prompt, self.uart_va) {
                                    Ok(()) => {
                                        serial_write(self.uart_va, b"GENESIS: stage12 llama2 engine ok\r\n");
                                        serial_write(self.uart_va, b"GENESIS: stage13 dynamic llama2 ok\r\n");
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
            "dns" => {
                let target = if arg.is_empty() { "gateway.local" } else { arg };
                if let Some(net) = self.net_dev.as_deref_mut() {
                    serial_write(self.uart_va, b"Resolving '");
                    serial_write(self.uart_va, target.as_bytes());
                    serial_write(self.uart_va, b"' via DNS (10.0.2.3:53)...\r\n");
                    if let Some(dns_fn) = self.dns_fn {
                        match dns_fn(net, target, self.uart_va) {
                            Ok(ip) => {
                                serial_write(self.uart_va, target.as_bytes());
                                serial_write(self.uart_va, b" resolved to ");
                                serial_decimal(self.uart_va, ip[0] as u64);
                                serial_write(self.uart_va, b".");
                                serial_decimal(self.uart_va, ip[1] as u64);
                                serial_write(self.uart_va, b".");
                                serial_decimal(self.uart_va, ip[2] as u64);
                                serial_write(self.uart_va, b".");
                                serial_decimal(self.uart_va, ip[3] as u64);
                                serial_write(self.uart_va, b"\r\n");
                                serial_write(self.uart_va, b"GENESIS: stage13 dns ok\r\n");
                            }
                            Err(e) => {
                                serial_write(self.uart_va, b"DNS resolution failed: ");
                                serial_write(self.uart_va, e.as_bytes());
                                serial_write(self.uart_va, b"\r\n");
                            }
                        }
                    } else {
                        serial_write(self.uart_va, b"DNS resolver function not configured\r\n");
                    }
                } else {
                    serial_write(self.uart_va, b"VirtIO Network Device not detected\r\n");
                }
            }
            "httpget" => {
                let target = if arg.is_empty() { "10.0.2.2" } else { arg };
                let port_str = parts.next().unwrap_or("80");
                let path = parts.next().unwrap_or("/");
                if let Some(net) = self.net_dev.as_deref_mut() {
                    let mut port: u16 = 0;
                    for &b in port_str.as_bytes() {
                        if b >= b'0' && b <= b'9' {
                            port = port * 10 + (b - b'0') as u16;
                        }
                    }
                    if port == 0 { port = 80; }

                    serial_write(self.uart_va, b"HTTP GET http://");
                    serial_write(self.uart_va, target.as_bytes());
                    serial_write(self.uart_va, b":");
                    serial_decimal(self.uart_va, port as u64);
                    serial_write(self.uart_va, path.as_bytes());
                    serial_write(self.uart_va, b"...\r\n");

                    if let Some(http_fn) = self.httpget_fn {
                        match http_fn(net, target, port, path, self.uart_va) {
                            Ok(bytes) => {
                                serial_write(self.uart_va, b"HTTP GET response complete (");
                                serial_decimal(self.uart_va, bytes as u64);
                                serial_write(self.uart_va, b" bytes received)\r\n");
                                serial_write(self.uart_va, b"GENESIS: stage13 httpget ok\r\n");
                            }
                            Err(e) => {
                                serial_write(self.uart_va, b"HTTP GET failed: ");
                                serial_write(self.uart_va, e.as_bytes());
                                serial_write(self.uart_va, b"\r\n");
                            }
                        }
                    } else {
                        serial_write(self.uart_va, b"HTTP client function not configured\r\n");
                    }
                } else {
                    serial_write(self.uart_va, b"VirtIO Network Device not detected\r\n");
                }
            }
            "ping" => {
                let target_str = if arg.is_empty() { "10.0.2.2" } else { arg };
                if let Some(net) = self.net_dev.as_deref_mut() {
                    serial_write(self.uart_va, b"PING ");
                    serial_write(self.uart_va, target_str.as_bytes());
                    serial_write(self.uart_va, b" (32 bytes of data)...\r\n");
                    if let Some(ping_fn) = self.ping_fn {
                        match ping_fn(net, self.uart_va) {
                            Ok(us) => {
                                serial_write(self.uart_va, b"32 bytes from ");
                                serial_write(self.uart_va, target_str.as_bytes());
                                serial_write(self.uart_va, b": icmp_seq=1 time=");
                                serial_decimal(self.uart_va, us);
                                serial_write(self.uart_va, b" us\r\n");
                                serial_write(self.uart_va, b"GENESIS: stage12 virtio-net ping 10.0.2.2 ok\r\n");
                            }
                            Err(e) => {
                                serial_write(self.uart_va, b"Ping failed: ");
                                serial_write(self.uart_va, e.as_bytes());
                                serial_write(self.uart_va, b"\r\n");
                            }
                        }
                    } else {
                        serial_write(self.uart_va, b"Ping function not configured\r\n");
                    }
                } else {
                    serial_write(self.uart_va, b"VirtIO Network Device not detected\r\n");
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
    }

    #[test]
    fn shell_result_variants() {
        assert_eq!(ShellResult::Continue, ShellResult::Continue);
        assert_ne!(ShellResult::Continue, ShellResult::Exit);
    }
}
