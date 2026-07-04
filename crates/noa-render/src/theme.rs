//! Palette / theme resolution: `Color -> [f32; 4]` (straight,
//! non-premultiplied, sRGB-encoded 0..1 components). The renderer converts
//! these values to the target surface's output space immediately before
//! uploading clear colors and cell instances.
//!
//! Uses the shared xterm 256-color table (16 ANSI + 6x6x6 cube + 24 grayscale
//! ramp) plus a default foreground/background, mirroring Ghostty's default
//! theme.

use noa_core::{Color, DEFAULT_BG, DEFAULT_CURSOR, DEFAULT_FG, Rgb, xterm_palette};
use noa_grid::TerminalColors;

/// A resolved terminal color theme: default fg/bg plus the 256-color palette.
///
/// `PartialEq` (WP4): every field is an integer `Rgb`, so value equality is
/// cheap and exact — used by the renderer's per-pane invalidation key to
/// detect a theme swap without needing a separate identity/id field.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub default_fg: Rgb,
    pub default_bg: Rgb,
    pub cursor: Rgb,
    pub selection_fg: Rgb,
    pub selection_bg: Rgb,
    pub search_fg: Rgb,
    pub search_bg: Rgb,
    pub active_search_fg: Rgb,
    pub active_search_bg: Rgb,
    /// Index 0..=255: 16 ANSI + 6x6x6 color cube (16..=231) + grayscale ramp (232..=255).
    pub palette: [Rgb; 256],
}

impl Default for Theme {
    fn default() -> Self {
        Self::new()
    }
}

impl Theme {
    pub fn new() -> Self {
        Theme {
            default_fg: DEFAULT_FG,
            default_bg: DEFAULT_BG,
            cursor: DEFAULT_CURSOR,
            selection_fg: DEFAULT_BG,
            selection_bg: DEFAULT_FG,
            search_fg: Rgb::new(0x1b, 0x1b, 0x1b),
            search_bg: Rgb::new(0xff, 0xd7, 0x5f),
            active_search_fg: Rgb::new(0xff, 0xff, 0xff),
            active_search_bg: Rgb::new(0x00, 0x87, 0xaf),
            palette: xterm_palette(),
        }
    }

    /// Resolve a cell color to an RGBA tuple, `0..=1` per channel.
    ///
    /// `is_fg` picks the default-color fallback (foreground vs background);
    /// palette/rgb colors resolve identically regardless of slot.
    pub fn resolve(&self, c: Color, is_fg: bool) -> [f32; 4] {
        let rgb = match c {
            Color::Default => {
                if is_fg {
                    self.default_fg
                } else {
                    self.default_bg
                }
            }
            Color::Palette(idx) => self.palette[idx as usize],
            Color::Rgb(rgb) => rgb,
        };
        rgba(rgb)
    }

    pub fn resolve_with_colors(&self, c: Color, is_fg: bool, colors: &TerminalColors) -> [f32; 4] {
        rgba(self.resolve_rgb_with_colors(c, is_fg, colors))
    }

    pub fn default_bg_with_colors(&self, colors: &TerminalColors) -> [f32; 4] {
        rgba(colors.default_bg().unwrap_or(self.default_bg))
    }

    pub fn cursor_with_colors(&self, colors: &TerminalColors) -> [f32; 4] {
        rgba(colors.cursor().unwrap_or(self.cursor))
    }

    pub fn selection_fg(&self) -> [f32; 4] {
        rgba(self.selection_fg)
    }

    pub fn selection_bg(&self) -> [f32; 4] {
        rgba(self.selection_bg)
    }

    pub fn search_fg(&self) -> [f32; 4] {
        rgba(self.search_fg)
    }

    pub fn search_bg(&self) -> [f32; 4] {
        rgba(self.search_bg)
    }

    pub fn active_search_fg(&self) -> [f32; 4] {
        rgba(self.active_search_fg)
    }

    pub fn active_search_bg(&self) -> [f32; 4] {
        rgba(self.active_search_bg)
    }

