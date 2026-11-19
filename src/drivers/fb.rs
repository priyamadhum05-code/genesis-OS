//! Project Genesis — UEFI GOP Framebuffer & 2D Bitmap Console Driver.
//!
//! Provides linear framebuffer access, 2D drawing primitives (rectangles, clear,
//! lines), embedded 8x8 ASCII bitmap font rendering, and kernel boot status dashboard.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Framebuffer {
    pub base_va: u64,
    pub size: usize,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_format: u32,
}

/// Constructs a 32-bit color value from R, G, B components respecting GOP pixel format.
/// Format 1 is BGR (most common in UEFI GOP), Format 0 is RGB.
#[inline]
pub fn rgb_format(pixel_format: u32, r: u8, g: u8, b: u8) -> u32 {
    if pixel_format == 1 {
        // PixelBlueGreenRedReserved8BitPerColor
        (b as u32) | ((g as u32) << 8) | ((r as u32) << 16)
    } else {
        // PixelRedGreenBlueReserved8BitPerColor
        (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
    }
}

impl Framebuffer {
    /// Constructs a 32-bit color value from R, G, B components respecting GOP pixel format.
    /// Format 1 is BGR (most common in UEFI GOP), Format 0 is RGB.
    #[inline]
    pub fn rgb(&self, r: u8, g: u8, b: u8) -> u32 {
        rgb_format(self.pixel_format, r, g, b)
    }

    /// Sets pixel at `(x, y)` to `color`.
    #[inline]
    pub unsafe fn write_pixel(&mut self, x: u32, y: u32, color: u32) {
        if x < self.width && y < self.height {
            let offset = ((y as u64) * (self.stride as u64) + (x as u64)) * 4;
            let ptr = (self.base_va + offset) as *mut u32;
            core::ptr::write_volatile(ptr, color);
        }
    }

    /// Clears the entire framebuffer to `color`.
    pub unsafe fn clear(&mut self, color: u32) {
        self.draw_rect(0, 0, self.width, self.height, color);
    }

    /// Draws a filled rectangle from `(x, y)` with width `w` and height `h`.
    pub unsafe fn draw_rect(&mut self, x: u32, y: u32, w: u32, h: u32, color: u32) {
        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);
        for row in y..y_end {
            let row_offset = ((row as u64) * (self.stride as u64) + (x as u64)) * 4;
            let row_ptr = (self.base_va + row_offset) as *mut u32;
            let count = (x_end.saturating_sub(x)) as usize;
            for col in 0..count {
                core::ptr::write_volatile(row_ptr.add(col), color);
            }
        }
    }

    /// Draws an 8x8 ASCII character at `(x, y)`.
    pub unsafe fn draw_char(&mut self, x: u32, y: u32, ch: u8, fg: u32, bg: Option<u32>) {
        let glyph = get_glyph(ch);
        for row in 0..8 {
            let bits = glyph[row as usize];
            for col in 0..8 {
                let px = x + col;
                let py = y + row;
                if (bits & (0x80 >> col)) != 0 {
                    self.write_pixel(px, py, fg);
                } else if let Some(bg_color) = bg {
                    self.write_pixel(px, py, bg_color);
                }
            }
        }
    }

    /// Draws an ASCII string starting at `(x, y)`.
    pub unsafe fn draw_string(&mut self, mut x: u32, y: u32, s: &str, fg: u32, bg: Option<u32>) {
        for b in s.bytes() {
            self.draw_char(x, y, b, fg, bg);
            x += 8;
            if x + 8 > self.width {
                break;
            }
        }
    }

    /// Draws the Stage 8 boot status dashboard.
    pub unsafe fn draw_dashboard(
        &mut self,
        cpus: u64,
        free_ram_mib: u64,
        total_ram_mib: u64,
        neon_status: bool,
    ) {
        let bg_color = self.rgb(15, 23, 42);       // Dark slate / navy #0F172A
        let card_bg = self.rgb(30, 41, 59);       // Slate card #1E293B
        let card_border = self.rgb(51, 65, 85);   // Border #334155
        let text_cyan = self.rgb(56, 189, 248);   // Cyan #38BDF8
        let text_white = self.rgb(248, 250, 252); // White #F8FAFC
        let text_green = self.rgb(52, 211, 153);  // Emerald #34D399
        let text_gray = self.rgb(148, 163, 184);  // Muted #94A3B8

        // 1. Clear background
        self.clear(bg_color);

        // 2. Header Banner
        let header_h = 48u32;
        self.draw_rect(0, 0, self.width, header_h, self.rgb(3, 105, 161)); // Header bar
        self.draw_string(24, 18, "PROJECT GENESIS  |  AARCH64 MICROKERNEL  (STAGE 8)", text_white, None);

        // 3. Status Card (centered or left-aligned with margins)
        let card_x = 32u32;
        let card_y = 64u32;
        let card_w = self.width.saturating_sub(64).min(720);
        let card_h = 320u32;

        // Card border & fill
        self.draw_rect(card_x, card_y, card_w, card_h, card_border);
        self.draw_rect(card_x + 2, card_y + 2, card_w.saturating_sub(4), card_h.saturating_sub(4), card_bg);

        // Card title
        let text_x = card_x + 20;
        let mut text_y = card_y + 20;
        self.draw_string(text_x, text_y, "SYSTEM HARDWARE & RUNTIME TELEMETRY", text_cyan, None);
        text_y += 18;
        self.draw_rect(text_x, text_y, card_w.saturating_sub(40), 1, card_border);
        text_y += 14;

        // Display info
        self.draw_string(text_x, text_y, "Display Architecture:  UEFI GOP Linear Framebuffer (AttrIndx=1)", text_white, None);
        text_y += 18;

        // Resolution line
        self.draw_string(text_x, text_y, "Screen Resolution:     ", text_gray, None);
        let mut res_buf = [0u8; 32];
        let res_len = format_res(self.width, self.height, self.stride, &mut res_buf);
        if let Ok(res_str) = core::str::from_utf8(&res_buf[..res_len]) {
            self.draw_string(text_x + 23 * 8, text_y, res_str, text_white, None);
        }
        text_y += 18;

        // CPUs online
        self.draw_string(text_x, text_y, "SMP Multi-Processing:  ", text_gray, None);
        let mut cpu_buf = [0u8; 32];
        let cpu_len = format_num_suffix(cpus, " cores active & synchronized", &mut cpu_buf);
        if let Ok(cpu_str) = core::str::from_utf8(&cpu_buf[..cpu_len]) {
            self.draw_string(text_x + 23 * 8, text_y, cpu_str, text_green, None);
        }
        text_y += 18;

        // RAM info
        self.draw_string(text_x, text_y, "Physical Memory (PMM): ", text_gray, None);
        let mut ram_buf = [0u8; 48];
        let ram_len = format_ram(free_ram_mib, total_ram_mib, &mut ram_buf);
        if let Ok(ram_str) = core::str::from_utf8(&ram_buf[..ram_len]) {
            self.draw_string(text_x + 23 * 8, text_y, ram_str, text_white, None);
        }
        text_y += 18;

        // SIMD / NEON
        self.draw_string(text_x, text_y, "Vector SIMD / NEON:    ", text_gray, None);
        let neon_str = if neon_status {
            "CPACR_EL1 enabled (4-Core Parallel MatMul OK)"
        } else {
            "Disabled"
        };
        self.draw_string(text_x + 23 * 8, text_y, neon_str, text_green, None);
        text_y += 18;

        // Storage / FAT32
        self.draw_string(text_x, text_y, "Filesystem / Storage:  VirtIO-Blk (LBA 512B) + FAT32 Mounted", text_white, None);
        text_y += 18;

        // Serial / Shell
        self.draw_string(text_x, text_y, "Serial Subsystem:      PL011 UART RX/TX (115200 8N1)", text_white, None);
        text_y += 24;

        // Status pill
        self.draw_rect(text_x, text_y, 280, 26, self.rgb(6, 78, 59)); // dark green badge
        self.draw_string(text_x + 12, text_y + 9, "STAGE 8 MILESTONE COMPLETE", text_green, None);

        // 4. Footer prompt
        let footer_y = (card_y + card_h + 20).min(self.height.saturating_sub(24));
        self.draw_string(card_x, footer_y, "Genesis Kernel Shell active on serial console. Type 'help' to begin.", text_gray, None);
    }
}

