//! Ryoku 1.0 Amaterasu ("力と美のために") Theme Engine & Fast 2D Rendering Primitives.
//!
//! Provides all 19 official Ryoku themes, 16-color ANSI terminal palette generation,
//! 4 procedural Bayer-dithered wallpapers, Matugen wallpaper-driven retinting,
//! Night Light warm color temperature filter, and NEON-vectorizable 2D drawing primitives.

extern crate alloc;

use crate::drivers::fb::{get_glyph, rgb_format};

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum RyokuThemeId {
    // Dark Themes (16)
    Catppuccin,
    Ethereal,
    Everforest,
    Gruvbox,
    Hackerman,
    Kanagawa,
    Lumon,
    MatteBlack,
    Miasma,
    Nord,
    OsakaJade,
    Retro82,
    Ristretto,
    RosePine,
    TokyoNight, // DEFAULT
    Vantablack,
    // Light Themes (3)
    CatppuccinLatte,
    FlexokiLight,
    White,
}

impl RyokuThemeId {
    pub const ALL: [RyokuThemeId; 19] = [
        RyokuThemeId::TokyoNight,
        RyokuThemeId::Catppuccin,
        RyokuThemeId::Ethereal,
        RyokuThemeId::Everforest,
        RyokuThemeId::Gruvbox,
        RyokuThemeId::Hackerman,
        RyokuThemeId::Kanagawa,
        RyokuThemeId::Lumon,
        RyokuThemeId::MatteBlack,
        RyokuThemeId::Miasma,
        RyokuThemeId::Nord,
        RyokuThemeId::OsakaJade,
        RyokuThemeId::Retro82,
        RyokuThemeId::Ristretto,
        RyokuThemeId::RosePine,
        RyokuThemeId::Vantablack,
        RyokuThemeId::CatppuccinLatte,
        RyokuThemeId::FlexokiLight,
        RyokuThemeId::White,
    ];