    fn resolve_rgb_with_colors(&self, c: Color, is_fg: bool, colors: &TerminalColors) -> Rgb {
        match c {
            Color::Default => {
                if is_fg {
                    colors.default_fg().unwrap_or(self.default_fg)
                } else {
                    colors.default_bg().unwrap_or(self.default_bg)
                }
            }
            Color::Palette(idx) => colors.palette(idx).unwrap_or(self.palette[idx as usize]),
            Color::Rgb(rgb) => rgb,
        }
    }
}

fn rgba(rgb: Rgb) -> [f32; 4] {
    [
        rgb.r as f32 / 255.0,
        rgb.g as f32 / 255.0,
        rgb.b as f32 / 255.0,
        1.0,
    ]
}

/// Component-wise linear blend of two colors: `t == 0.0` returns `a`, `t ==
/// 1.0` returns `b`, values between interpolate each 8-bit channel and round
/// to nearest. `t` is clamped to `0.0..=1.0`. The result never leaves the
/// `[min, max]` range of the two endpoints, so no channel clamp is needed.
pub fn blend(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let lerp = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Rgb::new(lerp(a.r, b.r), lerp(a.g, b.g), lerp(a.b, b.b))
}

/// A modal-overlay surface palette derived from a [`Theme`]'s own default
/// fg/bg (plus its selection colors). The confirm dialog, command palette,
/// and search prompt all paint from this one style so they share a single
/// visual language. Because every color interpolates between the theme's own
/// foreground and background, the result tracks the theme's light/dark
/// polarity automatically — a light theme yields a light elevated surface, a
/// dark theme a dark one, with no per-theme tuning.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverlayStyle {
    surface_bg: Rgb,
    surface_fg: Rgb,
    muted_fg: Rgb,
    border: Rgb,
    accent_bg: Rgb,
    accent_fg: Rgb,
}

impl OverlayStyle {
    /// Derive the overlay palette from `theme`:
    /// - `surface_bg` = 8% of the way from the terminal bg toward its fg — an
    ///   "elevated" surface just distinct from the terminal background;
    /// - `surface_fg` = the theme's default foreground;
    /// - `muted_fg` = 45% from fg toward bg — dimmed text for hints/counters;
    /// - `border` = 70% from fg toward bg — a low-contrast (~30% fg) outline;
    /// - `accent_bg`/`accent_fg` = the theme's selection colors (selected row).
    pub fn from_theme(theme: &Theme) -> Self {
        let fg = theme.default_fg;
        let bg = theme.default_bg;
        OverlayStyle {
            surface_bg: blend(bg, fg, 0.08),
            surface_fg: fg,
            muted_fg: blend(fg, bg, 0.45),
            border: blend(fg, bg, 0.70),
            accent_bg: theme.selection_bg,
            accent_fg: theme.selection_fg,
        }
    }

    pub fn surface_bg(&self) -> [f32; 4] {
        rgba(self.surface_bg)
    }

    pub fn surface_fg(&self) -> [f32; 4] {
        rgba(self.surface_fg)
    }

    pub fn muted_fg(&self) -> [f32; 4] {
        rgba(self.muted_fg)
    }

    pub fn border(&self) -> [f32; 4] {
        rgba(self.border)
    }

    pub fn accent_bg(&self) -> [f32; 4] {
        rgba(self.accent_bg)
    }