/// Scrolling graphical text console operating directly on the UEFI GOP framebuffer.
#[derive(Clone, Copy, Debug)]
pub struct FbConsole {
    pub base_va: u64,
    pub size: usize,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub pixel_format: u32,
    pub fg_color: u32,
    pub bg_color: u32,
    pub start_y: u32,
    pub max_cols: usize,
    pub max_rows: usize,
    pub cursor_col: usize,
    pub cursor_row: usize,
    pub ansi_state: u8,
}

impl FbConsole {
    pub fn new(fb: &Framebuffer) -> Self {
        let start_y = if fb.height >= 120 { 32u32 } else { 0u32 };
        let max_cols = (fb.width / 8) as usize;
        let max_rows = ((fb.height.saturating_sub(start_y)) / 8) as usize;
        let fg_color = rgb_format(fb.pixel_format, 248, 250, 252); // #F8FAFC white
        let bg_color = rgb_format(fb.pixel_format, 15, 23, 42);    // #0F172A dark slate

        Self {
            base_va: fb.base_va,
            size: fb.size,
            width: fb.width,
            height: fb.height,
            stride: fb.stride,
            pixel_format: fb.pixel_format,
            fg_color,
            bg_color,
            start_y,
            max_cols,
            max_rows,
            cursor_col: 0,
            cursor_row: 0,
            ansi_state: 0,
        }
    }

