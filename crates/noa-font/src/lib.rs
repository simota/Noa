//! `noa-font` — glyph pipeline: discovery -> shaping -> rasterization -> atlas.
//!
//! Ghostty analog: `font/`. This crate discovers a monospace system font
//! (plus a fallback stack, including Apple Color Emoji) via `font-kit`,
//! shapes text runs with `rustybuzz` (pure-Rust HarfBuzz port), rasterizes
//! glyphs with `swash`, and packs them into two `etagere`-backed CPU atlases
//! that the renderer uploads to the GPU: an R8 coverage-mask atlas for
//! regular glyphs, and an RGBA8 atlas for color-bitmap glyphs (emoji)
//! sampled as passthrough (`GlyphInfo::color`).
//!
//! [`FontGrid::get_or_raster`] remains the simple per-`char` cache path
//! (used for glyphs outside a shaped run); [`FontGrid::shape_run`] +
//! [`FontGrid::raster_shaped`] (WP2) are the run-based shape/raster path —
//! see the `shape` module for the frozen `ShapeCell`/`ShapedGlyph` seam
//! `noa-render` consumes.

mod atlas;
mod boxdraw;
mod config;
mod face;
mod grid;
mod raster;
mod shape;

pub use atlas::Atlas;
pub use config::{AlphaBlending, FontConfig, FontFeature, FontVariation, SyntheticStyle};
pub use face::{Metrics, list_families};
pub use grid::FontGrid;
pub use raster::{GlyphSynthesis, RasterizedGlyph};
pub use shape::{FaceId, ShapeCell, ShapedGlyph, StyleKey};

/// Cache key for a rasterized glyph. Inc-1: a single `char`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GlyphKey {
    pub ch: char,
}

/// Packed glyph record. Field shapes mirror `noa-render`'s `CellInstance`, but
/// the vertical bearing remains pen-relative and is converted by the renderer.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct GlyphInfo {
    /// Top-left of the glyph in the atlas, in pixels `[x, y]`.
    pub atlas_pos: [u16; 2],
    /// Glyph size in the atlas, in pixels `[w, h]`. `[0, 0]` = nothing to draw.
    pub atlas_size: [u16; 2],
    /// Pen bearing `[left, top]` (top is y-up from the baseline).
    pub bearing: [i16; 2],
    /// Horizontal advance in pixels.
    pub advance: f32,
    /// `true` when this glyph lives in the RGBA8 color atlas (sampled as
    /// passthrough, no foreground tint); `false` for the R8 mask atlas.
    pub color: bool,
}

/// Errors from font discovery / loading.
#[derive(Debug, thiserror::Error)]
pub enum FontError {
    /// No suitable monospace font could be located or read.
    #[error("no suitable monospace font found")]
    NoFont,
    /// The font bytes could not be parsed by swash.
    #[error("failed to parse font data")]
    Parse,
    /// The system font source could not enumerate its families.
    #[error("failed to enumerate system font families")]
    Enumerate,
}
