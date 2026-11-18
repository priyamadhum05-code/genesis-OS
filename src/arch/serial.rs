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
