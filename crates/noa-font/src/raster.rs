//! Glyph rasterization via swash.

use swash::FontRef;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::Format;

/// A rasterized grayscale (R8 alpha) glyph bitmap plus placement info.
#[derive(Clone, Debug, Default)]
pub struct RasterizedGlyph {
    /// R8 alpha coverage, row-major, `width * height` bytes.
    pub bitmap: Vec<u8>,
    /// Bitmap width in pixels.
    pub width: u32,
    /// Bitmap height in pixels.
    pub height: u32,
    /// Horizontal bearing (left edge offset from the pen origin).
    pub bearing_x: i32,
    /// Vertical bearing (top edge offset from the baseline, y-up).
    pub bearing_y: i32,
    /// Horizontal advance in pixels at the requested size.
    pub advance: f32,
}

/// Rasterize a single glyph to an R8 alpha mask at `px` pixels-per-em.
///
/// Returns an empty (zero-sized) glyph for whitespace / glyphs with no
/// outline coverage; callers can treat that as "nothing to blit".
pub fn rasterize(
    ctx: &mut ScaleContext,
    font: FontRef<'_>,
    glyph_id: u16,
    px: f32,
) -> RasterizedGlyph {
    let advance = font.glyph_metrics(&[]).scale(px).advance_width(glyph_id);

    let mut scaler = ctx.builder(font).size(px).hint(true).build();

    // Prefer alpha bitmap strikes, then outlines. We render a Mask (R8).
    let image = Render::new(&[
        Source::ColorBitmap(StrikeWith::BestFit),
        Source::Bitmap(StrikeWith::BestFit),
        Source::Outline,
    ])
    .format(Format::Alpha)
    .render(&mut scaler, glyph_id);

    let Some(img) = image else {
        return RasterizedGlyph {
            advance,
            ..Default::default()
        };
    };

    let width = img.placement.width;
    let height = img.placement.height;
    let expected = (width as usize) * (height as usize);

    // For Format::Alpha content `data` is 1 byte/px (R8). Some color sources
    // may still emit RGBA; normalize to an R8 coverage mask by taking alpha.
    let bitmap = if img.data.len() == expected {
        img.data
    } else if expected > 0 && img.data.len() == expected * 4 {
        img.data.chunks_exact(4).map(|px| px[3]).collect()
    } else {
        Vec::new()
    };

    if bitmap.len() == expected {
        RasterizedGlyph {
            bitmap,
            width,
            height,
            bearing_x: img.placement.left,
            bearing_y: img.placement.top,
            advance,
        }
    } else {
        // Unexpected layout — treat as an empty (advance-only) glyph.
        RasterizedGlyph {
            advance,
            ..Default::default()
        }
    }
}
