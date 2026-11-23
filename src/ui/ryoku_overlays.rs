//! Ryoku 1.0 Overlays: Launcher, 2x2 Overview, Orbital Lockscreen, Game Bar & ryoshot BMP.
//!
//! Enforces Ryoku's single-modal arbitration rule (opening any modal closes others).
//! Provides the Walker/tofi-style centered Launcher, the 2x2 Workspace Overview,
//! the Orbital Lockscreen with concentric ring arcs, the pinned Game Bar,
//! and the ryoshot 24-bit uncompressed BMP screen capture tool.

extern crate alloc;
use alloc::vec::Vec;

use super::ryoku_theme::{
    draw_circle_arc, draw_pill, draw_rounded_glass_panel, draw_rounded_rect_outline, draw_text,
    fill_rounded_rect, measure_text, rgb, unpack_rgb, RyokuPalette, StrBuf,
};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum ActiveModal {
    None,
    Launcher,
    Overview,
    Lockscreen,
    Capture,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LauncherTarget {
    App(usize), // WIN_TERMINAL..WIN_AI
    NextTheme,
    CycleBar,
    CycleWm,
    ToggleNightlight,
    ToggleGameBar,
    OpenOverview,
    LockScreen,
    Screenshot,
    Restart,
    Poweroff,
}

pub struct LauncherItem {
    pub title: &'static str,
    pub desc: &'static str,
    pub target: LauncherTarget,
}

pub const LAUNCHER_ITEMS: [LauncherItem; 16] = [
    LauncherItem { title: "Kitty Terminal", desc: "AArch64 EL0 shell & system utilities", target: LauncherTarget::App(0) },
    LauncherItem { title: "Vitals (btop)", desc: "Full-window CPU, RAM & disk resource monitor", target: LauncherTarget::App(1) },
    LauncherItem { title: "Files (Nautilus)", desc: "FAT32 file manager & ELF binary launcher", target: LauncherTarget::App(2) },
    LauncherItem { title: "Neovim Editor", desc: "Line-numbered text buffer & FAT32 save", target: LauncherTarget::App(3) },
    LauncherItem { title: "Ryoku Hub", desc: "Settings, themes, bar styles, display & network", target: LauncherTarget::App(5) },
    LauncherItem { title: "Genesis AI", desc: "Llama-2 SMP INT8 transformer inference", target: LauncherTarget::App(4) },
    LauncherItem { title: "Next Theme", desc: "Cycle through all 19 Ryoku color themes", target: LauncherTarget::NextTheme },
    LauncherItem { title: "Cycle Bar Style", desc: "Switch Orpheus / QsBar / Sumi / Kairos / Shima / Python / Nomarchy", target: LauncherTarget::CycleBar },
    LauncherItem { title: "Switch Window Manager", desc: "Cycle Hyprland BSP / Niri Columns / Wayfire Floating", target: LauncherTarget::CycleWm },
    LauncherItem { title: "Toggle Night Light", desc: "Enable/disable warm 3400K color temperature filter", target: LauncherTarget::ToggleNightlight },
    LauncherItem { title: "Take Screenshot (ryoshot)", desc: "Save 24-bit uncompressed BMP to FAT32 /SHOT_XX.BMP", target: LauncherTarget::Screenshot },
    LauncherItem { title: "Toggle Game Bar", desc: "Pin/unpin performance HUD over windows", target: LauncherTarget::ToggleGameBar },
    LauncherItem { title: "Workspace Overview", desc: "Display 2x2 live overview of all 4 workspaces", target: LauncherTarget::OpenOverview },
    LauncherItem { title: "Lock Screen", desc: "Engage Orbital lockscreen with concentric dials", target: LauncherTarget::LockScreen },
    LauncherItem { title: "Restart System", desc: "Reboot Genesis OS via PSCI SYSTEM_RESET", target: LauncherTarget::Restart },
    LauncherItem { title: "Power Off", desc: "Shutdown machine via PSCI SYSTEM_OFF", target: LauncherTarget::Poweroff },
];

pub struct LauncherState {
    pub query: [u8; 32],
    pub query_len: usize,
    pub selected_idx: usize,
}

impl LauncherState {
    pub const fn new() -> Self {
        Self {
            query: [0u8; 32],
            query_len: 0,
            selected_idx: 0,
        }
    }

    pub fn insert_char(&mut self, c: u8) {
        if self.query_len < 32 && ((c >= b'a' && c <= b'z') || (c >= b'A' && c <= b'Z') || (c >= b'0' && c <= b'9') || c == b' ' || c == b'-') {
            self.query[self.query_len] = c.to_ascii_lowercase();
            self.query_len += 1;
            self.selected_idx = 0;
        }
    }

    pub fn backspace(&mut self) {
        if self.query_len > 0 {
            self.query_len -= 1;
            self.selected_idx = 0;
        }
    }

    pub fn clear(&mut self) {
        self.query_len = 0;
        self.selected_idx = 0;
    }
}

pub struct CaptureState {
    pub delay_sec: u8, // 0 or 3
    pub countdown_ticks: u16,
    pub active_shot_number: u8, // 1..=99
}

impl CaptureState {
    pub const fn new() -> Self {
        Self {
            delay_sec: 0,
            countdown_ticks: 0,
            active_shot_number: 1,
        }
    }
}

/// Encodes uncompressed 24-bit Windows Bitmap (.BMP) from raw backbuffer
pub unsafe fn encode_backbuffer_bmp(
    buf: u64,
    stride: u32,
    width: u32,
    height: u32,
    fmt: u32,
) -> Vec<u8> {
    let row_bytes = ((width * 3 + 3) & !3) as usize; // Padded to 4-byte boundary
    let image_bytes = row_bytes * (height as usize);
    let total_file_size = 54 + image_bytes;

    let mut out: Vec<u8> = Vec::with_capacity(total_file_size);

    // 1. 14-byte BITMAPFILEHEADER
    out.push(b'B');
    out.push(b'M');
    out.extend_from_slice(&(total_file_size as u32).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes()); // Reserved 1
    out.extend_from_slice(&0u16.to_le_bytes()); // Reserved 2
    out.extend_from_slice(&54u32.to_le_bytes()); // Pixel data offset (54 bytes)

    // 2. 40-byte BITMAPINFOHEADER
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize = 40
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes()); // Bottom-up bitmap
    out.extend_from_slice(&1u16.to_le_bytes()); // Planes = 1
    out.extend_from_slice(&24u16.to_le_bytes()); // BitCount = 24 bpp
    out.extend_from_slice(&0u32.to_le_bytes()); // Compression = BI_RGB
    out.extend_from_slice(&(image_bytes as u32).to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes()); // 72 DPI (2835 ppm)
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes()); // ClrUsed = 0
    out.extend_from_slice(&0u32.to_le_bytes()); // ClrImportant = 0

    // 3. Pixel Data (Bottom-Up BGR24 scanlines)
    let ptr = buf as *const u32;
    for y_inv in 0..height {
        let y = height - 1 - y_inv;
        let row_ptr = ptr.add((y as usize) * (stride as usize));
        let mut written_bytes = 0;

        for x in 0..width {
            let col = *row_ptr.add(x as usize);
            let (r, g, b) = unpack_rgb(fmt, col);
            out.push(b);
            out.push(g);
            out.push(r);
            written_bytes += 3;
        }

        // Padding to 4-byte multiple
        while written_bytes < row_bytes {
            out.push(0);
            written_bytes += 1;
        }
    }

    out
}

