use noa_core::{DEFAULT_GRID_PADDING, GridPadding};
use noa_grid::CursorStyle;
#[cfg(target_os = "macos")]
use winit::platform::macos::{OptionAsAlt, WindowAttributesExtMacOS};
#[cfg(target_os = "macos")]
use winit::window::WindowAttributes;

/// Configuration the binary passes into [`crate::run`].
#[derive(Debug, Clone)]
pub struct AppConfig {
    pub cols: u16,
    pub rows: u16,
    pub font_size: f32,
    pub theme: Option<String>,
    /// Parsed font settings from `noa-config` (ADR-R1: a distinct type from
    /// `noa_font::FontConfig` — mapped to it via [`font_config_from_noa_config`]
    /// right before each `FontGrid::new` call, keeping `noa-font` free of any
    /// `noa-config`/`dirs` dependency).
    pub font: noa_config::FontConfig,
    /// OSC 52 clipboard read (query) policy.
    pub clipboard_read: noa_config::ClipboardAccess,
    /// Whether to confirm before pasting content that could run commands.
    pub clipboard_paste_protection: bool,
    /// Whether `CSI 21 t` may report the window title back to the program
    /// (`title-report`, default off — see `Terminal::title_report`).
    pub title_report: bool,
    /// `window-padding-x/y`: `None` keeps the built-in default for that axis.
    /// Resolved to a `GridPadding` once in [`App::new`].
    pub window_padding_x: Option<f32>,
    pub window_padding_y: Option<f32>,
    /// Theme color overrides (`background`, `foreground`, `cursor-color`,
    /// `selection-foreground`, `selection-background`).
    pub background: Option<noa_core::Rgb>,
    pub foreground: Option<noa_core::Rgb>,
    pub cursor_color: Option<noa_core::Rgb>,
    pub selection_foreground: Option<noa_core::Rgb>,
    pub selection_background: Option<noa_core::Rgb>,
    /// `minimum-contrast`: WCAG contrast-ratio floor for rendered text/cursor
    /// colors. `1.0` disables adjustment.
    pub minimum_contrast: f32,
    /// `cursor-style` shape and `cursor-style-blink` toggle.
    pub cursor_style: Option<noa_config::CursorShape>,
    pub cursor_style_blink: Option<bool>,
    /// `background-opacity`, clamped to `0.0..=1.0`. Drives window
    /// transparency: below 1.0 the window is created transparent, a
    /// non-Opaque surface alpha mode is chosen, and the renderer scales its
    /// clear-color alpha to match.
    pub background_opacity: f32,
    /// `background-blur-radius` in points (`0..=64`, 0 = off). Applied as a
    /// native macOS window background blur; a no-op on other platforms.
    pub background_blur_radius: u16,
    /// `background-image`: path to a PNG laid behind the terminal grid, or
    /// `None`. Decoded once at startup ([`decode_background_image`]); a missing
    /// or undecodable file logs a diagnostic and disables the image.
    pub background_image: Option<std::path::PathBuf>,
    /// `background-image-opacity`, `0.0..=1.0`. Scales the image quad's alpha,
    /// independent of `background-opacity`.
    pub background_image_opacity: f32,
    /// `background-image-position`: 9-anchor placement within the surface.
    pub background_image_position: noa_config::BackgroundImagePosition,
    /// `background-image-fit`: how the image scales into the surface.
    pub background_image_fit: noa_config::BackgroundImageFit,
    /// `background-image-repeat`: tile the image when it does not fill the
    /// surface.
    pub background_image_repeat: bool,
    /// `scrollback-limit`: total bytes of scrollback storage retained per pane
    /// before page-granular eviction (`0` disables scrollback). Applied to each
    /// new terminal at surface creation.
    pub scrollback_limit: usize,
    /// `window-save-state`: whether the window/tab/split session is saved on
    /// exit and restored on launch. `never` disables both.
    pub window_save_state: noa_config::WindowSaveState,
    /// `macos-option-as-alt`: which Option key(s) the macOS window layer
    /// rewrites as terminal Alt.
    pub macos_option_as_alt: noa_config::MacosOptionAsAlt,
    /// `macos-titlebar-style`: titlebar presentation for ordinary terminal
    /// windows.
    pub macos_titlebar_style: noa_config::MacosTitlebarStyle,
    /// Set when the user passed an explicit grid size on the CLI (`--cols` /
    /// `--rows`). Session restore is suppressed in that case so the requested
    /// dimensions win over the saved topology (Ghostty parity).
    pub cli_grid_override: bool,
    /// `quick-terminal-hotkey`: the global hotkey chord toggling the drop-down
    /// quick terminal (e.g. `cmd+grave`). `None` leaves the feature disabled.
    pub quick_terminal_hotkey: Option<String>,
    /// `quick-terminal-size`: the quick terminal's height as a fraction of the
    /// screen height (`0.1..=1.0`).
    pub quick_terminal_size: f32,
    /// `quick-terminal-autohide`: hide the quick terminal when it loses focus.
    pub quick_terminal_autohide: bool,
    /// `sidebar-enabled`: app-wide initial visibility of the session sidebar,
    /// seeded into each window's per-window toggle at creation (FR-4/FR-13).
    pub sidebar_enabled: bool,
    /// `sidebar-width`: the session sidebar's width in points when visible
    /// (FR-13). Multiplied by the window scale factor at the resize call site
    /// to get the pixel inset.
    pub sidebar_width: f32,
    /// `sidebar-hotkey`: the global chord that toggles the sidebar for the
    /// focused window (FR-13). `None` (or the empty-string "disabled" sentinel)
    /// registers no chord.
    pub sidebar_hotkey: Option<String>,
    /// `sidebar-preview-lines`: number of trailing output rows shown in each
    /// sidebar card. `0` disables the preview rows.
    pub sidebar_preview_lines: usize,
    /// `resize-overlay`: whether the `cols × rows` toast shows during a live
    /// resize (Ghostty parity).
    pub resize_overlay: noa_config::ResizeOverlay,
    /// `visual-bell`: flash the window briefly when its terminal rings BEL
    /// while OS-focused (where the desktop notification is suppressed).
    pub visual_bell: bool,
}

