//! `noa-font` — glyph pipeline: discovery -> shaping -> rasterization -> atlas.
//!
//! Ghostty analog: `font/`. This crate discovers a monospace system font via
//! `font-kit`, rasterizes glyphs with `swash`, and packs the resulting R8
//! coverage masks into an `etagere`-backed CPU atlas that the renderer uploads
//! to the GPU.
//!
//! For inc-1 shaping is trivial (per-`char` charmap lookup); the cache key is
//! just the `char`.

mod atlas;
mod face;
mod grid;
mod raster;

pub use atlas::Atlas;
pub use face::Metrics;
pub use grid::FontGrid;
pub use raster::RasterizedGlyph;

/// Cache key for a rasterized glyph. Inc-1: a single `char`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct GlyphKey {
    pub ch: char,
}

/// Packed glyph record. Field shapes mirror `noa-render`'s `CellInstance` so
/// the renderer can consume them without conversion.
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
}
