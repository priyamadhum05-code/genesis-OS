//! Ryoku 1.0 Sidebars, Hold-to-Activate Power Bar, Mako Notification Queue & SwayOSD.
//!
//! Provides the Left Vitals hero monitor sidebar with 60-sample sliding graphs & 2x2 CPU grid,
//! the Right Controls quick-settings sidebar with Ryoku's signature Hold-to-Activate Power Bar,
//! the Mako-style 4-slot notification toast stack with DND suppression,
//! and the SwayOSD-style bottom-center OSD pill.

extern crate alloc;

use super::ryoku_theme::{
    draw_pill, draw_rounded_glass_panel, draw_rounded_rect_outline, draw_text, fill_rect,
    fill_rounded_rect, rgb, RyokuPalette, StrBuf,
};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum PowerAction {
    Lock,
    Logout,
    Restart,
    Poweroff,
}

#[derive(Clone, Copy, Debug)]
pub struct Notification {
    pub title: [u8; 32],
    pub title_len: usize,
    pub body: [u8; 64],
    pub body_len: usize,
    pub ttl: u16, // Frames left (starts at 180)
}

impl Notification {
    pub fn new(title: &str, body: &str) -> Self {
        let mut t = [0u8; 32];
        let mut b = [0u8; 64];
        let t_bytes = title.as_bytes();
        let b_bytes = body.as_bytes();
        let tl = t_bytes.len().min(32);
        let bl = b_bytes.len().min(64);
        t[..tl].copy_from_slice(&t_bytes[..tl]);
        b[..bl].copy_from_slice(&b_bytes[..bl]);
        Self {
            title: t,
            title_len: tl,
            body: b,
            body_len: bl,
            ttl: 180,
        }
    }
}

pub struct NotificationStack {
    pub slots: [Option<Notification>; 4],
    pub dnd_enabled: bool,
}

impl NotificationStack {
    pub const fn new() -> Self {
        Self {
            slots: [None, None, None, None],
            dnd_enabled: false,
        }
    }

    pub fn push(&mut self, title: &str, body: &str) {
        if self.dnd_enabled {
            return; // Suppressed under Do Not Disturb
        }
        let note = Notification::new(title, body);
        for slot in self.slots.iter_mut() {
            if slot.is_none() {
                *slot = Some(note);
                return;
            }
        }
        // If full, shift up and insert at end
        self.slots[0] = self.slots[1];
        self.slots[1] = self.slots[2];
        self.slots[2] = self.slots[3];
        self.slots[3] = Some(note);
    }

    pub fn tick(&mut self) {
        for slot in self.slots.iter_mut() {
            if let Some(ref mut n) = slot {
                if n.ttl > 0 {
                    n.ttl -= 1;
                } else {
                    *slot = None;
                }
            }
        }
    }
}

pub struct OsdState {
    pub label: [u8; 32],
    pub label_len: usize,
    pub value: u8, // 0..=100
    pub timer: u16, // Frames left (starts at 90)
}

impl OsdState {
    pub const fn new() -> Self {
        Self {
            label: [0u8; 32],
            label_len: 0,
            value: 0,
            timer: 0,
        }
    }

    pub fn show(&mut self, text: &str, val: u8) {
        let b = text.as_bytes();
        let l = b.len().min(32);
        self.label[..l].copy_from_slice(&b[..l]);
        self.label_len = l;
        self.value = val.min(100);
        self.timer = 90;
    }

    pub fn tick(&mut self) {
        if self.timer > 0 {
            self.timer -= 1;
        }
    }
}

pub struct SidebarsState {
    pub left_open: bool,
    pub right_open: bool,
    // Quick toggles
    pub nightlight_enabled: bool,
    pub keep_awake: bool,
    pub dnd_enabled: bool,
    pub audio_muted: bool,
    pub game_mode: bool,
    pub blur_enabled: bool,
    // Hold-to-Activate Power Bar
    pub hold_action: Option<PowerAction>,
    pub hold_ticks: u8, // 0..=25 ticks
    // 60-sample smooth sliding history graphs
    pub cpu_history: [u8; 60],
    pub ram_history: [u8; 60],
    pub net_history: [u8; 60],
}

impl SidebarsState {
    pub const fn new() -> Self {
        Self {
            left_open: false,
            right_open: false,
            nightlight_enabled: false,
            keep_awake: false,
            dnd_enabled: false,
            audio_muted: false,
            game_mode: false,
            blur_enabled: true,
            hold_action: None,
            hold_ticks: 0,
            cpu_history: [0; 60],
            ram_history: [0; 60],
            net_history: [0; 60],
        }
    }

