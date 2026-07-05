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
/// Blue accent: focus ring, selection, hover — the shared
/// [`noa_render::UI_ACCENT`], so the chrome, the overlay (palette/dialog)
/// selection cues, and the pane focus indicator all read as one hue.
pub const CHROME_ACCENT: Rgb = noa_render::UI_ACCENT;
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

// Shared shape tokens for every rounded chrome/overlay card (logical px,
// scaled at draw time). Three radius steps — small transient chrome (menus,
// buttons), mid surfaces (overview tiles, pills), large elevated cards
// (sidebar cards, command palette) — and one ring-width scale so "hovered <
// selected < needs-attention" reads consistently across surfaces.
pub const RADIUS_SM: f32 = 6.0;
pub const RADIUS_MD: f32 = 8.0;
pub const RADIUS_LG: f32 = 10.0;
/// Thin accent border over a hovered (not selected) card.
pub const RING_HOVER: f32 = 1.5;
/// The selected/focused card's accent ring.
pub const RING_SELECTED: f32 = 2.0;
/// The red needs-attention ring — thicker than selection, paired with
/// [`GLOW_ATTENTION`], so a pending interaction request is unmissable.
pub const RING_ATTENTION: f32 = 2.5;
/// Outer glow radius accompanying [`RING_SELECTED`].
pub const GLOW_SELECTED: f32 = 8.0;
/// Outer glow radius accompanying [`RING_ATTENTION`].
pub const GLOW_ATTENTION: f32 = 12.0;

/// The full chrome color set as one value, so the sidebar and overview can
/// follow the terminal theme's light/dark polarity (a light theme gets light
/// chrome) instead of staying hardwired dark. [`CHROME_DARK`] reproduces the
/// individual `CHROME_*` constants above exactly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChromePalette {
    pub bg: Rgb,
    pub card: Rgb,
    pub card_selected: Rgb,
    pub band: Rgb,
    pub border: Rgb,
    pub divider: Rgb,
    pub accent: Rgb,
    pub pill: Rgb,
    pub pill_border: Rgb,
    pub fg: Rgb,
    pub dim_fg: Rgb,
    pub dot_blue: Rgb,
    pub dot_green: Rgb,
    pub dot_yellow: Rgb,
    pub dot_red: Rgb,
}

/// The original dark chrome — byte-identical to the `CHROME_*` constants.
pub const CHROME_DARK: ChromePalette = ChromePalette {
    bg: CHROME_BG,
    card: CHROME_CARD,
    card_selected: CHROME_CARD_SELECTED,
    band: CHROME_BAND,
    border: CHROME_BORDER,
    divider: CHROME_DIVIDER,
    accent: CHROME_ACCENT,
    pill: CHROME_PILL,
    pill_border: CHROME_PILL_BORDER,
    fg: CHROME_FG,
    dim_fg: CHROME_DIM_FG,
    dot_blue: CHROME_DOT_BLUE,
    dot_green: CHROME_DOT_GREEN,
    dot_yellow: CHROME_DOT_YELLOW,
    dot_red: CHROME_DOT_RED,
};

/// Light-polarity chrome for light terminal themes: the same relationships as
/// the dark set (backdrop < card < selected card, hairline seam, dim vs
/// primary text) mirrored around a light neutral, with the status-dot hues
/// darkened enough to keep ≥3:1 contrast against the light card face.
pub const CHROME_LIGHT: ChromePalette = ChromePalette {
    bg: Rgb::new(0xec, 0xee, 0xf4),
    card: Rgb::new(0xf7, 0xf8, 0xfb),
    card_selected: Rgb::new(0xe3, 0xeb, 0xf8),
    band: Rgb::new(0xe2, 0xe5, 0xee),
    border: Rgb::new(0xb8, 0xbf, 0xcf),
    divider: Rgb::new(0xdf, 0xe2, 0xec),
    accent: CHROME_ACCENT,
    pill: Rgb::new(0xe8, 0xea, 0xf2),
    pill_border: Rgb::new(0xc2, 0xc8, 0xda),
    fg: Rgb::new(0x23, 0x29, 0x3a),
    dim_fg: Rgb::new(0x6a, 0x72, 0x84),
    dot_blue: Rgb::new(0x2f, 0x7f, 0xe0),
    dot_green: Rgb::new(0x2c, 0x9e, 0x50),
    dot_yellow: Rgb::new(0xb9, 0x8a, 0x1e),
    dot_red: Rgb::new(0xe0, 0x31, 0x31),
};

/// The chrome polarity chosen from the resolved terminal theme, set once at
/// GPU/theme init (before any chrome surface draws). The theme itself is
/// startup-fixed, so a `OnceLock` matches its lifetime; a second call (a
/// second window reusing the shared GPU) is a no-op.
static ACTIVE_PALETTE: std::sync::OnceLock<ChromePalette> = std::sync::OnceLock::new();

/// Select light or dark chrome from the terminal theme's polarity.
pub fn select_palette(theme_is_light: bool) {
    let _ = ACTIVE_PALETTE.set(if theme_is_light {
        CHROME_LIGHT
    } else {
        CHROME_DARK
    });
}

/// The active chrome palette (dark until [`select_palette`] runs).
pub fn palette() -> &'static ChromePalette {
    ACTIVE_PALETTE.get().unwrap_or(&CHROME_DARK)
}

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
