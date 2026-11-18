//! Serial output utilities via PL011 UART.

pub unsafe fn mmio_read(addr: usize) -> u32 {
    core::ptr::read_volatile(addr as *const u32)
}

pub unsafe fn mmio_write(addr: usize, v: u32) {
    core::ptr::write_volatile(addr as *mut u32, v)
}

pub unsafe fn serial_write(uart: usize, bytes: &[u8]) {
    for byte in bytes {
        while mmio_read(uart + 0x18) & (1 << 5) != 0 {
            core::arch::asm!("yield", options(nomem, nostack, preserves_flags));
        }
        mmio_write(uart, *byte as u32);
    }
}

pub unsafe fn serial_hex(uart: usize, value: u64) {
    serial_write(uart, b"0x");
    for i in (0..16).rev() {
        let nibble = ((value >> (i * 4)) & 0xF) as u8;
        let c = if nibble < 10 { b'0' + nibble } else { b'a' + (nibble - 10) };
        serial_write(uart, &[c]);
    }
}

pub unsafe fn serial_decimal(uart: usize, mut value: u64) {
    let mut digits = [0u8; 20];
    let mut used = 0usize;
    loop {
        digits[used] = b'0' + (value % 10) as u8;
        used += 1;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    while used != 0 {
        used -= 1;
        serial_write(uart, &digits[used..used + 1]);
    }
}

/// Attempts to read a single byte from PL011 UART without blocking.
/// Checks bit 4 (RXFE - Receive FIFO Empty) in UARTFR (uart + 0x18).
/// Returns `Some(byte)` if a byte is available in UARTDR (uart + 0x00), or `None`.
pub unsafe fn serial_try_read_byte(uart: usize) -> Option<u8> {
    #[cfg(target_os = "uefi")]
    {
        if (mmio_read(uart + 0x18) & (1 << 4)) == 0 {
            Some((mmio_read(uart + 0x00) & 0xFF) as u8)
        } else {
            None
        }
    }
    #[cfg(not(target_os = "uefi"))]
    {
        let _ = uart;
        None
    }
}

/// Blocks until a byte is received from PL011 UART.
pub unsafe fn serial_read_byte(uart: usize) -> u8 {
    loop {
        if let Some(b) = serial_try_read_byte(uart) {
            return b;
        }
        #[cfg(target_os = "uefi")]
        core::arch::asm!("yield", options(nomem, nostack, preserves_flags));
        #[cfg(not(target_os = "uefi"))]
        core::hint::spin_loop();
    }
}

/// Reads a line of text from UART into `buf` with local echo and backspace editing.
/// Terminates on `\r` (CR) or `\n` (LF) and returns the number of bytes read.
pub unsafe fn serial_read_line(uart: usize, buf: &mut [u8]) -> usize {
    let mut len = 0usize;
    loop {
        let b = serial_read_byte(uart);
        if b == b'\r' || b == b'\n' {
            serial_write(uart, b"\r\n");
            break;
        } else if b == 0x08 || b == 0x7F {
            // Backspace or DEL
            if len > 0 {
                len -= 1;
                // Move cursor back, write space to erase, move back again
                serial_write(uart, b"\x08 \x08");
            }
        } else if b >= 0x20 && b < 0x7F {
            // Printable ASCII character
            if len < buf.len() {
                buf[len] = b;
                len += 1;
                serial_write(uart, &[b]);
            }
        }
    }
    len
}
