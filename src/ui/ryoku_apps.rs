//! Ryoku 1.0 Built-in Applications: Kitty, Vitals, Files, Neovim, Ryoku Hub & Genesis AI.
//!
//! Provides the complete application suite themed with the active Ryoku palette,
//! with fastfetch system readout, ryoku-* CLI commands, FAT32 LFN management,
//! ELF stdout capture, Neovim text editing, 8-page Ryoku Hub configuration, and Genesis AI.

extern crate alloc;
use super::ryoku_bars::BarConfig;
use super::ryoku_sidebars::SidebarsState;
use super::ryoku_theme::{
    draw_pill, draw_rounded_rect_outline, draw_text, fill_rect, fill_rounded_rect, rgb,
    RyokuPalette, RyokuThemeId, StrBuf,
};
use crate::fs::FatDirEntry;

pub const TERM_MAX_LINES: usize = 32;
pub const TERM_LINE_LEN: usize = 80;

pub const EXPLORER_MAX_FILES: usize = 48;
pub const EDITOR_CAPACITY: usize = 4096;

pub const HUB_PAGE_APPEARANCE: usize = 0;
pub const HUB_PAGE_BAR_SHELL: usize = 1;
pub const HUB_PAGE_WM: usize = 2;
pub const HUB_PAGE_DISPLAY: usize = 3;
pub const HUB_PAGE_CONNECTIONS: usize = 4;
pub const HUB_PAGE_STORAGE: usize = 5;
pub const HUB_PAGE_AUDIO: usize = 6;
pub const HUB_PAGE_SYSTEM: usize = 7;
pub const HUB_PAGE_COUNT: usize = 8;

#[derive(Clone, Copy)]
pub struct TermLine {
    pub text: [u8; TERM_LINE_LEN],
    pub len: usize,
    pub color_idx: u8, // 0..15 ANSI palette index
}

impl TermLine {
    pub const fn empty() -> Self {
        Self {
            text: [0; TERM_LINE_LEN],
            len: 0,
            color_idx: 7, // Default fg
        }
    }
}

pub struct KittyAppState {
    pub lines: [TermLine; TERM_MAX_LINES],
    pub line_count: usize,
    pub input_buf: [u8; 80],
    pub input_len: usize,
    pub cursor_pos: usize,
    pub scroll_offset: usize,
}

impl KittyAppState {
    pub fn new(palette: &RyokuPalette) -> Self {
        let mut state = Self {
            lines: [TermLine::empty(); TERM_MAX_LINES],
            line_count: 0,
            input_buf: [0; 80],
            input_len: 0,
            cursor_pos: 0,
            scroll_offset: 0,
        };

        // Startup fastfetch-style welcome banner
        state.append_line("  ██████╗ ███████╗███╗   ██╗███████╗███████╗██╗███████╗", 4);
        state.append_line(" ██╔════╝ ██╔════╝████╗  ██║██╔════╝██╔════╝██║██╔════╝", 4);
        state.append_line(" ██║  ███╗█████╗  ██╔██╗ ██║█████╗  ███████╗██║███████╗", 4);
        state.append_line(" ██║   ██║██╔══╝  ██║╚██╗██║██╔══╝  ╚════██║██║╚════██║", 4);
        state.append_line(" ╚██████╔╝███████╗██║ ╚████║███████╗███████║██║███████║", 4);
        state.append_line("  ╚═════╝ ╚══════╝╚═╝  ╚═══╝╚══════╝╚══════╝╚═╝╚══════╝", 4);
        state.append_line(" -------------------------------------------------------------", 8);
        state.append_line(" OS: Project Genesis x Ryoku 1.0 Amaterasu (力と美のために)", 7);
        state.append_line(" Kernel: AArch64 EL1 High-Memory Write^Execute Verified", 7);
        state.append_line(" Compositor: Hyprland / Niri / Wayfire Triple-Engine WM", 7);
        let info = StrBuf::new().push_str(" Theme: ").push_str(palette.name).push_str(" | Bar: Orpheus | Palette: 16 ANSI");
        state.append_line(&info, 6);
        state.append_line(" Type 'help' or 'ryoku-theme-list' for commands.", 2);
        state
    }