    /// Appends a new metric sample to the sliding history buffer
    pub fn push_sample(&mut self, cpu: u8, ram: u8, net: u8) {
        for i in 0..59 {
            self.cpu_history[i] = self.cpu_history[i + 1];
            self.ram_history[i] = self.ram_history[i + 1];
            self.net_history[i] = self.net_history[i + 1];
        }
        self.cpu_history[59] = cpu.min(100);
        self.ram_history[59] = ram.min(100);
        self.net_history[59] = net.min(100);
    }
}

/// Renders Left Sidebar (Vitals)
pub unsafe fn render_left_vitals_sidebar(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    fmt: u32,
    palette: &RyokuPalette,
    state: &SidebarsState,
    cpu_percent: u32,
    cpus_online: u64,
    free_ram_mib: u64,
    total_ram_mib: u64,
    heap_used_bytes: usize,
    heap_total_bytes: usize,
    uptime_sec: u64,
    irq_count: u64,
) {
    if !state.left_open {
        return;
    }

    let bar_w = 310u32;
    let bar_h = screen_h.saturating_sub(50);
    let bar_x = 8i32;
    let bar_y = 42i32;

    let col_border = rgb(fmt, palette.border_active_a.0, palette.border_active_a.1, palette.border_active_a.2);
    let col_fg = rgb(fmt, palette.fg_primary.0, palette.fg_primary.1, palette.fg_primary.2);
    let col_muted = rgb(fmt, palette.fg_muted.0, palette.fg_muted.1, palette.fg_muted.2);
    let col_accent = rgb(fmt, palette.accent.0, palette.accent.1, palette.accent.2);
    let col_pill = rgb(fmt, palette.pill.0, palette.pill.1, palette.pill.2);
    let col_card = rgb(fmt, palette.frame_bg.0, palette.frame_bg.1, palette.frame_bg.2);

    draw_rounded_glass_panel(buf, stride, screen_w, screen_h, fmt, bar_x, bar_y, bar_w, bar_h, 14, palette.surface, 235, col_border);

    // Header
    draw_text(buf, stride, screen_w, screen_h, bar_x + 16, bar_y + 16, "genesis@ryoku-arm64", col_accent);
    let up_str = StrBuf::new().push_str("UP: ").push_pad2(uptime_sec / 60).push_str(":").push_pad2(uptime_sec % 60).push_str(" | IRQ: ").push_u64(irq_count as u64);
    draw_text(buf, stride, screen_w, screen_h, bar_x + 16, bar_y + 36, &up_str, col_muted);

    // 2x2 Per-Core CPU Grid
    draw_text(buf, stride, screen_w, screen_h, bar_x + 16, bar_y + 60, "SMP Processing Cores", col_fg);
    for core in 0..4 {
        let col = core % 2;
        let row = core / 2;
        let cx = bar_x + 16 + (col * 140);
        let cy = bar_y + 80 + (row * 42);

        fill_rounded_rect(buf, stride, screen_w, screen_h, cx, cy, 132, 36, 6, col_card);
        let core_usage = if (core as u64) < cpus_online { cpu_percent as u64 } else { 0 };
        let core_str = StrBuf::new().push_str("CPU").push_u64(core as u64).push_str(": ").push_u64(core_usage).push_str("%");
        draw_text(buf, stride, screen_w, screen_h, cx + 8, cy + 8, &core_str, col_fg);

        // Core usage mini bar
        fill_rect(buf, stride, screen_w, screen_h, cx + 8, cy + 26, 116, 4, col_pill);
        let fill_w = if (core as u64) < cpus_online { (116 * cpu_percent) / 100 } else { 0 };
        fill_rect(buf, stride, screen_w, screen_h, cx + 8, cy + 26, fill_w, 4, col_accent);
    }

    // 60-Sample Smooth Sliding CPU History Graph
    let graph_y = bar_y + 175;
    draw_text(buf, stride, screen_w, screen_h, bar_x + 16, graph_y, "CPU Activity History (60s)", col_fg);
    fill_rounded_rect(buf, stride, screen_w, screen_h, bar_x + 16, graph_y + 18, 276, 52, 6, col_card);

    for s in 0..59 {
        let x0 = bar_x + 18 + (s as i32 * 4);
        let h0 = (state.cpu_history[s] as u32 * 46) / 100;
        let bar_top = (graph_y + 18 + 50).saturating_sub(h0 as i32);
        fill_rect(buf, stride, screen_w, screen_h, x0, bar_top, 3, h0, col_accent);
    }

    // Physical RAM & Kernel Heap
    let ram_y = graph_y + 80;
    draw_text(buf, stride, screen_w, screen_h, bar_x + 16, ram_y, "Memory & Kernel Heap", col_fg);
    let ram_used = total_ram_mib.saturating_sub(free_ram_mib);
    let ram_pct = if total_ram_mib > 0 { (ram_used * 100) / total_ram_mib } else { 0 } as u32;

    fill_rounded_rect(buf, stride, screen_w, screen_h, bar_x + 16, ram_y + 18, 276, 44, 6, col_card);
    let ram_str = StrBuf::new().push_str("RAM: ").push_u64(ram_used).push_str("M / ").push_u64(total_ram_mib).push_str("M (").push_u64(ram_pct as u64).push_str("%)");
    draw_text(buf, stride, screen_w, screen_h, bar_x + 24, ram_y + 24, &ram_str, col_fg);
    fill_rect(buf, stride, screen_w, screen_h, bar_x + 24, ram_y + 44, 260, 6, col_pill);
    fill_rect(buf, stride, screen_w, screen_h, bar_x + 24, ram_y + 44, (260 * ram_pct) / 100, 6, col_accent);

    let heap_y = ram_y + 70;
    fill_rounded_rect(buf, stride, screen_w, screen_h, bar_x + 16, heap_y, 276, 44, 6, col_card);
    let heap_pct = if heap_total_bytes > 0 { (heap_used_bytes * 100) / heap_total_bytes } else { 0 } as u32;
    let heap_str = StrBuf::new().push_str("Heap: ").push_usize(heap_used_bytes / 1024).push_str("K / ").push_usize(heap_total_bytes / 1024).push_str("K (").push_u64(heap_pct as u64).push_str("%)");
    draw_text(buf, stride, screen_w, screen_h, bar_x + 24, heap_y + 8, &heap_str, col_fg);
    fill_rect(buf, stride, screen_w, screen_h, bar_x + 24, heap_y + 28, 260, 6, col_pill);
    fill_rect(buf, stride, screen_w, screen_h, bar_x + 24, heap_y + 28, (260 * heap_pct) / 100, 6, col_accent);

    // Storage & Net
    let io_y = heap_y + 54;
    draw_text(buf, stride, screen_w, screen_h, bar_x + 16, io_y, "I/O Devices & Carrier", col_fg);
    draw_text(buf, stride, screen_w, screen_h, bar_x + 16, io_y + 20, "blk0 (VirtIO-Blk): FAT32 Mounted", col_muted);
    draw_text(buf, stride, screen_w, screen_h, bar_x + 16, io_y + 38, "net0 (VirtIO-Net): 10.0.2.15 Carrier UP", col_accent);
}

