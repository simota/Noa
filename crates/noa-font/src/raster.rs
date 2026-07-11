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
    /// Dilate the R8 coverage mask to emulate macOS CoreText / Ghostty
    /// `font-thicken` stem-darkening. Unlike `embolden` (per-style faux-bold),
    /// this applies to every non-color glyph regardless of weight — swash has
    /// no native CoreText smoothing, so without it noa renders thinner than
    /// Ghostty. Never applied to color (emoji) bitmaps.
    pub thicken: bool,
    /// Thicken intensity `0..=255` (Ghostty `font-thicken-strength`); `0` is a
    /// no-op. Ignored when `thicken` is false.
    pub thicken_strength: u8,
}

/// Thicken strokes by *stem-darkening* the R8 coverage mask (see
/// [`GlyphSynthesis::thicken`]): raise each partial-coverage pixel toward full
/// opacity via a gamma curve `out = coverage^gamma` (`gamma < 1`). This is how
/// CoreText/FreeType make text appear heavier — it darkens the anti-aliased
/// ramp so stems read bolder, but leaves fully-off (`0`) and fully-on (`255`)
/// pixels untouched, so glyph edges stay **crisp** (a morphological dilation
/// would instead bleed coverage into black neighbours and blur the edges).
///
/// `strength` (`font-thicken-strength`, `0..=255`) sets how aggressive the
/// curve is; `0` is a no-op. Returns the mask unchanged when there is nothing
/// to darken.
fn thicken_mask(bitmap: &[u8], strength: u8) -> Vec<u8> {
    if strength == 0 {
        return bitmap.to_vec();
    }
    // gamma in (0.33 ..= 1.0): full strength → 0.33, tapering to 1.0 (identity)
    // at strength 0. The 0.33 endpoint is *measured*, not guessed: rendering a
    // screen full of Fira Code 16 white-on-dark text and comparing mean glyph
    // luminance against Ghostty (`native` CoreText blending) on the same
    // display, noa's default (strength 255) landed at ~73 vs Ghostty's ~76 —
    // visibly lighter. A gamma sweep on that harness put strength 255 → 0.33 at
    // parity (~76, matching bright-pixel fraction too) while staying crisp with
    // open counters down to 10 pt (no blob at terminal sizes).
    let gamma = 1.0 - 0.67 * (strength as f32 / 255.0);
    let mut lut = [0u8; 256];
    for (c, slot) in lut.iter_mut().enumerate() {
        let norm = c as f32 / 255.0;
        *slot = (norm.powf(gamma) * 255.0).round().clamp(0.0, 255.0) as u8;
    }
    bitmap.iter().map(|&c| lut[c as usize]).collect()
}

/// Render `glyph_id` at `px` pixels-per-em once, returning swash's raw image
/// (if any) plus the font's pen advance at that size. Factored out of
/// [`rasterize_with_variations`] so the fit-to-cell path below can re-invoke
/// it at a smaller `px` without duplicating the scaler/render setup.
fn render_glyph_once(
    ctx: &mut ScaleContext,
    font: FontRef<'_>,
    glyph_id: u16,
    px: f32,
    normalized_coords: &[swash::NormalizedCoord],
    synthesis: GlyphSynthesis,
) -> (Option<swash::scale::image::Image>, f32) {
    let advance = font
        .glyph_metrics(normalized_coords)
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

    (render.render(&mut scaler, glyph_id), advance)
}