    pub fn append_line(&mut self, text: &str, color_idx: u8) {
        if self.line_count < TERM_MAX_LINES {
            let mut line = TermLine::empty();
            let b = text.as_bytes();
            let len = b.len().min(TERM_LINE_LEN);
            line.text[..len].copy_from_slice(&b[..len]);
            line.len = len;
            line.color_idx = color_idx;
            self.lines[self.line_count] = line;
            self.line_count += 1;
        } else {
            for i in 0..TERM_MAX_LINES - 1 {
                self.lines[i] = self.lines[i + 1];
            }
            let mut line = TermLine::empty();
            let b = text.as_bytes();
            let len = b.len().min(TERM_LINE_LEN);
            line.text[..len].copy_from_slice(&b[..len]);
            line.len = len;
            line.color_idx = color_idx;
            self.lines[TERM_MAX_LINES - 1] = line;
        }
    }
}

pub struct ExplorerAppState {
    pub files: [FatDirEntry; EXPLORER_MAX_FILES],
    pub file_count: usize,
    pub selected_idx: usize,
    pub scroll_offset: usize,
    pub current_dir: [u8; 64],
    pub current_dir_len: usize,
    pub preview_buf: [u8; 128],
    pub preview_len: usize,
    pub preview_is_hex: bool,
}

impl ExplorerAppState {
    pub const fn new() -> Self {
        Self {
            files: [FatDirEntry::empty(); EXPLORER_MAX_FILES],
            file_count: 0,
            selected_idx: 0,
            scroll_offset: 0,
            current_dir: [b'/'; 64],
            current_dir_len: 1,
            preview_buf: [0; 128],
            preview_len: 0,
            preview_is_hex: false,
        }
    }
}

pub struct EditorAppState {
    pub filename: [u8; 32],
    pub filename_len: usize,
    pub buffer: [u8; EDITOR_CAPACITY],
    pub buffer_len: usize,
    pub cursor_pos: usize,
    pub scroll_line: usize,
    pub dirty: bool,
    pub status_msg: [u8; 48],
    pub status_len: usize,
}

impl EditorAppState {
    pub const fn new() -> Self {
        Self {
            filename: [0; 32],
            filename_len: 0,
            buffer: [0; EDITOR_CAPACITY],
            buffer_len: 0,
            cursor_pos: 0,
            scroll_line: 0,
            dirty: false,
            status_msg: [0; 48],
            status_len: 0,
        }
    }

    pub fn set_status(&mut self, msg: &str) {
        let b = msg.as_bytes();
        let len = b.len().min(48);
        self.status_msg[..len].copy_from_slice(&b[..len]);
        self.status_len = len;
    }
}

pub struct HubAppState {
    pub active_page: usize,
    pub search_query: [u8; 32],
    pub search_len: usize,
    pub search_focused: bool,
    pub status_text: [u8; 64],
    pub status_len: usize,
}

impl HubAppState {
    pub const fn new() -> Self {
        Self {
            active_page: HUB_PAGE_APPEARANCE,
            search_query: [0; 32],
            search_len: 0,
            search_focused: false,
            status_text: [0; 64],
            status_len: 0,
        }
    }

    pub fn set_status(&mut self, msg: &str) {
        let b = msg.as_bytes();
        let len = b.len().min(64);
        self.status_text[..len].copy_from_slice(&b[..len]);
        self.status_len = len;
    }
}

pub struct AiAppState {
    pub prompt_buf: [u8; 128],
    pub prompt_len: usize,
    pub history_lines: [[u8; 72]; 16],
    pub history_lens: [usize; 16],
    pub history_count: usize,
    pub is_generating: bool,
}

impl AiAppState {
    pub const fn new() -> Self {
        Self {
            prompt_buf: [0; 128],
            prompt_len: 0,
            history_lines: [[0; 72]; 16],
            history_lens: [0; 16],
            history_count: 0,
            is_generating: false,
        }
    }