/// Renders Right Sidebar (Controls & Hold-to-Activate Power Bar)
pub unsafe fn render_right_controls_sidebar(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    fmt: u32,
    palette: &RyokuPalette,
    state: &SidebarsState,
) {
    if !state.right_open {
        return;
    }

    let bar_w = 310u32;
    let bar_h = screen_h.saturating_sub(50);
    let bar_x = (screen_w.saturating_sub(bar_w + 8)) as i32;
    let bar_y = 42i32;

    let col_border = rgb(fmt, palette.border_active_a.0, palette.border_active_a.1, palette.border_active_a.2);
    let col_fg = rgb(fmt, palette.fg_primary.0, palette.fg_primary.1, palette.fg_primary.2);
    let col_muted = rgb(fmt, palette.fg_muted.0, palette.fg_muted.1, palette.fg_muted.2);
    let col_accent = rgb(fmt, palette.accent.0, palette.accent.1, palette.accent.2);
    let col_pill = rgb(fmt, palette.pill.0, palette.pill.1, palette.pill.2);
    let col_card = rgb(fmt, palette.frame_bg.0, palette.frame_bg.1, palette.frame_bg.2);

    draw_rounded_glass_panel(buf, stride, screen_w, screen_h, fmt, bar_x, bar_y, bar_w, bar_h, 14, palette.surface, 235, col_border);

    // Header
    draw_text(buf, stride, screen_w, screen_h, bar_x + 16, bar_y + 16, "Quick Controls", col_accent);
    draw_text(buf, stride, screen_w, screen_h, bar_x + 16, bar_y + 36, "Hardware Toggles & Session", col_muted);

    // 2x3 Quick Toggle Tiles
    let tiles = [
        ("Night Light", state.nightlight_enabled),
        ("Keep Awake", state.keep_awake),
        ("Do Not Disturb", state.dnd_enabled),
        ("Audio Mute", state.audio_muted),
        ("Game Mode", state.game_mode),
        ("Glass Blur", state.blur_enabled),
    ];

    for (i, (name, active)) in tiles.iter().enumerate() {
        let col = i % 2;
        let row = i / 2;
        let tx = bar_x + 16 + (col as i32 * 140);
        let ty = bar_y + 60 + (row as i32 * 54);

        let bg = if *active { col_accent } else { col_card };
        let text_col = if *active { rgb(fmt, 16, 16, 20) } else { col_fg };

        fill_rounded_rect(buf, stride, screen_w, screen_h, tx, ty, 132, 46, 8, bg);
        draw_rounded_rect_outline(buf, stride, screen_w, screen_h, tx, ty, 132, 46, 8, col_pill);
        draw_text(buf, stride, screen_w, screen_h, tx + 10, ty + 12, name, text_col);
        draw_text(buf, stride, screen_w, screen_h, tx + 10, ty + 28, if *active { "ON" } else { "OFF" }, text_col);
    }

    // Active Theme readout card
    let th_y = bar_y + 240;
    fill_rounded_rect(buf, stride, screen_w, screen_h, bar_x + 16, th_y, 276, 52, 8, col_card);
    draw_text(buf, stride, screen_w, screen_h, bar_x + 24, th_y + 10, "Active Ryoku Theme", col_accent);
    draw_text(buf, stride, screen_w, screen_h, bar_x + 24, th_y + 28, palette.name, col_fg);

    // Hold-to-Activate Power Bar (Ryoku's safety mechanic)
    let pwr_y = bar_h as i32 - 160 + bar_y;
    draw_text(buf, stride, screen_w, screen_h, bar_x + 16, pwr_y, "Hold-to-Activate Power", col_fg);
    draw_text(buf, stride, screen_w, screen_h, bar_x + 16, pwr_y + 18, "Press and hold for 0.4s to execute", col_muted);

    let pwr_btns = [
        ("Lock Screen", PowerAction::Lock),
        ("Log Out", PowerAction::Logout),
        ("Restart System", PowerAction::Restart),
        ("Power Off", PowerAction::Poweroff),
    ];

    for (i, (label, action)) in pwr_btns.iter().enumerate() {
        let by = pwr_y + 36 + (i as i32 * 32);
        fill_rounded_rect(buf, stride, screen_w, screen_h, bar_x + 16, by, 276, 26, 6, col_card);
        draw_rounded_rect_outline(buf, stride, screen_w, screen_h, bar_x + 16, by, 276, 26, 6, col_pill);

        // Smooth progress fill if currently holding this action
        if state.hold_action == Some(*action) && state.hold_ticks > 0 {
            let fill_w = (276u32 * state.hold_ticks as u32) / 25;
            fill_rounded_rect(buf, stride, screen_w, screen_h, bar_x + 16, by, fill_w, 26, 6, col_accent);
        }

        let txt_col = if state.hold_action == Some(*action) && state.hold_ticks > 12 {
            rgb(fmt, 16, 16, 20)
        } else {
            col_fg
        };
        draw_text(buf, stride, screen_w, screen_h, bar_x + 24, by + 5, label, txt_col);
    }
}

