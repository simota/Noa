//! Glyph rasterization via swash.

use swash::FontRef;
use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::Format;

/// A rasterized glyph bitmap plus placement info.
///
/// `bitmap` is R8 alpha coverage (`width * height` bytes) when `color` is
/// `false`, or RGBA8 color data (`width * height * 4` bytes) when `color` is
/// `true` (REQ-EMOJI-2) — e.g. Apple Color Emoji's `sbix` bitmap strikes.
#[derive(Clone, Debug, Default)]
pub struct RasterizedGlyph {
    /// R8 alpha coverage, or RGBA8 color data when `color` is `true`.
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
    /// `true` when `bitmap` is RGBA8 color data (belongs in the color atlas,
    /// sampled as passthrough); `false` for the R8 alpha mask path.
    pub color: bool,
}

/// Rasterize a single glyph at `px` pixels-per-em.
///
/// Color-bitmap glyphs (e.g. emoji `sbix`/CBDT strikes, detected via swash's
/// [`Content::Color`]) keep their full RGBA data (REQ-EMOJI-2); everything
/// else rasterizes to an R8 alpha mask, as before.
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

    // Prefer color bitmap strikes, then alpha bitmap strikes, then outlines.
    // `.format(Format::Alpha)` only affects the outline path (Format is
    // meaningless for bitmap strikes, which carry their own native format).
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
    let expected_px = (width as usize) * (height as usize);

    if img.content == Content::Color {
        // Color bitmap strike: keep the full RGBA data, no alpha reduction.
        if expected_px > 0 && img.data.len() == expected_px * 4 {
            return RasterizedGlyph {
                bitmap: img.data,
                width,
                height,
                bearing_x: img.placement.left,
                bearing_y: img.placement.top,
                advance,
                color: true,
            };
        }
        // Unexpected layout — treat as an empty (advance-only) glyph.
        return RasterizedGlyph {
            advance,
            ..Default::default()
        };
    }

    // Non-color path: normalize to an R8 coverage mask. Some non-color
    // bitmap sources may still emit RGBA; take the alpha channel.
    let bitmap = if img.data.len() == expected_px {
        img.data
    } else if expected_px > 0 && img.data.len() == expected_px * 4 {
        img.data.chunks_exact(4).map(|px| px[3]).collect()
    } else {
        Vec::new()
    };

    if bitmap.len() == expected_px {
        RasterizedGlyph {
            bitmap,
            width,
            height,
            bearing_x: img.placement.left,
            bearing_y: img.placement.top,
            advance,
            color: false,
        }
    } else {
        // Unexpected layout — treat as an empty (advance-only) glyph.
        RasterizedGlyph {
            advance,
            ..Default::default()
        }
    }
}