    pub fn append_history(&mut self, text: &str) {
        if self.history_count < 16 {
            let b = text.as_bytes();
            let len = b.len().min(72);
            self.history_lines[self.history_count][..len].copy_from_slice(&b[..len]);
            self.history_lens[self.history_count] = len;
            self.history_count += 1;
        } else {
            for i in 0..15 {
                self.history_lines[i] = self.history_lines[i + 1];
                self.history_lens[i] = self.history_lens[i + 1];
            }
            let b = text.as_bytes();
            let len = b.len().min(72);
            self.history_lines[15][..len].copy_from_slice(&b[..len]);
            self.history_lens[15] = len;
        }
    }
}

/// Renders Kitty Terminal contents
pub unsafe fn render_kitty_app(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    fmt: u32,
    palette: &RyokuPalette,
    state: &KittyAppState,
    win_x: i32,
    win_y: i32,
    win_w: u32,
    win_h: u32,
) {
    let col_card = rgb(fmt, palette.frame_bg.0, palette.frame_bg.1, palette.frame_bg.2);
    let col_fg = rgb(fmt, palette.fg_primary.0, palette.fg_primary.1, palette.fg_primary.2);
    let col_accent = rgb(fmt, palette.accent.0, palette.accent.1, palette.accent.2);

    fill_rect(buf, stride, screen_w, screen_h, win_x + 2, win_y + 30, win_w.saturating_sub(4), win_h.saturating_sub(32), col_card);

    let max_lines = ((win_h.saturating_sub(60)) / 16) as usize;
    let start_idx = state.line_count.saturating_sub(max_lines + state.scroll_offset);
    let mut draw_y = win_y + 36;

    for i in start_idx..state.line_count.saturating_sub(state.scroll_offset) {
        if i >= state.line_count {
            break;
        }
        let line = &state.lines[i];
        if let Ok(t_str) = core::str::from_utf8(&line.text[..line.len]) {
            let (r, g, b) = palette.term_colors[(line.color_idx % 16) as usize];
            draw_text(buf, stride, screen_w, screen_h, win_x + 10, draw_y, t_str, rgb(fmt, r, g, b));
        }
        draw_y += 16;
    }

    // Command input line: ryoku> <input>
    let inp_y = win_y + win_h as i32 - 24;
    draw_text(buf, stride, screen_w, screen_h, win_x + 10, inp_y, "ryoku> ", col_accent);
    if let Ok(inp_str) = core::str::from_utf8(&state.input_buf[..state.input_len]) {
        draw_text(buf, stride, screen_w, screen_h, win_x + 10 + 56, inp_y, inp_str, col_fg);
    }

    // Cursor block
    let cur_x = win_x + 10 + 56 + (state.cursor_pos as i32 * 8);
    fill_rect(buf, stride, screen_w, screen_h, cur_x, inp_y, 8, 16, col_accent);
}

