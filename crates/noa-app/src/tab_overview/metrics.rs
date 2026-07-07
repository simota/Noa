use noa_core::GridPadding;
use std::time::Duration;

/// Spec-locked maximum number of live thumbnail tiles in the overview grid.
pub const OVERVIEW_GRID_CAP: usize = 9;

/// Spec-locked 10Hz throttle for thumbnail regeneration.
pub const OVERVIEW_TILE_MIN_RENDER_INTERVAL: Duration = Duration::from_millis(100);

/// Per-frame cap for offscreen tile work. The render path is sequential, but
/// this keeps one overview frame from doing unbounded terminal locks.
pub const OVERVIEW_MAX_RENDER_TILES_PER_FRAME: usize = 2;

/// Spec-locked gap between adjacent tiles (REQ-OV-11, mockup parity v2) —
/// roughly 4% of a typical tile width. Compile-time constant, no config knob
/// (⚠G precedent: v1's throttle is likewise fixed rather than tunable).
///
/// This and every `OVERVIEW_*` dimension below are **design metrics at scale
/// 1.0**; the overlay lays out in physical pixels, so production reads them
/// through [`OverviewMetrics`] (DPR-multiplied), mirroring `SidebarMetrics`.
pub const OVERVIEW_TILE_GUTTER: u32 = 18;

/// Spec-locked margin between the tile grid and the Overview window bounds
/// (REQ-OV-11).
pub const OVERVIEW_OUTER_MARGIN: u32 = 26;

/// Title-bar band height rendered at the top of every overview tile, live or
/// placeholder (REQ-OV-12/REQ-OV-13). Compile-time constant.
pub const OVERVIEW_TITLE_BAR_H: u32 = 30;

/// Height reserved at the *top* of the Overview window for the "Search sessions"
/// field (REQ-OV-16). v2/P2 only *reserves* this band in the grid-bounds math
/// so P3's search-field draw doesn't reflow the grid; P2 draws nothing here.
/// Compile-time constant (⚠G precedent: no config knob).
pub const OVERVIEW_SEARCH_BAND_H: u32 = 64;

/// Height reserved at the *bottom* of the Overview window for the hint bar
/// (REQ-OV-17). Compile-time constant.
pub const OVERVIEW_HINT_BAND_H: u32 = 54;

/// Mockup-parity chrome palette (REQ-OV-12/14, v2) — no config knob (⚠G
/// precedent), but the light/dark polarity follows the terminal theme via the
/// shared [`crate::chrome`] palette (selected once at startup), so the
/// overview and the session sidebar stay visually unified. Returned as
/// straight display-space RGBA because the Overview surface uses a
/// **non-sRGB** format (`Bgra8Unorm`, see `preferred_surface_format`), so
/// these are written to the target unchanged (no gamma re-encode).
///
/// Backdrop behind every card (mockup: "暗色の背景").
pub fn overview_bg_color() -> [f32; 4] {
    crate::chrome::rgba(crate::chrome::palette().bg)
}
/// Card face — one step lighter than [`overview_bg_color`] (mockup: "一段明るいカード面").
pub fn overview_card_color() -> [f32; 4] {
    crate::chrome::rgba(crate::chrome::palette().card)
}
/// Title-bar band — distinguishable from the card face (mockup: "区別可能な帯").
pub fn overview_title_bar_color() -> [f32; 4] {
    crate::chrome::rgba(crate::chrome::palette().band)
}
/// Thin resting card border.
pub fn overview_border_color() -> [f32; 4] {
    crate::chrome::rgba(crate::chrome::palette().border)
}
/// Blue accent focus ring for the selected tile (REQ-OV-14).
pub fn overview_focus_ring_color() -> [f32; 4] {
    crate::chrome::rgba(crate::chrome::palette().accent)
}
/// Search / hint pill face in the overview chrome.
pub fn overview_chrome_pill_color() -> [f32; 4] {
    crate::chrome::rgba(crate::chrome::palette().pill)
}
/// Thin border around search and hint pills.
pub fn overview_chrome_border_color() -> [f32; 4] {
    crate::chrome::rgba(crate::chrome::palette().pill_border)
}
/// Corner radius (px) of every card — the shared mid-size chrome radius.
pub const OVERVIEW_CARD_CORNER_RADIUS: f32 = crate::chrome::RADIUS_MD;
/// Resting border thickness (px).
pub const OVERVIEW_CARD_BORDER_WIDTH: f32 = 1.0;
/// Focus-ring thickness (px) — thicker than the resting border so the
/// selection reads as a single bright ring inside the separate outer glow.
pub const OVERVIEW_CARD_FOCUS_WIDTH: f32 = crate::chrome::RING_SELECTED;
/// Selected-card glow radius outside the card edge.
pub const OVERVIEW_CARD_FOCUS_GLOW_WIDTH: f32 = crate::chrome::GLOW_SELECTED;
/// Rounded search-field size within [`OverviewChrome::search_band`].
pub const OVERVIEW_SEARCH_FIELD_H: u32 = 34;
pub const OVERVIEW_SEARCH_FIELD_MIN_W: u32 = 180;
pub const OVERVIEW_SEARCH_FIELD_MAX_W: u32 = 320;
/// Rounded bottom hint-bar size within [`OverviewChrome::hint_band`].
pub const OVERVIEW_HINT_BAR_H: u32 = 32;
pub const OVERVIEW_HINT_BAR_MIN_W: u32 = 320;
pub const OVERVIEW_HINT_BAR_MAX_W: u32 = 460;

