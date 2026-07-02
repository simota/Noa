//! Palette / theme resolution: `Color -> [f32; 4]` (straight, non-premultiplied,
//! srgb-encoded 0..1 components — the surface format is `*Srgb`, so uploading
//! plain `u8/255` values here and letting the format do the srgb decode on
//! sampling keeps color math simple and consistent).
//!
//! Uses the shared xterm 256-color table (16 ANSI + 6x6x6 cube + 24 grayscale
//! ramp) plus a default foreground/background, mirroring Ghostty's default
//! theme.

use noa_core::{Color, DEFAULT_BG, DEFAULT_CURSOR, DEFAULT_FG, Rgb, xterm_palette};
use noa_grid::TerminalColors;

/// A resolved terminal color theme: default fg/bg plus the 256-color palette.
#[derive(Clone, Debug)]
pub struct Theme {
    pub default_fg: Rgb,
    pub default_bg: Rgb,
    pub cursor: Rgb,
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
}
