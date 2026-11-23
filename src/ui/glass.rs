//! Liquid Glass Desktop UI Rendering Primitives for Project Genesis (Stage 30).
//!
//! Provides procedural multi-stop Aurora/Gradient wallpapers,
//! rounded-corner alpha-blended glass panels with frosted blur sampling,
//! specular top-edge highlights, soft drop shadows, and modern window chrome.

use crate::drivers::fb::rgb_format;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum GlassTheme {
    MidnightAurora, // Deep blue-violet to dark teal aurora
    OceanGlass,     // Deep ocean azure to aquamarine
    SunsetAmber,    // Deep plum/indigo to warm amber/rose
    ObsidianPro,    // Pure sleek graphite / dark obsidian
}

impl GlassTheme {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::MidnightAurora => "Midnight Aurora",
            Self::OceanGlass => "Ocean Glass",
            Self::SunsetAmber => "Sunset Amber",
            Self::ObsidianPro => "Obsidian Pro",
        }
    }

    pub const fn stops(&self) -> ((u8, u8, u8), (u8, u8, u8), (u8, u8, u8)) {
        match self {
            // Top, Middle, Bottom RGB
            Self::MidnightAurora => ((10, 15, 29), (30, 27, 75), (6, 78, 59)),
            Self::OceanGlass => ((2, 27, 43), (12, 74, 110), (14, 116, 144)),
            Self::SunsetAmber => ((46, 16, 101), (131, 24, 67), (120, 53, 15)),
            Self::ObsidianPro => ((9, 9, 11), (24, 24, 27), (39, 39, 42)),
        }
    }

    pub const fn default_tint(&self) -> ((u8, u8, u8), u8) {
        match self {
            Self::MidnightAurora => ((15, 23, 42), 180), // Slate-900 tint, 70% opacity
            Self::OceanGlass => ((8, 30, 50), 175),
            Self::SunsetAmber => ((35, 18, 45), 185),
            Self::ObsidianPro => ((18, 18, 20), 200),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum AccentColor {
    ElectricBlue,
    Violet,
    Emerald,
    Amber,
    Rose,
}

impl AccentColor {
    pub const fn name(&self) -> &'static str {
        match self {
            Self::ElectricBlue => "Electric Blue",
            Self::Violet => "Cosmic Violet",
            Self::Emerald => "Emerald Glass",
            Self::Amber => "Warm Amber",
            Self::Rose => "Vibrant Rose",
        }
    }

    pub const fn rgb(&self) -> (u8, u8, u8) {
        match self {
            Self::ElectricBlue => (59, 130, 246), // #3B82F6
            Self::Violet => (139, 92, 246),       // #8B5CF6
            Self::Emerald => (16, 185, 129),      // #10B981
            Self::Amber => (245, 158, 11),        // #F59E0B
            Self::Rose => (244, 63, 94),          // #F43F5E
        }
    }
}

#[inline(always)]
pub fn blend_channel(base: u8, overlay: u8, alpha: u8) -> u8 {
    let b = base as u32;
    let o = overlay as u32;
    let a = alpha as u32;
    ((b * (255 - a) + o * a) / 255) as u8
}

#[inline(always)]
pub fn unpack_pixel(pixel: u32, pixel_format: u32) -> (u8, u8, u8) {
    if pixel_format == 1 {
        // Format 1: BGR (B in 0..7, G in 8..15, R in 16..23)
        ((pixel >> 16) as u8, (pixel >> 8) as u8, pixel as u8)
    } else {
        // Format 0: RGB (R in 0..7, G in 8..15, B in 16..23)
        (pixel as u8, (pixel >> 8) as u8, (pixel >> 16) as u8)
    }
}

#[inline(always)]
pub fn blend_pixel(base: u32, overlay_rgb: (u8, u8, u8), alpha: u8, pixel_format: u32) -> u32 {
    if alpha == 0 {
        return base;
    }
    let (br, bg, bb) = unpack_pixel(base, pixel_format);
    let (or, og, ob) = overlay_rgb;
    let r = blend_channel(br, or, alpha);
    let g = blend_channel(bg, og, alpha);
    let b = blend_channel(bb, ob, alpha);
    rgb_format(pixel_format, r, g, b)
}

/// Computes distance squared from corner center for rounded rectangle testing.
#[inline(always)]
pub fn is_inside_rounded_rect(
    px: i32,
    py: i32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    radius: u32,
) -> bool {
    let r = radius as i32;
    let right = x + w as i32;
    let bottom = y + h as i32;

    if px < x || px >= right || py < y || py >= bottom {
        return false;
    }
    if r <= 0 {
        return true;
    }

    let dx = if px < x + r {
        (x + r) - px
    } else if px >= right - r {
        px - (right - r - 1)
    } else {
        0
    };

    let dy = if py < y + r {
        (y + r) - py
    } else if py >= bottom - r {
        py - (bottom - r - 1)
    } else {
        0
    };

    if dx > 0 && dy > 0 {
        (dx * dx + dy * dy) <= (r * r)
    } else {
        true
    }
}

/// Renders a smooth multi-stop procedural gradient wallpaper with aurora glow curves.
pub unsafe fn render_aurora_wallpaper(
    buf_va: u64,
    stride: u32,
    width: u32,
    height: u32,
    pixel_format: u32,
    theme: GlassTheme,
) {
    if buf_va == 0 || width == 0 || height == 0 {
        return;
    }
    let (top, mid, bot) = theme.stops();

    for y in 0..height {
        let row_ptr = (buf_va + ((y as u64) * (stride as u64) * 4)) as *mut u32;
        let t_y = (y * 255) / height.max(1);

        // Vertical gradient interpolation
        let (base_r, base_g, base_b) = if t_y < 128 {
            let factor = (t_y * 2) as u16;
            (
                ((top.0 as u16 * (255 - factor) + mid.0 as u16 * factor) / 255) as u8,
                ((top.1 as u16 * (255 - factor) + mid.1 as u16 * factor) / 255) as u8,
                ((top.2 as u16 * (255 - factor) + mid.2 as u16 * factor) / 255) as u8,
            )
        } else {
            let factor = ((t_y - 128) * 2) as u16;
            (
                ((mid.0 as u16 * (255 - factor) + bot.0 as u16 * factor) / 255) as u8,
                ((mid.1 as u16 * (255 - factor) + bot.1 as u16 * factor) / 255) as u8,
                ((mid.2 as u16 * (255 - factor) + bot.2 as u16 * factor) / 255) as u8,
            )
        };

        for x in 0..width {
            // Horizontal ambient curve: slight diagonal wave across width
            let wave = (((x + y / 2) % (width.max(1))) * 20) / width.max(1);
            let r = base_r.saturating_add(wave as u8);
            let g = base_g.saturating_add((wave / 2) as u8);
            let b = base_b.saturating_add((wave * 3 / 4) as u8);

            let color = rgb_format(pixel_format, r, g, b);
            core::ptr::write_volatile(row_ptr.add(x as usize), color);
        }
    }
}

/// Renders a rounded-corner alpha-blended Liquid Glass panel with optional frosted blur,
/// specular top-edge highlight, soft drop shadow, and crisp border.
pub unsafe fn draw_rounded_glass_panel(
    buf_va: u64,
    stride: u32,
    fb_w: u32,
    fb_h: u32,
    pixel_format: u32,
    x: i32,
    y: i32,
    w: u32,
    h: u32,
    radius: u32,
    tint_rgb: (u8, u8, u8),
    alpha: u8,
    border_rgb: (u8, u8, u8),
    shadow: bool,
    blur: bool,
) {
    if buf_va == 0 || w == 0 || h == 0 {
        return;
    }

    let r_i32 = radius as i32;

    // 1. Soft Drop Shadow
    if shadow {
        let shadow_y_off = 3i32;
        let shadow_rad = 4i32;
        let sh_x0 = (x - shadow_rad).max(0);
        let sh_y0 = (y + shadow_y_off).max(0);
        let sh_x1 = (x + w as i32 + shadow_rad).min(fb_w as i32);
        let sh_y1 = (y + h as i32 + shadow_y_off + shadow_rad).min(fb_h as i32);

        for sy in sh_y0..sh_y1 {
            let row_ptr = (buf_va + ((sy as u64) * (stride as u64) * 4)) as *mut u32;
            for sx in sh_x0..sh_x1 {
                // If not inside the panel itself, apply shadow darkening
                if !is_inside_rounded_rect(sx, sy, x, y, w, h, radius) {
                    let cur = core::ptr::read_volatile(row_ptr.add(sx as usize));
                    let darkened = blend_pixel(cur, (0, 0, 0), 38, pixel_format);
                    core::ptr::write_volatile(row_ptr.add(sx as usize), darkened);
                }
            }
        }
    }

    // 2. Glass Panel Body
    let y0 = y.max(0);
    let y1 = (y + h as i32).min(fb_h as i32);
    let x0 = x.max(0);
    let x1 = (x + w as i32).min(fb_w as i32);

    for py in y0..y1 {
        let row_ptr = (buf_va + ((py as u64) * (stride as u64) * 4)) as *mut u32;
        for px in x0..x1 {
            if !is_inside_rounded_rect(px, py, x, y, w, h, radius) {
                continue;
            }

            let cur = core::ptr::read_volatile(row_ptr.add(px as usize));

            // Frosted blur backdrop sampling: sample neighbor pixel to diffuse background
            let backdrop = if blur && px + 2 < fb_w as i32 && py + 1 < fb_h as i32 {
                let neighbor_offset = (((py + 1) as u64) * (stride as u64) + ((px + 2) as u64)) * 4;
                let neighbor = core::ptr::read_volatile((buf_va + neighbor_offset) as *const u32);
                let (r1, g1, b1) = unpack_pixel(cur, pixel_format);
                let (r2, g2, b2) = unpack_pixel(neighbor, pixel_format);
                let avg_r = ((r1 as u16 + r2 as u16) / 2) as u8;
                let avg_g = ((g1 as u16 + g2 as u16) / 2) as u8;
                let avg_b = ((b1 as u16 + b2 as u16) / 2) as u8;
                rgb_format(pixel_format, avg_r, avg_g, avg_b)
            } else {
                cur
            };

            // Blend tinted glass color over backdrop
            let mut glass_color = blend_pixel(backdrop, tint_rgb, alpha, pixel_format);

            // 1px specular top-edge highlight
            if py == y || (py == y + 1 && px >= x + r_i32 && px < x + w as i32 - r_i32) {
                glass_color = blend_pixel(glass_color, (255, 255, 255), 70, pixel_format);
            }

            // 1px subtle glass border outline
            let is_border = px == x
                || px == x + w as i32 - 1
                || py == y
                || py == y + h as i32 - 1
                || (radius > 0
                    && (px < x + r_i32 || px >= x + w as i32 - r_i32 || py < y + r_i32 || py >= y + h as i32 - r_i32)
                    && !is_inside_rounded_rect(px, py, x + 1, y + 1, w.saturating_sub(2), h.saturating_sub(2), radius.saturating_sub(1)));

            if is_border {
                glass_color = blend_pixel(glass_color, border_rgb, 120, pixel_format);
            }

            core::ptr::write_volatile(row_ptr.add(px as usize), glass_color);
        }
    }
}

/// Draws modern macOS/iOS style traffic light titlebar controls (Close, Minimize, Maximize).
pub unsafe fn draw_traffic_lights(
    buf_va: u64,
    stride: u32,
    fb_w: u32,
    fb_h: u32,
    pixel_format: u32,
    win_x: i32,
    win_y: i32,
    focused: bool,
) {
    let col_red = if focused { (239, 68, 68) } else { (100, 116, 139) };
    let col_yellow = if focused { (245, 158, 11) } else { (100, 116, 139) };
    let col_green = if focused { (16, 185, 129) } else { (100, 116, 139) };

    let r = 4u32;
    let cy = win_y + 12;

    // Red Close (win_x + 14)
    draw_circle(buf_va, stride, fb_w, fb_h, pixel_format, win_x + 14, cy, r, col_red);
    // Yellow Minimize (win_x + 24)
    draw_circle(buf_va, stride, fb_w, fb_h, pixel_format, win_x + 24, cy, r, col_yellow);
    // Green Maximize (win_x + 40)
    draw_circle(buf_va, stride, fb_w, fb_h, pixel_format, win_x + 40, cy, r, col_green);
}

/// Helper to draw filled anti-aliased small circle.
#[inline]
pub unsafe fn draw_circle(
    buf_va: u64,
    stride: u32,
    fb_w: u32,
    fb_h: u32,
    pixel_format: u32,
    cx: i32,
    cy: i32,
    r: u32,
    rgb: (u8, u8, u8),
) {
    let r_i = r as i32;
    let r2 = (r * r) as i32;
    let color = rgb_format(pixel_format, rgb.0, rgb.1, rgb.2);

    for dy in -r_i..=r_i {
        let py = cy + dy;
        if py < 0 || py >= fb_h as i32 {
            continue;
        }
        let row_ptr = (buf_va + ((py as u64) * (stride as u64) * 4)) as *mut u32;
        for dx in -r_i..=r_i {
            let px = cx + dx;
            if px < 0 || px >= fb_w as i32 {
                continue;
            }
            if dx * dx + dy * dy <= r2 {
                core::ptr::write_volatile(row_ptr.add(px as usize), color);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blend_channel_bounds() {
        assert_eq!(blend_channel(100, 200, 0), 100);
        assert_eq!(blend_channel(100, 200, 255), 200);
        assert_eq!(blend_channel(0, 255, 128), 128);
    }

    #[test]
    fn test_blend_pixel_formats() {
        // Format 1: BGR
        let black = 0u32;
        let white_blended = blend_pixel(black, (255, 255, 255), 255, 1);
        let (r, g, b) = unpack_pixel(white_blended, 1);
        assert_eq!((r, g, b), (255, 255, 255));

        // Format 0: RGB
        let red_blended = blend_pixel(black, (200, 50, 25), 255, 0);
        let (r, g, b) = unpack_pixel(red_blended, 0);
        assert_eq!((r, g, b), (200, 50, 25));
    }

    #[test]
    fn test_rounded_rect_masking() {
        // 100x100 rect with radius 10 at (0, 0)
        assert!(is_inside_rounded_rect(50, 50, 0, 0, 100, 100, 10)); // Center inside
        assert!(is_inside_rounded_rect(10, 10, 0, 0, 100, 100, 10)); // Tangent corner
        assert!(!is_inside_rounded_rect(1, 1, 0, 0, 100, 100, 10));  // Outside corner arc
        assert!(!is_inside_rounded_rect(-1, 50, 0, 0, 100, 100, 10)); // Outside left
    }

    #[test]
    fn test_theme_and_accent_properties() {
        assert_eq!(GlassTheme::MidnightAurora.name(), "Midnight Aurora");
        assert_eq!(GlassTheme::OceanGlass.name(), "Ocean Glass");
        assert_eq!(AccentColor::ElectricBlue.name(), "Electric Blue");
        assert_eq!(AccentColor::Emerald.rgb(), (16, 185, 129));
    }
}