/// Width of the close (✕) button's clickable region at the title bar's right
/// edge (REQ-OV-13). Square with the title bar.
const OVERVIEW_CLOSE_BUTTON_W: u32 = OVERVIEW_TITLE_BAR_H;

/// Every Overview chrome dimension resolved for one window's scale factor
/// (DPR). The `OVERVIEW_*` constants are the design metrics at scale 1.0
/// (mockup parity); the overlay lays out in *physical* pixels
/// (`window.inner_size()`) while its fonts are DPR-scaled
/// (`font_pixel_size`), so every band, pill, and ring must scale by the same
/// factor or a Retina band is half its intended size and clips its text
/// (sidebar precedent: `SidebarMetrics`). Construct once per frame from
/// `window.scale_factor()`. Pure and `Copy`, so layout and hit-tests stay
/// unit-testable at any scale without a window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OverviewMetrics {
    scale: f32,
    /// Title-bar band height at the top of every tile (scaled).
    pub title_bar_h: u32,
    /// Reserved top search band height (scaled).
    pub search_band_h: u32,
    /// Reserved bottom hint band height (scaled).
    pub hint_band_h: u32,
    /// Gap between adjacent tiles (scaled).
    pub tile_gutter: u32,
    /// Margin between the tile grid and the window bounds (scaled).
    pub outer_margin: u32,
    /// Rounded search-field size within the search band (scaled).
    pub search_field_h: u32,
    pub search_field_min_w: u32,
    pub search_field_max_w: u32,
    /// Rounded hint-bar size within the hint band (scaled).
    pub hint_bar_h: u32,
    pub hint_bar_min_w: u32,
    pub hint_bar_max_w: u32,
    /// Close (✕) button clickable width (scaled; square with the title bar).
    pub close_button_w: u32,
    /// Card corner radius / border / focus-ring widths (scaled).
    pub card_corner_radius: f32,
    pub card_border_width: f32,
    pub card_focus_width: f32,
    pub card_focus_glow_width: f32,
}

impl OverviewMetrics {
    /// Resolve the design metrics for `scale` (a non-finite or non-positive
    /// value falls back to 1.0).
    pub fn new(scale: f32) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        let s = |v: u32| ((v as f32) * scale).round() as u32;
        Self {
            scale,
            title_bar_h: s(OVERVIEW_TITLE_BAR_H),
            search_band_h: s(OVERVIEW_SEARCH_BAND_H),
            hint_band_h: s(OVERVIEW_HINT_BAND_H),
            tile_gutter: s(OVERVIEW_TILE_GUTTER),
            outer_margin: s(OVERVIEW_OUTER_MARGIN),
            search_field_h: s(OVERVIEW_SEARCH_FIELD_H),
            search_field_min_w: s(OVERVIEW_SEARCH_FIELD_MIN_W),
            search_field_max_w: s(OVERVIEW_SEARCH_FIELD_MAX_W),
            hint_bar_h: s(OVERVIEW_HINT_BAR_H),
            hint_bar_min_w: s(OVERVIEW_HINT_BAR_MIN_W),
            hint_bar_max_w: s(OVERVIEW_HINT_BAR_MAX_W),
            close_button_w: s(OVERVIEW_CLOSE_BUTTON_W),
            card_corner_radius: OVERVIEW_CARD_CORNER_RADIUS * scale,
            card_border_width: OVERVIEW_CARD_BORDER_WIDTH * scale,
            card_focus_width: OVERVIEW_CARD_FOCUS_WIDTH * scale,
            card_focus_glow_width: OVERVIEW_CARD_FOCUS_GLOW_WIDTH * scale,
        }
    }

    /// The DPR this metrics set was resolved for — for scaling the few ring
    /// widths that live at their `crate::chrome` call sites (hover/attention).
    pub fn scale(&self) -> f32 {
        self.scale
    }
}

/// Fixed horizontal inset (design px at scale 1.0) between a chrome band's
/// edge and its label row.
const OVERVIEW_LABEL_PAD_X: f32 = 10.0;

/// Interior padding for a chrome-band label row: the fixed horizontal inset
/// plus a top inset that vertically centers the single cell row within the
/// band (`cell_h` is the label font's physical cell height; `band_h` the
/// band's physical height).
pub fn overview_label_padding(band_h: u32, cell_h: f32, scale: f32) -> GridPadding {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let pad_x = OVERVIEW_LABEL_PAD_X * scale;
    let top = ((band_h as f32 - cell_h) / 2.0).max(0.0);
    GridPadding::new(top, pad_x, 0.0, pad_x)
}