/// Renders Vitals full-window btop-style monitor
pub unsafe fn render_vitals_app(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    fmt: u32,
    palette: &RyokuPalette,
    sidebars: &SidebarsState,
    cpu_percent: u32,
    free_ram_mib: u64,
    total_ram_mib: u64,
    heap_used_bytes: usize,
    heap_total_bytes: usize,
    uptime_sec: u64,
    irq_count: u64,
    win_x: i32,
    win_y: i32,
    win_w: u32,
    win_h: u32,
) {
    let col_card = rgb(fmt, palette.frame_bg.0, palette.frame_bg.1, palette.frame_bg.2);
    let col_fg = rgb(fmt, palette.fg_primary.0, palette.fg_primary.1, palette.fg_primary.2);
    let col_muted = rgb(fmt, palette.fg_muted.0, palette.fg_muted.1, palette.fg_muted.2);
    let col_accent = rgb(fmt, palette.accent.0, palette.accent.1, palette.accent.2);
    let col_pill = rgb(fmt, palette.pill.0, palette.pill.1, palette.pill.2);

    fill_rect(buf, stride, screen_w, screen_h, win_x + 2, win_y + 30, win_w.saturating_sub(4), win_h.saturating_sub(32), col_card);

    let info_str = StrBuf::new().push_str("btop resource monitor v1.0 | UP: ").push_pad2(uptime_sec / 60).push_str(":").push_pad2(uptime_sec % 60).push_str(" | IRQs: ").push_u64(irq_count as u64);
    draw_text(buf, stride, screen_w, screen_h, win_x + 16, win_y + 40, &info_str, col_accent);

    // CPU Section
    let cpu_title = StrBuf::new().push_str("CPU Activity & History (").push_u64(cpu_percent as u64).push_str("%)");
    draw_text(buf, stride, screen_w, screen_h, win_x + 16, win_y + 64, &cpu_title, col_fg);
    fill_rect(buf, stride, screen_w, screen_h, win_x + 16, win_y + 84, win_w.saturating_sub(32), 60, col_pill);

    let bar_step = (win_w.saturating_sub(40) / 60).max(1) as i32;
    for s in 0..59 {
        let h0 = (sidebars.cpu_history[s] as u32 * 54) / 100;
        let x0 = win_x + 20 + (s as i32 * bar_step);
        fill_rect(buf, stride, screen_w, screen_h, x0, win_y + 84 + 56 - h0 as i32, (bar_step - 1).max(1) as u32, h0, col_accent);
    }

    // Memory Section
    let mem_y = win_y + 160;
    draw_text(buf, stride, screen_w, screen_h, win_x + 16, mem_y, "Memory & PMM Allocation", col_fg);
    let ram_used = total_ram_mib.saturating_sub(free_ram_mib);
    let ram_pct = if total_ram_mib > 0 { (ram_used * 100) / total_ram_mib } else { 0 } as u32;
    let ram_str = StrBuf::new().push_str("Physical RAM: ").push_u64(ram_used).push_str("M / ").push_u64(total_ram_mib).push_str("M (").push_u64(ram_pct as u64).push_str("% used)");
    draw_text(buf, stride, screen_w, screen_h, win_x + 16, mem_y + 24, &ram_str, col_muted);

    let heap_pct = if heap_total_bytes > 0 { (heap_used_bytes * 100) / heap_total_bytes } else { 0 } as u32;
    let heap_str = StrBuf::new().push_str("Kernel Heap: ").push_usize(heap_used_bytes / 1024).push_str("K / ").push_usize(heap_total_bytes / 1024).push_str("K (").push_u64(heap_pct as u64).push_str("% used)");
    draw_text(buf, stride, screen_w, screen_h, win_x + 16, mem_y + 44, &heap_str, col_muted);
}

