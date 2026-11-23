//! Ryoku 1.0 Authentic Bar Styles, Layout Geometries, Work Area Insets & Rail Popouts.
//!
//! Provides all 7 Ryoku bar styles (Orpheus, QsBar, Sumi, Kairos, Shima, Python, Nomarchy),
//! 5 BarForm layouts (Islands, Full, Fit, Dock, Notch), Top/Bottom positioning, 12h/24h clock,
//! dynamic work-area inset calculation, and 4 rail-attached click popout cards.

extern crate alloc;

use super::ryoku_theme::{
    draw_pill, draw_rounded_glass_panel, draw_rounded_rect_outline, draw_text, fill_rect,
    fill_rounded_rect, measure_text, rgb, RyokuPalette, StrBuf,
};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BarStyle {
    Orpheus, // Liquid pill islands: [力] Launcher | WS [1 ● 3 4] | Title+Clock | Vitals+Net+Vol+Hub
    QsBar,   // Full-color bar respecting BarForm and 5 individual widget toggles
    Sumi,    // Monochrome ink-and-paper rail, hairline border, no loud fills
    Kairos,  // Single minimalist centered floating capsule (320px)
    Shima,   // Top bar + paired bottom floating application dock (44px)
    Python,  // Top bar with live 8-bar animated equalizer/visualizer in vitals pill
    Nomarchy,// Monospace bracketed modules: [力] [1|2|3|4] [HYPR] [CPU:12%] [12:35]
}

impl BarStyle {
    pub const ALL: [BarStyle; 7] = [
        Self::Orpheus,
        Self::QsBar,
        Self::Sumi,
        Self::Kairos,
        Self::Shima,
        Self::Python,
        Self::Nomarchy,
    ];

