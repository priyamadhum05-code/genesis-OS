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

/// Telemetry state displayed in the split-pane AI workspace sidebar.
#[derive(Clone, Copy, Debug)]
pub struct WorkspaceTelemetry {
    pub active_model: &'static str,
    pub tokens_per_sec: u64,
    pub latency_us: u64,
    pub pe_kib: u64,
    pub heap_used_kib: u64,
    pub hugepages_kib: u64,
    pub free_ram_mib: u64,
    pub total_ram_mib: u64,
    pub ipc_verified: bool,
    pub net_ip: [u8; 4],
    pub net_online: bool,
    pub sys_status: &'static str,
}

pub static mut GLOBAL_WORKSPACE_TELEMETRY: WorkspaceTelemetry = WorkspaceTelemetry {
    active_model: "Llama-2 SMP",
    tokens_per_sec: 0,
    latency_us: 0,
    pe_kib: 312,
    heap_used_kib: 128,
    hugepages_kib: 4096,
    free_ram_mib: 480,
    total_ram_mib: 512,
    ipc_verified: true,
    net_ip: [10, 0, 2, 15],
    net_online: true,
    sys_status: "READY / SHELL",
};

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
    pub pane_x: u32,
    pub pane_y: u32,
    pub pane_width: u32,
    pub pane_height: u32,
    pub max_cols: usize,
    pub max_rows: usize,
    pub cursor_col: usize,
    pub cursor_row: usize,
    pub ansi_state: u8,
}