/// Renders Centered Launcher Modal (Walker / tofi style)
pub unsafe fn render_launcher_modal(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    fmt: u32,
    palette: &RyokuPalette,
    state: &LauncherState,
) {
    let card_w = 520u32;
    let card_h = 380u32;
    let cx = ((screen_w - card_w) / 2) as i32;
    let cy = ((screen_h - card_h) / 2) as i32;

    let col_border = rgb(fmt, palette.border_active_a.0, palette.border_active_a.1, palette.border_active_a.2);
    let col_accent = rgb(fmt, palette.accent.0, palette.accent.1, palette.accent.2);
    let col_fg = rgb(fmt, palette.fg_primary.0, palette.fg_primary.1, palette.fg_primary.2);
    let col_muted = rgb(fmt, palette.fg_muted.0, palette.fg_muted.1, palette.fg_muted.2);
    let col_pill = rgb(fmt, palette.pill.0, palette.pill.1, palette.pill.2);

    draw_rounded_glass_panel(buf, stride, screen_w, screen_h, fmt, cx, cy, card_w, card_h, 14, palette.surface, 245, col_border);

    // Search bar input field
    fill_rounded_rect(buf, stride, screen_w, screen_h, cx + 16, cy + 16, card_w - 32, 38, 8, rgb(fmt, palette.frame_bg.0, palette.frame_bg.1, palette.frame_bg.2));
    draw_rounded_rect_outline(buf, stride, screen_w, screen_h, cx + 16, cy + 16, card_w - 32, 38, 8, col_accent);

    draw_text(buf, stride, screen_w, screen_h, cx + 28, cy + 28, "力", col_accent);
    if state.query_len == 0 {
        draw_text(buf, stride, screen_w, screen_h, cx + 52, cy + 28, "Search apps, commands or actions...", col_muted);
    } else if let Ok(q_str) = core::str::from_utf8(&state.query[..state.query_len]) {
        draw_text(buf, stride, screen_w, screen_h, cx + 52, cy + 28, q_str, col_fg);
    }

    // Filtered items list
    let q_str = core::str::from_utf8(&state.query[..state.query_len]).unwrap_or("");
    let mut visible_count = 0;
    let mut item_y = cy + 68;

    for item in LAUNCHER_ITEMS.iter() {
        if !q_str.is_empty() && !item.title.to_ascii_lowercase().contains(q_str) && !item.desc.to_ascii_lowercase().contains(q_str) {
            continue;
        }

        if visible_count >= 6 {
            break; // Show up to 6 results per page
        }

        let is_selected = visible_count == state.selected_idx;
        let bg_col = if is_selected { col_pill } else { 0 };
        if bg_col != 0 {
            fill_rounded_rect(buf, stride, screen_w, screen_h, cx + 16, item_y, card_w - 32, 44, 8, bg_col);
            draw_rounded_rect_outline(buf, stride, screen_w, screen_h, cx + 16, item_y, card_w - 32, 44, 8, col_accent);
        }

        let title_col = if is_selected { col_accent } else { col_fg };
        draw_text(buf, stride, screen_w, screen_h, cx + 28, item_y + 8, item.title, title_col);
        draw_text(buf, stride, screen_w, screen_h, cx + 28, item_y + 24, item.desc, col_muted);

        visible_count += 1;
        item_y += 48;
    }

    // Footer shortcuts
    draw_text(buf, stride, screen_w, screen_h, cx + 20, cy + card_h as i32 - 24, "↑/↓ Navigate | Enter Launch | Esc Close", col_muted);
}