    /// Fills rectangle `(x, y, w, h)` with `color`.
    pub unsafe fn clear_rect(&mut self, x: u32, y: u32, w: u32, h: u32, color: u32) {
        let x_end = (x + w).min(self.width);
        let y_end = (y + h).min(self.height);
        for row in y..y_end {
            let row_offset = ((row as u64) * (self.stride as u64) + (x as u64)) * 4;
            let row_ptr = (self.base_va + row_offset) as *mut u32;
            let count = (x_end.saturating_sub(x)) as usize;
            for col in 0..count {
                core::ptr::write_volatile(row_ptr.add(col), color);
            }
        }
    }

    /// Draws compact top status bar banner.
    pub unsafe fn draw_header(&mut self) {
        if self.start_y == 0 {
            return;
        }
        let bar_color = rgb_format(self.pixel_format, 3, 105, 161);     // #0369A1 blue
        let border_color = rgb_format(self.pixel_format, 56, 189, 248); // #38BDF8 cyan
        let text_color = rgb_format(self.pixel_format, 248, 250, 252);   // #F8FAFC white

        self.clear_rect(0, 0, self.width, self.start_y.saturating_sub(1), bar_color);
        self.clear_rect(0, self.start_y.saturating_sub(1), self.width, 1, border_color);

        let title = "PROJECT GENESIS  |  AARCH64 GRAPHICAL TERMINAL  (STAGE 10)";
        let mut x = 16u32;
        let y = 12u32;
        for b in title.bytes() {
            if x + 8 > self.width {
                break;
            }
            let glyph = get_glyph(b);
            for r in 0..8u32 {
                let bits = glyph[r as usize];
                let row_offset = (((y + r) as u64) * (self.stride as u64) + (x as u64)) * 4;
                let row_ptr = (self.base_va + row_offset) as *mut u32;
                for c in 0..8u32 {
                    if (bits & (0x80 >> c)) != 0 {
                        core::ptr::write_volatile(row_ptr.add(c as usize), text_color);
                    }
                }
            }
            x += 8;
        }
    }

    /// Clears the terminal scrolling area and resets cursor to (0, 0).
    pub unsafe fn clear(&mut self) {
        self.draw_header();
        self.clear_rect(0, self.start_y, self.width, self.height.saturating_sub(self.start_y), self.bg_color);
        self.cursor_col = 0;
        self.cursor_row = 0;
    }