    pub fn accent_fg(&self) -> [f32; 4] {
        rgba(self.accent_fg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_colors_correct() {
        let theme = Theme::new();
        assert_eq!(theme.palette[1], Rgb::new(0xcd, 0x00, 0x00)); // red
        assert_eq!(theme.palette[2], Rgb::new(0x00, 0xcd, 0x00)); // green
        assert_eq!(theme.palette[15], Rgb::new(0xff, 0xff, 0xff)); // bright white
    }

    #[test]
    fn cube_endpoints_correct() {
        let theme = Theme::new();
        // 16 = (0,0,0) corner of the cube.
        assert_eq!(theme.palette[16], Rgb::new(0, 0, 0));
        // 231 = (5,5,5) corner of the cube -> (255,255,255).
        assert_eq!(theme.palette[231], Rgb::new(0xff, 0xff, 0xff));
    }

    #[test]
    fn grayscale_ramp_correct() {
        let theme = Theme::new();
        assert_eq!(theme.palette[232], Rgb::new(8, 8, 8));
        assert_eq!(theme.palette[255], Rgb::new(238, 238, 238));
    }

    #[test]
    fn resolve_default_and_rgb() {
        let theme = Theme::new();
        assert_eq!(
            theme.resolve(Color::Default, true),
            [
                theme.default_fg.r as f32 / 255.0,
                theme.default_fg.g as f32 / 255.0,
                theme.default_fg.b as f32 / 255.0,
                1.0,
            ]
        );
        let rgb = Rgb::new(10, 20, 30);
        assert_eq!(
            theme.resolve(Color::Rgb(rgb), false),
            [10.0 / 255.0, 20.0 / 255.0, 30.0 / 255.0, 1.0]
        );
    }

    #[test]
    fn resolve_uses_terminal_color_overrides() {
        let theme = Theme::new();
        let mut colors = TerminalColors::default();
        colors.set_palette(1, Rgb::new(1, 2, 3));
        colors.set_default_fg(Rgb::new(4, 5, 6));
        colors.set_default_bg(Rgb::new(7, 8, 9));
        colors.set_cursor(Rgb::new(10, 11, 12));

        assert_eq!(
            theme.resolve_with_colors(Color::Palette(1), true, &colors),
            [1.0 / 255.0, 2.0 / 255.0, 3.0 / 255.0, 1.0]
        );
        assert_eq!(
            theme.resolve_with_colors(Color::Default, true, &colors),
            [4.0 / 255.0, 5.0 / 255.0, 6.0 / 255.0, 1.0]
        );
        assert_eq!(
            theme.default_bg_with_colors(&colors),
            [7.0 / 255.0, 8.0 / 255.0, 9.0 / 255.0, 1.0]
        );
        assert_eq!(
            theme.cursor_with_colors(&colors),
            [10.0 / 255.0, 11.0 / 255.0, 12.0 / 255.0, 1.0]
        );
    }

    #[test]
    fn selection_colors_are_theme_defined() {
        let theme = Theme::new();

        assert_eq!(theme.selection_fg(), theme.resolve(Color::Default, false));
        assert_eq!(theme.selection_bg(), theme.resolve(Color::Default, true));
    }

    #[test]
    fn search_colors_are_theme_defined() {
        let theme = Theme::new();

        assert_eq!(theme.search_bg(), rgba(theme.search_bg));
        assert_eq!(theme.active_search_bg(), rgba(theme.active_search_bg));
    }

    #[test]
    fn blend_endpoints_and_midpoint() {
        let a = Rgb::new(0, 0, 0);
        let b = Rgb::new(200, 100, 40);
        // Endpoints return the inputs exactly.
        assert_eq!(blend(a, b, 0.0), a);
        assert_eq!(blend(a, b, 1.0), b);
        // Midpoint is the rounded component-wise mean.
        assert_eq!(blend(a, b, 0.5), Rgb::new(100, 50, 20));
        // Out-of-range t clamps to the endpoints.
        assert_eq!(blend(a, b, -1.0), a);
        assert_eq!(blend(a, b, 2.0), b);
    }

    #[test]
    fn overlay_style_tracks_theme_polarity() {
        // Dark theme (default): surface is lighter than bg, border sits low
        // contrast between fg and bg.
        let theme = Theme::new();
        let style = OverlayStyle::from_theme(&theme);
        assert_eq!(
            style.surface_bg,
            blend(theme.default_bg, theme.default_fg, 0.08)
        );
        assert_eq!(style.surface_fg, theme.default_fg);
        assert_eq!(
            style.muted_fg,
            blend(theme.default_fg, theme.default_bg, 0.45)
        );
        assert_eq!(
            style.border,
            blend(theme.default_fg, theme.default_bg, 0.70)
        );
        assert_eq!(style.accent_bg, theme.selection_bg);
        assert_eq!(style.accent_fg, theme.selection_fg);

        // A light theme (swapped fg/bg) yields a surface that is DARKER than
        // its background — the construction adapts without special-casing.
        let mut light = Theme::new();
        light.default_fg = Rgb::new(0x20, 0x20, 0x20);
        light.default_bg = Rgb::new(0xf7, 0xf7, 0xf7);
        let light_style = OverlayStyle::from_theme(&light);
        assert!(light_style.surface_bg.r < light.default_bg.r);
        assert!(light_style.muted_fg.r > light.default_fg.r);
    }
}