/// Normalize a non-color swash image into an R8 coverage mask
/// [`RasterizedGlyph`], applying stem-darkening if configured. Returns an
/// empty (advance-only) glyph on an unexpected data layout.
fn mask_glyph_from_image(
    img: swash::scale::image::Image,
    advance: f32,
    synthesis: GlyphSynthesis,
) -> RasterizedGlyph {
    let width = img.placement.width;
    let height = img.placement.height;
    let expected_px = (width as usize) * (height as usize);

    // Some non-color bitmap sources may still emit RGBA; take the alpha
    // channel.
    let mut bitmap = if img.data.len() == expected_px {
        img.data
    } else if expected_px > 0 && img.data.len() == expected_px * 4 {
        img.data.chunks_exact(4).map(|px| px[3]).collect()
    } else {
        Vec::new()
    };

    if bitmap.len() != expected_px {
        // Unexpected layout — treat as an empty (advance-only) glyph.
        return RasterizedGlyph {
            advance,
            ..Default::default()
        };
    }

    if synthesis.thicken && expected_px > 0 {
        bitmap = thicken_mask(&bitmap, synthesis.thicken_strength);
    }
    RasterizedGlyph {
        bitmap,
        width,
        height,
        bearing_x: img.placement.left,
        bearing_y: img.placement.top,
        advance,
        color: false,
    }
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
/// `fit_width`, when given, is the glyph's allotted span in device pixels
/// (`span_cells * cell_w`, per the source cell's `unicode-width`). Some
/// macOS fallback faces ship metrics sized for a wider layout than noa's
/// grid gives the codepoint (e.g. East Asian Ambiguous-width symbols like
/// `①`), so their outline advance overshoots the cell(s) and bleeds into a
/// neighbor. When the rasterized advance exceeds `fit_width` by more than a
/// 10% tolerance (headroom for ordinary ink overshoot, e.g. `x`'s slightly
/// negative bearing), the glyph is uniformly downscaled — re-rendered at a
/// smaller `px` so hinting stays correct at the new size, kitty-style —
/// and centered in its allotted span. Color bitmap strikes are never
/// downscaled (guarded below regardless of `fit_width`, since they resolve
/// to a fixed strike and are already sized for a 2-cell span with headroom
/// to spare); this is noa's own polish on top of the Ghostty parity target.
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
    fit_width: Option<f32>,
) -> RasterizedGlyph {
    let normalized_coords: Vec<swash::NormalizedCoord> = if variation_coords.is_empty() {
        Vec::new()
    } else {
        font.variations()
            .normalized_coords(variation_coords.iter().copied())
            .collect()
    };

    let (image, advance) =
        render_glyph_once(ctx, font, glyph_id, px, &normalized_coords, synthesis);

    let Some(img) = image else {
        return RasterizedGlyph {
            advance,
            ..Default::default()
        };
    };

    if img.content == Content::Color {
        // Color bitmap strike: keep the full RGBA data, no alpha reduction,
        // and never fit-scaled (see doc comment above).
        let width = img.placement.width;
        let height = img.placement.height;
        let expected_px = (width as usize) * (height as usize);
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

    if let Some(fit) = fit_width
        && fit > 0.0
        && advance > fit * 1.1
    {
        let scale = fit / advance;
        let scaled_px = px * scale;
        let (scaled_image, scaled_advance) =
            render_glyph_once(ctx, font, glyph_id, scaled_px, &normalized_coords, synthesis);
        // A shrunk render can only still resolve to an outline/alpha-bitmap
        // source (color strikes are picked by fixed-size `StrikeWith::BestFit`
        // regardless of `px`, and the un-scaled render above already proved
        // this glyph is not one) — the `Content::Color` check is defense in
        // depth, not a path this can actually take.
        if let Some(scaled_img) = scaled_image
            && scaled_img.content != Content::Color
        {
            let mut glyph = mask_glyph_from_image(scaled_img, scaled_advance, synthesis);
            let extra = fit - glyph.advance;
            if extra > 0.0 {
                glyph.bearing_x += (extra / 2.0).round() as i32;
            }
            return glyph;
        }
        // Scaled render failed to produce anything — fall through and use
        // the original, unscaled image rather than drop the glyph entirely.
    }

    mask_glyph_from_image(img, advance, synthesis)
}

#[cfg(test)]
mod tests {
    use super::thicken_mask;

    #[test]
    fn thicken_strength_zero_is_identity() {
        let mask = [0u8, 128, 64, 255, 30, 200];
        assert_eq!(thicken_mask(&mask, 0), mask);
    }

    #[test]
    fn thicken_keeps_edges_crisp_and_darkens_the_ramp() {
        // Off and fully-on pixels must be preserved exactly (no edge bleed);
        // partial-coverage pixels must gain opacity (heavier stems).
        let mask = [0u8, 64, 128, 200, 255];
        let out = thicken_mask(&mask, 255);
        assert_eq!(out[0], 0, "off pixels stay black — crisp outer edge");
        assert_eq!(out[4], 255, "saturated pixels stay full — crisp inner");
        for i in 1..=3 {
            assert!(
                out[i] > mask[i],
                "partial pixel {i} must darken: {} -> {}",
                mask[i],
                out[i]
            );
        }
    }

    #[test]
    fn thicken_is_monotonic_in_strength() {
        let mask = [128u8];
        let weak = thicken_mask(&mask, 64)[0];
        let strong = thicken_mask(&mask, 255)[0];
        assert!(
            strong > weak && weak >= mask[0],
            "stronger thicken must darken more: base={} weak={weak} strong={strong}",
            mask[0]
        );
    }
}
