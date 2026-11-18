//! Project Genesis — Interactive Kernel Shell (Stage 8).
//!
//! Provides interactive command-line dispatch for system diagnostics, FAT32 navigation,
//! SIMD NEON compute benchmarks, framebuffer dashboard refresh, and PSCI shutdown.

use core::sync::atomic::Ordering;
use crate::{
    arch::{
        psci_system_off, serial_decimal, serial_hex, serial_write, ONLINE_CPUS,
    },
    drivers::{Framebuffer, VirtIOBlockDevice},
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
    pub fat: Option<&'a FatVolume>,
    pub scratch_pa: u64,
    pub scratch_va: u64,
    pub file_buf_pa: u64,
    pub file_buf_va: u64,
    pub file_buf_cap: usize,
    pub fb: Option<&'a mut Framebuffer>,
    pub total_ram_mib: u64,
    pub free_ram_mib: u64,
    pub bench_fn: Option<fn(u64) -> (u64, u64)>,
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
                serial_write(self.uart_va, b"Project Genesis Kernel Shell (Stage 8)\r\n");
                serial_write(self.uart_va, b"Available commands:\r\n");
                serial_write(self.uart_va, b"  help              - List available commands\r\n");
                serial_write(self.uart_va, b"  info              - Display system status, memory, registers, and framebuffer\r\n");
                serial_write(self.uart_va, b"  ls [path]         - List directory contents on FAT32 volume\r\n");
                serial_write(self.uart_va, b"  cat <file>        - Print text contents of a file\r\n");
                serial_write(self.uart_va, b"  hexdump <file>    - Display hexadecimal dump of a file\r\n");
                serial_write(self.uart_va, b"  bench             - Run 4-core SMP NEON 64x64 MatMul benchmark\r\n");
                serial_write(self.uart_va, b"  draw              - Redraw UEFI GOP framebuffer dashboard\r\n");
                serial_write(self.uart_va, b"  halt / poweroff   - Power off system via PSCI SYSTEM_OFF\r\n");
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
            "draw" => {
                if let Some(fb) = self.fb.as_deref_mut() {
                    let cpus = ONLINE_CPUS.load(Ordering::Acquire);
                    fb.draw_dashboard(cpus, self.free_ram_mib, self.total_ram_mib, true);
                    serial_write(self.uart_va, b"UEFI GOP framebuffer dashboard redrawn.\r\n");
                } else {
                    serial_write(self.uart_va, b"UEFI GOP framebuffer not available (headless mode)\r\n");
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
    }

    #[test]
    fn shell_result_variants() {
        assert_eq!(ShellResult::Continue, ShellResult::Continue);
        assert_ne!(ShellResult::Continue, ShellResult::Exit);
    }
}
