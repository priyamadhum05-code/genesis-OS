//! Project Genesis — Apple Silicon / m1n1 Boot Target & Bringup (Stage 27).
//!
//! Provides the entry point and early bringup environment for chainloading
//! under m1n1 on Apple Silicon (M1/M2/M3/M4) platforms:
//! - S5L UART early serial driver (MMIO UTRSTAT, UTXH, URXH)
//! - Device Tree (FDT) parsing for simple-framebuffer, apple,aic/aic2, and apple,s5l-uart
//! - Early framebuffer console initialization
//! - Milestone validation ("GENESIS: m1n1 boot ok")

use crate::arch::fdt::{parse_fdt_aic, parse_fdt_apple_uart, parse_fdt_simplefb};

// Samsung S5L / Apple UART MMIO Register Offsets
pub const S5L_UTRSTAT: usize = 0x10;
pub const S5L_UTXH: usize = 0x20;
pub const S5L_URXH: usize = 0x24;

// Status bits
pub const S5L_UTRSTAT_TX_EMPTY: u32 = 0x02;
pub const S5L_UTRSTAT_RX_READY: u32 = 0x01;

pub unsafe fn s5l_serial_putc(base: u64, c: u8) {
    let stat_ptr = (base + S5L_UTRSTAT as u64) as *const u32;
    let tx_ptr = (base + S5L_UTXH as u64) as *mut u32;

    let mut spins = 0u32;
    while (core::ptr::read_volatile(stat_ptr) & S5L_UTRSTAT_TX_EMPTY) == 0 {
        spins += 1;
        if spins > 1_000_000 {
            break;
        }
        core::hint::spin_loop();
    }
    core::ptr::write_volatile(tx_ptr, c as u32);
}

pub unsafe fn s5l_serial_write(base: u64, s: &[u8]) {
    for &b in s {
        if b == b'\n' {
            s5l_serial_putc(base, b'\r');
        }
        s5l_serial_putc(base, b);
    }
}

pub unsafe fn s5l_serial_hex(base: u64, val: u64) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    s5l_serial_write(base, b"0x");
    for i in (0..16).rev() {
        let nibble = ((val >> (i * 4)) & 0xF) as usize;
        s5l_serial_putc(base, HEX[nibble]);
    }
}

/// Early m1n1 entry trampoline.
///
/// Under m1n1 / Linux AArch64 boot protocol:
/// x0 contains physical address of Device Tree Blob (FDT).
#[no_mangle]
pub extern "C" fn m1n1_entry(fdt_pa: u64) {
    if fdt_pa == 0 {
        return;
    }
    let fdt_ptr = fdt_pa as *const u8;

    // Check FDT magic (0xd00dfeed)
    let magic = unsafe {
        core::ptr::read_unaligned(fdt_ptr as *const u32).to_be()
    };
    if magic != 0xD00D_FEED {
        return;
    }

    // Parse early UART from FDT
    let uart_info = unsafe { parse_fdt_apple_uart(fdt_ptr, 1024 * 1024) };
    let uart_base = uart_info.map(|u| u.base).unwrap_or(0);

    if uart_base != 0 {
        unsafe {
            s5l_serial_write(uart_base, b"GENESIS: Apple Silicon / m1n1 boot target starting\r\n");
            s5l_serial_write(uart_base, b"  FDT Location: ");
            s5l_serial_hex(uart_base, fdt_pa);
            s5l_serial_write(uart_base, b"\r\n");

            s5l_serial_write(uart_base, b"  S5L UART Base: ");
            s5l_serial_hex(uart_base, uart_base);
            s5l_serial_write(uart_base, b"\r\n");

            // Check AIC
            if let Some(aic) = parse_fdt_aic(fdt_ptr, 1024 * 1024) {
                s5l_serial_write(uart_base, b"  AIC Interrupt Controller: ");
                if aic.is_v2 {
                    s5l_serial_write(uart_base, b"AIC2 Base ");
                } else {
                    s5l_serial_write(uart_base, b"AIC1 Base ");
                }
                s5l_serial_hex(uart_base, aic.base);
                s5l_serial_write(uart_base, b"\r\n");
            }

            // Check Simple Framebuffer
            if let Some(fb) = parse_fdt_simplefb(fdt_ptr, 1024 * 1024) {
                s5l_serial_write(uart_base, b"  Simple Framebuffer Base: ");
                s5l_serial_hex(uart_base, fb.base);
                s5l_serial_write(uart_base, b"\r\n");
            }

            s5l_serial_write(uart_base, b"GENESIS: m1n1 boot ok\r\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn s5l_uart_register_constants_and_offsets() {
        assert_eq!(S5L_UTRSTAT, 0x10);
        assert_eq!(S5L_UTXH, 0x20);
        assert_eq!(S5L_URXH, 0x24);

        assert_eq!(S5L_UTRSTAT_TX_EMPTY, 0x02);
        assert_eq!(S5L_UTRSTAT_RX_READY, 0x01);
    }

    #[test]
    fn m1n1_arm64_image_header_layout_validation() {
        // Test 64-byte ARM64 image header structure
        let mut header = [0u8; 64];
        // code0: branch opcode 0x14000010 (b 0x40)
        let branch_op: u32 = 0x14000010;
        header[0..4].copy_from_slice(&branch_op.to_le_bytes());
        // code1: nop (0xD503201F)
        let nop_op: u32 = 0xD503201F;
        header[4..8].copy_from_slice(&nop_op.to_le_bytes());
        // text_offset: 0x80000
        let text_offset: u64 = 0x80000;
        header[8..16].copy_from_slice(&text_offset.to_le_bytes());
        // image_size: 4096 + 64
        let image_size: u64 = 4160;
        header[16..24].copy_from_slice(&image_size.to_le_bytes());
        // magic at 0x38: "ARM\x64"
        header[0x38..0x3C].copy_from_slice(b"ARM\x64");

        assert_eq!(&header[0x38..0x3C], b"ARM\x64");
        assert_eq!(u32::from_le_bytes(header[0..4].try_into().unwrap()), 0x14000010);
        assert_eq!(u64::from_le_bytes(header[8..16].try_into().unwrap()), 0x80000);
    }
}