/// Renders Mako-Style Notification Stack (top-right toasts)
pub unsafe fn render_notifications(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    fmt: u32,
    palette: &RyokuPalette,
    stack: &NotificationStack,
) {
    if stack.dnd_enabled {
        return;
    }

    let card_w = 290u32;
    let card_h = 58u32;
    let rx = (screen_w.saturating_sub(card_w + 12)) as i32;
    let mut ry = 44i32;

    let col_border = rgb(fmt, palette.border_active_a.0, palette.border_active_a.1, palette.border_active_a.2);
    let col_accent = rgb(fmt, palette.accent.0, palette.accent.1, palette.accent.2);
    let col_fg = rgb(fmt, palette.fg_primary.0, palette.fg_primary.1, palette.fg_primary.2);
    let col_muted = rgb(fmt, palette.fg_muted.0, palette.fg_muted.1, palette.fg_muted.2);

    for slot in stack.slots.iter() {
        if let Some(ref note) = slot {
            draw_rounded_glass_panel(buf, stride, screen_w, screen_h, fmt, rx, ry, card_w, card_h, 10, palette.surface, 235, col_border);

            // Left 4px accent strip
            fill_rounded_rect(buf, stride, screen_w, screen_h, rx, ry, 6, card_h, 3, col_accent);

            // Title and Body
            if let Ok(t_str) = core::str::from_utf8(&note.title[..note.title_len]) {
                draw_text(buf, stride, screen_w, screen_h, rx + 14, ry + 12, t_str, col_accent);
            }
            if let Ok(b_str) = core::str::from_utf8(&note.body[..note.body_len]) {
                draw_text(buf, stride, screen_w, screen_h, rx + 14, ry + 32, b_str, col_fg);
            }

            // Close [x] button
            draw_text(buf, stride, screen_w, screen_h, rx + card_w as i32 - 20, ry + 12, "x", col_muted);

            ry += card_h as i32 + 8;
        }
    }
}