/// Renders 2x2 Workspace Overview Grid
pub unsafe fn render_overview_modal(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    fmt: u32,
    palette: &RyokuPalette,
    active_ws: usize,
    busy_workspaces: [bool; 4],
) {
    let card_w = 760u32;
    let card_h = 480u32;
    let cx = ((screen_w - card_w) / 2) as i32;
    let cy = ((screen_h - card_h) / 2) as i32;

    let col_border = rgb(fmt, palette.border_active_a.0, palette.border_active_a.1, palette.border_active_a.2);
    let col_accent = rgb(fmt, palette.accent.0, palette.accent.1, palette.accent.2);
    let col_fg = rgb(fmt, palette.fg_primary.0, palette.fg_primary.1, palette.fg_primary.2);
    let col_muted = rgb(fmt, palette.fg_muted.0, palette.fg_muted.1, palette.fg_muted.2);
    let col_card = rgb(fmt, palette.frame_bg.0, palette.frame_bg.1, palette.frame_bg.2);

    draw_rounded_glass_panel(buf, stride, screen_w, screen_h, fmt, cx, cy, card_w, card_h, 14, palette.surface, 240, col_border);
    draw_text(buf, stride, screen_w, screen_h, cx + 24, cy + 18, "Workspace Overview (2x2 Grid)", col_accent);

    let quad_w = 345u32;
    let quad_h = 195u32;

    let ws_names = [
        "WS 1: Kitty Terminal & Vitals",
        "WS 2: Files & Neovim Editor",
        "WS 3: Ryoku Hub Settings",
        "WS 4: Genesis AI Transformer",
    ];

    for i in 0..4 {
        let col = i % 2;
        let row = i / 2;
        let qx = cx + 24 + (col as i32 * 360);
        let qy = cy + 48 + (row as i32 * 205);

        let is_cur = (i + 1) == active_ws;
        fill_rounded_rect(buf, stride, screen_w, screen_h, qx, qy, quad_w, quad_h, 10, col_card);

        let border_c = if is_cur { col_accent } else { col_border };
        draw_rounded_rect_outline(buf, stride, screen_w, screen_h, qx, qy, quad_w, quad_h, 10, border_c);

        draw_text(buf, stride, screen_w, screen_h, qx + 16, qy + 14, ws_names[i], if is_cur { col_accent } else { col_fg });

        let status_str = if is_cur {
            "[ACTIVE WORKSPACE]"
        } else if busy_workspaces[i] {
            "Open Windows Active"
        } else {
            "Empty Workspace"
        };
        draw_text(buf, stride, screen_w, screen_h, qx + 16, qy + 36, status_str, col_muted);

        // Click hint
        draw_pill(buf, stride, screen_w, screen_h, fmt, qx + 16, qy + quad_h as i32 - 36, 120, 24, rgb(fmt, palette.pill.0, palette.pill.1, palette.pill.2), border_c);
        draw_text(buf, stride, screen_w, screen_h, qx + 24, qy + quad_h as i32 - 30, "Switch Here", col_fg);
    }
}