    pub fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "catppuccin" => Some(Self::Catppuccin),
            "ethereal" => Some(Self::Ethereal),
            "everforest" => Some(Self::Everforest),
            "gruvbox" => Some(Self::Gruvbox),
            "hackerman" => Some(Self::Hackerman),
            "kanagawa" => Some(Self::Kanagawa),
            "lumon" => Some(Self::Lumon),
            "matte-black" => Some(Self::MatteBlack),
            "miasma" => Some(Self::Miasma),
            "nord" => Some(Self::Nord),
            "osaka-jade" => Some(Self::OsakaJade),
            "retro-82" => Some(Self::Retro82),
            "ristretto" => Some(Self::Ristretto),
            "rose-pine" => Some(Self::RosePine),
            "tokyo-night" => Some(Self::TokyoNight),
            "vantablack" => Some(Self::Vantablack),
            "catppuccin-latte" => Some(Self::CatppuccinLatte),
            "flexoki-light" => Some(Self::FlexokiLight),
            "white" => Some(Self::White),
            _ => None,
        }
    }

    pub fn next(&self) -> Self {
        let mut idx = 0;
        while idx < Self::ALL.len() {
            if Self::ALL[idx] == *self {
                return Self::ALL[(idx + 1) % Self::ALL.len()];
            }
            idx += 1;
        }
        Self::TokyoNight
    }

    pub fn palette(&self) -> RyokuPalette {
        match self {
            Self::Catppuccin => RyokuPalette::new(
                *self,
                "Catppuccin Mocha",
                "catppuccin",
                false,
                ((17, 17, 27), (30, 30, 46), (49, 50, 68)),
                (24, 24, 37),
                (30, 30, 46),
                (49, 50, 68),
                (205, 214, 244),
                (166, 173, 200),
                (203, 166, 247),
                (203, 166, 247),
                (137, 180, 250),
                (69, 71, 90),
            ),
            Self::Ethereal => RyokuPalette::new(
                *self,
                "Ethereal",
                "ethereal",
                false,
                ((10, 14, 26), (22, 30, 52), (16, 58, 72)),
                (14, 19, 34),
                (20, 27, 46),
                (34, 44, 72),
                (228, 236, 250),
                (140, 160, 195),
                (125, 211, 252),
                (125, 211, 252),
                (192, 132, 252),
                (45, 55, 85),
            ),
            Self::Everforest => RyokuPalette::new(
                *self,
                "Everforest",
                "everforest",
                false,
                ((35, 42, 46), (45, 53, 59), (52, 63, 68)),
                (39, 46, 51),
                (45, 53, 59),
                (61, 72, 77),
                (211, 198, 170),
                (133, 146, 137),
                (167, 192, 128),
                (167, 192, 128),
                (131, 192, 146),
                (76, 86, 92),
            ),
            Self::Gruvbox => RyokuPalette::new(
                *self,
                "Gruvbox Dark",
                "gruvbox",
                false,
                ((29, 32, 33), (40, 40, 40), (60, 56, 54)),
                (29, 32, 33),
                (40, 40, 40),
                (60, 56, 54),
                (235, 219, 178),
                (168, 153, 132),
                (254, 128, 25),
                (254, 128, 25),
                (250, 189, 47),
                (80, 73, 69),
            ),
            Self::Hackerman => RyokuPalette::new(
                *self,
                "Hackerman",
                "hackerman",
                false,
                ((5, 9, 13), (9, 16, 22), (8, 36, 32)),
                (7, 11, 15),
                (11, 17, 24),
                (18, 29, 38),
                (208, 247, 232),
                (98, 148, 136),
                (0, 255, 156),
                (0, 255, 156),
                (0, 212, 255),
                (26, 44, 48),
            ),
            Self::Kanagawa => RyokuPalette::new(
                *self,
                "Kanagawa",
                "kanagawa",
                false,
                ((22, 22, 29), (31, 31, 40), (42, 42, 55)),
                (22, 22, 29),
                (31, 31, 40),
                (54, 54, 70),
                (220, 215, 186),
                (114, 113, 105),
                (126, 156, 216),
                (126, 156, 216),
                (149, 127, 184),
                (62, 62, 78),
            ),
            Self::Lumon => RyokuPalette::new(
                *self,
                "Lumon Industries",
                "lumon",
                false,
                ((12, 18, 24), (18, 28, 36), (22, 42, 50)),
                (14, 22, 28),
                (20, 30, 38),
                (32, 48, 58),
                (226, 240, 242),
                (132, 162, 172),
                (102, 217, 232),
                (102, 217, 232),
                (56, 189, 248),
                (42, 60, 72),
            ),
            Self::MatteBlack => RyokuPalette::new(
                *self,
                "Matte Black",
                "matte-black",
                false,
                ((10, 10, 12), (18, 18, 21), (28, 28, 32)),
                (12, 12, 14),
                (18, 18, 20),
                (32, 32, 36),
                (234, 230, 223),
                (145, 145, 152),
                (228, 228, 231),
                (228, 228, 231),
                (113, 113, 122),
                (48, 48, 54),
            ),
            Self::Miasma => RyokuPalette::new(
                *self,
                "Miasma",
                "miasma",
                false,
                ((24, 22, 20), (34, 34, 30), (42, 52, 38)),
                (28, 27, 24),
                (34, 34, 30),
                (52, 54, 46),
                (215, 209, 188),
                (144, 142, 122),
                (120, 168, 108),
                (120, 168, 108),
                (201, 165, 84),
                (68, 66, 56),
            ),
            Self::Nord => RyokuPalette::new(
                *self,
                "Nord",
                "nord",
                false,
                ((36, 41, 51), (46, 52, 64), (59, 66, 82)),
                (46, 52, 64),
                (59, 66, 82),
                (67, 76, 94),
                (236, 239, 244),
                (165, 175, 192),
                (136, 192, 208),
                (136, 192, 208),
                (129, 161, 193),
                (76, 86, 106),
            ),
            Self::OsakaJade => RyokuPalette::new(
                *self,
                "Osaka Jade",
                "osaka-jade",
                false,
                ((14, 22, 20), (20, 34, 30), (16, 58, 46)),
                (16, 26, 23),
                (22, 36, 32),
                (34, 54, 48),
                (224, 238, 232),
                (130, 164, 152),
                (64, 206, 154),
                (64, 206, 154),
                (52, 211, 153),
                (42, 68, 60),
            ),
            Self::Retro82 => RyokuPalette::new(
                *self,
                "Retro '82",
                "retro-82",
                false,
                ((20, 12, 32), (42, 18, 56), (72, 22, 74)),
                (24, 14, 38),
                (34, 20, 52),
                (56, 30, 82),
                (248, 232, 248),
                (182, 144, 196),
                (255, 94, 186),
                (255, 94, 186),
                (88, 224, 255),
                (78, 38, 96),
            ),
            Self::Ristretto => RyokuPalette::new(
                *self,
                "Ristretto",
                "ristretto",
                false,
                ((34, 27, 26), (44, 37, 37), (64, 50, 48)),
                (36, 29, 29),
                (44, 37, 37),
                (64, 54, 54),
                (230, 217, 211),
                (165, 146, 142),
                (243, 141, 112),
                (243, 141, 112),
                (249, 204, 108),
                (84, 68, 66),
            ),
            Self::RosePine => RyokuPalette::new(
                *self,
                "Rosé Pine",
                "rose-pine",
                false,
                ((25, 23, 36), (31, 29, 46), (38, 35, 58)),
                (25, 23, 36),
                (31, 29, 46),
                (50, 46, 72),
                (224, 222, 244),
                (144, 140, 170),
                (235, 188, 186),
                (235, 188, 186),
                (196, 167, 231),
                (68, 62, 92),
            ),
            Self::TokyoNight => RyokuPalette::new(
                *self,
                "Tokyo Night",
                "tokyo-night",
                false,
                ((18, 20, 32), (26, 27, 38), (36, 40, 59)),
                (22, 23, 34),
                (26, 27, 38),
                (41, 46, 66),
                (192, 202, 245),
                (122, 132, 172),
                (122, 162, 247),
                (122, 162, 247),
                (187, 154, 247),
                (56, 64, 98),
            ),
            Self::Vantablack => RyokuPalette::new(
                *self,
                "Vantablack",
                "vantablack",
                false,
                ((4, 4, 6), (8, 8, 10), (16, 16, 20)),
                (6, 6, 8),
                (12, 12, 15),
                (24, 24, 28),
                (238, 234, 226),
                (136, 136, 142),
                (238, 234, 226),
                (238, 234, 226),
                (96, 96, 104),
                (40, 40, 46),
            ),
            Self::CatppuccinLatte => RyokuPalette::new(
                *self,
                "Catppuccin Latte",
                "catppuccin-latte",
                true,
                ((239, 241, 245), (230, 233, 239), (214, 220, 235)),
                (239, 241, 245),
                (246, 247, 250),
                (220, 224, 232),
                (76, 79, 105),
                (108, 111, 133),
                (136, 57, 239),
                (136, 57, 239),
                (30, 102, 245),
                (188, 192, 204),
            ),
            Self::FlexokiLight => RyokuPalette::new(
                *self,
                "Flexoki Light",
                "flexoki-light",
                true,
                ((255, 252, 240), (242, 240, 229), (230, 228, 217)),
                (242, 240, 229),
                (255, 252, 240),
                (230, 228, 217),
                (16, 15, 15),
                (111, 110, 105),
                (32, 94, 166),
                (32, 94, 166),
                (175, 48, 41),
                (188, 185, 175),
            ),
            Self::White => RyokuPalette::new(
                *self,
                "Pure White",
                "white",
                true,
                ((248, 248, 250), (236, 238, 242), (224, 228, 234)),
                (244, 245, 248),
                (255, 255, 255),
                (230, 232, 236),
                (18, 18, 22),
                (106, 110, 118),
                (37, 99, 235),
                (37, 99, 235),
                (124, 58, 237),
                (200, 204, 212),
            ),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RyokuPalette {
    pub id: RyokuThemeId,
    pub name: &'static str,
    pub slug: &'static str,
    pub is_light: bool,
    pub wall: ((u8, u8, u8), (u8, u8, u8), (u8, u8, u8)),
    pub frame_bg: (u8, u8, u8),
    pub surface: (u8, u8, u8),
    pub pill: (u8, u8, u8),
    pub fg_primary: (u8, u8, u8),
    pub fg_muted: (u8, u8, u8),
    pub accent: (u8, u8, u8),
    pub border_active_a: (u8, u8, u8),
    pub border_active_b: (u8, u8, u8),
    pub border_inactive: (u8, u8, u8),
    pub term_colors: [(u8, u8, u8); 16],
}

impl RyokuPalette {
    pub fn new(
        id: RyokuThemeId,
        name: &'static str,
        slug: &'static str,
        is_light: bool,
        wall: ((u8, u8, u8), (u8, u8, u8), (u8, u8, u8)),
        frame_bg: (u8, u8, u8),
        surface: (u8, u8, u8),
        pill: (u8, u8, u8),
        fg_primary: (u8, u8, u8),
        fg_muted: (u8, u8, u8),
        accent: (u8, u8, u8),
        border_active_a: (u8, u8, u8),
        border_active_b: (u8, u8, u8),
        border_inactive: (u8, u8, u8),
    ) -> Self {
        // Build 16 ANSI terminal colors adapted to this theme palette
        let term_colors = if is_light {
            [
                (30, 30, 34),       // Black
                (210, 45, 55),      // Red
                (40, 160, 80),      // Green
                (195, 130, 20),     // Yellow
                (35, 100, 220),     // Blue
                (160, 50, 210),     // Magenta
                (20, 150, 175),     // Cyan
                (230, 230, 235),    // White
                (100, 105, 115),    // Bright Black
                (235, 75, 85),      // Bright Red
                (60, 190, 100),     // Bright Green
                (225, 160, 40),     // Bright Yellow
                (65, 130, 250),     // Bright Blue
                (190, 80, 240),     // Bright Magenta
                (40, 180, 210),     // Bright Cyan
                (255, 255, 255),    // Bright White
            ]
        } else {
            [
                frame_bg,           // Black
                (247, 118, 142),    // Red
                (158, 206, 106),    // Green
                (224, 175, 104),    // Yellow
                accent,             // Blue / Primary Accent
                border_active_b,    // Magenta / Secondary Accent
                (125, 207, 255),    // Cyan
                fg_primary,         // White
                (86, 95, 137),      // Bright Black
                (255, 130, 150),    // Bright Red
                (185, 230, 120),    // Bright Green
                (245, 195, 120),    // Bright Yellow
                (140, 180, 255),    // Bright Blue
                (210, 175, 255),    // Bright Magenta
                (155, 225, 255),    // Bright Cyan
                (255, 255, 255),    // Bright White
            ]
        };

        Self {
            id,
            name,
            slug,
            is_light,
            wall,
            frame_bg,
            surface,
            pill,
            fg_primary,
            fg_muted,
            accent,
            border_active_a,
            border_active_b,
            border_inactive,
            term_colors,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WallpaperPattern {
    AmaterasuMarble, // Procedural sinusoidal marble veins + Bayer dithering
    SakuraMist,      // Diagonal gradient + glowing mist bands + subtle bokeh
    CyberGrid,       // Perspective horizon cyber grid lines
    MonolithInk,     // Minimalist deep ink with large centered '力' watermark
}

impl WallpaperPattern {
    pub const ALL: [WallpaperPattern; 4] = [
        Self::AmaterasuMarble,
        Self::SakuraMist,
        Self::CyberGrid,
        Self::MonolithInk,
    ];

    pub fn name(&self) -> &'static str {
        match self {
            Self::AmaterasuMarble => "Amaterasu Marble",
            Self::SakuraMist => "Sakura Mist",
            Self::CyberGrid => "Cyber Grid",
            Self::MonolithInk => "Monolith Ink",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::AmaterasuMarble => Self::SakuraMist,
            Self::SakuraMist => Self::CyberGrid,
            Self::CyberGrid => Self::MonolithInk,
            Self::MonolithInk => Self::AmaterasuMarble,
        }
    }
}

// 4x4 Bayer Dithering Matrix (-8..+7) to eliminate color banding
const BAYER_4X4: [[i16; 4]; 4] = [
    [-8, 0, -6, 2],
    [4, -4, 6, -2],
    [-5, 3, -7, 1],
    [7, -1, 5, -3],
];

/// Applies 35% Matugen wallpaper tinting to active accent and borders
pub fn apply_matugen_tint(palette: &mut RyokuPalette, sampled_rgb: (u8, u8, u8)) {
    let blend = |orig: (u8, u8, u8), samp: (u8, u8, u8)| -> (u8, u8, u8) {
        (
            ((orig.0 as u32 * 65 + samp.0 as u32 * 35) / 100) as u8,
            ((orig.1 as u32 * 65 + samp.1 as u32 * 35) / 100) as u8,
            ((orig.2 as u32 * 65 + samp.2 as u32 * 35) / 100) as u8,
        )
    };
    palette.accent = blend(palette.accent, sampled_rgb);
    palette.border_active_a = blend(palette.border_active_a, sampled_rgb);
    palette.border_active_b = blend(palette.border_active_b, sampled_rgb);
}

/// Applies warm Night Light color temperature filter (6500K -> ~3400K)
#[inline]
pub const fn apply_night_light_rgb(rgb: (u8, u8, u8)) -> (u8, u8, u8) {
    let r = rgb.0;
    let g = ((rgb.1 as u16 * 238) / 255) as u8;
    let b = ((rgb.2 as u16 * 182) / 255) as u8;
    (r, g, b)
}

/// Fast non-volatile 64-byte NEON block memory copy
#[inline]
pub unsafe fn fast_copy_buffer(src_va: u64, dst_va: u64, bytes: usize) {
    if src_va == 0 || dst_va == 0 || bytes == 0 {
        return;
    }

    #[cfg(target_arch = "aarch64")]
    {
        let mut src = src_va as *const u8;
        let mut dst = dst_va as *mut u8;
        let mut rem = bytes;

        while rem >= 64 {
            core::arch::asm!(
                "ldp q0, q1, [{src}]",
                "ldp q2, q3, [{src}, #32]",
                "stp q0, q1, [{dst}]",
                "stp q2, q3, [{dst}, #32]",
                src = in(reg) src,
                dst = in(reg) dst,
                out("v0") _,
                out("v1") _,
                out("v2") _,
                out("v3") _,
                options(nostack)
            );
            src = src.add(64);
            dst = dst.add(64);
            rem -= 64;
        }

        while rem >= 8 {
            let val = core::ptr::read_unaligned(src as *const u64);
            core::ptr::write_unaligned(dst as *mut u64, val);
            src = src.add(8);
            dst = dst.add(8);
            rem -= 8;
        }

        if rem > 0 {
            core::ptr::copy_nonoverlapping(src, dst, rem);
        }
    }

    #[cfg(not(target_arch = "aarch64"))]
    {
        core::ptr::copy_nonoverlapping(src_va as *const u8, dst_va as *mut u8, bytes);
    }
}

#[inline(always)]
pub fn rgb(fmt: u32, r: u8, g: u8, b: u8) -> u32 {
    rgb_format(fmt, r, g, b)
}

#[inline(always)]
pub fn unpack_rgb(fmt: u32, color: u32) -> (u8, u8, u8) {
    if fmt == 1 {
        // BGR
        let b = (color & 0xFF) as u8;
        let g = ((color >> 8) & 0xFF) as u8;
        let r = ((color >> 16) & 0xFF) as u8;
        (r, g, b)
    } else {
        // RGB
        let r = (color & 0xFF) as u8;
        let g = ((color >> 8) & 0xFF) as u8;
        let b = ((color >> 16) & 0xFF) as u8;
        (r, g, b)
    }
}

#[inline(always)]
pub fn blend_channel(src: u8, dst: u8, alpha: u8) -> u8 {
    let a = alpha as u32;
    let inv_a = 255 - a;
    ((src as u32 * a + dst as u32 * inv_a + 128) >> 8) as u8
}

#[inline(always)]
pub fn blend_pixel(fmt: u32, dst_color: u32, r: u8, g: u8, b: u8, alpha: u8) -> u32 {
    if alpha == 255 {
        return rgb(fmt, r, g, b);
    }
    if alpha == 0 {
        return dst_color;
    }
    let (dr, dg, db) = unpack_rgb(fmt, dst_color);
    let out_r = blend_channel(r, dr, alpha);
    let out_g = blend_channel(g, dg, alpha);
    let out_b = blend_channel(b, db, alpha);
    rgb(fmt, out_r, out_g, out_b)
}

/// Fast filled rectangle
pub unsafe fn fill_rect(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    color: u32,
) {
    if x >= screen_w as i32 || y >= screen_h as i32 || w == 0 || h == 0 {
        return;
    }
    let x0 = x.max(0) as u32;
    let y0 = y.max(0) as u32;
    let x1 = ((x + w as i32).max(0) as u32).min(screen_w);
    let y1 = ((y + h as i32).max(0) as u32).min(screen_h);
    let count = (x1.saturating_sub(x0)) as usize;
    if count == 0 {
        return;
    }

    let ptr = buf as *mut u32;
    for py in y0..y1 {
        let row_ptr = ptr.add((py as usize) * (stride as usize) + (x0 as usize));
        for px in 0..count {
            *row_ptr.add(px) = color;
        }
    }
}

/// Fast filled rounded rectangle with interior row-fill fast path
pub unsafe fn fill_rounded_rect(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    radius: u32,
    color: u32,
) {
    if x >= screen_w as i32 || y >= screen_h as i32 || w == 0 || h == 0 {
        return;
    }
    let r = radius.min(w / 2).min(h / 2);
    if r <= 1 {
        fill_rect(buf, stride, screen_w, screen_h, x, y, w, h, color);
        return;
    }

    let x0 = x.max(0) as u32;
    let y0 = y.max(0) as u32;
    let x1 = ((x + w as i32).max(0) as u32).min(screen_w);
    let y1 = ((y + h as i32).max(0) as u32).min(screen_h);
    let r_sq = (r * r) as i32;

    let int_y0 = ((y + r as i32).max(0) as u32).min(screen_h);
    let int_y1 = ((y + h as i32 - r as i32).max(0) as u32).min(screen_h);

    let ptr = buf as *mut u32;

    for py in y0..y1 {
        let row_ptr = ptr.add((py as usize) * (stride as usize));

        // Fast middle interior: no corner check needed
        if py >= int_y0 && py < int_y1 {
            let count = (x1.saturating_sub(x0)) as usize;
            for px in 0..count {
                *row_ptr.add((x0 as usize) + px) = color;
            }
            continue;
        }

        // Top or bottom rounded rows: check rounded corner quadrants
        let dy = if py < int_y0 {
            (y + r as i32) - py as i32
        } else {
            py as i32 - (y + h as i32 - 1 - r as i32)
        };
        let dy_sq = dy * dy;

        for px in x0..x1 {
            let dx = if px < (x + r as i32) as u32 {
                (x + r as i32) - px as i32
            } else if px >= ((x + w as i32 - r as i32) as u32) {
                px as i32 - (x + w as i32 - 1 - r as i32)
            } else {
                0
            };

            if dx == 0 || (dx * dx + dy_sq <= r_sq) {
                *row_ptr.add(px as usize) = color;
            }
        }
    }
}

/// Draws an alpha-blended frosted glass panel with drop shadow, specular highlight, and crisp border
pub unsafe fn draw_rounded_glass_panel(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    fmt: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    radius: u32,
    tint_rgb: (u8, u8, u8),
    alpha: u8,
    border_color: u32,
) {
    if x >= screen_w as i32 || y >= screen_h as i32 || w == 0 || h == 0 {
        return;
    }

    let r = radius.min(w / 2).min(h / 2);
    let ptr = buf as *mut u32;

    // 1. Soft Drop Shadow (4px outer fringes on right and bottom edges)
    let right_sh_x0 = (x + w as i32).max(0) as u32;
    let right_sh_x1 = ((x + w as i32 + 4).max(0) as u32).min(screen_w);
    let win_y0 = (y + 4).max(0) as u32;
    let win_y1 = ((y + h as i32 + 4).max(0) as u32).min(screen_h);

    if right_sh_x0 < right_sh_x1 {
        for py in win_y0..win_y1 {
            let row_ptr = ptr.add((py as usize) * (stride as usize));
            for px in right_sh_x0..right_sh_x1 {
                let cur = *row_ptr.add(px as usize);
                *row_ptr.add(px as usize) = blend_pixel(fmt, cur, 0, 0, 0, 60);
            }
        }
    }

    let bot_sh_y0 = (y + h as i32).max(0) as u32;
    let bot_sh_y1 = ((y + h as i32 + 4).max(0) as u32).min(screen_h);
    let win_x0 = (x + 4).max(0) as u32;
    let win_x1 = (x + w as i32).max(0) as u32;

    if bot_sh_y0 < bot_sh_y1 {
        for py in bot_sh_y0..bot_sh_y1 {
            let row_ptr = ptr.add((py as usize) * (stride as usize));
            for px in win_x0..win_x1 {
                let cur = *row_ptr.add(px as usize);
                *row_ptr.add(px as usize) = blend_pixel(fmt, cur, 0, 0, 0, 60);
            }
        }
    }

    // 2. Alpha Tint Panel & Specular Highlight
    let x0 = x.max(0) as u32;
    let y0 = y.max(0) as u32;
    let x1 = ((x + w as i32).max(0) as u32).min(screen_w);
    let y1 = ((y + h as i32).max(0) as u32).min(screen_h);

    let int_y0 = ((y + r as i32).max(0) as u32).min(screen_h);
    let int_y1 = ((y + h as i32 - r as i32).max(0) as u32).min(screen_h);
    let r_sq = (r * r) as i32;

    for py in y0..y1 {
        let row_ptr = ptr.add((py as usize) * (stride as usize));

        // Fast middle interior: skip corner tests
        if py >= int_y0 && py < int_y1 {
            for px in x0..x1 {
                let cur = *row_ptr.add(px as usize);
                *row_ptr.add(px as usize) = blend_pixel(fmt, cur, tint_rgb.0, tint_rgb.1, tint_rgb.2, alpha);
            }
            continue;
        }

        let dy = if py < int_y0 {
            (y + r as i32) - py as i32
        } else {
            py as i32 - (y + h as i32 - 1 - r as i32)
        };
        let dy_sq = dy * dy;

        for px in x0..x1 {
            let dx = if px < (x + r as i32) as u32 {
                (x + r as i32) - px as i32
            } else if px >= ((x + w as i32 - r as i32) as u32) {
                px as i32 - (x + w as i32 - 1 - r as i32)
            } else {
                0
            };

            if dx == 0 || (dx * dx + dy_sq <= r_sq) {
                let cur = *row_ptr.add(px as usize);
                // Subtle 1px specular top highlight
                let pixel = if py == y0 && dy == 0 {
                    blend_pixel(fmt, cur, 255, 255, 255, 80)
                } else {
                    blend_pixel(fmt, cur, tint_rgb.0, tint_rgb.1, tint_rgb.2, alpha)
                };
                *row_ptr.add(px as usize) = pixel;
            }
        }
    }

    // 3. Crisp Rounded Border
    draw_rounded_rect_outline(buf, stride, screen_w, screen_h, x, y, w, h, r, border_color);
}

/// Draws 1px outline for rounded rectangle
pub unsafe fn draw_rounded_rect_outline(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    radius: u32,
    color: u32,
) {
    if x >= screen_w as i32 || y >= screen_h as i32 || w == 0 || h == 0 {
        return;
    }
    let r = radius.min(w / 2).min(h / 2);
    let x0 = x.max(0) as u32;
    let y0 = y.max(0) as u32;
    let x1 = ((x + w as i32).max(0) as u32).min(screen_w);
    let y1 = ((y + h as i32).max(0) as u32).min(screen_h);
    let r_sq_outer = (r * r) as i32;
    let r_sq_inner = if r > 0 { ((r - 1) * (r - 1)) as i32 } else { 0 };

    let ptr = buf as *mut u32;

    for py in y0..y1 {
        let is_edge_y = py == y0 || py == y1 - 1;
        let dy = if py < (y + r as i32) as u32 {
            (y + r as i32) - py as i32
        } else if py >= (y + h as i32 - r as i32) as u32 {
            py as i32 - (y + h as i32 - 1 - r as i32)
        } else {
            0
        };
        let dy_sq = dy * dy;

        let row_ptr = ptr.add((py as usize) * (stride as usize));

        for px in x0..x1 {
            let is_edge_x = px == x0 || px == x1 - 1;
            let dx = if px < (x + r as i32) as u32 {
                (x + r as i32) - px as i32
            } else if px >= (x + w as i32 - r as i32) as u32 {
                px as i32 - (x + w as i32 - 1 - r as i32)
            } else {
                0
            };

            if dx == 0 && dy == 0 {
                if is_edge_x || is_edge_y {
                    *row_ptr.add(px as usize) = color;
                }
            } else {
                let dist_sq = dx * dx + dy_sq;
                if dist_sq <= r_sq_outer && dist_sq >= r_sq_inner {
                    *row_ptr.add(px as usize) = color;
                }
            }
        }
    }
}

/// Hyprland's signature rounded gradient border interpolating color_a -> color_b diagonally
pub unsafe fn draw_gradient_border_rect(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    fmt: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    radius: u32,
    thickness: u32,
    color_a: (u8, u8, u8),
    color_b: (u8, u8, u8),
) {
    if x >= screen_w as i32 || y >= screen_h as i32 || w == 0 || h == 0 || thickness == 0 {
        return;
    }
    let r = radius.min(w / 2).min(h / 2);
    let x0 = x.max(0) as u32;
    let y0 = y.max(0) as u32;
    let x1 = ((x + w as i32).max(0) as u32).min(screen_w);
    let y1 = ((y + h as i32).max(0) as u32).min(screen_h);

    let span = (w + h).max(1) as i32;
    let ptr = buf as *mut u32;

    for t in 0..thickness {
        let cur_r = r.saturating_sub(t);
        let cur_x0 = x0 + t;
        let cur_y0 = y0 + t;
        let cur_x1 = x1.saturating_sub(t);
        let cur_y1 = y1.saturating_sub(t);

        if cur_x0 >= cur_x1 || cur_y0 >= cur_y1 {
            break;
        }

        for py in cur_y0..cur_y1 {
            let is_edge_y = py == cur_y0 || py == cur_y1 - 1;
            let dy = if py < (y + t as i32 + cur_r as i32) as u32 {
                (y + t as i32 + cur_r as i32) - py as i32
            } else if py >= (y + h as i32 - t as i32 - cur_r as i32) as u32 {
                py as i32 - (y + h as i32 - 1 - t as i32 - cur_r as i32)
            } else {
                0
            };

            let row_ptr = ptr.add((py as usize) * (stride as usize));

            for px in cur_x0..cur_x1 {
                let is_edge_x = px == cur_x0 || px == cur_x1 - 1;
                let dx = if px < (x + t as i32 + cur_r as i32) as u32 {
                    (x + t as i32 + cur_r as i32) - px as i32
                } else if px >= (x + w as i32 - t as i32 - cur_r as i32) as u32 {
                    px as i32 - (x + w as i32 - 1 - t as i32 - cur_r as i32)
                } else {
                    0
                };

                let should_draw = if dx == 0 && dy == 0 {
                    is_edge_x || is_edge_y
                } else {
                    let d_sq = dx * dx + dy * dy;
                    let r_sq = (cur_r * cur_r) as i32;
                    let in_sq = if cur_r > 0 { ((cur_r - 1) * (cur_r - 1)) as i32 } else { 0 };
                    d_sq <= r_sq && d_sq >= in_sq
                };

                if should_draw {
                    // Linear interpolation across diagonal (px - x + py - y) / (w + h)
                    let dist = (px as i32 - x + py as i32 - y).clamp(0, span);
                    let ratio = (dist * 255) / span;
                    let inv = 255 - ratio;
                    let r_col = ((color_a.0 as i32 * inv + color_b.0 as i32 * ratio) / 255) as u8;
                    let g_col = ((color_a.1 as i32 * inv + color_b.1 as i32 * ratio) / 255) as u8;
                    let b_col = ((color_a.2 as i32 * inv + color_b.2 as i32 * ratio) / 255) as u8;
                    *row_ptr.add(px as usize) = rgb(fmt, r_col, g_col, b_col);
                }
            }
        }
    }
}

/// Draws a pill button with radius = h / 2
pub unsafe fn draw_pill(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    _fmt: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    bg_color: u32,
    border_color: u32,
) {
    let r = h / 2;
    fill_rounded_rect(buf, stride, screen_w, screen_h, x, y, w, h, r, bg_color);
    if border_color != 0 {
        draw_rounded_rect_outline(buf, stride, screen_w, screen_h, x, y, w, h, r, border_color);
    }
}

/// Midpoint integer circle ring & progress arc (for Orbital Lockscreen)
pub unsafe fn draw_circle_arc(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    cx: i32,
    cy: i32,
    radius: u32,
    thickness: u32,
    color: u32,
) {
    if radius == 0 || thickness == 0 {
        return;
    }
    let ptr = buf as *mut u32;
    let r_outer = radius as i32;
    let r_inner = (radius.saturating_sub(thickness)) as i32;
    let r_out_sq = r_outer * r_outer;
    let r_in_sq = r_inner * r_inner;

    let y_min = (cy - r_outer).max(0) as u32;
    let y_max = (cy + r_outer).min(screen_h as i32 - 1).max(0) as u32;

    for py in y_min..=y_max {
        let dy = py as i32 - cy;
        let dy_sq = dy * dy;
        let row_ptr = ptr.add((py as usize) * (stride as usize));

        let x_min = (cx - r_outer).max(0) as u32;
        let x_max = (cx + r_outer).min(screen_w as i32 - 1).max(0) as u32;

        for px in x_min..=x_max {
            let dx = px as i32 - cx;
            let d_sq = dx * dx + dy_sq;
            if d_sq <= r_out_sq && d_sq >= r_in_sq {
                *row_ptr.add(px as usize) = color;
            }
        }
    }
}

/// Renders 8x16 font character
pub unsafe fn draw_char(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    x: i32,
    y: i32,
    c: u8,
    color: u32,
) {
    if x + 8 <= 0 || y + 16 <= 0 || x >= screen_w as i32 || y >= screen_h as i32 {
        return;
    }
    let glyph = get_glyph(c);
    let ptr = buf as *mut u32;

    for row in 0..16 {
        let py = y + row as i32;
        if py < 0 || py >= screen_h as i32 {
            continue;
        }
        let bits = glyph[row / 2];
        let row_ptr = ptr.add((py as usize) * (stride as usize));

        for col in 0..8 {
            let px = x + col as i32;
            if px >= 0 && px < screen_w as i32 {
                if (bits & (0x80 >> col)) != 0 {
                    *row_ptr.add(px as usize) = color;
                }
            }
        }
    }
}

/// Draws ASCII text
pub unsafe fn draw_text(
    buf: u64,
    stride: u32,
    screen_w: u32,
    screen_h: u32,
    x: i32,
    y: i32,
    text: &str,
    color: u32,
) {
    let mut cx = x;
    for &b in text.as_bytes() {
        if b == b'\n' {
            break;
        }
        draw_char(buf, stride, screen_w, screen_h, cx, y, b, color);
        cx += 8;
    }
}

/// Measures text width in pixels (8px per char)
pub fn measure_text(text: &str) -> u32 {
    (text.len() as u32) * 8
}

/// Procedural wallpaper renderer supporting all 4 patterns with Bayer dithering
pub unsafe fn render_procedural_wallpaper(
    buf: u64,
    stride: u32,
    width: u32,
    height: u32,
    fmt: u32,
    pattern: WallpaperPattern,
    palette: &RyokuPalette,
) {
    if buf == 0 || width == 0 || height == 0 {
        return;
    }
    let ptr = buf as *mut u32;
    let (top_rgb, mid_rgb, bot_rgb) = palette.wall;
    let h_half = (height / 2).max(1);

    for y in 0..height {
        let row_ptr = ptr.add((y as usize) * (stride as usize));
        let bayer_row = (y % 4) as usize;

        // Base 3-stop vertical gradient interpolation
        let (base_r, base_g, base_b) = if y < h_half {
            let t = (y as u32 * 256) / h_half;
            let inv = 256 - t;
            (
                (top_rgb.0 as u32 * inv + mid_rgb.0 as u32 * t) / 256,
                (top_rgb.1 as u32 * inv + mid_rgb.1 as u32 * t) / 256,
                (top_rgb.2 as u32 * inv + mid_rgb.2 as u32 * t) / 256,
            )
        } else {
            let t = ((y - h_half) as u32 * 256) / h_half;
            let inv = 256 - t;
            (
                (mid_rgb.0 as u32 * inv + bot_rgb.0 as u32 * t) / 256,
                (mid_rgb.1 as u32 * inv + bot_rgb.1 as u32 * t) / 256,
                (mid_rgb.2 as u32 * inv + bot_rgb.2 as u32 * t) / 256,
            )
        };

        for x in 0..width {
            let dither = BAYER_4X4[bayer_row][(x % 4) as usize];
            let mut r = (base_r as i16 + dither).clamp(0, 255) as u8;
            let mut g = (base_g as i16 + dither).clamp(0, 255) as u8;
            let mut b = (base_b as i16 + dither).clamp(0, 255) as u8;

            match pattern {
                WallpaperPattern::AmaterasuMarble => {
                    // Sinusoidal marble veins
                    let wave = ((x as i32 * 3 + y as i32 * 2) / 16) % 32;
                    if wave >= 28 {
                        let boost = (wave - 27) as u8 * 5;
                        r = r.saturating_add(boost);
                        g = g.saturating_add(boost);
                        b = b.saturating_add(boost + 2);
                    }
                }
                WallpaperPattern::SakuraMist => {
                    // Soft glowing mist band across the diagonal
                    let diag = (x as i32 + y as i32) / 24;
                    if diag % 16 == 0 {
                        r = r.saturating_add(8);
                        g = g.saturating_add(4);
                        b = b.saturating_add(10);
                    }
                }
                WallpaperPattern::CyberGrid => {
                    // Horizon grid lines in subtle accent
                    if y > height / 2 && (y % 32 == 0 || (x + y / 4) % 64 == 0) {
                        r = blend_channel(palette.accent.0, r, 30);
                        g = blend_channel(palette.accent.1, g, 30);
                        b = blend_channel(palette.accent.2, b, 30);
                    }
                }
                WallpaperPattern::MonolithInk => {
                    // Minimalist subtle centered vignette
                    let cx = (width / 2) as i32;
                    let cy = (height / 2) as i32;
                    let dx = (x as i32 - cx).abs();
                    let dy = (y as i32 - cy).abs();
                    if dx < 180 && dy < 180 {
                        let vig = ((180 - dx.max(dy)) / 18) as u8;
                        r = r.saturating_sub(vig);
                        g = g.saturating_sub(vig);
                        b = b.saturating_sub(vig);
                    }
                }
            }

            *row_ptr.add(x as usize) = rgb(fmt, r, g, b);
        }
    }
}

/// Samples 4x4 points across the wallpaper and extracts the dominant saturated tone
pub unsafe fn sample_matugen_dominant(
    wallpaper_va: u64,
    stride: u32,
    width: u32,
    height: u32,
    fmt: u32,
) -> (u8, u8, u8) {
    if wallpaper_va == 0 || width == 0 || height == 0 {
        return (122, 162, 247); // Default TokyoNight blue fallback
    }
    let ptr = wallpaper_va as *const u32;
    let mut best_sat = 0u32;
    let mut best_rgb = (122, 162, 247);

    for yi in 1..=4 {
        let y = ((height as u64 * yi) / 5) as u32;
        let row_ptr = ptr.add((y as usize) * (stride as usize));
        for xi in 1..=4 {
            let x = ((width as u64 * xi) / 5) as u32;
            let col = *row_ptr.add(x as usize);
            let (r, g, b) = unpack_rgb(fmt, col);
            let max_c = r.max(g).max(b) as u32;
            let min_c = r.min(g).min(b) as u32;
            let sat = max_c.saturating_sub(min_c);
            if sat > best_sat {
                best_sat = sat;
                best_rgb = (r, g, b);
            }
        }
    }

    best_rgb
}

/// Formats an unsigned 64-bit integer into a decimal byte slice without trait objects or vtables
pub fn write_u64_to_buf(mut val: u64, buf: &mut [u8]) -> usize {
    if val == 0 {
        if !buf.is_empty() {
            buf[0] = b'0';
            return 1;
        }
        return 0;
    }
    let mut temp = [0u8; 20];
    let mut len = 0;
    while val > 0 && len < temp.len() {
        temp[len] = b'0' + (val % 10) as u8;
        val /= 10;
        len += 1;
    }
    let out_len = len.min(buf.len());
    for i in 0..out_len {
        buf[i] = temp[len - 1 - i];
    }
    out_len
}

/// Appends a u64 to an alloc::string::String without formatting vtables
pub fn push_u64(s: &mut alloc::string::String, val: u64) {
    let mut buf = [0u8; 20];
    let len = write_u64_to_buf(val, &mut buf);
    if let Ok(sub) = core::str::from_utf8(&buf[..len]) {
        s.push_str(sub);
    }
}

/// Appends a 2-digit zero-padded u64 to an alloc::string::String without formatting vtables
pub fn push_u64_pad2(s: &mut alloc::string::String, val: u64) {
    if val < 10 {
        s.push('0');
    }
    push_u64(s, val);
}

/// Stack/heap string builder avoiding core::fmt / Display trait object vtables
#[derive(Clone, Default)]
pub struct StrBuf {
    inner: alloc::string::String,
}

impl StrBuf {
    pub fn new() -> Self {
        Self { inner: alloc::string::String::new() }
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self { inner: alloc::string::String::with_capacity(cap) }
    }

    pub fn push_str(mut self, s: &str) -> Self {
        self.inner.push_str(s);
        self
    }

    pub fn push_char(mut self, c: char) -> Self {
        self.inner.push(c);
        self
    }

    pub fn push_u64(mut self, val: u64) -> Self {
        push_u64(&mut self.inner, val);
        self
    }

    pub fn push_usize(self, val: usize) -> Self {
        self.push_u64(val as u64)
    }

    pub fn push_pad2(mut self, val: u64) -> Self {
        push_u64_pad2(&mut self.inner, val);
        self
    }

    pub fn push_pad_right(mut self, s: &str, width: usize) -> Self {
        self.inner.push_str(s);
        for _ in 0..width.saturating_sub(s.len()) {
            self.inner.push(' ');
        }
        self
    }

    pub fn as_str(&self) -> &str {
        &self.inner
    }

    pub fn into_string(self) -> alloc::string::String {
        self.inner
    }
}

impl core::ops::Deref for StrBuf {
    type Target = str;
    fn deref(&self) -> &str {
        &self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_19_themes_valid_and_distinct() {
        assert_eq!(RyokuThemeId::ALL.len(), 19);
        for id in &RyokuThemeId::ALL {
            let pal = id.palette();
            assert!(!pal.name.is_empty());
            assert!(!pal.slug.is_empty());
            assert_eq!(RyokuThemeId::from_slug(pal.slug), Some(*id));
            assert_eq!(pal.term_colors.len(), 16);
        }
    }

    #[test]
    fn test_night_light_warm_filter() {
        let white = (255, 255, 255);
        let warm = apply_night_light_rgb(white);
        assert_eq!(warm.0, 255); // Red preserved
        assert_eq!(warm.1, 238); // Green gently attenuated
        assert_eq!(warm.2, 182); // Blue filtered to warm ~3400K
    }

    #[test]
    fn test_matugen_tinting_application() {
        let mut pal = RyokuThemeId::TokyoNight.palette();
        let sampled = (255, 200, 50); // Warm amber sample
        apply_matugen_tint(&mut pal, sampled);
        // Accent should blend 35% of sample with 65% of original (122, 162, 247)
        let expected_r = ((122 * 65 + 255 * 35) / 100) as u8;
        assert_eq!(pal.accent.0, expected_r);
    }

    #[test]
    fn test_fast_copy_buffer_roundtrip() {
        let mut src = [0u8; 256];
        let mut dst = [0u8; 256];
        for i in 0..256 {
            src[i] = (i * 3 + 7) as u8;
        }
        unsafe {
            fast_copy_buffer(src.as_ptr() as u64, dst.as_mut_ptr() as u64, 256);
        }
        assert_eq!(&src[..], &dst[..]);
    }

    #[test]
    fn test_bayer_matrix_properties() {
        let mut sum = 0i32;
        for r in 0..4 {
            for c in 0..4 {
                sum += BAYER_4X4[r][c] as i32;
                assert!(BAYER_4X4[r][c] >= -8 && BAYER_4X4[r][c] <= 7);
            }
        }
        // Dither energy sums to -8 across the 16 cells (centered near zero)
        assert_eq!(sum, -8);
    }
}
