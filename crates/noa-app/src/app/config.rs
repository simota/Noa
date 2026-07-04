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
        noa_config::MacosTitlebarStyle::Hidden => attrs
            .with_title_hidden(true)
            .with_titlebar_hidden(true)
            .with_fullsize_content_view(true),
    }
}