/// Maps the parsed `noa-config` font settings onto the `noa-font` runtime
/// config consumed by `FontGrid::new` (ADR-R1). WP0 only threads the values
/// through; later WPs make more of them observably load-bearing.
pub(super) fn font_config_from_noa_config(cfg: &noa_config::FontConfig) -> noa_font::FontConfig {
    let default = noa_font::FontConfig::default();
    let synthetic_style = match cfg.synthetic_style {
        None | Some(noa_config::SyntheticStyleMode::Both) => default.synthetic_style,
        Some(noa_config::SyntheticStyleMode::Neither) => noa_font::SyntheticStyle {
            bold: false,
            italic: false,
        },
        Some(noa_config::SyntheticStyleMode::NoBold) => noa_font::SyntheticStyle {
            bold: false,
            italic: true,
        },
        Some(noa_config::SyntheticStyleMode::NoItalic) => noa_font::SyntheticStyle {
            bold: true,
            italic: false,
        },
    };
    let alpha_blending = match cfg.alpha_blending {
        None | Some(noa_config::AlphaBlendingMode::Native) => noa_font::AlphaBlending::Native,
        Some(
            noa_config::AlphaBlendingMode::Linear | noa_config::AlphaBlendingMode::LinearCorrected,
        ) => noa_font::AlphaBlending::LinearFallback,
    };

    noa_font::FontConfig {
        families: cfg.families.clone(),
        families_bold: cfg.families_bold.clone(),
        families_italic: cfg.families_italic.clone(),
        families_bold_italic: cfg.families_bold_italic.clone(),
        features: cfg
            .features
            .iter()
            .map(|feature| noa_font::FontFeature {
                tag: feature.tag,
                enabled: feature.enabled,
            })
            .collect(),
        variations: map_font_variations(&cfg.variations),
        variations_bold: map_font_variations(&cfg.variations_bold),
        variations_italic: map_font_variations(&cfg.variations_italic),
        variations_bold_italic: map_font_variations(&cfg.variations_bold_italic),
        synthetic_style,
        alpha_blending,
        thicken: cfg.thicken.unwrap_or(default.thicken),
        thicken_strength: cfg.thicken_strength.unwrap_or(default.thicken_strength),
    }
}

