//! Surface-format/alpha, focus reporting, font sizing, and IME helpers.

use super::*;

/// Abort startup with a user-readable message instead of a panic backtrace.
/// Only for GPU/font bring-up failures the user must fix in their
/// environment (missing Metal driver, headless session, broken font).
pub(crate) fn gpu_init_fatal(
    persister: &mut crate::session_persist::SessionPersister,
    what: &str,
    detail: impl std::fmt::Display,
) -> ! {
    log::error!("GPU initialization failed: {what}: {detail}");
    eprintln!("noa: {what} ({detail})");
    eprintln!("noa: a working GPU (Metal) and a usable monospace font are required.");
    persister.flush();
    std::process::exit(1);
}

/// Choose the swapchain surface format for the configured `alpha-blending`
/// mode (WP3). The mode decides which blend space the GPU's fixed-function
/// alpha blend unit runs in, and that space is a property of the target
/// format:
///
/// - When the surface format `.is_srgb()`, the blend unit decodes stored
///   texels to linear before blending and re-encodes to sRGB on write, so
///   `wgpu::BlendState::ALPHA_BLENDING` (`pipeline.rs`) executes in **linear**
///   space — that is Ghostty's `linear` / `linear-corrected`.
/// - A non-sRGB format blends directly in gamma-encoded space (how
///   CoreText/FreeType render by default) — Ghostty's `native`. Blending glyph
///   coverage in linear space visibly thins dark-on-light glyph edges.
///
/// So `native` (`AlphaBlending::is_linear() == false`) prefers a **non-sRGB**
/// format (`Bgra8Unorm`), and `linear`/`linear-corrected` prefer an **sRGB**
/// one (`Bgra8UnormSrgb`). Either choice stays in lockstep with `Renderer`'s
/// `target_format_is_srgb: format.is_srgb()`, which routes `surface_output_rgba`
/// so solid colors are pre-linearized only on an sRGB target (no double-gamma).
/// Each falls back to the other, then to the first available format, if the
/// adapter lacks the preferred option. Do **not** hardcode this back to
/// `Bgra8Unorm`: that would silently ignore a `linear` config and reintroduce
/// the fixed native-only behavior.
pub(crate) fn preferred_surface_format(
    available: &[wgpu::TextureFormat],
    alpha_blending: noa_font::AlphaBlending,
) -> wgpu::TextureFormat {
    let (preferred, fallback) = if alpha_blending.is_linear() {
        (
            wgpu::TextureFormat::Bgra8UnormSrgb,
            wgpu::TextureFormat::Bgra8Unorm,
        )
    } else {
        (
            wgpu::TextureFormat::Bgra8Unorm,
            wgpu::TextureFormat::Bgra8UnormSrgb,
        )
    };
    available
        .iter()
        .copied()
        .find(|f| *f == preferred)
        .or_else(|| available.iter().copied().find(|f| *f == fallback))
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

const OCCLUDED_SURFACE_EXTENT: u32 = 1;

pub(crate) fn effective_surface_config(
    config: &wgpu::SurfaceConfiguration,
    occluded: bool,
) -> wgpu::SurfaceConfiguration {
    let mut effective = config.clone();
    if occluded {
        effective.width = OCCLUDED_SURFACE_EXTENT;
        effective.height = OCCLUDED_SURFACE_EXTENT;
    }
    effective
}

pub(crate) fn configure_wgpu_surface(
    surface: &wgpu::Surface<'static>,
    device: &wgpu::Device,
    config: &wgpu::SurfaceConfiguration,
    occluded: bool,
) {
    let effective = effective_surface_config(config, occluded);
    surface.configure(device, &effective);
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

/// The session sidebar renders its synthetic cells at a dedicated point size
/// — deliberately smaller and denser than the terminal font (mockup parity)
/// and independent of the user's terminal font size. `sidebar-font-size`
/// (default this constant) lets the user resize it; this constant remains the
/// layout-design baseline the sidebar's coherent zoom factor is computed
/// against (`App::sidebar_font_zoom`). Scaled by DPR like the terminal font
/// so glyphs stay crisp.
pub(crate) const SIDEBAR_FONT_POINT_SIZE: f32 = 11.5;

/// Physical pixel size for the sidebar font at `point_size`/`scale_factor`.
pub(crate) fn sidebar_font_pixel_size(point_size: f32, scale_factor: f64) -> f32 {
    font_pixel_size(point_size, scale_factor)
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