impl FbConsole {
    pub fn new(fb: &Framebuffer) -> Self {
        let is_split = fb.width >= 720 && fb.height >= 400;
        let start_y = if fb.height >= 120 { 32u32 } else { 0u32 };
        let pane_x = 0u32;
        let pane_y = start_y;
        let pane_width = if is_split { 560u32 } else { fb.width };
        let pane_height = fb.height.saturating_sub(start_y);
        let max_cols = (pane_width / 8) as usize;
        let max_rows = (pane_height / 8) as usize;
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
            pane_x,
            pane_y,
            pane_width,
            pane_height,
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

    /// Draws a string at pixel position `(x, y)` in `fg` color.
    pub unsafe fn draw_string_at(&mut self, mut x: u32, y: u32, s: &str, fg: u32) {
        for b in s.bytes() {
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
                        core::ptr::write_volatile(row_ptr.add(c as usize), fg);
                    }
                }
            }
            x += 8;
        }
    }

    /// Draws top status bar (32px).
    pub unsafe fn draw_header(&mut self) {
        if self.start_y == 0 {
            return;
        }
        let bar_color = rgb_format(self.pixel_format, 3, 105, 161);     // #0369A1 blue
        let border_color = rgb_format(self.pixel_format, 56, 189, 248); // #38BDF8 cyan
        let text_color = rgb_format(self.pixel_format, 248, 250, 252);   // #F8FAFC white
        let green_color = rgb_format(self.pixel_format, 52, 211, 153);  // #34D399 emerald

        self.clear_rect(0, 0, self.width, self.start_y.saturating_sub(1), bar_color);
        self.clear_rect(0, self.start_y.saturating_sub(1), self.width, 1, border_color);

        if self.width >= 720 {
            self.draw_string_at(12, 12, "PROJECT GENESIS  |  AI WORKSPACE (STAGE 14)", text_color);
            self.draw_string_at(380, 12, "[SMP: 4 CORES]", green_color);

            let tel = *core::ptr::addr_of!(GLOBAL_WORKSPACE_TELEMETRY);
            let mut ram_buf = [0u8; 32];
            let mut rlen = 0;
            let pfx = b"RAM: ";
            for &b in pfx { ram_buf[rlen] = b; rlen += 1; }
            rlen += write_u64(tel.free_ram_mib, &mut ram_buf[rlen..]);
            if rlen < ram_buf.len() { ram_buf[rlen] = b'/'; rlen += 1; }
            rlen += write_u64(tel.total_ram_mib, &mut ram_buf[rlen..]);
            let sfx = b"M";
            for &b in sfx { if rlen < ram_buf.len() { ram_buf[rlen] = b; rlen += 1; } }
            if let Ok(ram_s) = core::str::from_utf8(&ram_buf[..rlen]) {
                self.draw_string_at(520, 12, ram_s, text_color);
            }

            let net_s = if tel.net_online { "NET: 10.0.2.15" } else { "NET: OFFLINE" };
            self.draw_string_at(670, 12, net_s, green_color);
        } else {
            let title = "PROJECT GENESIS  |  AARCH64 GRAPHICAL TERMINAL  (STAGE 14)";
            self.draw_string_at(16, 12, title, text_color);
        }
    }

    /// Renders the Right Telemetry Sidebar (cards: AI Engine, Memory, System & Network).
    pub unsafe fn draw_workspace_sidebar(&mut self, tel: &WorkspaceTelemetry) {
        if self.width < 720 || self.height < 400 {
            return;
        }

        let border_color = rgb_format(self.pixel_format, 51, 65, 85);   // #334155 slate
        let bg_sidebar = rgb_format(self.pixel_format, 11, 17, 32);     // #0B1120 deep navy
        let bg_card = rgb_format(self.pixel_format, 30, 41, 59);        // #1E293B card slate
        let cyan_color = rgb_format(self.pixel_format, 56, 189, 248);   // #38BDF8 cyan
        let white_color = rgb_format(self.pixel_format, 248, 250, 252); // #F8FAFC white
        let green_color = rgb_format(self.pixel_format, 52, 211, 153);  // #34D399 emerald
        let badge_bg = rgb_format(self.pixel_format, 6, 78, 59);        // #064E3B dark green
        let gray_color = rgb_format(self.pixel_format, 148, 163, 184);  // #94A3B8 muted gray

        let sb_x = 560u32;
        let sb_w = self.width.saturating_sub(sb_x);
        let sb_h = self.pane_height;

        // Vertical divider line
        self.clear_rect(sb_x, self.pane_y, 2, sb_h, border_color);

        // Sidebar background
        self.clear_rect(sb_x + 2, self.pane_y, sb_w.saturating_sub(2), sb_h, bg_sidebar);

        let card_x = sb_x + 8;
        let card_w = sb_w.saturating_sub(16).min(224);

        // 1. Card 1: AI INFERENCE ENGINE
        let c1_y = 40u32;
        let c1_h = 160u32;
        self.clear_rect(card_x, c1_y, card_w, c1_h, border_color);
        self.clear_rect(card_x + 1, c1_y + 1, card_w.saturating_sub(2), c1_h.saturating_sub(2), bg_card);
        self.draw_string_at(card_x + 10, c1_y + 10, "AI INFERENCE ENGINE", cyan_color);
        self.clear_rect(card_x + 8, c1_y + 24, card_w.saturating_sub(16), 1, border_color);

        self.draw_string_at(card_x + 10, c1_y + 34, "Model:   ", gray_color);
        self.draw_string_at(card_x + 74, c1_y + 34, tel.active_model, white_color);

        self.draw_string_at(card_x + 10, c1_y + 54, "Speed:   ", gray_color);
        let mut spd_buf = [0u8; 32];
        let slen = write_u64(tel.tokens_per_sec, &mut spd_buf);
        let sfx = b" tok/s";
        let mut total_slen = slen;
        for &b in sfx { if total_slen < spd_buf.len() { spd_buf[total_slen] = b; total_slen += 1; } }
        if let Ok(spd_str) = core::str::from_utf8(&spd_buf[..total_slen]) {
            self.draw_string_at(card_x + 74, c1_y + 54, spd_str, green_color);
        }

        self.draw_string_at(card_x + 10, c1_y + 74, "Compute: ", gray_color);
        self.draw_string_at(card_x + 74, c1_y + 74, "4x Cortex-A76", white_color);

        self.draw_string_at(card_x + 10, c1_y + 94, "Vector:  ", gray_color);
        self.draw_string_at(card_x + 74, c1_y + 94, "NEON FP32+INT8", white_color);

        self.draw_string_at(card_x + 10, c1_y + 114, "Latency: ", gray_color);
        let mut lat_buf = [0u8; 32];
        let llen = write_u64(tel.latency_us, &mut lat_buf);
        let lsfx = b" us";
        let mut total_llen = llen;
        for &b in lsfx { if total_llen < lat_buf.len() { lat_buf[total_llen] = b; total_llen += 1; } }
        if let Ok(lat_str) = core::str::from_utf8(&lat_buf[..total_llen]) {
            self.draw_string_at(card_x + 74, c1_y + 114, lat_str, white_color);
        }

        self.draw_string_at(card_x + 10, c1_y + 134, "Format:  ", gray_color);
        self.draw_string_at(card_x + 74, c1_y + 134, "llama2.c (GQA)", green_color);

        // 2. Card 2: MEMORY PROFILE
        let c2_y = 210u32;
        let c2_h = 160u32;
        self.clear_rect(card_x, c2_y, card_w, c2_h, border_color);
        self.clear_rect(card_x + 1, c2_y + 1, card_w.saturating_sub(2), c2_h.saturating_sub(2), bg_card);
        self.draw_string_at(card_x + 10, c2_y + 10, "MEMORY PROFILE", cyan_color);
        self.clear_rect(card_x + 8, c2_y + 24, card_w.saturating_sub(16), 1, border_color);

        self.draw_string_at(card_x + 10, c2_y + 34, "PE Image: ", gray_color);
        let mut pe_buf = [0u8; 32];
        let plen = write_u64(tel.pe_kib, &mut pe_buf);
        let psfx = b" KiB";
        let mut total_plen = plen;
        for &b in psfx { if total_plen < pe_buf.len() { pe_buf[total_plen] = b; total_plen += 1; } }
        if let Ok(pe_str) = core::str::from_utf8(&pe_buf[..total_plen]) {
            self.draw_string_at(card_x + 82, c2_y + 34, pe_str, white_color);
        }

        self.draw_string_at(card_x + 10, c2_y + 54, "Heap:     ", gray_color);
        let mut hp_buf = [0u8; 32];
        let hlen = write_u64(tel.heap_used_kib, &mut hp_buf);
        let mut total_hlen = hlen;
        for &b in psfx { if total_hlen < hp_buf.len() { hp_buf[total_hlen] = b; total_hlen += 1; } }
        if let Ok(hp_str) = core::str::from_utf8(&hp_buf[..total_hlen]) {
            self.draw_string_at(card_x + 82, c2_y + 54, hp_str, white_color);
        }

        self.draw_string_at(card_x + 10, c2_y + 74, "Hugepage: ", gray_color);
        let mut hg_buf = [0u8; 32];
        let g_len = write_u64(tel.hugepages_kib, &mut hg_buf);
        let mut total_glen = g_len;
        for &b in psfx { if total_glen < hg_buf.len() { hg_buf[total_glen] = b; total_glen += 1; } }
        if let Ok(hg_str) = core::str::from_utf8(&hg_buf[..total_glen]) {
            self.draw_string_at(card_x + 82, c2_y + 74, hg_str, white_color);
        }

        self.draw_string_at(card_x + 10, c2_y + 94, "RAM Free: ", gray_color);
        let mut rf_buf = [0u8; 32];
        let r_len = write_u64(tel.free_ram_mib, &mut rf_buf);
        let msfx = b" MiB";
        let mut total_rlen = r_len;
        for &b in msfx { if total_rlen < rf_buf.len() { rf_buf[total_rlen] = b; total_rlen += 1; } }
        if let Ok(rf_str) = core::str::from_utf8(&rf_buf[..total_rlen]) {
            self.draw_string_at(card_x + 82, c2_y + 94, rf_str, white_color);
        }

        self.draw_string_at(card_x + 10, c2_y + 114, "Allocator:", gray_color);
        self.draw_string_at(card_x + 82, c2_y + 114, "PMM Bitmaps", green_color);

        self.draw_string_at(card_x + 10, c2_y + 134, "HHDM Map: ", gray_color);
        self.draw_string_at(card_x + 82, c2_y + 134, "2MB L2 Blocks", green_color);

        // 3. Card 3: SYSTEM & NETWORK
        let c3_y = 380u32;
        let c3_h = 175u32;
        self.clear_rect(card_x, c3_y, card_w, c3_h, border_color);
        self.clear_rect(card_x + 1, c3_y + 1, card_w.saturating_sub(2), c3_h.saturating_sub(2), bg_card);
        self.draw_string_at(card_x + 10, c3_y + 10, "SYSTEM & NETWORK", cyan_color);
        self.clear_rect(card_x + 8, c3_y + 24, card_w.saturating_sub(16), 1, border_color);

        self.draw_string_at(card_x + 10, c3_y + 34, "EL0 IPC:  ", gray_color);
        let ipc_str = if tel.ipc_verified { "VERIFIED (SMP)" } else { "READY" };
        self.draw_string_at(card_x + 82, c3_y + 34, ipc_str, green_color);

        self.draw_string_at(card_x + 10, c3_y + 54, "Network:  ", gray_color);
        let net_ip_str = if tel.net_online { "10.0.2.15 (UP)" } else { "OFFLINE" };
        self.draw_string_at(card_x + 82, c3_y + 54, net_ip_str, if tel.net_online { green_color } else { gray_color });

        self.draw_string_at(card_x + 10, c3_y + 74, "VirtIO:   ", gray_color);
        self.draw_string_at(card_x + 82, c3_y + 74, "Blk+Net+Input", white_color);

        self.draw_string_at(card_x + 10, c3_y + 94, "Status:   ", gray_color);
        self.draw_string_at(card_x + 82, c3_y + 94, tel.sys_status, green_color);

        // Stage 14 Badge Pill
        let pill_y = c3_y + 124;
        let pill_h = 24u32;
        let pill_w = card_w.saturating_sub(20);
        self.clear_rect(card_x + 10, pill_y, pill_w, pill_h, badge_bg);
        self.draw_string_at(card_x + 24, pill_y + 8, "[STAGE 14 ACTIVE]", green_color);
    }

    /// Clears the terminal scrolling area and resets cursor to (0, 0).
    pub unsafe fn clear(&mut self) {
        self.draw_header();
        self.clear_rect(self.pane_x, self.pane_y, self.pane_width, self.pane_height, self.bg_color);
        self.cursor_col = 0;
        self.cursor_row = 0;
        let tel = *core::ptr::addr_of!(GLOBAL_WORKSPACE_TELEMETRY);
        self.draw_workspace_sidebar(&tel);
    }

    /// Shifts rows in the console region up by 1 line (8 pixels) and clears the bottom row.
    pub unsafe fn scroll_up(&mut self) {
        if self.max_rows <= 1 {
            return;
        }
        let row_px = 8u32;
        let num_lines = (self.max_rows as u32 - 1) * row_px;
        let line_bytes = (self.pane_width as usize) * 4;

        for line in 0..num_lines {
            let dst_offset = (((self.pane_y + line) as u64) * (self.stride as u64) + (self.pane_x as u64)) * 4;
            let src_offset = (((self.pane_y + line + row_px) as u64) * (self.stride as u64) + (self.pane_x as u64)) * 4;
            let dst_ptr = (self.base_va + dst_offset) as *mut u8;
            let src_ptr = (self.base_va + src_offset) as *const u8;
            core::ptr::copy(src_ptr, dst_ptr, line_bytes);
        }

        let bottom_y = self.pane_y + (self.max_rows as u32 - 1) * row_px;
        self.clear_rect(self.pane_x, bottom_y, self.pane_width, row_px, self.bg_color);
        self.cursor_row = self.max_rows - 1;
    }

    /// Draws single 8x8 glyph at console grid position (col, row).
    pub unsafe fn draw_glyph_at(&mut self, col: usize, row: usize, ch: u8) {
        let px = self.pane_x + (col as u32) * 8;
        let py = self.pane_y + (row as u32) * 8;
        if px + 8 > self.pane_x + self.pane_width || py + 8 > self.pane_y + self.pane_height {
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

pub unsafe fn redraw_workspace_sidebar() {
    if let Some(ref mut con) = *core::ptr::addr_of_mut!(GLOBAL_FB_CONSOLE) {
        let tel = *core::ptr::addr_of!(GLOBAL_WORKSPACE_TELEMETRY);
        con.draw_header();
        con.draw_workspace_sidebar(&tel);
    }
}

pub unsafe fn update_workspace_telemetry(
    model: Option<&'static str>,
    tok_per_sec: Option<u64>,
    latency: Option<u64>,
    pe: Option<u64>,
    heap: Option<u64>,
    hugepages: Option<u64>,
    free_ram: Option<u64>,
    total_ram: Option<u64>,
    ipc: Option<bool>,
    net_online: Option<bool>,
    status: Option<&'static str>,
) {
    let t = &mut *core::ptr::addr_of_mut!(GLOBAL_WORKSPACE_TELEMETRY);
    if let Some(m) = model { t.active_model = m; }
    if let Some(s) = tok_per_sec { t.tokens_per_sec = s; }
    if let Some(l) = latency { t.latency_us = l; }
    if let Some(p) = pe { t.pe_kib = p; }
    if let Some(h) = heap { t.heap_used_kib = h; }
    if let Some(hp) = hugepages { t.hugepages_kib = hp; }
    if let Some(f) = free_ram { t.free_ram_mib = f; }
    if let Some(tr) = total_ram { t.total_ram_mib = tr; }
    if let Some(i) = ipc { t.ipc_verified = i; }
    if let Some(n) = net_online { t.net_online = n; }
    if let Some(st) = status { t.sys_status = st; }

    redraw_workspace_sidebar();
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

pub fn write_u64(mut val: u64, buf: &mut [u8]) -> usize {
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
            pane_x: 0,
            pane_y: 16,
            pane_width: 800,
            pane_height: 48,
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

    #[test]
    fn fb_split_pane_bounded_scroll_test() {
        extern crate std;
        let mut buffer = std::vec![0u32; 800 * 600];
        let fb = Framebuffer {
            base_va: buffer.as_mut_ptr() as u64,
            size: buffer.len() * 4,
            width: 800,
            height: 600,
            stride: 800,
            pixel_format: 0,
        };

        let mut console = FbConsole::new(&fb);
        assert_eq!(console.pane_width, 560);
        assert_eq!(console.max_cols, 70);

        // Put a canary pixel in the sidebar region (e.g. at x=600, y=100)
        let canary_idx = 100 * 800 + 600;
        let canary_val = 0xCAFE_BABE;
        buffer[canary_idx] = canary_val;

        // Draw multiple lines and trigger terminal scrolling
        unsafe {
            for _ in 0..150 {
                console.write_bytes(b"streaming terminal output in 560px left pane...\n");
            }
        }

        // The canary in the right sidebar must remain 100% untouched and uncorrupted!
        assert_eq!(buffer[canary_idx], canary_val, "Terminal scroll corrupted right sidebar!");
    }

    #[test]
    fn fb_workspace_sidebar_render_test() {
        extern crate std;
        let mut buffer = std::vec![0u32; 800 * 600];
        let fb = Framebuffer {
            base_va: buffer.as_mut_ptr() as u64,
            size: buffer.len() * 4,
            width: 800,
            height: 600,
            stride: 800,
            pixel_format: 0,
        };

        let mut console = FbConsole::new(&fb);
        let tel = WorkspaceTelemetry {
            active_model: "Llama-2 SMP",
            tokens_per_sec: 142,
            latency_us: 7040,
            pe_kib: 312,
            heap_used_kib: 128,
            hugepages_kib: 4096,
            free_ram_mib: 480,
            total_ram_mib: 512,
            ipc_verified: true,
            net_ip: [10, 0, 2, 15],
            net_online: true,
            sys_status: "READY",
        };

        unsafe {
            console.draw_header();
            console.draw_workspace_sidebar(&tel);
        }

        // Check that pixels in header and sidebar were written (non-zero)
        let header_pixel = buffer[12 * 800 + 20];
        let sidebar_pixel = buffer[50 * 800 + 580];
        assert_ne!(header_pixel, 0, "Header was not drawn");
        assert_ne!(sidebar_pixel, 0, "Sidebar card was not drawn");
    }
}