/// Renders Files (Nautilus) file explorer
pub unsafe fn render_files_app(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    fmt: u32,
    palette: &RyokuPalette,
    state: &ExplorerAppState,
    win_x: i32,
    win_y: i32,
    win_w: u32,
    win_h: u32,
) {
    let col_card = rgb(fmt, palette.frame_bg.0, palette.frame_bg.1, palette.frame_bg.2);
    let col_fg = rgb(fmt, palette.fg_primary.0, palette.fg_primary.1, palette.fg_primary.2);
    let col_muted = rgb(fmt, palette.fg_muted.0, palette.fg_muted.1, palette.fg_muted.2);
    let col_accent = rgb(fmt, palette.accent.0, palette.accent.1, palette.accent.2);
    let col_pill = rgb(fmt, palette.pill.0, palette.pill.1, palette.pill.2);
    let col_border = rgb(fmt, palette.border_inactive.0, palette.border_inactive.1, palette.border_inactive.2);

    fill_rect(buf, stride, screen_w, screen_h, win_x + 2, win_y + 30, win_w.saturating_sub(4), win_h.saturating_sub(32), col_card);

    // Toolbar: Current Dir | [New Dir] | [Delete] | [Run] | [Edit]
    fill_rounded_rect(buf, stride, screen_w, screen_h, win_x + 10, win_y + 36, win_w.saturating_sub(20), 32, 6, col_pill);
    if let Ok(d_str) = core::str::from_utf8(&state.current_dir[..state.current_dir_len]) {
        draw_text(buf, stride, screen_w, screen_h, win_x + 20, win_y + 44, d_str, col_accent);
    }
    draw_text(buf, stride, screen_w, screen_h, win_x + win_w as i32 - 240, win_y + 44, "[MKDIR] [RM] [RUN] [EDIT]", col_fg);

    // File list (left pane)
    let list_w = (win_w / 2).max(180);
    let mut item_y = win_y + 76;
    for i in 0..state.file_count.min(14) {
        let entry = &state.files[i];
        let is_sel = i == state.selected_idx;
        if is_sel {
            fill_rounded_rect(buf, stride, screen_w, screen_h, win_x + 10, item_y - 2, list_w.saturating_sub(20), 20, 4, col_pill);
        }

        let name = entry.name_str();
        let sym = if entry.is_dir() { "[DIR]" } else { "     " };
        let entry_str = StrBuf::new().push_str(sym).push_str(" ").push_pad_right(name, 14).push_str(" ").push_u64(entry.size as u64).push_str("B");
        let col = if is_sel { col_accent } else { col_fg };
        draw_text(buf, stride, screen_w, screen_h, win_x + 14, item_y, &entry_str, col);
        item_y += 22;
    }

    // Preview pane (right pane)
    let prev_x = win_x + list_w as i32;
    fill_rounded_rect(buf, stride, screen_w, screen_h, prev_x, win_y + 74, win_w.saturating_sub(list_w + 12), win_h.saturating_sub(84), 8, rgb(fmt, palette.surface.0, palette.surface.1, palette.surface.2));
    draw_rounded_rect_outline(buf, stride, screen_w, screen_h, prev_x, win_y + 74, win_w.saturating_sub(list_w + 12), win_h.saturating_sub(84), 8, col_border);

    draw_text(buf, stride, screen_w, screen_h, prev_x + 12, win_y + 84, "File Preview", col_accent);
    if state.preview_len > 0 {
        if let Ok(prev_str) = core::str::from_utf8(&state.preview_buf[..state.preview_len]) {
            draw_text(buf, stride, screen_w, screen_h, prev_x + 12, win_y + 110, prev_str, col_fg);
        }
    } else {
        draw_text(buf, stride, screen_w, screen_h, prev_x + 12, win_y + 110, "(Select a file to preview)", col_muted);
    }
}

/// Renders Neovim Editor
pub unsafe fn render_editor_app(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    fmt: u32,
    palette: &RyokuPalette,
    state: &EditorAppState,
    win_x: i32,
    win_y: i32,
    win_w: u32,
    win_h: u32,
) {
    let col_card = rgb(fmt, palette.frame_bg.0, palette.frame_bg.1, palette.frame_bg.2);
    let col_fg = rgb(fmt, palette.fg_primary.0, palette.fg_primary.1, palette.fg_primary.2);
    let col_muted = rgb(fmt, palette.fg_muted.0, palette.fg_muted.1, palette.fg_muted.2);
    let col_accent = rgb(fmt, palette.accent.0, palette.accent.1, palette.accent.2);
    let col_pill = rgb(fmt, palette.pill.0, palette.pill.1, palette.pill.2);

    fill_rect(buf, stride, screen_w, screen_h, win_x + 2, win_y + 30, win_w.saturating_sub(4), win_h.saturating_sub(32), col_card);

    // Toolbar: Filename | [SAVE (Ctrl+S)]
    fill_rounded_rect(buf, stride, screen_w, screen_h, win_x + 10, win_y + 36, win_w.saturating_sub(20), 28, 6, col_pill);
    let fn_str = core::str::from_utf8(&state.filename[..state.filename_len]).unwrap_or("UNTITLED.TXT");
    let dirty_str = if state.dirty { " [MODIFIED]" } else { "" };
    let title_str = StrBuf::new().push_str("Neovim: ").push_str(fn_str).push_str(dirty_str);
    draw_text(buf, stride, screen_w, screen_h, win_x + 20, win_y + 42, &title_str, col_accent);
    draw_text(buf, stride, screen_w, screen_h, win_x + win_w as i32 - 120, win_y + 42, "[SAVE: Ctrl+S]", col_fg);

    // Editor content with line numbers
    let mut cur_line = 1;
    let mut draw_y = win_y + 74;
    draw_text(buf, stride, screen_w, screen_h, win_x + 14, draw_y, " 1 | ", col_muted);

    let mut col_off = win_x + 48;
    for &b in &state.buffer[..state.buffer_len] {
        if b == b'\n' {
            cur_line += 1;
            draw_y += 18;
            if draw_y > win_y + win_h as i32 - 40 {
                break;
            }
            let l_str = if cur_line < 10 {
                StrBuf::new().push_str(" ").push_u64(cur_line).push_str(" | ")
            } else {
                StrBuf::new().push_u64(cur_line).push_str(" | ")
            };
            draw_text(buf, stride, screen_w, screen_h, win_x + 14, draw_y, &l_str, col_muted);
            col_off = win_x + 48;
        } else {
            crate::ui::ryoku_theme::draw_char(buf, stride, screen_w, screen_h, col_off, draw_y, b, col_fg);
            col_off += 8;
        }
    }

    // Status bar at bottom
    let stat_y = win_y + win_h as i32 - 22;
    fill_rect(buf, stride, screen_w, screen_h, win_x + 4, stat_y, win_w.saturating_sub(8), 18, col_pill);
    if let Ok(st_str) = core::str::from_utf8(&state.status_msg[..state.status_len]) {
        draw_text(buf, stride, screen_w, screen_h, win_x + 14, stat_y + 2, st_str, col_accent);
    }
}