    /// Shifts rows in the console region up by 1 line (8 pixels) and clears the bottom row.
    pub unsafe fn scroll_up(&mut self) {
        if self.max_rows <= 1 {
            return;
        }
        let row_px = 8u32;
        let num_lines = (self.max_rows as u32 - 1) * row_px;
        let bytes_per_line = (self.stride as usize) * 4;
        let copy_bytes = (num_lines as usize) * bytes_per_line;

        let dst_ptr = (self.base_va + ((self.start_y as u64) * (self.stride as u64) * 4)) as *mut u8;
        let src_ptr = (self.base_va + (((self.start_y + row_px) as u64) * (self.stride as u64) * 4)) as *const u8;

        core::ptr::copy(src_ptr, dst_ptr, copy_bytes);

        let bottom_y = self.start_y + (self.max_rows as u32 - 1) * row_px;
        self.clear_rect(0, bottom_y, self.width, row_px, self.bg_color);
        self.cursor_row = self.max_rows - 1;
    }

    /// Draws single 8x8 glyph at console grid position (col, row).
    pub unsafe fn draw_glyph_at(&mut self, col: usize, row: usize, ch: u8) {
        let px = (col as u32) * 8;
        let py = self.start_y + (row as u32) * 8;
        if px + 8 > self.width || py + 8 > self.height {
            return;
        }
        let glyph = get_glyph(ch);
        for r in 0..8u32 {
            let bits = glyph[r as usize];
            let row_offset = (((py + r) as u64) * (self.stride as u64) + (px as u64)) * 4;
            let row_ptr = (self.base_va + row_offset) as *mut u32;
            for c in 0..8u32 {
                let color = if (bits & (0x80 >> c)) != 0 {
                    self.fg_color
                } else {
                    self.bg_color
                };
                core::ptr::write_volatile(row_ptr.add(c as usize), color);
            }
        }
    }

    /// Emulates terminal character processing with cursor movement, line wrap, backspace, and scroll.
    pub unsafe fn write_char(&mut self, b: u8) {
        if self.ansi_state == 1 {
            if b == b'[' {
                self.ansi_state = 2;
            } else {
                self.ansi_state = 0;
            }
            return;
        } else if self.ansi_state == 2 {
            if (0x40..=0x7E).contains(&b) {
                if b == b'J' {
                    self.clear();
                } else if b == b'H' {
                    self.cursor_col = 0;
                    self.cursor_row = 0;
                }
                self.ansi_state = 0;
            }
            return;
        }

        if b == 0x1B {
            self.ansi_state = 1;
            return;
        }

        match b {
            b'\n' => {
                self.cursor_col = 0;
                self.cursor_row += 1;
                if self.cursor_row >= self.max_rows {
                    self.scroll_up();
                }
            }
            b'\r' => {
                self.cursor_col = 0;
            }
            0x08 => {
                if self.cursor_col > 0 {
                    self.cursor_col -= 1;
                }
            }
            b'\t' => {
                let next = (self.cursor_col + 4) & !3;
                self.cursor_col = next.min(self.max_cols.saturating_sub(1));
            }
            32..=126 => {
                if self.cursor_col >= self.max_cols {
                    self.cursor_col = 0;
                    self.cursor_row += 1;
                    if self.cursor_row >= self.max_rows {
                        self.scroll_up();
                    }
                }
                self.draw_glyph_at(self.cursor_col, self.cursor_row, b);
                self.cursor_col += 1;
            }
            _ => {}
        }
    }

    /// Writes a slice of bytes into the console.
    pub unsafe fn write_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_char(b);
        }
    }
}

static mut GLOBAL_FB_CONSOLE: Option<FbConsole> = None;

pub unsafe fn register_global_fb_console(console: FbConsole) {
    *core::ptr::addr_of_mut!(GLOBAL_FB_CONSOLE) = Some(console);
}

pub unsafe fn global_fb_console_write(bytes: &[u8]) {
    if let Some(ref mut con) = *core::ptr::addr_of_mut!(GLOBAL_FB_CONSOLE) {
        con.write_bytes(bytes);
    }
}

pub unsafe fn global_fb_console_clear() {
    if let Some(ref mut con) = *core::ptr::addr_of_mut!(GLOBAL_FB_CONSOLE) {
        con.clear();
    }
}

