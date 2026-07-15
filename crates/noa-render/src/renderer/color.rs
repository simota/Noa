//! Split out of the former monolithic `renderer.rs` — color-space conversions and opacity helpers.
//! Shares the parent module namespace via `use super::*`.

use super::*;

/// Scale a clear color's alpha by `background-opacity`, leaving rgb intact.
/// Only the clear color carries the setting: it fills the window padding and
/// every default-background cell (those emit no bg quad, so the clear shows
/// through). Explicit-bg / selection / cursor quads keep alpha 1.0 and stay
/// opaque. With the surface in `PostMultiplied` alpha mode this makes the
/// default-bg regions translucent while inked glyphs — whose coverage pushes
/// the framebuffer alpha back toward 1.0 through `ALPHA_BLENDING` — stay solid.
pub(super) fn apply_background_opacity(clear_color: [f32; 4], opacity: f32) -> [f32; 4] {
    let mut out = clear_color;
    out[3] = clear_color[3] * opacity.clamp(0.0, 1.0);
    out
}

/// The exact clear color the cell pass would use for a frame whose default
/// background is the theme background — for the startup pre-first-frame
/// solid paint (window shown before the renderer/font exist). Routes through
/// the same [`surface_output_rgba`] sRGB handling and the same
/// `background-opacity` alpha scaling as [`Renderer::draw_panes`]'s clear, so
/// the early solid frame is byte-identical to the first real frame's
/// background (no flash-of-wrong-color when the real frame replaces it).
pub fn startup_clear_color(
    theme: &Theme,
    target_format_is_srgb: bool,
    background_opacity: f32,
) -> wgpu::Color {
    let c = apply_background_opacity(
        surface_output_rgba(crate::theme::rgba(theme.default_bg), target_format_is_srgb),
        background_opacity,
    );
    wgpu::Color {
        r: f64::from(c[0]),
        g: f64::from(c[1]),
        b: f64::from(c[2]),
        a: f64::from(c[3]),
    }
}

pub(super) fn to_u8_color(c: [f32; 4]) -> [u8; 4] {
    [
        (c[0].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[1].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[2].clamp(0.0, 1.0) * 255.0).round() as u8,
        (c[3].clamp(0.0, 1.0) * 255.0).round() as u8,
    ]
}

pub(super) fn surface_output_rgba(c: [f32; 4], target_format_is_srgb: bool) -> [f32; 4] {
    if !target_format_is_srgb {
        return c;
    }

    [
        srgb_to_linear(c[0]),
        srgb_to_linear(c[1]),
        srgb_to_linear(c[2]),
        c[3].clamp(0.0, 1.0),
    ]
}

pub(super) fn srgb_to_linear(channel: f32) -> f32 {
    let channel = channel.clamp(0.0, 1.0);
    let scaled = channel * 255.0;
    let rounded = scaled.round();
    if (scaled - rounded).abs() <= 0.0001 {
        return srgb_to_linear_u8_lut()[rounded as usize];
    }

    srgb_to_linear_exact(channel)
}

pub(super) fn srgb_to_linear_u8_lut() -> &'static [f32; 256] {
    static LUT: OnceLock<[f32; 256]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = [0.0; 256];
        for (idx, slot) in lut.iter_mut().enumerate() {
            *slot = srgb_to_linear_exact(idx as f32 / 255.0);
        }
        lut
    })
}

pub(super) fn srgb_to_linear_exact(channel: f32) -> f32 {
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

pub(super) fn glyph_cell_bearing(metrics: Metrics, pen_bearing: [i16; 2]) -> [i16; 2] {
    [
        pen_bearing[0],
        (metrics.ascent.round() - pen_bearing[1] as f32) as i16,
    ]
}