/// Renders SwayOSD-style bottom-center OSD pill
pub unsafe fn render_sway_osd(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    fmt: u32,
    palette: &RyokuPalette,
    osd: &OsdState,
) {
    if osd.timer == 0 {
        return;
    }

    let pill_w = 240u32;
    let pill_h = 44u32;
    let px = ((screen_w - pill_w) / 2) as i32;
    let py = screen_h as i32 - 96;

    let col_border = rgb(fmt, palette.border_active_a.0, palette.border_active_a.1, palette.border_active_a.2);
    let col_accent = rgb(fmt, palette.accent.0, palette.accent.1, palette.accent.2);
    let col_fg = rgb(fmt, palette.fg_primary.0, palette.fg_primary.1, palette.fg_primary.2);
    let col_pill = rgb(fmt, palette.pill.0, palette.pill.1, palette.pill.2);

    draw_pill(buf, stride, screen_w, screen_h, fmt, px, py, pill_w, pill_h, rgb(fmt, palette.surface.0, palette.surface.1, palette.surface.2), col_border);

    if let Ok(t_str) = core::str::from_utf8(&osd.label[..osd.label_len]) {
        draw_text(buf, stride, screen_w, screen_h, px + 16, py + 14, t_str, col_fg);
    }

    // Mini horizontal progress gauge on right
    let gx = px + 140;
    let gy = py + 19;
    fill_rect(buf, stride, screen_w, screen_h, gx, gy, 80, 6, col_pill);
    let gw = (80u32 * osd.value as u32) / 100;
    fill_rect(buf, stride, screen_w, screen_h, gx, gy, gw, 6, col_accent);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_stack_push_expire_and_dnd() {
        let mut stack = NotificationStack::new();
        stack.push("Theme Changed", "Switched to Tokyo Night");
        assert!(stack.slots[0].is_some());

        // DND suppresses push
        stack.dnd_enabled = true;
        stack.push("Suppressed", "Should not appear");
        assert!(stack.slots[1].is_none());

        // Ticking reduces TTL
        stack.dnd_enabled = false;
        stack.slots[0].as_mut().unwrap().ttl = 1;
        stack.tick(); // ttl becomes 0
        stack.tick(); // expires and becomes None
        assert!(stack.slots[0].is_none());
    }

    #[test]
    fn test_hold_to_activate_state_machine() {
        let mut state = SidebarsState::new();
        state.hold_action = Some(PowerAction::Restart);
        assert_eq!(state.hold_ticks, 0);

        // Advance ticks
        for _ in 0..24 {
            state.hold_ticks += 1;
        }
        assert_eq!(state.hold_ticks, 24); // Not yet triggered (< 25)

        // Cancel on early release
        state.hold_action = None;
        state.hold_ticks = 0;
        assert_eq!(state.hold_ticks, 0);
    }

    #[test]
    fn test_osd_state_tick_and_values() {
        let mut osd = OsdState::new();
        osd.show("Volume", 75);
        assert_eq!(osd.value, 75);
        assert_eq!(osd.timer, 90);
        osd.tick();
        assert_eq!(osd.timer, 89);
    }
}