/// Renders Ryoku Hub (WIN_HUB = 5) 8-Page Configuration Center
pub unsafe fn render_hub_app(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    fmt: u32,
    palette: &RyokuPalette,
    state: &HubAppState,
    bar_cfg: &BarConfig,
    wm_mode_name: &str,
    total_ram_mib: u64,
    free_ram_mib: u64,
    uptime_sec: u64,
    win_x: i32,
    win_y: i32,
    win_w: u32,
    win_h: u32,
) {
    let col_card = rgb(fmt, palette.frame_bg.0, palette.frame_bg.1, palette.frame_bg.2);
    let col_fg = rgb(fmt, palette.fg_primary.0, palette.fg_primary.1, palette.fg_primary.2);
    let col_muted = rgb(fmt, palette.fg_muted.0, palette.fg_muted.1, palette.fg_muted.2);
    let col_accent = rgb(fmt, palette.accent.0, palette.accent.1, palette.accent.2);
    let col_pill = rgb(fmt, palette.pill.0, palette.pill.1, palette.pill.2);
    let col_border = rgb(fmt, palette.border_inactive.0, palette.border_inactive.1, palette.border_inactive.2);

    fill_rect(buf, stride, screen_w, screen_h, win_x + 2, win_y + 30, win_w.saturating_sub(4), win_h.saturating_sub(32), col_card);

    // 1. Permanent 170px Left Navigation Rail
    let rail_w = 170u32;
    fill_rect(buf, stride, screen_w, screen_h, win_x + 2, win_y + 30, rail_w, win_h.saturating_sub(32), rgb(fmt, palette.surface.0, palette.surface.1, palette.surface.2));
    draw_rounded_rect_outline(buf, stride, screen_w, screen_h, win_x + 2, win_y + 30, rail_w, win_h.saturating_sub(32), 0, col_border);

    // Search Box in Rail (Ctrl+F)
    fill_rounded_rect(buf, stride, screen_w, screen_h, win_x + 8, win_y + 36, rail_w - 16, 26, 6, col_card);
    draw_text(buf, stride, screen_w, screen_h, win_x + 14, win_y + 42, "Search (Ctrl+F)", col_muted);

    let pages = [
        "Appearance",
        "Bar & Shell",
        "Window Mgr",
        "Display",
        "Connections",
        "Storage",
        "Audio",
        "System",
    ];

    let mut nav_y = win_y + 72;
    for (i, page) in pages.iter().enumerate() {
        let is_cur = i == state.active_page;
        if is_cur {
            fill_rounded_rect(buf, stride, screen_w, screen_h, win_x + 8, nav_y - 2, rail_w - 16, 26, 6, col_pill);
            fill_rounded_rect(buf, stride, screen_w, screen_h, win_x + 8, nav_y - 2, 4, 26, 2, col_accent);
        }
        let col = if is_cur { col_accent } else { col_fg };
        draw_text(buf, stride, screen_w, screen_h, win_x + 20, nav_y + 4, page, col);
        nav_y += 32;
    }

    // 2. Main Page Content (right area)
    let content_x = win_x + rail_w as i32 + 16;
    let content_y = win_y + 40;
    let title = pages[state.active_page];
    draw_text(buf, stride, screen_w, screen_h, content_x, content_y, title, col_accent);

    match state.active_page {
        HUB_PAGE_APPEARANCE => {
            draw_text(buf, stride, screen_w, screen_h, content_x, content_y + 26, "Official Ryoku Themes (Click to activate):", col_fg);
            let mut swatch_x = content_x;
            let mut swatch_y = content_y + 48;
            for (idx, th_id) in RyokuThemeId::ALL.iter().enumerate() {
                let th_pal = th_id.palette();
                let is_active = th_pal.id == palette.id;
                let bg_col = rgb(fmt, th_pal.surface.0, th_pal.surface.1, th_pal.surface.2);
                let acc_col = rgb(fmt, th_pal.accent.0, th_pal.accent.1, th_pal.accent.2);

                fill_rounded_rect(buf, stride, screen_w, screen_h, swatch_x, swatch_y, 110, 36, 6, bg_col);
                draw_rounded_rect_outline(buf, stride, screen_w, screen_h, swatch_x, swatch_y, 110, 36, 6, if is_active { acc_col } else { col_border });
                draw_text(buf, stride, screen_w, screen_h, swatch_x + 8, swatch_y + 10, th_pal.slug, if is_active { acc_col } else { col_fg });

                swatch_x += 116;
                if (idx + 1) % 4 == 0 {
                    swatch_x = content_x;
                    swatch_y += 42;
                }
            }
        }
        HUB_PAGE_BAR_SHELL => {
            draw_text(buf, stride, screen_w, screen_h, content_x, content_y + 26, "Ryoku Bar Styles & Layouts:", col_fg);
            let pos_name = match bar_cfg.position {
                crate::ui::ryoku_bars::BarPosition::Top => "Top",
                crate::ui::ryoku_bars::BarPosition::Bottom => "Bottom",
            };
            let bar_info = StrBuf::new().push_str("Current Style: ").push_str(bar_cfg.style.name()).push_str(" | Form: ").push_str(bar_cfg.form.name()).push_str(" | Position: ").push_str(pos_name);
            draw_text(buf, stride, screen_w, screen_h, content_x, content_y + 50, &bar_info, col_muted);

            draw_pill(buf, stride, screen_w, screen_h, fmt, content_x, content_y + 80, 160, 32, col_pill, col_border);
            draw_text(buf, stride, screen_w, screen_h, content_x + 16, content_y + 88, "[ Cycle Bar Style ]", col_accent);

            draw_pill(buf, stride, screen_w, screen_h, fmt, content_x + 180, content_y + 80, 160, 32, col_pill, col_border);
            draw_text(buf, stride, screen_w, screen_h, content_x + 196, content_y + 88, "[ Cycle Bar Form ]", col_accent);
        }
        HUB_PAGE_WM => {
            draw_text(buf, stride, screen_w, screen_h, content_x, content_y + 26, "Window Manager Configuration:", col_fg);
            let wm_info = StrBuf::new().push_str("Active Compositor Engine: ").push_str(wm_mode_name);
            draw_text(buf, stride, screen_w, screen_h, content_x, content_y + 50, &wm_info, col_muted);

            draw_pill(buf, stride, screen_w, screen_h, fmt, content_x, content_y + 80, 140, 32, col_pill, col_border);
            draw_text(buf, stride, screen_w, screen_h, content_x + 12, content_y + 88, "Hyprland (BSP)", col_accent);

            draw_pill(buf, stride, screen_w, screen_h, fmt, content_x + 150, content_y + 80, 140, 32, col_pill, col_border);
            draw_text(buf, stride, screen_w, screen_h, content_x + 162, content_y + 88, "Niri (Columns)", col_fg);

            draw_pill(buf, stride, screen_w, screen_h, fmt, content_x + 300, content_y + 80, 140, 32, col_pill, col_border);
            draw_text(buf, stride, screen_w, screen_h, content_x + 312, content_y + 88, "Wayfire (Float)", col_fg);
        }
        _ => {
            let desc = StrBuf::new().push_str("Active Configuration Module: ").push_str(title);
            draw_text(buf, stride, screen_w, screen_h, content_x, content_y + 26, &desc, col_muted);
            let mem_desc = StrBuf::new().push_str("Total RAM: ").push_u64(total_ram_mib).push_str("M | Free: ").push_u64(free_ram_mib).push_str("M | Uptime: ").push_u64(uptime_sec).push_str("s");
            draw_text(buf, stride, screen_w, screen_h, content_x, content_y + 50, &mem_desc, col_fg);
        }
    }

    // Bottom status bar
    let stat_y = win_y + win_h as i32 - 22;
    fill_rect(buf, stride, screen_w, screen_h, win_x + 4, stat_y, win_w.saturating_sub(8), 18, col_pill);
    let stat_info = StrBuf::new().push_str("Ryoku 1.0 Amaterasu | Active Theme: ").push_str(palette.name).push_str(" | WM: ").push_str(wm_mode_name);
    draw_text(buf, stride, screen_w, screen_h, win_x + 14, stat_y + 2, &stat_info, col_accent);
}