fn map_font_variations(variations: &[noa_config::FontVariation]) -> Vec<noa_font::FontVariation> {
    variations
        .iter()
        .map(|variation| noa_font::FontVariation {
            tag: variation.tag,
            value: variation.value,
        })
        .collect()
}

/// Derive the grid padding from `window-padding-x/y`. An unset axis keeps the
/// corresponding edge(s) of [`DEFAULT_GRID_PADDING`]; a set axis applies its
/// value to both edges of that axis.
pub(super) fn resolve_grid_padding(x: Option<f32>, y: Option<f32>) -> GridPadding {
    let default = DEFAULT_GRID_PADDING;
    GridPadding {
        top: y.unwrap_or(default.top),
        right: x.unwrap_or(default.right),
        bottom: y.unwrap_or(default.bottom),
        left: x.unwrap_or(default.left),
    }
}

/// Map `cursor-style` + `cursor-style-blink` onto a grid [`CursorStyle`].
/// Returns `None` when neither key is set, so the terminal keeps its own
/// default (Ghostty's blinking block). When only the blink toggle is set the
/// shape defaults to block; when only the shape is set it defaults to blinking.
pub(super) fn resolve_cursor_style(
    shape: Option<noa_config::CursorShape>,
    blink: Option<bool>,
) -> Option<CursorStyle> {
    if shape.is_none() && blink.is_none() {
        return None;
    }
    let shape = shape.unwrap_or(noa_config::CursorShape::Block);
    let blinking = blink.unwrap_or(true);
    Some(match (shape, blinking) {
        (noa_config::CursorShape::Block, true) => CursorStyle::BlinkingBlock,
        (noa_config::CursorShape::Block, false) => CursorStyle::SteadyBlock,
        (noa_config::CursorShape::Bar, true) => CursorStyle::BlinkingBar,
        (noa_config::CursorShape::Bar, false) => CursorStyle::SteadyBar,
        (noa_config::CursorShape::Underline, true) => CursorStyle::BlinkingUnderline,
        (noa_config::CursorShape::Underline, false) => CursorStyle::SteadyUnderline,
    })
}

/// Map the parsed `background-image-fit` onto the render-side enum (the render
/// crate keeps its own copy so it stays free of a `noa-config` dependency).
pub(super) fn background_image_fit(
    value: noa_config::BackgroundImageFit,
) -> noa_render::BackgroundImageFit {
    match value {
        noa_config::BackgroundImageFit::None => noa_render::BackgroundImageFit::None,
        noa_config::BackgroundImageFit::Contain => noa_render::BackgroundImageFit::Contain,
        noa_config::BackgroundImageFit::Cover => noa_render::BackgroundImageFit::Cover,
        noa_config::BackgroundImageFit::Stretch => noa_render::BackgroundImageFit::Stretch,
    }
}

/// Map the parsed `background-image-position` onto the render-side enum.
pub(super) fn background_image_position(
    value: noa_config::BackgroundImagePosition,
) -> noa_render::BackgroundImagePosition {
    use noa_config::BackgroundImagePosition as C;
    use noa_render::BackgroundImagePosition as R;
    match value {
        C::TopLeft => R::TopLeft,
        C::TopCenter => R::TopCenter,
        C::TopRight => R::TopRight,
        C::CenterLeft => R::CenterLeft,
        C::Center => R::Center,
        C::CenterRight => R::CenterRight,
        C::BottomLeft => R::BottomLeft,
        C::BottomCenter => R::BottomCenter,
        C::BottomRight => R::BottomRight,
    }
}

/// Decode the configured background image once at startup into a render-ready
/// [`noa_render::BackgroundImage`]. PNG-only (spec scope): a missing file, a
/// non-PNG/undecodable file, or a zero-sized image logs a diagnostic and
/// returns `None`, disabling the image while the terminal launches normally
/// (spec FR-8 / AC-8 / AC-15). Never panics.
pub(super) fn decode_background_image(config: &AppConfig) -> Option<noa_render::BackgroundImage> {
    let path = config.background_image.as_ref()?;
    decode_background_image_at(
        path,
        background_image_fit(config.background_image_fit),
        background_image_position(config.background_image_position),
        config.background_image_repeat,
        config.background_image_opacity,
    )
}

