//! Surface-format/alpha, focus reporting, font sizing, and IME helpers.

use super::*;


/// Choose the swapchain surface format, preferring a **non-sRGB** format
/// (`Bgra8Unorm`) over an sRGB one (`Bgra8UnormSrgb`).
///
/// This is the WP3 (REQ-AA-1) "native gamma-correct AA" fix. When the
/// surface format `.is_srgb()`, the GPU's fixed-function alpha blend unit
/// decodes stored texels to linear before blending and re-encodes to sRGB
/// on write — so `wgpu::BlendState::ALPHA_BLENDING` (`pipeline.rs`) executes
/// in **linear** space. That's a different blend space than Ghostty's
/// `native` macOS text-rendering mode, which blends glyph coverage against
/// the background directly in gamma-encoded space (how CoreText/FreeType
/// render by default) — the mismatch visibly thins dark-on-light glyph
/// edges relative to Ghostty.
///
/// Preferring a non-sRGB surface format makes all blending — solid
/// backgrounds, selection highlights, and glyph coverage — happen in gamma
/// space, matching `native`. This is in lockstep with
/// `Renderer::new`'s `target_format_is_srgb: format.is_srgb()`
/// (`noa-render/src/renderer.rs`), which routes `surface_output_rgba`
/// (`noa-render/src/renderer.rs`) into its no-op branch whenever the
/// surface format is non-sRGB: colors are written to the target unchanged,
/// no double-gamma. Do **not** "fix" this back to preferring
/// `Bgra8UnormSrgb` — that reintroduces the linear-blend thinning bug.
/// Falls back to `Bgra8UnormSrgb`, then to the first available format, if
/// the adapter offers no non-sRGB option.
pub(crate) fn preferred_surface_format(available: &[wgpu::TextureFormat]) -> wgpu::TextureFormat {
    available
        .iter()
        .copied()
        .find(|f| *f == wgpu::TextureFormat::Bgra8Unorm)
        .or_else(|| {
            available
                .iter()
                .copied()
                .find(|f| *f == wgpu::TextureFormat::Bgra8UnormSrgb)
        })
        .unwrap_or(available[0])
}

/// Pick the surface's composite-alpha mode. An opaque window keeps the
/// existing Opaque preference (solid terminal colors). A transparent window
/// (`background-opacity` below 1.0) instead prefers, in order, `PostMultiplied`
/// (our colors are straight, non-premultiplied), then `PreMultiplied`, then
/// `Inherit`, before falling back to whatever the surface offers first.
pub(crate) fn preferred_surface_alpha_mode(
    caps: &wgpu::SurfaceCapabilities,
    transparent: bool,
) -> wgpu::CompositeAlphaMode {
    let preference: &[wgpu::CompositeAlphaMode] = if transparent {
        &[
            wgpu::CompositeAlphaMode::PostMultiplied,
            wgpu::CompositeAlphaMode::PreMultiplied,
            wgpu::CompositeAlphaMode::Inherit,
        ]
    } else {
        &[wgpu::CompositeAlphaMode::Opaque]
    };
    preference
        .iter()
        .copied()
        .find(|mode| caps.alpha_modes.contains(mode))
        .or_else(|| caps.alpha_modes.first().copied())
        .unwrap_or(wgpu::CompositeAlphaMode::Auto)
}

pub(crate) fn focus_report_bytes(focused: bool, focus_reporting: bool) -> Option<&'static [u8]> {
    if !focus_reporting {
        return None;
    }
    if focused {
        Some(b"\x1b[I")
    } else {
        Some(b"\x1b[O")
    }
}

pub(crate) fn pane_owns_keyboard_focus<Window: PartialEq, Pane: PartialEq>(
    window_id: Window,
    pane_id: Pane,
    os_focused: Option<Window>,
    focused_pane: Pane,
) -> bool {
    pane_id == focused_pane && os_focused.as_ref() == Some(&window_id)
}

pub(crate) fn font_pixel_size(point_size: f32, scale_factor: f64) -> f32 {
    (point_size * scale_factor.max(f64::EPSILON) as f32).max(1.0)
}

/// The session sidebar renders its synthetic cells at a fixed, dedicated point
/// size — deliberately smaller and denser than the terminal font (mockup
/// parity) and independent of the user's terminal font size. Scaled by DPR like
/// the terminal font so glyphs stay crisp.
pub(crate) const SIDEBAR_FONT_POINT_SIZE: f32 = 11.5;

/// Physical pixel size for the sidebar font at `scale_factor`.
pub(crate) fn sidebar_font_pixel_size(scale_factor: f64) -> f32 {
    font_pixel_size(SIDEBAR_FONT_POINT_SIZE, scale_factor)
}

pub(crate) fn initial_window_logical_size(
    metrics: noa_font::Metrics,
    grid_size: GridSize,
    scale_factor: f64,
    padding: GridPadding,
) -> LogicalSize<f64> {
    let scale_factor = scale_factor.max(f64::EPSILON) as f32;
    let physical_w = (metrics.cell_w * grid_size.cols as f32 + padding.horizontal())
        .ceil()
        .max(1.0);
    let physical_h = (metrics.cell_h * grid_size.rows as f32 + padding.vertical())
        .ceil()
        .max(1.0);

    LogicalSize::new(
        (physical_w / scale_factor) as f64,
        (physical_h / scale_factor) as f64,
    )
}

pub(crate) fn grid_size_for_physical_size(
    size: PhysicalSize<u32>,
    metrics: noa_font::Metrics,
    padding: GridPadding,
) -> GridSize {
    let content_width = (size.width as f32 - padding.horizontal()).max(0.0);
    let content_height = (size.height as f32 - padding.vertical()).max(0.0);
    let cols = (content_width / metrics.cell_w.max(f32::EPSILON))
        .floor()
        .clamp(1.0, u16::MAX as f32) as u16;
    let rows = (content_height / metrics.cell_h.max(f32::EPSILON))
        .floor()
        .clamp(1.0, u16::MAX as f32) as u16;
    GridSize::new(cols, rows)
}

pub(crate) fn update_ime_cursor_area(
    window: &Window,
    metrics: noa_font::Metrics,
    x: u16,
    y: u16,
    pane_rect: PaneRectApp,
    padding: GridPadding,
) {
    let (position, size) = ime_cursor_area(metrics, x, y, pane_rect, padding);
    window.set_ime_cursor_area(position, size);
}

pub(crate) fn ime_cursor_area(
    metrics: noa_font::Metrics,
    x: u16,
    y: u16,
    pane_rect: PaneRectApp,
    padding: GridPadding,
) -> (PhysicalPosition<i32>, PhysicalSize<u32>) {
    let position = PhysicalPosition::new(
        (pane_rect.x as f32 + padding.left + metrics.cell_w * x as f32)
            .round()
            .max(0.0) as i32,
        (pane_rect.y as f32 + padding.top + metrics.cell_h * y as f32)
            .round()
            .max(0.0) as i32,
    );
    let size = PhysicalSize::new(
        metrics.cell_w.ceil().max(1.0) as u32,
        metrics.cell_h.ceil().max(1.0) as u32,
    );
    (position, size)
}