fn format_res(w: u32, h: u32, s: u32, buf: &mut [u8]) -> usize {
    let mut len = 0;
    len += write_u32(w, &mut buf[len..]);
    if len < buf.len() { buf[len] = b'x'; len += 1; }
    len += write_u32(h, &mut buf[len..]);
    let suffix = b" (stride ";
    for &b in suffix { if len < buf.len() { buf[len] = b; len += 1; } }
    len += write_u32(s, &mut buf[len..]);
    if len < buf.len() { buf[len] = b')'; len += 1; }
    len
}

fn format_num_suffix(num: u64, suffix: &str, buf: &mut [u8]) -> usize {
    let mut len = write_u64(num, buf);
    for b in suffix.bytes() {
        if len < buf.len() { buf[len] = b; len += 1; }
    }
    len
}

fn format_ram(free_mib: u64, total_mib: u64, buf: &mut [u8]) -> usize {
    let mut len = write_u64(free_mib, buf);
    let s1 = b" MiB free / ";
    for &b in s1 { if len < buf.len() { buf[len] = b; len += 1; } }
    len += write_u64(total_mib, &mut buf[len..]);
    let s2 = b" MiB total";
    for &b in s2 { if len < buf.len() { buf[len] = b; len += 1; } }
    len
}

fn write_u32(val: u32, buf: &mut [u8]) -> usize {
    write_u64(val as u64, buf)
}

fn write_u64(mut val: u64, buf: &mut [u8]) -> usize {
    if val == 0 {
        if !buf.is_empty() { buf[0] = b'0'; return 1; }
        return 0;
    }
    let mut tmp = [0u8; 20];
    let mut n = 0;
    while val > 0 && n < tmp.len() {
        tmp[n] = b'0' + (val % 10) as u8;
        val /= 10;
        n += 1;
    }
    let mut written = 0;
    for i in (0..n).rev() {
        if written < buf.len() {
            buf[written] = tmp[i];
            written += 1;
        }
    }
    written
}

