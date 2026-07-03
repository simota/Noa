//! Glyph rasterization via swash.

use swash::FontRef;
use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Format, Transform};

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

/// Faux-bold / faux-italic synthesis knobs for a rasterize call (REQ-SHAPE-7,
/// [Should]). `FontGrid::raster_shaped` decides these from
/// `FontConfig.synthetic_style` + the run's `StyleKey`: noa currently has no
/// separate bold/italic font-family loading (WP0 stores
/// `families_bold`/`families_italic` but nothing resolves a distinct face
/// from them yet), so any requested bold/italic style is treated as "the
/// resolved face lacks the native style" and synthesized whenever the
/// corresponding config toggle is on.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GlyphSynthesis {
    pub embolden: bool,
    pub shear: bool,
}

/// Rasterize a single glyph at `px` pixels-per-em, with no variation-axis
/// coordinates and no synthetic style. Thin wrapper over
/// [`rasterize_with_variations`] for the (still common) unvaried case.
///
/// Returns an empty (zero-sized) glyph for whitespace / glyphs with no
/// outline coverage; callers can treat that as "nothing to blit".
pub fn rasterize(
    ctx: &mut ScaleContext,
    font: FontRef<'_>,
    glyph_id: u16,
    px: f32,
) -> RasterizedGlyph {
    rasterize_with_variations(ctx, font, glyph_id, px, &[], GlyphSynthesis::default())
}

/// Rasterize a single glyph at `px` pixels-per-em, applying `variation_coords`
/// (D1: MUST be the same coords `FontGrid::shape_run` used for shaping this
/// style — see `shape::variation_coords_for`) and optional synthetic-style
/// transforms.
///
/// Color-bitmap glyphs (e.g. emoji `sbix`/CBDT strikes, detected via swash's
/// [`Content::Color`]) keep their full RGBA data (REQ-EMOJI-2); everything
/// else rasterizes to an R8 alpha mask, as before.
///
/// Returns an empty (zero-sized) glyph for whitespace / glyphs with no
/// outline coverage; callers can treat that as "nothing to blit".
pub fn rasterize_with_variations(
    ctx: &mut ScaleContext,
    font: FontRef<'_>,
    glyph_id: u16,
    px: f32,
    variation_coords: &[(u32, f32)],
    synthesis: GlyphSynthesis,
) -> RasterizedGlyph {
    let normalized_coords: Vec<swash::NormalizedCoord> = if variation_coords.is_empty() {
        Vec::new()
    } else {
        font.variations()
            .normalized_coords(variation_coords.iter().copied())
            .collect()
    };

    let advance = font
        .glyph_metrics(&normalized_coords)
        .scale(px)
        .advance_width(glyph_id);

    let mut scaler = ctx
        .builder(font)
        .size(px)
        .hint(true)
        .normalized_coords(normalized_coords.iter().copied())
        .build();

    // Prefer color bitmap strikes, then alpha bitmap strikes, then outlines.
    // `.format(Format::Alpha)` only affects the outline path (Format is
    // meaningless for bitmap strikes, which carry their own native format).
    let mut render = Render::new(&[
        Source::ColorBitmap(StrikeWith::BestFit),
        Source::Bitmap(StrikeWith::BestFit),
        Source::Outline,
    ]);
    render.format(Format::Alpha);
    if synthesis.embolden {
        // A modest embolden strength proportional to size — a best-effort
        // approximation (REQ-SHAPE-7 is [Should]; exact CoreText-matching
        // stroke-width tuning is out of scope for this chain).
        render.embolden(px * 0.02);
    }
    if synthesis.shear {
        // Faux-italic shear. Bitmap strikes (color emoji, pre-rendered
        // bitmap fonts) ignore this — shearing only affects outline
        // rendering, which matches Ghostty/CoreText behavior of never
        // synthesizing italic onto a bitmap strike.
        render.transform(Some(Transform::new(1.0, 0.0, -0.25, 1.0, 0.0, 0.0)));
    }

    let image = render.render(&mut scaler, glyph_id);

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