/// Renders Orbital Lockscreen with concentric ring arcs
pub unsafe fn render_orbital_lockscreen(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    fmt: u32,
    palette: &RyokuPalette,
    hours_utc: u8,
    minutes_utc: u8,
    clock_24h: bool,
) {
    // 1. Full-screen dimmed backdrop (60% dark tint)
    let ptr = buf as *mut u32;
    for y in 0..screen_h {
        let row_ptr = ptr.add((y as usize) * (stride as usize));
        for x in 0..screen_w {
            let col = *row_ptr.add(x as usize);
            let (r, g, b) = unpack_rgb(fmt, col);
            *row_ptr.add(x as usize) = rgb(fmt, (r as u32 * 35 / 100) as u8, (g as u32 * 35 / 100) as u8, (b as u32 * 35 / 100) as u8);
        }
    }

    let cx = (screen_w / 2) as i32;
    let cy = (screen_h / 2) as i32 - 30;

    let col_accent = rgb(fmt, palette.accent.0, palette.accent.1, palette.accent.2);
    let col_accent2 = rgb(fmt, palette.border_active_b.0, palette.border_active_b.1, palette.border_active_b.2);
    let col_fg = rgb(fmt, palette.fg_primary.0, palette.fg_primary.1, palette.fg_primary.2);
    let col_muted = rgb(fmt, palette.fg_muted.0, palette.fg_muted.1, palette.fg_muted.2);

    // 2. Concentric Orbital Rings
    draw_circle_arc(buf, stride, screen_w, screen_h, cx, cy, 180, 2, col_accent);
    draw_circle_arc(buf, stride, screen_w, screen_h, cx, cy, 140, 1, col_muted);
    draw_circle_arc(buf, stride, screen_w, screen_h, cx, cy, 100, 2, col_accent2);

    // 3. Giant Clock & Date in center
    let mut time_buf = [0u8; 16];
    let t_len = super::ryoku_bars::format_bar_time(&mut time_buf, hours_utc, minutes_utc, clock_24h);
    let t_str = core::str::from_utf8(&time_buf[..t_len]).unwrap_or("12:00");

    let clock_w = measure_text(t_str) as i32;
    draw_text(buf, stride, screen_w, screen_h, cx - clock_w / 2, cy - 20, t_str, col_fg);
    let date_str = "Monday, 2026-11-23 UTC";
    let date_w = measure_text(date_str) as i32;
    draw_text(buf, stride, screen_w, screen_h, cx - date_w / 2, cy + 6, date_str, col_muted);

    let motto = "力と美のために (For Strength and Beauty)";
    let motto_w = measure_text(motto) as i32;
    draw_text(buf, stride, screen_w, screen_h, cx - motto_w / 2, cy + 30, motto, col_accent);

    // 4. Unlock pill at bottom
    let pill_w = 260u32;
    let pill_x = ((screen_w - pill_w) / 2) as i32;
    let pill_y = screen_h as i32 - 120;
    draw_pill(buf, stride, screen_w, screen_h, fmt, pill_x, pill_y, pill_w, 42, rgb(fmt, palette.frame_bg.0, palette.frame_bg.1, palette.frame_bg.2), col_accent);
    draw_text(buf, stride, screen_w, screen_h, pill_x + 36, pill_y + 14, "[ Enter or Click to Unlock ]", col_fg);
}