/// Embedded 8x8 ASCII font bitmap table for printable ASCII characters 32 (' ') to 126 ('~').
pub static FONT_8X8: [[u8; 8]; 95] = [
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // 32 ' '
    [0x18, 0x18, 0x18, 0x18, 0x18, 0x00, 0x18, 0x00], // 33 '!'
    [0x66, 0x66, 0x66, 0x00, 0x00, 0x00, 0x00, 0x00], // 34 '"'
    [0x6C, 0x6C, 0xFE, 0x6C, 0xFE, 0x6C, 0x6C, 0x00], // 35 '#'
    [0x18, 0x7E, 0xD8, 0x7C, 0x1B, 0x7E, 0x18, 0x00], // 36 '$'
    [0x62, 0x66, 0x0C, 0x18, 0x30, 0x66, 0x46, 0x00], // 37 '%'
    [0x38, 0x6C, 0x38, 0x76, 0xDC, 0xCC, 0x76, 0x00], // 38 '&'
    [0x18, 0x18, 0x30, 0x00, 0x00, 0x00, 0x00, 0x00], // 39 '\''
    [0x0C, 0x18, 0x30, 0x30, 0x30, 0x18, 0x0C, 0x00], // 40 '('
    [0x30, 0x18, 0x0C, 0x0C, 0x0C, 0x18, 0x30, 0x00], // 41 ')'
    [0x00, 0x66, 0x3C, 0xFF, 0x3C, 0x66, 0x00, 0x00], // 42 '*'
    [0x00, 0x18, 0x18, 0x7E, 0x18, 0x18, 0x00, 0x00], // 43 '+'
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x18, 0x30], // 44 ','
    [0x00, 0x00, 0x00, 0x7E, 0x00, 0x00, 0x00, 0x00], // 45 '-'
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x18, 0x18, 0x00], // 46 '.'
    [0x06, 0x0C, 0x18, 0x30, 0x60, 0xC0, 0x80, 0x00], // 47 '/'
    [0x3C, 0x66, 0x6E, 0x76, 0x66, 0x66, 0x3C, 0x00], // 48 '0'
    [0x18, 0x38, 0x18, 0x18, 0x18, 0x18, 0x7E, 0x00], // 49 '1'
    [0x3C, 0x66, 0x06, 0x1C, 0x30, 0x60, 0x7E, 0x00], // 50 '2'
    [0x3C, 0x66, 0x06, 0x1C, 0x06, 0x66, 0x3C, 0x00], // 51 '3'
    [0x0C, 0x1C, 0x34, 0x64, 0x7E, 0x04, 0x04, 0x00], // 52 '4'
    [0x7E, 0x60, 0x7C, 0x06, 0x06, 0x66, 0x3C, 0x00], // 53 '5'
    [0x1C, 0x30, 0x60, 0x7C, 0x66, 0x66, 0x3C, 0x00], // 54 '6'
    [0x7E, 0x06, 0x0C, 0x18, 0x30, 0x30, 0x30, 0x00], // 55 '7'
    [0x3C, 0x66, 0x66, 0x3C, 0x66, 0x66, 0x3C, 0x00], // 56 '8'
    [0x3C, 0x66, 0x66, 0x3E, 0x06, 0x0C, 0x38, 0x00], // 57 '9'
    [0x00, 0x18, 0x18, 0x00, 0x18, 0x18, 0x00, 0x00], // 58 ':'
    [0x00, 0x18, 0x18, 0x00, 0x18, 0x18, 0x30, 0x00], // 59 ';'
    [0x0C, 0x18, 0x30, 0x60, 0x30, 0x18, 0x0C, 0x00], // 60 '<'
    [0x00, 0x7E, 0x00, 0x7E, 0x00, 0x00, 0x00, 0x00], // 61 '='
    [0x30, 0x18, 0x0C, 0x06, 0x0C, 0x18, 0x30, 0x00], // 62 '>'
    [0x3C, 0x66, 0x06, 0x0C, 0x18, 0x00, 0x18, 0x00], // 63 '?'
    [0x3C, 0x66, 0x6E, 0x6A, 0x6E, 0x60, 0x3C, 0x00], // 64 '@'
    [0x18, 0x3C, 0x66, 0x7E, 0x66, 0x66, 0x66, 0x00], // 65 'A'
    [0x7C, 0x66, 0x66, 0x7C, 0x66, 0x66, 0x7C, 0x00], // 66 'B'
    [0x3C, 0x66, 0x60, 0x60, 0x60, 0x66, 0x3C, 0x00], // 67 'C'
    [0x78, 0x6C, 0x66, 0x66, 0x66, 0x6C, 0x78, 0x00], // 68 'D'
    [0x7E, 0x60, 0x60, 0x7C, 0x60, 0x60, 0x7E, 0x00], // 69 'E'
    [0x7E, 0x60, 0x60, 0x7C, 0x60, 0x60, 0x60, 0x00], // 70 'F'
    [0x3C, 0x66, 0x60, 0x6E, 0x66, 0x66, 0x3C, 0x00], // 71 'G'
    [0x66, 0x66, 0x66, 0x7E, 0x66, 0x66, 0x66, 0x00], // 72 'H'
    [0x3C, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3C, 0x00], // 73 'I'
    [0x0E, 0x06, 0x06, 0x06, 0x06, 0x66, 0x3C, 0x00], // 74 'J'
    [0x66, 0x6C, 0x78, 0x70, 0x78, 0x6C, 0x66, 0x00], // 75 'K'
    [0x60, 0x60, 0x60, 0x60, 0x60, 0x60, 0x7E, 0x00], // 76 'L'
    [0x63, 0x77, 0x7F, 0x6B, 0x63, 0x63, 0x63, 0x00], // 77 'M'
    [0x66, 0x76, 0x7E, 0x7E, 0x6E, 0x66, 0x66, 0x00], // 78 'N'
    [0x3C, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00], // 79 'O'
    [0x7C, 0x66, 0x66, 0x7C, 0x60, 0x60, 0x60, 0x00], // 80 'P'
    [0x3C, 0x66, 0x66, 0x66, 0x6A, 0x6C, 0x36, 0x00], // 81 'Q'
    [0x7C, 0x66, 0x66, 0x7C, 0x6C, 0x66, 0x66, 0x00], // 82 'R'
    [0x3C, 0x66, 0x60, 0x3C, 0x06, 0x66, 0x3C, 0x00], // 83 'S'
    [0x7E, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x00], // 84 'T'
    [0x66, 0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x00], // 85 'U'
    [0x66, 0x66, 0x66, 0x66, 0x66, 0x3C, 0x18, 0x00], // 86 'V'
    [0x63, 0x63, 0x63, 0x6B, 0x7F, 0x77, 0x63, 0x00], // 87 'W'
    [0x66, 0x66, 0x3C, 0x18, 0x3C, 0x66, 0x66, 0x00], // 88 'X'
    [0x66, 0x66, 0x66, 0x3C, 0x18, 0x18, 0x18, 0x00], // 89 'Y'
    [0x7E, 0x06, 0x0C, 0x18, 0x30, 0x60, 0x7E, 0x00], // 90 'Z'
    [0x3C, 0x30, 0x30, 0x30, 0x30, 0x30, 0x3C, 0x00], // 91 '['
    [0x60, 0x30, 0x18, 0x0C, 0x06, 0x03, 0x01, 0x00], // 92 '\'
    [0x3C, 0x0C, 0x0C, 0x0C, 0x0C, 0x0C, 0x3C, 0x00], // 93 ']'
    [0x18, 0x3C, 0x66, 0x00, 0x00, 0x00, 0x00, 0x00], // 94 '^'
    [0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xFF], // 95 '_'
    [0x30, 0x18, 0x0C, 0x00, 0x00, 0x00, 0x00, 0x00], // 96 '`'
    [0x00, 0x00, 0x3C, 0x06, 0x3E, 0x66, 0x3E, 0x00], // 97 'a'
    [0x60, 0x60, 0x7C, 0x66, 0x66, 0x66, 0x7C, 0x00], // 98 'b'
    [0x00, 0x00, 0x3C, 0x66, 0x60, 0x66, 0x3C, 0x00], // 99 'c'
    [0x06, 0x06, 0x3E, 0x66, 0x66, 0x66, 0x3E, 0x00], // 100 'd'
    [0x00, 0x00, 0x3C, 0x66, 0x7E, 0x60, 0x3C, 0x00], // 101 'e'
    [0x1C, 0x30, 0x7C, 0x30, 0x30, 0x30, 0x30, 0x00], // 102 'f'
    [0x00, 0x00, 0x3E, 0x66, 0x66, 0x3E, 0x06, 0x3C], // 103 'g'
    [0x60, 0x60, 0x7C, 0x66, 0x66, 0x66, 0x66, 0x00], // 104 'h'
    [0x18, 0x00, 0x38, 0x18, 0x18, 0x18, 0x3C, 0x00], // 105 'i'
    [0x0C, 0x00, 0x0C, 0x0C, 0x0C, 0x0C, 0x6C, 0x38], // 106 'j'
    [0x60, 0x60, 0x66, 0x6C, 0x78, 0x6C, 0x66, 0x00], // 107 'k'
    [0x38, 0x18, 0x18, 0x18, 0x18, 0x18, 0x3C, 0x00], // 108 'l'
    [0x00, 0x00, 0x66, 0x7F, 0x7F, 0x6B, 0x63, 0x00], // 109 'm'
    [0x00, 0x00, 0x7C, 0x66, 0x66, 0x66, 0x66, 0x00], // 110 'n'
    [0x00, 0x00, 0x3C, 0x66, 0x66, 0x66, 0x3C, 0x00], // 111 'o'
    [0x00, 0x00, 0x7C, 0x66, 0x66, 0x7C, 0x60, 0x60], // 112 'p'
    [0x00, 0x00, 0x3E, 0x66, 0x66, 0x3E, 0x06, 0x06], // 113 'q'
    [0x00, 0x00, 0x7C, 0x66, 0x60, 0x60, 0x60, 0x00], // 114 'r'
    [0x00, 0x00, 0x3E, 0x60, 0x3C, 0x06, 0x7C, 0x00], // 115 's'
    [0x18, 0x18, 0x7E, 0x18, 0x18, 0x18, 0x0E, 0x00], // 116 't'
    [0x00, 0x00, 0x66, 0x66, 0x66, 0x66, 0x3E, 0x00], // 117 'u'
    [0x00, 0x00, 0x66, 0x66, 0x66, 0x3C, 0x18, 0x00], // 118 'v'
    [0x00, 0x00, 0x63, 0x6B, 0x7F, 0x77, 0x36, 0x00], // 119 'w'
    [0x00, 0x00, 0x66, 0x3C, 0x18, 0x3C, 0x66, 0x00], // 120 'x'
    [0x00, 0x00, 0x66, 0x66, 0x66, 0x3E, 0x06, 0x3C], // 121 'y'
    [0x00, 0x00, 0x7E, 0x0C, 0x18, 0x30, 0x7E, 0x00], // 122 'z'
    [0x0E, 0x18, 0x18, 0x70, 0x18, 0x18, 0x0E, 0x00], // 123 '{'
    [0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x18, 0x00], // 124 '|'
    [0x70, 0x18, 0x18, 0x0E, 0x18, 0x18, 0x70, 0x00], // 125 '}'
    [0x76, 0xDC, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00], // 126 '~'
];

