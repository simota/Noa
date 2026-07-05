//! Shared chrome palette for noa's own UI surfaces (session sidebar, tab
//! overview). Both surfaces previously carried private near-duplicate color
//! tables; this module is the single source so the dot semantics, attention
//! treatment, and card face colors stay visually unified. GUI-agnostic (no
//! `winit`/`wgpu`): plain `Rgb` values plus a const converter to the straight
//! display-space RGBA the overview's non-sRGB surface expects.

use noa_core::Rgb;

/// Near-black navy backdrop behind every card (overview mockup: "暗色の背景").
pub const CHROME_BG: Rgb = Rgb::new(0x09, 0x0c, 0x15);
/// Card face — one step lighter than [`CHROME_BG`].
pub const CHROME_CARD: Rgb = Rgb::new(0x14, 0x17, 0x20);
/// The selected card's background — brighter still, paired with the accent ring.
pub const CHROME_CARD_SELECTED: Rgb = Rgb::new(0x1f, 0x25, 0x33);
/// Title-bar / pill band — distinguishable from the card face.
pub const CHROME_BAND: Rgb = Rgb::new(0x1e, 0x21, 0x2d);
/// Thin resting card border.
pub const CHROME_BORDER: Rgb = Rgb::new(0x4c, 0x51, 0x61);
/// Hairline seam between a chrome surface and the terminal panes — only a
/// hair lighter than [`CHROME_BG`] so the edge reads as a faint depth cue
/// rather than a drawn line competing with the card strokes.
pub const CHROME_DIVIDER: Rgb = Rgb::new(0x14, 0x18, 0x22);
/// Blue accent: focus ring, selection, hover.
pub const CHROME_ACCENT: Rgb = Rgb::new(0x14, 0xa2, 0xff);
/// Chrome pill face (overview search / hint bars, sidebar menu popup).
pub const CHROME_PILL: Rgb = Rgb::new(0x21, 0x23, 0x36);
/// Thin border around chrome pills.
pub const CHROME_PILL_BORDER: Rgb = Rgb::new(0x40, 0x46, 0x64);
/// Primary chrome text.
pub const CHROME_FG: Rgb = Rgb::new(0xd8, 0xdc, 0xe4);
/// Secondary/dim chrome text.
pub const CHROME_DIM_FG: Rgb = Rgb::new(0x8a, 0x90, 0x9c);

// Status-dot semantics shared by the sidebar cards and the overview title
// bands (FR-11/FR-16): blue = busy, green = idle, yellow = unread bell,
// red = pending attention (a program awaits the user's reply).
pub const CHROME_DOT_BLUE: Rgb = Rgb::new(0x4c, 0x9a, 0xff);
pub const CHROME_DOT_GREEN: Rgb = Rgb::new(0x46, 0xc4, 0x66);
pub const CHROME_DOT_YELLOW: Rgb = Rgb::new(0xe6, 0xb4, 0x50);
pub const CHROME_DOT_RED: Rgb = Rgb::new(0xff, 0x4d, 0x4d);

/// Convert a chrome `Rgb` to straight display-space RGBA. The overview and
/// sidebar surfaces use non-sRGB formats (`Bgra8Unorm`), so the components
/// are a plain `/255` with no gamma re-encode.
pub const fn rgba(color: Rgb) -> [f32; 4] {
    [
        color.r as f32 / 255.0,
        color.g as f32 / 255.0,
        color.b as f32 / 255.0,
        1.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rgba_maps_channels_to_unit_range() {
        assert_eq!(rgba(Rgb::new(0, 0, 0)), [0.0, 0.0, 0.0, 1.0]);
        assert_eq!(rgba(Rgb::new(255, 255, 255)), [1.0, 1.0, 1.0, 1.0]);
        let mid = rgba(CHROME_ACCENT);
        assert!((mid[0] - 0x14 as f32 / 255.0).abs() < f32::EPSILON);
        assert_eq!(mid[3], 1.0);
    }

    // This module is shared by GUI-agnostic pure modules; keep it free of
    // windowing/GPU imports (same rule as sidebar.rs / session_store.rs).
    #[test]
    fn chrome_is_gui_agnostic() {
        let source = include_str!("chrome.rs");
        for forbidden in [
            ["use ", "winit"].concat(),
            ["use ", "wgpu"].concat(),
            ["winit", "::"].concat(),
            ["wgpu", "::"].concat(),
        ] {
            assert!(
                !source.contains(&forbidden),
                "chrome.rs must not reference `{forbidden}`"
            );
        }
    }
}