/// Renders Floating ryoshot Capture Bar
pub unsafe fn render_capture_bar(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    fmt: u32,
    palette: &RyokuPalette,
    state: &CaptureState,
) {
    let bar_w = 440u32;
    let bar_h = 56u32;
    let bx = ((screen_w - bar_w) / 2) as i32;
    let by = screen_h as i32 - 110;

    let col_border = rgb(fmt, palette.border_active_a.0, palette.border_active_a.1, palette.border_active_a.2);
    let col_accent = rgb(fmt, palette.accent.0, palette.accent.1, palette.accent.2);
    let col_fg = rgb(fmt, palette.fg_primary.0, palette.fg_primary.1, palette.fg_primary.2);
    let col_pill = rgb(fmt, palette.pill.0, palette.pill.1, palette.pill.2);

    draw_rounded_glass_panel(buf, stride, screen_w, screen_h, fmt, bx, by, bar_w, bar_h, 12, palette.surface, 240, col_border);

    // [Shot Full] Button
    draw_pill(buf, stride, screen_w, screen_h, fmt, bx + 16, by + 12, 110, 32, col_pill, col_accent);
    draw_text(buf, stride, screen_w, screen_h, bx + 28, by + 20, "Shot Full", col_accent);

    // [Delay: 0s / 3s] Button
    let del_str = StrBuf::new().push_str("Delay: ").push_u64(state.delay_sec as u64).push_str("s");
    draw_pill(buf, stride, screen_w, screen_h, fmt, bx + 138, by + 12, 110, 32, col_pill, col_border);
    draw_text(buf, stride, screen_w, screen_h, bx + 152, by + 20, &del_str, col_fg);

    // Target filename readout
    let fn_str = StrBuf::new().push_str("File: /SHOT_").push_pad2(state.active_shot_number as u64).push_str(".BMP");
    draw_text(buf, stride, screen_w, screen_h, bx + 260, by + 20, &fn_str, col_fg);
}