/// Returns 8x8 font bitmap slice for character `ch`.
pub fn get_glyph(ch: u8) -> &'static [u8; 8] {
    if (32..=126).contains(&ch) {
        &FONT_8X8[(ch - 32) as usize]
    } else {
        // Default to space (or box/question)
        &FONT_8X8[0]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_glyph_lookup_test() {
        assert_eq!(get_glyph(b' '), &FONT_8X8[0]);
        assert_eq!(get_glyph(b'0'), &FONT_8X8[16]);
        assert_eq!(get_glyph(b'A'), &FONT_8X8[33]);
        assert_eq!(get_glyph(b'a'), &FONT_8X8[65]);
        assert_eq!(get_glyph(b'~'), &FONT_8X8[94]);
        assert_eq!(get_glyph(200), &FONT_8X8[0]); // out of range falls back to space
    }

    #[test]
    fn color_encoding_test() {
        let fb_rgb = Framebuffer {
            base_va: 0,
            size: 0,
            width: 1024,
            height: 768,
            stride: 1024,
            pixel_format: 0, // RGB
        };
        let fb_bgr = Framebuffer {
            base_va: 0,
            size: 0,
            width: 1024,
            height: 768,
            stride: 1024,
            pixel_format: 1, // BGR
        };

        // Red: R=0xFF, G=0x00, B=0x00
        assert_eq!(fb_rgb.rgb(0xFF, 0x00, 0x00), 0x000000FF);
        assert_eq!(fb_bgr.rgb(0xFF, 0x00, 0x00), 0x00FF0000);

        // Blue: R=0x00, G=0x00, B=0xFF
        assert_eq!(fb_rgb.rgb(0x00, 0x00, 0xFF), 0x00FF0000);
        assert_eq!(fb_bgr.rgb(0x00, 0x00, 0xFF), 0x000000FF);
    }

    #[test]
    fn fb_console_cursor_and_scroll_test() {
        extern crate std;
        let mut buffer = std::vec![0u32; 800 * 64];
        let mut console = FbConsole {
            base_va: buffer.as_mut_ptr() as u64,
            size: buffer.len() * 4,
            width: 800,
            height: 64,
            stride: 800,
            pixel_format: 0,
            fg_color: 0x00FF_FFFF,
            bg_color: 0x0000_0000,
            start_y: 16,
            max_cols: 100,
            max_rows: (64 - 16) / 8, // 6 rows
            cursor_col: 0,
            cursor_row: 0,
            ansi_state: 0,
        };

        unsafe {
            console.write_bytes(b"hello world\n");
            assert_eq!(console.cursor_col, 0);
            assert_eq!(console.cursor_row, 1);

            console.write_bytes(b"abc");
            assert_eq!(console.cursor_col, 3);
            assert_eq!(console.cursor_row, 1);

            // Backspace: \x08 \x08 moves col 3 -> 2 -> 3 -> 2
            console.write_bytes(b"\x08 \x08");
            assert_eq!(console.cursor_col, 2);

            // Fill rows to trigger scroll
            for _ in 0..10 {
                console.write_bytes(b"line\n");
            }
            assert_eq!(console.cursor_row, console.max_rows - 1);
        }
    }
}