/// Renders Genesis AI app window
pub unsafe fn render_ai_app(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    fmt: u32,
    palette: &RyokuPalette,
    state: &AiAppState,
    win_x: i32,
    win_y: i32,
    win_w: u32,
    win_h: u32,
) {
    let col_card = rgb(fmt, palette.frame_bg.0, palette.frame_bg.1, palette.frame_bg.2);
    let col_fg = rgb(fmt, palette.fg_primary.0, palette.fg_primary.1, palette.fg_primary.2);
    let col_muted = rgb(fmt, palette.fg_muted.0, palette.fg_muted.1, palette.fg_muted.2);
    let col_accent = rgb(fmt, palette.accent.0, palette.accent.1, palette.accent.2);

    fill_rect(buf, stride, screen_w, screen_h, win_x + 2, win_y + 30, win_w.saturating_sub(4), win_h.saturating_sub(32), col_card);

    draw_text(buf, stride, screen_w, screen_h, win_x + 16, win_y + 40, "Genesis AI Assistant (Llama-2 SMP)", col_accent);
    draw_text(buf, stride, screen_w, screen_h, win_x + 16, win_y + 58, "Status: Ready (Type prompt & press Enter)", col_muted);

    // Prompt input
    let inp_y = win_y + win_h as i32 - 30;
    draw_text(buf, stride, screen_w, screen_h, win_x + 16, inp_y, "AI> ", col_accent);
    if let Ok(p_str) = core::str::from_utf8(&state.prompt_buf[..state.prompt_len]) {
        draw_text(buf, stride, screen_w, screen_h, win_x + 48, inp_y, p_str, col_fg);
    }

    // History output
    let mut h_y = win_y + 80;
    for i in 0..state.history_count {
        if let Ok(l_str) = core::str::from_utf8(&state.history_lines[i][..state.history_lens[i]]) {
            draw_text(buf, stride, screen_w, screen_h, win_x + 16, h_y, l_str, col_fg);
            h_y += 18;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_kitty_terminal_history_and_banner() {
        let pal = RyokuThemeId::TokyoNight.palette();
        let mut term = KittyAppState::new(&pal);
        assert!(term.line_count > 0);
        let orig_count = term.line_count;
        term.append_line("Test line 1", 2);
        assert_eq!(term.line_count, orig_count + 1);
    }

    #[test]
    fn test_hub_pages_and_navigation() {
        let mut hub = HubAppState::new();
        assert_eq!(hub.active_page, HUB_PAGE_APPEARANCE);
        hub.active_page = HUB_PAGE_WM;
        assert_eq!(hub.active_page, HUB_PAGE_WM);
        hub.set_status("Updated settings");
        assert!(hub.status_len > 0);
    }
}