/// Read + PNG-decode one background image. Split out from
/// [`decode_background_image`] so the failure paths (missing / non-PNG /
/// empty-file) are unit-testable without constructing a whole [`AppConfig`].
/// Every failure logs a diagnostic and returns `None` (spec FR-8/AC-8/AC-15) —
/// never panics.
fn decode_background_image_at(
    path: &std::path::Path,
    fit: noa_render::BackgroundImageFit,
    position: noa_render::BackgroundImagePosition,
    repeat: bool,
    opacity: f32,
) -> Option<noa_render::BackgroundImage> {
    let resolved = expand_tilde(path);
    let bytes = match std::fs::read(&resolved) {
        Ok(bytes) => bytes,
        Err(error) => {
            log::warn!(
                "background-image: cannot read {}: {error}; disabling background image",
                resolved.display()
            );
            return None;
        }
    };
    let (width, height, rgba) = match decode_png_rgba(&bytes) {
        Ok(decoded) => decoded,
        Err(error) => {
            log::warn!(
                "background-image: cannot decode {} as PNG: {error}; disabling background image \
                 (only PNG is supported)",
                resolved.display()
            );
            return None;
        }
    };
    if width == 0 || height == 0 {
        log::warn!(
            "background-image: {} decoded to an empty image; disabling background image",
            resolved.display()
        );
        return None;
    }
    Some(noa_render::BackgroundImage {
        rgba: std::sync::Arc::from(rgba),
        width,
        height,
        fit,
        position,
        repeat,
        opacity: opacity.clamp(0.0, 1.0),
    })
}

/// Expand a leading `~` / `~/` to the home directory (noa-config stores the
/// path verbatim to stay IO-free). Any other form is left untouched.
fn expand_tilde(path: &std::path::Path) -> std::path::PathBuf {
    if let Ok(rest) = path.strip_prefix("~")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    path.to_path_buf()
}

/// Decode a PNG byte buffer to straight RGBA8 `(width, height, rgba)`. Mirrors
/// `noa-grid`'s Kitty-graphics PNG path (grayscale/RGB expanded, 16-bit
/// truncated to the high byte, indexed rejected).
fn decode_png_rgba(bytes: &[u8]) -> anyhow::Result<(u32, u32, Vec<u8>)> {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info()?;
    let info = reader.info();
    let (width, height) = (info.width, info.height);
    let buf_size = reader
        .output_buffer_size()
        .ok_or_else(|| anyhow::anyhow!("image too large"))?;
    let mut buf = vec![0u8; buf_size];
    let frame = reader.next_frame(&mut buf)?;
    buf.truncate(frame.buffer_size());

    let pixels = (width as usize) * (height as usize);
    let sample_bytes = match frame.bit_depth {
        png::BitDepth::Sixteen => 2,
        _ => 1,
    };
    let sample = |i: usize| -> u8 { buf.get(i * sample_bytes).copied().unwrap_or(0) };
    let mut rgba = Vec::with_capacity(pixels * 4);
    match frame.color_type {
        png::ColorType::Rgba => {
            for i in 0..pixels {
                let base = i * 4;
                rgba.push(sample(base));
                rgba.push(sample(base + 1));
                rgba.push(sample(base + 2));
                rgba.push(sample(base + 3));
            }
        }
        png::ColorType::Rgb => {
            for i in 0..pixels {
                let base = i * 3;
                rgba.push(sample(base));
                rgba.push(sample(base + 1));
                rgba.push(sample(base + 2));
                rgba.push(0xff);
            }
        }
        png::ColorType::GrayscaleAlpha => {
            for i in 0..pixels {
                let base = i * 2;
                let g = sample(base);
                rgba.extend_from_slice(&[g, g, g, sample(base + 1)]);
            }
        }
        png::ColorType::Grayscale => {
            for i in 0..pixels {
                let g = sample(i);
                rgba.extend_from_slice(&[g, g, g, 0xff]);
            }
        }
        png::ColorType::Indexed => {
            anyhow::bail!("indexed-color PNG is not supported");
        }
    }
    Ok((width, height, rgba))
}

