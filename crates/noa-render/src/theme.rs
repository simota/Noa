//! Palette / theme resolution: `Color -> [f32; 4]` (straight, non-premultiplied,
//! srgb-encoded 0..1 components — the surface format is `*Srgb`, so uploading
//! plain `u8/255` values here and letting the format do the srgb decode on
//! sampling keeps color math simple and consistent).
//!
//! Owns the xterm 256-color table (16 ANSI + 6x6x6 cube + 24 grayscale ramp)
//! plus a default foreground/background, mirroring Ghostty's default theme.

use noa_core::{Color, Rgb};

/// A resolved terminal color theme: default fg/bg plus the 256-color palette.
#[derive(Clone, Debug)]
pub struct Theme {
    pub default_fg: Rgb,
    pub default_bg: Rgb,
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
            default_fg: Rgb::new(0xe0, 0xe0, 0xe0),
            default_bg: Rgb::new(0x1e, 0x1e, 0x1e),
            palette: build_xterm_palette(),
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
        [
            rgb.r as f32 / 255.0,
            rgb.g as f32 / 255.0,
            rgb.b as f32 / 255.0,
            1.0,
        ]
    }
}

/// Build the standard 256-color xterm palette:
/// * 0..=7: the "normal" ANSI 8.
/// * 8..=15: the "bright" ANSI 8.
/// * 16..=231: a 6x6x6 RGB color cube.
/// * 232..=255: a 24-step grayscale ramp.
fn build_xterm_palette() -> [Rgb; 256] {
    let mut p = [Rgb::new(0, 0, 0); 256];

    // 0..=7 normal, 8..=15 bright — standard xterm values.
    const ANSI16: [(u8, u8, u8); 16] = [
        (0x00, 0x00, 0x00),
        (0xcd, 0x00, 0x00),
        (0x00, 0xcd, 0x00),
        (0xcd, 0xcd, 0x00),
        (0x00, 0x00, 0xee),
        (0xcd, 0x00, 0xcd),
        (0x00, 0xcd, 0xcd),
        (0xe5, 0xe5, 0xe5),
        (0x7f, 0x7f, 0x7f),
        (0xff, 0x00, 0x00),
        (0x00, 0xff, 0x00),
        (0xff, 0xff, 0x00),
        (0x5c, 0x5c, 0xff),
        (0xff, 0x00, 0xff),
        (0x00, 0xff, 0xff),
        (0xff, 0xff, 0xff),
    ];
    for (i, &(r, g, b)) in ANSI16.iter().enumerate() {
        p[i] = Rgb::new(r, g, b);
    }

    // 16..=231: 6x6x6 cube. Step values match xterm's table exactly.
    const STEP: [u8; 6] = [0x00, 0x5f, 0x87, 0xaf, 0xd7, 0xff];
    let mut idx = 16usize;
    for &r in &STEP {
        for &g in &STEP {
            for &b in &STEP {
                p[idx] = Rgb::new(r, g, b);
                idx += 1;
            }
        }
    }

    // 232..=255: grayscale ramp, 8 + 10*n (xterm's formula).
    for i in 0..24u8 {
        let v = 8 + i * 10;
        p[232 + i as usize] = Rgb::new(v, v, v);
    }

    p
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
        assert_eq!(theme.resolve(Color::Default, true), [
            theme.default_fg.r as f32 / 255.0,
            theme.default_fg.g as f32 / 255.0,
            theme.default_fg.b as f32 / 255.0,
            1.0,
        ]);
        let rgb = Rgb::new(10, 20, 30);
        assert_eq!(
            theme.resolve(Color::Rgb(rgb), false),
            [10.0 / 255.0, 20.0 / 255.0, 30.0 / 255.0, 1.0]
        );
    }
}