    pub fn name(&self) -> &'static str {
        match self {
            Self::Orpheus => "Orpheus",
            Self::QsBar => "QsBar",
            Self::Sumi => "Sumi",
            Self::Kairos => "Kairos",
            Self::Shima => "Shima",
            Self::Python => "Python",
            Self::Nomarchy => "Nomarchy",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "orpheus" => Some(Self::Orpheus),
            "qsbar" => Some(Self::QsBar),
            "sumi" => Some(Self::Sumi),
            "kairos" => Some(Self::Kairos),
            "shima" => Some(Self::Shima),
            "python" => Some(Self::Python),
            "nomarchy" => Some(Self::Nomarchy),
            _ => None,
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::Orpheus => Self::QsBar,
            Self::QsBar => Self::Sumi,
            Self::Sumi => Self::Kairos,
            Self::Kairos => Self::Shima,
            Self::Shima => Self::Python,
            Self::Python => Self::Nomarchy,
            Self::Nomarchy => Self::Orpheus,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BarForm {
    Islands, // Separated floating pills with outer margin & island gaps
    Full,    // Full-width edge-to-edge bar (0px margin)
    Fit,     // Single centered floating capsule fitted tightly to widgets
    Dock,    // Floating dock-style capsule with quick launchers
    Notch,   // Dynamic notch connected flush to screen edge
}

impl BarForm {
    pub const ALL: [BarForm; 5] = [
        Self::Islands,
        Self::Full,
        Self::Fit,
        Self::Dock,
        Self::Notch,
    ];

    pub fn name(&self) -> &'static str {
        match self {
            Self::Islands => "Islands",
            Self::Full => "Full",
            Self::Fit => "Fit",
            Self::Dock => "Dock",
            Self::Notch => "Notch",
        }
    }

    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "islands" => Some(Self::Islands),
            "full" => Some(Self::Full),
            "fit" => Some(Self::Fit),
            "dock" => Some(Self::Dock),
            "notch" => Some(Self::Notch),
            _ => None,
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::Islands => Self::Full,
            Self::Full => Self::Fit,
            Self::Fit => Self::Dock,
            Self::Dock => Self::Notch,
            Self::Notch => Self::Islands,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BarPosition {
    Top,
    Bottom,
}

impl BarPosition {
    pub fn next(&self) -> Self {
        match self {
            Self::Top => Self::Bottom,
            Self::Bottom => Self::Top,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BarPopout {
    None,
    Clock,
    Vitals,
    Connections,
    Audio,
}

#[derive(Clone, Copy, Debug)]
pub struct BarConfig {
    pub style: BarStyle,
    pub form: BarForm,
    pub position: BarPosition,
    pub popout: BarPopout,
    pub clock_24h: bool,
    pub show_workspaces: bool,
    pub show_clock: bool,
    pub show_vitals: bool,
    pub show_net: bool,
    pub show_audio: bool,
}

impl Default for BarConfig {
    fn default() -> Self {
        Self {
            style: BarStyle::Orpheus,
            form: BarForm::Islands,
            position: BarPosition::Top,
            popout: BarPopout::None,
            clock_24h: false,
            show_workspaces: true,
            show_clock: true,
            show_vitals: true,
            show_net: true,
            show_audio: true,
        }
    }
}

/// Computes the usable workspace desktop rectangle (wx, wy, ww, wh) so windows never overlap bars or docks
pub fn compute_work_area(
    screen_w: u32,
    screen_h: u32,
    bar_style: BarStyle,
    bar_form: BarForm,
    bar_pos: BarPosition,
) -> (i32, i32, u32, u32) {
    let bar_h: u32 = match bar_form {
        BarForm::Full => 32,
        BarForm::Islands => 36,
        BarForm::Fit => 36,
        BarForm::Dock => 44,
        BarForm::Notch => 34,
    };

    let top_inset = if bar_pos == BarPosition::Top { bar_h + 4 } else { 4 };
    let mut bottom_inset = if bar_pos == BarPosition::Bottom { bar_h + 4 } else { 4 };

    // Shima style also has a 44px paired bottom floating application dock
    if bar_style == BarStyle::Shima {
        bottom_inset += 52;
    }

    let wx = 6i32;
    let wy = top_inset as i32;
    let ww = screen_w.saturating_sub(12);
    let wh = screen_h.saturating_sub(top_inset + bottom_inset);

    (wx, wy, ww, wh)
}

/// Formats current time into a string buffer respecting 12h / 24h mode
pub fn format_bar_time(buf: &mut [u8; 16], hours_utc: u8, minutes_utc: u8, clock_24h: bool) -> usize {
    let mut p = 0;
    if clock_24h {
        buf[p] = b'0' + (hours_utc / 10);
        buf[p + 1] = b'0' + (hours_utc % 10);
        buf[p + 2] = b':';
        buf[p + 3] = b'0' + (minutes_utc / 10);
        buf[p + 4] = b'0' + (minutes_utc % 10);
        5
    } else {
        let (h12, ampm) = if hours_utc == 0 {
            (12, b"AM")
        } else if hours_utc < 12 {
            (hours_utc, b"AM")
        } else if hours_utc == 12 {
            (12, b"PM")
        } else {
            (hours_utc - 12, b"PM")
        };
        if h12 >= 10 {
            buf[p] = b'1';
            buf[p + 1] = b'0' + (h12 - 10);
            p += 2;
        } else {
            buf[p] = b'0' + h12;
            p += 1;
        }
        buf[p] = b':';
        buf[p + 1] = b'0' + (minutes_utc / 10);
        buf[p + 2] = b'0' + (minutes_utc % 10);
        buf[p + 3] = b' ';
        buf[p + 4] = ampm[0];
        buf[p + 5] = ampm[1];
        p + 6
    }
}

/// Actions dispatched by clicking bar items
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum BarClickAction {
    None,
    OpenLauncher,
    SwitchWorkspace(usize),
    CycleWm,
    ToggleClockPopout,
    ToggleVitalsPopout,
    ToggleNetPopout,
    ToggleAudioPopout,
    OpenCapture,
    OpenHub,
    OpenLock,
    ToggleControlsSidebar,
    LaunchApp(usize), // WIN_TERMINAL..WIN_AI
}

/// Dispatches clicks inside the top/bottom bar or Shima dock
pub fn handle_bar_click_event(
    px: i32,
    py: i32,
    screen_w: u32,
    screen_h: u32,
    cfg: &BarConfig,
) -> BarClickAction {
    let bar_y = if cfg.position == BarPosition::Top { 4 } else { screen_h as i32 - 40 };

    // Check Shima bottom dock click
    if cfg.style == BarStyle::Shima {
        let dock_y = screen_h as i32 - 48;
        if py >= dock_y && py <= dock_y + 44 {
            let dock_w = 340i32;
            let dock_x = (screen_w as i32 - dock_w) / 2;
            if px >= dock_x && px <= dock_x + dock_w {
                let slot = (px - dock_x - 8) / 52;
                if slot >= 0 && slot < 6 {
                    return BarClickAction::LaunchApp(slot as usize);
                }
            }
        }
    }

    if py < bar_y || py > bar_y + 36 {
        return BarClickAction::None;
    }

    // Left area: Launcher pill & Workspace strip
    if px >= 6 && px <= 46 {
        return BarClickAction::OpenLauncher;
    }
    if px >= 52 && px <= 140 {
        let ws = ((px - 52) / 22) as usize + 1;
        if ws >= 1 && ws <= 4 {
            return BarClickAction::SwitchWorkspace(ws);
        }
    }
    if px >= 144 && px <= 204 {
        return BarClickAction::CycleWm;
    }

    // Center area: Clock / Title
    let cx = screen_w as i32 / 2;
    if px >= cx - 90 && px <= cx + 90 {
        return BarClickAction::ToggleClockPopout;
    }

    // Right area: Vitals | Net | Audio | Shot | Hub | Lock | Controls
    let rx = screen_w as i32 - 6;
    if px >= rx - 36 && px <= rx {
        return BarClickAction::ToggleControlsSidebar;
    }
    if px >= rx - 70 && px <= rx - 40 {
        return BarClickAction::OpenLock;
    }
    if px >= rx - 106 && px <= rx - 74 {
        return BarClickAction::OpenHub;
    }
    if px >= rx - 146 && px <= rx - 110 {
        return BarClickAction::OpenCapture;
    }
    if px >= rx - 186 && px <= rx - 150 {
        return BarClickAction::ToggleAudioPopout;
    }
    if px >= rx - 230 && px <= rx - 190 {
        return BarClickAction::ToggleNetPopout;
    }
    if px >= rx - 350 && px <= rx - 234 {
        return BarClickAction::ToggleVitalsPopout;
    }

    BarClickAction::None
}

/// Renders the active Ryoku bar (and paired Shima dock if active)
pub unsafe fn render_ryoku_bar(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    fmt: u32,
    palette: &RyokuPalette,
    cfg: &BarConfig,
    active_ws: usize,
    busy_workspaces: [bool; 4],
    active_title: &str,
    wm_mode_name: &str,
    cpu_percent: u32,
    free_ram_mib: u64,
    hours_utc: u8,
    minutes_utc: u8,
    frame_count: u64,
) {
    let col_frame = rgb(fmt, palette.frame_bg.0, palette.frame_bg.1, palette.frame_bg.2);
    let col_accent = rgb(fmt, palette.accent.0, palette.accent.1, palette.accent.2);
    let col_fg = rgb(fmt, palette.fg_primary.0, palette.fg_primary.1, palette.fg_primary.2);
    let col_muted = rgb(fmt, palette.fg_muted.0, palette.fg_muted.1, palette.fg_muted.2);
    let col_border = rgb(fmt, palette.border_inactive.0, palette.border_inactive.1, palette.border_inactive.2);

    let bar_y = if cfg.position == BarPosition::Top { 4 } else { screen_h as i32 - 36 };

    // Format current clock text
    let mut time_buf = [0u8; 16];
    let time_len = format_bar_time(&mut time_buf, hours_utc, minutes_utc, cfg.clock_24h);
    let time_str = core::str::from_utf8(&time_buf[..time_len]).unwrap_or("12:00");

    match cfg.style {
        BarStyle::Nomarchy => {
            // Utilitarian monospace bar
            fill_rounded_rect(buf, stride, screen_w, screen_h, 4, bar_y, screen_w - 8, 30, 2, col_frame);
            draw_rounded_rect_outline(buf, stride, screen_w, screen_h, 4, bar_y, screen_w - 8, 30, 2, col_border);

            draw_text(buf, stride, screen_w, screen_h, 12, bar_y + 7, "[力] [", col_accent);
            let mut ws_x = 12 + measure_text("[力] [") as i32;
            for i in 1..=4 {
                let sym = if i == active_ws { b'*' } else { b'0' + (i as u8) };
                let col = if i == active_ws { col_accent } else { col_fg };
                crate::ui::ryoku_theme::draw_char(buf, stride, screen_w, screen_h, ws_x, bar_y + 7, sym, col);
                ws_x += 10;
                if i < 4 {
                    crate::ui::ryoku_theme::draw_char(buf, stride, screen_w, screen_h, ws_x, bar_y + 7, b'|', col_muted);
                    ws_x += 8;
                }
            }
            draw_text(buf, stride, screen_w, screen_h, ws_x, bar_y + 7, "]", col_accent);

            draw_text(buf, stride, screen_w, screen_h, ws_x + 16, bar_y + 7, wm_mode_name, col_muted);

            // Center: title
            let cx = (screen_w / 2) as i32;
            let title_w = measure_text(active_title) as i32;
            draw_text(buf, stride, screen_w, screen_h, cx - title_w / 2, bar_y + 7, active_title, col_fg);

            // Right: telemetry & time
            let v_str = StrBuf::new().push_str("CPU:").push_u64(cpu_percent as u64).push_str("% RAM:").push_u64(free_ram_mib).push_str("M");
            let rx = (screen_w - 12) as i32;
            let time_w = measure_text(time_str) as i32;
            draw_text(buf, stride, screen_w, screen_h, rx - time_w, bar_y + 7, time_str, col_accent);
            let v_w = measure_text(&v_str) as i32;
            draw_text(buf, stride, screen_w, screen_h, rx - time_w - v_w - 16, bar_y + 7, &v_str, col_fg);
        }
        BarStyle::Kairos => {
            // Minimalist centered 340px capsule
            let bar_w = 340u32;
            let bar_x = ((screen_w - bar_w) / 2) as i32;
            draw_pill(buf, stride, screen_w, screen_h, fmt, bar_x, bar_y, bar_w, 32, col_frame, col_border);
            draw_text(buf, stride, screen_w, screen_h, bar_x + 16, bar_y + 8, "力", col_accent);
            let ws_str = StrBuf::new().push_str("WS:").push_u64(active_ws as u64);
            draw_text(buf, stride, screen_w, screen_h, bar_x + 48, bar_y + 8, &ws_str, col_fg);
            draw_text(buf, stride, screen_w, screen_h, bar_x + 120, bar_y + 8, time_str, col_accent);
            let vit_str = StrBuf::new().push_u64(cpu_percent as u64).push_str("%");
            draw_text(buf, stride, screen_w, screen_h, bar_x + 220, bar_y + 8, &vit_str, col_muted);
            draw_text(buf, stride, screen_w, screen_h, bar_x + 280, bar_y + 8, "[HUB]", col_fg);
        }
        _ => {
            // Orpheus, QsBar, Sumi, Shima, Python: 3-island layout
            let center_w = 220u32;
            let right_w = 360u32;

            let lx = 6i32;
            let cx = ((screen_w - center_w) / 2) as i32;
            let rx = (screen_w - right_w - 6) as i32;

            // 1. Left Island: [力] Launcher | Workspace Strip | WM Pill
            draw_pill(buf, stride, screen_w, screen_h, fmt, lx, bar_y, 34, 30, col_frame, col_border);
            draw_text(buf, stride, screen_w, screen_h, lx + 12, bar_y + 7, "力", col_accent);

            // Workspace Strip [1 ● 3 4]
            draw_pill(buf, stride, screen_w, screen_h, fmt, lx + 40, bar_y, 90, 30, col_frame, col_border);
            for i in 1..=4 {
                let wx = lx + 44 + (i as i32 - 1) * 21;
                if i == active_ws {
                    draw_pill(buf, stride, screen_w, screen_h, fmt, wx, bar_y + 4, 18, 22, col_accent, 0);
                    let c = b'0' + (i as u8);
                    crate::ui::ryoku_theme::draw_char(buf, stride, screen_w, screen_h, wx + 5, bar_y + 7, c, rgb(fmt, 16, 16, 20));
                } else if busy_workspaces[i - 1] {
                    // Filled circle ● for busy workspace
                    crate::ui::ryoku_theme::draw_char(buf, stride, screen_w, screen_h, wx + 5, bar_y + 7, b'*', col_fg);
                } else {
                    // Dot · for empty workspace
                    crate::ui::ryoku_theme::draw_char(buf, stride, screen_w, screen_h, wx + 5, bar_y + 7, b'.', col_muted);
                }
            }

            // WM pill
            draw_pill(buf, stride, screen_w, screen_h, fmt, lx + 136, bar_y, 60, 30, col_frame, col_border);
            draw_text(buf, stride, screen_w, screen_h, lx + 144, bar_y + 7, wm_mode_name, col_muted);

            // 2. Center Island: Window Title + Time
            draw_pill(buf, stride, screen_w, screen_h, fmt, cx, bar_y, center_w, 30, col_frame, col_border);
            let t_slice = if active_title.len() > 14 { &active_title[..14] } else { active_title };
            draw_text(buf, stride, screen_w, screen_h, cx + 12, bar_y + 7, t_slice, col_fg);
            draw_text(buf, stride, screen_w, screen_h, cx + center_w as i32 - (time_len as i32 * 8) - 12, bar_y + 7, time_str, col_accent);

            // 3. Right Island: Vitals + Net + Audio + Shot + Hub + Lock + Controls
            draw_pill(buf, stride, screen_w, screen_h, fmt, rx, bar_y, right_w, 30, col_frame, col_border);

            // Python style: live 8-bar equalizer visualizer inside vitals
            if cfg.style == BarStyle::Python {
                let eq_x = rx + 10;
                for b in 0..8 {
                    let phase = ((frame_count / 3 + b as u64 * 2) % 12) as u32;
                    let bar_h = 3 + phase;
                    fill_rect(buf, stride, screen_w, screen_h, eq_x + b as i32 * 4, bar_y + 22 - bar_h as i32, 2, bar_h, col_accent);
                }
            }

            let vit_str = StrBuf::new().push_u64(cpu_percent as u64).push_str("%|").push_u64(free_ram_mib).push_str("M");
            let vit_off = if cfg.style == BarStyle::Python { rx + 48 } else { rx + 12 };
            draw_text(buf, stride, screen_w, screen_h, vit_off, bar_y + 7, &vit_str, col_fg);

            draw_text(buf, stride, screen_w, screen_h, rx + 144, bar_y + 7, "NET", col_muted);
            draw_text(buf, stride, screen_w, screen_h, rx + 180, bar_y + 7, "VOL", col_muted);
            draw_text(buf, stride, screen_w, screen_h, rx + 216, bar_y + 7, "SHOT", col_fg);
            draw_text(buf, stride, screen_w, screen_h, rx + 260, bar_y + 7, "HUB", col_accent);
            draw_text(buf, stride, screen_w, screen_h, rx + 296, bar_y + 7, "LOCK", col_muted);
            draw_text(buf, stride, screen_w, screen_h, rx + 338, bar_y + 7, "=", col_accent);
        }
    }

    // Shima Paired Floating Application Dock (44px)
    if cfg.style == BarStyle::Shima {
        let dock_w = 340u32;
        let dock_x = ((screen_w - dock_w) / 2) as i32;
        let dock_y = screen_h as i32 - 48;
        draw_rounded_glass_panel(buf, stride, screen_w, screen_h, fmt, dock_x, dock_y, dock_w, 42, 12, palette.surface, 220, col_border);

        let apps = ["Term", "Sys", "Files", "Edit", "Hub", "AI"];
        for (i, name) in apps.iter().enumerate() {
            let ax = dock_x + 12 + (i as i32 * 54);
            draw_pill(buf, stride, screen_w, screen_h, fmt, ax, dock_y + 6, 46, 30, col_frame, col_border);
            draw_text(buf, stride, screen_w, screen_h, ax + 8, dock_y + 13, name, col_fg);
        }
    }
}

/// Renders the open popout card (Clock, Vitals, Connections, Audio)
pub unsafe fn render_ryoku_popout(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    fmt: u32,
    palette: &RyokuPalette,
    cfg: &BarConfig,
    cpu_percent: u32,
    free_ram_mib: u64,
    total_ram_mib: u64,
    heap_used_bytes: usize,
    heap_total_bytes: usize,
    hours_utc: u8,
    minutes_utc: u8,
    uptime_sec: u64,
    irq_count: u64,
    audio_muted: bool,
) {
    if cfg.popout == BarPopout::None {
        return;
    }

    let pop_w = 300u32;
    let pop_h = 160u32;
    let pop_y = if cfg.position == BarPosition::Top { 44 } else { screen_h as i32 - pop_h as i32 - 44 };

    let col_border = rgb(fmt, palette.border_active_a.0, palette.border_active_a.1, palette.border_active_a.2);
    let col_fg = rgb(fmt, palette.fg_primary.0, palette.fg_primary.1, palette.fg_primary.2);
    let col_muted = rgb(fmt, palette.fg_muted.0, palette.fg_muted.1, palette.fg_muted.2);
    let col_accent = rgb(fmt, palette.accent.0, palette.accent.1, palette.accent.2);
    let col_pill = rgb(fmt, palette.pill.0, palette.pill.1, palette.pill.2);

    match cfg.popout {
        BarPopout::Clock => {
            let pop_x = ((screen_w - pop_w) / 2) as i32;
            draw_rounded_glass_panel(buf, stride, screen_w, screen_h, fmt, pop_x, pop_y, pop_w, pop_h, 12, palette.surface, 235, col_border);

            let mut time_buf = [0u8; 16];
            let t_len = format_bar_time(&mut time_buf, hours_utc, minutes_utc, cfg.clock_24h);
            let t_str = core::str::from_utf8(&time_buf[..t_len]).unwrap_or("12:00");
            draw_text(buf, stride, screen_w, screen_h, pop_x + 20, pop_y + 16, t_str, col_accent);

            let up_str = StrBuf::new().push_str("Uptime: ").push_pad2(uptime_sec / 60).push_str(":").push_pad2(uptime_sec % 60);
            draw_text(buf, stride, screen_w, screen_h, pop_x + 20, pop_y + 40, &up_str, col_muted);
            draw_text(buf, stride, screen_w, screen_h, pop_x + 20, pop_y + 60, "Calendar: 2026-11-23 UTC", col_fg);

            // [Switch 12h / 24h] button
            let btn_mode = if cfg.clock_24h { "[ Switch to 12h ]" } else { "[ Switch to 24h ]" };
            draw_pill(buf, stride, screen_w, screen_h, fmt, pop_x + 20, pop_y + 105, 170, 32, col_pill, col_border);
            draw_text(buf, stride, screen_w, screen_h, pop_x + 30, pop_y + 113, btn_mode, col_fg);
        }
        BarPopout::Vitals => {
            let pop_x = (screen_w.saturating_sub(pop_w + 10)) as i32;
            draw_rounded_glass_panel(buf, stride, screen_w, screen_h, fmt, pop_x, pop_y, pop_w, pop_h, 12, palette.surface, 235, col_border);

            draw_text(buf, stride, screen_w, screen_h, pop_x + 16, pop_y + 14, "System Vitals", col_accent);
            let cpu_str = StrBuf::new().push_str("CPU Usage: ").push_u64(cpu_percent as u64).push_str("%");
            draw_text(buf, stride, screen_w, screen_h, pop_x + 16, pop_y + 36, &cpu_str, col_fg);

            let ram_str = StrBuf::new().push_str("RAM: ").push_u64(total_ram_mib.saturating_sub(free_ram_mib)).push_str("M / ").push_u64(total_ram_mib).push_str("M");
            draw_text(buf, stride, screen_w, screen_h, pop_x + 16, pop_y + 56, &ram_str, col_fg);

            let heap_str = StrBuf::new().push_str("Heap: ").push_usize(heap_used_bytes / 1024).push_str("K / ").push_usize(heap_total_bytes / 1024).push_str("K");
            draw_text(buf, stride, screen_w, screen_h, pop_x + 16, pop_y + 76, &heap_str, col_muted);

            let irq_str = StrBuf::new().push_str("IRQs Serviced: ").push_u64(irq_count as u64);
            draw_text(buf, stride, screen_w, screen_h, pop_x + 16, pop_y + 96, &irq_str, col_muted);

            draw_pill(buf, stride, screen_w, screen_h, fmt, pop_x + 16, pop_y + 118, 180, 28, col_pill, col_border);
            draw_text(buf, stride, screen_w, screen_h, pop_x + 24, pop_y + 124, "Open Full Vitals", col_fg);
        }
        BarPopout::Connections => {
            let pop_x = (screen_w.saturating_sub(pop_w + 40)) as i32;
            draw_rounded_glass_panel(buf, stride, screen_w, screen_h, fmt, pop_x, pop_y, pop_w, pop_h, 12, palette.surface, 235, col_border);

            draw_text(buf, stride, screen_w, screen_h, pop_x + 16, pop_y + 14, "Network Connections", col_accent);
            draw_text(buf, stride, screen_w, screen_h, pop_x + 16, pop_y + 36, "Active: VirtIO-Net (net0)", col_fg);
            draw_text(buf, stride, screen_w, screen_h, pop_x + 16, pop_y + 56, "IP: 10.0.2.15 (DHCP)", col_muted);
            draw_text(buf, stride, screen_w, screen_h, pop_x + 16, pop_y + 76, "MAC: 52:54:00:12:34:56", col_muted);
            draw_text(buf, stride, screen_w, screen_h, pop_x + 16, pop_y + 96, "State: Carrier UP", col_accent);

            draw_pill(buf, stride, screen_w, screen_h, fmt, pop_x + 16, pop_y + 118, 180, 28, col_pill, col_border);
            draw_text(buf, stride, screen_w, screen_h, pop_x + 24, pop_y + 124, "Switch to e1000e", col_fg);
        }
        BarPopout::Audio => {
            let pop_x = (screen_w.saturating_sub(pop_w + 60)) as i32;
            draw_rounded_glass_panel(buf, stride, screen_w, screen_h, fmt, pop_x, pop_y, pop_w, pop_h, 12, palette.surface, 235, col_border);

            draw_text(buf, stride, screen_w, screen_h, pop_x + 16, pop_y + 14, "VirtIO-Sound Audio", col_accent);
            draw_text(buf, stride, screen_w, screen_h, pop_x + 16, pop_y + 36, "Output: Stereo 48 kHz", col_fg);
            let mute_str = if audio_muted { "Muted: YES" } else { "Muted: NO (Active)" };
            draw_text(buf, stride, screen_w, screen_h, pop_x + 16, pop_y + 56, mute_str, col_muted);

            // Buttons: [Toggle Mute] and [Play 440Hz Chime]
            draw_pill(buf, stride, screen_w, screen_h, fmt, pop_x + 16, pop_y + 88, 120, 28, col_pill, col_border);
            draw_text(buf, stride, screen_w, screen_h, pop_x + 24, pop_y + 94, if audio_muted { "Unmute" } else { "Mute" }, col_fg);

            draw_pill(buf, stride, screen_w, screen_h, fmt, pop_x + 148, pop_y + 88, 136, 28, col_pill, col_border);
            draw_text(buf, stride, screen_w, screen_h, pop_x + 156, pop_y + 94, "Play Chime", col_accent);
        }
        BarPopout::None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_bar_styles_and_forms() {
        assert_eq!(BarStyle::ALL.len(), 7);
        assert_eq!(BarForm::ALL.len(), 5);
        for s in &BarStyle::ALL {
            assert!(!s.name().is_empty());
            assert_eq!(BarStyle::from_slug(s.name().to_ascii_lowercase().as_str()), Some(*s));
        }
    }

    #[test]
    fn test_work_area_clearance_and_insets() {
        // Top full bar
        let (wx, wy, ww, wh) = compute_work_area(1440, 900, BarStyle::Orpheus, BarForm::Full, BarPosition::Top);
        assert_eq!(wx, 6);
        assert_eq!(wy, 36);
        assert!(wh < 900);
        assert_eq!(ww, 1440 - 12);

        // Shima style extra bottom inset
        let (_, _, _, wh_shima) = compute_work_area(1440, 900, BarStyle::Shima, BarForm::Islands, BarPosition::Top);
        let (_, _, _, wh_orpheus) = compute_work_area(1440, 900, BarStyle::Orpheus, BarForm::Islands, BarPosition::Top);
        assert!(wh_shima < wh_orpheus); // Shima has additional dock clearance
    }

    #[test]
    fn test_bar_time_formatting_12h_and_24h() {
        let mut buf = [0u8; 16];

        // 14:05 in 24h -> "14:05"
        let len24 = format_bar_time(&mut buf, 14, 5, true);
        assert_eq!(core::str::from_utf8(&buf[..len24]).unwrap(), "14:05");

        // 14:05 in 12h -> "2:05 PM"
        let len12 = format_bar_time(&mut buf, 14, 5, false);
        assert_eq!(core::str::from_utf8(&buf[..len12]).unwrap(), "2:05 PM");

        // 00:30 in 12h -> "12:30 AM"
        let len_mid = format_bar_time(&mut buf, 0, 30, false);
        assert_eq!(core::str::from_utf8(&buf[..len_mid]).unwrap(), "12:30 AM");
    }
}