/// Renders Pinned Game Bar HUD
pub unsafe fn render_game_bar(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    fmt: u32,
    palette: &RyokuPalette,
    fps: u32,
    cpu_percent: u32,
    free_ram_mib: u64,
) {
    let bar_w = 360u32;
    let bar_h = 132u32;
    let gx = (screen_w.saturating_sub(bar_w + 16)) as i32;
    let gy = 52i32;

    let col_border = rgb(fmt, palette.border_active_b.0, palette.border_active_b.1, palette.border_active_b.2);
    let col_accent = rgb(fmt, palette.accent.0, palette.accent.1, palette.accent.2);
    let col_fg = rgb(fmt, palette.fg_primary.0, palette.fg_primary.1, palette.fg_primary.2);
    let col_muted = rgb(fmt, palette.fg_muted.0, palette.fg_muted.1, palette.fg_muted.2);
    let col_pill = rgb(fmt, palette.pill.0, palette.pill.1, palette.pill.2);

    draw_rounded_glass_panel(buf, stride, screen_w, screen_h, fmt, gx, gy, bar_w, bar_h, 12, palette.surface, 220, col_border);

    // Title
    draw_text(buf, stride, screen_w, screen_h, gx + 16, gy + 12, "Ryoku Performance HUD (Game Bar)", col_accent);

    let fps_str = StrBuf::new().push_str("FPS: ").push_u64(fps as u64).push_str(" (16.6 ms)");
    draw_text(buf, stride, screen_w, screen_h, gx + 16, gy + 36, &fps_str, col_fg);

    let perf_str = StrBuf::new().push_str("CPU: ").push_u64(cpu_percent as u64).push_str("% | RAM: ").push_u64(free_ram_mib).push_str("M Free");
    draw_text(buf, stride, screen_w, screen_h, gx + 16, gy + 56, &perf_str, col_muted);

    // Quick Action Pills
    draw_pill(buf, stride, screen_w, screen_h, fmt, gx + 16, gy + 84, 96, 28, col_pill, col_border);
    draw_text(buf, stride, screen_w, screen_h, gx + 28, gy + 90, "Chime", col_fg);

    draw_pill(buf, stride, screen_w, screen_h, fmt, gx + 124, gy + 84, 96, 28, col_pill, col_border);
    draw_text(buf, stride, screen_w, screen_h, gx + 136, gy + 90, "Capture", col_accent);

    draw_pill(buf, stride, screen_w, screen_h, fmt, gx + 232, gy + 84, 96, 28, col_pill, col_border);
    draw_text(buf, stride, screen_w, screen_h, gx + 252, gy + 90, "Close", col_muted);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bmp_header_and_row_padding() {
        // Create 2x2 mock backbuffer
        let width = 2u32;
        let height = 2u32;
        let buf = [0xFF00_00FFu32, 0xFF00_FF00u32, 0xFFFF_0000u32, 0xFFFF_FFFFu32];

        unsafe {
            let bmp = encode_backbuffer_bmp(buf.as_ptr() as u64, 2, width, height, 0);

            // 1. Signature 'BM'
            assert_eq!(bmp[0], b'B');
            assert_eq!(bmp[1], b'M');

            // 2. Header size = 54
            let off = u32::from_le_bytes([bmp[10], bmp[11], bmp[12], bmp[13]]);
            assert_eq!(off, 54);

            // 3. BitCount = 24 bpp
            let bpp = u16::from_le_bytes([bmp[28], bmp[29]]);
            assert_eq!(bpp, 24);

            // 4. Row bytes for width=2 is (2 * 3 + 3) & !3 = 8 bytes
            let expected_size = 54 + 8 * 2;
            assert_eq!(bmp.len(), expected_size);
        }
    }

    #[test]
    fn test_launcher_query_and_backspace() {
        let mut launcher = LauncherState::new();
        launcher.insert_char(b'K');
        launcher.insert_char(b'i');
        launcher.insert_char(b't');
        assert_eq!(&launcher.query[..launcher.query_len], b"kit");
        launcher.backspace();
        assert_eq!(&launcher.query[..launcher.query_len], b"ki");
        launcher.clear();
        assert_eq!(launcher.query_len, 0);
    }
}