#[cfg(target_os = "macos")]
pub(super) fn macos_option_as_alt(value: noa_config::MacosOptionAsAlt) -> OptionAsAlt {
    match value {
        noa_config::MacosOptionAsAlt::None => OptionAsAlt::None,
        noa_config::MacosOptionAsAlt::Left => OptionAsAlt::OnlyLeft,
        noa_config::MacosOptionAsAlt::Right => OptionAsAlt::OnlyRight,
        noa_config::MacosOptionAsAlt::Both => OptionAsAlt::Both,
    }
}

#[cfg(target_os = "macos")]
pub(super) fn apply_macos_titlebar_style(
    attrs: WindowAttributes,
    style: noa_config::MacosTitlebarStyle,
) -> WindowAttributes {
    match style {
        noa_config::MacosTitlebarStyle::Native => attrs,
        noa_config::MacosTitlebarStyle::Transparent => attrs
            .with_titlebar_transparent(true)
            .with_fullsize_content_view(true),
    }
}

pub(super) fn needs_macos_titlebar_backdrop(background_opacity: f32) -> bool {
    background_opacity < 1.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "noa-bgimg-{}-{}-{name}",
            std::process::id(),
            // A per-call counter avoids collisions between cases in one process.
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn write_1x1_png(path: &std::path::Path) {
        let file = std::fs::File::create(path).unwrap();
        let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), 1, 1);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().unwrap();
        writer.write_image_data(&[10, 20, 30, 255]).unwrap();
        writer.finish().unwrap();
    }

    #[test]
    fn translucent_macos_titlebar_chrome_needs_backdrop() {
        assert!(needs_macos_titlebar_backdrop(0.85));
        assert!(!needs_macos_titlebar_backdrop(1.0));
    }

    // AC-8: a missing path does not panic and returns None (no image).
    #[test]
    fn decode_missing_path_returns_none() {
        let path = temp_path("missing.png");
        assert!(!path.exists());
        let result = decode_background_image_at(
            &path,
            noa_render::BackgroundImageFit::Contain,
            noa_render::BackgroundImagePosition::Center,
            false,
            1.0,
        );
        assert!(result.is_none());
    }

    // AC-8: a non-PNG (plain text) file returns None without panicking.
    #[test]
    fn decode_non_png_returns_none() {
        let path = temp_path("notes.txt");
        std::fs::write(&path, b"this is not a png, it's plain text\n").unwrap();
        let result = decode_background_image_at(
            &path,
            noa_render::BackgroundImageFit::Contain,
            noa_render::BackgroundImagePosition::Center,
            false,
            1.0,
        );
        let _ = std::fs::remove_file(&path);
        assert!(result.is_none());
    }

    // AC-8: a zero-byte file returns None without panicking.
    #[test]
    fn decode_empty_file_returns_none() {
        let path = temp_path("empty.png");
        std::fs::write(&path, b"").unwrap();
        let result = decode_background_image_at(
            &path,
            noa_render::BackgroundImageFit::Contain,
            noa_render::BackgroundImagePosition::Center,
            false,
            1.0,
        );
        let _ = std::fs::remove_file(&path);
        assert!(result.is_none());
    }

    // Happy path: a valid PNG decodes to a `BackgroundImage` carrying the
    // placement params (opacity clamped).
    #[test]
    fn decode_valid_png_returns_image_with_params() {
        let path = temp_path("wall.png");
        write_1x1_png(&path);
        let image = decode_background_image_at(
            &path,
            noa_render::BackgroundImageFit::Cover,
            noa_render::BackgroundImagePosition::TopRight,
            true,
            2.0, // out of range -> clamps to 1.0
        )
        .expect("valid PNG decodes");
        let _ = std::fs::remove_file(&path);
        assert_eq!((image.width, image.height), (1, 1));
        assert_eq!(&*image.rgba, &[10, 20, 30, 255]);
        assert_eq!(image.fit, noa_render::BackgroundImageFit::Cover);
        assert_eq!(
            image.position,
            noa_render::BackgroundImagePosition::TopRight
        );
        assert!(image.repeat);
        assert_eq!(image.opacity, 1.0);
    }
}
