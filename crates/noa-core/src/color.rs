//! Terminal color model.
//!
//! Mirrors Ghostty's fg/bg color slots. Palette resolution (index -> concrete
//! RGB via the active theme) happens at render time, not here.

/// A 24-bit RGB triple.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// A cell foreground/background color.
///
/// * `Default` — resolve to the theme's default fg/bg.
/// * `Palette(n)` — index into the 256-color palette (16 ANSI + 240).
/// * `Rgb(_)` — 24-bit truecolor.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Color {
    #[default]
    Default,
    Palette(u8),
    Rgb(Rgb),
}
